//! Where the geometry, the material table and the declared inertial meet.
//!
//! A robot description states a link's mass and inertia tensor as bare numbers. Nothing in the
//! usual toolchain asks where they came from or whether they are consistent with the shape the
//! same file declares — so a hand-typed tensor, a placeholder from a CAD export, and a value
//! measured on a scale are indistinguishable once written down, and all three simulate happily.
//!
//! This crate closes that loop. [`ferroscope_mesh`] integrates the true mass properties off the
//! triangles; [CadFuture]'s LUT supplies a density with a citation; and [`check_inertial`]
//! compares the two, so a description that disagrees with its own geometry says so before
//! anything is simulated.
//!
//! [CadFuture]: https://github.com/dcharlot-physicalai-bmi/cad-future
//!
//! # The tier is part of the answer
//!
//! CadFuture resolves every engineering query at the cheapest tier that can answer it — LUT,
//! then closed-form formula, then solver, then a model. Which tier answered is not an
//! implementation detail: it is the difference between a number that cost picojoules and one
//! that cost joules, and between a number with a citation and one with a residual. Every value
//! this crate returns carries its [`Provenance`], and that provenance is written into the
//! recording, because a quantity whose origin is not in the file is a quantity nobody can audit.

use ferroscope_mesh::{MassProperties, Mesh};
use physical_units::Density;

pub use physical_cascade::Tier;
pub use physical_lut::materials::{Material, MaterialCategory};

/// Where a value came from, and what it cited.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Provenance {
    pub tier: Tier,
    /// What the table cites, verbatim from the material record.
    pub source: &'static str,
    /// The table entry that answered, so the lookup can be repeated by hand.
    pub key: &'static str,
}

impl Provenance {
    /// A short label for the recording: the tier, then what it cited.
    pub fn label(&self) -> String {
        format!("{:?}/{}/{}", self.tier, self.key, self.source)
    }
}

/// A value together with where it came from.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Resolved<T> {
    pub value: T,
    pub provenance: Provenance,
}

/// Look up a material by its id, e.g. `"6061-T6"`, `"PLA"`, `"Ti-6Al-4V"`.
pub fn material(id: &str) -> Option<&'static Material> {
    physical_lut::materials::lookup(id)
}

/// Pairs of spellings that name the same thing on different sides of an ocean.
///
/// The table is written in US English. A caller — very often a model, and very often one
/// answering someone who wrote the word the way IUPAC and most of the world write it — asks for
/// "aluminium" and gets nothing back from a table holding forty aluminium alloys. That is a
/// wrong answer dressed as an empty one, which is the worst kind.
const SPELLINGS: &[(&str, &str)] = &[
    ("aluminium", "aluminum"),
    ("sulphur", "sulfur"),
    ("fibre", "fiber"),
    ("magnesium alloy", "magnesium"),
    ("titanium alloy", "titanium"),
    ("polythene", "polyethylene"),
    ("perspex", "acrylic"),
    ("plexiglas", "acrylic"),
];

/// Every spelling of a query worth trying, the original first.
fn variants(query: &str) -> Vec<String> {
    let lower = query.to_lowercase();
    let mut out = vec![lower.clone()];
    for (a, b) in SPELLINGS {
        for (from, to) in [(a, b), (b, a)] {
            if lower.contains(from) {
                let v = lower.replace(from, to);
                if !out.contains(&v) {
                    out.push(v);
                }
            }
        }
    }
    out
}

/// Free-text search across the table, for when the caller has a name rather than an id.
///
/// Tries the query as written first, then the same query under the other common spelling, so
/// "aluminium" and "aluminum" reach the same forty alloys. Results keep first-seen order and no
/// material appears twice.
pub fn search(query: &str) -> impl Iterator<Item = &'static Material> {
    let mut out: Vec<&'static Material> = Vec::new();
    for v in variants(query) {
        for m in physical_lut::materials::search(&v) {
            if !out.iter().any(|e| e.id == m.id) {
                out.push(m);
            }
        }
    }
    out.into_iter()
}

/// How many materials the table carries.
pub fn material_count() -> usize {
    physical_lut::materials::count()
}

/// Density for a named material, resolved through the cascade so the tier is recorded.
pub fn density(material_id: &str) -> Option<Resolved<Density>> {
    let m = material(material_id)?;
    Some(Resolved {
        value: m.density,
        // A material property is a table read by construction. Asserting the tier from the
        // cascade rather than assuming it keeps this honest if the cascade ever moves a
        // property to a computed tier.
        provenance: Provenance {
            tier: physical_cascade::density(material_id)
                .map(|r| r.tier)
                .unwrap_or(Tier::Unresolved),
            source: m.source,
            key: m.id,
        },
    })
}

/// The mass properties a mesh would have if it were solid and made of the named material.
///
/// This is the number a CAD package would give you, computed the same way: an exact integral
/// over the geometry, not a bounding-box estimate. The density is the only input that is looked
/// up rather than computed, and it arrives with its citation attached.
pub fn mass_properties(mesh: &Mesh, material_id: &str) -> Option<Resolved<MassProperties>> {
    let d = density(material_id)?;
    Some(Resolved {
        value: mesh.mass_properties(d.value.value()),
        provenance: d.provenance,
    })
}

/// One disagreement between a description and the geometry it describes.
#[derive(Clone, Debug, PartialEq)]
pub struct Finding {
    pub kind: &'static str,
    pub detail: String,
    /// True when this is a defect rather than an observation worth printing.
    pub fails: bool,
}

/// How far a declared value may sit from the computed one before it is called a disagreement.
///
/// Generous on purpose. A real link is not solid — it has pockets, ribs, bosses and a wall
/// thickness — so its mass is legitimately *below* what a solid mesh of the same outline would
/// weigh, often by a lot. What this catches is the other direction and the gross case: a link
/// heavier than solid stock, or a tensor off by an order of magnitude.
#[derive(Clone, Copy, Debug)]
pub struct Tolerance {
    /// A declared mass above `solid × this` is impossible for a part of that shape.
    pub max_mass_fraction: f64,
    /// Below `solid × this`, the part would be mostly air; worth saying, not a defect.
    pub thin_mass_fraction: f64,
    /// A principal moment off by more than this factor, once mass is accounted for.
    pub inertia_factor: f64,
}

impl Default for Tolerance {
    fn default() -> Self {
        Tolerance {
            // Nothing machined from one material can outweigh the solid envelope of its own
            // outline. A little headroom for fasteners and inserts of a denser material.
            max_mass_fraction: 1.15,
            thin_mass_fraction: 0.15,
            // Shape, not scale: the tensor is normalised by mass before comparison, so this is
            // asking whether the mass is distributed anything like the geometry says.
            inertia_factor: 3.0,
        }
    }
}

/// Compare a declared `<inertial>` against the geometry and material it claims to describe.
///
/// `declared_inertia` is in URDF order: `ixx ixy ixz iyy iyz izz`.
///
/// The comparison is deliberately scale-free where it can be. Mass is checked against the solid
/// envelope, which is a hard upper bound for a homogeneous part. The tensor is then normalised
/// by each side's own mass before comparison, so a hollow part with the right *shape* passes
/// while a tensor that describes a different shape entirely does not.
pub fn check_inertial(
    mesh: &Mesh,
    material_id: &str,
    declared_mass: f64,
    declared_inertia: [f64; 6],
    tol: Tolerance,
) -> Vec<Finding> {
    let mut out = Vec::new();
    let Some(solid) = mass_properties(mesh, material_id) else {
        out.push(Finding {
            kind: "unknown-material",
            detail: format!(
                "{material_id:?} is not in the table of {} materials, so the declared inertial \
                 could not be checked against the geometry",
                material_count()
            ),
            fails: false,
        });
        return out;
    };
    let p = solid.value;

    let (watertight, open_edges) = mesh.is_watertight();
    if !watertight {
        // Said first and loudly: every number below is an integral over a surface that does not
        // close, so they are reported as context rather than withheld, but they are not evidence.
        out.push(Finding {
            kind: "open-mesh",
            detail: format!(
                "the mesh is not closed ({open_edges} unmatched edge(s)), so its volume is not \
                 well defined and the comparison below is indicative only"
            ),
            fails: false,
        });
    }
    if p.volume <= 0.0 {
        out.push(Finding {
            kind: "inverted-mesh",
            detail: format!(
                "the mesh encloses a signed volume of {:.6e} m³, which means its faces wind \
                 inward; no mass property computed from it is meaningful",
                p.volume
            ),
            fails: true,
        });
        return out;
    }

    let frac = declared_mass / p.mass;
    if declared_mass > 0.0 && frac > tol.max_mass_fraction {
        out.push(Finding {
            kind: "heavier-than-solid",
            detail: format!(
                "declared {declared_mass} kg is {frac:.2}x the {:.4} kg this shape weighs solid \
                 in {} ({:.1} cm³ at {:.0} kg/m³, {}): a homogeneous part cannot outweigh its \
                 own envelope",
                p.mass,
                solid.provenance.key,
                p.volume * 1e6,
                p.mass / p.volume,
                solid.provenance.source
            ),
            fails: true,
        });
    } else if declared_mass > 0.0 && frac < tol.thin_mass_fraction {
        out.push(Finding {
            kind: "much-lighter-than-solid",
            detail: format!(
                "declared {declared_mass} kg is {:.1}% of the {:.4} kg this shape weighs solid \
                 in {}: consistent with a thin-walled or heavily pocketed part, and worth \
                 confirming that is what it is",
                frac * 100.0,
                p.mass,
                solid.provenance.key
            ),
            fails: false,
        });
    }

    // Shape comparison, mass divided out of both sides.
    if declared_mass > 0.0 {
        let want = p.urdf_inertia();
        let scale = p.mass / declared_mass;
        for (k, name) in [(0usize, "ixx"), (3, "iyy"), (5, "izz")] {
            let d = declared_inertia[k] * scale;
            let w = want[k];
            if w <= 0.0 || d <= 0.0 {
                continue;
            }
            let ratio = if d > w { d / w } else { w / d };
            if ratio > tol.inertia_factor {
                out.push(Finding {
                    kind: "inertia-unlike-geometry",
                    detail: format!(
                        "{name} is {ratio:.1}x away from the geometry once mass is divided out \
                         ({d:.3e} vs {w:.3e} kg·m²): this tensor does not describe this shape"
                    ),
                    fails: true,
                });
            }
        }
    }

    if out.iter().all(|f| !f.fails) {
        out.push(Finding {
            kind: "consistent",
            detail: format!(
                "declared mass is {:.0}% of solid {} ({:.1} cm³, {} via {:?}), and the tensor \
                 matches the geometry's shape",
                frac * 100.0,
                solid.provenance.key,
                p.volume * 1e6,
                solid.provenance.source,
                solid.provenance.tier
            ),
            fails: false,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // A 40 x 20 x 35 mm block, about the size of the SO-101's servo.
    fn block() -> Mesh {
        Mesh::box_mesh([0.020, 0.010, 0.0175])
    }

    #[test]
    fn the_table_is_present_and_cites_its_sources() {
        assert!(
            material_count() > 300,
            "expected a real table, got {}",
            material_count()
        );
        let al = material("6061-T6").expect("6061-T6 is a standard alloy");
        assert!(
            !al.source.is_empty(),
            "a material with no citation is a guess"
        );
        assert!(
            (al.density.value() - 2700.0).abs() < 100.0,
            "aluminium is about 2700 kg/m³, table says {}",
            al.density.value()
        );
    }

    #[test]
    fn both_spellings_of_aluminium_reach_the_same_alloys() {
        // The table is written in US English. A query in the spelling IUPAC uses must not come
        // back empty from a table holding dozens of the thing asked for.
        let uk: Vec<_> = search("aluminium").map(|m| m.id).collect();
        let us: Vec<_> = search("aluminum").map(|m| m.id).collect();
        assert!(
            uk.len() > 10,
            "expected many aluminium alloys, got {}",
            uk.len()
        );
        assert_eq!(uk, us, "the two spellings must reach the same set");
    }

    #[test]
    fn a_search_never_returns_the_same_material_twice() {
        // Trying several spellings means several passes over one table, and a caller shown the
        // same alloy twice cannot tell that from two genuinely similar ones.
        let hits: Vec<_> = search("aluminium").map(|m| m.id).collect();
        let mut sorted = hits.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), hits.len(), "duplicates in {hits:?}");
    }

    #[test]
    fn an_ordinary_query_still_works_and_is_not_reordered() {
        let steel: Vec<_> = search("steel").map(|m| m.id).collect();
        assert!(steel.len() > 5, "got {steel:?}");
        let direct: Vec<_> = physical_lut::materials::search("steel")
            .map(|m| m.id)
            .collect();
        assert_eq!(
            steel, direct,
            "a query with no variant must pass straight through"
        );
    }

    #[test]
    fn density_arrives_from_the_lut_tier_not_a_solver() {
        let d = density("6061-T6").unwrap();
        assert_eq!(
            d.provenance.tier,
            Tier::Lut,
            "a material property must be a table read, not a computation"
        );
        assert_eq!(d.provenance.key, "6061-T6");
    }

    #[test]
    fn a_solid_block_weighs_what_the_table_says_it_should() {
        let p = mass_properties(&block(), "6061-T6").unwrap().value;
        // 40 x 20 x 35 mm = 28 cm³; aluminium at ~2700 kg/m³ is about 75.6 g.
        assert!((p.volume - 2.8e-5).abs() < 1e-9, "{} m³", p.volume);
        assert!((p.mass - 0.0756).abs() < 0.004, "{} kg", p.mass);
    }

    #[test]
    fn a_link_heavier_than_solid_stock_is_refused() {
        // 1 kg of aluminium in a 28 cm³ envelope would need a density of 35,700 kg/m³.
        let f = check_inertial(
            &block(),
            "6061-T6",
            1.0,
            [1e-4, 0.0, 0.0, 1e-4, 0.0, 1e-4],
            Tolerance::default(),
        );
        let hit = f
            .iter()
            .find(|f| f.kind == "heavier-than-solid")
            .expect("must catch it");
        assert!(hit.fails);
        assert!(hit.detail.contains("cannot outweigh"), "{}", hit.detail);
    }

    #[test]
    fn a_plausible_pocketed_part_passes_and_says_why() {
        // 60 % of solid: an ordinary machined part with pockets.
        let m = block();
        let solid = mass_properties(&m, "6061-T6").unwrap().value;
        let declared = solid.mass * 0.6;
        // The right tensor for that mass: the geometry's shape scaled to the lighter part.
        let want = solid.urdf_inertia();
        let s = 0.6;
        let f = check_inertial(
            &m,
            "6061-T6",
            declared,
            [
                want[0] * s,
                want[1] * s,
                want[2] * s,
                want[3] * s,
                want[4] * s,
                want[5] * s,
            ],
            Tolerance::default(),
        );
        assert!(f.iter().all(|f| !f.fails), "{f:?}");
        assert!(f.iter().any(|f| f.kind == "consistent"), "{f:?}");
    }

    #[test]
    fn a_tensor_describing_a_different_shape_is_caught_even_at_the_right_mass() {
        // The mass is exactly right; the tensor is a long rod's, not a block's. Mass alone
        // cannot catch this, which is why the shape is compared separately.
        let m = block();
        let solid = mass_properties(&m, "6061-T6").unwrap().value;
        let f = check_inertial(
            &m,
            "6061-T6",
            solid.mass,
            [1e-2, 0.0, 0.0, 1e-8, 0.0, 1e-8],
            Tolerance::default(),
        );
        assert!(
            f.iter()
                .any(|f| f.kind == "inertia-unlike-geometry" && f.fails),
            "a wrong shape at the right mass must still be caught: {f:?}"
        );
    }

    #[test]
    fn an_unknown_material_is_reported_rather_than_assumed() {
        let f = check_inertial(&block(), "unobtanium", 0.1, [1e-5; 6], Tolerance::default());
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].kind, "unknown-material");
        assert!(
            !f[0].fails,
            "not knowing a material is not the description's fault"
        );
    }

    #[test]
    fn an_open_mesh_is_flagged_before_its_numbers_are_quoted() {
        let mut m = block();
        m.indices.truncate(m.indices.len() - 3);
        let f = check_inertial(&m, "6061-T6", 0.05, [1e-5; 6], Tolerance::default());
        assert_eq!(f[0].kind, "open-mesh", "the caveat must come first: {f:?}");
    }
}
