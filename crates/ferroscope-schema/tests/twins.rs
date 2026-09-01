//! Every reader has a twin, and the twins must agree.
//!
//! This crate answers four questions twice: once over a slice, for a browser holding an
//! `ArrayBuffer`, and once over a stream, for a file larger than memory. That is a deliberate
//! design — the two shapes have genuinely different costs — and it is also the single most
//! productive source of defects in this project:
//!
//! - CDR decoding went into the streaming bundle reader and not the slice one, so the CLI
//!   plotted a `ros2 bag` and the browser showed the same file with no lanes.
//! - It then reached four readers of ten, so `diff` compared nothing across two recordings that
//!   differ and reported "bit-exact: every float identical".
//!
//! Each of those was found by hand, after shipping. The guard that generalises is not "reader X
//! handles feature Y" — that is satisfiable by fixing one surface — but that **the twins agree**,
//! which is not. One test per pair, over a fixture that exercises both payload encodings.

use ferroscope_mcap::{Writer, WriterOptions};
use ferroscope_receipt::Precision;
use ferroscope_schema::{JointState, Recorder, Stamp};

/// A sealed Ferroscope recording: JSON payloads, a receipt, several schemas.
fn sealed() -> Vec<u8> {
    let mut rec = Recorder::new(Vec::new(), Precision::Quantized { drop_bits: 12 });
    let (mut x, mut v) = (0.2f64, 0.0f64);
    for step in 0..120u64 {
        let t = Stamp::at(step * 1_000_000, step * 1_010_000, step);
        let f = -40.0 * x;
        v += f * 1e-3;
        x += v * 1e-3;
        rec.scalar("/err", t, x, "m").unwrap();
        rec.joints(
            "/joints",
            t,
            &JointState {
                names: vec!["hip".into(), "knee".into()],
                position: vec![x, v],
                velocity: vec![v, 0.0],
                effort: vec![f, 0.0],
            },
        )
        .unwrap();
    }
    let spec = ferroscope_receipt::RunSpec::new("twins", 1)
        .dt_ns(1_000_000)
        .steps(120)
        .integrator("semi-implicit-euler")
        .solver("none")
        .build("twins-test");
    rec.seal(spec, "test-platform").unwrap().0
}

/// A ROS 2 recording: CDR payloads, a `ros2msg` schema, no receipt.
fn ros2() -> Vec<u8> {
    const DEF: &str = "int32 sec\nuint32 nanosec\n";
    let mut w = Writer::new(Vec::new(), WriterOptions::new("ros2", "twins-test"));
    let s = w
        .add_schema("builtin_interfaces/Time", "ros2msg", DEF.as_bytes())
        .unwrap();
    let c = w.add_channel("/clock", s, "cdr", &[]).unwrap();
    for i in 0..120u32 {
        let t = u64::from(i) * 1_000_000;
        let mut p = vec![0x00, 0x01, 0x00, 0x00];
        p.extend_from_slice(&(i as i32).to_le_bytes());
        p.extend_from_slice(&(i * 7).to_le_bytes());
        w.write_message(c, i, t, t, &p).unwrap();
    }
    w.finish().unwrap()
}

fn both() -> Vec<(&'static str, Vec<u8>)> {
    vec![("sealed ferroscope", sealed()), ("ros2 cdr", ros2())]
}

#[test]
fn bundle_twins_agree() {
    for (what, bytes) in both() {
        let slice = ferroscope_schema::bundle(&bytes).expect(what);
        let streamed =
            ferroscope_schema::bundle_streaming(|| Ok(std::io::Cursor::new(bytes.clone())))
                .expect(what);
        assert_eq!(slice, streamed, "bundle readers disagree on {what}");
    }
}

#[test]
fn trace_twins_agree() {
    for (what, bytes) in both() {
        let (ra, slice) = ferroscope_schema::trace_from(&bytes).expect(what);
        let (rb, streamed) =
            ferroscope_schema::trace_from_streaming(std::io::Cursor::new(&bytes)).expect(what);
        assert_eq!(
            slice.samples, streamed.samples,
            "trace readers disagree on {what}"
        );
        assert_eq!(
            ra.map(|r| r.trace_digest),
            rb.map(|r| r.trace_digest),
            "trace readers disagree about the receipt on {what}"
        );
        assert!(
            !slice.samples.is_empty(),
            "{what} yielded no samples at all"
        );
    }
}

#[test]
fn verify_twins_agree() {
    for (what, bytes) in both() {
        let slice = ferroscope_schema::verify(&bytes);
        let streamed =
            ferroscope_schema::verify_streaming(|| Ok(std::io::Cursor::new(bytes.clone())));
        match (slice, streamed) {
            (Some(a), Some(b)) => {
                assert_eq!(a.recomputed, b.recomputed, "digests disagree on {what}");
                assert_eq!(
                    a.trace_matches, b.trace_matches,
                    "verdicts disagree on {what}"
                );
                assert_eq!(a.messages, b.messages, "message counts disagree on {what}");
            }
            (None, None) => {} // no receipt: both must say so
            (a, b) => panic!(
                "one verify reader found a receipt in {what} and the other did not: {} vs {}",
                a.is_some(),
                b.is_some()
            ),
        }
    }
}

#[test]
fn label_twins_agree() {
    // The pair with no coverage at all until this file, and both halves were edited on the same
    // day the other twins were found to have diverged.
    for (what, bytes) in both() {
        let slice = ferroscope_schema::channel_labels(&bytes);
        let streamed = ferroscope_schema::channel_labels_streaming(std::io::Cursor::new(&bytes));
        assert_eq!(slice, streamed, "label readers disagree on {what}");
        assert!(!slice.is_empty(), "{what} produced no component labels");
    }
}
