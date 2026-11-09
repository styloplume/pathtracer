//! Physically based materials (BSDFs) and emitters.
//!
//! All BSDF routines work in the local shading frame where the surface normal is +Z.
//! `eval` returns `f(wo, wi) * |cos(theta_i)|` together with the solid-angle pdf of
//! sampling `wi`; `sample` returns the Monte Carlo weight `f * |cos| / pdf`.

use crate::math::{INV_PI, PI, Vec3};
use crate::rng::Rng;
use crate::sampling::cosine_hemisphere;

#[derive(Clone, Debug)]
pub enum Material {
    /// Lambertian reflector.
    Diffuse { albedo: Vec3 },
    /// Metal: GGX microfacet reflection with Schlick Fresnel (F0 = `color`).
    /// Roughness 0 gives a perfect mirror.
    Conductor { color: Vec3, roughness: f32 },
    /// Smooth glass-like interface with exact dielectric Fresnel.
    Dielectric { ior: f32, tint: Vec3 },
    /// Area light. One-sided lights emit towards their geometric normal.
    Emissive { radiance: Vec3, two_sided: bool },
}

pub struct BsdfSample {
    pub wi: Vec3,
    pub weight: Vec3,
    pub pdf: f32,
    pub delta: bool,
}

/// Below this GGX alpha a conductor is treated as a perfect mirror.
const MIN_ALPHA: f32 = 1e-3;

impl Material {
    /// True if the BSDF is a Dirac delta (cannot be evaluated for arbitrary directions).
    pub fn is_delta(&self) -> bool {
        match *self {
            Material::Dielectric { .. } => true,
            Material::Conductor { roughness, .. } => alpha(roughness) < MIN_ALPHA,
            _ => false,
        }
    }

    pub fn is_transmissive(&self) -> bool {
        matches!(self, Material::Dielectric { .. })
    }

    /// Surface color for the albedo AOV (denoiser guide), in [0, 1].
    pub fn albedo(&self) -> Vec3 {
        let c = match *self {
            Material::Diffuse { albedo } => albedo,
            Material::Conductor { color, .. } => color,
            Material::Dielectric { tint, .. } => tint,
            Material::Emissive { radiance, .. } => radiance / radiance.max_elem().max(1.0),
        };
        c.max(Vec3::ZERO).min(Vec3::ONE)
    }

    pub fn eval(&self, wo: Vec3, wi: Vec3) -> (Vec3, f32) {
        match *self {
            Material::Diffuse { albedo } => {
                if wo.z <= 0.0 || wi.z <= 0.0 {
                    return (Vec3::ZERO, 0.0);
                }
                (albedo * (INV_PI * wi.z), wi.z * INV_PI)
            }
            Material::Conductor { color, roughness } => {
                let a = alpha(roughness);
                if a < MIN_ALPHA || wo.z <= 0.0 || wi.z <= 0.0 {
                    return (Vec3::ZERO, 0.0);
                }
                let h = (wo + wi).normalize();
                let d = ggx_d(h.z, a);
                let g1o = smith_g1(wo.z, a);
                let g1i = smith_g1(wi.z, a);
                let f = schlick(color, wo.dot(h).max(0.0));
                // f * cos_i = F D G / (4 cos_o cos_i) * cos_i
                let f_cos = f * (d * g1o * g1i / (4.0 * wo.z));
                // Visible-normal sampling pdf, converted to the reflected direction.
                let pdf = g1o * d / (4.0 * wo.z);
                (f_cos, pdf)
            }
            Material::Dielectric { .. } | Material::Emissive { .. } => (Vec3::ZERO, 0.0),
        }
    }

    pub fn sample(&self, wo: Vec3, rng: &mut Rng) -> Option<BsdfSample> {
        match *self {
            Material::Diffuse { albedo } => {
                if wo.z <= 0.0 {
                    return None;
                }
                let wi = cosine_hemisphere(rng.f32(), rng.f32());
                if wi.z <= 0.0 {
                    return None;
                }
                Some(BsdfSample { wi, weight: albedo, pdf: wi.z * INV_PI, delta: false })
            }
            Material::Conductor { color, roughness } => {
                if wo.z <= 0.0 {
                    return None;
                }
                let a = alpha(roughness);
                if a < MIN_ALPHA {
                    let wi = Vec3::new(-wo.x, -wo.y, wo.z);
                    return Some(BsdfSample { wi, weight: schlick(color, wo.z), pdf: 1.0, delta: true });
                }
                let h = sample_ggx_vndf(wo, a, rng.f32(), rng.f32());
                let wi = h * (2.0 * wo.dot(h)) - wo;
                if wi.z <= 0.0 {
                    return None;
                }
                let d = ggx_d(h.z, a);
                let g1o = smith_g1(wo.z, a);
                let pdf = g1o * d / (4.0 * wo.z);
                // (F D G1o G1i / 4 cos_o) / (G1o D / 4 cos_o) = F G1i
                let weight = schlick(color, wo.dot(h).max(0.0)) * smith_g1(wi.z, a);
                Some(BsdfSample { wi, weight, pdf, delta: false })
            }
            Material::Dielectric { ior, tint } => {
                let entering = wo.z > 0.0;
                let eta = if entering { 1.0 / ior } else { ior }; // eta_i / eta_t
                let cos_i = wo.z.abs();
                let (f, cos_t) = fresnel_dielectric(cos_i, eta);
                if rng.f32() < f {
                    let wi = Vec3::new(-wo.x, -wo.y, wo.z);
                    Some(BsdfSample { wi, weight: Vec3::ONE, pdf: f, delta: true })
                } else {
                    let n = if entering { Vec3::Z } else { -Vec3::Z };
                    let wi = (-wo * eta + n * (eta * cos_i - cos_t)).normalize();
                    Some(BsdfSample { wi, weight: tint, pdf: 1.0 - f, delta: true })
                }
            }
            Material::Emissive { .. } => None,
        }
    }
}

#[inline]
fn alpha(roughness: f32) -> f32 {
    // Perceptual roughness -> GGX alpha.
    let r = roughness.clamp(0.0, 1.0);
    r * r
}

/// GGX / Trowbridge-Reitz normal distribution.
#[inline]
fn ggx_d(cos_h: f32, a: f32) -> f32 {
    if cos_h <= 0.0 {
        return 0.0;
    }
    let a2 = a * a;
    let c2 = cos_h * cos_h;
    let denom = c2 * (a2 - 1.0) + 1.0;
    a2 / (PI * denom * denom)
}

/// Smith masking term for GGX.
#[inline]
fn smith_g1(cos: f32, a: f32) -> f32 {
    let a2 = a * a;
    2.0 * cos / (cos + (a2 + (1.0 - a2) * cos * cos).sqrt())
}

#[inline]
fn schlick(f0: Vec3, cos: f32) -> Vec3 {
    let m = (1.0 - cos).clamp(0.0, 1.0);
    let m5 = m * m * m * m * m;
    f0 + (Vec3::ONE - f0) * m5
}

/// Exact unpolarized Fresnel reflectance for a dielectric interface.
/// `eta` is eta_incident / eta_transmitted. Returns (F, cos_theta_t).
fn fresnel_dielectric(cos_i: f32, eta: f32) -> (f32, f32) {
    let sin2_t = eta * eta * (1.0 - cos_i * cos_i);
    if sin2_t >= 1.0 {
        return (1.0, 0.0); // total internal reflection
    }
    let cos_t = (1.0 - sin2_t).sqrt();
    let rs = (eta * cos_i - cos_t) / (eta * cos_i + cos_t);
    let rp = (cos_i - eta * cos_t) / (cos_i + eta * cos_t);
    (0.5 * (rs * rs + rp * rp), cos_t)
}

/// Samples a GGX visible normal (Heitz 2018).
fn sample_ggx_vndf(wo: Vec3, a: f32, u1: f32, u2: f32) -> Vec3 {
    let vh = Vec3::new(a * wo.x, a * wo.y, wo.z).normalize();
    let lensq = vh.x * vh.x + vh.y * vh.y;
    let t1 = if lensq > 0.0 { Vec3::new(-vh.y, vh.x, 0.0) * (1.0 / lensq.sqrt()) } else { Vec3::new(1.0, 0.0, 0.0) };
    let t2 = vh.cross(t1);
    let r = u1.sqrt();
    let phi = 2.0 * PI * u2;
    let p1 = r * phi.cos();
    let s = 0.5 * (1.0 + vh.z);
    let p2 = (1.0 - s) * (1.0 - p1 * p1).max(0.0).sqrt() + s * r * phi.sin();
    let nh = t1 * p1 + t2 * p2 + vh * (1.0 - p1 * p1 - p2 * p2).max(0.0).sqrt();
    Vec3::new(a * nh.x, a * nh.y, nh.z.max(0.0)).normalize()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A white furnace test: estimate directional albedo by sampling. Energy must not
    /// exceed 1, and sampled weights must agree with eval()/pdf.
    #[test]
    fn conductor_sampling_matches_eval() {
        let m = Material::Conductor { color: Vec3::ONE, roughness: 0.5 };
        let wo = Vec3::new(0.4, 0.1, 0.9).normalize();
        let mut rng = Rng::new(7, 0);
        let mut albedo = 0.0;
        let n = 20000;
        for _ in 0..n {
            if let Some(s) = m.sample(wo, &mut rng) {
                let (f, pdf) = m.eval(wo, s.wi);
                assert!((pdf - s.pdf).abs() <= 1e-3 * pdf.max(1.0));
                assert!((f.x / pdf - s.weight.x).abs() < 1e-3 * s.weight.x.max(1.0));
                albedo += s.weight.x;
            }
        }
        let albedo = albedo / n as f32;
        assert!(albedo <= 1.0 && albedo > 0.8, "albedo {albedo}");
    }

    #[test]
    fn fresnel_limits() {
        let (f, _) = fresnel_dielectric(1.0, 1.0 / 1.5);
        assert!((f - 0.04).abs() < 1e-3);
        let (f, _) = fresnel_dielectric(0.1, 1.5);
        assert_eq!(f, 1.0);
    }
}
