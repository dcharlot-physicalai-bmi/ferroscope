//! Describe a scene; get a recording.
//!
//! A scene is JSON: some bodies, how each of them moves, and optionally a robot from its own
//! URDF. [`Scene::parse`] reads it, [`Scene::record`] plays it out and writes plain MCAP with a
//! determinism receipt and an energy ledger, exactly like every other Ferroscope recording.
//!
//! ```
//! use ferroscope_scene::Scene;
//!
//! let s = Scene::parse(r#"{
//!   "name": "one falling crate",
//!   "duration_s": 2.0,
//!   "bodies": [
//!     { "id": "crate", "shape": "box", "size": [0.3, 0.3, 0.3],
//!       "motion": { "kind": "fall", "from": [0, 0, 2.0] } }
//!   ]
//! }"#).unwrap();
//!
//! assert_eq!(s.bodies.len(), 1);
//! let out = s.record(|_| None).unwrap();
//! assert!(out.bytes.len() > 1000);
//! ```
//!
//! # Why this exists in a format an agent writes
//!
//! The rest of Ferroscope reads files a simulator produced. This goes the other way: it is the
//! surface an agent uses to *say what it wants to see* and get back something with a receipt on
//! it. Which is why the errors matter more than usual — every refusal below names the JSON path
//! that was wrong and what would have been right, because the thing reading the error is the
//! thing that has to fix it.

#![forbid(unsafe_code)]

use ferroscope_ledger::Rail;
use ferroscope_receipt::{Precision, Receipt, RunSpec};
use ferroscope_schema::json::{self, Value};
use ferroscope_schema::{Geometry, Recorder, Shape, Stamp};

mod motion;
mod suite;
pub use motion::Motion;
pub use suite::{CaseResult, Check, Measure, Suite};

/// What was wrong, and where.
#[derive(Clone, Debug, PartialEq)]
pub struct Problem {
    /// A JSON path into the document, e.g. `bodies[1].motion.radius`.
    pub path: String,
    pub message: String,
}

impl std::fmt::Display for Problem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.path, self.message)
    }
}

/// One thing in the scene.
#[derive(Clone, Debug, PartialEq)]
pub struct Body {
    pub id: String,
    pub shape: Shape,
    /// Box full extents, cylinder `[r, r, length]`, sphere three semi-axes.
    pub size: [f64; 3],
    pub color: [f64; 4],
    /// A material id from the CadFuture table, carried through to the recording so a reader can
    /// look up what this is made of. Not interpreted here.
    pub material: Option<String>,
    pub motion: Motion,
}

/// A robot, from its own description.
#[derive(Clone, Debug, PartialEq)]
pub struct RobotRef {
    pub id: String,
    /// The URDF path as written in the scene. Resolved by the caller's loader, not by this
    /// crate: what a path means depends on who is asking, and a scene format that reads the
    /// filesystem by itself is a scene format that cannot be run in a sandbox.
    pub urdf: String,
    pub at: [f64; 3],
    /// `each` drives one joint at a time; `all` drives them together.
    pub sweep_each: bool,
}

/// A described scene.
#[derive(Clone, Debug, PartialEq)]
pub struct Scene {
    pub name: String,
    pub duration_s: f64,
    pub rate_hz: f64,
    pub gravity: f64,
    /// Ground plane extents, or `None` for no ground.
    pub ground: Option<[f64; 2]>,
    pub bodies: Vec<Body>,
    pub robots: Vec<RobotRef>,
}

impl Default for Scene {
    fn default() -> Self {
        Scene {
            name: "scene".into(),
            duration_s: 4.0,
            rate_hz: 120.0,
            gravity: -9.81,
            ground: Some([6.0, 6.0]),
            bodies: Vec::new(),
            robots: Vec::new(),
        }
    }
}

/// Everything a caller needs after recording, without re-reading the file.
pub struct Recorded {
    pub bytes: Vec<u8>,
    pub receipt: Receipt,
    pub total_j: f64,
    pub compute_fraction: f64,
    pub steps: u64,
    /// The lowest point any body reached, and which one. `None` when nothing was measurable.
    pub lowest: Option<(f64, String)>,
    /// The last sim time, in seconds, at which each body actually moved.
    ///
    /// "Does it settle before the run ends" is the question a drop test is really asking, and it
    /// is one of the few things here that varies with the parameters people sweep.
    pub settled_by: Vec<(String, f64)>,
    /// The lowest point each body and robot reached, by id.
    ///
    /// The scene-wide minimum is usually the wrong thing to assert on: put a robot in the scene
    /// and it dominates, so a check named "the crate stays on the floor" quietly becomes a check
    /// about the arm. Scoping a check to a body is what makes it mean what it says.
    pub lowest_by: Vec<(String, f64)>,
    /// Notes worth showing the caller: things that are legal but probably not intended.
    pub notes: Vec<String>,
}

/// Keep the running minimum for one id.
fn note_lowest(acc: &mut Vec<(String, f64)>, id: &str, z: f64) {
    match acc.iter_mut().find(|(k, _)| k == id) {
        Some((_, w)) if z < *w => *w = z,
        Some(_) => {}
        None => acc.push((id.to_string(), z)),
    }
}

// ---------------------------------------------------------------------------
// Reading
// ---------------------------------------------------------------------------

fn want_obj<'a>(
    v: &'a Value,
    path: &str,
    out: &mut Vec<Problem>,
) -> Option<&'a Vec<(String, Value)>> {
    match v {
        Value::Obj(kv) => Some(kv),
        _ => {
            out.push(Problem {
                path: path.into(),
                message: "expected an object".into(),
            });
            None
        }
    }
}

fn num(v: &Value, path: &str, key: &str, default: f64, out: &mut Vec<Problem>) -> f64 {
    match v.get(key) {
        None => default,
        Some(Value::Num(n)) if n.is_finite() => *n,
        Some(Value::Num(_)) => {
            out.push(Problem {
                path: format!("{path}.{key}"),
                message: "must be a finite number".into(),
            });
            default
        }
        Some(_) => {
            out.push(Problem {
                path: format!("{path}.{key}"),
                message: "expected a number".into(),
            });
            default
        }
    }
}

pub(crate) fn vec3(
    v: &Value,
    path: &str,
    key: &str,
    default: [f64; 3],
    out: &mut Vec<Problem>,
) -> [f64; 3] {
    match v.get(key) {
        None => default,
        Some(Value::Arr(a)) if a.len() == 3 => {
            let mut r = [0.0; 3];
            for (i, e) in a.iter().enumerate() {
                match e {
                    Value::Num(n) if n.is_finite() => r[i] = *n,
                    _ => out.push(Problem {
                        path: format!("{path}.{key}[{i}]"),
                        message: "expected a finite number".into(),
                    }),
                }
            }
            r
        }
        Some(Value::Arr(a)) => {
            out.push(Problem {
                path: format!("{path}.{key}"),
                message: format!("expected 3 numbers, found {}", a.len()),
            });
            default
        }
        Some(_) => {
            out.push(Problem {
                path: format!("{path}.{key}"),
                message: "expected an array of 3 numbers, like [0, 0, 1]".into(),
            });
            default
        }
    }
}

/// `#rrggbb`, `#rrggbbaa`, or four numbers in 0..1.
fn color(v: &Value, path: &str, out: &mut Vec<Problem>) -> [f64; 4] {
    const DEFAULT: [f64; 4] = [0.62, 0.66, 0.75, 1.0];
    match v.get("color") {
        None => DEFAULT,
        Some(Value::Str(s)) => {
            let h = s.trim_start_matches('#');
            if (h.len() == 6 || h.len() == 8) && h.chars().all(|c| c.is_ascii_hexdigit()) {
                let b = |i: usize| {
                    u8::from_str_radix(&h[i * 2..i * 2 + 2], 16).unwrap_or(0) as f64 / 255.0
                };
                [b(0), b(1), b(2), if h.len() == 8 { b(3) } else { 1.0 }]
            } else {
                out.push(Problem {
                    path: format!("{path}.color"),
                    message: format!("{s:?} is not a hex colour; expected \"#rrggbb\""),
                });
                DEFAULT
            }
        }
        Some(Value::Arr(a)) if a.len() == 4 => {
            let mut r = DEFAULT;
            for (i, e) in a.iter().enumerate() {
                if let Value::Num(n) = e {
                    r[i] = n.clamp(0.0, 1.0);
                }
            }
            r
        }
        Some(_) => {
            out.push(Problem {
                path: format!("{path}.color"),
                message: "expected \"#rrggbb\" or four numbers in 0..1".into(),
            });
            DEFAULT
        }
    }
}

fn shape(v: &Value, path: &str, out: &mut Vec<Problem>) -> Shape {
    match v.get("shape").and_then(|s| s.as_str()) {
        Some("box") | None => Shape::Box,
        Some("sphere") => Shape::Sphere,
        Some("cylinder") => Shape::Cylinder,
        Some("plane") => Shape::Plane,
        Some(other) => {
            out.push(Problem {
                path: format!("{path}.shape"),
                message: format!(
                    "unknown shape {other:?}; expected one of box, sphere, cylinder, plane"
                ),
            });
            Shape::Box
        }
    }
}

impl Scene {
    /// The tool description an agent reads before writing one of these.
    pub const SCHEMA: &'static str = include_str!("schema.json");

    /// Read a scene, or return every problem found rather than only the first.
    ///
    /// Every problem at once, deliberately: the caller is usually a model that will rewrite the
    /// whole document, and making it discover five mistakes across five round trips is five
    /// times the work for no more information.
    pub fn parse(text: &str) -> Result<Scene, Vec<Problem>> {
        let Some(v) = json::parse(text) else {
            return Err(vec![Problem {
                path: "$".into(),
                message: "not valid JSON".into(),
            }]);
        };
        let mut out = Vec::new();
        if want_obj(&v, "$", &mut out).is_none() {
            return Err(out);
        }

        let mut s = Scene {
            name: v
                .get("name")
                .and_then(|x| x.as_str())
                .unwrap_or("scene")
                .to_string(),
            duration_s: num(&v, "$", "duration_s", 4.0, &mut out),
            rate_hz: num(&v, "$", "rate_hz", 120.0, &mut out),
            gravity: num(&v, "$", "gravity", -9.81, &mut out),
            ground: match v.get("ground") {
                None => Some([6.0, 6.0]),
                Some(Value::Null) => None,
                Some(Value::Arr(a)) if a.len() == 2 => {
                    Some([a[0].as_f64().unwrap_or(6.0), a[1].as_f64().unwrap_or(6.0)])
                }
                Some(_) => {
                    out.push(Problem {
                        path: "$.ground".into(),
                        message: "expected [width, depth] or null for no ground".into(),
                    });
                    Some([6.0, 6.0])
                }
            },
            ..Default::default()
        };

        if s.duration_s <= 0.0 {
            out.push(Problem {
                path: "$.duration_s".into(),
                message: "must be greater than zero".into(),
            });
        }
        if s.rate_hz <= 0.0 {
            out.push(Problem {
                path: "$.rate_hz".into(),
                message: "must be greater than zero".into(),
            });
        }
        // A scene is played out step by step and every step is written down, so an unbounded
        // rate is an unbounded file. Refused with the number, not silently clamped.
        let steps = (s.duration_s * s.rate_hz).round();
        if steps > 200_000.0 {
            out.push(Problem {
                path: "$".into(),
                message: format!(
                    "duration_s x rate_hz is {steps:.0} steps, above the 200000 cap; lower one \
                     of them"
                ),
            });
        }

        match v.get("bodies") {
            None | Some(Value::Null) => {}
            Some(Value::Arr(a)) => {
                for (i, b) in a.iter().enumerate() {
                    let path = format!("bodies[{i}]");
                    if want_obj(b, &path, &mut out).is_none() {
                        continue;
                    }
                    let id = b
                        .get("id")
                        .and_then(|x| x.as_str())
                        .map(str::to_string)
                        .unwrap_or_else(|| format!("body{i}"));
                    let sh = shape(b, &path, &mut out);
                    let size = vec3(b, &path, "size", [0.2, 0.2, 0.2], &mut out);
                    if size.iter().any(|x| *x <= 0.0) && sh != Shape::Plane {
                        out.push(Problem {
                            path: format!("{path}.size"),
                            message: format!("every extent must be positive, got {size:?}"),
                        });
                    }
                    let motion = motion::parse(b, &path, &mut out);
                    s.bodies.push(Body {
                        id,
                        shape: sh,
                        size,
                        color: color(b, &path, &mut out),
                        material: b
                            .get("material")
                            .and_then(|x| x.as_str())
                            .map(str::to_string),
                        motion,
                    });
                }
            }
            Some(_) => out.push(Problem {
                path: "$.bodies".into(),
                message: "expected an array".into(),
            }),
        }

        match v.get("robots") {
            None | Some(Value::Null) => {}
            Some(Value::Arr(a)) => {
                for (i, r) in a.iter().enumerate() {
                    let path = format!("robots[{i}]");
                    if want_obj(r, &path, &mut out).is_none() {
                        continue;
                    }
                    let Some(urdf) = r.get("urdf").and_then(|x| x.as_str()) else {
                        out.push(Problem {
                            path: format!("{path}.urdf"),
                            message: "a robot needs a urdf path".into(),
                        });
                        continue;
                    };
                    s.robots.push(RobotRef {
                        id: r
                            .get("id")
                            .and_then(|x| x.as_str())
                            .unwrap_or("robot")
                            .to_string(),
                        urdf: urdf.to_string(),
                        at: vec3(r, &path, "at", [0.0; 3], &mut out),
                        sweep_each: !matches!(r.get("sweep").and_then(|x| x.as_str()), Some("all")),
                    });
                }
            }
            Some(_) => out.push(Problem {
                path: "$.robots".into(),
                message: "expected an array".into(),
            }),
        }

        // Duplicate ids would collide on the same topic and one would silently overwrite the
        // other in the viewer's scene tree.
        for i in 0..s.bodies.len() {
            if s.bodies[..i].iter().any(|b| b.id == s.bodies[i].id) {
                out.push(Problem {
                    path: format!("bodies[{i}].id"),
                    message: format!("{:?} is used more than once", s.bodies[i].id),
                });
            }
        }

        if s.bodies.is_empty() && s.robots.is_empty() {
            out.push(Problem {
                path: "$".into(),
                message: "a scene with no bodies and no robots would record nothing".into(),
            });
        }

        if out.is_empty() {
            Ok(s)
        } else {
            Err(out)
        }
    }

    /// How many steps this scene will record.
    pub fn steps(&self) -> u64 {
        (self.duration_s * self.rate_hz).round().max(1.0) as u64
    }

    /// Play the scene out and write it.
    ///
    /// `load_urdf` resolves a robot's `urdf` field to its text. Returning `None` records the
    /// scene without that robot and notes it, rather than failing the whole thing: a missing
    /// description should not cost the caller the crate that was in the same scene.
    pub fn record<F>(&self, load_urdf: F) -> Result<Recorded, String>
    where
        F: Fn(&str) -> Option<String>,
    {
        self.record_with(load_urdf, Vec::new)
    }

    /// [`Scene::record`], plus a production note for the recording's own file.
    ///
    /// `production` runs after the digests are fixed and just before the file closes, so a
    /// meter stopped inside it covers the whole recording. This crate never opens a meter
    /// itself — it stays wasm-clean — the caller that has one passes the closure.
    pub fn record_with<F, P>(&self, load_urdf: F, production: P) -> Result<Recorded, String>
    where
        F: Fn(&str) -> Option<String>,
        P: FnOnce() -> Vec<(String, String)>,
    {
        let steps = self.steps();
        let dt_ns = (1e9 / self.rate_hz).round().max(1.0) as u64;
        let mut rec = Recorder::new(Vec::new(), Precision::Quantized { drop_bits: 12 });
        let t0 = Stamp::sim(0, 0);
        let mut notes: Vec<String> = Vec::new();

        if let Some([w, d]) = self.ground {
            rec.geometry(
                "/scene/ground",
                t0,
                &Geometry::plane("world", "ground", w, d),
            )
            .map_err(|e| e.to_string())?;
        }

        // Declare every body once, then move it. A geometry that is re-declared every step is a
        // recording that is mostly redundant bytes.
        for b in &self.bodies {
            let g = Geometry {
                // The geometry's frame is the body's OWN frame, which the per-step transform
                // then moves. Naming "world" here instead pins the body to the origin and the
                // transforms match nothing — it renders, it just never moves, which is the
                // one failure mode a still screenshot cannot show you.
                frame: b.id.clone(),
                id: b.id.clone(),
                shape: b.shape,
                size: b.size,
                translation: [0.0; 3],
                rotation: [0.0, 0.0, 0.0, 1.0],
                color: b.color,
                points: Vec::new(),
                mesh: String::new(),
            };
            rec.geometry(&format!("/scene/body/{}", b.id), t0, &g)
                .map_err(|e| e.to_string())?;
            if let Some(m) = &b.material {
                rec.event("/log", t0, "info", &format!("{}: material {m}", b.id))
                    .map_err(|e| e.to_string())?;
            }
        }

        // Robots: parse, declare, and remember them for the per-step sweep.
        let mut robots = Vec::new();
        for r in &self.robots {
            let Some(text) = load_urdf(&r.urdf) else {
                notes.push(format!(
                    "robot {:?}: {} could not be loaded, so it is not in this recording",
                    r.id, r.urdf
                ));
                continue;
            };
            let robot = ferroscope_urdf::Robot::parse(&text)
                .map_err(|e| format!("robot {:?}: {}: {e}", r.id, r.urdf))?;
            let prefix = format!("/scene/{}", r.id);
            robot
                .declare(&mut rec, t0, &prefix)
                .map_err(|e| e.to_string())?;
            for f in robot.check() {
                notes.push(format!("robot {:?}: {}: {}", r.id, f.kind, f.detail));
            }
            let movable: Vec<_> = robot.movable_joints().cloned().collect();
            robots.push((r.clone(), robot, movable, prefix));
        }

        let mut lowest: Option<(f64, String)> = None;
        let mut lowest_by: Vec<(String, f64)> = Vec::new();
        let mut settled_by: Vec<(String, f64)> = Vec::new();
        let mut last_pos: Vec<(String, [f64; 3])> = Vec::new();
        for step in 0..steps {
            let t_s = step as f64 / self.rate_hz;
            let t = Stamp::sim(step * dt_ns, step);
            for b in &self.bodies {
                let (p, q) = b.motion.at(t_s, self.gravity);
                rec.transform(&format!("/scene/tf/{}", b.id), t, "world", &b.id, p, q)
                    .map_err(|e| e.to_string())?;
                // The floor of the body's own bounding extent, which is what a reader means by
                // "did it go through the ground".
                let half = match b.shape {
                    Shape::Sphere => b.size[2],
                    _ => b.size[2] * 0.5,
                };
                let z = p[2] - half;
                if lowest.as_ref().is_none_or(|(w, _)| z < *w) {
                    lowest = Some((z, b.id.clone()));
                }
                note_lowest(&mut lowest_by, &b.id, z);
                // The last moment it moved. A body that is still moving at the final step has
                // its settle time equal to the run length, which is exactly the signal a
                // "settles in time" check needs.
                match last_pos.iter_mut().find(|(k, _)| *k == b.id) {
                    Some((_, prev)) => {
                        let d = (0..3).map(|k| (p[k] - prev[k]).abs()).fold(0.0, f64::max);
                        if d > 1e-4 {
                            *prev = p;
                            match settled_by.iter_mut().find(|(k, _)| *k == b.id) {
                                Some((_, w)) => *w = t_s,
                                None => settled_by.push((b.id.clone(), t_s)),
                            }
                        }
                    }
                    None => {
                        last_pos.push((b.id.clone(), p));
                        settled_by.push((b.id.clone(), 0.0));
                    }
                }
            }
            for (r, robot, movable, prefix) in &robots {
                let u = step as f64 / steps as f64;
                let n = movable.len().max(1);
                let q: Vec<(String, f64)> = movable
                    .iter()
                    .enumerate()
                    .map(|(k, j)| {
                        let (lo, hi) = j.limits.unwrap_or((-3.0, 3.0));
                        let home = 0.0f64.clamp(lo, hi);
                        if r.sweep_each {
                            let slice = u * n as f64 - k as f64;
                            let v = if (0.0..1.0).contains(&slice) {
                                let s = (std::f64::consts::TAU * slice).sin();
                                home + if s >= 0.0 {
                                    (hi - home) * s
                                } else {
                                    (home - lo) * s
                                }
                            } else {
                                home
                            };
                            (j.name.clone(), v)
                        } else {
                            let phase = std::f64::consts::TAU * (u + k as f64 / n as f64);
                            let mid = (lo + hi) * 0.5;
                            (j.name.clone(), mid + (hi - lo) * 0.35 * phase.sin())
                        }
                    })
                    .collect();
                robot
                    .log_pose(&mut rec, t, &q, prefix)
                    .map_err(|e| e.to_string())?;
                for (name, v) in &q {
                    rec.scalar(&format!("/joints/{}/{name}", r.id), t, *v, "rad")
                        .map_err(|e| e.to_string())?;
                }
                if let Some((z, link)) = robot.lowest_point(&q) {
                    let z = z + r.at[2];
                    if lowest.as_ref().is_none_or(|(w, _)| z < *w) {
                        lowest = Some((z, format!("{}/{link}", r.id)));
                    }
                    note_lowest(&mut lowest_by, &r.id, z);
                }
            }
            // A stated, crude power model: the scene is kinematic, so this is what it would
            // cost to *compute and drive* it, labelled an estimate because nothing measured a
            // motor or a die.
            let moving = self.bodies.iter().filter(|b| b.motion.is_moving()).count();
            rec.energy(
                "/energy/actuation",
                t,
                Rail::Actuation,
                "bodies",
                2.0 + 3.0 * moving as f64 + 4.0 * robots.len() as f64,
            )
            .map_err(|e| e.to_string())?;
            rec.energy("/energy/soc", t, Rail::Compute, "soc", 7.8)
                .map_err(|e| e.to_string())?;
        }

        let mut spec = RunSpec::new(format!("scene:{}", self.name), 0)
            .dt_ns(dt_ns)
            .steps(steps)
            .integrator("closed form (described motion)")
            .solver("none")
            .build(concat!("ferroscope-scene ", env!("CARGO_PKG_VERSION")));
        spec = spec.config("gravity", format!("{}", self.gravity));
        for b in &self.bodies {
            spec = spec.config(format!("body.{}", b.id), b.motion.describe());
        }
        for r in &self.robots {
            spec = spec.config(format!("robot.{}", r.id), r.urdf.clone());
        }

        let platform = format!("{}-{}", std::env::consts::ARCH, std::env::consts::OS);
        let (bytes, receipt, quote) = rec
            .seal_with(spec, &platform, production)
            .map_err(|e| e.to_string())?;
        Ok(Recorded {
            bytes,
            receipt,
            total_j: quote.total_j,
            compute_fraction: quote.compute_fraction(),
            steps,
            lowest,
            lowest_by,
            settled_by,
            notes,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_minimal_scene_reads_and_records() {
        let s = Scene::parse(
            r#"{"name":"t","duration_s":1.0,"rate_hz":60,
                "bodies":[{"id":"a","shape":"box","size":[1,1,1],
                           "motion":{"kind":"static","at":[0,0,0.5]}}]}"#,
        )
        .unwrap();
        assert_eq!(s.steps(), 60);
        let out = s.record(|_| None).unwrap();
        assert!(out.bytes.len() > 500);
        assert_eq!(out.steps, 60);
        // A static box of side 1 centred at z=0.5 has its floor exactly on the ground.
        let (z, id) = out.lowest.unwrap();
        assert_eq!(id, "a");
        assert!(z.abs() < 1e-9, "{z}");
    }

    #[test]
    fn every_problem_is_reported_at_once_with_its_path() {
        let e = Scene::parse(
            r#"{"duration_s":-1,"rate_hz":0,
                "bodies":[{"id":"a","shape":"blob","size":[1,1],"color":"nope",
                           "motion":{"kind":"orbitt"}}]}"#,
        )
        .unwrap_err();
        let paths: Vec<&str> = e.iter().map(|p| p.path.as_str()).collect();
        for want in [
            "$.duration_s",
            "$.rate_hz",
            "bodies[0].shape",
            "bodies[0].size",
            "bodies[0].color",
            "bodies[0].motion.kind",
        ] {
            assert!(paths.contains(&want), "missing {want} in {paths:?}");
        }
    }

    #[test]
    fn an_unknown_motion_says_what_the_known_ones_are() {
        let e = Scene::parse(r#"{"bodies":[{"id":"a","motion":{"kind":"orbitt"}}]}"#).unwrap_err();
        let m = &e
            .iter()
            .find(|p| p.path.ends_with("motion.kind"))
            .unwrap()
            .message;
        for k in ["static", "linear", "orbit", "oscillate", "fall"] {
            assert!(m.contains(k), "{m} should list {k}");
        }
    }

    #[test]
    fn duplicate_ids_are_refused_because_they_would_share_a_topic() {
        let e = Scene::parse(
            r#"{"bodies":[{"id":"a","motion":{"kind":"static"}},
                          {"id":"a","motion":{"kind":"static"}}]}"#,
        )
        .unwrap_err();
        assert!(e.iter().any(|p| p.path == "bodies[1].id"), "{e:?}");
    }

    #[test]
    fn an_empty_scene_is_refused_rather_than_recorded_as_nothing() {
        let e = Scene::parse(r#"{"name":"empty"}"#).unwrap_err();
        assert!(
            e.iter().any(|p| p.message.contains("record nothing")),
            "{e:?}"
        );
    }

    #[test]
    fn an_unbounded_recording_is_refused_with_the_number() {
        let e = Scene::parse(
            r#"{"duration_s":3600,"rate_hz":1000,
                                 "bodies":[{"id":"a","motion":{"kind":"static"}}]}"#,
        )
        .unwrap_err();
        let m = &e
            .iter()
            .find(|p| p.message.contains("cap"))
            .unwrap()
            .message;
        assert!(m.contains("3600000"), "{m}");
    }

    #[test]
    fn a_missing_robot_costs_the_robot_and_not_the_scene() {
        let s = Scene::parse(
            r#"{"bodies":[{"id":"a","motion":{"kind":"static"}}],
                "robots":[{"id":"r","urdf":"nowhere.urdf"}]}"#,
        )
        .unwrap();
        let out = s.record(|_| None).unwrap();
        assert!(
            out.notes.iter().any(|n| n.contains("could not be loaded")),
            "{:?}",
            out.notes
        );
        assert!(
            out.bytes.len() > 500,
            "the rest of the scene still recorded"
        );
    }

    #[test]
    fn a_robot_in_a_scene_is_declared_and_swept() {
        let urdf = r#"<robot name="r">
            <link name="base"><inertial><mass value="1"/><inertia ixx="1" ixy="0" ixz="0" iyy="1" iyz="0" izz="1"/></inertial>
              <visual><geometry><box size="0.1 0.1 0.1"/></geometry></visual>
              <collision><geometry><box size="0.1 0.1 0.1"/></geometry></collision></link>
            <link name="arm"><inertial><mass value="1"/><inertia ixx="1" ixy="0" ixz="0" iyy="1" iyz="0" izz="1"/></inertial>
              <visual><geometry><box size="0.4 0.05 0.05"/></geometry></visual>
              <collision><geometry><box size="0.4 0.05 0.05"/></geometry></collision></link>
            <joint name="j" type="revolute"><parent link="base"/><child link="arm"/>
              <origin xyz="0 0 0.05"/><axis xyz="0 1 0"/>
              <limit lower="-1" upper="1" effort="1" velocity="1"/></joint>
          </robot>"#;
        let s = Scene::parse(
            r#"{"duration_s":0.5,"rate_hz":40,
                "bodies":[{"id":"a","motion":{"kind":"static"}}],
                "robots":[{"id":"r","urdf":"x.urdf"}]}"#,
        )
        .unwrap();
        let out = s.record(|_| Some(urdf.to_string())).unwrap();
        let log = ferroscope_schema::mcap::read(&out.bytes).unwrap();
        let topics: Vec<&str> = log.channels.iter().map(|c| c.topic.as_str()).collect();
        assert!(topics.contains(&"/scene/r/visual/arm"), "{topics:?}");
        assert!(topics.contains(&"/scene/r/tf/arm"), "{topics:?}");
        assert!(topics.contains(&"/joints/r/j"), "{topics:?}");
    }

    #[test]
    fn a_moving_body_actually_moves() {
        // The bug this pins: geometry declared in the "world" frame is pinned to the origin,
        // so every transform written for it is discarded and the body sits still. Both files
        // parse, both render, and only the positions differ.
        let s = Scene::parse(
            r#"{"duration_s":2.0,"rate_hz":50,
                "bodies":[{"id":"ball","shape":"sphere","size":[0.1,0.1,0.1],
                           "motion":{"kind":"fall","from":[0,0,3]}}]}"#,
        )
        .unwrap();
        let out = s.record(|_| None).unwrap();
        let log = ferroscope_schema::mcap::read(&out.bytes).unwrap();

        // The geometry must be parented to a frame the transforms actually address.
        let geo = log
            .channels
            .iter()
            .find(|c| c.topic == "/scene/body/ball")
            .expect("the body must be declared");
        let tf = log
            .channels
            .iter()
            .find(|c| c.topic == "/scene/tf/ball")
            .expect("the body must be moved");
        let g_msgs: Vec<_> = log
            .messages
            .iter()
            .filter(|m| m.channel_id == geo.id)
            .collect();
        let t_msgs: Vec<_> = log
            .messages
            .iter()
            .filter(|m| m.channel_id == tf.id)
            .collect();
        assert_eq!(g_msgs.len(), 1, "declared once");
        assert_eq!(t_msgs.len(), 100, "moved every step");

        let frame = |m: &ferroscope_schema::mcap::Message| {
            let v = ferroscope_schema::json::parse(std::str::from_utf8(&m.data).unwrap()).unwrap();
            (
                v.get("frame")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string(),
                v.get("child")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string(),
            )
        };
        let (geo_frame, _) = frame(g_msgs[0]);
        let (_, tf_child) = frame(t_msgs[0]);
        assert_eq!(
            geo_frame, tf_child,
            "the geometry's frame must be the frame the transforms name as their child, or the \
             body never moves"
        );

        // And the positions must genuinely differ across the fall.
        let z = |m: &ferroscope_schema::mcap::Message| {
            let v = ferroscope_schema::json::parse(std::str::from_utf8(&m.data).unwrap()).unwrap();
            v.get("translation").and_then(|a| a.as_array()).unwrap()[2]
                .as_f64()
                .unwrap()
        };
        let (first, last) = (z(t_msgs[0]), z(t_msgs[t_msgs.len() - 1]));
        assert!(
            (first - 3.0).abs() < 1e-6,
            "starts where it was dropped: {first}"
        );
        assert!(last < 0.2, "and ends on the ground: {last}");
    }

    #[test]
    fn the_recording_verifies_against_its_own_receipt() {
        let s = Scene::parse(
            r#"{"duration_s":0.5,"rate_hz":60,
                "bodies":[{"id":"a","motion":{"kind":"fall","from":[0,0,3]}}]}"#,
        )
        .unwrap();
        let out = s.record(|_| None).unwrap();
        let v = ferroscope_schema::verify(&out.bytes).expect("a scene carries a receipt");
        assert!(
            v.spec_matches && v.trace_matches,
            "a scene must stand behind its own receipt"
        );
    }
}
