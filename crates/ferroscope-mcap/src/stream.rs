//! Reading a recording without holding it.
//!
//! [`read`](crate::read) takes the whole file as a slice and returns everything in it, which is
//! the right shape in a browser — the recording arrives as one `ArrayBuffer` and there is no
//! handle to seek — and the wrong shape for a file larger than the machine. Measured, that
//! reader costs about **2.1× the file in memory**, so a 2.6 GB recording wanted 5.5 GB and a
//! 48 GB machine started compressing memory rather than holding it.
//!
//! This is the other shape: pull records off a [`Read`] one at a time and hand each to a
//! visitor, keeping only the record in hand. The questions that are *folds* — recomputing a
//! receipt, totalling a ledger, striding lanes down to what a screen draws — need nothing more,
//! and they are exactly the questions worth asking of a recording too big to open.
//!
//! What it costs to keep memory flat is the ability to look backwards: a visitor sees each
//! record once, in file order, and anything it wants later it must keep itself.

use std::io::Read;

use crate::{Attachment, Channel, Cur, Error, MAGIC, Result, Schema, op};

/// One record, borrowed for the duration of the call.
///
/// Borrowed rather than owned on purpose: an owned enum would allocate per record and put the
/// allocator back in the hot path this exists to keep flat.
#[derive(Debug)]
pub enum Record<'a> {
    Header {
        profile: &'a str,
        library: &'a str,
    },
    Schema(Schema),
    Channel(Channel),
    /// A message whose payload is BORROWED from the reader's buffer.
    ///
    /// Owned would mean one allocation per message, in the hot loop of the one reader that
    /// exists to keep memory flat — measured at 2.7 million allocations for a 552 MB file.
    Message(MessageRef<'a>),
    /// Attachments can be large — a glTF mesh — so the bytes are handed over borrowed and the
    /// visitor decides whether to keep them.
    Attachment(Attachment),
    Metadata {
        name: String,
        kv: Vec<(String, String)>,
    },
    /// Anything this crate does not model, named by opcode so a visitor can count it.
    Other {
        opcode: u8,
    },
}

/// A message with its payload still in the reader's buffer.
#[derive(Debug)]
pub struct MessageRef<'a> {
    pub channel_id: u16,
    pub sequence: u32,
    pub log_time: u64,
    pub publish_time: u64,
    pub data: &'a [u8],
}

/// What a visitor wants to happen next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flow {
    /// Keep reading.
    Continue,
    /// Stop here and return successfully. The footer has been reached, or the visitor has
    /// everything it needs and the rest of the file is not worth the read.
    Stop,
}

/// Read a recording from a stream, handing every record to `visit` in file order.
///
/// Memory is bounded by the largest single record rather than by the file: chunks are decoded
/// into a reusable buffer, and the Ferroscope recorder targets 64 KiB chunks. An attachment is
/// the one record that can be arbitrarily large, and it is materialised because a visitor that
/// wants a mesh wants all of it.
///
/// The closing magic is *not* required, so this reads a file still being written — the same
/// relaxation [`read_prefix`](crate::read_prefix) makes, for the same reason.
pub fn stream<R: Read, F>(mut r: R, mut visit: F) -> Result<u64>
where
    F: FnMut(Record<'_>) -> Result<Flow>,
{
    let mut magic = [0u8; 8];
    read_exact_or(&mut r, &mut magic, "start")?;
    if magic[..5] != MAGIC[..5] || magic[6..8] != MAGIC[6..8] {
        return Err(Error::BadMagic { at: "start" });
    }
    if magic[5] != MAGIC[5] {
        return Err(Error::UnsupportedVersion(magic[5].saturating_sub(b'0')));
    }

    // One buffer, reused for every record. This is the whole memory story.
    let mut body: Vec<u8> = Vec::new();
    let mut records = 0u64;

    loop {
        let mut hdr = [0u8; 9];
        match r.read(&mut hdr[..1]) {
            Ok(0) => return Ok(records), // clean end of stream
            Ok(_) => {}
            Err(e) => return Err(Error::Io(e)),
        }
        // The closing magic begins with the same byte as no opcode this crate emits, but a
        // truncated read is the honest signal: try to fill the rest of the header and stop if
        // the stream ends inside it.
        if fill(&mut r, &mut hdr[1..9])? == 0 {
            return Ok(records);
        }
        let opcode = hdr[0];
        let len =
            usize::try_from(u64::from_le_bytes(hdr[1..9].try_into().unwrap())).map_err(|_| {
                Error::Truncated {
                    offset: 0,
                    want: usize::MAX,
                    have: 0,
                }
            })?;

        body.clear();
        body.try_reserve(len).map_err(|_| Error::Truncated {
            offset: 0,
            want: len,
            have: 0,
        })?;
        body.resize(len, 0);
        if fill(&mut r, &mut body)? != len {
            return Err(Error::Truncated {
                offset: 0,
                want: len,
                have: body.len(),
            });
        }
        records += 1;

        let flow = match opcode {
            op::HEADER => {
                let mut b = Cur::new(&body);
                let profile = b.string()?;
                let library = b.string()?;
                visit(Record::Header {
                    profile: &profile,
                    library: &library,
                })?
            }
            op::SCHEMA => visit(Record::Schema(crate::read::parse_schema(&body)?))?,
            op::CHANNEL => visit(Record::Channel(crate::read::parse_channel(&body)?))?,
            op::MESSAGE => visit(Record::Message(message_ref(&body)?))?,
            op::ATTACHMENT => visit(Record::Attachment(crate::read::parse_attachment(&body)?))?,
            op::METADATA => {
                let mut b = Cur::new(&body);
                let name = b.string()?;
                let kv = b.map_ss()?;
                visit(Record::Metadata { name, kv })?
            }
            op::CHUNK => stream_chunk(&body, &mut visit)?,
            op::FOOTER => Flow::Stop,
            other => visit(Record::Other { opcode: other })?,
        };
        if flow == Flow::Stop {
            return Ok(records);
        }
    }
}

/// Walk the records inside one chunk. The chunk itself is already in memory — bounded by the
/// writer's chunk target — so this is an ordinary slice walk.
fn stream_chunk<F>(body: &[u8], visit: &mut F) -> Result<Flow>
where
    F: FnMut(Record<'_>) -> Result<Flow>,
{
    let mut b = Cur::new(body);
    let _start = b.u64()?;
    let _end = b.u64()?;
    let _uncompressed = b.u64()?;
    let stored_crc = b.u32()?;
    let compression = b.string()?;
    let n = b.u64()? as usize;
    if b.remaining() < n {
        return Err(Error::Truncated {
            offset: b.pos,
            want: n,
            have: b.remaining(),
        });
    }
    let records = &b.buf[b.pos..b.pos + n];
    if !(compression.is_empty() || compression == "none") {
        return Err(Error::UnsupportedCompression(compression));
    }
    if stored_crc != 0 {
        let actual = crate::crc32(records);
        if actual != stored_crc {
            return Err(Error::ChunkCrcMismatch {
                expected: stored_crc,
                actual,
            });
        }
    }

    let mut inner = Cur::new(records);
    while inner.remaining() >= 9 {
        let opcode = inner.u8()?;
        let len = inner.u64()? as usize;
        if inner.remaining() < len {
            return Err(Error::Truncated {
                offset: inner.pos,
                want: len,
                have: inner.remaining(),
            });
        }
        let rec = &inner.buf[inner.pos..inner.pos + len];
        inner.skip(len)?;
        let flow = match opcode {
            op::SCHEMA => visit(Record::Schema(crate::read::parse_schema(rec)?))?,
            op::CHANNEL => visit(Record::Channel(crate::read::parse_channel(rec)?))?,
            op::MESSAGE => visit(Record::Message(message_ref(rec)?))?,
            other => visit(Record::Other { opcode: other })?,
        };
        if flow == Flow::Stop {
            return Ok(Flow::Stop);
        }
    }
    Ok(Flow::Continue)
}

/// The message header, with the payload left where it lies.
fn message_ref(body: &[u8]) -> Result<MessageRef<'_>> {
    if body.len() < 22 {
        return Err(Error::Truncated {
            offset: 0,
            want: 22,
            have: body.len(),
        });
    }
    Ok(MessageRef {
        channel_id: u16::from_le_bytes(body[0..2].try_into().unwrap()),
        sequence: u32::from_le_bytes(body[2..6].try_into().unwrap()),
        log_time: u64::from_le_bytes(body[6..14].try_into().unwrap()),
        publish_time: u64::from_le_bytes(body[14..22].try_into().unwrap()),
        data: &body[22..],
    })
}

fn read_exact_or<R: Read>(r: &mut R, buf: &mut [u8], at: &'static str) -> Result<()> {
    if fill(r, buf)? != buf.len() {
        return Err(Error::BadMagic { at });
    }
    Ok(())
}

/// Read until `buf` is full or the stream ends; returns how many bytes landed.
fn fill<R: Read>(r: &mut R, buf: &mut [u8]) -> Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match r.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(Error::Io(e)),
        }
    }
    Ok(n)
}
