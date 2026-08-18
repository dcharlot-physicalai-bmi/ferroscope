//! `ferroscope power` — what this machine will tell you, and what a command actually cost.
//!
//! The energy ledger's claim is that its joules come from measured power integrated over the run,
//! never a datasheet TDP. Every recording written by `demo`, `urdf` and `scene` carries a
//! *modelled* compute rail, clearly labelled as an estimate. This verb is the one that measures:
//! it samples the machine's own counters while a command runs and writes a recording whose
//! compute rail is real.
//!
//! When the machine will not say — which on macOS without `sudo` is always, and on Linux without
//! root is usual — it writes no joules at all rather than a confident zero, and says why.

use ferroscope_ledger::Rail;
use ferroscope_power::Meter;
use ferroscope_receipt::{Precision, RunSpec};
use ferroscope_schema::{Recorder, Stamp};
use std::time::{Duration, Instant};

pub fn run(flags: &[&str]) -> Result<bool, String> {
    let mut out: Option<&str> = None;
    let mut hz = 10.0f64;
    let mut cmd: Vec<&str> = Vec::new();
    let mut i = 0;
    while i < flags.len() {
        match flags[i] {
            "--out" => {
                out = Some(flags.get(i + 1).ok_or("--out needs a path")?);
                i += 2;
            }
            "--rate" => {
                hz = flags
                    .get(i + 1)
                    .and_then(|s| s.parse().ok())
                    .filter(|v: &f64| *v > 0.0)
                    .ok_or("--rate needs a positive number in Hz")?;
                i += 2;
            }
            "--" => {
                cmd = flags[i + 1..].to_vec();
                break;
            }
            other => return Err(format!("unknown flag {other}")),
        }
    }

    let mut meter = Meter::open();
    println!("power source");
    println!("  {}", meter.describe());
    if !meter.is_measuring() {
        // Said once, plainly, before anything else happens. Everything below is shaped by it.
        println!("\n  No joules will be reported. A measurement nobody can stand behind is worse");
        println!(
            "  than an absent one, which is the same rule the ledger's coverage verdict applies."
        );
    }

    if cmd.is_empty() {
        if !meter.is_measuring() {
            println!(
                "\n  run `ferroscope power -- <command>` under sufficient privilege to measure one"
            );
        } else {
            println!("\n  run `ferroscope power -- <command>` to measure what a command costs");
        }
        // Reporting what the machine offers is a successful answer either way: the question was
        // "what can you measure", and "nothing, because X" is an answer to it.
        return Ok(true);
    }

    let dt = Duration::from_secs_f64(1.0 / hz);
    let mut rec = Recorder::new(Vec::new(), Precision::Quantized { drop_bits: 12 });
    let t0 = Instant::now();

    println!("\nrunning: {}", cmd.join(" "));
    let mut child = std::process::Command::new(cmd[0])
        .args(&cmd[1..])
        .spawn()
        .map_err(|e| format!("cannot start {}: {e}", cmd[0]))?;

    // Prime the counter: a cumulative register has nothing to difference against on the first
    // read, so that read is spent before the clock starts mattering.
    let _ = meter.sample();

    let mut step = 0u64;
    let mut samples = 0usize;
    let mut peak_w = 0.0f64;
    let status = loop {
        std::thread::sleep(dt);
        let elapsed = t0.elapsed();
        let t = Stamp::sim(elapsed.as_nanos() as u64, step);

        if let Ok(Some(w)) = meter.sample() {
            samples += 1;
            peak_w = peak_w.max(w);
            for (name, dw) in meter.by_domain() {
                rec.energy(&format!("/energy/{name}"), t, Rail::Compute, name, *dw)
                    .map_err(|e| e.to_string())?;
            }
            if meter.by_domain().is_empty() {
                rec.energy("/energy/cpu", t, Rail::Compute, "cpu", w)
                    .map_err(|e| e.to_string())?;
            }
        }
        step += 1;

        match child.try_wait().map_err(|e| e.to_string())? {
            Some(s) => break s,
            None => continue,
        }
    };

    let wall = t0.elapsed();
    println!(
        "\n  exit         {}",
        status
            .code()
            .map(|c| c.to_string())
            .unwrap_or_else(|| "signal".into())
    );
    println!("  wall         {:.3} s", wall.as_secs_f64());
    println!("  samples      {samples}");

    if samples == 0 {
        println!("  energy       NOT MEASURED: {}", meter.describe());
        // The command ran and its exit status is the answer the caller wanted; the missing
        // measurement is reported, not converted into a failure of the command.
        return Ok(status.success());
    }
    println!("  peak         {peak_w:.3} W");

    let spec = RunSpec::new(format!("power:{}", cmd.join(" ")), 0)
        .steps(step)
        .integrator("none (measured)")
        .solver("none")
        .config("source", meter.describe())
        .config("sample_rate_hz", format!("{hz}"))
        .build(concat!("ferroscope ", env!("CARGO_PKG_VERSION")));

    let platform = format!("{}-{}", std::env::consts::ARCH, std::env::consts::OS);
    let (bytes, receipt, quote) = rec.seal(spec, &platform).map_err(|e| e.to_string())?;

    println!(
        "  E_compute    {:.3} J  (measured, {} samples)",
        quote.compute_j, samples
    );
    println!("  mean power   {:.3} W", quote.mean_power_w());
    println!("  coverage     {}", quote.coverage);
    if !quote.quotable {
        println!("  → reported, but the sampling cannot support it as a measurement.");
    }

    if let Some(path) = out {
        std::fs::write(path, &bytes).map_err(|e| format!("cannot write {path}: {e}"))?;
        println!("\nwrote {path} ({} bytes)", bytes.len());
        println!("  trace digest {}", receipt.trace_digest);
    }
    Ok(status.success())
}
