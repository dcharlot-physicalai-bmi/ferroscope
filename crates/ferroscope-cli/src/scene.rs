//! `ferroscope scene` — record a described scene.
//!
//! The same thing the MCP server's `scene_record` does, for people rather than agents. The
//! description is JSON either way, so a scene an agent wrote runs here unchanged, and one a
//! person wrote runs there.

use ferroscope_receipt::Precision;
use ferroscope_scene::Scene;

pub fn run(scene_path: &str, out: &str, flags: &[&str]) -> Result<bool, String> {
    let mut check_only = false;
    let mut sweep = false;
    // A scene sweeps joints with `sin` and `cos`, and a libm is not the same function on every
    // operating system. The precision the receipt declares is what decides whether two machines
    // can be said to have produced the same run, so it is settable here.
    let mut precision = Precision::Quantized { drop_bits: 12 };
    let mut resolutions: Vec<(String, f64)> = Vec::new();
    let mut i = 0;
    while i < flags.len() {
        match flags[i] {
            "--check" => {
                check_only = true;
                i += 1;
            }
            "--sweep" => {
                sweep = true;
                i += 1;
            }
            "--precision" => {
                precision = crate::demo::parse_precision(flags.get(i + 1).copied())?;
                i += 2;
            }
            "--resolution" => {
                resolutions.push(crate::demo::parse_resolution(flags.get(i + 1).copied())?);
                i += 2;
            }
            other => return Err(format!("unknown flag {other}")),
        }
    }
    if scene_path == "--schema" {
        println!("{}", Scene::SCHEMA);
        return Ok(true);
    }
    let text = std::fs::read_to_string(scene_path)
        .map_err(|e| format!("cannot read {scene_path}: {e}"))?;

    if sweep {
        return sweep_cases(scene_path, &text, out, check_only);
    }

    let scene = match Scene::parse(&text) {
        Ok(s) => s,
        Err(problems) => {
            // Every problem at once, each with its JSON path. Whoever is fixing this — a person
            // or the model that wrote it — should need one pass, not one per mistake.
            eprintln!("{scene_path}: {} problem(s)", problems.len());
            for p in &problems {
                eprintln!("  {}: {}", p.path, p.message);
            }
            eprintln!("\nrun `ferroscope scene --schema` for the shape, defaults and an example");
            return Ok(false);
        }
    };

    println!("{scene_path}");
    println!("  scene        {}", scene.name);
    println!(
        "  timeline     {} s at {} Hz ({} steps)",
        scene.duration_s,
        scene.rate_hz,
        scene.steps()
    );
    for b in &scene.bodies {
        println!("  body         {:<16} {}", b.id, b.motion.describe());
    }
    for r in &scene.robots {
        println!("  robot        {:<16} {}", r.id, r.urdf);
    }
    if check_only {
        println!("\n  CHECKS       this scene is valid");
        return Ok(true);
    }

    // The scene names URDF paths; resolving them relative to the scene file is what a person
    // means when they write "arm.urdf" next to their scene.
    let base = std::path::Path::new(scene_path)
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_default();
    let mut meter = crate::production::start();
    let mut note: Vec<(String, String)> = Vec::new();
    let rec = scene.record_declaring(
        precision,
        &resolutions,
        |p| {
            // Relative to the scene file first, because that is what "arm.urdf" written next to
            // it means; then the built-in names, so a scene may also just say "so101".
            std::fs::read_to_string(base.join(p))
                .ok()
                .or_else(|| crate::builtin::load(p))
        },
        || {
            note = meter.production_note();
            note.clone()
        },
    )?;
    std::fs::write(out, &rec.bytes).map_err(|e| format!("cannot write {out}: {e}"))?;

    for n in &rec.notes {
        println!("  note         {n}");
    }
    println!("\nwrote {out} ({} bytes)", rec.bytes.len());
    crate::production::print(&note);
    println!("  spec digest  {}", rec.receipt.spec_digest);
    println!("  trace digest {}", rec.receipt.trace_digest);
    println!(
        "  energy       {:.2} J estimated ({:.1} % compute)",
        rec.total_j,
        rec.compute_fraction * 100.0
    );
    if let Some((z, who)) = &rec.lowest {
        print_clearance(*z, who);
    }
    println!("  open it      https://ferroscope.physicalai-bmi.org/viewer");
    Ok(true)
}

/// Record every case and report a verdict per case.
///
/// The exit code is the point: a scenario that quietly reports failures is a scenario nobody
/// gates on. One failing check fails the sweep.
fn sweep_cases(path: &str, text: &str, out: &str, check_only: bool) -> Result<bool, String> {
    let suite = match ferroscope_scene::Suite::parse(text) {
        Ok(s) => s,
        Err(problems) => {
            eprintln!("{path}: {} problem(s)", problems.len());
            for p in &problems {
                eprintln!("  {}: {}", p.path, p.message);
            }
            return Ok(false);
        }
    };

    println!("{path}");
    println!("  suite        {}", suite.name);
    println!("  cases        {}", suite.cases.len());
    for c in &suite.checks {
        let bound = match (c.at_least, c.at_most) {
            (Some(lo), Some(hi)) => format!("{lo} .. {hi}"),
            (Some(lo), None) => format!(">= {lo}"),
            (None, Some(hi)) => format!("<= {hi}"),
            _ => String::new(),
        };
        println!("  check        {:<28} {} {bound}", c.name, c.measure.name());
    }
    if check_only {
        println!("\n  CHECKS       this suite is valid");
        return Ok(true);
    }

    // The output path becomes a stem: one recording per case, so a failing case can be opened.
    let stem = out.strip_suffix(".mcap").unwrap_or(out);
    let mut meter = crate::production::start();
    let mut case_notes: Vec<Vec<(String, String)>> = Vec::new();
    let results = suite.run_with(crate::builtin::load, &mut || {
        let n = meter.production_note();
        case_notes.push(n.clone());
        n
    })?;

    // The measured value of every check is printed on a pass as well as a failure. A column of
    // "pass" with no numbers is a table nobody can sanity-check, and the one number a scene-wide
    // column WOULD show is usually the wrong one: a robot dominates the scene minimum, so a
    // crate-scoped verdict sat next to the arm's depth and read as a contradiction.
    println!("\n  {:<26} {:>7} {:>9}  VERDICT", "CASE", "STEPS", "JOULES");
    let mut failed = 0usize;
    for (i, r) in results.iter().enumerate() {
        println!(
            "  {:<26} {:>7} {:>9.2}  {}",
            r.label,
            r.recorded.steps,
            r.recorded.total_j,
            if r.passed() { "pass" } else { "FAIL" }
        );
        for (name, ok, why) in &r.checks {
            println!("      {} {name}: {why}", if *ok { "ok  " } else { "FAIL" });
        }
        if !r.passed() {
            failed += 1;
        }
        let file = format!("{stem}-{i}.mcap");
        std::fs::write(&file, &r.recorded.bytes)
            .map_err(|e| format!("cannot write {file}: {e}"))?;
    }

    println!(
        "\n  {} case(s), {} passed, {} failed",
        results.len(),
        results.len() - failed,
        failed
    );
    // Production across the sweep: each case's note is the counter delta since the previous
    // one, so the honest total is their sum — and a single unmeasured case makes the total
    // unquotable, because a sum with a hole in it reads as smaller than the truth.
    let joules: Vec<f64> = case_notes
        .iter()
        .filter_map(|kv| kv.iter().find(|(k, _)| k == "joules"))
        .filter_map(|(_, v)| v.parse().ok())
        .collect();
    if joules.len() == results.len() && !joules.is_empty() {
        let all_counter = case_notes.iter().all(|kv| {
            kv.iter()
                .any(|(k, v)| k == "basis" && v == "cumulative energy counter")
        });
        println!(
            "  production   {}{:.6} J {} across {} case(s)",
            if all_counter { "" } else { "~" },
            joules.iter().sum::<f64>(),
            if all_counter {
                "measured"
            } else {
                "approximated from instantaneous samples"
            },
            joules.len()
        );
    } else if let Some(kv) = case_notes
        .iter()
        .find(|kv| kv.iter().any(|(k, _)| k == "unavailable"))
    {
        // The total is refused because at least one case went unmeasured; the line worth
        // printing is that hole, not the first case's own joules dressed up as a summary.
        crate::production::print(kv);
    }
    println!(
        "  wrote        {stem}-0.mcap .. {stem}-{}.mcap",
        results.len() - 1
    );
    Ok(failed == 0)
}

/// Report the lowest point a scene reached — and, when it is under the floor, the mount height
/// that fixes it.
///
/// A joint sweep is kinematic: it walks each joint through its declared range and asks nothing
/// about contact, so an arm standing at the origin will put its own gripper under the ground
/// exactly as a real one would if you bolted it to the floor. Saying only "BELOW THE GROUND
/// PLANE" reads as a defect in the tool. Saying how high to mount it turns the same measurement
/// into the next action, and the number is one the run already computed.
pub fn print_clearance(z: f64, who: &str) {
    if z >= -1e-6 {
        println!("  clearance    lowest point {z:.4} m ({who})");
        return;
    }
    println!("  clearance    BELOW THE GROUND PLANE: {who} reached {z:.4} m");
    // A centimetre of margin, rounded up, so the suggestion is a number a person would type.
    let mount = ((-z + 0.01) * 100.0).ceil() / 100.0;
    // A robot's links are recorded as "<robot id>/<link>", so the thing to mount is named in
    // the measurement itself.
    match who.split_once('/').map(|(id, _)| id) {
        Some(id) => println!(
            "               a joint sweep ignores contact, so a robot at the origin reaches \
             under the floor. Mount it: \"at\":[0,0,{mount:.2}] on robot \"{id}\"."
        ),
        None => println!(
            "               raise it by {mount:.2} m, or lower the ground, if the scene is \
             meant to stay above it."
        ),
    }
}
