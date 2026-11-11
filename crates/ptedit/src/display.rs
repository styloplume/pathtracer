//! What the viewport draws, built once per scene from the render-ready `Scene` (the
//! same geometry the tracer renders): a list of draws, each a part placed in the world and
//! owned by the `[[objects]]` entry that made it.
//!
//! Triangles are copied into compact meshes, one per object (world geometry) or per
//! geometry group (prototypes, moving objects). Analytic shapes become canonical meshes
//! (unit sphere, tube, disk, cube) placed by a transform and tessellated per frame at a
//! density that follows their size on screen. Implicit surfaces show as bounding boxes,
//! and so do the instances of large scatters.

use pt_core::geometry::{Aabb, NO_NORMALS, NO_UVS, Triangle};
use pt_core::math::{Mat3, PI, Transform, Vec3};
use pt_core::scene::Scene;
use pt_core::shapes::ShapeKind;
use std::collections::{HashMap, HashSet};

/// Objects placing more instances than this show them as bounding boxes.
pub const SCATTER_BOXES: usize = 500;
/// Segments around a shape at each level of detail.
pub const LEVELS: [u32; 5] = [8, 16, 32, 64, 128];

/// Triangles with optional per-corner normals (`Triangle::n` indexes `normals`).
pub struct Mesh {
    pub positions: Vec<Vec3>,
    pub normals: Vec<Vec3>,
    pub triangles: Vec<Triangle>,
    /// Unique edges, for wireframes.
    pub edges: Vec<[u32; 2]>,
    pub bounds: Aabb,
    /// Triangles drawn highlighted (the selected faces of a mesh being edited); empty for
    /// none.
    pub tint: Vec<bool>,
}

impl Default for Mesh {
    fn default() -> Mesh {
        Mesh {
            positions: Vec::new(),
            normals: Vec::new(),
            triangles: Vec::new(),
            edges: Vec::new(),
            bounds: Aabb::EMPTY,
            tint: Vec::new(),
        }
    }
}

impl Mesh {
    /// A polygon mesh being edited: its faces as triangles in `material`, its polygon
    /// edges (no diagonals) for wireframes, and the faces `tinted` highlighted.
    pub fn of_polygons(m: &pt_geom::Mesh, material: u32, tinted: &[bool]) -> Mesh {
        let mut d = Mesh { positions: m.positions.clone(), ..Mesh::default() };
        for (hs, f) in m.triangles() {
            d.tri(hs.map(|h| m.origin(h)), false);
            if let Some(t) = d.triangles.last_mut() {
                t.material = material;
            }
            d.tint.push(tinted.get(f as usize).copied().unwrap_or(false));
        }
        if !d.tint.iter().any(|&t| t) {
            d.tint.clear();
        }
        d.edges = m.edges().into_iter().map(|(a, b)| [a, b]).collect();
        d.set_positions(&m.positions);
        d
    }

    /// New positions for the same triangles (a mesh being edited, moved).
    pub fn set_positions(&mut self, p: &[Vec3]) {
        self.positions.clear();
        self.positions.extend_from_slice(p);
        self.bounds = Aabb::EMPTY;
        for &q in &self.positions {
            self.bounds.grow_point(q);
        }
    }

    fn finish(mut self) -> Mesh {
        let mut seen = HashSet::new();
        for t in &self.triangles {
            for k in 0..3 {
                let (a, b) = (t.v[k], t.v[(k + 1) % 3]);
                if seen.insert((a.min(b), a.max(b))) {
                    self.edges.push([a, b]);
                }
            }
        }
        self.bounds = Aabb::EMPTY;
        for &p in &self.positions {
            self.bounds.grow_point(p);
        }
        self
    }

    fn tri(&mut self, v: [u32; 3], smooth: bool) {
        let n = if smooth { v } else { [NO_NORMALS; 3] };
        self.triangles.push(Triangle { v, n, uv: [NO_UVS; 3], material: 0 });
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Canonical {
    /// Unit sphere at the origin.
    Sphere,
    /// Open tube of radius 1 around +z, from z = 0 to 1.
    Tube,
    /// Disk of radius 1 in the xy plane facing +z, with a hole of the given radius
    /// (thousandths).
    Disk(u32),
    /// The cube [-1, 1]³ (one level of detail).
    Cube,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Part {
    /// An index into `Display::meshes`.
    Mesh(u32),
    /// A canonical shape, tessellated per frame.
    Shape(Canonical),
    /// The cube [-1, 1]³ as a wire box only: the bounds standing in for an implicit
    /// surface.
    Bounds,
    /// The cube [-1, 1]³ standing in for a whole instance (of a large scatter): solid
    /// where surfaces are drawn.
    Proxy,
}

pub struct Draw {
    pub part: Part,
    /// Part space to world.
    pub xf: Transform,
    /// The `[[objects]]` entry (None never happens for scene geometry, but keeps the
    /// display usable for scenes the provenance does not cover).
    pub object: Option<u32>,
    /// Material of a whole part (shapes, proxies); meshes carry theirs per triangle and
    /// this is their first one, which stand-ins for them use.
    pub material: u32,
    pub bounds: Aabb,
}

pub struct Display {
    pub meshes: Vec<Mesh>,
    pub draws: Vec<Draw>,
    /// Display color (sRGB, 0..1) per scene material.
    pub colors: Vec<[f32; 3]>,
    /// World bounds per `[[objects]]` entry.
    pub object_bounds: Vec<Aabb>,
    pub bounds: Aabb,
    /// Tessellations of the canonical shapes, per level of detail.
    canonical: HashMap<(Canonical, usize), u32>,
    /// The mesh standing in for an object being edited: object and `meshes` index.
    edit: Option<(usize, u32)>,
}

fn linear_to_srgb(c: f32) -> f32 {
    if c <= 0.003_130_8 { c * 12.92 } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 }
}

/// The transform whose columns are `a`, `b`, `c`, then `t`.
fn columns(a: Vec3, b: Vec3, c: Vec3, t: Vec3) -> Transform {
    let m = Mat3 { m: [[a.x, b.x, c.x], [a.y, b.y, c.y], [a.z, b.z, c.z]] };
    Transform::from_matrix(m, t).unwrap_or(Transform::IDENTITY)
}

/// The wire box of `b` as a placement of the cube [-1, 1]³.
pub fn box_xf(b: &Aabb) -> Transform {
    let e = ((b.max - b.min) * 0.5).max(Vec3::splat(1e-6));
    columns(Vec3::new(e.x, 0.0, 0.0), Vec3::new(0.0, e.y, 0.0), Vec3::new(0.0, 0.0, e.z), b.centroid())
}

/// A shape as a part and its placement; implicit surfaces as their bounding box.
fn shape_part(kind: &ShapeKind) -> (Part, Transform) {
    match *kind {
        ShapeKind::Sphere { center, radius } => {
            let r = radius.abs();
            (
                Part::Shape(Canonical::Sphere),
                columns(Vec3::new(r, 0.0, 0.0), Vec3::new(0.0, r, 0.0), Vec3::Z * r, center),
            )
        }
        ShapeKind::Disk { center, frame, radius, inner } => {
            let hole = ((inner / radius).clamp(0.0, 0.999) * 1000.0).round() as u32;
            (Part::Shape(Canonical::Disk(hole)), columns(frame.t * radius, frame.b * radius, frame.n * radius, center))
        }
        ShapeKind::Cylinder { base, frame, radius, height } => {
            (Part::Shape(Canonical::Tube), columns(frame.t * radius, frame.b * radius, frame.n * height, base))
        }
        ShapeKind::Box { center, frame, half } => {
            (Part::Shape(Canonical::Cube), columns(frame.t * half.x, frame.b * half.y, frame.n * half.z, center))
        }
        ShapeKind::Implicit(_) => (Part::Bounds, box_xf(&kind.bounds())),
    }
}

impl Display {
    pub fn build(scene: &Scene) -> Display {
        let mut d = Display {
            meshes: Vec::new(),
            draws: Vec::new(),
            colors: scene.materials.iter().map(|m| m.preview_color().to_srgb()).collect(),
            object_bounds: vec![Aabb::EMPTY; scene.provenance.objects.len()],
            bounds: Aabb::EMPTY,
            canonical: HashMap::new(),
            edit: None,
        };
        d.meshes.push(cube_mesh());
        if d.colors.is_empty() {
            d.colors.push([0.8; 3]);
        }

        // The instances each object placed, and the primitives of those instances' groups.
        let mut placed: Vec<Vec<usize>> = vec![Vec::new(); scene.provenance.objects.len()];
        for (i, _) in scene.instances().iter().enumerate() {
            if let Some(o) = scene.instance_object(i as u32) {
                placed[o].push(i);
            }
        }
        let nt = scene.triangles.len() as u32;
        let mut group_parts: HashMap<u32, Vec<(Part, Transform, u32)>> = HashMap::new();

        for (o, span) in scene.provenance.objects.iter().enumerate() {
            let owner = Some(o as u32);
            let instanced: HashSet<u32> =
                placed[o].iter().flat_map(|&i| scene.group_prims(scene.instances()[i].group).iter().copied()).collect();
            // World geometry of this object.
            let tris: Vec<u32> = span.triangles.clone().filter(|t| !instanced.contains(t)).collect();
            if !tris.is_empty() {
                let mesh = d.add_mesh(scene, &tris);
                d.push(Part::Mesh(mesh), Transform::IDENTITY, owner, scene.triangles[tris[0] as usize].material);
            }
            for s in span.shapes.clone() {
                if instanced.contains(&(nt + s)) {
                    continue;
                }
                let shape = &scene.shapes[s as usize];
                let (part, xf) = shape_part(&shape.kind);
                d.push(part, xf, owner, shape.material);
            }
            // Instances: their group's parts, or a box each for large scatters.
            let boxes = placed[o].len() > SCATTER_BOXES;
            for &i in &placed[o] {
                let inst = &scene.instances()[i];
                let Some(ixf) = inst.xf.at(0.0) else { continue };
                if boxes {
                    let gb = scene.group_bounds(inst.group);
                    let material = scene.group_prims(inst.group).first().map_or(0, |&p| scene.prim_material(p));
                    d.push(Part::Proxy, box_xf(&gb).then(&ixf), owner, material);
                    continue;
                }
                let parts = group_parts.entry(inst.group).or_insert_with(|| {
                    let prims = scene.group_prims(inst.group);
                    let tris: Vec<u32> = prims.iter().copied().filter(|&p| p < nt).collect();
                    let mut parts = Vec::new();
                    if !tris.is_empty() {
                        let mesh = d.meshes.len() as u32;
                        d.meshes.push(mesh_of(scene, &tris));
                        parts.push((Part::Mesh(mesh), Transform::IDENTITY, scene.triangles[tris[0] as usize].material));
                    }
                    for &p in prims.iter().filter(|&&p| p >= nt) {
                        let shape = &scene.shapes[(p - nt) as usize];
                        let (part, xf) = shape_part(&shape.kind);
                        parts.push((part, xf, shape.material));
                    }
                    parts
                });
                for (part, xf, material) in parts.clone() {
                    d.push(part, xf.then(&ixf), owner, material);
                }
            }
        }
        d
    }

    /// Moves what `[[objects]]` entry `object` draws by `delta` (after its placement), as
    /// `Scene::transform_object` moves its geometry.
    pub fn transform_object(&mut self, object: usize, delta: &Transform) {
        let mut b = Aabb::EMPTY;
        let local: Vec<(usize, Aabb)> = (0..self.draws.len())
            .filter(|&i| self.draws[i].object == Some(object as u32))
            .map(|i| (i, self.local_bounds(self.draws[i].part)))
            .collect();
        for (i, lb) in local {
            let d = &mut self.draws[i];
            d.xf = d.xf.then(delta);
            d.bounds = lb.transformed(&d.xf);
            b.grow(&d.bounds);
        }
        if let Some(ob) = self.object_bounds.get_mut(object) {
            *ob = b;
        }
        self.bounds = Aabb::EMPTY;
        for d in &self.draws {
            self.bounds.grow(&d.bounds);
        }
    }

    /// The material object `o` is drawn with (its first part's).
    pub fn material_of(&self, o: usize) -> u32 {
        self.draws.iter().find(|d| d.object == Some(o as u32)).map_or(0, |d| d.material)
    }

    /// Shows `mesh` placed by `xf` in place of what object `o` draws (an object being
    /// edited); called again with the mesh changed.
    pub fn set_edit_mesh(&mut self, o: usize, mesh: Mesh, xf: Transform) {
        let material = self.draws.iter().find(|d| d.object == Some(o as u32)).map_or(0, |d| d.material);
        self.draws.retain(|d| d.object != Some(o as u32));
        let index = if let Some((_, i)) = self.edit {
            self.meshes[i as usize] = mesh;
            i
        } else {
            self.meshes.push(mesh);
            self.meshes.len() as u32 - 1
        };
        self.edit = Some((o, index));
        let bounds = self.meshes[index as usize].bounds.transformed(&xf);
        self.draws.push(Draw { part: Part::Mesh(index), xf, object: Some(o as u32), material, bounds });
        self.edit_bounds();
    }

    /// The mesh being edited moved to positions `p` (the same faces).
    pub fn move_edit_mesh(&mut self, p: &[Vec3]) {
        let Some((_, i)) = self.edit else { return };
        self.meshes[i as usize].set_positions(p);
        let b = self.meshes[i as usize].bounds;
        if let Some(d) = self.draws.iter_mut().rev().find(|d| d.part == Part::Mesh(i)) {
            d.bounds = b.transformed(&d.xf);
        }
        self.edit_bounds();
    }

    /// Bounds again after the mesh being edited changed.
    fn edit_bounds(&mut self) {
        let Some((o, _)) = self.edit else { return };
        let mut ob = Aabb::EMPTY;
        self.bounds = Aabb::EMPTY;
        for d in &self.draws {
            self.bounds.grow(&d.bounds);
            if d.object == Some(o as u32) {
                ob.grow(&d.bounds);
            }
        }
        if let Some(b) = self.object_bounds.get_mut(o) {
            *b = ob;
        }
    }

    fn add_mesh(&mut self, scene: &Scene, tris: &[u32]) -> u32 {
        self.meshes.push(mesh_of(scene, tris));
        self.meshes.len() as u32 - 1
    }

    fn push(&mut self, part: Part, xf: Transform, object: Option<u32>, material: u32) {
        if let Part::Shape(c) = part {
            self.add_canonical(c);
        }
        let bounds = self.local_bounds(part).transformed(&xf);
        self.bounds.grow(&bounds);
        if let Some(o) = object {
            self.object_bounds[o as usize].grow(&bounds);
        }
        self.draws.push(Draw { part, xf, object, material, bounds });
    }

    /// Bounds of a part in its own space.
    fn local_bounds(&self, part: Part) -> Aabb {
        match part {
            Part::Mesh(m) => self.meshes[m as usize].bounds,
            Part::Shape(Canonical::Tube) => Aabb { min: Vec3::new(-1.0, -1.0, 0.0), max: Vec3::ONE },
            Part::Shape(Canonical::Disk(_)) => Aabb { min: Vec3::new(-1.0, -1.0, 0.0), max: Vec3::new(1.0, 1.0, 0.0) },
            Part::Shape(Canonical::Sphere | Canonical::Cube) | Part::Bounds | Part::Proxy => {
                Aabb { min: Vec3::splat(-1.0), max: Vec3::ONE }
            }
        }
    }

    /// The mesh of canonical shape `c` at level of detail `level` (an index into `LEVELS`).
    pub fn canonical(&self, c: Canonical, level: usize) -> u32 {
        self.canonical.get(&(c, level.min(LEVELS.len() - 1))).copied().unwrap_or(CUBE)
    }

    /// Tessellates `c` at every level of detail, once.
    fn add_canonical(&mut self, c: Canonical) {
        if c == Canonical::Cube || self.canonical.contains_key(&(c, 0)) {
            return;
        }
        for (level, &n) in LEVELS.iter().enumerate() {
            let mesh = match c {
                Canonical::Sphere => sphere_mesh(n),
                Canonical::Tube => tube_mesh(n),
                Canonical::Disk(hole) => disk_mesh(n, hole as f32 / 1000.0),
                Canonical::Cube => cube_mesh(),
            };
            self.meshes.push(mesh);
            self.canonical.insert((c, level), self.meshes.len() as u32 - 1);
        }
    }
}

/// `Display::meshes` index of the cube [-1, 1]³, whose edges are the 12 box edges.
pub const CUBE: u32 = 0;

trait Srgb {
    fn to_srgb(self) -> [f32; 3];
}

impl Srgb for Vec3 {
    fn to_srgb(self) -> [f32; 3] {
        [linear_to_srgb(self.x), linear_to_srgb(self.y), linear_to_srgb(self.z)]
    }
}

/// A compact copy of scene triangles `tris` (their own vertex and normal arrays).
fn mesh_of(scene: &Scene, tris: &[u32]) -> Mesh {
    let mut m = Mesh::default();
    let (mut vmap, mut nmap) = (HashMap::new(), HashMap::new());
    for &t in tris {
        let src = &scene.triangles[t as usize];
        let v = src.v.map(|i| {
            *vmap.entry(i).or_insert_with(|| {
                m.positions.push(scene.positions[i as usize]);
                m.positions.len() as u32 - 1
            })
        });
        let n = if src.has_normals() {
            src.n.map(|i| {
                *nmap.entry(i).or_insert_with(|| {
                    m.normals.push(scene.normals[i as usize]);
                    m.normals.len() as u32 - 1
                })
            })
        } else {
            [NO_NORMALS; 3]
        };
        m.triangles.push(Triangle { v, n, uv: [NO_UVS; 3], material: src.material });
    }
    m.finish()
}

fn sphere_mesh(n: u32) -> Mesh {
    let rings = (n / 2).max(2);
    let mut m = Mesh::default();
    for r in 0..=rings {
        let theta = PI * r as f32 / rings as f32;
        let (st, ct) = theta.sin_cos();
        for s in 0..=n {
            let phi = 2.0 * PI * s as f32 / n as f32;
            let p = Vec3::new(st * phi.cos(), ct, st * phi.sin());
            m.positions.push(p);
            m.normals.push(p);
        }
    }
    let at = |r: u32, s: u32| r * (n + 1) + s;
    for r in 0..rings {
        for s in 0..n {
            let (a, b, c, d) = (at(r, s), at(r, s + 1), at(r + 1, s + 1), at(r + 1, s));
            if r > 0 {
                m.tri([a, b, c], true);
            }
            if r + 1 < rings {
                m.tri([a, c, d], true);
            }
        }
    }
    m.finish()
}

fn tube_mesh(n: u32) -> Mesh {
    let mut m = Mesh::default();
    for s in 0..n {
        let phi = 2.0 * PI * s as f32 / n as f32;
        let (y, x) = phi.sin_cos();
        m.positions.extend([Vec3::new(x, y, 0.0), Vec3::new(x, y, 1.0)]);
        m.normals.extend([Vec3::new(x, y, 0.0); 2]);
    }
    for s in 0..n {
        let (a, b) = (2 * s, 2 * ((s + 1) % n));
        m.tri([a, b, b + 1], true);
        m.tri([a, b + 1, a + 1], true);
    }
    m.finish()
}

fn disk_mesh(n: u32, hole: f32) -> Mesh {
    let mut m = Mesh::default();
    let ring = |r: f32, s: u32| {
        let phi = 2.0 * PI * s as f32 / n as f32;
        Vec3::new(r * phi.cos(), r * phi.sin(), 0.0)
    };
    if hole <= 0.0 {
        m.positions.push(Vec3::ZERO);
        m.positions.extend((0..n).map(|s| ring(1.0, s)));
        for s in 0..n {
            m.tri([0, 1 + s, 1 + (s + 1) % n], false);
        }
    } else {
        for s in 0..n {
            m.positions.extend([ring(hole, s), ring(1.0, s)]);
        }
        for s in 0..n {
            let (a, b) = (2 * s, 2 * ((s + 1) % n));
            m.tri([a, b, b + 1], false);
            m.tri([a, b + 1, a + 1], false);
        }
    }
    m.finish()
}

fn cube_mesh() -> Mesh {
    let mut m = Mesh::default();
    for i in 0..8u32 {
        let c = |bit: u32| if i & bit == 0 { -1.0 } else { 1.0 };
        m.positions.push(Vec3::new(c(1), c(2), c(4)));
    }
    // Two triangles per face, as quads of corner indices.
    for q in [[0, 2, 3, 1], [4, 5, 7, 6], [0, 1, 5, 4], [2, 6, 7, 3], [0, 4, 6, 2], [1, 3, 7, 5]] {
        m.tri([q[0], q[1], q[2]], false);
        m.tri([q[0], q[2], q[3]], false);
    }
    let mut m = m.finish();
    // The wireframe of a box is its 12 edges, without the face diagonals.
    m.edges.retain(|&[a, b]| (a ^ b).is_power_of_two());
    m
}
