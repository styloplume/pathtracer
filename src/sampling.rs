//! Warping functions from uniform samples to useful distributions.

use crate::math::{PI, Vec3};

/// Shirley-Chiu concentric mapping of the unit square to the unit disk.
pub fn concentric_disk(u1: f32, u2: f32) -> (f32, f32) {
    let a = 2.0 * u1 - 1.0;
    let b = 2.0 * u2 - 1.0;
    if a == 0.0 && b == 0.0 {
        return (0.0, 0.0);
    }
    let (r, theta) = if a.abs() > b.abs() {
        (a, (PI / 4.0) * (b / a))
    } else {
        (b, PI / 2.0 - (PI / 4.0) * (a / b))
    };
    (r * theta.cos(), r * theta.sin())
}

/// Cosine-weighted direction on the +Z hemisphere. pdf = cos(theta) / pi.
pub fn cosine_hemisphere(u1: f32, u2: f32) -> Vec3 {
    let (x, y) = concentric_disk(u1, u2);
    let z = (1.0 - x * x - y * y).max(0.0).sqrt();
    Vec3::new(x, y, z)
}

/// Uniform direction on the unit sphere. pdf = 1 / (4 pi).
pub fn uniform_sphere(u1: f32, u2: f32) -> Vec3 {
    let z = 1.0 - 2.0 * u1;
    let r = (1.0 - z * z).max(0.0).sqrt();
    let phi = 2.0 * PI * u2;
    Vec3::new(r * phi.cos(), r * phi.sin(), z)
}

/// Uniform barycentric coordinates (b1, b2) on a triangle.
pub fn uniform_triangle(u1: f32, u2: f32) -> (f32, f32) {
    let su = u1.sqrt();
    (1.0 - su, u2 * su)
}

/// Veach's power heuristic (beta = 2) for multiple importance sampling.
#[inline]
pub fn power_heuristic(pdf_a: f32, pdf_b: f32) -> f32 {
    let a = pdf_a * pdf_a;
    let b = pdf_b * pdf_b;
    if a + b > 0.0 { a / (a + b) } else { 0.0 }
}
