//! Declaration, selection, execution, and the CLI your binary gets for free.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

use ferroscope_receipt::{Precision, RunSpec, Tolerance};
use ferroscope_schema::json::Value;
use ferroscope_schema::{trace_from, verify};

use crate::run::{Halt, Outcome, Run};
use crate::store::{DEFAULT_ROOT, Predicate, Query, Record, Store};

/// The cap on expanded cases for one scenario. Antioch caps at 2,000; matching it keeps a
/// ported sweep portable, and the refusal names the count so a narrower grid is obvious.
pub const CASE_CAP: usize = 2_000;

type Body = Box<dyn Fn(&mut Run) -> Result<(), Halt> + Send + Sync>;

/// One named input set.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Case {
    pub id: String,
    pub tags: Vec<String>,
    pub set: Vec<(String, f64)>,
}

impl Case {
    /// A named case with no overrides.
    pub fn new(id: impl Into<String>) -> Case {
        Case {
            id: id.into(),
            ..Default::default()
        }
    }

    /// Override one parameter.
    pub fn set(mut self, key: impl Into<String>, value: f64) -> Case {
        self.set.push((key.into(), value));
        self
    }

    pub fn tag(mut self, t: impl Into<String>) -> Case {
        self.tags.push(t.into());
        self
    }

    /// One axis: `Case::sweep("seed", 0..10)` or `Case::sweep("mu", [0.3, 0.8])`.
    ///
    /// Ids derive from the override, `mu=0.3`-style, the same convention Antioch uses when a
    /// case id is omitted.
    pub fn sweep(
        key: impl Into<String>,
        values: impl IntoIterator<Item = impl Into<f64>>,
    ) -> Vec<Case> {
        let key = key.into();
        values
            .into_iter()
            .map(|v| {
                let v = v.into();
                Case::new(derive_id(&[(key.clone(), v)])).set(key.clone(), v)
            })
            .collect()
    }

    /// The Cartesian product of several axes, in declared order.
    ///
    /// ```
    /// # use ferroscope_run::Case;
    /// let cases = Case::grid(&[("seed", vec![1.0, 2.0]), ("mu", vec![0.3, 0.8])]).unwrap();
    /// assert_eq!(cases.len(), 4);
    /// assert_eq!(cases[0].id, "seed=1,mu=0.3");
    /// ```
    pub fn grid(axes: &[(&str, Vec<f64>)]) -> Result<Vec<Case>, String> {
        let total: usize = axes.iter().map(|(_, v)| v.len().max(1)).product();
        if total > CASE_CAP {
            return Err(format!(
                "{total} expanded cases exceeds the {CASE_CAP}-case cap; narrow the grid"
            ));
        }
        let mut out: Vec<Vec<(String, f64)>> = vec![Vec::new()];
        for (key, values) in axes {
            let mut next = Vec::with_capacity(out.len() * values.len());
            for base in &out {
                for v in values {
                    let mut row = base.clone();
                    row.push(((*key).to_string(), *v));
                    next.push(row);
                }
            }
            out = next;
        }
        Ok(out
            .into_iter()
            .map(|row| Case {
                id: derive_id(&row),
                tags: Vec::new(),
                set: row,
            })
            .collect())
    }

    /// Correlated rows, for parameters that must move together.
    pub fn combinations(rows: Vec<Vec<(&str, f64)>>) -> Vec<Case> {
        rows.into_iter()
            .map(|row| {
                let set: Vec<(String, f64)> =
                    row.into_iter().map(|(k, v)| (k.to_string(), v)).collect();
                Case {
                    id: derive_id(&set),
                    tags: Vec::new(),
                    set,
                }
            })
            .collect()
    }
}

fn fmt_num(v: f64) -> String {
    let mut s = String::new();
    ferroscope_schema::json::write_number(&mut s, v);
    s
}

fn derive_id(set: &[(String, f64)]) -> String {
    if set.is_empty() {
        return "default".to_string();
    }
    set.iter()
        .map(|(k, v)| format!("{k}={}", fmt_num(*v)))
        .collect::<Vec<_>>()
        .join(",")
}

/// A parameterized run definition.
pub struct Scenario {
    pub name: String,
    pub description: String,
    pub tags: Vec<String>,
    pub params: Vec<(String, f64)>,
    pub cases: Vec<Case>,
    pub steps: u64,
    pub dt_ns: u64,
    pub precision: Precision,
    body: Body,
}

impl Scenario {
    pub fn new(name: impl Into<String>) -> Scenario {
        Scenario {
            name: name.into(),
            description: String::new(),
            tags: Vec::new(),
            params: Vec::new(),
            cases: Vec::new(),
            steps: 1_000,
            dt_ns: 1_000_000,
            // Quantized by default: a bit-exact digest across GPU fabrics is a promise no
            // simulator keeps, and a receipt should state what it can actually verify.
            precision: Precision::Quantized { drop_bits: 12 },
            body: Box::new(|_| Ok(())),
        }
    }

    pub fn describe(mut self, d: impl Into<String>) -> Scenario {
        self.description = d.into();
        self
    }

    pub fn tags<I: IntoIterator<Item = S>, S: Into<String>>(mut self, tags: I) -> Scenario {
        self.tags.extend(tags.into_iter().map(Into::into));
        self
    }

    /// Declare a parameter and its default. The declaration is the schema.
    pub fn param(mut self, name: impl Into<String>, default: f64) -> Scenario {
        self.params.push((name.into(), default));
        self
    }

    /// The step budget for [`Run::running`].
    pub fn steps(mut self, n: u64) -> Scenario {
        self.steps = n;
        self
    }

    /// The fixed timestep, in seconds.
    pub fn dt(mut self, seconds: f64) -> Scenario {
        self.dt_ns = (seconds * 1e9).round() as u64;
        self
    }

    /// How much of each float the run's trace digest may see.
    pub fn precision(mut self, p: Precision) -> Scenario {
        self.precision = p;
        self
    }

    pub fn case(mut self, c: Case) -> Scenario {
        self.cases.push(c);
        self
    }

    pub fn cases<I: IntoIterator<Item = Case>>(mut self, cs: I) -> Scenario {
        self.cases.extend(cs);
        self
    }

    pub fn body(
        mut self,
        f: impl Fn(&mut Run) -> Result<(), Halt> + Send + Sync + 'static,
    ) -> Scenario {
        self.body = Box::new(f);
        self
    }

    /// The cases this scenario will actually run: the declared ones, or one implicit default.
    pub fn expanded(&self) -> Vec<Case> {
        if self.cases.is_empty() {
            vec![Case::new("default")]
        } else {
            self.cases.clone()
        }
    }
}

/// One clause of a suite's selection. Fields inside a clause narrow together.
#[derive(Clone, Debug, Default)]
pub struct Clause {
    pub scenarios: Vec<String>,
    pub tags: Vec<String>,
    pub exclude_tags: Vec<String>,
    pub cases: Vec<String>,
}

/// A named union of selector clauses, in authored order.
#[derive(Clone, Debug, Default)]
pub struct Suite {
    pub name: String,
    pub description: String,
    pub select: Vec<Clause>,
}

impl Suite {
    pub fn new(name: impl Into<String>) -> Suite {
        Suite {
            name: name.into(),
            ..Default::default()
        }
    }

    pub fn describe(mut self, d: impl Into<String>) -> Suite {
        self.description = d.into();
        self
    }

    /// Shorthand for a one-clause suite that selects by tag.
    pub fn tags<I: IntoIterator<Item = S>, S: Into<String>>(mut self, tags: I) -> Suite {
        self.select.push(Clause {
            tags: tags.into_iter().map(Into::into).collect(),
            ..Default::default()
        });
        self
    }

    /// Shorthand for a one-clause suite that names scenarios.
    pub fn scenarios<I: IntoIterator<Item = S>, S: Into<String>>(mut self, names: I) -> Suite {
        self.select.push(Clause {
            scenarios: names.into_iter().map(Into::into).collect(),
            ..Default::default()
        });
        self
    }

    pub fn clause(mut self, c: Clause) -> Suite {
        self.select.push(c);
        self
    }
}

/// What to run.
#[derive(Clone, Debug, Default)]
pub struct Selection {
    pub scenarios: Vec<String>,
    pub tags: Vec<String>,
    pub exclude_tags: Vec<String>,
    pub cases: Vec<String>,
    /// Ad-hoc parameter overrides, applied to every selected case.
    pub set: Vec<(String, f64)>,
}

impl Selection {
    fn selects(&self, s: &Scenario, c: &Case) -> bool {
        if !self.scenarios.is_empty() && !self.scenarios.contains(&s.name) {
            return false;
        }
        if !self.tags.is_empty()
            && !self
                .tags
                .iter()
                .any(|t| s.tags.contains(t) || c.tags.contains(t))
        {
            return false;
        }
        if self
            .exclude_tags
            .iter()
            .any(|t| s.tags.contains(t) || c.tags.contains(t))
        {
            return false;
        }
        if !self.cases.is_empty() && !self.cases.contains(&c.id) {
            return false;
        }
        true
    }

    fn from_clause(c: &Clause) -> Selection {
        Selection {
            scenarios: c.scenarios.clone(),
            tags: c.tags.clone(),
            exclude_tags: c.exclude_tags.clone(),
            cases: c.cases.clone(),
            set: Vec::new(),
        }
    }
}

/// The harness: declarations in, runs and a CLI out.
pub struct Harness {
    scenarios: Vec<Scenario>,
    suites: Vec<Suite>,
    root: PathBuf,
}

impl Default for Harness {
    fn default() -> Self {
        Self::new()
    }
}

impl Harness {
    pub fn new() -> Harness {
        Harness {
            scenarios: Vec::new(),
            suites: Vec::new(),
            root: PathBuf::from(DEFAULT_ROOT),
        }
    }

    /// Where run history lives. Defaults to `.ferroscope` beside the working directory.
    pub fn store(mut self, root: impl Into<PathBuf>) -> Harness {
        self.root = root.into();
        self
    }

    pub fn scenario(mut self, s: Scenario) -> Harness {
        self.scenarios.push(s);
        self
    }

    pub fn suite(mut self, s: Suite) -> Harness {
        self.suites.push(s);
        self
    }

    /// Every `(scenario, case)` pair a selection would run.
    pub fn collect(&self, sel: &Selection) -> Vec<(&Scenario, Case)> {
        let mut out = Vec::new();
        for s in &self.scenarios {
            for c in s.expanded() {
                if sel.selects(s, &c) {
                    out.push((s, c));
                }
            }
        }
        out
    }

    /// The union of a named suite's clauses, in authored order, without duplicates.
    pub fn collect_suite(&self, name: &str) -> Result<Vec<(&Scenario, Case)>, String> {
        let suite = self.suites.iter().find(|s| s.name == name).ok_or_else(|| {
            let known: Vec<&str> = self.suites.iter().map(|s| s.name.as_str()).collect();
            format!("no suite named {name:?} (declared: {})", known.join(", "))
        })?;
        let mut out: Vec<(&Scenario, Case)> = Vec::new();
        for clause in &suite.select {
            for (s, c) in self.collect(&Selection::from_clause(clause)) {
                if !out
                    .iter()
                    .any(|(os, oc)| os.name == s.name && oc.id == c.id)
                {
                    out.push((s, c));
                }
            }
        }
        Ok(out)
    }

    /// Run one `(scenario, case)` pair and store the result.
    pub fn execute_one(
        &self,
        s: &Scenario,
        c: &Case,
        extra: &[(String, f64)],
        store: &Store,
    ) -> Record {
        // Params: declared defaults, then the case, then any ad-hoc --set.
        let mut params = s.params.clone();
        for (k, v) in c.set.iter().chain(extra.iter()) {
            match params.iter_mut().find(|(pk, _)| pk == k) {
                Some(slot) => slot.1 = *v,
                None => params.push((k.clone(), *v)),
            }
        }

        let mut run = Run::new(s.precision, params.clone(), s.steps, s.dt_ns);
        // A panic in a body is a defect in the run, not a verdict on the task, so it is caught
        // and reported as ERRORED rather than taking the whole suite down.
        let halt = match catch_unwind(AssertUnwindSafe(|| (s.body)(&mut run))) {
            Ok(Ok(())) => None,
            Ok(Err(h)) => Some(h),
            Err(payload) => Some(Halt::error(panic_message(payload))),
        };

        let parts = run.into_parts();
        let outcome = match (&halt, parts.outcome_override) {
            (Some(h), _) => h.outcome,
            (None, Some(o)) => o,
            (None, None) => {
                if parts.checks.iter().any(|k| !k.passed) {
                    Outcome::Failed
                } else {
                    Outcome::Passed
                }
            }
        };
        let reason = halt.map(|h| h.reason).unwrap_or_default();

        let mut spec = RunSpec::new(&s.name, params_seed(&params))
            .dt_ns(s.dt_ns)
            .steps(parts.steps)
            .integrator("caller")
            .solver("caller")
            .build(concat!("ferroscope-run ", env!("CARGO_PKG_VERSION")));
        spec = spec.config("case", &c.id);
        for (k, v) in &params {
            spec = spec.config(k, fmt_num(*v));
        }

        let platform = format!("{}-{}", std::env::consts::ARCH, std::env::consts::OS);
        let (bytes, receipt, quote) = parts
            .rec
            .seal(spec, &platform)
            .unwrap_or_else(|e| panic!("sealing a recording cannot fail: {e}"));

        // Recompute the receipt from the bytes we are about to store. A stored recording that
        // does not verify at write time would never verify later either.
        let verified = verify(&bytes).map(|v| v.ok()).unwrap_or(false);

        let mut tags = s.tags.clone();
        tags.extend(c.tags.iter().cloned());

        let rec = Record {
            id: format!(
                "{}-{}-{}",
                parts.started_us,
                sanitize(&s.name),
                sanitize(&c.id)
            ),
            scenario: s.name.clone(),
            case: c.id.clone(),
            tags,
            outcome,
            reason,
            params,
            checks: parts.checks,
            results: parts.results,
            artifacts: parts.artifacts.iter().map(|(n, _)| n.clone()).collect(),
            spec_digest: receipt.spec_digest.clone(),
            trace_digest: receipt.trace_digest.clone(),
            verified,
            energy_j: quote.total_j,
            compute_j: quote.compute_j,
            actuation_j: quote.actuation_j,
            quotable: quote.quotable,
            steps: parts.steps,
            sim_s: parts.sim_ns as f64 * 1e-9,
            wall_s: parts.elapsed.as_secs_f64(),
            started_us: parts.started_us,
            platform,
            mcap_bytes: bytes.len() as u64,
        };
        let _ = store.put(&rec, &bytes, &parts.artifacts);
        rec
    }

    /// Run a selection and return every record, in the order they ran.
    pub fn execute(&self, sel: &Selection) -> std::io::Result<Vec<Record>> {
        let store = Store::open(&self.root)?;
        Ok(self
            .collect(sel)
            .into_iter()
            .map(|(s, c)| self.execute_one(s, &c, &sel.set, &store))
            .collect())
    }

    // -----------------------------------------------------------------------
    // The CLI
    // -----------------------------------------------------------------------

    /// Parse `std::env::args()` and act. Give your binary this as its `main` and it has the
    /// whole surface: `collect`, `run`, `suites`, `list`, `show`, `compare`.
    pub fn main(mut self) -> ExitCode {
        let args: Vec<String> = std::env::args().skip(1).collect();
        let argv: Vec<&str> = args.iter().map(String::as_str).collect();
        match self.dispatch(&argv) {
            Ok(code) => code,
            Err(e) => {
                eprintln!("error: {e}");
                ExitCode::from(2)
            }
        }
    }

    fn dispatch(&mut self, argv: &[&str]) -> Result<ExitCode, String> {
        let (verb, rest) = match argv.split_first() {
            None | Some((&"help", _)) | Some((&"-h", _)) | Some((&"--help", _)) => {
                self.usage();
                return Ok(ExitCode::SUCCESS);
            }
            Some((v, r)) => (*v, r),
        };
        let flags = Flags::parse(rest)?;
        if let Some(root) = &flags.store {
            self.root = PathBuf::from(root);
        }
        match verb {
            "collect" => self.cmd_collect(&flags),
            "run" => self.cmd_run(&flags),
            "suites" => self.cmd_suites(&flags),
            "list" => self.cmd_list(&flags),
            "show" => self.cmd_show(&flags),
            "compare" => self.cmd_compare(&flags),
            other => Err(format!(
                "unknown command {other:?}; try collect, run, suites, list, show, compare"
            )),
        }
    }

    fn usage(&self) {
        println!(
            "\
{n} scenarios, {s} suites, history in {root}

USAGE
  collect  [--scenario S] [--tag T] [--exclude-tag T] [--case C] [--json]
  run      [--scenario S] [--tag T] [--exclude-tag T] [--case C] [--set k=v]
           [--suite NAME] [--json]
  suites   [--json]
  list     [--scenario S] [--tag T] [--outcome O] [-q TEXT]
           [--param k:op:v] [--result k:op:v] [--limit N] [--json]
  show     RUN_ID [--json]
  compare  RUN_A RUN_B [--abs F] [--rel F]

  --store DIR overrides the history directory on any command.

PREDICATE OPERATORS  = typed equality   ~ substring   < less   > greater   @ dotted path

EXIT CODES  0 every selected run passed | 1 something failed | 2 the tool could not answer",
            n = self.scenarios.len(),
            s = self.suites.len(),
            root = self.root.display(),
        );
    }

    fn cmd_collect(&self, f: &Flags) -> Result<ExitCode, String> {
        let picked = self.collect(&f.selection());
        if f.json {
            let items: Vec<Value> = picked
                .iter()
                .map(|(s, c)| {
                    Value::Obj(vec![
                        ("scenario".into(), Value::Str(s.name.clone())),
                        ("description".into(), Value::Str(s.description.clone())),
                        ("case".into(), Value::Str(c.id.clone())),
                        (
                            "tags".into(),
                            Value::Arr(
                                s.tags
                                    .iter()
                                    .chain(c.tags.iter())
                                    .cloned()
                                    .map(Value::Str)
                                    .collect(),
                            ),
                        ),
                        ("steps".into(), Value::Num(s.steps as f64)),
                        ("dt_s".into(), Value::Num(s.dt_ns as f64 * 1e-9)),
                    ])
                })
                .collect();
            println!("{}", Value::Arr(items).to_json());
        } else {
            for (s, c) in &picked {
                let tags: Vec<&str> = s
                    .tags
                    .iter()
                    .chain(c.tags.iter())
                    .map(String::as_str)
                    .collect();
                println!("  {:<28} {:<24} [{}]", s.name, c.id, tags.join(" "));
            }
            println!("{} case(s) selected", picked.len());
        }
        Ok(ExitCode::SUCCESS)
    }

    fn cmd_suites(&self, f: &Flags) -> Result<ExitCode, String> {
        if f.json {
            let items: Vec<Value> = self
                .suites
                .iter()
                .map(|s| {
                    Value::Obj(vec![
                        ("name".into(), Value::Str(s.name.clone())),
                        ("description".into(), Value::Str(s.description.clone())),
                        (
                            "cases".into(),
                            Value::Num(
                                self.collect_suite(&s.name).map(|v| v.len()).unwrap_or(0) as f64
                            ),
                        ),
                    ])
                })
                .collect();
            println!("{}", Value::Arr(items).to_json());
        } else {
            for s in &self.suites {
                let n = self.collect_suite(&s.name).map(|v| v.len()).unwrap_or(0);
                println!("  {:<20} {n:>4} case(s)   {}", s.name, s.description);
            }
        }
        Ok(ExitCode::SUCCESS)
    }

    fn cmd_run(&self, f: &Flags) -> Result<ExitCode, String> {
        let store = Store::open(&self.root).map_err(|e| e.to_string())?;
        let picked: Vec<(&Scenario, Case)> = match &f.suite {
            Some(name) => self.collect_suite(name)?,
            None => self.collect(&f.selection()),
        };
        if picked.is_empty() {
            return Err(
                "the selection matched no cases; try `collect` to see what is declared".into(),
            );
        }

        let started = Instant::now();
        let mut records = Vec::with_capacity(picked.len());
        for (s, c) in &picked {
            let r = self.execute_one(s, c, &f.set, &store);
            if !f.json {
                let failed = r.checks.iter().filter(|k| !k.passed).count();
                let verdict = if failed > 0 {
                    format!("{} check(s), {failed} failed", r.checks.len())
                } else if r.checks.is_empty() {
                    "no checks declared".to_string()
                } else {
                    format!("{} check(s)", r.checks.len())
                };
                println!(
                    "  {:<40} {:<8} {:<26} {:>8.1} ms {:>9.2} J{}",
                    ellipsize(&format!("{}[{}]", s.name, c.id), 40),
                    r.outcome.as_str(),
                    verdict,
                    r.wall_s * 1e3,
                    r.energy_j,
                    if r.verified { "" } else { "  RECEIPT MISMATCH" },
                );
                for k in r.failed_checks() {
                    println!("      x {}: {}", k.criterion, k.detail);
                }
                if !r.reason.is_empty() {
                    println!("      ! {}", r.reason);
                }
            }
            records.push(r);
        }

        let passed = records.iter().filter(|r| r.outcome.ok()).count();
        let energy: f64 = records.iter().map(|r| r.energy_j).sum();
        if f.json {
            let items: Vec<Value> = records.iter().map(|r| r.to_value()).collect();
            println!("{}", Value::Arr(items).to_json());
        } else {
            println!(
                "{} run(s), {passed} passed, {} not, {:.2} J total, in {} ms",
                records.len(),
                records.len() - passed,
                energy,
                started.elapsed().as_millis()
            );
        }
        Ok(if passed == records.len() {
            ExitCode::SUCCESS
        } else {
            ExitCode::from(1)
        })
    }

    fn cmd_list(&self, f: &Flags) -> Result<ExitCode, String> {
        let store = Store::open(&self.root).map_err(|e| e.to_string())?;
        let mut q = Query::new();
        q.scenario = f.scenarios.clone();
        q.case = f.cases.clone();
        q.tag = f.tags.clone();
        q.exclude_tag = f.exclude_tags.clone();
        q.search = f.search.clone();
        q.params = f.params.clone();
        q.results = f.results.clone();
        q.limit = f.limit.unwrap_or(50);
        for o in &f.outcomes {
            q.outcome.push(
                Outcome::parse(o).ok_or_else(|| {
                    format!("unknown outcome {o:?} (passed failed skipped errored)")
                })?,
            );
        }
        let rows = store.list(&q);
        if f.json {
            let items: Vec<Value> = rows.iter().map(|r| r.to_value()).collect();
            println!("{}", Value::Arr(items).to_json());
        } else if rows.is_empty() {
            println!(
                "no runs match; the history directory is {}",
                self.root.display()
            );
        } else {
            println!(
                "  {:<8} {:<22} {:<18} {:>9} {:>9} {:>7}  RUN ID",
                "OUTCOME", "SCENARIO", "CASE", "JOULES", "WALL ms", "RTF"
            );
            for r in &rows {
                println!(
                    "  {:<8} {:<22} {:<18} {:>9.2} {:>9.1} {:>7.1} {}",
                    r.outcome.as_str(),
                    ellipsize(&r.scenario, 22),
                    ellipsize(&r.case, 18),
                    r.energy_j,
                    r.wall_s * 1e3,
                    r.real_time_factor(),
                    r.id
                );
            }
            println!("{} run(s)", rows.len());
        }
        Ok(ExitCode::SUCCESS)
    }

    fn cmd_show(&self, f: &Flags) -> Result<ExitCode, String> {
        let id = f
            .positional
            .first()
            .ok_or("show needs a run id; `list` prints them")?;
        let store = Store::open(&self.root).map_err(|e| e.to_string())?;
        let r = store
            .get(id)
            .ok_or_else(|| format!("no run {id:?} in {}", self.root.display()))?;
        if f.json {
            println!("{}", r.to_value().to_json());
            return Ok(if r.outcome.ok() {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(1)
            });
        }
        println!("{}", r.id);
        println!("  scenario     {} [{}]", r.scenario, r.case);
        println!("  outcome      {}", r.outcome);
        if !r.reason.is_empty() {
            println!("  reason       {}", r.reason);
        }
        println!("  tags         {}", r.tags.join(" "));
        println!(
            "  params       {}",
            r.params
                .iter()
                .map(|(k, v)| format!("{k}={}", fmt_num(*v)))
                .collect::<Vec<_>>()
                .join(" ")
        );
        println!("\n  CHECKS");
        if r.checks.is_empty() {
            println!("    (none declared: this run reports only that the code did not crash)");
        }
        for c in &r.checks {
            println!(
                "    {} {:<26} {}",
                if c.passed { "ok  " } else { "FAIL" },
                c.criterion,
                c.detail
            );
        }
        if !r.results.is_empty() {
            println!("\n  RESULTS");
            for (k, v) in &r.results {
                println!("    {k:<26} {}", v.brief());
            }
        }
        println!("\n  COST");
        println!(
            "    energy         {:.3} J  (compute {:.3}, actuation {:.3}){}",
            r.energy_j,
            r.compute_j,
            r.actuation_j,
            if r.quotable { "" } else { "   DO NOT QUOTE" }
        );
        match r.joules_per_pass() {
            Some(j) => println!("    per pass       {j:.3} J"),
            None => println!("    per pass       undefined (nothing passed)"),
        }
        println!("    wall           {:.1} ms", r.wall_s * 1e3);
        println!(
            "    simulated      {:.3} s  (real-time factor {:.1})",
            r.sim_s,
            r.real_time_factor()
        );
        println!("\n  RECEIPT");
        println!("    spec digest    {}", r.spec_digest);
        println!("    trace digest   {}", r.trace_digest);
        println!(
            "    verified       {}",
            if r.verified { "yes" } else { "NO" }
        );
        println!("    platform       {}", r.platform);
        println!(
            "    recording      {} ({} bytes)",
            self.root.join(&r.id).join("run.mcap").display(),
            r.mcap_bytes
        );
        Ok(if r.outcome.ok() {
            ExitCode::SUCCESS
        } else {
            ExitCode::from(1)
        })
    }

    fn cmd_compare(&self, f: &Flags) -> Result<ExitCode, String> {
        let (a, b) = match f.positional.as_slice() {
            [a, b] => (a, b),
            _ => return Err("compare needs two run ids".into()),
        };
        let store = Store::open(&self.root).map_err(|e| e.to_string())?;
        let ra = store.get(a).ok_or_else(|| format!("no run {a:?}"))?;
        let rb = store.get(b).ok_or_else(|| format!("no run {b:?}"))?;
        let ba = store
            .recording(a)
            .ok_or_else(|| format!("no recording for {a:?}"))?;
        let bb = store
            .recording(b)
            .ok_or_else(|| format!("no recording for {b:?}"))?;

        println!(
            "A  {}  {} [{}]  on {}",
            ra.id, ra.scenario, ra.case, ra.platform
        );
        println!(
            "B  {}  {} [{}]  on {}\n",
            rb.id, rb.scenario, rb.case, rb.platform
        );

        let (reca, ta) = trace_from(&ba).ok_or("cannot read recording A")?;
        let (recb, tb) = trace_from(&bb).ok_or("cannot read recording B")?;
        let tol = Tolerance {
            abs: f.abs.unwrap_or(1e-9),
            rel: f.rel.unwrap_or(1e-9),
        };

        // Recompute both receipts first. This path used to take the same shortcut the CLI and
        // the browser comparator took — `digests_agree` on two STORED digest strings, returning
        // SUCCESS on a match — so a recording whose metadata block carried another run's
        // trace_digest passed however its messages read. This is the surface a suite gate calls
        // to decide whether a scenario reproduced, so it is the one where that mattered most.
        let va = verify(&ba);
        let vb = verify(&bb);
        let trustworthy =
            va.as_ref().is_some_and(|v| v.ok()) && vb.as_ref().is_some_and(|v| v.ok());
        if !trustworthy {
            for (label, v) in [("A", &va), ("B", &vb)] {
                match v {
                    Some(v) if v.ok() => {}
                    Some(_) => println!("  {label} DOES NOT VERIFY against its own receipt"),
                    None => println!("  {label} carries no recomputable receipt"),
                }
            }
            println!("  what follows compares bytes, not evidence.");
        }

        if let (Some(x), Some(y)) = (&reca, &recb) {
            let diffs = ferroscope_receipt::spec_differences(&x.spec, &y.spec);
            if !diffs.is_empty() {
                println!("  the specs differ, so these are not two runs of the same experiment:");
                for d in diffs.iter().take(6) {
                    println!("    {:<20} {}  ->  {}", d.field, d.a, d.b);
                }
            }
        }

        let p = ferroscope_receipt::profile(&ta, &tb, tol);
        let verdict = p.verdict.clone();
        if p.structural.is_some() {
            println!("  on what both runs recorded: {verdict}");
        } else {
            println!("  {verdict}");
        }
        if let Some((ch, step)) = &p.onset {
            println!("  onset     step {step} on {ch} - where the bits first parted");
        }
        if let Some(d) = p.dominant() {
            println!("  shape     {}", d.shape);
            for c in p.channels.iter().take(4) {
                println!(
                    "    {:<24} delta/scale {:.3e} at step {}",
                    c.channel, c.worst_scaled, c.worst_scaled_step
                );
            }
        }
        if let Some(st) = &p.structural {
            println!(
                "  coverage  the verdict covers the {} sample(s) both runs recorded; {} excluded",
                st.shared_samples, st.excluded_samples
            );
        }
        // The cost delta is the other half of a comparison, and the half nobody reports.
        let dj = rb.energy_j - ra.energy_j;
        println!(
            "  energy    A {:.3} J   B {:.3} J   delta {:+.3} J ({:+.1} %)",
            ra.energy_j,
            rb.energy_j,
            dj,
            if ra.energy_j > 0.0 {
                dj / ra.energy_j * 100.0
            } else {
                0.0
            }
        );
        Ok(if verdict.reproduced() && trustworthy {
            ExitCode::SUCCESS
        } else {
            ExitCode::from(1)
        })
    }
}

/// A stable seed from the resolved parameters, so a spec digest moves when an input does even
/// when the scenario declares no parameter literally called `seed`.
fn params_seed(params: &[(String, f64)]) -> u64 {
    if let Some((_, v)) = params.iter().find(|(k, _)| k == "seed") {
        return *v as u64;
    }
    let mut h = ferroscope_receipt::Sha256::new();
    for (k, v) in params {
        h.update(k.as_bytes());
        h.update(&v.to_le_bytes());
    }
    u64::from_le_bytes(h.finish()[..8].try_into().unwrap())
}

/// Keep a table column a column. A long case id must not push the numbers out of alignment;
/// `show` prints the full value.
fn ellipsize(s: &str, width: usize) -> String {
    if s.chars().count() <= width {
        return s.to_string();
    }
    let keep = width.saturating_sub(1);
    let mut out: String = s.chars().take(keep).collect();
    out.push('\u{2026}');
    out
}

fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect()
}

fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        format!("panic: {s}")
    } else if let Some(s) = payload.downcast_ref::<String>() {
        format!("panic: {s}")
    } else {
        "panic: (non-string payload)".to_string()
    }
}

// ---------------------------------------------------------------------------
// Flag parsing. Small on purpose: a harness that needs a CLI framework to start
// is a harness that takes a second to print its help.
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Flags {
    scenarios: Vec<String>,
    tags: Vec<String>,
    exclude_tags: Vec<String>,
    cases: Vec<String>,
    outcomes: Vec<String>,
    set: Vec<(String, f64)>,
    params: Vec<Predicate>,
    results: Vec<Predicate>,
    search: Option<String>,
    suite: Option<String>,
    store: Option<String>,
    limit: Option<usize>,
    abs: Option<f64>,
    rel: Option<f64>,
    json: bool,
    positional: Vec<String>,
}

impl Flags {
    fn parse(argv: &[&str]) -> Result<Flags, String> {
        let mut f = Flags::default();
        let mut i = 0;
        while i < argv.len() {
            let a = argv[i];
            let need = |i: usize| -> Result<&str, String> {
                argv.get(i + 1)
                    .copied()
                    .ok_or_else(|| format!("{a} needs a value"))
            };
            match a {
                "--json" => f.json = true,
                "--scenario" => {
                    f.scenarios.push(need(i)?.into());
                    i += 1;
                }
                "--tag" | "-t" => {
                    f.tags.push(need(i)?.into());
                    i += 1;
                }
                "--exclude-tag" => {
                    f.exclude_tags.push(need(i)?.into());
                    i += 1;
                }
                "--case" => {
                    f.cases.push(need(i)?.into());
                    i += 1;
                }
                "--outcome" => {
                    f.outcomes.push(need(i)?.into());
                    i += 1;
                }
                "--suite" => {
                    f.suite = Some(need(i)?.into());
                    i += 1;
                }
                "--store" => {
                    f.store = Some(need(i)?.into());
                    i += 1;
                }
                "-q" | "--search" => {
                    f.search = Some(need(i)?.into());
                    i += 1;
                }
                "--limit" => {
                    f.limit = Some(need(i)?.parse().map_err(|_| "--limit needs a number")?);
                    i += 1;
                }
                "--abs" => {
                    f.abs = Some(need(i)?.parse().map_err(|_| "--abs needs a number")?);
                    i += 1;
                }
                "--rel" => {
                    f.rel = Some(need(i)?.parse().map_err(|_| "--rel needs a number")?);
                    i += 1;
                }
                "--set" => {
                    let kv = need(i)?;
                    let (k, v) = kv
                        .split_once('=')
                        .ok_or_else(|| format!("--set wants key=value, got {kv:?}"))?;
                    f.set.push((
                        k.to_string(),
                        v.parse()
                            .map_err(|_| format!("--set {k} needs a number, got {v:?}"))?,
                    ));
                    i += 1;
                }
                "--param" => {
                    f.params.push(Predicate::parse(need(i)?)?);
                    i += 1;
                }
                "--result" => {
                    f.results.push(Predicate::parse(need(i)?)?);
                    i += 1;
                }
                other if other.starts_with('-') => {
                    return Err(format!("unknown flag {other}"));
                }
                other => f.positional.push(other.to_string()),
            }
            i += 1;
        }
        Ok(f)
    }

    fn selection(&self) -> Selection {
        Selection {
            scenarios: self.scenarios.clone(),
            tags: self.tags.clone(),
            exclude_tags: self.exclude_tags.clone(),
            cases: self.cases.clone(),
            set: self.set.clone(),
        }
    }
}
