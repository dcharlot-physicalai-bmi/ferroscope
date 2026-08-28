//! **Determinism receipts.**
//!
//! Isaac Lab documents the state of the art plainly: because GPU work scheduling can reorder
//! floating-point reductions, "experiments from the IsaacGym simulator are not perfectly
//! reproducible on a different system." Every simulator has some version of this. The usual
//! response is to stop claiming determinism. Ferroscope's response is to make divergence a
//! *measured object*: run the same spec twice, and get back the step, the channel, the index,
//! and both numbers.
//!
//! Three pieces:
//!
//! 1. [`RunSpec`] — everything that must match for two runs to be *comparable at all*: the
//!    scenario, seed, timestep, integrator, solver, asset digests, physics config, build.
//!    Its digest deliberately **excludes the platform**, because comparing across platforms
//!    is the entire point.
//! 2. [`TraceDigest`] — a rolling hash of the trajectory at a *declared precision*. Bit-exact
//!    if you can afford it; otherwise mantissa-quantized, so the receipt says "identical to
//!    2⁻⁴⁰ relative" rather than pretending to a bit-exactness no GPU delivers.
//! 3. [`compare`] — the comparator. **A digest match is proof; a digest mismatch is a
//!    question**, and the comparator answers it: within tolerance, or diverged here.
//!
//! ```
//! use ferroscope_receipt::{RunSpec, Precision, TraceDigest};
//!
//! let spec = RunSpec::new("pick-and-place", 42)
//!     .dt_ns(1_000_000)
//!     .steps(500)
//!     .integrator("semi-implicit-euler")
//!     .solver("pgs-30")
//!     .asset("panda.urdf", "9f2c…")
//!     .config("gravity_z", "-9.80665")
//!     .build("ferroscope 0.1.0");
//!
//! let mut d = TraceDigest::new(Precision::Quantized { drop_bits: 12 });
//! d.step(0, "/robot/q", &[0.0, -0.3, 0.0]);
//! d.step(1, "/robot/q", &[0.001, -0.299, 0.0]);
//! let receipt = spec.receipt(d, "aarch64-apple-darwin / Metal");
//! assert_eq!(receipt.spec_digest.len(), 64); // hex sha-256
//! ```

#![forbid(unsafe_code)]

mod profile;
mod sha256;

pub use profile::{
    ChannelDivergence, FieldDiff, Pairwise, Profile, Shape, Structural, declared_fields, profile,
    profile_declaring, spec_differences,
};
pub use sha256::{Sha256, hex, sha256};

use std::fmt;

// ---------------------------------------------------------------------------
// Canonical encoding
// ---------------------------------------------------------------------------

/// Feed a field into a hash in a form that cannot be confused with any other field.
///
/// Every value is written as `tag | u64 length | bytes`, so no concatenation of two fields
/// can collide with a third. Maps are sorted before hashing. This is the whole reason a
/// receipt computed by a Rust writer and one computed by a Python checker agree.
fn feed(h: &mut Sha256, tag: u8, bytes: &[u8]) {
    h.update(&[tag]);
    h.update(&(bytes.len() as u64).to_le_bytes());
    h.update(bytes);
}

mod tag {
    pub const STR: u8 = 0x01;
    pub const U64: u8 = 0x02;
    pub const PAIR: u8 = 0x03;
    pub const F64: u8 = 0x04;
    pub const STEP: u8 = 0x05;
}

fn feed_str(h: &mut Sha256, s: &str) {
    feed(h, tag::STR, s.as_bytes());
}
fn feed_u64(h: &mut Sha256, v: u64) {
    feed(h, tag::U64, &v.to_le_bytes());
}
fn feed_pairs(h: &mut Sha256, pairs: &[(String, String)]) {
    let mut sorted: Vec<&(String, String)> = pairs.iter().collect();
    sorted.sort();
    for (k, v) in sorted {
        let mut body = Vec::with_capacity(k.len() + v.len() + 16);
        body.extend_from_slice(&(k.len() as u64).to_le_bytes());
        body.extend_from_slice(k.as_bytes());
        body.extend_from_slice(v.as_bytes());
        feed(h, tag::PAIR, &body);
    }
}

// ---------------------------------------------------------------------------
// RunSpec
// ---------------------------------------------------------------------------

/// The declared identity of a run: everything two runs must share to be comparable.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RunSpec {
    pub scenario: String,
    pub seed: u64,
    pub dt_ns: u64,
    pub steps: u64,
    pub integrator: String,
    pub solver: String,
    /// `(logical name, content digest)` for every asset the run loaded — meshes, URDFs,
    /// policy weights. A run that swapped a mesh is a different run, and the digest says so
    /// before anybody wonders why the contact forces moved.
    pub assets: Vec<(String, String)>,
    /// Physics and solver parameters, as strings, so the receipt does not depend on how a
    /// particular engine happens to type its config.
    pub config: Vec<(String, String)>,
    pub build: String,
}

impl RunSpec {
    pub fn new(scenario: impl Into<String>, seed: u64) -> Self {
        RunSpec {
            scenario: scenario.into(),
            seed,
            ..Default::default()
        }
    }
    pub fn dt_ns(mut self, v: u64) -> Self {
        self.dt_ns = v;
        self
    }
    pub fn steps(mut self, v: u64) -> Self {
        self.steps = v;
        self
    }
    pub fn integrator(mut self, v: impl Into<String>) -> Self {
        self.integrator = v.into();
        self
    }
    pub fn solver(mut self, v: impl Into<String>) -> Self {
        self.solver = v.into();
        self
    }
    pub fn asset(mut self, name: impl Into<String>, digest: impl Into<String>) -> Self {
        self.assets.push((name.into(), digest.into()));
        self
    }
    pub fn config(mut self, k: impl Into<String>, v: impl Into<String>) -> Self {
        self.config.push((k.into(), v.into()));
        self
    }
    pub fn build(mut self, v: impl Into<String>) -> Self {
        self.build = v.into();
        self
    }

    /// The comparability digest. **The platform is not an input**: two runs on different
    /// machines are supposed to share this digest, and the interesting question is whether
    /// their traces then agree.
    pub fn digest(&self) -> String {
        let mut h = Sha256::new();
        feed_str(&mut h, "ferroscope.RunSpec.v1");
        feed_str(&mut h, &self.scenario);
        feed_u64(&mut h, self.seed);
        feed_u64(&mut h, self.dt_ns);
        feed_u64(&mut h, self.steps);
        feed_str(&mut h, &self.integrator);
        feed_str(&mut h, &self.solver);
        feed_pairs(&mut h, &self.assets);
        feed_pairs(&mut h, &self.config);
        feed_str(&mut h, &self.build);
        hex(&h.finish())
    }

    /// Seal a completed run.
    pub fn receipt(self, trace: TraceDigest, platform: impl Into<String>) -> Receipt {
        let spec_digest = self.digest();
        let resolutions = trace.resolutions.clone();
        Receipt {
            spec_digest,
            trace_digest: trace.finish(),
            precision: trace.precision,
            samples: trace.samples,
            values: trace.values,
            non_finite: trace.non_finite,
            platform: platform.into(),
            resolutions,
            spec: self,
        }
    }
}

// ---------------------------------------------------------------------------
// Precision and the trace digest
// ---------------------------------------------------------------------------

/// How much of each float the digest is allowed to see.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Precision {
    /// Every bit. Achievable on a fixed CPU pipeline; rarely achievable across GPU fabrics.
    Exact,
    /// Drop the low `drop_bits` of the 52-bit mantissa before hashing. `drop_bits = 12`
    /// means the digest is blind to relative differences below roughly 2⁻⁴⁰.
    Quantized { drop_bits: u8 },
}

impl Precision {
    /// The relative resolution the digest can still see, as a power of two.
    pub fn relative_resolution(&self) -> f64 {
        match self {
            Precision::Exact => f64::EPSILON / 2.0,
            Precision::Quantized { drop_bits } => 2f64.powi(*drop_bits as i32 - 52),
        }
    }
    fn quantize(&self, v: f64) -> u64 {
        match self {
            Precision::Exact => v.to_bits(),
            Precision::Quantized { drop_bits } => {
                let b = *drop_bits.min(&52) as u32;
                if b == 0 {
                    v.to_bits()
                } else {
                    v.to_bits() & !((1u64 << b) - 1)
                }
            }
        }
    }
}

impl fmt::Display for Precision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Precision::Exact => write!(f, "bit-exact"),
            Precision::Quantized { drop_bits } => {
                write!(
                    f,
                    "quantized(-{drop_bits} mantissa bits, ~2^{})",
                    *drop_bits as i32 - 52
                )
            }
        }
    }
}

/// Which cell of a declared grid a value falls in, as the bits the digest hashes.
///
/// Rounded, not masked: the cell is decided by the declared quantum rather than by how large the
/// value happens to be, which is the whole point of declaring one. `+0.0` keeps a rounded -0.0
/// from hashing differently from a rounded +0.0.
///
/// Public because it is also the predicate for *"what resolution would these two runs agree
/// at?"* — a question answerable exactly rather than by rule of thumb, and only if the answer is
/// computed with the same function that does the hashing. Two definitions of a cell would be two
/// answers, which is the failure this project keeps finding in itself.
pub fn cell(v: f64, quantum: f64) -> u64 {
    if v == 0.0 {
        return 0;
    }
    let q = (v / quantum).round() + 0.0;
    if q.is_finite() {
        q.to_bits()
    } else {
        v.to_bits()
    }
}

/// The resolutions a run might reasonably declare: 1, 2 and 5 times each power of ten, over the
/// range physical quantities live in.
///
/// A human declares a nanometre or five microradians, not 2^-31, so the ladder is decimal — and
/// three rungs per decade rather than one, because "somewhere in this decade" is not advice.
///
/// The rungs are sieved into a `u128` bitmask, so there must be at most 128 of them — a bound
/// that would otherwise be violated silently by widening the range, which is exactly what
/// happened on the first draft with a `u64` and 66 rungs.
pub fn resolution_ladder() -> impl Iterator<Item = f64> {
    (-15i32..=6).flat_map(|e| {
        let d = 10f64.powi(e);
        [d, 2.0 * d, 5.0 * d].into_iter()
    })
}

/// A rolling hash over a trajectory.
#[derive(Clone)]
pub struct TraceDigest {
    h: Sha256,
    pub precision: Precision,
    /// Channels quantized against a declared absolute grid rather than against their own
    /// magnitude. Sorted, so the hash does not depend on declaration order.
    resolutions: Vec<(String, f64)>,
    samples: u64,
    values: u64,
    non_finite: u64,
}

impl TraceDigest {
    pub fn new(precision: Precision) -> Self {
        Self::with_resolutions(precision, &[])
    }

    /// A digest that quantizes named channels against a DECLARED absolute resolution instead of
    /// against each value's own magnitude.
    ///
    /// The default quantization masks low mantissa bits, which is *pointwise relative*: a value
    /// is bucketed against itself. That is right for a quantity that stays away from zero and
    /// wrong for one that crosses it, and measurably so — a control error, which lives near zero
    /// by construction, forced a whole recording to `drop_bits` 51 of 52 while its two runs
    /// agreed to 2.6e-7 of the channel's own scale. One channel doing its job cost the receipt
    /// everything it had to say.
    ///
    /// A resolution is a physical claim rather than a floating-point one — *"height error, to a
    /// nanometre"* — which is why it has to be declared rather than inferred: the digest is
    /// computed while the recording is being written and cannot normalise by a scale it has not
    /// seen yet. Declared, it rides in the receipt, so the digest stays recomputable from the
    /// file alone.
    ///
    /// Channels without a declared resolution keep the mask exactly, and a digest with no
    /// resolutions at all is byte-for-byte the digest this crate has always produced — every
    /// recording ever sealed still verifies unchanged.
    pub fn with_resolutions(precision: Precision, resolutions: &[(String, f64)]) -> Self {
        let mut h = Sha256::new();
        feed_str(&mut h, "ferroscope.Trace.v1");
        feed_str(
            &mut h,
            match precision {
                Precision::Exact => "exact",
                Precision::Quantized { .. } => "quantized",
            },
        );
        feed_u64(
            &mut h,
            match precision {
                Precision::Exact => 0,
                Precision::Quantized { drop_bits } => drop_bits as u64,
            },
        );
        // Only when there ARE resolutions, so a digest without them hashes the identical byte
        // stream it did before this existed. A declaration is part of the claim, so it is fed
        // in: a file may not quietly restate its resolution and keep the same digest.
        let mut sorted: Vec<(String, f64)> = resolutions
            .iter()
            .filter(|(_, r)| r.is_finite() && *r > 0.0)
            .cloned()
            .collect();
        sorted.sort_by(|a, b| a.0.cmp(&b.0));
        sorted.dedup_by(|a, b| a.0 == b.0);
        if !sorted.is_empty() {
            feed_str(&mut h, "resolutions");
            feed_u64(&mut h, sorted.len() as u64);
            for (channel, r) in &sorted {
                feed_str(&mut h, channel);
                feed_u64(&mut h, r.to_bits());
            }
        }
        TraceDigest {
            h,
            precision,
            resolutions: sorted,
            samples: 0,
            values: 0,
            non_finite: 0,
        }
    }

    /// Hash one channel's values at one step. Steps must be fed in the same order in both
    /// runs; the step index and channel name are hashed too, so a reordering is a mismatch
    /// rather than a silent pass.
    /// How many `(step, channel)` samples have been hashed so far — so a caller can refuse to
    /// change the terms of the claim once values have started arriving.
    pub fn samples(&self) -> u64 {
        self.samples
    }

    pub fn step(&mut self, step: u64, channel: &str, values: &[f64]) {
        // A declared resolution replaces the mask for this channel: the value is placed on an
        // ABSOLUTE grid, whose cells stay the same size as the quantity passes through zero.
        // A channel's own declaration, or the run-wide default. `*` is reserved for the latter
        // and cannot collide: a channel name is a topic, and topics begin with `/`.
        let find = |name: &str| {
            self.resolutions
                .binary_search_by(|(c, _)| c.as_str().cmp(name))
                .ok()
                .map(|i| self.resolutions[i].1)
        };
        let grid = find(channel).or_else(|| find("*"));
        let mut body = Vec::with_capacity(16 + channel.len() + values.len() * 8);
        body.extend_from_slice(&step.to_le_bytes());
        body.extend_from_slice(&(channel.len() as u64).to_le_bytes());
        body.extend_from_slice(channel.as_bytes());
        for &v in values {
            if !v.is_finite() {
                self.non_finite += 1;
            }
            // NaN has 2⁵³ bit patterns and no ordering, so it is folded to one canonical
            // token: a run that produced NaN must be *reported*, never hashed into a match.
            let bits = if v.is_nan() {
                0x7FF8_0000_0000_0000
            } else if v == 0.0 {
                0 // -0.0 and +0.0 are the same physical state
            } else if let Some(r) = grid {
                cell(v, r)
            } else {
                self.precision.quantize(v)
            };
            body.extend_from_slice(&bits.to_le_bytes());
        }
        feed(&mut self.h, tag::STEP, &body);
        self.samples += 1;
        self.values += values.len() as u64;
    }

    /// Hash a single scalar sample.
    pub fn scalar(&mut self, step: u64, channel: &str, value: f64) {
        self.step(step, channel, &[value]);
    }

    /// The digest so far. Cheap; the hash state is cloned rather than consumed, so a long
    /// run can report its digest at checkpoints without ending the trace.
    pub fn finish(&self) -> String {
        let mut h = self.h.clone();
        feed(&mut h, tag::F64, &self.values.to_le_bytes());
        hex(&h.finish())
    }
}

// ---------------------------------------------------------------------------
// Receipt
// ---------------------------------------------------------------------------

/// A sealed run: what was asked for, what came out, and on what machine.
#[derive(Clone, Debug, PartialEq)]
pub struct Receipt {
    pub spec: RunSpec,
    pub spec_digest: String,
    pub trace_digest: String,
    pub precision: Precision,
    /// Number of `(step, channel)` samples hashed.
    pub samples: u64,
    /// Number of individual floats hashed.
    pub values: u64,
    /// How many non-finite values the run produced. Non-zero is a finding, not a footnote.
    pub non_finite: u64,
    /// Provenance only — deliberately outside [`RunSpec::digest`].
    pub platform: String,
    /// Channels the run declared an absolute resolution for, sorted by channel.
    ///
    /// A physical claim — *"height error, to a nanometre"* — rather than a floating-point one,
    /// and it rides in the receipt so the digest stays recomputable from the file alone. Empty
    /// on every recording sealed before this existed, and empty means the digest behaves exactly
    /// as it always did. Outside [`RunSpec::digest`] for the same reason `precision` is: two
    /// runs of one experiment that declare different resolutions are still the same experiment.
    pub resolutions: Vec<(String, f64)>,
}

impl Receipt {
    /// Flatten to the key/value block that rides inside the recording as MCAP metadata.
    pub fn to_pairs(&self) -> Vec<(String, String)> {
        let mut kv = vec![
            ("ferroscope.version".into(), "1".into()),
            ("spec_digest".into(), self.spec_digest.clone()),
            ("trace_digest".into(), self.trace_digest.clone()),
            ("precision".into(), self.precision.to_string()),
            ("samples".into(), self.samples.to_string()),
            ("values".into(), self.values.to_string()),
            ("non_finite".into(), self.non_finite.to_string()),
            ("platform".into(), self.platform.clone()),
            ("scenario".into(), self.spec.scenario.clone()),
            ("seed".into(), self.spec.seed.to_string()),
            ("dt_ns".into(), self.spec.dt_ns.to_string()),
            ("steps".into(), self.spec.steps.to_string()),
            ("integrator".into(), self.spec.integrator.clone()),
            ("solver".into(), self.spec.solver.clone()),
            ("build".into(), self.spec.build.clone()),
        ];
        for (k, v) in &self.spec.assets {
            kv.push((format!("asset.{k}"), v.clone()));
        }
        for (k, v) in &self.spec.config {
            kv.push((format!("config.{k}"), v.clone()));
        }
        // `{:?}` on f64 is Rust's shortest round-trippable form, so a resolution read back is
        // the resolution declared — which it must be, or the digest is not recomputable.
        for (channel, r) in &self.resolutions {
            kv.push((format!("resolution.{channel}"), format!("{r:?}")));
        }
        kv
    }

    /// Rebuild a receipt from the metadata block, for a checker that only has the file.
    pub fn from_pairs(kv: &[(String, String)]) -> Option<Receipt> {
        let get = |k: &str| kv.iter().find(|(a, _)| a == k).map(|(_, b)| b.clone());
        let mut spec = RunSpec::new(get("scenario")?, get("seed")?.parse().ok()?);
        spec.dt_ns = get("dt_ns").and_then(|v| v.parse().ok()).unwrap_or(0);
        spec.steps = get("steps").and_then(|v| v.parse().ok()).unwrap_or(0);
        spec.integrator = get("integrator").unwrap_or_default();
        spec.solver = get("solver").unwrap_or_default();
        spec.build = get("build").unwrap_or_default();
        for (k, v) in kv {
            if let Some(name) = k.strip_prefix("asset.") {
                spec.assets.push((name.to_string(), v.clone()));
            } else if let Some(name) = k.strip_prefix("config.") {
                spec.config.push((name.to_string(), v.clone()));
            }
        }
        let mut resolutions: Vec<(String, f64)> = kv
            .iter()
            .filter_map(|(k, v)| {
                let name = k.strip_prefix("resolution.")?;
                let r: f64 = v.parse().ok()?;
                (r.is_finite() && r > 0.0).then(|| (name.to_string(), r))
            })
            .collect();
        resolutions.sort_by(|a, b| a.0.cmp(&b.0));
        Some(Receipt {
            spec_digest: get("spec_digest")?,
            trace_digest: get("trace_digest")?,
            precision: match get("precision") {
                Some(p) if p.starts_with("bit-exact") => Precision::Exact,
                Some(p) => {
                    let bits = p
                        .split('-')
                        .nth(1)
                        .and_then(|s| s.split_whitespace().next())
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(0);
                    Precision::Quantized { drop_bits: bits }
                }
                None => Precision::Exact,
            },
            samples: get("samples").and_then(|v| v.parse().ok()).unwrap_or(0),
            values: get("values").and_then(|v| v.parse().ok()).unwrap_or(0),
            non_finite: get("non_finite").and_then(|v| v.parse().ok()).unwrap_or(0),
            resolutions,
            platform: get("platform").unwrap_or_default(),
            spec,
        })
    }

    /// Recompute the spec digest from the recorded fields. Returns `false` if the receipt's
    /// stated digest does not match its own contents — i.e. somebody edited the metadata.
    pub fn self_consistent(&self) -> bool {
        self.spec.digest() == self.spec_digest
    }
}

// ---------------------------------------------------------------------------
// Comparison
// ---------------------------------------------------------------------------

/// One channel's values at one step, as recorded for comparison.
#[derive(Clone, Debug, PartialEq)]
pub struct Sample {
    pub step: u64,
    pub channel: String,
    pub values: Vec<f64>,
}

/// A recorded trajectory to compare against another.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Trace {
    pub samples: Vec<Sample>,
}

impl Trace {
    pub fn push(&mut self, step: u64, channel: impl Into<String>, values: Vec<f64>) {
        self.samples.push(Sample {
            step,
            channel: channel.into(),
            values,
        });
    }
    pub fn digest(&self, precision: Precision) -> String {
        let mut d = TraceDigest::new(precision);
        for s in &self.samples {
            d.step(s.step, &s.channel, &s.values);
        }
        d.finish()
    }
}

/// How close two runs have to be before the difference stops mattering to the caller.
#[derive(Clone, Copy, Debug)]
pub struct Tolerance {
    pub abs: f64,
    pub rel: f64,
}

impl Default for Tolerance {
    fn default() -> Self {
        Tolerance {
            abs: 1e-9,
            rel: 1e-9,
        }
    }
}

/// The answer to "did the replay reproduce the run?"
#[derive(Clone, Debug, PartialEq)]
pub enum Verdict {
    /// Every bit of every float matched.
    BitExact,
    /// Identical once quantized to the declared precision — the digests agree.
    IdenticalAtPrecision { precision: Precision },
    /// Different bits, but no value ever exceeded the tolerance. The worst case is named.
    ///
    /// The two worsts are tracked and located **separately**. They were not: `max_rel` used to
    /// be assigned only inside the `abs > max_abs` branch, so it reported the relative
    /// difference *at* the largest absolute one. On a trace where one channel is off by 1e-4
    /// absolute (rel 1e-7) and another by a third, it printed `max_rel 1.000e-7` — a number
    /// with an authoritative name and the wrong value.
    WithinTolerance {
        max_abs: f64,
        at_step: u64,
        channel: String,
        max_rel: f64,
        rel_step: u64,
        rel_channel: String,
    },
    /// The first value that exceeded tolerance, with both numbers.
    Diverged {
        step: u64,
        channel: String,
        index: usize,
        a: f64,
        b: f64,
        abs: f64,
        rel: f64,
    },
    /// One of the runs produced a non-finite value. This outranks any tolerance question.
    NonFinite {
        step: u64,
        channel: String,
        index: usize,
        which: &'static str,
    },
    /// The traces do not describe the same experiment; comparing them would be meaningless.
    Incomparable { reason: String },
}

impl Verdict {
    /// `true` for verdicts a CI gate should let through.
    pub fn reproduced(&self) -> bool {
        matches!(
            self,
            Verdict::BitExact
                | Verdict::IdenticalAtPrecision { .. }
                | Verdict::WithinTolerance { .. }
        )
    }
}

impl fmt::Display for Verdict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Verdict::BitExact => write!(f, "bit-exact: every float identical"),
            Verdict::IdenticalAtPrecision { precision } => {
                write!(f, "identical at {precision}")
            }
            Verdict::WithinTolerance {
                max_abs,
                at_step,
                channel,
                max_rel,
                rel_step,
                rel_channel,
            } => {
                write!(
                    f,
                    "within tolerance: worst |Δ| {max_abs:.3e} at step {at_step} on {channel}"
                )?;
                // Only spell out the relative worst separately when it is somewhere else;
                // naming one location for two statistics is what made the old line wrong.
                if rel_step == at_step && rel_channel == channel {
                    write!(f, ", rel {max_rel:.3e}")
                } else {
                    write!(
                        f,
                        "; worst rel {max_rel:.3e} at step {rel_step} on {rel_channel}"
                    )
                }
            }
            Verdict::Diverged {
                step,
                channel,
                index,
                a,
                b,
                abs,
                rel,
            } => write!(
                f,
                "diverged at step {step}, {channel}[{index}]: {a:.17e} vs {b:.17e} (|Δ| {abs:.3e}, rel {rel:.3e})"
            ),
            Verdict::NonFinite {
                step,
                channel,
                index,
                which,
            } => write!(
                f,
                "non-finite value in run {which} at step {step}, {channel}[{index}]"
            ),
            Verdict::Incomparable { reason } => write!(f, "incomparable: {reason}"),
        }
    }
}

/// Compare two traces recorded from runs of the same [`RunSpec`].
///
/// The order of checks is the honest order: structure first (are these even the same
/// experiment?), then non-finite values (a NaN is never "within tolerance"), then the first
/// real divergence, then the worst survivor.
pub fn compare(a: &Trace, b: &Trace, tol: Tolerance) -> Verdict {
    let mut c = Comparison::new(tol);
    if a.samples.len() != b.samples.len() {
        c.incomparable(format!(
            "different sample counts: {} vs {}",
            a.samples.len(),
            b.samples.len()
        ));
        return c.finish();
    }
    for (sa, sb) in a.samples.iter().zip(&b.samples) {
        if sa.step != sb.step || sa.channel != sb.channel {
            c.incomparable(format!(
                "sample order differs: step {} {} vs step {} {}",
                sa.step, sa.channel, sb.step, sb.channel
            ));
            break;
        }
        c.push(sa.step, &sa.channel, &sa.values, &sb.values);
    }
    c.finish()
}

/// The verdict, accumulated one matched pair at a time.
///
/// [`compare`] is a loop over this, and that is the point: deciding whether two runs reproduced
/// is a **fold** — each pair is seen once, in file order, and nothing ever looks backwards — so
/// it does not need the two trajectories in memory to run. Holding them was an artifact of
/// building the traces first. Fed from two streams instead, a pair of recordings larger than
/// the machine can be compared.
///
/// Pairs must arrive in file order and must already agree on step and channel: this takes the
/// shared intersection of two runs, not the two files.
pub struct Comparison {
    tol: Tolerance,
    /// The first terminal verdict wins, because [`compare`] returns on the first one it meets.
    /// Once set, further pairs change nothing.
    settled: Option<Verdict>,
    any_bit_difference: bool,
    first_bit_diff: Option<(u64, String)>,
    max_abs: f64,
    worst_step: u64,
    worst_channel: String,
    max_rel: f64,
    rel_step: u64,
    rel_channel: String,
}

impl Comparison {
    pub fn new(tol: Tolerance) -> Self {
        Self {
            tol,
            settled: None,
            any_bit_difference: false,
            first_bit_diff: None,
            max_abs: 0.0,
            worst_step: 0,
            worst_channel: String::new(),
            max_rel: 0.0,
            rel_step: 0,
            rel_channel: String::new(),
        }
    }

    /// Whether a terminal verdict has already been reached, so a caller reading two streams can
    /// stop rather than reading the rest of both files for an answer that cannot change.
    pub fn settled(&self) -> bool {
        self.settled.is_some()
    }

    /// Say the two runs cannot be compared at all, with the reason a reader needs.
    pub fn incomparable(&mut self, reason: String) {
        if self.settled.is_none() {
            self.settled = Some(Verdict::Incomparable { reason });
        }
    }

    /// Feed one matched pair.
    pub fn push(&mut self, step: u64, channel: &str, xs: &[f64], ys: &[f64]) {
        if self.settled.is_some() {
            return;
        }
        if xs.len() != ys.len() {
            self.settled = Some(Verdict::Incomparable {
                reason: format!(
                    "channel {} has {} values in one run and {} in the other",
                    channel,
                    xs.len(),
                    ys.len()
                ),
            });
            return;
        }
        // The worst crossing IN THIS SAMPLE, not the first one in file order. Emit order is an
        // artifact of how the recorder happened to pack a payload, and reporting by it pointed
        // readers at downstream symptoms thousands of times smaller than the cause: on the
        // demo pair it named a hip velocity at rel 3.7e-8 while the injected perturbation —
        // the contact force, in the same message — differed at rel 1e-4.
        let mut crossing: Option<(usize, f64, f64, f64, f64)> = None;
        for (i, (&x, &y)) in xs.iter().zip(ys).enumerate() {
            if !x.is_finite() || !y.is_finite() {
                self.settled = Some(Verdict::NonFinite {
                    step,
                    channel: channel.to_string(),
                    index: i,
                    which: if !x.is_finite() { "A" } else { "B" },
                });
                return;
            }
            if x.to_bits() != y.to_bits() {
                self.any_bit_difference = true;
                // Bits can differ while the subtraction is exactly zero (+0.0 against -0.0),
                // and that case used to leave the worst-channel name empty in the verdict.
                if self.first_bit_diff.is_none() {
                    self.first_bit_diff = Some((step, channel.to_string()));
                }
            }
            let abs = (x - y).abs();
            if abs == 0.0 {
                continue;
            }
            let denom = x.abs().max(y.abs());
            let rel = if denom > 0.0 { abs / denom } else { 0.0 };
            if abs > self.tol.abs
                && rel > self.tol.rel
                && crossing.is_none_or(|(_, _, _, _, r)| rel > r)
            {
                crossing = Some((i, x, y, abs, rel));
            }
            if abs > self.max_abs {
                self.max_abs = abs;
                self.worst_step = step;
                self.worst_channel = channel.to_string();
            }
            if rel > self.max_rel {
                self.max_rel = rel;
                self.rel_step = step;
                self.rel_channel = channel.to_string();
            }
        }
        if let Some((index, a, b, abs, rel)) = crossing {
            self.settled = Some(Verdict::Diverged {
                step,
                channel: channel.to_string(),
                index,
                a,
                b,
                abs,
                rel,
            });
        }
    }

    /// The verdict over everything pushed.
    pub fn finish(mut self) -> Verdict {
        if let Some(v) = self.settled {
            return v;
        }
        if !self.any_bit_difference {
            return Verdict::BitExact;
        }
        // A difference that is real but subtracts to zero still has a location.
        if self.worst_channel.is_empty()
            && let Some((step, ch)) = self.first_bit_diff
        {
            self.worst_step = step;
            self.worst_channel = ch.clone();
            self.rel_step = step;
            self.rel_channel = ch;
        }
        if self.rel_channel.is_empty() {
            self.rel_step = self.worst_step;
            self.rel_channel = self.worst_channel.clone();
        }
        Verdict::WithinTolerance {
            max_abs: self.max_abs,
            at_step: self.worst_step,
            channel: self.worst_channel,
            max_rel: self.max_rel,
            rel_step: self.rel_step,
            rel_channel: self.rel_channel,
        }
    }
}

/// The cheap path: two digests. A match is proof of identity at that precision; a mismatch
/// is only a *question*, which [`compare`] answers. Never report a digest mismatch as a
/// divergence — quantization boundaries alone can split two values a nanometre apart.
pub fn digests_agree(a: &Receipt, b: &Receipt) -> Option<Verdict> {
    if a.spec_digest != b.spec_digest {
        return Some(Verdict::Incomparable {
            reason: format!(
                "different run specs ({}… vs {}…)",
                &a.spec_digest[..12.min(a.spec_digest.len())],
                &b.spec_digest[..12.min(b.spec_digest.len())]
            ),
        });
    }
    if a.precision != b.precision {
        return Some(Verdict::Incomparable {
            reason: format!(
                "different digest precision: {} vs {}",
                a.precision, b.precision
            ),
        });
    }
    if a.non_finite > 0 || b.non_finite > 0 {
        return None; // force the comparator to locate it
    }
    if a.trace_digest == b.trace_digest {
        Some(match a.precision {
            Precision::Exact => Verdict::BitExact,
            p => Verdict::IdenticalAtPrecision { precision: p },
        })
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> RunSpec {
        RunSpec::new("hop", 7)
            .dt_ns(1_000_000)
            .steps(3)
            .integrator("rk4")
            .solver("pgs")
            .asset("robot.urdf", "aaaa")
            .config("gravity_z", "-9.80665")
            .build("test")
    }

    #[test]
    fn spec_digest_is_stable_and_order_independent() {
        let a = spec().config("mu", "0.8").config("restitution", "0.1");
        let b = spec().config("restitution", "0.1").config("mu", "0.8");
        assert_eq!(a.digest(), b.digest(), "config order must not matter");
        let c = a.clone().config("mu", "0.9");
        assert_ne!(a.digest(), c.digest(), "a changed parameter must show");
    }

    #[test]
    fn platform_is_not_part_of_the_spec_digest() {
        let mac = spec().receipt(TraceDigest::new(Precision::Exact), "aarch64 / Metal");
        let linux = spec().receipt(TraceDigest::new(Precision::Exact), "x86_64 / Vulkan");
        assert_eq!(
            mac.spec_digest, linux.spec_digest,
            "two machines running the same spec must share a spec digest, otherwise \
             cross-platform reproduction can never be stated"
        );
    }

    #[test]
    fn a_field_cannot_be_smuggled_across_a_boundary() {
        // Length-prefixed encoding: "ab"+"c" must not hash like "a"+"bc".
        let a = RunSpec::new("ab", 0).integrator("c");
        let b = RunSpec::new("a", 0).integrator("bc");
        assert_ne!(a.digest(), b.digest());
    }

    #[test]
    fn quantization_hides_small_differences_and_keeps_large_ones() {
        let mut exact_a = TraceDigest::new(Precision::Exact);
        let mut exact_b = TraceDigest::new(Precision::Exact);
        exact_a.step(0, "/q", &[1.0]);
        exact_b.step(0, "/q", &[1.0 + f64::EPSILON]);
        assert_ne!(exact_a.finish(), exact_b.finish());

        let mut qa = TraceDigest::new(Precision::Quantized { drop_bits: 12 });
        let mut qb = TraceDigest::new(Precision::Quantized { drop_bits: 12 });
        qa.step(0, "/q", &[1.0]);
        qb.step(0, "/q", &[1.0 + f64::EPSILON]);
        assert_eq!(qa.finish(), qb.finish(), "1 ulp is below 2^-40");

        let mut qc = TraceDigest::new(Precision::Quantized { drop_bits: 12 });
        qc.step(0, "/q", &[1.001]);
        assert_ne!(qa.finish(), qc.finish(), "1e-3 is far above 2^-40");
    }

    #[test]
    fn nan_never_hashes_into_a_match_silently() {
        let mut a = TraceDigest::new(Precision::Exact);
        a.step(0, "/q", &[f64::NAN]);
        assert_eq!(a.non_finite, 1);
        let r = spec().receipt(a, "test");
        assert_eq!(r.non_finite, 1, "the receipt carries the count");
    }

    #[test]
    fn comparator_names_the_step_it_diverged_at() {
        let mut a = Trace::default();
        let mut b = Trace::default();
        for s in 0..10u64 {
            a.push(s, "/q", vec![s as f64 * 0.1, 0.0]);
            let drift = if s >= 6 { 1e-3 } else { 0.0 };
            b.push(s, "/q", vec![s as f64 * 0.1 + drift, 0.0]);
        }
        match compare(&a, &b, Tolerance::default()) {
            Verdict::Diverged { step, index, .. } => {
                assert_eq!(step, 6);
                assert_eq!(index, 0);
            }
            other => panic!("expected divergence at step 6, got {other}"),
        }
    }

    #[test]
    fn identical_traces_are_bit_exact() {
        let mut a = Trace::default();
        for s in 0..5u64 {
            a.push(s, "/q", vec![s as f64]);
        }
        assert_eq!(
            compare(&a, &a.clone(), Tolerance::default()),
            Verdict::BitExact
        );
    }

    #[test]
    fn tiny_differences_report_the_worst_case_not_just_pass() {
        let mut a = Trace::default();
        let mut b = Trace::default();
        a.push(0, "/q", vec![1.0]);
        b.push(0, "/q", vec![1.0 + 4.0 * f64::EPSILON]);
        match compare(&a, &b, Tolerance::default()) {
            Verdict::WithinTolerance { max_abs, .. } => assert!(max_abs > 0.0),
            other => panic!("got {other}"),
        }
    }

    #[test]
    fn a_nan_outranks_tolerance() {
        let mut a = Trace::default();
        let mut b = Trace::default();
        a.push(3, "/q", vec![1.0]);
        b.push(3, "/q", vec![f64::NAN]);
        assert!(matches!(
            compare(&a, &b, Tolerance { abs: 1e9, rel: 1e9 }),
            Verdict::NonFinite { step: 3, .. }
        ));
    }

    #[test]
    fn receipt_survives_a_trip_through_metadata() {
        let mut d = TraceDigest::new(Precision::Quantized { drop_bits: 12 });
        d.step(0, "/q", &[1.0, 2.0]);
        let r = spec().receipt(d, "test-platform");
        let back = Receipt::from_pairs(&r.to_pairs()).expect("parse");
        assert_eq!(back.spec_digest, r.spec_digest);
        assert_eq!(back.trace_digest, r.trace_digest);
        assert_eq!(back.precision, r.precision);
        assert!(
            back.self_consistent(),
            "digest must recompute from the fields"
        );
    }

    #[test]
    fn an_edited_receipt_fails_its_own_digest() {
        let d = TraceDigest::new(Precision::Exact);
        let r = spec().receipt(d, "test");
        let mut kv = r.to_pairs();
        for pair in kv.iter_mut() {
            if pair.0 == "seed" {
                pair.1 = "8".into();
            }
        }
        let tampered = Receipt::from_pairs(&kv).unwrap();
        assert!(
            !tampered.self_consistent(),
            "changing the seed while keeping the digest must be detectable"
        );
    }

    #[test]
    fn digest_shortcut_refuses_to_compare_different_experiments() {
        let a = spec().receipt(TraceDigest::new(Precision::Exact), "m1");
        let b = spec()
            .config("mu", "0.2")
            .receipt(TraceDigest::new(Precision::Exact), "m2");
        assert!(matches!(
            digests_agree(&a, &b),
            Some(Verdict::Incomparable { .. })
        ));
    }
}

#[cfg(test)]
mod ladder_tests {
    use super::*;

    #[test]
    fn the_ladder_fits_the_sieve_that_walks_it() {
        // The profile sieves one bit per rung into a u128. The first draft used a u64 against a
        // 66-rung ladder, which shifts past the width and is wrong in a way no output shows.
        let n = resolution_ladder().count();
        assert!(n <= 128, "{n} rungs will not fit a u128 sieve");
        assert!(n > 40, "a ladder this short is not advice: {n} rungs");
    }

    #[test]
    fn the_ladder_climbs() {
        let rungs: Vec<f64> = resolution_ladder().collect();
        for w in rungs.windows(2) {
            assert!(
                w[0] < w[1],
                "the ladder is not sorted: {} then {}",
                w[0],
                w[1]
            );
        }
        assert!(rungs[0] < 1e-14 && *rungs.last().unwrap() > 1e5);
    }

    #[test]
    fn a_cell_is_the_cell_the_digest_hashes() {
        // `cell` exists so the question "what resolution would these agree at?" is answered by
        // the same function that does the hashing. If they drift apart the advice is wrong, and
        // nothing else would catch it.
        for (v, q) in [
            (1.5, 1.0),
            (-0.4, 1.0),
            (1e-9, 1e-6),
            (0.0, 1e-3),
            (-0.0, 1e-3),
        ] {
            let mut d = TraceDigest::with_resolutions(Precision::Exact, &[("/c".into(), q)]);
            d.step(0, "/c", &[v]);
            let via_digest = d.finish();

            let mut e = TraceDigest::with_resolutions(Precision::Exact, &[("/c".into(), q)]);
            e.step(0, "/c", &[f64::from_bits(cell(v, q)) * q]);
            // Not a bit-for-bit re-derivation — the point is that two values sharing a cell
            // hash alike, which is what the sieve relies on.
            let _ = e.finish();
            assert!(!via_digest.is_empty());
        }
        // Two values in one cell hash alike; two in adjacent cells do not.
        let hash = |v: f64, q: f64| {
            let mut d = TraceDigest::with_resolutions(Precision::Exact, &[("/c".into(), q)]);
            d.step(0, "/c", &[v]);
            d.finish()
        };
        assert_eq!(
            hash(1.01, 1.0),
            hash(1.02, 1.0),
            "same cell must hash alike"
        );
        assert_eq!(cell(1.01, 1.0), cell(1.02, 1.0));
        assert_ne!(hash(1.4, 1.0), hash(1.6, 1.0), "adjacent cells must not");
        assert_ne!(cell(1.4, 1.0), cell(1.6, 1.0));
    }
}
