//! The writer. Emits a fully indexed, fully summarized MCAP file: chunked data section,
//! per-channel message indexes, chunk index, statistics, summary offsets, and both CRCs.
//! An indexed file is what lets a viewer seek without reading the whole recording, which is
//! the difference between opening a 40 GB log in a browser tab and not opening it at all.

use std::collections::BTreeMap;
use std::io::Write;

use crate::{Channel, Crc32, Error, MAGIC, Result, Schema, op};
use crate::{put_bytes, put_map_ss, put_map_u16_u64, put_str, put_u8, put_u16, put_u32, put_u64};

/// Writer configuration.
#[derive(Clone, Debug)]
pub struct WriterOptions {
    /// MCAP profile string (`""` for none). Ferroscope recordings use `ferroscope`.
    pub profile: String,
    /// Free-form producer identification, e.g. `ferroscope 0.1.0`.
    pub library: String,
    /// Flush a chunk once its uncompressed records reach this many bytes.
    pub chunk_target: usize,
}

impl WriterOptions {
    pub fn new(profile: impl Into<String>, library: impl Into<String>) -> Self {
        WriterOptions {
            profile: profile.into(),
            library: library.into(),
            chunk_target: 1 << 20,
        }
    }
    pub fn chunk_target(mut self, bytes: usize) -> Self {
        self.chunk_target = bytes;
        self
    }
}

struct ChunkIndexRec {
    start: u64,
    end: u64,
    offset: u64,
    length: u64,
    index_offsets: BTreeMap<u16, u64>,
    index_length: u64,
    size: u64,
}

/// Writes MCAP to any [`std::io::Write`] sink.
pub struct Writer<W: Write> {
    sink: W,
    pos: u64,
    started: bool,
    opts: WriterOptions,

    data_crc: Crc32,
    sum_crc: Crc32,
    in_data: bool,
    in_summary: bool,

    schemas: Vec<Schema>,
    channels: Vec<Channel>,

    chunk: Vec<u8>,
    chunk_start: u64,
    chunk_end: u64,
    chunk_index: BTreeMap<u16, Vec<(u64, u64)>>,
    chunk_indexes: Vec<ChunkIndexRec>,

    metadata_index: Vec<(String, u64, u64)>,
    attachment_index: Vec<(String, String, u64, u64, u64, u64, u64)>,

    msg_count: u64,
    per_channel: BTreeMap<u16, u64>,
    t_min: u64,
    t_max: u64,
}

impl<W: Write> Writer<W> {
    pub fn new(sink: W, opts: WriterOptions) -> Self {
        Writer {
            sink,
            pos: 0,
            started: false,
            opts,
            data_crc: Crc32::new(),
            sum_crc: Crc32::new(),
            in_data: true,
            in_summary: false,
            schemas: Vec::new(),
            channels: Vec::new(),
            chunk: Vec::new(),
            chunk_start: u64::MAX,
            chunk_end: 0,
            chunk_index: BTreeMap::new(),
            chunk_indexes: Vec::new(),
            metadata_index: Vec::new(),
            attachment_index: Vec::new(),
            msg_count: 0,
            per_channel: BTreeMap::new(),
            t_min: u64::MAX,
            t_max: 0,
        }
    }

    fn raw(&mut self, bytes: &[u8]) -> Result<()> {
        self.sink.write_all(bytes)?;
        self.pos += bytes.len() as u64;
        if self.in_data {
            self.data_crc.update(bytes);
        }
        if self.in_summary {
            self.sum_crc.update(bytes);
        }
        Ok(())
    }

    fn record(&mut self, opcode: u8, body: &[u8]) -> Result<()> {
        let mut head = [0u8; 9];
        head[0] = opcode;
        head[1..].copy_from_slice(&(body.len() as u64).to_le_bytes());
        self.raw(&head)?;
        self.raw(body)
    }

    fn ensure_started(&mut self) -> Result<()> {
        if self.started {
            return Ok(());
        }
        self.started = true;
        self.raw(&MAGIC)?;
        let mut b = Vec::new();
        put_str(&mut b, &self.opts.profile.clone());
        put_str(&mut b, &self.opts.library.clone());
        self.record(op::HEADER, &b)
    }

    /// Declare a message type. Returns the schema id to pass to [`Writer::add_channel`].
    pub fn add_schema(&mut self, name: &str, encoding: &str, data: &[u8]) -> Result<u16> {
        self.ensure_started()?;
        let id = u16::try_from(self.schemas.len() + 1).map_err(|_| Error::IdSpaceExhausted)?;
        let s = Schema {
            id,
            name: name.to_string(),
            encoding: encoding.to_string(),
            data: data.to_vec(),
        };
        let mut b = Vec::new();
        put_u16(&mut b, s.id);
        put_str(&mut b, &s.name);
        put_str(&mut b, &s.encoding);
        put_bytes(&mut b, &s.data);
        self.record(op::SCHEMA, &b)?;
        self.schemas.push(s);
        Ok(id)
    }

    /// Declare a topic. Pass `schema_id = 0` for a channel with no declared schema.
    pub fn add_channel(
        &mut self,
        topic: &str,
        schema_id: u16,
        message_encoding: &str,
        metadata: &[(String, String)],
    ) -> Result<u16> {
        self.ensure_started()?;
        let id = u16::try_from(self.channels.len()).map_err(|_| Error::IdSpaceExhausted)?;
        let c = Channel {
            id,
            schema_id,
            topic: topic.to_string(),
            message_encoding: message_encoding.to_string(),
            metadata: metadata.to_vec(),
        };
        let mut b = Vec::new();
        put_u16(&mut b, c.id);
        put_u16(&mut b, c.schema_id);
        put_str(&mut b, &c.topic);
        put_str(&mut b, &c.message_encoding);
        put_map_ss(&mut b, &c.metadata);
        self.record(op::CHANNEL, &b)?;
        self.channels.push(c);
        Ok(id)
    }

    /// Append one message. Times are nanoseconds; `log_time` drives the index and must be
    /// non-decreasing within a chunk for the index to be useful (this writer does not sort).
    pub fn write_message(
        &mut self,
        channel_id: u16,
        sequence: u32,
        log_time: u64,
        publish_time: u64,
        data: &[u8],
    ) -> Result<()> {
        self.ensure_started()?;
        if !self.channels.iter().any(|c| c.id == channel_id) {
            return Err(Error::UnknownChannel(channel_id));
        }

        let mut b = Vec::new();
        put_u16(&mut b, channel_id);
        put_u32(&mut b, sequence);
        put_u64(&mut b, log_time);
        put_u64(&mut b, publish_time);
        b.extend_from_slice(data);

        let offset = self.chunk.len() as u64;
        self.chunk.push(op::MESSAGE);
        self.chunk
            .extend_from_slice(&(b.len() as u64).to_le_bytes());
        self.chunk.extend_from_slice(&b);

        self.chunk_index
            .entry(channel_id)
            .or_default()
            .push((log_time, offset));
        self.chunk_start = self.chunk_start.min(log_time);
        self.chunk_end = self.chunk_end.max(log_time);
        self.t_min = self.t_min.min(log_time);
        self.t_max = self.t_max.max(log_time);
        self.msg_count += 1;
        *self.per_channel.entry(channel_id).or_insert(0) += 1;

        if self.chunk.len() >= self.opts.chunk_target {
            self.flush_chunk()?;
        }
        Ok(())
    }

    /// Attach a named key/value block — where a Ferroscope run puts its determinism receipt,
    /// so the receipt travels inside the recording rather than beside it.
    pub fn write_metadata(&mut self, name: &str, kv: &[(String, String)]) -> Result<()> {
        self.ensure_started()?;
        self.flush_chunk()?;
        let mut b = Vec::new();
        put_str(&mut b, name);
        put_map_ss(&mut b, kv);
        let at = self.pos;
        self.record(op::METADATA, &b)?;
        self.metadata_index
            .push((name.to_string(), at, self.pos - at));
        Ok(())
    }

    /// Attach a blob. Attachments live in the data section outside any chunk, so a reader can
    /// seek straight to one from the summary without decompressing anything.
    ///
    /// `log_time` and `create_time` are nanoseconds; pass the same value for both when the
    /// distinction does not apply.
    pub fn write_attachment(
        &mut self,
        name: &str,
        media_type: &str,
        data: &[u8],
        log_time: u64,
        create_time: u64,
    ) -> Result<()> {
        self.ensure_started()?;
        self.flush_chunk()?;

        // The record's CRC covers its own fields from log_time through data, so the body is
        // built first and checksummed before the length prefix is known.
        let mut b = Vec::new();
        put_u64(&mut b, log_time);
        put_u64(&mut b, create_time);
        put_str(&mut b, name);
        put_str(&mut b, media_type);
        put_u64(&mut b, data.len() as u64);
        b.extend_from_slice(data);
        let crc = crate::crc32(&b);
        put_u32(&mut b, crc);

        let at = self.pos;
        self.record(op::ATTACHMENT, &b)?;
        self.attachment_index.push((
            name.to_string(),
            media_type.to_string(),
            at,
            self.pos - at,
            log_time,
            create_time,
            data.len() as u64,
        ));
        Ok(())
    }

    fn flush_chunk(&mut self) -> Result<()> {
        if self.chunk.is_empty() {
            return Ok(());
        }
        let records = std::mem::take(&mut self.chunk);
        let uncompressed_crc = crate::crc32(&records);

        let mut b = Vec::new();
        put_u64(&mut b, self.chunk_start);
        put_u64(&mut b, self.chunk_end);
        put_u64(&mut b, records.len() as u64);
        put_u32(&mut b, uncompressed_crc);
        put_str(&mut b, ""); // no compression: see the crate docs for why this is the default
        put_u64(&mut b, records.len() as u64);
        b.extend_from_slice(&records);

        let offset = self.pos;
        self.record(op::CHUNK, &b)?;
        let length = self.pos - offset;

        let index = std::mem::take(&mut self.chunk_index);
        let mut index_offsets = BTreeMap::new();
        let index_start = self.pos;
        for (cid, entries) in &index {
            index_offsets.insert(*cid, self.pos);
            let mut ib = Vec::new();
            put_u16(&mut ib, *cid);
            let mut arr = Vec::new();
            for (t, o) in entries {
                put_u64(&mut arr, *t);
                put_u64(&mut arr, *o);
            }
            put_bytes(&mut ib, &arr);
            self.record(op::MESSAGE_INDEX, &ib)?;
        }
        let index_length = self.pos - index_start;

        self.chunk_indexes.push(ChunkIndexRec {
            start: self.chunk_start,
            end: self.chunk_end,
            offset,
            length,
            index_offsets,
            index_length,
            size: records.len() as u64,
        });
        self.chunk_start = u64::MAX;
        self.chunk_end = 0;
        Ok(())
    }

    /// Close the file: flush, write the data-section CRC, the summary, the summary offsets,
    /// the footer, and the trailing magic. Returns the sink.
    pub fn finish(mut self) -> Result<W> {
        self.ensure_started()?;
        self.flush_chunk()?;

        // --- Data end. Its own bytes are not part of the checksum it carries.
        let data_crc = self.data_crc.finish();
        self.in_data = false;
        let mut b = Vec::new();
        put_u32(&mut b, data_crc);
        self.record(op::DATA_END, &b)?;

        // --- Summary section.
        self.in_summary = true;
        let summary_start = self.pos;

        let schema_start = self.pos;
        for s in std::mem::take(&mut self.schemas) {
            let mut b = Vec::new();
            put_u16(&mut b, s.id);
            put_str(&mut b, &s.name);
            put_str(&mut b, &s.encoding);
            put_bytes(&mut b, &s.data);
            self.record(op::SCHEMA, &b)?;
            self.schemas.push(s);
        }
        let schema_len = self.pos - schema_start;

        let channel_start = self.pos;
        for c in std::mem::take(&mut self.channels) {
            let mut b = Vec::new();
            put_u16(&mut b, c.id);
            put_u16(&mut b, c.schema_id);
            put_str(&mut b, &c.topic);
            put_str(&mut b, &c.message_encoding);
            put_map_ss(&mut b, &c.metadata);
            self.record(op::CHANNEL, &b)?;
            self.channels.push(c);
        }
        let channel_len = self.pos - channel_start;

        let ci_start = self.pos;
        for ci in std::mem::take(&mut self.chunk_indexes) {
            let mut b = Vec::new();
            put_u64(&mut b, ci.start);
            put_u64(&mut b, ci.end);
            put_u64(&mut b, ci.offset);
            put_u64(&mut b, ci.length);
            put_map_u16_u64(&mut b, &ci.index_offsets);
            put_u64(&mut b, ci.index_length);
            put_str(&mut b, "");
            put_u64(&mut b, ci.size);
            put_u64(&mut b, ci.size);
            self.record(op::CHUNK_INDEX, &b)?;
            self.chunk_indexes.push(ci);
        }
        let ci_len = self.pos - ci_start;

        let mi_start = self.pos;
        for (name, off, len) in std::mem::take(&mut self.metadata_index) {
            let mut b = Vec::new();
            put_u64(&mut b, off);
            put_u64(&mut b, len);
            put_str(&mut b, &name);
            self.record(op::METADATA_INDEX, &b)?;
            self.metadata_index.push((name, off, len));
        }
        let mi_len = self.pos - mi_start;

        let ai_start = self.pos;
        for a in std::mem::take(&mut self.attachment_index) {
            let mut b = Vec::new();
            put_u64(&mut b, a.2); // offset
            put_u64(&mut b, a.3); // length
            put_u64(&mut b, a.4); // log_time
            put_u64(&mut b, a.5); // create_time
            put_u64(&mut b, a.6); // data_size
            put_str(&mut b, &a.0);
            put_str(&mut b, &a.1);
            self.record(op::ATTACHMENT_INDEX, &b)?;
            self.attachment_index.push(a);
        }
        let ai_len = self.pos - ai_start;

        let stats_start = self.pos;
        {
            let mut b = Vec::new();
            put_u64(&mut b, self.msg_count);
            put_u16(&mut b, self.schemas.len() as u16);
            put_u32(&mut b, self.channels.len() as u32);
            put_u32(&mut b, self.attachment_index.len() as u32);
            put_u32(&mut b, self.metadata_index.len() as u32);
            put_u32(&mut b, self.chunk_indexes.len() as u32);
            put_u64(&mut b, if self.msg_count == 0 { 0 } else { self.t_min });
            put_u64(&mut b, if self.msg_count == 0 { 0 } else { self.t_max });
            let counts = std::mem::take(&mut self.per_channel);
            put_map_u16_u64(&mut b, &counts);
            self.per_channel = counts;
            self.record(op::STATISTICS, &b)?;
        }
        let stats_len = self.pos - stats_start;

        // --- Summary offset section: one record per group, so a reader can jump straight
        //     to the channels without parsing the chunk index.
        let summary_offset_start = self.pos;
        for (opcode, start, len) in [
            (op::SCHEMA, schema_start, schema_len),
            (op::CHANNEL, channel_start, channel_len),
            (op::CHUNK_INDEX, ci_start, ci_len),
            (op::METADATA_INDEX, mi_start, mi_len),
            (op::ATTACHMENT_INDEX, ai_start, ai_len),
            (op::STATISTICS, stats_start, stats_len),
        ] {
            if len == 0 {
                continue;
            }
            let mut b = Vec::new();
            put_u8(&mut b, opcode);
            put_u64(&mut b, start);
            put_u64(&mut b, len);
            self.record(op::SUMMARY_OFFSET, &b)?;
        }

        // --- Footer. The summary CRC covers the summary section up to and including the
        //     footer's `summary_offset_start` field, so it is written in two halves.
        let mut fb = Vec::new();
        put_u64(&mut fb, summary_start);
        put_u64(&mut fb, summary_offset_start);
        let mut head = [0u8; 9];
        head[0] = op::FOOTER;
        head[1..].copy_from_slice(&((fb.len() + 4) as u64).to_le_bytes());
        self.raw(&head)?;
        self.raw(&fb)?;
        let summary_crc = self.sum_crc.finish();
        self.in_summary = false;
        self.raw(&summary_crc.to_le_bytes())?;
        self.raw(&MAGIC)?;

        self.sink.flush()?;
        Ok(self.sink)
    }

    /// Bytes written so far. Useful for a recorder that rotates files by size.
    pub fn position(&self) -> u64 {
        self.pos
    }
}
