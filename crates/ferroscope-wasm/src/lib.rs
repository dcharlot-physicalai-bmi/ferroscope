//! **Ferroscope in the browser.**
//!
//! Three functions. They take the bytes of a recording and return JSON. Nothing is uploaded,
//! nothing is transcoded server-side, and no account is involved, because the parser, the
//! energy ledger, the SHA-256 and the comparator are all `std`-only Rust that happens to
//! compile for `wasm32`.
//!
//! That is the whole argument for the interface layer being open. Foxglove's app is
//! proprietary and its data platform is metered; Antioch's simulation runs in someone's
//! cloud. Neither can hand you a page that opens *your* recording with the network turned
//! off. This can, and it is the same code path the CLI uses, so the numbers agree by
//! construction rather than by testing.
//!
//! ```js
//! import init, { open, diff, version } from './ferroscope_wasm.js';
//! await init();
//! const bundle = JSON.parse(open(new Uint8Array(await file.arrayBuffer())));
//! ```

#![forbid(unsafe_code)]

use wasm_bindgen::prelude::*;

use ferroscope_receipt::{compare, digests_agree, Tolerance, Verdict};
use ferroscope_schema::{bundle, trace_from, verify};

/// The crate version, so a page can report which build produced what it is showing.
#[wasm_bindgen]
pub fn version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

/// Open a recording. Returns the viewer bundle as a JSON string: topics, pose and scalar
/// lanes, power by rail, contacts, the clock drift lane, the closed energy ledger, and the
/// receipt **with the trace digest recomputed from these very bytes**.
///
/// Throws a JS error naming what went wrong rather than returning a half-parsed run.
#[wasm_bindgen]
pub fn open(bytes: &[u8]) -> Result<String, JsValue> {
    bundle(bytes).ok_or_else(|| {
        // Say which of the two failures it was: the container or the payloads. A viewer that
        // reports "could not open" for both sends the reader looking in the wrong place.
        let msg = match ferroscope_schema::mcap::read(bytes) {
            Err(e) => format!("not a readable MCAP file: {e}"),
            Ok(_) => "the file parses as MCAP, but a message payload is not JSON this build \
                      understands (a non-Ferroscope recording will land here)"
                .to_string(),
        };
        JsValue::from_str(&msg)
    })
}

/// Open a growing live prefix of a recording — the bytes a WebSocket has delivered so far.
///
/// Same bundle as [`open`], with no closing magic required and no receipt expected yet: the
/// moment the producer seals, the same buffer is a complete file and [`open`] takes over,
/// receipt and all.
#[wasm_bindgen]
pub fn open_prefix(bytes: &[u8]) -> Result<String, JsValue> {
    ferroscope_schema::bundle_prefix(bytes).ok_or_else(|| {
        let msg = match ferroscope_schema::mcap::read_prefix(bytes) {
            Err(e) => format!("not a readable MCAP prefix: {e}"),
            Ok(_) => "the prefix parses, but a message payload is not JSON this build \
                      understands"
                .to_string(),
        };
        JsValue::from_str(&msg)
    })
}

/// Verify one recording against its own receipt. Returns JSON:
/// `{ verified, spec_matches, trace_matches, stored, recomputed, messages, non_finite }`.
#[wasm_bindgen]
pub fn verify_receipt(bytes: &[u8]) -> Result<String, JsValue> {
    let v = verify(bytes).ok_or_else(|| {
        JsValue::from_str(
            "this recording carries no Ferroscope receipt, so there is nothing to verify",
        )
    })?;
    Ok(format!(
        "{{\"verified\":{},\"spec_matches\":{},\"trace_matches\":{},\"stored\":\"{}\",\
          \"recomputed\":\"{}\",\"messages\":{},\"non_finite\":{},\"precision\":\"{}\",\
          \"platform\":\"{}\",\"scenario\":\"{}\",\"seed\":{}}}",
        v.ok(),
        v.spec_matches,
        v.trace_matches,
        v.receipt.trace_digest,
        v.recomputed,
        v.messages,
        v.receipt.non_finite,
        esc(&v.receipt.precision.to_string()),
        esc(&v.receipt.platform),
        esc(&v.receipt.spec.scenario),
        v.receipt.spec.seed,
    ))
}

/// Compare two recordings. This is the function that has no equivalent anywhere: drop two
/// `.mcap` files onto a page with the network off and find out whether the second run
/// reproduced the first, and if it did not, at which step it stopped.
///
/// `abs` and `rel` are the tolerances; pass `0` for either to use the default `1e-9`.
#[wasm_bindgen]
pub fn diff(a: &[u8], b: &[u8], abs: f64, rel: f64) -> Result<String, JsValue> {
    let (ra, ta) = trace_from(a).ok_or_else(|| JsValue::from_str("cannot read recording A"))?;
    let (rb, tb) = trace_from(b).ok_or_else(|| JsValue::from_str("cannot read recording B"))?;

    let tol = Tolerance {
        abs: if abs > 0.0 { abs } else { 1e-9 },
        rel: if rel > 0.0 { rel } else { 1e-9 },
    };

    // The cheap path first, exactly as the CLI does it: a digest match is proof, a digest
    // mismatch is only a question, and `compare` is what answers it.
    let mut by_digest = false;
    let verdict = match (&ra, &rb) {
        (Some(x), Some(y)) => match digests_agree(x, y) {
            Some(v) => {
                by_digest = true;
                v
            }
            None => compare(&ta, &tb, tol),
        },
        _ => compare(&ta, &tb, tol),
    };

    let steps = ta.samples.iter().map(|s| s.step).max().unwrap_or(0);
    let (kind, step, channel, index, va, vb, absd, reld) = match &verdict {
        Verdict::BitExact => ("bit-exact", -1i64, String::new(), -1i64, 0.0, 0.0, 0.0, 0.0),
        Verdict::IdenticalAtPrecision { .. } => (
            "identical-at-precision",
            -1,
            String::new(),
            -1,
            0.0,
            0.0,
            0.0,
            0.0,
        ),
        Verdict::WithinTolerance {
            max_abs,
            max_rel,
            at_step,
            channel,
        } => (
            "within-tolerance",
            *at_step as i64,
            channel.clone(),
            -1,
            0.0,
            0.0,
            *max_abs,
            *max_rel,
        ),
        Verdict::Diverged {
            step,
            channel,
            index,
            a,
            b,
            abs,
            rel,
        } => (
            "diverged",
            *step as i64,
            channel.clone(),
            *index as i64,
            *a,
            *b,
            *abs,
            *rel,
        ),
        Verdict::NonFinite {
            step,
            channel,
            index,
            ..
        } => (
            "non-finite",
            *step as i64,
            channel.clone(),
            *index as i64,
            0.0,
            0.0,
            0.0,
            0.0,
        ),
        Verdict::Incomparable { reason } => {
            ("incomparable", -1, reason.clone(), -1, 0.0, 0.0, 0.0, 0.0)
        }
    };

    Ok(format!(
        "{{\"kind\":\"{kind}\",\"reproduced\":{},\"by_digest\":{by_digest},\"text\":\"{}\",\
          \"step\":{step},\"steps\":{steps},\"channel\":\"{}\",\"index\":{index},\
          \"a\":{},\"b\":{},\"abs\":{},\"rel\":{},\
          \"platform_a\":\"{}\",\"platform_b\":\"{}\",\"same_spec\":{}}}",
        verdict.reproduced(),
        esc(&verdict.to_string()),
        esc(&channel),
        fin(va),
        fin(vb),
        fin(absd),
        fin(reld),
        esc(ra.as_ref().map(|r| r.platform.as_str()).unwrap_or("")),
        esc(rb.as_ref().map(|r| r.platform.as_str()).unwrap_or("")),
        match (&ra, &rb) {
            (Some(x), Some(y)) => x.spec_digest == y.spec_digest,
            _ => false,
        },
    ))
}

/// Compute the per-step divergence curve between two recordings on one channel, so a viewer
/// can draw the lane that makes the point: flat zero, then never zero again.
///
/// Returns a JSON array of `[step, |Δ|]`, taking the largest absolute difference across the
/// channel's values at each step.
#[wasm_bindgen]
pub fn divergence_curve(a: &[u8], b: &[u8], channel: &str) -> Result<String, JsValue> {
    let (_, ta) = trace_from(a).ok_or_else(|| JsValue::from_str("cannot read recording A"))?;
    let (_, tb) = trace_from(b).ok_or_else(|| JsValue::from_str("cannot read recording B"))?;

    let mut out = String::from("[");
    let mut first = true;
    for (sa, sb) in ta.samples.iter().zip(&tb.samples) {
        if sa.channel != channel || sb.channel != channel || sa.step != sb.step {
            continue;
        }
        let d = sa
            .values
            .iter()
            .zip(&sb.values)
            .map(|(x, y)| (x - y).abs())
            .fold(0.0f64, f64::max);
        if !first {
            out.push(',');
        }
        first = false;
        out.push_str(&format!("[{},{}]", sa.step, fin(d)));
    }
    out.push(']');
    Ok(out)
}

/// The raw bytes of one attachment, by name.
///
/// A glTF has no business being base64'd into a JSON document, so the viewer asks for the blob
/// directly and hands it to a loader.
#[wasm_bindgen]
pub fn attachment(bytes: &[u8], name: &str) -> Result<Vec<u8>, JsValue> {
    let log = ferroscope_schema::mcap::read(bytes)
        .map_err(|e| JsValue::from_str(&format!("not a readable MCAP file: {e}")))?;
    log.attachment(name).map(|a| a.data.clone()).ok_or_else(|| {
        let have: Vec<&str> = log.attachments.iter().map(|a| a.name.as_str()).collect();
        JsValue::from_str(&format!(
            "no attachment named {name:?}; this recording carries [{}]",
            have.join(", ")
        ))
    })
}

/// Every channel name in a recording, for a viewer that wants to offer a choice.
#[wasm_bindgen]
pub fn channels(bytes: &[u8]) -> Result<String, JsValue> {
    let log = ferroscope_schema::mcap::read(bytes)
        .map_err(|e| JsValue::from_str(&format!("not a readable MCAP file: {e}")))?;
    let mut out = String::from("[");
    for (i, c) in log.channels.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push('"');
        out.push_str(&esc(&c.topic));
        out.push('"');
    }
    out.push(']');
    Ok(out)
}

/// JSON string escaping. Channel names and platform strings come out of a file somebody else
/// wrote, so they are escaped rather than trusted.
fn esc(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o
}

/// JSON has no NaN and no infinity, so a non-finite number becomes `null` rather than a token
/// that would make the whole document unparseable.
fn fin(v: f64) -> String {
    if v.is_finite() {
        format!("{v:?}")
    } else {
        "null".to_string()
    }
}

/// Read an English phrase into a scene, in the tab.
///
/// Returns `{ scene, understood[], assumed[], ignored[] }`, or an object with `error` and `hint`
/// when the phrase names nothing recordable. Deterministic and offline: no request leaves the
/// page, which is the same promise the viewer already makes about the files you open in it.
#[wasm_bindgen]
pub fn scene_from_text(text: &str) -> String {
    use ferroscope_schema::json::Obj;
    match ferroscope_phrase::read(text) {
        Ok(r) => Obj::new()
            .str("scene", &r.scene_json)
            .strs("understood", &r.understood)
            .strs("assumed", &r.assumed)
            .strs("ignored", &r.ignored)
            .finish(),
        Err(e) => Obj::new()
            .str("error", &e.message)
            .str("hint", &e.hint)
            .finish(),
    }
}

fn wasm_note() -> Vec<(String, String)> {
    // A browser tab and a Workers isolate expose no power interface at all, and the block
    // exists so that every recording says either what it cost or why there is no number.
    vec![(
        "unavailable".into(),
        "wasm32 has no power interface: the sandbox exposes no energy counters".into(),
    )]
}

/// Record a scene, in the tab, and hand back the MCAP bytes.
///
/// The same crate the CLI and the edge endpoint run, so a scene authored here produces the same
/// bytes and the same receipt as one authored anywhere else.
#[wasm_bindgen]
pub fn record_scene(scene_json: &str) -> Result<Vec<u8>, JsValue> {
    let scene = ferroscope_scene::Scene::parse(scene_json).map_err(|problems| {
        JsValue::from_str(&format!(
            "{} problem(s):\n{}",
            problems.len(),
            problems
                .iter()
                .map(|p| format!("  {}: {}", p.path, p.message))
                .collect::<Vec<_>>()
                .join("\n")
        ))
    })?;
    // A browser has no filesystem, so a robot named in the scene is fetched by the page and
    // handed back through `record_scene_with`. Here, robots are skipped and noted.
    let rec = scene
        .record_with(|_| None, wasm_note)
        .map_err(|e| JsValue::from_str(&e))?;
    Ok(rec.bytes)
}

/// Record a scene with one robot description supplied by the caller.
///
/// The page fetches the URDF (it knows how to make requests; this does not) and passes the text
/// in. Any robot whose name does not match is skipped and noted in the recording.
#[wasm_bindgen]
pub fn record_scene_with(
    scene_json: &str,
    robot_name: &str,
    urdf: &str,
) -> Result<Vec<u8>, JsValue> {
    let scene = ferroscope_scene::Scene::parse(scene_json)
        .map_err(|p| JsValue::from_str(&format!("{} problem(s) in the scene", p.len())))?;
    let rec = scene
        .record_with(
            |want| {
                let stem = want
                    .rsplit(['/', '\\'])
                    .next()
                    .unwrap_or(want)
                    .trim_end_matches(".urdf");
                (stem == robot_name).then(|| urdf.to_string())
            },
            wasm_note,
        )
        .map_err(|e| JsValue::from_str(&e))?;
    Ok(rec.bytes)
}

/// Run a scene's cases and return the verdict table, without the recordings.
///
/// Returns `{ name, cases: [{ label, steps, joules, passed, checks: [{name, ok, why}] }] }`, or
/// `{ error, problems }`. The bytes are left out on purpose: a grid of 256 cases is a lot of
/// memory to hand a page that will look at one of them, so the caller records the case it wants
/// with [`record_case`].
#[wasm_bindgen]
pub fn sweep_scene(scene_json: &str, robot_name: &str, urdf: &str) -> String {
    use ferroscope_schema::json::Obj;
    let suite = match ferroscope_scene::Suite::parse(scene_json) {
        Ok(s) => s,
        Err(problems) => {
            return Obj::new()
                .str("error", "invalid scene")
                .strs(
                    "problems",
                    &problems
                        .iter()
                        .map(|p| format!("{}: {}", p.path, p.message))
                        .collect::<Vec<_>>(),
                )
                .finish();
        }
    };
    let results = match suite.run_with(|want| resolve(want, robot_name, urdf), &mut wasm_note) {
        Ok(r) => r,
        Err(e) => return Obj::new().str("error", &e).finish(),
    };
    let cases: Vec<String> = results
        .iter()
        .map(|r| {
            let checks: Vec<String> = r
                .checks
                .iter()
                .map(|(n, ok, why)| {
                    format!(r#"{{"name":{},"ok":{ok},"why":{}}}"#, quote(n), quote(why))
                })
                .collect();
            format!(
                r#"{{"label":{},"steps":{},"joules":{:.4},"passed":{},"checks":[{}]}}"#,
                quote(&r.label),
                r.recorded.steps,
                r.recorded.total_j,
                r.passed(),
                checks.join(",")
            )
        })
        .collect();
    format!(
        r#"{{"name":{},"passed":{},"failed":{},"cases":[{}]}}"#,
        quote(&suite.name),
        results.iter().filter(|r| r.passed()).count(),
        results.iter().filter(|r| !r.passed()).count(),
        cases.join(",")
    )
}

/// Record one case of a scene's grid and hand back its MCAP bytes.
#[wasm_bindgen]
pub fn record_case(
    scene_json: &str,
    index: usize,
    robot_name: &str,
    urdf: &str,
) -> Result<Vec<u8>, JsValue> {
    let suite = ferroscope_scene::Suite::parse(scene_json)
        .map_err(|p| JsValue::from_str(&format!("{} problem(s) in the scene", p.len())))?;
    let scene = suite
        .scene(index)
        .map_err(|p| JsValue::from_str(&format!("case {index}: {} problem(s)", p.len())))?;
    let rec = scene
        .record_with(|want| resolve(want, robot_name, urdf), wasm_note)
        .map_err(|e| JsValue::from_str(&e))?;
    Ok(rec.bytes)
}

/// Match a robot reference against the one description the page supplied.
fn resolve(want: &str, name: &str, urdf: &str) -> Option<String> {
    if name.is_empty() {
        return None;
    }
    let stem = want
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(want)
        .trim_end_matches(".urdf");
    (stem == name).then(|| urdf.to_string())
}

fn quote(s: &str) -> String {
    let mut out = String::new();
    ferroscope_schema::json::write_string(&mut out, s);
    out
}
