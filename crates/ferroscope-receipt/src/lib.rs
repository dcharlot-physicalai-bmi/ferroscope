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

mod sha256;
pub use sha256::{hex, sha256, Sha256};

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
        Receipt {
            spec_digest,
            trace_digest: trace.finish(),
            precision: trace.precision,
            samples: trace.samples,
            values: trace.values,
            non_finite: trace.non_finite,
            platform: platform.into(),
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

/// A rolling hash over a trajectory.
#[derive(Clone)]
pub struct TraceDigest {
    h: Sha256,
    pub precision: Precision,
    samples: u64,
    values: u64,
    non_finite: u64,
}

impl TraceDigest {
    pub fn new(precision: Precision) -> Self {
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
        TraceDigest {
            h,
            precision,
            samples: 0,
            values: 0,
            non_finite: 0,
        }
    }

    /// Hash one channel's values at one step. Steps must be fed in the same order in both
    /// runs; the step index and channel name are hashed too, so a reordering is a mismatch
    /// rather than a silent pass.
    pub fn step(&mut self, step: u64, channel: &str, values: &[f64]) {
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
    WithinTolerance {
        max_abs: f64,
        max_rel: f64,
        at_step: u64,
        channel: String,
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
                max_rel,
                at_step,
                channel,
            } => write!(
                f,
                "within tolerance: worst |Δ| {max_abs:.3e} (rel {max_rel:.3e}) at step {at_step} on {channel}"
            ),
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
    if a.samples.len() != b.samples.len() {
        return Verdict::Incomparable {
            reason: format!(
                "different sample counts: {} vs {}",
                a.samples.len(),
                b.samples.len()
            ),
        };
    }

    let mut any_bit_difference = false;
    let mut max_abs = 0.0f64;
    let mut max_rel = 0.0f64;
    let mut worst_step = 0u64;
    let mut worst_channel = String::new();

    for (sa, sb) in a.samples.iter().zip(&b.samples) {
        if sa.step != sb.step || sa.channel != sb.channel {
            return Verdict::Incomparable {
                reason: format!(
                    "sample order differs: step {} {} vs step {} {}",
                    sa.step, sa.channel, sb.step, sb.channel
                ),
            };
        }
        if sa.values.len() != sb.values.len() {
            return Verdict::Incomparable {
                reason: format!(
                    "channel {} has {} values in one run and {} in the other",
                    sa.channel,
                    sa.values.len(),
                    sb.values.len()
                ),
            };
        }
        for (i, (&x, &y)) in sa.values.iter().zip(&sb.values).enumerate() {
            if !x.is_finite() || !y.is_finite() {
                return Verdict::NonFinite {
                    step: sa.step,
                    channel: sa.channel.clone(),
                    index: i,
                    which: if !x.is_finite() { "A" } else { "B" },
                };
            }
            if x.to_bits() != y.to_bits() {
                any_bit_difference = true;
            }
            let abs = (x - y).abs();
            if abs == 0.0 {
                continue;
            }
            let denom = x.abs().max(y.abs());
            let rel = if denom > 0.0 { abs / denom } else { 0.0 };
            if abs > tol.abs && rel > tol.rel {
                return Verdict::Diverged {
                    step: sa.step,
                    channel: sa.channel.clone(),
                    index: i,
                    a: x,
                    b: y,
                    abs,
                    rel,
                };
            }
            if abs > max_abs {
                max_abs = abs;
                max_rel = rel;
                worst_step = sa.step;
                worst_channel = sa.channel.clone();
            }
        }
    }

    if !any_bit_difference {
        Verdict::BitExact
    } else {
        Verdict::WithinTolerance {
            max_abs,
            max_rel,
            at_step: worst_step,
            channel: worst_channel,
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
