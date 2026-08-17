//! Run history as a directory.
//!
//! Antioch's run history is a shared server: durable, queryable, and unreachable without an
//! account, an organization, and a network. The information in it is a few hundred bytes per
//! run beside the recording. So here it is a directory:
//!
//! ```text
//! .ferroscope/
//!   <run-id>/
//!     run.json      the record: inputs, checks, results, digests, joules, timings
//!     run.mcap      the recording, receipt sealed inside it
//!     <artifact>    anything the body attached
//! ```
//!
//! One consequence is worth stating plainly: **`ferroscope list` works on an aeroplane.**
//! Another is that the history is yours: `git add`, `rsync`, or delete it. Query predicates
//! are ported from Antioch's `key:op:value` grammar because that grammar is good.

use std::fs;
use std::path::{Path, PathBuf};

use ferroscope_schema::json::{self, Value};

use crate::run::{Check, Outcome};

/// The default history directory, relative to the working directory.
pub const DEFAULT_ROOT: &str = ".ferroscope";

/// One finished run, as stored.
#[derive(Clone, Debug, PartialEq)]
pub struct Record {
    pub id: String,
    pub scenario: String,
    pub case: String,
    pub tags: Vec<String>,
    pub outcome: Outcome,
    /// Why it halted, when it halted.
    pub reason: String,
    pub params: Vec<(String, f64)>,
    pub checks: Vec<Check>,
    pub results: Vec<(String, Value)>,
    pub artifacts: Vec<String>,

    pub spec_digest: String,
    pub trace_digest: String,
    /// The receipt recomputed from the stored recording at write time.
    pub verified: bool,

    pub energy_j: f64,
    pub compute_j: f64,
    pub actuation_j: f64,
    /// `false` when the sampling could not support the energy figure.
    pub quotable: bool,

    pub steps: u64,
    pub sim_s: f64,
    pub wall_s: f64,
    pub started_us: u64,
    pub platform: String,
    pub mcap_bytes: u64,
}

impl Record {
    /// Simulated seconds per wall second. Below 1.0 means the run is slower than real time.
    pub fn real_time_factor(&self) -> f64 {
        if self.wall_s > 0.0 {
            self.sim_s / self.wall_s
        } else {
            0.0
        }
    }

    /// Joules per passing check: the cost of one unit of evidence.
    ///
    /// Antioch's own documentation states the gap this fills: machine time is
    /// assignment-scoped, "idle time included, so many runs can share it and **there is no
    /// per-run or per-scenario cost figure to report**". Every record here has one.
    pub fn joules_per_pass(&self) -> Option<f64> {
        let passes = self.checks.iter().filter(|c| c.passed).count();
        if passes == 0 {
            None
        } else {
            Some(self.energy_j / passes as f64)
        }
    }

    pub fn failed_checks(&self) -> impl Iterator<Item = &Check> {
        self.checks.iter().filter(|c| !c.passed)
    }

    pub fn to_value(&self) -> Value {
        let pairs = |v: &[(String, f64)]| {
            Value::Obj(v.iter().map(|(k, x)| (k.clone(), Value::Num(*x))).collect())
        };
        Value::Obj(vec![
            ("format".into(), Value::Str("ferroscope.record.v1".into())),
            ("id".into(), Value::Str(self.id.clone())),
            ("scenario".into(), Value::Str(self.scenario.clone())),
            ("case".into(), Value::Str(self.case.clone())),
            (
                "tags".into(),
                Value::Arr(self.tags.iter().cloned().map(Value::Str).collect()),
            ),
            (
                "outcome".into(),
                Value::Str(self.outcome.as_str().to_string()),
            ),
            ("reason".into(), Value::Str(self.reason.clone())),
            ("params".into(), pairs(&self.params)),
            (
                "checks".into(),
                Value::Arr(
                    self.checks
                        .iter()
                        .map(|c| {
                            Value::Obj(vec![
                                ("criterion".into(), Value::Str(c.criterion.clone())),
                                ("passed".into(), Value::Bool(c.passed)),
                                ("detail".into(), Value::Str(c.detail.clone())),
                            ])
                        })
                        .collect(),
                ),
            ),
            ("results".into(), Value::Obj(self.results.clone())),
            (
                "artifacts".into(),
                Value::Arr(self.artifacts.iter().cloned().map(Value::Str).collect()),
            ),
            ("spec_digest".into(), Value::Str(self.spec_digest.clone())),
            ("trace_digest".into(), Value::Str(self.trace_digest.clone())),
            ("verified".into(), Value::Bool(self.verified)),
            ("energy_j".into(), Value::Num(self.energy_j)),
            ("compute_j".into(), Value::Num(self.compute_j)),
            ("actuation_j".into(), Value::Num(self.actuation_j)),
            ("quotable".into(), Value::Bool(self.quotable)),
            ("steps".into(), Value::Num(self.steps as f64)),
            ("sim_s".into(), Value::Num(self.sim_s)),
            ("wall_s".into(), Value::Num(self.wall_s)),
            ("started_us".into(), Value::Num(self.started_us as f64)),
            ("platform".into(), Value::Str(self.platform.clone())),
            ("mcap_bytes".into(), Value::Num(self.mcap_bytes as f64)),
        ])
    }

    pub fn from_value(v: &Value) -> Option<Record> {
        let s = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
        let n = |k: &str| v.get(k).and_then(|x| x.as_f64()).unwrap_or(0.0);
        let b = |k: &str| matches!(v.get(k), Some(Value::Bool(true)));
        let strs = |k: &str| -> Vec<String> {
            v.get(k)
                .and_then(|x| x.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|e| e.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default()
        };
        let numpairs = |k: &str| -> Vec<(String, f64)> {
            match v.get(k) {
                Some(Value::Obj(kv)) => kv
                    .iter()
                    .filter_map(|(a, x)| x.as_f64().map(|y| (a.clone(), y)))
                    .collect(),
                _ => Vec::new(),
            }
        };
        Some(Record {
            id: s("id"),
            scenario: s("scenario"),
            case: s("case"),
            tags: strs("tags"),
            outcome: Outcome::parse(&s("outcome"))?,
            reason: s("reason"),
            params: numpairs("params"),
            checks: v
                .get("checks")
                .and_then(|x| x.as_array())
                .map(|a| {
                    a.iter()
                        .map(|c| Check {
                            criterion: c
                                .get("criterion")
                                .and_then(|x| x.as_str())
                                .unwrap_or("")
                                .to_string(),
                            passed: matches!(c.get("passed"), Some(Value::Bool(true))),
                            detail: c
                                .get("detail")
                                .and_then(|x| x.as_str())
                                .unwrap_or("")
                                .to_string(),
                        })
                        .collect()
                })
                .unwrap_or_default(),
            results: match v.get("results") {
                Some(Value::Obj(kv)) => kv.clone(),
                _ => Vec::new(),
            },
            artifacts: strs("artifacts"),
            spec_digest: s("spec_digest"),
            trace_digest: s("trace_digest"),
            verified: b("verified"),
            energy_j: n("energy_j"),
            compute_j: n("compute_j"),
            actuation_j: n("actuation_j"),
            quotable: b("quotable"),
            steps: n("steps") as u64,
            sim_s: n("sim_s"),
            wall_s: n("wall_s"),
            started_us: n("started_us") as u64,
            platform: s("platform"),
            mcap_bytes: n("mcap_bytes") as u64,
        })
    }
}

// ---------------------------------------------------------------------------
// Predicates
// ---------------------------------------------------------------------------

/// The comparison operators, ported verbatim from Antioch's `key:op:value` filter grammar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    /// `=` typed equality; containment when the stored value is an array.
    Eq,
    /// `~` case-insensitive substring.
    Like,
    /// `<` numeric ordering.
    Lt,
    /// `>` numeric ordering.
    Gt,
    /// `@` containment along a dotted path. Only this operator traverses dots; for every
    /// other operator a dotted key is literal, exactly as Antioch specifies.
    Path,
}

/// One `key:op:value` predicate.
#[derive(Clone, Debug)]
pub struct Predicate {
    pub key: String,
    pub op: Op,
    pub value: String,
}

impl Predicate {
    /// Parse `key:op:value`. The value may itself contain colons.
    pub fn parse(spec: &str) -> Result<Predicate, String> {
        let mut it = spec.splitn(3, ':');
        let key = it.next().unwrap_or("").to_string();
        let op = it.next().unwrap_or("");
        let value = it.next().unwrap_or("").to_string();
        if key.is_empty() || op.is_empty() {
            return Err(format!(
                "predicate {spec:?} is not key:op:value (ops: = ~ < > @)"
            ));
        }
        let op = match op {
            "=" => Op::Eq,
            "~" => Op::Like,
            "<" => Op::Lt,
            ">" => Op::Gt,
            "@" => Op::Path,
            other => {
                return Err(format!(
                    "unknown predicate operator {other:?} (use = ~ < > @)"
                ))
            }
        };
        Ok(Predicate { key, op, value })
    }

    fn matches_value(&self, found: Option<&Value>) -> bool {
        let Some(found) = found else { return false };
        match self.op {
            Op::Eq => match found {
                Value::Arr(a) => a.iter().any(|e| e.brief() == self.value),
                other => other.brief() == self.value,
            },
            Op::Like => found
                .brief()
                .to_lowercase()
                .contains(&self.value.to_lowercase()),
            Op::Lt => match (found.as_f64(), self.value.parse::<f64>()) {
                (Some(a), Ok(b)) => a < b,
                _ => false,
            },
            Op::Gt => match (found.as_f64(), self.value.parse::<f64>()) {
                (Some(a), Ok(b)) => a > b,
                _ => false,
            },
            Op::Path => false, // handled by the caller, which owns the root value
        }
    }

    /// Apply to a `results`-shaped map.
    pub fn matches_map(&self, map: &[(String, Value)]) -> bool {
        if self.op == Op::Path {
            let root = Value::Obj(map.to_vec());
            return match root.path(&self.key) {
                Some(Value::Arr(a)) => a.iter().any(|e| e.brief() == self.value),
                Some(other) => other.brief().contains(&self.value),
                None => false,
            };
        }
        let found = map.iter().find(|(k, _)| *k == self.key).map(|(_, v)| v);
        self.matches_value(found)
    }

    /// Apply to a numeric parameter map.
    pub fn matches_params(&self, params: &[(String, f64)]) -> bool {
        let owned: Vec<(String, Value)> = params
            .iter()
            .map(|(k, v)| (k.clone(), Value::Num(*v)))
            .collect();
        self.matches_map(&owned)
    }
}

/// A history query. Every field narrows; repeated values within a field are a union.
#[derive(Clone, Debug, Default)]
pub struct Query {
    pub scenario: Vec<String>,
    pub case: Vec<String>,
    pub tag: Vec<String>,
    pub exclude_tag: Vec<String>,
    pub outcome: Vec<Outcome>,
    pub search: Option<String>,
    /// Only runs started at or after this many microseconds since the epoch.
    pub since_us: Option<u64>,
    pub until_us: Option<u64>,
    pub params: Vec<Predicate>,
    pub results: Vec<Predicate>,
    pub limit: usize,
}

impl Query {
    pub fn new() -> Query {
        Query {
            limit: 50,
            ..Default::default()
        }
    }

    pub fn matches(&self, r: &Record) -> bool {
        if !self.scenario.is_empty() && !self.scenario.contains(&r.scenario) {
            return false;
        }
        if !self.case.is_empty() && !self.case.contains(&r.case) {
            return false;
        }
        if !self.tag.is_empty() && !self.tag.iter().any(|t| r.tags.contains(t)) {
            return false;
        }
        if self.exclude_tag.iter().any(|t| r.tags.contains(t)) {
            return false;
        }
        if !self.outcome.is_empty() && !self.outcome.contains(&r.outcome) {
            return false;
        }
        if let Some(q) = &self.search {
            if !r.scenario.to_lowercase().contains(&q.to_lowercase()) {
                return false;
            }
        }
        if let Some(t) = self.since_us {
            if r.started_us < t {
                return false;
            }
        }
        if let Some(t) = self.until_us {
            if r.started_us > t {
                return false;
            }
        }
        if !self.params.iter().all(|p| p.matches_params(&r.params)) {
            return false;
        }
        if !self.results.iter().all(|p| p.matches_map(&r.results)) {
            return false;
        }
        true
    }
}

// ---------------------------------------------------------------------------
// Store
// ---------------------------------------------------------------------------

/// A run-history directory.
pub struct Store {
    root: PathBuf,
}

impl Store {
    pub fn open(root: impl Into<PathBuf>) -> std::io::Result<Store> {
        let root = root.into();
        fs::create_dir_all(&root)?;
        Ok(Store { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn dir_of(&self, id: &str) -> PathBuf {
        self.root.join(id)
    }

    /// Write one run: the record, the recording, and any attached artifacts.
    pub fn put(
        &self,
        rec: &Record,
        mcap: &[u8],
        artifacts: &[(String, PathBuf)],
    ) -> std::io::Result<PathBuf> {
        let dir = self.dir_of(&rec.id);
        fs::create_dir_all(&dir)?;
        fs::write(dir.join("run.mcap"), mcap)?;
        fs::write(dir.join("run.json"), rec.to_value().to_json())?;
        for (name, src) in artifacts {
            // Copy rather than reference: an artifact that lives somewhere else is an
            // artifact that goes missing.
            if src.is_file() {
                let _ = fs::copy(src, dir.join(name));
            }
        }
        Ok(dir)
    }

    /// Load one record by id.
    pub fn get(&self, id: &str) -> Option<Record> {
        let text = fs::read_to_string(self.dir_of(id).join("run.json")).ok()?;
        Record::from_value(&json::parse(&text)?)
    }

    /// The recording bytes for one run.
    pub fn recording(&self, id: &str) -> Option<Vec<u8>> {
        fs::read(self.dir_of(id).join("run.mcap")).ok()
    }

    /// Every record, newest first, filtered and limited.
    pub fn list(&self, q: &Query) -> Vec<Record> {
        let mut out: Vec<Record> = Vec::new();
        let Ok(entries) = fs::read_dir(&self.root) else {
            return out;
        };
        for e in entries.flatten() {
            if !e.path().is_dir() {
                continue;
            }
            let Ok(text) = fs::read_to_string(e.path().join("run.json")) else {
                continue;
            };
            let Some(v) = json::parse(&text) else {
                continue;
            };
            let Some(r) = Record::from_value(&v) else {
                continue;
            };
            if q.matches(&r) {
                out.push(r);
            }
        }
        out.sort_by_key(|r| std::cmp::Reverse(r.started_us));
        if q.limit > 0 {
            out.truncate(q.limit);
        }
        out
    }

    /// Delete one run's directory.
    pub fn remove(&self, id: &str) -> std::io::Result<()> {
        fs::remove_dir_all(self.dir_of(id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(id: &str, scenario: &str, outcome: Outcome, started: u64) -> Record {
        Record {
            id: id.into(),
            scenario: scenario.into(),
            case: "nominal".into(),
            tags: vec!["smoke".into()],
            outcome,
            reason: String::new(),
            params: vec![("seed".into(), 42.0)],
            checks: vec![
                Check {
                    criterion: "upright".into(),
                    passed: true,
                    detail: "1.2 <= 5.0".into(),
                },
                Check {
                    criterion: "seated".into(),
                    passed: outcome != Outcome::Failed,
                    detail: "0.4 <= 1.0".into(),
                },
            ],
            results: vec![
                ("final_z".into(), Value::Num(0.31)),
                (
                    "grasp".into(),
                    Value::Obj(vec![("quality".into(), Value::Str("pass".into()))]),
                ),
            ],
            artifacts: vec![],
            spec_digest: "aaaa".into(),
            trace_digest: "bbbb".into(),
            verified: true,
            energy_j: 34.9,
            compute_j: 8.4,
            actuation_j: 25.1,
            quotable: true,
            steps: 1000,
            sim_s: 1.0,
            wall_s: 0.02,
            started_us: started,
            platform: "test".into(),
            mcap_bytes: 1234,
        }
    }

    #[test]
    fn a_record_round_trips_through_json() {
        let r = rec("r1", "hop", Outcome::Passed, 1000);
        let back = Record::from_value(&json::parse(&r.to_value().to_json()).unwrap()).unwrap();
        assert_eq!(back, r);
    }

    #[test]
    fn the_store_is_a_directory_and_lists_newest_first() {
        let dir = std::env::temp_dir().join(format!("fsrun-store-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let s = Store::open(&dir).unwrap();
        s.put(&rec("a", "hop", Outcome::Passed, 100), b"x", &[])
            .unwrap();
        s.put(&rec("b", "hop", Outcome::Failed, 300), b"y", &[])
            .unwrap();
        s.put(&rec("c", "walk", Outcome::Passed, 200), b"z", &[])
            .unwrap();

        let all = s.list(&Query::new());
        assert_eq!(
            all.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            ["b", "c", "a"]
        );
        assert_eq!(s.get("b").unwrap().outcome, Outcome::Failed);
        assert_eq!(s.recording("c").unwrap(), b"z");

        let mut q = Query::new();
        q.scenario = vec!["hop".into()];
        assert_eq!(s.list(&q).len(), 2);

        let mut q = Query::new();
        q.outcome = vec![Outcome::Failed];
        assert_eq!(s.list(&q)[0].id, "b");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn predicates_follow_the_documented_grammar() {
        let r = rec("a", "hop", Outcome::Passed, 100);

        // = typed equality on a parameter
        assert!(Predicate::parse("seed:=:42")
            .unwrap()
            .matches_params(&r.params));
        assert!(!Predicate::parse("seed:=:7")
            .unwrap()
            .matches_params(&r.params));

        // < and > numeric ordering on a result
        assert!(Predicate::parse("final_z:<:0.4")
            .unwrap()
            .matches_map(&r.results));
        assert!(!Predicate::parse("final_z:>:0.4")
            .unwrap()
            .matches_map(&r.results));

        // ~ case-insensitive substring
        let mut with_label = r.clone();
        with_label
            .results
            .push(("label".into(), Value::Str("Warehouse Aisle".into())));
        assert!(Predicate::parse("label:~:warehouse")
            .unwrap()
            .matches_map(&with_label.results));

        // @ traverses a dotted path; every other operator treats the dot as literal.
        assert!(Predicate::parse("grasp.quality:@:pass")
            .unwrap()
            .matches_map(&r.results));
        assert!(
            !Predicate::parse("grasp.quality:=:pass")
                .unwrap()
                .matches_map(&r.results),
            "a dotted key must be literal for =, per the documented grammar"
        );
    }

    #[test]
    fn a_malformed_predicate_is_refused_with_the_operator_list() {
        assert!(Predicate::parse("seed").is_err());
        let e = Predicate::parse("seed:!:1").unwrap_err();
        assert!(e.contains("= ~ < > @"), "got {e}");
    }

    #[test]
    fn cost_per_unit_of_evidence_is_reported_and_undefined_at_zero() {
        let mut r = rec("a", "hop", Outcome::Passed, 100);
        // Two passing checks over 34.9 J.
        let jpp = r.joules_per_pass().unwrap();
        assert!((jpp - 34.9 / 2.0).abs() < 1e-9);
        for c in r.checks.iter_mut() {
            c.passed = false;
        }
        assert_eq!(r.joules_per_pass(), None);
    }

    #[test]
    fn the_real_time_factor_comes_out_of_the_three_clocks() {
        let r = rec("a", "hop", Outcome::Passed, 100);
        assert!((r.real_time_factor() - 50.0).abs() < 1e-9);
    }
}
