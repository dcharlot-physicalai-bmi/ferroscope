//! A ROS 2 recording, through both bundle readers.
//!
//! Ferroscope has two implementations of "flatten a recording into lanes": one over a slice, for
//! a browser holding an `ArrayBuffer`, and one over a stream, for a file larger than memory. When
//! CDR decoding was added to only the streaming one, the CLI plotted a `ros2 bag` and the browser
//! showed the same file as a topic list with nothing in it — a defect on one surface and not the
//! other, which is the shape this project has shipped before.
//!
//! So the test is not "the streaming path decodes CDR". It is that the two paths AGREE.

use ferroscope_mcap::{Writer, WriterOptions};

/// `builtin_interfaces/Time`: two 4-byte fields, so a CDR payload is easy to write by hand and
/// impossible to get subtly wrong.
const DEF: &str = "int32 sec\nuint32 nanosec\n";

fn cdr_time(sec: i32, nanosec: u32) -> Vec<u8> {
    let mut v = vec![0x00, 0x01, 0x00, 0x00]; // plain CDR, little-endian
    v.extend_from_slice(&sec.to_le_bytes());
    v.extend_from_slice(&nanosec.to_le_bytes());
    v
}

fn recording() -> Vec<u8> {
    let mut w = Writer::new(Vec::new(), WriterOptions::new("ros2", "ros2-test"));
    let s = w
        .add_schema("builtin_interfaces/Time", "ros2msg", DEF.as_bytes())
        .unwrap();
    let c = w.add_channel("/clock", s, "cdr", &[]).unwrap();
    for i in 0..200u32 {
        let t = u64::from(i) * 1_000_000;
        w.write_message(c, i, t, t, &cdr_time(i as i32, i * 7))
            .unwrap();
    }
    w.finish().unwrap()
}

#[test]
fn both_bundle_readers_decode_ros2_identically() {
    let bytes = recording();
    let slice = ferroscope_schema::bundle(&bytes).expect("slice bundle");
    let streamed = ferroscope_schema::bundle_streaming(|| Ok(std::io::Cursor::new(bytes.clone())))
        .expect("streaming bundle");
    assert_eq!(
        slice, streamed,
        "the slice and streaming readers disagree about a ROS 2 recording"
    );
}

#[test]
fn a_cdr_channel_becomes_named_lanes() {
    let bytes = recording();
    let b = ferroscope_schema::bundle(&bytes).expect("bundle");
    // Named from the definition the file carries, not by index.
    assert!(
        b.contains("/clock:sec"),
        "no `sec` lane in the bundle: {}",
        &b[..b.len().min(400)]
    );
    assert!(b.contains("/clock:nanosec"), "no `nanosec` lane");
    // And the values must be the ones written: message 3 has nanosec = 21.
    let v: serde_lite::Value = serde_lite::parse(&b).expect("bundle is JSON");
    let lane = v.lane("/clock:nanosec").expect("nanosec lane");
    assert_eq!(lane[3][1], 21.0, "decoded the wrong value");
}

/// A payload the definition does not fit must be SKIPPED, never zero-filled: a fabricated sample
/// would enter the digest and the plot as though it were data.
#[test]
fn an_undecodable_payload_is_dropped_not_invented() {
    let mut w = Writer::new(Vec::new(), WriterOptions::new("ros2", "ros2-test"));
    let s = w
        .add_schema("builtin_interfaces/Time", "ros2msg", DEF.as_bytes())
        .unwrap();
    let c = w.add_channel("/clock", s, "cdr", &[]).unwrap();
    w.write_message(c, 0, 0, 0, &cdr_time(1, 2)).unwrap();
    // Four bytes of encapsulation and nothing else: the definition wants eight more.
    w.write_message(c, 1, 1, 1, &[0x00, 0x01, 0x00, 0x00])
        .unwrap();
    let bytes = w.finish().unwrap();

    let b = ferroscope_schema::bundle(&bytes).expect("bundle");
    let v: serde_lite::Value = serde_lite::parse(&b).expect("bundle is JSON");
    let lane = v.lane("/clock:sec").expect("sec lane");
    assert_eq!(lane.len(), 1, "the undecodable message became a data point");
}

/// A very small JSON reader, so this test does not need a dependency to read the bundle it just
/// produced. Only what the assertions above use.
mod serde_lite {
    pub struct Value(String);
    pub fn parse(s: &str) -> Option<Value> {
        Some(Value(s.to_string()))
    }
    impl Value {
        /// The points of one lane, as `[[x, y], ...]`.
        pub fn lane(&self, key: &str) -> Option<Vec<Vec<f64>>> {
            let needle = format!("\"{key}\":");
            let at = self.0.find(&needle)? + needle.len();
            let rest = &self.0[at..];
            let end = rest.find("]]")? + 2;
            let body = &rest[..end];
            let mut out = Vec::new();
            for pair in body
                .trim_start_matches('[')
                .trim_end_matches(']')
                .split("],[")
            {
                let nums: Vec<f64> = pair
                    .trim_matches(|c| c == '[' || c == ']')
                    .split(',')
                    .filter_map(|n| n.trim().parse().ok())
                    .collect();
                if !nums.is_empty() {
                    out.push(nums);
                }
            }
            Some(out)
        }
    }
}
