//! End-to-end: record → seal → read the file back → recompute the receipt from the bytes.
//!
//! The tests that matter here are the negative ones. A receipt that only ever says "yes" is
//! decoration; each case below is a specific way a recording can stop deserving its own
//! receipt, and the test asserts that it gets caught.

use ferroscope_ledger::Rail;
use ferroscope_mcap::{Writer, WriterOptions};
use ferroscope_receipt::{Precision, Receipt, Tolerance, Verdict, compare};
use ferroscope_schema::{
    Contact, JointState, RECEIPT_BLOCK, Recorder, Stamp, json, mcap, trace_from, verify,
};

/// A short deterministic run: a mass on a spring, sampled at 1 kHz.
fn record_with_production(
    seed: u64,
    perturb_at: Option<u64>,
    production: impl FnOnce() -> Vec<(String, String)>,
) -> Vec<u8> {
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
    rec.seal_with(spec, "test-platform", production).unwrap().0
}

/// The same run, sealed the plain way.
fn record(seed: u64, perturb_at: Option<u64>) -> Vec<u8> {
    record_with_production(seed, perturb_at, Vec::new)
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
fn a_production_block_moves_neither_digest() {
    // The invariant everything rests on: what producing a file COST is measured, varies run to
    // run, and must therefore never touch the determinism claim. Two identical runs, one sealed
    // plain and one carrying a production note, must agree digest for digest — and both must
    // still verify from their own bytes.
    let plain = record(7, None);
    let noted = record_with_production(7, None, || {
        vec![
            ("joules".into(), "12.345678".into()),
            ("duration_s".into(), "1.9".into()),
            ("source".into(), "test counter".into()),
            ("basis".into(), "cumulative energy counter".into()),
        ]
    });

    let (vp, vn) = (verify(&plain).unwrap(), verify(&noted).unwrap());
    assert!(vp.ok() && vn.ok(), "both must stand behind their receipts");
    assert_eq!(
        vp.receipt.trace_digest, vn.receipt.trace_digest,
        "a production note moved the trace digest: the measurement leaked into the claim"
    );
    assert_eq!(
        vp.receipt.spec_digest, vn.receipt.spec_digest,
        "a production note moved the spec digest"
    );

    // And the note is genuinely in the file, readable by name.
    let log = mcap::read(&noted).unwrap();
    let kv = log
        .metadata_block(ferroscope_schema::PRODUCTION_BLOCK)
        .expect("the production block must be present");
    assert_eq!(
        kv.iter()
            .find(|(k, _)| k == "joules")
            .map(|(_, v)| v.as_str()),
        Some("12.345678")
    );
    assert!(
        mcap::read(&plain)
            .unwrap()
            .metadata_block(ferroscope_schema::PRODUCTION_BLOCK)
            .is_none(),
        "a plain seal must not invent a block"
    );
}

#[test]
fn an_empty_production_note_writes_no_block() {
    // "Nothing to say" and "a block full of nothing" are different files; the reader that
    // checks for the block's presence must be able to trust it.
    let bytes = record_with_production(3, None, Vec::new);
    assert!(
        mcap::read(&bytes)
            .unwrap()
            .metadata_block(ferroscope_schema::PRODUCTION_BLOCK)
            .is_none()
    );
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

#[test]
fn streaming_verify_agrees_with_the_slice_verify() {
    // Two verifiers are two definitions of the receipt unless something holds them equal.
    let bytes = record(7, None);
    let slice = ferroscope_schema::verify(&bytes).expect("slice verify");
    let streamed = ferroscope_schema::verify_streaming(|| Ok(std::io::Cursor::new(bytes.clone())))
        .expect("streaming verify");

    assert_eq!(slice.recomputed, streamed.recomputed, "digests differ");
    assert_eq!(slice.receipt.trace_digest, streamed.receipt.trace_digest);
    assert_eq!(slice.ok(), streamed.ok());
    assert_eq!(slice.messages, streamed.messages, "message counts differ");
    assert_eq!(
        slice.quote.total_j, streamed.quote.total_j,
        "ledgers differ"
    );
    assert_eq!(slice.quote.compute_j, streamed.quote.compute_j);
    assert_eq!(slice.quote.actuation_j, streamed.quote.actuation_j);
    assert!(streamed.ok(), "a good file must verify on both paths");
}

#[test]
fn streaming_verify_catches_a_tampered_payload() {
    // Tamper a real MESSAGE payload. The first `"watts":` in the file is inside the JSON SCHEMA
    // text — `"watts":{"type":"number"}` — and editing that is not editing a recorded number, so
    // the file still verifies and should. A payload is the occurrence followed by a digit or a
    // minus sign.
    let mut bytes = record(7, None);
    let needle = b"\"watts\":";
    let at = bytes
        .windows(needle.len())
        .enumerate()
        .filter(|(_, w)| *w == needle)
        .map(|(i, _)| i)
        .find(|i| {
            let c = bytes[i + needle.len()];
            c.is_ascii_digit() || c == b'-'
        })
        .expect("no watts VALUE in the fixture, only schema text");
    let digit = bytes[at + needle.len()..]
        .iter()
        .position(|c| c.is_ascii_digit())
        .expect("digit")
        + at
        + needle.len();
    bytes[digit] = if bytes[digit] == b'9' {
        b'1'
    } else {
        bytes[digit] + 1
    };

    // Both paths must refuse, and refuse the same way — either the chunk CRC catches the edit
    // or the recomputed digest does. Asserting them together is what keeps the two verifiers
    // from drifting into two different standards of evidence.
    let streamed = ferroscope_schema::verify_streaming(|| Ok(std::io::Cursor::new(bytes.clone())));
    let sliced = ferroscope_schema::verify(&bytes);
    let accepted = |v: &Option<ferroscope_schema::Verification>| v.as_ref().is_some_and(|x| x.ok());
    assert!(
        !accepted(&streamed),
        "streaming verify accepted a tampered payload"
    );
    assert!(
        !accepted(&sliced),
        "slice verify accepted a tampered payload"
    );
    assert_eq!(
        accepted(&streamed),
        accepted(&sliced),
        "the two verifiers disagree about a tampered file"
    );
}

#[test]
fn the_streaming_bundle_is_byte_identical_to_the_slice_bundle() {
    // Two bundle paths are two definitions of what a bundle is unless something holds them
    // equal — the failure this project has had once per comparator. Byte-identity is the
    // strongest form of that check available, and it is achievable because the stride is
    // computed from counts rather than discovered on the way through.
    for bytes in [record(11, None), strided_recording()] {
        let sliced = ferroscope_schema::bundle(&bytes).expect("slice bundle");
        let streamed =
            ferroscope_schema::bundle_streaming(|| Ok(std::io::Cursor::new(bytes.clone())))
                .expect("streaming bundle");
        assert_eq!(
            sliced.len(),
            streamed.len(),
            "bundle sizes differ for a {} byte recording: {} vs {}",
            bytes.len(),
            sliced.len(),
            streamed.len()
        );
        assert_eq!(sliced, streamed, "the two bundle paths disagree");
    }
}

#[test]
fn the_streaming_bundle_carries_the_receipt_it_recomputed() {
    let bytes = record(11, None);
    let streamed = ferroscope_schema::bundle_streaming(|| Ok(std::io::Cursor::new(bytes.clone())))
        .expect("streaming bundle");
    assert!(
        streamed.contains("\"verified\":true"),
        "a good recording's bundle must say it verified"
    );
    assert!(streamed.contains("\"receipt\""), "receipt missing");
}

/// Drive a [`BundleFold`] the way a browser does: two passes over the same bytes, delivered in
/// blocks of `block` that fall wherever they fall.
fn folded_in_blocks(bytes: &[u8], block: usize) -> Option<String> {
    let mut fold = ferroscope_schema::BundleFold::new();
    for part in bytes.chunks(block.max(1)) {
        if !fold.push(part) {
            break;
        }
    }
    assert_eq!(fold.pass(), 1, "pass one did not stay pass one");
    fold.rewind();
    assert_eq!(fold.pass(), 2, "rewind did not begin pass two");
    for part in bytes.chunks(block.max(1)) {
        if !fold.push(part) {
            break;
        }
    }
    fold.finish()
}

#[test]
fn a_pushed_bundle_is_byte_identical_to_a_read_one() {
    // The browser cannot hand out a `Read`: `File.slice(a, b).arrayBuffer()` yields a block
    // when it is ready. So the fold is pushed rather than pulled — and the whole point is that
    // this changes nothing about the answer. Byte-identity across every block size is the
    // check, because a bundle that merely "looks the same" is how two paths drift apart.
    for bytes in [record(11, None), strided_recording()] {
        let sliced = ferroscope_schema::bundle(&bytes).expect("slice bundle");
        for block in [1usize, 3, 64, 1000, 4096, 65536, bytes.len()] {
            let pushed = folded_in_blocks(&bytes, block).expect("pushed bundle");
            assert_eq!(
                pushed,
                sliced,
                "the bundle changed when a {} byte recording arrived {block} at a time",
                bytes.len()
            );
        }
    }
}

/// A recording with more points than a lane can draw, so every lane is strided.
///
/// The stride is the one part of the bundle that depends on a count taken in an earlier pass,
/// and so the one part a pushed fold could get wrong on its own. A 200-step fixture is under
/// the limit and strides by 1 \u{2014} which is to say it does not test this at all, and a
/// mutation that threw the strides away passed the byte-identity check against it.
fn strided_recording() -> Vec<u8> {
    long_recording(9_000)
}

/// A recording of `steps` steps, for the tests that need length rather than content.
fn long_recording(steps: u64) -> Vec<u8> {
    let mut rec = Recorder::new(Vec::new(), Precision::Exact);
    for step in 0..steps {
        let t = Stamp::at(step * 1_000_000, step * 1_000_000, step);
        let x = (step as f64) * 1e-4;
        rec.transform(
            "/body",
            t,
            "world",
            "body",
            [x, 0.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        )
        .unwrap();
        rec.scalar("/err", t, x, "m").unwrap();
        rec.energy("/energy/motor", t, Rail::Actuation, "motor", 3.0)
            .unwrap();
    }
    let spec = ferroscope_receipt::RunSpec::new("long", 1)
        .dt_ns(1_000_000)
        .steps(steps)
        .integrator("semi-implicit-euler")
        .solver("none")
        .build("integration-test");
    rec.seal_with(spec, "test-platform", Vec::new).unwrap().0
}

/// The largest number of bytes the fold ever holds while reading `bytes`.
fn peak_buffered(bytes: &[u8]) -> usize {
    let mut fold = ferroscope_schema::BundleFold::new();
    let mut worst = 0usize;
    for part in bytes.chunks(8192) {
        if !fold.push(part) {
            break;
        }
        worst = worst.max(fold.buffered());
    }
    worst
}

#[test]
fn a_pushed_fold_holds_one_record_not_the_recording() {
    // What the browser is buying. Not "a small number of bytes" — an absolute bound would just
    // encode today's chunk target — but a number that does NOT move when the recording gets an
    // order of magnitude longer. A fold that accumulated the file would still pass every
    // correctness test above and would fail here.
    let short = long_recording(200);
    let long = long_recording(4_000);
    assert!(
        long.len() > short.len() * 5,
        "fixtures are not far enough apart: {} vs {}",
        short.len(),
        long.len()
    );
    let (a, b) = (peak_buffered(&short), peak_buffered(&long));
    assert!(
        b <= a + 8192,
        "the fold held {a} bytes of a {} byte recording and {b} of a {} byte one \u{2014} it is \
         growing with the file",
        short.len(),
        long.len()
    );
}

#[test]
fn a_pushed_fold_that_never_rewinds_yields_nothing() {
    // Pass two is where the lanes are built. A fold asked to finish after one pass has counted
    // the recording and drawn none of it, and returning a bundle of empty lanes would be a
    // recording that silently renders blank.
    let bytes = record(11, None);
    let mut fold = ferroscope_schema::BundleFold::new();
    for part in bytes.chunks(4096) {
        if !fold.push(part) {
            break;
        }
    }
    assert!(fold.finish().is_none(), "a one-pass fold produced a bundle");
}

#[test]
fn a_pushed_fold_refuses_a_torn_recording() {
    // A file cut mid-record has to be refused rather than folded into a short bundle wearing a
    // whole one's clothes.
    let bytes = record(40, None);
    let cut = bytes.len() * 2 / 3;
    let torn = &bytes[..cut];
    let mut fold = ferroscope_schema::BundleFold::new();
    for part in torn.chunks(4096) {
        if !fold.push(part) {
            break;
        }
    }
    fold.rewind();
    for part in torn.chunks(4096) {
        if !fold.push(part) {
            break;
        }
    }
    let out = fold.finish();
    // Either the framing rejected it outright, or the bundle it produced does not claim the
    // recording verified — what must never happen is a torn file reporting a clean receipt.
    if let Some(json) = out {
        assert!(
            !json.contains("\"verified\":true"),
            "a file cut at {cut} bytes produced a bundle claiming it verified"
        );
    }
}

#[test]
fn a_pushed_fold_keeps_what_a_screen_draws_not_what_the_run_recorded() {
    // The other half of flat memory, and the half nothing was checking: the framing buffer is
    // bounded by one record, but the LANES would grow with the recording if the stride were not
    // applied on the way in. Byte-identity cannot see this \u{2014} `finish` strides again on
    // the way out, so a fold that kept every point emits exactly the same bundle, just after
    // holding every sample to do it. In a browser that is the difference between opening the
    // file and not, so it gets its own measurement. (A mutation that threw the strides away
    // passed every other test in this file.)
    const STEPS: usize = 12_000;
    const LANES: usize = 4; // pose, frame, scalar, power
    let bytes = long_recording(STEPS as u64);
    let mut fold = ferroscope_schema::BundleFold::new();
    for part in bytes.chunks(65536) {
        if !fold.push(part) {
            break;
        }
    }
    assert_eq!(fold.kept_points(), 0, "pass one is a count, not a draw");
    fold.rewind();
    for part in bytes.chunks(65536) {
        if !fold.push(part) {
            break;
        }
    }
    let kept = fold.kept_points();
    let recorded = STEPS * LANES;
    assert!(kept > 0, "no lane points kept at all");
    // 4,000 points is what one lane can be drawn at, so four lanes is the ceiling however long
    // the run is. Keeping every sample would be three times that here and unbounded in general.
    assert!(
        kept <= 4_000 * LANES,
        "kept {kept} points of {recorded} recorded, over the {} a screen can draw",
        4_000 * LANES
    );
    assert!(
        kept * 2 < recorded,
        "kept {kept} of {recorded} recorded \u{2014} the stride is not being applied on the way in"
    );
}

#[test]
fn a_pushed_fold_refuses_two_passes_over_different_bytes() {
    // Nothing makes the caller push the same bytes twice, and in a browser the file on disk can
    // change between the two reads. A short second pass builds partial lanes and reports no
    // error: the run renders as if it barely happened. A recording with a receipt would at
    // least fail its digest; a live prefix has no receipt and would say nothing at all.
    let bytes = record(11, None);
    let mut fold = ferroscope_schema::BundleFold::new();
    for part in bytes.chunks(4096) {
        if !fold.push(part) {
            break;
        }
    }
    fold.rewind();
    for part in bytes[..bytes.len() / 2].chunks(4096) {
        if !fold.push(part) {
            break;
        }
    }
    assert!(
        fold.finish().is_none(),
        "a fold whose second pass saw half the recording produced a bundle"
    );
}

/// Compare two recordings both ways: by building the trajectories, and by walking both files.
fn both_ways(
    a: &[u8],
    b: &[u8],
) -> (
    ferroscope_receipt::Profile,
    Option<ferroscope_receipt::Profile>,
) {
    let tol = ferroscope_receipt::Tolerance::default();
    let ta = ferroscope_schema::trace_from(a).expect("trace a").1;
    let tb = ferroscope_schema::trace_from(b).expect("trace b").1;
    let held = ferroscope_receipt::profile(&ta, &tb, tol);
    let streamed = ferroscope_schema::profile_streaming(
        || Ok(std::io::Cursor::new(a.to_vec())),
        || Ok(std::io::Cursor::new(b.to_vec())),
        tol,
    );
    (held, streamed)
}

#[test]
fn a_streamed_comparison_is_the_same_comparison() {
    // `diff` was the last verb that bought its answer with memory, and the reason was never the
    // question: deciding where two runs parted is a fold over pairs in file order. This is what
    // says the fold and the held comparison agree — not "about the same", the same Profile.
    for (name, a, b) in [
        ("identical", record(11, None), record(11, None)),
        ("perturbed", record(11, None), record(11, Some(60))),
        ("perturbed late", record(3, None), record(3, Some(180))),
        ("different seeds", record(11, None), record(12, None)),
    ] {
        let (held, streamed) = both_ways(&a, &b);
        let streamed =
            streamed.unwrap_or_else(|| panic!("{name}: streaming refused an aligned pair"));
        assert_eq!(held, streamed, "{name}: the two comparisons disagree");
    }
}

#[test]
fn a_streamed_comparison_handles_a_run_that_stopped_early() {
    // A run that crashed half way is not an exotic case, and its tail is exactly what the report
    // already calls a gap. The streaming walk has to arrive at the same structure as the held
    // one, including which side is missing what.
    let whole = record(11, None);
    let short = record_steps(11, 120);
    let (held, streamed) = both_ways(&whole, &short);
    let streamed = streamed.expect("streaming refused a prefix-aligned pair");
    assert!(
        held.structural.is_some(),
        "the fixture is not actually a short run"
    );
    assert_eq!(
        held, streamed,
        "the two comparisons disagree about a short run"
    );
}

#[test]
fn a_streamed_comparison_matches_runs_that_do_not_emit_in_the_same_order() {
    // The first version of this walked the two queues in lockstep and refused the moment their
    // fronts disagreed. That is fine until a channel fires CONDITIONALLY — the fixture records a
    // contact only past a threshold — so two runs that have genuinely diverged stop emitting the
    // same samples at the same steps, which is exactly the pair anybody wants compared. Matching
    // by STEP rather than by position handles it, and the answer has to stay the held one's.
    let a = record(11, None);
    let b = reordered(&a);
    let (held, streamed) = both_ways(&a, &b);
    let streamed = streamed.expect("streaming refused a pair it can match by step");
    assert_eq!(
        held, streamed,
        "matching by step disagrees with matching by key"
    );
}

#[test]
fn a_streamed_comparison_refuses_what_it_cannot_pair() {
    // What it still must refuse: a file whose steps do not advance. The walk matches a step's
    // samples as a group and relies on both files being ordered; pairing across a file that
    // goes backwards would be guesswork, and a comparator that silently paired the wrong
    // samples would be worse than a slow one.
    let a = record(11, None);
    let b = backwards();
    assert!(
        ferroscope_schema::profile_streaming(
            || Ok(std::io::Cursor::new(a.clone())),
            || Ok(std::io::Cursor::new(b.clone())),
            ferroscope_receipt::Tolerance::default(),
        )
        .is_none(),
        "streaming paired a recording whose steps run backwards"
    );
}

/// A recording whose steps count DOWN — ordered, but not the way the walk assumes.
fn backwards() -> Vec<u8> {
    let mut rec = Recorder::new(Vec::new(), Precision::Quantized { drop_bits: 12 });
    for i in 0..120u64 {
        let step = 119 - i;
        let t = Stamp::at(step * 1_000_000, step * 1_010_000, step);
        rec.scalar("/err", t, step as f64, "m").unwrap();
        rec.transform(
            "/body",
            t,
            "world",
            "body",
            [step as f64, 0.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        )
        .unwrap();
    }
    let spec = ferroscope_receipt::RunSpec::new("spring", 11)
        .dt_ns(1_000_000)
        .steps(120)
        .integrator("semi-implicit-euler")
        .solver("none")
        .build("integration-test");
    rec.seal_with(spec, "test-platform", Vec::new).unwrap().0
}

/// The same spring run, cut short — a run that crashed half way, which is not an exotic case.
///
/// Byte for byte the same generator as [`record_with_production`], including the contacts it
/// only records past a threshold and the events it drops every fifty steps. A fixture that
/// merely *resembled* the main one would be a different pair of runs, not a shorter one: the
/// first draft left out contacts entirely and produced two runs with different channel sets,
/// which is the case the streaming walk is supposed to refuse.
fn record_steps(seed: u64, steps: u64) -> Vec<u8> {
    let mut rec = Recorder::new(Vec::new(), Precision::Quantized { drop_bits: 12 });
    let mut x = 0.2f64;
    let mut v = 0.0f64;
    for step in 0..steps {
        let t = Stamp::at(step * 1_000_000, step * 1_010_000, step);
        let f = -40.0 * x;
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
        .steps(steps)
        .integrator("semi-implicit-euler")
        .solver("none")
        .build("integration-test");
    rec.seal_with(spec, "test-platform", Vec::new).unwrap().0
}

/// The same run with its channels emitted in a different order — the case a lockstep walk must
/// refuse rather than pair up wrongly.
fn reordered(_original: &[u8]) -> Vec<u8> {
    let mut rec = Recorder::new(Vec::new(), Precision::Quantized { drop_bits: 12 });
    let mut x = 0.2f64;
    let mut v = 0.0f64;
    for step in 0..120u64 {
        let t = Stamp::at(step * 1_000_000, step * 1_010_000, step);
        let f = -40.0 * x;
        v += f * 1e-3;
        x += v * 1e-3;
        // `/err` first, `/body` last: the same numbers, a different file order.
        rec.scalar("/err", t, x, "m").unwrap();
        rec.energy("/energy/soc", t, Rail::Compute, "soc", 6.0)
            .unwrap();
        rec.transform(
            "/body",
            t,
            "world",
            "body",
            [x, 0.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        )
        .unwrap();
    }
    let spec = ferroscope_receipt::RunSpec::new("spring", 11)
        .dt_ns(1_000_000)
        .steps(120)
        .integrator("semi-implicit-euler")
        .solver("none")
        .build("integration-test");
    rec.seal_with(spec, "test-platform", Vec::new).unwrap().0
}
