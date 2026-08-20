//! What producing a recording cost, on the machine that produced it.
//!
//! Not the task's energy — the compute rail inside a described scene models the *robot*, and
//! replacing that with this workstation's package power would be a category error. This is the
//! other quantity: what this machine spent computing and writing the file, measured from its own
//! counters when it allows that, and a stated reason when it does not. It lands in the
//! recording's `ferroscope.production` metadata block, outside both digests, because it varies
//! run to run by nature and must never touch the determinism claim.

/// The machine's meter, primed so the first note covers work from now on.
pub fn start() -> ferroscope_power::Meter {
    let mut meter = ferroscope_power::Meter::open();
    // Prime the counters. An unavailable meter reports itself in the note; nothing to do here.
    let _ = meter.sample_energy();
    meter
}

/// Print the one line a person needs, mirroring exactly what went into the file.
pub fn print(kv: &[(String, String)]) {
    let get = |k: &str| kv.iter().find(|(a, _)| a == k).map(|(_, v)| v.as_str());
    match (get("joules"), get("unavailable")) {
        (Some(j), _) => {
            // Only a cumulative counter earns the word "measured". An instantaneous sample
            // multiplied by the wall clock is an approximation, and the line says so.
            let counter = get("basis") == Some("cumulative energy counter");
            println!(
                "  production   {}{j} J {} over {} s ({})",
                if counter { "" } else { "~" },
                if counter {
                    "measured"
                } else {
                    "approximated from one power sample"
                },
                get("duration_s").unwrap_or("?"),
                get("source").unwrap_or("?")
            );
        }
        (_, Some(why)) => println!("  production   not measured: {why}"),
        _ => {}
    }
}
