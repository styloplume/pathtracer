//! Unidirectional path tracer with next-event estimation and multiple importance sampling.

use crate::material::Material;
use crate::math::{Frame, Ray, Vec3, offset_origin};
use crate::rng::Rng;
use crate::sampling::power_heuristic;
use crate::scene::Scene;

/// Estimates the radiance arriving along `ray`.
pub fn radiance(scene: &Scene, mut ray: Ray, rng: &mut Rng) -> Vec3 {
    let mut l = Vec3::ZERO;
    let mut beta = Vec3::ONE;
    // Pdf of the BSDF sample that generated the current ray, for MIS on emitter hits.
    let mut prev_pdf = 0.0f32;
    let mut prev_delta = true;

    for depth in 0..scene.settings.max_depth {
        let Some(hit) = scene.intersect(&ray, f32::INFINITY) else {
            l += beta * scene.background.eval(ray.d);
            break;
        };
        let si = scene.interaction(&ray, &hit);
        let mat = &scene.materials[si.material as usize];
        let wo_world = -ray.d;

        // Emitters: add emission (MIS-weighted against light sampling) and stop.
        if let Material::Emissive { radiance, two_sided } = *mat {
            let cos_l = wo_world.dot(si.ng);
            if cos_l > 0.0 || two_sided {
                if prev_delta {
                    l += beta * radiance;
                } else {
                    let light_pdf = scene.light_pdf(hit.prim, hit.t * hit.t, cos_l.abs());
                    l += beta * radiance * power_heuristic(prev_pdf, light_pdf);
                }
            }
            break;
        }

        // Opaque surfaces are two-sided: shade the side the ray arrived from.
        // Dielectrics keep the outward normal so they know whether we're entering.
        let transmissive = mat.is_transmissive();
        let (mut ns, ng) = if !transmissive && wo_world.dot(si.ng) < 0.0 { (-si.ns, -si.ng) } else { (si.ns, si.ng) };
        if !transmissive && wo_world.dot(ns) <= 0.0 {
            ns = ng; // shading normal faces away from the viewer; fall back to flat
        }
        let frame = Frame::from_normal(ns);
        let wo = frame.to_local(wo_world);

        // Next-event estimation: sample a point on a light directly.
        if !mat.is_delta() && scene.has_lights() {
            let origin = offset_origin(si.p, ng, ng);
            if let Some(ls) = scene.sample_light(origin, rng) {
                if ls.wi.dot(ng) > 0.0 {
                    let (f, bsdf_pdf) = mat.eval(wo, frame.to_local(ls.wi));
                    if !f.is_black() && !scene.occluded(&Ray::new(origin, ls.wi), ls.dist * (1.0 - 1e-3)) {
                        let w = power_heuristic(ls.pdf, bsdf_pdf);
                        l += beta * f * ls.radiance * (w / ls.pdf);
                    }
                }
            }
        }

        // Continue the path by sampling the BSDF.
        let Some(bs) = mat.sample(wo, rng) else { break };
        let wi_world = frame.to_world(bs.wi);
        if !transmissive && wi_world.dot(ng) <= 0.0 {
            break; // shading normal sent us below the geometric surface
        }
        beta *= bs.weight;
        if beta.is_black() {
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
    l
}
