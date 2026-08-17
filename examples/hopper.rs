//! A complete Ferroscope project: three scenarios, a swept case grid, two suites, and a
//! verdict, in one file with no manifest, no container, and no machine to wait for.
//!
//! ```sh
//! cargo run --release --example hopper -- collect
//! cargo run --release --example hopper -- run --suite smoke
//! cargo run --release --example hopper -- list
//! cargo run --release --example hopper -- show <RUN_ID>
//! cargo run --release --example hopper -- compare <RUN_A> <RUN_B>
//! ```
//!
//! The point of comparison is not the physics, which is a spring-loaded leg in twelve lines.
//! It is everything that surrounds the physics: this file declares what varies, what the task
//! must achieve, and how a reader can tell, and it gets three clocks, an energy ledger, a
//! determinism receipt and a queryable history without asking for any of them.

use ferroscope_run::prelude::*;
use ferroscope_run::Case;

const MASS_KG: f64 = 12.0;
const G: f64 = -9.80665;
const REST_M: f64 = 0.32;

fn main() -> std::process::ExitCode {
    Harness::new()
        .scenario(
            Scenario::new("hop")
                .describe("One leg, one hop. Did it leave the ground and land on its feet?")
                .tags(["locomotion", "smoke"])
                .param("stiffness", 8000.0)
                .param("restitution", 0.4)
                .steps(1_000)
                .dt(1.0 / 1000.0)
                .cases(Case::sweep("stiffness", [4000.0, 8000.0, 16000.0]))
                .body(hop),
        )
        .scenario(
            Scenario::new("hop_grid")
                .describe("The same hop across stiffness and restitution together.")
                .tags(["locomotion", "sweep"])
                .param("stiffness", 8000.0)
                .param("restitution", 0.4)
                .steps(1_000)
                .dt(1.0 / 1000.0)
                .cases(
                    Case::grid(&[
                        ("stiffness", vec![4000.0, 8000.0, 16000.0]),
                        ("restitution", vec![0.2, 0.4, 0.6]),
                    ])
                    .expect("nine cases is under the cap"),
                )
                .body(hop),
        )
        .scenario(
            Scenario::new("budget")
                .describe("Hold a stance for a second and stay inside a joule budget.")
                .tags(["energy", "smoke"])
                .param("budget_j", 30.0)
                .steps(1_000)
                .dt(1.0 / 1000.0)
                .body(budget),
        )
        .suite(
            Suite::new("smoke")
                .describe("The fast path")
                .tags(["smoke"]),
        )
        .suite(
            Suite::new("acceptance")
                .describe("Everything, including the grid")
                .tags(["locomotion", "energy"]),
        )
        .main()
}

/// The hop. `run.tick()` is the only bookkeeping call in the loop, and it is what buys the
/// three clocks, the digest, and the energy integration.
fn hop(run: &mut Run) -> Result<(), Halt> {
    let k = run.param("stiffness");
    let restitution = run.param("restitution");
    if k <= 0.0 {
        return Err(run.fail(format!("stiffness must be positive, got {k}")));
    }

    // A unilateral Kelvin-Voigt contact: a spring that never pulls, with damping derived from
    // the restitution parameter so `restitution` is a real input rather than a label.
    let damping = 0.6 * (1.0 - restitution) * (k * MASS_KG).sqrt();
    // Launched upward from the rest length, which is what makes a hop a hop.
    let (mut z, mut vz) = (REST_M, 1.5f64);
    let mut peak = z;
    let mut floor = z;
    let mut contacts = 0u32;
    let mut in_contact = false;
    let mut worst_penetration = 0.0f64;

    while run.running() {
        let t = run.tick();
        let penetration = (REST_M - z).max(0.0);
        let f = if penetration > 0.0 {
            (k * penetration - damping * vz).max(0.0)
        } else {
            0.0
        };
        if penetration > 0.0 && !in_contact {
            contacts += 1;
        }
        in_contact = penetration > 0.0;
        worst_penetration = worst_penetration.max(penetration);

        vz += (G + f / MASS_KG) * run.dt();
        z += vz * run.dt();
        peak = peak.max(z);
        floor = floor.min(z);

        run.position("/robot/base", t, [0.0, 0.0, z]);
        run.scalar("/control/height", t, z, "m");
        if in_contact {
            run.contact(
                "/robot/contacts",
                t,
                &Contact {
                    body_a: "foot".into(),
                    body_b: "ground".into(),
                    point: [0.0, 0.0, 0.0],
                    normal: [0.0, 0.0, 1.0],
                    force_n: f,
                    penetration_m: penetration,
                },
            );
        }
        // Two rails, so E_task splits the way a real machine does.
        run.energy(
            "/energy/leg",
            t,
            Rail::Actuation,
            "leg",
            (f * vz).abs() * 0.45 + 0.004 * f,
        );
        run.energy("/energy/soc", t, Rail::Compute, "soc", 7.8);
    }

    let quote = run.energy_so_far();
    run.result("peak_height_m", peak);
    run.result("lowest_m", floor);
    run.result("contacts", contacts);
    run.result("worst_penetration_m", worst_penetration);
    run.result("energy_j", quote.total_j);
    run.result("gate_peak_m", 0.36);
    run.result("gate_penetration_m", 0.06);

    // One check per criterion the task defines, each carrying its measurement.
    run.check(
        "left the ground",
        peak > 0.36,
        format!("peak {peak:.4} m > 0.360 m"),
    );
    run.check(
        "leg did not bottom out",
        worst_penetration <= 0.06,
        format!("worst penetration {worst_penetration:.4} m <= 0.0600 m"),
    );
    run.check(
        "bounced at least twice",
        contacts >= 2,
        format!("{contacts} contact intervals >= 2"),
    );
    Ok(())
}

/// A stance hold with a joule budget, so a scenario fails on cost rather than on geometry.
fn budget(run: &mut Run) -> Result<(), Halt> {
    let budget_j = run.param("budget_j");
    while run.running() {
        let t = run.tick();
        // Holding against gravity plus a compute load with a replan spike every 100 ms.
        let hold_w = MASS_KG * -G * 0.06;
        let replan = if run.step() % 100 < 6 { 9.5 } else { 0.0 };
        run.energy("/energy/legs", t, Rail::Actuation, "legs", hold_w);
        run.energy("/energy/soc", t, Rail::Compute, "soc", 7.8 + replan);
        run.scalar("/control/stance_error", t, 0.0, "m");
    }
    let q = run.energy_so_far();
    run.result("energy_j", q.total_j);
    run.result("budget_j", budget_j);
    run.result("compute_fraction", q.compute_fraction());
    run.check(
        "inside the joule budget",
        q.total_j <= budget_j,
        format!("{:.2} J <= {budget_j:.2} J", q.total_j),
    );
    run.check(
        "the energy figure is quotable",
        q.quotable,
        q.coverage.to_string(),
    );
    Ok(())
}
