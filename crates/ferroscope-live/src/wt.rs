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

use std::sync::{Arc, Mutex};
use wtransport::{Endpoint, Identity, ServerConfig};

/// A WebTransport broadcast server on localhost.
pub struct WtServer {
    history: Arc<Mutex<Vec<u8>>>,
    tx: tokio::sync::broadcast::Sender<Vec<u8>>,
    port: u16,
    cert_hash_hex: String,
    runtime: tokio::runtime::Handle,
    /// Sessions in flight, so [`WtServer::finish`] can wait for their last bytes to leave.
    open_sessions: Arc<Mutex<usize>>,
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

        let (port_tx, port_rx) = std::sync::mpsc::channel();
        let accept_tx = tx.clone();
        let accept_history = Arc::clone(&history);
        let accept_sessions = Arc::clone(&open_sessions);
        let accept_done = Arc::clone(&done);
        let accept_notify = Arc::clone(&notify);
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
                        // Catch-up first: a viewer that joins mid-run needs the file's front
                        // matter or it can draw nothing. Subscribe BEFORE snapshotting so no
                        // record falls between the snapshot and the subscription.
                        let mut rx = tx.subscribe();
                        let snapshot = history.lock().unwrap().clone();
                        let mut ok = send.write_all(&snapshot).await.is_ok();
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
                                // Sealed already (or the flag flipped between iterations):
                                // drain what is queued — the seal's own records are usually in
                                // here — then FIN.
                                while let Ok(bytes) = rx.try_recv() {
                                    if send.write_all(&bytes).await.is_err() {
                                        ok = false;
                                        break;
                                    }
                                }
                                finished = true;
                                break;
                            }
                            tokio::select! {
                                r = rx.recv() => match r {
                                    Ok(bytes) => ok = send.write_all(&bytes).await.is_ok(),
                                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                                        finished = true;
                                    }
                                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                                        // A viewer 1024 records behind cannot be caught up
                                        // coherently mid-stream; drop it rather than hand it a
                                        // file with a hole in the middle.
                                        ok = false;
                                    }
                                },
                                _ = &mut sealed => {
                                    while let Ok(bytes) = rx.try_recv() {
                                        if send.write_all(&bytes).await.is_err() {
                                            ok = false;
                                            break;
                                        }
                                    }
                                    finished = true;
                                }
                            }
                        }
                        if ok && finished {
                            // The FIN is the file's own end: the client's read loop ends at
                            // exactly the last byte the producer sealed.
                            let _ = send.finish().await;
                            // UDP has no lingering socket: when this process exits, unacked
                            // packets — the FIN very much included — die with it, and the
                            // client hangs to the QUIC idle timeout with every byte delivered
                            // and no end. So the session outlives its own FIN until the peer
                            // closes (a probe exits at once; a browser tab makes the producer's
                            // finish() spend its timeout, which is what the timeout is for).
                            let _ = conn.closed().await;
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

    /// Send one complete record (or the magic) to every viewer, and keep it for late joiners.
    pub fn broadcast(&self, record: &[u8]) {
        self.history.lock().unwrap().extend_from_slice(record);
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
    pub fn finish(self, timeout: std::time::Duration) {
        self.done.store(true, std::sync::atomic::Ordering::SeqCst);
        self.notify.notify_waiters();
        drop(self.tx);
        let deadline = std::time::Instant::now() + timeout;
        while *self.open_sessions.lock().unwrap() > 0 && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        // The runtime thread parks on accept() forever; the process ending reaps it.
        let _ = self.runtime;
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
            self.history.lock().unwrap().extend_from_slice(&record);
            let _ = self.server_tx.send(record);
        }
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}
