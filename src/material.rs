//! Materials and BSDFs.
//!
//! A `Material` holds (possibly textured) parameters. At a shading point it is
//! evaluated into a `Bsdf`: a weighted mixture of up to `MAX_LOBES` lobes. All BSDF
//! routines work in the local shading frame where the normal is +Z. `eval` returns
//! `f(wo, wi) * |cos(theta_i)|` together with the solid-angle pdf of sampling `wi`;
//! `sample` returns the Monte Carlo weight `f * |cos| / pdf`.
//!
//! Transmission does not apply the `1 / eta^2` radiance scaling; it cancels for camera
//! paths that enter and leave a closed object, which is the case this renderer targets.

use crate::math::{INV_PI, PI, Vec3};
use crate::rng::Rng;
use crate::sampling::cosine_hemisphere;
use crate::scene::Interaction;
use crate::texture::{TexCtx, Texture};
use std::sync::OnceLock;

// ---------------------------------------------------------------------------- materials

pub struct Material {
    pub kind: MaterialKind,
    /// Tangent-space normal map (OpenGL convention, +Y up) and its strength.
    pub normal_map: Option<(Texture, f32)>,
    /// Scalar height texture and the height (world units) of a texture value of 1.
    pub bump: Option<(Texture, f32)>,
    /// Coverage: 0 = cut out, 1 = opaque, in between = stochastic.
    pub alpha: Option<Texture>,
}

pub enum MaterialKind {
    /// Lambertian reflector.
    Diffuse { albedo: Texture },
    /// GGX metal. `color` is F0 for Schlick Fresnel, or a tint over measured complex
    /// Fresnel. Roughness 0 gives a perfect mirror.
    Conductor { color: Texture, roughness: Texture, fresnel: MetalFresnel, multiscatter: bool },
    /// Glass-like interface, smooth or GGX-rough, with Beer–Lambert absorption inside.
    Dielectric { ior: f32, roughness: Texture, absorption: Vec3 },
    /// Diffuse base under a (smooth or rough) dielectric coating.
    Plastic { color: Texture, roughness: Texture, ior: f32 },
    /// Uber material in the spirit of the Disney / Blender principled BSDF.
    Principled(Box<Principled>),
    /// Area light. One-sided lights emit towards their geometric normal.
    Emissive { emission: Texture, two_sided: bool },
}

pub enum MetalFresnel {
    Schlick,
    /// Complex index of refraction per RGB channel; `favg` is the hemispherical average.
    Complex { eta: Vec3, k: Vec3, favg: Vec3 },
}

pub struct Principled {
    pub base_color: Texture,
    pub metallic: Texture,
    pub roughness: Texture,
    /// Dielectric specular level (0.5 = F0 0.04 = IOR 1.5); ignored when `ior` is set.
    pub specular: Texture,
    pub ior: Option<f32>,
    pub transmission: Texture,
    pub clearcoat: Texture,
    pub clearcoat_roughness: Texture,
    pub sheen: Texture,
    pub sheen_tint: Texture,
}

impl Material {
    pub fn new(kind: MaterialKind) -> Material {
        Material { kind, normal_map: None, bump: None, alpha: None }
    }

    pub fn diffuse(albedo: Vec3) -> Material {
        Material::new(MaterialKind::Diffuse { albedo: Texture::Const(albedo) })
    }

    /// Emitted radiance (and two-sidedness) if this is a light.
    #[inline]
    pub fn emission(&self, c: &TexCtx) -> Option<(Vec3, bool)> {
        match &self.kind {
            MaterialKind::Emissive { emission, two_sided } => Some((emission.eval(c), *two_sided)),
            _ => None,
        }
    }

    pub fn is_emissive(&self) -> bool {
        matches!(self.kind, MaterialKind::Emissive { .. })
    }

    /// Mean emitted radiance (for light selection).
    pub fn emission_mean(&self) -> Vec3 {
        match &self.kind {
            MaterialKind::Emissive { emission, .. } => emission.mean(),
            _ => Vec3::ZERO,
        }
    }

    /// Materials that transmit light keep the outward normal (they need to know
    /// whether a ray enters or leaves); all others are shaded two-sided.
    pub fn is_transmissive(&self) -> bool {
        match &self.kind {
            MaterialKind::Dielectric { .. } => true,
            MaterialKind::Principled(p) => p.transmission.constant().is_none_or(|t| t.x > 0.0),
            _ => false,
        }
    }

    /// Absorption coefficient of the interior (per unit distance).
    #[inline]
    pub fn absorption(&self) -> Vec3 {
        match self.kind {
            MaterialKind::Dielectric { absorption, .. } => absorption,
            _ => Vec3::ZERO,
        }
    }

    /// Shading normal after normal / bump mapping (unperturbed if there are no maps).
    pub fn shading_normal(&self, si: &Interaction, c: &TexCtx) -> Vec3 {
        let mut n = si.ns;
        if self.bump.is_none() && self.normal_map.is_none() {
            return n;
        }
        // Tangents projected into the shading plane.
        let tangents = |n: Vec3| {
            let du = si.dpdu - n * n.dot(si.dpdu);
            let dv = si.dpdv - n * n.dot(si.dpdv);
            if du.length_squared() > 0.0 && dv.length_squared() > 0.0 && du.cross(dv).length_squared() > 0.0 {
                (du, dv)
            } else {
                let f = crate::math::Frame::from_normal(n);
                (f.t, f.b)
            }
        };
        if let Some((tex, scale)) = &self.bump {
            let (dpdu, dpdv) = tangents(n);
            let d = tex.du();
            let h = tex.eval_f(c);
            let at = |du: f32, dv: f32| {
                let cc = TexCtx { uv: [c.uv[0] + du, c.uv[1] + dv], p: c.p + si.dpdu * du + si.dpdv * dv };
                tex.eval_f(&cc)
            };
            let (hu, hv) = (at(d, 0.0), at(0.0, d));
            let sign = if dpdu.cross(dpdv).dot(n) < 0.0 { -1.0 } else { 1.0 };
            let pu = dpdu + n * ((hu - h) / d * scale);
            let pv = dpdv + n * ((hv - h) / d * scale);
            let nb = pu.cross(pv) * sign;
            let len = nb.length();
            if len > 0.0 && len.is_finite() {
                n = nb * (1.0 / len);
            }
        }
        if let Some((tex, strength)) = &self.normal_map {
            let (dpdu, dpdv) = tangents(n);
            let t = dpdu.normalize();
            let mut b = n.cross(t);
            if b.dot(dpdv) < 0.0 {
                b = -b;
            }
            let m = tex.eval(c) * 2.0 - Vec3::ONE;
            let nm = t * (m.x * strength) + b * (m.y * strength) + n * m.z.max(1e-3);
            let len = nm.length();
            if len > 0.0 && len.is_finite() {
                n = nm * (1.0 / len);
            }
        }
        n
    }

    /// Evaluates the textures and builds the BSDF for outgoing direction `wo` (local frame).
    pub fn bsdf(&self, c: &TexCtx, wo: Vec3) -> Bsdf {
        let mut b = Bsdf::default();
        let rough = |t: &Texture| alpha(t.eval_f(c));
        match &self.kind {
            MaterialKind::Diffuse { albedo } => b.push(Lobe::Lambert { albedo: albedo.eval(c) }, 1.0),
            MaterialKind::Conductor { color, roughness, fresnel, multiscatter } => {
                let tint = color.eval(c).max(Vec3::ZERO);
                let a = rough(roughness);
                let (fr, favg) = match fresnel {
                    MetalFresnel::Schlick => (Fresnel::Schlick(tint), (tint * 20.0 + Vec3::ONE) / 21.0),
                    MetalFresnel::Complex { eta, k, favg } => {
                        (Fresnel::Conductor { eta: *eta, k: *k, tint }, *favg * tint)
                    }
                };
                let ms = if *multiscatter { ms_factor(favg, a) } else { None };
                b.push(Lobe::Reflect { alpha: a, fresnel: fr, ms }, 1.0);
            }
            MaterialKind::Dielectric { ior, roughness, .. } => {
                b.push(Lobe::Dielectric { ior: *ior, alpha: rough(roughness), tint: Vec3::ONE }, 1.0)
            }
            MaterialKind::Plastic { color, roughness, ior } => {
                let a = rough(roughness);
                b.push(Lobe::Reflect { alpha: a, fresnel: Fresnel::Dielectric(*ior), ms: None }, 1.0);
                b.push(Lobe::CoatedDiffuse { albedo: color.eval(c), ior: *ior, fdr: fdr_internal(*ior) }, 1.0);
            }
            MaterialKind::Principled(p) => p.build(c, wo, &mut b),
            MaterialKind::Emissive { .. } => {}
        }
        b.finish(wo);
        b
    }
}

impl Principled {
    fn build(&self, c: &TexCtx, wo: Vec3, b: &mut Bsdf) {
        let base = self.base_color.eval(c).max(Vec3::ZERO);
        let unit = |t: &Texture| t.eval_f(c).clamp(0.0, 1.0);
        let metallic = unit(&self.metallic);
        let a = alpha(self.roughness.eval_f(c));
        let ior = self.ior.unwrap_or_else(|| {
            let f0 = 0.08 * unit(&self.specular);
            let s = f0.sqrt().min(0.95);
            (1.0 + s) / (1.0 - s)
        });
        let transmission = unit(&self.transmission);
        let clearcoat = unit(&self.clearcoat);
        // The coat reflects F(wo) of the energy; everything below gets the rest.
        let coat_ior = 1.5;
        let under = if clearcoat > 0.0 { 1.0 - clearcoat * fr_dielectric(wo.z.abs(), coat_ior) } else { 1.0 };

        if metallic > 0.0 {
            let favg = (base * 20.0 + Vec3::ONE) / 21.0;
            b.push(Lobe::Reflect { alpha: a, fresnel: Fresnel::Schlick(base), ms: ms_factor(favg, a) }, metallic * under);
        }
        let dielectric = (1.0 - metallic) * under;
        if dielectric * transmission > 0.0 {
            b.push(Lobe::Dielectric { ior, alpha: a, tint: base }, dielectric * transmission);
        }
        let opaque = dielectric * (1.0 - transmission);
        if opaque > 0.0 {
            if ior > 1.0 {
                b.push(Lobe::Reflect { alpha: a, fresnel: Fresnel::Dielectric(ior), ms: None }, opaque);
                b.push(Lobe::CoatedDiffuse { albedo: base, ior, fdr: fdr_internal(ior) }, opaque);
            } else {
                b.push(Lobe::Lambert { albedo: base }, opaque);
            }
            let sheen = unit(&self.sheen);
            if sheen > 0.0 {
                let lum = base.luminance();
                let tint = if lum > 0.0 { base / lum } else { Vec3::ONE };
                let color = Vec3::ONE.lerp(tint, unit(&self.sheen_tint)) * sheen;
                b.push(Lobe::Sheen { color }, opaque);
            }
        }
        if clearcoat > 0.0 {
            let ca = alpha(self.clearcoat_roughness.eval_f(c));
            b.push(Lobe::Reflect { alpha: ca, fresnel: Fresnel::Dielectric(coat_ior), ms: None }, clearcoat);
        }
    }
}

// ---------------------------------------------------------------------------- measured metals

/// Complex IOR (eta, k) at roughly 650 / 550 / 450 nm for common metals.
pub fn metal_preset(name: &str) -> Option<(Vec3, Vec3)> {
    let v = Vec3::new;
    Some(match name {
        "gold" | "au" => (v(0.143, 0.374, 1.442), v(3.983, 2.385, 1.603)),
        "silver" | "ag" => (v(0.155, 0.117, 0.138), v(4.828, 3.122, 2.147)),
        "copper" | "cu" => (v(0.200, 0.924, 1.102), v(3.912, 2.452, 2.142)),
        "aluminium" | "aluminum" | "al" => (v(1.657, 0.880, 0.521), v(9.224, 6.270, 4.837)),
        "iron" | "fe" => (v(2.911, 2.950, 2.585), v(3.089, 2.932, 2.767)),
        "chromium" | "chrome" | "cr" => (v(3.110, 3.180, 2.320), v(3.310, 3.330, 3.130)),
        "platinum" | "pt" => (v(2.375, 2.085, 1.845), v(4.265, 3.715, 3.137)),
        _ => return None,
    })
}

pub const METAL_PRESETS: &str = "gold, silver, copper, aluminium, iron, chromium, platinum";

impl MetalFresnel {
    pub fn complex(eta: Vec3, k: Vec3) -> MetalFresnel {
        // favg = 2 * integral of F(mu) mu dmu (midpoint rule).
        let n = 128;
        let mut favg = Vec3::ZERO;
        for i in 0..n {
            let mu = (i as f32 + 0.5) / n as f32;
            favg += fr_conductor3(mu, eta, k) * (2.0 * mu / n as f32);
        }
        MetalFresnel::Complex { eta, k, favg }
    }
}

// ---------------------------------------------------------------------------- BSDF

pub struct BsdfSample {
    pub wi: Vec3,
    pub weight: Vec3,
    pub pdf: f32,
    pub delta: bool,
}

/// Below this GGX alpha a surface is treated as perfectly smooth.
const MIN_ALPHA: f32 = 1e-3;
pub const MAX_LOBES: usize = 6;

#[derive(Clone, Copy, Debug)]
pub enum Fresnel {
    Schlick(Vec3),
    /// Exact conductor Fresnel, multiplied by `tint`.
    Conductor { eta: Vec3, k: Vec3, tint: Vec3 },
    /// Exact dielectric Fresnel for a ray arriving from the outside.
    Dielectric(f32),
}

impl Fresnel {
    #[inline]
    fn eval(&self, cos: f32) -> Vec3 {
        match *self {
            Fresnel::Schlick(f0) => schlick(f0, cos),
            Fresnel::Conductor { eta, k, tint } => fr_conductor3(cos, eta, k) * tint,
            Fresnel::Dielectric(ior) => Vec3::splat(fr_dielectric(cos, ior)),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum Lobe {
    Lambert { albedo: Vec3 },
    /// Disney-style retro-reflective sheen for cloth.
    Sheen { color: Vec3 },
    /// GGX microfacet reflection (mirror below `MIN_ALPHA`). `ms` is the Kulla–Conty
    /// multiple-scattering Fresnel term when energy compensation is enabled.
    Reflect { alpha: f32, fresnel: Fresnel, ms: Option<Vec3> },
    /// Reflection + refraction through a dielectric interface; `tint` colors transmission.
    Dielectric { ior: f32, alpha: f32, tint: Vec3 },
    /// Lambertian base below a dielectric interface (the coating's specular reflection
    /// is a separate `Reflect` lobe). `fdr` is the interface's internal diffuse reflectance.
    CoatedDiffuse { albedo: Vec3, ior: f32, fdr: f32 },
}

impl Lobe {
    fn is_delta(&self) -> bool {
        match *self {
            Lobe::Reflect { alpha, .. } | Lobe::Dielectric { alpha, .. } => alpha < MIN_ALPHA,
            _ => false,
        }
    }

    /// Rough estimate of the lobe's reflectance towards `wo`, for picking lobes to sample.
    fn sampling_weight(&self, wo: Vec3) -> f32 {
        if let Lobe::Dielectric { .. } = self {
            return 1.0;
        }
        if wo.z <= 0.0 {
            return 0.0;
        }
        let w = match *self {
            Lobe::Lambert { albedo } => albedo.luminance(),
            Lobe::Sheen { color } => 0.25 * color.luminance(),
            Lobe::Reflect { fresnel, ms, .. } => {
                fresnel.eval(wo.z).luminance() + ms.map_or(0.0, |m| m.luminance() * 0.5)
            }
            Lobe::CoatedDiffuse { albedo, ior, .. } => (1.0 - fr_dielectric(wo.z, ior)) * albedo.luminance(),
            Lobe::Dielectric { .. } => 1.0,
        };
        w.max(1e-3)
    }

    /// Reflectance-like color for the albedo AOV.
    fn albedo(&self) -> Vec3 {
        match *self {
            Lobe::Lambert { albedo } | Lobe::CoatedDiffuse { albedo, .. } => albedo,
            Lobe::Sheen { .. } => Vec3::ZERO,
            Lobe::Reflect { fresnel, .. } => fresnel.eval(1.0),
            Lobe::Dielectric { tint, .. } => tint,
        }
    }

    fn eval(&self, wo: Vec3, wi: Vec3) -> (Vec3, f32) {
        let zero = (Vec3::ZERO, 0.0);
        match *self {
            Lobe::Lambert { albedo } => {
                if wo.z <= 0.0 || wi.z <= 0.0 {
                    return zero;
                }
                (albedo * (INV_PI * wi.z), wi.z * INV_PI)
            }
            Lobe::Sheen { color } => {
                if wo.z <= 0.0 || wi.z <= 0.0 {
                    return zero;
                }
                let h = (wo + wi).normalize();
                let m = (1.0 - wi.dot(h)).clamp(0.0, 1.0);
                (color * (m * m * m * m * m * wi.z), wi.z * INV_PI)
            }
            Lobe::Reflect { alpha: a, fresnel, ms } => {
                if a < MIN_ALPHA || wo.z <= 0.0 || wi.z <= 0.0 {
                    return zero;
                }
                let h = (wo + wi).normalize();
                let d = ggx_d(h.z, a);
                let g1o = smith_g1(wo.z, a);
                let g1i = smith_g1(wi.z, a);
                // f * cos_i = F D G / (4 cos_o cos_i) * cos_i
                let mut f = fresnel.eval(wo.dot(h).max(0.0)) * (d * g1o * g1i / (4.0 * wo.z));
                // Visible-normal sampling pdf, converted to the reflected direction.
                let mut pdf = g1o * d / (4.0 * wo.z);
                if let Some(fms) = ms {
                    let t = energy_table();
                    let (eo, ei, eavg) = (t.e(wo.z, a), t.e(wi.z, a), t.eavg(a));
                    f += fms * ((1.0 - eo) * (1.0 - ei) / (PI * (1.0 - eavg)) * wi.z);
                    let q = ms_probability(eo);
                    pdf = (1.0 - q) * pdf + q * wi.z * INV_PI;
                }
                (f, pdf)
            }
            Lobe::Dielectric { ior, alpha: a, tint } => {
                if a < MIN_ALPHA {
                    return zero;
                }
                rough_dielectric_eval(wo, wi, ior, a, tint)
            }
            Lobe::CoatedDiffuse { albedo, ior, fdr } => {
                if wo.z <= 0.0 || wi.z <= 0.0 {
                    return zero;
                }
                let t = (1.0 - fr_dielectric(wo.z, ior)) * (1.0 - fr_dielectric(wi.z, ior));
                // Interreflections between the base and the coating, energy conserving.
                let k = t / (ior * ior) * INV_PI * wi.z;
                let f = Vec3::new(
                    albedo.x / (1.0 - albedo.x * fdr),
                    albedo.y / (1.0 - albedo.y * fdr),
                    albedo.z / (1.0 - albedo.z * fdr),
                ) * k;
                (f, wi.z * INV_PI)
            }
        }
    }

    fn sample(&self, wo: Vec3, rng: &mut Rng) -> Option<BsdfSample> {
        match *self {
            Lobe::Lambert { .. } | Lobe::Sheen { .. } | Lobe::CoatedDiffuse { .. } => {
                if wo.z <= 0.0 {
                    return None;
                }
                let wi = cosine_hemisphere(rng.f32(), rng.f32());
                self.sample_from_eval(wo, wi)
            }
            Lobe::Reflect { alpha: a, fresnel, ms } => {
                if wo.z <= 0.0 {
                    return None;
                }
                if a < MIN_ALPHA {
                    let wi = Vec3::new(-wo.x, -wo.y, wo.z);
                    return Some(BsdfSample { wi, weight: fresnel.eval(wo.z), pdf: 1.0, delta: true });
                }
                let ms_q = ms.map_or(0.0, |_| ms_probability(energy_table().e(wo.z, a)));
                let (u1, u2) = (rng.f32(), rng.f32());
                let wi = if rng.f32() < ms_q {
                    cosine_hemisphere(u1, u2)
                } else {
                    let h = sample_ggx_vndf(wo, a, u1, u2);
                    h * (2.0 * wo.dot(h)) - wo
                };
                if wi.z <= 0.0 {
                    return None;
                }
                if ms.is_none() {
                    // (F D G1o G1i / 4 cos_o) / (G1o D / 4 cos_o) = F G1i
                    let h = (wo + wi).normalize();
                    let d = ggx_d(h.z, a);
                    let pdf = smith_g1(wo.z, a) * d / (4.0 * wo.z);
                    let weight = fresnel.eval(wo.dot(h).max(0.0)) * smith_g1(wi.z, a);
                    return (pdf > 0.0).then_some(BsdfSample { wi, weight, pdf, delta: false });
                }
                self.sample_from_eval(wo, wi)
            }
            Lobe::Dielectric { ior, alpha: a, tint } => {
                if wo.z == 0.0 {
                    return None;
                }
                if a < MIN_ALPHA {
                    let f = fr_dielectric(wo.z, ior);
                    if rng.f32() < f {
                        let wi = Vec3::new(-wo.x, -wo.y, wo.z);
                        return Some(BsdfSample { wi, weight: Vec3::ONE, pdf: f, delta: true });
                    }
                    let (wi, _) = refract(wo, Vec3::Z, ior)?;
                    return Some(BsdfSample { wi, weight: tint, pdf: 1.0 - f, delta: true });
                }
                let up = if wo.z < 0.0 { -wo } else { wo };
                let wm = sample_ggx_vndf(up, a, rng.f32(), rng.f32());
                let r = fr_dielectric(wo.dot(wm), ior);
                let wi = if rng.f32() < r {
                    let wi = wm * (2.0 * wo.dot(wm)) - wo;
                    if wi.z * wo.z <= 0.0 {
                        return None;
                    }
                    wi
                } else {
                    let (wi, _) = refract(wo, wm, ior)?;
                    if wi.z * wo.z >= 0.0 {
                        return None;
                    }
                    wi
                };
                self.sample_from_eval(wo, wi)
            }
        }
    }

    /// Builds a sample for direction `wi` from `eval` (exactly consistent with it).
    fn sample_from_eval(&self, wo: Vec3, wi: Vec3) -> Option<BsdfSample> {
        let (f, pdf) = self.eval(wo, wi);
        (pdf > 0.0 && pdf.is_finite()).then(|| BsdfSample { wi, weight: f / pdf, pdf, delta: false })
    }
}

/// A weighted mixture of lobes. `eval` sums `weight * f`; sampling picks a lobe with
/// probability proportional to `weight * estimated reflectance`.
#[derive(Default)]
pub struct Bsdf {
    lobes: [Option<Lobe>; MAX_LOBES],
    weight: [f32; MAX_LOBES],
    prob: [f32; MAX_LOBES],
    n: usize,
    delta: bool,
}

impl Bsdf {
    pub fn push(&mut self, lobe: Lobe, weight: f32) {
        if weight > 0.0 && self.n < MAX_LOBES {
            self.lobes[self.n] = Some(lobe);
            self.weight[self.n] = weight;
            self.n += 1;
        }
    }

    fn lobes(&self) -> impl Iterator<Item = (usize, &Lobe)> {
        self.lobes[..self.n].iter().enumerate().map(|(i, l)| (i, l.as_ref().unwrap()))
    }

    pub fn finish(&mut self, wo: Vec3) {
        let mut total = 0.0;
        for i in 0..self.n {
            let p = self.weight[i] * self.lobes[i].as_ref().unwrap().sampling_weight(wo);
            self.prob[i] = p;
            total += p;
        }
        for p in &mut self.prob[..self.n] {
            *p = if total > 0.0 { *p / total } else { 0.0 };
        }
        self.delta = self.n > 0 && self.lobes().all(|(_, l)| l.is_delta());
    }

    /// True if every lobe is a Dirac delta (no point in light sampling).
    pub fn is_delta(&self) -> bool {
        self.delta || self.n == 0
    }

    pub fn albedo(&self) -> Vec3 {
        let mut a = Vec3::ZERO;
        for (i, l) in self.lobes() {
            a += l.albedo() * self.weight[i];
        }
        a.max(Vec3::ZERO).min(Vec3::ONE)
    }

    pub fn eval(&self, wo: Vec3, wi: Vec3) -> (Vec3, f32) {
        let (mut f, mut pdf) = (Vec3::ZERO, 0.0);
        for (i, l) in self.lobes() {
            if l.is_delta() {
                continue;
            }
            let (lf, lp) = l.eval(wo, wi);
            f += lf * self.weight[i];
            pdf += lp * self.prob[i];
        }
        (f, pdf)
    }

    pub fn sample(&self, wo: Vec3, rng: &mut Rng) -> Option<BsdfSample> {
        if self.n == 0 {
            return None;
        }
        let u = rng.f32();
        let mut k = self.n - 1;
        let mut acc = 0.0;
        for i in 0..self.n {
            acc += self.prob[i];
            if u < acc {
                k = i;
                break;
            }
        }
        let p = self.prob[k];
        if p <= 0.0 {
            return None;
        }
        let lobe = self.lobes[k].as_ref().unwrap();
        let s = lobe.sample(wo, rng)?;
        if s.delta || self.n == 1 {
            let w = self.weight[k] / p;
            return Some(BsdfSample { wi: s.wi, weight: s.weight * w, pdf: s.pdf * p, delta: s.delta });
        }
        let (f, pdf) = self.eval(wo, s.wi);
        (pdf > 0.0).then(|| BsdfSample { wi: s.wi, weight: f / pdf, pdf, delta: false })
    }
}

// ---------------------------------------------------------------------------- microfacet helpers

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

/// Smith masking term for GGX (`cos` > 0).
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

/// Exact unpolarized Fresnel reflectance of a dielectric interface. `ior` is
/// eta_inside / eta_outside; a negative `cos_i` means the ray arrives from inside.
pub fn fr_dielectric(cos_i: f32, ior: f32) -> f32 {
    let (mut c, mut eta) = (cos_i.clamp(-1.0, 1.0), ior);
    if c < 0.0 {
        eta = 1.0 / eta;
        c = -c;
    }
    let sin2_t = (1.0 - c * c) / (eta * eta);
    if sin2_t >= 1.0 {
        return 1.0; // total internal reflection
    }
    let cos_t = (1.0 - sin2_t).sqrt();
    let r_parl = (eta * c - cos_t) / (eta * c + cos_t);
    let r_perp = (c - eta * cos_t) / (c + eta * cos_t);
    0.5 * (r_parl * r_parl + r_perp * r_perp)
}

/// Exact unpolarized Fresnel reflectance of a conductor with complex IOR eta + i k.
fn fr_conductor(cos_i: f32, eta: f32, k: f32) -> f32 {
    let c2 = cos_i.clamp(0.0, 1.0).powi(2);
    let s2 = 1.0 - c2;
    let (n2, k2) = (eta * eta, k * k);
    let t0 = n2 - k2 - s2;
    let a2b2 = (t0 * t0 + 4.0 * n2 * k2).sqrt();
    let a = (0.5 * (a2b2 + t0)).max(0.0).sqrt();
    let t1 = a2b2 + c2;
    let t2 = 2.0 * a * c2.sqrt();
    let rs = (t1 - t2) / (t1 + t2);
    let t3 = c2 * a2b2 + s2 * s2;
    let t4 = t2 * s2;
    let rp = rs * (t3 - t4) / (t3 + t4);
    0.5 * (rp + rs)
}

fn fr_conductor3(cos: f32, eta: Vec3, k: Vec3) -> Vec3 {
    Vec3::new(fr_conductor(cos, eta.x, k.x), fr_conductor(cos, eta.y, k.y), fr_conductor(cos, eta.z, k.z))
}

/// Refracts `wi` through a surface with normal `n` (either side). `ior` is
/// eta_inside / eta_outside relative to `n`. Returns the direction and the relative IOR used.
fn refract(wi: Vec3, n: Vec3, ior: f32) -> Option<(Vec3, f32)> {
    let (mut n, mut eta) = (n, ior);
    let mut cos_i = n.dot(wi);
    if cos_i < 0.0 {
        eta = 1.0 / eta;
        cos_i = -cos_i;
        n = -n;
    }
    let sin2_t = (1.0 - cos_i * cos_i).max(0.0) / (eta * eta);
    if sin2_t >= 1.0 {
        return None;
    }
    let cos_t = (1.0 - sin2_t).sqrt();
    Some(((-wi / eta + n * (cos_i / eta - cos_t)).normalize(), eta))
}

/// Walter et al. 2007 rough dielectric with visible-normal sampling (as in pbrt-v4,
/// without the radiance-mode `1 / eta^2` factor).
fn rough_dielectric_eval(wo: Vec3, wi: Vec3, ior: f32, a: f32, tint: Vec3) -> (Vec3, f32) {
    let zero = (Vec3::ZERO, 0.0);
    let (cos_o, cos_i) = (wo.z, wi.z);
    if cos_o == 0.0 || cos_i == 0.0 {
        return zero;
    }
    let reflect = cos_o * cos_i > 0.0;
    let etap = if reflect {
        1.0
    } else if cos_o > 0.0 {
        ior
    } else {
        1.0 / ior
    };
    let wm = wi * etap + wo;
    if wm.length_squared() == 0.0 {
        return zero;
    }
    let mut wm = wm.normalize();
    if wm.z < 0.0 {
        wm = -wm;
    }
    let (dot_i, dot_o) = (wm.dot(wi), wm.dot(wo));
    // Discard back-facing microfacets.
    if dot_i * cos_i < 0.0 || dot_o * cos_o < 0.0 {
        return zero;
    }
    let f = fr_dielectric(dot_o, ior);
    let d = ggx_d(wm.z, a);
    let g1o = smith_g1(cos_o.abs(), a);
    let g1i = smith_g1(cos_i.abs(), a);
    let pdf_wm = g1o * d * dot_o.abs() / cos_o.abs();
    if reflect {
        let fc = f * d * g1o * g1i / (4.0 * cos_o.abs());
        let pdf = pdf_wm / (4.0 * dot_o.abs()) * f;
        (Vec3::splat(fc), pdf)
    } else {
        let t = 1.0 - f;
        let denom = (dot_i + dot_o / etap).powi(2);
        if denom == 0.0 {
            return zero;
        }
        let fc = t * d * g1o * g1i * (dot_i * dot_o).abs() / (cos_o.abs() * denom);
        let pdf = pdf_wm * t * dot_i.abs() / denom;
        (tint * fc, pdf)
    }
}

/// Samples a GGX visible normal (Heitz 2018). `wo` must be in the upper hemisphere.
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

// ---------------------------------------------------------------------------- energy compensation

const E_RES: usize = 32;

/// Directional albedo E(mu, alpha) of single-scattering GGX with F = 1, and its
/// cosine-weighted average, for Kulla–Conty multiple-scattering compensation.
struct EnergyTable {
    e: [[f32; E_RES]; E_RES], // [alpha][mu]
    eavg: [f32; E_RES],
}

fn energy_table() -> &'static EnergyTable {
    static TABLE: OnceLock<EnergyTable> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut t = EnergyTable { e: [[1.0; E_RES]; E_RES], eavg: [1.0; E_RES] };
        let s = 32; // s * s stratified samples per entry
        for ai in 1..E_RES {
            let a = ai as f32 / (E_RES - 1) as f32;
            let a = a.max(MIN_ALPHA);
            for mi in 0..E_RES {
                let mu = (mi as f32 / (E_RES - 1) as f32).max(1e-3);
                let wo = Vec3::new((1.0 - mu * mu).sqrt(), 0.0, mu);
                let mut sum = 0.0;
                for i in 0..s {
                    for j in 0..s {
                        let h = sample_ggx_vndf(wo, a, (i as f32 + 0.5) / s as f32, (j as f32 + 0.5) / s as f32);
                        let wi = h * (2.0 * wo.dot(h)) - wo;
                        if wi.z > 0.0 {
                            sum += smith_g1(wi.z, a);
                        }
                    }
                }
                t.e[ai][mi] = (sum / (s * s) as f32).min(1.0);
            }
            // eavg = 2 * integral of E(mu) mu dmu (trapezoid over the mu grid).
            let mut acc = 0.0;
            for mi in 0..E_RES - 1 {
                let (m0, m1) = (mi as f32 / (E_RES - 1) as f32, (mi + 1) as f32 / (E_RES - 1) as f32);
                acc += 0.5 * (t.e[ai][mi] * m0 + t.e[ai][mi + 1] * m1) * (m1 - m0);
            }
            t.eavg[ai] = (2.0 * acc).min(1.0);
        }
        t
    })
}

impl EnergyTable {
    fn e(&self, mu: f32, a: f32) -> f32 {
        let fa = a.clamp(0.0, 1.0) * (E_RES - 1) as f32;
        let fm = mu.clamp(0.0, 1.0) * (E_RES - 1) as f32;
        let (ia, im) = ((fa as usize).min(E_RES - 2), (fm as usize).min(E_RES - 2));
        let (ta, tm) = (fa - ia as f32, fm - im as f32);
        let row = |i: usize| self.e[i][im] * (1.0 - tm) + self.e[i][im + 1] * tm;
        row(ia) * (1.0 - ta) + row(ia + 1) * ta
    }
    fn eavg(&self, a: f32) -> f32 {
        let fa = a.clamp(0.0, 1.0) * (E_RES - 1) as f32;
        let ia = (fa as usize).min(E_RES - 2);
        let t = fa - ia as f32;
        self.eavg[ia] * (1.0 - t) + self.eavg[ia + 1] * t
    }
}

/// Multiple-scattering Fresnel term F_ms = Favg^2 Eavg / (1 - Favg (1 - Eavg)), or
/// None when there is nothing to compensate.
fn ms_factor(favg: Vec3, a: f32) -> Option<Vec3> {
    if a < MIN_ALPHA {
        return None;
    }
    let eavg = energy_table().eavg(a);
    if eavg >= 0.999 {
        return None;
    }
    let f = |fa: f32| fa * fa * eavg / (1.0 - fa * (1.0 - eavg));
    Some(Vec3::new(f(favg.x), f(favg.y), f(favg.z)))
}

/// Probability of sampling the (cosine-distributed) multiple-scattering lobe.
#[inline]
fn ms_probability(eo: f32) -> f32 {
    (1.0 - eo).clamp(0.0, 0.5)
}

/// Hemispherical average of Fresnel reflectance seen from inside a dielectric with
/// relative IOR `ior` (light trapped under a coating), tabulated over ior in [1, 3].
fn fdr_internal(ior: f32) -> f32 {
    const N: usize = 64;
    static TABLE: OnceLock<[f32; N]> = OnceLock::new();
    let t = TABLE.get_or_init(|| {
        std::array::from_fn(|i| {
            let eta = 1.0 + 2.0 * i as f32 / (N - 1) as f32;
            let steps = 256;
            (0..steps)
                .map(|j| {
                    let mu = (j as f32 + 0.5) / steps as f32;
                    fr_dielectric(-mu, eta) * 2.0 * mu / steps as f32
                })
                .sum()
        })
    });
    let x = ((ior - 1.0) / 2.0).clamp(0.0, 1.0) * (N - 1) as f32;
    let i = (x as usize).min(N - 2);
    let f = x - i as f32;
    t[i] * (1.0 - f) + t[i + 1] * f
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wo() -> Vec3 {
        Vec3::new(0.4, 0.1, 0.9).normalize()
    }

    /// Checks sample() against eval() for non-delta samples and returns the
    /// estimated directional albedo (mean weight luminance).
    fn check(b: &Bsdf, wo: Vec3, n: usize) -> f32 {
        let mut rng = Rng::new(7, 0);
        let mut sum = 0.0;
        for _ in 0..n {
            let Some(s) = b.sample(wo, &mut rng) else { continue };
            assert!(s.weight.is_finite() && s.pdf > 0.0);
            if !s.delta {
                let (f, pdf) = b.eval(wo, s.wi);
                assert!((pdf - s.pdf).abs() <= 2e-3 * pdf.max(1.0), "pdf {pdf} vs {}", s.pdf);
                let w = f / pdf;
                assert!((w - s.weight).abs().max_elem() <= 2e-3 * s.weight.max_elem().max(1.0));
            }
            sum += s.weight.luminance();
        }
        sum / n as f32
    }

    /// Integrates the pdf over the sphere with uniform samples; must be <= 1.
    fn pdf_integral(b: &Bsdf, wo: Vec3) -> f32 {
        let n = 200_000;
        let mut rng = Rng::new(3, 1);
        let mut sum = 0.0;
        for _ in 0..n {
            let wi = crate::sampling::uniform_sphere(rng.f32(), rng.f32());
            sum += b.eval(wo, wi).1;
        }
        sum / n as f32 * 4.0 * PI
    }

    fn bsdf(lobe: Lobe, wo: Vec3) -> Bsdf {
        let mut b = Bsdf::default();
        b.push(lobe, 1.0);
        b.finish(wo);
        b
    }

    #[test]
    fn conductor_sampling_matches_eval_and_conserves_energy() {
        for a in [0.1, 0.25, 0.5, 1.0] {
            let single = bsdf(Lobe::Reflect { alpha: a, fresnel: Fresnel::Schlick(Vec3::ONE), ms: None }, wo());
            let e_ss = check(&single, wo(), 40_000);
            assert!(e_ss <= 1.0 + 1e-3, "single scattering albedo {e_ss}");
            let ms = ms_factor(Vec3::ONE, a);
            let multi = bsdf(Lobe::Reflect { alpha: a, fresnel: Fresnel::Schlick(Vec3::ONE), ms }, wo());
            let e_ms = check(&multi, wo(), 40_000);
            // A white rough conductor with compensation reflects (almost) everything.
            assert!((e_ms - 1.0).abs() < 0.03, "alpha {a}: compensated albedo {e_ms} (single {e_ss})");
            let pi = pdf_integral(&multi, wo());
            // Below 1: VNDF samples that reflect under the horizon are lost.
            assert!(pi <= 1.02 && pi > 0.7, "alpha {a}: pdf integral {pi}");
        }
    }

    #[test]
    fn rough_dielectric_is_consistent() {
        for (a, ior) in [(0.3f32, 1.5f32), (0.6, 1.33), (0.15, 2.0)] {
            for w in [wo(), -wo(), Vec3::new(0.8, 0.0, 0.6), Vec3::new(0.3, 0.0, -0.954)] {
                let b = bsdf(Lobe::Dielectric { ior, alpha: a, tint: Vec3::ONE }, w);
                let e = check(&b, w, 40_000);
                // Single-scattering microfacets lose energy, most of it from inside (TIR).
                assert!(e <= 1.0 + 1e-2 && e > 0.6, "a {a} ior {ior} wo {w:?}: albedo {e}");
                let pi = pdf_integral(&b, w);
                assert!(pi <= 1.03 && pi > 0.75, "a {a} ior {ior} wo {w:?}: pdf integral {pi}");
            }
        }
    }

    /// Generalized reciprocity of refraction: f(wo, wi) / eta_i^2 = f(wi, wo) / eta_o^2
    /// (for this importance-mode BSDF), and plain reciprocity for reflection.
    #[test]
    fn rough_dielectric_reciprocity() {
        let (ior, a) = (1.5f32, 0.4f32);
        let mut rng = Rng::new(11, 2);
        let mut checked = 0;
        for _ in 0..2000 {
            let wo = crate::sampling::uniform_sphere(rng.f32(), rng.f32());
            let wi = crate::sampling::uniform_sphere(rng.f32(), rng.f32());
            let (f1, _) = rough_dielectric_eval(wo, wi, ior, a, Vec3::ONE);
            let (f2, _) = rough_dielectric_eval(wi, wo, ior, a, Vec3::ONE);
            // eval returns f * |cos_i|; divide the cosines back out.
            let (f1, f2) = (f1.x / wi.z.abs(), f2.x / wo.z.abs());
            if f1 == 0.0 && f2 == 0.0 {
                continue;
            }
            let eta = |w: Vec3| if w.z > 0.0 { 1.0 } else { ior };
            let (l, r) = (f1 / eta(wi).powi(2), f2 / eta(wo).powi(2));
            assert!((l - r).abs() <= 1e-3 * l.max(r).max(1.0), "{wo:?} {wi:?}: {l} vs {r}");
            checked += 1;
        }
        assert!(checked > 100);
    }

    #[test]
    fn plastic_and_principled_conserve_energy() {
        for a in [0.0f32, 0.2, 0.7] {
            let mut b = Bsdf::default();
            b.push(Lobe::Reflect { alpha: a, fresnel: Fresnel::Dielectric(1.5), ms: None }, 1.0);
            b.push(Lobe::CoatedDiffuse { albedo: Vec3::ONE, ior: 1.5, fdr: fdr_internal(1.5) }, 1.0);
            b.finish(wo());
            let e = check(&b, wo(), 60_000);
            // White plastic: smooth coat is exactly energy conserving, rough loses a little.
            assert!(e <= 1.02 && e > 0.9, "alpha {a}: plastic albedo {e}");
        }
        let p = Principled {
            base_color: Texture::Const(Vec3::new(0.8, 0.5, 0.3)),
            metallic: Texture::Const(Vec3::splat(0.3)),
            roughness: Texture::Const(Vec3::splat(0.4)),
            specular: Texture::Const(Vec3::splat(0.5)),
            ior: None,
            transmission: Texture::Const(Vec3::splat(0.3)),
            clearcoat: Texture::Const(Vec3::splat(0.5)),
            clearcoat_roughness: Texture::Const(Vec3::splat(0.1)),
            sheen: Texture::Const(Vec3::splat(0.5)),
            sheen_tint: Texture::Const(Vec3::splat(0.5)),
        };
        let m = Material::new(MaterialKind::Principled(Box::new(p)));
        let b = m.bsdf(&TexCtx::default(), wo());
        assert_eq!(b.n, 6);
        let e = check(&b, wo(), 60_000);
        assert!(e < 1.05 && e > 0.2, "principled albedo {e}");
    }

    #[test]
    fn fresnel_limits() {
        assert!((fr_dielectric(1.0, 1.5) - 0.04).abs() < 1e-3);
        assert_eq!(fr_dielectric(-0.1, 1.5), 1.0); // TIR from inside
        assert!((fr_dielectric(-1.0, 1.5) - 0.04).abs() < 1e-3);
        // A dielectric conductor (k = 0) matches the dielectric formula.
        assert!((fr_conductor(0.6, 1.5, 0.0) - fr_dielectric(0.6, 1.5)).abs() < 1e-4);
        assert!((fdr_internal(1.5) - 0.596).abs() < 0.01, "{}", fdr_internal(1.5));
    }

    #[test]
    fn measured_metals_match_reference_f0() {
        // Linear F0 from Real-Time Rendering 4th ed., table 9.2.
        let refs = [
            ("gold", Vec3::new(1.000, 0.766, 0.336)),
            ("silver", Vec3::new(0.972, 0.960, 0.915)),
            ("copper", Vec3::new(0.955, 0.638, 0.538)),
            ("aluminium", Vec3::new(0.913, 0.922, 0.924)),
            ("iron", Vec3::new(0.562, 0.565, 0.578)),
            ("chromium", Vec3::new(0.549, 0.556, 0.554)),
            ("platinum", Vec3::new(0.673, 0.637, 0.585)),
        ];
        for (name, f0) in refs {
            let (eta, k) = metal_preset(name).unwrap();
            let got = fr_conductor3(1.0, eta, k);
            // Published iron measurements disagree with this reference in the blue.
            let tol = if name == "iron" { 0.1 } else { 0.07 };
            assert!((got - f0).abs().max_elem() < tol, "{name}: {got:?} vs {f0:?}");
        }
    }
}
