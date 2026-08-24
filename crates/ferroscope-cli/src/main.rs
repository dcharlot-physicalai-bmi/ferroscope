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
//! ferroscope live    run.mcap          replay it as a live stream, on its own clock
//! ferroscope demo    out.mcap          write a synthetic run to try the above on
//! ```

use std::process::ExitCode;

mod builtin;
mod demo;
mod glb;
mod live;
mod power;
mod production;
mod say;
mod scene;
mod urdf;

use ferroscope_ledger::Rail;
use ferroscope_receipt::{Tolerance, Verdict};
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
        ["live", file, rest @ ..] => run(live::run(file, rest)),
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
  ferroscope live    <run.mcap>            REPLAY it as a live stream, on its own clock
                     [--port <n>] [--wt] [--rate <x>] [--hold <s>] [--no-wait]
                     port 8737 by default, which is the port the viewer's live button dials
  ferroscope demo    <out.mcap>            write a synthetic run
                     [--seed <n>] [--steps <n>] [--drift <step>] [--platform <s>]
  ferroscope urdf    <robot.urdf> <out.mcap>  record YOUR robot, and check its description
                     [--check] [--steps <n>] [--rate <hz>] [--sweep all|each]
                     [--no-collision] [--no-inertial] [--meshes <dir>]
  ferroscope scene   <scene.json> <out.mcap>  record a DESCRIBED scene
                     [--check] [--sweep]   ·   --schema prints the scene format
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
    if let Some(kv) = log.metadata_block(ferroscope_schema::PRODUCTION_BLOCK) {
        // What making this copy of the file cost, on the machine that made it. Outside both
        // digests on purpose: the same experiment on a busier machine costs more.
        println!("\n  production");
        for (k, v) in kv {
            println!("    {k:<22} {v}");
        }
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

    if let Some(kv) = mcap::read(&bytes).ok().and_then(|log| {
        log.metadata_block(ferroscope_schema::PRODUCTION_BLOCK)
            .map(<[_]>::to_vec)
    }) {
        production::print(&kv);
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

    let bytes_a = slurp(a_path)?;
    let bytes_b = slurp(b_path)?;
    let (ra, ta) = trace_from(&bytes_a).ok_or_else(|| format!("cannot read {a_path}"))?;
    let (rb, tb) = trace_from(&bytes_b).ok_or_else(|| format!("cannot read {b_path}"))?;

    // Recompute both receipts BEFORE comparing anything. This used to be skipped entirely: the
    // fast path compared two digest strings read straight out of metadata, so two files whose
    // blocks were edited to carry the same trace_digest passed with exit 0 whatever their
    // messages held. A green diff meant two metadata blocks agreed, not that two runs
    // reproduced — and the receipt is the product's whole claim.
    let va = verify(&bytes_a);
    let vb = verify(&bytes_b);
    let mut trustworthy = true;
    for (label, path, v, r) in [("A", a_path, &va, &ra), ("B", b_path, &vb, &rb)] {
        println!("{label}  {path}");
        match v {
            Some(v) if v.ok() => println!(
                "   {}  on {}  VERIFIED (receipt recomputed from the file)",
                &v.receipt.spec_digest[..16],
                v.receipt.platform
            ),
            Some(v) => {
                trustworthy = false;
                println!(
                    "   {}  on {}  DOES NOT VERIFY — {}",
                    &v.receipt.spec_digest[..16],
                    v.receipt.platform,
                    if v.trace_matches {
                        "the spec digest does not match its own fields"
                    } else {
                        "the recomputed trace digest does not match the stored one"
                    }
                );
            }
            None => {
                trustworthy = false;
                let d = r
                    .as_ref()
                    .map(|r| r.spec_digest[..16].to_string())
                    .unwrap_or_else(|| "(no receipt)".into());
                println!("   {d}  carries no recomputable receipt");
            }
        }
    }
    if !trustworthy {
        println!(
            "\n  one of these files does not stand behind its own receipt, so whatever follows \
             compares bytes, not evidence."
        );
    }
    println!();

    // When the specs differ these are not two runs of the same experiment, and saying which
    // field moved is the whole answer — the fields are already parsed.
    let mut specs_differ = false;
    if let (Some(a), Some(b)) = (&ra, &rb) {
        let diffs = ferroscope_receipt::spec_differences(&a.spec, &b.spec);
        if !diffs.is_empty() {
            specs_differ = true;
            println!("  the specs differ, so these are not two runs of the same experiment:");
            for d in &diffs {
                println!("    {:<22} {}  →  {}", d.field, d.a, d.b);
                println!("    {:22} {}", "", d.means);
            }
            println!(
                "    ({} declared fields compared; a spec declaring few fields is a weak \
                 comparability claim)",
                ferroscope_receipt::declared_fields(&a.spec)
            );
            // "Did these reproduce?" has no yes available when the two runs did not ask the
            // same question, so the exit code stays 1 however well the numbers line up. When
            // only the toolchain moved, the trace comparison below is still worth reading, and
            // saying so is the difference between a useful red and a baffling one.
            if diffs.len() == 1 && diffs[0].field == "build" {
                println!(
                    "    only the build differs, so the comparison below is still physically \
                     meaningful — but this is not a reproduction of the same declared run."
                );
            }
            println!();
        } else if a.trace_digest == b.trace_digest && trustworthy {
            // A digest match is proof, and now it is proof of RECOMPUTED digests.
            println!(
                "  {}",
                Verdict::IdenticalAtPrecision {
                    precision: a.precision
                }
            );
            println!(
                "  (both receipts recomputed from their files and agree; no per-step comparison \
                 was needed)"
            );
            return Ok(true);
        }
    }

    let p = ferroscope_receipt::profile(&ta, &tb, tol);
    let labels = ferroscope_schema::channel_labels(&bytes_a);
    println!("  {}", p.verdict);
    // The verdict names an array slot; the recording knows what lives in that slot.
    if let Verdict::Diverged { channel, index, .. } = &p.verdict {
        if let Some(name) = labels.get(channel).and_then(|l| l.get(*index)) {
            println!("  that is       {channel} {name}");
        }
    }

    if let Some(s) = &p.structural {
        println!("\n  the runs did not record the same things:");
        if s.a_last_step != s.b_last_step {
            println!(
                "    extent       A ends at step {}, B at step {} — {} stopped first",
                s.a_last_step,
                s.b_last_step,
                if s.a_last_step < s.b_last_step {
                    "A"
                } else {
                    "B"
                }
            );
        }
        for (name, only) in [("only in A", &s.only_in_a), ("only in B", &s.only_in_b)] {
            if !only.is_empty() {
                println!("    {name:<12} {}", only.join(", "));
            }
        }
        for (ch, first, n, side) in s.gaps.iter().take(6) {
            println!(
                "    gap          {ch} — {n} step(s) present on one side only, first at step \
                 {first} (missing from {side})"
            );
        }
        println!(
            "    coverage     the verdict above covers the {} sample(s) both runs recorded; \
             {} were excluded and it says nothing about them",
            s.shared_samples, s.excluded_samples
        );
    }

    if !p.channels.is_empty() {
        if let Some((ch, step)) = &p.onset {
            println!("\n  onset        step {step} on {ch} — where the bits first parted");
        }
        match p.crossing {
            Some(c) => println!(
                "  crossing     step {c} — where a value first exceeded abs {:.1e} / rel {:.1e} \
                 (this step moves when the tolerance moves; the onset does not)",
                tol.abs, tol.rel
            ),
            None => println!(
                "  crossing     none — nothing exceeded abs {:.1e} / rel {:.1e}",
                tol.abs, tol.rel
            ),
        }
        if let Some(d) = p.dominant() {
            println!("  shape        {}", d.shape);
        }
        println!(
            "  extent       {} channel(s) differ, ranked by relative difference — they are not \
             independent, so this is a ranking and not a count of separate faults:",
            p.channels.len()
        );
        for d in p.channels.iter().take(6) {
            let name = labels
                .get(&d.channel)
                .and_then(|l| l.get(d.worst_index))
                .cloned()
                .unwrap_or_else(|| format!("[{}]", d.worst_index));
            println!(
                "    {:<22} rel {:.3e} at step {:<7} {name}  {:.12} vs {:.12}",
                d.channel, d.worst_rel, d.worst_rel_step, d.a_at_worst, d.b_at_worst
            );
        }
        if p.channels.len() > 6 {
            println!("    … and {} more", p.channels.len() - 6);
        }
    }

    // The joules axis, which this tool has always had and diff has never shown. Two runs can
    // reproduce trajectory-for-trajectory and cost measurably different energy, and which RAIL
    // moved says where to look.
    if let (Some(x), Some(y)) = (&va, &vb) {
        print_energy_delta(&x.quote, &y.quote);
    }

    if let Verdict::WithinTolerance { .. } = &p.verdict {
        if let Some(r) = &ra {
            println!(
                "  → tolerance was abs {:.1e} / rel {:.1e}; nothing exceeded both. The digest \
                 itself cannot see below {:.1e} relative.",
                tol.abs,
                tol.rel,
                r.precision.relative_resolution()
            );
        }
    }
    Ok(p.verdict.reproduced() && trustworthy && !specs_differ)
}

/// Report what the two runs cost, per rail, and refuse the comparison when either quote is not
/// admissible rather than printing a difference of two numbers that do not mean the same thing.
fn print_energy_delta(a: &ferroscope_ledger::Quote, b: &ferroscope_ledger::Quote) {
    if a.total_j == 0.0 && b.total_j == 0.0 {
        return;
    }
    println!(
        "\n  energy       A {:.3} J    B {:.3} J    Δ {:+.6} J",
        a.total_j,
        b.total_j,
        b.total_j - a.total_j
    );
    if !a.quotable || !b.quotable {
        println!(
            "               NOT comparable: coverage is {} for A and {} for B, and a gap can \
             hide the entire difference",
            a.coverage, b.coverage
        );
        return;
    }
    let rail = |name: &str, x: f64, y: f64| -> String {
        let d = y - x;
        if d == 0.0 {
            format!("{name} identical")
        } else if x != 0.0 && (d / x).abs() < 1e-4 {
            format!("{name} {d:+.3e} J (below 0.01 %)")
        } else if x == 0.0 {
            format!("{name} {d:+.6} J")
        } else {
            format!("{name} {:+.2} %", d / x * 100.0)
        }
    };
    println!(
        "               {}   {}   {}",
        rail("total", a.total_j, b.total_j),
        rail("compute", a.compute_j, b.compute_j),
        rail("actuation", a.actuation_j, b.actuation_j)
    );
    // A rail that did not move localises the fault for free — but only as well as the labels
    // the recorder chose, which is a declaration and not a measurement.
    if (a.compute_j - b.compute_j).abs() < 1e-6 && (a.actuation_j - b.actuation_j).abs() >= 1e-6 {
        println!(
            "               the compute rail is unchanged and the actuation rail is not: the \
             control schedule held and the physics moved (rails are the recorder's own labels)"
        );
    }
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
