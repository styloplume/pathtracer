//! Participating media: homogeneous and heterogeneous volumes, and the random walk of
//! subsurface scattering.
//!
//! Media fill the inside (or outside) of closed surfaces: a material may name the media on
//! either side of it (`interior` / `exterior`), and the special `interface` material marks a
//! boundary that is otherwise invisible. Nested media are not tracked automatically; a path
//! crossing a boundary switches to the medium declared on the far side (or keeps its medium
//! if the material declares none).
//!
//! Free-flight sampling:
//!
//! - **Homogeneous** media sample distances analytically with the coefficients of one
//!   *hero* colour channel, picked uniformly per path. The path tracks, per channel, the
//!   ratio of the pdf its distances would have had under that channel's coefficients
//!   ([`Throughput::r_u`]); contributions are divided by the channel average, which is
//!   one-sample MIS over the channels for the whole path (as in pbrt-v4). Weights stay
//!   bounded however often a path scatters, so chromatic media and subsurface random
//!   walks with very different per-channel radii converge well.
//! - **Heterogeneous** media (a density grid or a procedural field times the base
//!   coefficients) use delta tracking against per-channel constant majorants, with the
//!   same hero-channel spectral MIS. Absorption is applied as a weight on null collisions
//!   rather than terminating paths.
//!
//! Shadow rays use analytic transmittance for homogeneous media and ratio tracking for
//! heterogeneous ones, passing through `interface` surfaces.

use crate::geometry::Aabb;
use crate::lights::{Light, LightSample};
use crate::math::{Frame, PI, Ray, Vec3, offset_origin};
use crate::sampler::Sampler;
use crate::scene::Scene;
use crate::spectrum::{Channels, Color};
use crate::texture::{TexCtx, Texture};
use std::path::Path;

/// Which media a surface separates: `interior` lies on the side its (outward) geometric
/// normal points away from. `None` is vacuum.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MediumInterface {
    pub interior: Option<u32>,
    pub exterior: Option<u32>,
}

impl MediumInterface {
    /// The medium entered by a ray travelling along `d` through a surface with outward normal `n`.
    #[inline]
    pub fn crossing(&self, n: Vec3, d: Vec3) -> Option<u32> {
        if d.dot(n) < 0.0 { self.interior } else { self.exterior }
    }
}

pub struct Medium {
    /// Absorption and scattering coefficients (per scene unit), scaled by the density.
    pub sigma_a: Vec3,
    pub sigma_s: Vec3,
    /// Henyey–Greenstein asymmetry, in (-1, 1).
    pub g: f32,
    /// Spatially varying density; None for a homogeneous medium.
    pub density: Option<Density>,
}

pub struct Density {
    pub field: Field,
    /// Upper bound of the field (the majorant's density).
    pub max: f32,
}

pub enum Field {
    Grid(Grid),
    /// A world-space procedural texture (noise, checker, ...), clamped to [0, max].
    Texture(Texture),
}

/// A voxel grid of densities stretched over `bounds`, trilinearly interpolated
/// (voxel centers at `(i + 0.5) / res`), zero outside.
pub struct Grid {
    pub res: [usize; 3],
    pub data: Vec<f32>,
    pub bounds: Aabb,
}

impl Grid {
    pub fn max(&self) -> f32 {
        self.data.iter().copied().fold(0.0, f32::max)
    }

    pub fn eval(&self, p: Vec3) -> f32 {
        let size = self.bounds.max - self.bounds.min;
        let q = p - self.bounds.min;
        let u = [q.x / size.x, q.y / size.y, q.z / size.z];
        if u.iter().any(|v| !(0.0..=1.0).contains(v)) {
            return 0.0;
        }
        let mut i0 = [0usize; 3];
        let mut f = [0.0f32; 3];
        for k in 0..3 {
            let x = (u[k] * self.res[k] as f32 - 0.5).clamp(0.0, (self.res[k] - 1) as f32);
            i0[k] = (x as usize).min(self.res[k].saturating_sub(2));
            f[k] = (x - i0[k] as f32).min(1.0);
        }
        let at = |x: usize, y: usize, z: usize| {
            let (x, y, z) = (x.min(self.res[0] - 1), y.min(self.res[1] - 1), z.min(self.res[2] - 1));
            self.data[(z * self.res[1] + y) * self.res[0] + x]
        };
        let lerp = |a: f32, b: f32, t: f32| a + (b - a) * t;
        let [x, y, z] = i0;
        let c00 = lerp(at(x, y, z), at(x + 1, y, z), f[0]);
        let c10 = lerp(at(x, y + 1, z), at(x + 1, y + 1, z), f[0]);
        let c01 = lerp(at(x, y, z + 1), at(x + 1, y, z + 1), f[0]);
        let c11 = lerp(at(x, y + 1, z + 1), at(x + 1, y + 1, z + 1), f[0]);
        lerp(lerp(c00, c10, f[1]), lerp(c01, c11, f[1]), f[2])
    }
}

/// Loads a Mitsuba 0.5 `.vol` grid (float32 or uint8 encoding; multi-channel grids use
/// their first channel). The file's bounding box places it in the scene.
pub fn load_vol(path: &Path) -> Result<Grid, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    parse_vol(&bytes).map_err(|e| format!("{}: {e}", path.display()))
}

pub fn parse_vol(b: &[u8]) -> Result<Grid, String> {
    if b.len() < 48 || &b[0..3] != b"VOL" || b[3] != 3 {
        return Err("not a Mitsuba .vol (version 3) file".into());
    }
    let i32_at = |o: usize| i32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);
    let f32_at = |o: usize| f32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);
    let encoding = i32_at(4);
    let dims = [i32_at(8), i32_at(12), i32_at(16), i32_at(20)];
    if dims.iter().any(|&d| d <= 0 || d > 1 << 14) {
        return Err(format!("bad grid size {dims:?}"));
    }
    let [nx, ny, nz, nc] = dims.map(|d| d as usize);
    let bounds =
        Aabb { min: Vec3::new(f32_at(24), f32_at(28), f32_at(32)), max: Vec3::new(f32_at(36), f32_at(40), f32_at(44)) };
    let count = nx * ny * nz;
    let elem = match encoding {
        1 => 4,
        3 => 1,
        e => return Err(format!("unsupported encoding {e} (only float32 = 1 and uint8 = 3)")),
    };
    let data_bytes = &b[48..];
    if data_bytes.len() < count * nc * elem {
        return Err("file is truncated".into());
    }
    let data = (0..count)
        .map(|i| {
            let o = i * nc * elem;
            let v = if elem == 4 {
                f32::from_le_bytes([data_bytes[o], data_bytes[o + 1], data_bytes[o + 2], data_bytes[o + 3]])
            } else {
                f32::from(data_bytes[o]) / 255.0
            };
            if v.is_finite() { v.max(0.0) } else { 0.0 }
        })
        .collect();
    if !(bounds.max.x > bounds.min.x && bounds.max.y > bounds.min.y && bounds.max.z > bounds.min.z) {
        return Err("empty bounding box".into());
    }
    Ok(Grid { res: [nx, ny, nz], data, bounds })
}

impl Density {
    #[inline]
    fn eval(&self, p: Vec3) -> f32 {
        match &self.field {
            Field::Grid(g) => g.eval(p),
            Field::Texture(t) => t.eval_f(&TexCtx { p, ..TexCtx::default() }).clamp(0.0, self.max),
        }
    }

    /// The part of the ray segment [0, `t_max`) where the density can be non-zero.
    fn clip(&self, ray: &Ray, t_max: f32) -> Option<(f32, f32)> {
        let bounds = match &self.field {
            Field::Grid(g) => g.bounds,
            Field::Texture(t) => match t.support() {
                Some(b) => b,
                None => return Some((0.0, t_max)),
            },
        };
        let inv = ray.d.recip();
        let (a, b) = ((bounds.min - ray.o) * inv, (bounds.max - ray.o) * inv);
        let (near, far) = (a.min(b), a.max(b));
        let t0 = near.x.max(near.y).max(near.z).max(0.0);
        let t1 = far.x.min(far.y).min(far.z).min(t_max);
        (t0 < t1).then_some((t0, t1))
    }
}

impl Medium {
    /// Single-scattering albedo (for the albedo AOV).
    pub fn albedo(&self) -> Vec3 {
        albedo(self.sigma_s, self.sigma_a + self.sigma_s)
    }
}

fn albedo(sigma_s: Vec3, sigma_t: Vec3) -> Vec3 {
    let f = |s: f32, t: f32| if t > 0.0 { s / t } else { 0.0 };
    Vec3::new(f(sigma_s.x, sigma_t.x), f(sigma_s.y, sigma_t.y), f(sigma_s.z, sigma_t.z))
}

// ---------------------------------------------------------------------------- subsurface

/// Homogeneous scattering coefficients (`sigma_s`, `sigma_t`) that make a semi-infinite slab
/// show multiple-scattering albedo `color` with mean free path about `radius`
/// (Chiang, Kutz & Burley 2016, "Practical and Controllable Subsurface Scattering").
pub fn subsurface_coefficients(color: Vec3, radius: Vec3) -> (Vec3, Vec3) {
    let ch = |a: f32, r: f32| {
        let a = a.clamp(1e-3, 0.999);
        let alpha = 1.0 - (4.097_12 + 4.208_63 * a - (9.592_17 + 41.680_8 * a + 17.712_6 * a * a).sqrt()).powi(2);
        let s = 1.9 - a + 3.5 * (a - 0.8) * (a - 0.8);
        let st = 1.0 / (r.max(1e-6) * s);
        (alpha * st, st)
    };
    let (x, y, z) = (ch(color.x, radius.x), ch(color.y, radius.y), ch(color.z, radius.z));
    (Vec3::new(x.0, y.0, z.0), Vec3::new(x.1, y.1, z.1))
}

// ---------------------------------------------------------------------------- path state

/// The medium a path is travelling through.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PathMedium {
    Vacuum,
    Scene(u32),
    /// Inside a subsurface-scattering object, with coefficients from the entry point;
    /// the walk ends at the next surface hit, returning to `outer`.
    Subsurface {
        sigma_s: Vec3,
        sigma_t: Vec3,
        g: f32,
        outer: Option<u32>,
    },
}

impl PathMedium {
    pub fn from_id(m: Option<u32>) -> PathMedium {
        m.map_or(PathMedium::Vacuum, PathMedium::Scene)
    }

    /// The scene medium (for leaving a subsurface walk).
    pub fn scene_id(&self) -> Option<u32> {
        match *self {
            PathMedium::Vacuum => None,
            PathMedium::Scene(m) => Some(m),
            PathMedium::Subsurface { outer, .. } => outer,
        }
    }
}

/// Henyey–Greenstein phase function for the angle between the propagation direction
/// and the scattered direction (`cos` = their dot product).
#[inline]
pub fn hg(cos: f32, g: f32) -> f32 {
    let d = (1.0 + g * g - 2.0 * g * cos).max(1e-12);
    (1.0 - g * g) / (4.0 * PI * d * d.sqrt())
}

/// Samples a scattered direction around the propagation direction `d`. Returns it and its pdf.
pub fn sample_hg(d: Vec3, g: f32, (u1, u2): (f32, f32)) -> (Vec3, f32) {
    let cos = if g.abs() < 1e-3 {
        1.0 - 2.0 * u1
    } else {
        let s = (1.0 - g * g) / (1.0 - g + 2.0 * g * u1);
        ((1.0 + g * g - s * s) / (2.0 * g)).clamp(-1.0, 1.0)
    };
    let sin = (1.0 - cos * cos).max(0.0).sqrt();
    let phi = 2.0 * PI * u2;
    let w = Frame::from_normal(d).to_world(Vec3::new(sin * phi.cos(), sin * phi.sin(), cos));
    (w, hg(cos, g))
}

/// Equi-angular sampling (Kulla & Fajardo 2012) of a distance in [0, `t_max`] along `ray`,
/// with density proportional to the inverse squared distance to point `c`. Returns (t, pdf).
pub fn sample_equiangular(ray: &Ray, c: Vec3, t_max: f32, u: f32) -> Option<(f32, f32)> {
    let delta = (c - ray.o).dot(ray.d);
    let d = (ray.o + ray.d * delta - c).length().max(1e-5);
    let theta_a = (-delta).atan2(d);
    let theta_b = if t_max.is_finite() { (t_max - delta).atan2(d) } else { 0.5 * PI };
    if theta_b <= theta_a {
        return None;
    }
    let t = delta + d * (theta_a + u * (theta_b - theta_a)).tan();
    let x = t - delta;
    let pdf = d / ((theta_b - theta_a) * (d * d + x * x));
    (t >= 0.0 && t <= t_max && pdf > 0.0 && pdf.is_finite()).then_some((t, pdf))
}

/// Upper bound on null collisions per segment: a guard against pathological fields,
/// not a quality setting.
const MAX_COLLISIONS: u32 = 1 << 16;

#[inline]
fn exp_neg<C: Color>(sigma: C, t: f32) -> C {
    sigma.map(|s| if s > 0.0 { (-s * t).exp() } else { 1.0 })
}

#[inline]
fn at(ray: &Ray, t: f32) -> Vec3 {
    ray.o + ray.d * t
}

/// Path throughput with spectral MIS for distance sampling.
#[derive(Clone, Copy, Debug)]
pub struct Throughput<C> {
    /// Path contribution over the pdf of the hero channel's sampling decisions.
    pub beta: C,
    /// Per channel: the pdf of those decisions under that channel's coefficients, over
    /// the hero channel's pdf (1 everywhere until the path samples a medium).
    pub r_u: C,
    hero: Option<usize>,
}

impl<C: Color> Default for Throughput<C> {
    fn default() -> Self {
        Self::new()
    }
}

impl<C: Color> Throughput<C> {
    pub fn new() -> Self {
        Throughput { beta: C::splat(1.0), r_u: C::splat(1.0), hero: None }
    }

    /// The MIS-weighted throughput that contributions are multiplied by.
    #[inline]
    pub fn weight(&self) -> C {
        let r = self.r_u.avg();
        if r > 0.0 { self.beta * (1.0 / r) } else { C::default() }
    }

    /// The hero channel, picked on first use.
    fn hero(&mut self, s: &mut Sampler) -> usize {
        *self.hero.get_or_insert_with(|| ((s.get1d() * C::N as f32) as usize).min(C::N - 1))
    }
}

/// Analytic free-flight sampling in a homogeneous medium with the hero channel's
/// coefficients. Returns the scattering distance, or None if the ray reaches `t_max`.
fn sample_homogeneous<C: Color>(ss: C, st: C, t_max: f32, s: &mut Sampler, tp: &mut Throughput<C>) -> Option<f32> {
    if ss.is_black() {
        tp.beta *= exp_neg(st, t_max);
        return None;
    }
    let hero = tp.hero(s);
    let sh = st.channel(hero);
    let t = if sh > 0.0 { -(1.0 - s.get1d()).ln() / sh } else { f32::INFINITY };
    // Per channel: the pdf of this decision (scatter at t, or pass) under its coefficients.
    let (pdf, weight, t) = if t < t_max {
        let tr = exp_neg(st, t);
        (st * tr, tr * ss, Some(t))
    } else {
        let tr = exp_neg(st, t_max);
        (tr, tr, None)
    };
    let ph = pdf.channel(hero);
    if ph > 0.0 {
        tp.beta *= weight * (1.0 / ph);
        tp.r_u *= pdf * (1.0 / ph);
    } else {
        tp.beta = C::default();
    }
    t
}

/// Delta tracking with per-channel majorants, sampled with the hero channel's (spectral MIS
/// as in the homogeneous case). Absorption is a weight on null collisions rather than a
/// termination. Returns the scattering distance, or None if the ray reaches `t_max` (or the
/// path died: the throughput is black then).
fn sample_heterogeneous<C: Color>(
    d: &Density,
    ss0: C,
    st0: C,
    ray: &Ray,
    t_max: f32,
    s: &mut Sampler,
    tp: &mut Throughput<C>,
) -> Option<f32> {
    let (t0, t1) = d.clip(ray, t_max)?;
    let maj = st0 * d.max;
    let hero = tp.hero(s);
    let mh = maj.channel(hero);
    // Multiplies in an event with per-channel contribution `f` and pdf `p`.
    let apply = |tp: &mut Throughput<C>, f: C, p: C| {
        let ph = p.channel(hero);
        if ph > 0.0 {
            tp.beta *= f * (1.0 / ph);
            tp.r_u *= p * (1.0 / ph);
        } else {
            tp.beta = C::default();
        }
    };
    let mut t_prev = t0;
    for _ in 0..MAX_COLLISIONS {
        let t = if mh > 0.0 { t_prev - (1.0 - s.get1d()).ln() / mh } else { f32::INFINITY };
        if t >= t1 {
            let tm = exp_neg(maj, t1 - t_prev);
            apply(tp, tm, tm);
            return None;
        }
        let tm = exp_neg(maj, t - t_prev);
        t_prev = t;
        let dens = d.eval(at(ray, t));
        let ss = ss0 * dens;
        // Scatter with the hero channel's probability sigma_s / majorant, else continue
        // (a null collision, weighted by sigma_n / (majorant - sigma_s): absorption).
        if s.get1d() < ss.channel(hero) / mh {
            apply(tp, tm * ss, tm * ss);
            return (!tp.beta.is_black()).then_some(t);
        }
        let sn = (maj + st0 * -dens).map(|v| v.max(0.0));
        apply(tp, tm * sn, tm * (maj + ss * -1.0).map(|v| v.max(0.0)));
        if tp.beta.is_black() {
            return None;
        }
    }
    tp.beta = C::default();
    None
}
/// Ratio tracking estimate of the transmittance over [0, `t_max`), with Russian roulette
/// once it gets small.
fn transmittance_heterogeneous<C: Color>(d: &Density, st0: C, ray: &Ray, t_max: f32, s: &mut Sampler) -> C {
    let mut tr = C::splat(1.0);
    let Some((mut t, t1)) = d.clip(ray, t_max) else { return tr };
    let maj = d.max * st0.max_elem();
    if maj <= 0.0 {
        return tr;
    }
    for _ in 0..MAX_COLLISIONS {
        t -= (1.0 - s.get1d()).ln() / maj;
        if t >= t1 {
            return tr;
        }
        let dens = d.eval(at(ray, t));
        if dens <= 0.0 {
            continue;
        }
        tr *= (C::splat(maj) + st0 * -dens).map(|v| v.max(0.0)) * (1.0 / maj);
        let m = tr.max_elem();
        if m < 0.2 {
            if m <= 0.0 || s.get1d() >= m {
                return C::default();
            }
            tr *= 1.0 / m;
        }
    }
    C::default()
}

impl Scene {
    /// Samples where along `ray` (up to `t_max`) the path scatters in `medium`. Returns the
    /// distance and the phase function's asymmetry, or None if the ray reaches `t_max`
    /// (check the throughput for black: the path may have been absorbed).
    pub fn sample_medium<Ch: Channels>(
        &self,
        medium: &PathMedium,
        ray: &Ray,
        t_max: f32,
        ch: &Ch,
        s: &mut Sampler,
        tp: &mut Throughput<Ch::C>,
    ) -> Option<(f32, f32)> {
        match *medium {
            PathMedium::Vacuum => None,
            PathMedium::Subsurface { sigma_s, sigma_t, g, .. } => {
                sample_homogeneous(ch.uplift(sigma_s), ch.uplift(sigma_t), t_max, s, tp).map(|t| (t, g))
            }
            PathMedium::Scene(id) => {
                let m = &self.media[id as usize];
                let (ss, st) = (ch.uplift(m.sigma_s), ch.uplift(m.sigma_a + m.sigma_s));
                let t = match &m.density {
                    None => sample_homogeneous(ss, st, t_max, s, tp),
                    Some(d) => sample_heterogeneous(d, ss, st, ray, t_max, s, tp),
                };
                t.map(|t| (t, m.g))
            }
        }
    }

    /// Transmittance of `medium` along `ray` over [0, `t_max`).
    fn medium_transmittance<Ch: Channels>(
        &self,
        medium: &PathMedium,
        ray: &Ray,
        t_max: f32,
        ch: &Ch,
        s: &mut Sampler,
    ) -> Ch::C {
        match *medium {
            PathMedium::Vacuum => Ch::C::splat(1.0),
            PathMedium::Subsurface { sigma_t, .. } => exp_neg(ch.uplift(sigma_t), t_max),
            PathMedium::Scene(id) => {
                let m = &self.media[id as usize];
                let st = ch.uplift(m.sigma_a + m.sigma_s);
                match &m.density {
                    None => exp_neg(st, t_max),
                    Some(d) => transmittance_heterogeneous(d, st, ray, t_max, s),
                }
            }
        }
    }

    /// Light scattered towards the ray origin from point and spot lights, by single
    /// scattering anywhere in [0, `t_max`) of a scene medium: one equi-angular sample
    /// (per unit throughput). These lights are left out of next-event estimation at
    /// distance-sampled medium vertices, which would turn their 1 / r^2 peak into fireflies.
    pub fn equiangular_nee<Ch: Channels>(
        &self,
        medium: &PathMedium,
        ray: &Ray,
        t_max: f32,
        ch: &Ch,
        s: &mut Sampler,
    ) -> Option<Ch::C> {
        let PathMedium::Scene(id) = *medium else { return None };
        let m = &self.media[id as usize];
        if self.point_lights.is_empty() || m.sigma_s.is_black() {
            return None;
        }
        let (u_light, u_t) = s.get2d();
        let n = self.point_lights.len();
        let li = self.point_lights[((u_light * n as f32) as usize).min(n - 1)];
        let (Light::Point { pos, .. } | Light::Spot { pos, .. }) = &self.lights[li as usize] else { return None };
        let (t, pdf) = sample_equiangular(ray, *pos, t_max, u_t)?;
        let p = at(ray, t);
        let density = m.density.as_ref().map_or(1.0, |d| d.eval(p));
        if density <= 0.0 {
            return None;
        }
        let ls = self.sample_one_light(li, p, 0.0, 0.0)?;
        if ls.radiance.is_black() {
            return None;
        }
        let ph = hg(ray.d.dot(ls.wi), m.g);
        let tr_in = self.medium_transmittance(medium, ray, t, ch, s);
        if tr_in.is_black() {
            return None;
        }
        let tr_out = self.light_transmittance(p, Vec3::ZERO, &ls, ray.time, *medium, ch, s)?;
        let w = ph * density * n as f32 / pdf;
        Some(tr_in * tr_out * ch.uplift(m.sigma_s) * ch.uplift(ls.radiance) * w)
    }

    /// Single-scattering albedo of a path medium (albedo AOV).
    pub fn medium_albedo(&self, medium: &PathMedium) -> Vec3 {
        match *medium {
            PathMedium::Vacuum => Vec3::ZERO,
            PathMedium::Subsurface { sigma_s, sigma_t, .. } => albedo(sigma_s, sigma_t),
            PathMedium::Scene(id) => self.media[id as usize].albedo(),
        }
    }

    /// The medium the origin of `ray` lies in: found by following the ray to the first
    /// surface that declares media and checking which side it was reached from.
    pub fn initial_medium(&self, ray: &Ray) -> PathMedium {
        if !self.has_media {
            return PathMedium::Vacuum;
        }
        let mut o = ray.o;
        for _ in 0..64 {
            let r = Ray::new(o, ray.d, ray.time);
            let Some(h) = self.intersect(&r, f32::INFINITY) else { break };
            let si = self.interaction(&r, &h);
            if let Some(mi) = &self.materials[si.material as usize].media {
                return PathMedium::from_id(if ray.d.dot(si.ng) > 0.0 { mi.interior } else { mi.exterior });
            }
            o = offset_origin(si.p, si.ng, ray.d);
        }
        PathMedium::Vacuum
    }

    /// Transmittance from `p` (a surface point with geometric normal `ng`, or a point in a
    /// medium with `ng` zero) to light sample `ls`, starting in `medium`; None if blocked.
    /// Passes through `interface` surfaces, switching media.
    #[expect(clippy::too_many_arguments, reason = "a shadow ray through media needs the full path context")]
    pub fn light_transmittance<Ch: Channels>(
        &self,
        p: Vec3,
        ng: Vec3,
        ls: &LightSample,
        time: f32,
        medium: PathMedium,
        ch: &Ch,
        s: &mut Sampler,
    ) -> Option<Ch::C> {
        let one = Ch::C::splat(1.0);
        if !self.has_media && medium == PathMedium::Vacuum {
            return (!self.occluded_light(p, ng, ls, time)).then_some(one);
        }
        let mut o = if ng == Vec3::ZERO { p } else { offset_origin(p, ng, ls.wi) };
        let target = p + ls.wi * ls.dist;
        let mut medium = medium;
        let mut tr = one;
        for _ in 0..64 {
            let (d, dist) = if ls.dist.is_finite() {
                let v = target - o;
                let len = v.length();
                if len <= 0.0 {
                    return Some(tr);
                }
                (v * (1.0 / len), len * (1.0 - 1e-3))
            } else {
                (ls.wi, f32::INFINITY)
            };
            let ray = Ray::new(o, d, time);
            let hit = self.intersect(&ray, dist);
            let seg = hit.as_ref().map_or(dist, |h| h.t);
            if medium != PathMedium::Vacuum {
                tr *= self.medium_transmittance(&medium, &ray, seg, ch, s);
                if tr.is_black() {
                    return None;
                }
            }
            let Some(h) = hit else { return Some(tr) };
            let si = self.interaction(&ray, &h);
            let mat = &self.materials[si.material as usize];
            if !mat.is_interface() {
                return None;
            }
            if let Some(mi) = &mat.media {
                medium = PathMedium::from_id(mi.crossing(si.ng, d));
            }
            o = offset_origin(si.p, si.ng, d);
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hg_sampling_matches_pdf() {
        let mut s = Sampler::independent(3, 3);
        let d = Vec3::new(0.3, -0.5, 0.8).normalize();
        for g in [-0.7f32, 0.0, 0.4, 0.9] {
            let n = 100_000;
            let mut mean_cos = 0.0;
            for _ in 0..n {
                let (w, pdf) = sample_hg(d, g, s.get2d());
                assert!((w.length() - 1.0).abs() < 1e-4);
                assert!((pdf - hg(w.dot(d), g)).abs() <= 1e-3 * pdf.max(1.0));
                mean_cos += w.dot(d);
            }
            // The mean cosine of Henyey–Greenstein is g.
            assert!((mean_cos / n as f32 - g).abs() < 0.01, "g {g}: {}", mean_cos / n as f32);
        }
        // Normalized over the sphere.
        let total: f32 =
            (0..20_000).map(|i| hg(-1.0 + 2.0 * (i as f32 + 0.5) / 20_000.0, 0.6) * 2.0 * PI * 2.0 / 20_000.0).sum();
        assert!((total - 1.0).abs() < 1e-3, "{total}");
    }

    /// Both trackers estimate the transmittance of a chromatic homogeneous medium, and the
    /// probability of scattering before a distance, without bias.
    #[test]
    fn free_flight_sampling_is_unbiased() {
        let (ss, sa) = (Vec3::new(0.5, 1.0, 2.0), Vec3::new(0.3, 0.1, 0.0));
        let st = ss + sa;
        let d = 0.8f32;
        let expect_pass = Vec3::new((-st.x * d).exp(), (-st.y * d).exp(), (-st.z * d).exp());
        let grid = Density {
            field: Field::Grid(Grid {
                res: [1, 1, 1],
                data: vec![0.5],
                bounds: Aabb { min: Vec3::splat(-10.0), max: Vec3::splat(10.0) },
            }),
            max: 1.0,
        };
        let ray = Ray::new(Vec3::ZERO, Vec3::Z, 0.0);
        let mut s = Sampler::independent(1, 2);
        let n = 200_000;
        let (mut pass_h, mut pass_d, mut ratio) = (Vec3::ZERO, Vec3::ZERO, Vec3::ZERO);
        let mut scatter_h = Vec3::ZERO;
        for _ in 0..n {
            let mut tp = Throughput::new();
            match sample_homogeneous(ss, st, d, &mut s, &mut tp) {
                None => pass_h += tp.weight(),
                Some(_) => scatter_h += tp.weight(),
            }
            // The grid at density 0.5 with doubled coefficients is the same medium.
            let mut tp = Throughput::new();
            if sample_heterogeneous(&grid, ss * 2.0, st * 2.0, &ray, d, &mut s, &mut tp).is_none() {
                pass_d += tp.weight();
            }
            ratio += transmittance_heterogeneous(&grid, st * 2.0, &ray, d, &mut s);
        }
        let inv = 1.0 / n as f32;
        for (name, got) in [("homogeneous", pass_h * inv), ("delta", pass_d * inv), ("ratio", ratio * inv)] {
            assert!((got - expect_pass).abs().max_elem() < 0.01, "{name}: {got:?} vs {expect_pass:?}");
        }
        // E[weight of scattering before d] = integral of T(t) sigma_s dt.
        let expect_scatter = Vec3::new(
            ss.x / st.x * (1.0 - expect_pass.x),
            ss.y / st.y * (1.0 - expect_pass.y),
            ss.z / st.z * (1.0 - expect_pass.z),
        );
        assert!((scatter_h * inv - expect_scatter).abs().max_elem() < 0.01, "{:?}", scatter_h * inv);
    }

    #[test]
    fn grid_interpolates_and_parses() {
        let mut bytes = b"VOL\x03".to_vec();
        for v in [1i32, 2, 1, 1, 1] {
            bytes.extend_from_slice(&v.to_le_bytes());
        }
        for v in [0.0f32, 0.0, 0.0, 2.0, 1.0, 1.0, 0.0, 4.0] {
            bytes.extend_from_slice(&v.to_le_bytes());
        }
        let g = parse_vol(&bytes).unwrap();
        assert_eq!(g.res, [2, 1, 1]);
        assert!((g.max() - 4.0).abs() < 1e-6);
        // Voxel centers at x = 0.5 and 1.5; linear in between, clamped outside.
        assert!((g.eval(Vec3::new(1.0, 0.5, 0.5)) - 2.0).abs() < 1e-5);
        assert!((g.eval(Vec3::new(0.2, 0.5, 0.5))).abs() < 1e-5);
        assert!((g.eval(Vec3::new(1.9, 0.5, 0.5)) - 4.0).abs() < 1e-5);
        assert_eq!(g.eval(Vec3::new(2.1, 0.5, 0.5)), 0.0);
        assert!(parse_vol(&bytes[..50]).is_err());
    }

    #[test]
    fn subsurface_albedo_inversion() {
        let (ss, st) = subsurface_coefficients(Vec3::new(0.0, 0.8, 1.0), Vec3::ONE);
        let a = albedo(ss, st);
        assert!(a.x < 0.01 && (a.y - 0.99).abs() < 0.01 && a.z > 0.999, "{a:?}");
        assert!(st.max_elem() > 0.0 && st.is_finite());
    }
}
