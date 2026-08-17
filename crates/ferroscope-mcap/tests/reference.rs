//! **The oracle test.** A format implementation that only round-trips against itself proves
//! nothing: two matching bugs read as a pass. So every file this crate writes is handed to
//! Foxglove's reference `mcap` crate — a completely independent parser — and every file the
//! reference crate writes is read back by this one.
//!
//! `mcap` is a dev-dependency only. It never ships in anything that depends on Ferroscope.

use std::collections::BTreeMap;

use ferroscope_mcap::{read, Writer, WriterOptions};

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
