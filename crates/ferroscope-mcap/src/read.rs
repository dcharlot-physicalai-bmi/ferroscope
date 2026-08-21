//! The reader. Takes a whole file as `&[u8]` and returns everything in it.
//!
//! Reading from a slice rather than a `Read` is deliberate: in a browser the recording
//! arrives as an `ArrayBuffer` and there is no file handle to seek, and natively a memory
//! map is both faster and simpler. Records are validated as they are parsed — a truncated
//! file names the byte it died on instead of returning a plausible prefix.

use std::collections::BTreeMap;

use crate::{op, Attachment, Channel, Cur, Error, Message, Result, Schema, MAGIC};

/// The `Statistics` record from the summary section, when the writer produced one.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Statistics {
    pub message_count: u64,
    pub schema_count: u16,
    pub channel_count: u32,
    pub attachment_count: u32,
    pub metadata_count: u32,
    pub chunk_count: u32,
    pub message_start_time: u64,
    pub message_end_time: u64,
    pub channel_message_counts: BTreeMap<u16, u64>,
}

/// A parsed recording.
#[derive(Clone, Debug, Default)]
pub struct Log {
    pub profile: String,
    pub library: String,
    pub schemas: Vec<Schema>,
    pub channels: Vec<Channel>,
    pub messages: Vec<Message>,
    pub metadata: Vec<(String, Vec<(String, String)>)>,
    pub attachments: Vec<Attachment>,
    pub statistics: Option<Statistics>,
    /// The `DataEnd` checksum as stored. `0` means the writer declined to compute one.
    pub data_section_crc: Option<u32>,
}

impl Log {
    pub fn channel(&self, id: u16) -> Option<&Channel> {
        self.channels.iter().find(|c| c.id == id)
    }
    pub fn schema(&self, id: u16) -> Option<&Schema> {
        self.schemas.iter().find(|s| s.id == id)
    }
    pub fn channel_by_topic(&self, topic: &str) -> Option<&Channel> {
        self.channels.iter().find(|c| c.topic == topic)
    }
    /// Every message on one topic, in file order.
    pub fn messages_on<'a>(&'a self, topic: &'a str) -> impl Iterator<Item = &'a Message> + 'a {
        let id = self.channel_by_topic(topic).map(|c| c.id);
        self.messages
            .iter()
            .filter(move |m| Some(m.channel_id) == id)
    }
    /// One attachment by name.
    pub fn attachment(&self, name: &str) -> Option<&Attachment> {
        self.attachments.iter().find(|a| a.name == name)
    }

    /// Named metadata block, if present.
    pub fn metadata_block(&self, name: &str) -> Option<&[(String, String)]> {
        self.metadata
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, kv)| kv.as_slice())
    }
    /// `(first, last)` log times over all messages.
    pub fn time_span(&self) -> Option<(u64, u64)> {
        let mut it = self.messages.iter().map(|m| m.log_time);
        let first = it.next()?;
        Some(it.fold((first, first), |(lo, hi), t| (lo.min(t), hi.max(t))))
    }
}

/// Parse a complete MCAP file.
pub fn read(bytes: &[u8]) -> Result<Log> {
    read_inner(bytes, true)
}

/// One top-level record's byte range, with the simulation instant replay would pace it by.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RecordSpan {
    pub opcode: u8,
    /// Absolute range in the file, including the record's own 9-byte opcode+length header.
    pub start: usize,
    pub end: usize,
    /// A `Message`'s `log_time`, or a `Chunk`'s `message_start_time` — when this record
    /// happened, for the records that happened at a simulation instant at all.
    pub log_time: Option<u64>,
}

/// The byte range of every top-level record in a complete file, in file order.
///
/// The contract that makes replay possible: the spans tile the file exactly, so
/// `magic + spans + magic` reassembles it byte for byte. Anything less — a gap, an overlap, a
/// file that ends mid-record — is an error here rather than a silently different stream later.
pub fn record_spans(bytes: &[u8]) -> Result<Vec<RecordSpan>> {
    if bytes.len() < 16 || bytes[..8] != MAGIC {
        return Err(Error::BadMagic { at: "start" });
    }
    if bytes[bytes.len() - 8..] != MAGIC {
        return Err(Error::BadMagic { at: "end" });
    }
    let body_end = bytes.len() - 8;
    let mut spans = Vec::new();
    let mut pos = 8;
    while pos < body_end {
        if body_end - pos < 9 {
            return Err(Error::Truncated {
                offset: pos,
                want: 9,
                have: body_end - pos,
            });
        }
        let opcode = bytes[pos];
        // try_from, not `as`: on wasm32 — where this crate runs, on bytes a page was handed —
        // `u64 as usize` keeps the low 32 bits, so a declared length of exactly 2^32 becomes 0
        // and the parser walks into a record's payload as though it were top-level records.
        // The same file would then tile one way natively and another way in the browser.
        let len = match usize::try_from(u64::from_le_bytes(
            bytes[pos + 1..pos + 9].try_into().unwrap(),
        )) {
            Ok(n) => n,
            Err(_) => {
                return Err(Error::Truncated {
                    offset: pos + 9,
                    want: usize::MAX,
                    have: body_end - pos - 9,
                })
            }
        };
        if body_end - pos - 9 < len {
            return Err(Error::Truncated {
                offset: pos + 9,
                want: len,
                have: body_end - pos - 9,
            });
        }
        let body = &bytes[pos + 9..pos + 9 + len];
        let log_time = match opcode {
            // Message: channel_id u16, sequence u32, then log_time.
            op::MESSAGE if body.len() >= 14 => {
                Some(u64::from_le_bytes(body[6..14].try_into().unwrap()))
            }
            // Chunk: message_start_time leads the record.
            op::CHUNK if body.len() >= 8 => Some(u64::from_le_bytes(body[..8].try_into().unwrap())),
            _ => None,
        };
        spans.push(RecordSpan {
            opcode,
            start: pos,
            end: pos + 9 + len,
            log_time,
        });
        pos += 9 + len;
    }
    Ok(spans)
}

/// Read a growing prefix of a recording, as a live viewer holds one.
///
/// The same parser with two relaxations: the closing magic is not required, and parsing stops
/// cleanly at the end of whatever has arrived. A buffer that ends mid-record is still refused —
/// a live stream frames whole records, so a torn record is corruption, not patience.
pub fn read_prefix(bytes: &[u8]) -> Result<Log> {
    read_inner(bytes, false)
}

fn read_inner(bytes: &[u8], require_end_magic: bool) -> Result<Log> {
    // Split the version byte out of the magic check so a future MCAP 1 file gets told what
    // is wrong with it instead of "not an MCAP file".
    if bytes.len() < 16 || bytes[..5] != MAGIC[..5] || bytes[6..8] != MAGIC[6..8] {
        return Err(Error::BadMagic { at: "start" });
    }
    if bytes[5] != MAGIC[5] {
        return Err(Error::UnsupportedVersion(bytes[5].saturating_sub(b'0')));
    }
    if require_end_magic && bytes[bytes.len() - 8..] != MAGIC {
        return Err(Error::BadMagic { at: "end" });
    }

    let mut log = Log::default();
    let mut c = Cur::new(bytes);
    c.skip(8)?;

    while c.remaining() >= 9 {
        let opcode = c.u8()?;
        let len = c.u64()? as usize;
        if c.remaining() < len {
            return Err(Error::Truncated {
                offset: c.pos,
                want: len,
                have: c.remaining(),
            });
        }
        let body = &c.buf[c.pos..c.pos + len];
        c.skip(len)?;

        match opcode {
            op::HEADER => {
                let mut b = Cur::new(body);
                log.profile = b.string()?;
                log.library = b.string()?;
            }
            op::SCHEMA => push_schema(&mut log, parse_schema(body)?),
            op::CHANNEL => push_channel(&mut log, parse_channel(body)?),
            op::MESSAGE => log.messages.push(parse_message(body)?),
            op::CHUNK => read_chunk(&mut log, body)?,
            op::METADATA => {
                let mut b = Cur::new(body);
                let name = b.string()?;
                let kv = b.map_ss()?;
                log.metadata.push((name, kv));
            }
            op::ATTACHMENT => log.attachments.push(parse_attachment(body)?),
            op::STATISTICS => log.statistics = Some(parse_statistics(body)?),
            op::DATA_END => {
                let mut b = Cur::new(body);
                log.data_section_crc = Some(b.u32()?);
            }
            op::FOOTER => break,
            // Indexes and attachments are skipped: this reader loads everything anyway, so
            // an index would only tell it what it already has.
            _ => {}
        }
    }

    Ok(log)
}

fn push_schema(log: &mut Log, s: Schema) {
    // Schemas appear twice in an indexed file (data section and summary). Keep the first.
    if !log.schemas.iter().any(|e| e.id == s.id) {
        log.schemas.push(s);
    }
}

fn push_channel(log: &mut Log, ch: Channel) {
    if !log.channels.iter().any(|e| e.id == ch.id) {
        log.channels.push(ch);
    }
}

fn parse_schema(body: &[u8]) -> Result<Schema> {
    let mut b = Cur::new(body);
    Ok(Schema {
        id: b.u16()?,
        name: b.string()?,
        encoding: b.string()?,
        data: b.bytes32()?,
    })
}

fn parse_channel(body: &[u8]) -> Result<Channel> {
    let mut b = Cur::new(body);
    Ok(Channel {
        id: b.u16()?,
        schema_id: b.u16()?,
        topic: b.string()?,
        message_encoding: b.string()?,
        metadata: b.map_ss()?,
    })
}

fn parse_message(body: &[u8]) -> Result<Message> {
    let mut b = Cur::new(body);
    Ok(Message {
        channel_id: b.u16()?,
        sequence: b.u32()?,
        log_time: b.u64()?,
        publish_time: b.u64()?,
        data: b.rest().to_vec(),
    })
}

fn parse_attachment(body: &[u8]) -> Result<Attachment> {
    let mut b = Cur::new(body);
    let log_time = b.u64()?;
    let create_time = b.u64()?;
    let name = b.string()?;
    let media_type = b.string()?;
    let n = b.u64()? as usize;
    if b.remaining() < n + 4 {
        return Err(Error::Truncated {
            offset: b.pos,
            want: n + 4,
            have: b.remaining(),
        });
    }
    let data = b.buf[b.pos..b.pos + n].to_vec();
    b.skip(n)?;
    let stored = b.u32()?;
    if stored != 0 {
        // The CRC covers the record's own fields from log_time through data.
        let actual = crate::crc32(&body[..body.len() - 4]);
        if actual != stored {
            return Err(Error::AttachmentCrcMismatch {
                name,
                expected: stored,
                actual,
            });
        }
    }
    Ok(Attachment {
        log_time,
        create_time,
        name,
        media_type,
        data,
    })
}

fn parse_statistics(body: &[u8]) -> Result<Statistics> {
    let mut b = Cur::new(body);
    Ok(Statistics {
        message_count: b.u64()?,
        schema_count: b.u16()?,
        channel_count: b.u32()?,
        attachment_count: b.u32()?,
        metadata_count: b.u32()?,
        chunk_count: b.u32()?,
        message_start_time: b.u64()?,
        message_end_time: b.u64()?,
        channel_message_counts: b.map_u16_u64()?,
    })
}

fn read_chunk(log: &mut Log, body: &[u8]) -> Result<()> {
    let mut b = Cur::new(body);
    let _start = b.u64()?;
    let _end = b.u64()?;
    let _uncompressed_size = b.u64()?;
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
        match opcode {
            op::SCHEMA => push_schema(log, parse_schema(rec)?),
            op::CHANNEL => push_channel(log, parse_channel(rec)?),
            op::MESSAGE => log.messages.push(parse_message(rec)?),
            _ => {}
        }
    }
    Ok(())
}
