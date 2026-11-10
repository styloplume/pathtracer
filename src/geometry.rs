//! Geometric primitives and their ray intersection routines.

use crate::math::{Transform, Vec3};

#[derive(Clone, Copy, Debug)]
pub struct Aabb {
    pub min: Vec3,
    pub max: Vec3,
}

impl Aabb {
    pub const EMPTY: Aabb = Aabb { min: Vec3::splat(f32::INFINITY), max: Vec3::splat(f32::NEG_INFINITY) };

    #[inline]
    pub fn grow_point(&mut self, p: Vec3) {
        self.min = self.min.min(p);
        self.max = self.max.max(p);
    }
    #[inline]
    pub fn grow(&mut self, b: &Aabb) {
        self.min = self.min.min(b.min);
        self.max = self.max.max(b.max);
    }
    #[inline]
    pub fn centroid(&self) -> Vec3 {
        (self.min + self.max) * 0.5
    }
    #[inline]
    pub fn surface_area(&self) -> f32 {
        let d = self.max - self.min;
        if d.x < 0.0 || d.y < 0.0 || d.z < 0.0 {
            return 0.0;
        }
        2.0 * (d.x * d.y + d.y * d.z + d.z * d.x)
    }
    pub fn is_empty(&self) -> bool {
        !(self.min.x <= self.max.x && self.min.y <= self.max.y && self.min.z <= self.max.z)
    }
    /// Bounds of the transformed box (of its eight corners).
    pub fn transformed(&self, xf: &Transform) -> Aabb {
        let mut b = Aabb::EMPTY;
        if self.is_empty() {
            return b;
        }
        for i in 0..8 {
            let pick = |bit: usize, lo: f32, hi: f32| if i & bit == 0 { lo } else { hi };
            let c = Vec3::new(
                pick(1, self.min.x, self.max.x),
                pick(2, self.min.y, self.max.y),
                pick(4, self.min.z, self.max.z),
            );
            b.grow_point(xf.point(c));
        }
        b
    }
    /// Slab test. Returns the entry distance, or infinity on a miss.
    #[cfg(test)]
    pub fn hit(&self, o: Vec3, inv_d: Vec3, t_max: f32) -> f32 {
        let t0 = (self.min - o) * inv_d;
        let t1 = (self.max - o) * inv_d;
        let near = t0.min(t1);
        let far = t0.max(t1);
        let enter = near.x.max(near.y).max(near.z).max(0.0);
        let exit = far.x.min(far.y).min(far.z).min(t_max);
        if enter <= exit { enter } else { f32::INFINITY }
    }
}

pub const NO_NORMALS: u32 = u32::MAX;
pub const NO_UVS: u32 = u32::MAX;

/// A triangle referencing the scene's global vertex arrays.
#[derive(Clone, Copy, Debug)]
pub struct Triangle {
    pub v: [u32; 3],
    /// Indices into the normal array, or `[NO_NORMALS; 3]` for flat shading.
    pub n: [u32; 3],
    /// Indices into the UV array, or `[NO_UVS; 3]` for the default (0,0) (1,0) (0,1).
    pub uv: [u32; 3],
    pub material: u32,
}

impl Triangle {
    #[inline]
    pub fn has_normals(&self) -> bool {
        self.n[0] != NO_NORMALS
    }
    #[inline]
    pub fn has_uvs(&self) -> bool {
        self.uv[0] != NO_UVS
    }
}

/// Per-ray setup for watertight ray/triangle intersection (Woop, Benthin & Wald 2013):
/// the ray is turned into the +Z axis by a permutation and a shear, so the edge tests
/// of neighbouring triangles evaluate the same expressions and no ray slips between them.
#[derive(Clone, Copy, Debug)]
pub struct TriRay {
    o: Vec3,
    kx: usize,
    ky: usize,
    kz: usize,
    sx: f32,
    sy: f32,
    sz: f32,
}

impl TriRay {
    #[inline]
    pub fn new(o: Vec3, d: Vec3) -> TriRay {
        let a = d.abs();
        let kz = if a.x > a.y {
            if a.x > a.z { 0 } else { 2 }
        } else if a.y > a.z {
            1
        } else {
            2
        };
        let (mut kx, mut ky) = ((kz + 1) % 3, (kz + 2) % 3);
        // Keep the winding: swap the other axes if the ray points down the chosen one.
        if d[kz] < 0.0 {
            std::mem::swap(&mut kx, &mut ky);
        }
        let sz = 1.0 / d[kz];
        TriRay { o, kx, ky, kz, sx: d[kx] * sz, sy: d[ky] * sz, sz }
    }

    /// Returns (t, b1, b2) with t in (0, `t_max`), where p = (1-b1-b2) p0 + b1 p1 + b2 p2.
    #[inline]
    pub fn intersect(&self, p0: Vec3, p1: Vec3, p2: Vec3, t_max: f32) -> Option<(f32, f32, f32)> {
        let (kx, ky, kz) = (self.kx, self.ky, self.kz);
        let (a, b, c) = (p0 - self.o, p1 - self.o, p2 - self.o);
        let (ax, ay) = (a[kx] - self.sx * a[kz], a[ky] - self.sy * a[kz]);
        let (bx, by) = (b[kx] - self.sx * b[kz], b[ky] - self.sy * b[kz]);
        let (cx, cy) = (c[kx] - self.sx * c[kz], c[ky] - self.sy * c[kz]);
        let mut u = cx * by - cy * bx;
        let mut v = ax * cy - ay * cx;
        let mut w = bx * ay - by * ax;
        // Exactly on an edge in single precision: decide in double precision.
        if u == 0.0 || v == 0.0 || w == 0.0 {
            let f = f64::from;
            u = (f(cx) * f(by) - f(cy) * f(bx)) as f32;
            v = (f(ax) * f(cy) - f(ay) * f(cx)) as f32;
            w = (f(bx) * f(ay) - f(by) * f(ax)) as f32;
        }
        if (u < 0.0 || v < 0.0 || w < 0.0) && (u > 0.0 || v > 0.0 || w > 0.0) {
            return None;
        }
        let det = u + v + w;
        if det == 0.0 || !det.is_finite() {
            return None;
        }
        let t = u * (self.sz * a[kz]) + v * (self.sz * b[kz]) + w * (self.sz * c[kz]);
        // t / det must lie in (0, t_max); compare without dividing.
        let outside = if det > 0.0 { t <= 0.0 || t >= t_max * det } else { t >= 0.0 || t <= t_max * det };
        if outside {
            return None;
        }
        let inv = 1.0 / det;
        Some((t * inv, v * inv, w * inv))
    }
}

/// Watertight ray/triangle test without per-ray setup reuse.
#[cfg(test)]
pub fn intersect_triangle(o: Vec3, d: Vec3, p0: Vec3, p1: Vec3, p2: Vec3, t_max: f32) -> Option<(f32, f32, f32)> {
    TriRay::new(o, d).intersect(p0, p1, p2, t_max)
}

/// Both ray parameters where the ray crosses the sphere, ordered (may be negative),
/// with the numerically robust discriminant from Ray Tracing Gems ch. 7.
/// `d` must be normalized.
#[inline]
pub fn sphere_roots(o: Vec3, d: Vec3, center: Vec3, radius: f32) -> Option<(f32, f32)> {
    let oc = o - center;
    let b = oc.dot(d);
    let l = oc - d * b;
    let disc = radius * radius - l.dot(l);
    if disc < 0.0 {
        return None;
    }
    let c = oc.dot(oc) - radius * radius;
    let sq = disc.sqrt();
    // Stable root computation.
    let q = if b > 0.0 { -b - sq } else { -b + sq };
    let (t0, t1) = (c / q, q);
    Some(if t0 > t1 { (t1, t0) } else { (t0, t1) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sphere_hit_from_outside_and_inside() {
        let c = Vec3::new(0.0, 0.0, -5.0);
        let (t0, t1) = sphere_roots(Vec3::ZERO, Vec3::new(0.0, 0.0, -1.0), c, 1.0).unwrap();
        assert!((t0 - 4.0).abs() < 1e-5 && (t1 - 6.0).abs() < 1e-5);
        let (t0, t1) = sphere_roots(c, Vec3::new(0.0, 0.0, -1.0), c, 1.0).unwrap();
        assert!((t0 + 1.0).abs() < 1e-5 && (t1 - 1.0).abs() < 1e-5);
        assert!(sphere_roots(Vec3::new(0.0, 3.0, 0.0), Vec3::new(0.0, 0.0, -1.0), c, 1.0).is_none());
    }

    #[test]
    fn triangle_hit_and_miss() {
        let (p0, p1, p2) = (Vec3::new(-1.0, -1.0, 0.0), Vec3::new(1.0, -1.0, 0.0), Vec3::new(0.0, 1.0, 0.0));
        let o = Vec3::new(0.0, 0.0, 2.0);
        let (t, b1, b2) = intersect_triangle(o, Vec3::new(0.0, 0.0, -1.0), p0, p1, p2, f32::INFINITY).unwrap();
        assert!((t - 2.0).abs() < 1e-5);
        // Barycentrics reproduce the hit point (0, 0, 0).
        let p = p0 * (1.0 - b1 - b2) + p1 * b1 + p2 * b2;
        assert!(p.length() < 1e-5, "{p:?}");
        assert!(intersect_triangle(o, Vec3::new(0.0, 0.0, 1.0), p0, p1, p2, f32::INFINITY).is_none());
        assert!(intersect_triangle(o, Vec3::new(0.0, 0.0, -1.0), p0, p1, p2, 1.5).is_none());
        // Both windings hit.
        assert!(intersect_triangle(o, Vec3::new(0.0, 0.0, -1.0), p0, p2, p1, f32::INFINITY).is_some());
    }

    /// Rays through the shared vertex and edges of a triangle fan never slip through.
    #[test]
    fn triangle_fan_is_watertight() {
        let n = 7;
        let center = Vec3::new(0.3, 0.2, 0.0);
        let rim: Vec<Vec3> = (0..n)
            .map(|i| {
                let a = i as f32 / n as f32 * 2.0 * std::f32::consts::PI;
                center + Vec3::new(a.cos() * 1.3, a.sin() * 0.7, 0.1 * a.sin())
            })
            .collect();
        let tris: Vec<[Vec3; 3]> = (0..n).map(|i| [center, rim[i], rim[(i + 1) % n]]).collect();
        let hits = |o: Vec3, d: Vec3| {
            tris.iter().filter(|t| intersect_triangle(o, d, t[0], t[1], t[2], 1e9).is_some()).count()
        };
        for k in 0..200 {
            let o = Vec3::new(0.01 * k as f32 - 1.0, 0.37 - 0.002 * k as f32, 3.0 + 0.1 * k as f32);
            assert!(hits(o, (center - o).normalize()) >= 1, "ray {k} through the shared vertex missed");
            // Points on the shared edges.
            let e = center.lerp(rim[k % n], 0.25 + 0.003 * k as f32);
            assert!(hits(o, (e - o).normalize()) >= 1, "ray {k} through a shared edge missed");
        }
    }
}
