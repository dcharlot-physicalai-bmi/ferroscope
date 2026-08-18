//! Read a mesh, ask it what it weighs, and write it out as glTF.
//!
//! A robot description makes two claims about every link: a *shape* (the mesh) and a *mass
//! distribution* (the `<inertial>` block). Nothing in the usual toolchain checks that the second
//! is consistent with the first, because the two live in different files and the arithmetic that
//! connects them is a volume integral nobody wants to write twice.
//!
//! It is written here once. [`Mesh::mass_properties`] integrates volume, centroid and the full
//! inertia tensor straight off the triangles by the divergence theorem, so a declared inertia can
//! be compared against the geometry it claims to describe.
//!
//! ```
//! # use ferroscope_mesh::Mesh;
//! // A 2 x 4 x 6 box, as a triangle soup.
//! let m = Mesh::box_mesh([1.0, 2.0, 3.0]);
//! let p = m.mass_properties(1000.0); // kg/m³
//! assert!((p.volume - 48.0).abs() < 1e-9);
//! // A solid box: I_xx = m (b² + c²) / 12.
//! let want = p.mass * (4.0f64.powi(2) + 6.0f64.powi(2)) / 12.0;
//! assert!((p.inertia[0][0] - want).abs() / want < 1e-9);
//! ```
//!
//! No dependencies, and it builds for `wasm32-unknown-unknown` unchanged.

use std::fmt;

pub mod gltf;
pub mod stl;

/// What could not be read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// The bytes are neither a binary STL of the length they declare nor readable text.
    NotStl,
    Malformed {
        line: usize,
        what: String,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::NotStl => write!(
                f,
                "not an STL: the byte length does not match the declared triangle count, and the \
                 bytes are not valid text"
            ),
            Error::Malformed { line, what } => write!(f, "line {line}: {what}"),
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

/// An indexed triangle mesh.
///
/// STL arrives as a soup with every vertex repeated per face; [`Mesh::from_triangles`] welds it
/// into an indexed mesh, which is what makes the edge bookkeeping in [`Mesh::is_watertight`]
/// possible at all and cuts a real robot part by roughly six times.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Mesh {
    pub positions: Vec<[f64; 3]>,
    pub indices: Vec<u32>,
}

/// What a mesh weighs, if it were solid and of one material.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MassProperties {
    /// Signed volume in the mesh's own units cubed. Negative means the winding is inside out.
    pub volume: f64,
    pub mass: f64,
    /// Centre of mass, in the mesh's own frame.
    pub centroid: [f64; 3],
    /// The inertia tensor **about the centroid**, in the mesh's own axes.
    pub inertia: [[f64; 3]; 3],
}

impl MassProperties {
    /// The six independent entries, ordered as URDF writes them.
    pub fn urdf_inertia(&self) -> [f64; 6] {
        let i = &self.inertia;
        [i[0][0], i[0][1], i[0][2], i[1][1], i[1][2], i[2][2]]
    }
}

fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

impl Mesh {
    /// Weld a triangle soup into an indexed mesh.
    ///
    /// Vertices are matched on their exact bits. That is deliberate: STL stores `f32`, so two
    /// faces that meet at a corner carry byte-identical coordinates when the file is sound, and
    /// a tolerance here would silently merge two genuinely distinct vertices a micron apart. If
    /// a mesh fails [`Mesh::is_watertight`], the cause is the file, and that is worth reporting
    /// rather than papering over.
    pub fn from_triangles(tris: &[[[f64; 3]; 3]]) -> Mesh {
        let mut positions: Vec<[f64; 3]> = Vec::new();
        let mut indices = Vec::with_capacity(tris.len() * 3);
        let mut seen: Vec<(u64, u64, u64, u32)> = Vec::new();
        for t in tris {
            for v in t {
                let key = (v[0].to_bits(), v[1].to_bits(), v[2].to_bits());
                match seen
                    .iter()
                    .find(|(a, b, c, _)| (*a, *b, *c) == (key.0, key.1, key.2))
                {
                    Some((_, _, _, i)) => indices.push(*i),
                    None => {
                        let i = positions.len() as u32;
                        positions.push(*v);
                        seen.push((key.0, key.1, key.2, i));
                        indices.push(i);
                    }
                }
            }
        }
        Mesh { positions, indices }
    }

    /// An axis-aligned box of the given half-extents, centred on the origin. Outward winding.
    pub fn box_mesh(half: [f64; 3]) -> Mesh {
        let [a, b, c] = half;
        let v = [
            [-a, -b, -c],
            [a, -b, -c],
            [a, b, -c],
            [-a, b, -c],
            [-a, -b, c],
            [a, -b, c],
            [a, b, c],
            [-a, b, c],
        ];
        const FACES: [[usize; 4]; 6] = [
            [0, 3, 2, 1], // -z
            [4, 5, 6, 7], // +z
            [0, 1, 5, 4], // -y
            [2, 3, 7, 6], // +y
            [0, 4, 7, 3], // -x
            [1, 2, 6, 5], // +x
        ];
        let mut tris = Vec::new();
        for f in FACES {
            tris.push([v[f[0]], v[f[1]], v[f[2]]]);
            tris.push([v[f[0]], v[f[2]], v[f[3]]]);
        }
        Mesh::from_triangles(&tris)
    }

    pub fn triangles(&self) -> usize {
        self.indices.len() / 3
    }

    fn tri(&self, t: usize) -> [[f64; 3]; 3] {
        let i = t * 3;
        [
            self.positions[self.indices[i] as usize],
            self.positions[self.indices[i + 1] as usize],
            self.positions[self.indices[i + 2] as usize],
        ]
    }

    /// Axis-aligned bounds, or `None` for an empty mesh.
    pub fn bounds(&self) -> Option<([f64; 3], [f64; 3])> {
        let first = *self.positions.first()?;
        let mut lo = first;
        let mut hi = first;
        for p in &self.positions {
            for k in 0..3 {
                lo[k] = lo[k].min(p[k]);
                hi[k] = hi[k].max(p[k]);
            }
        }
        Some((lo, hi))
    }

    /// Signed volume by the divergence theorem: the sum of the tetrahedra each face forms with
    /// the origin. Positive for outward winding, and independent of where the origin sits.
    pub fn volume(&self) -> f64 {
        (0..self.triangles())
            .map(|t| {
                let [a, b, c] = self.tri(t);
                dot(a, cross(b, c)) / 6.0
            })
            .sum()
    }

    /// Volume, centre of mass and the full inertia tensor about that centre, at uniform density.
    ///
    /// Each face makes a tetrahedron with the origin; the second-order moment of a tetrahedron
    /// has a closed form, and the signed sum over a closed surface leaves exactly the solid. The
    /// result is exact for any closed mesh — no sampling, no voxels — which is why it is worth
    /// checking a hand-written `<inertial>` against.
    ///
    /// A mesh that is not closed still returns numbers, because refusing here would be worse
    /// than reporting; check [`Mesh::is_watertight`] to know whether to trust them.
    pub fn mass_properties(&self, density: f64) -> MassProperties {
        let mut vol = 0.0;
        let mut m1 = [0.0f64; 3];
        // The full second-moment integral ∫ x xᵀ dV, accumulated about the origin.
        let mut c = [[0.0f64; 3]; 3];
        for t in 0..self.triangles() {
            let [a, b, cc] = self.tri(t);
            let det = dot(a, cross(b, cc)); // = 6 × the tetrahedron's signed volume
            vol += det / 6.0;
            for k in 0..3 {
                m1[k] += det / 24.0 * (a[k] + b[k] + cc[k]);
            }
            // ∫ x xᵀ over the tetrahedron (0,a,b,c). The canonical tetrahedron on the unit
            // basis integrates to 1/60 on the diagonal and 1/120 off it, which is what the
            // 2× weighting on the squared terms reproduces.
            for i in 0..3 {
                for j in 0..3 {
                    c[i][j] += det / 120.0
                        * (2.0 * (a[i] * a[j] + b[i] * b[j] + cc[i] * cc[j])
                            + a[i] * b[j]
                            + b[i] * a[j]
                            + a[i] * cc[j]
                            + cc[i] * a[j]
                            + b[i] * cc[j]
                            + cc[i] * b[j]);
                }
            }
        }
        let mass = vol * density;
        let centroid = if vol.abs() > f64::EPSILON {
            [m1[0] / vol, m1[1] / vol, m1[2] / vol]
        } else {
            [0.0; 3]
        };
        // Shift the second moment to the centroid, then turn it into an inertia tensor:
        // I = tr(C)·1 − C, which is the standard identity between the covariance of the mass
        // distribution and its moment of inertia.
        let mut cc = [[0.0f64; 3]; 3];
        for i in 0..3 {
            for j in 0..3 {
                cc[i][j] = (c[i][j] - vol * centroid[i] * centroid[j]) * density;
            }
        }
        let tr = cc[0][0] + cc[1][1] + cc[2][2];
        let mut inertia = [[0.0f64; 3]; 3];
        for i in 0..3 {
            for j in 0..3 {
                inertia[i][j] = if i == j { tr - cc[i][j] } else { -cc[i][j] };
            }
        }
        MassProperties {
            volume: vol,
            mass,
            centroid,
            inertia,
        }
    }

    /// Whether every edge is shared by exactly two faces.
    ///
    /// This is the property the volume integral needs to mean anything: an open surface has no
    /// inside, so its "volume" is an artefact of where the origin happens to be. Returns the
    /// verdict and the number of edges that failed it.
    pub fn is_watertight(&self) -> (bool, usize) {
        let mut edges: Vec<([u32; 2], i32)> = Vec::new();
        for t in 0..self.triangles() {
            let i = t * 3;
            for k in 0..3 {
                let (a, b) = (self.indices[i + k], self.indices[i + (k + 1) % 3]);
                // Undirected key, but count the direction: a sound closed surface traverses
                // every edge exactly once each way, so the signed count cancels to zero.
                let key = if a < b { [a, b] } else { [b, a] };
                let dir = if a < b { 1 } else { -1 };
                match edges.iter_mut().find(|(k, _)| *k == key) {
                    Some((_, n)) => *n += dir,
                    None => edges.push((key, dir)),
                }
            }
        }
        let bad = edges.iter().filter(|(_, n)| *n != 0).count();
        (bad == 0, bad)
    }

    /// Faces with zero area, which draw as nothing and contribute nothing to the integral, but
    /// break many downstream convex-hull and collision routines.
    pub fn degenerate_faces(&self) -> usize {
        (0..self.triangles())
            .filter(|t| {
                let [a, b, c] = self.tri(*t);
                let n = cross(sub(b, a), sub(c, a));
                dot(n, n).sqrt() <= 1e-20
            })
            .count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Every claim below is checked against a shape whose answer is known in closed form. A mass
    // integrator that is only ever compared with itself is a mass integrator that is wrong.

    #[test]
    fn a_box_reproduces_the_textbook_inertia() {
        let m = Mesh::box_mesh([0.5, 1.0, 1.5]); // 1 x 2 x 3
        let p = m.mass_properties(2000.0);
        assert!((p.volume - 6.0).abs() < 1e-12, "{}", p.volume);
        assert!((p.mass - 12000.0).abs() < 1e-9);
        for k in 0..3 {
            assert!(p.centroid[k].abs() < 1e-12, "centred box: {:?}", p.centroid);
        }
        // I = m/12 · (sum of the squares of the OTHER two edge lengths)
        let (w, d, h) = (1.0, 2.0, 3.0);
        for (i, want) in [
            (0, p.mass * (d * d + h * h) / 12.0),
            (1, p.mass * (w * w + h * h) / 12.0),
            (2, p.mass * (w * w + d * d) / 12.0),
        ] {
            assert!(
                (p.inertia[i][i] - want).abs() / want < 1e-12,
                "I[{i}][{i}] = {} want {want}",
                p.inertia[i][i]
            );
        }
        for i in 0..3 {
            for j in 0..3 {
                if i != j {
                    assert!(
                        p.inertia[i][j].abs() < 1e-9,
                        "an axis-aligned box has no products"
                    );
                }
            }
        }
    }

    #[test]
    fn the_answer_does_not_move_when_the_shape_does() {
        // The origin is arbitrary in a divergence-theorem integral. If the tensor about the
        // centroid changes when the mesh is translated, the centroid shift is wrong.
        let a = Mesh::box_mesh([0.3, 0.4, 0.5]);
        let mut b = a.clone();
        for p in &mut b.positions {
            p[0] += 17.0;
            p[1] -= 4.5;
            p[2] += 0.25;
        }
        let (pa, pb) = (a.mass_properties(800.0), b.mass_properties(800.0));
        assert!((pa.volume - pb.volume).abs() < 1e-10);
        assert!((pb.centroid[0] - 17.0).abs() < 1e-10, "{:?}", pb.centroid);
        for i in 0..3 {
            for j in 0..3 {
                let (x, y) = (pa.inertia[i][j], pb.inertia[i][j]);
                assert!(
                    (x - y).abs() < 1e-8,
                    "I[{i}][{j}] moved with the mesh: {x} vs {y}"
                );
            }
        }
    }

    #[test]
    fn inside_out_winding_shows_up_as_negative_volume() {
        let mut m = Mesh::box_mesh([1.0, 1.0, 1.0]);
        for t in 0..m.triangles() {
            m.indices.swap(t * 3 + 1, t * 3 + 2);
        }
        assert!(m.volume() < 0.0, "reversed winding must be visible");
        assert!(m.is_watertight().0, "reversing every face is still closed");
    }

    #[test]
    fn an_open_surface_is_not_watertight_and_says_how_open() {
        let mut m = Mesh::box_mesh([1.0, 1.0, 1.0]);
        m.indices.truncate(m.indices.len() - 3); // remove one triangle: a 3-edge hole
        let (ok, bad) = m.is_watertight();
        assert!(!ok);
        assert_eq!(bad, 3, "a missing triangle leaves exactly its three edges");
    }

    #[test]
    fn a_zero_area_face_is_counted() {
        let p = [0.0, 0.0, 0.0];
        let q = [1.0, 0.0, 0.0];
        let m = Mesh::from_triangles(&[[p, q, q]]);
        assert_eq!(m.degenerate_faces(), 1);
    }

    #[test]
    fn welding_shares_the_corners_it_should() {
        let m = Mesh::box_mesh([1.0, 1.0, 1.0]);
        assert_eq!(m.triangles(), 12);
        assert_eq!(m.positions.len(), 8, "a box has eight distinct corners");
    }
}
