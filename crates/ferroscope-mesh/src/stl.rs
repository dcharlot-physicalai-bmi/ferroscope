//! STL in, both dialects, in `std` alone.
//!
//! STL is a triangle soup: no indices, no shared vertices, no units, no colour. Everything a
//! robot description needs is therefore *inferred* here, and what cannot be inferred is said out
//! loud rather than guessed at.

use crate::{Error, Mesh, Result};

/// Read either dialect, deciding by arithmetic rather than by the leading word.
///
/// A binary STL may legally begin with the ASCII text `solid`, so sniffing the first five bytes
/// is the classic way to misread one. The length is not ambiguous: a binary file is exactly
/// `84 + 50n` bytes for the `n` it declares at offset 80.
pub fn read(bytes: &[u8]) -> Result<Mesh> {
    if is_binary(bytes) {
        binary(bytes)
    } else {
        ascii(bytes)
    }
}

fn is_binary(b: &[u8]) -> bool {
    if b.len() < 84 {
        return false;
    }
    let n = u32::from_le_bytes([b[80], b[81], b[82], b[83]]) as usize;
    // Checked multiplication: a corrupt header claiming 4 billion triangles must not wrap to a
    // small number and make a text file look binary.
    match n.checked_mul(50).and_then(|s| s.checked_add(84)) {
        Some(want) => want == b.len(),
        None => false,
    }
}

fn binary(b: &[u8]) -> Result<Mesh> {
    let n = u32::from_le_bytes([b[80], b[81], b[82], b[83]]) as usize;
    let mut tris = Vec::with_capacity(n);
    for t in 0..n {
        let o = 84 + t * 50;
        let f = |k: usize| {
            f32::from_le_bytes([b[o + k], b[o + k + 1], b[o + k + 2], b[o + k + 3]]) as f64
        };
        // Bytes 0..12 are the stored facet normal, which is ignored: it is frequently zero,
        // frequently unnormalised, and frequently disagrees with the winding. The winding is
        // what a renderer and a volume integral both actually use, so the normal is recomputed.
        tris.push([
            [f(12), f(16), f(20)],
            [f(24), f(28), f(32)],
            [f(36), f(40), f(44)],
        ]);
    }
    Ok(Mesh::from_triangles(&tris))
}

fn ascii(b: &[u8]) -> Result<Mesh> {
    let text = std::str::from_utf8(b).map_err(|_| Error::NotStl)?;
    let mut tris: Vec<[[f64; 3]; 3]> = Vec::new();
    let mut loops: Vec<[f64; 3]> = Vec::new();
    let mut saw_solid = false;
    for (line_no, line) in text.lines().enumerate() {
        let mut w = line.split_whitespace();
        match w.next() {
            Some("solid") => saw_solid = true,
            Some("vertex") => {
                let v: Vec<f64> = w.filter_map(|t| t.parse().ok()).collect();
                if v.len() != 3 {
                    return Err(Error::Malformed {
                        line: line_no + 1,
                        what: "a vertex needs three numbers".into(),
                    });
                }
                loops.push([v[0], v[1], v[2]]);
            }
            Some("endloop") => {
                if loops.len() != 3 {
                    return Err(Error::Malformed {
                        line: line_no + 1,
                        what: format!("a facet loop has {} vertices, not 3", loops.len()),
                    });
                }
                tris.push([loops[0], loops[1], loops[2]]);
                loops.clear();
            }
            _ => {}
        }
    }
    if !saw_solid {
        return Err(Error::NotStl);
    }
    Ok(Mesh::from_triangles(&tris))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tetra() -> Vec<[[f64; 3]; 3]> {
        let a = [0.0, 0.0, 0.0];
        let b = [1.0, 0.0, 0.0];
        let c = [0.0, 1.0, 0.0];
        let d = [0.0, 0.0, 1.0];
        // Outward winding for a tetrahedron on the origin corner.
        vec![[a, c, b], [a, b, d], [a, d, c], [b, c, d]]
    }

    fn as_binary(tris: &[[[f64; 3]; 3]]) -> Vec<u8> {
        let mut v = vec![0u8; 80];
        v.extend_from_slice(&(tris.len() as u32).to_le_bytes());
        for t in tris {
            for _ in 0..3 {
                v.extend_from_slice(&0f32.to_le_bytes()); // a zero normal, as real files carry
            }
            for p in t {
                for k in p {
                    v.extend_from_slice(&(*k as f32).to_le_bytes());
                }
            }
            v.extend_from_slice(&0u16.to_le_bytes());
        }
        v
    }

    #[test]
    fn both_dialects_read_to_the_same_mesh() {
        let tris = tetra();
        let bin = read(&as_binary(&tris)).unwrap();

        let mut txt = String::from("solid t\n");
        for t in &tris {
            txt.push_str("facet normal 0 0 0\n outer loop\n");
            for p in t {
                txt.push_str(&format!("  vertex {} {} {}\n", p[0], p[1], p[2]));
            }
            txt.push_str(" endloop\nendfacet\n");
        }
        txt.push_str("endsolid t\n");
        let asc = read(txt.as_bytes()).unwrap();

        assert_eq!(bin.triangles(), 4);
        assert_eq!(asc.triangles(), 4);
        assert!((bin.volume() - asc.volume()).abs() < 1e-12);
        assert!(
            (bin.volume() - 1.0 / 6.0).abs() < 1e-12,
            "the unit corner tetrahedron has volume 1/6, got {}",
            bin.volume()
        );
    }

    #[test]
    fn a_binary_file_beginning_with_solid_is_still_read_as_binary() {
        // The classic misread: sniffing the first five bytes. The length is what decides.
        let mut v = as_binary(&tetra());
        v[..5].copy_from_slice(b"solid");
        assert!(is_binary(&v));
        assert_eq!(read(&v).unwrap().triangles(), 4);
    }

    #[test]
    fn a_corrupt_triangle_count_does_not_wrap_into_a_plausible_one() {
        let mut v = as_binary(&tetra());
        v[80..84].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(!is_binary(&v), "u32::MAX * 50 must not wrap");
        // And it is then refused as text rather than read as 4 billion triangles.
        assert!(read(&v).is_err());
    }

    #[test]
    fn a_short_facet_is_refused_with_its_line_number() {
        let e = read(b"solid t\nfacet normal 0 0 0\n outer loop\n  vertex 0 0 0\n endloop\n")
            .unwrap_err();
        match e {
            Error::Malformed { line, what } => {
                assert_eq!(line, 5);
                assert!(what.contains("1 vertices"), "{what}");
            }
            other => panic!("expected Malformed, got {other:?}"),
        }
    }
}
