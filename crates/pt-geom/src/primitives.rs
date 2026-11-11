//! The scene's primitive shapes as polygon meshes, with outward-facing polygons: what an
//! object becomes when it is turned into an editable mesh.

use crate::mesh::{Mesh, Polygon};
use pt_core::math::{Frame, PI, Vec3};

/// Unit tangents `t`, `b` with `t × b = n`, for a unit `n`.
fn basis(n: Vec3) -> (Vec3, Vec3) {
    let t = Frame::from_normal(n).t;
    (t, n.cross(t))
}

/// The box `center ± size / 2`: six quads.
pub fn cube(center: Vec3, size: Vec3) -> Mesh {
    let h = size * 0.5;
    let p = (0..8u32)
        .map(|i| {
            let s = |bit: u32| if i & bit == 0 { -1.0 } else { 1.0 };
            center + Vec3::new(s(1) * h.x, s(2) * h.y, s(4) * h.z)
        })
        .collect();
    let faces = [[0, 4, 6, 2], [1, 3, 7, 5], [0, 1, 5, 4], [2, 6, 7, 3], [0, 2, 3, 1], [4, 5, 7, 6]];
    Mesh::build(p, faces.iter().map(|f| Polygon::new(f.to_vec(), 0)).collect())
}

/// A sphere of `segments` around and `rings` from pole to pole (Y up): quads, with
/// triangle fans at the poles.
pub fn uv_sphere(center: Vec3, radius: f32, segments: u32, rings: u32) -> Mesh {
    let (n, r) = (segments.max(3), rings.max(2));
    let mut p = vec![center + Vec3::new(0.0, radius, 0.0)];
    for k in 1..r {
        let theta = PI * k as f32 / r as f32;
        for s in 0..n {
            let phi = 2.0 * PI * s as f32 / n as f32;
            p.push(center + Vec3::new(theta.sin() * phi.cos(), theta.cos(), theta.sin() * phi.sin()) * radius);
        }
    }
    p.push(center - Vec3::new(0.0, radius, 0.0));
    let bottom = p.len() as u32 - 1;
    let at = |k: u32, s: u32| 1 + (k - 1) * n + s % n;
    let mut f = Vec::new();
    for s in 0..n {
        f.push(Polygon::new(vec![0, at(1, s + 1), at(1, s)], 0));
        f.push(Polygon::new(vec![at(r - 1, s), at(r - 1, s + 1), bottom], 0));
        for k in 1..r - 1 {
            f.push(Polygon::new(vec![at(k, s), at(k, s + 1), at(k + 1, s + 1), at(k + 1, s)], 0));
        }
    }
    Mesh::build(p, f)
}

/// A tube from `base` along unit `axis`, with `segments` quads around and, when `capped`,
/// a polygon at each end.
pub fn cylinder(base: Vec3, axis: Vec3, radius: f32, height: f32, segments: u32, capped: bool) -> Mesh {
    let n = segments.max(3);
    let (t, b) = basis(axis);
    let mut p = Vec::new();
    for s in 0..n {
        let phi = 2.0 * PI * s as f32 / n as f32;
        let d = (t * phi.cos() + b * phi.sin()) * radius;
        p.extend([base + d, base + d + axis * height]);
    }
    let (bot, top) = (|s: u32| 2 * (s % n), |s: u32| 2 * (s % n) + 1);
    let mut f: Vec<Polygon> = (0..n).map(|s| Polygon::new(vec![bot(s), bot(s + 1), top(s + 1), top(s)], 0)).collect();
    if capped {
        f.push(Polygon::new((0..n).rev().map(bot).collect(), 0));
        f.push(Polygon::new((0..n).map(top).collect(), 0));
    }
    Mesh::build(p, f)
}

/// A disk facing unit `normal`: one polygon, or a ring of quads around a hole.
pub fn disk(center: Vec3, normal: Vec3, radius: f32, inner: f32, segments: u32) -> Mesh {
    let n = segments.max(3);
    let (t, b) = basis(normal);
    let ring = |r: f32, s: u32| {
        let phi = 2.0 * PI * s as f32 / n as f32;
        center + (t * phi.cos() + b * phi.sin()) * r
    };
    if inner <= 0.0 {
        return Mesh::build((0..n).map(|s| ring(radius, s)).collect(), vec![Polygon::new((0..n).collect(), 0)]);
    }
    let p = (0..n).flat_map(|s| [ring(inner, s), ring(radius, s)]).collect();
    let f = (0..n).map(|s| Polygon::new(vec![2 * s, 2 * s + 1, 2 * ((s + 1) % n) + 1, 2 * ((s + 1) % n)], 0)).collect();
    Mesh::build(p, f)
}

/// The parallelogram `origin`, `+u`, `+u+v`, `+v` (facing `u × v`), with the tracer's
/// texture coordinates.
pub fn quad(origin: Vec3, u: Vec3, v: Vec3) -> Mesh {
    let poly = Polygon {
        verts: vec![0, 1, 2, 3],
        uvs: Some(vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]]),
        material: 0,
    };
    Mesh::build(vec![origin, origin + u, origin + u + v, origin + v], vec![poly])
}

pub fn triangle(p0: Vec3, p1: Vec3, p2: Vec3) -> Mesh {
    Mesh::build(vec![p0, p1, p2], vec![Polygon::new(vec![0, 1, 2], 0)])
}

/// `nx` by `nz` quads of side `size` on the ground (facing +Y), from the origin: vertex
/// (i, j) is `i + (nx + 1) j` at (`i size`, 0, `j size`), with texture coordinates 0..1.
pub fn grid(nx: u32, nz: u32, size: f32) -> Mesh {
    let w = nx + 1;
    let p = (0..=nz).flat_map(|j| (0..=nx).map(move |i| Vec3::new(i as f32 * size, 0.0, j as f32 * size))).collect();
    let uv = |i: u32, j: u32| [i as f32 / nx.max(1) as f32, j as f32 / nz.max(1) as f32];
    let mut f = Vec::new();
    for j in 0..nz {
        for i in 0..nx {
            let verts = vec![i + w * j, i + w * (j + 1), i + 1 + w * (j + 1), i + 1 + w * j];
            let uvs = Some(vec![uv(i, j), uv(i, j + 1), uv(i + 1, j + 1), uv(i + 1, j)]);
            f.push(Polygon { verts, uvs, material: 0 });
        }
    }
    Mesh::build(p, f)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Closed shapes face outwards and have no boundary; open ones face their normal.
    #[test]
    fn primitives_face_outwards() {
        let closed = [
            cube(Vec3::new(1.0, 2.0, 3.0), Vec3::new(1.0, 2.0, 3.0)),
            uv_sphere(Vec3::ZERO, 2.0, 12, 6),
            cylinder(Vec3::ZERO, Vec3::new(0.0, 0.0, 1.0), 1.0, 2.0, 10, true),
        ];
        for m in &closed {
            let c = m.positions.iter().fold(Vec3::ZERO, |a, &p| a + p) * (1.0 / m.vertex_count() as f32);
            for f in 0..m.face_count() as u32 {
                assert!(m.face_normal(f).dot(m.face_center(f) - c) > 0.0, "face {f} faces in");
                assert!(m.face_half_edges(f).all(|h| m.twin(h).is_some()), "closed");
            }
        }
        assert_eq!(cube(Vec3::ZERO, Vec3::ONE).edges().len(), 12);
        let n = Vec3::new(0.0, 0.6, 0.8);
        for d in [disk(Vec3::ZERO, n, 1.0, 0.0, 8), disk(Vec3::ZERO, n, 1.0, 0.5, 8)] {
            assert!((0..d.face_count() as u32).all(|f| (d.face_normal(f) - n).length() < 1e-5));
        }
        let q = quad(Vec3::ZERO, Vec3::new(2.0, 0.0, 0.0), Vec3::new(0.0, 0.0, -1.0));
        assert_eq!(q.face_normal(0), Vec3::new(0.0, 1.0, 0.0));
        assert_eq!(grid(2, 2, 1.0).face_normal(0), Vec3::new(0.0, 1.0, 0.0));
    }
}
