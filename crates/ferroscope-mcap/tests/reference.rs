//! **The oracle test.** A format implementation that only round-trips against itself proves
//! nothing: two matching bugs read as a pass. So every file this crate writes is handed to
//! Foxglove's reference `mcap` crate — a completely independent parser — and every file the
//! reference crate writes is read back by this one.
//!
//! `mcap` is a dev-dependency only. It never ships in anything that depends on Ferroscope.

use std::collections::BTreeMap;

use ferroscope_mcap::{Writer, WriterOptions, read};

fn sample() -> Vec<u8> {
    let mut w = Writer::new(
        Vec::new(),
        // A small chunk target so the fixture exercises multi-chunk files and the chunk
        // index, not just the single-chunk happy path.
        WriterOptions::new("ferroscope", "ferroscope-mcap/test").chunk_target(512),
    );
    let s_pose = w
        .add_schema(
            "ferroscope.Pose",
            "jsonschema",
            br#"{"type":"object","properties":{"x":{"type":"number"}}}"#,
        )
        .unwrap();
    let s_joule = w
        .add_schema(
            "ferroscope.EnergySample",
            "jsonschema",
            br#"{"type":"object","properties":{"w":{"type":"number"}}}"#,
        )
        .unwrap();
    let c_pose = w
        .add_channel(
            "/robot/pose",
            s_pose,
            "json",
            &[("frame".into(), "world".into())],
        )
        .unwrap();
    let c_joule = w
        .add_channel("/energy/actuation", s_joule, "json", &[])
        .unwrap();

    for i in 0..200u32 {
        let t = 1_000_000_000 + i as u64 * 10_000_000;
        let pose = format!(r#"{{"x":{:.3}}}"#, i as f64 * 0.01);
        w.write_message(c_pose, i, t, t, pose.as_bytes()).unwrap();
        if i % 4 == 0 {
            let j = format!(r#"{{"w":{:.2}}}"#, 12.0 + (i % 7) as f64);
            w.write_message(c_joule, i / 4, t, t, j.as_bytes()).unwrap();
        }
    }
    w.write_attachment(
        "robot.glb",
        "model/gltf-binary",
        b"glTF\x02\x00\x00\x00 not really a mesh, but the bytes round-trip",
        1_500_000_000,
        1_400_000_000,
    )
    .unwrap();
    w.write_metadata(
        "ferroscope.receipt",
        &[
            ("seed".into(), "42".into()),
            ("integrator".into(), "rk4".into()),
        ],
    )
    .unwrap();
    w.finish().unwrap()
}

#[test]
fn reference_reader_accepts_our_files() {
    let bytes = sample();

    // `mcap::MessageStream` validates magic, record framing, chunk CRCs and the summary as
    // it goes. If any field of our writer is wrong, this is where it surfaces.
    let mut count = 0usize;
    let mut per_topic: BTreeMap<String, usize> = BTreeMap::new();
    for msg in mcap::MessageStream::new(&bytes).expect("reference reader rejected the file") {
        let msg = msg.expect("reference reader failed mid-stream");
        *per_topic.entry(msg.channel.topic.clone()).or_default() += 1;
        count += 1;
    }
    assert_eq!(count, 250, "message count");
    assert_eq!(per_topic["/robot/pose"], 200);
    assert_eq!(per_topic["/energy/actuation"], 50);
}

#[test]
fn reference_reads_our_summary_and_metadata() {
    let bytes = sample();
    let summary = mcap::Summary::read(&bytes)
        .expect("summary parse")
        .expect("file has a summary section");

    let stats = summary.stats.expect("statistics record");
    assert_eq!(stats.message_count, 250);
    assert_eq!(stats.channel_count, 2);
    assert!(stats.chunk_count >= 2, "expected multiple chunks");
    assert_eq!(stats.metadata_count, 1);

    let topics: Vec<_> = summary.channels.values().map(|c| c.topic.clone()).collect();
    assert!(topics.contains(&"/robot/pose".to_string()));

    // Schemas survive with their encoding, which is what lets a viewer that has never heard
    // of Ferroscope still render the payload.
    let pose = summary
        .schemas
        .values()
        .find(|s| s.name == "ferroscope.Pose")
        .expect("pose schema");
    assert_eq!(pose.encoding, "jsonschema");
}

#[test]
fn we_read_reference_files() {
    // Now the other direction: the reference crate writes, we read. Its default writer
    // uses zstd chunks, so this also pins our refusal to guess at a codec we do not link.
    let mut out = std::io::Cursor::new(Vec::new());
    {
        let mut w = mcap::Writer::new(&mut out).unwrap();
        let schema = w
            .add_schema("ref.Thing", "jsonschema", br#"{"type":"object"}"#)
            .unwrap();
        let id = w
            .add_channel(schema, "/ref", "json", &Default::default())
            .unwrap();
        for i in 0..10u32 {
            w.write_to_known_channel(
                &mcap::records::MessageHeader {
                    channel_id: id,
                    sequence: i,
                    log_time: 1_000 + i as u64,
                    publish_time: 1_000 + i as u64,
                },
                br#"{"n":1}"#,
            )
            .unwrap();
        }
        w.finish().unwrap();
    }
    let bytes = out.into_inner();

    match read(&bytes) {
        Ok(log) => {
            // The reference crate chose an uncompressed or unchunked layout we can parse.
            assert_eq!(log.messages.len(), 10);
            assert_eq!(log.channels[0].topic, "/ref");
        }
        Err(ferroscope_mcap::Error::UnsupportedCompression(codec)) => {
            // The expected outcome for a zstd-chunked file, and the error we promise: it
            // names the codec instead of returning a short read.
            assert!(
                codec == "zstd" || codec == "lz4",
                "unexpected codec {codec}"
            );
        }
        Err(e) => panic!("unexpected error reading a reference file: {e}"),
    }
}

#[test]
fn round_trip_through_our_own_reader() {
    let bytes = sample();
    let log = read(&bytes).unwrap();
    assert_eq!(log.profile, "ferroscope");
    assert_eq!(log.messages.len(), 250);
    assert_eq!(log.messages_on("/energy/actuation").count(), 50);
    assert_eq!(log.metadata_block("ferroscope.receipt").unwrap()[0].1, "42");
    let stats = log.statistics.as_ref().expect("statistics");
    assert_eq!(stats.message_count, 250);
    let (t0, t1) = log.time_span().unwrap();
    assert_eq!(t0, 1_000_000_000);
    assert_eq!(t1, 1_000_000_000 + 199 * 10_000_000);
}

#[test]
fn truncation_is_named_not_guessed() {
    let bytes = sample();
    let short = &bytes[..bytes.len() / 2];
    let err = read(short).unwrap_err();
    // A half file must not read as a half run.
    assert!(
        matches!(err, ferroscope_mcap::Error::BadMagic { at: "end" }),
        "got {err}"
    );
}

#[test]
fn a_flipped_byte_inside_a_chunk_is_caught() {
    let mut bytes = sample();
    // Locate a real message payload rather than trusting an offset — record sizes move as
    // soon as anything about the fixture changes, and a test that silently starts corrupting
    // a header byte would still "pass" for the wrong reason.
    let needle = br#"{"x":1.230}"#;
    let at = bytes
        .windows(needle.len())
        .position(|w| w == needle)
        .expect("fixture payload not found");
    bytes[at + 6] ^= 0xFF;
    match read(&bytes) {
        Err(ferroscope_mcap::Error::ChunkCrcMismatch { .. }) => {}
        Err(e) => panic!("expected a CRC mismatch, got {e}"),
        Ok(_) => panic!("corruption inside a chunk went unreported"),
    }
}

#[test]
fn attachments_round_trip_and_the_reference_reader_sees_them() {
    let bytes = sample();

    // Ours.
    let log = read(&bytes).unwrap();
    assert_eq!(log.attachments.len(), 1);
    let a = log.attachment("robot.glb").expect("by name");
    assert_eq!(a.media_type, "model/gltf-binary");
    assert_eq!(a.log_time, 1_500_000_000);
    assert_eq!(a.create_time, 1_400_000_000);
    assert!(a.data.starts_with(b"glTF"));
    assert_eq!(log.statistics.as_ref().unwrap().attachment_count, 1);

    // Foxglove's, which validates the attachment CRC as it reads.
    let summary = mcap::Summary::read(&bytes).unwrap().unwrap();
    assert_eq!(
        summary.attachment_indexes.len(),
        1,
        "indexed in the summary"
    );
    let idx = &summary.attachment_indexes[0];
    assert_eq!(idx.name, "robot.glb");
    assert_eq!(idx.media_type, "model/gltf-binary");
    let got = mcap::read::attachment(&bytes, idx)
        .expect("reference reader could not read the attachment");
    assert_eq!(got.name, "robot.glb");
    assert!(got.data.starts_with(b"glTF"));
}

#[test]
fn a_corrupted_attachment_is_caught_by_its_own_crc() {
    let mut bytes = sample();
    let needle = b"not really a mesh";
    let at = bytes
        .windows(needle.len())
        .position(|w| w == needle)
        .expect("attachment payload not found");
    bytes[at + 3] ^= 0xFF;
    match read(&bytes) {
        Err(ferroscope_mcap::Error::AttachmentCrcMismatch { name, .. }) => {
            assert_eq!(name, "robot.glb")
        }
        Err(e) => panic!("expected an attachment CRC mismatch, got {e}"),
        Ok(_) => panic!("a corrupted attachment went unreported"),
    }
}

#[test]
fn record_spans_tile_the_file_exactly() {
    let bytes = sample();
    let spans = ferroscope_mcap::record_spans(&bytes).expect("spans");
    assert!(!spans.is_empty());
    // magic + spans + magic reassembles the file byte for byte — the contract replay stands on.
    let mut rebuilt = bytes[..8].to_vec();
    let mut expect = 8;
    for s in &spans {
        assert_eq!(s.start, expect, "a gap or overlap between records");
        rebuilt.extend_from_slice(&bytes[s.start..s.end]);
        expect = s.end;
    }
    rebuilt.extend_from_slice(&bytes[bytes.len() - 8..]);
    assert_eq!(rebuilt, bytes);
}

#[test]
fn record_spans_carry_the_times_replay_paces_by() {
    let bytes = sample();
    let spans = ferroscope_mcap::record_spans(&bytes).expect("spans");
    // The sample writer chunks, so the timed records are chunks carrying message_start_time.
    let timed: Vec<u64> = spans.iter().filter_map(|s| s.log_time).collect();
    assert!(!timed.is_empty(), "no record carried a time");
    assert!(
        timed.windows(2).all(|w| w[0] <= w[1]),
        "record times went backwards: {timed:?}"
    );
    let log = read(&bytes).expect("read");
    let (t0, _) = log.time_span().expect("span");
    assert_eq!(timed[0], t0, "the first timed record is the first instant");
}

#[test]
fn record_spans_refuse_a_torn_file() {
    let bytes = sample();
    // Both of these are refused by the closing-magic check, which is the cheap half.
    assert!(ferroscope_mcap::record_spans(&bytes[..bytes.len() - 3]).is_err());
    assert!(ferroscope_mcap::record_spans(&bytes[..40]).is_err());

    // The half that matters, and that the two cases above never reached: magic at BOTH ends
    // with a record inside whose declared length runs past the data section. Without the
    // Truncated guard this is a slice-index panic, so a test that never enters the parse loop
    // is asserting nothing about the only code standing between a hostile file and a crash.
    let mut torn = ferroscope_mcap::MAGIC.to_vec();
    torn.push(ferroscope_mcap::op::MESSAGE);
    torn.extend_from_slice(&(4096u64).to_le_bytes());
    torn.extend_from_slice(b"only a few bytes, not 4096");
    torn.extend_from_slice(&ferroscope_mcap::MAGIC);
    match ferroscope_mcap::record_spans(&torn) {
        Err(ferroscope_mcap::Error::Truncated { want, have, .. }) => {
            assert_eq!(want, 4096);
            assert!(have < want);
        }
        other => panic!("a record running past the end was not refused: {other:?}"),
    }

    // A header that cannot even hold a length, inside otherwise valid magic.
    let mut stub = ferroscope_mcap::MAGIC.to_vec();
    stub.extend_from_slice(&[0x05, 0x01, 0x02]);
    stub.extend_from_slice(&ferroscope_mcap::MAGIC);
    assert!(matches!(
        ferroscope_mcap::record_spans(&stub),
        Err(ferroscope_mcap::Error::Truncated { want: 9, .. })
    ));
}

#[test]
fn record_spans_refuse_a_length_no_machine_could_hold() {
    // u64::MAX as usize silently becomes usize::MAX on 64-bit and 0xFFFF_FFFF on 32-bit; a
    // length of exactly 2^32 becomes ZERO there, which is the dangerous one — the guard passes
    // and the parser re-reads the payload as records. Neither may be accepted anywhere.
    for declared in [u64::MAX, 1u64 << 32, (1u64 << 32) + 9] {
        let mut hostile = ferroscope_mcap::MAGIC.to_vec();
        hostile.push(ferroscope_mcap::op::MESSAGE);
        hostile.extend_from_slice(&declared.to_le_bytes());
        hostile.extend_from_slice(&[0u8; 64]);
        hostile.extend_from_slice(&ferroscope_mcap::MAGIC);
        assert!(
            ferroscope_mcap::record_spans(&hostile).is_err(),
            "a record declaring {declared} bytes was accepted"
        );
    }
}

#[test]
fn the_streaming_reader_sees_what_the_slice_reader_sees() {
    // Two readers are two definitions of the format unless something holds them equal. This is
    // that something: the same file, walked both ways, must yield the same messages in the same
    // order with the same bytes.
    let bytes = sample();
    let whole = read(&bytes).expect("slice read");

    let mut streamed: Vec<(u16, u64, u64, Vec<u8>)> = Vec::new();
    let mut schemas = 0usize;
    let mut channels = 0usize;
    let mut attachments = 0usize;
    let n = ferroscope_mcap::stream(std::io::Cursor::new(&bytes), |rec| {
        match rec {
            ferroscope_mcap::Record::Message(m) => {
                streamed.push((m.channel_id, m.log_time, m.publish_time, m.data.to_vec()))
            }
            ferroscope_mcap::Record::Schema(_) => schemas += 1,
            ferroscope_mcap::Record::Channel(_) => channels += 1,
            ferroscope_mcap::Record::Attachment(_) => attachments += 1,
            _ => {}
        }
        Ok(ferroscope_mcap::Flow::Continue)
    })
    .expect("stream read");

    assert!(n > 0, "no records streamed");
    assert_eq!(
        streamed.len(),
        whole.messages.len(),
        "message count differs between readers"
    );
    for ((cid, lt, pt, data), b) in streamed.iter().zip(&whole.messages) {
        assert_eq!(*cid, b.channel_id);
        assert_eq!(*lt, b.log_time);
        assert_eq!(*pt, b.publish_time);
        assert_eq!(data, &b.data, "payload differs at log_time {lt}");
    }
    assert!(schemas >= whole.schemas.len(), "schemas missed");
    assert!(channels >= whole.channels.len(), "channels missed");
    assert_eq!(attachments, whole.attachments.len(), "attachments missed");
}

#[test]
fn a_streaming_visitor_can_stop_early() {
    let bytes = sample();
    let mut seen = 0usize;
    ferroscope_mcap::stream(std::io::Cursor::new(&bytes), |rec| {
        if let ferroscope_mcap::Record::Message(_) = rec {
            seen += 1;
            if seen == 2 {
                return Ok(ferroscope_mcap::Flow::Stop);
            }
        }
        Ok(ferroscope_mcap::Flow::Continue)
    })
    .expect("stream read");
    assert_eq!(seen, 2, "Stop did not stop the walk");
}

#[test]
fn the_streaming_reader_refuses_a_file_that_is_not_one() {
    assert!(
        ferroscope_mcap::stream(std::io::Cursor::new(b"not an mcap file at all"), |_| Ok(
            ferroscope_mcap::Flow::Continue
        ))
        .is_err()
    );
}
