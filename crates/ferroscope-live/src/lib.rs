//! Stream a recording as it is written.
//!
//! The design rule: **the stream is the file.** Every WebSocket frame this server sends is one
//! complete MCAP record, in file order, starting with the magic. A client that appends the
//! frames it receives holds, at every instant, a valid prefix of the recording — and the moment
//! the producer seals, it holds the whole file, receipt included, and can verify it with the
//! same code that verifies one read from disk. Live viewing and archived evidence are not two
//! formats; they are one format at two moments.
//!
//! ```no_run
//! use ferroscope_live::LiveServer;
//! use std::io::Write;
//!
//! let server = LiveServer::bind(8737).unwrap();
//! // Wrap any sink a Recorder writes to; every record fans out to connected viewers.
//! let sink = server.tee(Vec::new());
//! let mut rec = ferroscope_schema::Recorder::new(sink, ferroscope_receipt::Precision::Exact);
//! # let _ = rec.scalar("/x", ferroscope_schema::Stamp::sim(0, 0), 1.0, "m");
//! ```
//!
//! Transport is WebSocket rather than WebTransport, deliberately. WebTransport needs an
//! HTTP/3 + TLS stack — a large dependency tree for a link whose client is a browser tab on the
//! same desk — and its real advantages (unreliable datagrams, no head-of-line blocking) belong
//! to lossy fleet telemetry, not to a local viewer. Browsers treat `ws://localhost` as a secure
//! context even from an https page, which is exactly how a hosted viewer reaches a local
//! producer. One protocol, hand-rolled in std, zero dependencies, server-to-client only.

#![forbid(unsafe_code)]

mod sha1;
#[cfg(feature = "webtransport")]
mod wt;
#[cfg(feature = "webtransport")]
pub use wt::{WtServer, WtTee};

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

/// A WebSocket broadcast server bound to localhost.
pub struct LiveServer {
    clients: Arc<Mutex<Vec<TcpStream>>>,
    port: u16,
    /// Everything broadcast so far, so a viewer that connects mid-run still gets a valid file
    /// prefix: schemas and channels live at the front of the stream, and a client that missed
    /// them could draw nothing.
    history: Arc<Mutex<Vec<u8>>>,
}

impl LiveServer {
    /// Bind on `127.0.0.1:port` and accept viewers on a background thread.
    ///
    /// Localhost only, by construction: this serves whatever the producer records to whoever
    /// can reach the socket, and "whoever is on this machine" is the only audience that needs
    /// no further thought.
    pub fn bind(port: u16) -> std::io::Result<LiveServer> {
        let listener = TcpListener::bind(("127.0.0.1", port))?;
        let port = listener.local_addr()?.port();
        let clients: Arc<Mutex<Vec<TcpStream>>> = Arc::new(Mutex::new(Vec::new()));
        let history: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));

        let accept_clients = Arc::clone(&clients);
        let accept_history = Arc::clone(&history);
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                if let Ok(mut s) = handshake(stream) {
                    // Catch-up before live: the file's front matter is the only place schemas
                    // and channels live, and frames are records, so replaying history keeps the
                    // invariant that the client always holds a valid prefix.
                    //
                    // Two passes. The bulk goes out with no lock held, because sending
                    // megabytes to a slow client must not stall the producer. Then whatever
                    // arrived meanwhile goes out under the history lock — the same lock every
                    // broadcast holds while framing — and the client joins the list before the
                    // lock drops, so no record can fall between its snapshot and its
                    // subscription, and none can reach it twice.
                    let past = accept_history.lock().unwrap().clone();
                    if !past.is_empty() && frame_to(&mut s, &past).is_err() {
                        continue;
                    }
                    let history = accept_history.lock().unwrap();
                    let delta = &history[past.len()..];
                    if !delta.is_empty() && frame_to(&mut s, delta).is_err() {
                        continue;
                    }
                    accept_clients.lock().unwrap().push(s);
                    drop(history);
                }
            }
        });

        Ok(LiveServer {
            clients,
            port,
            history,
        })
    }

    /// The port actually bound, for `bind(0)`.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// How many viewers are currently connected.
    pub fn viewers(&self) -> usize {
        self.clients.lock().unwrap().len()
    }

    /// Send one complete MCAP record (or the leading magic) to every connected viewer.
    ///
    /// A viewer that cannot be written to is dropped without ceremony: a stall in one browser
    /// tab must never back-pressure the simulation.
    pub fn broadcast(&self, record: &[u8]) {
        // The history lock is held across the framing, and the accept thread admits a client
        // under the same lock: without that, a record could land in the history after a
        // joiner's snapshot but be framed before the joiner is in the client list — a hole in
        // that viewer's file that no later frame repairs.
        let mut history = self.history.lock().unwrap();
        history.extend_from_slice(record);
        let mut clients = self.clients.lock().unwrap();
        clients.retain_mut(|c| frame_to(c, record).is_ok());
    }

    /// Wrap a sink so that everything a [`ferroscope_schema::Recorder`] writes is also framed
    /// out to viewers, record by record.
    pub fn tee<W: Write>(&self, inner: W) -> Tee<W> {
        Tee {
            inner,
            framer: RecordFramer::new(),
            clients: Arc::clone(&self.clients),
            history: Arc::clone(&self.history),
        }
    }
}

/// One binary frame, server to client, unmasked, FIN set.
fn frame_to(s: &mut TcpStream, payload: &[u8]) -> std::io::Result<()> {
    let mut hdr = Vec::with_capacity(10);
    hdr.push(0x82); // FIN + binary
    match payload.len() {
        n if n < 126 => hdr.push(n as u8),
        n if n <= 0xFFFF => {
            hdr.push(126);
            hdr.extend_from_slice(&(n as u16).to_be_bytes());
        }
        n => {
            hdr.push(127);
            hdr.extend_from_slice(&(n as u64).to_be_bytes());
        }
    }
    s.write_all(&hdr)?;
    s.write_all(payload)
}

/// The RFC 6455 opening handshake, server side.
fn handshake(mut s: TcpStream) -> std::io::Result<TcpStream> {
    let mut buf = Vec::new();
    let mut byte = [0u8; 1];
    // Read until the blank line; a request that never sends one is dropped by the read timeout
    // a caller may have set, or by the peer closing.
    while !buf.ends_with(b"\r\n\r\n") {
        if buf.len() > 8192 {
            return Err(std::io::Error::other("oversized handshake"));
        }
        let n = s.read(&mut byte)?;
        if n == 0 {
            return Err(std::io::Error::other("closed during handshake"));
        }
        buf.push(byte[0]);
    }
    let text = String::from_utf8_lossy(&buf);
    let key = text
        .lines()
        .find_map(|l| {
            let (k, v) = l.split_once(':')?;
            k.trim()
                .eq_ignore_ascii_case("sec-websocket-key")
                .then(|| v.trim().to_string())
        })
        .ok_or_else(|| std::io::Error::other("no Sec-WebSocket-Key"))?;
    let accept = sha1::base64(&sha1::sha1(
        format!("{key}258EAFA5-E914-47DA-95CA-C5AB0DC85B11").as_bytes(),
    ));
    s.write_all(
        format!(
            "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\n\
             Connection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"
        )
        .as_bytes(),
    )?;
    Ok(s)
}

/// Splits a byte stream into complete MCAP records.
///
/// The writer underneath a [`ferroscope_schema::Recorder`] calls `write` in whatever pieces it
/// likes; the wire wants record-aligned frames so a client can parse each one on arrival. MCAP's
/// framing makes the split deterministic: 8 bytes of magic, then records of
/// `opcode: u8, length: u64le, payload`.
pub struct RecordFramer {
    buf: Vec<u8>,
    magic_done: bool,
}

impl RecordFramer {
    pub fn new() -> RecordFramer {
        RecordFramer {
            buf: Vec::new(),
            magic_done: false,
        }
    }

    /// Feed bytes; get back every complete record (the leading magic counts as one unit).
    pub fn push(&mut self, bytes: &[u8]) -> Vec<Vec<u8>> {
        self.buf.extend_from_slice(bytes);
        let mut out = Vec::new();
        loop {
            if !self.magic_done {
                if self.buf.len() < 8 {
                    break;
                }
                out.push(self.buf.drain(..8).collect());
                self.magic_done = true;
                continue;
            }
            // The closing magic follows the footer with no record header of its own, and it
            // may arrive in a write() call of its own — so it is checked FIRST, before the
            // nine-byte minimum a record needs would break out of the loop and strand it.
            // No record can start these bytes: 0x89 is not an MCAP opcode.
            if self.buf.len() >= 8 && &self.buf[..8] == b"\x89MCAP0\r\n" {
                out.push(self.buf.drain(..8).collect());
                continue;
            }
            if self.buf.len() < 9 {
                break;
            }
            let len = u64::from_le_bytes(self.buf[1..9].try_into().unwrap()) as usize;
            let total = 1 + 8 + len;
            if self.buf.len() < total {
                break;
            }
            out.push(self.buf.drain(..total).collect());
        }
        out
    }
}

impl Default for RecordFramer {
    fn default() -> Self {
        Self::new()
    }
}

/// A sink that records to `inner` and streams every complete record to the server's viewers.
pub struct Tee<W: Write> {
    inner: W,
    framer: RecordFramer,
    clients: Arc<Mutex<Vec<TcpStream>>>,
    history: Arc<Mutex<Vec<u8>>>,
}

impl<W: Write> Tee<W> {
    /// The wrapped sink back, once sealing is done.
    pub fn into_inner(self) -> W {
        self.inner
    }
}

impl<W: Write> Write for Tee<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.inner.write_all(buf)?;
        for record in self.framer.push(buf) {
            // Extend and frame under one hold of the history lock, for the same reason
            // broadcast() does: a client joining mid-record must see it exactly once.
            let mut history = self.history.lock().unwrap();
            history.extend_from_slice(&record);
            let mut clients = self.clients.lock().unwrap();
            clients.retain_mut(|c| frame_to(c, &record).is_ok());
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

/// Test scaffolding: a Tee with no server behind it, and its unwrapper. Public because the
/// probe binaries and integration tests need to build one; useless otherwise.
#[doc(hidden)]
pub fn test_tee<W: Write>(inner: W, history: Arc<Mutex<Vec<u8>>>) -> Tee<W> {
    Tee {
        inner,
        framer: RecordFramer::new(),
        clients: Arc::new(Mutex::new(Vec::new())),
        history,
    }
}

#[doc(hidden)]
pub fn tee_into_inner<W: Write>(t: Tee<W>) -> W {
    t.into_inner()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_framer_reassembles_records_from_arbitrary_splits() {
        // magic, then two records, delivered one byte at a time — the cruellest split.
        let mut file = Vec::new();
        file.extend_from_slice(b"\x89MCAP0\r\n");
        file.push(0x01);
        file.extend_from_slice(&3u64.to_le_bytes());
        file.extend_from_slice(b"abc");
        file.push(0x05);
        file.extend_from_slice(&1u64.to_le_bytes());
        file.push(b'z');

        // The closing magic delivered byte by byte, in later pushes than the record it
        // follows: the shipped bug was a framer that stranded it exactly here.
        file.extend_from_slice(b"\x89MCAP0\r\n");
        let mut f = RecordFramer::new();
        let mut got: Vec<Vec<u8>> = Vec::new();
        for b in &file {
            got.extend(f.push(&[*b]));
        }
        assert_eq!(got.len(), 4, "magic + two records + closing magic");
        assert_eq!(got[3], b"\x89MCAP0\r\n");
        assert_eq!(got[0], b"\x89MCAP0\r\n");
        assert_eq!(got[1][0], 0x01);
        assert_eq!(&got[1][9..], b"abc");
        assert_eq!(&got[2][9..], b"z");
    }

    #[test]
    fn the_closing_magic_is_emitted_as_its_own_unit() {
        let mut file = Vec::new();
        file.extend_from_slice(b"\x89MCAP0\r\n");
        file.push(0x02); // a "footer-ish" record
        file.extend_from_slice(&2u64.to_le_bytes());
        file.extend_from_slice(b"ok");
        file.extend_from_slice(b"\x89MCAP0\r\n");
        let mut f = RecordFramer::new();
        let got = f.push(&file);
        assert_eq!(got.len(), 3);
        assert_eq!(
            got[2], b"\x89MCAP0\r\n",
            "the trailing magic must not be stranded"
        );
    }

    #[test]
    fn a_tee_reproduces_the_file_byte_for_byte() {
        // The invariant everything rests on: the inner sink and the concatenated frames are the
        // SAME bytes. Record a real run through a Tee (no server, empty client set) and compare.
        let clients = Arc::new(Mutex::new(Vec::new()));
        let history = Arc::new(Mutex::new(Vec::new()));
        let tee = Tee {
            inner: Vec::new(),
            framer: RecordFramer::new(),
            clients,
            history: Arc::clone(&history),
        };
        let mut rec = ferroscope_schema::Recorder::new(tee, ferroscope_receipt::Precision::Exact);
        for step in 0..50u64 {
            rec.scalar(
                "/x",
                ferroscope_schema::Stamp::sim(step * 1_000_000, step),
                (step as f64).sin(),
                "m",
            )
            .unwrap();
        }
        let spec = ferroscope_receipt::RunSpec::new("tee", 0)
            .steps(50)
            .build("live-test");
        let (tee, _receipt, _quote) = rec.seal(spec, "test").unwrap();
        let file = tee.into_inner();
        let streamed = history.lock().unwrap().clone();
        assert_eq!(
            file, streamed,
            "the stream and the file must be identical bytes"
        );
        // And the streamed copy verifies exactly like the file it is.
        let v = ferroscope_schema::verify(&streamed).expect("a receipt");
        assert!(v.ok(), "the streamed bytes must stand behind the receipt");
    }
}
