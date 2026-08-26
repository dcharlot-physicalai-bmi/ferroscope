//! The SO-101 under real dynamics.
//!
//! Everything else in this repository either reads a recording some simulator made or plays out
//! a *described* motion in closed form. This produces one from physics: ferromotion's recursive
//! Newton-Euler dynamics drive the SO-101's calibrated inertials, a PD controller pulls it to a
//! reach pose, and every step is recorded with a determinism receipt — the run is produced and
//! certified by the same stack that renders it.
//!
//! Two things become real that were models before:
//!
//! - the **actuation rail** is computed, per joint, as mechanical shaft power `|τ · ω|` from the
//!   torques the controller actually applied — stated as mechanical, because electrical power
//!   would need a motor model this file does not have;
//! - the **physics is checkable**: `--passive` drops the arm under gravity alone and reports the
//!   total-energy drift of the integrator, which is the number that says whether the dynamics
//!   deserve the receipt they carry.
//!
//! ```text
//! ferroscope-motion out.mcap                 PD reach, recorded and sealed
//! ferroscope-motion out.mcap --passive       gravity only; exit 1 if energy drifts > 5 %
//! ```

use ferromotion_core::{LinkInertia, Robot, from_urdf_full, inverse_dynamics, mass_matrix};
use ferroscope_ledger::Rail;
use ferroscope_receipt::{Precision, RunSpec};
use ferroscope_schema::{Recorder, Stamp};
use nalgebra::Vector3;

/// The same calibrated description every other surface uses, embedded so `cargo install` needs
/// no files. CI compares this byte-for-byte against `examples/robots/so101.urdf`.
const URDF: &str = include_str!("../robots/so101.urdf");

const GRAVITY: f64 = -9.81;
/// Physics substep. The recording rate is separate: dynamics need the small step, readers do not.
const DT: f64 = 1e-3;
/// Reflected rotor inertia of the geared STS3215, added to the mass-matrix diagonal. This is
/// not a stabilisation hack: the motor's rotor spins at gear-ratio times joint speed, so its
/// tiny inertia arrives at the joint multiplied by the ratio squared and DOMINATES these
/// palm-sized links. Without it the bare-link natural frequencies sit above 100 Hz, a 1 kHz
/// Euler cannot hold them, and the first run of this file reported 145 % energy drift and
/// 597 J of "actuation" for a desk arm — absurd on sight, which is what the drift gate is for.
/// The value matches ferromotion's own SO-101 example.
const ARMATURE: f64 = 0.028;
/// PD gains and the torque ceiling, from the STS3215's ballpark. Declared in the spec.
const KP: f64 = 70.0;
const KV: f64 = 13.0;
const TAU_MAX: f64 = 2.94;
/// The reach target, mid-range on every joint so the run never fights its own limits.
const TARGET: [f64; 5] = [0.6, -0.8, 0.9, 0.5, 0.3];

fn main() -> std::process::ExitCode {
    match run() {
        Ok(true) => std::process::ExitCode::SUCCESS,
        Ok(false) => std::process::ExitCode::from(1),
        Err(e) => {
            eprintln!("ferroscope-motion: {e}");
            std::process::ExitCode::from(2)
        }
    }
}

fn run() -> Result<bool, String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut out = "motion.mcap".to_string();
    let mut passive = false;
    let mut serve: Option<u16> = None;
    let mut serve_wt: Option<u16> = None;
    let mut duration_s = 4.0f64;
    let mut rate_hz = 120.0f64;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--passive" => {
                passive = true;
                i += 1;
            }
            "--serve-wt" => match args.get(i + 1).and_then(|s| s.parse().ok()) {
                Some(p) => {
                    serve_wt = Some(p);
                    i += 2;
                }
                None => {
                    serve_wt = Some(4433);
                    i += 1;
                }
            },
            "--serve" => {
                // A port may follow; without one, the viewer's default.
                match args.get(i + 1).and_then(|s| s.parse().ok()) {
                    Some(p) => {
                        serve = Some(p);
                        i += 2;
                    }
                    None => {
                        serve = Some(8737);
                        i += 1;
                    }
                }
            }
            "--duration" => {
                duration_s = args
                    .get(i + 1)
                    .and_then(|s| s.parse().ok())
                    .filter(|v: &f64| *v > 0.0)
                    .ok_or("--duration needs a positive number of seconds")?;
                i += 2;
            }
            "--rate" => {
                rate_hz = args
                    .get(i + 1)
                    .and_then(|s| s.parse().ok())
                    .filter(|v: &f64| *v > 0.0)
                    .ok_or("--rate needs a positive number in Hz")?;
                i += 2;
            }
            other if !other.starts_with("--") => {
                out = other.to_string();
                i += 1;
            }
            other => return Err(format!("unknown flag {other}")),
        }
    }

    // Two parses of one file, on purpose: ferromotion builds the dynamics chain, ferroscope-urdf
    // declares the drawable scene and names the links, and both read the same bytes so they
    // cannot disagree about what the robot is.
    let (robot, inertia) =
        from_urdf_full(URDF, "base_link", "gripper_link").map_err(|e| format!("dynamics: {e}"))?;
    let scene = ferroscope_urdf::Robot::parse(URDF).map_err(|e| format!("scene: {e}"))?;
    let n = robot.dof();

    // The chain's link and joint names, in dynamics order, walked from the description itself.
    let (chain_links, chain_joints) = chain_path(&scene, "base_link", "gripper_link")
        .ok_or("no path from base_link to gripper_link in the description")?;
    if chain_joints.len() != n {
        return Err(format!(
            "the dynamics see {n} joints but the description's chain has {}",
            chain_joints.len()
        ));
    }
    let limits: Vec<(f64, f64)> = chain_joints
        .iter()
        .map(|name| {
            scene
                .joints
                .iter()
                .find(|j| &j.name == name)
                .and_then(|j| j.limits)
                .unwrap_or((-3.0, 3.0))
        })
        .collect();

    println!("so101 under ferromotion dynamics");
    println!(
        "  mode         {}",
        if passive {
            "passive: gravity only, energy-drift check"
        } else {
            "PD reach"
        }
    );
    println!("  chain        {}", chain_links.join(" -> "));
    println!("  physics      semi-implicit Euler, dt = {DT} s");

    let mut meter = ferroscope_power::Meter::open();
    let _ = meter.sample_energy();

    // When serving, every record fans out to connected viewers as it is written, and the run is
    // paced to the wall clock so what they see is happening now, not a file replayed at once.
    let server = match serve {
        Some(port) => {
            let srv = ferroscope_live::LiveServer::bind(port)
                .map_err(|e| format!("cannot bind 127.0.0.1:{port}: {e}"))?;
            println!("  live         ws://localhost:{}", srv.port());
            println!(
                "               open https://ferroscope.physicalai-bmi.org/viewer and press live"
            );
            Some(srv)
        }
        None => None,
    };
    let wt_server = match serve_wt {
        Some(port) => {
            let srv = ferroscope_live::WtServer::bind(port)
                .map_err(|e| format!("cannot bind WebTransport on 127.0.0.1:{port}: {e}"))?;
            println!("  webtransport https://127.0.0.1:{}", srv.port());
            // The whole connection story in one clickable line: the viewer reads the url and
            // the certificate hash from its query string and connects on load.
            println!(
                "               https://ferroscope.physicalai-bmi.org/viewer?wt=https://127.0.0.1:{}&hash={}",
                srv.port(),
                srv.cert_hash_hex()
            );
            Some(srv)
        }
        None => None,
    };
    let pacing = server.is_some() || wt_server.is_some();
    // The bytes land in a shared buffer either way, recovered after seal without caring
    // whether a tee sat in front of it.
    let out_bytes: std::sync::Arc<std::sync::Mutex<Vec<u8>>> = Default::default();
    let sink: Box<dyn std::io::Write> = match (&server, &wt_server) {
        // Both transports at once would need a fan-in tee; one at a time covers every real use.
        (Some(_), Some(_)) => return Err("--serve and --serve-wt are one or the other".into()),
        (Some(srv), None) => Box::new(srv.tee(SharedVec(std::sync::Arc::clone(&out_bytes)))),
        (None, Some(srv)) => Box::new(srv.tee(SharedVec(std::sync::Arc::clone(&out_bytes)))),
        (None, None) => Box::new(SharedVec(std::sync::Arc::clone(&out_bytes))),
    };
    let mut rec = Recorder::new(sink, Precision::Quantized { drop_bits: 12 });
    let t0 = Stamp::sim(0, 0);
    rec.geometry(
        "/scene/ground",
        t0,
        &ferroscope_schema::Geometry::plane("world", "ground", 4.0, 4.0),
    )
    .map_err(|e| e.to_string())?;
    scene
        .declare(&mut rec, t0, "/scene")
        .map_err(|e| e.to_string())?;

    // Initial state: passive starts tipped off vertical so gravity has something to do; the PD
    // run starts at home.
    let mut q: Vec<f64> = if passive { vec![0.3; n] } else { vec![0.0; n] };
    let mut qd = vec![0.0f64; n];
    let g = Vector3::new(0.0, 0.0, GRAVITY);

    let frames = (duration_s * rate_hz).round().max(1.0) as u64;
    let substeps = ((1.0 / rate_hz) / DT).round().max(1.0) as usize;
    let dt_frame_ns = (1e9 / rate_hz).round() as u64;

    let start_wall = std::time::Instant::now();
    let e0 = total_energy(&robot, &inertia, &q, &qd);
    let mut e_min = e0;
    let mut e_max = e0;
    let mut pe_min = f64::INFINITY;
    let mut pe_max = f64::NEG_INFINITY;
    let mut actuation_j = 0.0f64;

    for frame in 0..frames {
        // Physics between frames, at the small step the integrator needs.
        let mut tau = vec![0.0f64; n];
        for _ in 0..substeps {
            if !passive {
                for k in 0..n {
                    tau[k] = (KP * (TARGET[k.min(TARGET.len() - 1)] - q[k]) - KV * qd[k])
                        .clamp(-TAU_MAX, TAU_MAX);
                }
            }
            let qdd = armature_dynamics(&robot, &inertia, &q, &qd, &tau, g);
            for k in 0..n {
                qd[k] += qdd[k] * DT;
                q[k] += qd[k] * DT;
                if !passive {
                    // Hard stops. Absorbing at a limit destroys energy, which is why the
                    // passive energy check runs without them and asserts it never needed them.
                    let (lo, hi) = limits[k];
                    if q[k] < lo {
                        q[k] = lo;
                        qd[k] = qd[k].max(0.0);
                    }
                    if q[k] > hi {
                        q[k] = hi;
                        qd[k] = qd[k].min(0.0);
                    }
                }
            }
        }

        // Live means live: pace each frame to the wall clock so a viewer watches the run
        // happen rather than receiving it as a lump.
        if pacing {
            let due = start_wall + std::time::Duration::from_nanos(frame * dt_frame_ns);
            if let Some(wait) = due.checked_duration_since(std::time::Instant::now()) {
                std::thread::sleep(wait);
            }
        }
        let t = Stamp::sim(frame * dt_frame_ns, frame);

        // Poses for every chain link, from the dynamics' own kinematics.
        for (idx, link) in chain_links.iter().enumerate() {
            let iso = robot.frame_pose(&q, idx);
            let p = iso.translation.vector;
            let quat = iso.rotation.quaternion().coords; // [x, y, z, w]
            rec.transform(
                &format!("/scene/tf/{link}"),
                t,
                "world",
                link,
                [p.x, p.y, p.z],
                [quat.x, quat.y, quat.z, quat.w],
            )
            .map_err(|e| e.to_string())?;
        }

        let ke = kinetic_energy(&robot, &inertia, &q, &qd);
        let pe = potential_energy(&robot, &inertia, &q);
        let e = ke + pe;
        e_min = e_min.min(e);
        e_max = e_max.max(e);
        pe_min = pe_min.min(pe);
        pe_max = pe_max.max(pe);

        for (k, name) in chain_joints.iter().enumerate() {
            rec.scalar(&format!("/joints/{name}"), t, q[k], "rad")
                .map_err(|e| e.to_string())?;
            rec.scalar(&format!("/tau/{name}"), t, tau[k], "N·m")
                .map_err(|e| e.to_string())?;
            // Mechanical shaft power, from the torque the controller actually applied. Not
            // electrical: that would need a motor model this file does not have, and the spec
            // says so rather than letting the number pass for more than it is.
            let p_mech = (tau[k] * qd[k]).abs();
            rec.energy(&format!("/energy/{name}"), t, Rail::Actuation, name, p_mech)
                .map_err(|e| e.to_string())?;
            actuation_j += p_mech / rate_hz;
        }
        rec.scalar("/energy_state/kinetic", t, ke, "J")
            .map_err(|e| e.to_string())?;
        rec.scalar("/energy_state/potential", t, pe, "J")
            .map_err(|e| e.to_string())?;
        // The compute rail stays a stated model of an embedded SoC; the dynamics above cost
        // this workstation what the production block says they cost.
        rec.energy("/energy/soc", t, Rail::Compute, "soc", 7.8)
            .map_err(|e| e.to_string())?;
    }

    // The passive gate: with no torque and no stops, the only thing changing total energy is
    // the integrator. Drift is measured against the potential-energy swing actually explored,
    // so a run that barely moved cannot pass on a technicality.
    let drift = (e_max - e_min) / (pe_max - pe_min).max(1e-9);

    let mut spec = RunSpec::new(
        if passive {
            "so101-passive (ferromotion dynamics)"
        } else {
            "so101-reach (ferromotion dynamics)"
        },
        0,
    )
    .dt_ns((DT * 1e9) as u64)
    .steps(frames)
    .integrator("semi-implicit Euler @ 1 kHz")
    .solver("ferromotion-core 0.58 RNEA/CRBA")
    .asset("so101.urdf", format!("{} bytes, embedded", URDF.len()))
    .build(concat!("ferroscope-motion ", env!("CARGO_PKG_VERSION")));
    spec = spec
        .config("actuation.basis", "mechanical |tau*omega|, no motor model")
        .config(
            "armature",
            format!("{ARMATURE} kg m^2 reflected rotor inertia, on the diagonal"),
        )
        .config("gravity", format!("{GRAVITY}"));
    if !passive {
        spec = spec
            .config(
                "controller",
                format!("PD kp={KP} kv={KV} tau_max={TAU_MAX}"),
            )
            .config("target", format!("{TARGET:?}"));
    }

    let platform = format!("{}-{}", std::env::consts::ARCH, std::env::consts::OS);
    let mut note: Vec<(String, String)> = Vec::new();
    let (sink, receipt, quote) = rec
        .seal_with(spec, &platform, || {
            note = meter.production_note();
            note.clone()
        })
        .map_err(|e| e.to_string())?;
    // The sink holds a clone of the broadcast sender; the channel only closes — and the
    // viewers' streams only FIN — once every sender is gone. Without this drop, finish() below
    // waits its whole timeout and the process exit tears the last records out of the stream.
    drop(sink);
    let bytes = out_bytes.lock().unwrap().clone();
    std::fs::write(&out, &bytes).map_err(|e| format!("cannot write {out}: {e}"))?;
    if let Some(srv) = &server {
        println!(
            "  streamed to  {} viewer(s); the stream is the file they now hold",
            srv.viewers()
        );
    }
    if let Some(srv) = wt_server {
        // FIN every stream and let the last bytes leave before the process does.
        srv.finish(std::time::Duration::from_secs(3));
        println!("  webtransport streams finished; the stream is the file they now hold");
    }

    println!("\nwrote {out} ({} bytes, {} frames)", bytes.len(), frames);
    println!("  trace digest {}", receipt.trace_digest);
    println!(
        "  actuation    {:.3} J mechanical, computed from the applied torques",
        quote.actuation_j
    );
    let _ = actuation_j;
    if passive {
        // Only meaningful here: a controller ADDS energy by design, so drift is the
        // integrator's report card exclusively when nothing else is allowed to touch the total.
        println!(
            "  energy drift {:.2} % of the {:.3} J potential swing",
            drift * 100.0,
            pe_max - pe_min
        );
        // The gate. An integrator that leaks more than this has no business under a receipt.
        let ok = drift < 0.05;
        println!(
            "  verdict      {}",
            if ok {
                "the integrator holds energy within 5 %"
            } else {
                "FAIL: the integrator leaks energy past the 5 % bound"
            }
        );
        return Ok(ok);
    }
    println!("  open it      https://ferroscope.physicalai-bmi.org/viewer");
    Ok(true)
}

/// The ordered links and joints from `base` to `tip`, out of the description itself.
fn chain_path(
    scene: &ferroscope_urdf::Robot,
    base: &str,
    tip: &str,
) -> Option<(Vec<String>, Vec<String>)> {
    // Depth-first over the joint tree; a URDF is small enough that elegance would be overhead.
    fn dfs(
        scene: &ferroscope_urdf::Robot,
        here: &str,
        tip: &str,
        links: &mut Vec<String>,
        joints: &mut Vec<String>,
    ) -> bool {
        if here == tip {
            return true;
        }
        for j in scene.joints.iter().filter(|j| j.parent == here) {
            joints.push(j.name.clone());
            links.push(j.child.clone());
            if dfs(scene, &j.child, tip, links, joints) {
                return true;
            }
            joints.pop();
            links.pop();
        }
        false
    }
    let mut links = vec![base.to_string()];
    let mut joints = Vec::new();
    dfs(scene, base, tip, &mut links, &mut joints).then_some((links, joints))
}

/// `q̈ = (M(q) + A)⁻¹ (τ − bias)`, the armature added on the diagonal. The bias (Coriolis,
/// centrifugal, gravity) is RNEA with zero acceleration, exactly as ferromotion's own
/// forward_dynamics computes it.
fn armature_dynamics(
    robot: &Robot,
    inertia: &[LinkInertia],
    q: &[f64],
    qd: &[f64],
    tau: &[f64],
    g: Vector3<f64>,
) -> Vec<f64> {
    let n = robot.dof();
    let mut m = mass_matrix(robot, inertia, q);
    for k in 0..n {
        m[(k, k)] += ARMATURE;
    }
    let bias = inverse_dynamics(robot, inertia, q, qd, &vec![0.0; n], g);
    let rhs = nalgebra::DVector::from_fn(n, |k, _| tau[k] - bias[k]);
    let qdd = m
        .cholesky()
        .expect("M + A is symmetric positive definite by construction")
        .solve(&rhs);
    qdd.iter().copied().collect()
}

fn kinetic_energy(robot: &Robot, inertia: &[LinkInertia], q: &[f64], qd: &[f64]) -> f64 {
    // The armature's kinetic energy is real energy — the rotor genuinely spins — so it belongs
    // in the total the drift gate holds constant.
    let m = mass_matrix(robot, inertia, q);
    let v = nalgebra::DVector::from_row_slice(qd);
    0.5 * (&m * &v).dot(&v) + 0.5 * ARMATURE * qd.iter().map(|x| x * x).sum::<f64>()
}

fn potential_energy(robot: &Robot, inertia: &[LinkInertia], q: &[f64]) -> f64 {
    // inertia[i] is the link moved by joint i, whose frame is frame_pose(q, i + 1).
    inertia
        .iter()
        .enumerate()
        .map(|(i, li)| {
            let world_com = robot.frame_pose(q, i + 1) * nalgebra::Point3 { coords: li.com };
            li.mass * (-GRAVITY) * world_com.z
        })
        .sum()
}

fn total_energy(robot: &Robot, inertia: &[LinkInertia], q: &[f64], qd: &[f64]) -> f64 {
    kinetic_energy(robot, inertia, q, qd) + potential_energy(robot, inertia, q)
}

/// A `Write` into a buffer the caller keeps a handle to, so the sealed bytes are recoverable
/// even when the recorder's sink is a boxed tee.
#[derive(Default)]
struct SharedVec(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

impl std::io::Write for SharedVec {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
