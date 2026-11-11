//! The renderable scene: geometry, materials, acceleration structures and lights.
//!
//! Primitives (triangles, then analytic shapes) are organized in groups, each with its
//! own bottom-level BVH. Group 0 is the static world geometry, already in world space;
//! the other groups hold instancing prototypes and moving objects in their own object
//! space. Instances place a group with a static or animated transform, and a top-level
//! BVH over the instances' bounds finds them. Rays enter an instance by transforming into
//! its object space at the ray's time.

use crate::bvh::Bvh;
use crate::camera::CameraDesc;
use crate::filter::FilterKind;
use crate::geometry::{Aabb, TriRay, Triangle};
use crate::image::Tonemap;
use crate::lights::{Light, LightSampler, LightSamplerKind};
use crate::material::Material;
use crate::math::{Frame, Ray, Transform, Trs, Vec3};
use crate::medium::Medium;
use crate::output::OutputSettings;
use crate::rng::hash64;
use crate::sampler::SamplerKind;
use crate::shapes::{Shape, ShapeKind};
use crate::texture::TexCtx;
use std::ops::Range;
use std::time::{Duration, Instant};

/// `Scene::instance_origin` of the world instance (it comes from no `InstanceDesc`).
const WORLD: u32 = u32::MAX;

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
    /// Sample generator (independent, Sobol, blue noise).
    pub sampler: SamplerKind,
    /// Maximum number of scattering events in media (and subsurface random-walk steps).
    pub max_volume_depth: u32,
    /// Path guiding: learn where light comes from and sample those directions.
    pub guiding: bool,
    /// Samples per pixel spent training the guiding distribution (it is frozen after).
    pub guiding_spp: u32,
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
            sampler: SamplerKind::Sobol,
            max_volume_depth: 256,
            guiding: false,
            guiding_spp: 128,
        }
    }
}

pub struct Hit {
    pub t: f32,
    pub prim: u32,
    /// The instance the primitive was hit through.
    pub inst: u32,
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
    /// Position seen by textures: object space for instances, so world-space patterns
    /// stick to the object as it moves.
    pub p_tex: Vec3,
    /// Geometric normal in texture space (triplanar projection).
    pub n_tex: Vec3,
    /// Texture space to world space (the instance transform); None if they coincide.
    pub tex_xf: Option<Transform>,
}

impl Interaction {
    /// Texture lookup context; `footprint` is the world-space width of the pixel's
    /// footprint here (0 = unknown).
    pub fn tex_ctx(&self, footprint: f32) -> TexCtx {
        let footprint = match &self.tex_xf {
            // Object-space units per world unit, roughly (a cube root of the volume scale).
            Some(xf) => footprint / xf.m.determinant().abs().cbrt().max(1e-12),
            None => footprint,
        };
        TexCtx { uv: self.uv, p: self.p_tex, n: self.n_tex, footprint }
    }

    /// A texture-space gradient as a world-space vector (gradients transform with the
    /// inverse transpose, like normals).
    pub fn grad_to_world(&self, g: Vec3) -> Vec3 {
        self.tex_xf.as_ref().map_or(g, |xf| xf.covector(g))
    }
}

/// How an instance is placed: object space to world space, possibly over time.
#[derive(Clone, Copy, Debug)]
pub enum InstanceXf {
    Identity,
    Static(Transform),
    /// Interpolated from the first (time 0) to the second (time 1) key.
    Motion(Trs, Trs),
}

impl InstanceXf {
    /// The transform at `time`; None for the identity.
    #[inline]
    pub fn at(&self, time: f32) -> Option<Transform> {
        match self {
            InstanceXf::Identity => None,
            InstanceXf::Static(xf) => Some(*xf),
            InstanceXf::Motion(a, b) => Some(a.lerp(b, time).transform_unchecked()),
        }
    }

    /// World bounds of object-space bounds `b` over the whole shutter interval.
    fn bounds(&self, b: &Aabb) -> Aabb {
        match self {
            InstanceXf::Identity => *b,
            InstanceXf::Static(xf) => b.transformed(xf),
            InstanceXf::Motion(a, z) => {
                // Bounds at many times, padded by how far an arc between two of those
                // times can bulge out of the straight line between its ends.
                const STEPS: usize = 64;
                let mut out = Aabb::EMPTY;
                for i in 0..=STEPS {
                    out.grow(&b.transformed(&a.lerp(z, i as f32 / STEPS as f32).transform_unchecked()));
                }
                let dr = (z.rotate - a.rotate).abs();
                let step = (dr.x + dr.y + dr.z).to_radians() / STEPS as f32;
                let s = a.scale.abs().max(z.scale.abs());
                let reach = (b.min.abs().max(b.max.abs()) * s).length();
                let pad = Vec3::splat(reach * step * step / 4.0 + 1e-6 * reach);
                Aabb { min: out.min - pad, max: out.max + pad }
            }
        }
    }
}

/// A placed copy of a geometry group, as handed to `Scene::new`.
#[derive(Clone, Copy, Debug)]
pub struct InstanceDesc {
    pub group: u32,
    pub xf: InstanceXf,
}

/// Where one `[[objects]]` entry's geometry went: its triangles and shapes, and the
/// instances it placed (indices into `Geometry::instances`). Ranges of consecutive
/// objects never overlap and follow the objects' order.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ObjectSpan {
    pub triangles: Range<u32>,
    pub shapes: Range<u32>,
    pub instances: Range<u32>,
}

/// Which scene entries produced what (for the editor): one span per `[[objects]]` entry,
/// and the range of `Scene::lights` holding the `[[lights]]` entries, in order.
#[derive(Clone, Debug, Default)]
pub struct Provenance {
    pub objects: Vec<ObjectSpan>,
    pub lights: Range<u32>,
}

/// Geometry arrays handed to `Scene::new`.
#[derive(Default)]
pub struct Geometry {
    pub positions: Vec<Vec3>,
    pub normals: Vec<Vec3>,
    pub uvs: Vec<[f32; 2]>,
    pub triangles: Vec<Triangle>,
    pub shapes: Vec<Shape>,
    /// Group of each triangle and shape; missing entries mean group 0 (the world).
    pub tri_group: Vec<u32>,
    pub shape_group: Vec<u32>,
    /// Placements of groups other than the world.
    pub instances: Vec<InstanceDesc>,
}

pub struct Scene {
    pub settings: RenderSettings,
    pub camera: CameraDesc,
    pub materials: Vec<Material>,
    pub positions: Vec<Vec3>,
    pub normals: Vec<Vec3>,
    pub uvs: Vec<[f32; 2]>,
    pub triangles: Vec<Triangle>,
    pub shapes: Vec<Shape>,
    /// Files the scene was loaded from (scene file first, then includes, meshes, material
    /// libraries and textures); watched for hot reload.
    pub files: Vec<std::path::PathBuf>,
    pub output: OutputSettings,
    /// Participating media, referenced by materials' medium interfaces.
    pub media: Vec<Medium>,
    /// True if any material declares media (paths must track the medium they are in).
    pub(crate) has_media: bool,
    /// One BVH per geometry group; leaf indices are primitive ids.
    blas: Vec<Bvh>,
    instances: Vec<InstanceDesc>,
    /// Index into `Geometry::instances` of each instance (`WORLD` for the world group).
    instance_origin: Vec<u32>,
    /// The scene entries each primitive, instance and light comes from (set by the loader).
    pub provenance: Provenance,
    /// BVH over the instances' world bounds; leaf indices are instance ids.
    tlas: Bvh,
    /// True if any material has an alpha mask (enables alpha testing in traversal).
    any_alpha: bool,
    /// Explicit lights and the environment first, then one area light per emissive
    /// primitive of the world group.
    pub(crate) lights: Vec<Light>,
    pub(crate) light_sampler: LightSampler,
    /// Light id of each primitive (`lights::NO_LIGHT` for non-emitters).
    pub(crate) prim_light: Vec<u32>,
    /// Point and spot lights (equi-angular sampling in media).
    pub(crate) point_lights: Vec<u32>,
    /// Time `Scene::new` spent building the acceleration structures (BLAS + TLAS).
    pub bvh_build_time: Duration,
}

/// Primitive ids are `0..triangles.len()` for triangles, followed by shapes.
impl Scene {
    pub fn new(
        settings: RenderSettings,
        camera: CameraDesc,
        lights: Vec<Light>,
        materials: Vec<Material>,
        geo: Geometry,
    ) -> Scene {
        let any_alpha = materials.iter().any(|m| m.alpha.is_some());
        let has_media =
            materials.iter().any(|m| m.media.is_some_and(|mi| mi.interior.is_some() || mi.exterior.is_some()));
        let mut scene = Scene {
            settings,
            camera,
            materials,
            positions: geo.positions,
            normals: geo.normals,
            uvs: geo.uvs,
            triangles: geo.triangles,
            shapes: geo.shapes,
            files: Vec::new(),
            output: OutputSettings::default(),
            media: Vec::new(),
            has_media,
            blas: Vec::new(),
            instances: Vec::new(),
            instance_origin: Vec::new(),
            provenance: Provenance::default(),
            tlas: Bvh::build(&[]),
            any_alpha,
            lights,
            light_sampler: LightSampler::default(),
            prim_light: Vec::new(),
            point_lights: Vec::new(),
            bvh_build_time: Duration::ZERO,
        };

        // Primitives of each group.
        let build_start = Instant::now();
        let nt = scene.triangles.len();
        let group_of = |groups: &[u32], i: usize| groups.get(i).copied().unwrap_or(0) as usize;
        let n_groups = 1 + geo
            .tri_group
            .iter()
            .chain(&geo.shape_group)
            .copied()
            .chain(geo.instances.iter().map(|i| i.group))
            .max()
            .unwrap_or(0) as usize;
        let mut members: Vec<Vec<u32>> = vec![Vec::new(); n_groups];
        for i in 0..nt {
            members[group_of(&geo.tri_group, i)].push(i as u32);
        }
        for i in 0..scene.shapes.len() {
            members[group_of(&geo.shape_group, i)].push((nt + i) as u32);
        }
        scene.blas = members
            .iter()
            .map(|m| {
                let bounds: Vec<Aabb> = m.iter().map(|&p| scene.prim_bounds(p)).collect();
                let mut bvh = Bvh::build(&bounds);
                for i in &mut bvh.indices {
                    *i = m[*i as usize];
                }
                bvh
            })
            .collect();

        if !members[0].is_empty() {
            scene.instances.push(InstanceDesc { group: 0, xf: InstanceXf::Identity });
            scene.instance_origin.push(WORLD);
        }
        for (i, inst) in geo.instances.iter().enumerate() {
            if !members[inst.group as usize].is_empty() {
                scene.instances.push(*inst);
                scene.instance_origin.push(i as u32);
            }
        }
        let inst_bounds: Vec<Aabb> =
            scene.instances.iter().map(|i| i.xf.bounds(&scene.blas[i.group as usize].bounds())).collect();
        scene.tlas = Bvh::build(&inst_bounds);
        scene.bvh_build_time = build_start.elapsed();

        let instanced_emitters =
            members[1..].iter().flatten().any(|&p| scene.materials[scene.prim_material(p) as usize].is_emissive());
        if instanced_emitters {
            eprintln!(
                "warning: emissive surfaces of instances and moving objects are not sampled as lights \
                 (they only add light when hit by chance: expect noise)"
            );
        }
        scene.build_lights(scene.settings.light_sampler, &members[0]);
        scene
    }

    pub fn num_prims(&self) -> usize {
        self.triangles.len() + self.shapes.len()
    }

    /// World-space bounds of all geometry (`Aabb::EMPTY` for an empty scene).
    pub fn bounds(&self) -> Aabb {
        self.tlas.bounds()
    }

    pub fn stats(&self) -> String {
        let nodes: usize = self.blas.iter().map(Bvh::node_count).sum::<usize>() + self.tlas.node_count();
        let depth = self.blas.iter().map(Bvh::depth).max().unwrap_or(0);
        let instances = self.instances.iter().filter(|i| i.group != 0).count();
        let instanced = if instances > 0 {
            // Primitives as rendered: each instance counts its group's primitives.
            let rendered: usize = self.instances.iter().map(|i| self.blas[i.group as usize].indices.len()).sum();
            format!(", {instances} instances of {} groups ({rendered} primitives rendered)", self.blas.len() - 1)
        } else {
            String::new()
        };
        format!(
            "{} triangles, {} shapes{instanced}, {} materials, {} lights ({} emissive primitives), BVH4 {} nodes / \
             depth {}",
            self.triangles.len(),
            self.shapes.len(),
            self.materials.len(),
            self.lights.len(),
            self.prim_light.iter().filter(|&&l| l != crate::lights::NO_LIGHT).count(),
            nodes,
            depth
        )
    }

    /// The placed geometry groups: the world (group 0, identity) if it has primitives,
    /// then every instance whose group has primitives.
    pub fn instances(&self) -> &[InstanceDesc] {
        &self.instances
    }

    /// The primitives of geometry group `group`.
    pub fn group_prims(&self, group: u32) -> &[u32] {
        &self.blas[group as usize].indices
    }

    /// Bounds of geometry group `group` in its own space.
    pub fn group_bounds(&self, group: u32) -> Aabb {
        self.blas[group as usize].bounds()
    }

    /// The `[[objects]]` entry that placed instance `inst` (an index into `instances()`);
    /// None for the world instance.
    pub fn instance_object(&self, inst: u32) -> Option<usize> {
        let origin = *self.instance_origin.get(inst as usize)?;
        if origin == WORLD {
            return None;
        }
        let objs = &self.provenance.objects;
        let i = objs.partition_point(|s| s.instances.end <= origin);
        objs.get(i).filter(|s| s.instances.contains(&origin)).map(|_| i)
    }

    /// The `[[objects]]` entry whose geometry a hit on primitive `prim` through instance
    /// `inst` belongs to (an instance's placing object, else the primitive's own).
    pub fn object_of(&self, prim: u32, inst: u32) -> Option<usize> {
        if let Some(i) = self.instance_object(inst) {
            return Some(i);
        }
        let objs = &self.provenance.objects;
        let nt = self.triangles.len() as u32;
        let i = if prim < nt {
            objs.partition_point(|s| s.triangles.end <= prim)
        } else {
            objs.partition_point(|s| s.shapes.end <= prim - nt)
        };
        let s = objs.get(i)?;
        let inside = if prim < nt { s.triangles.contains(&prim) } else { s.shapes.contains(&(prim - nt)) };
        inside.then_some(i)
    }

    /// Moves the geometry of `[[objects]]` entry `object` by `delta` (applied after its
    /// current placement) without rebuilding the acceleration structures: world geometry is
    /// transformed in place, the instances it placed get `delta` on top, and the BVHs are
    /// refitted. Returns false, changing nothing, when that cannot represent the result
    /// (a shape the transform would turn into another kind, moving instances): the caller
    /// then loads the scene again.
    pub fn transform_object(&mut self, object: usize, delta: &Transform) -> bool {
        let Some(span) = self.provenance.objects.get(object).cloned() else { return false };
        let placed: Vec<usize> = (0..self.instances.len())
            .filter(|&k| self.instance_origin[k] != WORLD && span.instances.contains(&self.instance_origin[k]))
            .collect();
        if placed.iter().any(|&k| matches!(self.instances[k].xf, InstanceXf::Motion(..))) {
            return false;
        }
        // The object's own geometry groups (moving objects) are placed, not moved.
        let own: std::collections::HashSet<u32> =
            placed.iter().flat_map(|&k| self.blas[self.instances[k].group as usize].indices.iter().copied()).collect();
        let nt = self.triangles.len() as u32;
        let tris: Vec<u32> = span.triangles.clone().filter(|t| !own.contains(t)).collect();
        let shapes: Vec<u32> = span.shapes.clone().filter(|s| !own.contains(&(nt + s))).collect();
        let mut moved_shapes = Vec::with_capacity(shapes.len());
        for &s in &shapes {
            match self.shapes[s as usize].kind.transformed(delta) {
                Some(k) => moved_shapes.push(k),
                None => return false,
            }
        }
        for (&s, k) in shapes.iter().zip(moved_shapes) {
            self.shapes[s as usize].kind = k;
        }
        let (mut vs, mut ns) = (std::collections::HashSet::new(), std::collections::HashSet::new());
        for &t in &tris {
            let tri = &self.triangles[t as usize];
            vs.extend(tri.v);
            if tri.has_normals() {
                ns.extend(tri.n);
            }
        }
        for v in vs {
            self.positions[v as usize] = delta.point(self.positions[v as usize]);
        }
        for n in ns {
            self.normals[n as usize] = delta.normal(self.normals[n as usize]);
        }
        if delta.flips_handedness() {
            // Mirrored triangles keep facing outwards.
            for &t in &tris {
                let tri = &mut self.triangles[t as usize];
                tri.v.swap(1, 2);
                tri.n.swap(1, 2);
                tri.uv.swap(1, 2);
            }
        }
        for &k in &placed {
            let xf = &mut self.instances[k].xf;
            *xf = match *xf {
                InstanceXf::Identity => InstanceXf::Static(*delta),
                InstanceXf::Static(x) => InstanceXf::Static(x.then(delta)),
                InstanceXf::Motion(..) => unreachable!("refused above"),
            };
        }
        if let Some(world) = self.blas.first() {
            let bounds: Vec<(u32, Aabb)> = world.indices.iter().map(|&p| (p, self.prim_bounds(p))).collect();
            let lookup: std::collections::HashMap<u32, Aabb> = bounds.into_iter().collect();
            self.blas[0].refit(|p| lookup[&p]);
        }
        let inst_bounds: Vec<Aabb> =
            self.instances.iter().map(|i| i.xf.bounds(&self.blas[i.group as usize].bounds())).collect();
        self.tlas.refit(|i| inst_bounds[i as usize]);
        let emits = tris
            .iter()
            .chain(&shapes.iter().map(|s| nt + s).collect::<Vec<_>>())
            .any(|&p| self.prim_light.get(p as usize).is_some_and(|&l| l != crate::lights::NO_LIGHT));
        if emits {
            self.rebuild_lights();
        }
        true
    }

    /// Replaces the light of `[[lights]]` entry `entry` (a light moved in the editor).
    pub fn set_light(&mut self, entry: usize, light: Light) -> bool {
        let i = self.provenance.lights.start as usize + entry;
        if i >= self.provenance.lights.end as usize {
            return false;
        }
        self.lights[i] = light;
        self.rebuild_lights();
        true
    }

    /// Builds the area lights and the light sampler again (after emitters or lights moved).
    fn rebuild_lights(&mut self) {
        self.lights.retain(|l| !matches!(l, Light::Area { .. }));
        let world = self.blas.first().map(|b| b.indices.clone()).unwrap_or_default();
        self.build_lights(self.settings.light_sampler, &world);
    }

    #[inline]
    pub fn tri_points(&self, t: &Triangle) -> (Vec3, Vec3, Vec3) {
        (self.positions[t.v[0] as usize], self.positions[t.v[1] as usize], self.positions[t.v[2] as usize])
    }

    /// The shape of primitive `prim`, None for triangles.
    #[inline]
    pub fn shape(&self, prim: u32) -> Option<&Shape> {
        (prim as usize).checked_sub(self.triangles.len()).map(|i| &self.shapes[i])
    }

    /// Bounds in the primitive's own (group) space.
    pub(crate) fn prim_bounds(&self, prim: u32) -> Aabb {
        if let Some(s) = self.shape(prim) {
            return s.kind.bounds();
        }
        let (p0, p1, p2) = self.tri_points(&self.triangles[prim as usize]);
        let mut b = Aabb::EMPTY;
        b.grow_point(p0);
        b.grow_point(p1);
        b.grow_point(p2);
        b
    }

    pub fn prim_material(&self, prim: u32) -> u32 {
        self.shape(prim).map_or_else(|| self.triangles[prim as usize].material, |s| s.material)
    }

    pub(crate) fn prim_area(&self, prim: u32) -> f32 {
        if let Some(s) = self.shape(prim) {
            return s.kind.area();
        }
        let (p0, p1, p2) = self.tri_points(&self.triangles[prim as usize]);
        0.5 * (p1 - p0).cross(p2 - p0).length()
    }

    /// Intersects one primitive with a ray in its group's space (`d` unit length).
    #[inline]
    fn intersect_prim(&self, prim: u32, inst: u32, tri: &TriRay, o: Vec3, d: Vec3, t_max: f32) -> Option<Hit> {
        let Some(s) = self.shape(prim) else {
            let (p0, p1, p2) = self.tri_points(&self.triangles[prim as usize]);
            let (t, b1, b2) = tri.intersect(p0, p1, p2, t_max)?;
            let h = Hit { t, prim, inst, b1, b2 };
            return (!self.any_alpha || self.alpha_pass(&h, o, d)).then_some(h);
        };
        if let ShapeKind::Implicit(im) = &s.kind {
            let h = Hit { t: im.intersect(o, d, t_max)?, prim, inst, b1: 0.0, b2: 0.0 };
            return (!self.any_alpha || self.alpha_pass(&h, o, d)).then_some(h);
        }
        let (t0, t1) = s.kind.hits(o, d)?;
        // With alpha masks the far side may be visible through a cut-out near side.
        for t in [t0, t1] {
            if t > 0.0 && t < t_max {
                let h = Hit { t, prim, inst, b1: 0.0, b2: 0.0 };
                if !self.any_alpha || self.alpha_pass(&h, o, d) {
                    return Some(h);
                }
            }
        }
        None
    }

    /// Alpha test: false if the hit falls on a cut-out part of a masked material.
    /// Fractional alpha is resolved stochastically with a hash of the hit point.
    fn alpha_pass(&self, h: &Hit, o: Vec3, d: Vec3) -> bool {
        let Some(tex) = &self.materials[self.prim_material(h.prim) as usize].alpha else { return true };
        let p = o + d * h.t;
        let uv = self.hit_uv(h, p);
        let a = tex.eval_f(&TexCtx { uv, p, ..TexCtx::default() });
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

    /// Texture coordinates of a hit at group-space point `p` (cheaper than a full `interaction`).
    pub(crate) fn hit_uv(&self, h: &Hit, p: Vec3) -> [f32; 2] {
        if let Some(s) = self.shape(h.prim) {
            return s.kind.surface(p).uv;
        }
        let uv = self.tri_uvs(&self.triangles[h.prim as usize]);
        let b0 = 1.0 - h.b1 - h.b2;
        [uv[0][0] * b0 + uv[1][0] * h.b1 + uv[2][0] * h.b2, uv[0][1] * b0 + uv[1][1] * h.b1 + uv[2][1] * h.b2]
    }

    #[inline]
    fn tri_uvs(&self, t: &Triangle) -> [[f32; 2]; 3] {
        if t.has_uvs() { t.uv.map(|i| self.uvs[i as usize]) } else { [[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]] }
    }

    /// Closest (or, with `any_hit`, any) hit of group `group` for a group-space ray.
    fn traverse_group(&self, group: u32, inst: u32, o: Vec3, d: Vec3, t_max: f32, any_hit: bool) -> Option<Hit> {
        let bvh = &self.blas[group as usize];
        let tri = TriRay::new(o, d);
        let mut best = None;
        bvh.traverse(o, d, t_max, |first, count, t_max| {
            for k in first..first + count {
                let prim = bvh.indices[k as usize];
                if let Some(h) = self.intersect_prim(prim, inst, &tri, o, d, *t_max) {
                    *t_max = h.t;
                    best = Some(h);
                    if any_hit {
                        return true;
                    }
                }
            }
            false
        });
        best
    }

    fn intersect_instance(&self, id: u32, ray: &Ray, t_max: f32, any_hit: bool) -> Option<Hit> {
        let inst = &self.instances[id as usize];
        let Some(xf) = inst.xf.at(ray.time) else {
            return self.traverse_group(inst.group, id, ray.o, ray.d, t_max, any_hit);
        };
        // Object-space ray with a unit direction; distances scale by `len`.
        let d = xf.inv_vector(ray.d);
        let len = d.length();
        if len <= 0.0 || !len.is_finite() {
            return None;
        }
        let o = xf.inv_point(ray.o);
        let mut h = self.traverse_group(inst.group, id, o, d * (1.0 / len), t_max * len, any_hit)?;
        h.t /= len;
        Some(h)
    }

    fn traverse(&self, ray: &Ray, t_max: f32, any_hit: bool) -> Option<Hit> {
        if let [only] = self.instances.as_slice()
            && matches!(only.xf, InstanceXf::Identity)
        {
            return self.traverse_group(only.group, 0, ray.o, ray.d, t_max, any_hit);
        }
        let mut best = None;
        self.tlas.traverse(ray.o, ray.d, t_max, |first, count, t_max| {
            for k in first..first + count {
                let id = self.tlas.indices[k as usize];
                if let Some(h) = self.intersect_instance(id, ray, *t_max, any_hit) {
                    *t_max = h.t;
                    best = Some(h);
                    if any_hit {
                        return true;
                    }
                }
            }
            false
        });
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

    /// Surface data in the primitive's group space; `p` is the hit point there.
    fn local_interaction(&self, hit: &Hit, p: Vec3) -> Interaction {
        if let Some(s) = self.shape(hit.prim) {
            let sp = s.kind.surface(p);
            return Interaction {
                p: sp.p,
                ng: sp.n,
                ns: sp.n,
                uv: sp.uv,
                dpdu: sp.dpdu,
                dpdv: sp.dpdv,
                material: s.material,
                p_tex: sp.p,
                n_tex: sp.n,
                tex_xf: None,
            };
        }
        let tri = &self.triangles[hit.prim as usize];
        let (p0, p1, p2) = self.tri_points(tri);
        let b0 = 1.0 - hit.b1 - hit.b2;
        let p = p0 * b0 + p1 * hit.b1 + p2 * hit.b2;
        let (ng, ns) = self.tri_normals(tri, hit.b1, hit.b2);
        let t = self.tri_uvs(tri);
        let uv =
            [t[0][0] * b0 + t[1][0] * hit.b1 + t[2][0] * hit.b2, t[0][1] * b0 + t[1][1] * hit.b1 + t[2][1] * hit.b2];
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
        Interaction { p, ng, ns, uv, dpdu, dpdv, material: tri.material, p_tex: p, n_tex: ng, tex_xf: None }
    }

    pub fn interaction(&self, ray: &Ray, hit: &Hit) -> Interaction {
        let xf = self.instances[hit.inst as usize].xf.at(ray.time);
        let p = ray.o + ray.d * hit.t;
        let mut si = match &xf {
            None => self.local_interaction(hit, p),
            Some(xf) => {
                let mut si = self.local_interaction(hit, xf.inv_point(p));
                si.p = xf.point(si.p);
                si.ng = xf.normal(si.ng);
                si.ns = xf.normal(si.ns);
                if si.ng.dot(si.ns) < 0.0 {
                    si.ng = -si.ng;
                }
                si.dpdu = xf.vector(si.dpdu);
                si.dpdv = xf.vector(si.dpdv);
                si.tex_xf = Some(*xf);
                si
            }
        };
        let valid = si.dpdu.is_finite() && si.dpdv.is_finite() && si.dpdu.cross(si.dpdv).length_squared() > 0.0;
        if !valid {
            let f = Frame::from_normal(si.ns);
            (si.dpdu, si.dpdv) = (f.t, f.b);
        }
        si
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::{NO_NORMALS, NO_UVS};

    fn unit_tri_geometry() -> Geometry {
        Geometry {
            positions: vec![Vec3::new(-1.0, -1.0, 0.0), Vec3::new(1.0, -1.0, 0.0), Vec3::new(0.0, 1.0, 0.0)],
            triangles: vec![Triangle { v: [0, 1, 2], n: [NO_NORMALS; 3], uv: [NO_UVS; 3], material: 0 }],
            ..Geometry::default()
        }
    }

    fn scene(geo: Geometry) -> Scene {
        Scene::new(RenderSettings::default(), CameraDesc::default(), vec![], vec![Material::diffuse(Vec3::ONE)], geo)
    }

    /// A prototype (group 1) placed by several instances: rays find each copy at the
    /// right distance with world-space normals.
    #[test]
    fn instances_place_prototypes() {
        let mut geo = unit_tri_geometry();
        geo.tri_group = vec![1];
        geo.shapes.push(Shape { kind: ShapeKind::Sphere { center: Vec3::ZERO, radius: 0.5 }, material: 0 });
        geo.shape_group = vec![1];
        for i in 0..50 {
            let trs = Trs {
                translate: Vec3::new(i as f32 * 3.0, 0.0, -10.0),
                rotate: Vec3::new(0.0, 0.0, 0.0),
                scale: Vec3::new(1.0, 1.0, 2.0),
            };
            geo.instances.push(InstanceDesc { group: 1, xf: InstanceXf::Static(trs.transform().unwrap()) });
        }
        let s = scene(geo);
        assert!(s.stats().contains("50 instances"), "{}", s.stats());
        for i in [0, 17, 49] {
            let o = Vec3::new(i as f32 * 3.0, 0.0, 0.0);
            let ray = Ray::new(o, Vec3::new(0.0, 0.0, -1.0), 0.0);
            let h = s.intersect(&ray, f32::INFINITY).expect("hits the instance");
            // The sphere, scaled to radius 1 along z, sticks out in front of the triangle.
            assert!((h.t - 9.0).abs() < 1e-4, "t = {}", h.t);
            let si = s.interaction(&ray, &h);
            assert!((si.p - Vec3::new(o.x, 0.0, -9.0)).length() < 1e-4);
            assert!(si.ng.dot(Vec3::Z) > 0.999, "{:?}", si.ng);
            assert!(s.occluded(&ray, 9.5) && !s.occluded(&ray, 8.5));
        }
        assert!(s.intersect(&Ray::new(Vec3::new(1.5, 0.0, 0.0), Vec3::new(0.0, 0.0, -1.0), 0.0), 1e9).is_none());
    }

    /// A moving instance is found where its transform puts it at the ray's time.
    #[test]
    fn motion_follows_ray_time() {
        let mut geo = Geometry::default();
        geo.shapes.push(Shape { kind: ShapeKind::Sphere { center: Vec3::ZERO, radius: 0.5 }, material: 0 });
        geo.shape_group = vec![1];
        let a = Trs { translate: Vec3::new(0.0, 0.0, -5.0), ..Trs::IDENTITY };
        let b = Trs { translate: Vec3::new(4.0, 0.0, -5.0), rotate: Vec3::new(0.0, 720.0, 0.0), ..Trs::IDENTITY };
        geo.instances.push(InstanceDesc { group: 1, xf: InstanceXf::Motion(a, b) });
        let s = scene(geo);
        let b = s.bounds();
        assert!(b.min.x <= -0.5 && b.max.x >= 4.5, "motion bounds {b:?}");
        for (time, x) in [(0.0, 0.0), (0.5, 2.0), (1.0, 4.0)] {
            let hit = |x: f32| s.intersect(&Ray::new(Vec3::new(x, 0.0, 0.0), Vec3::new(0.0, 0.0, -1.0), time), 1e9);
            assert!(hit(x).is_some_and(|h| (h.t - 4.5).abs() < 1e-4), "time {time}");
            assert!(hit(x + 1.0).is_none() && hit(x - 1.0).is_none(), "time {time}");
        }
    }
}
