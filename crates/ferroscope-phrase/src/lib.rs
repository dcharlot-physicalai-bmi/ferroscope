//! Say what you want to see, in English.
//!
//! ```
//! use ferroscope_phrase::read;
//!
//! let r = read("drop a red crate from 2 m next to an SO-101 arm for 5 seconds").unwrap();
//! assert!(r.scene_json.contains("\"kind\":\"fall\""));
//! assert!(r.scene_json.contains("\"urdf\":\"so101\""));
//! // And it says what it did, rather than only doing it.
//! assert!(r.understood.iter().any(|u| u.contains("falling")));
//! ```
//!
//! # The point is the report, not the parse
//!
//! Any phrase parser will fail on language it was not built for. What separates a useful one from
//! an infuriating one is whether it tells you *which words it ignored* — because a sentence that
//! silently loses "onto the conveyor" produces a scene with no conveyor and no explanation, and
//! the reader is left comparing their sentence against a picture, guessing.
//!
//! So every reading carries three lists: what it [understood](Reading::understood), what it
//! [assumed](Reading::assumed) because you did not say, and what it [ignored](Reading::ignored)
//! because the word has no meaning here. The JSON it produces is ordinary
//! [scene JSON](https://github.com/dcharlot-physicalai-bmi/ferroscope) — readable, editable, and
//! validated by exactly the same code as a hand-written one, so this is a *starting point you can
//! correct*, never a black box.
//!
//! For open-ended language, a model is the right tool and the MCP server exists for that. This is
//! for the common cases, offline, in a browser, with no key and no round trip.

#![forbid(unsafe_code)]

pub mod vocab;

use vocab::*;

/// Why a phrase could not become a scene at all.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Problem {
    pub message: String,
    /// What to try instead. Never empty: a refusal with no way forward is just a wall.
    pub hint: String,
}

impl std::fmt::Display for Problem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}\n  {}", self.message, self.hint)
    }
}

/// A phrase, read.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Reading {
    /// Scene JSON, ready for `Scene::parse`, `scene_record`, or hand-editing.
    pub scene_json: String,
    /// One line per thing that made it into the scene.
    pub understood: Vec<String>,
    /// Defaults filled in because the phrase did not say.
    pub assumed: Vec<String>,
    /// Words with no meaning in this vocabulary. The list that stops the silence.
    pub ignored: Vec<String>,
}

// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq)]
enum Unit {
    Metres,
    Seconds,
    Hertz,
    None,
}

/// A number and the unit attached to it, if any.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Qty {
    value: f64,
    unit: Unit,
}

fn tokenize(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for c in text.chars() {
        // A hyphen is kept inside a word ("so-101") but a decimal point only between digits.
        if c.is_alphanumeric()
            || c == '-'
            || (c == '.' && cur.chars().last().is_some_and(|p| p.is_ascii_digit()))
        {
            cur.push(c.to_ascii_lowercase());
        } else {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            if c == ',' || c == ';' {
                out.push(",".into());
            }
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Read a number, with a unit taken from the same token (`30cm`) or the next one (`30 cm`).
fn quantity(tokens: &[String], i: usize) -> Option<(Qty, usize)> {
    let t = &tokens[i];
    // Split a token like "30cm" into its number and its suffix.
    let split = t.find(|c: char| !c.is_ascii_digit() && c != '.' && c != '-');
    let (num_s, suffix) = match split {
        Some(k) if k > 0 => (&t[..k], &t[k..]),
        Some(_) => return None,
        None => (t.as_str(), ""),
    };
    let value: f64 = num_s.parse().ok()?;
    if !value.is_finite() {
        return None;
    }
    let (unit, used) = if !suffix.is_empty() {
        (unit_of(suffix)?, 1)
    } else {
        match tokens.get(i + 1).and_then(|n| unit_of(n)) {
            Some(u) => (u, 2),
            None => (Unit::None, 1),
        }
    };
    let value = match unit {
        Unit::Metres => scale_length(suffix_or(tokens, i, suffix), value),
        _ => value,
    };
    Some((Qty { value, unit }, used))
}

fn suffix_or<'a>(tokens: &'a [String], i: usize, suffix: &'a str) -> &'a str {
    if suffix.is_empty() {
        tokens.get(i + 1).map(|s| s.as_str()).unwrap_or("")
    } else {
        suffix
    }
}

fn scale_length(word: &str, v: f64) -> f64 {
    match word {
        "cm" | "centimetre" | "centimetres" | "centimeter" | "centimeters" => v / 100.0,
        "mm" | "millimetre" | "millimetres" | "millimeter" | "millimeters" => v / 1000.0,
        _ => v,
    }
}

fn unit_of(w: &str) -> Option<Unit> {
    match w {
        "m" | "metre" | "metres" | "meter" | "meters" | "cm" | "centimetre" | "centimetres"
        | "centimeter" | "centimeters" | "mm" | "millimetre" | "millimetres" | "millimeter"
        | "millimeters" => Some(Unit::Metres),
        "s" | "sec" | "secs" | "second" | "seconds" => Some(Unit::Seconds),
        "hz" | "hertz" => Some(Unit::Hertz),
        _ => None,
    }
}

fn num_word(w: &str) -> Option<f64> {
    NUMBER_WORDS.iter().find(|(k, _)| *k == w).map(|(_, v)| *v)
}

/// What one clause turned out to be about.
#[derive(Default)]
struct Clause {
    shape: Option<&'static str>,
    shape_word: String,
    motion: Option<&'static str>,
    motion_word: String,
    robot: Option<&'static str>,
    colour: Option<&'static str>,
    material: Option<&'static str>,
    count: usize,
    /// Lengths with no preposition in front of them: the body's own size.
    sizes: Vec<f64>,
    /// Lengths introduced by "from", "at", "to", "by": how far the motion goes.
    distances: Vec<f64>,
    seconds: Vec<f64>,
    hertz: Vec<f64>,
    plain: Vec<f64>,
    unknown: Vec<String>,
}

fn json_str(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn n(v: f64) -> String {
    // Trim the trailing zeros a naive format leaves behind, so the JSON a person reads back is
    // the number they typed rather than 0.30000000000000004.
    let s = format!("{:.4}", v);
    let s = s.trim_end_matches('0').trim_end_matches('.').to_string();
    if s.is_empty() || s == "-" {
        "0".into()
    } else {
        s
    }
}

/// Read an English phrase into scene JSON.
pub fn read(text: &str) -> Result<Reading, Problem> {
    let tokens = tokenize(text);
    if tokens.is_empty() {
        return Err(Problem {
            message: "there is nothing here to read".into(),
            hint: "try: drop a crate from 2 m beside an SO-101 arm".into(),
        });
    }

    // Global settings and per-clause groups. A clause is separated by a comma or by "and", which
    // is how people actually list things.
    let mut clauses: Vec<Clause> = vec![Clause::default()];
    let mut duration: Option<f64> = None;
    let mut rate: Option<f64> = None;
    let mut gravity: Option<(f64, &'static str)> = None;
    let mut ignored: Vec<String> = Vec::new();

    let mut i = 0;
    // Set by a preposition, cleared by the next token: marks the length that follows as being
    // about the motion rather than about the body.
    let mut distance_next = false;
    while i < tokens.len() {
        let w = tokens[i].as_str();
        let after_preposition = distance_next;
        distance_next = matches!(w, "from" | "at" | "to" | "by" | "radius" | "of");

        if w == "," || w == "and" {
            // Only start a new clause once the current one has something in it, so "a red, round
            // ball" does not become two bodies.
            let c = clauses.last().unwrap();
            if c.shape.is_some() || c.robot.is_some() {
                clauses.push(Clause::default());
            }
            i += 1;
            continue;
        }

        // "on the moon" / "in zero g" — gravity, before the filler pass eats the nouns.
        if w == "moon" {
            gravity = Some((-1.62, "moon"));
            i += 1;
            continue;
        }
        if w == "mars" {
            gravity = Some((-3.72, "mars"));
            i += 1;
            continue;
        }
        if w == "zero-g"
            || w == "zerog"
            || (w == "zero" && tokens.get(i + 1).is_some_and(|t| t == "g"))
        {
            gravity = Some((0.0, "zero gravity"));
            i += if w == "zero" { 2 } else { 1 };
            continue;
        }

        if let Some((q, used)) = quantity(&tokens, i) {
            let c = clauses.last_mut().unwrap();
            match q.unit {
                // "a 30 cm crate" is a size; "from 2 m" is a distance. Without this the drop
                // height became the crate's own dimensions, which is how you get a 2 m crate.
                Unit::Metres if after_preposition => c.distances.push(q.value),
                Unit::Metres => c.sizes.push(q.value),
                Unit::Seconds => c.seconds.push(q.value),
                Unit::Hertz => c.hertz.push(q.value),
                Unit::None => c.plain.push(q.value),
            }
            i += used;
            continue;
        }

        if let Some(v) = num_word(w) {
            // "a"/"an" are also filler; only treat them as a count when a shape follows.
            let is_article = w == "a" || w == "an";
            let shape_next = tokens
                .get(i + 1)
                .is_some_and(|t| lookup(SHAPES, t).is_some() || lookup(ROBOTS, t).is_some());
            if !is_article || shape_next {
                clauses.last_mut().unwrap().plain.push(v);
            }
            i += 1;
            continue;
        }

        if let Some(s) = lookup(SHAPES, w) {
            let c = clauses.last_mut().unwrap();
            if c.shape.is_some() || c.robot.is_some() {
                clauses.push(Clause::default());
            }
            let c = clauses.last_mut().unwrap();
            c.shape = Some(s);
            c.shape_word = w.to_string();
            // A plural means more than one even without a number in front of it.
            if w.ends_with('s') && c.count == 0 {
                c.count = 2;
            }
            i += 1;
            continue;
        }

        if let Some(r) = lookup(ROBOTS, w) {
            let c = clauses.last_mut().unwrap();
            if c.shape.is_some() {
                clauses.push(Clause::default());
            }
            let c = clauses.last_mut().unwrap();
            // "an SO-101 arm" and "a robot arm" name one machine with two words. Only a robot
            // word that follows something else starts a second one.
            match c.robot {
                None => c.robot = Some(r),
                Some(prev) => {
                    let adjacent = i > 0 && lookup(ROBOTS, &tokens[i - 1]).is_some();
                    if !adjacent {
                        clauses.push(Clause::default());
                        clauses.last_mut().unwrap().robot = Some(r);
                    } else if prev == "arm" && r != "arm" {
                        // The more specific word wins: "arm" then "SO-101" is an SO-101.
                        c.robot = Some(r);
                    }
                }
            }
            i += 1;
            continue;
        }

        if let Some(m) = lookup(MOTIONS, w) {
            let c = clauses.last_mut().unwrap();
            c.motion = Some(m);
            c.motion_word = w.to_string();
            i += 1;
            continue;
        }

        if let Some(col) = lookup(COLOURS, w) {
            clauses.last_mut().unwrap().colour = Some(col);
            i += 1;
            continue;
        }

        if let Some(mat) = lookup(MATERIALS, w) {
            clauses.last_mut().unwrap().material = Some(mat);
            i += 1;
            continue;
        }

        if FILLER.contains(&w) {
            i += 1;
            continue;
        }

        clauses.last_mut().unwrap().unknown.push(w.to_string());
        i += 1;
    }

    // Seconds and hertz said anywhere are global: "for 5 seconds" is about the scene, not about
    // whichever noun happened to come before it. Except a period attached to a moving body,
    // which is resolved per clause below.
    let mut understood: Vec<String> = Vec::new();
    let mut assumed: Vec<String> = Vec::new();
    let mut bodies: Vec<String> = Vec::new();
    let mut robots: Vec<String> = Vec::new();
    let mut used_names: Vec<String> = Vec::new();

    for c in &clauses {
        ignored.extend(c.unknown.iter().cloned());
    }

    let has_robot = clauses.iter().any(|c| c.robot.is_some());
    // A robot occupies the origin, so bodies in the same scene start clear of it. Without this
    // "drop a crate beside an arm" drops the crate straight through the arm.
    let offset = if has_robot { 0.8 } else { 0.0 };

    for c in &clauses {
        if let Some(r) = c.robot {
            let id = unique(&mut used_names, if r == "so101" { "arm" } else { r });
            robots.push(format!(
                "{{\"id\":{},\"urdf\":{},\"sweep\":\"each\"}}",
                json_str(&id),
                json_str(r)
            ));
            understood.push(format!(
                "{id}: the {r} description, sweeping one joint at a time"
            ));
            continue;
        }
        let Some(shape) = c.shape else { continue };

        // Counts: an explicit number beats a bare plural.
        let count = c
            .plain
            .first()
            .copied()
            .filter(|v| *v >= 1.0 && *v <= 12.0 && v.fract() == 0.0)
            .map(|v| v as usize)
            .unwrap_or(if c.count > 0 { c.count } else { 1 });

        let motion = c.motion.unwrap_or("static");
        // The first length is the body's size; a second one is the motion's distance. That is
        // the order people say them in: "a 30 cm crate from 2 m".
        let size = c.sizes.first().copied().unwrap_or(0.3);
        let dist = c.distances.first().copied();
        let period = c.seconds.first().copied();

        for k in 0..count {
            let base = c.shape_word.trim_end_matches('s');
            let want = if count > 1 {
                format!("{base}{}", k + 1)
            } else {
                base.to_string()
            };
            let id = unique(&mut used_names, &want);
            // Bodies spread along x so several of them are not stacked in one spot.
            let x = offset
                + if count > 1 {
                    (k as f64 - (count as f64 - 1.0) / 2.0) * (size * 2.0).max(0.4)
                } else {
                    0.0
                };
            let half = size / 2.0;
            let m = match motion {
                "fall" => {
                    let from = dist.unwrap_or(2.0);
                    format!(
                        "{{\"kind\":\"fall\",\"from\":[{},0,{}],\"restitution\":0.35}}",
                        n(x),
                        n(from)
                    )
                }
                "orbit" => format!(
                    "{{\"kind\":\"orbit\",\"center\":[{},0,{}],\"radius\":{},\"period_s\":{}}}",
                    n(offset),
                    n((size + 0.6).max(0.8)),
                    n(dist.unwrap_or(0.8)),
                    n(period.unwrap_or(4.0))
                ),
                "linear" => format!(
                    "{{\"kind\":\"linear\",\"from\":[{},{},{}],\"to\":[{},{},{}],\"period_s\":{}}}",
                    n(-dist.unwrap_or(1.2)),
                    n(x),
                    n(half),
                    n(dist.unwrap_or(1.2)),
                    n(x),
                    n(half),
                    n(period.unwrap_or(4.0))
                ),
                "oscillate" => format!(
                    "{{\"kind\":\"oscillate\",\"at\":[{},0,{}],\"axis\":[0,0,1],\"amplitude\":{},\"period_s\":{}}}",
                    n(x),
                    n(half + 0.4),
                    n(dist.unwrap_or(0.3)),
                    n(period.unwrap_or(2.0))
                ),
                _ => format!("{{\"kind\":\"static\",\"at\":[{},0,{}]}}", n(x), n(half)),
            };
            let mut b = format!(
                "{{\"id\":{},\"shape\":{},\"size\":[{},{},{}],\"motion\":{}",
                json_str(&id),
                json_str(shape),
                n(if shape == "sphere" { half } else { size }),
                n(if shape == "sphere" { half } else { size }),
                n(if shape == "sphere" { half } else { size }),
                m
            );
            if let Some(col) = c.colour {
                b.push_str(&format!(",\"color\":{}", json_str(col)));
            }
            if let Some(mat) = c.material {
                b.push_str(&format!(",\"material\":{}", json_str(mat)));
            }
            b.push('}');
            bodies.push(b);
        }

        let verb = match motion {
            "fall" => format!("falling from {} m", n(dist.unwrap_or(2.0))),
            "orbit" => format!(
                "orbiting at {} m every {} s",
                n(dist.unwrap_or(0.8)),
                n(period.unwrap_or(4.0))
            ),
            "linear" => format!(
                "sliding +-{} m every {} s",
                n(dist.unwrap_or(1.2)),
                n(period.unwrap_or(4.0))
            ),
            "oscillate" => format!(
                "oscillating +-{} m every {} s",
                n(dist.unwrap_or(0.3)),
                n(period.unwrap_or(2.0))
            ),
            _ => "sitting still".to_string(),
        };
        understood.push(format!(
            "{}{} {} of {} m, {verb}",
            if count > 1 {
                format!("{count} ")
            } else {
                String::new()
            },
            if count > 1 {
                plural(shape)
            } else {
                shape.to_string()
            },
            if count > 1 { "each" } else { "one" },
            n(size)
        ));
        if c.sizes.is_empty() {
            assumed.push(format!("{shape} size {} m (not stated)", n(size)));
        }
        if c.motion.is_none() {
            assumed.push(format!("{shape} sits still (no motion word)"));
        }
    }

    // Global timing: seconds and hertz that no clause consumed as a period.
    for c in &clauses {
        if (c.motion.is_none() || c.motion == Some("fall") || c.motion == Some("static"))
            && let Some(s) = c.seconds.first()
        {
            duration = Some(*s);
        }
        if let Some(h) = c.hertz.first() {
            rate = Some(*h);
        }
    }

    if bodies.is_empty() && robots.is_empty() {
        let mut hint = String::from("name something to put in the scene: ");
        hint.push_str("a crate, a ball, a cylinder, or an SO-101 arm.");
        if !ignored.is_empty() {
            hint.push_str(&format!(
                " These words meant nothing here: {}.",
                ignored.join(", ")
            ));
        }
        return Err(Problem {
            message: "nothing in that phrase names a thing to record".into(),
            hint,
        });
    }

    let dur = duration.unwrap_or(4.0);
    if duration.is_none() {
        assumed.push("4 s long (say \"for 6 seconds\" to change it)".into());
    }
    let hz = rate.unwrap_or(120.0);
    if rate.is_none() {
        assumed.push("120 Hz (say \"at 60 Hz\" to change it)".into());
    }
    let g = gravity.map(|(v, _)| v).unwrap_or(-9.81);
    if let Some((_, where_)) = gravity {
        understood.push(format!("gravity on the {where_}: {} m/s²", n(g)));
    }

    let name = text.trim().trim_end_matches('.').to_string();
    let scene_json = format!(
        "{{\n  \"name\": {},\n  \"duration_s\": {},\n  \"rate_hz\": {},\n  \"gravity\": {},\n  \"bodies\": [\n    {}\n  ],\n  \"robots\": [\n    {}\n  ]\n}}",
        json_str(if name.is_empty() { "scene" } else { &name }),
        n(dur),
        n(hz),
        n(g),
        bodies.join(",\n    "),
        robots.join(",\n    ")
    );

    ignored.sort();
    ignored.dedup();
    Ok(Reading {
        scene_json,
        understood,
        assumed,
        ignored,
    })
}

/// English plurals for the four shape words, rather than a bare "+es" that produced "spherees".
fn plural(shape: &str) -> String {
    match shape {
        "box" => "boxes".into(),
        other => format!("{other}s"),
    }
}

fn unique(used: &mut Vec<String>, base: &str) -> String {
    let mut id = base.to_string();
    let mut k = 2;
    while used.contains(&id) {
        id = format!("{base}{k}");
        k += 1;
    }
    used.push(id.clone());
    id
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every phrase below is checked against the REAL scene reader, not against a string. A
    /// parser whose output is only ever compared with its own expectations is a parser that
    /// agrees with itself.
    fn scene(phrase: &str) -> (Reading, ferroscope_scene::Scene) {
        let r = read(phrase).unwrap_or_else(|e| panic!("{phrase:?} refused: {e}"));
        let s = ferroscope_scene::Scene::parse(&r.scene_json).unwrap_or_else(|p| {
            panic!(
                "{phrase:?} produced JSON the scene reader rejects: {p:?}\n{}",
                r.scene_json
            )
        });
        (r, s)
    }

    #[test]
    fn a_plain_sentence_becomes_a_scene_that_records() {
        let (_, s) = scene("drop a red crate from 2 m next to an SO-101 arm for 5 seconds");
        assert_eq!(s.bodies.len(), 1);
        assert_eq!(s.robots.len(), 1, "\"SO-101 arm\" is one machine, not two");
        assert_eq!(s.robots[0].urdf, "so101");
        assert!((s.duration_s - 5.0).abs() < 1e-9, "for 5 seconds");
        let out = s.record(|_| None).unwrap();
        assert!(out.bytes.len() > 500);
    }

    #[test]
    fn a_distance_after_a_preposition_is_not_the_bodys_size() {
        // "from 2 m" is a drop height. Reading it as the size gave a 2 m crate.
        let (_, s) = scene("drop a crate from 2 m");
        assert!(
            (s.bodies[0].size[2] - 0.3).abs() < 1e-9,
            "got {:?}",
            s.bodies[0].size
        );
        match s.bodies[0].motion {
            ferroscope_scene::Motion::Fall { from, .. } => {
                assert!((from[2] - 2.0).abs() < 1e-9, "dropped from {from:?}")
            }
            other => panic!("expected a fall, got {other:?}"),
        }
    }

    #[test]
    fn a_size_before_the_noun_is_the_size_and_the_unit_is_honoured() {
        let (_, s) = scene("a 30 cm cube orbiting at 0.8 m every 3 s");
        assert!((s.bodies[0].size[0] - 0.3).abs() < 1e-9, "30 cm is 0.3 m");
        match s.bodies[0].motion {
            ferroscope_scene::Motion::Orbit {
                radius, period_s, ..
            } => {
                assert!((radius - 0.8).abs() < 1e-9);
                assert!((period_s - 3.0).abs() < 1e-9);
            }
            other => panic!("expected an orbit, got {other:?}"),
        }
    }

    #[test]
    fn bodies_stand_clear_of_a_robot_rather_than_inside_it() {
        // "beside an arm" is how people say it, and dropping the crate at the origin buries
        // the machine it was meant to sit next to.
        let (_, with) = scene("drop a crate beside an SO-101 arm");
        let (_, without) = scene("drop a crate");
        let x = |s: &ferroscope_scene::Scene| match s.bodies[0].motion {
            ferroscope_scene::Motion::Fall { from, .. } => from[0],
            _ => 0.0,
        };
        assert!(
            x(&without).abs() < 1e-9,
            "no robot, no offset: {}",
            x(&without)
        );
        assert!(
            x(&with) > 0.5,
            "a body sharing a scene with a robot must clear it: {}",
            x(&with)
        );
    }

    #[test]
    fn a_count_makes_several_bodies_with_distinct_ids() {
        let (_, s) = scene("three balls falling");
        assert_eq!(s.bodies.len(), 3);
        let ids: Vec<&str> = s.bodies.iter().map(|b| b.id.as_str()).collect();
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), 3, "ids collide: {ids:?}");
        // And they are not stacked in one spot.
        let xs: Vec<f64> = s
            .bodies
            .iter()
            .map(|b| match b.motion {
                ferroscope_scene::Motion::Fall { from, .. } => from[0],
                _ => 0.0,
            })
            .collect();
        assert!(
            xs[0] != xs[1] && xs[1] != xs[2],
            "all three at the same x: {xs:?}"
        );
    }

    #[test]
    fn a_bare_plural_counts_as_more_than_one() {
        let (_, s) = scene("crates falling");
        assert!(
            s.bodies.len() >= 2,
            "a plural means several, got {}",
            s.bodies.len()
        );
    }

    #[test]
    fn words_it_cannot_use_are_reported_rather_than_dropped_in_silence() {
        // The whole reason this is usable: a sentence that loses "conveyor belt" must SAY it did.
        let (r, _) = scene("three balls falling onto a conveyor belt");
        assert!(
            r.ignored.contains(&"conveyor".to_string()),
            "{:?}",
            r.ignored
        );
        assert!(r.ignored.contains(&"belt".to_string()), "{:?}", r.ignored);
        // And filler is not reported, or the one word that mattered would be buried.
        for noise in ["a", "the", "onto"] {
            assert!(
                !r.ignored.contains(&noise.to_string()),
                "{noise:?} should not be reported"
            );
        }
    }

    #[test]
    fn what_it_guessed_is_listed_separately_from_what_you_said() {
        let (r, _) = scene("a crate");
        assert!(
            r.assumed.iter().any(|a| a.contains("size")),
            "{:?}",
            r.assumed
        );
        assert!(
            r.assumed.iter().any(|a| a.contains("sits still")),
            "{:?}",
            r.assumed
        );
        assert!(
            r.assumed.iter().any(|a| a.contains("4 s")),
            "{:?}",
            r.assumed
        );
        // A stated value must NOT appear as an assumption.
        let (r2, _) = scene("a crate for 9 seconds");
        assert!(
            !r2.assumed.iter().any(|a| a.contains("4 s")),
            "{:?}",
            r2.assumed
        );
    }

    #[test]
    fn a_phrase_naming_nothing_is_refused_with_a_way_forward() {
        let e = read("make it look nice").unwrap_err();
        assert!(e.message.contains("names a thing"), "{}", e.message);
        assert!(
            e.hint.contains("crate"),
            "a refusal must say what would work: {}",
            e.hint
        );
        assert!(
            e.hint.contains("nice"),
            "and which words it could not use: {}",
            e.hint
        );
        assert!(read("").is_err());
    }

    #[test]
    fn other_worlds_change_gravity() {
        let (r, s) = scene("drop a ball on the moon");
        assert!((s.gravity + 1.62).abs() < 1e-9, "got {}", s.gravity);
        assert!(r.understood.iter().any(|u| u.contains("moon")));
        let (_, mars) = scene("drop a ball on mars");
        assert!((mars.gravity + 3.72).abs() < 1e-9);
    }

    #[test]
    fn colour_and_material_words_reach_the_scene() {
        let (_, s) = scene("a red aluminium crate");
        assert_eq!(s.bodies[0].material.as_deref(), Some("6061-T6"));
        assert!(
            s.bodies[0].color[0] > 0.7 && s.bodies[0].color[1] < 0.4,
            "{:?}",
            s.bodies[0].color
        );
    }

    #[test]
    fn every_motion_word_reaches_its_motion() {
        for (phrase, want) in [
            ("a crate falling", "fall"),
            ("a crate orbiting", "orbit"),
            ("a crate sliding", "linear"),
            ("a crate oscillating", "oscillate"),
            ("a crate sitting still", "static"),
        ] {
            let (_, s) = scene(phrase);
            let got = match s.bodies[0].motion {
                ferroscope_scene::Motion::Fall { .. } => "fall",
                ferroscope_scene::Motion::Orbit { .. } => "orbit",
                ferroscope_scene::Motion::Linear { .. } => "linear",
                ferroscope_scene::Motion::Oscillate { .. } => "oscillate",
                ferroscope_scene::Motion::Static { .. } => "static",
            };
            assert_eq!(got, want, "{phrase:?}");
        }
    }

    #[test]
    fn every_shape_and_motion_word_in_the_vocabulary_actually_parses() {
        // A vocabulary table nothing exercises drifts out of step with the parser that reads it.
        for (word, want) in SHAPES {
            let (_, s) = scene(&format!("a {word}"));
            assert_eq!(
                format!("{:?}", s.bodies[0].shape).to_lowercase(),
                *want,
                "shape word {word:?}"
            );
        }
        for (word, _) in MOTIONS {
            let r = read(&format!("a crate {word}"));
            assert!(r.is_ok(), "motion word {word:?} does not parse");
        }
        for (word, _) in ROBOTS {
            let (_, s) = scene(&format!("a crate and a {word}"));
            assert_eq!(s.robots.len(), 1, "robot word {word:?}");
        }
    }

    #[test]
    fn nothing_it_emits_is_ever_rejected_by_the_scene_reader() {
        // A spread of shapes people actually type, each round-tripped through the real reader.
        for p in [
            "a ball",
            "two crates",
            "ten cylinders sliding",
            "a crate and a ball and an arm",
            "drop a 5 cm marble from 40 cm at 240 hz",
            "a gold pillar oscillating every 2 s",
            "an SO-101",
            "a robot arm and three boxes falling for 8 seconds",
            "a steel drum orbiting at 1.5 m",
            "crates, balls, cylinders",
        ] {
            let (_, s) = scene(p);
            assert!(
                !s.bodies.is_empty() || !s.robots.is_empty(),
                "{p:?} recorded nothing"
            );
            s.record(|_| None)
                .unwrap_or_else(|e| panic!("{p:?} failed to record: {e}"));
        }
    }
}
