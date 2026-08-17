//! A synthetic run, so the rest of the CLI has something to be pointed at.
//!
//! It is a one-legged hopper: a controller drives a vertical hop, the leg makes and breaks
//! contact, the actuator draws current in proportion to the force it develops, and the
//! onboard compute draws a near-constant load with a spike each time the planner replans.
//! Small, but it has every ingredient the interface exists for — contact, three clocks, two
//! energy rails, and a determinism receipt.
//!
//! `--drift <step>` injects a divergence at a chosen step, which is how the `diff` verb has
//! something to find. That is the demo the whole design is about: two files, one question,
//! and an answer that names a step number.

use ferroscope_ledger::Rail;
use ferroscope_receipt::{Precision, RunSpec};
use ferroscope_schema::{Contact, Geometry, JointState, Recorder, Stamp};

/// A tiny deterministic generator. Not for cryptography and not for statistics — it exists
/// so the demo has texture without importing anything, and so the same seed gives the same
/// file on every machine, which is the property the demo is about.
struct Lcg(u64);
impl Lcg {
    fn next_f64(&mut self) -> f64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((self.0 >> 11) as f64) / ((1u64 << 53) as f64)
    }
}

#[allow(dead_code)]
pub fn write(out: &str) -> Result<bool, String> {
    write_with(out, &[])
}

pub fn write_with(out: &str, flags: &[&str]) -> Result<bool, String> {
    let mut seed = 42u64;
    let mut drift_at: Option<u64> = None;
    let mut platform = default_platform();
    let mut steps = 1_000u64;
    let mut i = 0;
    while i < flags.len() {
        match flags[i] {
            "--seed" => {
                seed = flags
                    .get(i + 1)
                    .and_then(|s| s.parse().ok())
                    .ok_or("--seed needs a number")?;
                i += 2;
            }
            "--drift" => {
                drift_at = Some(
                    flags
                        .get(i + 1)
                        .and_then(|s| s.parse().ok())
                        .ok_or("--drift needs a step number")?,
                );
                i += 2;
            }
            "--steps" => {
                steps = flags
                    .get(i + 1)
                    .and_then(|s| s.parse().ok())
                    .ok_or("--steps needs a number")?;
                i += 2;
            }
            "--platform" => {
                platform = flags
                    .get(i + 1)
                    .ok_or("--platform needs a string")?
                    .to_string();
                i += 2;
            }
            other => return Err(format!("unknown flag {other}")),
        }
    }

    const DT_NS: u64 = 1_000_000; // 1 kHz control

    let mut rec = Recorder::new(Vec::new(), Precision::Quantized { drop_bits: 12 });
    let mut rng = Lcg(seed);
    let mut wall = 0u64;

    // Hop dynamics, integrated semi-implicitly so the recording has a real trajectory in it
    // rather than a sampled analytic curve.
    // Declare the scene once, so the built-in demo exercises the whole format and the 3-D
    // panel has a hopper to draw rather than a moving dot.
    let t0 = Stamp::sim(0, 0);
    rec.geometry(
        "/scene/ground",
        t0,
        &Geometry::plane("world", "ground", 3.0, 3.0),
    )
    .map_err(|e| e.to_string())?;
    rec.geometry(
        "/scene/body",
        t0,
        &Geometry::boxed("base", "body", [0.22, 0.16, 0.12]).colored([0.81, 0.67, 0.36, 1.0]),
    )
    .map_err(|e| e.to_string())?;

    let mut drift_applied: Option<u64> = None;
    let mut z = 0.35f64;
    let mut vz = 0.0f64;
    // Travelling forward turns the trail, the ghosts and the contact points into information
    // instead of one dot seen from five angles.
    let mut x = -0.26f64;
    let vx = 1.7f64;
    let k = 8_000.0; // leg stiffness, N/m
    let m = 12.0; // body mass, kg
    let g = -9.80665;
    let rest = 0.32; // uncompressed leg length

    for step in 0..steps {
        let sim_ns = step * DT_NS;
        // Wall time runs a little behind sim and jitters, the way a real loop does. This is
        // the drift the third clock exists to expose.
        wall += DT_NS + (rng.next_f64() * 120_000.0) as u64;
        let t = Stamp::at(sim_ns, wall, step);

        let penetration = (rest - z).max(0.0);
        let mut fz = if penetration > 0.0 {
            k * penetration
        } else {
            0.0
        };

        // The injected divergence: one step where the contact force is perturbed by a part
        // in ten thousand. The trajectory never returns to the unperturbed one — but in a
        // dissipative system it does not explode either: it settles at a persistent offset
        // around 1e-9 m, which no plot can show and no eye can catch. That is exactly the
        // class of difference a digest exists to catch.
        //
        // It is applied at the first step *in contact* at or after the requested one: a
        // perturbation of a zero force while the robot is airborne is not a perturbation,
        // and a demo that quietly did nothing would be worse than no demo.
        if drift_applied.is_none() && drift_at.is_some_and(|d| step >= d) && penetration > 0.0 {
            fz *= 1.0001;
            drift_applied = Some(step);
        }

        let az = g + fz / m;
        vz += az * (DT_NS as f64 * 1e-9);
        z += vz * (DT_NS as f64 * 1e-9);
        x += vx * (DT_NS as f64 * 1e-9);
        if z < 0.05 {
            z = 0.05;
            vz = vz.abs() * 0.4;
        }

        // --- Pose and joint state
        rec.transform(
            "/robot/base",
            t,
            "world",
            "base",
            [x, 0.0, z],
            [0.0, 0.0, 0.0, 1.0],
        )
        .map_err(|e| e.to_string())?;

        // The leg is a cylinder from foot to body, so its length IS the compression, and it
        // turns red while in contact.
        let leg_len = (z - 0.02).max(0.01);
        rec.geometry(
            "/scene/leg",
            t,
            &Geometry::cylinder("world", "leg", 0.022, leg_len)
                .at([x, 0.0, leg_len * 0.5])
                .colored(if penetration > 0.0 {
                    [1.0, 0.42, 0.42, 1.0]
                } else {
                    [0.54, 0.63, 0.74, 1.0]
                }),
        )
        .map_err(|e| e.to_string())?;
        rec.geometry(
            "/scene/foot",
            t,
            &Geometry::sphere("world", "foot", 0.03).at([x, 0.0, 0.02]),
        )
        .map_err(|e| e.to_string())?;

        let knee = (rest - z).max(0.0) * 6.0;
        rec.joints(
            "/robot/joints",
            t,
            &JointState {
                names: vec!["hip".into(), "knee".into()],
                position: vec![-knee * 0.5, knee],
                velocity: vec![-vz * 0.5, vz],
                effort: vec![fz * 0.18, fz * 0.32],
            },
        )
        .map_err(|e| e.to_string())?;

        // --- Contact, only while there is one. A contact channel that reports zero-force
        //     contacts every step is how penetration gets hidden.
        if penetration > 0.0 {
            rec.contact(
                "/robot/contacts",
                t,
                &Contact {
                    body_a: "foot".into(),
                    body_b: "ground".into(),
                    point: [x, 0.0, 0.0],
                    normal: [0.0, 0.0, 1.0],
                    force_n: fz,
                    penetration_m: penetration,
                },
            )
            .map_err(|e| e.to_string())?;
        }

        // --- Energy. Actuation power tracks |force x velocity| plus a holding cost; compute
        //     is a steady SoC load with a replan spike every 100 ms. The ratio is the point:
        //     on this machine compute is not a rounding error.
        let mech = (fz * vz).abs();
        let holding = 0.004 * fz;
        rec.energy(
            "/energy/actuation/leg",
            t,
            Rail::Actuation,
            "leg",
            mech * 0.45 + holding,
        )
        .map_err(|e| e.to_string())?;
        let replan = if step % 100 < 6 { 9.5 } else { 0.0 };
        rec.energy("/energy/compute/soc", t, Rail::Compute, "soc", 7.8 + replan)
            .map_err(|e| e.to_string())?;
        rec.energy("/energy/overhead/bus", t, Rail::Overhead, "bus", 1.4)
            .map_err(|e| e.to_string())?;

        // --- A scalar lane and the occasional event.
        rec.scalar("/control/height_error", t, z - 0.35, "m")
            .map_err(|e| e.to_string())?;
        if step % 100 == 0 {
            rec.event("/log", t, "info", &format!("replan {}", step / 100))
                .map_err(|e| e.to_string())?;
        }
    }

    let spec = RunSpec::new("hopper-1leg", seed)
        .dt_ns(DT_NS)
        .steps(steps)
        .integrator("semi-implicit-euler")
        .solver("penalty-contact")
        .asset("hopper.urdf", "demo-fixture")
        .config("gravity_z", "-9.80665")
        .config("leg_stiffness_n_per_m", "8000")
        .config("body_mass_kg", "12")
        .build(concat!("ferroscope ", env!("CARGO_PKG_VERSION")));

    let (bytes, receipt, quote) = rec.seal(spec, &platform).map_err(|e| e.to_string())?;
    std::fs::write(out, &bytes).map_err(|e| format!("cannot write {out}: {e}"))?;

    println!("wrote {out} ({} bytes)", bytes.len());
    println!("  spec digest   {}", receipt.spec_digest);
    println!("  trace digest  {}", receipt.trace_digest);
    println!("  precision     {}", receipt.precision);
    println!(
        "  energy        {:.2} J total  ({:.1} % compute, {:.1} % actuation)",
        quote.total_j,
        quote.compute_fraction() * 100.0,
        if quote.total_j > 0.0 {
            quote.actuation_j / quote.total_j * 100.0
        } else {
            0.0
        }
    );
    match (drift_at, drift_applied) {
        (Some(want), Some(got)) => {
            println!("  drift injected at step {got} (first contact at or after {want})")
        }
        (Some(want), None) => println!(
            "  WARNING: no contact occurred at or after step {want}; no drift was injected"
        ),
        _ => {}
    }
    Ok(true)
}

fn default_platform() -> String {
    format!("{}-{}", std::env::consts::ARCH, std::env::consts::OS)
}
