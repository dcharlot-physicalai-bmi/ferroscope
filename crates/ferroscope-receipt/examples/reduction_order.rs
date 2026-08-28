//! **What a reordered reduction costs a receipt.**
//!
//! Every cross-platform number this project has measured so far is CPU `f64` running the same
//! operations in the same order, and there it found bit-exactness where IEEE-754 guarantees it
//! and a two-ULP libm gap where it does not. A fabric that reduces in PARALLEL breaks the
//! guarantee for a different reason: `+` on floats is not associative, so summing the same
//! values in a different order is a different computation, and a GPU picks its order from the
//! shape of the hardware rather than from your program.
//!
//! That is the case a declared precision exists for, and it had never been measured. This is the
//! measurement, and it needs no GPU: the reorderings a GPU imposes — a block per workgroup,
//! combined at the end — can be performed exactly on a CPU. What a GPU adds beyond reordering is
//! a narrower type, and that is reported separately because it is a different effect of a
//! different size.
//!
//! ```sh
//! cargo run --release --example reduction_order -p ferroscope-receipt
//! ```

use ferroscope_receipt::{Precision, TraceDigest};

/// SplitMix64: a deterministic generator, so this experiment is a measurement rather than an
/// anecdote. No dependency, which is the house rule.
struct Rng(u64);

impl Rng {
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    /// Uniform in [0, 1).
    fn unit(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 * (1.0 / (1u64 << 53) as f64)
    }
}

/// The shapes of thing a solver actually adds up.
#[derive(Clone, Copy)]
enum Shape {
    /// Comparable magnitudes, all positive: a mass sum, a bulk energy total.
    Uniform,
    /// Magnitudes spread over ten decades: contact forces, where a few dominate and most are
    /// nearly nothing. This is the case where order matters most, because a small addend added
    /// to a large running total disappears entirely.
    Mixed,
    /// Signed and nearly cancelling: a net force at equilibrium, where the answer is small and
    /// the terms are not.
    Cancelling,
}

impl Shape {
    fn name(self) -> &'static str {
        match self {
            Shape::Uniform => "uniform",
            Shape::Mixed => "mixed magnitudes",
            Shape::Cancelling => "near-cancelling",
        }
    }
    fn sample(self, rng: &mut Rng) -> f64 {
        match self {
            Shape::Uniform => rng.unit(),
            // 10^(-5..5), so ten decades.
            Shape::Mixed => 10f64.powf(rng.unit() * 10.0 - 5.0),
            Shape::Cancelling => {
                let m = rng.unit();
                if rng.next_u64() & 1 == 0 { m } else { -m }
            }
        }
    }
}

/// Sum in file order: what a single-threaded loop does.
fn forward(v: &[f64]) -> f64 {
    v.iter().sum()
}

/// The same values, arriving the other way. Nothing about the physics changed.
fn reverse(v: &[f64]) -> f64 {
    v.iter().rev().sum()
}

/// Pairwise (tree) summation: what a careful numerics library does, and the most accurate of
/// these without compensation. It is here as the reference, not as a contender.
fn pairwise(v: &[f64]) -> f64 {
    if v.len() <= 8 {
        return v.iter().sum();
    }
    let mid = v.len() / 2;
    pairwise(&v[..mid]) + pairwise(&v[mid..])
}

/// A block per workgroup, combined at the end — the shape of every GPU reduction. `blocks` is
/// how many partial sums the fabric happens to produce, which depends on the hardware and not
/// on the program.
fn blocked(v: &[f64], blocks: usize) -> f64 {
    let n = v.len().div_ceil(blocks.max(1));
    let mut partials: Vec<f64> = Vec::new();
    let mut i = 0;
    while i < v.len() {
        let end = (i + n).min(v.len());
        partials.push(v[i..end].iter().sum());
        i = end;
    }
    partials.iter().sum()
}

/// Kahan compensated summation: the closest thing to the true value available here, used only to
/// say how far the others are from right rather than merely from each other.
fn kahan(v: &[f64]) -> f64 {
    let (mut sum, mut c) = (0.0f64, 0.0f64);
    for &x in v {
        let y = x - c;
        let t = sum + y;
        c = (t - sum) - y;
        sum = t;
    }
    sum
}

/// One value's distance from another, in units of the last place of the larger.
fn ulps_apart(a: f64, b: f64) -> f64 {
    let scale = a.abs().max(b.abs());
    if scale == 0.0 {
        return 0.0;
    }
    let ulp = ulp_of(scale);
    (a - b).abs() / ulp
}

fn ulp_of(x: f64) -> f64 {
    let bits = x.abs().to_bits();
    f64::from_bits(bits + 1) - x.abs()
}

/// The smallest `drop_bits` at which every one of these values hashes to the same digest.
///
/// This is deliberately the EMPIRICAL threshold rather than one derived from the spread, because
/// the digest masks low bits rather than rounding them: two values a hair apart but on opposite
/// sides of a mask boundary still differ, however many bits are dropped. The gap between what
/// the spread implies and what actually agrees is the cost of that, and it is reported.
fn agreeing_drop_bits(values: &[f64]) -> Option<u8> {
    (0u8..=52).find(|&b| {
        let hash = |v: f64| {
            let mut d = TraceDigest::new(Precision::Quantized { drop_bits: b });
            d.step(0, "/sum", &[v]);
            d.finish()
        };
        let first = hash(values[0]);
        values[1..].iter().all(|&v| hash(v) == first)
    })
}

/// What the spread alone says you need, if masking were rounding.
fn bits_implied(spread_ulps: f64) -> u32 {
    if spread_ulps <= 1.0 {
        0
    } else {
        spread_ulps.log2().ceil() as u32
    }
}

fn main() {
    println!("REORDERED REDUCTIONS, AND WHAT A RECEIPT MUST DECLARE");
    println!();
    println!("Floating-point addition is not associative, so summing the same values in a");
    println!("different order is a different computation. A GPU chooses its order from the shape");
    println!("of the hardware. Every ordering below is one a real fabric produces; the values are");
    println!("identical in all of them.");
    println!();

    for shape in [Shape::Uniform, Shape::Mixed, Shape::Cancelling] {
        println!("=== {} ===", shape.name());
        println!(
            "{:>10}  {:>12}  {:>12}  {:>10}  {:>9}  {:>8}  {:>7}  {:>7}",
            "terms", "spread |Δ|", "vs Kahan", "spread ULP", "ULP/√n", "|sum|", "implied", "agrees"
        );
        for n in [10usize, 100, 1_000, 10_000, 100_000, 1_000_000] {
            let mut rng = Rng(0x5EED_1234_ABCD_0001);
            let v: Vec<f64> = (0..n).map(|_| shape.sample(&mut rng)).collect();

            let mut sorted_asc = v.clone();
            sorted_asc.sort_by(|a, b| a.abs().partial_cmp(&b.abs()).unwrap());
            let mut sorted_desc = sorted_asc.clone();
            sorted_desc.reverse();

            // Every ordering a fabric might hand you. Block counts are workgroup counts.
            let sums = vec![
                forward(&v),
                reverse(&v),
                pairwise(&v),
                blocked(&v, 32),
                blocked(&v, 256),
                blocked(&v, 1024),
                forward(&sorted_asc),
                forward(&sorted_desc),
            ];
            let truth = kahan(&sorted_asc);

            let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
            for &s in &sums {
                lo = lo.min(s);
                hi = hi.max(s);
            }
            let spread = hi - lo;
            let worst_vs_truth = sums
                .iter()
                .map(|&s| (s - truth).abs())
                .fold(0.0f64, f64::max);
            let spread_ulps = ulps_apart(hi, lo);
            let implied = bits_implied(spread_ulps);
            let agrees = agreeing_drop_bits(&sums);

            // Rounding error in a sum of n terms accumulates like a random walk, so the
            // spread should grow like √n ULP. This column is the test of that: a constant
            // means the law holds and the number is a property of the arithmetic rather than
            // of this particular data.
            let per_root_n = spread_ulps / (n as f64).sqrt();
            println!(
                "{n:>10}  {spread:>12.3e}  {worst_vs_truth:>12.3e}  {spread_ulps:>10.1}  \
                 {per_root_n:>9.2}  {:>8.2e}  {implied:>7}  {:>7}",
                truth.abs(),
                match agrees {
                    Some(b) => format!("{b}"),
                    None => "never".into(),
                }
            );
        }
        println!();
    }

    println!("HOW TO READ IT");
    println!();
    println!("  spread |Δ|   how far apart the orderings are from EACH OTHER — the quantity a");
    println!("               receipt has to survive, because two machines pick different orders.");
    println!("  vs Kahan     how far the worst of them is from the compensated sum, i.e. from");
    println!("               right. Reordering does not merely disagree, it is also wrong.");
    println!("  implied      bits the spread alone says you must drop: ceil(log2(spread in ULP)).");
    println!("  agrees       bits actually needed for every ordering to hash identically.");
    println!();
    println!("`agrees` exceeding `implied` is not noise. The digest MASKS low bits rather than");
    println!("rounding them, so two values a hair apart on opposite sides of a mask boundary");
    println!("still differ however close they are. The margin is the price of masking, and it is");
    println!("why a receipt's declared precision must clear the spread rather than meet it.");
    println!();
    println!("NOT MEASURED HERE: a narrower type. WGSL has no f64 at all and Metal on Apple");
    println!("silicon exposes none, so a browser or Apple GPU does not reorder your f64 — it");
    println!("cannot hold it. That is a 29-bit cliff (52 - 23), not a reordering effect, and");
    println!("calling both 'GPU nondeterminism' hides which one you are paying for.");
}
