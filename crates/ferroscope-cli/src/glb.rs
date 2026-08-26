//! A minimal glTF 2.0 binary writer, so the demo can carry a real mesh.
//!
//! This exists to exercise the mesh path end to end with nothing downloaded: the demo builds a
//! rounded body, packs it as a `.glb`, and attaches it to the recording. The viewer then loads
//! it out of the attachment. Not a general glTF exporter, and not published as one; a hundred
//! lines that produce one valid file beats a dependency that produces every file.

/// A rounded box, as an implicit surface `|x/a|ⁿ + |y/b|ⁿ + |z/c|ⁿ = 1`.
///
/// `n = 2` is an ellipsoid and `n → ∞` is a box, so `n = 6` reads as a machined part with a
/// generous chamfer, which is exactly what a robot link looks like and is visibly not the
/// primitive box beside it.
fn rounded_box(
    half: [f64; 3],
    n: f64,
    seg: usize,
    ring: usize,
) -> (Vec<[f32; 3]>, Vec<[f32; 3]>, Vec<u32>) {
    let mut pos = Vec::new();
    let mut nrm = Vec::new();
    let mut idx = Vec::new();

    for j in 0..=ring {
        let phi = j as f64 / ring as f64 * std::f64::consts::PI;
        for i in 0..=seg {
            let th = i as f64 / seg as f64 * std::f64::consts::TAU;
            // A direction on the unit sphere, then pushed out to the implicit surface.
            let d = [phi.sin() * th.cos(), phi.sin() * th.sin(), phi.cos()];
            let s: f64 = (0..3)
                .map(|k| (d[k].abs() / half[k]).powf(n))
                .sum::<f64>()
                .powf(-1.0 / n);
            let p = [d[0] * s, d[1] * s, d[2] * s];
            // The gradient of the implicit function is the outward normal.
            let mut g = [0.0f64; 3];
            for k in 0..3 {
                let a = half[k];
                g[k] = n / a * (p[k].abs() / a).powf(n - 1.0) * p[k].signum();
            }
            let gl = (g[0] * g[0] + g[1] * g[1] + g[2] * g[2]).sqrt().max(1e-12);
            pos.push([p[0] as f32, p[1] as f32, p[2] as f32]);
            nrm.push([(g[0] / gl) as f32, (g[1] / gl) as f32, (g[2] / gl) as f32]);
        }
    }
    let w = seg + 1;
    for j in 0..ring {
        for i in 0..seg {
            let a = (j * w + i) as u32;
            let b = (j * w + i + 1) as u32;
            let c = ((j + 1) * w + i + 1) as u32;
            let d = ((j + 1) * w + i) as u32;
            idx.extend_from_slice(&[a, b, c, a, c, d]);
        }
    }
    (pos, nrm, idx)
}

/// Pack a rounded body into a `.glb`.
pub fn body_glb(half: [f64; 3], color: [f32; 4]) -> Vec<u8> {
    let (pos, nrm, idx) = rounded_box(half, 6.0, 48, 32);

    let mut bin: Vec<u8> = Vec::new();
    let pos_off = 0usize;
    for p in &pos {
        for v in p {
            bin.extend_from_slice(&v.to_le_bytes());
        }
    }
    let nrm_off = bin.len();
    for v3 in &nrm {
        for v in v3 {
            bin.extend_from_slice(&v.to_le_bytes());
        }
    }
    let idx_off = bin.len();
    for i in &idx {
        bin.extend_from_slice(&i.to_le_bytes());
    }
    while !bin.len().is_multiple_of(4) {
        bin.push(0);
    }

    let mut lo = [f32::MAX; 3];
    let mut hi = [f32::MIN; 3];
    for p in &pos {
        for k in 0..3 {
            lo[k] = lo[k].min(p[k]);
            hi[k] = hi[k].max(p[k]);
        }
    }

    // POSITION accessors are required to carry min and max; a viewer uses them for bounds.
    let json = format!(
        concat!(
            r#"{{"asset":{{"version":"2.0","generator":"ferroscope demo"}},"scene":0,"#,
            r#""scenes":[{{"nodes":[0]}}],"nodes":[{{"mesh":0,"name":"body"}}],"#,
            r#""meshes":[{{"name":"body","primitives":[{{"attributes":{{"POSITION":0,"NORMAL":1}},"indices":2,"material":0}}]}}],"#,
            r#""materials":[{{"name":"shell","pbrMetallicRoughness":{{"baseColorFactor":[{cr},{cg},{cb},{ca}],"metallicFactor":0.25,"roughnessFactor":0.45}}}}],"#,
            r#""accessors":[{{"bufferView":0,"componentType":5126,"count":{np},"type":"VEC3","min":[{lx},{ly},{lz}],"max":[{hx},{hy},{hz}]}},"#,
            r#"{{"bufferView":1,"componentType":5126,"count":{np},"type":"VEC3"}},"#,
            r#"{{"bufferView":2,"componentType":5125,"count":{ni},"type":"SCALAR"}}],"#,
            r#""bufferViews":[{{"buffer":0,"byteOffset":{po},"byteLength":{pl},"target":34962}},"#,
            r#"{{"buffer":0,"byteOffset":{no},"byteLength":{nl},"target":34962}},"#,
            r#"{{"buffer":0,"byteOffset":{io},"byteLength":{il},"target":34963}}],"#,
            r#""buffers":[{{"byteLength":{bl}}}]}}"#
        ),
        cr = color[0],
        cg = color[1],
        cb = color[2],
        ca = color[3],
        np = pos.len(),
        ni = idx.len(),
        lx = lo[0],
        ly = lo[1],
        lz = lo[2],
        hx = hi[0],
        hy = hi[1],
        hz = hi[2],
        po = pos_off,
        pl = pos.len() * 12,
        no = nrm_off,
        nl = nrm.len() * 12,
        io = idx_off,
        il = idx.len() * 4,
        bl = bin.len(),
    );
    let mut json_bytes = json.into_bytes();
    while json_bytes.len() % 4 != 0 {
        json_bytes.push(b' '); // the JSON chunk pads with spaces, the BIN chunk with zeros
    }

    let total = 12 + 8 + json_bytes.len() + 8 + bin.len();
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(b"glTF");
    out.extend_from_slice(&2u32.to_le_bytes());
    out.extend_from_slice(&(total as u32).to_le_bytes());
    out.extend_from_slice(&(json_bytes.len() as u32).to_le_bytes());
    out.extend_from_slice(b"JSON");
    out.extend_from_slice(&json_bytes);
    out.extend_from_slice(&(bin.len() as u32).to_le_bytes());
    out.extend_from_slice(b"BIN\0");
    out.extend_from_slice(&bin);
    debug_assert_eq!(out.len(), total);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_glb_container_is_well_formed() {
        let g = body_glb([0.11, 0.08, 0.06], [0.81, 0.67, 0.36, 1.0]);
        assert_eq!(&g[0..4], b"glTF");
        assert_eq!(u32::from_le_bytes(g[4..8].try_into().unwrap()), 2);
        assert_eq!(
            u32::from_le_bytes(g[8..12].try_into().unwrap()) as usize,
            g.len(),
            "the header length must be the file length"
        );
        let jlen = u32::from_le_bytes(g[12..16].try_into().unwrap()) as usize;
        assert_eq!(&g[16..20], b"JSON");
        assert_eq!(jlen % 4, 0, "chunks are 4-byte aligned");
        let json = std::str::from_utf8(&g[20..20 + jlen]).unwrap();
        assert!(json.contains("\"version\":\"2.0\""));
        assert!(json.contains("\"POSITION\""));
        let b0 = 20 + jlen;
        let blen = u32::from_le_bytes(g[b0..b0 + 4].try_into().unwrap()) as usize;
        assert_eq!(&g[b0 + 4..b0 + 8], b"BIN\0");
        assert_eq!(b0 + 8 + blen, g.len(), "the BIN chunk runs to the end");
    }

    #[test]
    fn the_surface_is_a_rounded_box_rather_than_a_sphere() {
        let (pos, nrm, idx) = rounded_box([0.11, 0.08, 0.06], 6.0, 24, 16);
        assert_eq!(pos.len(), nrm.len());
        assert_eq!(idx.len() % 3, 0);
        // A corner of a rounded box sits much further from the centre than an ellipsoid's.
        let far = pos
            .iter()
            .map(|p| (p[0] * p[0] + p[1] * p[1] + p[2] * p[2]).sqrt())
            .fold(0.0f32, f32::max);
        // Two-sided, because "not a sphere" and "not a box" are both part of the claim: the
        // surface must reach past the longest semi-axis and stop short of the true box corner.
        let longest_semi_axis = 0.11f32;
        let box_corner = (0.11f32 * 0.11 + 0.08 * 0.08 + 0.06 * 0.06).sqrt();
        assert!(
            far > longest_semi_axis * 1.10,
            "n=6 should bulge past the semi-axis {longest_semi_axis}; got {far}"
        );
        assert!(
            far < box_corner,
            "and stop short of the box corner {box_corner}; got {far}"
        );
        // Normals are unit length.
        for n in &nrm {
            let l = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
            assert!((l - 1.0).abs() < 1e-3, "normal length {l}");
        }
    }
}
