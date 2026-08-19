//! The Ferroscope CLI.
//!
//! Six verbs, no configuration file, no daemon, no account:
//!
//! ```text
//! ferroscope inspect run.mcap          what is in this recording
//! ferroscope verify  run.mcap          does it still stand behind its own receipt
//! ferroscope energy  run.mcap          E_task = E_compute + E_actuation
//! ferroscope diff    a.mcap b.mcap     did the replay reproduce the run, and where not
//! ferroscope export  run.mcap out.json a viewer bundle, for the browser
//! ferroscope demo    out.mcap          write a synthetic run to try the above on
//! ```

use std::process::ExitCode;

mod builtin;
mod demo;
mod glb;
mod power;
mod say;
mod scene;
mod urdf;

use ferroscope_ledger::Rail;
use ferroscope_receipt::{compare, digests_agree, Tolerance, Verdict};
use ferroscope_schema::{bundle, mcap, trace_from, verify};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let argv: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    match argv.as_slice() {
        [] | ["-h"] | ["--help"] | ["help"] => {
            usage();
            ExitCode::SUCCESS
        }
        ["--version"] | ["-V"] => {
            println!("ferroscope {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        ["inspect", file] => run(cmd_inspect(file)),
        ["verify", file] => run(cmd_verify(file)),
        ["energy", file] => run(cmd_energy(file)),
        ["diff", a, b, rest @ ..] => run(cmd_diff(a, b, rest)),
        ["export", file, out] => run(cmd_export(file, out)),
        ["demo", rest @ ..] if !rest.is_empty() => {
            let (out, flags) = split_out(rest, "demo.mcap");
            run(demo::write_with(out, flags))
        }
        ["power", rest @ ..] => run(power::run(rest)),
        ["say", phrase, rest @ ..] => {
            let (out, flags) = split_out(rest, "scene.mcap");
            run(say::run(phrase, out, flags))
        }
        ["scene", src, rest @ ..] => {
            let (out, flags) = split_out(rest, "scene.mcap");
            run(scene::run(src, out, flags))
        }
        ["urdf", src, rest @ ..] => {
            let (out, flags) = split_out(rest, "robot.mcap");
            run(urdf::run(src, out, flags))
        }
        _ => {
            eprintln!("ferroscope: unrecognized arguments: {}", args.join(" "));
            usage();
            ExitCode::from(2)
        }
    }
}

/// Split an optional leading output path from the flags that follow it.
///
/// The output path is optional, so `urdf robot.urdf --check` must not bind `--check` as the
/// file to write. Only the *first* token can be the path, and only when it does not start with
/// `--`: that keeps a flag's own value (`--steps 400`) from being mistaken for a filename.
fn split_out<'a>(rest: &'a [&'a str], default: &'a str) -> (&'a str, &'a [&'a str]) {
    match rest {
        [first, tail @ ..] if !first.starts_with("--") => (first, tail),
        _ => (default, rest),
    }
}

fn run(r: Result<bool, String>) -> ExitCode {
    match r {
        Ok(true) => ExitCode::SUCCESS,
        // A false verdict is not an error: the tool worked, the run did not reproduce.
        // CI needs to tell those apart, so they get different exit codes.
        Ok(false) => ExitCode::from(1),
        Err(e) => {
            eprintln!("ferroscope: {e}");
            ExitCode::from(2)
        }
    }
}

fn usage() {
    println!(
        "\
ferroscope {v}, the open interface layer for physical AI

USAGE
  ferroscope inspect <run.mcap>            topics, schemas, clocks, receipt
  ferroscope verify  <run.mcap>            recompute the receipt from the file itself
  ferroscope energy  <run.mcap>            E_task = E_compute + E_actuation
  ferroscope diff    <a.mcap> <b.mcap>     did the replay reproduce the run
                     [--abs <f>] [--rel <f>]
  ferroscope export  <run.mcap> <out.json> viewer bundle for the browser
  ferroscope demo    <out.mcap>            write a synthetic run
                     [--seed <n>] [--steps <n>] [--drift <step>] [--platform <s>]
  ferroscope urdf    <robot.urdf> <out.mcap>  record YOUR robot, and check its description
                     [--check] [--steps <n>] [--rate <hz>] [--sweep all|each]
                     [--no-collision] [--no-inertial] [--meshes <dir>]
  ferroscope scene   <scene.json> <out.mcap>  record a DESCRIBED scene
                     [--check]   ·   --schema prints the scene format
  ferroscope say     \"<phrase>\" [<out.mcap>]  describe the scene in ENGLISH
                     [--json] [--check]
  ferroscope power   [-- <command>]         what this machine can measure, and what
                     [--out <run.mcap>] [--rate <hz>]    a command actually cost

EXIT CODES
  0  the answer is yes
  1  the answer is no (verify failed, runs diverged)
  2  the tool could not answer (bad file, bad arguments)

Recordings are plain MCAP. They also open in any other MCAP viewer, and in the browser
viewer at https://ferroscope.physicalai-bmi.org/viewer (no upload, no account).
{home}",
        v = env!("CARGO_PKG_VERSION"),
        home = env!("CARGO_PKG_HOMEPAGE"),
    )
}

fn slurp(path: &str) -> Result<Vec<u8>, String> {
    std::fs::read(path).map_err(|e| format!("cannot read {path}: {e}"))
}

// ---------------------------------------------------------------------------

fn cmd_inspect(path: &str) -> Result<bool, String> {
    let bytes = slurp(path)?;
    let log = mcap::read(&bytes).map_err(|e| e.to_string())?;

    println!("{path}");
    println!("  profile     {}", log.profile);
    println!("  library     {}", log.library);
    println!("  messages    {}", log.messages.len());
    if let Some((t0, t1)) = log.time_span() {
        println!(
            "  sim span    {:.3} s  ({} → {} ns)",
            (t1 - t0) as f64 * 1e-9,
            t0,
            t1
        );
    }

    println!("\n  {:<32} {:>8}  SCHEMA", "TOPIC", "MSGS");
    let mut rows: Vec<(String, usize, String)> = log
        .channels
        .iter()
        .map(|c| {
            let n = log.messages.iter().filter(|m| m.channel_id == c.id).count();
            let s = log
                .schema(c.schema_id)
                .map(|s| s.name.clone())
                .unwrap_or_else(|| "(none)".into());
            (c.topic.clone(), n, s)
        })
        .collect();
    rows.sort_by_key(|r| std::cmp::Reverse(r.1));
    for (topic, n, schema) in rows {
        println!("  {topic:<32} {n:>8}  {schema}");
    }

    // The three-clock report: this is what a single log_time cannot tell you.
    let mut max_lag = 0i64;
    let mut lag_at = 0u64;
    for m in &log.messages {
        let lag = m.publish_time as i64 - m.log_time as i64;
        if lag.abs() > max_lag.abs() {
            max_lag = lag;
            lag_at = m.log_time;
        }
    }
    if max_lag != 0 {
        println!(
            "\n  clocks      worst wall−sim drift {:.3} ms at sim t={:.3} s",
            max_lag as f64 * 1e-6,
            lag_at as f64 * 1e-9
        );
    }

    if let Some(kv) = log.metadata_block(ferroscope_schema::RECEIPT_BLOCK) {
        println!("\n  receipt");
        for (k, v) in kv {
            println!("    {k:<22} {v}");
        }
    } else {
        println!("\n  receipt     none: this recording makes no reproducibility claim");
    }
    Ok(true)
}

fn cmd_verify(path: &str) -> Result<bool, String> {
    let bytes = slurp(path)?;
    let v = verify(&bytes).ok_or_else(|| {
        format!("{path} has no Ferroscope receipt, or its payloads are unreadable")
    })?;

    println!("{path}");
    println!("  scenario        {}", v.receipt.spec.scenario);
    println!("  seed            {}", v.receipt.spec.seed);
    println!("  precision       {}", v.receipt.precision);
    println!("  platform        {}", v.receipt.platform);
    println!("  messages hashed {}", v.messages);
    println!();
    println!(
        "  spec digest     {}  {}",
        v.receipt.spec_digest,
        mark(v.spec_matches)
    );
    println!(
        "  trace digest    {}  {}",
        v.receipt.trace_digest,
        mark(v.trace_matches)
    );
    if !v.trace_matches {
        println!("  recomputed      {}", v.recomputed);
    }
    if v.receipt.non_finite > 0 {
        println!(
            "\n  ⚠ the run produced {} non-finite value(s); the digest records that rather \
             than hashing them into a match",
            v.receipt.non_finite
        );
    }
    println!();
    if v.ok() {
        println!("  VERIFIED: this file still stands behind its own receipt.");
    } else if !v.spec_matches {
        println!("  FAILED: the receipt's own fields no longer hash to its stated spec digest.");
        println!("           Somebody edited the metadata after the run.");
    } else {
        println!("  FAILED: the messages in this file no longer hash to the stored trace digest.");
    }
    Ok(v.ok())
}

fn mark(ok: bool) -> &'static str {
    if ok {
        "ok"
    } else {
        "MISMATCH"
    }
}

fn cmd_energy(path: &str) -> Result<bool, String> {
    let bytes = slurp(path)?;
    let v = verify(&bytes).ok_or_else(|| format!("{path} is not a Ferroscope recording"))?;
    let q = &v.quote;

    println!("{path}\n");
    println!("  E_task = E_compute + E_actuation");
    println!("  ---------------------------------------------");
    println!(
        "  compute      {:>12.3} J   {:>5.1} %",
        q.compute_j,
        q.compute_fraction() * 100.0
    );
    println!(
        "  actuation    {:>12.3} J   {:>5.1} %",
        q.actuation_j,
        if q.total_j > 0.0 {
            q.actuation_j / q.total_j * 100.0
        } else {
            0.0
        }
    );
    if q.overhead_j > 0.0 {
        println!(
            "  overhead     {:>12.3} J   {:>5.1} %",
            q.overhead_j,
            q.overhead_j / q.total_j * 100.0
        );
    }
    println!("  ---------------------------------------------");
    println!("  total        {:>12.3} J", q.total_j);
    println!("  duration     {:>12.3} s", q.duration_s);
    println!("  mean power   {:>12.3} W", q.mean_power_w());
    println!("  peak         {:>12.3} W   {}", q.peak_w, q.peak_source);

    if !q.by_source.is_empty() {
        println!("\n  {:<10} {:<18} {:>12}", "RAIL", "SOURCE", "JOULES");
        for (rail, name, j) in q.by_source.iter().take(12) {
            let r = match rail {
                Rail::Compute => "compute",
                Rail::Actuation => "actuation",
                Rail::Overhead => "overhead",
            };
            println!("  {r:<10} {name:<18} {j:>12.3}");
        }
    }

    println!("\n  coverage     {}", q.coverage);
    if !q.quotable {
        println!("  → this number is reported but must not be cited as a measurement.");
    }
    Ok(q.quotable)
}

fn cmd_diff(a_path: &str, b_path: &str, rest: &[&str]) -> Result<bool, String> {
    let mut tol = Tolerance::default();
    let mut i = 0;
    while i < rest.len() {
        match rest[i] {
            "--abs" => {
                tol.abs = rest
                    .get(i + 1)
                    .and_then(|s| s.parse().ok())
                    .ok_or("--abs needs a number")?;
                i += 2;
            }
            "--rel" => {
                tol.rel = rest
                    .get(i + 1)
                    .and_then(|s| s.parse().ok())
                    .ok_or("--rel needs a number")?;
                i += 2;
            }
            other => return Err(format!("unknown flag {other}")),
        }
    }

    let (ra, ta) = trace_from(&slurp(a_path)?).ok_or_else(|| format!("cannot read {a_path}"))?;
    let (rb, tb) = trace_from(&slurp(b_path)?).ok_or_else(|| format!("cannot read {b_path}"))?;

    println!("A  {a_path}");
    if let Some(r) = &ra {
        println!("   {}  on {}", &r.spec_digest[..16], r.platform);
    }
    println!("B  {b_path}");
    if let Some(r) = &rb {
        println!("   {}  on {}", &r.spec_digest[..16], r.platform);
    }
    println!();

    // The cheap path first. A digest match is proof; a mismatch is only a question, and the
    // comparator below is what answers it.
    if let (Some(a), Some(b)) = (&ra, &rb) {
        if let Some(v) = digests_agree(a, b) {
            println!("  {v}");
            if let Verdict::Incomparable { .. } = v {
                return Ok(false);
            }
            println!("  (digests alone; no per-step comparison was needed)");
            return Ok(v.reproduced());
        }
        println!("  digests differ, comparing step by step");
    }

    let verdict = compare(&ta, &tb, tol);
    println!("  {verdict}");
    match &verdict {
        Verdict::Diverged { step, .. } => {
            let span = ta.samples.iter().map(|s| s.step).max().unwrap_or(0);
            println!(
                "  → the runs agreed for {:.1} % of the trajectory, then did not.",
                if span > 0 {
                    *step as f64 / span as f64 * 100.0
                } else {
                    0.0
                }
            );
        }
        Verdict::WithinTolerance { .. } => {
            println!(
                "  → tolerance was abs {:.1e} / rel {:.1e}; nothing exceeded both.",
                tol.abs, tol.rel
            );
        }
        _ => {}
    }
    Ok(verdict.reproduced())
}

fn cmd_export(path: &str, out: &str) -> Result<bool, String> {
    let bytes = slurp(path)?;
    let bundle = bundle(&bytes).ok_or_else(|| format!("cannot read {path}"))?;
    std::fs::write(out, &bundle).map_err(|e| format!("cannot write {out}: {e}"))?;
    println!("wrote {out} ({} bytes)", bundle.len());
    Ok(true)
}

#[cfg(test)]
mod arg_tests {
    use super::split_out;

    #[test]
    fn a_flag_in_the_output_slot_is_a_flag_and_not_a_filename() {
        // This shipped broken: `urdf robot.urdf --check` bound "--check" as the file to write,
        // so --check never reached the parser and a 1.4 MB file named `--check` appeared in the
        // working directory. The exit code was still right, which is why nobody noticed.
        let (out, flags) = split_out(&["--check"], "robot.mcap");
        assert_eq!(out, "robot.mcap");
        assert_eq!(flags, ["--check"]);
    }

    #[test]
    fn an_explicit_output_path_still_wins_and_keeps_its_flags() {
        let (out, flags) = split_out(&["run.mcap", "--steps", "10"], "robot.mcap");
        assert_eq!(out, "run.mcap");
        assert_eq!(flags, ["--steps", "10"]);
    }

    #[test]
    fn a_flags_own_value_is_never_mistaken_for_the_output_path() {
        // Only the FIRST token can be the path, so the 400 in `--steps 400` stays a value.
        let (out, flags) = split_out(&["--steps", "400"], "robot.mcap");
        assert_eq!(out, "robot.mcap");
        assert_eq!(flags, ["--steps", "400"]);
    }
}
