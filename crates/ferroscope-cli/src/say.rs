//! `ferroscope say` — describe the scene in English.
//!
//! The scene format is JSON because an agent writes it and a machine reads it. A person should
//! not have to. This reads an English phrase, prints what it understood, what it assumed and
//! what it could not use, and records the result.
//!
//! The JSON is always printed. That is the point: this is a starting point you can correct, not
//! a black box — and correcting it means editing ordinary scene JSON that the same reader,
//! the same validator and the same error messages already cover.

use ferroscope_scene::Scene;

pub fn run(phrase: &str, out: &str, flags: &[&str]) -> Result<bool, String> {
    let mut show_json = false;
    let mut dry = false;
    for f in flags {
        match *f {
            "--json" => show_json = true,
            "--check" => dry = true,
            other => return Err(format!("unknown flag {other}")),
        }
    }

    let reading = match ferroscope_phrase::read(phrase) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("{e}");
            return Ok(false);
        }
    };

    println!("\"{phrase}\"");
    for u in &reading.understood {
        println!("  understood   {u}");
    }
    for a in &reading.assumed {
        println!("  assumed      {a}");
    }
    if !reading.ignored.is_empty() {
        // The list that makes this usable. A sentence quietly stripped of "conveyor belt"
        // produces a scene with no conveyor and no explanation, and the reader is left
        // comparing their sentence against a picture, guessing which half arrived.
        println!(
            "  NOT USED     {} — no meaning in the scene vocabulary",
            reading
                .ignored
                .iter()
                .map(|w| format!("\"{w}\""))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    if show_json || dry {
        println!("\n{}", reading.scene_json);
    }

    let scene = Scene::parse(&reading.scene_json).map_err(|problems| {
        // This is a bug in the phrase reader, not in the phrase: it emitted JSON its own scene
        // reader refuses. Say which, rather than blaming the sentence.
        format!(
            "the phrase reader produced a scene the reader refuses, which is a bug:\n{}\n{}",
            problems
                .iter()
                .map(|p| format!("  {}: {}", p.path, p.message))
                .collect::<Vec<_>>()
                .join("\n"),
            reading.scene_json
        )
    })?;

    if dry {
        println!(
            "\n  CHECKS       this scene is valid ({} steps)",
            scene.steps()
        );
        return Ok(true);
    }

    let mut meter = crate::production::start();
    let mut note: Vec<(String, String)> = Vec::new();
    let rec = scene.record_with(crate::builtin::load, || {
        note = meter.production_note();
        note.clone()
    })?;
    std::fs::write(out, &rec.bytes).map_err(|e| format!("cannot write {out}: {e}"))?;
    for n in &rec.notes {
        println!("  note         {n}");
    }
    println!(
        "\nwrote {out} ({} bytes, {} steps)",
        rec.bytes.len(),
        rec.steps
    );
    crate::production::print(&note);
    println!("  trace digest {}", rec.receipt.trace_digest);
    println!(
        "  energy       {:.2} J estimated ({:.1} % compute)",
        rec.total_j,
        rec.compute_fraction * 100.0
    );
    if let Some((z, who)) = &rec.lowest {
        if *z < -1e-6 {
            println!("  clearance    BELOW THE GROUND PLANE: {who} reached {z:.4} m");
        }
    }
    println!("  open it      https://ferroscope.physicalai-bmi.org/viewer");
    if !show_json {
        println!("\n  not what you meant? `--json` prints the scene to edit by hand.");
    }
    Ok(true)
}
