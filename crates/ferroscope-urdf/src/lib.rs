//! **Point it at your robot.**
//!
//! Reads a URDF and turns it into a Ferroscope scene: one [`ferroscope_schema::Geometry`] per
//! visual, parented to its link, and one [`ferroscope_schema::Transform`] per link per step
//! from forward kinematics. The viewer then draws the robot because the recording says what
//! the robot is, not because the viewer was taught about it.
//!
//! Zero dependencies beyond the Ferroscope crates: the XML dialect URDF uses is small enough
//! to read directly, and a robot description is not worth an XML stack.
//!
//! ```
//! use ferroscope_urdf::Robot;
//!
//! let robot = Robot::parse(r#"
//!   <robot name="two_link">
//!     <link name="base">
//!       <visual><geometry><cylinder radius="0.06" length="0.04"/></geometry></visual>
//!     </link>
//!     <link name="upper">
//!       <visual>
//!         <origin xyz="0 0 0.15"/>
//!         <geometry><box size="0.06 0.06 0.3"/></geometry>
//!       </visual>
//!     </link>
//!     <joint name="shoulder" type="revolute">
//!       <parent link="base"/><child link="upper"/>
//!       <origin xyz="0 0 0.04"/><axis xyz="0 1 0"/>
//!       <limit lower="-1.5" upper="1.5" effort="10" velocity="2"/>
//!     </joint>
//!   </robot>
//! "#).unwrap();
//!
//! assert_eq!(robot.name, "two_link");
//! assert_eq!(robot.links.len(), 2);
//! assert_eq!(robot.movable_joints().count(), 1);
//!
//! // Forward kinematics: the upper link swings about y at the shoulder.
//! let poses = robot.forward_kinematics(&[("shoulder".into(), std::f64::consts::FRAC_PI_2)]);
//! let upper = poses.iter().find(|(l, _)| l == "upper").unwrap();
//! assert!((upper.1.translation[2] - 0.04).abs() < 1e-12);
//! ```

#![forbid(unsafe_code)]

pub mod xml;

use ferroscope_schema::{Geometry, Recorder, Shape, Stamp};
use std::io::Write;

/// A rigid transform, as URDF writes one: `xyz` and fixed-axis `rpy`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pose {
    pub translation: [f64; 3],
    /// `[x, y, z, w]`.
    pub rotation: [f64; 4],
}

impl Default for Pose {
    fn default() -> Self {
        Pose {
            translation: [0.0; 3],
            rotation: [0.0, 0.0, 0.0, 1.0],
        }
    }
}

impl Pose {
    /// URDF's `rpy` is a fixed-axis roll-pitch-yaw, which composes as `Rz(y) · Ry(p) · Rx(r)`.
    pub fn from_rpy(t: [f64; 3], rpy: [f64; 3]) -> Pose {
        let (cr, sr) = ((rpy[0] * 0.5).cos(), (rpy[0] * 0.5).sin());
        let (cp, sp) = ((rpy[1] * 0.5).cos(), (rpy[1] * 0.5).sin());
        let (cy, sy) = ((rpy[2] * 0.5).cos(), (rpy[2] * 0.5).sin());
        Pose {
            translation: t,
            rotation: [
                sr * cp * cy - cr * sp * sy,
                cr * sp * cy + sr * cp * sy,
                cr * cp * sy - sr * sp * cy,
                cr * cp * cy + sr * sp * sy,
            ],
        }
    }

    /// A rotation of `angle` about a unit `axis`.
    pub fn from_axis_angle(axis: [f64; 3], angle: f64) -> Pose {
        let n = (axis[0] * axis[0] + axis[1] * axis[1] + axis[2] * axis[2]).sqrt();
        if n < 1e-12 {
            return Pose::default();
        }
        let (s, c) = ((angle * 0.5).sin(), (angle * 0.5).cos());
        Pose {
            translation: [0.0; 3],
            rotation: [axis[0] / n * s, axis[1] / n * s, axis[2] / n * s, c],
        }
    }

    /// `self` then `other`, as transforms compose.
    pub fn then(&self, other: &Pose) -> Pose {
        let q = qmul(self.rotation, other.rotation);
        let t = qrot(self.rotation, other.translation);
        Pose {
            translation: [
                self.translation[0] + t[0],
                self.translation[1] + t[1],
                self.translation[2] + t[2],
            ],
            rotation: q,
        }
    }
}

fn qmul(a: [f64; 4], b: [f64; 4]) -> [f64; 4] {
    [
        a[3] * b[0] + a[0] * b[3] + a[1] * b[2] - a[2] * b[1],
        a[3] * b[1] - a[0] * b[2] + a[1] * b[3] + a[2] * b[0],
        a[3] * b[2] + a[0] * b[1] - a[1] * b[0] + a[2] * b[3],
        a[3] * b[3] - a[0] * b[0] - a[1] * b[1] - a[2] * b[2],
    ]
}

fn qrot(q: [f64; 4], v: [f64; 3]) -> [f64; 3] {
    let u = [q[0], q[1], q[2]];
    let uv = cross(u, v);
    let uuv = cross(u, uv);
    [
        v[0] + 2.0 * (q[3] * uv[0] + uuv[0]),
        v[1] + 2.0 * (q[3] * uv[1] + uuv[1]),
        v[2] + 2.0 * (q[3] * uv[2] + uuv[2]),
    ]
}

fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

/// One drawable on a link.
#[derive(Clone, Debug, PartialEq)]
pub struct Visual {
    pub pose: Pose,
    pub shape: Shape,
    /// Box full extents, cylinder `[r, r, length]`, sphere `[r, r, r]`, mesh scale.
    pub size: [f64; 3],
    pub color: [f64; 4],
    /// For a `<mesh>`: the `filename` as written. Attach the bytes under this name to draw it.
    pub mesh: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Link {
    pub name: String,
    pub visuals: Vec<Visual>,
}

/// What a joint does to the transform between its parent and child.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JointKind {
    Fixed,
    Revolute,
    Continuous,
    Prismatic,
    /// `floating` and `planar`, which this crate treats as fixed and says so.
    Unsupported,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Joint {
    pub name: String,
    pub kind: JointKind,
    pub parent: String,
    pub child: String,
    pub origin: Pose,
    pub axis: [f64; 3],
    pub limits: Option<(f64, f64)>,
}

impl Joint {
    /// The joint's own transform at position `q`.
    pub fn transform(&self, q: f64) -> Pose {
        match self.kind {
            JointKind::Revolute | JointKind::Continuous => Pose::from_axis_angle(self.axis, q),
            JointKind::Prismatic => Pose {
                translation: [self.axis[0] * q, self.axis[1] * q, self.axis[2] * q],
                rotation: [0.0, 0.0, 0.0, 1.0],
            },
            _ => Pose::default(),
        }
    }
    /// Clamp to the declared limits, when there are any.
    pub fn clamp(&self, q: f64) -> f64 {
        match self.limits {
            Some((lo, hi)) if self.kind != JointKind::Continuous => q.max(lo).min(hi),
            _ => q,
        }
    }
}

/// A parsed robot description.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Robot {
    pub name: String,
    pub links: Vec<Link>,
    pub joints: Vec<Joint>,
    /// Anything the parser understood but chose not to model, named rather than dropped.
    pub notes: Vec<String>,
}

/// What went wrong reading a description.
#[derive(Clone, Debug)]
pub enum Error {
    Xml(xml::XmlError),
    /// The document parsed but is not a robot description.
    NotUrdf(String),
    /// The kinematic tree does not close: a joint names a link that is not declared, or the
    /// links form a cycle, or there is more than one root.
    BadTree(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Xml(e) => write!(f, "{e}"),
            Error::NotUrdf(s) => write!(f, "not a URDF: {s}"),
            Error::BadTree(s) => write!(f, "the kinematic tree is broken: {s}"),
        }
    }
}

impl std::error::Error for Error {}

const PALETTE: [[f64; 4]; 6] = [
    [0.81, 0.67, 0.36, 1.0],
    [0.54, 0.63, 0.74, 1.0],
    [0.27, 0.78, 0.69, 1.0],
    [0.62, 0.55, 1.0, 1.0],
    [0.90, 0.55, 0.45, 1.0],
    [0.45, 0.70, 0.90, 1.0],
];

impl Robot {
    /// Parse a URDF document.
    pub fn parse(text: &str) -> Result<Robot, Error> {
        let root = xml::parse(text).map_err(Error::Xml)?;
        if root.name != "robot" {
            return Err(Error::NotUrdf(format!(
                "the root element is <{}>, not <robot>",
                root.name
            )));
        }
        let mut r = Robot {
            name: root.attr("name").unwrap_or("robot").to_string(),
            ..Default::default()
        };

        for (i, l) in root.children_named("link").enumerate() {
            let name = match l.attr("name") {
                Some(n) => n.to_string(),
                None => {
                    r.notes.push("a <link> has no name and was skipped".into());
                    continue;
                }
            };
            let mut visuals = Vec::new();
            for v in l.children_named("visual") {
                match parse_visual(v, PALETTE[i % PALETTE.len()]) {
                    Ok(vis) => visuals.push(vis),
                    Err(why) => r.notes.push(format!("link {name}: {why}")),
                }
            }
            r.links.push(Link { name, visuals });
        }

        for j in root.children_named("joint") {
            let name = j.attr("name").unwrap_or("joint").to_string();
            let kind = match j.attr("type").unwrap_or("fixed") {
                "fixed" => JointKind::Fixed,
                "revolute" => JointKind::Revolute,
                "continuous" => JointKind::Continuous,
                "prismatic" => JointKind::Prismatic,
                other => {
                    r.notes
                        .push(format!("joint {name}: type {other:?} is held fixed"));
                    JointKind::Unsupported
                }
            };
            let parent = match j.child("parent").and_then(|p| p.attr("link")) {
                Some(p) => p.to_string(),
                None => {
                    r.notes
                        .push(format!("joint {name} has no <parent>, skipped"));
                    continue;
                }
            };
            let child = match j.child("child").and_then(|c| c.attr("link")) {
                Some(c) => c.to_string(),
                None => {
                    r.notes
                        .push(format!("joint {name} has no <child>, skipped"));
                    continue;
                }
            };
            let limits = j
                .child("limit")
                .and_then(|l| Some((l.attr_f64("lower")?, l.attr_f64("upper")?)));
            r.joints.push(Joint {
                name,
                kind,
                parent,
                child,
                origin: origin_of(j.child("origin")),
                axis: j
                    .child("axis")
                    .and_then(|a| a.attr_vec("xyz"))
                    .and_then(|v| <[f64; 3]>::try_from(v).ok())
                    .unwrap_or([1.0, 0.0, 0.0]),
                limits,
            });
        }

        r.check_tree()?;
        Ok(r)
    }

    /// The single link with no parent joint.
    pub fn root_link(&self) -> Option<&str> {
        self.links
            .iter()
            .map(|l| l.name.as_str())
            .find(|n| !self.joints.iter().any(|j| j.child == *n))
    }

    /// Joints a caller can command.
    pub fn movable_joints(&self) -> impl Iterator<Item = &Joint> {
        self.joints.iter().filter(|j| {
            matches!(
                j.kind,
                JointKind::Revolute | JointKind::Continuous | JointKind::Prismatic
            )
        })
    }

    fn check_tree(&self) -> Result<(), Error> {
        for j in &self.joints {
            for l in [&j.parent, &j.child] {
                if !self.links.iter().any(|x| &x.name == l) {
                    return Err(Error::BadTree(format!(
                        "joint {} names link {l:?}, which is not declared",
                        j.name
                    )));
                }
            }
        }
        let roots: Vec<&str> = self
            .links
            .iter()
            .map(|l| l.name.as_str())
            .filter(|n| !self.joints.iter().any(|j| j.child == *n))
            .collect();
        match roots.len() {
            1 => Ok(()),
            0 => Err(Error::BadTree(
                "every link has a parent, so the joints form a cycle".into(),
            )),
            _ => Err(Error::BadTree(format!(
                "{} links have no parent ({}); a robot has one root",
                roots.len(),
                roots.join(", ")
            ))),
        }
    }

    /// World pose of every link, given joint positions by name. Unnamed joints sit at zero.
    ///
    /// Returned in tree order, parents before children, so a consumer can stream them without
    /// waiting for the whole set.
    pub fn forward_kinematics(&self, q: &[(String, f64)]) -> Vec<(String, Pose)> {
        let mut out: Vec<(String, Pose)> = Vec::with_capacity(self.links.len());
        let Some(root) = self.root_link() else {
            return out;
        };
        out.push((root.to_string(), Pose::default()));

        // Breadth-first from the root. A URDF tree is small, so a scan per level costs nothing
        // and needs no index to go stale.
        let mut frontier = vec![root.to_string()];
        while let Some(parent) = frontier.pop() {
            let parent_pose = out
                .iter()
                .find(|(n, _)| *n == parent)
                .map(|(_, p)| *p)
                .unwrap_or_default();
            for j in self.joints.iter().filter(|j| j.parent == parent) {
                let qv = q
                    .iter()
                    .find(|(n, _)| *n == j.name)
                    .map(|(_, v)| j.clamp(*v))
                    .unwrap_or(0.0);
                let pose = parent_pose.then(&j.origin).then(&j.transform(qv));
                out.push((j.child.clone(), pose));
                frontier.push(j.child.clone());
            }
        }
        out
    }

    /// Declare every visual once, parented to its link's frame.
    ///
    /// Call before the loop; call [`Robot::log_pose`] each step to move the frames. Visuals
    /// land on `<prefix>/visual/<link>` and transforms on `<prefix>/tf/<link>`, because one
    /// MCAP channel carries exactly one schema.
    pub fn declare<W: Write>(
        &self,
        rec: &mut Recorder<W>,
        t: Stamp,
        topic_prefix: &str,
    ) -> ferroscope_mcap::Result<()> {
        for link in &self.links {
            for (i, v) in link.visuals.iter().enumerate() {
                let id = if link.visuals.len() == 1 {
                    link.name.clone()
                } else {
                    format!("{}#{i}", link.name)
                };
                let g = Geometry {
                    frame: link.name.clone(),
                    id,
                    shape: v.shape,
                    size: v.size,
                    translation: v.pose.translation,
                    rotation: v.pose.rotation,
                    color: v.color,
                    points: Vec::new(),
                    mesh: v.mesh.clone(),
                };
                rec.geometry(&format!("{topic_prefix}/visual/{}", link.name), t, &g)?;
            }
        }
        Ok(())
    }

    /// Log every link's world transform for one instant.
    pub fn log_pose<W: Write>(
        &self,
        rec: &mut Recorder<W>,
        t: Stamp,
        q: &[(String, f64)],
        topic_prefix: &str,
    ) -> ferroscope_mcap::Result<()> {
        for (link, pose) in self.forward_kinematics(q) {
            rec.transform(
                &format!("{topic_prefix}/tf/{link}"),
                t,
                "world",
                &link,
                pose.translation,
                pose.rotation,
            )?;
        }
        Ok(())
    }
}

fn origin_of(e: Option<&xml::Element>) -> Pose {
    let Some(e) = e else {
        return Pose::default();
    };
    let xyz = e
        .attr_vec("xyz")
        .and_then(|v| <[f64; 3]>::try_from(v).ok())
        .unwrap_or([0.0; 3]);
    let rpy = e
        .attr_vec("rpy")
        .and_then(|v| <[f64; 3]>::try_from(v).ok())
        .unwrap_or([0.0; 3]);
    Pose::from_rpy(xyz, rpy)
}

fn parse_visual(v: &xml::Element, fallback: [f64; 4]) -> Result<Visual, String> {
    let geom = v.child("geometry").ok_or("a <visual> has no <geometry>")?;
    let color = v
        .child("material")
        .and_then(|m| m.child("color"))
        .and_then(|c| c.attr_vec("rgba"))
        .and_then(|v| <[f64; 4]>::try_from(v).ok())
        .unwrap_or(fallback);

    let (shape, size, mesh) = if let Some(b) = geom.child("box") {
        let s = b
            .attr_vec("size")
            .and_then(|v| <[f64; 3]>::try_from(v).ok())
            .ok_or("<box> needs size=\"x y z\"")?;
        (Shape::Box, s, String::new())
    } else if let Some(c) = geom.child("cylinder") {
        let r = c.attr_f64("radius").ok_or("<cylinder> needs a radius")?;
        let l = c.attr_f64("length").ok_or("<cylinder> needs a length")?;
        (Shape::Cylinder, [r, r, l], String::new())
    } else if let Some(s) = geom.child("sphere") {
        let r = s.attr_f64("radius").ok_or("<sphere> needs a radius")?;
        (Shape::Sphere, [r, r, r], String::new())
    } else if let Some(m) = geom.child("mesh") {
        let f = m.attr("filename").ok_or("<mesh> needs a filename")?;
        let scale = m
            .attr_vec("scale")
            .and_then(|v| <[f64; 3]>::try_from(v).ok())
            .unwrap_or([1.0; 3]);
        (Shape::Mesh, scale, f.to_string())
    } else {
        return Err("a <geometry> holds no box, cylinder, sphere or mesh".into());
    };

    Ok(Visual {
        pose: origin_of(v.child("origin")),
        shape,
        size,
        color,
        mesh,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const ARM: &str = r#"
    <robot name="arm">
      <link name="base">
        <visual><geometry><cylinder radius="0.08" length="0.05"/></geometry>
          <material name="g"><color rgba="0.3 0.35 0.5 1"/></material></visual>
      </link>
      <link name="upper">
        <visual><origin xyz="0 0 0.15"/><geometry><box size="0.06 0.06 0.3"/></geometry></visual>
      </link>
      <link name="fore">
        <visual><origin xyz="0 0 0.11"/><geometry><box size="0.05 0.05 0.22"/></geometry></visual>
      </link>
      <joint name="shoulder" type="revolute">
        <parent link="base"/><child link="upper"/>
        <origin xyz="0 0 0.05"/><axis xyz="0 1 0"/>
        <limit lower="-1.2" upper="1.2" effort="8" velocity="2"/>
      </joint>
      <joint name="elbow" type="revolute">
        <parent link="upper"/><child link="fore"/>
        <origin xyz="0 0 0.3"/><axis xyz="0 1 0"/>
        <limit lower="-2.0" upper="2.0" effort="6" velocity="2"/>
      </joint>
    </robot>"#;

    #[test]
    fn a_three_link_arm_parses_with_its_tree_intact() {
        let r = Robot::parse(ARM).unwrap();
        assert_eq!(r.name, "arm");
        assert_eq!(r.links.len(), 3);
        assert_eq!(r.joints.len(), 2);
        assert_eq!(r.root_link(), Some("base"));
        assert_eq!(r.movable_joints().count(), 2);
        assert!(
            r.notes.is_empty(),
            "nothing should have been skipped: {:?}",
            r.notes
        );
        // A declared material wins over the palette; an undeclared one takes the fallback.
        assert_eq!(r.links[0].visuals[0].color, [0.3, 0.35, 0.5, 1.0]);
        assert_eq!(r.links[1].visuals[0].color, PALETTE[1]);
    }

    #[test]
    fn forward_kinematics_puts_the_links_where_the_geometry_says() {
        let r = Robot::parse(ARM).unwrap();
        let at = |q: &[(String, f64)], link: &str| {
            r.forward_kinematics(q)
                .into_iter()
                .find(|(n, _)| n == link)
                .unwrap()
                .1
        };
        // At rest the chain stacks along z: base at 0, upper at 0.05, fore at 0.35.
        let rest = at(&[], "fore");
        assert!((rest.translation[2] - 0.35).abs() < 1e-12, "{rest:?}");
        assert!(rest.translation[0].abs() < 1e-12);

        // Rotating the shoulder about +y swings the forearm into the x-z plane. The expected
        // place is computed from the angle rather than pinned to a constant, so the test states
        // the geometry instead of a number somebody would later have to re-derive.
        //
        // Note the angle is the CLAMPED one: the shoulder is limited to 1.2 rad, and asking for
        // pi/2 lands on the limit. That is the behaviour, so the test uses it.
        let a = 1.2f64;
        let bent = at(&[("shoulder".into(), std::f64::consts::FRAC_PI_2)], "fore");
        assert!(
            (bent.translation[0] - 0.30 * a.sin()).abs() < 1e-9,
            "x should be 0.30·sin({a}); got {bent:?}"
        );
        assert!(
            (bent.translation[2] - (0.05 + 0.30 * a.cos())).abs() < 1e-9,
            "z should be 0.05 + 0.30·cos({a}); got {bent:?}"
        );
        // And the forearm carries the rotation, not just the offset.
        let half = a * 0.5;
        assert!((bent.rotation[1] - half.sin()).abs() < 1e-9, "{bent:?}");
        assert!((bent.rotation[3] - half.cos()).abs() < 1e-9, "{bent:?}");
    }

    #[test]
    fn joint_limits_are_enforced_rather_than_advisory() {
        let r = Robot::parse(ARM).unwrap();
        let hard = r.forward_kinematics(&[("shoulder".into(), 99.0)]);
        let soft = r.forward_kinematics(&[("shoulder".into(), 1.2)]);
        let get = |v: &Vec<(String, Pose)>| v.iter().find(|(n, _)| n == "fore").unwrap().1;
        assert_eq!(
            get(&hard).translation,
            get(&soft).translation,
            "a command past the limit must land on the limit"
        );
    }

    #[test]
    fn a_continuous_joint_ignores_limits_because_it_has_none() {
        let r = Robot::parse(
            r#"<robot name="w">
                 <link name="a"><visual><geometry><sphere radius="0.1"/></geometry></visual></link>
                 <link name="b"><visual><geometry><sphere radius="0.1"/></geometry></visual></link>
                 <joint name="spin" type="continuous">
                   <parent link="a"/><child link="b"/><axis xyz="0 0 1"/>
                   <limit lower="-1" upper="1" effort="1" velocity="1"/>
                 </joint>
               </robot>"#,
        )
        .unwrap();
        let j = r.joints.iter().find(|j| j.name == "spin").unwrap();
        assert_eq!(j.clamp(10.0), 10.0);
    }

    #[test]
    fn a_broken_tree_is_named_rather_than_half_drawn() {
        let dangling = Robot::parse(
            r#"<robot name="x">
                 <link name="a"><visual><geometry><sphere radius="1"/></geometry></visual></link>
                 <joint name="j" type="fixed"><parent link="a"/><child link="ghost"/></joint>
               </robot>"#,
        );
        assert!(format!("{}", dangling.unwrap_err()).contains("ghost"));

        let two_roots = Robot::parse(
            r#"<robot name="x">
                 <link name="a"><visual><geometry><sphere radius="1"/></geometry></visual></link>
                 <link name="b"><visual><geometry><sphere radius="1"/></geometry></visual></link>
               </robot>"#,
        );
        assert!(format!("{}", two_roots.unwrap_err()).contains("one root"));
    }

    #[test]
    fn an_unsupported_joint_type_is_noted_not_dropped() {
        let r = Robot::parse(
            r#"<robot name="x">
                 <link name="a"><visual><geometry><sphere radius="1"/></geometry></visual></link>
                 <link name="b"><visual><geometry><sphere radius="1"/></geometry></visual></link>
                 <joint name="f" type="floating"><parent link="a"/><child link="b"/></joint>
               </robot>"#,
        )
        .unwrap();
        assert_eq!(r.joints[0].kind, JointKind::Unsupported);
        assert!(
            r.notes.iter().any(|n| n.contains("floating")),
            "{:?}",
            r.notes
        );
    }

    #[test]
    fn a_mesh_visual_keeps_its_filename_for_an_attachment_to_match() {
        let r = Robot::parse(
            r#"<robot name="x"><link name="a"><visual>
                 <geometry><mesh filename="link.glb" scale="0.5 0.5 0.5"/></geometry>
               </visual></link></robot>"#,
        )
        .unwrap();
        let v = &r.links[0].visuals[0];
        assert_eq!(v.shape, Shape::Mesh);
        assert_eq!(v.mesh, "link.glb");
        assert_eq!(v.size, [0.5, 0.5, 0.5]);
    }

    #[test]
    fn rpy_follows_the_urdf_convention() {
        // A 90 degree yaw takes +x to +y.
        let p = Pose::from_rpy([0.0; 3], [0.0, 0.0, std::f64::consts::FRAC_PI_2]);
        let v = qrot(p.rotation, [1.0, 0.0, 0.0]);
        assert!(v[0].abs() < 1e-12 && (v[1] - 1.0).abs() < 1e-12, "{v:?}");
        // A 90 degree roll takes +y to +z.
        let p = Pose::from_rpy([0.0; 3], [std::f64::consts::FRAC_PI_2, 0.0, 0.0]);
        let v = qrot(p.rotation, [0.0, 1.0, 0.0]);
        assert!((v[2] - 1.0).abs() < 1e-12, "{v:?}");
    }
}
