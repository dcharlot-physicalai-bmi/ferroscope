//! End-to-end: record → seal → read the file back → recompute the receipt from the bytes.
//!
//! The tests that matter here are the negative ones. A receipt that only ever says "yes" is
//! decoration; each case below is a specific way a recording can stop deserving its own
//! receipt, and the test asserts that it gets caught.

use ferroscope_ledger::Rail;
use ferroscope_mcap::{Writer, WriterOptions};
use ferroscope_receipt::{compare, Precision, Receipt, Tolerance, Verdict};
use ferroscope_schema::{
    json, mcap, trace_from, verify, Contact, JointState, Recorder, Stamp, RECEIPT_BLOCK,
};

/// A short deterministic run: a mass on a spring, sampled at 1 kHz.
fn record(seed: u64, perturb_at: Option<u64>) -> Vec<u8> {
    let mut rec = Recorder::new(Vec::new(), Precision::Quantized { drop_bits: 12 });
    let mut x = 0.2f64;
    let mut v = 0.0f64;
    for step in 0..200u64 {
        let t = Stamp::at(step * 1_000_000, step * 1_010_000, step);
        let mut f = -40.0 * x;
        if Some(step) == perturb_at {
            f *= 1.001;
        }
        v += f * 1e-3;
        x += v * 1e-3;

        rec.transform(
            "/body",
            t,
            "world",
            "body",
            [x, 0.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        )
        .unwrap();
        rec.joints(
            "/joints",
            t,
            &JointState {
                names: vec!["slide".into()],
                position: vec![x],
                velocity: vec![v],
                effort: vec![f],
            },
        )
        .unwrap();
        rec.energy("/energy/soc", t, Rail::Compute, "soc", 6.0)
            .unwrap();
        rec.energy("/energy/motor", t, Rail::Actuation, "motor", f.abs() * 2.0)
            .unwrap();
        rec.scalar("/err", t, x, "m").unwrap();
        if step % 50 == 0 {
            rec.event("/log", t, "info", "tick").unwrap();
        }
        if x.abs() > 0.15 {
            rec.contact(
                "/contacts",
                t,
                &Contact {
                    body_a: "body".into(),
                    body_b: "stop".into(),
                    point: [x, 0.0, 0.0],
                    normal: [1.0, 0.0, 0.0],
                    force_n: f.abs(),
                    penetration_m: x.abs() - 0.15,
                },
            )
            .unwrap();
        }
    }
    let spec = ferroscope_receipt::RunSpec::new("spring", seed)
        .dt_ns(1_000_000)
        .steps(200)
        .integrator("semi-implicit-euler")
        .solver("none")
        .build("integration-test");
    rec.seal(spec, "test-platform").unwrap().0
}

#[test]
fn a_sealed_recording_verifies_from_its_own_bytes() {
    let bytes = record(1, None);
    let v = verify(&bytes).expect("receipt present");
    assert!(v.spec_matches, "spec digest recomputes");
    assert!(v.trace_matches, "trace digest recomputes from the messages");
    assert!(v.ok());
    // Events carry no numbers and must not enter the digest — otherwise a log line could
    // change a determinism verdict.
    assert_eq!(v.messages, 200 * 5 + count_contacts(&bytes));
}

fn count_contacts(bytes: &[u8]) -> usize {
    let log = mcap::read(bytes).unwrap();
    log.messages_on("/contacts").count()
}

#[test]
fn the_energy_ledger_survives_the_file() {
    let bytes = record(1, None);
    let v = verify(&bytes).unwrap();
    // 6 W of compute over 199 ms of intervals.
    assert!(v.quote.compute_j > 1.0, "got {}", v.quote.compute_j);
    assert!(v.quote.actuation_j > 0.0);
    assert!(v.quote.quotable, "1 kHz sampling has no gaps");
    assert!(
        (v.quote.compute_j + v.quote.actuation_j + v.quote.overhead_j - v.quote.total_j).abs()
            < 1e-9
    );
}

#[test]
fn two_runs_of_the_same_spec_agree_and_the_shortcut_says_so() {
    let a = record(1, None);
    let b = record(1, None);
    let va = verify(&a).unwrap();
    let vb = verify(&b).unwrap();
    assert_eq!(va.receipt.spec_digest, vb.receipt.spec_digest);
    assert_eq!(va.receipt.trace_digest, vb.receipt.trace_digest);

    let (_, ta) = trace_from(&a).unwrap();
    let (_, tb) = trace_from(&b).unwrap();
    assert_eq!(compare(&ta, &tb, Tolerance::default()), Verdict::BitExact);
}

#[test]
fn a_perturbed_run_is_located_not_merely_rejected() {
    let a = record(1, None);
    let b = record(1, Some(90));
    let (_, ta) = trace_from(&a).unwrap();
    let (_, tb) = trace_from(&b).unwrap();
    match compare(&ta, &tb, Tolerance::default()) {
        Verdict::Diverged { step, channel, .. } => {
            assert_eq!(step, 90, "the first differing step, not just 'differs'");
            assert!(!channel.is_empty());
        }
        other => panic!("expected a located divergence, got {other}"),
    }
}

#[test]
fn editing_the_receipt_metadata_is_caught() {
    let bytes = record(1, None);
    let log = mcap::read(&bytes).unwrap();
    let mut kv = log.metadata_block(RECEIPT_BLOCK).unwrap().to_vec();
    for pair in kv.iter_mut() {
        if pair.0 == "seed" {
            pair.1 = "999".into();
        }
    }
    let r = Receipt::from_pairs(&kv).unwrap();
    assert!(
        !r.self_consistent(),
        "a seed changed after the fact must not still hash to the stored spec digest"
    );
}

#[test]
fn re_encoding_the_file_with_one_number_changed_fails_the_trace_digest() {
    // The hard case: not a corrupted byte (the chunk CRC catches those) but a *well-formed*
    // file, re-encoded properly, carrying the original receipt, with a single value nudged.
    // Only the trace digest can catch this, which is the reason it exists.
    let bytes = record(1, None);
    let log = mcap::read(&bytes).unwrap();

    let mut w = Writer::new(Vec::new(), WriterOptions::new("ferroscope", "tamper-test"));
    let mut schema_map = std::collections::BTreeMap::new();
    for s in &log.schemas {
        schema_map.insert(s.id, w.add_schema(&s.name, &s.encoding, &s.data).unwrap());
    }
    let mut chan_map = std::collections::BTreeMap::new();
    for c in &log.channels {
        chan_map.insert(
            c.id,
            w.add_channel(
                &c.topic,
                *schema_map.get(&c.schema_id).unwrap_or(&0),
                &c.message_encoding,
                &c.metadata,
            )
            .unwrap(),
        );
    }
    let target = log.channel_by_topic("/err").unwrap().id;
    let mut nudged = false;
    for m in &log.messages {
        let mut data = m.data.clone();
        if m.channel_id == target && m.sequence == 120 && !nudged {
            let text = String::from_utf8(data).unwrap();
            let v = json::parse(&text).unwrap();
            let old = v.get("value").unwrap().as_f64().unwrap();
            // One part in a million: invisible on a plot, fatal to a digest.
            let new = old * (1.0 + 1e-6);
            data = text
                .replace(
                    &format!("\"value\":{}", trim(old)),
                    &format!("\"value\":{}", trim(new)),
                )
                .into_bytes();
            nudged = true;
        }
        w.write_message(
            chan_map[&m.channel_id],
            m.sequence,
            m.log_time,
            m.publish_time,
            &data,
        )
        .unwrap();
    }
    assert!(nudged, "the fixture must actually have been modified");
    for (name, kv) in &log.metadata {
        w.write_metadata(name, kv).unwrap();
    }
    let tampered = w.finish().unwrap();

    // The file is structurally perfect: it parses, and its CRCs are correct.
    assert!(mcap::read(&tampered).is_ok());
    let v = verify(&tampered).expect("receipt still present");
    assert!(v.spec_matches, "the spec was not touched");
    assert!(
        !v.trace_matches,
        "a value changed by one part in a million must break the trace digest"
    );
    assert!(!v.ok());
}

fn trim(v: f64) -> String {
    let mut s = String::new();
    json::write_number(&mut s, v);
    s
}

#[test]
fn a_recording_opens_in_a_viewer_that_never_heard_of_ferroscope() {
    // Every channel must carry a schema with a published encoding; a viewer with no
    // Ferroscope support renders the payload from that alone.
    let bytes = record(1, None);
    let log = mcap::read(&bytes).unwrap();
    assert!(!log.channels.is_empty());
    for c in &log.channels {
        assert_eq!(c.message_encoding, "json");
        let s = log.schema(c.schema_id).expect("every channel has a schema");
        assert_eq!(s.encoding, "jsonschema");
        let text = std::str::from_utf8(&s.data).unwrap();
        assert!(
            json::parse(text).is_some(),
            "schema for {} is not valid JSON",
            c.topic
        );
    }
}

#[test]
fn the_three_clocks_are_all_recoverable() {
    let bytes = record(1, None);
    let log = mcap::read(&bytes).unwrap();
    let m = log.messages_on("/body").nth(10).unwrap();
    // sim time is the MCAP log_time, wall time is publish_time…
    assert_eq!(m.log_time, 10 * 1_000_000);
    assert_eq!(m.publish_time, 10 * 1_010_000);
    // …and the control step is in the payload, where a viewer with no Ferroscope support
    // can still read it as a plain field.
    let v = json::parse(std::str::from_utf8(&m.data).unwrap()).unwrap();
    assert_eq!(v.get("step").unwrap().as_f64(), Some(10.0));
}

#[test]
fn one_topic_cannot_carry_two_schemas() {
    // This is not hypothetical. The URDF exporter wrote geometry and transforms to the same
    // `/scene/<link>` topic, and every transform landed on a geometry-schema'd channel where a
    // reader would decode it with the wrong shape. Nothing stopped it, so now something does.
    let mut rec = Recorder::new(Vec::new(), Precision::Exact);
    let t = Stamp::sim(0, 0);
    rec.transform(
        "/scene/link",
        t,
        "world",
        "link",
        [0.0, 0.0, 1.0],
        [0.0, 0.0, 0.0, 1.0],
    )
    .unwrap();
    let err = rec
        .geometry(
            "/scene/link",
            t,
            &ferroscope_schema::Geometry::boxed("world", "link", [1.0; 3]),
        )
        .unwrap_err();
    // And the message says how to fix it, not just that something is wrong.
    assert!(format!("{err}").contains("use a second topic"), "{err}");
    match &err {
        ferroscope_mcap::Error::SchemaConflict {
            topic,
            existing,
            requested,
        } => {
            assert_eq!(topic, "/scene/link");
            assert_eq!(existing, "ferroscope.Transform");
            assert_eq!(requested, "ferroscope.Geometry");
        }
        other => panic!("expected a schema conflict, got {other}"),
    }
}

#[test]
fn the_same_schema_on_one_topic_is_still_fine() {
    let mut rec = Recorder::new(Vec::new(), Precision::Exact);
    for step in 0..3u64 {
        let t = Stamp::sim(step, step);
        rec.transform(
            "/scene/link",
            t,
            "world",
            "link",
            [0.0, 0.0, step as f64],
            [0.0, 0.0, 0.0, 1.0],
        )
        .unwrap();
    }
    let (bytes, _, _) = rec
        .seal(ferroscope_receipt::RunSpec::new("s", 0), "test")
        .unwrap();
    assert_eq!(
        mcap::read(&bytes)
            .unwrap()
            .messages_on("/scene/link")
            .count(),
        3
    );
}
