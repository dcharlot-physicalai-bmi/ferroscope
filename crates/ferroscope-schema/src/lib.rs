//! **The recording model.**
//!
//! Three ideas the rest of the field leaves on the floor:
//!
//! ### 1. A physical-AI run has three clocks, not one
//!
//! MCAP gives a message a `log_time` and a `publish_time`. That is enough for a log and not
//! enough for a robot. A run has *simulated* time, *wall* time, and a *control step* index,
//! and the interesting bugs live in the drift between them: a controller that holds 1 kHz in
//! sim and 780 Hz on hardware is not a controller that works. Every Ferroscope message
//! carries all three, so real-time factor and control-loop jitter are readable straight off
//! the recording rather than reconstructed by hand. See [`Stamp`].
//!
//! ### 2. Energy is a channel, not an afterthought
//!
//! [`EnergySample`] is a well-known type with a rail (`compute` / `actuation` / `overhead`),
//! so the viewer can total `E_task = E_compute + E_actuation` without being told which topic
//! means what. See [`ferroscope_ledger`].
//!
//! ### 3. The receipt is recomputable from the file
//!
//! The recorder hashes every numeric payload as it writes it, and seals the run with a
//! [`ferroscope_receipt::Receipt`] stored in the recording's own metadata. Because the hash
//! is defined over *what is in the file*, anyone holding the file can recompute it — see
//! [`verify`] — without the simulator, the source, or the machine that produced it.
//!
//! Payloads are JSON with published JSON Schemas, so a recording opens in any MCAP viewer
//! that already exists, with no plugin. Being readable by the incumbent is the price of
//! asking anyone to try a new one.
//!
//! ```
//! use ferroscope_schema::{Recorder, Stamp, Transform, EnergySample};
//! use ferroscope_ledger::Rail;
//! use ferroscope_receipt::{RunSpec, Precision};
//!
//! let mut rec = Recorder::new(Vec::new(), Precision::Quantized { drop_bits: 12 });
//! let t = Stamp { sim_ns: 0, wall_ns: 0, step: 0 };
//! rec.transform("/robot/base", t, "world", "base", [0.0; 3], [0.0, 0.0, 0.0, 1.0]).unwrap();
//! rec.energy("/energy/soc", t, Rail::Compute, "soc", 8.0).unwrap();
//!
//! let spec = RunSpec::new("demo", 1).steps(1).build("doctest");
//! let (bytes, receipt, quote) = rec.seal(spec, "doctest").unwrap();
//! assert!(receipt.self_consistent());
//! assert_eq!(quote.compute_j, 0.0); // one sample is not an interval
//! assert!(!bytes.is_empty());
//! ```

#![forbid(unsafe_code)]

pub mod bundle;
pub mod json;

use std::collections::BTreeMap;
use std::io::Write;

use ferroscope_ledger::{Ledger, Quote, Rail};
use ferroscope_mcap::{read, Log, Writer, WriterOptions};
use ferroscope_receipt::{Precision, Receipt, RunSpec, Trace, TraceDigest};

pub use bundle::bundle;
pub use ferroscope_ledger as ledger;
pub use ferroscope_mcap as mcap;
pub use ferroscope_receipt as receipt;

/// The metadata block name a Ferroscope receipt lives under.
pub const RECEIPT_BLOCK: &str = "ferroscope.receipt";
/// The MCAP profile string Ferroscope recordings declare.
pub const PROFILE: &str = "ferroscope";

// ---------------------------------------------------------------------------
// Time
// ---------------------------------------------------------------------------

/// A three-clock timestamp.
///
/// `sim_ns` is the simulator's own clock — monotonic, exact, and the one a replay must
/// reproduce. `wall_ns` is the clock the world runs on. `step` is the control iteration
/// index, which is what a controller actually counts. Their differences are the signal:
/// `wall - sim` is the real-time factor, and the spread of `wall` between consecutive
/// `step`s is loop jitter.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stamp {
    pub sim_ns: u64,
    pub wall_ns: u64,
    pub step: u64,
}

impl Stamp {
    pub fn at(sim_ns: u64, wall_ns: u64, step: u64) -> Self {
        Stamp {
            sim_ns,
            wall_ns,
            step,
        }
    }
    /// A run with no separate wall clock (offline replay, batch generation).
    pub fn sim(sim_ns: u64, step: u64) -> Self {
        Stamp {
            sim_ns,
            wall_ns: sim_ns,
            step,
        }
    }
    /// Positive when the run is slower than real time.
    pub fn lag_ns(&self) -> i64 {
        self.wall_ns as i64 - self.sim_ns as i64
    }
}

// ---------------------------------------------------------------------------
// Well-known payloads
// ---------------------------------------------------------------------------

/// A rigid transform between two named frames.
#[derive(Clone, Debug, PartialEq)]
pub struct Transform {
    pub parent: String,
    pub child: String,
    pub translation: [f64; 3],
    /// `[x, y, z, w]`.
    pub rotation: [f64; 4],
}

/// The state of an articulated system.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct JointState {
    pub names: Vec<String>,
    pub position: Vec<f64>,
    pub velocity: Vec<f64>,
    pub effort: Vec<f64>,
}

/// One contact, with the two quantities that decide whether a simulator is lying: the normal
/// force and the penetration depth it allowed to get there.
#[derive(Clone, Debug, PartialEq)]
pub struct Contact {
    pub body_a: String,
    pub body_b: String,
    pub point: [f64; 3],
    pub normal: [f64; 3],
    pub force_n: f64,
    pub penetration_m: f64,
}

/// Instantaneous power on one named source.
#[derive(Clone, Debug, PartialEq)]
pub struct EnergySample {
    pub rail: Rail,
    pub source: String,
    pub watts: f64,
}

/// The JSON Schema text for each well-known type, published so any MCAP viewer renders a
/// Ferroscope recording without a plugin.
pub mod schemas {
    pub const STAMP_PROPS: &str =
        r#""sim_ns":{"type":"integer"},"wall_ns":{"type":"integer"},"step":{"type":"integer"}"#;

    pub const TRANSFORM: &str = r#"{"type":"object","title":"ferroscope.Transform","properties":{"sim_ns":{"type":"integer"},"wall_ns":{"type":"integer"},"step":{"type":"integer"},"parent":{"type":"string"},"child":{"type":"string"},"translation":{"type":"array","items":{"type":"number"},"minItems":3,"maxItems":3},"rotation":{"type":"array","items":{"type":"number"},"minItems":4,"maxItems":4}},"required":["parent","child","translation","rotation"]}"#;

    pub const JOINT_STATE: &str = r#"{"type":"object","title":"ferroscope.JointState","properties":{"sim_ns":{"type":"integer"},"wall_ns":{"type":"integer"},"step":{"type":"integer"},"names":{"type":"array","items":{"type":"string"}},"position":{"type":"array","items":{"type":"number"}},"velocity":{"type":"array","items":{"type":"number"}},"effort":{"type":"array","items":{"type":"number"}}}}"#;

    pub const CONTACT: &str = r#"{"type":"object","title":"ferroscope.Contact","properties":{"sim_ns":{"type":"integer"},"wall_ns":{"type":"integer"},"step":{"type":"integer"},"body_a":{"type":"string"},"body_b":{"type":"string"},"point":{"type":"array","items":{"type":"number"}},"normal":{"type":"array","items":{"type":"number"}},"force_n":{"type":"number"},"penetration_m":{"type":"number"}}}"#;

    pub const ENERGY_SAMPLE: &str = r#"{"type":"object","title":"ferroscope.EnergySample","description":"Instantaneous power on one named source. rail is one of compute|actuation|overhead so a viewer can total E_task = E_compute + E_actuation without being told which topic means what.","properties":{"sim_ns":{"type":"integer"},"wall_ns":{"type":"integer"},"step":{"type":"integer"},"rail":{"type":"string","enum":["compute","actuation","overhead"]},"source":{"type":"string"},"watts":{"type":"number"}},"required":["rail","source","watts"]}"#;

    pub const SCALAR: &str = r#"{"type":"object","title":"ferroscope.Scalar","properties":{"sim_ns":{"type":"integer"},"wall_ns":{"type":"integer"},"step":{"type":"integer"},"value":{"type":"number"},"unit":{"type":"string"}},"required":["value"]}"#;

    pub const EVENT: &str = r#"{"type":"object","title":"ferroscope.Event","properties":{"sim_ns":{"type":"integer"},"wall_ns":{"type":"integer"},"step":{"type":"integer"},"level":{"type":"string","enum":["debug","info","warn","error"]},"text":{"type":"string"}},"required":["level","text"]}"#;
}

fn stamp_obj(t: Stamp) -> json::Obj {
    json::Obj::new()
        .uint("sim_ns", t.sim_ns)
        .uint("wall_ns", t.wall_ns)
        .uint("step", t.step)
}

// ---------------------------------------------------------------------------
// Recorder
// ---------------------------------------------------------------------------

/// Writes a Ferroscope recording, keeping the energy ledger and the trace digest in step
/// with it so that sealing the run is one call and the receipt cannot drift from the data.
pub struct Recorder<W: Write> {
    w: Writer<W>,
    schema_ids: BTreeMap<&'static str, u16>,
    channel_ids: BTreeMap<String, u16>,
    seq: BTreeMap<u16, u32>,
    ledger: Ledger,
    digest: TraceDigest,
    trace: Trace,
    keep: bool,
    /// Kept so `seal` can report what the run actually spanned.
    first_sim_ns: Option<u64>,
    last_sim_ns: u64,
    max_lag_ns: i64,
}

impl<W: Write> Recorder<W> {
    pub fn new(sink: W, precision: Precision) -> Self {
        Recorder {
            w: Writer::new(
                sink,
                WriterOptions::new(PROFILE, concat!("ferroscope ", env!("CARGO_PKG_VERSION"))),
            ),
            schema_ids: BTreeMap::new(),
            channel_ids: BTreeMap::new(),
            seq: BTreeMap::new(),
            ledger: Ledger::new(),
            digest: TraceDigest::new(precision),
            trace: Trace::default(),
            keep: true,
            first_sim_ns: None,
            last_sim_ns: 0,
            max_lag_ns: 0,
        }
    }

    /// Keep the full trace in memory so [`Recorder::seal`] can hand back something the
    /// comparator can use directly. Off by default for long runs; the digest is unaffected.
    pub fn keep_trace(&mut self, keep: bool) {
        if !keep {
            self.trace.samples.clear();
        }
        self.keep = keep;
    }

    fn schema(&mut self, name: &'static str, text: &str) -> ferroscope_mcap::Result<u16> {
        if let Some(id) = self.schema_ids.get(name) {
            return Ok(*id);
        }
        let id = self.w.add_schema(name, "jsonschema", text.as_bytes())?;
        self.schema_ids.insert(name, id);
        Ok(id)
    }

    fn channel(
        &mut self,
        topic: &str,
        schema_name: &'static str,
        schema_text: &str,
    ) -> ferroscope_mcap::Result<u16> {
        if let Some(id) = self.channel_ids.get(topic) {
            return Ok(*id);
        }
        let sid = self.schema(schema_name, schema_text)?;
        let id = self.w.add_channel(topic, sid, "json", &[])?;
        self.channel_ids.insert(topic.to_string(), id);
        Ok(id)
    }

    fn emit(
        &mut self,
        cid: u16,
        t: Stamp,
        payload: &str,
        values: &[f64],
        topic: &str,
    ) -> ferroscope_mcap::Result<()> {
        let seq = self.seq.entry(cid).or_insert(0);
        let s = *seq;
        *seq += 1;
        self.w
            .write_message(cid, s, t.sim_ns, t.wall_ns, payload.as_bytes())?;

        self.digest.step(t.step, topic, values);
        if self.keep {
            self.trace.push(t.step, topic, values.to_vec());
        }
        self.first_sim_ns.get_or_insert(t.sim_ns);
        self.last_sim_ns = self.last_sim_ns.max(t.sim_ns);
        self.max_lag_ns = self.max_lag_ns.max(t.lag_ns().abs());
        Ok(())
    }

    /// A rigid transform.
    pub fn transform(
        &mut self,
        topic: &str,
        t: Stamp,
        parent: &str,
        child: &str,
        translation: [f64; 3],
        rotation: [f64; 4],
    ) -> ferroscope_mcap::Result<()> {
        let cid = self.channel(topic, "ferroscope.Transform", schemas::TRANSFORM)?;
        let payload = stamp_obj(t)
            .str("parent", parent)
            .str("child", child)
            .nums("translation", &translation)
            .nums("rotation", &rotation)
            .finish();
        let mut v = translation.to_vec();
        v.extend_from_slice(&rotation);
        self.emit(cid, t, &payload, &v, topic)
    }

    /// Joint positions, and optionally velocities and efforts.
    pub fn joints(
        &mut self,
        topic: &str,
        t: Stamp,
        js: &JointState,
    ) -> ferroscope_mcap::Result<()> {
        let cid = self.channel(topic, "ferroscope.JointState", schemas::JOINT_STATE)?;
        let payload = stamp_obj(t)
            .strs("names", &js.names)
            .nums("position", &js.position)
            .nums("velocity", &js.velocity)
            .nums("effort", &js.effort)
            .finish();
        let mut v = js.position.clone();
        v.extend_from_slice(&js.velocity);
        v.extend_from_slice(&js.effort);
        self.emit(cid, t, &payload, &v, topic)
    }

    /// One contact.
    pub fn contact(&mut self, topic: &str, t: Stamp, c: &Contact) -> ferroscope_mcap::Result<()> {
        let cid = self.channel(topic, "ferroscope.Contact", schemas::CONTACT)?;
        let payload = stamp_obj(t)
            .str("body_a", &c.body_a)
            .str("body_b", &c.body_b)
            .nums("point", &c.point)
            .nums("normal", &c.normal)
            .num("force_n", c.force_n)
            .num("penetration_m", c.penetration_m)
            .finish();
        let mut v = c.point.to_vec();
        v.extend_from_slice(&c.normal);
        v.push(c.force_n);
        v.push(c.penetration_m);
        self.emit(cid, t, &payload, &v, topic)
    }

    /// Power on one source. Also books it into the energy ledger.
    pub fn energy(
        &mut self,
        topic: &str,
        t: Stamp,
        rail: Rail,
        source: &str,
        watts: f64,
    ) -> ferroscope_mcap::Result<()> {
        let cid = self.channel(topic, "ferroscope.EnergySample", schemas::ENERGY_SAMPLE)?;
        let payload = stamp_obj(t)
            .str("rail", rail.as_str())
            .str("source", source)
            .num("watts", watts)
            .finish();
        self.ledger.sample(rail, source, t.sim_ns, watts);
        self.emit(cid, t, &payload, &[watts], topic)
    }

    /// Any scalar worth a lane: tracking error, solver residual, battery state of charge.
    pub fn scalar(
        &mut self,
        topic: &str,
        t: Stamp,
        value: f64,
        unit: &str,
    ) -> ferroscope_mcap::Result<()> {
        let cid = self.channel(topic, "ferroscope.Scalar", schemas::SCALAR)?;
        let payload = stamp_obj(t).num("value", value).str("unit", unit).finish();
        self.emit(cid, t, &payload, &[value], topic)
    }

    /// A discrete event. Events carry no numbers, so they do not enter the trace digest —
    /// a log line must never be able to change a determinism verdict.
    pub fn event(
        &mut self,
        topic: &str,
        t: Stamp,
        level: &str,
        text: &str,
    ) -> ferroscope_mcap::Result<()> {
        let cid = self.channel(topic, "ferroscope.Event", schemas::EVENT)?;
        let payload = stamp_obj(t).str("level", level).str("text", text).finish();
        let seq = self.seq.entry(cid).or_insert(0);
        let s = *seq;
        *seq += 1;
        self.w
            .write_message(cid, s, t.sim_ns, t.wall_ns, payload.as_bytes())
    }

    /// Everything logged so far, for a caller that wants to run [`ferroscope_receipt::compare`]
    /// in-process rather than against a second file.
    pub fn trace(&self) -> &Trace {
        &self.trace
    }

    /// The energy ledger so far.
    pub fn ledger(&self) -> &Ledger {
        &self.ledger
    }

    /// Close the recording: write the receipt into the file's metadata, finish the MCAP,
    /// and hand back the sink, the receipt, and the energy quote.
    pub fn seal(
        mut self,
        spec: RunSpec,
        platform: &str,
    ) -> ferroscope_mcap::Result<(W, Receipt, Quote)> {
        let quote = self.ledger.quote();
        let receipt = spec.receipt(self.digest.clone(), platform);
        let mut kv = receipt.to_pairs();
        kv.push(("energy.total_j".into(), format!("{:.6}", quote.total_j)));
        kv.push(("energy.compute_j".into(), format!("{:.6}", quote.compute_j)));
        kv.push((
            "energy.actuation_j".into(),
            format!("{:.6}", quote.actuation_j),
        ));
        kv.push(("energy.quotable".into(), quote.quotable.to_string()));
        kv.push(("energy.coverage".into(), quote.coverage.to_string()));
        kv.push(("clock.max_lag_ns".into(), self.max_lag_ns.to_string()));
        self.w.write_metadata(RECEIPT_BLOCK, &kv)?;
        let sink = self.w.finish()?;
        Ok((sink, receipt, quote))
    }
}

// ---------------------------------------------------------------------------
// Verification
// ---------------------------------------------------------------------------

/// What [`verify`] found in a recording.
#[derive(Clone, Debug)]
pub struct Verification {
    pub receipt: Receipt,
    /// The trace digest recomputed from the messages in the file.
    pub recomputed: String,
    /// `true` when the recomputed digest matches the one the recorder stored.
    pub trace_matches: bool,
    /// `true` when the receipt's own fields still hash to its stated spec digest.
    pub spec_matches: bool,
    /// The energy ledger rebuilt from the file's `EnergySample` messages.
    pub quote: Quote,
    pub messages: usize,
}

impl Verification {
    /// The one-line answer: does this file still stand behind its own receipt?
    pub fn ok(&self) -> bool {
        self.trace_matches && self.spec_matches
    }
}

/// Recompute a recording's receipt from the recording.
///
/// This is the property that makes the receipt worth anything: no simulator, no source tree,
/// no access to the machine that produced the file. Read the bytes, re-hash the numbers in
/// file order, compare.
pub fn verify(bytes: &[u8]) -> Option<Verification> {
    let log: Log = read(bytes).ok()?;
    let kv = log.metadata_block(RECEIPT_BLOCK)?;
    let receipt = Receipt::from_pairs(kv)?;

    let mut digest = TraceDigest::new(receipt.precision);
    let mut ledger = Ledger::new();
    let mut count = 0usize;

    for m in &log.messages {
        let ch = log.channel(m.channel_id)?;
        let schema = log
            .schema(ch.schema_id)
            .map(|s| s.name.as_str())
            .unwrap_or("");
        // Events are excluded from the digest by construction, on both paths.
        if schema == "ferroscope.Event" {
            continue;
        }
        let text = std::str::from_utf8(&m.data).ok()?;
        let v = json::parse(text)?;
        let step = v.get("step").and_then(|s| s.as_f64()).unwrap_or(0.0) as u64;

        let values = digest_values(schema, &v);
        digest.step(step, &ch.topic, &values);
        count += 1;

        if schema == "ferroscope.EnergySample" {
            let rail = match v.get("rail").and_then(|r| r.as_str()) {
                Some("compute") => Rail::Compute,
                Some("actuation") => Rail::Actuation,
                _ => Rail::Overhead,
            };
            let source = v.get("source").and_then(|s| s.as_str()).unwrap_or("");
            let watts = v.get("watts").and_then(|w| w.as_f64()).unwrap_or(0.0);
            ledger.sample(rail, source, m.log_time, watts);
        }
    }

    let recomputed = digest.finish();
    Some(Verification {
        trace_matches: recomputed == receipt.trace_digest,
        spec_matches: receipt.self_consistent(),
        recomputed,
        receipt,
        quote: ledger.quote(),
        messages: count,
    })
}

/// Rebuild the comparable trace from a recording, so two files produced on two machines can
/// be handed straight to [`ferroscope_receipt::compare`].
pub fn trace_from(bytes: &[u8]) -> Option<(Option<Receipt>, Trace)> {
    let log: Log = read(bytes).ok()?;
    let receipt = log
        .metadata_block(RECEIPT_BLOCK)
        .and_then(Receipt::from_pairs);
    let mut trace = Trace::default();
    for m in &log.messages {
        let ch = log.channel(m.channel_id)?;
        let schema = log
            .schema(ch.schema_id)
            .map(|s| s.name.as_str())
            .unwrap_or("");
        if schema == "ferroscope.Event" {
            continue;
        }
        let v = json::parse(std::str::from_utf8(&m.data).ok()?)?;
        let step = v.get("step").and_then(|s| s.as_f64()).unwrap_or(0.0) as u64;
        trace.push(step, ch.topic.clone(), digest_values(schema, &v));
    }
    Some((receipt, trace))
}

/// The numbers a payload contributes to the digest, in the same order the recorder fed them.
///
/// This must mirror [`Recorder`] field for field — which is exactly why the round-trip test
/// exists: a change on one side that is not made on the other fails immediately rather than
/// silently invalidating every receipt in the archive.
fn digest_values(schema: &str, v: &json::Value) -> Vec<f64> {
    let arr = |k: &str| -> Vec<f64> {
        v.get(k)
            .and_then(|x| x.as_array())
            .map(|a| a.iter().filter_map(|e| e.as_f64()).collect())
            .unwrap_or_default()
    };
    let one = |k: &str| -> Vec<f64> { v.get(k).and_then(|x| x.as_f64()).into_iter().collect() };

    match schema {
        "ferroscope.Transform" => {
            let mut out = arr("translation");
            out.extend(arr("rotation"));
            out
        }
        "ferroscope.JointState" => {
            let mut out = arr("position");
            out.extend(arr("velocity"));
            out.extend(arr("effort"));
            out
        }
        "ferroscope.Contact" => {
            let mut out = arr("point");
            out.extend(arr("normal"));
            out.extend(one("force_n"));
            out.extend(one("penetration_m"));
            out
        }
        "ferroscope.EnergySample" => one("watts"),
        "ferroscope.Scalar" => one("value"),
        _ => {
            // An unknown schema still contributes, in document order, so a recording is never
            // silently only-partly covered by its own digest.
            let mut out = Vec::new();
            v.numbers(&mut out);
            out
        }
    }
}
