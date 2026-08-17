//! **MCAP without the C.**
//!
//! A reader and writer for [MCAP](https://mcap.dev) v0 — the self-describing container the
//! robotics field standardized on — implemented in `std` alone. No `zstd`, no `lz4`, no
//! `binrw`, no build script, no C toolchain. The consequence is the point: this crate
//! compiles for `wasm32-unknown-unknown` unchanged, so a browser tab can open the same
//! recording a workstation wrote, byte for byte, with no server in between.
//!
//! Files are written with **uncompressed chunks**, which is a conforming MCAP profile that
//! every reader must accept. Compression is a codec decision, not a format decision, and
//! paying for a C codec on every platform in order to sometimes save bytes is what keeps
//! robotics log tooling off the browser. If you want the bytes smaller, the transport
//! (HTTP, WebTransport) already compresses.
//!
//! ```
//! use ferroscope_mcap::{Writer, WriterOptions, read};
//!
//! let mut w = Writer::new(Vec::new(), WriterOptions::new("ferroscope", "doctest"));
//! let s = w.add_schema("example.Reading", "jsonschema", br#"{"type":"object"}"#).unwrap();
//! let c = w.add_channel("/imu", s, "json", &[]).unwrap();
//! w.write_message(c, 0, 1_000, 1_000, br#"{"ax":0.1}"#).unwrap();
//! let bytes = w.finish().unwrap();
//!
//! let log = read(&bytes).unwrap();
//! assert_eq!(log.messages.len(), 1);
//! assert_eq!(log.channels[0].topic, "/imu");
//! ```

#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::fmt;

mod crc32;
mod read;
mod write;

pub use crc32::{crc32, Crc32};
pub use read::{read, Log, Statistics};
pub use write::{Writer, WriterOptions};

/// The eight bytes that open and close every MCAP file. The `0x30` is ASCII `'0'`: the
/// format major version, so a reader can refuse a future incompatible file at byte 6.
pub const MAGIC: [u8; 8] = [0x89, b'M', b'C', b'A', b'P', 0x30, b'\r', b'\n'];

/// Record opcodes, as defined by the MCAP specification.
pub mod op {
    pub const HEADER: u8 = 0x01;
    pub const FOOTER: u8 = 0x02;
    pub const SCHEMA: u8 = 0x03;
    pub const CHANNEL: u8 = 0x04;
    pub const MESSAGE: u8 = 0x05;
    pub const CHUNK: u8 = 0x06;
    pub const MESSAGE_INDEX: u8 = 0x07;
    pub const CHUNK_INDEX: u8 = 0x08;
    pub const ATTACHMENT: u8 = 0x09;
    pub const ATTACHMENT_INDEX: u8 = 0x0A;
    pub const STATISTICS: u8 = 0x0B;
    pub const METADATA: u8 = 0x0C;
    pub const METADATA_INDEX: u8 = 0x0D;
    pub const SUMMARY_OFFSET: u8 = 0x0E;
    pub const DATA_END: u8 = 0x0F;
}

/// A message payload's type, described once and referenced by every channel that carries it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Schema {
    pub id: u16,
    pub name: String,
    /// `jsonschema`, `protobuf`, `ros2msg`, … — how `data` should be interpreted.
    pub encoding: String,
    pub data: Vec<u8>,
}

/// A named stream of messages that all share one schema.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Channel {
    pub id: u16,
    /// `0` means "no schema" — legal, and a good reason for a viewer to say so out loud.
    pub schema_id: u16,
    pub topic: String,
    pub message_encoding: String,
    pub metadata: Vec<(String, String)>,
}

/// One message on one channel, at one instant.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Message {
    pub channel_id: u16,
    pub sequence: u32,
    /// When the recorder wrote it (nanoseconds since the Unix epoch).
    pub log_time: u64,
    /// When the producer created it. Equal to `log_time` when nothing better is known.
    pub publish_time: u64,
    pub data: Vec<u8>,
}

/// Everything that can go wrong reading or writing MCAP, named precisely enough to fix.
#[derive(Debug)]
pub enum Error {
    /// The file does not begin, or does not end, with [`MAGIC`].
    BadMagic {
        at: &'static str,
    },
    /// The file claims a major version this crate does not implement.
    UnsupportedVersion(u8),
    /// A record ran off the end of the buffer — the file is truncated or lying about a length.
    Truncated {
        offset: usize,
        want: usize,
        have: usize,
    },
    /// A chunk uses a compression codec this crate deliberately does not link.
    UnsupportedCompression(String),
    /// A chunk's stored CRC32 does not match its contents.
    ChunkCrcMismatch {
        expected: u32,
        actual: u32,
    },
    /// A string field was not valid UTF-8.
    BadUtf8 {
        offset: usize,
    },
    /// Schema or channel ids are exhausted (they are `u16`).
    IdSpaceExhausted,
    /// A message referenced a channel that was never declared.
    UnknownChannel(u16),
    Io(std::io::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::BadMagic { at } => write!(f, "not an MCAP file: bad magic at {at}"),
            Error::UnsupportedVersion(v) => {
                write!(f, "MCAP major version {v} is newer than this reader (0)")
            }
            Error::Truncated { offset, want, have } => write!(
                f,
                "truncated at byte {offset}: record wants {want} bytes, {have} remain"
            ),
            Error::UnsupportedCompression(c) => write!(
                f,
                "chunk compression {c:?} is not linked by this crate (it writes and reads \
                 uncompressed chunks so it can build for wasm32); decompress upstream"
            ),
            Error::ChunkCrcMismatch { expected, actual } => write!(
                f,
                "chunk CRC32 mismatch: stored {expected:#010x}, computed {actual:#010x}"
            ),
            Error::BadUtf8 { offset } => write!(f, "invalid UTF-8 in string at byte {offset}"),
            Error::IdSpaceExhausted => write!(f, "more than 65535 schemas or channels"),
            Error::UnknownChannel(id) => write!(f, "message references undeclared channel {id}"),
            Error::Io(e) => write!(f, "io: {e}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

// ---------------------------------------------------------------------------
// Little-endian primitives. MCAP is little-endian everywhere; strings, arrays and
// maps are all length-prefixed in *bytes*, which is what makes a record skippable
// without understanding it.
// ---------------------------------------------------------------------------

pub(crate) fn put_u8(o: &mut Vec<u8>, v: u8) {
    o.push(v);
}
pub(crate) fn put_u16(o: &mut Vec<u8>, v: u16) {
    o.extend_from_slice(&v.to_le_bytes());
}
pub(crate) fn put_u32(o: &mut Vec<u8>, v: u32) {
    o.extend_from_slice(&v.to_le_bytes());
}
pub(crate) fn put_u64(o: &mut Vec<u8>, v: u64) {
    o.extend_from_slice(&v.to_le_bytes());
}
pub(crate) fn put_str(o: &mut Vec<u8>, s: &str) {
    put_u32(o, s.len() as u32);
    o.extend_from_slice(s.as_bytes());
}
pub(crate) fn put_bytes(o: &mut Vec<u8>, b: &[u8]) {
    put_u32(o, b.len() as u32);
    o.extend_from_slice(b);
}
pub(crate) fn put_map_ss(o: &mut Vec<u8>, m: &[(String, String)]) {
    let mut body = Vec::new();
    for (k, v) in m {
        put_str(&mut body, k);
        put_str(&mut body, v);
    }
    put_bytes(o, &body);
}
pub(crate) fn put_map_u16_u64(o: &mut Vec<u8>, m: &BTreeMap<u16, u64>) {
    let mut body = Vec::new();
    for (k, v) in m {
        put_u16(&mut body, *k);
        put_u64(&mut body, *v);
    }
    put_bytes(o, &body);
}

/// A cursor that refuses to read past the end and says exactly where it stopped.
pub(crate) struct Cur<'a> {
    pub buf: &'a [u8],
    pub pos: usize,
}

impl<'a> Cur<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Cur { buf, pos: 0 }
    }
    pub fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        if self.remaining() < n {
            return Err(Error::Truncated {
                offset: self.pos,
                want: n,
                have: self.remaining(),
            });
        }
        let s = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }
    pub fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    pub fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    pub fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    pub fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    pub fn string(&mut self) -> Result<String> {
        let n = self.u32()? as usize;
        let at = self.pos;
        let b = self.take(n)?;
        String::from_utf8(b.to_vec()).map_err(|_| Error::BadUtf8 { offset: at })
    }
    pub fn bytes32(&mut self) -> Result<Vec<u8>> {
        let n = self.u32()? as usize;
        Ok(self.take(n)?.to_vec())
    }
    pub fn rest(&mut self) -> &'a [u8] {
        let s = &self.buf[self.pos..];
        self.pos = self.buf.len();
        s
    }
    pub fn map_ss(&mut self) -> Result<Vec<(String, String)>> {
        let n = self.u32()? as usize;
        let body = self.take(n)?;
        let mut c = Cur::new(body);
        let mut out = Vec::new();
        while c.remaining() > 0 {
            let k = c.string()?;
            let v = c.string()?;
            out.push((k, v));
        }
        Ok(out)
    }
    pub fn map_u16_u64(&mut self) -> Result<BTreeMap<u16, u64>> {
        let n = self.u32()? as usize;
        let body = self.take(n)?;
        let mut c = Cur::new(body);
        let mut out = BTreeMap::new();
        while c.remaining() > 0 {
            let k = c.u16()?;
            let v = c.u64()?;
            out.insert(k, v);
        }
        Ok(out)
    }
    pub fn skip(&mut self, n: usize) -> Result<()> {
        self.take(n).map(|_| ())
    }
}
