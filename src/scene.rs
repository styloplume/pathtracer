//! The renderable scene: geometry, materials, acceleration structure and lights.

use crate::bvh::Bvh;
use crate::camera::CameraDesc;
use crate::geometry::{Aabb, Sphere, Triangle, intersect_sphere, intersect_triangle};
use crate::image::Tonemap;
use crate::material::Material;
use crate::math::{PI, Ray, Vec3};
use crate::rng::Rng;
use crate::sampling::{uniform_sphere, uniform_triangle};

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
        }
    }
}

#[derive(Clone, Debug)]
pub enum Background {
    Color(Vec3),
    /// Simple sky: blends horizon -> zenith above the horizon and horizon -> ground below.
    Gradient { zenith: Vec3, horizon: Vec3, ground: Vec3 },
}

impl Background {
    #[inline]
    pub fn eval(&self, d: Vec3) -> Vec3 {
        match *self {
            Background::Color(c) => c,
            Background::Gradient { zenith, horizon, ground } => {
                if d.y >= 0.0 {
                    horizon.lerp(zenith, d.y.sqrt())
                } else {
                    horizon.lerp(ground, (-d.y * 8.0).min(1.0))
                }
            }
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
    pub material: u32,
}

pub struct LightSample {
    pub wi: Vec3,
    pub dist: f32,
    pub radiance: Vec3,
    /// Solid-angle pdf (including the light selection probability).
    pub pdf: f32,
}

pub struct Scene {
    pub settings: RenderSettings,
    pub camera: CameraDesc,
    pub background: Background,
    pub materials: Vec<Material>,
    pub positions: Vec<Vec3>,
    pub normals: Vec<Vec3>,
    pub triangles: Vec<Triangle>,
    pub spheres: Vec<Sphere>,
    /// Files the scene was loaded from (scene file first, then meshes); watched for hot reload.
    pub files: Vec<std::path::PathBuf>,
    bvh: Bvh,
    light_prims: Vec<u32>,
    light_cdf: Vec<f32>,
    /// Per primitive: (probability of selecting it as a light) / area. Zero for non-emitters.
    light_pdf_area: Vec<f32>,
}

/// Primitive ids are `0..triangles.len()` for triangles, followed by spheres.
impl Scene {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        settings: RenderSettings,
        camera: CameraDesc,
        background: Background,
        materials: Vec<Material>,
        positions: Vec<Vec3>,
        normals: Vec<Vec3>,
        triangles: Vec<Triangle>,
        spheres: Vec<Sphere>,
    ) -> Scene {
        let mut scene = Scene {
            settings,
            camera,
            background,
            materials,
            positions,
            normals,
            triangles,
            spheres,
            files: Vec::new(),
            bvh: Bvh { nodes: Vec::new(), indices: Vec::new() },
            light_prims: Vec::new(),
            light_cdf: Vec::new(),
            light_pdf_area: Vec::new(),
        };
        let bounds: Vec<Aabb> = (0..scene.num_prims() as u32).map(|p| scene.prim_bounds(p)).collect();
        scene.bvh = Bvh::build(&bounds);
        scene.build_lights();
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
            "{} triangles, {} spheres, {} materials, {} emitters, BVH {} nodes / depth {}",
            self.triangles.len(),
            self.spheres.len(),
            self.materials.len(),
            self.light_prims.len(),
            self.bvh.nodes.len(),
            self.bvh.depth()
        )
    }

    #[inline]
    fn tri_points(&self, t: &Triangle) -> (Vec3, Vec3, Vec3) {
        (self.positions[t.v[0] as usize], self.positions[t.v[1] as usize], self.positions[t.v[2] as usize])
    }

    fn prim_bounds(&self, prim: u32) -> Aabb {
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

    fn prim_material(&self, prim: u32) -> u32 {
        let nt = self.triangles.len() as u32;
        if prim < nt { self.triangles[prim as usize].material } else { self.spheres[(prim - nt) as usize].material }
    }

    fn prim_area(&self, prim: u32) -> f32 {
        let nt = self.triangles.len() as u32;
        if prim < nt {
            let (p0, p1, p2) = self.tri_points(&self.triangles[prim as usize]);
            0.5 * (p1 - p0).cross(p2 - p0).length()
        } else {
            let r = self.spheres[(prim - nt) as usize].radius;
            4.0 * PI * r * r
        }
    }

    fn build_lights(&mut self) {
        let n = self.num_prims();
        let mut weights = Vec::new();
        for prim in 0..n as u32 {
            if let Material::Emissive { radiance, .. } = self.materials[self.prim_material(prim) as usize] {
                let area = self.prim_area(prim);
                let lum = radiance.luminance();
                if lum > 0.0 && area > 0.0 {
                    self.light_prims.push(prim);
                    weights.push((lum * area) as f64);
                }
            }
        }
        self.light_pdf_area = vec![0.0; n];
        let total: f64 = weights.iter().sum();
        let mut acc = 0.0f64;
        for (i, &prim) in self.light_prims.iter().enumerate() {
            acc += weights[i];
            self.light_cdf.push((acc / total) as f32);
            self.light_pdf_area[prim as usize] = (weights[i] / total) as f32 / self.prim_area(prim);
        }
    }

    pub fn has_lights(&self) -> bool {
        !self.light_prims.is_empty()
    }

    #[inline]
    fn intersect_prim(&self, prim: u32, ray: &Ray, t_max: f32) -> Option<Hit> {
        let nt = self.triangles.len() as u32;
        if prim < nt {
            let (p0, p1, p2) = self.tri_points(&self.triangles[prim as usize]);
            intersect_triangle(ray.o, ray.d, p0, p1, p2, t_max).map(|(t, b1, b2)| Hit { t, prim, b1, b2 })
        } else {
            let s = &self.spheres[(prim - nt) as usize];
            intersect_sphere(ray.o, ray.d, s.center, s.radius, t_max).map(|t| Hit { t, prim, b1: 0.0, b2: 0.0 })
        }
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
    fn tri_normals(&self, tri: &Triangle, b1: f32, b2: f32) -> (Vec3, Vec3) {
        let (p0, p1, p2) = self.tri_points(tri);
        let mut ng = (p1 - p0).cross(p2 - p0).normalize();
        if !tri.has_normals() {
            return (ng, ng);
        }
        let b0 = 1.0 - b1 - b2;
        let n = |i: usize| self.normals[tri.n[i] as usize];
        let ns = n(0) * b0 + n(1) * b1 + n(2) * b2;
        let len = ns.length();
        if !(len > 0.0) {
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
        if hit.prim < nt {
            let tri = &self.triangles[hit.prim as usize];
            let (p0, p1, p2) = self.tri_points(tri);
            let p = p0 * (1.0 - hit.b1 - hit.b2) + p1 * hit.b1 + p2 * hit.b2;
            let (ng, ns) = self.tri_normals(tri, hit.b1, hit.b2);
            Interaction { p, ng, ns, material: tri.material }
        } else {
            let s = &self.spheres[(hit.prim - nt) as usize];
            let p = ray.o + ray.d * hit.t;
            let n = ((p - s.center) * (1.0 / s.radius)).normalize();
            Interaction { p: s.center + n * s.radius, ng: n, ns: n, material: s.material }
        }
    }

    /// Picks an emitter proportionally to its power and samples a point on it uniformly by area.
    pub fn sample_light(&self, p: Vec3, rng: &mut Rng) -> Option<LightSample> {
        if self.light_prims.is_empty() {
            return None;
        }
        let u = rng.f32();
        let i = self.light_cdf.partition_point(|&c| c <= u).min(self.light_prims.len() - 1);
        let prim = self.light_prims[i];
        let nt = self.triangles.len() as u32;
        let (lp, ln, mat) = if prim < nt {
            let tri = &self.triangles[prim as usize];
            let (b1, b2) = uniform_triangle(rng.f32(), rng.f32());
            let (p0, p1, p2) = self.tri_points(tri);
            let lp = p0 * (1.0 - b1 - b2) + p1 * b1 + p2 * b2;
            (lp, self.tri_normals(tri, b1, b2).0, tri.material)
        } else {
            let s = &self.spheres[(prim - nt) as usize];
            let n = uniform_sphere(rng.f32(), rng.f32());
            (s.center + n * s.radius, n, s.material)
        };
        let Material::Emissive { radiance, two_sided } = self.materials[mat as usize] else {
            return None;
        };
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
        let pdf = self.light_pdf_area[prim as usize] * dist2 / cos_l;
        Some(LightSample { wi, dist, radiance, pdf })
    }

    /// Solid-angle pdf that `sample_light` would have produced for a point on `prim`.
    #[inline]
    pub fn light_pdf(&self, prim: u32, dist2: f32, cos_l: f32) -> f32 {
        self.light_pdf_area[prim as usize] * dist2 / cos_l.max(1e-8)
    }
}
