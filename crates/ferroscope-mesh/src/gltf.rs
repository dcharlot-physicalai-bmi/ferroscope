//! glTF 2.0 binary out.
//!
//! Enough of the format to carry a robot part: positions, normals, indices, one PBR material.
//! Written by hand because the alternative is a general glTF stack for four accessors, and this
//! crate's whole claim is that it adds nothing to your dependency tree.

use crate::Mesh;

/// Per-vertex normals, area-weighted from the faces that meet at each vertex.
///
/// Area weighting rather than a plain average: a long thin triangle and a large one meeting at a
/// corner should not get equal say, and the cross product supplies the weight for free because
/// its magnitude *is* twice the face area.
fn vertex_normals(m: &Mesh) -> Vec<[f32; 3]> {
    let mut n = vec![[0.0f64; 3]; m.positions.len()];
    for t in 0..m.triangles() {
        let (i, j, k) = (
            m.indices[t * 3] as usize,
            m.indices[t * 3 + 1] as usize,
            m.indices[t * 3 + 2] as usize,
        );
        let (a, b, c) = (m.positions[i], m.positions[j], m.positions[k]);
        let u = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
        let v = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
        let f = [
            u[1] * v[2] - u[2] * v[1],
            u[2] * v[0] - u[0] * v[2],
            u[0] * v[1] - u[1] * v[0],
        ];
        for idx in [i, j, k] {
            for d in 0..3 {
                n[idx][d] += f[d];
            }
        }
    }
    n.iter()
        .map(|v| {
            let l = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
            if l > 1e-20 {
                [(v[0] / l) as f32, (v[1] / l) as f32, (v[2] / l) as f32]
            } else {
                // A vertex used only by degenerate faces has no defined normal. Up is a
                // renderable answer, and `degenerate_faces()` is where the real story is told.
                [0.0, 0.0, 1.0]
            }
        })
        .collect()
}

/// Pack a mesh into a `.glb` with one material.
///
/// `name` lands in the node and mesh names, which is what a viewer shows in its scene tree, so
/// pass the link name rather than the filename.
pub fn write(m: &Mesh, name: &str, color: [f32; 4]) -> Vec<u8> {
    let nrm = vertex_normals(m);

    let mut bin: Vec<u8> = Vec::new();
    let pos_off = 0usize;
    for p in &m.positions {
        for v in p {
            bin.extend_from_slice(&(*v as f32).to_le_bytes());
        }
    }
    let nrm_off = bin.len();
    for v3 in &nrm {
        for v in v3 {
            bin.extend_from_slice(&v.to_le_bytes());
        }
    }
    let idx_off = bin.len();
    for i in &m.indices {
        bin.extend_from_slice(&i.to_le_bytes());
    }
    while !bin.len().is_multiple_of(4) {
        bin.push(0);
    }

    let (lo, hi) = m.bounds().unwrap_or(([0.0; 3], [0.0; 3]));
    // A glTF name is a JSON string, and a link name is not required to be one.
    let safe: String = name
        .chars()
        .map(|c| if c == '"' || c == '\\' { '_' } else { c })
        .filter(|c| !c.is_control())
        .collect();

    let json = format!(
        concat!(
            r#"{{"asset":{{"version":"2.0","generator":"ferroscope-mesh"}},"scene":0,"#,
            r#""scenes":[{{"nodes":[0]}}],"nodes":[{{"mesh":0,"name":"{nm}"}}],"#,
            r#""meshes":[{{"name":"{nm}","primitives":[{{"attributes":{{"POSITION":0,"NORMAL":1}},"indices":2,"material":0}}]}}],"#,
            r#""materials":[{{"name":"{nm}_mat","pbrMetallicRoughness":{{"baseColorFactor":[{cr},{cg},{cb},{ca}],"metallicFactor":0.2,"roughnessFactor":0.55}}}}],"#,
            r#""accessors":[{{"bufferView":0,"componentType":5126,"count":{np},"type":"VEC3","min":[{lx},{ly},{lz}],"max":[{hx},{hy},{hz}]}},"#,
            r#"{{"bufferView":1,"componentType":5126,"count":{np},"type":"VEC3"}},"#,
            r#"{{"bufferView":2,"componentType":5125,"count":{ni},"type":"SCALAR"}}],"#,
            r#""bufferViews":[{{"buffer":0,"byteOffset":{po},"byteLength":{pl},"target":34962}},"#,
            r#"{{"buffer":0,"byteOffset":{no},"byteLength":{nl},"target":34962}},"#,
            r#"{{"buffer":0,"byteOffset":{io},"byteLength":{il},"target":34963}}],"#,
            r#""buffers":[{{"byteLength":{bl}}}]}}"#
        ),
        nm = safe,
        cr = color[0],
        cg = color[1],
        cb = color[2],
        ca = color[3],
        np = m.positions.len(),
        ni = m.indices.len(),
        lx = lo[0] as f32,
        ly = lo[1] as f32,
        lz = lo[2] as f32,
        hx = hi[0] as f32,
        hy = hi[1] as f32,
        hz = hi[2] as f32,
        po = pos_off,
        pl = m.positions.len() * 12,
        no = nrm_off,
        nl = nrm.len() * 12,
        io = idx_off,
        il = m.indices.len() * 4,
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

    fn parse_chunks(g: &[u8]) -> (String, usize) {
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
        let json = std::str::from_utf8(&g[20..20 + jlen]).unwrap().to_string();
        let b0 = 20 + jlen;
        let blen = u32::from_le_bytes(g[b0..b0 + 4].try_into().unwrap()) as usize;
        assert_eq!(&g[b0 + 4..b0 + 8], b"BIN\0");
        assert_eq!(b0 + 8 + blen, g.len(), "the BIN chunk runs to the end");
        (json, blen)
    }

    #[test]
    fn the_container_is_well_formed_and_declares_its_own_bounds() {
        let m = Mesh::box_mesh([0.1, 0.2, 0.3]);
        let (json, _) = parse_chunks(&write(&m, "upper_arm", [0.8, 0.7, 0.3, 1.0]));
        assert!(json.contains(r#""name":"upper_arm""#), "{json}");
        assert!(json.contains(r#""min":[-0.1,-0.2,-0.3]"#), "{json}");
        assert!(json.contains(r#""max":[0.1,0.2,0.3]"#), "{json}");
    }

    #[test]
    fn a_name_that_would_break_the_json_is_neutralised() {
        let m = Mesh::box_mesh([1.0, 1.0, 1.0]);
        let (json, _) = parse_chunks(&write(&m, "we\"ird\\link", [1.0, 1.0, 1.0, 1.0]));
        // The point is that the document still parses as one JSON object with the right shape;
        // a raw quote would have ended the string early and the chunk test above would pass
        // while the file was garbage to every reader.
        assert!(json.contains(r#""name":"we_ird_link""#), "{json}");
    }

    #[test]
    fn normals_point_outward_on_a_box() {
        let m = Mesh::box_mesh([1.0, 1.0, 1.0]);
        let n = vertex_normals(&m);
        for (p, v) in m.positions.iter().zip(&n) {
            // Every corner of a centred box has its normal in the same octant as the corner.
            let d = p[0] * v[0] as f64 + p[1] * v[1] as f64 + p[2] * v[2] as f64;
            assert!(d > 0.0, "normal {v:?} points inward at corner {p:?}");
        }
    }
}
