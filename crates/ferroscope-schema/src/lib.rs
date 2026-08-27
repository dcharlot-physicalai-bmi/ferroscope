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
use ferroscope_mcap::{Log, Writer, WriterOptions, read};
use ferroscope_receipt::{Precision, Receipt, RunSpec, Trace, TraceDigest};

pub use bundle::{BundleFold, bundle, bundle_prefix, bundle_streaming};
pub use ferroscope_ledger as ledger;
pub use ferroscope_mcap as mcap;
pub use ferroscope_receipt as receipt;

/// The metadata block name a Ferroscope receipt lives under.
pub const RECEIPT_BLOCK: &str = "ferroscope.receipt";

/// The metadata block naming what it cost to PRODUCE this recording, on the machine that
/// produced it — measured, when that machine allowed it, or a stated reason it could not be.
///
/// Deliberately a separate block from the receipt, and deliberately outside both digests:
/// production cost varies run to run by nature (the same scene on a busy machine costs more),
/// so it can never be part of the determinism claim. The receipt says "this is the same
/// experiment"; this block says "and here is what making this copy of it cost".
pub const PRODUCTION_BLOCK: &str = "ferroscope.production";
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

/// A drawable primitive. A viewer draws what the recording declares, rather than guessing a
/// robot's shape from its transforms, which is the difference between a 3-D panel that shows the
/// machine and one that shows an axis triad.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shape {
    /// `size` is the full extent in x, y, z.
    Box,
    /// `size` is the three semi-axes. Equal values are a sphere; unequal ones an ellipsoid,
    /// which is what an inertia tensor's principal moments describe.
    Sphere,
    /// `size[0]` is the radius, `size[2]` the length along local z.
    Cylinder,
    /// An x-y plane of `size[0]` by `size[1]`, for a ground or a table.
    Plane,
    /// A polyline through `points`, in the parent frame.
    Lines,
    /// A mesh carried in the recording as an attachment, named by [`Geometry::mesh`].
    /// `size` is a scale factor per axis. Nothing outside the file is referenced, which is
    /// the point: a recording whose meshes live in a sibling directory is not evidence.
    Mesh,
}

impl Shape {
    pub fn as_str(&self) -> &'static str {
        match self {
            Shape::Box => "box",
            Shape::Sphere => "sphere",
            Shape::Cylinder => "cylinder",
            Shape::Plane => "plane",
            Shape::Lines => "lines",
            Shape::Mesh => "mesh",
        }
    }
}

/// One drawable, attached to a frame.
///
/// Log it once for static scenery and it persists; log it again on the same `(frame, id)` to move
/// or recolour it. `color` is deliberately **excluded from the trace digest**: a rendering choice
/// must never be able to change a determinism verdict, the same rule that keeps log lines out.
#[derive(Clone, Debug, PartialEq)]
pub struct Geometry {
    /// The frame this hangs off, matching a [`Transform`]'s `child`, or `world`.
    pub frame: String,
    /// Stable within the frame, so a later sample replaces this one instead of adding to it.
    pub id: String,
    pub shape: Shape,
    pub size: [f64; 3],
    /// Pose within the parent frame.
    pub translation: [f64; 3],
    /// `[x, y, z, w]`.
    pub rotation: [f64; 4],
    /// `[r, g, b, a]`, each 0 to 1.
    pub color: [f64; 4],
    /// Only for [`Shape::Lines`].
    pub points: Vec<[f64; 3]>,
    /// Only for [`Shape::Mesh`]: the attachment name holding the glTF binary.
    pub mesh: String,
}

impl Geometry {
    /// A box at the origin of its frame.
    pub fn boxed(frame: &str, id: &str, size: [f64; 3]) -> Geometry {
        Geometry {
            frame: frame.into(),
            id: id.into(),
            shape: Shape::Box,
            size,
            translation: [0.0; 3],
            rotation: [0.0, 0.0, 0.0, 1.0],
            color: [0.81, 0.67, 0.36, 1.0],
            points: Vec::new(),
            mesh: String::new(),
        }
    }

    /// A mesh from an attachment, scaled by `scale` on each axis.
    pub fn mesh(frame: &str, id: &str, attachment: &str, scale: [f64; 3]) -> Geometry {
        Geometry {
            shape: Shape::Mesh,
            size: scale,
            mesh: attachment.to_string(),
            ..Geometry::boxed(frame, id, scale)
        }
    }
    pub fn sphere(frame: &str, id: &str, radius: f64) -> Geometry {
        Geometry {
            shape: Shape::Sphere,
            size: [radius, radius, radius],
            ..Geometry::boxed(frame, id, [radius; 3])
        }
    }
    pub fn cylinder(frame: &str, id: &str, radius: f64, length: f64) -> Geometry {
        Geometry {
            shape: Shape::Cylinder,
            size: [radius, radius, length],
            ..Geometry::boxed(frame, id, [radius, radius, length])
        }
    }
    pub fn plane(frame: &str, id: &str, x: f64, y: f64) -> Geometry {
        Geometry {
            shape: Shape::Plane,
            size: [x, y, 0.0],
            color: [0.14, 0.19, 0.34, 1.0],
            ..Geometry::boxed(frame, id, [x, y, 0.0])
        }
    }
    pub fn lines(frame: &str, id: &str, points: Vec<[f64; 3]>) -> Geometry {
        Geometry {
            shape: Shape::Lines,
            points,
            color: [0.27, 0.78, 0.69, 1.0],
            ..Geometry::boxed(frame, id, [0.0; 3])
        }
    }
    pub fn at(mut self, translation: [f64; 3]) -> Geometry {
        self.translation = translation;
        self
    }
    pub fn oriented(mut self, rotation: [f64; 4]) -> Geometry {
        self.rotation = rotation;
        self
    }
    pub fn colored(mut self, color: [f64; 4]) -> Geometry {
        self.color = color;
        self
    }
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

    pub const GEOMETRY: &str = r#"{"type":"object","title":"ferroscope.Geometry","description":"One drawable primitive attached to a frame. Log once for static scenery; log again on the same (frame,id) to move it. color is excluded from the run's trace digest because a rendering choice must not change a determinism verdict.","properties":{"sim_ns":{"type":"integer"},"wall_ns":{"type":"integer"},"step":{"type":"integer"},"frame":{"type":"string"},"id":{"type":"string"},"shape":{"type":"string","enum":["box","sphere","cylinder","plane","lines","mesh"]},"size":{"type":"array","items":{"type":"number"},"minItems":3,"maxItems":3},"translation":{"type":"array","items":{"type":"number"},"minItems":3,"maxItems":3},"rotation":{"type":"array","items":{"type":"number"},"minItems":4,"maxItems":4},"color":{"type":"array","items":{"type":"number"},"minItems":4,"maxItems":4},"points":{"type":"array","items":{"type":"array","items":{"type":"number"}}},"mesh":{"type":"string","description":"For shape=mesh: the name of the attachment holding the glTF binary."}},"required":["frame","id","shape"]}"#;

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
    channel_schema: BTreeMap<String, &'static str>,
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
                // 64 KiB chunks, not the writer's 1 MiB default: a chunk is one record, a
                // record is the unit a live stream frames and a replay paces, and a megabyte
                // batches half a run into a single burst. 64 KiB keeps the stream's pulse
                // near a viewer's frame rate for ~1% of index overhead.
                WriterOptions::new(PROFILE, concat!("ferroscope ", env!("CARGO_PKG_VERSION")))
                    .chunk_target(64 << 10),
            ),
            schema_ids: BTreeMap::new(),
            channel_ids: BTreeMap::new(),
            channel_schema: BTreeMap::new(),
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
            // One channel is one schema. Reusing a topic for a second type would write payloads
            // a reader decodes with the wrong shape, and it would do so silently, which is how
            // a URDF recording ended up with transforms on geometry channels.
            let existing = self.channel_schema.get(topic).copied().unwrap_or("");
            if existing != schema_name {
                return Err(ferroscope_mcap::Error::SchemaConflict {
                    topic: topic.to_string(),
                    existing: existing.to_string(),
                    requested: schema_name.to_string(),
                });
            }
            return Ok(*id);
        }
        let sid = self.schema(schema_name, schema_text)?;
        let id = self.w.add_channel(topic, sid, "json", &[])?;
        self.channel_ids.insert(topic.to_string(), id);
        self.channel_schema.insert(topic.to_string(), schema_name);
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

    /// A drawable. Static scenery is one call before the loop; a moving part is one call per
    /// step on the same `(frame, id)`.
    pub fn geometry(&mut self, topic: &str, t: Stamp, g: &Geometry) -> ferroscope_mcap::Result<()> {
        let cid = self.channel(topic, "ferroscope.Geometry", schemas::GEOMETRY)?;
        let mut pts = String::from("[");
        for (i, p) in g.points.iter().enumerate() {
            if i > 0 {
                pts.push(',');
            }
            pts.push('[');
            for (j, x) in p.iter().enumerate() {
                if j > 0 {
                    pts.push(',');
                }
                json::write_number(&mut pts, *x);
            }
            pts.push(']');
        }
        pts.push(']');
        let payload = stamp_obj(t)
            .str("frame", &g.frame)
            .str("id", &g.id)
            .str("shape", g.shape.as_str())
            .nums("size", &g.size)
            .nums("translation", &g.translation)
            .nums("rotation", &g.rotation)
            .nums("color", &g.color)
            .raw("points", &pts)
            .str("mesh", &g.mesh)
            .finish();
        // The digest sees the geometry that could change a physical conclusion, and not the
        // colour it was drawn in.
        let mut v = g.size.to_vec();
        v.extend_from_slice(&g.translation);
        v.extend_from_slice(&g.rotation);
        for p in &g.points {
            v.extend_from_slice(p);
        }
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

    /// Carry a blob inside the recording: a mesh, a URDF, a calibration. Reference a mesh
    /// attachment from a [`Geometry`] with [`Geometry::mesh`].
    pub fn attach(
        &mut self,
        name: &str,
        media_type: &str,
        data: &[u8],
        t: Stamp,
    ) -> ferroscope_mcap::Result<()> {
        self.w
            .write_attachment(name, media_type, data, t.wall_ns, t.sim_ns)
    }

    /// Close the recording: write the receipt into the file's metadata, finish the MCAP,
    /// and hand back the sink, the receipt, and the energy quote.
    pub fn seal(
        self,
        spec: RunSpec,
        platform: &str,
    ) -> ferroscope_mcap::Result<(W, Receipt, Quote)> {
        self.seal_with(spec, platform, Vec::new)
    }

    /// [`Recorder::seal`], plus a production note.
    ///
    /// `production` is called after both digests are fixed and immediately before the block is
    /// written. What its note covers is whatever interval the caller's meter was primed over —
    /// prime before the work and the note spans the work; prime late and it does not. This
    /// method cannot vouch for the caller's timing, only for the block's placement. Its pairs land in
    /// [`PRODUCTION_BLOCK`]; an empty return writes no block at all. Because metadata records
    /// are not messages, nothing here can move either digest — [`verify`] recomputes from
    /// messages alone — and the test suite holds that as an invariant rather than trusting the
    /// construction.
    pub fn seal_with(
        mut self,
        spec: RunSpec,
        platform: &str,
        production: impl FnOnce() -> Vec<(String, String)>,
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
        let note = production();
        if !note.is_empty() {
            self.w.write_metadata(PRODUCTION_BLOCK, &note)?;
        }
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

/// Recompute a recording's receipt WITHOUT holding the recording.
///
/// [`verify`] takes the whole file as a slice and costs about 2.1x the file in memory; this
/// costs the largest single record. Both answer the same question, because recomputing a
/// receipt is a fold: hash each payload in file order, total the ledger, compare at the end.
///
/// It takes a way to OPEN the stream rather than a stream, because it needs two passes and the
/// reason is worth stating: the receipt is written by `seal`, so it sits at the END of the
/// file, and the digest cannot start until it knows the precision the receipt declares. The
/// first draft buffered messages until the block arrived — which is the whole recording, the
/// exact thing this function exists not to do. So pass one walks to the receipt and parses no
/// payloads; pass two hashes. Two cheap reads, flat memory.
///
/// ```no_run
/// # use std::fs::File;
/// let v = ferroscope_schema::verify_streaming(|| File::open("huge.mcap")).unwrap();
/// assert!(v.ok());
/// ```
pub fn verify_streaming<F, R>(open: F) -> Option<Verification>
where
    F: Fn() -> std::io::Result<R>,
    R: std::io::Read,
{
    let mut fold = VerifyFold::new();
    pour_into(open().ok()?, |b| fold.push(b)).ok()?;
    fold.rewind();
    pour_into(open().ok()?, |b| fold.push(b)).ok()?;
    fold.finish()
}

/// Read a stream into a pushed fold, one block at a time.
fn pour_into<R: std::io::Read>(
    mut r: R,
    mut push: impl FnMut(&[u8]) -> bool,
) -> std::io::Result<()> {
    let mut block = vec![0u8; 64 << 10];
    loop {
        match r.read(&mut block) {
            Ok(0) => return Ok(()),
            Ok(n) => {
                if !push(&block[..n]) {
                    return Ok(());
                }
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
}

/// A receipt recomputed from PUSHED blocks, with the ledger and the payload labels alongside.
///
/// The pushed form of [`verify_streaming`], which is a loop over it. Two passes, for the reason
/// that function gives: the receipt naming the precision to hash at is written by `seal` at the
/// end of the file, so nothing can be hashed until it has been read. Push the file, call
/// [`rewind`](VerifyFold::rewind), push it again, then [`finish`](VerifyFold::finish).
///
/// It picks up the channels' component labels on the way past, because the second pass parses
/// every payload anyway and a separate pass to learn that a joint is called `hip` would be a
/// third read of the file.
pub struct VerifyFold {
    feed: ferroscope_mcap::Feed,
    schemas: BTreeMap<u16, String>,
    channels: BTreeMap<u16, (String, u16)>,
    receipt: Option<Receipt>,
    /// Pass two only.
    digest: Option<TraceDigest>,
    ledger: Ledger,
    count: usize,
    joint_names: BTreeMap<String, Vec<String>>,
    second: bool,
    torn: bool,
}

impl Default for VerifyFold {
    fn default() -> Self {
        Self::new()
    }
}

impl VerifyFold {
    pub fn new() -> Self {
        Self {
            feed: ferroscope_mcap::Feed::new(),
            schemas: BTreeMap::new(),
            channels: BTreeMap::new(),
            receipt: None,
            digest: None,
            ledger: Ledger::new(),
            count: 0,
            joint_names: BTreeMap::new(),
            second: false,
            torn: false,
        }
    }

    /// Add the next block, in file order. Returns false once this pass has all it needs.
    pub fn push(&mut self, block: &[u8]) -> bool {
        use ferroscope_mcap::{Flow, Record};
        if self.torn || self.feed.finished() {
            return false;
        }
        self.feed.push(block);
        let (schemas, channels, receipt, digest, ledger, count, joint_names, second) = (
            &mut self.schemas,
            &mut self.channels,
            &mut self.receipt,
            &mut self.digest,
            &mut self.ledger,
            &mut self.count,
            &mut self.joint_names,
            self.second,
        );
        let outcome = self.feed.drain(&mut |rec| {
            match rec {
                Record::Schema(sc) => {
                    schemas.insert(sc.id, sc.name);
                }
                Record::Channel(ch) => {
                    channels.insert(ch.id, (ch.topic, ch.schema_id));
                }
                Record::Metadata { name, kv } if !second => {
                    if name == RECEIPT_BLOCK {
                        *receipt = Receipt::from_pairs(&kv);
                    }
                }
                Record::Message(m) if second => {
                    let Some((topic, schema_id)) = channels.get(&m.channel_id) else {
                        return Ok(Flow::Continue);
                    };
                    let schema = schemas.get(schema_id).map(|s| s.as_str()).unwrap_or("");
                    // Events are excluded from the digest by construction, on every path.
                    if schema == "ferroscope.Event" {
                        return Ok(Flow::Continue);
                    }
                    let Ok(text) = std::str::from_utf8(m.data) else {
                        return Ok(Flow::Continue);
                    };
                    let Some(v) = json::parse(text) else {
                        return Ok(Flow::Continue);
                    };
                    // Joint names live in the payload, so the FIRST message on a topic is the
                    // source — and only the first.
                    if schema == "ferroscope.JointState" && !joint_names.contains_key(topic) {
                        let names = v
                            .get("names")
                            .and_then(|x| x.as_array())
                            .map(|a| {
                                a.iter()
                                    .map(|e| e.as_str().unwrap_or("?").to_string())
                                    .collect::<Vec<_>>()
                            })
                            .unwrap_or_default();
                        joint_names.insert(topic.clone(), names);
                    }
                    let step = v.get("step").and_then(|s| s.as_f64()).unwrap_or(0.0) as u64;
                    if let Some(d) = digest.as_mut() {
                        d.step(step, topic, &digest_values(schema, &v));
                        *count += 1;
                    }
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
                _ => {}
            }
            Ok(Flow::Continue)
        });
        match outcome {
            Ok(Flow::Stop) => false,
            Ok(Flow::Continue) => true,
            Err(_) => {
                self.torn = true;
                false
            }
        }
    }

    /// End pass one and begin pass two. Push the same bytes again from the start.
    pub fn rewind(&mut self) {
        if self.torn || self.second {
            return;
        }
        self.digest = self.receipt.as_ref().map(|r| TraceDigest::new(r.precision));
        self.second = true;
        self.feed = ferroscope_mcap::Feed::new();
    }

    /// The receipt the file carries, whether or not it recomputes.
    pub fn receipt(&self) -> Option<&Receipt> {
        self.receipt.as_ref()
    }

    /// Each channel's component labels, in the payload's own terms — `effort[hip]`, not `[5]`.
    pub fn labels(&self) -> BTreeMap<String, Vec<String>> {
        let mut out = BTreeMap::new();
        for (topic, schema_id) in self.channels.values() {
            let schema = self.schemas.get(schema_id).map(|s| s.as_str()).unwrap_or("");
            let joint = self.joint_names.get(topic).cloned().unwrap_or_default();
            let labels = component_labels(schema, &joint);
            if !labels.is_empty() {
                out.insert(topic.clone(), labels);
            }
        }
        out
    }

    /// The verification, or `None` when the file carries no receipt to check against.
    pub fn finish(self) -> Option<Verification> {
        if self.torn {
            return None;
        }
        let receipt = self.receipt?;
        let recomputed = self.digest?.finish();
        Some(Verification {
            trace_matches: recomputed == receipt.trace_digest,
            spec_matches: receipt.self_consistent(),
            recomputed,
            receipt,
            quote: self.ledger.quote(),
            messages: self.count,
        })
    }
}

/// [`trace_from`] over a stream, so building a trajectory never holds the file.
///
/// The trace itself is still proportional to the run. [`profile_streaming`] is the way to
/// compare two recordings without building either one; this remains the fallback for the pairs
/// it refuses, and the way to get a trajectory when something other than a comparison wants it.
pub fn trace_from_streaming<R: std::io::Read>(r: R) -> Option<(Option<Receipt>, Trace)> {
    use ferroscope_mcap::{Flow, Record};

    let mut receipt = None;
    let mut schemas: BTreeMap<u16, String> = BTreeMap::new();
    let mut channels: BTreeMap<u16, (String, u16)> = BTreeMap::new();
    let mut trace = Trace::default();

    ferroscope_mcap::stream(r, |rec| {
        match rec {
            Record::Schema(sc) => {
                schemas.insert(sc.id, sc.name);
            }
            Record::Channel(ch) => {
                channels.insert(ch.id, (ch.topic, ch.schema_id));
            }
            Record::Metadata { name, kv } => {
                if name == RECEIPT_BLOCK {
                    receipt = Receipt::from_pairs(&kv);
                }
            }
            Record::Message(m) => {
                let Some((topic, schema_id)) = channels.get(&m.channel_id) else {
                    return Ok(Flow::Continue);
                };
                let schema = schemas.get(schema_id).map(|s| s.as_str()).unwrap_or("");
                if schema == "ferroscope.Event" {
                    return Ok(Flow::Continue);
                }
                let Ok(text) = std::str::from_utf8(m.data) else {
                    return Ok(Flow::Continue);
                };
                let Some(v) = json::parse(text) else {
                    return Ok(Flow::Continue);
                };
                let step = v.get("step").and_then(|s| s.as_f64()).unwrap_or(0.0) as u64;
                trace.push(step, topic.clone(), digest_values(schema, &v));
            }
            _ => {}
        }
        Ok(Flow::Continue)
    })
    .ok()?;
    Some((receipt, trace))
}

/// [`channel_labels`] over a stream: the layout of each channel, without the file.
pub fn channel_labels_streaming<R: std::io::Read>(r: R) -> BTreeMap<String, Vec<String>> {
    use ferroscope_mcap::{Flow, Record};

    let mut schemas: BTreeMap<u16, String> = BTreeMap::new();
    let mut channels: BTreeMap<u16, (String, u16)> = BTreeMap::new();
    let mut names: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();

    let _ = ferroscope_mcap::stream(r, |rec| {
        match rec {
            Record::Schema(sc) => {
                schemas.insert(sc.id, sc.name);
            }
            Record::Channel(ch) => {
                channels.insert(ch.id, (ch.topic, ch.schema_id));
            }
            Record::Message(m) => {
                // Joint names live in the payload, so the FIRST message on a topic is the
                // source — and only the first, which is what keeps this a single cheap pass.
                let Some((topic, schema_id)) = channels.get(&m.channel_id) else {
                    return Ok(Flow::Continue);
                };
                if schemas.get(schema_id).map(|s| s.as_str()) != Some("ferroscope.JointState") {
                    return Ok(Flow::Continue);
                }
                if names.contains_key(topic) {
                    return Ok(Flow::Continue);
                }
                let joint_names = std::str::from_utf8(m.data)
                    .ok()
                    .and_then(json::parse)
                    .and_then(|v| {
                        v.get("names").and_then(|x| x.as_array()).map(|a| {
                            a.iter()
                                .map(|e| e.as_str().unwrap_or("?").to_string())
                                .collect::<Vec<_>>()
                        })
                    })
                    .unwrap_or_default();
                names.insert(topic.clone(), joint_names);
            }
            _ => {}
        }
        Ok(Flow::Continue)
    });

    for (topic, schema_id) in channels.values() {
        let schema = schemas.get(schema_id).map(|s| s.as_str()).unwrap_or("");
        let joint = names.get(topic).cloned().unwrap_or_default();
        let labels = component_labels(schema, &joint);
        if !labels.is_empty() {
            out.insert(topic.clone(), labels);
        }
    }
    out
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
        "ferroscope.Geometry" => {
            let mut out = arr("size");
            out.extend(arr("translation"));
            out.extend(arr("rotation"));
            if let Some(json::Value::Arr(pts)) = v.get("points") {
                for p in pts {
                    if let Some(a) = p.as_array() {
                        out.extend(a.iter().filter_map(|e| e.as_f64()));
                    }
                }
            }
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

/// Human names for the components a channel contributes to the digest, per topic.
///
/// The comparator reports `channel[4]`, and `[4]` is not a name. The layouts are known — this
/// module packs them in [`digest_values`] — and `JointState` carries the joint names in its own
/// payload, so `effort[hip]` is available wherever `[4]` was printed. A reader chasing a
/// divergence should be told which quantity moved, not which array slot.
pub fn channel_labels(bytes: &[u8]) -> BTreeMap<String, Vec<String>> {
    let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let Ok(log) = read(bytes) else { return out };
    for ch in &log.channels {
        let schema = log
            .schema(ch.schema_id)
            .map(|s| s.name.as_str())
            .unwrap_or("");
        // Joint names live in the payload, so the first message on the topic is the source.
        let names: Vec<String> = if schema == "ferroscope.JointState" {
            log.messages_on(&ch.topic)
                .next()
                .and_then(|m| json::parse(std::str::from_utf8(&m.data).ok()?))
                .and_then(|v| {
                    v.get("names").and_then(|x| x.as_array()).map(|a| {
                        a.iter()
                            .map(|e| e.as_str().unwrap_or("?").to_string())
                            .collect()
                    })
                })
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        let labels = component_labels(schema, &names);
        if !labels.is_empty() {
            out.insert(ch.topic.clone(), labels);
        }
    }
    out
}

/// The component names for one schema, in the order [`digest_values`] packs them.
///
/// Mirrors that function exactly; a layout change on one side without the other shows up as a
/// label that names the wrong quantity, which is why the round-trip test covers both.
pub fn component_labels(schema: &str, joint_names: &[String]) -> Vec<String> {
    let per = |prefix: &str| -> Vec<String> {
        if joint_names.is_empty() {
            Vec::new()
        } else {
            joint_names
                .iter()
                .map(|n| format!("{prefix}[{n}]"))
                .collect()
        }
    };
    match schema {
        "ferroscope.Transform" => ["tx", "ty", "tz", "qx", "qy", "qz", "qw"]
            .iter()
            .map(|s| s.to_string())
            .collect(),
        "ferroscope.JointState" => {
            let mut v = per("position");
            v.extend(per("velocity"));
            v.extend(per("effort"));
            v
        }
        "ferroscope.Contact" => [
            "point.x",
            "point.y",
            "point.z",
            "normal.x",
            "normal.y",
            "normal.z",
            "force_n",
            "penetration_m",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect(),
        "ferroscope.Geometry" => [
            "size.x", "size.y", "size.z", "tx", "ty", "tz", "qx", "qy", "qz", "qw",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect(),
        "ferroscope.EnergySample" => vec!["watts".into()],
        "ferroscope.Scalar" => vec!["value".into()],
        _ => Vec::new(),
    }
}

/// Read only the receipt out of a recording, parsing no payloads at all.
///
/// The cheapest read there is: every message is skipped by opcode, so this costs one pass over
/// the bytes and nothing else. It exists because [`profile_streaming`] never builds a trace, and
/// the receipt used to arrive as a by-product of building one.
pub fn receipt_streaming<R: std::io::Read>(r: R) -> Option<Receipt> {
    use ferroscope_mcap::{Flow, Record};
    let mut receipt = None;
    ferroscope_mcap::stream(r, |rec| {
        if let Record::Metadata { name, kv } = rec
            && name == RECEIPT_BLOCK
        {
            receipt = Receipt::from_pairs(&kv);
        }
        Ok(Flow::Continue)
    })
    .ok()?;
    receipt
}

/// One recording's comparable samples, produced a block at a time.
///
/// The half of a streaming comparison that reads a file: push blocks in, take [`Sample`]s out in
/// file order. Two of these, advanced together, is what lets two recordings be compared without
/// either trajectory being held.
struct SampleFeed {
    feed: ferroscope_mcap::Feed,
    schemas: BTreeMap<u16, String>,
    channels: BTreeMap<u16, (String, u16)>,
    queue: std::collections::VecDeque<ferroscope_receipt::Sample>,
    /// Every sample this file produced, whether or not it found a partner. It is what turns a
    /// shared count into a statement about coverage.
    total: usize,
    last_step: u64,
    seen_channels: std::collections::BTreeSet<String>,
    /// Whether this file's steps only ever advance. The walk matches a step's samples as a
    /// group and relies on it; a file that goes backwards would have its samples skipped as
    /// unmatchable while the held comparison, which matches on `(channel, step)` wherever they
    /// lie, would pair them. Two answers to one question, so the walk refuses instead.
    ordered: bool,
    done: bool,
    torn: bool,
}

impl SampleFeed {
    fn new() -> Self {
        Self {
            feed: ferroscope_mcap::Feed::new(),
            schemas: BTreeMap::new(),
            channels: BTreeMap::new(),
            queue: std::collections::VecDeque::new(),
            total: 0,
            last_step: 0,
            seen_channels: std::collections::BTreeSet::new(),
            ordered: true,
            done: false,
            torn: false,
        }
    }

    /// Add the next block. Returns false once this file has no more to give.
    fn push(&mut self, block: &[u8]) -> bool {
        use ferroscope_mcap::{Flow, Record};
        if self.done {
            return false;
        }
        self.feed.push(block);
        let (schemas, channels, queue, total, last_step, seen, ordered) = (
            &mut self.schemas,
            &mut self.channels,
            &mut self.queue,
            &mut self.total,
            &mut self.last_step,
            &mut self.seen_channels,
            &mut self.ordered,
        );
        let outcome = self.feed.drain(&mut |rec| {
            match rec {
                Record::Schema(sc) => {
                    schemas.insert(sc.id, sc.name);
                }
                Record::Channel(ch) => {
                    channels.insert(ch.id, (ch.topic, ch.schema_id));
                }
                Record::Message(m) => {
                    let Some((topic, schema_id)) = channels.get(&m.channel_id) else {
                        return Ok(Flow::Continue);
                    };
                    let schema = schemas.get(schema_id).map(|s| s.as_str()).unwrap_or("");
                    // Events carry no numbers and are outside the comparison, exactly as they
                    // are outside the digest — a log line must not move a reproduction verdict.
                    if schema == "ferroscope.Event" {
                        return Ok(Flow::Continue);
                    }
                    let Ok(text) = std::str::from_utf8(m.data) else {
                        return Ok(Flow::Continue);
                    };
                    let Some(v) = json::parse(text) else {
                        return Ok(Flow::Continue);
                    };
                    let step = v.get("step").and_then(|s| s.as_f64()).unwrap_or(0.0) as u64;
                    *total += 1;
                    if step < *last_step {
                        *ordered = false;
                    }
                    *last_step = (*last_step).max(step);
                    if !seen.contains(topic.as_str()) {
                        seen.insert(topic.clone());
                    }
                    queue.push_back(ferroscope_receipt::Sample {
                        step,
                        channel: topic.clone(),
                        values: digest_values(schema, &v),
                    });
                }
                _ => {}
            }
            Ok(Flow::Continue)
        });
        match outcome {
            Ok(Flow::Stop) => {
                self.done = true;
                false
            }
            Ok(Flow::Continue) => true,
            Err(_) => {
                self.torn = true;
                self.done = true;
                false
            }
        }
    }

    fn ended(&mut self) {
        self.done = true;
    }
}

/// How many samples to keep queued on each side before matching. One block of records is worth
/// far fewer than this, so in practice a side is refilled once per block rather than per sample.
const MATCH_QUEUE: usize = 4096;

/// Two recordings compared as their bytes arrive, holding neither trajectory.
///
/// The pushed form of [`profile_streaming`], for the same reason
/// [`BundleFold`](crate::BundleFold) is the pushed form of the bundle: a browser cannot hand out
/// a [`Read`](std::io::Read), it hands over `File.slice()` blocks when they are ready. Feed both
/// sides, alternating as [`wants_a`](PairStream::wants_a) and [`wants_b`](PairStream::wants_b)
/// ask, say when each file has ended, and take the report.
///
/// The precondition is checked at every pair rather than assumed once: the two runs must present
/// the same `(channel, step)` sequence in file order. Where they do not,
/// [`refused`](PairStream::refused) goes true and [`finish`](PairStream::finish) yields nothing —
/// the caller falls back to building both trajectories. A comparator that silently paired the
/// wrong samples would be worse than a slow one.
pub struct PairStream {
    fa: SampleFeed,
    fb: SampleFeed,
    pair: ferroscope_receipt::Pairwise,
    refused: bool,
    /// `(channel, the side that is MISSING it)` -> `(first step, how many steps)`.
    gaps: BTreeMap<(String, &'static str), (u64, usize)>,
    /// The last step matched, so a file whose steps go backwards is refused rather than paired
    /// by guesswork.
    last_step: u64,
    a_ended: bool,
    b_ended: bool,
}

impl PairStream {
    pub fn new(tol: ferroscope_receipt::Tolerance) -> Self {
        Self {
            fa: SampleFeed::new(),
            fb: SampleFeed::new(),
            pair: ferroscope_receipt::Pairwise::new(tol),
            refused: false,
            gaps: BTreeMap::new(),
            last_step: 0,
            a_ended: false,
            b_ended: false,
        }
    }

    /// Whether side A wants another block. Feeding a side that does not want one is how a
    /// lockstep walk turns back into holding a file: the queue simply grows.
    ///
    /// A side also wants more while its front STEP is still incomplete, because a step's samples
    /// are matched as a group and a half-read group cannot be matched against anything.
    pub fn wants_a(&self) -> bool {
        !self.refused
            && !self.a_ended
            && (self.fa.queue.len() < MATCH_QUEUE || self.group_step(true).is_none())
    }

    pub fn wants_b(&self) -> bool {
        !self.refused
            && !self.b_ended
            && (self.fb.queue.len() < MATCH_QUEUE || self.group_step(false).is_none())
    }

    /// Add the next block of A, in file order. Returns whether A wants another.
    pub fn push_a(&mut self, block: &[u8]) -> bool {
        if !self.refused && !self.fa.push(block) {
            self.a_ended = true;
        }
        self.match_up();
        self.wants_a()
    }

    /// Add the next block of B, in file order. Returns whether B wants another.
    pub fn push_b(&mut self, block: &[u8]) -> bool {
        if !self.refused && !self.fb.push(block) {
            self.b_ended = true;
        }
        self.match_up();
        self.wants_b()
    }

    /// Say that A's bytes have run out.
    pub fn end_a(&mut self) {
        self.a_ended = true;
        self.fa.ended();
        self.match_up();
    }

    /// Say that B's bytes have run out.
    pub fn end_b(&mut self) {
        self.b_ended = true;
        self.fb.ended();
        self.match_up();
    }

    /// The last step A recorded — what a page's timeline is scaled by.
    pub fn a_last_step(&self) -> u64 {
        self.fa.last_step
    }

    /// The last step B recorded.
    pub fn b_last_step(&self) -> u64 {
        self.fb.last_step
    }

    /// Whether the two runs turned out not to line up.
    pub fn refused(&self) -> bool {
        self.refused || self.fa.torn || self.fb.torn || !self.fa.ordered || !self.fb.ordered
    }

    /// Pair off everything both sides can currently match.
    ///
    /// **By step, not by position.** The first version of this walked the two queues in
    /// lockstep and refused the moment their fronts disagreed, which is fine until a channel
    /// fires CONDITIONALLY: the demo records a contact only while the body is against its stop,
    /// so two runs that have genuinely diverged stop emitting the same samples at the same
    /// steps — and that is exactly the pair anybody wants compared. Measured, a 1.3 GB pair
    /// perturbed at step 400,000 refused outright and fell back to 4 GB of trajectories.
    ///
    /// So a step is the unit. Both files emit samples in nondecreasing step order, so the
    /// samples for one step form a complete group as soon as a later step appears; the two
    /// groups are matched channel by channel, and whatever one side has and the other lacks is
    /// a gap — which is precisely what the held comparison, matching on `(channel, step)`,
    /// computes. Memory is one step's samples per side.
    fn match_up(&mut self) {
        if !self.fa.ordered || !self.fb.ordered {
            self.refused = true;
        }
        while !self.refused {
            let a = self.group_step(true);
            let b = self.group_step(false);
            match (a, b) {
                (Some(x), Some(y)) if x == y => self.pair_group(x),
                (Some(x), Some(y)) if x < y => self.skip_group(true, x),
                (Some(_), Some(y)) => self.skip_group(false, y),
                // One side is spent, so nothing left on the other can ever find a partner.
                (Some(x), None) if self.b_ended && self.fb.queue.is_empty() => {
                    self.skip_group(true, x)
                }
                (None, Some(y)) if self.a_ended && self.fa.queue.is_empty() => {
                    self.skip_group(false, y)
                }
                _ => return,
            }
        }
    }

    /// The step of the front group, if that group is COMPLETE — a later step has arrived behind
    /// it, or the file has ended. Matching half a step's samples would report gaps that are
    /// only the reader's own impatience.
    fn group_step(&self, from_a: bool) -> Option<u64> {
        let (q, ended) = if from_a {
            (&self.fa.queue, self.a_ended)
        } else {
            (&self.fb.queue, self.b_ended)
        };
        let first = q.front()?.step;
        if ended || q.iter().any(|s| s.step != first) {
            Some(first)
        } else {
            None
        }
    }

    /// Take one step's samples off the front of a side.
    fn take_group(&mut self, from_a: bool, step: u64) -> Vec<ferroscope_receipt::Sample> {
        let q = if from_a {
            &mut self.fa.queue
        } else {
            &mut self.fb.queue
        };
        let mut out = Vec::new();
        while q.front().is_some_and(|s| s.step == step) {
            out.push(q.pop_front().expect("front was Some"));
        }
        out
    }

    /// Match one step's samples on both sides, channel by channel.
    fn pair_group(&mut self, step: u64) {
        let ga = self.take_group(true, step);
        let gb = self.take_group(false, step);
        if ga.iter().any(|s| s.step < self.last_step) || gb.iter().any(|s| s.step < self.last_step)
        {
            // Steps going backwards means the files are not ordered the way this walk assumes,
            // and guessing at the pairing is the one thing it must not do.
            self.refused = true;
            return;
        }
        self.last_step = step;

        // B's samples for this step, by channel, in order.
        let mut by_channel: BTreeMap<&str, std::collections::VecDeque<usize>> = BTreeMap::new();
        for (i, s) in gb.iter().enumerate() {
            by_channel.entry(s.channel.as_str()).or_default().push_back(i);
        }
        let mut used = vec![false; gb.len()];

        // A in FILE ORDER: the verdict is "the first crossing encountered", so the order pairs
        // are fed in is part of the answer.
        for sa in &ga {
            match by_channel
                .get_mut(sa.channel.as_str())
                .and_then(|q| q.pop_front())
            {
                Some(i) => {
                    used[i] = true;
                    let sb = &gb[i];
                    self.pair.push(step, &sa.channel, &sa.values, &sb.values);
                }
                None => self.pair.unmatched_a(),
            }
        }

        // Gaps, counted the way the held comparison counts them: per (channel, step), naming
        // which side is missing it, or "count" when both have it and disagree on how many.
        let mut na: BTreeMap<&str, usize> = BTreeMap::new();
        let mut nb: BTreeMap<&str, usize> = BTreeMap::new();
        for s in &ga {
            *na.entry(s.channel.as_str()).or_default() += 1;
        }
        for s in &gb {
            *nb.entry(s.channel.as_str()).or_default() += 1;
        }
        for (ch, &count) in &na {
            let other = nb.get(ch).copied().unwrap_or(0);
            if other == 0 {
                self.note_gap(ch, "B", step);
            } else if other != count {
                self.note_gap(ch, "count", step);
            }
        }
        for ch in nb.keys() {
            if !na.contains_key(ch) {
                self.note_gap(ch, "A", step);
            }
        }
        let _ = used;
    }

    /// One side has a step the other will never reach: every sample in it is unmatchable.
    fn skip_group(&mut self, from_a: bool, step: u64) {
        let g = self.take_group(from_a, step);
        let mut seen: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
        for s in &g {
            if from_a {
                self.pair.unmatched_a();
            }
            seen.insert(s.channel.as_str());
        }
        let side = if from_a { "B" } else { "A" };
        let channels: Vec<String> = seen.into_iter().map(str::to_string).collect();
        for ch in channels {
            self.note_gap(&ch, side, step);
        }
    }

    fn note_gap(&mut self, channel: &str, side: &'static str, step: u64) {
        let e = self
            .gaps
            .entry((channel.to_string(), side))
            .or_insert((step, 0));
        e.0 = e.0.min(step);
        e.1 += 1;
    }

    /// The report, or nothing if the two runs could not be walked together.
    pub fn finish(mut self) -> Option<ferroscope_receipt::Profile> {
        if self.refused() {
            return None;
        }
        self.end_a();
        self.end_b();
        if self.refused() {
            return None;
        }
        let structural = ferroscope_receipt::Structural {
            a_last_step: self.fa.last_step,
            b_last_step: self.fb.last_step,
            only_in_a: self
                .fa
                .seen_channels
                .difference(&self.fb.seen_channels)
                .cloned()
                .collect(),
            only_in_b: self
                .fb
                .seen_channels
                .difference(&self.fa.seen_channels)
                .cloned()
                .collect(),
            gaps: self
                .gaps
                .into_iter()
                .map(|((ch, side), (first, n))| (ch, first, n, side))
                .collect(),
            ..Default::default()
        };
        Some(self.pair.finish(structural, self.fa.total, self.fb.total))
    }
}

/// Compare two recordings **without holding either trajectory**.
///
/// `diff` was the last read verb that still bought its answer with memory, and the reason was
/// never the question — deciding where two runs parted is a fold over pairs, and
/// [`Pairwise`](ferroscope_receipt::Pairwise) is that fold — it was that the traces were built
/// first. This walks both files at once and feeds the pairs straight in.
///
/// The precondition is stated rather than assumed: the two runs must present the **same
/// (channel, step) sequence in file order**, which two runs of one spec do. Where they do not,
/// this returns `None` and the caller falls back to the trace comparison rather than guessing —
/// a comparator that silently pairs the wrong samples would be worse than a slow one. One run
/// stopping early is handled here, because it is common and because the leftover tail is
/// exactly what the report already calls a gap.
///
/// Memory is bounded by the channel count, the per-channel divergence history the shape
/// classifier needs, and one match queue per side — not by the two recordings.
///
/// This is a loop over [`PairStream`], which is the same fold pushed rather than pulled.
pub fn profile_streaming<FA, RA, FB, RB>(
    open_a: FA,
    open_b: FB,
    tol: ferroscope_receipt::Tolerance,
) -> Option<ferroscope_receipt::Profile>
where
    FA: Fn() -> std::io::Result<RA>,
    RA: std::io::Read,
    FB: Fn() -> std::io::Result<RB>,
    RB: std::io::Read,
{
    let mut ra = open_a().ok()?;
    let mut rb = open_b().ok()?;
    let mut s = PairStream::new(tol);
    let mut block = vec![0u8; 64 << 10];

    while !s.refused() && (s.wants_a() || s.wants_b()) {
        for side in [true, false] {
            let want = if side { s.wants_a() } else { s.wants_b() };
            if !want {
                continue;
            }
            let r: &mut dyn std::io::Read = if side { &mut ra } else { &mut rb };
            match r.read(&mut block) {
                Ok(0) => {
                    if side {
                        s.end_a();
                    } else {
                        s.end_b();
                    }
                }
                Ok(n) => {
                    if side {
                        s.push_a(&block[..n]);
                    } else {
                        s.push_b(&block[..n]);
                    }
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => return None,
            }
        }
    }
    s.finish()
}
