//! Where two runs diverged, how the difference behaved, and what still lined up.
//!
//! [`compare`](crate::compare) answers *whether* a run reproduced and returns at the first
//! value that exceeds tolerance — the right shape for a CI gate, and the wrong shape for a
//! person holding two files at midnight. This module does the other job: one full walk that
//! reports the **onset** (where the bits first parted, which is the causal step), the
//! **crossing** (where the difference first exceeded a threshold, which is what a flag says),
//! the **shape** of the difference over time, and the **extent** across channels.
//!
//! Three cautions are built into the types, because each one is a way a confident-looking
//! statistic can be wrong:
//!
//! * Shape and ranking are judged on |Δ| against the **channel's own scale**, never on the
//!   pointwise relative difference. Absolute growth alone reports sensitivity where there is
//!   none (a robot moving forward at 1.7 m/s has a growing |Δ| at constant relative error);
//!   pointwise relative error reports it where the signal merely crosses zero, since the
//!   denominator vanishes there. The channel's scale is the denominator neither can distort.
//! * A shape is refused outright when too few steps follow the onset. A slope fitted to a
//!   handful of noisy points is not a finding, and [`Shape::TooShort`] says so instead.
//! * A window is summarised by its **envelope**, not its median. Physical channels are
//!   intermittent — a leg's actuation power is zero through every flight phase, a contact
//!   force is zero between touchdowns — so a median reads zero straight through a live
//!   divergence, and a persistent difference came out classified as a transient.
//! * Channels are **not independent**. One perturbation to a contact force shows up on the
//!   base pose, the leg, the foot, the joints and the contacts. The ranked list is a ranking,
//!   never a count of five separate failures.

use std::collections::{BTreeMap, BTreeSet};

use crate::{compare, Tolerance, Trace, Verdict};

/// Steps after the onset below which no shape is claimed.
const MIN_SHAPE_STEPS: usize = 20;

/// Non-zero samples a shape window must carry before it is trusted to represent the signal.
const MIN_NONZERO: usize = 3;

/// How much a relative difference must grow across the window to be called growing, and shrink
/// to be called fading. Between the two it is an offset that settled.
const GROWTH_FACTOR: f64 = 4.0;

/// How one channel's difference behaved from its onset to the end of the run.
#[derive(Clone, Debug, PartialEq)]
pub enum Shape {
    /// Not enough of the run followed the onset to say anything.
    TooShort { steps: usize },
    /// The difference is there and stays about the size it started.
    Settled { ratio: f64 },
    /// The difference grows: sensitivity, not a fixed parameter offset.
    Growing {
        ratio: f64,
        /// Steps for the relative difference to multiply by e, over the fitted window.
        e_folding_steps: f64,
    },
    /// The difference shrinks back toward nothing: a transient, one step's worth of solver.
    Fading { ratio: f64 },
}

impl std::fmt::Display for Shape {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Shape::TooShort { steps } => write!(
                f,
                "shape not characterised: only {steps} step(s) follow the onset, too few to tell \
                 a growing difference from a settled one"
            ),
            Shape::Settled { ratio } => write!(
                f,
                "settled: the difference against the channel's scale ends {ratio:.2}x where it \
                 started — an offset, so look for a parameter, asset or config that differs"
            ),
            Shape::Growing {
                ratio,
                e_folding_steps,
            } => write!(
                f,
                "growing: the difference against the channel's scale ends {ratio:.3e}x where it \
                 started, e-folding about every {e_folding_steps:.0} steps — sensitivity, \
                 so no tolerance makes this reproducible"
            ),
            Shape::Fading { ratio } => write!(
                f,
                "fading: the difference against the channel's scale ends {ratio:.2e}x where it \
                 started — a transient, so read the onset step itself"
            ),
        }
    }
}

/// One channel's divergence, from the first differing bit to the end of the run.
#[derive(Clone, Debug, PartialEq)]
pub struct ChannelDivergence {
    pub channel: String,
    /// First step on this channel where any bit differs.
    pub onset_step: u64,
    pub onset_abs: f64,
    pub onset_rel: f64,
    /// Largest POINTWISE relative difference on this channel, and where.
    ///
    /// Informative, but never the ranking key: a quantity passing through zero has a
    /// meaningless pointwise relative error, and ranking on it promotes a signal's own zero
    /// crossings above a real difference elsewhere. See [`ChannelDivergence::worst_scaled`].
    pub worst_rel: f64,
    pub worst_rel_step: u64,
    pub worst_abs: f64,
    /// The largest value of |Δ| divided by the channel's own scale — the largest magnitude
    /// either run reaches on it. This is what the list is ranked by, because it is a relative
    /// measure that a zero crossing cannot inflate.
    pub worst_scaled: f64,
    pub worst_scaled_step: u64,
    /// The channel's scale: `max(|value|)` over both runs. `0` when the channel is all zeros.
    pub scale: f64,
    /// The component index carrying `worst_rel`, for naming it in the payload's own terms.
    pub worst_index: usize,
    pub a_at_worst: f64,
    pub b_at_worst: f64,
    /// Whether this channel ever exceeded the caller's tolerance, and first where.
    pub first_crossing_step: Option<u64>,
    pub shape: Shape,
}

/// A difference in what the two runs recorded at all, as opposed to what they recorded.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Structural {
    pub a_last_step: u64,
    pub b_last_step: u64,
    pub only_in_a: Vec<String>,
    pub only_in_b: Vec<String>,
    /// `(channel, first step present on one side only, how many such steps, "A" or "B")`.
    pub gaps: Vec<(String, u64, usize, &'static str)>,
    /// Samples present on both sides, which is what any verdict below covers.
    pub shared_samples: usize,
    /// Samples excluded because one side did not have them.
    pub excluded_samples: usize,
}

/// The full answer: the headline verdict, plus where and how.
#[derive(Clone, Debug)]
pub struct Profile {
    /// The same verdict [`compare`] gives, computed over the samples both runs share.
    pub verdict: Verdict,
    /// `(channel, step)` where the bits first parted anywhere. The causal step.
    pub onset: Option<(String, u64)>,
    /// Step where some value first exceeded the tolerance. Moves when the flag moves.
    pub crossing: Option<u64>,
    /// Every channel that ever differs, ranked by |Δ| against the channel's own scale.
    pub channels: Vec<ChannelDivergence>,
    /// Present when the two runs did not record the same things. When this is `Some`, the
    /// verdict covers only the intersection and says nothing about the rest.
    pub structural: Option<Structural>,
}

impl Profile {
    /// The channel whose difference is largest against its own scale — the one to open first.
    pub fn dominant(&self) -> Option<&ChannelDivergence> {
        self.channels.first()
    }
}

type Keyed<'a> = BTreeMap<(&'a str, u64), Vec<&'a Vec<f64>>>;

fn key_by(t: &Trace) -> Keyed<'_> {
    let mut m: Keyed<'_> = BTreeMap::new();
    for s in &t.samples {
        m.entry((s.channel.as_str(), s.step))
            .or_default()
            .push(&s.values);
    }
    m
}

/// Profile two traces: the diagnostic pass, aligned on `(channel, step)`.
///
/// Alignment by key rather than by position is what turns the old dead end — "different sample
/// counts: 4409 vs 4396", two integers and nowhere to go — into a location. A run that stopped
/// early, a run with an added debug channel, and a run whose contact threshold moved all
/// produced that identical refusal, and all three want different next actions.
pub fn profile(a: &Trace, b: &Trace, tol: Tolerance) -> Profile {
    let ka = key_by(a);
    let kb = key_by(b);

    let chans_a: BTreeSet<&str> = ka.keys().map(|(c, _)| *c).collect();
    let chans_b: BTreeSet<&str> = kb.keys().map(|(c, _)| *c).collect();

    let mut structural = Structural {
        a_last_step: ka.keys().map(|(_, s)| *s).max().unwrap_or(0),
        b_last_step: kb.keys().map(|(_, s)| *s).max().unwrap_or(0),
        only_in_a: chans_a
            .difference(&chans_b)
            .map(|s| s.to_string())
            .collect(),
        only_in_b: chans_b
            .difference(&chans_a)
            .map(|s| s.to_string())
            .collect(),
        ..Default::default()
    };

    // Presence gaps, per channel, on the channels both runs have.
    let mut gaps: BTreeMap<(&str, &'static str), (u64, usize)> = BTreeMap::new();
    for (k, va) in &ka {
        match kb.get(k) {
            None => {
                let e = gaps.entry((k.0, "B")).or_insert((k.1, 0));
                e.0 = e.0.min(k.1);
                e.1 += 1;
            }
            Some(vb) if vb.len() != va.len() => {
                let e = gaps.entry((k.0, "count")).or_insert((k.1, 0));
                e.0 = e.0.min(k.1);
                e.1 += 1;
            }
            Some(_) => {}
        }
    }
    for k in kb.keys() {
        if !ka.contains_key(k) {
            let e = gaps.entry((k.0, "A")).or_insert((k.1, 0));
            e.0 = e.0.min(k.1);
            e.1 += 1;
        }
    }
    structural.gaps = gaps
        .into_iter()
        .map(|((ch, side), (first, n))| (ch.to_string(), first, n, side))
        .collect();

    // Walk the intersection, accumulating per-channel history.
    struct Acc {
        onset: Option<(u64, f64, f64)>,
        worst_rel: f64,
        worst_rel_step: u64,
        worst_abs: f64,
        worst_index: usize,
        a_at_worst: f64,
        b_at_worst: f64,
        first_crossing: Option<u64>,
        /// (step, |Δ|) — turned into a scaled series once the channel's scale is known.
        series: Vec<(u64, f64)>,
        scale: f64,
    }
    let mut acc: BTreeMap<&str, Acc> = BTreeMap::new();
    let mut shared_a = Trace::default();
    let mut shared_b = Trace::default();
    let mut excluded = 0usize;
    // Walk A in FILE ORDER, not in key order. The shared traces are handed to compare(), whose
    // answer is "the first crossing encountered", so rebuilding them channel-major silently
    // changed the headline: it named a crossing at step 434 while the earliest crossing in the
    // run was at 392, and the two lines of the same report disagreed.
    let mut seen: BTreeMap<(&str, u64), usize> = BTreeMap::new();

    for sa in &a.samples {
        let k = (sa.channel.as_str(), sa.step);
        let nth = seen.entry(k).or_insert(0);
        let i = *nth;
        *nth += 1;
        let Some(vb) = kb.get(&k).and_then(|v| v.get(i)) else {
            excluded += 1;
            continue;
        };
        {
            let xs = &sa.values;
            let ys = *vb;
            // Feed the shared pair to the ordinary comparator, so the headline verdict comes
            // from one implementation rather than two that could drift apart.
            shared_a.push(k.1, k.0, xs.clone());
            shared_b.push(k.1, k.0, ys.clone());
            if xs.len() != ys.len() {
                excluded += 1;
                continue;
            }

            let mut step_abs = 0.0f64;
            let mut differed = false;
            let e = acc.entry(k.0).or_insert(Acc {
                onset: None,
                worst_rel: 0.0,
                worst_rel_step: 0,
                worst_abs: 0.0,
                worst_index: 0,
                a_at_worst: 0.0,
                b_at_worst: 0.0,
                first_crossing: None,
                series: Vec::new(),
                scale: 0.0,
            });
            for (i, (&x, &y)) in xs.iter().zip(ys.iter()).enumerate() {
                if !x.is_finite() || !y.is_finite() {
                    continue;
                }
                // The channel's own scale, from both runs, whether or not this value differs:
                // it is what a difference on this channel should be judged against.
                e.scale = e.scale.max(x.abs()).max(y.abs());
                if x.to_bits() != y.to_bits() {
                    differed = true;
                }
                let abs = (x - y).abs();
                if abs == 0.0 {
                    continue;
                }
                let denom = x.abs().max(y.abs());
                let rel = if denom > 0.0 { abs / denom } else { 0.0 };
                step_abs = step_abs.max(abs);
                if e.onset.is_none() {
                    e.onset = Some((k.1, abs, rel));
                }
                if rel > e.worst_rel {
                    e.worst_rel = rel;
                    e.worst_rel_step = k.1;
                    e.worst_abs = abs;
                    e.worst_index = i;
                    e.a_at_worst = x;
                    e.b_at_worst = y;
                }
                if abs > tol.abs && rel > tol.rel && e.first_crossing.is_none() {
                    e.first_crossing = Some(k.1);
                }
            }
            if differed && e.onset.is_none() {
                // Bits differ but the subtraction is zero (+0.0 against -0.0).
                e.onset = Some((k.1, 0.0, 0.0));
            }
            if e.onset.is_some() {
                e.series.push((k.1, step_abs));
            }
        }
    }
    structural.shared_samples = shared_a.samples.len();
    // Both sides. `excluded` counted only samples A had and B lacked; a sample B recorded and A
    // did not is equally outside the verdict, and a coverage line that ignores it overstates
    // what the comparison covered.
    let _ = excluded;
    structural.excluded_samples =
        (a.samples.len() - shared_a.samples.len()) + (b.samples.len() - shared_b.samples.len());

    let verdict = compare(&shared_a, &shared_b, tol);

    let mut channels: Vec<ChannelDivergence> = acc
        .into_iter()
        .filter_map(|(ch, e)| {
            let (onset_step, onset_abs, onset_rel) = e.onset?;
            // |Δ| against the channel's own scale. Where the scale is zero the channel is all
            // zeros, and any difference on it is total.
            let scaled = |d: f64| {
                if e.scale > 0.0 {
                    d / e.scale
                } else if d > 0.0 {
                    1.0
                } else {
                    0.0
                }
            };
            let (worst_scaled, worst_scaled_step) =
                e.series.iter().map(|(st, d)| (scaled(*d), *st)).fold(
                    (0.0f64, onset_step),
                    |acc, x| if x.0 > acc.0 { x } else { acc },
                );
            Some(ChannelDivergence {
                channel: ch.to_string(),
                onset_step,
                onset_abs,
                onset_rel,
                worst_rel: e.worst_rel,
                worst_rel_step: e.worst_rel_step,
                worst_abs: e.worst_abs,
                worst_scaled,
                worst_scaled_step,
                scale: e.scale,
                worst_index: e.worst_index,
                a_at_worst: e.a_at_worst,
                b_at_worst: e.b_at_worst,
                first_crossing_step: e.first_crossing,
                // Shape is judged on the SCALED series for the same reason the ranking is: a
                // signal crossing zero makes the pointwise relative difference spike, and a
                // shape fitted to those spikes describes the zero crossings, not the run.
                shape: shape_of(
                    &e.series
                        .iter()
                        .map(|(st, d)| (*st, scaled(*d)))
                        .collect::<Vec<_>>(),
                ),
            })
        })
        .collect();
    channels.sort_by(|x, y| {
        y.worst_scaled
            .partial_cmp(&x.worst_scaled)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| x.onset_step.cmp(&y.onset_step))
    });

    let onset = channels
        .iter()
        .min_by_key(|c| c.onset_step)
        .map(|c| (c.channel.clone(), c.onset_step));
    let crossing = channels.iter().filter_map(|c| c.first_crossing_step).min();

    let clean = structural.only_in_a.is_empty()
        && structural.only_in_b.is_empty()
        && structural.gaps.is_empty()
        && excluded == 0;

    Profile {
        verdict,
        onset,
        crossing,
        channels,
        structural: if clean { None } else { Some(structural) },
    }
}

/// Classify a `(step, relative difference)` series from its onset.
///
/// Medians of the first and last tenth rather than endpoint values: a single noisy last step
/// would otherwise decide the classification for the whole run.
fn shape_of(series: &[(u64, f64)]) -> Shape {
    let live: Vec<f64> = series.iter().map(|(_, r)| *r).collect();
    if live.len() < MIN_SHAPE_STEPS {
        return Shape::TooShort { steps: live.len() };
    }
    // The ENVELOPE of each window, over a window WIDE ENOUGH to contain the signal. Physical
    // channels are intermittent — a leg's actuation power is zero through every flight phase, a
    // contact force is zero between touchdowns — and both halves of that matter:
    //
    //   * A median reads 0 through a live divergence, so the envelope is used instead. That
    //     alone was not enough.
    //   * A fixed 10 % window can land ENTIRELY inside one gap. Measured on a real 600-step
    //     demo pair: the last 10 % held max 0 while the last 20 % held 9.4e-6, above the head's
    //     1.9e-6 — a GROWING difference reported as "fading: a transient".
    //
    // So each window grows until it holds a few non-zero samples. If it cannot, the difference
    // really has stopped, and only then does "fading" mean what the word says.
    let head = end_envelope(&live, false);
    let tail = end_envelope(&live, true);
    if head <= 0.0 {
        // Started at an exact-zero difference (signed zero); nothing to take a ratio against.
        return if tail > 0.0 {
            Shape::Growing {
                ratio: f64::INFINITY,
                e_folding_steps: f64::NAN,
            }
        } else {
            Shape::Settled { ratio: 1.0 }
        };
    }
    let ratio = tail / head;
    let span = (series.last().unwrap().0)
        .saturating_sub(series[0].0)
        .max(1) as f64;
    if ratio >= GROWTH_FACTOR {
        Shape::Growing {
            ratio,
            e_folding_steps: span / ratio.ln(),
        }
    } else if ratio <= 1.0 / GROWTH_FACTOR {
        Shape::Fading { ratio }
    } else {
        Shape::Settled { ratio }
    }
}

/// The envelope of the window at one end of the series, widened until it carries signal.
fn end_envelope(v: &[f64], from_end: bool) -> f64 {
    let mut n = (v.len() / 10).max(3);
    let half = (v.len() / 2).max(n);
    loop {
        let w = if from_end { &v[v.len() - n..] } else { &v[..n] };
        if w.iter().filter(|x| **x > 0.0).count() >= MIN_NONZERO || n >= half {
            return quantile(w, 0.9);
        }
        n = (n * 2).min(half);
    }
}

fn quantile(v: &[f64], q: f64) -> f64 {
    let mut s: Vec<f64> = v.to_vec();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    if s.is_empty() {
        return 0.0;
    }
    let i = ((s.len() - 1) as f64 * q).round() as usize;
    s[i.min(s.len() - 1)]
}

// ---------------------------------------------------------------------------
// What the spec says changed
// ---------------------------------------------------------------------------

/// One declared field that differs between two runs' specs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FieldDiff {
    pub field: String,
    pub a: String,
    pub b: String,
    /// What the reader should do about it, in one clause.
    pub means: &'static str,
}

/// Name the declared fields that differ, instead of printing two digest prefixes.
///
/// A spec digest mismatch is reported today as `incomparable`, and the reader's next action is
/// to open both files by hand — although both `RunSpec`s were already parsed. Naming the field
/// turns that into: rerun B with A's seed, put gravity back, note that the solver changed.
pub fn spec_differences(a: &crate::RunSpec, b: &crate::RunSpec) -> Vec<FieldDiff> {
    let mut out = Vec::new();
    let mut push = |field: &str, x: String, y: String, means: &'static str| {
        if x != y {
            out.push(FieldDiff {
                field: field.into(),
                a: x,
                b: y,
                means,
            });
        }
    };
    push(
        "scenario",
        a.scenario.clone(),
        b.scenario.clone(),
        "a different experiment — there is no reproduction question to ask",
    );
    push(
        "seed",
        a.seed.to_string(),
        b.seed.to_string(),
        "variation by design; you asked for a different sample",
    );
    push(
        "dt_ns",
        a.dt_ns.to_string(),
        b.dt_ns.to_string(),
        "different timestep, so step N is a different instant in each file and comparing by step index is meaningless",
    );
    push(
        "steps",
        a.steps.to_string(),
        b.steps.to_string(),
        "one run is longer; the shared prefix is still comparable",
    );
    push(
        "integrator",
        a.integrator.clone(),
        b.integrator.clone(),
        "the numerical method changed; the question becomes whether the difference stays bounded",
    );
    push(
        "solver",
        a.solver.clone(),
        b.solver.clone(),
        "the numerical method changed; the question becomes whether the difference stays bounded",
    );
    push(
        "build",
        a.build.clone(),
        b.build.clone(),
        "the toolchain moved, not the experiment — a version bump alone makes an archive incomparable",
    );
    pairs_diff(&a.assets, &b.assets, "asset", "a mesh, URDF or policy changed; that explains forces that otherwise read as nondeterminism", &mut out);
    pairs_diff(
        &a.config,
        &b.config,
        "config",
        "a declared parameter changed — the most common real cause of \"it stopped reproducing\"",
        &mut out,
    );
    out
}

fn pairs_diff(
    a: &[(String, String)],
    b: &[(String, String)],
    prefix: &str,
    means: &'static str,
    out: &mut Vec<FieldDiff>,
) {
    let am: BTreeMap<&str, &str> = a.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let bm: BTreeMap<&str, &str> = b.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    for (k, va) in &am {
        match bm.get(k) {
            // Values are compared as the strings they were declared as, never normalised
            // numerically: "-9.80665" and "-9.8066500" are reported as different because they
            // were written differently, and asserting they are the same would be a judgement
            // the receipt did not make.
            Some(vb) if vb != va => out.push(FieldDiff {
                field: format!("{prefix}.{k}"),
                a: (*va).to_string(),
                b: (*vb).to_string(),
                means,
            }),
            None => out.push(FieldDiff {
                field: format!("{prefix}.{k}"),
                a: (*va).to_string(),
                b: "(absent)".into(),
                means,
            }),
            _ => {}
        }
    }
    for (k, vb) in &bm {
        if !am.contains_key(k) {
            out.push(FieldDiff {
                field: format!("{prefix}.{k}"),
                a: "(absent)".into(),
                b: (*vb).to_string(),
                means,
            });
        }
    }
}

/// How many fields the spec actually declares.
///
/// Reported alongside a field comparison so that "the declared fields agree" can never be read
/// as "the runs are identical". A spec declaring three keys is a far weaker comparability claim
/// than one declaring thirty, and collapsing that distinction is how an empty statement
/// acquires authority.
pub fn declared_fields(s: &crate::RunSpec) -> usize {
    // scenario, seed, dt_ns, steps, integrator, solver, build are always present as fields;
    // only the non-empty ones are worth counting as declarations.
    let base = [
        !s.scenario.is_empty(),
        s.seed != 0,
        s.dt_ns != 0,
        s.steps != 0,
        !s.integrator.is_empty(),
        !s.solver.is_empty(),
        !s.build.is_empty(),
    ]
    .iter()
    .filter(|x| **x)
    .count();
    base + s.assets.len() + s.config.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trace_of(n: u64, f: impl Fn(u64) -> f64) -> Trace {
        let mut t = Trace::default();
        for i in 0..n {
            t.push(i, "/x", vec![f(i)]);
        }
        t
    }

    #[test]
    fn a_constant_offset_is_settled_not_growing() {
        let a = trace_of(400, |i| 1.0 + i as f64);
        let b = trace_of(400, |i| 1.0 + i as f64 + 1e-6);
        let p = profile(&a, &b, Tolerance::default());
        let d = p.dominant().expect("a channel diverged");
        // The VALUE grows all run; the relative difference does not. Judging on absolute
        // difference alone would call this growing, which is the false alarm the shape exists
        // to avoid.
        assert!(
            matches!(d.shape, Shape::Settled { .. } | Shape::Fading { .. }),
            "a constant absolute offset on a growing quantity read as {:?}",
            d.shape
        );
    }

    #[test]
    fn an_exponential_difference_is_growing() {
        let a = trace_of(400, |_| 1.0);
        let b = trace_of(400, |i| 1.0 + 1e-12 * 1.05f64.powi(i as i32));
        let p = profile(&a, &b, Tolerance::default());
        let d = p.dominant().expect("a channel diverged");
        match d.shape {
            Shape::Growing {
                e_folding_steps, ..
            } => assert!(e_folding_steps > 0.0 && e_folding_steps < 400.0),
            ref other => panic!("an exponentially growing difference read as {other:?}"),
        }
    }

    #[test]
    fn a_short_run_refuses_to_name_a_shape() {
        let a = trace_of(8, |_| 1.0);
        let b = trace_of(8, |i| 1.0 + 1e-9 * i as f64);
        let p = profile(&a, &b, Tolerance::default());
        let d = p.dominant().expect("a channel diverged");
        assert!(matches!(d.shape, Shape::TooShort { .. }), "{:?}", d.shape);
    }

    #[test]
    fn onset_precedes_the_tolerance_crossing() {
        // Differs from step 100 at 1e-12, crosses a 1e-6 tolerance only at step 300.
        let a = trace_of(400, |_| 1.0);
        let b = trace_of(400, |i| match i {
            0..=99 => 1.0,
            100..=299 => 1.0 + 1e-12,
            _ => 1.0 + 1e-3,
        });
        let tol = Tolerance {
            abs: 1e-6,
            rel: 1e-6,
        };
        let p = profile(&a, &b, tol);
        assert_eq!(p.onset.as_ref().map(|(_, s)| *s), Some(100));
        assert_eq!(p.crossing, Some(300));
    }

    #[test]
    fn a_short_run_is_localized_not_refused() {
        let a = trace_of(400, |_| 1.0);
        let b = trace_of(120, |_| 1.0);
        let p = profile(&a, &b, Tolerance::default());
        let s = p.structural.expect("a length difference is structural");
        assert_eq!(s.a_last_step, 399);
        assert_eq!(s.b_last_step, 119);
        assert_eq!(s.shared_samples, 120);
        assert!(s.excluded_samples > 0);
        // And the part they share still gets a verdict, which the old code could not give.
        assert_eq!(p.verdict, Verdict::BitExact);
    }

    #[test]
    fn an_added_channel_is_named_not_counted() {
        let mut a = Trace::default();
        let mut b = Trace::default();
        for i in 0..50 {
            a.push(i, "/x", vec![1.0]);
            b.push(i, "/x", vec![1.0]);
            b.push(i, "/debug/iters", vec![7.0]);
        }
        let p = profile(&a, &b, Tolerance::default());
        let s = p.structural.expect("an extra channel is structural");
        assert_eq!(s.only_in_b, vec!["/debug/iters".to_string()]);
        assert!(s.only_in_a.is_empty());
        assert_eq!(p.verdict, Verdict::BitExact);
    }

    #[test]
    fn the_headline_verdict_agrees_with_the_reported_crossing() {
        // Two channels, interleaved as a recorder writes them. The EARLIEST crossing is on
        // /zzz at step 50; /aaa does not cross until 150. Rebuilding the shared trace in key
        // order put /aaa first, so compare() returned step 150 while the profile reported a
        // crossing at 50 — the same report contradicting itself.
        let mut a = Trace::default();
        let mut b = Trace::default();
        for i in 0..300u64 {
            a.push(i, "/aaa", vec![1.0]);
            a.push(i, "/zzz", vec![1.0]);
            b.push(i, "/aaa", vec![if i >= 150 { 2.0 } else { 1.0 }]);
            b.push(i, "/zzz", vec![if i >= 50 { 2.0 } else { 1.0 }]);
        }
        let p = profile(&a, &b, Tolerance::default());
        assert_eq!(p.crossing, Some(50));
        match &p.verdict {
            Verdict::Diverged { step, channel, .. } => {
                assert_eq!(
                    *step, 50,
                    "the headline named a later crossing than the profile"
                );
                assert_eq!(channel, "/zzz");
            }
            other => panic!("expected a divergence, got {other:?}"),
        }
    }

    #[test]
    fn an_intermittent_channel_is_not_called_a_transient() {
        // A leg's actuation power: zero through every flight phase, non-zero in stance. The
        // difference persists to the last step, but more than half of any window is exactly
        // zero. Summarising a window by its median read 0 and classified a live, persistent
        // divergence as "fading: a transient".
        let mut a = Trace::default();
        let mut b = Trace::default();
        for i in 0..600u64 {
            let stance = (i % 10) < 3;
            let w = if stance { 5.0 } else { 0.0 };
            a.push(i, "/leg/power", vec![w]);
            b.push(i, "/leg/power", vec![if stance { w + 1e-3 } else { 0.0 }]);
        }
        let p = profile(&a, &b, Tolerance::default());
        let d = p.dominant().expect("the channel diverged");
        assert!(
            matches!(d.shape, Shape::Settled { .. }),
            "an intermittent but persistent difference read as {:?}",
            d.shape
        );
    }

    #[test]
    fn a_tail_window_landing_in_a_gap_does_not_read_as_fading() {
        // The case measured on a real 600-step demo pair: an intermittent channel whose
        // difference GROWS, where the last tenth of the series falls entirely inside a gap.
        //
        // The duty cycle has to be long relative to the window for this to bite — the first
        // version of this test used a period of 10 against a 60-sample window, so the window
        // always caught a burst, and the mutation that pins the fix passed it. Period 200 with
        // 60 steps of stance puts steps 540..599 wholly in a gap while the head window
        // 0..59 is wholly in a burst.
        let mut a = Trace::default();
        let mut b = Trace::default();
        let n = 600u64;
        for i in 0..n {
            let phase = i % 200;
            let stance = phase < 60;
            let base = if stance { 5.0 } else { 0.0 };
            let d = if stance {
                1e-6 * (1.0 + i as f64 / 50.0)
            } else {
                0.0
            };
            a.push(i, "/leg/power", vec![base]);
            b.push(i, "/leg/power", vec![base + d]);
        }
        let p = profile(&a, &b, Tolerance::default());
        let d = p.dominant().expect("the channel diverged");
        assert!(
            matches!(d.shape, Shape::Growing { .. }),
            "a growing difference whose tail window fell in a gap read as {:?}",
            d.shape
        );
    }

    #[test]
    fn a_zero_crossing_does_not_outrank_a_real_difference() {
        // /wave passes through zero every cycle and differs by a nanometre. At the crossing its
        // POINTWISE relative difference approaches 1, which is meaningless — the denominator is
        // the signal being near zero, not the difference being large. /load differs by a part
        // in a thousand of a quantity that never approaches zero, and is the real finding.
        let mut a = Trace::default();
        let mut b = Trace::default();
        for i in 0..400u64 {
            let t = i as f64 * 0.05;
            let w = t.sin();
            a.push(i, "/wave", vec![w]);
            b.push(i, "/wave", vec![w + 1e-9]);
            a.push(i, "/load", vec![10.0]);
            b.push(i, "/load", vec![10.0 + 1e-2]);
        }
        let p = profile(&a, &b, Tolerance::default());
        let top = p.dominant().expect("something diverged");
        assert_eq!(
            top.channel,
            "/load",
            "a zero crossing outranked a real difference: {:?}",
            p.channels
                .iter()
                .map(|c| (c.channel.as_str(), c.worst_rel, c.worst_scaled))
                .collect::<Vec<_>>()
        );
        // The pointwise relative difference on /wave really is enormous — which is exactly why
        // it must not be the ranking key, and why it is still reported.
        let wave = p.channels.iter().find(|c| c.channel == "/wave").unwrap();
        assert!(wave.worst_rel > 0.01, "rel was {}", wave.worst_rel);
        assert!(wave.worst_scaled < 1e-8, "scaled was {}", wave.worst_scaled);
    }

    #[test]
    fn spec_differences_name_the_field() {
        let a = crate::RunSpec::new("hop", 7)
            .config("gravity_z", "-9.80665")
            .build("ferroscope 0.1.11");
        let b = crate::RunSpec::new("hop", 7)
            .config("gravity_z", "-9.81")
            .build("ferroscope 0.1.12");
        let d = spec_differences(&a, &b);
        let fields: Vec<&str> = d.iter().map(|f| f.field.as_str()).collect();
        assert!(fields.contains(&"config.gravity_z"), "{fields:?}");
        assert!(fields.contains(&"build"), "{fields:?}");
        let g = d.iter().find(|f| f.field == "config.gravity_z").unwrap();
        assert_eq!(g.a, "-9.80665");
        assert_eq!(g.b, "-9.81");
    }

    #[test]
    fn identical_specs_have_no_differences() {
        let a = crate::RunSpec::new("hop", 7).config("k", "1");
        assert!(spec_differences(&a, &a.clone()).is_empty());
        assert_eq!(declared_fields(&a), 3); // scenario, seed, one config key
    }
}
