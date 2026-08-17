//! The run handle: the three clocks, the verdict, the evidence.

use std::path::PathBuf;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use ferroscope_ledger::{Quote, Rail};
use ferroscope_schema::json::Value;
use ferroscope_schema::{Contact, Geometry, JointState, Recorder, Stamp};

/// One named criterion and its verdict.
///
/// Ported from Antioch's `run.check(criterion, passed, detail=...)`, including the rule that
/// makes it useful: **`detail` carries the measurement, not a restatement of the name**.
/// `tilt 7.31° > 5.00°` is a finding; `upright failed` is not.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Check {
    pub criterion: String,
    pub passed: bool,
    pub detail: String,
}

/// What the run decided about the *task*, which is not the same question as whether the
/// process survived.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Every declared check passed, or none were declared.
    Passed,
    /// At least one check failed, or the body halted with `fail`.
    Failed,
    /// A precondition was not met. Not a failure.
    Skipped,
    /// The body panicked or halted with an error: a defect in the run, not a verdict on the
    /// task. Kept distinct from `Failed` on purpose.
    Errored,
}

impl Outcome {
    pub fn as_str(&self) -> &'static str {
        match self {
            Outcome::Passed => "passed",
            Outcome::Failed => "failed",
            Outcome::Skipped => "skipped",
            Outcome::Errored => "errored",
        }
    }
    pub fn parse(s: &str) -> Option<Outcome> {
        Some(match s {
            "passed" => Outcome::Passed,
            "failed" => Outcome::Failed,
            "skipped" => Outcome::Skipped,
            "errored" => Outcome::Errored,
            _ => return None,
        })
    }
    /// `true` for the one outcome a gate should let through.
    pub fn ok(&self) -> bool {
        matches!(self, Outcome::Passed)
    }
}

impl std::fmt::Display for Outcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Returned by [`Run::fail`], [`Run::skip`] and [`Run::error`] so a body can stop with `?`.
///
/// Halting is for "cannot continue": the rig did not build, the policy file is missing. A
/// criterion that merely did not hold is a [`Run::check`], and checks never stop the run,
/// because the other three measurements are exactly what you opened the run to see.
#[derive(Clone, Debug)]
pub struct Halt {
    pub(crate) outcome: Outcome,
    pub(crate) reason: String,
}

impl Halt {
    /// A defect in the run itself.
    pub fn error(reason: impl std::fmt::Display) -> Halt {
        Halt {
            outcome: Outcome::Errored,
            reason: reason.to_string(),
        }
    }
}

impl std::fmt::Display for Halt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.outcome, self.reason)
    }
}

/// The handle a scenario body is given.
///
/// It is one object because a run is one thing. Antioch splits the same information across a
/// `ScenarioRun` for verdicts, a module-scope `Logger` for telemetry, `antioch.yaml` for the
/// boot profile, and the platform for cost, four places, three of which need a server. Here
/// the three clocks, the energy ledger, the trace digest and the verdict all advance together
/// because [`Run::tick`] advances them.
pub struct Run {
    rec: Recorder<Vec<u8>>,
    params: Vec<(String, f64)>,
    checks: Vec<Check>,
    results: Vec<(String, Value)>,
    artifacts: Vec<(String, PathBuf)>,

    step: u64,
    total_steps: u64,
    dt_ns: u64,
    sim_ns: u64,
    started: Instant,
    started_us: u64,
    outcome_override: Option<Outcome>,
}

impl Run {
    pub(crate) fn new(
        precision: ferroscope_receipt::Precision,
        params: Vec<(String, f64)>,
        total_steps: u64,
        dt_ns: u64,
    ) -> Run {
        Run {
            rec: Recorder::new(Vec::new(), precision),
            params,
            checks: Vec::new(),
            results: Vec::new(),
            artifacts: Vec::new(),
            step: 0,
            total_steps,
            dt_ns,
            sim_ns: 0,
            started: Instant::now(),
            started_us: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_micros() as u64)
                .unwrap_or(0),
            outcome_override: None,
        }
    }

    // ---- inputs -----------------------------------------------------------

    /// A declared parameter's value for this case. Unknown names return `0.0` rather than
    /// panicking, because a typo in a parameter name should show up in the recorded results,
    /// not take the run down.
    pub fn param(&self, name: &str) -> f64 {
        self.params
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| *v)
            .unwrap_or(0.0)
    }

    /// Every parameter, in declaration order.
    pub fn params(&self) -> &[(String, f64)] {
        &self.params
    }

    /// How many steps this scenario declared.
    pub fn total_steps(&self) -> u64 {
        self.total_steps
    }

    /// The fixed timestep, in seconds.
    pub fn dt(&self) -> f64 {
        self.dt_ns as f64 * 1e-9
    }

    /// The current step index.
    pub fn step(&self) -> u64 {
        self.step
    }

    /// Simulated time so far, in seconds.
    pub fn sim_time(&self) -> f64 {
        self.sim_ns as f64 * 1e-9
    }

    // ---- the loop --------------------------------------------------------

    /// Advance one step and return the stamp to log against.
    ///
    /// This is the whole reason the three clocks cost nothing: `tick` moves simulated time by
    /// the declared `dt`, reads the real wall clock, and carries the control-step index. A
    /// controller that holds its rate in simulation and misses it on hardware shows up as
    /// drift between two of those three numbers, and the recording already has all three.
    pub fn tick(&mut self) -> Stamp {
        let t = Stamp::at(
            self.sim_ns,
            self.started.elapsed().as_nanos() as u64,
            self.step,
        );
        self.sim_ns += self.dt_ns;
        self.step += 1;
        t
    }

    /// `true` while the declared step budget has not been spent, for `while run.running()`.
    pub fn running(&self) -> bool {
        self.step < self.total_steps
    }

    // ---- verdict ---------------------------------------------------------

    /// Record one named criterion and its verdict. Returns `passed`, so it composes with
    /// control flow. Re-checking the same criterion replaces the verdict in place and keeps
    /// the declared order, so a per-step check in a control loop costs one row.
    pub fn check(&mut self, criterion: &str, passed: bool, detail: impl std::fmt::Display) -> bool {
        let detail = detail.to_string();
        if let Some(c) = self.checks.iter_mut().find(|c| c.criterion == criterion) {
            c.passed = passed;
            c.detail = detail;
        } else {
            self.checks.push(Check {
                criterion: criterion.to_string(),
                passed,
                detail,
            });
        }
        passed
    }

    /// Every check, in declared order.
    pub fn checks(&self) -> &[Check] {
        &self.checks
    }

    /// Stop: the run cannot continue and that is a failure of the task.
    pub fn fail(&mut self, reason: impl std::fmt::Display) -> Halt {
        Halt {
            outcome: Outcome::Failed,
            reason: reason.to_string(),
        }
    }

    /// Stop: a precondition was not met, which is not a failure.
    pub fn skip(&mut self, reason: impl std::fmt::Display) -> Halt {
        Halt {
            outcome: Outcome::Skipped,
            reason: reason.to_string(),
        }
    }

    /// Stop: something about the run itself is broken.
    pub fn error(&mut self, reason: impl std::fmt::Display) -> Halt {
        Halt::error(reason)
    }

    /// Override the derived outcome, for a judgement more subtle than a conjunction of checks.
    pub fn set_outcome(&mut self, outcome: Outcome) {
        self.outcome_override = Some(outcome);
    }

    /// The outcome the checks imply right now.
    pub fn outcome(&self) -> Outcome {
        if let Some(o) = self.outcome_override {
            return o;
        }
        if self.checks.iter().any(|c| !c.passed) {
            Outcome::Failed
        } else {
            Outcome::Passed
        }
    }

    // ---- evidence --------------------------------------------------------

    /// Save a named metric. Record the *threshold a check used*, not only its verdict: a
    /// reader six weeks later needs to know what "passed" meant.
    pub fn result(&mut self, name: &str, value: impl Into<Metric>) {
        let value: Value = value.into().0;
        if let Some(slot) = self.results.iter_mut().find(|(k, _)| k == name) {
            slot.1 = value;
        } else {
            self.results.push((name.to_string(), value));
        }
    }

    pub fn results(&self) -> &[(String, Value)] {
        &self.results
    }

    /// Attach a file produced by the run. Recorded by path; the store copies it in beside the
    /// recording, so an artifact cannot outlive its run or go missing from another machine.
    pub fn artifact(&mut self, name: &str, path: impl Into<PathBuf>) {
        self.artifacts.push((name.to_string(), path.into()));
    }

    pub fn artifacts(&self) -> &[(String, PathBuf)] {
        &self.artifacts
    }

    // ---- telemetry (delegated to the recorder) ---------------------------

    /// A rigid transform.
    pub fn transform(
        &mut self,
        topic: &str,
        t: Stamp,
        parent: &str,
        child: &str,
        translation: [f64; 3],
        rotation: [f64; 4],
    ) {
        let _ = self
            .rec
            .transform(topic, t, parent, child, translation, rotation);
    }

    /// A pose with no rotation, for the common case.
    pub fn position(&mut self, topic: &str, t: Stamp, xyz: [f64; 3]) {
        self.transform(topic, t, "world", topic, xyz, [0.0, 0.0, 0.0, 1.0]);
    }

    pub fn joints(&mut self, topic: &str, t: Stamp, js: &JointState) {
        let _ = self.rec.joints(topic, t, js);
    }

    /// Declare a drawable. Call it once before the loop for static scenery, or once per step on
    /// the same `(frame, id)` for a moving part. This is what makes the 3-D panel show the
    /// machine rather than an axis triad.
    pub fn geometry(&mut self, topic: &str, t: Stamp, g: &Geometry) {
        let _ = self.rec.geometry(topic, t, g);
    }

    pub fn contact(&mut self, topic: &str, t: Stamp, c: &Contact) {
        let _ = self.rec.contact(topic, t, c);
    }

    /// Power on one named source, booked into the energy ledger as it is logged.
    pub fn energy(&mut self, topic: &str, t: Stamp, rail: Rail, source: &str, watts: f64) {
        let _ = self.rec.energy(topic, t, rail, source, watts);
    }

    pub fn scalar(&mut self, topic: &str, t: Stamp, value: f64, unit: &str) {
        let _ = self.rec.scalar(topic, t, value, unit);
    }

    pub fn event(&mut self, t: Stamp, level: &str, text: &str) {
        let _ = self.rec.event("/log", t, level, text);
    }

    /// The energy ledger so far, for a body that wants to check its own joule budget.
    pub fn energy_so_far(&self) -> Quote {
        self.rec.ledger().quote()
    }

    // ---- sealing ---------------------------------------------------------

    pub(crate) fn into_parts(self) -> Parts {
        let elapsed = self.started.elapsed();
        Parts {
            rec: self.rec,
            checks: self.checks,
            results: self.results,
            artifacts: self.artifacts,
            outcome_override: self.outcome_override,
            steps: self.step,
            sim_ns: self.sim_ns,
            started_us: self.started_us,
            elapsed,
        }
    }
}

/// Everything the harness needs out of a finished body.
pub(crate) struct Parts {
    pub rec: Recorder<Vec<u8>>,
    pub checks: Vec<Check>,
    pub results: Vec<(String, Value)>,
    pub artifacts: Vec<(String, PathBuf)>,
    pub outcome_override: Option<Outcome>,
    pub steps: u64,
    pub sim_ns: u64,
    pub started_us: u64,
    pub elapsed: std::time::Duration,
}

// ---------------------------------------------------------------------------
// Value conversions, so `run.result("tilt_deg", 7.31)` needs no ceremony.
// ---------------------------------------------------------------------------

macro_rules! from_num {
    ($($t:ty),*) => { $( impl From<$t> for Metric { fn from(v: $t) -> Self { Metric(Value::Num(v as f64)) } } )* };
}

/// A recorded metric. A newtype around the JSON value, so `run.result("tilt_deg", 7.31)` and
/// `run.result("policy", "flow-matching")` both work without the caller naming a type.
pub struct Metric(pub Value);
from_num!(f64, f32, i64, i32, u64, u32, usize);

impl From<bool> for Metric {
    fn from(v: bool) -> Self {
        Metric(Value::Bool(v))
    }
}
impl From<&str> for Metric {
    fn from(v: &str) -> Self {
        Metric(Value::Str(v.to_string()))
    }
}
impl From<String> for Metric {
    fn from(v: String) -> Self {
        Metric(Value::Str(v))
    }
}
impl From<Vec<f64>> for Metric {
    fn from(v: Vec<f64>) -> Self {
        Metric(Value::Arr(v.into_iter().map(Value::Num).collect()))
    }
}
impl From<Metric> for Value {
    fn from(w: Metric) -> Value {
        w.0
    }
}
