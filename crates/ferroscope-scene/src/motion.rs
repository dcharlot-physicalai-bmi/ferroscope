//! How a body moves, described rather than solved.
//!
//! Every motion here has a closed form, so the pose at time *t* costs the same whether *t* is
//! the first step or the millionth, and two runs of the same scene agree bit for bit without
//! anything having to be deterministic about the order of operations. That is what lets a
//! described scene carry the same determinism receipt as a simulated one.

use crate::{vec3, Problem};
use ferroscope_schema::json::Value;

/// The motions a scene can describe.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Motion {
    /// Sitting still.
    Static { at: [f64; 3] },
    /// Straight there and back, once per period.
    Linear {
        from: [f64; 3],
        to: [f64; 3],
        period_s: f64,
    },
    /// A circle about a centre, in the plane normal to `axis`.
    Orbit {
        center: [f64; 3],
        radius: f64,
        period_s: f64,
        axis: [f64; 3],
    },
    /// Back and forth along an axis.
    Oscillate {
        at: [f64; 3],
        axis: [f64; 3],
        amplitude: f64,
        period_s: f64,
    },
    /// Dropped, and bouncing off the ground with the given restitution.
    ///
    /// The only motion here that is integrated rather than merely evaluated — and it is still
    /// closed form, because a ballistic arc with a fixed restitution has one: each bounce is a
    /// parabola whose duration is a fixed fraction of the last. That matters because a scene
    /// scrubbed to a timestamp must give the same answer as one played to it.
    Fall {
        from: [f64; 3],
        restitution: f64,
        /// The height of the resting contact, i.e. half the body's own extent.
        rest_z: f64,
    },
}

impl Default for Motion {
    fn default() -> Self {
        Motion::Static { at: [0.0; 3] }
    }
}

fn unit(v: [f64; 3], fallback: [f64; 3]) -> [f64; 3] {
    let n = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if n < 1e-12 {
        fallback
    } else {
        [v[0] / n, v[1] / n, v[2] / n]
    }
}

impl Motion {
    /// Whether this motion actually moves, which the energy model needs to know.
    pub fn is_moving(&self) -> bool {
        !matches!(self, Motion::Static { .. })
    }

    /// A one-line description, for the run spec.
    pub fn describe(&self) -> String {
        match self {
            Motion::Static { at } => format!("static at {at:?}"),
            Motion::Linear { from, to, period_s } => {
                format!("linear {from:?} -> {to:?} every {period_s} s")
            }
            Motion::Orbit {
                center,
                radius,
                period_s,
                axis,
            } => format!("orbit r={radius} about {center:?} axis {axis:?} every {period_s} s"),
            Motion::Oscillate {
                at,
                axis,
                amplitude,
                period_s,
            } => format!("oscillate +-{amplitude} along {axis:?} at {at:?} every {period_s} s"),
            Motion::Fall {
                from,
                restitution,
                rest_z,
            } => format!("fall from {from:?} restitution {restitution} resting at {rest_z}"),
        }
    }

    /// Position and orientation at time `t`, in seconds.
    pub fn at(&self, t: f64, gravity: f64) -> ([f64; 3], [f64; 4]) {
        const IDENT: [f64; 4] = [0.0, 0.0, 0.0, 1.0];
        match *self {
            Motion::Static { at } => (at, IDENT),
            Motion::Linear { from, to, period_s } => {
                // Triangle wave in 0..1: there over the first half, back over the second, with
                // no jump at the wrap.
                let u = if period_s > 0.0 { t / period_s } else { 0.0 };
                let f = u.rem_euclid(1.0);
                let s = if f < 0.5 { f * 2.0 } else { 2.0 - f * 2.0 };
                (
                    [
                        from[0] + (to[0] - from[0]) * s,
                        from[1] + (to[1] - from[1]) * s,
                        from[2] + (to[2] - from[2]) * s,
                    ],
                    IDENT,
                )
            }
            Motion::Orbit {
                center,
                radius,
                period_s,
                axis,
            } => {
                let w = if period_s > 0.0 {
                    std::f64::consts::TAU / period_s
                } else {
                    0.0
                };
                let n = unit(axis, [0.0, 0.0, 1.0]);
                // Any two vectors perpendicular to the axis will do; picking the one that is
                // least aligned with it keeps the cross product well conditioned.
                let seed = if n[2].abs() < 0.9 {
                    [0.0, 0.0, 1.0]
                } else {
                    [1.0, 0.0, 0.0]
                };
                let u = unit(
                    [
                        n[1] * seed[2] - n[2] * seed[1],
                        n[2] * seed[0] - n[0] * seed[2],
                        n[0] * seed[1] - n[1] * seed[0],
                    ],
                    [1.0, 0.0, 0.0],
                );
                let v = [
                    n[1] * u[2] - n[2] * u[1],
                    n[2] * u[0] - n[0] * u[2],
                    n[0] * u[1] - n[1] * u[0],
                ];
                let (c, s) = ((w * t).cos(), (w * t).sin());
                let p = [
                    center[0] + radius * (u[0] * c + v[0] * s),
                    center[1] + radius * (u[1] * c + v[1] * s),
                    center[2] + radius * (u[2] * c + v[2] * s),
                ];
                // Face along travel, so a body on an orbit reads as going somewhere.
                let yaw = (v[1] * c - u[1] * s).atan2(v[0] * c - u[0] * s);
                (p, [0.0, 0.0, (yaw * 0.5).sin(), (yaw * 0.5).cos()])
            }
            Motion::Oscillate {
                at,
                axis,
                amplitude,
                period_s,
            } => {
                let w = if period_s > 0.0 {
                    std::f64::consts::TAU / period_s
                } else {
                    0.0
                };
                let n = unit(axis, [1.0, 0.0, 0.0]);
                let s = amplitude * (w * t).sin();
                (
                    [at[0] + n[0] * s, at[1] + n[1] * s, at[2] + n[2] * s],
                    IDENT,
                )
            }
            Motion::Fall {
                from,
                restitution,
                rest_z,
            } => {
                let g = gravity.abs().max(1e-9);
                let h = (from[2] - rest_z).max(0.0);
                // Time of the first contact, then each subsequent flight is `restitution`
                // times the previous one's take-off speed, so its duration scales the same way.
                let t1 = (2.0 * h / g).sqrt();
                if t <= t1 || restitution <= 0.0 {
                    let z = from[2] - 0.5 * g * t * t;
                    return ([from[0], from[1], z.max(rest_z)], IDENT);
                }
                let v0 = g * t1; // speed at the first contact
                let mut tau = t - t1;
                let mut v = v0 * restitution;
                // Each bounce is shorter than the last by a fixed ratio, so the total flight
                // time converges. Walking the bounces is exact and terminates: at r < 1 the
                // remaining time is a geometric series, and once v is negligible the body is
                // at rest for good.
                loop {
                    let flight = 2.0 * v / g;
                    if flight < 1e-9 {
                        return ([from[0], from[1], rest_z], IDENT);
                    }
                    if tau < flight {
                        let z = rest_z + v * tau - 0.5 * g * tau * tau;
                        return ([from[0], from[1], z.max(rest_z)], IDENT);
                    }
                    tau -= flight;
                    v *= restitution;
                }
            }
        }
    }
}

pub(crate) fn parse(b: &Value, path: &str, out: &mut Vec<Problem>) -> Motion {
    let Some(m) = b.get("motion") else {
        return Motion::Static {
            at: vec3(b, path, "at", [0.0; 3], out),
        };
    };
    let mpath = format!("{path}.motion");
    let period = |d: f64, out: &mut Vec<Problem>| -> f64 {
        match m.get("period_s") {
            None => d,
            Some(Value::Num(n)) if *n > 0.0 && n.is_finite() => *n,
            Some(_) => {
                out.push(Problem {
                    path: format!("{mpath}.period_s"),
                    message: "must be a positive number of seconds".into(),
                });
                d
            }
        }
    };
    match m.get("kind").and_then(|k| k.as_str()) {
        Some("static") | None => Motion::Static {
            at: vec3(m, &mpath, "at", [0.0; 3], out),
        },
        Some("linear") => Motion::Linear {
            from: vec3(m, &mpath, "from", [0.0; 3], out),
            to: vec3(m, &mpath, "to", [1.0, 0.0, 0.0], out),
            period_s: period(4.0, out),
        },
        Some("orbit") => {
            let radius = m.get("radius").and_then(|r| r.as_f64()).unwrap_or(1.0);
            if !radius.is_finite() || radius <= 0.0 {
                out.push(Problem {
                    path: format!("{mpath}.radius"),
                    message: "must be a positive number".into(),
                });
            }
            Motion::Orbit {
                center: vec3(m, &mpath, "center", [0.0; 3], out),
                radius: if radius > 0.0 { radius } else { 1.0 },
                period_s: period(4.0, out),
                axis: vec3(m, &mpath, "axis", [0.0, 0.0, 1.0], out),
            }
        }
        Some("oscillate") => Motion::Oscillate {
            at: vec3(m, &mpath, "at", [0.0; 3], out),
            axis: vec3(m, &mpath, "axis", [1.0, 0.0, 0.0], out),
            amplitude: m.get("amplitude").and_then(|a| a.as_f64()).unwrap_or(0.5),
            period_s: period(4.0, out),
        },
        Some("fall") => {
            let r = m.get("restitution").and_then(|r| r.as_f64()).unwrap_or(0.4);
            if !(0.0..1.0).contains(&r) {
                out.push(Problem {
                    path: format!("{mpath}.restitution"),
                    message: format!(
                        "must be at least 0 and below 1, got {r}: a body that returns all its \
                         energy never comes to rest"
                    ),
                });
            }
            // Resting height is half the body's own extent, so a dropped box lands on the
            // ground rather than half through it. Spheres carry a semi-axis, not a diameter.
            let size = vec3(b, path, "size", [0.2, 0.2, 0.2], &mut Vec::new());
            let is_sphere = b.get("shape").and_then(|s| s.as_str()) == Some("sphere");
            Motion::Fall {
                from: vec3(m, &mpath, "from", [0.0, 0.0, 2.0], out),
                restitution: r.clamp(0.0, 0.999),
                rest_z: if is_sphere { size[2] } else { size[2] * 0.5 },
            }
        }
        Some(other) => {
            out.push(Problem {
                path: format!("{mpath}.kind"),
                message: format!(
                    "unknown motion {other:?}; expected one of static, linear, orbit, \
                     oscillate, fall"
                ),
            });
            Motion::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const G: f64 = -9.81;

    #[test]
    fn a_dropped_body_lands_when_the_arithmetic_says_it_should() {
        let m = Motion::Fall {
            from: [0.0, 0.0, 5.0],
            restitution: 0.0,
            rest_z: 0.0,
        };
        // h = ½gt², so 5 m takes sqrt(2·5/9.81) = 1.0096 s.
        let t = (2.0 * 5.0f64 / 9.81).sqrt();
        assert!(
            m.at(t * 0.5, G).0[2] > 3.7,
            "still falling at half the time"
        );
        assert!((m.at(t, G).0[2]).abs() < 1e-9, "on the ground at t={t}");
        assert!((m.at(t * 3.0, G).0[2]).abs() < 1e-9, "and stays there");
    }

    #[test]
    fn a_bouncing_body_loses_height_and_comes_to_rest() {
        let m = Motion::Fall {
            from: [0.0, 0.0, 2.0],
            restitution: 0.5,
            rest_z: 0.0,
        };
        let peak = |a: f64, b: f64| {
            (0..400)
                .map(|i| a + (b - a) * i as f64 / 400.0)
                .map(|t| m.at(t, G).0[2])
                .fold(0.0f64, f64::max)
        };
        let t1 = (2.0 * 2.0f64 / 9.81).sqrt();
        let first = peak(t1, t1 + 2.0 * (0.5 * 9.81 * t1) / 9.81);
        // Restitution 0.5 halves the speed, so the next apex is a quarter of the height.
        assert!(
            (first - 0.5).abs() < 0.02,
            "first bounce should reach about 0.5 m, got {first}"
        );
        assert!(
            m.at(60.0, G).0[2].abs() < 1e-6,
            "must settle, not bounce forever"
        );
    }

    #[test]
    fn nothing_ever_goes_below_its_resting_height() {
        // The property that matters for a viewer: the body never passes through the floor.
        let m = Motion::Fall {
            from: [0.0, 0.0, 3.0],
            restitution: 0.7,
            rest_z: 0.15,
        };
        for i in 0..5000 {
            let z = m.at(i as f64 * 0.004, G).0[2];
            assert!(z >= 0.15 - 1e-9, "z={z} at step {i}");
        }
    }

    #[test]
    fn an_orbit_holds_its_radius_and_returns_to_its_start() {
        let m = Motion::Orbit {
            center: [1.0, 2.0, 3.0],
            radius: 0.75,
            period_s: 2.0,
            axis: [0.0, 0.0, 1.0],
        };
        for i in 0..200 {
            let p = m.at(i as f64 * 0.01, G).0;
            let d = ((p[0] - 1.0).powi(2) + (p[1] - 2.0).powi(2) + (p[2] - 3.0).powi(2)).sqrt();
            assert!((d - 0.75).abs() < 1e-9, "radius drifted to {d}");
        }
        let a = m.at(0.0, G).0;
        let b = m.at(2.0, G).0;
        for k in 0..3 {
            assert!((a[k] - b[k]).abs() < 1e-9, "one period must close the loop");
        }
    }

    #[test]
    fn a_tilted_orbit_stays_in_its_own_plane() {
        let axis = [0.0, 1.0, 0.0];
        let m = Motion::Orbit {
            center: [0.0; 3],
            radius: 1.0,
            period_s: 1.0,
            axis,
        };
        for i in 0..100 {
            let p = m.at(i as f64 * 0.01, G).0;
            let along = p[0] * axis[0] + p[1] * axis[1] + p[2] * axis[2];
            assert!(along.abs() < 1e-9, "left the plane by {along}");
        }
    }

    #[test]
    fn a_linear_motion_reverses_without_a_jump() {
        let m = Motion::Linear {
            from: [0.0; 3],
            to: [2.0, 0.0, 0.0],
            period_s: 1.0,
        };
        assert!((m.at(0.0, G).0[0]).abs() < 1e-12);
        assert!(
            (m.at(0.5, G).0[0] - 2.0).abs() < 1e-12,
            "halfway is the far end"
        );
        assert!((m.at(1.0, G).0[0]).abs() < 1e-12, "and back at the period");
        // No discontinuity across the wrap.
        let a = m.at(0.999, G).0[0];
        let b = m.at(1.001, G).0[0];
        assert!(
            (a - b).abs() < 0.02,
            "jump of {} at the wrap",
            (a - b).abs()
        );
    }

    #[test]
    fn a_zero_period_is_treated_as_holding_still_rather_than_dividing_by_zero() {
        for m in [
            Motion::Orbit {
                center: [0.0; 3],
                radius: 1.0,
                period_s: 0.0,
                axis: [0.0, 0.0, 1.0],
            },
            Motion::Oscillate {
                at: [0.0; 3],
                axis: [1.0, 0.0, 0.0],
                amplitude: 1.0,
                period_s: 0.0,
            },
        ] {
            for t in [0.0, 1.0, 7.5] {
                let p = m.at(t, G).0;
                assert!(p.iter().all(|x| x.is_finite()), "{p:?} from {m:?}");
            }
        }
    }
}
