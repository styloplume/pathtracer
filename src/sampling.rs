//! Warping functions from uniform samples to useful distributions.

use crate::math::{PI, Vec3};

/// Largest f32 below 1: keeps continuous samples inside [0, 1).
pub const ONE_MINUS_EPSILON: f32 = 1.0 - f32::EPSILON / 2.0;

/// Shirley-Chiu concentric mapping of the unit square to the unit disk.
pub fn concentric_disk(u1: f32, u2: f32) -> (f32, f32) {
    let a = 2.0 * u1 - 1.0;
    let b = 2.0 * u2 - 1.0;
    if a == 0.0 && b == 0.0 {
        return (0.0, 0.0);
    }
    let (r, theta) = if a.abs() > b.abs() { (a, (PI / 4.0) * (b / a)) } else { (b, PI / 2.0 - (PI / 4.0) * (a / b)) };
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

/// Uniform direction in the cone of directions around +Z with `cos(theta) >= cos_max`.
/// pdf = 1 / (2 pi (1 - `cos_max`)).
pub fn uniform_cone(u1: f32, u2: f32, cos_max: f32) -> Vec3 {
    let cos = (1.0 - u1) + u1 * cos_max;
    let sin = (1.0 - cos * cos).max(0.0).sqrt();
    let phi = 2.0 * PI * u2;
    Vec3::new(sin * phi.cos(), sin * phi.sin(), cos)
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

/// Angle between two unit vectors, accurate for nearly (anti)parallel vectors.
pub fn angle_between(a: Vec3, b: Vec3) -> f32 {
    if a.dot(b) < 0.0 {
        PI - 2.0 * ((a + b).length() * 0.5).min(1.0).asin()
    } else {
        2.0 * ((b - a).length() * 0.5).min(1.0).asin()
    }
}

/// Solid angle subtended by triangle `v` seen from `p` (Van Oosterom & Strackee).
pub fn spherical_triangle_area(v: [Vec3; 3], p: Vec3) -> f32 {
    let (a, b, c) = ((v[0] - p).normalize(), (v[1] - p).normalize(), (v[2] - p).normalize());
    (2.0 * a.dot(b.cross(c)).atan2(1.0 + a.dot(b) + a.dot(c) + b.dot(c))).abs()
}

/// Gram-Schmidt: the part of `v` orthogonal to the unit vector `w`.
#[inline]
fn gram_schmidt(v: Vec3, w: Vec3) -> Vec3 {
    v - w * v.dot(w)
}

/// Samples a direction uniformly in the solid angle of triangle `v` seen from `p`
/// (Arvo 1995, as in pbrt-v4). Returns the barycentrics (b1, b2) of the point the
/// direction hits, or None for a degenerate configuration. The pdf is one over
/// [`spherical_triangle_area`].
pub fn sample_spherical_triangle(v: [Vec3; 3], p: Vec3, u1: f32, u2: f32) -> Option<(f32, f32)> {
    let (a, b, c) = ((v[0] - p).normalize(), (v[1] - p).normalize(), (v[2] - p).normalize());
    let (n_ab, n_bc, n_ca) = (a.cross(b), b.cross(c), c.cross(a));
    if n_ab.length_squared() == 0.0 || n_bc.length_squared() == 0.0 || n_ca.length_squared() == 0.0 {
        return None;
    }
    let (n_ab, n_bc, n_ca) = (n_ab.normalize(), n_bc.normalize(), n_ca.normalize());
    let alpha = angle_between(n_ab, -n_ca);
    let beta = angle_between(n_bc, -n_ab);
    let gamma = angle_between(n_ca, -n_bc);
    // Uniformly pick the sub-triangle area A', then the point on arc a-c bounding it.
    let a_pi = alpha + beta + gamma;
    let ap_pi = PI + u1 * (a_pi - PI);
    let (sin_alpha, cos_alpha) = alpha.sin_cos();
    let (sin_ap, cos_ap) = ap_pi.sin_cos();
    let sin_phi = sin_ap * cos_alpha - cos_ap * sin_alpha;
    let cos_phi = cos_ap * cos_alpha + sin_ap * sin_alpha;
    let k1 = cos_phi + cos_alpha;
    let k2 = sin_phi - sin_alpha * a.dot(b);
    let cos_bp = ((k2 + (k2 * cos_phi - k1 * sin_phi) * cos_alpha) / ((k2 * sin_phi + k1 * cos_phi) * sin_alpha))
        .clamp(-1.0, 1.0);
    if !cos_bp.is_finite() {
        return None;
    }
    let sin_bp = (1.0 - cos_bp * cos_bp).max(0.0).sqrt();
    let cp = a * cos_bp + gram_schmidt(c, a).normalize() * sin_bp;
    // Then a direction along the arc b-c'.
    let cos_t = 1.0 - u2 * (1.0 - cp.dot(b));
    let sin_t = (1.0 - cos_t * cos_t).max(0.0).sqrt();
    let w = b * cos_t + gram_schmidt(cp, b).normalize() * sin_t;
    // Barycentrics of the ray p + t w on the triangle.
    let (e1, e2) = (v[1] - v[0], v[2] - v[0]);
    let s1 = w.cross(e2);
    let div = s1.dot(e1);
    if div == 0.0 || !div.is_finite() {
        return None;
    }
    let s = p - v[0];
    let mut b1 = (s.dot(s1) / div).clamp(0.0, 1.0);
    let mut b2 = (w.dot(s.cross(e1)) / div).clamp(0.0, 1.0);
    if b1 + b2 > 1.0 {
        let sum = b1 + b2;
        (b1, b2) = (b1 / sum, b2 / sum);
    }
    (b1.is_finite() && b2.is_finite()).then_some((b1, b2))
}

/// Piecewise-constant distribution over [0, 1) with `func.len()` equal bins.
pub struct Distribution1D {
    func: Vec<f32>,
    cdf: Vec<f32>,
    /// Integral of the (non-negative) function over [0, 1).
    pub integral: f32,
}

impl Distribution1D {
    pub fn new(f: &[f32]) -> Distribution1D {
        let n = f.len().max(1);
        let func: Vec<f32> = if f.is_empty() {
            vec![0.0]
        } else {
            f.iter().map(|&v| if v.is_finite() { v.max(0.0) } else { 0.0 }).collect()
        };
        let mut acc = vec![0f64; n + 1];
        for i in 0..n {
            acc[i + 1] = acc[i] + f64::from(func[i]) / n as f64;
        }
        let total = acc[n];
        let cdf = (0..=n).map(|i| if total > 0.0 { (acc[i] / total) as f32 } else { i as f32 / n as f32 }).collect();
        Distribution1D { func, cdf, integral: total as f32 }
    }

    /// Samples x in [0, 1). Returns (x, pdf, bin).
    pub fn sample(&self, u: f32) -> (f32, f32, usize) {
        let n = self.func.len();
        let i = self.cdf.partition_point(|&c| c <= u).saturating_sub(1).min(n - 1);
        let w = self.cdf[i + 1] - self.cdf[i];
        let du = if w > 0.0 { ((u - self.cdf[i]) / w).clamp(0.0, 1.0) } else { 0.0 };
        let x = ((i as f32 + du) / n as f32).min(ONE_MINUS_EPSILON);
        (x, self.bin_pdf(i), i)
    }

    #[inline]
    fn bin_pdf(&self, i: usize) -> f32 {
        if self.integral > 0.0 { self.func[i] / self.integral } else { 1.0 }
    }

    pub fn pdf(&self, x: f32) -> f32 {
        let n = self.func.len();
        self.bin_pdf(((x * n as f32) as usize).min(n - 1))
    }
}

/// Piecewise-constant distribution over [0, 1)^2, stored as `h` rows of `w` bins.
pub struct Distribution2D {
    rows: Vec<Distribution1D>,
    marginal: Distribution1D,
}

impl Distribution2D {
    /// `f` holds `h` rows of `w` values; row `r` covers v in [r/h, (r+1)/h).
    pub fn new(f: &[f32], w: usize, h: usize) -> Distribution2D {
        let rows: Vec<Distribution1D> = f.chunks_exact(w).take(h).map(Distribution1D::new).collect();
        let marginal = Distribution1D::new(&rows.iter().map(|r| r.integral).collect::<Vec<_>>());
        Distribution2D { rows, marginal }
    }

    pub fn integral(&self) -> f32 {
        self.marginal.integral
    }

    /// Returns ([u, v], pdf).
    pub fn sample(&self, u1: f32, u2: f32) -> ([f32; 2], f32) {
        let (v, pdf_v, row) = self.marginal.sample(u2);
        let (u, pdf_u, _) = self.rows[row].sample(u1);
        ([u, v], pdf_u * pdf_v)
    }

    pub fn pdf(&self, uv: [f32; 2]) -> f32 {
        let h = self.rows.len();
        let row = ((uv[1] * h as f32) as usize).min(h - 1);
        self.marginal.pdf(uv[1]) * self.rows[row].pdf(uv[0])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rng::Rng;

    #[test]
    fn distribution_matches_function() {
        let f = [0.0, 1.0, 3.0, 0.0, 4.0];
        let d = Distribution1D::new(&f);
        assert!((d.integral - 8.0 / 5.0).abs() < 1e-6);
        let mut rng = Rng::new(1, 1);
        let mut hist = [0usize; 5];
        for _ in 0..80_000 {
            let (x, pdf, i) = d.sample(rng.f32());
            assert!(pdf > 0.0 && (pdf - d.pdf(x)).abs() < 1e-6 && i == (x * 5.0) as usize);
            hist[i] += 1;
        }
        for (i, &c) in hist.iter().enumerate() {
            assert!((c as f32 / 80_000.0 - f[i] / 8.0).abs() < 0.01, "{hist:?}");
        }
        let d2 = Distribution2D::new(&[1.0, 2.0, 0.0, 5.0], 2, 2);
        assert!((d2.pdf([0.75, 0.75]) - 5.0 / 2.0).abs() < 1e-5);
        let (uv, pdf) = d2.sample(0.99, 0.99);
        assert!(uv[0] >= 0.5 && uv[1] >= 0.5 && (pdf - 2.5).abs() < 1e-5);
    }

    #[test]
    fn spherical_triangle_sampling_is_uniform_in_solid_angle() {
        let v = [Vec3::new(-1.0, 2.0, -1.0), Vec3::new(1.5, 2.0, -0.5), Vec3::new(0.0, 2.5, 1.0)];
        let p = Vec3::new(0.2, 0.0, 0.1);
        let area = spherical_triangle_area(v, p);
        // Estimate the solid angle of a sub-region (half-space x < 0) by sampling vs. by
        // uniform sphere sampling; both must agree if sampling is uniform.
        let mut rng = Rng::new(5, 9);
        let n = 200_000;
        let mut inside = 0;
        for _ in 0..n {
            let (b1, b2) = sample_spherical_triangle(v, p, rng.f32(), rng.f32()).unwrap();
            let q = v[0] * (1.0 - b1 - b2) + v[1] * b1 + v[2] * b2;
            if q.x < 0.0 {
                inside += 1;
            }
        }
        let frac_sampled = inside as f32 / n as f32;
        let (mut hit, mut hit_left) = (0, 0);
        for _ in 0..2_000_000 {
            let d = uniform_sphere(rng.f32(), rng.f32());
            if let Some((t, _, _)) = crate::geometry::intersect_triangle(p, d, v[0], v[1], v[2], f32::INFINITY) {
                hit += 1;
                if (p + d * t).x < 0.0 {
                    hit_left += 1;
                }
            }
        }
        let area_mc = hit as f32 / 2_000_000.0 * 4.0 * PI;
        assert!((area - area_mc).abs() < 0.02 * area, "{area} vs {area_mc}");
        let frac_mc = hit_left as f32 / hit as f32;
        assert!((frac_sampled - frac_mc).abs() < 0.01, "{frac_sampled} vs {frac_mc}");
    }

    #[test]
    fn cone_stays_inside() {
        let mut rng = Rng::new(2, 3);
        for _ in 0..1000 {
            let d = uniform_cone(rng.f32(), rng.f32(), 0.9);
            assert!(d.z >= 0.9 - 1e-6 && (d.length() - 1.0).abs() < 1e-5);
        }
    }
}
