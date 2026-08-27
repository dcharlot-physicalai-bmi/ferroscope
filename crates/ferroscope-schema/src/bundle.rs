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
pub fn bundle_streaming<F, R>(open: F) -> Option<String>
where
    F: Fn() -> std::io::Result<R>,
    R: std::io::Read,
{
    use ferroscope_mcap::{Flow, Record};

    // Pass one: counts, front and back matter. No payload is parsed.
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut chan: BTreeMap<u16, (String, u16)> = BTreeMap::new();
    let mut schema_names: BTreeMap<u16, String> = BTreeMap::new();
    let mut attachments: Vec<(String, String, usize)> = Vec::new();
    let mut receipt_kv: Option<Vec<(String, String)>> = None;
    let mut production_kv: Option<Vec<(String, String)>> = None;
    let mut profile = String::new();

    ferroscope_mcap::stream(open().ok()?, |rec| {
        match rec {
            Record::Header { profile: p, .. } => profile = p.to_string(),
            Record::Schema(sc) => {
                schema_names.insert(sc.id, sc.name);
            }
            Record::Channel(ch) => {
                chan.insert(ch.id, (ch.topic, ch.schema_id));
            }
            Record::Attachment(a) => {
                attachments.push((a.name, a.media_type, a.data.len()));
            }
            Record::Metadata { name, kv } => {
                if name == RECEIPT_BLOCK {
                    receipt_kv = Some(kv);
                } else if name == PRODUCTION_BLOCK {
                    production_kv = Some(kv);
                }
            }
            Record::Message(m) => {
                if let Some((topic, _)) = chan.get(&m.channel_id) {
                    *counts.entry(topic.clone()).or_default() += 1;
                }
            }
            _ => {}
        }
        Ok(Flow::Continue)
    })
    .ok()?;

    let receipt = receipt_kv.as_deref().and_then(Receipt::from_pairs);

    // Pass two: the lanes, strided on the way in, and the receipt recomputed alongside.
    let mut lanes = Lanes::with_strides(strides_from(&counts));
    let mut digest = receipt.as_ref().map(|r| TraceDigest::new(r.precision));
    let mut ledger = ferroscope_ledger::Ledger::new();
    let mut hashed = 0usize;

    ferroscope_mcap::stream(open().ok()?, |rec| {
        if let Record::Message(m) = rec {
            let Some((topic, schema_id)) = chan.get(&m.channel_id) else {
                return Ok(Flow::Continue);
            };
            let schema = schema_names
                .get(schema_id)
                .map(|s| s.as_str())
                .unwrap_or("");
            // One parse, two consumers.
            let Ok(text) = std::str::from_utf8(m.data) else {
                return Ok(Flow::Continue);
            };
            let Some(v) = crate::json::parse(text) else {
                return Ok(Flow::Continue);
            };
            lanes.push_parsed(topic, schema, m.log_time, m.publish_time, &v);

            if schema != "ferroscope.Event" {
                if let Some(d) = digest.as_mut() {
                    let step = v.get("step").and_then(|x| x.as_f64()).unwrap_or(0.0) as u64;
                    d.step(step, topic, &crate::digest_values(schema, &v));
                    hashed += 1;
                }
                if schema == "ferroscope.EnergySample" {
                    let rail = match v.get("rail").and_then(|r| r.as_str()) {
                        Some("compute") => ferroscope_ledger::Rail::Compute,
                        Some("actuation") => ferroscope_ledger::Rail::Actuation,
                        _ => ferroscope_ledger::Rail::Overhead,
                    };
                    let source = v.get("source").and_then(|x| x.as_str()).unwrap_or("");
                    let watts = v.get("watts").and_then(|w| w.as_f64()).unwrap_or(0.0);
                    ledger.sample(rail, source, m.log_time, watts);
                }
            }
        }
        Ok(Flow::Continue)
    })
    .ok()?;

    let verification = match (receipt, digest) {
        (Some(receipt), Some(d)) => {
            let recomputed = d.finish();
            Some(crate::Verification {
                trace_matches: recomputed == receipt.trace_digest,
                spec_matches: receipt.self_consistent(),
                recomputed,
                receipt,
                quote: ledger.quote(),
                messages: hashed,
            })
        }
        _ => None,
    };

    Some(lanes.finish(
        &profile,
        &attachments,
        receipt_kv.as_deref(),
        production_kv.as_deref(),
        verification,
    ))
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
