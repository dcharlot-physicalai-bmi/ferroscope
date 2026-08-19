//! `ferroscope scene` — record a described scene.
//!
//! The same thing the MCP server's `scene_record` does, for people rather than agents. The
//! description is JSON either way, so a scene an agent wrote runs here unchanged, and one a
//! person wrote runs there.

use ferroscope_scene::Scene;

pub fn run(scene_path: &str, out: &str, flags: &[&str]) -> Result<bool, String> {
    let mut check_only = false;
    for f in flags {
        match *f {
            "--check" => check_only = true,
            other => return Err(format!("unknown flag {other}")),
        }
    }
    if scene_path == "--schema" {
        println!("{}", Scene::SCHEMA);
        return Ok(true);
    }
    let text = std::fs::read_to_string(scene_path)
        .map_err(|e| format!("cannot read {scene_path}: {e}"))?;

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
    let rec = scene.record(|p| {
        // Relative to the scene file first, because that is what "arm.urdf" written next to it
        // means; then the built-in names, so a hand-written scene may also just say "so101".
        std::fs::read_to_string(base.join(p))
            .ok()
            .or_else(|| crate::builtin::load(p))
    })?;
    std::fs::write(out, &rec.bytes).map_err(|e| format!("cannot write {out}: {e}"))?;

    for n in &rec.notes {
        println!("  note         {n}");
    }
    println!("\nwrote {out} ({} bytes)", rec.bytes.len());
    println!("  spec digest  {}", rec.receipt.spec_digest);
    println!("  trace digest {}", rec.receipt.trace_digest);
    println!(
        "  energy       {:.2} J estimated ({:.1} % compute)",
        rec.total_j,
        rec.compute_fraction * 100.0
    );
    if let Some((z, who)) = &rec.lowest {
        if *z < -1e-6 {
            println!("  clearance    BELOW THE GROUND PLANE: {who} reached {z:.4} m");
        } else {
            println!("  clearance    lowest point {z:.4} m ({who})");
        }
    }
    println!("  open it      https://ferroscope.physicalai-bmi.org/viewer");
    Ok(true)
}
