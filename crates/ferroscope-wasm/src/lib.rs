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
