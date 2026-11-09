//! Unidirectional path tracer with next-event estimation and multiple importance sampling.

use crate::math::{Frame, Ray, Vec3, offset_origin};
use crate::rng::Rng;
use crate::sampling::power_heuristic;
use crate::scene::Scene;

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
pub fn radiance(scene: &Scene, mut ray: Ray, rng: &mut Rng) -> PathSample {
    let mut out = PathSample::default();
    let mut l = Vec3::ZERO;
    let mut beta = Vec3::ONE;
    // Pdf of the BSDF sample that generated the current ray, for MIS on emitter hits.
    let mut prev_pdf = 0.0f32;
    let mut prev_delta = true;
    // AOVs are recorded once, at the first non-delta vertex (or on escape).
    let mut aov_pending = true;

    for depth in 0..scene.settings.max_depth {
        let Some(hit) = scene.intersect(&ray, f32::INFINITY) else {
            let bg = scene.background.eval(ray.d);
            if aov_pending {
                aov_pending = false;
                out.albedo = beta * bg.max(Vec3::ZERO).min(Vec3::ONE);
            }
            l += beta * bg;
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
            beta *= exp3(sigma * -hit.t);
        }

        // Emitters: add emission (MIS-weighted against light sampling) and stop.
        if let Some((le, two_sided)) = mat.emission(&ctx) {
            if aov_pending {
                aov_pending = false;
                out.albedo = beta * (le / le.max_elem().max(1.0)).max(Vec3::ZERO);
                out.normal = if wo_world.dot(si.ng) < 0.0 { -si.ng } else { si.ng };
            }
            let cos_l = wo_world.dot(si.ng);
            if cos_l > 0.0 || two_sided {
                if prev_delta {
                    l += beta * le;
                } else {
                    let light_pdf = scene.light_pdf(hit.prim, hit.t * hit.t, cos_l.abs());
                    l += beta * le * power_heuristic(prev_pdf, light_pdf);
                }
            }
            break;
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
        let bsdf = mat.bsdf(&ctx, wo);
        if aov_pending && !bsdf.is_delta() {
            aov_pending = false;
            out.albedo = beta * bsdf.albedo();
            out.normal = ns;
        }

        // Next-event estimation: sample a point on a light directly.
        if !bsdf.is_delta()
            && scene.has_lights()
            && let Some(ls) = scene.sample_light(si.p, rng)
        {
            let wi = frame.to_local(ls.wi);
            let geo = ls.wi.dot(ng);
            // Both normals must agree on the side, and opaque surfaces only reflect.
            if geo * wi.z > 0.0 && (transmissive || geo > 0.0) {
                let (f, bsdf_pdf) = bsdf.eval(wo, wi);
                let origin = offset_origin(si.p, ng, ls.wi);
                if !f.is_black() && !scene.occluded(&Ray::new(origin, ls.wi), ls.dist * (1.0 - 1e-3)) {
                    let w = power_heuristic(ls.pdf, bsdf_pdf);
                    l += beta * f * ls.radiance * (w / ls.pdf);
                }
            }
        }

        // Continue the path by sampling the BSDF.
        let Some(bs) = bsdf.sample(wo, rng) else { break };
        let wi_world = frame.to_world(bs.wi);
        if wi_world.dot(ng) * bs.wi.z <= 0.0 {
            break; // shading and geometric normal disagree about the side
        }
        beta *= bs.weight;
        if beta.is_black() || !beta.is_finite() {
            break;
        }
        prev_pdf = bs.pdf;
        prev_delta = bs.delta;
        ray = Ray::new(offset_origin(si.p, ng, wi_world), wi_world);

        // Russian roulette.
        if depth >= 3 {
            let q = beta.max_elem().min(0.95);
            if rng.f32() >= q {
                break;
            }
            beta /= q;
        }
    }
    // A path that ended on delta surfaces only: fall back to the last throughput.
    if aov_pending {
        out.albedo = beta.max(Vec3::ZERO).min(Vec3::ONE);
    }
    out.radiance = l;
    out
}
