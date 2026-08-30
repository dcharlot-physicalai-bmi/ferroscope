//! A real `sensor_msgs/JointState`, recorded by `mcap-ros2-support` 0.5.7.
//!
//! Definition text and payload bytes are verbatim from a file that tooling wrote, not from this
//! crate's idea of the format. The expected numbers come from the generator that produced it.

use ferroscope_ros2::{Error, MessageDef};

const DEF: &str = "\
std_msgs/Header header
string[] name
float64[] position
float64[] velocity
float64[] effort

================================================================================
MSG: std_msgs/Header
builtin_interfaces/Time stamp
string frame_id

================================================================================
MSG: builtin_interfaces/Time
int32 sec
uint32 nanosec
";

/// The first message: stamp 0, frame_id "base", three joints, position [0, 0.8, 0],
/// velocity [0, 0, 0], effort [1.5, -0.25, 0].
const PAYLOAD: &[u8] = &[
    0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x05, 0x00, 0x00, 0x00,
    0x62, 0x61, 0x73, 0x65, 0x00, 0x00, 0x00, 0x00, 0x03, 0x00, 0x00, 0x00, 0x09, 0x00, 0x00, 0x00,
    0x73, 0x68, 0x6f, 0x75, 0x6c, 0x64, 0x65, 0x72, 0x00, 0x00, 0x00, 0x00, 0x06, 0x00, 0x00, 0x00,
    0x65, 0x6c, 0x62, 0x6f, 0x77, 0x00, 0x00, 0x00, 0x06, 0x00, 0x00, 0x00, 0x77, 0x72, 0x69, 0x73,
    0x74, 0x00, 0x00, 0x00, 0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x9a, 0x99, 0x99, 0x99, 0x99, 0x99, 0xe9, 0x3f, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0xf8, 0x3f, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xd0, 0xbf, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00,
];

#[test]
fn a_real_jointstate_decodes_to_the_numbers_that_were_recorded() {
    let def = MessageDef::parse("sensor_msgs/msg/JointState", DEF).expect("definition");
    let (values, labels) = def.decode_labeled(PAYLOAD).expect("decode");

    // sec, nanosec, then 3 position, 3 velocity, 3 effort. The three joint NAMES are strings:
    // consumed, and contributing no numbers.
    assert_eq!(
        values,
        vec![0.0, 0.0, 0.0, 0.8, 0.0, 0.0, 0.0, 0.0, 1.5, -0.25, 0.0]
    );
    assert_eq!(labels[0], "header.stamp.sec");
    assert_eq!(labels[1], "header.stamp.nanosec");
    assert_eq!(labels[3], "position[1]");
    assert_eq!(labels[8], "effort[0]");
}

#[test]
fn the_wrong_alignment_origin_is_caught_rather_than_returned() {
    // Measured before this crate existed: aligning from the start of the buffer instead of from
    // the end of the 4-byte encapsulation header decodes `position` as [0.0, -0.0, 0.0] and
    // leaves 36 of 164 bytes unread. Plausible numbers, silently wrong. `finish()` is the check
    // that makes that an error, so a truncated or mis-parsed message cannot pass as data --
    // here, proven by feeding a payload one byte short of what the definition wants.
    let def = MessageDef::parse("sensor_msgs/msg/JointState", DEF).unwrap();
    let short = &PAYLOAD[..PAYLOAD.len() - 8];
    assert!(matches!(
        def.decode_labeled(short),
        Err(Error::Short { .. })
    ));

    // And bytes the definition never claims are an error too, not a silent success.
    let mut long = PAYLOAD.to_vec();
    long.extend_from_slice(&[0u8; 16]);
    assert!(matches!(
        def.decode_numbers(&long),
        Err(Error::Trailing { .. })
    ));
}

#[test]
fn a_parameter_list_encapsulation_is_refused_not_guessed() {
    let def = MessageDef::parse("builtin_interfaces/Time", "int32 sec\nuint32 nanosec\n").unwrap();
    let mut p = vec![0x00, 0x03, 0x00, 0x00];
    p.extend_from_slice(&[0u8; 8]);
    assert!(matches!(
        def.decode_numbers(&p),
        Err(Error::Encapsulation(0x0003))
    ));
}

#[test]
fn big_endian_payloads_decode_too() {
    let def = MessageDef::parse("builtin_interfaces/Time", "int32 sec\nuint32 nanosec\n").unwrap();
    let be = [0x00, 0x00, 0x00, 0x00, 0, 0, 0, 2, 0, 0, 0, 5];
    let le = [0x00, 0x01, 0x00, 0x00, 2, 0, 0, 0, 5, 0, 0, 0];
    assert_eq!(def.decode_numbers(&be).unwrap(), vec![2.0, 5.0]);
    assert_eq!(def.decode_numbers(&le).unwrap(), vec![2.0, 5.0]);
}

#[test]
fn constants_and_comments_are_not_fields() {
    // A constant is not on the wire. If it were read as a field, every offset after it would be
    // wrong -- which `finish()` would catch, but the message deserves to just work.
    let def = MessageDef::parse(
        "test/Thing",
        "uint8 STATE_OK=1  # a constant\nint32 value  # a field\n",
    )
    .unwrap();
    assert_eq!(
        def.decode_numbers(&[0x00, 0x01, 0x00, 0x00, 7, 0, 0, 0])
            .unwrap(),
        vec![7.0]
    );
}
