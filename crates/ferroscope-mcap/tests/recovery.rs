//! Reading a recording that stops mid-record.
//!
//! A robot log is very often incomplete — the recorder was killed, the disk filled, the battery
//! went — and that file is frequently the most interesting one on the machine. Refusing all of it
//! because its last record is a stub throws away everything written correctly before the failure.

use ferroscope_mcap::{Flow, Record, Truncation, Writer, WriterOptions, stream, stream_recovering};

/// A recording with MANY chunks, so a cut lands inside one and leaves whole ones behind.
///
/// The chunk target matters to what recovery can do: a chunk is ONE record, so a file written as
/// a single chunk recovers nothing but its header. Small chunks here make the test about the
/// mechanism rather than about a lucky layout.
fn many_chunk_recording(messages: u32) -> Vec<u8> {
    let mut w = Writer::new(
        Vec::new(),
        WriterOptions::new("ferroscope", "recovery-test").chunk_target(4 << 10),
    );
    let s = w
        .add_schema("ferroscope.Scalar", "jsonschema", br#"{"type":"object"}"#)
        .unwrap();
    let c = w.add_channel("/sensor", s, "json", &[]).unwrap();
    for i in 0..messages {
        let t = 1_000_000 * u64::from(i);
        w.write_message(
            c,
            i,
            t,
            t,
            format!(r#"{{"step":{i},"value":{}}}"#, f64::from(i) * 0.25).as_bytes(),
        )
        .unwrap();
    }
    w.finish().unwrap()
}

fn count_messages(bytes: &[u8]) -> (u64, Option<Truncation>) {
    let mut n = 0u64;
    let (_, t) = stream_recovering(bytes, |r| {
        if matches!(r, Record::Message(_)) {
            n += 1;
        }
        Ok(Flow::Continue)
    })
    .expect("recovery should not error on a merely truncated file");
    (n, t)
}

#[test]
fn a_complete_recording_reports_no_truncation() {
    let bytes = many_chunk_recording(4_000);
    let (n, t) = count_messages(&bytes);
    assert_eq!(n, 4_000);
    assert_eq!(t, None, "a whole file must not be reported as truncated");
}

#[test]
fn a_truncated_recording_keeps_what_it_had() {
    let whole = many_chunk_recording(4_000);
    let cut = whole.len() * 60 / 100;
    let part = &whole[..cut];

    // The old behaviour, kept as the control: `stream` refuses the whole file.
    assert!(
        stream(part, |_| Ok(Flow::Continue)).is_err(),
        "the strict reader must still refuse a truncated file"
    );

    let (n, t) = count_messages(part);
    let t = t.expect("a file cut mid-record must report a truncation");
    assert!(
        n > 0,
        "recovery returned no messages from a file cut at 60%: it recovered nothing"
    );
    assert!(
        n < 4_000,
        "recovery returned every message from a file that is missing 40% of its bytes"
    );

    // The offset must be a position IN THE FILE. It named byte 0 of a 53 MB recording until the
    // day this test was written, because it reported an index into a buffer that is compacted.
    assert_eq!(
        t.offset as usize + 9 + t.have,
        cut,
        "offset + header + bytes present must account for exactly the file that is there"
    );
}

#[test]
fn recovery_is_not_repair() {
    // Corruption is not truncation. A file whose bytes are not what they claim must still be
    // refused: quietly handing back partial data for a damaged recording would be worse than
    // refusing it, because the caller cannot tell the two apart.
    let mut bytes = many_chunk_recording(2_000);
    let mid = bytes.len() / 2;
    bytes[mid] ^= 0xFF;
    let mut n = 0u64;
    let r = stream_recovering(&bytes[..], |rec| {
        if matches!(rec, Record::Message(_)) {
            n += 1;
        }
        Ok(Flow::Continue)
    });
    assert!(
        r.is_err(),
        "a corrupt chunk was silently recovered as if the file had merely stopped early"
    );
}

#[test]
fn a_torn_tail_is_not_the_same_as_a_truncation() {
    // The boundary, and it is a deliberate one. Fewer than nine dangling bytes is not yet a
    // record header, which is exactly what the last block of a file still BEING WRITTEN looks
    // like -- so it is not reported as truncation, and `read_prefix` exists for that case. Nine
    // or more is a header whose body never arrived: a file that stopped.
    let whole = many_chunk_recording(100);

    // Magic is 8 bytes; +4 leaves four dangling, which could still be a growing file.
    let (n, t) = count_messages(&whole[..12]);
    assert_eq!(n, 0);
    assert_eq!(
        t, None,
        "a tail too short to be a record header must read as a growing file, not a truncation"
    );

    // Nine dangling bytes IS a header, and its body is missing.
    let (_, t) = count_messages(&whole[..8 + 9 + 4]);
    let t = t.expect("a whole record header with no body is a truncation");
    assert_eq!(t.offset, 8, "the first record starts right after the magic");
}
