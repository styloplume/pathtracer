//! Geometric primitives and their ray intersection routines.

use crate::math::Vec3;

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
    /// Slab test. Returns the entry distance, or infinity on a miss.
    #[inline]
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

#[derive(Clone, Copy, Debug)]
pub struct Sphere {
    pub center: Vec3,
    pub radius: f32,
    pub material: u32,
}

pub const NO_NORMALS: u32 = u32::MAX;

/// A triangle referencing the scene's global vertex arrays.
#[derive(Clone, Copy, Debug)]
pub struct Triangle {
    pub v: [u32; 3],
    /// Indices into the normal array, or `[NO_NORMALS; 3]` for flat shading.
    pub n: [u32; 3],
    pub material: u32,
}

impl Triangle {
    #[inline]
    pub fn has_normals(&self) -> bool {
        self.n[0] != NO_NORMALS
    }
}

/// Möller–Trumbore. Returns (t, b1, b2) where p = (1-b1-b2) p0 + b1 p1 + b2 p2.
#[inline]
pub fn intersect_triangle(o: Vec3, d: Vec3, p0: Vec3, p1: Vec3, p2: Vec3, t_max: f32) -> Option<(f32, f32, f32)> {
    let e1 = p1 - p0;
    let e2 = p2 - p0;
    let pvec = d.cross(e2);
    let det = e1.dot(pvec);
    if det == 0.0 || !det.is_finite() {
        return None;
    }
    let inv = 1.0 / det;
    let tvec = o - p0;
    let b1 = tvec.dot(pvec) * inv;
    if !(0.0..=1.0).contains(&b1) {
        return None;
    }
    let qvec = tvec.cross(e1);
    let b2 = d.dot(qvec) * inv;
    if b2 < 0.0 || b1 + b2 > 1.0 {
        return None;
    }
    let t = e2.dot(qvec) * inv;
    if t > 0.0 && t < t_max { Some((t, b1, b2)) } else { None }
}

/// Ray/sphere intersection with the numerically robust discriminant from
/// Ray Tracing Gems ch. 7. `d` must be normalized.
#[inline]
pub fn intersect_sphere(o: Vec3, d: Vec3, center: Vec3, radius: f32, t_max: f32) -> Option<f32> {
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
    let (mut t0, mut t1) = (c / q, q);
    if t0 > t1 {
        std::mem::swap(&mut t0, &mut t1);
    }
    if t0 > 0.0 && t0 < t_max {
        Some(t0)
    } else if t1 > 0.0 && t1 < t_max {
        Some(t1)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sphere_hit_from_outside_and_inside() {
        let c = Vec3::new(0.0, 0.0, -5.0);
        let t = intersect_sphere(Vec3::ZERO, Vec3::new(0.0, 0.0, -1.0), c, 1.0, f32::INFINITY).unwrap();
        assert!((t - 4.0).abs() < 1e-5);
        let t = intersect_sphere(c, Vec3::new(0.0, 0.0, -1.0), c, 1.0, f32::INFINITY).unwrap();
        assert!((t - 1.0).abs() < 1e-5);
    }

    #[test]
    fn triangle_hit_and_miss() {
        let (p0, p1, p2) = (Vec3::new(-1.0, -1.0, 0.0), Vec3::new(1.0, -1.0, 0.0), Vec3::new(0.0, 1.0, 0.0));
        let o = Vec3::new(0.0, 0.0, 2.0);
        let hit = intersect_triangle(o, Vec3::new(0.0, 0.0, -1.0), p0, p1, p2, f32::INFINITY);
        assert!((hit.unwrap().0 - 2.0).abs() < 1e-5);
        assert!(intersect_triangle(o, Vec3::new(0.0, 0.0, 1.0), p0, p1, p2, f32::INFINITY).is_none());
    }
}
