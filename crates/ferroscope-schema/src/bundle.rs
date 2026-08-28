//! **The viewer bundle** — a recording flattened into the lanes a viewer draws.
//!
//! One function, two callers. `ferroscope export` writes it to a file for a page that has to
//! show a real run over a CDN with no wasm fetch at all; `ferroscope-wasm` calls the same
//! function inside the browser on bytes the reader just dropped onto the page. Both paths
//! produce identical numbers because there is only one implementation of them.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use crate::json::{Obj, Value};
use crate::{PRODUCTION_BLOCK, RECEIPT_BLOCK, mcap, verify};
use ferroscope_receipt::{Receipt, TraceDigest};

/// Series longer than this are strided down. A 1080 px lane cannot show more.
const MAX_POINTS: usize = 4_000;

/// Flatten a recording into the viewer bundle. `None` when the bytes are not a readable
/// MCAP file or a payload will not parse.
pub fn bundle(bytes: &[u8]) -> Option<String> {
    let log = mcap::read(bytes).ok()?;
    bundle_log(log, verify(bytes))
}

/// [`bundle`] for a growing live prefix: no closing magic required, no receipt expected yet.
/// The moment the producer seals, the same buffer becomes a complete file and [`bundle`]
/// takes over, receipt and all.
pub fn bundle_prefix(bytes: &[u8]) -> Option<String> {
    let log = mcap::read_prefix(bytes).ok()?;
    bundle_log(log, None)
}

/// Flatten a parsed recording into the bundle, striding each lane on the way in so the two
/// paths select the identical subset of points.
fn bundle_log(log: mcap::Log, v: Option<crate::Verification>) -> Option<String> {
    // Pass one over an already-parsed log is free: count per topic, then set the stride the
    // final JSON would have applied anyway.
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for m in &log.messages {
        let ch = log.channel(m.channel_id)?;
        *counts.entry(ch.topic.clone()).or_default() += 1;
    }
    let mut lanes = Lanes::with_strides(strides_from(&counts));
    for m in &log.messages {
        let ch = log.channel(m.channel_id)?;
        let schema = log
            .schema(ch.schema_id)
            .map(|s| s.name.as_str())
            .unwrap_or("");
        lanes.push(&ch.topic, schema, m.log_time, m.publish_time, &m.data);
    }
    let attachments: Vec<(String, String, usize)> = log
        .attachments
        .iter()
        .map(|a| (a.name.clone(), a.media_type.clone(), a.data.len()))
        .collect();
    Some(lanes.finish(
        &log.profile,
        &attachments,
        log.metadata_block(RECEIPT_BLOCK),
        log.metadata_block(PRODUCTION_BLOCK),
        v,
    ))
}

/// Flatten a recording into the viewer bundle WITHOUT holding the recording.
///
/// The same fold as [`bundle`], over a stream. It is the answer to a recording too large for a
/// browser to read: the CLI does the reading, the bundle is a thousandth of the size, and the
/// page opens that. Two passes — pass one counts each lane and picks up the receipt, pass two
/// strides the points on the way in and recomputes the digest — so memory is bounded by the
/// kept points rather than by the file.
///
/// Takes a way to OPEN the stream rather than a stream, for the same reason
/// [`crate::verify_streaming`] does: the receipt is written by `seal` and so lives at the end.
///
/// The fold itself is [`BundleFold`], which is also what the browser drives; this is the loop
/// that feeds it from a file.
pub fn bundle_streaming<F, R>(open: F) -> Option<String>
where
    F: Fn() -> std::io::Result<R>,
    R: std::io::Read,
{
    let mut fold = BundleFold::new();
    pour(open().ok()?, &mut fold).ok()?;
    fold.rewind();
    pour(open().ok()?, &mut fold).ok()?;
    fold.finish()
}

/// Read a stream into a fold, one block at a time.
fn pour<R: std::io::Read>(mut r: R, fold: &mut BundleFold) -> std::io::Result<()> {
    let mut block = vec![0u8; 64 << 10];
    loop {
        match r.read(&mut block) {
            Ok(0) => return Ok(()),
            Ok(n) => {
                if !fold.push(&block[..n]) {
                    return Ok(()); // the fold has everything it needs
                }
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
}

/// What pass one learns: everything that is not a lane.
#[derive(Default)]
struct FrontMatter {
    counts: BTreeMap<String, usize>,
    chan: BTreeMap<u16, (String, u16)>,
    schema_names: BTreeMap<u16, String>,
    attachments: Vec<(String, String, usize)>,
    receipt_kv: Option<Vec<(String, String)>>,
    production_kv: Option<Vec<(String, String)>>,
    profile: String,
}

/// What pass two accumulates: the lanes, the recomputed digest and the energy ledger.
struct Second {
    lanes: Lanes,
    digest: Option<TraceDigest>,
    ledger: ferroscope_ledger::Ledger,
    hashed: usize,
    /// Messages pass two actually saw, counted the same way pass one counted them.
    seen: usize,
}

/// The viewer bundle, folded from a recording that is PUSHED in rather than read.
///
/// A browser cannot hand out a [`Read`](std::io::Read): `File.slice(a, b).arrayBuffer()` gives
/// a block of bytes when it is ready, and there is no blocking read underneath it. So this is
/// the fold with the loop taken out — push blocks in, in file order, and the bundle comes out
/// at the end. [`bundle_streaming`] is this same fold with a file loop around it, which is what
/// keeps the CLI's bundle and the browser's bundle from becoming two different answers.
///
/// Two passes, for the reason [`bundle_streaming`] gives: a lane's stride comes from its
/// message count, and the receipt that says at what precision to recompute the digest is
/// written at the END of the file. Call [`rewind`](BundleFold::rewind) between them and push
/// the same bytes again — which costs a browser a second read of the file and buys a recording
/// of any size at all.
///
/// ```no_run
/// # fn blocks() -> Vec<Vec<u8>> { Vec::new() }
/// let mut fold = ferroscope_schema::BundleFold::new();
/// for b in blocks() { fold.push(&b); }
/// fold.rewind();
/// for b in blocks() { fold.push(&b); }
/// let json = fold.finish().expect("a readable recording");
/// ```
pub struct BundleFold {
    feed: ferroscope_mcap::Feed,
    front: FrontMatter,
    second: Option<Second>,
    /// Set the moment a block does not parse. A fold that has seen a torn record cannot be
    /// trusted to have counted the rest, so it stops rather than reporting a short bundle as a
    /// whole one.
    torn: bool,
    bytes: u64,
}

impl Default for BundleFold {
    fn default() -> Self {
        Self::new()
    }
}

impl BundleFold {
    pub fn new() -> Self {
        Self {
            feed: ferroscope_mcap::Feed::new(),
            front: FrontMatter::default(),
            second: None,
            torn: false,
            bytes: 0,
        }
    }

    /// Add the next block of the recording, in file order. Blocks may be any size and need not
    /// fall on record boundaries.
    ///
    /// Returns `false` once this pass has everything it needs — because the footer was reached,
    /// or because the recording did not parse. Pushing after that does nothing.
    pub fn push(&mut self, block: &[u8]) -> bool {
        use ferroscope_mcap::{Flow, Record};
        if self.torn || self.feed.finished() {
            return false;
        }
        self.bytes += block.len() as u64;
        self.feed.push(block);

        let outcome = match self.second.as_mut() {
            None => {
                let front = &mut self.front;
                self.feed.drain(&mut |rec| {
                    match rec {
                        Record::Header { profile: p, .. } => front.profile = p.to_string(),
                        Record::Schema(sc) => {
                            front.schema_names.insert(sc.id, sc.name);
                        }
                        Record::Channel(ch) => {
                            front.chan.insert(ch.id, (ch.topic, ch.schema_id));
                        }
                        Record::Attachment(a) => {
                            front.attachments.push((a.name, a.media_type, a.data.len()));
                        }
                        Record::Metadata { name, kv } => {
                            if name == RECEIPT_BLOCK {
                                front.receipt_kv = Some(kv);
                            } else if name == PRODUCTION_BLOCK {
                                front.production_kv = Some(kv);
                            }
                        }
                        Record::Message(m) => {
                            if let Some((topic, _)) = front.chan.get(&m.channel_id) {
                                *front.counts.entry(topic.clone()).or_default() += 1;
                            }
                        }
                        _ => {}
                    }
                    Ok(Flow::Continue)
                })
            }
            Some(second) => {
                let front = &self.front;
                self.feed.drain(&mut |rec| {
                    if let Record::Message(m) = rec {
                        absorb_message(front, second, &m);
                    }
                    Ok(Flow::Continue)
                })
            }
        };

        match outcome {
            Ok(Flow::Stop) => false,
            Ok(Flow::Continue) => true,
            Err(_) => {
                self.torn = true;
                false
            }
        }
    }

    /// End pass one and prepare pass two. Push the same bytes again from the start.
    pub fn rewind(&mut self) {
        if self.torn || self.second.is_some() {
            return;
        }
        let receipt = self
            .front
            .receipt_kv
            .as_deref()
            .and_then(Receipt::from_pairs);
        self.second = Some(Second {
            lanes: Lanes::with_strides(strides_from(&self.front.counts)),
            digest: receipt
                .as_ref()
                .map(|r| TraceDigest::with_resolutions(r.precision, &r.resolutions)),
            ledger: ferroscope_ledger::Ledger::new(),
            hashed: 0,
            seen: 0,
        });
        self.feed = ferroscope_mcap::Feed::new();
        self.bytes = 0;
    }

    /// Emit the bundle JSON. `None` if the bytes were not a readable recording, or if
    /// [`rewind`](BundleFold::rewind) was never called and so pass two never ran.
    pub fn finish(self) -> Option<String> {
        if self.torn {
            return None;
        }
        let second = self.second?;
        // The two passes must have seen the same recording. Nothing guarantees it: the caller
        // pushes the bytes and could push fewer the second time, and in a browser the file on
        // disk can change between the two reads. A short pass two produces a bundle with empty
        // or partial lanes and no error anywhere, which renders as a run that simply did not
        // happen. A recording carrying a receipt would fail its digest and say so; one without
        // a receipt — a live prefix — would say nothing at all.
        let counted: usize = self.front.counts.values().sum();
        if second.seen != counted {
            return None;
        }
        let receipt = self
            .front
            .receipt_kv
            .as_deref()
            .and_then(Receipt::from_pairs);
        let verification = match (receipt, second.digest) {
            (Some(receipt), Some(d)) => {
                let recomputed = d.finish();
                Some(crate::Verification {
                    trace_matches: recomputed == receipt.trace_digest,
                    spec_matches: receipt.self_consistent(),
                    recomputed,
                    receipt,
                    quote: second.ledger.quote(),
                    messages: second.hashed,
                })
            }
            _ => None,
        };
        Some(second.lanes.finish(
            &self.front.profile,
            &self.front.attachments,
            self.front.receipt_kv.as_deref(),
            self.front.production_kv.as_deref(),
            verification,
        ))
    }

    /// Which pass is being fed: 1 before [`rewind`](BundleFold::rewind), 2 after.
    pub fn pass(&self) -> u32 {
        if self.second.is_some() { 2 } else { 1 }
    }

    /// How many bytes this pass has taken.
    pub fn fed(&self) -> u64 {
        self.bytes
    }

    /// How many lane points the fold is holding, across every lane.
    ///
    /// Bounded by what a screen can draw rather than by the recording — the second half of the
    /// claim that a file larger than memory can be opened at all. Zero during pass one, which
    /// only counts.
    pub fn kept_points(&self) -> usize {
        self.second.as_ref().map_or(0, |s| s.lanes.kept_points())
    }

    /// How many pushed bytes are held because they are not yet a whole record — the fold's
    /// framing memory, which stays around one record however long the recording is.
    pub fn buffered(&self) -> usize {
        self.feed.buffered()
    }
}

/// One message, into the lanes, the digest and the ledger. One parse, three consumers.
fn absorb_message(front: &FrontMatter, second: &mut Second, m: &ferroscope_mcap::MessageRef<'_>) {
    let Some((topic, schema_id)) = front.chan.get(&m.channel_id) else {
        return;
    };
    second.seen += 1;
    let schema = front
        .schema_names
        .get(schema_id)
        .map(|s| s.as_str())
        .unwrap_or("");
    let Ok(text) = std::str::from_utf8(m.data) else {
        return;
    };
    let Some(v) = crate::json::parse(text) else {
        return;
    };
    second
        .lanes
        .push_parsed(topic, schema, m.log_time, m.publish_time, &v);

    if schema == "ferroscope.Event" {
        return;
    }
    if let Some(d) = second.digest.as_mut() {
        let step = v.get("step").and_then(|x| x.as_f64()).unwrap_or(0.0) as u64;
        d.step(step, topic, &crate::digest_values(schema, &v));
        second.hashed += 1;
    }
    if schema == "ferroscope.EnergySample" {
        let rail = match v.get("rail").and_then(|r| r.as_str()) {
            Some("compute") => ferroscope_ledger::Rail::Compute,
            Some("actuation") => ferroscope_ledger::Rail::Actuation,
            _ => ferroscope_ledger::Rail::Overhead,
        };
        let source = v.get("source").and_then(|x| x.as_str()).unwrap_or("");
        let watts = v.get("watts").and_then(|w| w.as_f64()).unwrap_or(0.0);
        second.ledger.sample(rail, source, m.log_time, watts);
    }
}

/// The stride each topic's lane will get, from its message count.
///
/// Mirrors [`stride`]: a lane of `n` points is drawn at `n.div_ceil(MAX_POINTS)`, keeping
/// indices 0, k, 2k… Applying it on the way in keeps memory bounded without changing which
/// points survive.
pub(crate) fn strides_from(counts: &BTreeMap<String, usize>) -> BTreeMap<String, usize> {
    counts
        .iter()
        .map(|(topic, n)| (topic.clone(), n.div_ceil(MAX_POINTS).max(1)))
        .collect()
}

/// The lanes a viewer draws, accumulated one message at a time.
///
/// One implementation, two callers: [`bundle`] walks a parsed [`mcap::Log`] and
/// [`bundle_streaming`] walks a [`Read`](std::io::Read). Sharing the accumulation is what keeps
/// them from becoming two definitions of what a bundle is — the failure this project has now
/// had four times over, once per comparator.
#[derive(Default)]
pub(crate) struct Lanes {
    poses: BTreeMap<String, Vec<[f64; 8]>>,
    scalars: BTreeMap<String, Vec<[f64; 2]>>,
    power: BTreeMap<String, Vec<[f64; 2]>>,
    contacts: Vec<[f64; 5]>,
    lag: Vec<[f64; 2]>,
    /// (frame, id) -> the geometry's declaration plus its pose track over time.
    geom: BTreeMap<String, (Value, Vec<[f64; 15]>)>,
    /// child frame name -> pose track, so a Geometry can name a frame rather than a topic.
    frames: BTreeMap<String, Vec<[f64; 8]>>,
    counts: BTreeMap<String, usize>,
    schema_of: BTreeMap<String, String>,
    messages: usize,
    span: Option<(u64, u64)>,
    /// When set, only every `k`-th message on a topic is kept, `k` looked up per topic.
    ///
    /// This is what lets the streaming path hold a bounded number of points: the stride the
    /// final JSON would have applied anyway is applied on the way IN. `stride()` keeps indices
    /// 0, k, 2k…, so keeping every k-th pushed message selects the identical subset — which is
    /// why both paths produce byte-identical bundles rather than merely similar ones.
    keep_every: Option<BTreeMap<String, usize>>,
    seen: BTreeMap<String, usize>,
}

impl Lanes {
    fn with_strides(strides: BTreeMap<String, usize>) -> Lanes {
        Lanes {
            keep_every: Some(strides),
            ..Default::default()
        }
    }

    /// How many lane points are being held.
    ///
    /// The other half of the memory story. [`Feed`](ferroscope_mcap::Feed) bounds the FRAMING
    /// memory at one record; this bounds the KEPT memory at what a screen can draw, and it is
    /// the half that would otherwise grow without limit over a long recording — a lane with
    /// every point of a 2.6 GB run in it is millions of samples nothing will ever draw, and in
    /// a browser it is the difference between opening the file and not.
    fn kept_points(&self) -> usize {
        self.poses.values().map(Vec::len).sum::<usize>()
            + self.scalars.values().map(Vec::len).sum::<usize>()
            + self.power.values().map(Vec::len).sum::<usize>()
            + self.frames.values().map(Vec::len).sum::<usize>()
            + self.geom.values().map(|(_, t)| t.len()).sum::<usize>()
            + self.contacts.len()
            + self.lag.len()
    }

    /// Feed one message whose payload is ALREADY parsed.
    ///
    /// The streaming path needs the parsed value anyway — the digest hashes every message, not
    /// just the kept ones — and parsing it a second time inside `push` was the single biggest
    /// cost in the export: two JSON parses per message over the whole file.
    fn push_parsed(
        &mut self,
        topic: &str,
        schema: &str,
        log_time: u64,
        publish_time: u64,
        val: &Value,
    ) {
        if !self.note(topic, schema, log_time) {
            return;
        }
        self.absorb(topic, schema, log_time, publish_time, val);
    }

    /// Count the message and decide whether its points are kept. `true` means keep.
    fn note(&mut self, topic: &str, schema: &str, log_time: u64) -> bool {
        *self.counts.entry(topic.to_string()).or_default() += 1;
        self.schema_of
            .entry(topic.to_string())
            .or_insert_with(|| schema.to_string());
        self.messages += 1;
        self.span = Some(match self.span {
            None => (log_time, log_time),
            Some((lo, hi)) => (lo.min(log_time), hi.max(log_time)),
        });
        match self.keep_every.as_ref().and_then(|m| m.get(topic)).copied() {
            Some(k) if k > 1 => {
                let n = self.seen.entry(topic.to_string()).or_insert(0);
                let i = *n;
                *n += 1;
                i.is_multiple_of(k)
            }
            _ => true,
        }
    }

    /// Feed one message. `schema` is the schema NAME, which is what dispatch keys on.
    fn push(&mut self, topic: &str, schema: &str, log_time: u64, publish_time: u64, data: &[u8]) {
        if !self.note(topic, schema, log_time) {
            return;
        }
        let Ok(text) = std::str::from_utf8(data) else {
            return;
        };
        let Some(val) = crate::json::parse(text) else {
            return;
        };
        self.absorb(topic, schema, log_time, publish_time, &val);
    }

    fn absorb(&mut self, topic: &str, schema: &str, log_time: u64, publish_time: u64, val: &Value) {
        let poses = &mut self.poses;
        let scalars = &mut self.scalars;
        let power = &mut self.power;
        let contacts = &mut self.contacts;
        let lag = &mut self.lag;
        let geom = &mut self.geom;
        let frames = &mut self.frames;
        let m_log_time = log_time;
        let m_publish_time = publish_time;
        let ch_topic = topic;
        let t = log_time as f64 * 1e-9;
        let num = |k: &str| val.get(k).and_then(|x| x.as_f64()).unwrap_or(0.0);
        let arr = |k: &str| -> Vec<f64> {
            val.get(k)
                .and_then(|x| x.as_array())
                .map(|a| a.iter().filter_map(|e| e.as_f64()).collect())
                .unwrap_or_default()
        };

        // Wall−sim drift, sampled from whichever channel is densest; one lane is enough.
        if ch_topic.ends_with("/base") {
            lag.push([t, (m_publish_time as i64 - m_log_time as i64) as f64 * 1e-6]);
        }

        match schema {
            "ferroscope.Transform" => {
                let p = arr("translation");
                let q = arr("rotation");
                if p.len() == 3 && q.len() == 4 {
                    let row = [t, p[0], p[1], p[2], q[0], q[1], q[2], q[3]];
                    poses.entry(ch_topic.to_string()).or_default().push(row);
                    if let Some(child) = val.get("child").and_then(|x| x.as_str()) {
                        frames.entry(child.to_string()).or_default().push(row);
                    }
                }
            }
            "ferroscope.Scalar" => scalars
                .entry(ch_topic.to_string())
                .or_default()
                .push([t, num("value")]),
            "ferroscope.EnergySample" => {
                let rail = val
                    .get("rail")
                    .and_then(|r| r.as_str())
                    .unwrap_or("overhead");
                let source = val.get("source").and_then(|s| s.as_str()).unwrap_or("?");
                power
                    .entry(format!("{rail}/{source}"))
                    .or_default()
                    .push([t, num("watts")]);
            }
            "ferroscope.Contact" => {
                let p = arr("point");
                if p.len() == 3 {
                    contacts.push([t, p[0], p[1], p[2], num("force_n")]);
                }
            }
            "ferroscope.Geometry" => {
                let frame = val.get("frame").and_then(|x| x.as_str()).unwrap_or("world");
                let gid = val.get("id").and_then(|x| x.as_str()).unwrap_or("?");
                let key = format!("{frame}/{gid}");
                let tr = arr("translation");
                let rot = arr("rotation");
                let sz = arr("size");
                let col = arr("color");
                let g = |v: &[f64], i: usize, d: f64| *v.get(i).unwrap_or(&d);
                let pose = [
                    t,
                    g(&tr, 0, 0.0),
                    g(&tr, 1, 0.0),
                    g(&tr, 2, 0.0),
                    g(&rot, 0, 0.0),
                    g(&rot, 1, 0.0),
                    g(&rot, 2, 0.0),
                    g(&rot, 3, 1.0),
                    g(&sz, 0, 0.1),
                    g(&sz, 1, 0.1),
                    g(&sz, 2, 0.1),
                    g(&col, 0, 0.8),
                    g(&col, 1, 0.7),
                    g(&col, 2, 0.4),
                    g(&col, 3, 1.0),
                ];
                let entry = geom.entry(key).or_insert_with(|| {
                    (
                        Obj::new()
                            .str("frame", frame)
                            .str("id", gid)
                            .str(
                                "shape",
                                val.get("shape").and_then(|x| x.as_str()).unwrap_or("box"),
                            )
                            .str(
                                "mesh",
                                val.get("mesh").and_then(|x| x.as_str()).unwrap_or(""),
                            )
                            .raw(
                                "points",
                                &val.get("points")
                                    .map(|p| p.to_json())
                                    .unwrap_or_else(|| "[]".into()),
                            )
                            .finish_value(),
                        Vec::new(),
                    )
                });
                entry.1.push(pose);
            }
            "ferroscope.JointState" => {
                // Joint positions ride as one scalar lane per joint, named from the file.
                let names = val
                    .get("names")
                    .and_then(|x| x.as_array())
                    .map(|a| {
                        a.iter()
                            .map(|e| e.as_str().unwrap_or("?").to_string())
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                for (i, p) in arr("position").into_iter().enumerate() {
                    let name = names.get(i).cloned().unwrap_or_else(|| i.to_string());
                    scalars
                        .entry(format!("{}:{}", ch_topic, name))
                        .or_default()
                        .push([t, p]);
                }
            }
            _ => {}
        }
    }

    /// Emit the bundle JSON. Everything that is not a lane — the profile, the attachment list,
    /// the receipt and production blocks — is passed in, because it comes from the file's
    /// front and back matter rather than from the messages.
    pub(crate) fn finish(
        &self,
        profile: &str,
        attachments: &[(String, String, usize)],
        receipt_block: Option<&[(String, String)]>,
        production_block: Option<&[(String, String)]>,
        v: Option<crate::Verification>,
    ) -> String {
        let Lanes {
            poses,
            scalars,
            power,
            contacts,
            lag,
            geom,
            frames,
            counts,
            schema_of,
            messages,
            span,
            ..
        } = self;

        let mut out = String::from("{");
        let _ = write!(out, "\"format\":\"ferroscope.bundle.v1\"");
        let _ = write!(out, ",\"profile\":\"{}\"", profile);
        let _ = write!(out, ",\"messages\":{}", messages);
        if let Some((t0, t1)) = span {
            let _ = write!(
                out,
                ",\"span\":[{},{}]",
                *t0 as f64 * 1e-9,
                *t1 as f64 * 1e-9
            );
        }

        out.push_str(",\"topics\":[");
        for (i, (topic, n)) in counts.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            let schema = schema_of.get(topic).cloned().unwrap_or_default();
            out.push_str(
                &Obj::new()
                    .str("topic", topic)
                    .str("schema", &schema)
                    .uint("count", *n as u64)
                    .finish(),
            );
        }
        out.push(']');

        out.push_str(",\"poses\":");
        write_map(&mut out, poses);
        out.push_str(",\"scalars\":");
        write_map(&mut out, scalars);
        out.push_str(",\"power\":");
        write_map(&mut out, power);
        // Geometry: one object per part, with its pose track strided down like any other lane.
        out.push_str(",\"geometry\":[");
        for (i, (_, (decl, track))) in geom.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push_str(decl.to_json().trim_end_matches('}'));
            out.push_str(",\"track\":");
            write_series(&mut out, &stride(track));
            out.push('}');
        }
        out.push(']');

        out.push_str(",\"attachments\":[");
        for (i, (name, media_type, bytes)) in attachments.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push_str(
                &Obj::new()
                    .str("name", name)
                    .str("media_type", media_type)
                    .uint("bytes", *bytes as u64)
                    .finish(),
            );
        }
        out.push(']');

        out.push_str(",\"frames\":");
        write_map(&mut out, frames);

        out.push_str(",\"lag_ms\":");
        write_series(&mut out, &stride(lag));
        out.push_str(",\"contacts\":");
        write_series(&mut out, &stride(contacts));

        if let Some(v) = &v {
            let q = &v.quote;
            let _ = write!(
                out,
                ",\"energy\":{}",
                Obj::new()
                    .num("compute_j", q.compute_j)
                    .num("actuation_j", q.actuation_j)
                    .num("overhead_j", q.overhead_j)
                    .num("total_j", q.total_j)
                    .num("duration_s", q.duration_s)
                    .num("mean_w", q.mean_power_w())
                    .num("peak_w", q.peak_w)
                    .str("peak_source", &q.peak_source)
                    .str("coverage", &q.coverage.to_string())
                    .raw("quotable", if q.quotable { "true" } else { "false" })
                    .finish()
            );
            let _ = write!(
                out,
                ",\"receipt\":{}",
                Obj::new()
                    .str("scenario", &v.receipt.spec.scenario)
                    .uint("seed", v.receipt.spec.seed)
                    .str("precision", &v.receipt.precision.to_string())
                    .str("platform", &v.receipt.platform)
                    .str("integrator", &v.receipt.spec.integrator)
                    .str("solver", &v.receipt.spec.solver)
                    .str("build", &v.receipt.spec.build)
                    .uint("steps", v.receipt.spec.steps)
                    .str("spec_digest", &v.receipt.spec_digest)
                    .str("trace_digest", &v.receipt.trace_digest)
                    .str("recomputed", &v.recomputed)
                    .uint("non_finite", v.receipt.non_finite)
                    .raw("verified", if v.ok() { "true" } else { "false" })
                    .finish()
            );
        } else if receipt_block.is_none() {
            out.push_str(",\"receipt\":null");
        }

        // What producing the file cost, when the file says. The viewer is where people actually
        // look, so this is the one surface the block must not be missing from.
        if let Some(kv) = production_block {
            let mut o = Obj::new();
            for (k, v) in kv {
                o = o.str(k, v);
            }
            let _ = write!(out, ",\"production\":{}", o.finish());
        }

        out.push('}');
        out
    }
}

fn stride<const N: usize>(v: &[[f64; N]]) -> Vec<[f64; N]> {
    if v.len() <= MAX_POINTS {
        return v.to_vec();
    }
    let step = v.len().div_ceil(MAX_POINTS);
    v.iter().step_by(step).copied().collect()
}

fn write_series<const N: usize>(out: &mut String, rows: &[[f64; N]]) {
    out.push('[');
    for (i, r) in rows.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push('[');
        for (j, x) in r.iter().enumerate() {
            if j > 0 {
                out.push(',');
            }
            crate::json::write_number(out, round6(*x));
        }
        out.push(']');
    }
    out.push(']');
}

fn write_map<const N: usize>(out: &mut String, m: &BTreeMap<String, Vec<[f64; N]>>) {
    out.push('{');
    for (i, (k, v)) in m.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        crate::json::write_string(out, k);
        out.push(':');
        write_series(out, &stride(v));
    }
    out.push('}');
}

/// Six decimals is a micrometre, a microsecond, or a microwatt — below anything a lane can
/// draw, and it keeps the bundle from tripling in size on float noise. The *receipt* still
/// travels at full precision, because that is the number nobody may round.
fn round6(x: f64) -> f64 {
    if x.is_finite() {
        (x * 1e6).round() / 1e6
    } else {
        x
    }
}
