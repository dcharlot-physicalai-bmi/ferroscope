"""Write a ROS 2 recording with the tooling ROS 2 users actually record with.

A fixture we encoded ourselves would prove only that our decoder matches our encoder, so this
uses `mcap-ros2-support`: CDR payloads, a `ros2msg` schema, zstd chunks — the shape that comes
off a real robot.

    python3 ci/make-ros2-fixture.py [out.mcap]
"""
import math
import sys

from mcap.writer import CompressionType
from mcap_ros2.writer import Writer

DEF = """std_msgs/Header header
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
"""

MESSAGES = 1500


def position(i):
    """The series the decoder is checked against."""
    return [math.sin(i * 0.01) * 1.2, math.cos(i * 0.013) * 0.8, i * 0.0004]


def main(out, drift_at=None, eps=1e-9):
    """`drift_at` perturbs position[0] from that message on, for comparator fixtures."""
    with open(out, "wb") as f:
        w = Writer(f, compression=CompressionType.ZSTD)
        schema = w.register_msgdef("sensor_msgs/msg/JointState", DEF)
        for i in range(MESSAGES):
            t = i * 10_000_000  # 100 Hz
            p = position(i)
            if drift_at is not None and i >= drift_at:
                p = [p[0] + eps, p[1], p[2]]
            w.write_message(
                "/joint_states",
                schema,
                {
                    "header": {
                        "stamp": {"sec": t // 10**9, "nanosec": t % 10**9},
                        "frame_id": "base",
                    },
                    "name": ["shoulder", "elbow", "wrist"],
                    "position": p,
                    "velocity": [0.0, 0.0, 0.0],
                    "effort": [1.5, -0.25, 0.0],
                },
                log_time=t,
                publish_time=t,
            )
        w.finish()
    print(f"wrote {out}: {MESSAGES} JointState messages, zstd chunks, CDR payloads")


if __name__ == "__main__":
    out = sys.argv[1] if len(sys.argv) > 1 else "ros2.mcap"
    drift = int(sys.argv[2]) if len(sys.argv) > 2 else None
    main(out, drift)
