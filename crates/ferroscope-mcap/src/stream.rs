//! Reading a recording without holding it.
//!
//! [`read`](crate::read) takes the whole file as a slice and returns everything in it, which is
//! the right shape in a browser — the recording arrives as one `ArrayBuffer` and there is no
//! handle to seek — and the wrong shape for a file larger than the machine. Measured, that
//! reader costs about **2.1× the file in memory**, so a 2.6 GB recording wanted 5.5 GB and a
//! 48 GB machine started compressing memory rather than holding it.
//!
//! This is the other shape: records come off one at a time and each is handed to a visitor,
//! keeping only the record in hand. The questions that are *folds* — recomputing a receipt,
//! totalling a ledger, striding lanes down to what a screen draws — need nothing more, and they
//! are exactly the questions worth asking of a recording too big to open.
//!
//! What it costs to keep memory flat is the ability to look backwards: a visitor sees each
//! record once, in file order, and anything it wants later it must keep itself.
//!
//! # Pull and push
//!
//! [`stream`] PULLS from a [`Read`], which is what a file on disk offers. A browser cannot
//! offer that: `File.slice(a, b).arrayBuffer()` hands over a block of bytes when it is ready
//! and there is no blocking read to implement `Read` with. So the framing lives in [`Feed`],
//! which is PUSHED bytes and drains whole records out of them, and [`stream`] is a thin loop
//! over it. One implementation of what a record is, for both directions — this project has
//! twice shipped a defect that existed on several surfaces because one question had several
//! implementations, and record framing is not going to be the third.

use std::io::Read;

use crate::{Attachment, Channel, Cur, Error, MAGIC, Result, Schema, op};

/// How much [`stream`] pulls from the reader at a time.
const READ_BLOCK: usize = 64 << 10;

/// Compact the feed's buffer once this much of it has been consumed, so a long recording does
/// not pay a memmove per record.
const COMPACT_AT: usize = 64 << 10;

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

/// The framing, driven by whoever has the bytes.
///
/// Push a block of a recording in with [`push`](Feed::push) and take whole records out with
/// [`drain`](Feed::drain), as many times as it takes; the blocks need not fall on record
/// boundaries and a record may be split across any number of them. This is what [`stream`] is
/// built from, and it is what a browser needs: `File.slice()` yields blocks, so the reader has
/// to be the thing that is pushed rather than the thing that pulls.
///
/// Memory is bounded by the largest single record rather than by the recording — plus one
/// block of slack, since a block that ends mid-record is held until the rest of it arrives. An
/// attachment is the one record that can be arbitrarily large, and it is materialised, because
/// a visitor that wants a mesh wants all of it.
#[derive(Default)]
pub struct Feed {
    buf: Vec<u8>,
    /// How much of `buf` has been handed to a visitor already.
    head: usize,
    /// The opening magic has been checked.
    started: bool,
    /// The footer has been seen, or a visitor said [`Flow::Stop`]. Nothing more is read.
    done: bool,
    records: u64,
    bytes: u64,
}

impl Feed {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add the next block of the recording. Blocks may be any size and need not align to
    /// anything; a zero-length block is a no-op, not an end of stream.
    pub fn push(&mut self, block: &[u8]) {
        if self.done || block.is_empty() {
            return;
        }
        self.bytes += block.len() as u64;
        self.buf.extend_from_slice(block);
    }

    /// Hand every whole record now buffered to `visit`, in file order.
    ///
    /// Returns [`Flow::Stop`] once the footer has been reached or a visitor has asked to stop;
    /// after that the feed is finished and further pushes are ignored. Returns
    /// [`Flow::Continue`] when the buffered bytes run out mid-record — push more and call again.
    pub fn drain(&mut self, visit: &mut dyn FnMut(Record<'_>) -> Result<Flow>) -> Result<Flow> {
        if self.done {
            return Ok(Flow::Stop);
        }
        if !self.started {
            if self.buf.len() - self.head < MAGIC.len() {
                return Ok(Flow::Continue); // not enough to judge yet
            }
            let magic = &self.buf[self.head..self.head + MAGIC.len()];
            if magic[..5] != MAGIC[..5] || magic[6..8] != MAGIC[6..8] {
                self.done = true;
                return Err(Error::BadMagic { at: "start" });
            }
            if magic[5] != MAGIC[5] {
                self.done = true;
                return Err(Error::UnsupportedVersion(magic[5].saturating_sub(b'0')));
            }
            self.head += MAGIC.len();
            self.started = true;
        }

        loop {
            let available = self.buf.len() - self.head;
            if available < 9 {
                break;
            }
            let opcode = self.buf[self.head];
            let len = u64::from_le_bytes(
                self.buf[self.head + 1..self.head + 9]
                    .try_into()
                    .expect("nine bytes are available"),
            );
            // A length that does not fit in this machine's addresses cannot be a record here:
            // on wasm32 `as usize` would truncate 2^32 to 0 and read the next record as a body.
            let Ok(len) = usize::try_from(len) else {
                self.done = true;
                return Err(Error::Truncated {
                    offset: self.head,
                    want: usize::MAX,
                    have: available - 9,
                });
            };
            if available - 9 < len {
                break; // the body is still arriving
            }
            let at = self.head + 9;
            // Advance BEFORE visiting, so a stop or an error leaves the cursor past the record
            // rather than on it.
            self.head = at + len;
            self.records += 1;
            let body = &self.buf[at..at + len];

            let flow = match opcode {
                op::HEADER => {
                    let mut b = Cur::new(body);
                    let profile = b.string()?;
                    let library = b.string()?;
                    visit(Record::Header {
                        profile: &profile,
                        library: &library,
                    })?
                }
                op::SCHEMA => visit(Record::Schema(crate::read::parse_schema(body)?))?,
                op::CHANNEL => visit(Record::Channel(crate::read::parse_channel(body)?))?,
                op::MESSAGE => visit(Record::Message(message_ref(body)?))?,
                op::ATTACHMENT => visit(Record::Attachment(crate::read::parse_attachment(body)?))?,
                op::METADATA => {
                    let mut b = Cur::new(body);
                    let name = b.string()?;
                    let kv = b.map_ss()?;
                    visit(Record::Metadata { name, kv })?
                }
                op::CHUNK => stream_chunk(body, visit)?,
                op::FOOTER => Flow::Stop,
                other => visit(Record::Other { opcode: other })?,
            };
            if flow == Flow::Stop {
                self.done = true;
                self.compact();
                return Ok(Flow::Stop);
            }
        }
        self.compact();
        Ok(Flow::Continue)
    }

    /// Say that the recording has ended.
    ///
    /// A file that stops in the middle of a record is truncated and this reports it. What is
    /// NOT an error is a missing closing magic: a recording still being written has no footer
    /// yet, and reading one is the point of [`crate::read_prefix`].
    pub fn end(&self) -> Result<()> {
        if self.done {
            return Ok(());
        }
        if !self.started {
            return Err(Error::BadMagic { at: "start" });
        }
        let dangling = self.buf.len() - self.head;
        // Fewer than nine bytes is not yet a record header — a torn tail, which is what the
        // last block of a growing file looks like. Nine or more is a header whose body never
        // arrived, which is a file that stops mid-record.
        if dangling >= 9 {
            let len = u64::from_le_bytes(
                self.buf[self.head + 1..self.head + 9]
                    .try_into()
                    .expect("nine bytes are available"),
            );
            return Err(Error::Truncated {
                offset: self.head,
                want: usize::try_from(len).unwrap_or(usize::MAX),
                have: dangling - 9,
            });
        }
        Ok(())
    }

    /// How many records have been handed to a visitor.
    pub fn records(&self) -> u64 {
        self.records
    }

    /// How many bytes have been pushed in.
    pub fn bytes(&self) -> u64 {
        self.bytes
    }

    /// How many pushed bytes are still held because they are not yet a whole record. This is
    /// the feed's memory, and on a well-formed recording it stays around one record.
    pub fn buffered(&self) -> usize {
        self.buf.len() - self.head
    }

    /// Whether the footer has been reached or a visitor asked to stop.
    pub fn finished(&self) -> bool {
        self.done
    }

    fn compact(&mut self) {
        if self.head == 0 {
            return;
        }
        if self.done {
            self.buf.clear();
            self.buf.shrink_to_fit();
            self.head = 0;
            return;
        }
        // Amortised: move the tail down only once the consumed prefix is worth a memmove,
        // rather than after every record.
        if self.head >= COMPACT_AT {
            self.buf.drain(..self.head);
            self.head = 0;
        }
    }
}

/// Read a recording from a stream, handing every record to `visit` in file order.
///
/// The pulling half of [`Feed`]: blocks come off the reader and go straight in. Memory is
/// bounded by the largest single record rather than by the file, and the Ferroscope recorder
/// targets 64 KiB chunks.
///
/// The closing magic is *not* required, so this reads a file still being written — the same
/// relaxation [`read_prefix`](crate::read_prefix) makes, for the same reason.
pub fn stream<R: Read, F>(mut r: R, mut visit: F) -> Result<u64>
where
    F: FnMut(Record<'_>) -> Result<Flow>,
{
    let mut feed = Feed::new();
    let mut block = vec![0u8; READ_BLOCK];
    loop {
        let n = match r.read(&mut block) {
            Ok(0) => break,
            Ok(n) => n,
            Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(Error::Io(e)),
        };
        feed.push(&block[..n]);
        if feed.drain(&mut visit)? == Flow::Stop {
            return Ok(feed.records());
        }
    }
    feed.end()?;
    Ok(feed.records())
}

/// Walk the records inside one chunk. The chunk itself is already in memory — bounded by the
/// writer's chunk target — so this is an ordinary slice walk.
fn stream_chunk(body: &[u8], visit: &mut dyn FnMut(Record<'_>) -> Result<Flow>) -> Result<Flow> {
    let mut b = Cur::new(body);
    let _start = b.u64()?;
    let _end = b.u64()?;
    let uncompressed_size = b.u64()?;
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
    let stored = &b.buf[b.pos..b.pos + n];
    // Borrowed when the chunk is stored plainly, owned when a codec decoded it. The CRC below is
    // the MCAP `uncompressed_crc`, so it must be taken over these bytes either way.
    let decoded = crate::decompress::chunk_records(&compression, stored, uncompressed_size)?;
    let records: &[u8] = &decoded;

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
