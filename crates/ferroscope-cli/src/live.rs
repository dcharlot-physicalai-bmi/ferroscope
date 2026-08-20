//! `ferroscope live` — replay any recording as a live stream.
//!
//! Until now only a producer mid-run could stream; a finished file could only be opened. This
//! verb closes that gap from the other side: it serves an existing recording over the same two
//! transports the producers use, whole records in file order, paced by the file's own log
//! clock. The invariant is unchanged — the stream is the file — so a viewer that appends what
//! it receives holds the byte-identical recording the moment the replay seals, receipt and all.
//!
//! A viewer that connects mid-replay still gets a valid file prefix: both servers hand new
//! joiners everything broadcast so far before the live tail, because schemas and channels only
//! ever appear at the front of the stream.

use ferroscope_mcap::RecordSpan;
use std::time::{Duration, Instant};

pub fn run(path: &str, rest: &[&str]) -> Result<bool, String> {
    let mut port: u16 = 0;
    let mut wt = false;
    let mut rate: f64 = 1.0;
    let mut hold: f64 = 2.0;
    let mut wait = true;
    let mut i = 0;
    while i < rest.len() {
        match rest[i] {
            "--no-wait" => {
                wait = false;
                i += 1;
            }
            "--port" => {
                port = rest
                    .get(i + 1)
                    .and_then(|s| s.parse().ok())
                    .ok_or("--port needs a number")?;
                i += 2;
            }
            "--wt" => {
                wt = true;
                i += 1;
            }
            "--rate" => {
                rate = rest
                    .get(i + 1)
                    .and_then(|s| s.parse().ok())
                    .ok_or("--rate needs a number")?;
                i += 2;
            }
            "--hold" => {
                hold = rest
                    .get(i + 1)
                    .and_then(|s| s.parse().ok())
                    .ok_or("--hold needs a number of seconds")?;
                i += 2;
            }
            other => return Err(format!("unknown flag {other}")),
        }
    }
    if !(rate.is_finite() && rate > 0.0) {
        return Err("--rate must be a positive number".into());
    }
    if !(hold.is_finite() && hold >= 0.0) {
        return Err("--hold must be zero or more seconds".into());
    }

    let bytes = crate::slurp(path)?;
    let spans = ferroscope_mcap::record_spans(&bytes).map_err(|e| format!("{path}: {e}"))?;
    let log = ferroscope_schema::mcap::read(&bytes).map_err(|e| format!("{path}: {e}"))?;
    let timed = spans.iter().filter(|s| s.log_time.is_some()).count();
    let span_s = log
        .time_span()
        .map(|(a, b)| (b - a) as f64 * 1e-9)
        .unwrap_or(0.0);

    println!("{path}");
    println!(
        "  records      {} ({timed} pacing points), {} messages, {} bytes",
        spans.len(),
        log.messages.len(),
        bytes.len()
    );
    println!(
        "  sim span     {span_s:.3} s, replaying at {rate}x over about {:.3} s",
        span_s / rate
    );
    match ferroscope_schema::verify(&bytes) {
        Some(v) if v.ok() => {
            println!("  receipt      VERIFIED — a viewer can re-verify the bytes it holds")
        }
        Some(_) => println!("  receipt      present but does NOT verify; streaming it anyway"),
        None => println!("  receipt      none"),
    }

    if wt {
        return replay_wt(&bytes, &spans, rate, port, wait);
    }

    let srv = ferroscope_live::LiveServer::bind(port)
        .map_err(|e| format!("cannot bind 127.0.0.1:{port}: {e}"))?;
    println!("  live         ws://localhost:{}", srv.port());
    println!("               open https://ferroscope.physicalai-bmi.org/viewer and press live");
    if wait {
        await_first_viewer(|| srv.viewers());
    }
    pace(&bytes, &spans, rate, |b| srv.broadcast(b));
    // The closing magic is out; the socket stays up a moment longer so a late joiner still
    // receives the complete file from history rather than a connection refused.
    std::thread::sleep(Duration::from_secs_f64(hold));
    println!(
        "  streamed to  {} viewer(s); the stream is the file they now hold",
        srv.viewers()
    );
    Ok(true)
}

#[cfg(feature = "webtransport")]
fn replay_wt(
    bytes: &[u8],
    spans: &[RecordSpan],
    rate: f64,
    port: u16,
    wait: bool,
) -> Result<bool, String> {
    let srv = ferroscope_live::WtServer::bind(port)
        .map_err(|e| format!("cannot bind WebTransport on 127.0.0.1:{port}: {e}"))?;
    println!("  webtransport https://127.0.0.1:{}", srv.port());
    println!(
        "               https://ferroscope.physicalai-bmi.org/viewer?wt=https://127.0.0.1:{}&hash={}",
        srv.port(),
        srv.cert_hash_hex()
    );
    if wait {
        await_first_viewer(|| srv.viewers());
    }
    pace(bytes, spans, rate, |b| srv.broadcast(b));
    // FIN every stream and let the last bytes leave before the process does.
    srv.finish(Duration::from_secs(3));
    println!("  webtransport streams finished; the stream is the file they now hold");
    Ok(true)
}

#[cfg(not(feature = "webtransport"))]
fn replay_wt(_: &[u8], _: &[RecordSpan], _: f64, _: u16, _: bool) -> Result<bool, String> {
    Err("this build has no WebTransport; install it with \
         `cargo install ferroscope-cli --features webtransport` (WebSocket replay needs no feature)"
        .into())
}

/// Block until someone is watching. A replay exists to be seen; starting the clock before the
/// first viewer arrives replays the run into silence — the failure mode that motivated this:
/// a short file could stream out, seal, and exit inside the window a client needs to connect.
fn await_first_viewer(viewers: impl Fn() -> usize) {
    println!("  waiting      for the first viewer (--no-wait starts immediately, Ctrl-C aborts)");
    while viewers() == 0 {
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Broadcast the file: magic, then every record at its own instant, then the closing magic.
///
/// Pacing is absolute — each record's target is measured from the start of the replay, not
/// from the previous record — so scheduling jitter never accumulates into drift.
fn pace(bytes: &[u8], spans: &[RecordSpan], rate: f64, send: impl Fn(&[u8])) {
    send(&bytes[..8]);
    let t0 = spans.iter().find_map(|s| s.log_time);
    let start = Instant::now();
    for s in spans {
        if let (Some(t), Some(t0v)) = (s.log_time, t0) {
            let target = Duration::from_secs_f64(t.saturating_sub(t0v) as f64 * 1e-9 / rate);
            if let Some(wait) = target.checked_sub(start.elapsed()) {
                std::thread::sleep(wait);
            }
        }
        send(&bytes[s.start..s.end]);
    }
    send(&bytes[bytes.len() - 8..]);
}
