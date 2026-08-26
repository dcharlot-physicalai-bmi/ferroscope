//! A scene with cases and checks: the scenario, not the single run.
//!
//! One recording answers "what happened". A scenario answers "does it *still* hold, across the
//! range I care about" — which is the question that decides whether something ships. So a scene
//! may declare `cases` (a grid of parameters) and `checks` (bounds on what the recording
//! measures), and every case produces its own recording, its own receipt, and its own verdict.
//!
//! ```
//! use ferroscope_scene::Suite;
//!
//! let s = Suite::parse(r#"{
//!   "name": "how high can it fall",
//!   "cases": { "h": [1.0, 2.0, 3.0] },
//!   "bodies": [
//!     { "id": "crate", "shape": "box", "size": [0.3, 0.3, 0.3],
//!       "motion": { "kind": "fall", "from": [0, 0, {"$": "h"}] } }
//!   ],
//!   "checks": [
//!     { "name": "stays above the floor", "measure": "lowest_point", "at_least": -0.001 }
//!   ]
//! }"#).unwrap();
//!
//! assert_eq!(s.cases.len(), 3);
//! let results = s.run(|_| None).unwrap();
//! assert!(results.iter().all(|r| r.passed()));
//! ```
//!
//! The parameter is written `{"$": "h"}` anywhere a number belongs, which keeps the template a
//! valid JSON document that an editor can still check — a scene with `"from": [0, 0, "$h"]` would
//! have to lie about its own types to say the same thing.

use crate::{Problem, Recorded, Scene};
use ferroscope_schema::json::{self, Value};

/// What a check can look at. Deliberately short: everything here is something the recording
/// actually measures, so a check can never assert about something nobody wrote down.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Measure {
    /// The lowest point anything reached, in metres. Below zero means it went through the ground.
    LowestPoint,
    /// E_task for the case, in joules.
    TotalJ,
    /// The share of E_task on the compute rail, 0..1.
    ComputeFraction,
    /// How many steps the case recorded.
    Steps,
    /// The last sim time, in seconds, at which a body moved. Scope it with `of`.
    SettledS,
}

impl Measure {
    fn parse(s: &str) -> Option<Measure> {
        match s {
            "lowest_point" => Some(Measure::LowestPoint),
            "total_j" => Some(Measure::TotalJ),
            "compute_fraction" => Some(Measure::ComputeFraction),
            "steps" => Some(Measure::Steps),
            "settled_s" => Some(Measure::SettledS),
            _ => None,
        }
    }
    pub fn name(&self) -> &'static str {
        match self {
            Measure::LowestPoint => "lowest_point",
            Measure::TotalJ => "total_j",
            Measure::ComputeFraction => "compute_fraction",
            Measure::Steps => "steps",
            Measure::SettledS => "settled_s",
        }
    }
    /// Every name, for an error message that has to say what would have worked.
    pub const ALL: &'static [&'static str] = &[
        "lowest_point",
        "total_j",
        "compute_fraction",
        "steps",
        "settled_s",
    ];

    fn read(&self, r: &Recorded, of: Option<&str>) -> f64 {
        match self {
            Measure::LowestPoint => match of {
                Some(id) => r
                    .lowest_by
                    .iter()
                    .find(|(k, _)| k == id)
                    .map(|(_, z)| *z)
                    .unwrap_or(f64::NAN),
                None => r.lowest.as_ref().map(|(z, _)| *z).unwrap_or(f64::NAN),
            },
            Measure::TotalJ => r.total_j,
            Measure::ComputeFraction => r.compute_fraction,
            Measure::Steps => r.steps as f64,
            Measure::SettledS => match of {
                Some(id) => r
                    .settled_by
                    .iter()
                    .find(|(k, _)| k == id)
                    .map(|(_, t)| *t)
                    .unwrap_or(f64::NAN),
                // Unscoped, the honest reading is the LAST body to settle: "everything has come
                // to rest" is what a whole-scene settle time means.
                None => r
                    .settled_by
                    .iter()
                    .map(|(_, t)| *t)
                    .fold(f64::NAN, |a, b| if a.is_nan() { b } else { a.max(b) }),
            },
        }
    }
}

/// One bound on one measure.
#[derive(Clone, Debug, PartialEq)]
pub struct Check {
    pub name: String,
    pub measure: Measure,
    /// Which body or robot this is about. `None` means the scene as a whole.
    ///
    /// Without this a check reads as being about one thing and silently measures another: put a
    /// robot beside a crate and the scene-wide lowest point is the robot's, every time.
    pub of: Option<String>,
    pub at_least: Option<f64>,
    pub at_most: Option<f64>,
}

impl Check {
    /// Judge one recording. Returns whether it held and a sentence saying why.
    pub fn judge(&self, r: &Recorded) -> (bool, String) {
        let v = self.measure.read(r, self.of.as_deref());
        let what = match &self.of {
            Some(id) => format!("{}[{id}]", self.measure.name()),
            None => self.measure.name().to_string(),
        };
        if !v.is_finite() {
            // A measure nothing produced is not a pass. A scene with no ground and no bodies has
            // no lowest point, and silently passing "stays above the floor" would be a lie.
            // Naming the ids that DO exist turns a typo from a silent vacuous pass into a
            // one-line fix.
            let known: Vec<&str> = match self.measure {
                Measure::SettledS => r.settled_by.iter().map(|(k, _)| k.as_str()).collect(),
                _ => r.lowest_by.iter().map(|(k, _)| k.as_str()).collect(),
            };
            return (
                false,
                format!(
                    "{what} was not measured in this run{}",
                    if self.of.is_some() && !known.is_empty() {
                        format!(" (measured here: {})", known.join(", "))
                    } else {
                        String::new()
                    }
                ),
            );
        }
        if let Some(lo) = self.at_least
            && v < lo
        {
            return (false, format!("{what} = {v:.4}, below {lo}"));
        }
        if let Some(hi) = self.at_most
            && v > hi
        {
            return (false, format!("{what} = {v:.4}, above {hi}"));
        }
        (true, format!("{what} = {v:.4}"))
    }
}

/// A scene template, its case grid, and its checks.
#[derive(Clone, Debug)]
pub struct Suite {
    pub name: String,
    /// The scene document with `{"$": "..."}` holes still in it.
    template: Value,
    /// One entry per case: a label and the parameters it binds.
    pub cases: Vec<(String, Vec<(String, f64)>)>,
    pub checks: Vec<Check>,
}

/// One case, recorded and judged.
pub struct CaseResult {
    pub label: String,
    pub set: Vec<(String, f64)>,
    pub recorded: Recorded,
    /// Per-check: name, whether it held, and the sentence explaining it.
    pub checks: Vec<(String, bool, String)>,
}

impl CaseResult {
    pub fn passed(&self) -> bool {
        self.checks.iter().all(|(_, ok, _)| *ok)
    }
}

/// Replace every `{"$": "name"}` with the bound number.
fn substitute(v: &Value, set: &[(String, f64)]) -> Value {
    match v {
        Value::Obj(kv) => {
            // A one-key object whose key is "$" is a hole, not an object.
            if kv.len() == 1
                && kv[0].0 == "$"
                && let Value::Str(name) = &kv[0].1
                && let Some((_, x)) = set.iter().find(|(k, _)| k == name)
            {
                return Value::Num(*x);
            }
            Value::Obj(
                kv.iter()
                    .map(|(k, x)| (k.clone(), substitute(x, set)))
                    .collect(),
            )
        }
        Value::Arr(a) => Value::Arr(a.iter().map(|x| substitute(x, set)).collect()),
        other => other.clone(),
    }
}

/// Every unbound `{"$": "name"}` left in a document, for an error that names them.
fn holes(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::Obj(kv) => {
            if kv.len() == 1
                && kv[0].0 == "$"
                && let Value::Str(name) = &kv[0].1
            {
                if !out.contains(name) {
                    out.push(name.clone());
                }
                return;
            }
            kv.iter().for_each(|(_, x)| holes(x, out));
        }
        Value::Arr(a) => a.iter().for_each(|x| holes(x, out)),
        _ => {}
    }
}

impl Suite {
    /// Read a scene document that may carry `cases` and `checks`.
    ///
    /// A document with neither is a suite of exactly one case, so everything below works the
    /// same whether or not the author thought of it as a scenario.
    pub fn parse(text: &str) -> Result<Suite, Vec<Problem>> {
        let Some(v) = json::parse(text) else {
            return Err(vec![Problem {
                path: "$".into(),
                message: "not valid JSON".into(),
            }]);
        };
        let mut out = Vec::new();
        let name = v
            .get("name")
            .and_then(|x| x.as_str())
            .unwrap_or("scene")
            .to_string();

        // The grid: one axis per named parameter, in authored order.
        let mut axes: Vec<(String, Vec<f64>)> = Vec::new();
        if let Some(Value::Obj(kv)) = v.get("cases") {
            for (k, val) in kv {
                match val {
                    Value::Arr(a) if !a.is_empty() => {
                        let mut vals = Vec::new();
                        for (i, e) in a.iter().enumerate() {
                            match e.as_f64() {
                                Some(x) if x.is_finite() => vals.push(x),
                                _ => out.push(Problem {
                                    path: format!("cases.{k}[{i}]"),
                                    message: "expected a finite number".into(),
                                }),
                            }
                        }
                        axes.push((k.clone(), vals));
                    }
                    Value::Arr(_) => out.push(Problem {
                        path: format!("cases.{k}"),
                        message: "an empty list would produce no cases at all".into(),
                    }),
                    _ => out.push(Problem {
                        path: format!("cases.{k}"),
                        message: "expected a list of numbers, like [1.0, 2.0, 3.0]".into(),
                    }),
                }
            }
        } else if v.get("cases").is_some() {
            out.push(Problem {
                path: "$.cases".into(),
                message: "expected an object of name -> list of numbers".into(),
            });
        }

        // Cartesian product, last axis varying fastest so a table reads in a sensible order.
        let mut cases: Vec<Vec<(String, f64)>> = vec![Vec::new()];
        for (k, vals) in &axes {
            let mut next = Vec::with_capacity(cases.len() * vals.len());
            for base in &cases {
                for x in vals {
                    let mut c = base.clone();
                    c.push((k.clone(), *x));
                    next.push(c);
                }
            }
            cases = next;
        }
        // A grid nobody meant to write: 6 axes of 10 is a million recordings.
        if cases.len() > 256 {
            out.push(Problem {
                path: "$.cases".into(),
                message: format!(
                    "these axes multiply out to {} cases, above the 256 cap; narrow one of them",
                    cases.len()
                ),
            });
        }

        // Every hole must be bound by some axis, or the case would record a literal object where
        // a number belongs and the scene reader would refuse it with a confusing message.
        let mut named = Vec::new();
        holes(&v, &mut named);
        for h in &named {
            if !axes.iter().any(|(k, _)| k == h) {
                out.push(Problem {
                    path: format!("$.cases.{h}"),
                    message: format!(
                        "{{\"$\": {h:?}}} appears in the scene but {h:?} is not in cases; add it, \
                         or replace the hole with a number"
                    ),
                });
            }
        }

        let mut checks = Vec::new();
        match v.get("checks") {
            None | Some(Value::Null) => {}
            Some(Value::Arr(a)) => {
                for (i, c) in a.iter().enumerate() {
                    let path = format!("checks[{i}]");
                    let Some(m) = c.get("measure").and_then(|x| x.as_str()) else {
                        out.push(Problem {
                            path: format!("{path}.measure"),
                            message: format!(
                                "a check needs a measure: one of {}",
                                Measure::ALL.join(", ")
                            ),
                        });
                        continue;
                    };
                    let Some(measure) = Measure::parse(m) else {
                        out.push(Problem {
                            path: format!("{path}.measure"),
                            message: format!(
                                "unknown measure {m:?}; expected one of {}",
                                Measure::ALL.join(", ")
                            ),
                        });
                        continue;
                    };
                    let at_least = c.get("at_least").and_then(|x| x.as_f64());
                    let at_most = c.get("at_most").and_then(|x| x.as_f64());
                    if at_least.is_none() && at_most.is_none() {
                        out.push(Problem {
                            path: path.clone(),
                            message: "a check with no at_least and no at_most cannot fail".into(),
                        });
                        continue;
                    }
                    checks.push(Check {
                        of: c.get("of").and_then(|x| x.as_str()).map(str::to_string),
                        name: c
                            .get("name")
                            .and_then(|x| x.as_str())
                            .unwrap_or(measure.name())
                            .to_string(),
                        measure,
                        at_least,
                        at_most,
                    });
                }
            }
            Some(_) => out.push(Problem {
                path: "$.checks".into(),
                message: "expected an array".into(),
            }),
        }

        // Labels: "h=2, r=0.5", or "1 of 1" when there is no grid.
        let labelled: Vec<(String, Vec<(String, f64)>)> = cases
            .into_iter()
            .enumerate()
            .map(|(i, set)| {
                let label = if set.is_empty() {
                    "default".to_string()
                } else {
                    set.iter()
                        .map(|(k, v)| format!("{k}={}", trim(*v)))
                        .collect::<Vec<_>>()
                        .join(", ")
                };
                let _ = i;
                (label, set)
            })
            .collect();

        // The template itself must be a readable scene once its holes are filled. Checking the
        // first case here means a broken template is reported once, now, rather than once per
        // case in the middle of a sweep.
        if out.is_empty() {
            let first = substitute(&v, &labelled[0].1);
            if let Err(mut e) = Scene::parse(&first.to_json()) {
                out.append(&mut e);
            }
        }

        if out.is_empty() {
            Ok(Suite {
                name,
                template: v,
                cases: labelled,
                checks,
            })
        } else {
            Err(out)
        }
    }

    /// The scene for one case, with its parameters bound.
    pub fn scene(&self, i: usize) -> Result<Scene, Vec<Problem>> {
        let set = &self
            .cases
            .get(i)
            .ok_or_else(|| {
                vec![Problem {
                    path: "$".into(),
                    message: format!("no case {i}"),
                }]
            })?
            .1;
        Scene::parse(&substitute(&self.template, set).to_json())
    }

    /// Record every case and judge it.
    pub fn run<F>(&self, load_urdf: F) -> Result<Vec<CaseResult>, String>
    where
        F: Fn(&str) -> Option<String> + Copy,
    {
        self.run_with(load_urdf, &mut || Vec::new())
    }

    /// [`Suite::run`], with a production note taken per case.
    ///
    /// `production` is called once at each case's seal. Against a cumulative energy counter
    /// that is exactly right: each call's delta is that case's own cost, with nothing dropped
    /// between cases and nothing counted twice.
    pub fn run_with<F>(
        &self,
        load_urdf: F,
        production: &mut dyn FnMut() -> Vec<(String, String)>,
    ) -> Result<Vec<CaseResult>, String>
    where
        F: Fn(&str) -> Option<String> + Copy,
    {
        let mut out = Vec::with_capacity(self.cases.len());
        for (i, (label, set)) in self.cases.iter().enumerate() {
            let scene = self
                .scene(i)
                .map_err(|p| format!("case {label}: {} problem(s)", p.len()))?;
            let recorded = scene.record_with(load_urdf, &mut *production)?;
            let checks = self
                .checks
                .iter()
                .map(|c| {
                    let (ok, why) = c.judge(&recorded);
                    (c.name.clone(), ok, why)
                })
                .collect();
            out.push(CaseResult {
                label: label.clone(),
                set: set.clone(),
                recorded,
                checks,
            });
        }
        Ok(out)
    }
}

fn trim(v: f64) -> String {
    let s = format!("{v:.4}");
    let s = s.trim_end_matches('0').trim_end_matches('.').to_string();
    if s.is_empty() { "0".into() } else { s }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DROP: &str = r#"{
      "name": "how high can it fall",
      "duration_s": 2.0, "rate_hz": 60,
      "cases": { "h": [1.0, 2.0, 3.0] },
      "bodies": [
        { "id": "crate", "shape": "box", "size": [0.3, 0.3, 0.3],
          "motion": { "kind": "fall", "from": [0, 0, {"$": "h"}] } }
      ],
      "checks": [
        { "name": "stays above the floor", "measure": "lowest_point", "at_least": -0.001 }
      ]
    }"#;

    #[test]
    fn a_grid_becomes_one_case_per_combination() {
        let s = Suite::parse(DROP).unwrap();
        assert_eq!(s.cases.len(), 3);
        assert_eq!(s.cases[0].0, "h=1");
        assert_eq!(s.cases[2].0, "h=3");
        // And the hole is really filled: case 2 drops from 3 m, not from a literal object.
        let sc = s.scene(2).unwrap();
        match sc.bodies[0].motion {
            crate::Motion::Fall { from, .. } => assert!((from[2] - 3.0).abs() < 1e-9, "{from:?}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn two_axes_multiply_out() {
        let s = Suite::parse(
            r#"{"cases":{"h":[1,2],"r":[0.1,0.2,0.3]},
                "bodies":[{"id":"a","size":[{"$":"r"},0.2,0.2],
                           "motion":{"kind":"fall","from":[0,0,{"$":"h"}]}}]}"#,
        )
        .unwrap();
        assert_eq!(s.cases.len(), 6);
        // Last axis varies fastest, so a printed table reads in a sensible order.
        assert_eq!(s.cases[0].0, "h=1, r=0.1");
        assert_eq!(s.cases[1].0, "h=1, r=0.2");
        assert_eq!(s.cases[3].0, "h=2, r=0.1");
    }

    #[test]
    fn every_case_records_and_is_judged() {
        let s = Suite::parse(DROP).unwrap();
        let r = s.run(|_| None).unwrap();
        assert_eq!(r.len(), 3);
        for c in &r {
            assert!(c.passed(), "{}: {:?}", c.label, c.checks);
            assert!(c.recorded.bytes.len() > 500);
            // Each case stands behind its own receipt.
            let v = ferroscope_schema::verify(&c.recorded.bytes).unwrap();
            assert!(
                v.spec_matches && v.trace_matches,
                "{} does not verify",
                c.label
            );
        }
        // Different heights must produce different runs, or the parameter did nothing.
        let d: Vec<String> = r
            .iter()
            .map(|c| c.recorded.receipt.trace_digest.clone())
            .collect();
        assert!(
            d[0] != d[1] && d[1] != d[2],
            "the cases are identical: {d:?}"
        );
    }

    #[test]
    fn a_check_scoped_to_a_body_measures_that_body_and_not_the_scene() {
        // The failure this exists for: a robot in the scene dominates the scene-wide minimum,
        // so "the crate stays on the floor" quietly becomes a statement about the arm.
        let doc = r#"{"duration_s":0.6,"rate_hz":30,
            "bodies":[{"id":"crate","shape":"box","size":[0.2,0.2,0.2],
                       "motion":{"kind":"static","at":[0,0,0.1]}},
                      {"id":"sinker","shape":"box","size":[0.2,0.2,0.2],
                       "motion":{"kind":"static","at":[0,0,-3]}}],
            "checks":[{"name":"crate","measure":"lowest_point","of":"crate","at_least":-0.001},
                      {"name":"scene","measure":"lowest_point","at_least":-0.001}]}"#;
        let r = Suite::parse(doc).unwrap().run(|_| None).unwrap();
        let by = |n: &str| r[0].checks.iter().find(|(k, _, _)| k == n).unwrap();
        assert!(
            by("crate").1,
            "the crate sits at 0 and must pass: {:?}",
            by("crate")
        );
        assert!(
            !by("scene").1,
            "the scene minimum is the sinker and must fail"
        );
        assert!(
            by("crate").2.contains("lowest_point[crate]"),
            "{}",
            by("crate").2
        );
    }

    #[test]
    fn settle_time_varies_with_the_parameter_being_swept() {
        // A scenario whose cases all report the same number is a scenario that cannot tell you
        // anything. Drop height and bounce genuinely change how long a crate takes to stop.
        let s = Suite::parse(
            r#"{"duration_s":4.0,"rate_hz":120,
                "cases":{"h":[0.4,4.0]},
                "bodies":[{"id":"crate","shape":"box","size":[0.3,0.3,0.3],
                           "motion":{"kind":"fall","from":[0,0,{"$":"h"}],"restitution":0.6}}],
                "checks":[{"name":"settles","measure":"settled_s","of":"crate","at_most":2.0}]}"#,
        )
        .unwrap();
        let r = s.run(|_| None).unwrap();
        let t = |i: usize| {
            r[i].recorded
                .settled_by
                .iter()
                .find(|(k, _)| k == "crate")
                .unwrap()
                .1
        };
        assert!(
            t(0) < t(1),
            "a 4 m drop must take longer to settle than 0.4 m: {} vs {}",
            t(0),
            t(1)
        );
        assert!(r[0].passed(), "0.4 m settles quickly: {:?}", r[0].checks);
        assert!(
            !r[1].passed(),
            "4 m at 0.6 restitution must still be moving at 2 s"
        );
    }

    #[test]
    fn a_check_naming_a_body_that_is_not_there_fails_and_lists_the_real_ones() {
        let r = Suite::parse(
            r#"{"duration_s":0.4,"rate_hz":20,
                "bodies":[{"id":"crate","motion":{"kind":"static"}}],
                "checks":[{"name":"typo","measure":"lowest_point","of":"crat","at_least":0}]}"#,
        )
        .unwrap()
        .run(|_| None)
        .unwrap();
        assert!(
            !r[0].passed(),
            "a check about a body that is not there must not pass"
        );
        assert!(r[0].checks[0].2.contains("crate"), "{}", r[0].checks[0].2);
    }

    #[test]
    fn a_check_that_fails_says_the_number_and_the_bound() {
        let s = Suite::parse(
            r#"{"duration_s":1.0,"rate_hz":60,
                "bodies":[{"id":"a","shape":"box","size":[0.2,0.2,0.2],
                           "motion":{"kind":"static","at":[0,0,-5]}}],
                "checks":[{"name":"above the floor","measure":"lowest_point","at_least":0}]}"#,
        )
        .unwrap();
        let r = s.run(|_| None).unwrap();
        assert!(!r[0].passed());
        let (_, _, why) = &r[0].checks[0];
        assert!(
            why.contains("lowest_point") && why.contains("below 0"),
            "{why}"
        );
    }

    #[test]
    fn an_unbound_hole_is_refused_by_name() {
        let e = Suite::parse(
            r#"{"cases":{"h":[1]},
                "bodies":[{"id":"a","motion":{"kind":"fall","from":[0,0,{"$":"height"}]}}]}"#,
        )
        .unwrap_err();
        let m = &e
            .iter()
            .find(|p| p.path.contains("height"))
            .unwrap()
            .message;
        assert!(m.contains("not in cases"), "{m}");
    }

    #[test]
    fn an_unknown_measure_lists_the_real_ones() {
        let e = Suite::parse(
            r#"{"bodies":[{"id":"a","motion":{"kind":"static"}}],
                "checks":[{"measure":"temperature","at_most":100}]}"#,
        )
        .unwrap_err();
        let m = &e
            .iter()
            .find(|p| p.path.contains("measure"))
            .unwrap()
            .message;
        for want in Measure::ALL {
            assert!(m.contains(want), "{m} should list {want}");
        }
    }

    #[test]
    fn a_check_with_no_bound_is_refused_because_it_cannot_fail() {
        let e = Suite::parse(
            r#"{"bodies":[{"id":"a","motion":{"kind":"static"}}],
                "checks":[{"measure":"total_j"}]}"#,
        )
        .unwrap_err();
        assert!(e.iter().any(|p| p.message.contains("cannot fail")), "{e:?}");
    }

    #[test]
    fn a_measure_nothing_produced_fails_rather_than_passes_vacuously() {
        // No ground and no bodies with extent: lowest_point is NaN, and "stays above the floor"
        // must NOT pass on a number nobody measured.
        let s = Suite::parse(
            r#"{"ground":null,"duration_s":0.2,"rate_hz":10,
                "robots":[{"id":"r","urdf":"missing.urdf"}],
                "checks":[{"name":"floor","measure":"lowest_point","at_least":0}]}"#,
        )
        .unwrap();
        let r = s.run(|_| None).unwrap();
        assert!(!r[0].passed(), "an unmeasured check must not pass");
        assert!(
            r[0].checks[0].2.contains("not measured"),
            "{:?}",
            r[0].checks
        );
    }

    #[test]
    fn a_scene_with_no_cases_is_a_suite_of_one() {
        let s = Suite::parse(
            r#"{"duration_s":0.5,"rate_hz":20,"bodies":[{"id":"a","motion":{"kind":"static"}}]}"#,
        )
        .unwrap();
        assert_eq!(s.cases.len(), 1);
        assert_eq!(s.cases[0].0, "default");
        assert_eq!(s.run(|_| None).unwrap().len(), 1);
    }

    #[test]
    fn an_unbounded_grid_is_refused_with_the_number() {
        let e = Suite::parse(
            r#"{"cases":{"a":[1,2,3,4,5,6,7,8,9,10],"b":[1,2,3,4,5,6,7,8,9,10],
                         "c":[1,2,3,4,5,6,7,8,9,10]},
                "bodies":[{"id":"x","motion":{"kind":"static"}}]}"#,
        )
        .unwrap_err();
        let m = &e
            .iter()
            .find(|p| p.message.contains("cap"))
            .unwrap()
            .message;
        assert!(m.contains("1000"), "{m}");
    }
}
