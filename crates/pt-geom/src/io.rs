//! Meshes from and to the tracer's mesh data, mesh files read through the tracer's
//! loaders (keeping their polygons), and PLY / OBJ writers.
//!
//! PLY files are binary, with an `edge` element holding the creases; OBJ files keep
//! texture coordinates per corner and material groups (`usemtl`, `mtllib`). Both carry a
//! "made by ptedit" comment.

use crate::mesh::{Mesh, NONE, Polygon};
use pt_core::mesh::{MeshData, MeshTriangle};
use std::fmt::Write as _;
use std::path::{Component, Path, PathBuf};

/// The comment the writers put in files.
pub const MADE_BY: &str = "made by ptedit";

/// A mesh from loaded mesh data: polygons put back together from their fans, corners'
/// texture coordinates, material groups and creases. Vertices no face uses are dropped.
pub fn from_mesh_data(d: &MeshData) -> Mesh {
    let tris = &d.triangles;
    let sizes: Vec<u32> = if d.polygons.iter().map(|&n| n.saturating_sub(2) as usize).sum::<usize>() == tris.len() {
        d.polygons.clone()
    } else {
        vec![3; tris.len()]
    };
    let mut polys = Vec::with_capacity(sizes.len());
    let mut t = 0;
    for n in sizes {
        let k = n.saturating_sub(2) as usize;
        let fan = &tris[t..t + k];
        t += k;
        let Some(first) = fan.first() else { continue };
        let mut verts = vec![first.v[0], first.v[1]];
        verts.extend(fan.iter().map(|tri| tri.v[2]));
        let uv = |i: u32| d.uvs.get(i as usize).copied().unwrap_or([0.0; 2]);
        let uvs = fan.iter().all(|tri| tri.uv.is_some()).then(|| {
            let mut u: Vec<[f32; 2]> = Vec::new();
            for (j, tri) in fan.iter().enumerate() {
                let c = tri.uv.unwrap_or([0; 3]);
                if j == 0 {
                    u.extend([uv(c[0]), uv(c[1])]);
                }
                u.push(uv(c[2]));
            }
            u
        });
        polys.push(Polygon { verts, uvs, material: first.group });
    }
    let mut m = Mesh::build(d.positions.clone(), polys);
    m.materials.clone_from(&d.groups);
    m.mtllibs.clone_from(&d.mtllibs);
    for &(a, b, w) in &d.creases {
        m.set_crease(a, b, w);
    }
    m.compact().0
}

/// The mesh as triangles for the tracer (polygons cut by ear clipping).
pub fn to_mesh_data(m: &Mesh) -> MeshData {
    let uvs = m.has_uvs();
    let mut d = MeshData {
        positions: m.positions.clone(),
        groups: if m.materials.is_empty() { vec![String::new()] } else { m.materials.clone() },
        mtllibs: m.mtllibs.clone(),
        creases: m.creases().into_iter().map(|((a, b), w)| (a, b, w)).collect(),
        ..MeshData::default()
    };
    for (hs, f) in m.triangles() {
        let uv = uvs.then(|| {
            hs.map(|h| {
                d.uvs.push(m.uv(h));
                d.uvs.len() as u32 - 1
            })
        });
        d.triangles.push(MeshTriangle { v: hs.map(|h| m.origin(h)), n: None, uv, group: m.face_material(f) });
    }
    d
}

/// A mesh file (OBJ, PLY, STL) as an editable mesh; material libraries become absolute.
pub fn load(path: &Path) -> Result<Mesh, String> {
    let d = pt_core::mesh::load(path)?;
    let mut m = from_mesh_data(&d);
    let dir = path.parent().unwrap_or(Path::new(""));
    m.mtllibs = m.mtllibs.iter().map(|l| dir.join(l).display().to_string()).collect();
    Ok(m)
}

/// Writes `m` to `path` as PLY or OBJ, by its extension.
pub fn save(m: &Mesh, path: &Path) -> Result<(), String> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    let bytes = match ext.as_str() {
        "ply" => ply_bytes(m),
        "obj" => obj_text(m, path.parent().unwrap_or(Path::new(""))).into_bytes(),
        _ => return Err(format!("cannot write '.{ext}' meshes (use .ply or .obj)")),
    };
    std::fs::write(path, bytes).map_err(|e| format!("cannot write '{}': {e}", path.display()))
}

/// Binary little-endian PLY: vertices, polygons and the creased edges.
pub fn ply_bytes(m: &Mesh) -> Vec<u8> {
    let creases = m.creases();
    let big = (0..m.face_count() as u32).any(|f| m.face_verts(f).len() > 255);
    let mut h = format!(
        "ply\nformat binary_little_endian 1.0\ncomment {MADE_BY}\nelement vertex {}\n\
         property float x\nproperty float y\nproperty float z\nelement face {}\n\
         property list {} int vertex_indices\n",
        m.vertex_count(),
        m.face_count(),
        if big { "int" } else { "uchar" }
    );
    if !creases.is_empty() {
        let _ = write!(
            h,
            "element edge {}\nproperty int vertex1\nproperty int vertex2\nproperty float crease\n",
            creases.len()
        );
    }
    h += "end_header\n";
    let mut out = h.into_bytes();
    for p in &m.positions {
        for c in [p.x, p.y, p.z] {
            out.extend_from_slice(&c.to_le_bytes());
        }
    }
    for f in 0..m.face_count() as u32 {
        let vs = m.face_verts(f);
        if big {
            out.extend_from_slice(&(vs.len() as i32).to_le_bytes());
        } else {
            out.push(vs.len() as u8);
        }
        for &v in vs {
            out.extend_from_slice(&(v as i32).to_le_bytes());
        }
    }
    for ((a, b), w) in creases {
        out.extend_from_slice(&(a as i32).to_le_bytes());
        out.extend_from_slice(&(b as i32).to_le_bytes());
        out.extend_from_slice(&w.to_le_bytes());
    }
    out
}

/// OBJ text: positions, texture coordinates per corner (shared where equal), polygons
/// by material group; material libraries written relative to `dir`.
pub fn obj_text(m: &Mesh, dir: &Path) -> String {
    let mut s = format!("# {MADE_BY}\n");
    for lib in &m.mtllibs {
        let _ = writeln!(s, "mtllib {}", relative(Path::new(lib), dir));
    }
    for p in &m.positions {
        let _ = writeln!(s, "v {} {} {}", p.x, p.y, p.z);
    }
    let mut uv_index: std::collections::HashMap<[u32; 2], u32> = std::collections::HashMap::new();
    let mut corner_uv = vec![NONE; m.half_edge_count()];
    if m.has_uvs() {
        for (h, slot) in corner_uv.iter_mut().enumerate() {
            let t = m.uv(h as u32);
            let n = uv_index.len() as u32;
            *slot = *uv_index.entry([t[0].to_bits(), t[1].to_bits()]).or_insert_with(|| {
                let _ = writeln!(s, "vt {} {}", t[0], t[1]);
                n
            });
        }
    }
    let mut current = NONE;
    for f in 0..m.face_count() as u32 {
        let g = m.face_material(f);
        if g != current {
            current = g;
            if let Some(name) = m.materials.get(g as usize).filter(|n| !n.is_empty()) {
                let _ = writeln!(s, "usemtl {name}");
            }
        }
        s.push('f');
        for h in m.face_half_edges(f) {
            let v = m.origin(h) + 1;
            match corner_uv[h as usize] {
                NONE => {
                    let _ = write!(s, " {v}");
                }
                t => {
                    let _ = write!(s, " {v}/{}", t + 1);
                }
            }
        }
        s.push('\n');
    }
    s
}

/// `path` relative to `dir` (with `/`), or as it is when they share no root.
pub fn relative(path: &Path, dir: &Path) -> String {
    let abs = |p: &Path| -> PathBuf {
        let p = if p.is_absolute() { p.to_path_buf() } else { std::env::current_dir().unwrap_or_default().join(p) };
        let mut out = PathBuf::new();
        for c in p.components() {
            match c {
                Component::ParentDir => {
                    out.pop();
                }
                Component::CurDir => {}
                c => out.push(c),
            }
        }
        out
    };
    let (p, d) = (abs(path), abs(dir));
    let (pc, dc): (Vec<Component<'_>>, Vec<Component<'_>>) = (p.components().collect(), d.components().collect());
    let common = pc.iter().zip(&dc).take_while(|(a, b)| a == b).count();
    if common == 0 {
        return path.display().to_string();
    }
    let ups = std::iter::repeat_n("..".to_owned(), dc.len() - common);
    let rest = pc[common..].iter().map(|c| c.as_os_str().to_string_lossy().into_owned());
    ups.chain(rest).collect::<Vec<_>>().join("/")
}

/// Whether the file at `path` was written by these writers.
pub fn made_by_us(path: &Path) -> bool {
    std::fs::read(path).is_ok_and(|b| {
        let head = &b[..b.len().min(400)];
        head.windows(MADE_BY.len()).any(|w| w == MADE_BY.as_bytes())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::primitives;
    use pt_core::math::Vec3;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("pt-geom-io-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d.join(name)
    }

    /// Writing and reading back keeps polygons, texture coordinates, groups and creases.
    #[test]
    fn files_keep_polygons_and_attributes() {
        let mut m = primitives::cube(Vec3::ZERO, Vec3::ONE);
        m.set_crease(0, 4, 1.0);
        let ply = tmp("cube.ply");
        save(&m, &ply).unwrap();
        assert!(made_by_us(&ply));
        let back = load(&ply).unwrap();
        assert_eq!(back.polygons(), m.polygons());
        assert_eq!(back.creases(), m.creases());

        let mut g = primitives::grid(2, 1, 1.0);
        g.materials = vec![String::new(), "red".into()];
        let polys: Vec<Polygon> =
            g.polygons().into_iter().enumerate().map(|(i, p)| Polygon { material: i as u32, ..p }).collect();
        let g = g.rebuild(g.positions.clone(), polys);
        let obj = tmp("grid.obj");
        save(&g, &obj).unwrap();
        let text = std::fs::read_to_string(&obj).unwrap();
        assert!(text.contains("usemtl red") && text.contains("vt 0.5 1"), "{text}");
        let back = load(&obj).unwrap();
        assert_eq!(back.polygons(), g.polygons());
        assert_eq!(back.materials, g.materials);

        // As the tracer's triangles.
        let d = to_mesh_data(&g);
        assert_eq!(d.triangles.len(), 4);
        assert_eq!(d.triangles[2].group, 1);
        std::fs::remove_file(ply).ok();
        std::fs::remove_file(obj).ok();
    }

    #[test]
    fn relative_paths() {
        let base = std::env::temp_dir();
        assert_eq!(relative(&base.join("a/b/m.mtl"), &base.join("a/c")), "../b/m.mtl");
        assert_eq!(relative(&base.join("a/m.mtl"), &base.join("a")), "m.mtl");
    }
}
