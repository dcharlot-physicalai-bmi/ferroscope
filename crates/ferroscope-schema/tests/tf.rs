//! `tf2_msgs/TFMessage` into the 3-D view.
//!
//! ROS 2 publishes WHERE things are on `/tf` and leaves what they look like to a separate robot
//! description, so a bag routinely carries a full transform tree and no geometry at all. Landing
//! those transforms in the bundle's `frames` is what lets the viewer draw the motion, and what
//! lets geometry declared against those frames follow a real robot.
//!
//! The payload here is encoded by hand rather than by a library, because the thing under test is
//! whether this crate reads what ROS 2 writes — and a fixture produced by our own encoder would
//! only prove the decoder agrees with it. The CDR alignment rule (each primitive to its own
//! width, measured from the end of the 4-byte encapsulation header) is applied explicitly below,
//! so a mistake in it shows up as a decode error rather than as plausible numbers.

use ferroscope_mcap::{Writer, WriterOptions};

const DEF: &str = "\
geometry_msgs/TransformStamped[] transforms

================================================================================
MSG: geometry_msgs/TransformStamped
std_msgs/Header header
string child_frame_id
geometry_msgs/Transform transform

================================================================================
MSG: std_msgs/Header
builtin_interfaces/Time stamp
string frame_id

================================================================================
MSG: builtin_interfaces/Time
int32 sec
uint32 nanosec

================================================================================
MSG: geometry_msgs/Transform
geometry_msgs/Vector3 translation
geometry_msgs/Quaternion rotation

================================================================================
MSG: geometry_msgs/Vector3
float64 x
float64 y
float64 z

================================================================================
MSG: geometry_msgs/Quaternion
float64 x
float64 y
float64 z
float64 w
";

/// A little-endian CDR writer that applies the alignment rule explicitly.
struct Cdr(Vec<u8>);

impl Cdr {
    fn new() -> Self {
        Self(vec![0x00, 0x01, 0x00, 0x00])
    }
    /// Pad to `n`, measured from the end of the encapsulation header.
    fn align(&mut self, n: usize) {
        while !(self.0.len() - 4).is_multiple_of(n) {
            self.0.push(0);
        }
    }
    fn u32(&mut self, v: u32) {
        self.align(4);
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn i32(&mut self, v: i32) {
        self.align(4);
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn f64(&mut self, v: f64) {
        self.align(8);
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    /// A `u32` length that INCLUDES the trailing NUL, then the bytes.
    fn str(&mut self, s: &str) {
        self.u32(s.len() as u32 + 1);
        self.0.extend_from_slice(s.as_bytes());
        self.0.push(0);
    }
}

/// One `/tf` message carrying `(child, parent, x, y, z)` transforms with identity rotation.
fn tf_payload(step: u32, tfs: &[(&str, &str, f64, f64, f64)]) -> Vec<u8> {
    let mut c = Cdr::new();
    c.u32(tfs.len() as u32);
    for (child, parent, x, y, z) in tfs {
        c.i32(step as i32); // header.stamp.sec
        c.u32(step * 1000); // header.stamp.nanosec
        c.str(parent); //     header.frame_id
        c.str(child); //      child_frame_id
        c.f64(*x);
        c.f64(*y);
        c.f64(*z);
        c.f64(0.0);
        c.f64(0.0);
        c.f64(0.0);
        c.f64(1.0); // rotation w
    }
    c.0
}

fn recording() -> Vec<u8> {
    let mut w = Writer::new(Vec::new(), WriterOptions::new("ros2", "tf-test"));
    let s = w
        .add_schema("tf2_msgs/msg/TFMessage", "ros2msg", DEF.as_bytes())
        .unwrap();
    let c = w.add_channel("/tf", s, "cdr", &[]).unwrap();
    for i in 0..50u32 {
        let t = u64::from(i) * 1_000_000;
        let x = f64::from(i) * 0.1;
        let p = tf_payload(
            i,
            &[
                ("base_link", "odom", x, 0.0, 0.0),
                ("lidar", "base_link", 0.0, 0.0, 0.4),
            ],
        );
        w.write_message(c, i, t, t, &p).unwrap();
    }
    w.finish().unwrap()
}

#[test]
fn a_transform_tree_becomes_frames_the_viewer_can_place() {
    let bytes = recording();
    let b = ferroscope_schema::bundle(&bytes).expect("bundle");
    // Frames are keyed by the CHILD frame, which is the one that moved. That name is a string in
    // the payload, and the decoder used to discard strings entirely.
    assert!(
        b.contains("\"base_link\""),
        "no base_link frame: {}",
        &b[..b.len().min(300)]
    );
    assert!(b.contains("\"lidar\""), "no lidar frame");
    // And the pose readout, per topic and child.
    assert!(b.contains("/tf:base_link"), "no per-topic pose lane");
}

#[test]
fn the_transform_values_are_the_ones_that_were_written() {
    let bytes = recording();
    let (_, trace) = ferroscope_schema::trace_from(&bytes).expect("trace");
    assert!(
        !trace.samples.is_empty(),
        "a TF recording produced no samples"
    );
    // 50 messages, each with two transforms of 11 numbers (2 stamp + 3 translation + 4 rotation
    // = 9 numbers per transform, plus the two stamp fields already counted).
    let s = &trace.samples[0];
    assert_eq!(s.channel, "/tf");
    let nums = &s.values;
    assert!(
        nums.len() >= 18,
        "expected both transforms' numbers, got {}",
        nums.len()
    );
    // The lidar's static offset is the only non-zero translation in the second transform.
    assert!(
        nums.iter().any(|v| (*v - 0.4).abs() < 1e-12),
        "the lidar's 0.4 m offset is not in the decoded numbers"
    );
}

#[test]
fn both_bundle_readers_agree_on_a_transform_tree() {
    let bytes = recording();
    let slice = ferroscope_schema::bundle(&bytes).expect("slice");
    let streamed = ferroscope_schema::bundle_streaming(|| Ok(std::io::Cursor::new(bytes.clone())))
        .expect("stream");
    assert_eq!(slice, streamed, "the two bundle readers disagree about /tf");
}
