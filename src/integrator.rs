//! Unidirectional path tracer with next-event estimation and multiple importance sampling.
//!
//! The path carries colour either as RGB or as hero-wavelength spectral samples (see
//! `spectrum.rs`); the light transport code is the same for both.

use crate::math::{Frame, Ray, Vec3, offset_origin};
use crate::rng::Rng;
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

/// Estimates the radiance arriving along `ray`.
pub fn radiance(scene: &Scene, ray: Ray, rng: &mut Rng) -> PathSample {
    if scene.settings.spectral {
        let mut wl = Wavelengths::sample(rng.f32());
        let (l, mut out) = trace(scene, ray, rng, &mut wl);
        out.radiance = wl.to_rgb(l);
        out
    } else {
        let (l, mut out) = trace(scene, ray, rng, &mut Rgb);
        out.radiance = l;
        out
    }
}

fn trace<Ch: Channels>(scene: &Scene, mut ray: Ray, rng: &mut Rng, ch: &mut Ch) -> (Ch::C, PathSample) {
    let mut out = PathSample::default();
    let mut l = Ch::C::default();
    let mut beta = Ch::C::splat(1.0);
    // RGB throughput, only used for the albedo AOV.
    let mut beta_rgb = Vec3::ONE;
    // The previous vertex and the pdf of the BSDF sample that left it, for MIS when
    // a light is hit.
    let mut prev_pdf = 0.0f32;
    let mut prev_delta = true;
    let (mut prev_p, mut prev_n) = (Vec3::ZERO, Vec3::ZERO);
    // AOVs are recorded once, at the first non-delta vertex (or on escape).
    let mut aov_pending = true;

    for depth in 0..scene.settings.max_depth {
        let Some(hit) = scene.intersect(&ray, f32::INFINITY) else {
            // Escaped: infinite lights (environment, sun disk).
            let mut seen = Vec3::ZERO;
            for &li in scene.infinite_lights() {
                let le = scene.lights()[li as usize].le(ray.d);
                if le.is_black() {
                    continue;
                }
                seen += le;
                let w = if prev_delta {
                    1.0
                } else {
                    power_heuristic(prev_pdf, scene.infinite_light_pdf(prev_p, prev_n, li, ray.d))
                };
                l += beta * ch.uplift(le) * w;
            }
            if aov_pending {
                aov_pending = false;
                out.albedo = beta_rgb * seen.max(Vec3::ZERO).min(Vec3::ONE);
            }
            break;
        };
        if depth == 0 {
            out.depth = hit.t;
        }
        let si = scene.interaction(&ray, &hit);
        let mat = &scene.materials[si.material as usize];
        let wo_world = -ray.d;
        let ctx = si.tex_ctx();

        // Beer–Lambert: a segment that reaches an absorbing dielectric from its back
        // side travelled through the interior.
        let sigma = mat.absorption();
        if !sigma.is_black() && wo_world.dot(si.ng) < 0.0 {
            beta *= ch.transmittance(sigma, hit.t);
            beta_rgb *= exp3(sigma * -hit.t);
        }

        // Emitters: add emission (MIS-weighted against light sampling) and stop.
        if let Some((le, two_sided)) = mat.emission(&ctx) {
            if aov_pending {
                aov_pending = false;
                out.albedo = beta_rgb * (le / le.max_elem().max(1.0)).max(Vec3::ZERO);
                out.normal = if wo_world.dot(si.ng) < 0.0 { -si.ng } else { si.ng };
            }
            let cos_l = wo_world.dot(si.ng);
            if cos_l > 0.0 || two_sided {
                let w = if prev_delta {
                    1.0
                } else {
                    power_heuristic(prev_pdf, scene.area_light_pdf(prev_p, prev_n, hit.prim, si.p, si.ng))
                };
                l += beta * ch.uplift(le) * w;
            }
            break;
        }

        // Wavelength-dependent refraction: only the hero wavelength can follow the path.
        if mat.is_dispersive() {
            ch.terminate_secondary();
        }

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
        let bsdf = mat.bsdf(&ctx, wo, ch.hero());
        if aov_pending && !bsdf.is_delta() {
            aov_pending = false;
            out.albedo = beta_rgb * bsdf.albedo();
            out.normal = ns;
        }

        // Next-event estimation: sample a light directly.
        if !bsdf.is_delta()
            && scene.has_lights()
            && let Some(ls) = scene.sample_light(si.p, ns, rng)
        {
            let wi = frame.to_local(ls.wi);
            let geo = ls.wi.dot(ng);
            // Both normals must agree on the side, and opaque surfaces only reflect.
            if geo * wi.z > 0.0 && (transmissive || geo > 0.0) {
                let (f, bsdf_pdf) = bsdf.eval(wo, wi);
                if !f.is_black() && !scene.occluded_light(si.p, ng, &ls, ray.time) {
                    let w = if ls.delta { 1.0 } else { power_heuristic(ls.pdf, bsdf_pdf) };
                    l += beta * ch.uplift(f) * ch.uplift(ls.radiance) * (w / ls.pdf);
                }
            }
        }

        // Continue the path by sampling the BSDF.
        let Some(bs) = bsdf.sample(wo, rng) else { break };
        let wi_world = frame.to_world(bs.wi);
        if wi_world.dot(ng) * bs.wi.z <= 0.0 {
            break; // shading and geometric normal disagree about the side
        }
        beta *= ch.uplift(bs.weight);
        beta_rgb *= bs.weight;
        if beta.is_black() || !beta.is_finite() {
            break;
        }
        prev_pdf = bs.pdf;
        prev_delta = bs.delta;
        (prev_p, prev_n) = (si.p, ns);
        ray = Ray::new(offset_origin(si.p, ng, wi_world), wi_world, ray.time);

        // Russian roulette.
        if depth >= 3 {
            let q = beta.max_elem().min(0.95);
            if rng.f32() >= q {
                break;
            }
            beta *= 1.0 / q;
        }
    }
    // A path that ended on delta surfaces only: fall back to the last throughput.
    if aov_pending {
        out.albedo = beta_rgb.max(Vec3::ZERO).min(Vec3::ONE);
    }
    (l, out)
}
