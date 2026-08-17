//! **The joules axis.**
//!
//! Every robotics viewer in the field plots position, velocity, torque, and latency. None of
//! them plots the quantity that decides whether a robot can do the job for a whole shift:
//! the energy the task cost. Ferroscope treats it as a first-class lane, and this crate is
//! the arithmetic behind it.
//!
//! The model is deliberately blunt:
//!
//! ```text
//! E_task = E_compute + E_actuation
//! ```
//!
//! and both terms come from *measured power integrated over the run*, never from a datasheet
//! TDP. The two ordinarily live in different worlds — the compute number comes from a power
//! rail or an on-die counter, the actuation number from motor bus telemetry — and the whole
//! point of putting them in one ledger is that their ratio is the design decision. On an AMR
//! doing autonomous navigation the motors are a *minority* of the draw; a viewer that only
//! shows joint torque hides that.
//!
//! ## Why there is a refusal
//!
//! Integrating power over time is trivial. Integrating power over time *honestly* is not:
//! a series sampled at 1 Hz through a 200 ms torque spike will quietly under-report, and a
//! series with a four-second hole in it will under-report by whatever happened in the hole.
//! So [`Ledger::quote`] returns a [`Quote`] carrying a [`Coverage`] verdict, and
//! [`Quote::quotable`] is false when the sampling cannot support the number. The number is
//! still there — it is just labelled.
//!
//! ```
//! use ferroscope_ledger::{Ledger, Rail};
//!
//! let mut led = Ledger::new();
//! // Two rails, sampled at 100 Hz for one second.
//! for i in 0..=100u64 {
//!     let t = i * 10_000_000; // ns
//!     led.sample(Rail::Compute, "soc", t, 8.0);
//!     led.sample(Rail::Actuation, "hip", t, 40.0);
//! }
//! let q = led.quote();
//! assert!(q.quotable);
//! assert!((q.total_j - 48.0).abs() < 1e-9);      // 48 W for 1 s
//! assert!((q.compute_fraction() - 8.0 / 48.0).abs() < 1e-12);
//! ```

#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::fmt;

/// Which side of `E_task = E_compute + E_actuation` a power source belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Rail {
    /// Processors, accelerators, memory, sensing electronics.
    Compute,
    /// Motors, actuators, pumps, valves — work done on the world.
    Actuation,
    /// Everything that is neither, and should not be silently folded into either:
    /// chassis electronics, comms radios, thermal management.
    Overhead,
}

impl Rail {
    pub fn as_str(&self) -> &'static str {
        match self {
            Rail::Compute => "compute",
            Rail::Actuation => "actuation",
            Rail::Overhead => "overhead",
        }
    }
}

impl fmt::Display for Rail {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One named power source: `(rail, name)`, e.g. `(Actuation, "hip_pitch")`.
#[derive(Clone, Debug, Default)]
struct Series {
    /// `(time_ns, watts)`, kept in the order sampled.
    points: Vec<(u64, f64)>,
}

impl Series {
    /// Trapezoidal integration. Stated rather than assumed: for power that is piecewise
    /// smooth between samples this is second-order accurate, and for power that steps
    /// between samples it splits the difference — which is why the gap statistics below
    /// travel with the number instead of being discarded.
    fn joules(&self) -> f64 {
        let mut e = 0.0;
        for w in self.points.windows(2) {
            let (t0, p0) = w[0];
            let (t1, p1) = w[1];
            if t1 <= t0 {
                continue;
            }
            let dt = (t1 - t0) as f64 * 1e-9;
            e += 0.5 * (p0 + p1) * dt;
        }
        e
    }
    fn span(&self) -> Option<(u64, u64)> {
        match (self.points.first(), self.points.last()) {
            (Some(a), Some(b)) => Some((a.0, b.0)),
            _ => None,
        }
    }
    fn largest_gap_ns(&self) -> u64 {
        self.points
            .windows(2)
            .map(|w| w[1].0.saturating_sub(w[0].0))
            .max()
            .unwrap_or(0)
    }
    fn median_interval_ns(&self) -> u64 {
        if self.points.len() < 2 {
            return 0;
        }
        let mut d: Vec<u64> = self
            .points
            .windows(2)
            .map(|w| w[1].0.saturating_sub(w[0].0))
            .collect();
        d.sort_unstable();
        d[d.len() / 2]
    }
    fn peak(&self) -> f64 {
        self.points.iter().fold(0.0f64, |m, p| m.max(p.1))
    }
}

/// Whether the sampling can support the energy number computed from it.
#[derive(Clone, Debug, PartialEq)]
pub enum Coverage {
    /// No gap exceeded the tolerance; the integral stands.
    Sound {
        median_interval_s: f64,
        samples: usize,
    },
    /// A gap large enough to hide a transient. The number is reported anyway, flagged.
    Gappy {
        largest_gap_s: f64,
        median_interval_s: f64,
        rail: Rail,
        source: String,
    },
    /// Fewer than two samples on some rail — there is no interval to integrate over.
    TooFewSamples {
        rail: Rail,
        source: String,
        n: usize,
    },
    /// Nothing was recorded at all.
    Empty,
}

impl fmt::Display for Coverage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Coverage::Sound {
                median_interval_s,
                samples,
            } => write!(
                f,
                "sound ({samples} samples, median interval {:.1} ms)",
                median_interval_s * 1e3
            ),
            Coverage::Gappy {
                largest_gap_s,
                median_interval_s,
                rail,
                source,
            } => write!(
                f,
                "DO NOT QUOTE: {rail}/{source} has a {:.0} ms gap against a {:.1} ms median \
                 interval, so a transient in that hole is invisible to the integral",
                largest_gap_s * 1e3,
                median_interval_s * 1e3
            ),
            Coverage::TooFewSamples { rail, source, n } => write!(
                f,
                "DO NOT QUOTE: {rail}/{source} has {n} sample(s); an integral needs at least two"
            ),
            Coverage::Empty => write!(f, "DO NOT QUOTE: nothing was sampled"),
        }
    }
}

/// The result of closing the books on a run.
#[derive(Clone, Debug)]
pub struct Quote {
    pub compute_j: f64,
    pub actuation_j: f64,
    pub overhead_j: f64,
    pub total_j: f64,
    pub duration_s: f64,
    /// Per-source joules, sorted, so the biggest consumer is one glance away.
    pub by_source: Vec<(Rail, String, f64)>,
    /// Highest instantaneous power seen on any rail, and where.
    pub peak_w: f64,
    pub peak_source: String,
    pub coverage: Coverage,
    /// `false` when [`Coverage`] says the sampling cannot support the number. The number is
    /// still populated — a flagged measurement beats a missing one, as long as it is flagged.
    pub quotable: bool,
}

impl Quote {
    pub fn mean_power_w(&self) -> f64 {
        if self.duration_s > 0.0 {
            self.total_j / self.duration_s
        } else {
            0.0
        }
    }
    /// The ratio that decides where optimization effort belongs.
    pub fn compute_fraction(&self) -> f64 {
        if self.total_j > 0.0 {
            self.compute_j / self.total_j
        } else {
            0.0
        }
    }
    /// Joules per *viable* task: the cost metric that counts failed attempts against you.
    /// `None` when nothing succeeded — the metric is undefined there, and reporting a large
    /// finite number instead of "no successes" is how a broken policy looks merely expensive.
    pub fn joules_per_viable_task(&self, attempts: u32, successes: u32) -> Option<f64> {
        let _ = attempts;
        if successes == 0 {
            None
        } else {
            Some(self.total_j / successes as f64)
        }
    }
}

/// Accumulates power samples and closes them into a [`Quote`].
#[derive(Clone, Debug, Default)]
pub struct Ledger {
    series: BTreeMap<(Rail, String), Series>,
    /// A gap larger than this multiple of the median sampling interval trips the refusal.
    pub gap_tolerance: f64,
}

impl Ledger {
    pub fn new() -> Self {
        Ledger {
            series: BTreeMap::new(),
            // Four times the nominal interval: enough slack for scheduler jitter, tight
            // enough to catch a dropped block of telemetry.
            gap_tolerance: 4.0,
        }
    }

    /// Record instantaneous power in watts at `time_ns`.
    pub fn sample(&mut self, rail: Rail, source: &str, time_ns: u64, watts: f64) {
        self.series
            .entry((rail, source.to_string()))
            .or_default()
            .points
            .push((time_ns, watts));
    }

    /// Record an energy amount directly, for sources that report joules rather than watts
    /// (an inverter's kWh counter, a battery gauge delta). Stored as a two-point series so
    /// it integrates to exactly `joules`.
    pub fn energy(&mut self, rail: Rail, source: &str, t0_ns: u64, t1_ns: u64, joules: f64) {
        if t1_ns <= t0_ns {
            return;
        }
        let w = joules / ((t1_ns - t0_ns) as f64 * 1e-9);
        let s = self.series.entry((rail, source.to_string())).or_default();
        s.points.push((t0_ns, w));
        s.points.push((t1_ns, w));
    }

    pub fn is_empty(&self) -> bool {
        self.series.is_empty()
    }

    /// Close the books.
    pub fn quote(&self) -> Quote {
        let mut q = Quote {
            compute_j: 0.0,
            actuation_j: 0.0,
            overhead_j: 0.0,
            total_j: 0.0,
            duration_s: 0.0,
            by_source: Vec::new(),
            peak_w: 0.0,
            peak_source: String::new(),
            coverage: Coverage::Empty,
            quotable: false,
        };
        if self.series.is_empty() {
            return q;
        }

        let mut t_lo = u64::MAX;
        let mut t_hi = 0u64;
        let mut worst: Option<Coverage> = None;
        let mut median_intervals: Vec<u64> = Vec::new();

        for ((rail, name), s) in &self.series {
            let mut sorted = s.clone();
            sorted.points.sort_by_key(|p| p.0);

            if sorted.points.len() < 2 {
                worst = Some(Coverage::TooFewSamples {
                    rail: *rail,
                    source: name.clone(),
                    n: sorted.points.len(),
                });
            }
            let j = sorted.joules();
            match rail {
                Rail::Compute => q.compute_j += j,
                Rail::Actuation => q.actuation_j += j,
                Rail::Overhead => q.overhead_j += j,
            }
            q.by_source.push((*rail, name.clone(), j));

            let pk = sorted.peak();
            if pk > q.peak_w {
                q.peak_w = pk;
                q.peak_source = format!("{rail}/{name}");
            }
            if let Some((a, b)) = sorted.span() {
                t_lo = t_lo.min(a);
                t_hi = t_hi.max(b);
            }

            let med = sorted.median_interval_ns();
            if med > 0 {
                median_intervals.push(med);
                let gap = sorted.largest_gap_ns();
                if gap as f64 > med as f64 * self.gap_tolerance
                    && !matches!(worst, Some(Coverage::TooFewSamples { .. }))
                {
                    worst = Some(Coverage::Gappy {
                        largest_gap_s: gap as f64 * 1e-9,
                        median_interval_s: med as f64 * 1e-9,
                        rail: *rail,
                        source: name.clone(),
                    });
                }
            }
        }

        q.total_j = q.compute_j + q.actuation_j + q.overhead_j;
        q.duration_s = if t_hi > t_lo {
            (t_hi - t_lo) as f64 * 1e-9
        } else {
            0.0
        };
        q.by_source
            .sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));

        median_intervals.sort_unstable();
        q.coverage = worst.unwrap_or(Coverage::Sound {
            median_interval_s: median_intervals
                .get(median_intervals.len() / 2)
                .copied()
                .unwrap_or(0) as f64
                * 1e-9,
            samples: self.series.values().map(|s| s.points.len()).sum(),
        });
        q.quotable = matches!(q.coverage, Coverage::Sound { .. });
        q
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn steady(led: &mut Ledger, rail: Rail, name: &str, hz: u64, secs: u64, watts: f64) {
        let step = 1_000_000_000 / hz;
        for i in 0..=hz * secs {
            led.sample(rail, name, i * step, watts);
        }
    }

    #[test]
    fn constant_power_integrates_to_power_times_time() {
        let mut led = Ledger::new();
        steady(&mut led, Rail::Compute, "soc", 100, 2, 15.0);
        let q = led.quote();
        assert!((q.total_j - 30.0).abs() < 1e-9, "got {}", q.total_j);
        assert!((q.duration_s - 2.0).abs() < 1e-9);
        assert!((q.mean_power_w() - 15.0).abs() < 1e-9);
        assert!(q.quotable);
    }

    #[test]
    fn a_ramp_integrates_to_the_triangle() {
        // 0 W to 100 W linearly over one second = 50 J, exactly, for the trapezoid rule.
        let mut led = Ledger::new();
        for i in 0..=1000u64 {
            led.sample(Rail::Actuation, "arm", i * 1_000_000, i as f64 * 0.1);
        }
        let q = led.quote();
        assert!((q.actuation_j - 50.0).abs() < 1e-6, "got {}", q.actuation_j);
    }

    #[test]
    fn rails_are_kept_apart() {
        let mut led = Ledger::new();
        steady(&mut led, Rail::Compute, "soc", 100, 1, 10.0);
        steady(&mut led, Rail::Actuation, "hip", 100, 1, 30.0);
        steady(&mut led, Rail::Overhead, "radio", 100, 1, 2.0);
        let q = led.quote();
        assert!((q.compute_j - 10.0).abs() < 1e-9);
        assert!((q.actuation_j - 30.0).abs() < 1e-9);
        assert!((q.overhead_j - 2.0).abs() < 1e-9);
        assert!((q.total_j - 42.0).abs() < 1e-9);
        // The ratio that motivates the whole lane: compute is not a rounding error.
        assert!((q.compute_fraction() - 10.0 / 42.0).abs() < 1e-12);
    }

    #[test]
    fn a_hole_in_the_telemetry_is_refused_not_smoothed() {
        let mut led = Ledger::new();
        for i in 0..100u64 {
            led.sample(Rail::Actuation, "hip", i * 10_000_000, 20.0);
        }
        // Four-second hole, then sampling resumes.
        for i in 0..100u64 {
            led.sample(Rail::Actuation, "hip", 5_000_000_000 + i * 10_000_000, 20.0);
        }
        let q = led.quote();
        assert!(!q.quotable, "a 4 s gap must not pass as a measurement");
        match q.coverage {
            Coverage::Gappy { largest_gap_s, .. } => assert!(largest_gap_s > 3.0),
            other => panic!("expected a gap verdict, got {other}"),
        }
        // …and the number is still there, so the flag is informative rather than punitive.
        assert!(q.total_j > 0.0);
    }

    #[test]
    fn one_sample_cannot_be_integrated() {
        let mut led = Ledger::new();
        led.sample(Rail::Compute, "soc", 0, 12.0);
        let q = led.quote();
        assert!(!q.quotable);
        assert!(matches!(q.coverage, Coverage::TooFewSamples { n: 1, .. }));
    }

    #[test]
    fn direct_energy_entries_integrate_exactly() {
        let mut led = Ledger::new();
        led.energy(Rail::Actuation, "drive", 0, 10_000_000_000, 250.0);
        let q = led.quote();
        assert!((q.actuation_j - 250.0).abs() < 1e-6);
    }

    #[test]
    fn viability_cost_is_undefined_without_a_success() {
        let mut led = Ledger::new();
        steady(&mut led, Rail::Actuation, "arm", 100, 1, 100.0);
        let q = led.quote();
        assert_eq!(q.joules_per_viable_task(10, 0), None);
        let jvt = q.joules_per_viable_task(10, 4).unwrap();
        assert!((jvt - 25.0).abs() < 1e-9);
    }

    #[test]
    fn the_biggest_consumer_sorts_first() {
        let mut led = Ledger::new();
        steady(&mut led, Rail::Actuation, "small", 100, 1, 5.0);
        steady(&mut led, Rail::Actuation, "big", 100, 1, 60.0);
        steady(&mut led, Rail::Compute, "soc", 100, 1, 20.0);
        let q = led.quote();
        assert_eq!(q.by_source[0].1, "big");
        assert_eq!(q.peak_source, "actuation/big");
    }

    #[test]
    fn out_of_order_samples_are_sorted_before_integration() {
        let mut led = Ledger::new();
        led.sample(Rail::Compute, "soc", 1_000_000_000, 10.0);
        led.sample(Rail::Compute, "soc", 0, 10.0);
        led.sample(Rail::Compute, "soc", 500_000_000, 10.0);
        let q = led.quote();
        assert!((q.total_j - 10.0).abs() < 1e-9, "got {}", q.total_j);
    }
}
