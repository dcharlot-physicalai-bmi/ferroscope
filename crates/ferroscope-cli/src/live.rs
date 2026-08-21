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
//!
//! Pacing happens at RECORD granularity, and a chunked recording carries the chunk's start
//! time on each chunk — so a run whose messages fit a single chunk has one pacing instant and
//! streams in one burst no matter how long its sim span is. This verb reports the span it can
//! actually pace rather than the sim span, because the first version printed the latter as the
//! replay length and could not keep that promise.

use ferroscope_mcap::RecordSpan;
use std::time::{Duration, Instant};

/// The port the viewer's `live` button dials. Binding anywhere else and then telling the user
/// to press that button is an instruction that cannot work, which is what this verb shipped
/// with: an ephemeral port and a banner naming a button hard-wired to 8737.
const VIEWER_PORT: u16 = 8737;

/// Bounds on `--rate`. Outside these, the arithmetic below stops being representable as a
/// `Duration` and `Duration::from_secs_f64` panics — exit 101, mid-stream, leaving connected
/// viewers holding a file with no end on it.
const RATE_MIN: f64 = 1e-6;
const RATE_MAX: f64 = 1e6;

/// The longest `--hold` worth honoring, and the longest single pace wait we will ever take.
const MAX_WAIT_S: f64 = 3600.0;

/// How long a WebTransport replay waits for its viewers' streams to drain, when `--hold` says
/// nothing. Generous on purpose: this budget is a whole file moving over QUIC, not linger.
const WT_DEFAULT_HOLD_S: f64 = 60.0;

pub fn run(path: &str, rest: &[&str]) -> Result<bool, String> {
    let mut port: u16 = VIEWER_PORT;
    let mut wt = false;
    let mut rate: f64 = 1.0;
    let mut hold: f64 = 2.0;
    let mut hold_set = false;
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
                hold_set = true;
                i += 2;
            }
            other => return Err(format!("unknown flag {other}")),
        }
    }
    if !(rate.is_finite() && (RATE_MIN..=RATE_MAX).contains(&rate)) {
        return Err(format!(
            "--rate must be between {RATE_MIN} and {RATE_MAX}; {rate} cannot be turned into a schedule"
        ));
    }
    if !(hold.is_finite() && (0.0..=MAX_WAIT_S).contains(&hold)) {
        return Err(format!(
            "--hold must be between 0 and {MAX_WAIT_S} seconds, not {hold}"
        ));
    }

    let bytes = crate::slurp(path)?;
    let spans = ferroscope_mcap::record_spans(&bytes).map_err(|e| format!("{path}: {e}"))?;
    let log = ferroscope_schema::mcap::read(&bytes).map_err(|e| format!("{path}: {e}"))?;
    let timed = spans.iter().filter(|s| s.log_time.is_some()).count();
    let span_s = log
        .time_span()
        .map(|(a, b)| (b - a) as f64 * 1e-9)
        .unwrap_or(0.0);
    // What the replay can ACTUALLY pace, which is not the same as the sim span: pacing happens
    // at record granularity, and a chunked file's records carry the chunk's start time. A run
    // whose messages fit one chunk has a single pacing point and streams in one burst no matter
    // what the sim span says. Printing the sim span as the replay length was a promise the
    // scheduler could not keep, so the two numbers are now reported separately.
    let paced_s = pacing_span_s(&spans);

    println!("{path}");
    println!(
        "  records      {} ({timed} pacing points), {} messages, {} bytes",
        spans.len(),
        log.messages.len(),
        bytes.len()
    );
    println!("  sim span     {span_s:.3} s");
    if timed <= 1 || paced_s <= 0.0 {
        println!(
            "  replay       one burst — every record shares a pacing instant, so there is \
             nothing to pace (record it with more, smaller chunks to replay it in time)"
        );
    } else {
        println!(
            "  replay       {:.3} s at {rate}x, paced across {timed} instants{}",
            paced_s / rate,
            if paced_s < span_s * 0.99 {
                format!(" (covers {:.0}% of the sim span)", paced_s / span_s * 100.0)
            } else {
                String::new()
            }
        );
    }
    match ferroscope_schema::verify(&bytes) {
        Some(v) if v.ok() => {
            println!("  receipt      VERIFIED — a viewer can re-verify the bytes it holds")
        }
        Some(_) => println!("  receipt      present but does NOT verify; streaming it anyway"),
        None => println!("  receipt      none"),
    }

    if wt {
        // The two transports need different endgames. A WebSocket viewer pulls the whole
        // history itself, so the hold is just socket linger and two seconds is plenty. A
        // WebTransport session has to PUSH whatever the viewer has not received yet, which for
        // a large recording is the whole file over QUIC flow control — a 71 MB replay was
        // reported unfinished at six seconds while the bytes were still legitimately moving.
        let wt_hold = if hold_set { hold } else { WT_DEFAULT_HOLD_S };
        return replay_wt(&bytes, &spans, rate, port, wait, wt_hold);
    }

    let srv = ferroscope_live::LiveServer::bind(port)
        .map_err(|e| format!("cannot bind 127.0.0.1:{port}: {e}"))?;
    println!("  live         ws://localhost:{}", srv.port());
    if srv.port() == VIEWER_PORT {
        println!("               open https://ferroscope.physicalai-bmi.org/viewer and press live");
    } else {
        // Only the default port is reachable from the viewer's button. Saying "press live"
        // while bound anywhere else sends the reader somewhere nothing is listening.
        println!(
            "               the viewer's live button dials port {VIEWER_PORT}; on {} \
             connect a client of your own",
            srv.port()
        );
    }
    if wait {
        await_first_viewer(|| srv.viewers());
    }
    pace(&bytes, &spans, rate, |b| srv.broadcast(b));
    // The closing magic is out; the socket stays up a moment longer so a late joiner still
    // receives the complete file from history rather than a connection refused.
    sleep_s(hold);
    // Say the invariant only over viewers that are actually still attached. The mutation test
    // for the write timeout printed this line to an audience of nobody: the one viewer had
    // been dropped mid-catch-up and the sentence was still there, sounding like success.
    match srv.viewers() {
        0 => {
            println!("  streamed to  no viewers — nothing was watching, so nothing holds the file");
            Ok(true)
        }
        held => {
            println!("  streamed to  {held} viewer(s); the stream is the file they now hold");
            Ok(true)
        }
    }
}

/// The wall-clock span the scheduler can actually spread the file across.
fn pacing_span_s(spans: &[RecordSpan]) -> f64 {
    let mut lo = u64::MAX;
    let mut hi = 0u64;
    for s in spans.iter().filter_map(|s| s.log_time) {
        lo = lo.min(s);
        hi = hi.max(s);
    }
    if lo == u64::MAX || hi <= lo {
        0.0
    } else {
        (hi - lo) as f64 * 1e-9
    }
}

/// Sleep, without a panic path. `Duration::from_secs_f64` panics on a value it cannot
/// represent, and this one is computed from file contents and a user-supplied rate.
fn sleep_s(secs: f64) {
    if !secs.is_finite() || secs <= 0.0 {
        return;
    }
    if let Ok(d) = Duration::try_from_secs_f64(secs.min(MAX_WAIT_S)) {
        std::thread::sleep(d);
    }
}

#[cfg(feature = "webtransport")]
fn replay_wt(
    bytes: &[u8],
    spans: &[RecordSpan],
    rate: f64,
    port: u16,
    wait: bool,
    hold: f64,
) -> Result<bool, String> {
    // The WebSocket default is the viewer's button; WebTransport is reached by the printed
    // link, which carries whatever port was bound, so an ephemeral one is right here.
    let port = if port == VIEWER_PORT { 0 } else { port };
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
    // FIN every stream and let the last bytes leave before the process does. --hold is that
    // budget; it used to be parsed, validated, documented, and then dropped on the floor here
    // in favour of a hardcoded three seconds.
    let report = srv.finish(Duration::try_from_secs_f64(hold.max(1.0)).unwrap_or(Duration::MAX));
    if report.all_sealed() {
        println!(
            "  webtransport {} stream(s) finished; the stream is the file they now hold",
            report.sealed
        );
        Ok(true)
    } else {
        // Never print the invariant over a transfer that did not hold it.
        println!(
            "  webtransport {} sealed, {} abandoned, {} still open at the deadline",
            report.sealed, report.abandoned, report.timed_out
        );
        eprintln!(
            "ferroscope: {} viewer(s) did not receive the sealed file; their streams were reset, \
             not finished, so nothing they hold can be mistaken for the recording",
            report.abandoned + report.timed_out
        );
        Ok(false)
    }
}

#[cfg(not(feature = "webtransport"))]
fn replay_wt(_: &[u8], _: &[RecordSpan], _: f64, _: u16, _: bool, _: f64) -> Result<bool, String> {
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
            let target_s = t.saturating_sub(t0v) as f64 * 1e-9 / rate;
            let elapsed_s = start.elapsed().as_secs_f64();
            if target_s > elapsed_s {
                sleep_s(target_s - elapsed_s);
            }
        }
        send(&bytes[s.start..s.end]);
    }
    send(&bytes[bytes.len() - 8..]);
}
