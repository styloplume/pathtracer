//! Editing in the viewport: the transform gizmos and their drags (with snapping to the
//! grid, angle steps, vertices and surfaces, numbers typed while dragging, and X / Y / Z
//! to switch the axis), handles (spot cones, the sun on the sky dome, scatter boxes, the
//! focus plane), click-to-focus, the render region, and the scene camera following the
//! view when it is locked to it.
//!
//! A drag rewrites the items' keys from the tables they had when it started (one undo
//! step), and moves their geometry in place (`Scene::transform_object`) instead of loading
//! the scene again; lights are replaced in place.

use crate::editor::{Editor, Section, Target};
use crate::gizmo::{self, Decor, Gizmo, Handle, HandleKind, Part, Tool};
use crate::modeling::{Knife, LoopCut, Modal, Snapshot};
use crate::overlay::{self, LIGHT_ID};
use crate::view::{Proj, sphere};
use crate::workspace::Rect as ViewRect;
use crate::xform::{self, Gesture, Op, Placement};
use pt_core::document::Kind;
use pt_core::geometry::Aabb;
use pt_core::math::{Frame, PI, Transform, Vec3};
use pt_core::platform::{Key, Modifiers};
use pt_core::toml::{Seg, Table, Value};

/// What snaps while moving.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Snap {
    #[default]
    Off,
    /// Positions to the grid's lines, rotations to 15°, scales to 0.1.
    Grid,
    /// To the nearest vertex of other objects under the cursor.
    Vertex,
    /// To the surface under the cursor.
    Surface,
}

impl Snap {
    pub const ALL: [Snap; 4] = [Snap::Off, Snap::Grid, Snap::Vertex, Snap::Surface];

    pub fn name(self) -> &'static str {
        match self {
            Snap::Off => "off",
            Snap::Grid => "grid",
            Snap::Vertex => "vertex",
            Snap::Surface => "surface",
        }
    }
}

const ANGLE_STEP: f32 = 15.0;
const SCALE_STEP: f32 = 0.1;
const DOME: u32 = 0x4a_64_90;
const SCATTER: u32 = 0x7a_c0_8a;
const FOCUS: u32 = 0xe0_90_d0;

/// One item a gizmo drag changes.
#[derive(Clone, Debug)]
struct DragItem {
    kind: Kind,
    index: usize,
    file: usize,
    path: Vec<Seg>,
    /// The item's table when the drag started.
    table: Table,
    axes: [Vec3; 3],
    /// What has been applied to its geometry so far.
    applied: Transform,
    /// An instance with `scatter`: its copies are placed by the loader in ways a refit does
    /// not reproduce exactly, so the scene is loaded again when the drag ends.
    scattered: bool,
}

#[derive(Clone, Debug)]
pub struct GizmoDrag {
    part: Part,
    tool: Tool,
    view: ViewRect,
    /// Where the drag started, view-local.
    start: (f32, f32),
    pivot: Vec3,
    axes: [Vec3; 3],
    items: Vec<DragItem>,
    /// Plane moves: where the ray met the plane at the start.
    start_hit: Option<Vec3>,
    /// Rotations: the angle so far (unwrapped) and the last screen angle.
    angle: f32,
    last_angle: Option<f32>,
    /// A number typed while dragging.
    pub typed: String,
    gesture: Gesture,
    wrote: bool,
    merge: String,
    /// Moving in place failed: the scene is loaded again after each change.
    reload: bool,
    /// In edit mode: the vertices moved instead of the selected items.
    edit: Option<EditDrag>,
}

/// Vertices of the mesh being edited moved by a gizmo.
#[derive(Clone, Debug)]
pub struct EditDrag {
    before: Snapshot,
    /// Positions (object space) when the drag started.
    base: Vec<Vec3>,
    /// The vertices that move and how much (proportional editing).
    weights: Vec<(u32, f32)>,
    moved: bool,
}

#[derive(Clone, Debug)]
pub struct HandleDrag {
    kind: HandleKind,
    view: ViewRect,
    /// The table the handle edits (when it started) and where it is.
    file: usize,
    path: Vec<Seg>,
    table: Table,
    merge: String,
    wrote: bool,
}

/// A drag that edits rather than navigates or selects.
#[derive(Clone, Debug)]
pub enum ToolDrag {
    Gizmo(Box<GizmoDrag>),
    Handle(Box<HandleDrag>),
    /// A render region from this corner (view-local pixels).
    Region(ViewRect, (i32, i32)),
    /// Edit mode: an extrusion, inset or bevel following the mouse.
    Modal(Box<Modal>),
    /// Edit mode: the loop cut, pointing at an edge.
    LoopCut(LoopCut),
    /// Edit mode: the knife, ready or drawing its line.
    Knife(Knife),
}

/// What the drawn decor depends on: document version, scene version, selection, tool,
/// axes, inspected item, looking through the camera, the mesh being edited.
pub type DecorKey = (u64, u64, overlay::Selection, Tool, bool, Option<Target>, bool, u64);

/// Spacing of the grid lines a view shows around `at` (the minor lines).
pub fn grid_step(proj: &Proj, at: Vec3) -> f32 {
    let size = if proj.ortho { proj.half_h } else { (at - proj.eye).length() * proj.tan_half }.max(1e-6);
    10f32.powf((size / 2.0).log10().floor())
}

fn snap_to(x: f32, step: f32) -> f32 {
    (x / step).round() * step
}

impl Editor {
    /// The selected objects and lights.
    pub fn selected(&self) -> Vec<(Kind, usize)> {
        let s = &self.ws.sel;
        let objects = s.objects.iter().enumerate().filter(|(_, x)| **x).map(|(i, _)| (Kind::Object, i));
        let lights = s.lights.iter().enumerate().filter(|(_, x)| **x).map(|(j, _)| (Kind::Light, j));
        objects.chain(lights).collect()
    }

    /// Where a directional light's icon is (its anchor), for light entry `j`.
    fn light_anchor(&self, j: usize) -> Option<Vec3> {
        let i = self.ws.scene.provenance.lights.start as usize + j;
        overlay::light_anchor(self.ws.scene.lights().get(i)?, &self.ws.display)
    }

    fn placement_of(&self, kind: Kind, i: usize) -> Option<(Placement, Table)> {
        let item = self.list(kind).get(i)?;
        let t = self.ws.doc.item_table(item)?;
        let bounds = match kind {
            Kind::Object => self.ws.display.object_bounds.get(i).copied().unwrap_or(Aabb::EMPTY),
            _ => Aabb::EMPTY,
        };
        let anchor = if kind == Kind::Light { self.light_anchor(i) } else { None };
        Some((xform::placement(kind, &t, &bounds, anchor)?, t))
    }

    fn can(p: &Placement, tool: Tool) -> bool {
        match tool {
            Tool::Select => false,
            Tool::Move => p.can_move,
            Tool::Rotate => p.can_rotate,
            Tool::Scale => p.can_scale,
        }
    }

    /// The gizmo for the selection: at the mean of the pivots of the items the tool can
    /// change (at most a few thousand are looked at).
    fn build_gizmo(&self) -> Option<Gizmo> {
        if self.tool == Tool::Select {
            return None;
        }
        if let Some(e) = &self.ws.edit {
            // The selected vertices, along the world's or the object's axes.
            let pivot = e.pivot()?;
            let world = [Vec3::new(1.0, 0.0, 0.0), Vec3::new(0.0, 1.0, 0.0), Vec3::Z];
            let axes = if self.local_space || self.tool == Tool::Scale { e.axes() } else { world };
            return Some(Gizmo { tool: self.tool, pivot, axes, hot: None, active: None, scale: self.ui.scale });
        }
        let mut pivots = Vec::new();
        let mut axes = None;
        for (kind, i) in self.selected().into_iter().take(4000) {
            if let Some((p, _)) = self.placement_of(kind, i)
                && Self::can(&p, self.tool)
            {
                pivots.push(p.pivot);
                axes.get_or_insert(p.axes);
            }
        }
        if pivots.is_empty() {
            return None;
        }
        let pivot = pivots.iter().fold(Vec3::ZERO, |a, &b| a + b) * (1.0 / pivots.len() as f32);
        let world = [Vec3::new(1.0, 0.0, 0.0), Vec3::new(0.0, 1.0, 0.0), Vec3::Z];
        // Scaling is always along the items' own axes.
        let axes = if self.local_space || self.tool == Tool::Scale { axes.unwrap_or(world) } else { world };
        Some(Gizmo { tool: self.tool, pivot, axes, hot: None, active: None, scale: self.ui.scale })
    }

    /// Rebuilds what is drawn over the views: the gizmo, handles and helper lines.
    pub fn update_decor(&mut self) {
        let mut d = Decor { scale: self.ui.scale, ..Decor::default() };
        let key = (
            self.ws.doc.version(),
            self.ws.scene_gen,
            self.ws.sel.clone(),
            self.tool,
            self.local_space,
            self.inspect,
            self.ws.through_camera,
            self.ws.edit.as_ref().map_or(0, |e| e.version + 1),
        );
        if let (Some(k), Some(cached)) = (&self.decor_key, &self.decor_cache)
            && *k == key
        {
            d = cached.clone();
        } else {
            d.gizmo = self.build_gizmo();
            self.handles(&mut d);
            self.decor_cache = Some(d.clone());
            self.decor_key = Some(key);
        }
        // Hover and drag state on top of the cached geometry.
        let old = &self.ws.decor;
        d.hot_handle = old.hot_handle.filter(|&i| i < d.handles.len());
        if let (Some(g), Some(og)) = (&mut d.gizmo, &old.gizmo) {
            g.hot = og.hot;
        }
        if let Some(ToolDrag::Gizmo(gd)) = &self.tool_drag
            && let Some(g) = &mut d.gizmo
        {
            g.active = Some(gd.part);
            g.pivot = gd.pivot;
            g.axes = gd.axes;
            if let Gesture::Move(m) = gd.gesture {
                g.pivot = gd.pivot + m;
            }
        }
        // The loop cut's preview, and the reach of proportional editing while dragging.
        d.lines.extend(self.loop_cut_lines());
        if let (Some(ToolDrag::Gizmo(gd)), Some(e)) = (&self.tool_drag, &self.ws.edit)
            && e.proportional
            && gd.edit.is_some()
        {
            let c = d.gizmo.map_or(gd.pivot, |g| g.pivot);
            let r = e.radius;
            for k in 0..48 {
                let a = |k: usize| 2.0 * PI * k as f32 / 48.0;
                let p = |k: usize| c + (Vec3::new(1.0, 0.0, 0.0) * a(k).cos() + Vec3::Z * a(k).sin()) * r;
                d.lines.push((p(k), p(k + 1), 0x80_80_ff));
            }
        }
        let changed = d.gizmo != self.ws.decor.gizmo
            || d.handles != self.ws.decor.handles
            || d.lines != self.ws.decor.lines
            || d.hot_handle != self.ws.decor.hot_handle;
        if changed {
            self.ws.decor = d;
            self.vp_dirty = true;
        }
    }

    /// Handles and helper lines of what is selected or inspected.
    fn handles(&self, d: &mut Decor) {
        let doc = &self.ws.doc;
        let (center, r) = sphere(&self.ws.display.bounds);
        let dome_r = r * 1.15;
        let mut dome = false;
        for (kind, i) in self.selected().into_iter().take(64) {
            let Some(item) = self.list(kind).get(i) else { continue };
            let Some(t) = doc.item_table(item) else { continue };
            match kind {
                Kind::Light => {
                    let ty = t.get("type").and_then(Value::as_str).unwrap_or("");
                    let light = self.ws.scene.lights().get(self.ws.scene.provenance.lights.start as usize + i);
                    match (ty, light) {
                        (
                            "spot" | "spotlight",
                            Some(&pt_core::lights::Light::Spot { pos, dir, cos_outer, cos_inner, .. }),
                        ) => {
                            let len = r * 0.3;
                            let tip = pos + dir * len;
                            let f = Frame::from_normal(dir);
                            let outer = cos_outer.clamp(-1.0, 1.0).acos().min(1.5);
                            let inner = cos_inner.clamp(-1.0, 1.0).acos().min(1.5);
                            let ri = len * inner.tan();
                            for k in 0..24 {
                                let a = |k: usize| 2.0 * PI * k as f32 / 24.0;
                                let p = |k: usize| tip + (f.t * a(k).cos() + f.b * a(k).sin()) * ri;
                                d.lines.push((p(k), p(k + 1), 0x90_80_50));
                            }
                            d.handles.push(Handle { kind: HandleKind::SpotAim(i), pos: tip, color: 0xff_ff_ff });
                            let at = |a: f32, axis: Vec3| tip + axis * (len * a.tan());
                            d.handles.push(Handle {
                                kind: HandleKind::SpotAngle(i),
                                pos: at(outer, f.t),
                                color: 0xe8_d0_70,
                            });
                            d.handles.push(Handle {
                                kind: HandleKind::SpotInner(i),
                                pos: at(inner, f.b),
                                color: 0xb0_a0_60,
                            });
                        }
                        ("directional" | "distant" | "sun", Some(&pt_core::lights::Light::Distant { dir, .. })) => {
                            dome = true;
                            d.handles.push(Handle {
                                kind: HandleKind::Sun(Some(i)),
                                pos: center + dir * dome_r,
                                color: 0xff_d0_60,
                            });
                        }
                        _ => {}
                    }
                }
                Kind::Object => {
                    if let Some(Value::Table(s)) = t.get("scatter") {
                        let base = xform::read_vec(&t, "translate").unwrap_or(Vec3::ZERO);
                        let lo = base + xform::read_vec(s, "min").unwrap_or(Vec3::ZERO);
                        let hi = base + xform::read_vec(s, "max").unwrap_or(Vec3::ZERO);
                        let b = Aabb { min: lo.min(hi), max: lo.max(hi) };
                        box_lines(&mut d.lines, &b, SCATTER);
                        for k in 0..3 {
                            for side in [false, true] {
                                let mut p = b.centroid();
                                p[k] = if side { hi[k] } else { lo[k] };
                                d.handles.push(Handle {
                                    kind: HandleKind::Scatter(i, k, side),
                                    pos: p,
                                    color: SCATTER,
                                });
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        // The sky's sun, with the background inspected.
        if self.inspect == Some(Target::Section(Section::Background))
            && let Some(bf) = doc.tree.background
            && let Some(Value::Table(bg)) = doc.value(bf, &[Seg::from("background")])
            && matches!(bg.get("type").and_then(Value::as_str), Some("sky" | "daylight" | "preetham"))
        {
            dome = true;
            let sun = sky_sun(&bg);
            d.handles.push(Handle { kind: HandleKind::Sun(None), pos: center + sun * dome_r, color: 0xff_d0_60 });
        }
        if dome {
            dome_lines(&mut d.lines, center, dome_r);
        }
        // The focus plane, with the camera inspected or looked through.
        if self.inspect == Some(Target::Section(Section::Camera)) || self.ws.through_camera {
            let cam = &self.ws.scene.camera;
            let fwd = cam.look_at - cam.position;
            if fwd.length_squared() > 0.0 {
                let fd = cam.focus_distance.unwrap_or_else(|| fwd.length());
                let f = fwd.normalize();
                let right = f.cross(cam.up).normalize();
                if right.is_finite() {
                    let up = right.cross(f);
                    let c = cam.position + f * fd;
                    let hh = (cam.fov.min(160.0).to_radians() * 0.5).tan() * fd;
                    let aspect = self.ws.scene.settings.width as f32 / self.ws.scene.settings.height.max(1) as f32;
                    let hw = hh * aspect;
                    let corner = |x: f32, y: f32| c + right * (x * hw) + up * (y * hh);
                    let q = [corner(-1.0, -1.0), corner(1.0, -1.0), corner(1.0, 1.0), corner(-1.0, 1.0)];
                    for k in 0..4 {
                        d.lines.push((q[k], q[(k + 1) % 4], FOCUS));
                    }
                    for k in 1..4 {
                        let t = k as f32 / 4.0 * 2.0 - 1.0;
                        d.lines.push((corner(t, -1.0), corner(t, 1.0), dim(FOCUS)));
                        d.lines.push((corner(-1.0, t), corner(1.0, t), dim(FOCUS)));
                    }
                    d.handles.push(Handle { kind: HandleKind::Focus, pos: c, color: FOCUS });
                }
            }
        }
    }

    // ------------------------------------------------------------------ hover and press

    /// Highlights the gizmo part or handle under window pixel (`x`, `y`) of view `r`.
    pub fn tool_hover(&mut self, r: &ViewRect, x: i32, y: i32) {
        let (lx, ly) = self.local(r, x, y);
        let proj = self.ws.proj(r);
        let hot_handle = gizmo::hit_handle(&self.ws.decor, &proj, lx, ly);
        let hot =
            if hot_handle.is_some() { None } else { self.ws.decor.gizmo.and_then(|g| gizmo::hit(&g, &proj, lx, ly)) };
        if self.ws.decor.hot_handle != hot_handle || self.ws.decor.gizmo.and_then(|g| g.hot) != hot {
            self.ws.decor.hot_handle = hot_handle;
            if let Some(g) = &mut self.ws.decor.gizmo {
                g.hot = hot;
            }
            self.vp_dirty = true;
        }
    }

    /// View-local position of window pixel (`x`, `y`) in view `r` (pixel centres).
    fn local(&self, r: &ViewRect, x: i32, y: i32) -> (f32, f32) {
        ((x - self.vp.x - r.x as i32) as f32 + 0.5, (y - self.vp.y - r.y as i32) as f32 + 0.5)
    }

    /// A left press in view `r`: starts a tool drag if it is on a handle or the gizmo (or a
    /// region or focus is being picked); returns whether it did.
    pub fn tool_press(&mut self, r: &ViewRect, x: i32, y: i32) -> bool {
        let (lx, ly) = self.local(r, x, y);
        if self.knife_press(r, lx, ly) {
            return true;
        }
        if self.focus_pick {
            self.focus_pick = false;
            self.pick_focus(r, lx, ly);
            return true;
        }
        if self.region_pick {
            self.region_pick = false;
            if self.ws.views[r.view].is_fixed() {
                self.notify("the render region is drawn in the perspective view");
                return true;
            }
            self.tool_drag = Some(ToolDrag::Region(*r, (lx as i32, ly as i32)));
            return true;
        }
        let proj = self.ws.proj(r);
        if let Some(h) = gizmo::hit_handle(&self.ws.decor, &proj, lx, ly) {
            let kind = self.ws.decor.handles[h].kind;
            if let Some((file, path)) = self.handle_target(kind) {
                let table = self.table_at(file, &path);
                self.groups += 1;
                let merge = format!("handle {}", self.groups);
                self.tool_drag = Some(ToolDrag::Handle(Box::new(HandleDrag {
                    kind,
                    view: *r,
                    file,
                    path,
                    table,
                    merge,
                    wrote: false,
                })));
                return true;
            }
        }
        let Some(g) = self.ws.decor.gizmo else { return false };
        let Some(part) = gizmo::hit(&g, &proj, lx, ly) else { return false };
        self.start_gizmo(r, part, (lx, ly), g);
        true
    }

    fn start_gizmo(&mut self, r: &ViewRect, part: Part, start: (f32, f32), g: Gizmo) {
        let edit = self.ws.edit.as_ref().map(|e| EditDrag {
            before: e.snapshot(g.tool.name()),
            base: e.mesh.positions.clone(),
            weights: e.weights(),
            moved: false,
        });
        let mut items = Vec::new();
        for (kind, i) in self.selected() {
            if edit.is_some() {
                break;
            }
            let Some((p, table)) = self.placement_of(kind, i) else { continue };
            if !Self::can(&p, g.tool) {
                continue;
            }
            let item = &self.list(kind)[i];
            let scattered = table.contains_key("scatter");
            items.push(DragItem {
                kind,
                index: i,
                file: item.file,
                path: item.path.clone(),
                table,
                axes: p.axes,
                applied: Transform::IDENTITY,
                scattered,
            });
        }
        if items.is_empty() && edit.as_ref().is_none_or(|e| e.weights.is_empty()) {
            return;
        }
        self.groups += 1;
        let gesture = match g.tool {
            Tool::Rotate => Gesture::Rotate { axis: Vec3::new(0.0, 1.0, 0.0), degrees: 0.0 },
            Tool::Scale => Gesture::Scale(Vec3::ONE),
            _ => Gesture::Move(Vec3::ZERO),
        };
        let mut d = GizmoDrag {
            part,
            tool: g.tool,
            view: *r,
            start,
            pivot: g.pivot,
            axes: g.axes,
            items,
            start_hit: None,
            angle: 0.0,
            last_angle: None,
            typed: String::new(),
            gesture,
            wrote: false,
            merge: format!("gizmo {}", self.groups),
            reload: false,
            edit,
        };
        d.start_hit = self.plane_point(&d, start.0, start.1);
        self.tool_drag = Some(ToolDrag::Gizmo(Box::new(d)));
        self.status_drag();
    }

    // ------------------------------------------------------------------ gizmo drags

    /// The plane a plane or view move happens in, and where the ray meets it.
    fn plane_point(&self, d: &GizmoDrag, x: f32, y: f32) -> Option<Vec3> {
        let proj = self.ws.proj(&d.view);
        let n = match d.part {
            Part::Plane(k) => d.axes[k],
            Part::Center => proj.fwd,
            _ => return None,
        };
        let ray = proj.ray(x, y);
        gizmo::plane_hit(d.pivot, n, ray.o, ray.d)
    }

    /// The gesture of the mouse at view-local (`x`, `y`).
    fn gesture_at(&mut self, d: &mut GizmoDrag, x: f32, y: f32, ctrl: bool) -> Gesture {
        let proj = self.ws.proj(&d.view);
        let snap = match (self.snap, ctrl) {
            (Snap::Off, true) => Snap::Grid,
            (_, true) => Snap::Off,
            (s, false) => s,
        };
        let typed = crate::ui::widgets::eval(&d.typed).map(|v| v as f32);
        let c = proj.to_screen(d.pivot).map_or((0.0, 0.0), |(cx, cy, _)| (cx, cy));
        match d.tool {
            Tool::Move | Tool::Select => {
                let delta = if let (Some(v), Part::Axis(k)) = (typed, d.part) {
                    return Gesture::Move(d.axes[k] * v);
                } else {
                    let ray = proj.ray(x, y);
                    match d.part {
                        Part::Axis(k) => {
                            let a = d.axes[k];
                            let r0 = proj.ray(d.start.0, d.start.1);
                            match (
                                gizmo::line_param(d.pivot, a, ray.o, ray.d),
                                gizmo::line_param(d.pivot, a, r0.o, r0.d),
                            ) {
                                (Some(t), Some(t0)) => a * (t - t0),
                                _ => Vec3::ZERO,
                            }
                        }
                        _ => match (self.plane_point(d, x, y), d.start_hit) {
                            (Some(h), Some(h0)) => h - h0,
                            _ => Vec3::ZERO,
                        },
                    }
                };
                Gesture::Move(self.snap_move(d, delta, &proj, x, y, snap))
            }
            Tool::Rotate => {
                let axis = match d.part {
                    Part::Ring(k) => d.axes[k],
                    _ => -proj.fwd,
                };
                if let Some(v) = typed {
                    return Gesture::Rotate { axis, degrees: v };
                }
                let a = (y - c.1).atan2(x - c.0);
                if let Some(last) = d.last_angle {
                    let mut da = a - last;
                    while da > PI {
                        da -= 2.0 * PI;
                    }
                    while da < -PI {
                        da += 2.0 * PI;
                    }
                    d.angle += da;
                } else {
                    let a0 = (d.start.1 - c.1).atan2(d.start.0 - c.0);
                    d.angle = a - a0;
                }
                d.last_angle = Some(a);
                // Screen y points down: a turn that looks counter-clockwise is negative.
                let facing = if proj.ortho { -proj.fwd } else { proj.eye - d.pivot };
                let sign = if axis.dot(facing) >= 0.0 { -1.0 } else { 1.0 };
                let mut deg = sign * d.angle.to_degrees();
                if snap == Snap::Grid {
                    deg = snap_to(deg, ANGLE_STEP);
                }
                Gesture::Rotate { axis, degrees: deg }
            }
            Tool::Scale => {
                let from = (d.start.0 - c.0, d.start.1 - c.1);
                let to = (x - c.0, y - c.1);
                let (k, f) = if let Part::Axis(k) = d.part {
                    let tip = proj.to_screen(d.pivot + d.axes[k] * gizmo::size(&proj, d.pivot, self.ui.scale));
                    let dir = tip.map_or((1.0, 0.0), |(tx, ty, _)| {
                        let (dx, dy) = (tx - c.0, ty - c.1);
                        let l = dx.hypot(dy).max(1e-3);
                        (dx / l, dy / l)
                    });
                    let f0 = from.0 * dir.0 + from.1 * dir.1;
                    let f1 = to.0 * dir.0 + to.1 * dir.1;
                    (Some(k), if f0.abs() > 1.0 { f1 / f0 } else { 1.0 })
                } else {
                    let f0 = from.0.hypot(from.1);
                    (None, if f0 > 1.0 { to.0.hypot(to.1) / f0 } else { 1.0 })
                };
                let mut f = typed.unwrap_or(f);
                if typed.is_none() && snap == Snap::Grid {
                    f = snap_to(f, SCALE_STEP);
                }
                if f.abs() < 1e-3 {
                    f = 1e-3f32.copysign(if f == 0.0 { 1.0 } else { f });
                }
                Gesture::Scale(match k {
                    Some(k) => {
                        let mut v = Vec3::ONE;
                        v[k] = f;
                        v
                    }
                    None => Vec3::splat(f),
                })
            }
        }
    }

    /// `delta` snapped: to the grid, or onto a vertex or the surface under the cursor
    /// (kept to the part's axis or plane).
    fn snap_move(&self, d: &GizmoDrag, delta: Vec3, proj: &Proj, x: f32, y: f32, snap: Snap) -> Vec3 {
        let dirs: Vec<Vec3> = match d.part {
            Part::Axis(k) => vec![d.axes[k]],
            Part::Plane(k) => vec![d.axes[(k + 1) % 3], d.axes[(k + 2) % 3]],
            _ => d.axes.to_vec(),
        };
        let constrain = |v: Vec3| match d.part {
            Part::Axis(k) => d.axes[k] * v.dot(d.axes[k]),
            Part::Plane(k) => v - d.axes[k] * v.dot(d.axes[k]),
            _ => v,
        };
        match snap {
            Snap::Off => delta,
            Snap::Grid => {
                let step = grid_step(proj, d.pivot);
                let world = d.axes[0] == Vec3::new(1.0, 0.0, 0.0) && d.axes[1] == Vec3::new(0.0, 1.0, 0.0);
                if world {
                    // Positions land on grid lines.
                    let mut target = d.pivot + delta;
                    for c in 0..3 {
                        if dirs.iter().any(|a| a[c].abs() > 1e-3) {
                            target[c] = snap_to(target[c], step);
                        }
                    }
                    target - d.pivot
                } else {
                    dirs.iter().fold(Vec3::ZERO, |acc, &a| acc + a * snap_to(delta.dot(a), step))
                }
            }
            Snap::Vertex => self.vertex_near(&d.view, proj, x, y).map_or(delta, |v| constrain(v - d.pivot)),
            Snap::Surface => self.surface_at(proj, x, y).map_or(delta, |v| constrain(v - d.pivot)),
        }
    }

    /// The vertex of an unselected object nearest to view-local (`x`, `y`), within reach.
    fn vertex_near(&self, _r: &ViewRect, proj: &Proj, x: f32, y: f32) -> Option<Vec3> {
        let reach = 12.0 * self.ui.scale;
        let disp = &self.ws.display;
        let mut best: Option<(f32, Vec3)> = None;
        let mut budget = 2_000_000usize;
        let editing = self.ws.edit.as_ref().map(|e| e.object as u32);
        // The mesh being edited: its vertices that do not move.
        if let Some(e) = &self.ws.edit {
            for v in 0..e.mesh.vertex_count() as u32 {
                if e.sel.has_vert(v) {
                    continue;
                }
                let w = e.world(v);
                if let Some((sx, sy, _)) = proj.to_screen(w) {
                    let dist = (sx - x).hypot(sy - y);
                    if dist <= reach && best.is_none_or(|b| dist < b.0) {
                        best = Some((dist, w));
                    }
                }
            }
        }
        for dr in &disp.draws {
            if dr.object.is_some() && dr.object == editing {
                continue;
            }
            if dr.object.is_some_and(|o| self.ws.sel.objects.get(o as usize).copied().unwrap_or(false)) {
                continue;
            }
            if dr.object.is_some_and(|o| self.ws.hidden.objects.get(o as usize).copied().unwrap_or(false)) {
                continue;
            }
            // Screen box of the draw's bounds.
            let mut lo = (f32::INFINITY, f32::INFINITY);
            let mut hi = (f32::NEG_INFINITY, f32::NEG_INFINITY);
            let b = dr.bounds;
            for i in 0..8 {
                let p = Vec3::new(
                    if i & 1 == 0 { b.min.x } else { b.max.x },
                    if i & 2 == 0 { b.min.y } else { b.max.y },
                    if i & 4 == 0 { b.min.z } else { b.max.z },
                );
                if let Some((sx, sy, _)) = proj.to_screen(p) {
                    lo = (lo.0.min(sx), lo.1.min(sy));
                    hi = (hi.0.max(sx), hi.1.max(sy));
                }
            }
            if x < lo.0 - reach || x > hi.0 + reach || y < lo.1 - reach || y > hi.1 + reach {
                continue;
            }
            let mesh = match dr.part {
                crate::display::Part::Mesh(m) => m,
                crate::display::Part::Shape(c) => disp.canonical(c, 1),
                _ => crate::display::CUBE,
            };
            let positions = &disp.meshes[mesh as usize].positions;
            if positions.len() > budget {
                continue;
            }
            budget -= positions.len();
            for &p in positions {
                let w = dr.xf.point(p);
                if let Some((sx, sy, _)) = proj.to_screen(w) {
                    let dist = (sx - x).hypot(sy - y);
                    if dist <= reach && best.is_none_or(|b| dist < b.0) {
                        best = Some((dist, w));
                    }
                }
            }
        }
        best.map(|b| b.1)
    }

    /// The point of the surface (of an unselected, shown object) under view-local (`x`, `y`).
    pub fn surface_at(&self, proj: &Proj, x: f32, y: f32) -> Option<Vec3> {
        let scene = &self.ws.scene;
        let mut ray = proj.ray(x, y);
        // The object being edited is not where the scene has it.
        let editing = self.ws.edit.as_ref().map(|e| e.object as u32 + 1);
        for _ in 0..64 {
            let hit = scene.intersect(&ray, f32::INFINITY)?;
            let p = ray.o + ray.d * hit.t;
            let id = scene.object_of(hit.prim, hit.inst).map(|o| o as u32 + 1);
            if !id.is_some_and(|id| self.ws.sel.has_id(id) || self.ws.hidden.has_id(id) || Some(id) == editing) {
                return Some(p);
            }
            ray.o = p + ray.d * (1e-4 * (1.0 + p.length()));
        }
        None
    }

    /// Writes gesture `g` for every item of the drag and moves them in place.
    fn apply_gesture(&mut self, d: &mut GizmoDrag, g: Gesture) {
        d.gesture = g;
        if let Some(ed) = &mut d.edit {
            // Vertices: each moved by its weight of the gesture, in the world.
            let map = xform::gesture_map(&g, d.pivot, &d.axes);
            if let Some(e) = &mut self.ws.edit {
                for &(v, w) in &ed.weights {
                    let p = e.xf.point(ed.base[v as usize]);
                    e.mesh.positions[v as usize] = e.xf.inv_point(p + (map.point(p) - p) * w);
                }
                ed.moved = true;
            }
            self.ws.move_edit();
            self.vp_dirty = true;
            return;
        }
        let merge = d.merge.clone();
        let mut failed = None;
        for item in &mut d.items {
            for op in xform::edits(item.kind, &item.table, &g, d.pivot, &item.axes) {
                let r = match op {
                    Op::Set(k, v) => {
                        let mut p = item.path.clone();
                        p.push(Seg::from(k));
                        self.ws.doc.set_merging(item.file, &p, &v, &merge)
                    }
                    Op::Remove(k) => {
                        let mut p = item.path.clone();
                        p.push(Seg::from(k));
                        if self.ws.doc.value(item.file, &p).is_some() {
                            self.ws.doc.remove_merging(item.file, &p, &merge)
                        } else {
                            Ok(false)
                        }
                    }
                };
                match r {
                    Ok(changed) => d.wrote |= changed,
                    Err(e) => failed = Some(e),
                }
            }
            if d.reload {
                continue;
            }
            match item.kind {
                Kind::Object => {
                    let map = xform::gesture_map(&g, d.pivot, &item.axes);
                    let step = item.applied.inverse().then(&map);
                    if self.ws.transform_object(item.index, &step) {
                        item.applied = map;
                    } else {
                        d.reload = true;
                    }
                }
                Kind::Light => {
                    let t = self.list(Kind::Light).get(item.index).and_then(|it| self.ws.doc.item_table(it));
                    if !t.is_some_and(|t| self.ws.update_light(item.index, &t)) {
                        d.reload = true;
                    }
                }
                _ => {}
            }
        }
        if let Some(e) = failed {
            self.notify(format!("error: {e}"));
        }
        if d.reload {
            self.scene_stale = true;
        }
        self.vp_dirty = true;
    }

    fn status_drag(&mut self) {
        let Some(ToolDrag::Gizmo(d)) = &self.tool_drag else { return };
        let what = match d.gesture {
            Gesture::Move(m) => {
                format!("move {} {} {}", xform::tidy(m.x), xform::tidy(m.y), xform::tidy(m.z))
            }
            Gesture::Rotate { degrees, .. } => format!("rotate {}°", xform::tidy(degrees)),
            Gesture::Scale(k) => format!("scale {} {} {}", xform::tidy(k.x), xform::tidy(k.y), xform::tidy(k.z)),
        };
        let typed = if d.typed.is_empty() { String::new() } else { format!("  [{}]", d.typed) };
        self.status = format!("{what}{typed}   type a value · X Y Z: axis · Ctrl: snap · Enter / Esc");
    }

    /// The mouse moved to window pixel (`x`, `y`) during a tool drag.
    pub fn tool_move(&mut self, x: i32, y: i32, mods: Modifiers) {
        match self.tool_drag.take() {
            Some(ToolDrag::Gizmo(mut d)) => {
                let (lx, ly) = self.local(&d.view, x, y);
                let g = self.gesture_at(&mut d, lx, ly, mods.ctrl);
                self.apply_gesture(&mut d, g);
                self.tool_drag = Some(ToolDrag::Gizmo(d));
                self.status_drag();
            }
            Some(ToolDrag::Handle(mut h)) => {
                let (lx, ly) = self.local(&h.view, x, y);
                self.drag_handle(&mut h, lx, ly);
                self.tool_drag = Some(ToolDrag::Handle(h));
            }
            Some(ToolDrag::Knife(k)) => {
                let view = k.view;
                self.tool_drag = Some(ToolDrag::Knife(k));
                if let Some(r) = view {
                    let (lx, ly) = self.local(&r, x, y);
                    self.knife_move(lx, ly);
                }
            }
            other => {
                self.tool_drag = other;
                self.vp_dirty = true;
            }
        }
    }

    /// The tool drag ends (the button was let go, or Enter).
    pub fn tool_release(&mut self, x: i32, y: i32) {
        match self.tool_drag.take() {
            Some(ToolDrag::Gizmo(d)) if d.edit.is_some() => {
                let Some(ed) = d.edit else { return };
                if ed.moved
                    && let Some(e) = &mut self.ws.edit
                {
                    e.record(ed.before);
                    let n = ed.weights.len();
                    self.notify(format!("{}d {n} vertices", d.tool.name()));
                }
                self.edit_changed();
            }
            Some(ToolDrag::Knife(k)) => self.knife_cut(&k),
            // Not drags: they end with a click or a key.
            other @ Some(ToolDrag::Modal(_) | ToolDrag::LoopCut(_)) => self.tool_drag = other,
            Some(ToolDrag::Gizmo(d)) => {
                self.ws.doc.seal();
                // Scatters are placed again by the loader exactly; a fallback has reloaded.
                if d.wrote && d.items.iter().any(|i| i.scattered) {
                    self.scene_stale = true;
                }
                self.notify(if d.wrote {
                    format!("{}d {}", d.tool.name(), self.selection_name())
                } else {
                    "nothing changed".into()
                });
            }
            Some(ToolDrag::Handle(h)) => {
                self.ws.doc.seal();
                if h.wrote {
                    self.scene_stale = true;
                }
            }
            Some(ToolDrag::Region(r, (x0, y0))) => {
                let (lx, ly) = self.local(&r, x, y);
                let (w, h) = (r.w.max(1) as f32, r.h.max(1) as f32);
                if (lx - x0 as f32).abs() < 4.0 || (ly - y0 as f32).abs() < 4.0 {
                    self.ws.region = None;
                    self.notify("render region cleared");
                } else {
                    self.ws.region = Some([x0 as f32 / w, y0 as f32 / h, lx / w, ly / h]);
                    self.ws.rendered = true;
                    self.notify("rendering the region");
                }
            }
            None => {}
        }
        self.vp_dirty = true;
    }

    /// The radius of proportional editing changed while vertices are dragged: which move
    /// and how much again, and the gesture done again with that.
    pub fn proportional_changed(&mut self) {
        let Some(ToolDrag::Gizmo(mut d)) = self.tool_drag.take() else { return };
        if let (Some(ed), Some(e)) = (&mut d.edit, &mut self.ws.edit) {
            e.mesh.positions.clone_from(&ed.base);
            ed.weights = e.weights();
        }
        let g = d.gesture;
        self.apply_gesture(&mut d, g);
        self.tool_drag = Some(ToolDrag::Gizmo(d));
    }

    /// Esc during a gizmo drag: everything goes back.
    pub fn tool_cancel(&mut self) {
        match self.tool_drag.take() {
            Some(ToolDrag::Gizmo(d)) if d.edit.is_some() => {
                if let (Some(ed), Some(e)) = (d.edit, &mut self.ws.edit) {
                    e.mesh.positions = ed.base;
                }
                self.ws.move_edit();
                self.notify("cancelled");
            }
            other @ Some(ToolDrag::Modal(_) | ToolDrag::LoopCut(_) | ToolDrag::Knife(_)) => {
                self.tool_drag = other;
                self.modal_cancel();
            }
            Some(ToolDrag::Gizmo(d)) if d.wrote => {
                self.ws.doc.seal();
                if self.ws.doc.undo().is_ok() {
                    self.scene_stale = true;
                }
                self.notify("cancelled");
            }
            Some(ToolDrag::Handle(h)) if h.wrote => {
                self.ws.doc.seal();
                if self.ws.doc.undo().is_ok() {
                    self.scene_stale = true;
                }
            }
            _ => {}
        }
        self.vp_dirty = true;
    }

    /// Keys during a gizmo drag: numbers typed, X / Y / Z for the axis, Enter, Escape.
    /// Returns whether a drag is going on (and took the keys).
    pub fn tool_keys(&mut self) -> bool {
        if let Some(taken) = self.modal_keys() {
            return taken;
        }
        let Some(ToolDrag::Gizmo(_)) = &self.tool_drag else {
            if self.tool_drag.is_some() && self.ui.take_key(Key::Escape) {
                self.tool_cancel();
            }
            return self.tool_drag.is_some();
        };
        let keys = std::mem::take(&mut self.ui.input.keys);
        let text = std::mem::take(&mut self.ui.input.text);
        let mut retype = false;
        for (key, mods) in keys {
            match key {
                Key::Escape => {
                    self.tool_cancel();
                    return true;
                }
                Key::Enter => {
                    let (x, y) = self.ui.input.mouse;
                    self.tool_release(x, y);
                    self.drag_done = true;
                    return true;
                }
                Key::Backspace => {
                    if let Some(ToolDrag::Gizmo(d)) = &mut self.tool_drag {
                        d.typed.pop();
                        retype = true;
                    }
                }
                Key::Char(c @ ('x' | 'y' | 'z')) if !mods.ctrl => {
                    let k = (c as u8 - b'x') as usize;
                    if let Some(ToolDrag::Gizmo(d)) = &mut self.tool_drag {
                        d.part = match (d.tool, mods.shift) {
                            (Tool::Rotate, _) => Part::Ring(k),
                            (Tool::Move, true) => Part::Plane(k),
                            _ => Part::Axis(k),
                        };
                        d.start_hit = None;
                        let (sx, sy) = d.start;
                        let dd = (**d).clone();
                        let hit = self.plane_point(&dd, sx, sy);
                        if let Some(ToolDrag::Gizmo(d)) = &mut self.tool_drag {
                            d.start_hit = hit;
                        }
                        retype = true;
                    }
                }
                _ => {}
            }
        }
        for c in text.chars() {
            if (c.is_ascii_digit() || matches!(c, '.' | '-' | '+' | '*' | '/' | '(' | ')'))
                && let Some(ToolDrag::Gizmo(d)) = &mut self.tool_drag
            {
                d.typed.push(c);
                retype = true;
            }
        }
        if retype {
            let (x, y) = self.ui.input.mouse;
            let mods = self.ui.input.mods;
            self.tool_move(x, y, mods);
        }
        true
    }

    fn selection_name(&self) -> String {
        let sel = self.selected();
        match sel.as_slice() {
            [(Kind::Object, i)] => self.ws.label(*i as u32 + 1),
            [(Kind::Light, j)] => self.ws.label(LIGHT_ID | *j as u32),
            s => format!("{} items", s.len()),
        }
    }

    // ------------------------------------------------------------------ handles

    /// The table a handle edits: file and path.
    fn handle_target(&self, kind: HandleKind) -> Option<(usize, Vec<Seg>)> {
        let doc = &self.ws.doc;
        match kind {
            HandleKind::SpotAngle(j) | HandleKind::SpotInner(j) | HandleKind::SpotAim(j) | HandleKind::Sun(Some(j)) => {
                let it = doc.tree.lights.get(j)?;
                Some((it.file, it.path.clone()))
            }
            HandleKind::Sun(None) => Some((doc.tree.background?, vec![Seg::from("background")])),
            HandleKind::Scatter(o, ..) => {
                let it = doc.tree.objects.get(o)?;
                let mut p = it.path.clone();
                p.push(Seg::from("scatter"));
                Some((it.file, p))
            }
            HandleKind::Focus => Some((self.camera_file(), vec![Seg::from("camera")])),
        }
    }

    /// The file whose `[camera]` holds the pose (where its keys are written).
    pub fn camera_file(&self) -> usize {
        let t = &self.ws.doc.tree;
        t.settings.iter().find(|s| s.section == "camera" && s.key == "position").map_or(0, |s| s.file)
    }

    fn table_at(&self, file: usize, path: &[Seg]) -> Table {
        self.ws.doc.value(file, path).and_then(|v| v.as_table().cloned()).unwrap_or_default()
    }

    /// Sets key `key` of the handle's table (creating the table if needed).
    fn handle_set(&mut self, h: &mut HandleDrag, key: &str, v: Value) {
        let mut p = h.path.clone();
        p.push(Seg::from(key));
        let r = if self.ws.doc.value(h.file, &h.path).is_some() {
            self.ws.doc.set_merging(h.file, &p, &v, &h.merge)
        } else {
            let mut t = Table::new();
            t.insert(key.to_owned(), v);
            self.ws.doc.set_merging(h.file, &h.path, &Value::Table(t), &h.merge)
        };
        match r {
            Ok(c) => h.wrote |= c,
            Err(e) => self.notify(format!("error: {e}")),
        }
    }

    fn drag_handle(&mut self, h: &mut HandleDrag, x: f32, y: f32) {
        let proj = self.ws.proj(&h.view);
        let ray = proj.ray(x, y);
        let (center, r) = sphere(&self.ws.display.bounds);
        match h.kind {
            HandleKind::SpotAngle(j) | HandleKind::SpotInner(j) | HandleKind::SpotAim(j) => {
                let light = self.ws.scene.lights().get(self.ws.scene.provenance.lights.start as usize + j);
                let Some(&pt_core::lights::Light::Spot { pos, dir, .. }) = light else { return };
                let len = r * 0.3;
                let tip = pos + dir * len;
                if let HandleKind::SpotAim(_) = h.kind {
                    let Some(hit) = gizmo::plane_hit(tip, proj.fwd, ray.o, ray.d) else { return };
                    if (hit - pos).length_squared() > 0.0 {
                        let ops = xform::aim_light(&h.table, hit - pos);
                        for op in ops {
                            if let Op::Set(k, v) = op {
                                self.handle_set(h, &k, v);
                            }
                        }
                    }
                } else {
                    let f = Frame::from_normal(dir);
                    let axis = if matches!(h.kind, HandleKind::SpotAngle(_)) { f.t } else { f.b };
                    let Some(t) = gizmo::line_param(tip, axis, ray.o, ray.d) else { return };
                    let deg = (t.abs() / len).atan().to_degrees().clamp(0.5, 89.0);
                    let deg = if self.snap == Snap::Grid { snap_to(deg, 5.0).max(5.0) } else { deg };
                    let deg = xform::tidy(deg);
                    let outer = h.table.get("angle").and_then(Value::as_f64).unwrap_or(30.0);
                    if matches!(h.kind, HandleKind::SpotAngle(_)) {
                        self.handle_set(h, "angle", Value::Number(deg));
                        if h.table.get("inner_angle").and_then(Value::as_f64).is_some_and(|i| i > deg) {
                            self.handle_set(h, "inner_angle", Value::Number(deg));
                        }
                    } else {
                        let outer = self
                            .ws
                            .doc
                            .value(h.file, &[h.path.clone(), vec![Seg::from("angle")]].concat())
                            .and_then(|v| v.as_f64())
                            .unwrap_or(outer);
                        self.handle_set(h, "inner_angle", Value::Number(deg.min(outer)));
                    }
                }
                let t = self.ws.doc.value(h.file, &h.path).and_then(|v| v.as_table().cloned());
                if !t.is_some_and(|t| self.ws.update_light(j, &t)) {
                    self.scene_stale = true;
                }
            }
            HandleKind::Sun(which) => {
                let dome = r * 1.15;
                // Where the ray meets the dome (else the point of the ray nearest to it).
                let oc = ray.o - center;
                let b = oc.dot(ray.d);
                let disc = b * b - (oc.length_squared() - dome * dome);
                let t = if disc >= 0.0 { -b + disc.sqrt() } else { -b };
                let toward = (ray.o + ray.d * t - center).normalize();
                if !toward.is_finite() {
                    return;
                }
                if let Some(j) = which {
                    for op in xform::aim_light(&h.table, -toward) {
                        if let Op::Set(k, v) = op {
                            self.handle_set(h, &k, v);
                        }
                    }
                    let t = self.ws.doc.value(h.file, &h.path).and_then(|v| v.as_table().cloned());
                    if !t.is_some_and(|t| self.ws.update_light(j, &t)) {
                        self.scene_stale = true;
                    }
                } else {
                    if h.table.contains_key("sun_direction") {
                        self.handle_set(h, "sun_direction", xform::vec_value(toward));
                    } else {
                        let near = h.table.get("sun_azimuth").and_then(Value::as_f64).unwrap_or(0.0) as f32;
                        let (el, az) = xform::to_angles(toward, near);
                        self.handle_set(h, "sun_elevation", Value::Number(xform::tidy(el)));
                        self.handle_set(h, "sun_azimuth", Value::Number(xform::tidy(az)));
                    }
                    // The sky is part of the scene: shown as it goes.
                    self.scene_stale = true;
                }
            }
            HandleKind::Scatter(o, k, max) => {
                let Some(t) = self.list(Kind::Object).get(o).and_then(|it| self.ws.doc.item_table(it)) else { return };
                let base = xform::read_vec(&t, "translate").unwrap_or(Vec3::ZERO);
                let key = if max { "max" } else { "min" };
                let cur = xform::read_vec(&h.table, key).unwrap_or(Vec3::ZERO);
                let mut axis = Vec3::ZERO;
                axis[k] = 1.0;
                let mut at = base + cur;
                if let Some(Value::Table(s)) = t.get("scatter") {
                    let other = xform::read_vec(s, if max { "min" } else { "max" }).unwrap_or(Vec3::ZERO);
                    at = base + (cur + other) * 0.5;
                    at[k] = base[k] + cur[k];
                }
                let Some(p) = gizmo::line_param(at, axis, ray.o, ray.d) else { return };
                let mut v = cur;
                v[k] = cur[k] + p;
                if self.snap == Snap::Grid {
                    v[k] = snap_to(v[k], grid_step(&proj, at));
                }
                self.handle_set(h, key, xform::vec_value(v));
            }
            HandleKind::Focus => {
                let cam = self.ws.scene.camera.clone();
                let f = (cam.look_at - cam.position).normalize();
                let fd = cam.focus_distance.unwrap_or_else(|| (cam.look_at - cam.position).length());
                let Some(t) = gizmo::line_param(cam.position + f * fd, f, ray.o, ray.d) else { return };
                let new = (fd + t).max(1e-3);
                self.handle_set(h, "focus_distance", Value::Number(xform::tidy(new)));
                self.scene_mut_camera(|c| c.focus_distance = Some(new));
            }
        }
        self.vp_dirty = true;
    }

    /// Changes the scene's camera in place (no reload).
    fn scene_mut_camera(&mut self, f: impl FnOnce(&mut pt_core::camera::CameraDesc)) {
        f(&mut self.ws.scene_mut().camera);
    }

    /// Click-to-focus: the focus distance becomes the depth of what is under view-local
    /// (`x`, `y`), along the scene camera's view direction.
    fn pick_focus(&mut self, r: &ViewRect, x: f32, y: f32) {
        let proj = self.ws.proj(r);
        let ray = proj.ray(x, y);
        let Some(hit) = self.ws.scene.intersect(&ray, f32::INFINITY) else {
            self.notify("nothing there to focus on");
            return;
        };
        let p = ray.o + ray.d * hit.t;
        let cam = self.ws.scene.camera.clone();
        let f = (cam.look_at - cam.position).normalize();
        let dist = (p - cam.position).dot(f);
        if dist.is_nan() || dist <= 0.0 {
            self.notify("that is behind the camera");
            return;
        }
        let path = [Seg::from("camera"), Seg::from("focus_distance")];
        let file = self.camera_file();
        let v = Value::Number(xform::tidy(dist));
        let r = if self.ws.doc.value(file, &[Seg::from("camera")]).is_some() {
            self.ws.doc.set(file, &path, &v)
        } else {
            let mut t = Table::new();
            t.insert("focus_distance".into(), v);
            self.ws.doc.set(file, &path[..1], &Value::Table(t))
        };
        match r {
            Ok(_) => {
                self.scene_mut_camera(|c| c.focus_distance = Some(dist));
                self.notify(format!("focus distance {}", xform::tidy(dist)));
            }
            Err(e) => self.notify(format!("error: {e}")),
        }
        self.vp_dirty = true;
    }

    /// The perspective view moved: with the camera locked to it, the camera follows (one
    /// undo step per drag); else looking through the camera ends.
    pub fn view_moved(&mut self, v: usize) {
        if !self.ws.through_camera || self.ws.views[v].is_fixed() {
            return;
        }
        if !self.ws.lock_camera {
            self.ws.through_camera = false;
            return;
        }
        let (pos, look) = self.ws.views[v].pose();
        let file = self.camera_file();
        let merge = format!("camera {}", self.camera_moves);
        let mut ok = true;
        for (k, val) in [("position", pos), ("look_at", look)] {
            let path = [Seg::from("camera"), Seg::from(k)];
            let r = if self.ws.doc.value(file, &path[..1]).is_some() {
                self.ws.doc.set_merging(file, &path, &xform::vec_value(val), &merge)
            } else {
                let mut t = Table::new();
                t.insert(k.into(), xform::vec_value(val));
                self.ws.doc.set_merging(file, &path[..1], &Value::Table(t), &merge)
            };
            if let Err(e) = r {
                self.notify(format!("error: {e}"));
                ok = false;
            }
        }
        if ok {
            self.scene_mut_camera(|c| {
                c.position = pos;
                c.look_at = look;
            });
        }
    }

    /// Ends a camera move (the next one is a new undo step).
    pub fn view_move_done(&mut self) {
        self.camera_moves += 1;
        self.ws.doc.seal();
    }
}

fn dim(c: u32) -> u32 {
    let ch = |s: u32| ((c >> s & 0xff) / 2) << s;
    ch(16) | ch(8) | ch(0)
}

fn box_lines(out: &mut Vec<(Vec3, Vec3, u32)>, b: &Aabb, color: u32) {
    let corner = |i: u32| {
        Vec3::new(
            if i & 1 == 0 { b.min.x } else { b.max.x },
            if i & 2 == 0 { b.min.y } else { b.max.y },
            if i & 4 == 0 { b.min.z } else { b.max.z },
        )
    };
    for i in 0..8u32 {
        for bit in [1, 2, 4] {
            if i & bit == 0 {
                out.push((corner(i), corner(i | bit), color));
            }
        }
    }
}

/// The sky dome: rings at 0, 30 and 60 degrees of elevation and eight meridians.
fn dome_lines(out: &mut Vec<(Vec3, Vec3, u32)>, c: Vec3, r: f32) {
    const N: usize = 48;
    for el in [0.0f32, 30.0, 60.0] {
        let p = |k: usize| c + xform::from_angles(el, 360.0 * k as f32 / N as f32) * r;
        for k in 0..N {
            out.push((p(k), p(k + 1), DOME));
        }
    }
    for m in 0..8 {
        let az = 45.0 * m as f32;
        let p = |k: usize| c + xform::from_angles(90.0 * k as f32 / 12.0, az) * r;
        for k in 0..12 {
            out.push((p(k), p(k + 1), DOME));
        }
    }
}

/// The direction towards the sky's sun, from `[background]`.
pub fn sky_sun(bg: &Table) -> Vec3 {
    match xform::read_vec(bg, "sun_direction") {
        Some(d) if d.length_squared() > 0.0 => d.normalize(),
        _ => {
            let n = |k: &str, d: f64| bg.get(k).and_then(Value::as_f64).unwrap_or(d) as f32;
            xform::from_angles(n("sun_elevation", 45.0), n("sun_azimuth", 0.0))
        }
    }
}
