//! Analytic shapes: spheres, disks (and annuli), open cylinders and oriented boxes.
//!
//! Every shape provides bounds, ray intersection (both crossings, for alpha masks),
//! surface parameterization (UV and its position derivatives) and uniform area
//! sampling, so any of them can be an area light.

use crate::geometry::{Aabb, sphere_roots};
use crate::math::{Frame, PI, Transform, Vec3};
use crate::sampling::uniform_sphere;

#[derive(Clone, Copy, Debug)]
pub enum ShapeKind {
    Sphere {
        center: Vec3,
        radius: f32,
    },
    /// Disk of `radius` around `center`, facing `frame.n`; an annulus if `inner` > 0.
    Disk {
        center: Vec3,
        frame: Frame,
        radius: f32,
        inner: f32,
    },
    /// Open tube of `radius` from `base` to `base + frame.n * height`.
    Cylinder {
        base: Vec3,
        frame: Frame,
        radius: f32,
        height: f32,
    },
    /// Box with axes `frame.t`, `frame.b`, `frame.n` and half extents `half` along them.
    Box {
        center: Vec3,
        frame: Frame,
        half: Vec3,
    },
}

#[derive(Clone, Copy, Debug)]
pub struct Shape {
    pub kind: ShapeKind,
    pub material: u32,
}

/// A point on a shape's surface with its local parameterization.
pub struct SurfacePoint {
    pub p: Vec3,
    /// Outward geometric normal (unit length).
    pub n: Vec3,
    pub uv: [f32; 2],
    pub dpdu: Vec3,
    pub dpdv: Vec3,
}

/// Latitude-longitude parameterization: u around +Y starting behind the sphere
/// (u = 0.5 faces +Z), v = 1 at the north pole. Returns (uv, dp/du, dp/dv).
pub fn sphere_param(n: Vec3, r: f32) -> ([f32; 2], Vec3, Vec3) {
    let phi = n.x.atan2(n.z);
    let theta = n.y.clamp(-1.0, 1.0).acos();
    let uv = [0.5 + phi * (0.5 / PI), 1.0 - theta / PI];
    let (sp, cp) = phi.sin_cos();
    let (st, ct) = theta.sin_cos();
    let dpdu = Vec3::new(cp, 0.0, -sp) * (2.0 * PI * r * st);
    let dpdv = Vec3::new(ct * sp, -st, ct * cp) * (-PI * r);
    (uv, dpdu, dpdv)
}

/// Bounds of a disk of radius `r` around `c` with unit normal `n`.
fn disk_bounds(c: Vec3, n: Vec3, r: f32) -> Aabb {
    let e = Vec3::new(
        (1.0 - n.x * n.x).max(0.0).sqrt(),
        (1.0 - n.y * n.y).max(0.0).sqrt(),
        (1.0 - n.z * n.z).max(0.0).sqrt(),
    ) * r;
    Aabb { min: c - e, max: c + e }
}

#[inline]
fn axis(f: &Frame, k: usize) -> Vec3 {
    match k {
        0 => f.t,
        1 => f.b,
        _ => f.n,
    }
}

/// Angle of (x, y) in [0, 2 pi).
#[inline]
fn angle(x: f32, y: f32) -> f32 {
    let phi = y.atan2(x);
    if phi < 0.0 { phi + 2.0 * PI } else { phi }
}

/// A right-handed frame from three orthonormal vectors that may be mirrored.
fn right_handed(t: Vec3, b: Vec3, n: Vec3) -> Frame {
    if t.cross(b).dot(n) < 0.0 { Frame { t: b, b: t, n } } else { Frame { t, b, n } }
}

impl ShapeKind {
    pub fn bounds(&self) -> Aabb {
        match *self {
            ShapeKind::Sphere { center, radius } => {
                let r = Vec3::splat(radius.abs());
                Aabb { min: center - r, max: center + r }
            }
            ShapeKind::Disk { center, frame, radius, .. } => disk_bounds(center, frame.n, radius),
            ShapeKind::Cylinder { base, frame, radius, height } => {
                let mut b = disk_bounds(base, frame.n, radius);
                b.grow(&disk_bounds(base + frame.n * height, frame.n, radius));
                b
            }
            ShapeKind::Box { center, frame, half } => {
                let e = frame.t.abs() * half.x + frame.b.abs() * half.y + frame.n.abs() * half.z;
                Aabb { min: center - e, max: center + e }
            }
        }
    }

    pub fn area(&self) -> f32 {
        match *self {
            ShapeKind::Sphere { radius, .. } => 4.0 * PI * radius * radius,
            ShapeKind::Disk { radius, inner, .. } => PI * (radius * radius - inner * inner),
            ShapeKind::Cylinder { radius, height, .. } => 2.0 * PI * radius * height,
            ShapeKind::Box { half: h, .. } => 8.0 * (h.x * h.y + h.y * h.z + h.z * h.x),
        }
    }

    /// Ray parameters (ascending) where the ray with origin `o` and unit direction `d`
    /// crosses the surface; a missing second crossing is infinite. May be negative.
    #[inline]
    pub fn hits(&self, o: Vec3, d: Vec3) -> Option<(f32, f32)> {
        match *self {
            ShapeKind::Sphere { center, radius } => sphere_roots(o, d, center, radius),
            ShapeKind::Disk { center, frame, radius, inner } => {
                let denom = d.dot(frame.n);
                if denom == 0.0 {
                    return None;
                }
                let t = (center - o).dot(frame.n) / denom;
                let q = o + d * t - center;
                let r2 = (q - frame.n * q.dot(frame.n)).length_squared();
                (r2 <= radius * radius && r2 >= inner * inner).then_some((t, f32::INFINITY))
            }
            ShapeKind::Cylinder { base, frame, radius, height } => {
                let lo = frame.to_local(o - base);
                let ld = frame.to_local(d);
                let a = ld.x * ld.x + ld.y * ld.y;
                if a == 0.0 {
                    return None;
                }
                // The robust quadratic of Ray Tracing Gems ch. 7, in the cross-section plane.
                let b = lo.x * ld.x + lo.y * ld.y;
                let c = lo.x * lo.x + lo.y * lo.y - radius * radius;
                let (lx, ly) = (lo.x - ld.x * (b / a), lo.y - ld.y * (b / a));
                let disc = radius * radius - (lx * lx + ly * ly);
                if disc < 0.0 {
                    return None;
                }
                let sq = (a * disc).sqrt();
                let q = if b > 0.0 { -b - sq } else { -b + sq };
                let (t0, t1) = if q == 0.0 { (0.0, 0.0) } else { (q / a, c / q) };
                let on = |t: f32| {
                    let z = lo.z + ld.z * t;
                    if (0.0..=height).contains(&z) { t } else { f32::INFINITY }
                };
                let (t0, t1) = (on(t0), on(t1));
                let (t0, t1) = if t0 <= t1 { (t0, t1) } else { (t1, t0) };
                (t0 < f32::INFINITY).then_some((t0, t1))
            }
            ShapeKind::Box { center, frame, half } => {
                let lo = frame.to_local(o - center);
                let ld = frame.to_local(d);
                let (mut near, mut far) = (f32::NEG_INFINITY, f32::INFINITY);
                for k in 0..3 {
                    let inv = 1.0 / ld[k];
                    let ta = (-half[k] - lo[k]) * inv;
                    let tb = (half[k] - lo[k]) * inv;
                    near = near.max(ta.min(tb));
                    far = far.min(ta.max(tb));
                }
                (near <= far).then_some((near, far))
            }
        }
    }

    /// Surface data at `p`, a point on (or within rounding error of) the surface.
    pub fn surface(&self, p: Vec3) -> SurfacePoint {
        match *self {
            ShapeKind::Sphere { center, radius } => {
                let n = ((p - center) * (1.0 / radius)).normalize();
                let (uv, dpdu, dpdv) = sphere_param(n, radius);
                SurfacePoint { p: center + n * radius, n, uv, dpdu, dpdv }
            }
            ShapeKind::Disk { center, frame, radius, inner } => {
                let q = p - center;
                let (x, y) = (q.dot(frame.t), q.dot(frame.b));
                let r = x.hypot(y);
                let dpdv = if r > 0.0 { (frame.t * x + frame.b * y) * ((inner - radius) / r) } else { Vec3::ZERO };
                SurfacePoint {
                    p: center + frame.t * x + frame.b * y,
                    n: frame.n,
                    uv: [angle(x, y) / (2.0 * PI), (radius - r) / (radius - inner)],
                    dpdu: (frame.b * x - frame.t * y) * (2.0 * PI),
                    dpdv,
                }
            }
            ShapeKind::Cylinder { base, frame, radius, height } => {
                let q = frame.to_local(p - base);
                let r = q.x.hypot(q.y);
                let (x, y) = if r > 0.0 { (q.x * radius / r, q.y * radius / r) } else { (radius, 0.0) };
                SurfacePoint {
                    p: base + frame.t * x + frame.b * y + frame.n * q.z,
                    n: (frame.t * x + frame.b * y) * (1.0 / radius),
                    uv: [angle(x, y) / (2.0 * PI), q.z / height],
                    dpdu: (frame.b * x - frame.t * y) * (2.0 * PI),
                    dpdv: frame.n * height,
                }
            }
            ShapeKind::Box { center, frame, half } => {
                let mut q = frame.to_local(p - center);
                let rel = |k: usize| (q[k] / half[k]).abs();
                let k = if rel(0) >= rel(1) && rel(0) >= rel(2) {
                    0
                } else if rel(1) >= rel(2) {
                    1
                } else {
                    2
                };
                let (i, j) = ((k + 1) % 3, (k + 2) % 3);
                let s = if q[k] < 0.0 { -1.0 } else { 1.0 };
                let mut qa = [q.x, q.y, q.z];
                qa[k] = s * half[k];
                q = Vec3::new(qa[0], qa[1], qa[2]);
                let (fu, fv) = (f32::midpoint(q[i] / half[i], 1.0), f32::midpoint(q[j] / half[j], 1.0));
                // Mirror u on the negative face so dp/du x dp/dv points outwards.
                let (u, dpdu) = if s > 0.0 {
                    (fu, axis(&frame, i) * (2.0 * half[i]))
                } else {
                    (1.0 - fu, axis(&frame, i) * (-2.0 * half[i]))
                };
                SurfacePoint {
                    p: center + frame.to_world(q),
                    n: axis(&frame, k) * s,
                    uv: [u, fv],
                    dpdu,
                    dpdv: axis(&frame, j) * (2.0 * half[j]),
                }
            }
        }
    }

    /// A point uniformly distributed over the surface area and its outward normal.
    pub fn sample(&self, u1: f32, u2: f32) -> (Vec3, Vec3) {
        match *self {
            ShapeKind::Sphere { center, radius } => {
                let n = uniform_sphere(u1, u2);
                (center + n * radius, n)
            }
            ShapeKind::Disk { center, frame, radius, inner } => {
                let r = (inner * inner + u1 * (radius * radius - inner * inner)).sqrt();
                let (s, c) = (2.0 * PI * u2).sin_cos();
                (center + (frame.t * c + frame.b * s) * r, frame.n)
            }
            ShapeKind::Cylinder { base, frame, radius, height } => {
                let (s, c) = (2.0 * PI * u2).sin_cos();
                let n = frame.t * c + frame.b * s;
                (base + n * radius + frame.n * (u1 * height), n)
            }
            ShapeKind::Box { center, frame, half: h } => {
                // Pick a pair of opposite faces by area, then a side, then a point.
                let areas = [h.y * h.z, h.z * h.x, h.x * h.y];
                let mut u = u1 * (areas[0] + areas[1] + areas[2]);
                let mut k = 0;
                while k < 2 && u >= areas[k] {
                    u -= areas[k];
                    k += 1;
                }
                let u = (u / areas[k]).clamp(0.0, 1.0);
                let (s, u) = if u < 0.5 { (-1.0, u * 2.0) } else { (1.0, u * 2.0 - 1.0) };
                let (i, j) = ((k + 1) % 3, (k + 2) % 3);
                let n = axis(&frame, k) * s;
                let p = center
                    + n * h[k]
                    + axis(&frame, i) * ((2.0 * u - 1.0) * h[i])
                    + axis(&frame, j) * ((2.0 * u2 - 1.0) * h[j]);
                (p, n)
            }
        }
    }

    /// The shape under an affine transform, if it stays a shape of the same kind
    /// (spheres, disks and cylinders need a uniform scale; boxes stay boxes under any
    /// rotation and per-axis scale applied before it).
    pub fn transformed(&self, xf: &Transform) -> Option<ShapeKind> {
        if xf.is_identity() {
            return Some(*self);
        }
        let rotate_frame = |f: &Frame, s: f32| {
            let v = |a: Vec3| xf.vector(a) * (1.0 / s);
            right_handed(v(f.t), v(f.b), v(f.n))
        };
        match *self {
            ShapeKind::Box { center, frame, half } => {
                let a = [xf.vector(frame.t), xf.vector(frame.b), xf.vector(frame.n)];
                let l = a.map(Vec3::length);
                if l.contains(&0.0) {
                    return None;
                }
                let u = [a[0] * (1.0 / l[0]), a[1] * (1.0 / l[1]), a[2] * (1.0 / l[2])];
                let orthogonal =
                    u[0].dot(u[1]).abs() < 1e-4 && u[1].dot(u[2]).abs() < 1e-4 && u[0].dot(u[2]).abs() < 1e-4;
                orthogonal.then(|| ShapeKind::Box {
                    center: xf.point(center),
                    frame: right_handed(u[0], u[1], u[2]),
                    half: Vec3::new(half.x * l[0], half.y * l[1], half.z * l[2]),
                })
            }
            ShapeKind::Sphere { center, radius } => {
                let s = xf.uniform_scale()?;
                Some(ShapeKind::Sphere { center: xf.point(center), radius: radius * s })
            }
            ShapeKind::Disk { center, frame, radius, inner } => {
                let s = xf.uniform_scale()?;
                let f = rotate_frame(&frame, s);
                Some(ShapeKind::Disk { center: xf.point(center), frame: f, radius: radius * s, inner: inner * s })
            }
            ShapeKind::Cylinder { base, frame, radius, height } => {
                let s = xf.uniform_scale()?;
                let f = rotate_frame(&frame, s);
                Some(ShapeKind::Cylinder { base: xf.point(base), frame: f, radius: radius * s, height: height * s })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::math::Trs;
    use crate::rng::Rng;

    fn shapes() -> Vec<ShapeKind> {
        let f = Frame::from_normal(Vec3::new(0.3, 1.0, -0.2).normalize());
        vec![
            ShapeKind::Sphere { center: Vec3::new(0.1, 0.2, 0.3), radius: 0.8 },
            ShapeKind::Disk { center: Vec3::new(0.1, 0.2, 0.3), frame: f, radius: 1.0, inner: 0.3 },
            ShapeKind::Cylinder { base: Vec3::new(0.1, -0.5, 0.3), frame: f, radius: 0.6, height: 1.2 },
            ShapeKind::Box { center: Vec3::new(0.1, 0.2, 0.3), frame: f, half: Vec3::new(0.4, 0.7, 0.3) },
        ]
    }

    /// Rays aimed at sampled surface points find them, and surface data is consistent.
    #[test]
    fn hits_sampled_points() {
        let mut rng = Rng::new(5, 6);
        for s in shapes() {
            let b = s.bounds();
            for _ in 0..500 {
                let (p, n) = s.sample(rng.f32(), rng.f32());
                assert!(p.x >= b.min.x - 1e-4 && p.x <= b.max.x + 1e-4, "{s:?}: sample outside bounds");
                assert!(p.y >= b.min.y - 1e-4 && p.z <= b.max.z + 1e-4);
                let o = p + n * 3.0 + Vec3::new(rng.f32(), rng.f32(), rng.f32()) * 0.5;
                let d = (p - o).normalize();
                let (t0, t1) = s.hits(o, d).expect("ray towards a surface point hits");
                let t = (p - o).length();
                // The sampled point may be hidden behind another part (e.g. inside a tube).
                assert!(t0 <= t + 1e-3 && (t0 - t).abs() < 1e-3 || (t1 - t).abs() < 1e-3, "{s:?}: {t0} {t1} vs {t}");
                let sp = s.surface(p);
                assert!((sp.p - p).length() < 1e-4, "{s:?}: projection moved the point");
                assert!(sp.n.dot(n) > 0.999, "{s:?}: normals disagree");
                // dp/du x dp/dv points outwards.
                assert!(sp.dpdu.cross(sp.dpdv).dot(n) > 0.0, "{s:?}: uv orientation");
                assert!((0.0..=1.0).contains(&sp.uv[0]) && (-1e-5..=1.0 + 1e-5).contains(&sp.uv[1]));
            }
        }
    }

    #[test]
    fn box_from_inside_and_cylinder_caps_are_open() {
        let bx = ShapeKind::Box { center: Vec3::ZERO, frame: Frame::from_normal(Vec3::Z), half: Vec3::ONE };
        let (t0, t1) = bx.hits(Vec3::ZERO, Vec3::new(1.0, 0.0, 0.0)).unwrap();
        assert!((t0 + 1.0).abs() < 1e-6 && (t1 - 1.0).abs() < 1e-6);
        let cyl = ShapeKind::Cylinder {
            base: Vec3::ZERO,
            frame: Frame::from_normal(Vec3::new(0.0, 1.0, 0.0)),
            radius: 1.0,
            height: 1.0,
        };
        // Straight down the axis: the open tube is never hit.
        assert!(cyl.hits(Vec3::new(0.0, 5.0, 0.0), Vec3::new(0.0, -1.0, 0.0)).is_none());
        let (t0, t1) = cyl.hits(Vec3::new(-3.0, 0.5, 0.0), Vec3::new(1.0, 0.0, 0.0)).unwrap();
        assert!((t0 - 2.0).abs() < 1e-5 && (t1 - 4.0).abs() < 1e-5);
    }

    #[test]
    fn area_matches_sampling_density() {
        // Monte Carlo estimate of the box area from the fraction of hits.
        let s = ShapeKind::Disk { center: Vec3::ZERO, frame: Frame::from_normal(Vec3::Z), radius: 1.0, inner: 0.5 };
        let mut rng = Rng::new(1, 1);
        let n = 100_000;
        let inside = (0..n)
            .filter(|_| {
                let o = Vec3::new(rng.f32() * 2.0 - 1.0, rng.f32() * 2.0 - 1.0, 1.0);
                s.hits(o, Vec3::new(0.0, 0.0, -1.0)).is_some()
            })
            .count();
        let est = 4.0 * inside as f32 / n as f32;
        assert!((est - s.area()).abs() < 0.03, "{est} vs {}", s.area());
    }

    #[test]
    fn transforms_bake_into_shapes() {
        let xf =
            Trs { translate: Vec3::new(1.0, 2.0, 3.0), rotate: Vec3::new(10.0, 50.0, -20.0), scale: Vec3::splat(2.0) }
                .transform()
                .unwrap();
        let mut rng = Rng::new(9, 9);
        for s in shapes() {
            let t = s.transformed(&xf).expect("uniform scale keeps every shape");
            assert!((t.area() - 4.0 * s.area()).abs() < 1e-3 * t.area());
            for _ in 0..50 {
                let (p, n) = s.sample(rng.f32(), rng.f32());
                let (pw, nw) = (xf.point(p), xf.normal(n));
                let sp = t.surface(pw);
                assert!((sp.p - pw).length() < 1e-4, "{s:?}");
                assert!(sp.n.dot(nw) > 0.999, "{s:?}");
            }
        }
        let squash = Trs { scale: Vec3::new(1.0, 2.0, 1.0), ..Trs::IDENTITY }.transform().unwrap();
        let sphere = shapes()[0];
        assert!(sphere.transformed(&squash).is_none());
        assert!(shapes()[3].transformed(&squash).is_none(), "a rotated box under a skewing scale is no box");
    }
}
