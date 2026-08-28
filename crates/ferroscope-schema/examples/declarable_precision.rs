//! **What precision can two real recordings actually declare — and which channel decides?**
//!
//! The receipt's trace digest quantizes by MASKING low mantissa bits, which is a *pointwise
//! relative* operation: each value is bucketed against its own magnitude. That is the right shape
//! for a quantity that stays away from zero and the wrong shape for one that crosses it, and this
//! measures the difference on real files rather than on a synthetic.
//!
//! ```sh
//! ferroscope demo a.mcap --steps 3000 --seed 7 --precision exact
//! ferroscope demo b.mcap --steps 3000 --seed 7 --drift 1500 --precision exact
//! cargo run --release --example declarable_precision -p ferroscope-schema -- a.mcap b.mcap
//! ```
//!
//! What it prints, per channel:
//!
//! - **bits** — measured, by bisecting the real [`TraceDigest`]: the smallest `drop_bits` at
//!   which this channel's values hash identically in both runs. Ground truth, not a model.
//! - **pred** — `52 + log2(Σ pointwise relative difference)`. Every sample must land in the SAME
//!   bucket, not merely be close, so what binds is not the worst difference but the expected
//!   number of boundary crossings — which is the sum, not the max. It lands within 0–2 bits on
//!   most channels and under-predicts by up to 7 where a channel crosses binades, so it is a
//!   lower bound with a margin, not a formula to set a receipt from.
//! - **scale**, **worst |Δ|**, **Σ|Δ|** — the channel's own magnitude, how far the two runs got
//!   at worst, and summed over every value. The last is what a DECLARED resolution must clear,
//!   because agreement is per sample and the boundary crossings accumulate.
//! - **agrees at**, **ratio** — the smallest declared resolution at which the two runs actually
//!   hash alike, found by bisecting on the decade, and how many times Σ|Δ| that is. Measured
//!   over six pairs the ratio ran from 1.0 to 109, which is why nothing here suggests a
//!   multiplier: one that is wrong a quarter of the time is worse than none.

use ferroscope_receipt::{Precision, TraceDigest};
use std::collections::BTreeMap;

fn trace(path: &str) -> ferroscope_receipt::Trace {
    let f = std::fs::File::open(path).unwrap_or_else(|e| panic!("cannot read {path}: {e}"));
    ferroscope_schema::trace_from_streaming(f)
        .unwrap_or_else(|| panic!("{path} is not a readable Ferroscope recording"))
        .1
}

/// Hash one channel's values at a given precision, using the real digest rather than a model of
/// it — the whole point is to measure what the receipt actually does.
fn hash_at(vals: &[f64], drop_bits: u8) -> String {
    let mut d = TraceDigest::new(Precision::Quantized { drop_bits });
    for (i, v) in vals.iter().enumerate() {
        d.step(i as u64, "/c", &[*v]);
    }
    d.finish()
}

/// The same, on a DECLARED absolute grid instead of the mask.
fn hash_on_grid(vals: &[f64], quantum: f64) -> String {
    let mut d = TraceDigest::with_resolutions(Precision::Exact, &[("/c".to_string(), quantum)]);
    for (i, v) in vals.iter().enumerate() {
        d.step(i as u64, "/c", &[*v]);
    }
    d.finish()
}

/// The smallest declared resolution at which two runs of a channel hash identically, found by
/// bisecting on the exponent — the answer a user actually wants, in the channel's own units.
///
/// Ternary rather than binary in spirit: agreement is not perfectly monotone in the quantum,
/// because a coarser grid can still land two values either side of a cell edge. The bisection
/// looks for the boundary of the region that agrees, and the caller is told it is a boundary
/// rather than a guarantee.
fn agreeing_resolution(xs: &[f64], ys: &[f64]) -> Option<f64> {
    let scale = xs.iter().chain(ys).fold(0.0f64, |m, v| m.max(v.abs()));
    if scale == 0.0 {
        return Some(0.0);
    }
    let mut lo = -300i32; // definitely too fine
    let mut hi = (scale.log10().ceil() as i32) + 2; // certainly coarse enough: one cell
    if hash_on_grid(xs, 10f64.powi(hi)) != hash_on_grid(ys, 10f64.powi(hi)) {
        return None;
    }
    while hi - lo > 1 {
        let mid = lo + (hi - lo) / 2;
        let q = 10f64.powi(mid);
        if hash_on_grid(xs, q) == hash_on_grid(ys, q) {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    Some(10f64.powi(hi))
}

struct Row {
    bits: u16,
    /// Measured: the smallest declared resolution at which the two runs agree.
    grid: Option<f64>,
    /// Σ|Δ| — every sample must land in the same cell, so the cell must clear the SUM.
    sum_abs: f64,
    predicted: i32,
    channel: String,
    scale: f64,
    worst_abs: f64,
    smallest: f64,
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() != 2 {
        eprintln!("usage: declarable_precision <a.mcap> <b.mcap>");
        std::process::exit(2);
    }
    let (ta, tb) = (trace(&args[0]), trace(&args[1]));

    // Pair by (channel, step) occurrence, the way the comparator does.
    let mut b_index: BTreeMap<(String, u64), Vec<&Vec<f64>>> = BTreeMap::new();
    for s in &tb.samples {
        b_index
            .entry((s.channel.clone(), s.step))
            .or_default()
            .push(&s.values);
    }
    let mut seen: BTreeMap<(String, u64), usize> = BTreeMap::new();
    let mut paired: BTreeMap<String, (Vec<f64>, Vec<f64>)> = BTreeMap::new();
    for s in &ta.samples {
        let key = (s.channel.clone(), s.step);
        let nth = seen.entry(key.clone()).or_insert(0);
        let i = *nth;
        *nth += 1;
        if let Some(other) = b_index.get(&key).and_then(|v| v.get(i))
            && other.len() == s.values.len()
        {
            let e = paired.entry(s.channel.clone()).or_default();
            e.0.extend(s.values.iter().copied());
            e.1.extend(other.iter().copied());
        }
    }

    let mut rows: Vec<Row> = Vec::new();
    for (channel, (xs, ys)) in &paired {
        let scale = xs.iter().chain(ys).fold(0.0f64, |m, v| m.max(v.abs()));
        let worst_abs = xs
            .iter()
            .zip(ys)
            .fold(0.0f64, |m, (x, y)| m.max((x - y).abs()));
        let smallest = xs
            .iter()
            .chain(ys)
            .filter(|v| **v != 0.0)
            .fold(f64::INFINITY, |m, v| m.min(v.abs()));
        let sum_pointwise: f64 = xs
            .iter()
            .zip(ys)
            .filter(|(x, y)| **x != 0.0 || **y != 0.0)
            .map(|(x, y)| (x - y).abs() / x.abs().max(y.abs()))
            .sum();
        let predicted = if sum_pointwise == 0.0 {
            0
        } else {
            (52.0 + sum_pointwise.log2()).ceil() as i32
        };
        let sum_abs: f64 = xs.iter().zip(ys).map(|(x, y)| (x - y).abs()).sum();
        let grid = agreeing_resolution(xs, ys);
        let bits = (0u8..=52)
            .find(|&b| hash_at(xs, b) == hash_at(ys, b))
            .map(u16::from)
            .unwrap_or(53);
        rows.push(Row {
            bits,
            grid,
            sum_abs,
            predicted,
            channel: channel.clone(),
            scale,
            worst_abs,
            smallest: if smallest.is_finite() { smallest } else { 0.0 },
        });
    }
    rows.sort_by_key(|r| std::cmp::Reverse(r.bits));

    println!(
        "{:>5} {:>5}  {:<28} {:>11} {:>11} {:>11} {:>11} {:>7}",
        "bits", "pred", "channel", "scale", "worst |Δ|", "Σ|Δ|", "agrees at", "ratio"
    );
    for r in &rows {
        println!(
            "{:>5} {:>5}  {:<28} {:>11.3e} {:>11.3e} {:>11.3e} {:>11} {:>7}",
            if r.bits > 52 {
                "never".to_string()
            } else {
                r.bits.to_string()
            },
            r.predicted,
            r.channel,
            r.scale,
            r.worst_abs,
            r.sum_abs,
            if r.sum_abs == 0.0 {
                "identical".to_string()
            } else {
                match r.grid {
                    Some(g) => format!("{g:.0e}"),
                    None => "never".to_string(),
                }
            },
            match (r.grid, r.sum_abs) {
                (Some(g), s) if s > 0.0 && g > 0.0 => format!("{:.1}", g / s),
                _ => "-".to_string(),
            }
        );
    }

    let binding = rows.iter().max_by_key(|r| r.bits);
    if let Some(b) = binding {
        println!();
        println!(
            "the whole trace can declare no better than drop_bits {} — forced by {}",
            b.bits, b.channel
        );
        if b.scale > 0.0 && b.smallest > 0.0 {
            println!(
                "  that channel's scale is {:.3e} and the two runs got {:.3e} apart, which is \
                 {:.2e} of its scale —",
                b.scale,
                b.worst_abs,
                b.worst_abs / b.scale
            );
            println!(
                "  but it visits {:.3e}, and a MASK is applied to each value's own magnitude.",
                b.smallest
            );
        }
        println!();
        println!("The digest quantizes pointwise-relative, so what binds it is whichever channel");
        println!("passes closest to zero — not the channel's scale, and not the size of the");
        println!("disagreement. The COMPARATOR already knows better: it carries an absolute AND a");
        println!("relative tolerance, and it ranks by |Δ| against each channel's own scale. The");
        println!("digest carries only the relative half.");
        println!();
        println!("Fixing that is not a one-line change, and the reason is worth stating: the");
        println!("digest is computed while the recording is being WRITTEN, so it cannot normalise");
        println!("by a scale it has not seen yet. A correct fix means the scale is DECLARED —");
        println!("\"this channel is metres, resolution 1 um\" — which is a physical statement");
        println!("rather than a floating-point one, and a change to what a receipt contains.");
    }
}
