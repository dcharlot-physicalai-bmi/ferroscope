//! The WebTransport transport: HTTP/3 over QUIC, for the browsers and networks that want it.
//!
//! The invariant is the same one the WebSocket transport holds: **the stream is the file.** Here
//! it is even more literal — each viewer gets one unidirectional QUIC stream carrying the raw
//! recording bytes in file order, no framing layer at all, FIN when the recording seals. A
//! client that saves the stream to disk has saved the file.
//!
//! WebTransport requires TLS even on localhost. The designed answer for local tooling is a
//! self-signed certificate (ECDSA, valid under fourteen days) whose SHA-256 the browser accepts
//! via `serverCertificateHashes` — no CA, no trust store edits. [`WtServer::cert_hash_hex`] is
//! that hash; the producer prints it inside a clickable viewer link.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use wtransport::{Endpoint, Identity, ServerConfig, VarInt};

/// The stream error code a viewer sees when its transfer was abandoned rather than sealed.
///
/// This exists because of the sharpest bug this transport has had: on QUIC, **dropping a send
/// stream is a graceful FIN**, and FIN is precisely how this protocol says "the recording
/// sealed, what you hold is the file". A session that gave up therefore handed the viewer a
/// structurally valid *prefix* carrying the seal's own signal, and both sides reported success.
/// Every abnormal end now resets the stream instead, so a truncated transfer is unmistakable.
pub const ABANDONED: u32 = 1;

/// What [`WtServer::finish`] observed: viewers that received the sealed file, and viewers that
/// did not. A producer that streamed to nobody successfully should not print that it did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FinishReport {
    /// Sessions that received every byte through the seal and were FINed.
    pub sealed: usize,
    /// Sessions abandoned before the seal — reset, never FINed.
    pub abandoned: usize,
    /// Sessions still in flight when the timeout expired.
    pub timed_out: usize,
}

impl FinishReport {
    /// `true` when every viewer that ever connected left holding the complete file.
    pub fn all_sealed(&self) -> bool {
        self.abandoned == 0 && self.timed_out == 0
    }
}

/// A WebTransport broadcast server on localhost.
pub struct WtServer {
    history: Arc<Mutex<Vec<u8>>>,
    tx: tokio::sync::broadcast::Sender<Vec<u8>>,
    port: u16,
    cert_hash_hex: String,
    runtime: tokio::runtime::Handle,
    /// Sessions in flight, so [`WtServer::finish`] can wait for their last bytes to leave.
    open_sessions: Arc<Mutex<usize>>,
    /// Sessions that have not yet reached a verdict — neither FINed nor reset.
    ///
    /// Distinct from `open_sessions` on purpose: a session that has sealed still lingers in
    /// `conn.closed()` so its FIN cannot die with the process, and counting that as "still
    /// open at the deadline" reported a healthy transfer as a failure.
    undecided: Arc<AtomicUsize>,
    sealed_count: Arc<AtomicUsize>,
    abandoned_count: Arc<AtomicUsize>,
    /// The explicit end-of-recording signal. Sender-counting cannot carry it: the accept loop
    /// must hold a Sender forever to subscribe new sessions, so the channel never closes on its
    /// own — the first version waited on exactly that, and every stream died at the QUIC idle
    /// timeout with all its bytes delivered and no FIN.
    done: Arc<std::sync::atomic::AtomicBool>,
    notify: Arc<tokio::sync::Notify>,
}

impl WtServer {
    /// Bind on `127.0.0.1:port` with a fresh self-signed identity, accepting viewers on a
    /// background runtime.
    pub fn bind(port: u16) -> std::io::Result<WtServer> {
        let identity = Identity::self_signed(["localhost", "127.0.0.1", "::1"])
            .map_err(std::io::Error::other)?;
        let cert_hash_hex = identity
            .certificate_chain()
            .as_slice()
            .first()
            .map(|c| {
                c.hash()
                    .as_ref()
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect::<String>()
            })
            .ok_or_else(|| std::io::Error::other("no certificate in the fresh identity"))?;

        let (tx, _) = tokio::sync::broadcast::channel::<Vec<u8>>(1024);
        let history: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
        let open_sessions: Arc<Mutex<usize>> = Arc::new(Mutex::new(0));
        let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let notify = Arc::new(tokio::sync::Notify::new());

        // The producer is synchronous; the QUIC stack is not. One runtime on its own thread
        // keeps the producer's loop free of async, which is the whole point of the sync API.
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()?;
        let handle = runtime.handle().clone();

        let config = ServerConfig::builder()
            .with_bind_default(port)
            .with_identity(identity)
            .build();

        let sealed_count = Arc::new(AtomicUsize::new(0));
        let abandoned_count = Arc::new(AtomicUsize::new(0));
        let undecided = Arc::new(AtomicUsize::new(0));

        let (port_tx, port_rx) = std::sync::mpsc::channel();
        let accept_tx = tx.clone();
        let accept_history = Arc::clone(&history);
        let accept_sessions = Arc::clone(&open_sessions);
        let accept_done = Arc::clone(&done);
        let accept_notify = Arc::clone(&notify);
        let accept_sealed = Arc::clone(&sealed_count);
        let accept_abandoned = Arc::clone(&abandoned_count);
        let accept_undecided = Arc::clone(&undecided);
        std::thread::spawn(move || {
            runtime.block_on(async move {
                let server = match Endpoint::server(config) {
                    Ok(s) => s,
                    Err(e) => {
                        let _ = port_tx.send(Err(std::io::Error::other(e)));
                        return;
                    }
                };
                let _ = port_tx.send(
                    server
                        .local_addr()
                        .map(|a| a.port())
                        .map_err(std::io::Error::other),
                );
                loop {
                    let incoming = server.accept().await;
                    let tx = accept_tx.clone();
                    let history = Arc::clone(&accept_history);
                    let sessions = Arc::clone(&accept_sessions);
                    let done = Arc::clone(&accept_done);
                    let notify = Arc::clone(&accept_notify);
                    let sealed_n = Arc::clone(&accept_sealed);
                    let abandoned_n = Arc::clone(&accept_abandoned);
                    let undecided_n = Arc::clone(&accept_undecided);
                    tokio::spawn(async move {
                        let Ok(request) = incoming.await else { return };
                        let Ok(conn) = request.accept().await else {
                            return;
                        };
                        // open_uni resolves to a pending stream that itself resolves.
                        let Ok(opening) = conn.open_uni().await else {
                            return;
                        };
                        let Ok(mut send) = opening.await else { return };
                        *sessions.lock().unwrap() += 1;
                        undecided_n.fetch_add(1, Ordering::SeqCst);
                        // Catch-up first: a viewer that joins mid-run needs the file's front
                        // matter or it can draw nothing. Subscribe and snapshot under the
                        // history lock — the lock every broadcast sends under — so a record is
                        // either in the snapshot or in the subscription, exactly one of the
                        // two. Subscribing first without the lock looked safe and was not: a
                        // broadcast between the subscribe and the snapshot arrived twice.
                        let (mut rx, snapshot) = {
                            let history = history.lock().unwrap();
                            (tx.subscribe(), history.clone())
                        };
                        // Bytes this session has written. Because every broadcast appends to
                        // history in the same order it is sent, what a session has written is
                        // always a PREFIX of history — which is what makes the two recoveries
                        // below exact rather than approximate.
                        let mut sent = 0usize;
                        let mut ok = send.write_all(&snapshot).await.is_ok();
                        if ok {
                            sent = snapshot.len();
                        }
                        let mut finished = false;
                        while ok && !finished {
                            // The lost-wake-up race, closed the way the Notify docs demand:
                            // create the notified future FIRST, then read the flag. A
                            // notify_waiters() that fires between iterations wakes nobody, and
                            // the first version of this loop hung on exactly that — every byte
                            // delivered, no FIN, and the client dead at the QUIC idle timeout.
                            let sealed = notify.notified();
                            tokio::pin!(sealed);
                            if done.load(std::sync::atomic::Ordering::SeqCst) {
                                ok = flush_tail(&mut send, &history, &mut sent).await;
                                finished = true;
                                break;
                            }
                            tokio::select! {
                                r = rx.recv() => match r {
                                    Ok(bytes) => {
                                        ok = send.write_all(&bytes).await.is_ok();
                                        if ok { sent += bytes.len(); }
                                    }
                                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                                        finished = true;
                                    }
                                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                                        // A viewer that fell behind the channel is NOT dropped:
                                        // the server still holds every byte it has broadcast, so
                                        // the session re-reads the tail it missed straight from
                                        // history and carries on. Resubscribing and snapshotting
                                        // under the history lock makes the new subscription start
                                        // exactly where the snapshot ends, the same way a fresh
                                        // join does. Dropping here is what used to hand a viewer
                                        // a truncated file, and on QUIC that looked like a seal.
                                        let (again, snap) = {
                                            let h = history.lock().unwrap();
                                            (tx.subscribe(), h.clone())
                                        };
                                        rx = again;
                                        if snap.len() > sent {
                                            ok = send.write_all(&snap[sent..]).await.is_ok();
                                            if ok { sent = snap.len(); }
                                        }
                                    }
                                },
                                _ = &mut sealed => {
                                    ok = flush_tail(&mut send, &history, &mut sent).await;
                                    finished = true;
                                }
                            }
                        }
                        if ok && finished {
                            // The FIN is the file's own end: the client's read loop ends at
                            // exactly the last byte the producer sealed.
                            let _ = send.finish().await;
                            sealed_n.fetch_add(1, Ordering::SeqCst);
                            undecided_n.fetch_sub(1, Ordering::SeqCst);
                            // UDP has no lingering socket: when this process exits, unacked
                            // packets — the FIN very much included — die with it, and the
                            // client hangs to the QUIC idle timeout with every byte delivered
                            // and no end. So the session outlives its own FIN until the peer
                            // closes (a probe exits at once; a browser tab makes the producer's
                            // finish() spend its timeout, which is what the timeout is for).
                            let _ = conn.closed().await;
                        } else {
                            // Never let a failure wear the seal's clothes. Dropping the stream
                            // would FIN it — quinn's Drop calls finish() — and the viewer would
                            // keep a truncated prefix believing the recording ended there.
                            let _ = send.reset(VarInt::from_u32(ABANDONED));
                            abandoned_n.fetch_add(1, Ordering::SeqCst);
                            undecided_n.fetch_sub(1, Ordering::SeqCst);
                        }
                        *sessions.lock().unwrap() -= 1;
                    });
                }
            });
        });

        let port = port_rx
            .recv()
            .map_err(|_| std::io::Error::other("the endpoint thread died before binding"))??;

        Ok(WtServer {
            history,
            tx,
            port,
            cert_hash_hex,
            runtime: handle,
            open_sessions,
            undecided,
            sealed_count,
            abandoned_count,
            done,
            notify,
        })
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// The SHA-256 of the self-signed certificate, hex, for `serverCertificateHashes`.
    pub fn cert_hash_hex(&self) -> &str {
        &self.cert_hash_hex
    }

    /// How many viewer sessions hold an open stream right now.
    pub fn viewers(&self) -> usize {
        *self.open_sessions.lock().unwrap()
    }

    /// Send one complete record (or the magic) to every viewer, and keep it for late joiners.
    pub fn broadcast(&self, record: &[u8]) {
        // The send happens under the history lock, and a session subscribes and snapshots
        // under the same lock: every record is therefore either in a joiner's snapshot or in
        // its subscription, never both and never neither.
        let mut history = self.history.lock().unwrap();
        history.extend_from_slice(record);
        // No receivers is not an error: a run with no viewers still records.
        let _ = self.tx.send(record.to_vec());
    }

    /// Wrap a sink so everything a recorder writes also streams to viewers, record by record.
    pub fn tee<W: std::io::Write>(&self, inner: W) -> WtTee<W> {
        WtTee {
            inner,
            framer: crate::RecordFramer::new(),
            server_tx: self.tx.clone(),
            history: Arc::clone(&self.history),
        }
    }

    /// Declare the recording finished: FIN every viewer's stream and wait for the bytes to
    /// leave, up to the timeout. Without this, a producer that exits right after sealing races
    /// its own last records out of existence.
    ///
    /// The report says how many viewers actually left holding the sealed file. A caller that
    /// prints "the stream is the file they now hold" is only entitled to say so when
    /// [`FinishReport::all_sealed`] agrees.
    pub fn finish(self, timeout: std::time::Duration) -> FinishReport {
        self.done.store(true, std::sync::atomic::Ordering::SeqCst);
        self.notify.notify_waiters();
        drop(self.tx);
        let deadline = std::time::Instant::now() + timeout;
        while *self.open_sessions.lock().unwrap() > 0 && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let timed_out = self.undecided.load(Ordering::SeqCst);
        // The runtime thread parks on accept() forever; the process ending reaps it.
        let _ = self.runtime;
        FinishReport {
            sealed: self.sealed_count.load(Ordering::SeqCst),
            abandoned: self.abandoned_count.load(Ordering::SeqCst),
            timed_out,
        }
    }
}

/// Write everything in history that this session has not sent yet.
///
/// Used at the seal and whenever a session falls behind. It replaced draining the broadcast
/// channel with `try_recv`, which had the same silent-truncation flaw as the Lagged arm: a
/// `try_recv` that returns `Lagged` ends a `while let Ok(..)` loop quietly, so the tail was
/// dropped and the stream FINed anyway. History is the whole file by then; it cannot lag.
async fn flush_tail(
    send: &mut wtransport::SendStream,
    history: &Arc<Mutex<Vec<u8>>>,
    sent: &mut usize,
) -> bool {
    let tail = {
        let h = history.lock().unwrap();
        if *sent >= h.len() {
            Vec::new()
        } else {
            h[*sent..].to_vec()
        }
    };
    if tail.is_empty() {
        return true;
    }
    if send.write_all(&tail).await.is_ok() {
        *sent += tail.len();
        true
    } else {
        false
    }
}

/// The WebTransport twin of [`crate::Tee`].
pub struct WtTee<W: std::io::Write> {
    inner: W,
    framer: crate::RecordFramer,
    server_tx: tokio::sync::broadcast::Sender<Vec<u8>>,
    history: Arc<Mutex<Vec<u8>>>,
}

impl<W: std::io::Write> std::io::Write for WtTee<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.inner.write_all(buf)?;
        for record in self.framer.push(buf) {
            // Send under the history lock, exactly as WtServer::broadcast does and for the
            // same exactly-once reason.
            let mut history = self.history.lock().unwrap();
            history.extend_from_slice(&record);
            let _ = self.server_tx.send(record);
        }
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}
