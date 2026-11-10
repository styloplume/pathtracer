//! Light sources and light selection.
//!
//! Lights are emissive primitives of the static world geometry (area lights; emitters
//! inside instances are only found by BSDF sampling), point / spot lights, distant lights
//! (directional, optionally with an angular size, e.g. the sun) and the environment.
//! Next-event estimation first picks a light, then samples a direction towards it:
//!
//! - spheres: uniform in the cone they subtend (solid-angle sampling); disks, cylinders
//!   and boxes: uniform by area;
//! - triangles: uniform in their spherical triangle (Arvo), falling back to area
//!   sampling for tiny or huge solid angles;
//! - distant lights with a size: uniform in their cone; the environment: by luminance.
//!
//! Light selection uses a light BVH (Conty Estevez & Kulla 2018, as in pbrt-v4) that
//! estimates each subtree's contribution at the shading point from its bounds, emitted
//! power and emission-direction cone, so scenes with thousands of emitters mostly pick
//! nearby, facing lights. Infinite lights are chosen with a fixed probability.

use crate::environment::Environment;
use crate::geometry::Aabb;
use crate::math::{Frame, PI, Ray, Vec3, offset_origin};
use crate::sampler::Sampler;
use crate::sampling::{
    ONE_MINUS_EPSILON, angle_between, sample_spherical_triangle, spherical_triangle_area, uniform_cone,
    uniform_triangle,
};
use crate::scene::{Hit, Scene};
use crate::shapes::{ShapeKind, sphere_param};
use crate::texture::TexCtx;

pub enum Light {
    /// An emissive triangle or sphere (scene primitive id).
    Area {
        prim: u32,
    },
    /// Isotropic point light; `intensity` is radiant intensity (radiance x area).
    Point {
        pos: Vec3,
        intensity: Vec3,
    },
    /// Spot light shining along `dir`, full intensity inside `cos_inner`, falling off
    /// smoothly to zero at `cos_outer`.
    Spot {
        pos: Vec3,
        dir: Vec3,
        intensity: Vec3,
        cos_inner: f32,
        cos_outer: f32,
    },
    /// Light from direction `dir` (unit, pointing towards the light). With `cos_max` = 1
    /// it is a delta light and `radiance` is the irradiance on a perpendicular surface;
    /// otherwise it is a uniformly bright disk of angular radius acos(`cos_max`).
    Distant {
        dir: Vec3,
        radiance: Vec3,
        cos_max: f32,
    },
    Environment(Box<Environment>),
}

impl Light {
    pub fn is_infinite(&self) -> bool {
        matches!(self, Light::Distant { .. } | Light::Environment(_))
    }

    fn is_delta_distant(cos_max: f32) -> bool {
        cos_max >= 1.0 - 1e-7
    }

    /// Radiance carried by a ray escaping the scene in direction `d` (infinite lights).
    pub fn le(&self, d: Vec3) -> Vec3 {
        match self {
            Light::Distant { dir, radiance, cos_max }
                if !Self::is_delta_distant(*cos_max) && d.dot(*dir) >= *cos_max =>
            {
                *radiance
            }
            Light::Environment(env) => env.eval(d),
            _ => Vec3::ZERO,
        }
    }

    /// Solid-angle pdf of sampling escaping direction `d` (infinite lights, no selection).
    fn pdf_dir(&self, d: Vec3) -> f32 {
        match self {
            Light::Distant { dir, cos_max, .. } if !Self::is_delta_distant(*cos_max) && d.dot(*dir) >= *cos_max => {
                1.0 / (2.0 * PI * (1.0 - cos_max))
            }
            Light::Environment(env) => env.pdf(d),
            _ => 0.0,
        }
    }
}

pub struct LightSample {
    pub wi: Vec3,
    /// Distance to the sampled point; infinite for infinite lights.
    pub dist: f32,
    pub radiance: Vec3,
    /// Solid-angle pdf including the light selection probability (for delta lights,
    /// the selection probability alone).
    pub pdf: f32,
    pub delta: bool,
}

/// Triangles subtending less (or more) solid angle than this are area sampled; the
/// spherical construction loses precision there (pbrt-v4's limits).
const MIN_SPHERICAL_AREA: f32 = 3e-4;
const MAX_SPHERICAL_AREA: f32 = 6.22;
/// Marks primitives that are not lights.
pub const NO_LIGHT: u32 = u32::MAX;

#[inline]
fn smoothstep(x: f32, a: f32, b: f32) -> f32 {
    if b <= a {
        return if x < a { 0.0 } else { 1.0 };
    }
    let t = ((x - a) / (b - a)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// The cone a sphere subtends from `p`: (cos of its half angle, 1 - that cos), with a
/// series expansion for tiny cones. None if `p` is inside the sphere.
fn sphere_cone(center: Vec3, radius: f32, p: Vec3) -> Option<(f32, f32)> {
    let dc2 = (p - center).length_squared();
    let r2 = radius * radius;
    if dc2 <= r2 * 1.0001 {
        return None;
    }
    let sin2 = r2 / dc2;
    let cos_max = (1.0 - sin2).max(0.0).sqrt();
    // sin^2(1.5 deg): below this 1 - cos_max suffers from cancellation.
    let one_minus = if sin2 < 0.000_685_23 { sin2 / 2.0 } else { 1.0 - cos_max };
    Some((cos_max, one_minus))
}

impl Scene {
    pub fn lights(&self) -> &[Light] {
        &self.lights
    }

    pub fn infinite_lights(&self) -> &[u32] {
        &self.light_sampler.infinite
    }

    pub fn has_lights(&self) -> bool {
        !self.lights.is_empty()
    }

    /// Shadow test for light sample `ls` taken at surface point `p` (geometric normal `ng`).
    /// The ray leaves from the offset origin and aims at the sampled point itself: reusing
    /// `ls.wi` from the offset origin would miss the light point by the offset and could
    /// hit the light's own surface first when the light is very close.
    pub fn occluded_light(&self, p: Vec3, ng: Vec3, ls: &LightSample, time: f32) -> bool {
        let origin = offset_origin(p, ng, ls.wi);
        if !ls.dist.is_finite() {
            return self.occluded(&Ray::new(origin, ls.wi, time), f32::INFINITY);
        }
        let d = (p + ls.wi * ls.dist) - origin;
        let dist = d.length();
        dist > 0.0 && self.occluded(&Ray::new(origin, d * (1.0 / dist), time), dist * (1.0 - 1e-3))
    }

    /// Picks a light for shading point `p` with normal `n` (zero if none) and samples
    /// a direction towards it.
    pub fn sample_light(&self, p: Vec3, n: Vec3, s: &mut Sampler) -> Option<LightSample> {
        let u = s.get1d();
        let (u1, u2) = s.get2d();
        let (li, pmf) = self.light_sampler.sample(p, n, u)?;
        let mut ls = self.sample_one_light(li, p, u1, u2)?;
        ls.pdf *= pmf;
        (ls.pdf > 0.0 && !ls.radiance.is_black()).then_some(ls)
    }

    pub(crate) fn sample_one_light(&self, li: u32, p: Vec3, u1: f32, u2: f32) -> Option<LightSample> {
        match &self.lights[li as usize] {
            Light::Area { prim } => self.sample_area_light(*prim, p, u1, u2),
            Light::Point { pos, intensity } => {
                let d = *pos - p;
                let dist2 = d.length_squared();
                (dist2 > 0.0).then(|| {
                    let dist = dist2.sqrt();
                    LightSample {
                        wi: d * (1.0 / dist),
                        dist,
                        radiance: *intensity * (1.0 / dist2),
                        pdf: 1.0,
                        delta: true,
                    }
                })
            }
            Light::Spot { pos, dir, intensity, cos_inner, cos_outer } => {
                let d = *pos - p;
                let dist2 = d.length_squared();
                if dist2 <= 0.0 {
                    return None;
                }
                let dist = dist2.sqrt();
                let wi = d * (1.0 / dist);
                let falloff = smoothstep(-wi.dot(*dir), *cos_outer, *cos_inner);
                Some(LightSample { wi, dist, radiance: *intensity * (falloff / dist2), pdf: 1.0, delta: true })
            }
            Light::Distant { dir, radiance, cos_max } => {
                if Light::is_delta_distant(*cos_max) {
                    return Some(LightSample {
                        wi: *dir,
                        dist: f32::INFINITY,
                        radiance: *radiance,
                        pdf: 1.0,
                        delta: true,
                    });
                }
                let wi = Frame::from_normal(*dir).to_world(uniform_cone(u1, u2, *cos_max));
                let pdf = 1.0 / (2.0 * PI * (1.0 - cos_max));
                Some(LightSample { wi, dist: f32::INFINITY, radiance: *radiance, pdf, delta: false })
            }
            Light::Environment(env) => {
                let (wi, pdf) = env.sample(u1, u2)?;
                Some(LightSample { wi, dist: f32::INFINITY, radiance: env.eval(wi), pdf, delta: false })
            }
        }
    }

    fn sample_area_light(&self, prim: u32, p: Vec3, u1: f32, u2: f32) -> Option<LightSample> {
        let (lp, ln, uv, mat, pdf_solid) = if let Some(s) = self.shape(prim) {
            let cone = match s.kind {
                ShapeKind::Sphere { center, radius } => sphere_cone(center, radius, p).map(|c| (center, radius, c)),
                _ => None,
            };
            if let Some((center, radius, (cos_max, one_minus))) = cone {
                // Sample the cone, then find the point on the sphere it hits (pbrt-v4).
                let dc = (p - center).length();
                let sin_max = radius / dc;
                let sin2_max = sin_max * sin_max;
                let (cos_t, sin2_t) = if sin2_max < 0.000_685_23 {
                    let s2 = sin2_max * u1;
                    ((1.0 - s2).sqrt(), s2)
                } else {
                    let c = (cos_max - 1.0) * u1 + 1.0;
                    (c, 1.0 - c * c)
                };
                let cos_alpha = sin2_t / sin_max + cos_t * (1.0 - sin2_t / sin2_max).max(0.0).sqrt();
                let sin_alpha = (1.0 - cos_alpha * cos_alpha).max(0.0).sqrt();
                let phi = u2 * 2.0 * PI;
                let w = Vec3::new(sin_alpha * phi.cos(), sin_alpha * phi.sin(), cos_alpha);
                let n = Frame::from_normal((center - p) * (1.0 / dc)).to_world(-w);
                let lp = center + n * radius;
                (lp, n, sphere_param(n, radius).0, s.material, Some(1.0 / (2.0 * PI * one_minus)))
            } else {
                let (lp, n) = s.kind.sample(u1, u2);
                (lp, n, s.kind.surface(lp).uv, s.material, None)
            }
        } else {
            let tri = &self.triangles[prim as usize];
            let (p0, p1, p2) = self.tri_points(tri);
            let omega = spherical_triangle_area([p0, p1, p2], p);
            let (b1, b2, pdf_solid) = if (MIN_SPHERICAL_AREA..=MAX_SPHERICAL_AREA).contains(&omega) {
                let (b1, b2) = sample_spherical_triangle([p0, p1, p2], p, u1, u2)?;
                (b1, b2, Some(1.0 / omega))
            } else {
                let (b1, b2) = uniform_triangle(u1, u2);
                (b1, b2, None)
            };
            let lp = p0 * (1.0 - b1 - b2) + p1 * b1 + p2 * b2;
            let uv = self.hit_uv(&Hit { t: 0.0, prim, inst: 0, b1, b2 }, lp);
            (lp, self.tri_normals(tri, b1, b2).0, uv, tri.material, pdf_solid)
        };
        let (radiance, two_sided) = self.materials[mat as usize].emission(&TexCtx { uv, p: lp })?;
        let d = lp - p;
        let dist2 = d.length_squared();
        if dist2 <= 0.0 {
            return None;
        }
        let dist = dist2.sqrt();
        let wi = d * (1.0 / dist);
        let mut cos_l = -wi.dot(ln);
        if two_sided {
            cos_l = cos_l.abs();
        }
        if cos_l <= 1e-6 {
            return None;
        }
        let pdf = pdf_solid.unwrap_or_else(|| dist2 / (cos_l * self.prim_area(prim)));
        Some(LightSample { wi, dist, radiance, pdf, delta: false })
    }

    /// Solid-angle pdf with which `sample_light` at (`p`, `n`) would have produced the
    /// point `pl` (geometric normal `nl`) on emissive primitive `prim`.
    pub fn area_light_pdf(&self, p: Vec3, n: Vec3, prim: u32, pl: Vec3, nl: Vec3) -> f32 {
        let li = self.prim_light[prim as usize];
        if li == NO_LIGHT {
            return 0.0;
        }
        let pmf = self.light_sampler.pmf(p, n, li);
        if pmf <= 0.0 {
            return 0.0;
        }
        let d = pl - p;
        let dist2 = d.length_squared();
        let area_pdf = || {
            let cos_l = (d.dot(nl) / dist2.sqrt()).abs();
            dist2 / (cos_l.max(1e-8) * self.prim_area(prim))
        };
        let pdf = match self.shape(prim).map(|s| s.kind) {
            None => {
                let (p0, p1, p2) = self.tri_points(&self.triangles[prim as usize]);
                let omega = spherical_triangle_area([p0, p1, p2], p);
                if (MIN_SPHERICAL_AREA..=MAX_SPHERICAL_AREA).contains(&omega) { 1.0 / omega } else { area_pdf() }
            }
            Some(ShapeKind::Sphere { center, radius }) => {
                sphere_cone(center, radius, p).map_or_else(area_pdf, |(_, om)| 1.0 / (2.0 * PI * om))
            }
            Some(_) => area_pdf(),
        };
        pmf * pdf
    }

    /// Solid-angle pdf of sampling escaping direction `d` from infinite light `li`.
    pub fn infinite_light_pdf(&self, p: Vec3, n: Vec3, li: u32, d: Vec3) -> f32 {
        self.light_sampler.pmf(p, n, li) * self.lights[li as usize].pdf_dir(d)
    }

    /// Bounds for the light BVH; None for infinite lights and lights without power.
    fn light_bounds(&self, light: &Light) -> Option<LightBounds> {
        let lb = match light {
            Light::Area { prim } => {
                let mat = &self.materials[self.prim_material(*prim) as usize];
                let (two_sided, lum) = match mat.emission(&TexCtx::default()) {
                    Some((_, two)) => (two, mat.emission_mean().luminance()),
                    None => return None,
                };
                let phi = lum * self.prim_area(*prim) * if two_sided { 2.0 } else { 1.0 };
                // Flat emitters emit into the hemisphere of their normal, curved ones everywhere.
                let (w, cos_o) = match self.shape(*prim).map(|s| s.kind) {
                    None => (self.tri_normals(&self.triangles[*prim as usize], 1.0 / 3.0, 1.0 / 3.0).0, 1.0),
                    Some(ShapeKind::Disk { frame, .. }) => (frame.n, 1.0),
                    Some(_) => (Vec3::Z, -1.0),
                };
                LightBounds { bounds: self.prim_bounds(*prim), w, phi, cos_o, cos_e: 0.0, two_sided }
            }
            Light::Point { pos, intensity } => LightBounds {
                bounds: Aabb { min: *pos, max: *pos },
                w: Vec3::Z,
                phi: 4.0 * PI * intensity.luminance(),
                cos_o: -1.0,
                cos_e: 0.0,
                two_sided: false,
            },
            Light::Spot { pos, dir, intensity, cos_inner, cos_outer } => LightBounds {
                bounds: Aabb { min: *pos, max: *pos },
                w: *dir,
                phi: 4.0 * PI * intensity.luminance(),
                cos_o: *cos_inner,
                cos_e: (cos_outer.clamp(-1.0, 1.0).acos() - cos_inner.clamp(-1.0, 1.0).acos()).cos(),
                two_sided: false,
            },
            Light::Distant { .. } | Light::Environment(_) => return None,
        };
        (lb.phi > 0.0 && lb.phi.is_finite()).then_some(lb)
    }

    /// Adds area lights for the emissive primitives among `prims` (world-space ones)
    /// and builds the light sampler.
    pub(crate) fn build_lights(&mut self, kind: LightSamplerKind, prims: &[u32]) {
        self.prim_light = vec![NO_LIGHT; self.num_prims()];
        for &prim in prims {
            let mat = &self.materials[self.prim_material(prim) as usize];
            if mat.is_emissive() && mat.emission_mean().luminance() > 0.0 && self.prim_area(prim) > 0.0 {
                self.prim_light[prim as usize] = self.lights.len() as u32;
                self.lights.push(Light::Area { prim });
            }
        }
        self.point_lights = (0..self.lights.len() as u32)
            .filter(|&i| matches!(self.lights[i as usize], Light::Point { .. } | Light::Spot { .. }))
            .collect();
        let mut infinite = Vec::new();
        let mut bounded = Vec::new();
        for (i, l) in self.lights.iter().enumerate() {
            if l.is_infinite() {
                infinite.push(i as u32);
            } else if let Some(lb) = self.light_bounds(l) {
                bounded.push((i as u32, lb));
            }
        }
        self.light_sampler = LightSampler::new(kind, infinite, bounded, self.lights.len());
    }
}

// ---------------------------------------------------------------------------- light sampler

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum LightSamplerKind {
    /// Light BVH: spatially and directionally aware.
    #[default]
    Bvh,
    /// Proportional to emitted power.
    Power,
    /// Every light equally likely.
    Uniform,
}

impl LightSamplerKind {
    pub fn parse(s: &str) -> Option<LightSamplerKind> {
        match s {
            "bvh" => Some(LightSamplerKind::Bvh),
            "power" => Some(LightSamplerKind::Power),
            "uniform" => Some(LightSamplerKind::Uniform),
            _ => None,
        }
    }
}

enum Strategy {
    Bvh(LightBvh),
    /// Discrete distribution over the bounded lights: (light ids, cdf, pmf by light id).
    Table {
        ids: Vec<u32>,
        cdf: Vec<f32>,
        pmf: Vec<f32>,
    },
}

#[derive(Default)]
pub struct LightSampler {
    infinite: Vec<u32>,
    /// Probability of picking an infinite light (shared equally among them).
    p_infinite: f32,
    strategy: Option<Strategy>,
}

impl LightSampler {
    fn new(kind: LightSamplerKind, infinite: Vec<u32>, bounded: Vec<(u32, LightBounds)>, n_lights: usize) -> Self {
        let n_inf = infinite.len() as f32;
        let p_infinite =
            if bounded.is_empty() { if infinite.is_empty() { 0.0 } else { 1.0 } } else { n_inf / (n_inf + 1.0) };
        let strategy = if bounded.is_empty() {
            None
        } else if kind == LightSamplerKind::Bvh {
            Some(Strategy::Bvh(LightBvh::build(bounded, n_lights)))
        } else {
            let w: Vec<f64> = bounded
                .iter()
                .map(|(_, lb)| if kind == LightSamplerKind::Power { f64::from(lb.phi) } else { 1.0 })
                .collect();
            let total: f64 = w.iter().sum();
            let mut acc = 0.0;
            let mut cdf = Vec::with_capacity(w.len());
            let mut pmf = vec![0.0; n_lights];
            for (i, (id, _)) in bounded.iter().enumerate() {
                acc += w[i];
                cdf.push((acc / total) as f32);
                pmf[*id as usize] = (w[i] / total) as f32;
            }
            Some(Strategy::Table { ids: bounded.iter().map(|(id, _)| *id).collect(), cdf, pmf })
        };
        LightSampler { infinite, p_infinite, strategy }
    }

    /// Picks a light: (light id, probability).
    fn sample(&self, p: Vec3, n: Vec3, u: f32) -> Option<(u32, f32)> {
        if u < self.p_infinite {
            let k = self.infinite.len();
            let i = ((u / self.p_infinite * k as f32) as usize).min(k - 1);
            return Some((self.infinite[i], self.p_infinite / k as f32));
        }
        let u = ((u - self.p_infinite) / (1.0 - self.p_infinite)).min(ONE_MINUS_EPSILON);
        let scale = 1.0 - self.p_infinite;
        match self.strategy.as_ref()? {
            Strategy::Bvh(bvh) => bvh.sample(p, n, u).map(|(id, pmf)| (id, pmf * scale)),
            Strategy::Table { ids, cdf, pmf } => {
                let i = cdf.partition_point(|&c| c <= u).min(ids.len() - 1);
                Some((ids[i], pmf[ids[i] as usize] * scale))
            }
        }
    }

    /// Probability that `sample` at (`p`, `n`) picks light `id`.
    fn pmf(&self, p: Vec3, n: Vec3, id: u32) -> f32 {
        if self.infinite.contains(&id) {
            return self.p_infinite / self.infinite.len() as f32;
        }
        let scale = 1.0 - self.p_infinite;
        match &self.strategy {
            None => 0.0,
            Some(Strategy::Bvh(bvh)) => bvh.pmf(p, n, id) * scale,
            Some(Strategy::Table { pmf, .. }) => pmf[id as usize] * scale,
        }
    }
}

// ---------------------------------------------------------------------------- light BVH

/// Spatial and directional bounds of a set of emitters.
#[derive(Clone, Copy, Debug)]
pub struct LightBounds {
    pub bounds: Aabb,
    /// Axis of the cone bounding the surface normals (emission directions).
    pub w: Vec3,
    /// Emitted power (luminance-weighted, up to a constant).
    pub phi: f32,
    /// Cosine of the normal cone's half angle.
    pub cos_o: f32,
    /// Cosine of the angle beyond the normal cone up to which light is emitted.
    pub cos_e: f32,
    pub two_sided: bool,
}

/// Rotates `v` by `angle` around unit `axis` (Rodrigues).
fn rotate(v: Vec3, axis: Vec3, angle: f32) -> Vec3 {
    let (s, c) = angle.sin_cos();
    v * c + axis.cross(v) * s + axis * (axis.dot(v) * (1.0 - c))
}

/// Smallest cone containing two cones (pbrt-v4 `Union(DirectionCone, DirectionCone)`).
fn cone_union(wa: Vec3, cos_a: f32, wb: Vec3, cos_b: f32) -> (Vec3, f32) {
    let (ta, tb) = (cos_a.clamp(-1.0, 1.0).acos(), cos_b.clamp(-1.0, 1.0).acos());
    let td = angle_between(wa, wb);
    if (td + tb).min(PI) <= ta {
        return (wa, cos_a);
    }
    if (td + ta).min(PI) <= tb {
        return (wb, cos_b);
    }
    let to = (ta + td + tb) / 2.0;
    if to >= PI {
        return (Vec3::Z, -1.0);
    }
    let axis = wa.cross(wb);
    if axis.length_squared() == 0.0 {
        return (Vec3::Z, -1.0);
    }
    (rotate(wa, axis.normalize(), to - ta).normalize(), to.cos())
}

#[inline]
fn cos_sub_clamped(sin_a: f32, cos_a: f32, sin_b: f32, cos_b: f32) -> f32 {
    if cos_a > cos_b { 1.0 } else { cos_a * cos_b + sin_a * sin_b }
}

#[inline]
fn sin_sub_clamped(sin_a: f32, cos_a: f32, sin_b: f32, cos_b: f32) -> f32 {
    if cos_a > cos_b { 0.0 } else { sin_a * cos_b - cos_a * sin_b }
}

#[inline]
fn safe_sqrt(x: f32) -> f32 {
    x.max(0.0).sqrt()
}

impl LightBounds {
    fn union(a: &LightBounds, b: &LightBounds) -> LightBounds {
        if a.phi == 0.0 {
            return *b;
        }
        if b.phi == 0.0 {
            return *a;
        }
        let (w, cos_o) = cone_union(a.w, a.cos_o, b.w, b.cos_o);
        let mut bounds = a.bounds;
        bounds.grow(&b.bounds);
        LightBounds {
            bounds,
            w,
            phi: a.phi + b.phi,
            cos_o,
            cos_e: a.cos_e.min(b.cos_e),
            two_sided: a.two_sided || b.two_sided,
        }
    }

    const EMPTY: LightBounds =
        LightBounds { bounds: Aabb::EMPTY, w: Vec3::Z, phi: 0.0, cos_o: 1.0, cos_e: 1.0, two_sided: false };

    /// Conservative estimate of the light reaching point `p` with normal `n` (zero
    /// normal: a point in space), pbrt-v4's `LightBounds::Importance`.
    fn importance(&self, p: Vec3, n: Vec3) -> f32 {
        let pc = self.bounds.centroid();
        let diag = (self.bounds.max - self.bounds.min).length();
        let d2 = (p - pc).length_squared().max(diag * 0.5);
        let to_p = p - pc;
        let len = to_p.length();
        let wi = if len > 0.0 { to_p * (1.0 / len) } else { Vec3::Z };
        let mut cos_w = self.w.dot(wi);
        if self.two_sided {
            cos_w = cos_w.abs();
        }
        let sin_w = safe_sqrt(1.0 - cos_w * cos_w);
        // Angle subtended by the bounds' bounding sphere.
        let r = diag * 0.5;
        let cos_b = if len * len < r * r { -1.0 } else { safe_sqrt(1.0 - r * r / (len * len)) };
        let sin_b = safe_sqrt(1.0 - cos_b * cos_b);
        let sin_o = safe_sqrt(1.0 - self.cos_o * self.cos_o);
        let cos_x = cos_sub_clamped(sin_w, cos_w, sin_o, self.cos_o);
        let sin_x = sin_sub_clamped(sin_w, cos_w, sin_o, self.cos_o);
        let cos_p = cos_sub_clamped(sin_x, cos_x, sin_b, cos_b);
        if cos_p <= self.cos_e {
            return 0.0;
        }
        let mut importance = self.phi * cos_p / d2;
        if n != Vec3::ZERO {
            let cos_i = wi.dot(n).abs();
            let sin_i = safe_sqrt(1.0 - cos_i * cos_i);
            importance *= cos_sub_clamped(sin_i, cos_i, sin_b, cos_b);
        }
        importance.max(0.0)
    }

    /// Split cost of a cluster (pbrt-v4 `EvaluateCost`): power times the solid angle
    /// of its emission times its size, with a penalty for thin boxes along `dim`.
    fn cost(&self, total: &Aabb, dim: usize) -> f32 {
        if self.phi == 0.0 {
            return 0.0;
        }
        let theta_o = self.cos_o.clamp(-1.0, 1.0).acos();
        let theta_e = self.cos_e.clamp(-1.0, 1.0).acos();
        let theta_w = (theta_o + theta_e).min(PI);
        let sin_o = theta_o.sin();
        let m_omega = 2.0 * PI * (1.0 - self.cos_o)
            + PI / 2.0 * (2.0 * theta_w * sin_o - (theta_o - 2.0 * theta_w).cos() - 2.0 * theta_o * sin_o + self.cos_o);
        let d = total.max - total.min;
        let kr = if d[dim] > 0.0 { d.max_elem() / d[dim] } else { 1.0 };
        let e = self.bounds.max - self.bounds.min;
        // The diagonal term keeps clusters of points (zero area) from being free.
        let size = self.bounds.surface_area() + 0.01 * e.length_squared();
        self.phi * m_omega * kr * size
    }
}

#[derive(Clone, Copy)]
struct LightNode {
    lb: LightBounds,
    /// Leaf: light id. Interior: index of the second child (the first follows the node).
    index: u32,
    leaf: bool,
}

struct LightBvh {
    nodes: Vec<LightNode>,
    /// Per light id: the path from the root (bit `d` set = second child at depth `d`).
    trails: Vec<u64>,
}

const BUCKETS: usize = 12;

impl LightBvh {
    fn build(mut items: Vec<(u32, LightBounds)>, n_lights: usize) -> LightBvh {
        let mut bvh = LightBvh { nodes: Vec::with_capacity(2 * items.len()), trails: vec![0; n_lights] };
        bvh.build_rec(&mut items, 0, 0);
        bvh
    }

    fn build_rec(&mut self, items: &mut [(u32, LightBounds)], trail: u64, depth: u32) -> usize {
        let node = self.nodes.len();
        if let [(id, lb)] = items {
            self.nodes.push(LightNode { lb: *lb, index: *id, leaf: true });
            self.trails[*id as usize] = trail;
            return node;
        }
        let mut bounds = Aabb::EMPTY;
        let mut cb = Aabb::EMPTY;
        let mut all = LightBounds::EMPTY;
        for (_, lb) in items.iter() {
            bounds.grow(&lb.bounds);
            cb.grow_point(lb.bounds.centroid());
            all = LightBounds::union(&all, lb);
        }
        let bucket = |c: f32, dim: usize| {
            let ext = cb.max[dim] - cb.min[dim];
            (((c - cb.min[dim]) / ext * BUCKETS as f32) as usize).min(BUCKETS - 1)
        };
        let mut best = (f32::INFINITY, 0usize, 0usize);
        // Deep trees would overflow the 64-bit trails; split evenly there instead.
        if depth < 32 {
            for dim in 0..3 {
                if cb.max[dim] <= cb.min[dim] {
                    continue;
                }
                let mut buckets = [LightBounds::EMPTY; BUCKETS];
                for (_, lb) in items.iter() {
                    let b = bucket(lb.bounds.centroid()[dim], dim);
                    buckets[b] = LightBounds::union(&buckets[b], lb);
                }
                for split in 0..BUCKETS - 1 {
                    let below = buckets[..=split].iter().fold(LightBounds::EMPTY, |a, b| LightBounds::union(&a, b));
                    let above = buckets[split + 1..].iter().fold(LightBounds::EMPTY, |a, b| LightBounds::union(&a, b));
                    if below.phi == 0.0 || above.phi == 0.0 {
                        continue;
                    }
                    let cost = below.cost(&bounds, dim) + above.cost(&bounds, dim);
                    if cost < best.0 {
                        best = (cost, dim, split);
                    }
                }
            }
        }
        let mut mid = if best.0.is_finite() {
            let (_, dim, split) = best;
            let mut i = 0;
            for j in 0..items.len() {
                if bucket(items[j].1.bounds.centroid()[dim], dim) <= split {
                    items.swap(i, j);
                    i += 1;
                }
            }
            i
        } else {
            items.len() / 2
        };
        if mid == 0 || mid == items.len() {
            mid = items.len() / 2;
        }
        self.nodes.push(LightNode { lb: all, index: 0, leaf: false });
        let (left, right) = items.split_at_mut(mid);
        self.build_rec(left, trail, depth + 1);
        let second = self.build_rec(right, trail | (1 << depth), depth + 1);
        self.nodes[node].index = second as u32;
        node
    }

    fn sample(&self, p: Vec3, n: Vec3, mut u: f32) -> Option<(u32, f32)> {
        let mut idx = 0;
        let mut pmf = 1.0;
        loop {
            let node = &self.nodes[idx];
            if node.leaf {
                return (idx > 0 || node.lb.importance(p, n) > 0.0).then_some((node.index, pmf));
            }
            let (c0, c1) = (idx + 1, node.index as usize);
            let (i0, i1) = (self.nodes[c0].lb.importance(p, n), self.nodes[c1].lb.importance(p, n));
            if i0 <= 0.0 && i1 <= 0.0 {
                return None;
            }
            let p0 = i0 / (i0 + i1);
            if u < p0 {
                idx = c0;
                pmf *= p0;
                u = (u / p0).min(ONE_MINUS_EPSILON);
            } else {
                idx = c1;
                pmf *= 1.0 - p0;
                u = ((u - p0) / (1.0 - p0)).min(ONE_MINUS_EPSILON);
            }
        }
    }

    fn pmf(&self, p: Vec3, n: Vec3, id: u32) -> f32 {
        let mut trail = self.trails[id as usize];
        let mut idx = 0;
        let mut pmf = 1.0;
        loop {
            let node = &self.nodes[idx];
            if node.leaf {
                return if node.index == id { pmf } else { 0.0 };
            }
            let (c0, c1) = (idx + 1, node.index as usize);
            let (i0, i1) = (self.nodes[c0].lb.importance(p, n), self.nodes[c1].lb.importance(p, n));
            if i0 <= 0.0 && i1 <= 0.0 {
                return 0.0;
            }
            if trail & 1 == 1 {
                pmf *= i1 / (i0 + i1);
                idx = c1;
            } else {
                pmf *= i0 / (i0 + i1);
                idx = c0;
            }
            trail >>= 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rng::Rng;

    fn point_bounds(p: Vec3, phi: f32) -> LightBounds {
        LightBounds { bounds: Aabb { min: p, max: p }, w: Vec3::Z, phi, cos_o: -1.0, cos_e: 0.0, two_sided: false }
    }

    #[test]
    fn bvh_pmf_matches_sampling() {
        let mut rng = Rng::new(1, 2);
        let n = 300;
        let items: Vec<(u32, LightBounds)> = (0..n)
            .map(|i| {
                let p = Vec3::new(rng.f32() * 20.0 - 10.0, rng.f32() * 5.0, rng.f32() * 20.0 - 10.0);
                let mut lb = point_bounds(p, 0.5 + rng.f32());
                if i % 3 == 0 {
                    // Some one-sided emitters facing down.
                    lb.w = Vec3::new(0.0, -1.0, 0.0);
                    lb.cos_o = 1.0;
                }
                (i, lb)
            })
            .collect();
        let bvh = LightBvh::build(items, n as usize);
        let (p, nrm) = (Vec3::new(1.0, 0.5, -2.0), Vec3::new(0.0, 1.0, 0.0));
        let total: f32 = (0..n).map(|i| bvh.pmf(p, nrm, i)).sum();
        assert!((total - 1.0).abs() < 1e-3, "pmfs sum to {total}");
        let mut counts = vec![0u32; n as usize];
        let samples = 300_000;
        for _ in 0..samples {
            let (id, pmf) = bvh.sample(p, nrm, rng.f32()).unwrap();
            assert!((pmf - bvh.pmf(p, nrm, id)).abs() <= 1e-4 * pmf.max(1e-3));
            counts[id as usize] += 1;
        }
        for i in 0..n {
            let expect = bvh.pmf(p, nrm, i);
            let got = counts[i as usize] as f32 / samples as f32;
            assert!((got - expect).abs() < 0.004 + 0.1 * expect, "light {i}: {got} vs {expect}");
        }
    }

    /// Shadow rays towards a light very close to the shading point (relative to the
    /// origin offset) must not hit the light itself.
    #[test]
    fn near_light_is_not_self_occluded() {
        use crate::material::{Material, MaterialKind};
        use crate::scene::{Geometry, RenderSettings};
        use crate::texture::Texture;
        let c = Vec3::new(100.0, 0.0, 0.0); // far from the origin: a large offset epsilon
        let positions =
            vec![c + Vec3::new(-0.05, 0.01, -0.05), c + Vec3::new(0.3, 0.06, 0.0), c + Vec3::new(-0.05, 0.01, 0.3)];
        let tri = crate::geometry::Triangle {
            v: [0, 1, 2], // facing down
            n: [crate::geometry::NO_NORMALS; 3],
            uv: [crate::geometry::NO_UVS; 3],
            material: 0,
        };
        let light = Material::new(MaterialKind::Emissive { emission: Texture::Const(Vec3::ONE), two_sided: false });
        let geo = Geometry { positions, triangles: vec![tri], ..Geometry::default() };
        let scene =
            Scene::new(RenderSettings::default(), crate::camera::CameraDesc::default(), vec![], vec![light], geo);
        let (p, n) = (c, Vec3::new(0.0, 1.0, 0.0));
        let mut s = Sampler::independent(3, 4);
        let (mut lit, mut total) = (0, 0);
        for _ in 0..2000 {
            if let Some(ls) = scene.sample_light(p, n, &mut s) {
                total += 1;
                if !scene.occluded_light(p, n, &ls, 0.0) {
                    lit += 1;
                }
            }
        }
        assert!(total > 1000 && lit == total, "{lit} of {total} light samples unoccluded");
    }

    #[test]
    fn importance_prefers_near_and_facing_lights() {
        let near = point_bounds(Vec3::new(0.0, 1.0, 0.0), 1.0);
        let far = point_bounds(Vec3::new(0.0, 10.0, 0.0), 1.0);
        let n = Vec3::new(0.0, 1.0, 0.0);
        assert!(near.importance(Vec3::ZERO, n) > 50.0 * far.importance(Vec3::ZERO, n));
        // A one-sided emitter facing away from the point contributes nothing.
        let away = LightBounds { w: Vec3::new(0.0, 1.0, 0.0), cos_o: 1.0, ..near };
        assert_eq!(away.importance(Vec3::ZERO, n), 0.0);
        // Grazing light counts less than light from straight above.
        let side = point_bounds(Vec3::new(1.0, 0.05, 0.0), 1.0);
        assert!(side.importance(Vec3::ZERO, n) < 0.2 * near.importance(Vec3::ZERO, n));
    }

    #[test]
    fn cone_union_contains_both() {
        let a = (Vec3::new(1.0, 0.0, 0.0), 0.9f32);
        let b = (Vec3::new(0.0, 1.0, 0.0), 0.95f32);
        let (w, c) = cone_union(a.0, a.1, b.0, b.1);
        for (axis, cos) in [a, b] {
            // The far edge of each input cone is inside the union.
            let edge = angle_between(w, axis) + cos.acos();
            assert!(edge <= c.acos() + 1e-4, "{edge} > {}", c.acos());
        }
    }
}
