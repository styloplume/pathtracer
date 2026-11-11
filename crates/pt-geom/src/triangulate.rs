//! Polygons into triangles by ear clipping in the polygon's plane.

use pt_core::math::{Frame, Vec3};

/// Newell's normal of a polygon (not normalized: its length is twice the area).
pub fn newell(p: &[Vec3]) -> Vec3 {
    let mut n = Vec3::ZERO;
    for (i, &a) in p.iter().enumerate() {
        let b = p[(i + 1) % p.len()];
        n += Vec3::new((a.y - b.y) * (a.z + b.z), (a.z - b.z) * (a.x + b.x), (a.x - b.x) * (a.y + b.y));
    }
    n
}

/// Triangles (corner indices) covering polygon `p`: ears clipped in the plane of the
/// polygon, so concave polygons come out right. Polygons ear clipping cannot handle
/// (self-intersecting, degenerate, very large) are fanned from their first corner.
pub fn ear_clip(p: &[Vec3]) -> Vec<[usize; 3]> {
    let n = p.len();
    let fan = |idx: &[usize]| -> Vec<[usize; 3]> { (1..idx.len() - 1).map(|i| [idx[0], idx[i], idx[i + 1]]).collect() };
    if n < 3 {
        return Vec::new();
    }
    let all: Vec<usize> = (0..n).collect();
    let normal = newell(p);
    if n == 3 || n > 512 || normal.length_squared() == 0.0 || !normal.is_finite() {
        return fan(&all);
    }
    let f = Frame::from_normal(normal.normalize());
    let q: Vec<[f32; 2]> = p.iter().map(|&v| [v.dot(f.t), v.dot(f.b)]).collect();
    let cross = |a: usize, b: usize, c: usize| {
        (q[b][0] - q[a][0]) * (q[c][1] - q[a][1]) - (q[b][1] - q[a][1]) * (q[c][0] - q[a][0])
    };
    let area: f32 = (0..n).map(|i| cross(0, i, (i + 1) % n)).sum();
    let s = if area >= 0.0 { 1.0 } else { -1.0 };
    let mut idx = all;
    let mut out = Vec::with_capacity(n - 2);
    while idx.len() > 3 {
        let m = idx.len();
        let mut clipped = false;
        for i in 0..m {
            let (a, b, c) = (idx[(i + m - 1) % m], idx[i], idx[(i + 1) % m]);
            if cross(a, b, c) * s <= 0.0 {
                continue;
            }
            // Corners at the same place as the ear's (polygons touching themselves) do not block it.
            let same =
                |i: usize, k: usize| q[i][0].to_bits() == q[k][0].to_bits() && q[i][1].to_bits() == q[k][1].to_bits();
            let blocks = idx.iter().any(|&j| {
                if j == a || j == b || j == c || same(j, a) || same(j, b) || same(j, c) {
                    return false;
                }
                cross(a, b, j) * s >= 0.0 && cross(b, c, j) * s >= 0.0 && cross(c, a, j) * s >= 0.0
            });
            if !blocks {
                out.push([a, b, c]);
                idx.remove(i);
                clipped = true;
                break;
            }
        }
        if !clipped {
            out.extend(fan(&idx));
            return out;
        }
    }
    out.push([idx[0], idx[1], idx[2]]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area(p: &[Vec3], tris: &[[usize; 3]]) -> f32 {
        tris.iter().map(|t| (p[t[1]] - p[t[0]]).cross(p[t[2]] - p[t[0]]).length() * 0.5).sum()
    }

    #[test]
    fn concave_polygons_are_covered_once() {
        // An L shape of area 3, in the XZ plane, both windings.
        let mut l: Vec<Vec3> = [(0.0, 0.0), (2.0, 0.0), (2.0, 1.0), (1.0, 1.0), (1.0, 2.0), (0.0, 2.0)]
            .iter()
            .map(|&(x, z)| Vec3::new(x, 0.0, z))
            .collect();
        for _ in 0..2 {
            let t = ear_clip(&l);
            assert_eq!(t.len(), 4);
            assert!((area(&l, &t) - 3.0).abs() < 1e-5, "{t:?}");
            // Every triangle turns the way the polygon does.
            let n = newell(&l);
            for tri in &t {
                assert!((l[tri[1]] - l[tri[0]]).cross(l[tri[2]] - l[tri[0]]).dot(n) > 0.0);
            }
            l.reverse();
        }
    }
}
