//! **The viewer bundle** — a recording flattened into the lanes a viewer draws.
//!
//! One function, two callers. `ferroscope export` writes it to a file for a page that has to
//! show a real run over a CDN with no wasm fetch at all; `ferroscope-wasm` calls the same
//! function inside the browser on bytes the reader just dropped onto the page. Both paths
//! produce identical numbers because there is only one implementation of them.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use crate::json::{Obj, Value};
use crate::{mcap, verify, RECEIPT_BLOCK};

/// Series longer than this are strided down. A 1080 px lane cannot show more.
const MAX_POINTS: usize = 4_000;

/// Flatten a recording into the viewer bundle. `None` when the bytes are not a readable
/// MCAP file or a payload will not parse.
pub fn bundle(bytes: &[u8]) -> Option<String> {
    let log = mcap::read(bytes).ok()?;
    let v = verify(bytes);

    let mut poses: BTreeMap<String, Vec<[f64; 8]>> = BTreeMap::new();
    let mut scalars: BTreeMap<String, Vec<[f64; 2]>> = BTreeMap::new();
    let mut power: BTreeMap<String, Vec<[f64; 2]>> = BTreeMap::new();
    let mut contacts: Vec<[f64; 5]> = Vec::new();
    let mut lag: Vec<[f64; 2]> = Vec::new();
    // (frame, id) -> the geometry's declaration plus its pose track over time.
    let mut geom: BTreeMap<String, (Value, Vec<[f64; 15]>)> = BTreeMap::new();
    // child frame name -> pose track, so a Geometry can name a frame rather than a topic.
    let mut frames: BTreeMap<String, Vec<[f64; 8]>> = BTreeMap::new();
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();

    for m in &log.messages {
        let ch = log.channel(m.channel_id)?;
        let schema = log
            .schema(ch.schema_id)
            .map(|s| s.name.as_str())
            .unwrap_or("");
        *counts.entry(ch.topic.clone()).or_default() += 1;

        let val = crate::json::parse(std::str::from_utf8(&m.data).ok()?)?;
        let t = m.log_time as f64 * 1e-9;
        let num = |k: &str| val.get(k).and_then(|x| x.as_f64()).unwrap_or(0.0);
        let arr = |k: &str| -> Vec<f64> {
            val.get(k)
                .and_then(|x| x.as_array())
                .map(|a| a.iter().filter_map(|e| e.as_f64()).collect())
                .unwrap_or_default()
        };

        // Wall−sim drift, sampled from whichever channel is densest; one lane is enough.
        if ch.topic.ends_with("/base") {
            lag.push([t, (m.publish_time as i64 - m.log_time as i64) as f64 * 1e-6]);
        }

        match schema {
            "ferroscope.Transform" => {
                let p = arr("translation");
                let q = arr("rotation");
                if p.len() == 3 && q.len() == 4 {
                    let row = [t, p[0], p[1], p[2], q[0], q[1], q[2], q[3]];
                    poses.entry(ch.topic.clone()).or_default().push(row);
                    if let Some(child) = val.get("child").and_then(|x| x.as_str()) {
                        frames.entry(child.to_string()).or_default().push(row);
                    }
                }
            }
            "ferroscope.Scalar" => scalars
                .entry(ch.topic.clone())
                .or_default()
                .push([t, num("value")]),
            "ferroscope.EnergySample" => {
                let rail = val
                    .get("rail")
                    .and_then(|r| r.as_str())
                    .unwrap_or("overhead");
                let source = val.get("source").and_then(|s| s.as_str()).unwrap_or("?");
                power
                    .entry(format!("{rail}/{source}"))
                    .or_default()
                    .push([t, num("watts")]);
            }
            "ferroscope.Contact" => {
                let p = arr("point");
                if p.len() == 3 {
                    contacts.push([t, p[0], p[1], p[2], num("force_n")]);
                }
            }
            "ferroscope.Geometry" => {
                let frame = val.get("frame").and_then(|x| x.as_str()).unwrap_or("world");
                let gid = val.get("id").and_then(|x| x.as_str()).unwrap_or("?");
                let key = format!("{frame}/{gid}");
                let tr = arr("translation");
                let rot = arr("rotation");
                let sz = arr("size");
                let col = arr("color");
                let g = |v: &[f64], i: usize, d: f64| *v.get(i).unwrap_or(&d);
                let pose = [
                    t,
                    g(&tr, 0, 0.0),
                    g(&tr, 1, 0.0),
                    g(&tr, 2, 0.0),
                    g(&rot, 0, 0.0),
                    g(&rot, 1, 0.0),
                    g(&rot, 2, 0.0),
                    g(&rot, 3, 1.0),
                    g(&sz, 0, 0.1),
                    g(&sz, 1, 0.1),
                    g(&sz, 2, 0.1),
                    g(&col, 0, 0.8),
                    g(&col, 1, 0.7),
                    g(&col, 2, 0.4),
                    g(&col, 3, 1.0),
                ];
                let entry = geom.entry(key).or_insert_with(|| {
                    (
                        Obj::new()
                            .str("frame", frame)
                            .str("id", gid)
                            .str(
                                "shape",
                                val.get("shape").and_then(|x| x.as_str()).unwrap_or("box"),
                            )
                            .str(
                                "mesh",
                                val.get("mesh").and_then(|x| x.as_str()).unwrap_or(""),
                            )
                            .raw(
                                "points",
                                &val.get("points")
                                    .map(|p| p.to_json())
                                    .unwrap_or_else(|| "[]".into()),
                            )
                            .finish_value(),
                        Vec::new(),
                    )
                });
                entry.1.push(pose);
            }
            "ferroscope.JointState" => {
                // Joint positions ride as one scalar lane per joint, named from the file.
                let names = val
                    .get("names")
                    .and_then(|x| x.as_array())
                    .map(|a| {
                        a.iter()
                            .map(|e| e.as_str().unwrap_or("?").to_string())
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                for (i, p) in arr("position").into_iter().enumerate() {
                    let name = names.get(i).cloned().unwrap_or_else(|| i.to_string());
                    scalars
                        .entry(format!("{}:{}", ch.topic, name))
                        .or_default()
                        .push([t, p]);
                }
            }
            _ => {}
        }
    }

    let mut out = String::from("{");
    let _ = write!(out, "\"format\":\"ferroscope.bundle.v1\"");
    let _ = write!(out, ",\"profile\":\"{}\"", log.profile);
    let _ = write!(out, ",\"messages\":{}", log.messages.len());
    if let Some((t0, t1)) = log.time_span() {
        let _ = write!(out, ",\"span\":[{},{}]", t0 as f64 * 1e-9, t1 as f64 * 1e-9);
    }

    out.push_str(",\"topics\":[");
    for (i, (topic, n)) in counts.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        let schema = log
            .channel_by_topic(topic)
            .and_then(|c| log.schema(c.schema_id))
            .map(|s| s.name.clone())
            .unwrap_or_default();
        out.push_str(
            &Obj::new()
                .str("topic", topic)
                .str("schema", &schema)
                .uint("count", *n as u64)
                .finish(),
        );
    }
    out.push(']');

    out.push_str(",\"poses\":");
    write_map(&mut out, &poses);
    out.push_str(",\"scalars\":");
    write_map(&mut out, &scalars);
    out.push_str(",\"power\":");
    write_map(&mut out, &power);
    // Geometry: one object per part, with its pose track strided down like any other lane.
    out.push_str(",\"geometry\":[");
    for (i, (_, (decl, track))) in geom.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(decl.to_json().trim_end_matches('}'));
        out.push_str(",\"track\":");
        write_series(&mut out, &stride(track));
        out.push('}');
    }
    out.push(']');

    out.push_str(",\"attachments\":[");
    for (i, a) in log.attachments.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(
            &Obj::new()
                .str("name", &a.name)
                .str("media_type", &a.media_type)
                .uint("bytes", a.data.len() as u64)
                .finish(),
        );
    }
    out.push(']');

    out.push_str(",\"frames\":");
    write_map(&mut out, &frames);

    out.push_str(",\"lag_ms\":");
    write_series(&mut out, &stride(&lag));
    out.push_str(",\"contacts\":");
    write_series(&mut out, &stride(&contacts));

    if let Some(v) = &v {
        let q = &v.quote;
        let _ = write!(
            out,
            ",\"energy\":{}",
            Obj::new()
                .num("compute_j", q.compute_j)
                .num("actuation_j", q.actuation_j)
                .num("overhead_j", q.overhead_j)
                .num("total_j", q.total_j)
                .num("duration_s", q.duration_s)
                .num("mean_w", q.mean_power_w())
                .num("peak_w", q.peak_w)
                .str("peak_source", &q.peak_source)
                .str("coverage", &q.coverage.to_string())
                .raw("quotable", if q.quotable { "true" } else { "false" })
                .finish()
        );
        let _ = write!(
            out,
            ",\"receipt\":{}",
            Obj::new()
                .str("scenario", &v.receipt.spec.scenario)
                .uint("seed", v.receipt.spec.seed)
                .str("precision", &v.receipt.precision.to_string())
                .str("platform", &v.receipt.platform)
                .str("integrator", &v.receipt.spec.integrator)
                .str("solver", &v.receipt.spec.solver)
                .str("build", &v.receipt.spec.build)
                .uint("steps", v.receipt.spec.steps)
                .str("spec_digest", &v.receipt.spec_digest)
                .str("trace_digest", &v.receipt.trace_digest)
                .str("recomputed", &v.recomputed)
                .uint("non_finite", v.receipt.non_finite)
                .raw("verified", if v.ok() { "true" } else { "false" })
                .finish()
        );
    } else if log.metadata_block(RECEIPT_BLOCK).is_none() {
        out.push_str(",\"receipt\":null");
    }

    out.push('}');
    Some(out)
}

fn stride<const N: usize>(v: &[[f64; N]]) -> Vec<[f64; N]> {
    if v.len() <= MAX_POINTS {
        return v.to_vec();
    }
    let step = v.len().div_ceil(MAX_POINTS);
    v.iter().step_by(step).copied().collect()
}

fn write_series<const N: usize>(out: &mut String, rows: &[[f64; N]]) {
    out.push('[');
    for (i, r) in rows.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push('[');
        for (j, x) in r.iter().enumerate() {
            if j > 0 {
                out.push(',');
            }
            crate::json::write_number(out, round6(*x));
        }
        out.push(']');
    }
    out.push(']');
}

fn write_map<const N: usize>(out: &mut String, m: &BTreeMap<String, Vec<[f64; N]>>) {
    out.push('{');
    for (i, (k, v)) in m.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        crate::json::write_string(out, k);
        out.push(':');
        write_series(out, &stride(v));
    }
    out.push('}');
}

/// Six decimals is a micrometre, a microsecond, or a microwatt — below anything a lane can
/// draw, and it keeps the bundle from tripling in size on float noise. The *receipt* still
/// travels at full precision, because that is the number nobody may round.
fn round6(x: f64) -> f64 {
    if x.is_finite() {
        (x * 1e6).round() / 1e6
    } else {
        x
    }
}
