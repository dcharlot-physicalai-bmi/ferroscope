//! The reader. Takes a whole file as `&[u8]` and returns everything in it.
//!
//! Reading from a slice rather than a `Read` is deliberate: in a browser the recording
//! arrives as an `ArrayBuffer` and there is no file handle to seek, and natively a memory
//! map is both faster and simpler. Records are validated as they are parsed — a truncated
//! file names the byte it died on instead of returning a plausible prefix.

use std::collections::BTreeMap;

use crate::{op, Channel, Cur, Error, Message, Result, Schema, MAGIC};

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
    // Split the version byte out of the magic check so a future MCAP 1 file gets told what
    // is wrong with it instead of "not an MCAP file".
    if bytes.len() < 16 || bytes[..5] != MAGIC[..5] || bytes[6..8] != MAGIC[6..8] {
        return Err(Error::BadMagic { at: "start" });
    }
    if bytes[5] != MAGIC[5] {
        return Err(Error::UnsupportedVersion(bytes[5].saturating_sub(b'0')));
    }
    if bytes[bytes.len() - 8..] != MAGIC {
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
