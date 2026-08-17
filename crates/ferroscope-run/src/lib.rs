//! **Scenarios, cases, suites and verdicts, with nothing between you and the run.**
//!
//! This crate ports the good ideas out of Antioch's scenario model into a Rust harness that
//! runs where you are. The model is deliberately recognizable: a scenario is a parameterized
//! 3-D integration test, cases turn one scenario into many comparable runs, a verdict is a set
//! of named checks with measured details, and a suite is a named union of selector clauses.
//! Those ideas are good and worth keeping.
//!
//! What is not kept is everything between the engineer and the answer. Antioch's own
//! documentation is the source for each of these:
//!
//! | Antioch | Here |
//! |---|---|
//! | A `antioch.yaml` manifest with a Compose-shaped `services` map, a `sim` service, an engine image and SDK tag | nothing. A scenario is a function in your binary |
//! | An ephemeral GPU VM per user per project. *"Keep the machine while iterating (allocation is the slow step)"*, and when none is warm the CLI *"polls up to 600 s"* | the process you already started |
//! | Simulator imports banned at module scope, because the CLI must discover scenarios *"before requesting a machine"* | no such rule; discovery is a `Vec` |
//! | Cost is assignment-scoped, *"idle time included … there is no per-run or per-scenario cost figure to report"* | every run records its own joules, wall seconds, and joules per passing check |
//! | Reproduction is re-queueing saved images and files; *"multi-machine interactive runs are not currently rerunnable"* | a determinism receipt per run, and a comparator that names the diverging step |
//! | *"The CLI has no `compare` command, the agent path is JSON"* | `compare` is a verb |
//!
//! ```no_run
//! use ferroscope_run::prelude::*;
//!
//! fn main() -> std::process::ExitCode {
//!     Harness::new()
//!         .scenario(
//!             Scenario::new("hop")
//!                 .describe("One leg, one hop, and the height it reached.")
//!                 .tags(["smoke"])
//!                 .param("stiffness", 8000.0)
//!                 .steps(1_000)
//!                 .dt(1.0 / 1000.0)
//!                 .cases(Case::sweep("stiffness", [6000.0, 8000.0, 10_000.0]))
//!                 .body(hop),
//!         )
//!         .suite(Suite::new("smoke").tags(["smoke"]))
//!         .main()
//! }
//!
//! fn hop(run: &mut Run) -> Result<(), Halt> {
//!     let k = run.param("stiffness");
//!     let (mut z, mut vz) = (0.35, 0.0);
//!     let mut peak = z;
//!     while run.running() {
//!         let t = run.tick();
//!         let f = if z < 0.32 { k * (0.32 - z) } else { 0.0 };
//!         vz += (-9.80665 + f / 12.0) * run.dt();
//!         z += vz * run.dt();
//!         peak = peak.max(z);
//!         run.position("/robot/base", t, [0.0, 0.0, z]);
//!         run.energy("/energy/leg", t, Rail::Actuation, "leg", (f * vz).abs() * 0.45);
//!         run.energy("/energy/soc", t, Rail::Compute, "soc", 7.8);
//!     }
//!     run.result("peak_height_m", peak);
//!     run.check("left the ground", peak > 0.36, format!("peak {peak:.4} m > 0.36 m"));
//!     Ok(())
//! }
//! ```
//!
//! ```text
//! $ cargo run -- run --tag smoke
//!   hop[stiffness=6000]  failed   1 check, 1 failed   0.9 ms   11.2 J
//!   hop[stiffness=8000]  passed   1 check              0.9 ms   14.8 J
//!   hop[stiffness=10000] passed   1 check              0.9 ms   17.6 J
//! 3 runs, 2 passed, 1 failed in 3 ms
//! ```

#![forbid(unsafe_code)]

pub mod run;
pub mod store;

mod harness;

pub use harness::{Case, Clause, Harness, Scenario, Selection, Suite, CASE_CAP};
pub use run::{Check, Halt, Metric, Outcome, Run};
pub use store::{Op, Predicate, Query, Record, Store, DEFAULT_ROOT};

/// Everything a scenario file needs in one line.
pub mod prelude {
    pub use crate::{Case, Halt, Harness, Outcome, Run, Scenario, Suite};
    pub use ferroscope_ledger::Rail;
    pub use ferroscope_schema::{Contact, JointState, Stamp};
}
