//! `ferroscope urdf` — read a robot description and record it moving.
//!
//! The point of this verb is that it takes *your* URDF. It parses the description, declares one
//! drawable per visual, sweeps every movable joint through its limits, and writes a recording
//! with a receipt and a joule estimate. Then the viewer draws your robot, because the file says
//! what your robot is.

use ferroscope_ledger::Rail;
use ferroscope_receipt::{Precision, RunSpec};
use ferroscope_schema::{Recorder, Stamp};
use ferroscope_urdf::Robot;

pub fn run(urdf_path: &str, out: &str, flags: &[&str]) -> Result<bool, String> {
    let mut steps = 400u64;
    let mut dt_ns = 1_000_000u64;
    let mut check_only = false;
    let mut want_collision = true;
    let mut want_inertial = true;
    let mut each = false;
    let mut i = 0;
    while i < flags.len() {
        match flags[i] {
            "--steps" => {
                steps = flags
                    .get(i + 1)
                    .and_then(|s| s.parse().ok())
                    .ok_or("--steps needs a number")?;
                i += 2;
            }
            "--rate" => {
                let hz: f64 = flags
                    .get(i + 1)
                    .and_then(|s| s.parse().ok())
                    .ok_or("--rate needs a number in Hz")?;
                if hz <= 0.0 {
                    return Err("--rate must be positive".into());
                }
                dt_ns = (1e9 / hz).round() as u64;
                i += 2;
            }
            "--check" => {
                check_only = true;
                i += 1;
            }
            "--no-collision" => {
                want_collision = false;
                i += 1;
            }
            "--no-inertial" => {
                want_inertial = false;
                i += 1;
            }
            "--sweep" => {
                each = match flags.get(i + 1) {
                    Some(&"each") => true,
                    Some(&"all") => false,
                    _ => return Err("--sweep takes `all` or `each`".into()),
                };
                i += 2;
            }
            other => return Err(format!("unknown flag {other}")),
        }
    }

    let text =
        std::fs::read_to_string(urdf_path).map_err(|e| format!("cannot read {urdf_path}: {e}"))?;
    let robot = Robot::parse(&text).map_err(|e| format!("{urdf_path}: {e}"))?;

    let movable: Vec<_> = robot.movable_joints().cloned().collect();
    println!("{urdf_path}");
    println!("  robot        {}", robot.name);
    println!("  root link    {}", robot.root_link().unwrap_or("?"));
    println!(
        "  links        {} ({} visual{})",
        robot.links.len(),
        robot.links.iter().map(|l| l.visuals.len()).sum::<usize>(),
        if robot.links.iter().map(|l| l.visuals.len()).sum::<usize>() == 1 {
            ""
        } else {
            "s"
        }
    );
    println!(
        "  joints       {} total, {} movable",
        robot.joints.len(),
        movable.len()
    );
    for n in &robot.notes {
        println!("  note         {n}");
    }
    let meshes: Vec<&str> = robot
        .links
        .iter()
        .flat_map(|l| l.visuals.iter())
        .filter(|v| !v.mesh.is_empty())
        .map(|v| v.mesh.as_str())
        .collect();
    if !meshes.is_empty() {
        // Say it rather than draw nothing: a mesh visual with no attachment is a hole in the
        // scene, and the reader should hear about it here instead of wondering in the viewer.
        println!(
            "  meshes       {} referenced and NOT attached: {}",
            meshes.len(),
            meshes.join(", ")
        );
        println!("               attach the glTF bytes under those names to draw them");
    }

    // The validator runs before anything else, because a description that is not physically
    // usable is worth saying so about whether or not you asked for a recording.
    let findings = robot.check();
    let failures = findings.iter().filter(|f| f.fails).count();
    if findings.is_empty() {
        println!("\n  CHECKS       all clear");
    } else {
        println!("\n  CHECKS");
        for f in &findings {
            println!(
                "    {} {:<22} {:<26} {}",
                if f.fails { "FAIL" } else { "note" },
                f.kind,
                f.link,
                f.detail
            );
        }
        println!("    {} finding(s), {failures} that fail", findings.len());
    }
    if check_only {
        return Ok(failures == 0);
    }

    let mut rec = Recorder::new(Vec::new(), Precision::Quantized { drop_bits: 12 });
    let t0 = Stamp::sim(0, 0);
    rec.geometry(
        "/scene/ground",
        t0,
        &ferroscope_schema::Geometry::plane("world", "ground", 4.0, 4.0),
    )
    .map_err(|e| e.to_string())?;
    robot
        .declare(&mut rec, t0, "/scene")
        .map_err(|e| e.to_string())?;
    if want_collision {
        robot
            .declare_collision(&mut rec, t0, "/scene")
            .map_err(|e| e.to_string())?;
    }
    if want_inertial {
        robot
            .declare_inertial(&mut rec, t0, "/scene")
            .map_err(|e| e.to_string())?;
    }
    // The findings ride in the recording too, so a reader who only has the file still sees them.
    for f in &findings {
        rec.event(
            "/log",
            t0,
            if f.fails { "error" } else { "warn" },
            &format!("{}: {}: {}", f.kind, f.link, f.detail),
        )
        .map_err(|e| e.to_string())?;
    }

    // Two ways to move a description you are inspecting.
    //
    // `all` drives every joint at once on a phase offset: the whole tree moves, so nothing sits
    // at zero pretending to be rigid. It is also a knot — a kinematic sweep has no collision
    // check, so a real arm folds through its own base and through the floor.
    //
    // `each` drives one joint at a time out of the home pose and back, which is what you
    // actually want when the question is "does this joint go where the file says". Every other
    // joint holds at home, so what you see moving is the joint being asked about.
    for step in 0..steps {
        let t = Stamp::sim(step * dt_ns, step);
        let u = step as f64 / steps as f64;
        let n = movable.len().max(1);
        let q: Vec<(String, f64)> = movable
            .iter()
            .enumerate()
            .map(|(k, j)| {
                let (lo, hi) = j.limits.unwrap_or((-3.0, 3.0));
                let home = 0.0f64.clamp(lo, hi);
                if each {
                    // Joint k owns the k-th slice of the timeline; outside it, it holds home.
                    let slice = u * n as f64 - k as f64;
                    let v = if (0.0..1.0).contains(&slice) {
                        // One full out-and-back through the whole declared range, starting and
                        // ending at home so consecutive joints hand over without a jump.
                        let s = (std::f64::consts::TAU * slice).sin();
                        home + if s >= 0.0 { (hi - home) * s } else { (home - lo) * s }
                    } else {
                        home
                    };
                    (j.name.clone(), v)
                } else {
                    let phase = std::f64::consts::TAU * (u + k as f64 / n as f64);
                    let mid = (lo + hi) * 0.5;
                    // 70 % of the declared range rather than 100 %, to fold through less.
                    (j.name.clone(), mid + (hi - lo) * 0.35 * phase.sin())
                }
            })
            .collect();
        robot
            .log_pose(&mut rec, t, &q, "/scene")
            .map_err(|e| e.to_string())?;
        for (name, v) in &q {
            rec.scalar(&format!("/joints/{name}"), t, *v, "rad")
                .map_err(|e| e.to_string())?;
        }
        // A crude but stated actuation estimate: joint speed against a nominal torque. It is
        // labelled an estimate because nothing here measured a motor.
        let speed: f64 = q
            .iter()
            .map(|(_, v)| (v * std::f64::consts::TAU / (steps as f64 * dt_ns as f64 * 1e-9)).abs())
            .sum();
        rec.energy(
            "/energy/joints",
            t,
            Rail::Actuation,
            "joints",
            4.0 + speed * 0.4,
        )
        .map_err(|e| e.to_string())?;
        rec.energy("/energy/soc", t, Rail::Compute, "soc", 7.8)
            .map_err(|e| e.to_string())?;
    }

    let mut spec = RunSpec::new(format!("urdf:{}", robot.name), 0)
        .dt_ns(dt_ns)
        .steps(steps)
        .integrator("none (kinematic sweep)")
        .solver("forward kinematics")
        .asset(urdf_path, format!("{} bytes", text.len()))
        .build(concat!("ferroscope ", env!("CARGO_PKG_VERSION")));
    for j in &movable {
        let (lo, hi) = j.limits.unwrap_or((-3.0, 3.0));
        spec = spec.config(format!("joint.{}", j.name), format!("{lo}..{hi}"));
    }

    let platform = format!("{}-{}", std::env::consts::ARCH, std::env::consts::OS);
    let (bytes, receipt, quote) = rec.seal(spec, &platform).map_err(|e| e.to_string())?;
    std::fs::write(out, &bytes).map_err(|e| format!("cannot write {out}: {e}"))?;

    println!("\nwrote {out} ({} bytes)", bytes.len());
    println!("  spec digest  {}", receipt.spec_digest);
    println!("  trace digest {}", receipt.trace_digest);
    println!(
        "  energy       {:.2} J estimated ({:.1} % compute)",
        quote.total_j,
        quote.compute_fraction() * 100.0
    );
    println!("  open it      https://ferroscope.physicalai-bmi.org/viewer");
    // Exit 1 when the description has defects, so this is a gate and not just a report.
    Ok(failures == 0)
}
