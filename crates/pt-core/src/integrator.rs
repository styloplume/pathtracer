//! Unidirectional volumetric path tracer with next-event estimation, multiple importance
//! sampling and optional path guiding.
//!
//! The path carries colour either as RGB or as hero-wavelength spectral samples (see
//! `spectrum.rs`); the light transport code is the same for both. Paths track the medium
//! they travel through (see `medium.rs`): between surfaces they may scatter in it, and
//! inside subsurface-scattering objects they random-walk until they reach the surface
//! again, where they leave through a diffuse exit lobe.

use crate::camera::RayCone;
use crate::guiding::{BSDF_FRACTION, Guide};
use crate::material::{Bsdf, BsdfSample, Lobe};
use crate::math::{Frame, Ray, Vec3, offset_origin};
use crate::medium::{PathMedium, Throughput, hg, sample_hg};
use crate::sampler::Sampler;
use crate::sampling::power_heuristic;
use crate::scene::Scene;
use crate::spectrum::{Channels, Color, Rgb, Wavelengths};

/// One camera path's radiance estimate plus the auxiliary outputs (AOVs) of its
/// first visible surface, used as denoiser guides and as AOV images.
#[derive(Clone, Copy, Debug, Default)]
pub struct PathSample {
    pub radiance: Vec3,
    /// Albedo of the first non-specular surface, times the throughput of any
    /// perfect-specular bounces before it (so mirrors/glass show what they reflect).
    pub albedo: Vec3,
    /// World-space shading normal of that surface, facing the incoming ray; zero on escape.
    pub normal: Vec3,
    /// Distance along the camera ray to the first hit; zero on escape.
    pub depth: f32,
}

#[inline]
fn exp3(v: Vec3) -> Vec3 {
    Vec3::new(v.x.exp(), v.y.exp(), v.z.exp())
}

/// Estimates the radiance arriving along `ray`, the centre of a pixel's ray `cone`. With a
/// guide, surface directions are sampled from the BSDF / guide mixture, and (while it
/// trains) the path's incident radiance is recorded into it.
pub fn radiance(scene: &Scene, ray: Ray, cone: RayCone, s: &mut Sampler, guide: Option<&Guide>) -> PathSample {
    if scene.settings.spectral {
        let mut wl = Wavelengths::sample(s.get1d());
        let (l, mut out) = trace(scene, ray, cone, s, &mut wl, guide);
        out.radiance = wl.to_rgb(l);
        out
    } else {
        let (l, mut out) = trace(scene, ray, cone, s, &mut Rgb, guide);
        out.radiance = l;
        out
    }
}

/// Boundaries (`interface` surfaces) a path may cross before it is given up on.
const MAX_CROSSINGS: u32 = 256;
/// Path vertices whose incident radiance is recorded for guiding.
const MAX_RECORDS: usize = 32;
/// Ray-cone spread (radians per unit distance) after a diffuse bounce or medium scattering.
const MAX_SPREAD: f32 = 1.0;

#[derive(Clone, Copy, Default)]
struct Record {
    leaf: u32,
    dir: Vec3,
    pdf: f32,
    /// 1 / throughput of the ray leaving the vertex.
    inv_beta: f32,
    radiance: f32,
}

/// Guiding records of one path: each contribution added to the pixel is also radiance
/// arriving at every earlier recorded vertex along its sampled direction.
struct Recorder {
    v: [Record; MAX_RECORDS],
    n: usize,
}

impl Recorder {
    #[inline]
    fn splat(&mut self, c: f32) {
        for r in &mut self.v[..self.n] {
            r.radiance += c * r.inv_beta;
        }
    }

    fn finish(&self, guide: &Guide) {
        for r in &self.v[..self.n] {
            guide.record(r.leaf, r.dir, r.radiance / r.pdf);
        }
    }
}

/// The previous scattering vertex, for MIS when a light is hit.
struct Prev {
    pdf: f32,
    delta: bool,
    p: Vec3,
    /// Shading normal; zero for medium vertices.
    n: Vec3,
}

#[expect(clippy::too_many_lines, reason = "the path loop reads best as one function")]
fn trace<Ch: Channels>(
    scene: &Scene,
    mut ray: Ray,
    mut cone: RayCone,
    s: &mut Sampler,
    ch: &mut Ch,
    guide: Option<&Guide>,
) -> (Ch::C, PathSample) {
    let st = &scene.settings;
    let mut out = PathSample::default();
    let mut l = Ch::C::default();
    let mut tp = Throughput::<Ch::C>::new();
    // RGB throughput, only used for the albedo AOV.
    let mut beta_rgb = Vec3::ONE;
    let mut prev = Prev { pdf: 0.0, delta: true, p: Vec3::ZERO, n: Vec3::ZERO };
    // AOVs are recorded once, at the first non-delta vertex (or on escape).
    let mut aov_pending = true;
    let mut first_hit = true;
    let mut medium = scene.initial_medium(&ray);
    let (mut bounces, mut volume_events, mut crossings) = (0u32, 0u32, 0u32);
    let recording = guide.filter(|g| g.recording());
    let mut rec = Recorder { v: [Record::default(); MAX_RECORDS], n: 0 };

    macro_rules! add {
        ($c:expr) => {{
            // Spectral MIS: divide by the channel-averaged pdf ratio of the path.
            let c = $c * (1.0 / tp.r_u.avg());
            l += c;
            if rec.n > 0 {
                rec.splat(c.avg());
            }
        }};
    }

    loop {
        if bounces >= st.max_depth {
            break;
        }
        let hit = scene.intersect(&ray, f32::INFINITY);

        // ---------------------------------------------------------------- medium
        if medium != PathMedium::Vacuum {
            let t_max = hit.as_ref().map_or(f32::INFINITY, |h| h.t);
            if let Some(c) = scene.equiangular_nee(&medium, &ray, t_max, ch, s) {
                add!(tp.beta * c);
            }
            if let Some((t, g)) = scene.sample_medium(&medium, &ray, t_max, ch, s, &mut tp) {
                volume_events += 1;
                if volume_events > st.max_volume_depth {
                    break;
                }
                let p = ray.o + ray.d * t;
                if first_hit {
                    first_hit = false;
                    out.depth = t;
                }
                if aov_pending {
                    aov_pending = false;
                    out.albedo = beta_rgb * scene.medium_albedo(&medium);
                }
                // Light cannot reach the inside of a subsurface object directly.
                let in_object = matches!(medium, PathMedium::Subsurface { .. });
                if !in_object
                    && scene.has_lights()
                    && let Some(ls) = scene.sample_light(p, Vec3::ZERO, s)
                {
                    let ph = hg(ray.d.dot(ls.wi), g);
                    // Point and spot lights are covered by equi-angular sampling.
                    let point_light = ls.delta && ls.dist.is_finite();
                    if ph > 0.0
                        && !point_light
                        && let Some(tr) = scene.light_transmittance(p, Vec3::ZERO, &ls, ray.time, medium, ch, s)
                    {
                        let w = if ls.delta { 1.0 } else { power_heuristic(ls.pdf, ph) };
                        add!(tp.beta * tr * ch.uplift(ls.radiance) * (ph * w / ls.pdf));
                    }
                }
                let (wi, pdf) = sample_hg(ray.d, g, s.get2d());
                prev = Prev { pdf, delta: false, p, n: Vec3::ZERO };
                cone = RayCone { width: cone.width_at(t), spread: MAX_SPREAD };
                ray = Ray::new(p, wi, ray.time);
                if !roulette(&mut tp, bounces + volume_events, s) {
                    break;
                }
                continue;
            }
            if tp.beta.is_black() || !tp.beta.is_finite() {
                break;
            }
        }

        let Some(hit) = hit else {
            // Escaped: infinite lights (environment, sun disk).
            let mut seen = Vec3::ZERO;
            for &li in scene.infinite_lights() {
                let le = scene.lights()[li as usize].le(ray.d);
                if le.is_black() {
                    continue;
                }
                seen += le;
                let w = if prev.delta {
                    1.0
                } else {
                    power_heuristic(prev.pdf, scene.infinite_light_pdf(prev.p, prev.n, li, ray.d))
                };
                add!(tp.beta * ch.uplift(le) * w);
            }
            if aov_pending {
                aov_pending = false;
                out.albedo = beta_rgb * seen.max(Vec3::ZERO).min(Vec3::ONE);
            }
            break;
        };
        let si = scene.interaction(&ray, &hit);
        let mat = &scene.materials[si.material as usize];
        let wo_world = -ray.d;
        let in_object = matches!(medium, PathMedium::Subsurface { .. });

        // Medium boundaries without a surface: pass straight through.
        if mat.is_interface() && !in_object {
            if let Some(mi) = &mat.media {
                medium = PathMedium::from_id(mi.crossing(si.ng, ray.d));
            }
            crossings += 1;
            if crossings > MAX_CROSSINGS {
                break;
            }
            ray = Ray::new(offset_origin(si.p, si.ng, ray.d), ray.d, ray.time);
            continue;
        }
        if first_hit {
            first_hit = false;
            out.depth = hit.t;
        }
        let footprint = cone.width_at(hit.t);
        let ctx = si.tex_ctx(footprint);

        // Beer–Lambert: a segment that reaches an absorbing dielectric from its back
        // side travelled through the interior.
        let sigma = mat.absorption();
        if !sigma.is_black() && wo_world.dot(si.ng) < 0.0 {
            tp.beta *= ch.transmittance(sigma, hit.t);
            beta_rgb *= exp3(sigma * -hit.t);
        }

        // Emitters: add emission (MIS-weighted against light sampling) and stop.
        if !in_object && let Some((le, two_sided)) = mat.emission(&ctx) {
            if aov_pending {
                aov_pending = false;
                out.albedo = beta_rgb * (le / le.max_elem().max(1.0)).max(Vec3::ZERO);
                out.normal = if wo_world.dot(si.ng) < 0.0 { -si.ng } else { si.ng };
            }
            let cos_l = wo_world.dot(si.ng);
            if cos_l > 0.0 || two_sided {
                let w = if prev.delta {
                    1.0
                } else {
                    power_heuristic(prev.pdf, scene.area_light_pdf(prev.p, prev.n, hit.prim, si.p, si.ng))
                };
                add!(tp.beta * ch.uplift(le) * w);
            }
            break;
        }

        // Wavelength-dependent refraction: only the hero wavelength can follow the path.
        if mat.is_dispersive() {
            ch.terminate_secondary();
        }

        // The scattering function at this vertex. A random walk inside a subsurface object
        // ends at the first surface it reaches: the path leaves through a diffuse lobe
        // facing the side it was heading to.
        let (bsdf, frame, wo, ns, ng, transmissive) = if in_object {
            let n = if ray.d.dot(si.ng) > 0.0 { si.ng } else { -si.ng };
            let frame = Frame::from_normal(n);
            let wo = frame.to_local(wo_world);
            let wo = Vec3::new(wo.x, wo.y, wo.z.abs().max(1e-4));
            (Bsdf::single(Lobe::Lambert { albedo: Vec3::ONE }, wo), frame, wo, n, n, false)
        } else {
            // Opaque surfaces are two-sided: shade the side the ray arrived from.
            // Transmissive ones keep the outward normal so they know whether we're entering.
            let transmissive = mat.is_transmissive();
            let ns0 = mat.shading_normal(&si, &ctx);
            let flip = !transmissive && wo_world.dot(si.ng) < 0.0;
            let (mut ns, ng) = if flip { (-ns0, -si.ng) } else { (ns0, si.ng) };
            if !transmissive && wo_world.dot(ns) <= 0.0 {
                ns = ng; // shading normal faces away from the viewer; fall back to flat
            }
            let frame = Frame::from_normal(ns);
            let wo = frame.to_local(wo_world);
            (mat.bsdf(&ctx, wo, ch.hero()), frame, wo, ns, ng, transmissive)
        };
        let delta = bsdf.is_delta();
        if aov_pending && !delta {
            aov_pending = false;
            out.albedo = beta_rgb * bsdf.albedo();
            out.normal = ns;
        }
        // The medium on the far side of this surface, for directions crossing it.
        let far_medium = if in_object {
            PathMedium::from_id(medium.scene_id())
        } else {
            mat.media.map_or(medium, |mi| PathMedium::from_id(mi.crossing(si.ng, -wo_world)))
        };
        let side_medium = |dir: Vec3| {
            if in_object || (transmissive && dir.dot(si.ng) * wo_world.dot(si.ng) < 0.0) { far_medium } else { medium }
        };

        // Guiding: the spatial leaf for sampling and recording at this vertex.
        let leaf = guide.filter(|_| !delta).map(|g| g.leaf(si.p));
        let guided = guide.zip(leaf).filter(|(g, l)| g.has_distribution(*l));
        let mix_pdf = |pdf_b: f32, wi_world: Vec3| match guided {
            Some((g, l)) => BSDF_FRACTION * pdf_b + (1.0 - BSDF_FRACTION) * g.pdf(l, wi_world),
            None => pdf_b,
        };

        // Next-event estimation: sample a light directly.
        if !delta
            && scene.has_lights()
            && let Some(ls) = scene.sample_light(si.p, ns, s)
        {
            let wi = frame.to_local(ls.wi);
            let geo = ls.wi.dot(ng);
            // Both normals must agree on the side, and opaque surfaces only reflect.
            if geo * wi.z > 0.0 && (transmissive || geo > 0.0) {
                let (f, bsdf_pdf) = bsdf.eval(wo, wi);
                if !f.is_black()
                    && let Some(tr) = scene.light_transmittance(si.p, ng, &ls, ray.time, side_medium(ls.wi), ch, s)
                {
                    let w = if ls.delta { 1.0 } else { power_heuristic(ls.pdf, mix_pdf(bsdf_pdf, ls.wi)) };
                    add!(tp.beta * tr * ch.uplift(f) * ch.uplift(ls.radiance) * (w / ls.pdf));
                }
            }
        }

        // Continue the path by sampling the BSDF (or the guide).
        let Some(bs) = sample_direction(&bsdf, wo, &frame, guided, s) else { break };
        let wi_world = frame.to_world(bs.wi);
        if wi_world.dot(ng) * bs.wi.z <= 0.0 {
            break; // shading and geometric normal disagree about the side
        }
        tp.beta *= ch.uplift(bs.weight);
        beta_rgb *= bs.weight;
        if tp.beta.is_black() || !tp.beta.is_finite() {
            break;
        }
        let b = tp.weight().avg();
        if recording.is_some()
            && let Some(l) = leaf
            && !bs.delta
            && rec.n < MAX_RECORDS
            && b > 0.0
        {
            rec.v[rec.n] = Record { leaf: l, dir: wi_world, pdf: bs.pdf, inv_beta: 1.0 / b, radiance: 0.0 };
            rec.n += 1;
        }
        prev = Prev { pdf: bs.pdf, delta: bs.delta, p: si.p, n: ns };
        // Glossy and diffuse bounces widen the cone roughly by the lobe's angular size
        // (a lobe of solid angle 1 / pdf); mirrors and glass keep it (curvature ignored).
        cone.width = footprint;
        if !bs.delta {
            cone.spread = cone.spread.max(bs.pdf.max(1e-6).recip().sqrt().min(MAX_SPREAD));
        }

        // Crossing the surface changes the medium; entering a subsurface object starts a walk.
        if in_object {
            medium = far_medium;
        } else if transmissive && wi_world.dot(si.ng) * wo_world.dot(si.ng) < 0.0 {
            medium = match mat.subsurface(&ctx) {
                Some((sigma_s, sigma_t, g)) if wi_world.dot(si.ng) < 0.0 => {
                    PathMedium::Subsurface { sigma_s, sigma_t, g, outer: medium.scene_id() }
                }
                _ => side_medium(wi_world),
            };
        }
        ray = Ray::new(offset_origin(si.p, ng, wi_world), wi_world, ray.time);
        bounces += 1;
        if !roulette(&mut tp, bounces + volume_events, s) {
            break;
        }
    }
    // A path that ended on delta surfaces only: fall back to the last throughput.
    if aov_pending {
        out.albedo = beta_rgb.max(Vec3::ZERO).min(Vec3::ONE);
    }
    if let Some(g) = recording {
        rec.finish(g);
    }
    (l, out)
}

/// Russian roulette after the first few events. Returns false if the path is terminated.
#[inline]
fn roulette<C: Color>(tp: &mut Throughput<C>, events: u32, s: &mut Sampler) -> bool {
    if events <= 3 {
        return true;
    }
    let q = tp.weight().max_elem().min(0.95);
    if s.get1d() >= q {
        return false;
    }
    tp.beta *= 1.0 / q;
    true
}

/// Samples the next direction from the BSDF, or from the one-sample mixture of the BSDF
/// and the guiding distribution of `guided` = (guide, spatial leaf).
fn sample_direction(
    bsdf: &Bsdf,
    wo: Vec3,
    frame: &Frame,
    guided: Option<(&Guide, u32)>,
    s: &mut Sampler,
) -> Option<BsdfSample> {
    let Some((g, leaf)) = guided else { return bsdf.sample(wo, s) };
    let mix = |f: Vec3, pdf_b: f32, wi: Vec3| {
        let pdf = BSDF_FRACTION * pdf_b + (1.0 - BSDF_FRACTION) * g.pdf(leaf, frame.to_world(wi));
        (pdf > 0.0 && !f.is_black()).then(|| BsdfSample { wi, weight: f / pdf, pdf, delta: false })
    };
    if s.get1d() < BSDF_FRACTION {
        let b = bsdf.sample(wo, s)?;
        if b.delta {
            // Only the BSDF branch produces delta directions.
            return Some(BsdfSample { weight: b.weight / BSDF_FRACTION, pdf: b.pdf * BSDF_FRACTION, ..b });
        }
        let (f, pdf_b) = bsdf.eval(wo, b.wi);
        mix(f, pdf_b, b.wi)
    } else {
        let (d, _) = g.sample(leaf, s.get2d());
        let wi = frame.to_local(d);
        let (f, pdf_b) = bsdf.eval(wo, wi);
        mix(f, pdf_b, wi)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::camera::CameraDesc;
    use crate::environment::{EnvSource, Environment};
    use crate::geometry::Aabb;
    use crate::lights::Light;
    use crate::material::{Material, MaterialKind};
    use crate::medium::{Density, Field, Grid, Medium, MediumInterface};
    use crate::scene::{Geometry, RenderSettings};
    use crate::shapes::{Shape, ShapeKind};
    use crate::texture::Texture;

    /// A sphere of `kind` under a uniform white sky.
    fn furnace(kind: MaterialKind, media: Vec<Medium>, spectral: bool) -> Scene {
        let mut mat = Material::new(kind);
        if !media.is_empty() {
            mat.media = Some(MediumInterface { interior: Some(0), exterior: None });
        }
        let geo = Geometry {
            shapes: vec![Shape { kind: ShapeKind::Sphere { center: Vec3::ZERO, radius: 1.0 }, material: 0 }],
            ..Geometry::default()
        };
        let settings = RenderSettings { max_depth: 1000, max_volume_depth: 100_000, spectral, ..Default::default() };
        let env = Light::Environment(Box::new(Environment::new(EnvSource::Color(Vec3::ONE), Vec3::ZERO)));
        let mut scene = Scene::new(settings, CameraDesc::default(), vec![env], vec![mat], geo);
        scene.media = media;
        scene
    }

    fn mean_radiance(scene: &Scene, n: u32) -> Vec3 {
        let mut sum = Vec3::ZERO;
        for i in 0..n {
            let mut s = Sampler::new(crate::sampler::SamplerKind::Sobol, i % 61, i / 61, 61, i, 3);
            let (u1, u2) = s.get2d();
            let o = Vec3::new(u1 * 1.6 - 0.8, u2 * 1.6 - 0.8, 4.0);
            sum +=
                radiance(scene, Ray::new(o, Vec3::new(0.0, 0.0, -1.0), 0.0), RayCone::default(), &mut s, None).radiance;
        }
        sum * (1.0 / n as f32)
    }

    /// Non-absorbing media inside an invisible boundary, under a white sky: every path
    /// must see exactly white (energy conservation of free-flight sampling, NEE through
    /// media and MIS), for chromatic, heterogeneous and spectral cases.
    #[test]
    fn white_furnace_with_media() {
        let homogeneous = Medium { sigma_a: Vec3::ZERO, sigma_s: Vec3::new(0.5, 2.0, 5.0), g: 0.6, density: None };
        let grid = Grid {
            res: [2, 2, 2],
            data: vec![0.0, 1.0, 3.0, 0.5, 2.0, 0.2, 1.5, 4.0],
            bounds: Aabb { min: Vec3::splat(-1.0), max: Vec3::splat(1.0) },
        };
        let heterogeneous = Medium {
            sigma_a: Vec3::ZERO,
            sigma_s: Vec3::new(1.0, 2.0, 0.5),
            g: -0.3,
            density: Some(Density { field: Field::Grid(grid), max: 4.0 }),
        };
        for (name, m, spectral) in [
            ("homogeneous", homogeneous, false),
            ("heterogeneous", heterogeneous, false),
            (
                "spectral",
                Medium { sigma_a: Vec3::ZERO, sigma_s: Vec3::new(3.0, 1.0, 2.0), g: 0.2, density: None },
                true,
            ),
        ] {
            let scene = furnace(MaterialKind::Interface, vec![m], spectral);
            let l = mean_radiance(&scene, 20_000);
            assert!((l - Vec3::ONE).abs().max_elem() < 0.02, "{name}: {l:?}");
        }
    }

    /// Absorption only: the analytic Beer–Lambert transmittance along a chord.
    #[test]
    fn absorbing_medium_matches_beer_lambert() {
        let m = Medium { sigma_a: Vec3::new(0.2, 0.7, 1.5), sigma_s: Vec3::ZERO, g: 0.0, density: None };
        let scene = furnace(MaterialKind::Interface, vec![m], false);
        let mut s = Sampler::new(crate::sampler::SamplerKind::Sobol, 0, 0, 1, 0, 1);
        let l = radiance(
            &scene,
            Ray::new(Vec3::new(0.0, 0.0, 4.0), Vec3::new(0.0, 0.0, -1.0), 0.0),
            RayCone::default(),
            &mut s,
            None,
        );
        let expect = Vec3::new((-0.4f32).exp(), (-1.4f32).exp(), (-3.0f32).exp());
        assert!((l.radiance - expect).abs().max_elem() < 1e-3, "{:?} vs {expect:?}", l.radiance);
    }

    /// A white subsurface sphere under a white sky stays (almost) white: entry, random
    /// walk and exit conserve energy (minus the rough coat's single-scattering loss).
    #[test]
    fn white_furnace_subsurface() {
        let kind = MaterialKind::Subsurface {
            color: Texture::Const(Vec3::ONE),
            radius: Vec3::new(0.5, 0.2, 0.1),
            ior: 1.4,
            roughness: Texture::Const(Vec3::splat(0.0)),
            g: 0.0,
        };
        let l = mean_radiance(&furnace(kind, vec![], false), 40_000);
        // Albedo 1 is clamped to 0.999 for the inversion: a little energy is absorbed.
        assert!(l.max_elem() <= 1.03 && l.x > 0.95 && l.y > 0.95 && l.z > 0.95, "{l:?}");
    }
}
