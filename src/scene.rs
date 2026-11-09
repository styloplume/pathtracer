//! The renderable scene: geometry, materials, acceleration structure and lights.

use crate::bvh::Bvh;
use crate::camera::CameraDesc;
use crate::filter::FilterKind;
use crate::geometry::{Aabb, Sphere, Triangle, intersect_triangle, sphere_roots};
use crate::image::Tonemap;
use crate::lights::{Light, LightSampler, LightSamplerKind};
use crate::material::Material;
use crate::math::{Frame, PI, Ray, Vec3};
use crate::output::OutputSettings;
use crate::rng::hash64;
use crate::texture::TexCtx;

#[derive(Clone, Debug)]
pub struct RenderSettings {
    pub width: u32,
    pub height: u32,
    /// Samples per pixel to accumulate before the image is considered finished.
    pub spp: u32,
    /// Maximum number of path segments.
    pub max_depth: u32,
    /// Worker threads; 0 means one per logical CPU.
    pub threads: usize,
    /// Size in pixels of the coarsest preview blocks (rounded up to a power of two).
    pub initial_block: u32,
    /// Exposure in stops.
    pub exposure: f32,
    pub tonemap: Tonemap,
    /// Clamp per-sample luminance to tame fireflies (biased!). 0 disables.
    pub clamp: f32,
    pub seed: u64,
    /// Pixel reconstruction filter and its radius in pixels.
    pub filter: FilterKind,
    pub filter_radius: f32,
    /// Adaptive sampling: pixels whose noise estimate is below this stop receiving
    /// samples; the render ends when all have. 0 disables.
    pub noise_threshold: f32,
    /// Samples every pixel gets before adaptive sampling may stop it.
    pub min_spp: u32,
    /// Stop after this many seconds of rendering. 0 disables.
    pub time_limit: f32,
    /// Hero-wavelength spectral rendering instead of RGB.
    pub spectral: bool,
    /// How next-event estimation picks a light.
    pub light_sampler: LightSamplerKind,
}

impl Default for RenderSettings {
    fn default() -> Self {
        RenderSettings {
            width: 960,
            height: 540,
            spp: 256,
            max_depth: 8,
            threads: 0,
            initial_block: 32,
            exposure: 0.0,
            tonemap: Tonemap::Aces,
            clamp: 0.0,
            seed: 0,
            filter: FilterKind::Gaussian,
            filter_radius: FilterKind::Gaussian.default_radius(),
            noise_threshold: 0.0,
            min_spp: 16,
            time_limit: 0.0,
            spectral: false,
            light_sampler: LightSamplerKind::Bvh,
        }
    }
}

pub struct Hit {
    pub t: f32,
    pub prim: u32,
    pub b1: f32,
    pub b2: f32,
}

pub struct Interaction {
    pub p: Vec3,
    /// Geometric normal (unit length, oriented to agree with the shading normal).
    pub ng: Vec3,
    /// Shading normal (unit length).
    pub ns: Vec3,
    pub uv: [f32; 2],
    /// Position derivatives with respect to u and v (tangents for normal / bump mapping).
    pub dpdu: Vec3,
    pub dpdv: Vec3,
    pub material: u32,
}

impl Interaction {
    pub fn tex_ctx(&self) -> TexCtx {
        TexCtx { uv: self.uv, p: self.p }
    }
}

/// Geometry arrays handed to `Scene::new`.
#[derive(Default)]
pub struct Geometry {
    pub positions: Vec<Vec3>,
    pub normals: Vec<Vec3>,
    pub uvs: Vec<[f32; 2]>,
    pub triangles: Vec<Triangle>,
    pub spheres: Vec<Sphere>,
}

/// Latitude-longitude parameterization: u around +Y starting behind the sphere
/// (u = 0.5 faces +Z), v = 1 at the north pole. Returns (uv, dp/du, dp/dv).
pub(crate) fn sphere_param(n: Vec3, r: f32) -> ([f32; 2], Vec3, Vec3) {
    let phi = n.x.atan2(n.z);
    let theta = n.y.clamp(-1.0, 1.0).acos();
    let uv = [0.5 + phi * (0.5 / PI), 1.0 - theta / PI];
    let (sp, cp) = phi.sin_cos();
    let (st, ct) = theta.sin_cos();
    let dpdu = Vec3::new(cp, 0.0, -sp) * (2.0 * PI * r * st);
    let dpdv = Vec3::new(ct * sp, -st, ct * cp) * (-PI * r);
    (uv, dpdu, dpdv)
}

pub struct Scene {
    pub settings: RenderSettings,
    pub camera: CameraDesc,
    pub materials: Vec<Material>,
    pub positions: Vec<Vec3>,
    pub normals: Vec<Vec3>,
    pub uvs: Vec<[f32; 2]>,
    pub triangles: Vec<Triangle>,
    pub spheres: Vec<Sphere>,
    /// Files the scene was loaded from (scene file first, then meshes, material
    /// libraries and textures); watched for hot reload.
    pub files: Vec<std::path::PathBuf>,
    pub output: OutputSettings,
    bvh: Bvh,
    /// True if any material has an alpha mask (enables alpha testing in traversal).
    any_alpha: bool,
    /// Explicit lights and the environment first, then one area light per emissive primitive.
    pub(crate) lights: Vec<Light>,
    pub(crate) light_sampler: LightSampler,
    /// Light id of each primitive (`lights::NO_LIGHT` for non-emitters).
    pub(crate) prim_light: Vec<u32>,
}

/// Primitive ids are `0..triangles.len()` for triangles, followed by spheres.
impl Scene {
    pub fn new(
        settings: RenderSettings,
        camera: CameraDesc,
        lights: Vec<Light>,
        materials: Vec<Material>,
        geo: Geometry,
    ) -> Scene {
        let any_alpha = materials.iter().any(|m| m.alpha.is_some());
        let mut scene = Scene {
            settings,
            camera,
            materials,
            positions: geo.positions,
            normals: geo.normals,
            uvs: geo.uvs,
            triangles: geo.triangles,
            spheres: geo.spheres,
            files: Vec::new(),
            output: OutputSettings::default(),
            bvh: Bvh { nodes: Vec::new(), indices: Vec::new() },
            any_alpha,
            lights,
            light_sampler: LightSampler::default(),
            prim_light: Vec::new(),
        };
        let bounds: Vec<Aabb> = (0..scene.num_prims() as u32).map(|p| scene.prim_bounds(p)).collect();
        scene.bvh = Bvh::build(&bounds);
        scene.build_lights(scene.settings.light_sampler);
        scene
    }

    pub fn num_prims(&self) -> usize {
        self.triangles.len() + self.spheres.len()
    }

    /// World-space bounds of all geometry (`Aabb::EMPTY` for an empty scene).
    pub fn bounds(&self) -> Aabb {
        self.bvh.nodes.first().map_or(Aabb::EMPTY, |n| n.bounds)
    }

    pub fn stats(&self) -> String {
        format!(
            "{} triangles, {} spheres, {} materials, {} lights ({} emissive primitives), BVH {} nodes / depth {}",
            self.triangles.len(),
            self.spheres.len(),
            self.materials.len(),
            self.lights.len(),
            self.prim_light.iter().filter(|&&l| l != crate::lights::NO_LIGHT).count(),
            self.bvh.nodes.len(),
            self.bvh.depth()
        )
    }

    #[inline]
    pub(crate) fn tri_points(&self, t: &Triangle) -> (Vec3, Vec3, Vec3) {
        (self.positions[t.v[0] as usize], self.positions[t.v[1] as usize], self.positions[t.v[2] as usize])
    }

    pub(crate) fn prim_bounds(&self, prim: u32) -> Aabb {
        let mut b = Aabb::EMPTY;
        let nt = self.triangles.len() as u32;
        if prim < nt {
            let (p0, p1, p2) = self.tri_points(&self.triangles[prim as usize]);
            b.grow_point(p0);
            b.grow_point(p1);
            b.grow_point(p2);
        } else {
            let s = &self.spheres[(prim - nt) as usize];
            b.grow_point(s.center - Vec3::splat(s.radius.abs()));
            b.grow_point(s.center + Vec3::splat(s.radius.abs()));
        }
        b
    }

    pub(crate) fn prim_material(&self, prim: u32) -> u32 {
        let nt = self.triangles.len() as u32;
        if prim < nt { self.triangles[prim as usize].material } else { self.spheres[(prim - nt) as usize].material }
    }

    pub(crate) fn prim_area(&self, prim: u32) -> f32 {
        let nt = self.triangles.len() as u32;
        if prim < nt {
            let (p0, p1, p2) = self.tri_points(&self.triangles[prim as usize]);
            0.5 * (p1 - p0).cross(p2 - p0).length()
        } else {
            let r = self.spheres[(prim - nt) as usize].radius;
            4.0 * PI * r * r
        }
    }

    #[inline]
    fn intersect_prim(&self, prim: u32, ray: &Ray, t_max: f32) -> Option<Hit> {
        let nt = self.triangles.len() as u32;
        if prim < nt {
            let (p0, p1, p2) = self.tri_points(&self.triangles[prim as usize]);
            let (t, b1, b2) = intersect_triangle(ray.o, ray.d, p0, p1, p2, t_max)?;
            let h = Hit { t, prim, b1, b2 };
            (!self.any_alpha || self.alpha_pass(&h, ray)).then_some(h)
        } else {
            let s = &self.spheres[(prim - nt) as usize];
            let (t0, t1) = sphere_roots(ray.o, ray.d, s.center, s.radius)?;
            // With alpha masks the far side may be visible through a cut-out near side.
            for t in [t0, t1] {
                if t > 0.0 && t < t_max {
                    let h = Hit { t, prim, b1: 0.0, b2: 0.0 };
                    if !self.any_alpha || self.alpha_pass(&h, ray) {
                        return Some(h);
                    }
                }
            }
            None
        }
    }

    /// Alpha test: false if the hit falls on a cut-out part of a masked material.
    /// Fractional alpha is resolved stochastically with a hash of the hit point.
    fn alpha_pass(&self, h: &Hit, ray: &Ray) -> bool {
        let Some(tex) = &self.materials[self.prim_material(h.prim) as usize].alpha else { return true };
        let p = ray.o + ray.d * h.t;
        let uv = self.hit_uv(h, p);
        let a = tex.eval_f(&TexCtx { uv, p });
        if a >= 1.0 {
            return true;
        }
        if a <= 0.0 {
            return false;
        }
        let key = u64::from(p.x.to_bits()) ^ (u64::from(p.y.to_bits()) << 21) ^ (u64::from(p.z.to_bits()) << 42);
        let u = (hash64(key ^ u64::from(h.prim)) >> 40) as f32 * (1.0 / (1u64 << 24) as f32);
        u < a
    }

    /// Texture coordinates of a hit (cheaper than a full `interaction`).
    pub(crate) fn hit_uv(&self, h: &Hit, p: Vec3) -> [f32; 2] {
        let nt = self.triangles.len() as u32;
        if h.prim < nt {
            let uv = self.tri_uvs(&self.triangles[h.prim as usize]);
            let b0 = 1.0 - h.b1 - h.b2;
            [uv[0][0] * b0 + uv[1][0] * h.b1 + uv[2][0] * h.b2, uv[0][1] * b0 + uv[1][1] * h.b1 + uv[2][1] * h.b2]
        } else {
            let s = &self.spheres[(h.prim - nt) as usize];
            sphere_param(((p - s.center) * (1.0 / s.radius)).normalize(), s.radius).0
        }
    }

    #[inline]
    fn tri_uvs(&self, t: &Triangle) -> [[f32; 2]; 3] {
        if t.has_uvs() { t.uv.map(|i| self.uvs[i as usize]) } else { [[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]] }
    }

    fn traverse(&self, ray: &Ray, t_max: f32, any_hit: bool) -> Option<Hit> {
        let nodes = &self.bvh.nodes;
        if nodes.is_empty() {
            return None;
        }
        let inv_d = ray.d.recip();
        let mut t_max = t_max;
        if !nodes[0].bounds.hit(ray.o, inv_d, t_max).is_finite() {
            return None;
        }
        let mut best = None;
        let mut stack = [0u32; 96];
        let mut sp = 0;
        let mut idx = 0usize;
        loop {
            let node = &nodes[idx];
            if node.count > 0 {
                for i in node.first..node.first + node.count {
                    let prim = self.bvh.indices[i as usize];
                    if let Some(h) = self.intersect_prim(prim, ray, t_max) {
                        t_max = h.t;
                        best = Some(h);
                        if any_hit {
                            return best;
                        }
                    }
                }
            } else {
                let l = node.first as usize;
                let tl = nodes[l].bounds.hit(ray.o, inv_d, t_max);
                let tr = nodes[l + 1].bounds.hit(ray.o, inv_d, t_max);
                match (tl.is_finite(), tr.is_finite()) {
                    (true, true) => {
                        let (near, far) = if tl <= tr { (l, l + 1) } else { (l + 1, l) };
                        stack[sp] = far as u32;
                        sp += 1;
                        idx = near;
                        continue;
                    }
                    (true, false) => {
                        idx = l;
                        continue;
                    }
                    (false, true) => {
                        idx = l + 1;
                        continue;
                    }
                    (false, false) => {}
                }
            }
            if sp == 0 {
                break;
            }
            sp -= 1;
            idx = stack[sp] as usize;
        }
        best
    }

    pub fn intersect(&self, ray: &Ray, t_max: f32) -> Option<Hit> {
        self.traverse(ray, t_max, false)
    }

    pub fn occluded(&self, ray: &Ray, t_max: f32) -> bool {
        self.traverse(ray, t_max, true).is_some()
    }

    /// Returns (geometric normal, shading normal) at barycentrics (b1, b2).
    pub(crate) fn tri_normals(&self, tri: &Triangle, b1: f32, b2: f32) -> (Vec3, Vec3) {
        let (p0, p1, p2) = self.tri_points(tri);
        let mut ng = (p1 - p0).cross(p2 - p0).normalize();
        if !tri.has_normals() {
            return (ng, ng);
        }
        let b0 = 1.0 - b1 - b2;
        let n = |i: usize| self.normals[tri.n[i] as usize];
        let ns = n(0) * b0 + n(1) * b1 + n(2) * b2;
        let len = ns.length();
        if len.is_nan() || len <= 0.0 {
            return (ng, ng);
        }
        let ns = ns * (1.0 / len);
        if ng.dot(ns) < 0.0 {
            // Trust the authored normals over the winding order.
            ng = -ng;
        }
        (ng, ns)
    }

    pub fn interaction(&self, ray: &Ray, hit: &Hit) -> Interaction {
        let nt = self.triangles.len() as u32;
        let mut si = if hit.prim < nt {
            let tri = &self.triangles[hit.prim as usize];
            let (p0, p1, p2) = self.tri_points(tri);
            let b0 = 1.0 - hit.b1 - hit.b2;
            let p = p0 * b0 + p1 * hit.b1 + p2 * hit.b2;
            let (ng, ns) = self.tri_normals(tri, hit.b1, hit.b2);
            let t = self.tri_uvs(tri);
            let uv = [
                t[0][0] * b0 + t[1][0] * hit.b1 + t[2][0] * hit.b2,
                t[0][1] * b0 + t[1][1] * hit.b1 + t[2][1] * hit.b2,
            ];
            // Solve [dp02 dp12] = [dpdu dpdv] * [duv02 duv12] (pbrt).
            let (d02, d12) = ([t[0][0] - t[2][0], t[0][1] - t[2][1]], [t[1][0] - t[2][0], t[1][1] - t[2][1]]);
            let (dp02, dp12) = (p0 - p2, p1 - p2);
            let det = d02[0] * d12[1] - d02[1] * d12[0];
            let (dpdu, dpdv) = if det.abs() > 1e-12 {
                let inv = 1.0 / det;
                ((dp02 * d12[1] - dp12 * d02[1]) * inv, (dp12 * d02[0] - dp02 * d12[0]) * inv)
            } else {
                (Vec3::ZERO, Vec3::ZERO)
            };
            Interaction { p, ng, ns, uv, dpdu, dpdv, material: tri.material }
        } else {
            let s = &self.spheres[(hit.prim - nt) as usize];
            let p = ray.o + ray.d * hit.t;
            let n = ((p - s.center) * (1.0 / s.radius)).normalize();
            let (uv, dpdu, dpdv) = sphere_param(n, s.radius);
            Interaction { p: s.center + n * s.radius, ng: n, ns: n, uv, dpdu, dpdv, material: s.material }
        };
        let valid = si.dpdu.is_finite() && si.dpdv.is_finite() && si.dpdu.cross(si.dpdv).length_squared() > 0.0;
        if !valid {
            let f = Frame::from_normal(si.ns);
            (si.dpdu, si.dpdv) = (f.t, f.b);
        }
        si
    }
}
