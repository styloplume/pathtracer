//! Edit mode: one object turned into a polygon mesh (`pt-geom`) and modeled in the
//! viewport. Its vertices, edges or faces are selected by clicking (double-click: loops,
//! Ctrl: rings), dragging a rectangle, growing, shrinking or picking whole linked parts;
//! the gizmos move, turn and scale them (with proportional editing and snapping); the
//! operations of `pt_geom::ops` change the mesh, extrude / inset / bevel following the
//! mouse until a click, the loop cut and the knife pointing at what they cut.
//!
//! The mesh lives in the object's own space (before its `translate` / `rotate` /
//! `scale`) and has its own undo steps. Leaving edit mode writes it to a new PLY file
//! next to the scene (OBJ when it has texture coordinates or material groups) and points
//! the object at it: one undoable edit of the scene, which keeps the old file (so undo
//! brings the old geometry back). Shapes (spheres, boxes, ...) become mesh objects then.

use crate::editor::{Editor, Target};
use crate::files;
use crate::overlay::{Line3, Overlays, SELECTED};
use crate::raster::LineStyle;
use crate::render::Shading;
use crate::tools::ToolDrag;
use crate::view::Proj;
use crate::workspace::Rect as ViewRect;
use crate::xform::{self, read_vec};
use pt_core::document::Kind;
use pt_core::geometry::Aabb;
use pt_core::math::{Transform, Trs, Vec3};
use pt_core::platform::{Key, Modifiers, MouseButton};
use pt_core::toml::{Seg, Table, Value};
use pt_geom::{Mesh, Mode, Selection, edge_key, ops, primitives, select};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

const CREASE: u32 = 0xd0_50_d0;
/// Segments around round shapes turned into meshes.
const SEGMENTS: u32 = 24;
/// Keys that give a shape its geometry, dropped when it becomes a mesh object.
const SHAPE_KEYS: &[&str] = &[
    "center",
    "radius",
    "size",
    "min",
    "max",
    "base",
    "axis",
    "height",
    "normal",
    "inner_radius",
    "origin",
    "u",
    "v",
    "p0",
    "p1",
    "p2",
    "capped",
];

/// The mesh and selection before a change (an undo step).
#[derive(Clone, Debug)]
pub struct Snapshot {
    pub mesh: Mesh,
    pub sel: Selection,
    pub label: String,
}

/// An object being edited as a mesh.
pub struct EditMesh {
    /// The `[[objects]]` entry, and where its table is.
    pub object: usize,
    pub file: usize,
    pub path: Vec<Seg>,
    pub label: String,
    /// The object's type as written, and the mesh file it was read from.
    pub ty: String,
    pub source: Option<PathBuf>,
    /// In the object's own space.
    pub mesh: Mesh,
    pub sel: Selection,
    pub mode: Mode,
    /// Object to world.
    pub xf: Transform,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    /// Bumped by every change of the mesh or the selection.
    pub version: u64,
    /// Proportional editing, and its radius (world units).
    pub proportional: bool,
    pub radius: f32,
}

impl EditMesh {
    pub fn world(&self, v: u32) -> Vec3 {
        self.xf.point(self.mesh.positions[v as usize])
    }

    pub fn snapshot(&self, label: &str) -> Snapshot {
        Snapshot { mesh: self.mesh.clone(), sel: self.sel.clone(), label: label.to_owned() }
    }

    /// The mesh changed from `before`.
    pub fn record(&mut self, before: Snapshot) {
        self.undo.push(before);
        self.redo.clear();
        self.version += 1;
    }

    /// Takes the result of an operation (an undo step).
    pub fn take(&mut self, done: ops::Done, label: &str) {
        let before = self.snapshot(label);
        self.mesh = done.mesh;
        self.sel = done.sel;
        self.sel.flush(&self.mesh, self.mode);
        self.record(before);
    }

    pub fn restore(&mut self, s: Snapshot) {
        self.mesh = s.mesh;
        self.sel = s.sel;
        self.version += 1;
    }

    /// Whether the mesh differs from what was read (it has undo steps).
    pub fn changed(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn undo_label(&self) -> Option<&str> {
        self.undo.last().map(|s| s.label.as_str())
    }

    pub fn redo_label(&self) -> Option<&str> {
        self.redo.last().map(|s| s.label.as_str())
    }

    pub fn undo(&mut self) -> Option<String> {
        let s = self.undo.pop()?;
        let now = self.snapshot(&s.label);
        let label = s.label.clone();
        self.redo.push(now);
        self.restore(s);
        Some(label)
    }

    pub fn redo(&mut self) -> Option<String> {
        let s = self.redo.pop()?;
        let now = self.snapshot(&s.label);
        let label = s.label.clone();
        self.undo.push(now);
        self.restore(s);
        Some(label)
    }

    /// The middle of the selected vertices, in the world.
    pub fn pivot(&self) -> Option<Vec3> {
        let v = self.sel.vert_list();
        (!v.is_empty()).then(|| v.iter().fold(Vec3::ZERO, |a, &v| a + self.world(v)) * (1.0 / v.len() as f32))
    }

    /// World bounds of the selected vertices (or of all of them).
    pub fn bounds(&self, selected: bool) -> Aabb {
        let mut b = Aabb::EMPTY;
        for v in 0..self.mesh.vertex_count() as u32 {
            if !selected || self.sel.has_vert(v) {
                b.grow_point(self.world(v));
            }
        }
        b
    }

    /// The object's own axes in the world (unit).
    pub fn axes(&self) -> [Vec3; 3] {
        [0, 1, 2].map(|k| {
            let mut a = Vec3::ZERO;
            a[k] = 1.0;
            let w = self.xf.vector(a);
            if w.length_squared() > 0.0 { w.normalize() } else { a }
        })
    }

    /// Object units per world unit (for amounts typed or dragged in the world).
    fn local_scale(&self) -> f32 {
        let s = self.xf.vector(Vec3::ONE).length() / Vec3::ONE.length();
        if s > 0.0 && s.is_finite() { 1.0 / s } else { 1.0 }
    }

    /// The vertices a drag moves and how much: the selected ones fully and, with
    /// proportional editing, the others within the radius less the farther they are.
    pub fn weights(&self) -> Vec<(u32, f32)> {
        let sel = self.sel.vert_list();
        if !self.proportional {
            return sel.into_iter().map(|v| (v, 1.0)).collect();
        }
        let r = self.radius.max(1e-9);
        let cell = |p: Vec3| ((p.x / r).floor() as i64, (p.y / r).floor() as i64, (p.z / r).floor() as i64);
        let mut grid: HashMap<(i64, i64, i64), Vec<Vec3>> = HashMap::new();
        for &v in &sel {
            let p = self.world(v);
            grid.entry(cell(p)).or_default().push(p);
        }
        let mut out = Vec::new();
        for v in 0..self.mesh.vertex_count() as u32 {
            if self.sel.has_vert(v) {
                out.push((v, 1.0));
                continue;
            }
            let p = self.world(v);
            let (x, y, z) = cell(p);
            let mut d = f32::INFINITY;
            for k in 0..27 {
                if let Some(ps) = grid.get(&(x + k % 3 - 1, y + k / 3 % 3 - 1, z + k / 9 - 1)) {
                    d = ps.iter().fold(d, |d, &q| d.min((q - p).length()));
                }
            }
            if d < r {
                let t = d / r;
                out.push((v, 1.0 - t * t * (3.0 - 2.0 * t)));
            }
        }
        out
    }
}

/// The transform of an object's `translate`, `rotate` and `scale`.
pub fn object_xf(t: &Table) -> Transform {
    let trs = Trs {
        translate: read_vec(t, "translate").unwrap_or(Vec3::ZERO),
        rotate: read_vec(t, "rotate").unwrap_or(Vec3::ZERO),
        scale: read_vec(t, "scale").unwrap_or(Vec3::ONE),
    };
    trs.transform().unwrap_or(Transform::IDENTITY)
}

/// An object's geometry as a mesh in its own space, and the file it came from; `dir` is
/// where its file's relative paths start.
pub fn object_mesh(t: &Table, dir: &Path) -> Result<(Mesh, Option<PathBuf>), String> {
    let up = Vec3::new(0.0, 1.0, 0.0);
    let v = |k: &str, d: Vec3| read_vec(t, k).unwrap_or(d);
    let n = |k: &str, d: f32| t.get(k).and_then(Value::as_f64).map_or(d, |x| x as f32);
    let unit = |k: &str| {
        let a = v(k, up);
        if a.length_squared() > 0.0 { a.normalize() } else { up }
    };
    let req = |k: &str| read_vec(t, k).ok_or_else(|| format!("the object has no '{k}'"));
    let mesh = match xform::object_type(t) {
        "sphere" => primitives::uv_sphere(v("center", Vec3::ZERO), n("radius", 1.0), SEGMENTS, SEGMENTS / 2),
        "box" | "cube" => match (read_vec(t, "min"), read_vec(t, "max")) {
            (Some(a), Some(b)) => primitives::cube((a + b) * 0.5, b - a),
            _ => primitives::cube(v("center", Vec3::ZERO), v("size", Vec3::ONE)),
        },
        "cylinder" | "tube" => {
            let capped = t.get("capped").and_then(Value::as_bool).unwrap_or(true);
            let (r, h) = (n("radius", 1.0), n("height", 1.0));
            primitives::cylinder(v("base", Vec3::ZERO), unit("axis"), r, h, SEGMENTS, capped)
        }
        "disk" | "disc" => {
            primitives::disk(v("center", Vec3::ZERO), unit("normal"), n("radius", 1.0), n("inner_radius", 0.0), 32)
        }
        "quad" => primitives::quad(req("origin")?, req("u")?, req("v")?),
        "triangle" => primitives::triangle(req("p0")?, req("p1")?, req("p2")?),
        "mesh" | "stl" => {
            let file = t.get("file").and_then(Value::as_str).ok_or("the mesh object has no 'file'")?;
            let path = dir.join(file);
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
            if matches!(ext.as_str(), "gltf" | "glb") {
                return Err("glTF meshes cannot be edited (convert them to OBJ or PLY)".into());
            }
            return Ok((pt_geom::io::load(&path)?, Some(path)));
        }
        "gltf" => return Err("glTF meshes cannot be edited (convert them to OBJ or PLY)".into()),
        "instance" => return Err("instances are edited through their prototype's objects".into()),
        other => return Err(format!("{other} objects are not meshes: shapes and mesh files can be edited")),
    };
    Ok((mesh, None))
}

/// A file name from an object's label.
fn stem_of(label: &str) -> String {
    let base = Path::new(label).file_stem().map_or_else(|| label.to_owned(), |s| s.to_string_lossy().into_owned());
    let s: String = base.chars().map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '-' }).collect();
    let s = s.trim_matches('-').to_owned();
    if s.is_empty() { "mesh".into() } else { s }
}

/// `dir/stem.ext`, or `stem-2.ext`, ... whichever does not exist yet.
fn unique_path(dir: &Path, stem: &str, ext: &str) -> PathBuf {
    (1..10_000)
        .map(|k| dir.join(if k == 1 { format!("{stem}.{ext}") } else { format!("{stem}-{k}.{ext}") }))
        .find(|p| !p.exists())
        .unwrap_or_else(|| dir.join(format!("{stem}-new.{ext}")))
}

fn dir_of(p: &Path) -> PathBuf {
    p.parent().filter(|d| !d.as_os_str().is_empty()).map_or_else(|| PathBuf::from("."), Path::to_path_buf)
}

/// The mesh being edited drawn over the views: its edges (selected ones highlighted,
/// creased ones in their own color), and its vertices or face centres as dots, depth
/// tested against the surfaces.
pub fn overlays(o: &mut Overlays, proj: &Proj, e: &EditMesh, shading: Shading, scale: i32) {
    let style = |color: u32| LineStyle { color, alpha: 1.0, depth_test: true, write: false, bias: 4e-3 };
    let plain = if shading == Shading::Wireframe { 0xb4_b8_c0 } else { 0x14_14_18 };
    let m = &e.mesh;
    let w: Vec<Vec3> = m.positions.iter().map(|&p| e.xf.point(p)).collect();
    let mut selected = Vec::new();
    for (a, b) in m.edges() {
        let (pa, pb) = (w[a as usize], w[b as usize]);
        if e.sel.has_edge(a, b) {
            selected.push((pa, pb));
        } else {
            let c = if m.crease(a, b) > 0.0 { CREASE } else { plain };
            o.grid.push(Line3 { a: pa, b: pb, style: style(c), id: 0 });
        }
    }
    for (a, b) in selected {
        o.grid.push(Line3 { a, b, style: style(SELECTED), id: 0 });
    }
    let half = if m.vertex_count() > 60_000 { 0 } else { scale.max(1) };
    let dot = |o: &mut Overlays, p: Vec3, color: u32| {
        let ps = proj.pixel_size(p);
        let (r, u) = (proj.right * ps, proj.up * ps);
        for k in -half..=half {
            let y = u * k as f32;
            o.grid.push(Line3 {
                a: p - r * half as f32 + y,
                b: p + r * (half as f32 + 0.5) + y,
                style: style(color),
                id: 0,
            });
        }
    };
    match e.mode {
        Mode::Vertex => {
            for v in 0..m.vertex_count() as u32 {
                dot(o, w[v as usize], if e.sel.has_vert(v) { SELECTED } else { plain });
            }
        }
        Mode::Face => {
            for f in 0..m.face_count() as u32 {
                dot(o, e.xf.point(m.face_center(f)), if e.sel.has_face(f) { SELECTED } else { plain });
            }
        }
        Mode::Edge => {}
    }
}

/// Modeling commands (the Mesh menu and edit mode's keys).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum MeshCmd {
    Mode(Mode),
    SelectLoop,
    SelectRing,
    Grow,
    Shrink,
    Linked,
    Extrude,
    Inset,
    Bevel,
    BevelVertices,
    LoopCut,
    Knife,
    Bridge,
    Merge,
    Weld,
    Dissolve,
    Fill,
    Mirror(usize),
    Flip,
    Triangulate,
    Delete,
    Crease,
    Proportional,
}

/// What an operation that follows the mouse does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModalKind {
    Extrude,
    Inset,
    Bevel { verts: bool },
}

impl ModalKind {
    fn name(self) -> &'static str {
        match self {
            ModalKind::Extrude => "extrude",
            ModalKind::Inset => "inset",
            ModalKind::Bevel { .. } => "bevel",
        }
    }
}

/// An extrusion, inset or bevel following the mouse.
#[derive(Clone, Debug)]
pub struct Modal {
    pub kind: ModalKind,
    before: Snapshot,
    /// Extrude and inset: the new topology, its positions, and how vertices move.
    ext: Option<ops::Extrusion>,
    base: Vec<Vec3>,
    /// Bevel: what is bevelled.
    edges: Vec<(u32, u32)>,
    verts: Vec<u32>,
    /// Where it happens (world), and the way extrusions go.
    pivot: Vec3,
    dir: Vec3,
    /// Along a world axis instead (X, Y, Z typed).
    pub axis: Option<usize>,
    view: ViewRect,
    start: (f32, f32),
    pub typed: String,
    pub amount: f32,
}

/// The loop cut: the edge pointed at and how many cuts.
#[derive(Clone, Debug, PartialEq)]
pub struct LoopCut {
    pub cuts: u32,
    pub edge: Option<(u32, u32)>,
}

/// The knife: a line drawn in a view (view-local pixels).
#[derive(Clone, Debug, PartialEq)]
pub struct Knife {
    pub view: Option<ViewRect>,
    pub from: (f32, f32),
    pub to: (f32, f32),
}

/// An element under the cursor.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Elem {
    Vert(u32),
    Edge(u32, u32),
    Face(u32),
}

fn seg_dist(p: (f32, f32), a: (f32, f32), b: (f32, f32)) -> (f32, f32) {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let l2 = dx * dx + dy * dy;
    let t = if l2 > 0.0 { (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / l2).clamp(0.0, 1.0) } else { 0.0 };
    ((p.0 - a.0 - dx * t).hypot(p.1 - a.1 - dy * t), t)
}

impl Editor {
    pub fn editing(&self) -> bool {
        self.ws.edit.is_some()
    }

    /// Tab: into edit mode on the selected object, or out of it.
    pub fn toggle_edit(&mut self) {
        if self.editing() { self.leave_edit() } else { self.enter_edit() }
    }

    pub fn enter_edit(&mut self) {
        let objects: Vec<usize> = self.selected().iter().filter(|(k, _)| *k == Kind::Object).map(|&(_, i)| i).collect();
        let o = match (objects.as_slice(), self.inspect) {
            ([o], _) => *o,
            ([], Some(Target::Item(Kind::Object, i))) => i,
            _ => {
                self.notify("select one object to edit its mesh");
                return;
            }
        };
        let Some(item) = self.list(Kind::Object).get(o).cloned() else { return };
        let Some(t) = self.ws.doc.item_table(&item) else { return };
        let dir = dir_of(&self.ws.doc.files[item.file].path);
        let (mesh, source) = match object_mesh(&t, &dir) {
            Ok(m) => m,
            Err(e) => {
                self.notify(format!("{}: {e}", item.label));
                return;
            }
        };
        if mesh.face_count() == 0 {
            self.notify(format!("{} has no faces to edit", item.label));
            return;
        }
        let xf = object_xf(&t);
        let b = mesh.bounds().transformed(&xf);
        let radius = ((b.max - b.min).length() * 0.25).max(1e-3);
        self.ws.edit = Some(EditMesh {
            object: o,
            file: item.file,
            path: item.path.clone(),
            label: item.label.clone(),
            ty: xform::object_type(&t).to_owned(),
            source,
            sel: Selection::new(&mesh),
            mesh,
            mode: Mode::Vertex,
            xf,
            undo: Vec::new(),
            redo: Vec::new(),
            version: 0,
            proportional: false,
            radius,
        });
        self.ws.sel.clear();
        self.inspect = Some(Target::Item(Kind::Object, o));
        self.tool_drag = None;
        self.ws.show_edit();
        self.vp_dirty = true;
        self.notify(format!("editing {}: 1 2 3 vertices / edges / faces, Tab leaves", item.label));
    }

    /// Out of edit mode, writing the mesh if it changed.
    pub fn leave_edit(&mut self) {
        if matches!(self.tool_drag, Some(ToolDrag::Modal(_) | ToolDrag::LoopCut(_) | ToolDrag::Knife(_))) {
            self.tool_cancel();
        }
        let Some(e) = self.ws.edit.take() else { return };
        if e.changed() {
            match self.commit_edit(&e) {
                Ok(name) => self.notify(format!("{} is now {name}", e.label)),
                Err(err) => {
                    self.notify(format!("error: {err}"));
                    self.ws.edit = Some(e);
                    return;
                }
            }
            self.scene_stale = true;
        } else {
            self.ws.display = crate::display::Display::build(&self.ws.scene);
            self.notify("left edit mode");
        }
        self.tool_drag = None;
        self.pending_select = Some(vec![e.object as u32 + 1]);
        self.vp_dirty = true;
    }

    /// Writes the mesh next to the scene and points the object at it; returns the file's
    /// name.
    fn commit_edit(&mut self, e: &EditMesh) -> Result<String, String> {
        let obj = e.source.as_ref().is_some_and(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("obj")))
            || e.mesh.has_uvs()
            || e.mesh.materials.iter().any(|m| !m.is_empty());
        let path = unique_path(&dir_of(self.ws.doc.path()), &stem_of(&e.label), if obj { "obj" } else { "ply" });
        pt_geom::io::save(&e.mesh, &path)?;
        let rel = files::relative(&path, &dir_of(&self.ws.doc.files[e.file].path));
        let table = self.ws.doc.value(e.file, &e.path).and_then(|v| v.as_table().cloned()).unwrap_or_default();
        let key = |k: &str| [e.path.clone(), vec![Seg::from(k)]].concat();
        self.ws.doc.group(&format!("edit mesh {}", e.label), |doc| {
            for k in SHAPE_KEYS {
                if table.contains_key(*k) {
                    doc.remove(e.file, &key(k))?;
                }
            }
            if table.contains_key("type") && e.ty != "mesh" {
                doc.set(e.file, &key("type"), &Value::String("mesh".into()))?;
            }
            doc.set(e.file, &key("file"), &Value::String(rel.clone()))?;
            Ok(())
        })?;
        Ok(path.file_name().map_or_else(|| rel.clone(), |n| n.to_string_lossy().into_owned()))
    }

    /// The mesh changed: shown again.
    pub fn edit_changed(&mut self) {
        if let Some(e) = &mut self.ws.edit {
            e.version += 1;
        }
        self.ws.show_edit();
        self.vp_dirty = true;
    }

    /// Runs an operation on the mesh being edited (an undo step).
    fn edit_op(&mut self, label: &str, op: impl FnOnce(&EditMesh) -> Result<ops::Done, String>) {
        let Some(e) = &mut self.ws.edit else { return };
        match op(e) {
            Ok(done) => {
                e.take(done, label);
                let n = e.sel.vert_list().len();
                self.edit_changed();
                self.notify(format!("{label} ({n} vertices selected)"));
            }
            Err(err) => self.notify(err),
        }
    }

    /// Selects everything, or nothing when everything is selected.
    pub fn edit_select_all(&mut self) {
        let Some(e) = &mut self.ws.edit else { return };
        e.sel = if e.sel.verts.iter().all(|&s| s) { Selection::new(&e.mesh) } else { Selection::all(&e.mesh) };
        self.edit_changed();
    }

    pub fn edit_clear(&mut self) {
        let Some(e) = &mut self.ws.edit else { return };
        e.sel = Selection::new(&e.mesh);
        self.edit_changed();
    }

    /// Undo or redo inside edit mode.
    pub fn edit_undo(&mut self, undo: bool) {
        let Some(e) = &mut self.ws.edit else { return };
        let r = if undo { e.undo() } else { e.redo() };
        match r {
            Some(l) => {
                self.edit_changed();
                self.notify(format!("{} {l}", if undo { "undid" } else { "redid" }));
            }
            None => self.notify(if undo { "nothing to undo in edit mode" } else { "nothing to redo" }),
        }
    }

    /// Frames the selected vertices (or the whole mesh).
    pub fn edit_frame(&mut self, v: usize) {
        let Some(e) = &self.ws.edit else { return };
        let b = if e.sel.is_empty() { e.bounds(false) } else { e.bounds(true) };
        let (c, r) = crate::view::sphere(&b);
        let r = if b.max == b.min { crate::view::sphere(&e.bounds(false)).1 * 0.3 } else { r };
        self.ws.views[v].frame(c, r);
        self.vp_dirty = true;
    }

    pub fn mesh_cmd(&mut self, c: MeshCmd) {
        let Some(e) = &mut self.ws.edit else {
            self.notify("Tab edits the selected object's mesh first");
            return;
        };
        let mode = e.mode;
        match c {
            MeshCmd::Mode(m) => {
                e.mode = m;
                e.sel.flush(&e.mesh, m);
                self.edit_changed();
                self.notify(format!("{} select mode", m.name()));
            }
            MeshCmd::SelectLoop | MeshCmd::SelectRing => {
                let ring = c == MeshCmd::SelectRing;
                let mut add = e.sel.clone();
                for (a, b) in e.sel.edge_list() {
                    if ring {
                        let (edges, faces) = select::edge_ring(&e.mesh, a, b);
                        add.edges.extend(edges);
                        for f in faces {
                            add.faces[f as usize] |= mode == Mode::Face;
                        }
                    } else {
                        add.edges.extend(select::edge_loop(&e.mesh, a, b));
                    }
                }
                add.flush(&e.mesh, if mode == Mode::Face && ring { Mode::Face } else { Mode::Edge });
                add.flush(&e.mesh, mode);
                e.sel = add;
                self.edit_changed();
            }
            MeshCmd::Grow => {
                select::grow(&e.mesh, &mut e.sel, mode);
                self.edit_changed();
            }
            MeshCmd::Shrink => {
                select::shrink(&e.mesh, &mut e.sel, mode);
                self.edit_changed();
            }
            MeshCmd::Linked => {
                let all = select::linked(&e.mesh, &e.sel.vert_list());
                let mut s = Selection::of_verts(&e.mesh, all);
                s.flush(&e.mesh, mode);
                e.sel = s;
                self.edit_changed();
            }
            MeshCmd::Extrude => self.start_modal(ModalKind::Extrude),
            MeshCmd::Inset => self.start_modal(ModalKind::Inset),
            MeshCmd::Bevel => self.start_modal(ModalKind::Bevel { verts: false }),
            MeshCmd::BevelVertices => self.start_modal(ModalKind::Bevel { verts: true }),
            MeshCmd::LoopCut => {
                self.tool_drag = Some(ToolDrag::LoopCut(LoopCut { cuts: 1, edge: None }));
                self.notify("loop cut: point at an edge and click · wheel or + -: number of cuts · Esc");
            }
            MeshCmd::Knife => {
                self.tool_drag = Some(ToolDrag::Knife(Knife { view: None, from: (0.0, 0.0), to: (0.0, 0.0) }));
                self.notify("knife: drag a line across the faces to cut (the selected ones, or all) · Esc");
            }
            MeshCmd::Bridge => self.edit_op("bridge", |e| {
                if e.mode == Mode::Face {
                    ops::bridge(&e.mesh, &[], &e.sel.face_list())
                } else {
                    ops::bridge(&e.mesh, &e.sel.edge_list(), &[])
                }
            }),
            MeshCmd::Merge => self.edit_op("merge", |e| {
                let v = e.sel.vert_list();
                let at =
                    v.iter().fold(Vec3::ZERO, |a, &v| a + e.mesh.positions[v as usize]) * (1.0 / v.len().max(1) as f32);
                ops::merge(&e.mesh, &v, at)
            }),
            MeshCmd::Weld => {
                let b = e.mesh.bounds();
                let dist = (b.max - b.min).length() * 1e-4;
                let only = (!e.sel.is_empty()).then(|| e.sel.verts.clone());
                let (done, n) = ops::weld(&e.mesh, only.as_deref(), dist);
                if n == 0 {
                    self.notify("no vertices close enough to weld");
                    return;
                }
                e.take(done, "weld");
                self.edit_changed();
                self.notify(format!("welded {n} vertices"));
            }
            MeshCmd::Dissolve => self.edit_op("dissolve", |e| {
                Ok(match e.mode {
                    Mode::Vertex => ops::dissolve_verts(&e.mesh, &e.sel.vert_list()),
                    Mode::Edge => ops::dissolve_edges(&e.mesh, &e.sel.edge_list()),
                    Mode::Face => ops::dissolve_faces(&e.mesh, &e.sel.face_list()),
                })
            }),
            MeshCmd::Fill => self.edit_op("fill", |e| ops::fill(&e.mesh, &e.sel)),
            MeshCmd::Mirror(axis) => self.edit_op("mirror", |e| {
                let faces = e.sel.face_list();
                let b = e.mesh.bounds();
                let eps = (b.max - b.min).length() * 1e-4;
                Ok(ops::mirror(&e.mesh, axis, (!faces.is_empty()).then_some(faces.as_slice()), eps))
            }),
            MeshCmd::Flip => self.edit_op("flip normals", |e| Ok(ops::flip(&e.mesh, &needs_faces(e)?))),
            MeshCmd::Triangulate => self.edit_op("triangulate", |e| Ok(ops::triangulate(&e.mesh, &needs_faces(e)?))),
            MeshCmd::Delete => self.edit_op("delete", |e| {
                if e.sel.is_empty() {
                    return Err("nothing selected to delete".into());
                }
                Ok(ops::delete(&e.mesh, &e.sel, e.mode))
            }),
            MeshCmd::Crease => {
                let edges = e.sel.edge_list();
                if edges.is_empty() {
                    self.notify("select the edges to crease");
                    return;
                }
                let before = e.snapshot("crease");
                let on = !edges.iter().all(|&(a, b)| e.mesh.crease(a, b) > 0.0);
                for (a, b) in edges {
                    e.mesh.set_crease(a, b, if on { 1.0 } else { 0.0 });
                }
                e.record(before);
                self.edit_changed();
                self.notify(if on { "creased" } else { "crease removed" });
            }
            MeshCmd::Proportional => {
                e.proportional = !e.proportional;
                let (on, r) = (e.proportional, e.radius);
                self.notify(if on {
                    format!("proportional editing, radius {} (wheel while dragging)", xform::tidy(r))
                } else {
                    "proportional editing off".into()
                });
            }
        }
    }

    // ------------------------------------------------------------------ picking

    /// Whether world point `p` is drawn unhidden in view `r` (by the depth drawn last).
    fn shown(&self, r: &ViewRect, proj: &Proj, p: Vec3) -> bool {
        let Some((x, y, z)) = proj.to_screen(p) else { return false };
        if x < 0.0 || y < 0.0 || x >= r.w as f32 || y >= r.h as f32 {
            return false;
        }
        self.ws.depth_at(r.view, r.w, r.h, x, y).is_none_or(|d| z >= d * 0.99 - 1e-9)
    }

    /// The element of kind `mode` nearest to view-local (`x`, `y`) that is in sight.
    fn edit_pick(&self, r: &ViewRect, x: f32, y: f32, mode: Mode) -> Option<Elem> {
        let e = self.ws.edit.as_ref()?;
        let proj = self.ws.proj(r);
        let reach = 12.0 * self.ui.scale.max(0.5);
        let m = &e.mesh;
        match mode {
            Mode::Vertex => {
                let mut c: Vec<(f32, u32)> = (0..m.vertex_count() as u32)
                    .filter_map(|v| {
                        let (sx, sy, _) = proj.to_screen(e.world(v))?;
                        let d = (sx - x).hypot(sy - y);
                        (d <= reach).then_some((d, v))
                    })
                    .collect();
                c.sort_by(|a, b| a.0.total_cmp(&b.0));
                c.into_iter().find(|&(_, v)| self.shown(r, &proj, e.world(v))).map(|(_, v)| Elem::Vert(v))
            }
            Mode::Edge => {
                let mut c: Vec<(f32, f32, u32, u32)> = m
                    .edges()
                    .into_iter()
                    .filter_map(|(a, b)| {
                        let (ax, ay, _) = proj.to_screen(e.world(a))?;
                        let (bx, by, _) = proj.to_screen(e.world(b))?;
                        let (d, t) = seg_dist((x, y), (ax, ay), (bx, by));
                        (d <= reach).then_some((d, t, a, b))
                    })
                    .collect();
                c.sort_by(|a, b| a.0.total_cmp(&b.0));
                c.into_iter()
                    .find(|&(_, t, a, b)| self.shown(r, &proj, e.world(a).lerp(e.world(b), t)))
                    .map(|(_, _, a, b)| Elem::Edge(a, b))
            }
            Mode::Face => {
                let ray = proj.ray(x, y);
                let mut best: Option<(f32, u32)> = None;
                for (hs, f) in m.triangles() {
                    let [a, b, c] = hs.map(|h| e.world(m.origin(h)));
                    if let Some(t) = ray_triangle(ray.o, ray.d, a, b, c)
                        && best.is_none_or(|b| t < b.0)
                    {
                        best = Some((t, f));
                    }
                }
                best.map(|(_, f)| Elem::Face(f))
            }
        }
    }

    /// A click (or double-click) or a rectangle in edit mode; window pixels.
    #[expect(clippy::too_many_arguments, reason = "a click: where, how, which keys")]
    pub fn edit_select(&mut self, r: &ViewRect, x0: i32, y0: i32, x1: i32, y1: i32, mods: Modifiers, clicks: u8) {
        let Some(mode) = self.ws.edit.as_ref().map(|e| e.mode) else { return };
        let (ox, oy) = (self.vp.x + r.x as i32, self.vp.y + r.y as i32);
        let (lx, ly) = ((x1 - ox) as f32 + 0.5, (y1 - oy) as f32 + 0.5);
        let mut picked: Vec<Elem> = Vec::new();
        let mut extra: Option<Selection> = None;
        if (x1 - x0).abs() + (y1 - y0).abs() < 4 {
            if clicks >= 2 {
                // Loops (Ctrl: rings) through the edge under the cursor.
                if let Some(Elem::Edge(a, b)) = self.edit_pick(r, lx, ly, Mode::Edge)
                    && let Some(e) = &self.ws.edit
                {
                    let ring = mods.ctrl || mode == Mode::Face;
                    let (edges, faces) = select::edge_ring(&e.mesh, a, b);
                    extra = Some(match (ring, mode) {
                        (true, Mode::Face) => Selection::of_faces(&e.mesh, faces),
                        (true, _) => Selection::of_edges(&e.mesh, edges),
                        (false, _) => Selection::of_edges(&e.mesh, select::edge_loop(&e.mesh, a, b)),
                    });
                }
            } else {
                picked.extend(self.edit_pick(r, lx, ly, mode));
            }
        } else {
            picked = self.edit_rect(r, (x0 - ox, y0 - oy), (x1 - ox, y1 - oy), mode);
        }
        let Some(e) = &mut self.ws.edit else { return };
        let keep = mods.shift || mods.ctrl || clicks >= 2;
        if !keep {
            e.sel = Selection::new(&e.mesh);
        }
        if let Some(s) = extra {
            for (v, on) in s.verts.iter().enumerate() {
                e.sel.verts[v] |= on;
            }
            e.sel.edges.extend(s.edges.iter().copied());
            for (f, on) in s.faces.iter().enumerate() {
                e.sel.faces[f] |= on;
            }
            e.sel.flush(&e.mesh, if mode == Mode::Face { Mode::Face } else { Mode::Edge });
        }
        let toggle = mods.ctrl && clicks < 2;
        for p in &picked {
            match *p {
                Elem::Vert(v) => e.sel.verts[v as usize] = !(toggle && e.sel.has_vert(v)),
                Elem::Edge(a, b) => {
                    if toggle && e.sel.has_edge(a, b) {
                        e.sel.edges.remove(&edge_key(a, b));
                    } else {
                        e.sel.edges.insert(edge_key(a, b));
                    }
                }
                Elem::Face(f) => e.sel.faces[f as usize] = !(toggle && e.sel.has_face(f)),
            }
        }
        e.sel.flush(&e.mesh, mode);
        let n = match mode {
            Mode::Vertex => e.sel.vert_list().len(),
            Mode::Edge => e.sel.edges.len(),
            Mode::Face => e.sel.face_list().len(),
        };
        self.edit_changed();
        self.notify(if n == 0 { "nothing selected".to_owned() } else { format!("{n} {}s selected", mode.name()) });
    }

    /// The elements inside a view-local rectangle that are in sight.
    fn edit_rect(&self, r: &ViewRect, a: (i32, i32), b: (i32, i32), mode: Mode) -> Vec<Elem> {
        let Some(e) = &self.ws.edit else { return Vec::new() };
        let proj = self.ws.proj(r);
        let (xa, xb) = (a.0.min(b.0) as f32, a.0.max(b.0) as f32 + 1.0);
        let (ya, yb) = (a.1.min(b.1) as f32, a.1.max(b.1) as f32 + 1.0);
        let inside = |p: Vec3| {
            proj.to_screen(p).is_some_and(|(x, y, _)| (xa..xb).contains(&x) && (ya..yb).contains(&y))
                && self.shown(r, &proj, p)
        };
        let m = &e.mesh;
        match mode {
            Mode::Vertex => (0..m.vertex_count() as u32).filter(|&v| inside(e.world(v))).map(Elem::Vert).collect(),
            Mode::Edge => m
                .edges()
                .into_iter()
                .filter(|&(a, b)| inside(e.world(a)) && inside(e.world(b)))
                .map(|(a, b)| Elem::Edge(a, b))
                .collect(),
            Mode::Face => {
                (0..m.face_count() as u32).filter(|&f| inside(e.xf.point(m.face_center(f)))).map(Elem::Face).collect()
            }
        }
    }

    // ------------------------------------------------------------------ operations following the mouse

    /// The view under the mouse (or the active one), and the mouse in it.
    fn mouse_view(&self) -> (ViewRect, (f32, f32)) {
        let (vw, vh) = (self.vp.w.max(1) as usize, self.vp.h.max(1) as usize);
        let (mx, my) = self.ui.input.mouse;
        let r = self.ws.rect_at(vw, vh, mx - self.vp.x, my - self.vp.y).unwrap_or_else(|| {
            self.ws.rects(vw, vh).into_iter().find(|r| r.view == self.ws.active).unwrap_or(ViewRect {
                view: self.ws.active,
                x: 0,
                y: 0,
                w: vw,
                h: vh,
            })
        });
        let local = ((mx - self.vp.x - r.x as i32) as f32 + 0.5, (my - self.vp.y - r.y as i32) as f32 + 0.5);
        (r, local)
    }

    fn start_modal(&mut self, kind: ModalKind) {
        let (view, start) = self.mouse_view();
        let Some(e) = &mut self.ws.edit else { return };
        if e.sel.is_empty() {
            self.notify(format!("select what to {}", kind.name()));
            return;
        }
        let before = e.snapshot(kind.name());
        let (mut ext, mut edges, mut verts) = (None, Vec::new(), Vec::new());
        match kind {
            ModalKind::Extrude => match ops::extrude(&e.mesh, &e.sel) {
                Ok(x) => ext = Some(x),
                Err(err) => {
                    self.notify(err);
                    return;
                }
            },
            ModalKind::Inset => {
                let faces = e.sel.face_list();
                if faces.is_empty() {
                    self.notify("select the faces to inset");
                    return;
                }
                ext = Some(ops::extrude_faces(&e.mesh, &faces));
            }
            ModalKind::Bevel { verts: v } => {
                if v {
                    verts = e.sel.vert_list();
                } else {
                    edges = e.sel.edge_list();
                }
                if let Err(err) = ops::bevel(&e.mesh, &edges, &verts, 0.0) {
                    self.notify(err);
                    return;
                }
            }
        }
        let pivot = e.pivot().unwrap_or(Vec3::ZERO);
        let mut dir = Vec3::ZERO;
        if let Some(x) = &ext {
            e.mesh = x.done.mesh.clone();
            e.sel = x.done.sel.clone();
            e.sel.flush(&e.mesh, e.mode);
            dir = x.normals.iter().fold(Vec3::ZERO, |a, &n| a + e.xf.normal(n));
        }
        let dir = if dir.length_squared() > 1e-12 { dir.normalize() } else { Vec3::new(0.0, 1.0, 0.0) };
        let base = e.mesh.positions.clone();
        self.tool_drag = Some(ToolDrag::Modal(Box::new(Modal {
            kind,
            before,
            ext,
            base,
            edges,
            verts,
            pivot,
            dir,
            axis: None,
            view,
            start,
            typed: String::new(),
            amount: 0.0,
        })));
        self.edit_changed();
        self.modal_status();
    }

    fn modal_status(&mut self) {
        let Some(ToolDrag::Modal(d)) = &self.tool_drag else { return };
        let along = match (d.kind, d.axis) {
            (ModalKind::Extrude, Some(k)) => format!(" along {}", ["X", "Y", "Z"][k]),
            _ => String::new(),
        };
        let typed = if d.typed.is_empty() { String::new() } else { format!("  [{}]", d.typed) };
        self.status = format!(
            "{} {}{along}{typed}   move the mouse · type a value · {}click or Enter: done · Esc: cancel",
            d.kind.name(),
            xform::tidy(d.amount),
            if d.kind == ModalKind::Extrude { "X Y Z: axis · " } else { "" }
        );
    }

    /// The mouse moved to view-local (`x`, `y`) of the modal operation's view.
    fn modal_update(&mut self, d: &mut Modal, x: f32, y: f32, ctrl: bool) {
        let proj = self.ws.proj(&d.view);
        let typed = crate::ui::widgets::eval(&d.typed).map(|v| v as f32);
        let amount = if let Some(v) = typed {
            v
        } else if d.kind == ModalKind::Extrude {
            let dir = d.axis.map_or(d.dir, |k| {
                let mut a = Vec3::ZERO;
                a[k] = 1.0;
                a
            });
            let (r, r0) = (proj.ray(x, y), proj.ray(d.start.0, d.start.1));
            match (crate::gizmo::line_param(d.pivot, dir, r.o, r.d), crate::gizmo::line_param(d.pivot, dir, r0.o, r0.d))
            {
                (Some(t), Some(t0)) => t - t0,
                _ => 0.0,
            }
        } else {
            let c = proj.to_screen(d.pivot).map_or((0.0, 0.0), |(cx, cy, _)| (cx, cy));
            let from = (d.start.0 - c.0).hypot(d.start.1 - c.1);
            let to = (x - c.0).hypot(y - c.1);
            ((to - from) * proj.pixel_size(d.pivot)).max(0.0)
        };
        let amount = if ctrl && typed.is_none() {
            let step = crate::tools::grid_step(&proj, d.pivot) * 0.1;
            (amount / step).round() * step
        } else {
            amount
        };
        d.amount = amount;
        let Some(e) = &mut self.ws.edit else { return };
        match d.kind {
            ModalKind::Extrude => {
                let Some(x) = &d.ext else { return };
                let dir = d.axis.map_or(d.dir, |k| {
                    let mut a = Vec3::ZERO;
                    a[k] = 1.0;
                    a
                });
                let mut p = d.base.clone();
                for &v in &x.moved {
                    let w = e.xf.point(d.base[v as usize]) + dir * amount;
                    p[v as usize] = e.xf.inv_point(w);
                }
                e.mesh.positions = p;
                self.ws.move_edit();
            }
            ModalKind::Inset => {
                let Some(x) = &d.ext else { return };
                e.mesh.positions = x.place(&d.base, &x.inset, amount * e.local_scale());
                self.ws.move_edit();
            }
            ModalKind::Bevel { .. } => {
                if let Ok(done) = ops::bevel(&d.before.mesh, &d.edges, &d.verts, amount * e.local_scale()) {
                    e.mesh = done.mesh;
                    e.sel = done.sel;
                    e.sel.flush(&e.mesh, e.mode);
                    e.version += 1;
                    self.ws.show_edit();
                }
            }
        }
        self.vp_dirty = true;
    }

    /// The modal operation is done (click, Enter).
    fn modal_finish(&mut self, d: Modal) {
        let Some(e) = &mut self.ws.edit else { return };
        if matches!(d.kind, ModalKind::Bevel { .. }) && d.amount <= 0.0 {
            e.restore(d.before);
            self.edit_changed();
            self.notify("nothing bevelled");
            return;
        }
        e.record(d.before);
        self.edit_changed();
        self.notify(format!("{} {}", d.kind.name(), xform::tidy(d.amount)));
    }

    /// Esc during a modal operation, the loop cut or the knife.
    pub fn modal_cancel(&mut self) {
        match self.tool_drag.take() {
            Some(ToolDrag::Modal(d)) => {
                if let Some(e) = &mut self.ws.edit {
                    e.restore(d.before);
                }
                self.edit_changed();
                self.notify("cancelled");
            }
            Some(ToolDrag::LoopCut(_) | ToolDrag::Knife(_)) => {
                self.notify("cancelled");
                self.vp_dirty = true;
            }
            other => self.tool_drag = other,
        }
    }

    /// Keys during a modal operation, the loop cut or the knife: numbers, X / Y / Z,
    /// + and -, Enter, Esc. None when none of those is going on.
    pub fn modal_keys(&mut self) -> Option<bool> {
        if !matches!(self.tool_drag, Some(ToolDrag::Modal(_) | ToolDrag::LoopCut(_) | ToolDrag::Knife(_))) {
            return None;
        }
        let keys = std::mem::take(&mut self.ui.input.keys);
        let text = std::mem::take(&mut self.ui.input.text);
        let mut retype = false;
        for (key, mods) in keys {
            match (key, &mut self.tool_drag) {
                (Key::Escape, _) => {
                    self.modal_cancel();
                    return Some(true);
                }
                (Key::Enter, Some(ToolDrag::Modal(_))) => {
                    if let Some(ToolDrag::Modal(d)) = self.tool_drag.take() {
                        self.modal_finish(*d);
                    }
                    return Some(true);
                }
                (Key::Backspace, Some(ToolDrag::Modal(d))) => {
                    d.typed.pop();
                    retype = true;
                }
                (Key::Char(c @ ('x' | 'y' | 'z')), Some(ToolDrag::Modal(d))) if !mods.ctrl => {
                    if d.kind == ModalKind::Extrude {
                        let k = (c as u8 - b'x') as usize;
                        d.axis = if d.axis == Some(k) { None } else { Some(k) };
                        retype = true;
                    }
                }
                (Key::Plus | Key::Minus, Some(ToolDrag::LoopCut(l))) => {
                    let up = key == Key::Plus;
                    l.cuts = if up { (l.cuts + 1).min(64) } else { l.cuts.saturating_sub(1).max(1) };
                    self.notify_cuts();
                }
                _ => {}
            }
        }
        for c in text.chars() {
            if (c.is_ascii_digit() || matches!(c, '.' | '-' | '+' | '*' | '/' | '(' | ')'))
                && let Some(ToolDrag::Modal(d)) = &mut self.tool_drag
            {
                d.typed.push(c);
                retype = true;
            }
        }
        if retype {
            let (x, y) = self.ui.input.mouse;
            let ctrl = self.ui.input.mods.ctrl;
            self.modal_mouse(x, y, ctrl);
        }
        Some(true)
    }

    fn notify_cuts(&mut self) {
        if let Some(ToolDrag::LoopCut(l)) = &self.tool_drag {
            let n = l.cuts;
            self.notify(format!("loop cut: {n} cut{}", if n == 1 { "" } else { "s" }));
            self.vp_dirty = true;
        }
    }

    /// The mouse moved to window pixel (`x`, `y`) with no button down: a modal operation
    /// follows it, the loop cut looks for the edge under it. Returns whether one did.
    pub fn modal_mouse(&mut self, x: i32, y: i32, ctrl: bool) -> bool {
        match self.tool_drag.take() {
            Some(ToolDrag::Modal(mut d)) => {
                let r = d.view;
                let (lx, ly) = ((x - self.vp.x - r.x as i32) as f32 + 0.5, (y - self.vp.y - r.y as i32) as f32 + 0.5);
                self.modal_update(&mut d, lx, ly, ctrl);
                self.tool_drag = Some(ToolDrag::Modal(d));
                self.modal_status();
                true
            }
            Some(ToolDrag::LoopCut(mut l)) => {
                let (r, (lx, ly)) = self.mouse_view_at(x, y);
                let edge = self.edit_pick(&r, lx, ly, Mode::Edge).and_then(|p| match p {
                    Elem::Edge(a, b) => Some((a, b)),
                    _ => None,
                });
                if edge != l.edge {
                    l.edge = edge;
                    self.vp_dirty = true;
                }
                self.tool_drag = Some(ToolDrag::LoopCut(l));
                true
            }
            other => {
                self.tool_drag = other;
                false
            }
        }
    }

    fn mouse_view_at(&self, x: i32, y: i32) -> (ViewRect, (f32, f32)) {
        let (vw, vh) = (self.vp.w.max(1) as usize, self.vp.h.max(1) as usize);
        let r = self.ws.rect_at(vw, vh, x - self.vp.x, y - self.vp.y).unwrap_or(ViewRect {
            view: self.ws.active,
            x: 0,
            y: 0,
            w: vw,
            h: vh,
        });
        (r, ((x - self.vp.x - r.x as i32) as f32 + 0.5, (y - self.vp.y - r.y as i32) as f32 + 0.5))
    }

    /// A mouse button pressed in a view while a modal operation or the loop cut runs:
    /// left finishes or cuts, right cancels. Returns whether it was taken.
    pub fn modal_press(&mut self, button: MouseButton, x: i32, y: i32) -> bool {
        match (&self.tool_drag, button) {
            (Some(ToolDrag::Modal(_) | ToolDrag::LoopCut(_)), MouseButton::Right) => {
                self.modal_cancel();
                true
            }
            (Some(ToolDrag::Modal(_)), MouseButton::Left) => {
                self.modal_mouse(x, y, false);
                if let Some(ToolDrag::Modal(d)) = self.tool_drag.take() {
                    self.modal_finish(*d);
                }
                true
            }
            (Some(ToolDrag::LoopCut(_)), MouseButton::Left) => {
                self.modal_mouse(x, y, false);
                let Some(ToolDrag::LoopCut(l)) = self.tool_drag.take() else { return true };
                if let Some((a, b)) = l.edge {
                    self.edit_op("loop cut", |e| ops::loop_cut(&e.mesh, a, b, l.cuts));
                } else {
                    self.notify("point at an edge to cut across it");
                    self.tool_drag = Some(ToolDrag::LoopCut(l));
                }
                true
            }
            _ => false,
        }
    }

    /// The wheel during edit mode: the number of loop cuts, or the radius of proportional
    /// editing while dragging. Returns whether it was taken.
    pub fn edit_wheel(&mut self, notches: f32) -> bool {
        if let Some(ToolDrag::LoopCut(l)) = &mut self.tool_drag {
            l.cuts = if notches > 0.0 { (l.cuts + 1).min(64) } else { l.cuts.saturating_sub(1).max(1) };
            self.notify_cuts();
            return true;
        }
        let dragging = matches!(&self.tool_drag, Some(ToolDrag::Gizmo(_)));
        let Some(e) = &mut self.ws.edit else { return false };
        if !(dragging && e.proportional) {
            return false;
        }
        e.radius = (e.radius * 1.15f32.powf(notches)).max(1e-6);
        let r = e.radius;
        self.proportional_changed();
        self.notify(format!("proportional radius {}", xform::tidy(r)));
        true
    }

    // ------------------------------------------------------------------ the knife

    /// A press in a view with the knife ready: the line starts. Returns whether it did.
    pub fn knife_press(&mut self, r: &ViewRect, x: f32, y: f32) -> bool {
        let Some(ToolDrag::Knife(k)) = &mut self.tool_drag else { return false };
        k.view = Some(*r);
        k.from = (x, y);
        k.to = (x, y);
        true
    }

    pub fn knife_move(&mut self, x: f32, y: f32) {
        if let Some(ToolDrag::Knife(k)) = &mut self.tool_drag {
            k.to = (x, y);
            self.vp_dirty = true;
        }
    }

    /// The knife's line is drawn: the faces under it are cut.
    pub fn knife_cut(&mut self, k: &Knife) {
        let Some(r) = k.view else { return };
        let (a, b) = (k.from, k.to);
        let len2 = (b.0 - a.0).powi(2) + (b.1 - a.1).powi(2);
        if len2 < 16.0 {
            self.notify("drag a line across the faces to cut them");
            return;
        }
        let proj = self.ws.proj(&r);
        let (ra, rb) = (proj.ray(a.0, a.1), proj.ray(b.0, b.1));
        let (n, p0) = if proj.ortho { (proj.fwd.cross(rb.o - ra.o), ra.o) } else { (ra.d.cross(rb.d), proj.eye) };
        if n.length_squared() == 0.0 {
            return;
        }
        let n = n.normalize();
        self.edit_op("knife", |e| {
            let xf = e.xf;
            let dist = move |p: Vec3| n.dot(xf.point(p) - p0);
            let keep = |p: Vec3| {
                proj.to_screen(xf.point(p)).is_some_and(|(x, y, _)| {
                    let t = ((x - a.0) * (b.0 - a.0) + (y - a.1) * (b.1 - a.1)) / len2;
                    (-0.01..=1.01).contains(&t)
                })
            };
            let faces = e.sel.face_list();
            let filter = (!faces.is_empty()).then(|| e.sel.faces.clone());
            let done = ops::cut(&e.mesh, filter.as_deref(), &dist, &keep);
            if done.mesh.vertex_count() == e.mesh.vertex_count() && done.mesh.face_count() == e.mesh.face_count() {
                return Err("the knife crossed no faces".into());
            }
            Ok(done)
        });
    }

    /// The loop cut's preview: where the cuts would go, as lines.
    pub fn loop_cut_lines(&self) -> Vec<(Vec3, Vec3, u32)> {
        let (Some(ToolDrag::LoopCut(l)), Some(e)) = (&self.tool_drag, &self.ws.edit) else { return Vec::new() };
        let Some((a, b)) = l.edge else { return Vec::new() };
        let m = &e.mesh;
        let (_, faces) = select::edge_ring(m, a, b);
        let (ring, _) = select::edge_ring(m, a, b);
        let mut out = Vec::new();
        for f in faces {
            let hs: Vec<u32> = m.face_half_edges(f).collect();
            let Some(i) = (0..4).find(|&i| ring.contains(&edge_key(m.origin(hs[i]), m.dest(hs[i])))) else { continue };
            let (h, opp) = (hs[i], hs[(i + 2) % 4]);
            let p = |v: u32| e.world(v);
            for k in 1..=l.cuts {
                let t = k as f32 / (l.cuts + 1) as f32;
                let s = p(m.origin(h)).lerp(p(m.dest(h)), t);
                let q = p(m.dest(opp)).lerp(p(m.origin(opp)), t);
                out.push((s, q, 0xff_e0_40));
            }
        }
        out
    }
}

/// Faces of the selection, or an error saying to select some.
fn needs_faces(e: &EditMesh) -> Result<Vec<u32>, String> {
    let f = e.sel.face_list();
    if f.is_empty() { Err("select faces first".into()) } else { Ok(f) }
}

/// Where ray (`o`, `d`) meets triangle `a`, `b`, `c` (Möller–Trumbore), either side.
fn ray_triangle(o: Vec3, d: Vec3, a: Vec3, b: Vec3, c: Vec3) -> Option<f32> {
    let (e1, e2) = (b - a, c - a);
    let p = d.cross(e2);
    let det = e1.dot(p);
    if det.abs() < 1e-12 {
        return None;
    }
    let inv = 1.0 / det;
    let s = o - a;
    let u = s.dot(p) * inv;
    if !(0.0..=1.0).contains(&u) {
        return None;
    }
    let q = s.cross(e1);
    let v = d.dot(q) * inv;
    if v < 0.0 || u + v > 1.0 {
        return None;
    }
    let t = e2.dot(q) * inv;
    (t > 0.0).then_some(t)
}
