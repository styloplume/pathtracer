//! The editor's state apart from the window: the document and the scene built from it,
//! the views and their layout, the selection, and drawing all views into one image.
//! Headless uses (`--snapshot`, tests) drive it directly.

use crate::display::Display;
use crate::gizmo::Decor;
use crate::overlay::{self, LIGHT_ID, Overlays, Selection};
use crate::paint::{self, Canvas};
use crate::ptview::PtView;
use crate::raster::{Raster, Target};
use crate::render::{self, BACKGROUND, KEY, Shading, Stats};
use crate::view::{View, ViewKind, sphere};
use pt_core::document::SceneDoc;
use pt_core::geometry::Aabb;
use pt_core::lights::Light;
use pt_core::math::{Mat3, Transform, Vec3};
use pt_core::renderer::Region;
use pt_core::scene::Scene;
use pt_core::toml::{Seg, Value};
use std::fmt::Write as _;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

/// The four views, in the order the quad layout shows them (left to right, top to bottom).
pub const KINDS: [ViewKind; 4] = [ViewKind::Top, ViewKind::Front, ViewKind::Side, ViewKind::Perspective];
pub const PERSPECTIVE: usize = 3;
const BORDER: u32 = 0x18_18_1b;
const ACTIVE_BORDER: u32 = 0x5a_6a_8a;

/// A view's place in the window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub view: usize,
    pub x: usize,
    pub y: usize,
    pub w: usize,
    pub h: usize,
}

impl Rect {
    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x as i32 && y >= self.y as i32 && x < (self.x + self.w) as i32 && y < (self.y + self.h) as i32
    }
}

#[expect(clippy::struct_excessive_bools, reason = "independent view toggles")]
pub struct Workspace {
    pub doc: SceneDoc,
    /// Shared with the path-traced view's thread; changed in place through `scene_mut`.
    pub scene: Arc<Scene>,
    /// Bumped whenever `scene` changes (the path-traced view restarts).
    pub scene_gen: u64,
    pub display: Display,
    pub views: [View; 4],
    /// Four views, or the perspective view alone.
    pub quad: bool,
    /// The view filling the window in the quad layout.
    pub maximized: Option<usize>,
    /// The view under the mouse, which navigation and framing apply to.
    pub active: usize,
    pub shading: Shading,
    pub grid: bool,
    pub all_bounds: bool,
    pub sel: Selection,
    /// Objects and lights hidden in the views (the editor's own state, not the scene's).
    pub hidden: Selection,
    raster: Raster,
    targets: Vec<Target>,
    pub stats: Stats,
    pub frame_ms: f32,
    /// Leave the views' text labels out of the image and collect them in `labels` (for
    /// the editor to draw with its own font).
    pub labels_apart: bool,
    pub labels: Vec<overlay::Label>,
    /// Why the document does not load (the views show the last scene that did).
    pub load_error: Option<String>,
    /// The perspective view path traced.
    pub rendered: bool,
    pub pt: PtView,
    /// The part of the perspective view the path tracer renders (0..1 of its width and
    /// height: x0, y0, x1, y1); None for all of it.
    pub region: Option<[f32; 4]>,
    /// The perspective view looks through the scene camera; with `lock_camera` moving the
    /// view moves the camera.
    pub through_camera: bool,
    pub lock_camera: bool,
    /// Gizmos, handles and helper lines, drawn in every view.
    pub decor: Decor,
    /// The background's `rotate` as loaded, and as being dragged (shown before reloading).
    pub env_loaded: Vec3,
    pub env_preview: Option<Vec3>,
}

/// A `rotate` value of `[background]`: a number turns about +Y.
pub fn env_rotation(v: Option<&Value>) -> Vec3 {
    match v {
        Some(Value::Number(n)) => Vec3::new(0.0, *n as f32, 0.0),
        Some(v) => {
            crate::xform::read_vec(&[("r".to_owned(), v.clone())].into_iter().collect(), "r").unwrap_or(Vec3::ZERO)
        }
        None => Vec3::ZERO,
    }
}

/// A scene with nothing in it.
fn empty_scene() -> Scene {
    use pt_core::{
        camera::CameraDesc,
        material::Material,
        math::Vec3,
        scene::{Geometry, RenderSettings},
    };
    Scene::new(
        RenderSettings::default(),
        CameraDesc::default(),
        vec![],
        vec![Material::diffuse(Vec3::ONE)],
        Geometry::default(),
    )
}

impl Workspace {
    /// Opens the scene file `path`. A scene the loader rejects still opens (with nothing in
    /// the views and the error in `load_error`), so that it can be fixed in the editor.
    pub fn open(path: &Path) -> Result<Workspace, String> {
        let doc = SceneDoc::open(path)?;
        let (scene, load_error) = match doc.to_scene() {
            Ok(s) => (s, None),
            Err(e) => (empty_scene(), Some(e)),
        };
        let display = Display::build(&scene);
        let views = KINDS.map(|k| View::new(k, &scene.camera, &display.bounds));
        let sel = Selection::new(scene.provenance.objects.len(), scene.provenance.lights.len());
        let hidden = sel.clone();
        let env_loaded = env_rotation(background_rotate(&doc).as_ref());
        Ok(Workspace {
            hidden,
            doc,
            scene: Arc::new(scene),
            scene_gen: 0,
            display,
            views,
            quad: false,
            maximized: None,
            active: PERSPECTIVE,
            shading: Shading::Solid,
            grid: true,
            all_bounds: false,
            sel,
            raster: Raster::with_all_cores(),
            targets: (0..4).map(|_| Target::new(0, 0)).collect(),
            stats: Stats::default(),
            frame_ms: 0.0,
            labels_apart: false,
            labels: Vec::new(),
            load_error,
            rendered: false,
            pt: PtView::default(),
            region: None,
            through_camera: false,
            lock_camera: false,
            decor: Decor::default(),
            env_loaded,
            env_preview: None,
        })
    }

    /// Rebuilds the scene after the document changed; the views stay where they are. When
    /// the loader rejects the document, the last scene that loaded stays.
    pub fn reload(&mut self) -> Result<(), String> {
        let scene = self.doc.to_scene().inspect_err(|e| self.load_error = Some(e.clone()))?;
        self.load_error = None;
        self.display = Display::build(&scene);
        let (objects, lights) = (scene.provenance.objects.len(), scene.provenance.lights.len());
        if (objects, lights) != (self.sel.objects.len(), self.sel.lights.len()) {
            self.sel = Selection::new(objects, lights);
            self.hidden = Selection::new(objects, lights);
        }
        self.scene = Arc::new(scene);
        self.scene_gen += 1;
        self.env_loaded = env_rotation(background_rotate(&self.doc).as_ref());
        self.env_preview = None;
        Ok(())
    }

    /// The scene, to change in place: the path-traced view lets go of it first.
    pub fn scene_mut(&mut self) -> &mut Scene {
        self.pt.release();
        self.scene_gen += 1;
        if Arc::get_mut(&mut self.scene).is_none() {
            // Still shared (a render thread that did not stop in time): work on a fresh copy.
            if let Ok(s) = self.doc.to_scene() {
                self.scene = Arc::new(s);
                self.display = Display::build(&self.scene);
            }
        }
        Arc::get_mut(&mut self.scene).expect("the render thread released the scene")
    }

    /// Moves object `o` by `delta` in the scene and the display without loading the scene
    /// again; false if that cannot be done (the caller reloads).
    pub fn transform_object(&mut self, o: usize, delta: &Transform) -> bool {
        if !self.scene_mut().transform_object(o, delta) {
            return false;
        }
        self.display.transform_object(o, delta);
        true
    }

    /// Replaces light entry `j` with the one `table` describes, without reloading.
    pub fn update_light(&mut self, j: usize, table: &pt_core::toml::Table) -> bool {
        match pt_core::loader::parse_light(table, "light") {
            Ok(l) => self.scene_mut().set_light(j, l),
            Err(_) => false,
        }
    }

    /// Where each shown view goes in a `w`x`h` window.
    pub fn rects(&self, w: usize, h: usize) -> Vec<Rect> {
        let single = |view| vec![Rect { view, x: 0, y: 0, w, h }];
        if !self.quad {
            return single(PERSPECTIVE);
        }
        if let Some(v) = self.maximized {
            return single(v);
        }
        // One-pixel separators between the four views.
        let (w0, h0) = (w.saturating_sub(1) / 2, h.saturating_sub(1) / 2);
        let (w1, h1) = (w.saturating_sub(w0 + 1), h.saturating_sub(h0 + 1));
        vec![
            Rect { view: 0, x: 0, y: 0, w: w0, h: h0 },
            Rect { view: 1, x: w0 + 1, y: 0, w: w1, h: h0 },
            Rect { view: 2, x: 0, y: h0 + 1, w: w0, h: h1 },
            Rect { view: 3, x: w0 + 1, y: h0 + 1, w: w1, h: h1 },
        ]
    }

    pub fn rect_at(&self, w: usize, h: usize, x: i32, y: i32) -> Option<Rect> {
        self.rects(w, h).into_iter().find(|r| r.contains(x, y))
    }

    pub fn proj(&self, r: &Rect) -> crate::view::Proj {
        self.views[r.view].proj(r.w, r.h, &self.display.bounds)
    }

    /// Draws every shown view into a `w`x`h` image.
    pub fn compose(&mut self, w: usize, h: usize) -> Vec<u32> {
        let start = Instant::now();
        let mut out = vec![BORDER; w * h];
        let rects = self.rects(w, h);
        let mut stats = Stats::default();
        self.labels.clear();
        for r in &rects {
            if r.w == 0 || r.h == 0 {
                continue;
            }
            let proj = self.proj(r);
            let view = &self.views[r.view];
            let scale = render::label_scale(r.h);
            let traced = self.rendered && view.kind == ViewKind::Perspective;
            let mut o = Overlays::default();
            if self.grid && !traced {
                overlay::grid(&mut o, &proj, view.kind, view_target(&proj, view.kind));
            }
            if !(self.through_camera && view.kind == ViewKind::Perspective) {
                overlay::camera(&mut o, &proj, &self.scene, &self.display);
            }
            overlay::lights(&mut o, &proj, &self.scene, &self.display, &self.sel, &self.hidden, scale);
            overlay::bounds(&mut o, &self.display, &self.sel, &self.hidden, self.all_bounds);
            overlay::axis_indicator(&mut o, &proj, scale);
            crate::gizmo::draw_decor(&mut o, &self.decor, &proj);
            let mut label = view.kind.name().to_owned();
            if !view.is_fixed() {
                label += if view.ortho { " ORTHO" } else { "" };
                if self.through_camera {
                    label = "CAMERA".into();
                    label += if self.lock_camera { " (LOCKED)" } else { "" };
                }
                if traced {
                    let (spp, done) = self.pt.progress();
                    let _ = write!(label, ", RENDERED {spp} SPP{}", if done { " DONE" } else { "" });
                }
            }
            let lw = paint::text_width(&label, scale);
            o.labels.push(overlay::Label { x: r.w as i32 - lw - 10, y: 8, text: label, color: 0xc8_c8_d0 });
            if self.labels_apart {
                // In image coordinates, at the centre of where the built-in font would put them.
                let ch = paint::CELL.1 as i32 * scale;
                for l in o.labels.drain(..) {
                    let w = paint::text_width(&l.text, scale);
                    let (cx, cy) = (r.x as i32 + l.x + w / 2, r.y as i32 + l.y + (ch - scale) / 2);
                    self.labels.push(overlay::Label { x: cx, y: cy, ..l });
                }
            }
            let (sel, hidden) = (&self.sel, &self.hidden);
            let target = &mut self.targets[r.view];
            target.resize(r.w, r.h);
            // A render region: the raster view outside it.
            let outside = match (traced, self.region) {
                (true, Some(_)) => {
                    let none = Overlays::default();
                    render::render(&mut self.raster, target, &self.display, &proj, self.shading, sel, hidden, &none);
                    Some(target.color.clone())
                }
                _ => None,
            };
            let shading = if traced { Shading::Rendered } else { self.shading };
            let s = render::render(&mut self.raster, target, &self.display, &proj, shading, sel, hidden, &o);
            stats.triangles += s.triangles;
            stats.boxes += s.boxes;
            if traced {
                let region = self.region_px(r.w, r.h);
                let camera = if self.through_camera { self.scene.camera.clone() } else { view.camera_desc() };
                self.pt.update(&self.scene, self.scene_gen, camera, (r.w, r.h), region);
                self.pt.refresh();
                let target = &mut self.targets[r.view];
                composite(target, &self.pt, region, outside.as_deref());
            } else if self.shading == Shading::Shaded {
                let rot = self.env_preview.filter(|&p| p != self.env_loaded).map(|p| {
                    let loaded = Mat3::rotate_xyz(self.env_loaded);
                    loaded.mul(&Mat3::rotate_xyz(p).transpose())
                });
                backdrop(&mut self.targets[r.view], &proj, &self.scene, rot);
            }
            let target = &self.targets[r.view];
            for y in 0..r.h {
                let row = (r.y + y) * w + r.x;
                out[row..row + r.w].copy_from_slice(&target.color[y * r.w..(y + 1) * r.w]);
            }
        }
        if rects.len() > 1
            && let Some(r) = rects.iter().find(|r| r.view == self.active)
        {
            let mut c = Canvas { px: &mut out, w, h };
            c.rect(r.x as i32 - 1, r.y as i32 - 1, (r.x + r.w) as i32, (r.y + r.h) as i32, ACTIVE_BORDER);
        }
        self.stats = stats;
        self.frame_ms = start.elapsed().as_secs_f32() * 1000.0;
        out
    }

    /// What is under view-local pixel (`x`, `y`): a light icon within reach, else the
    /// object the BVH finds along the view ray (the tracer's own intersection code).
    pub fn pick(&self, r: &Rect, x: f32, y: f32) -> Option<u32> {
        let proj = self.proj(r);
        let reach = 9.0 * render::label_scale(r.h) as f32;
        let mut best: Option<(f32, u32)> = None;
        for (j, i) in self.scene.provenance.lights.clone().enumerate() {
            if self.hidden.lights.get(j).copied().unwrap_or(false) {
                continue;
            }
            let Some(a) = overlay::light_anchor(&self.scene.lights()[i as usize], &self.display) else { continue };
            if let Some((sx, sy, _)) = proj.to_screen(a) {
                let d = (sx - x).hypot(sy - y);
                if d <= reach && best.is_none_or(|b| d < b.0) {
                    best = Some((d, LIGHT_ID | j as u32));
                }
            }
        }
        if let Some((_, id)) = best {
            return Some(id);
        }
        // Through hidden objects: cast again from just past each.
        let mut ray = proj.ray(x, y);
        for _ in 0..64 {
            let hit = self.scene.intersect(&ray, f32::INFINITY)?;
            let id = self.scene.object_of(hit.prim, hit.inst).map(|o| o as u32 + 1);
            if !id.is_some_and(|id| self.hidden.has_id(id)) {
                return id;
            }
            ray.o += ray.d * (hit.t * 1.0001 + 1e-4);
        }
        None
    }

    /// Everything drawn inside a view-local rectangle (as last drawn): visible objects and
    /// the light icons there.
    pub fn ids_in(&self, r: &Rect, x0: i32, y0: i32, x1: i32, y1: i32) -> Vec<u32> {
        let t = &self.targets[r.view];
        let (xa, xb) = (x0.min(x1).max(0) as usize, (x0.max(x1).max(0) as usize).min(t.w.saturating_sub(1)));
        let (ya, yb) = (y0.min(y1).max(0) as usize, (y0.max(y1).max(0) as usize).min(t.h.saturating_sub(1)));
        let mut ids = std::collections::BTreeSet::new();
        if t.w > 0 && t.h > 0 {
            for y in ya..=yb {
                ids.extend(
                    t.id[y * t.w + xa..=y * t.w + xb].iter().copied().filter(|&id| id != 0 && id & LIGHT_ID == 0),
                );
            }
        }
        let proj = self.proj(r);
        for (j, i) in self.scene.provenance.lights.clone().enumerate() {
            let Some(a) = overlay::light_anchor(&self.scene.lights()[i as usize], &self.display) else { continue };
            if let Some((sx, sy, _)) = proj.to_screen(a)
                && (xa as f32..=xb as f32).contains(&sx)
                && (ya as f32..=yb as f32).contains(&sy)
            {
                ids.insert(LIGHT_ID | j as u32);
            }
        }
        ids.into_iter().collect()
    }

    /// Bounds of the selection (lights as points); None if nothing is selected.
    pub fn selection_bounds(&self) -> Option<Aabb> {
        let mut b = Aabb::EMPTY;
        for (i, &s) in self.sel.objects.iter().enumerate() {
            if s {
                b.grow(&self.display.object_bounds[i]);
            }
        }
        for (j, i) in self.scene.provenance.lights.clone().enumerate() {
            if self.sel.lights[j]
                && let Some(a) = overlay::light_anchor(&self.scene.lights()[i as usize], &self.display)
            {
                b.grow_point(a);
            }
        }
        (!b.is_empty()).then_some(b)
    }

    /// Frames the selection (or the whole scene) in view `v`.
    pub fn frame(&mut self, v: usize) {
        let b = self.selection_bounds().unwrap_or(self.display.bounds);
        let (c, r) = sphere(&b);
        // A lone light: show some surroundings.
        let r = if b.max == b.min { sphere(&self.display.bounds).1 * 0.25 } else { r };
        self.views[v].frame(c, r);
    }

    /// The label of an id (status line).
    pub fn label(&self, id: u32) -> String {
        let (items, i) = if id & LIGHT_ID != 0 {
            (&self.doc.tree.lights, (id & !LIGHT_ID) as usize)
        } else {
            (&self.doc.tree.objects, id as usize - 1)
        };
        items.get(i).map_or_else(|| "?".into(), |item| item.label.clone())
    }
}

impl Workspace {
    /// The render region in pixels of a `w`x`h` view.
    pub fn region_px(&self, w: usize, h: usize) -> Region {
        let Some([x0, y0, x1, y1]) = self.region else { return Region { x: 0, y: 0, w, h } };
        // At least one pixel, inside the view.
        let span = |a: f32, b: f32, n: usize| {
            let n = n.max(1);
            let lo = ((a.min(b).clamp(0.0, 1.0) * n as f32) as usize).min(n - 1);
            let hi = ((a.max(b).clamp(0.0, 1.0) * n as f32).round() as usize).clamp(lo + 1, n);
            (lo, hi - lo)
        };
        let ((x, w), (y, h)) = (span(x0, x1, w), span(y0, y1, h));
        Region { x, y, w, h }
    }
}

/// The background's `rotate` as written in the document.
fn background_rotate(doc: &SceneDoc) -> Option<Value> {
    doc.value(doc.tree.background?, &[Seg::from("background"), Seg::from("rotate")])
}

/// Puts the path-traced image where the key color is (inside the region); outside it,
/// the raster image `outside` (when there is a region).
fn composite(t: &mut Target, pt: &PtView, region: Region, outside: Option<&[u32]>) {
    let ready = pt.region == region && pt.image.len() == region.w * region.h;
    for y in 0..t.h {
        for x in 0..t.w {
            let i = y * t.w + x;
            if t.color[i] != KEY {
                continue;
            }
            let inside = x >= region.x && x < region.x + region.w && y >= region.y && y < region.y + region.h;
            t.color[i] = match (inside, outside) {
                (true, _) if ready => pt.image[(y - region.y) * region.w + (x - region.x)],
                (false, Some(o)) => o[i],
                _ => BACKGROUND,
            };
        }
    }
    if outside.is_some() {
        // The region's frame.
        let mut c = Canvas { px: &mut t.color, w: t.w, h: t.h };
        let (x0, y0) = (region.x as i32, region.y as i32);
        c.rect(x0, y0, x0 + region.w as i32 - 1, y0 + region.h as i32 - 1, 0xd0_a0_40);
    }
}

/// The scene's environment behind the geometry (shaded views): what the background looks
/// like and how it is turned. `rot` previews a rotation not loaded yet.
fn backdrop(t: &mut Target, proj: &crate::view::Proj, scene: &Scene, rot: Option<Mat3>) {
    let Some(env) = scene.lights().iter().find_map(|l| if let Light::Environment(e) = l { Some(e) } else { None })
    else {
        return;
    };
    if proj.ortho {
        return;
    }
    let enc = pt_core::image::Encoder::new(scene.settings.exposure, scene.settings.tonemap);
    let (w, h) = (t.w, t.h);
    let rows: Vec<(usize, &mut [u32])> = t.color.chunks_mut(w.max(1)).enumerate().collect();
    let depth = &t.depth;
    let threads = std::thread::available_parallelism().map_or(4, usize::from);
    let per = h.div_ceil(threads).max(1);
    let mut rows = rows;
    std::thread::scope(|s| {
        while !rows.is_empty() {
            let chunk: Vec<(usize, &mut [u32])> = rows.drain(..per.min(rows.len())).collect();
            let enc = &enc;
            s.spawn(move || {
                for (y, row) in chunk {
                    // Every other pixel, the gaps filled from the left (it is a blurry backdrop).
                    let mut last = 0;
                    for (x, p) in row.iter_mut().enumerate() {
                        if depth[y * w + x] != 0.0 || *p != BACKGROUND {
                            continue;
                        }
                        if x % 2 == 0 || x == 0 {
                            let d = proj.ray(x as f32 + 0.5, y as f32 + 0.5).d;
                            let d = rot.map_or(d, |m| m.mul_vec(d));
                            last = enc.encode(env.eval(d));
                        }
                        *p = last;
                    }
                }
            });
        }
    });
}

/// Where the grid centres itself: where the view's centre ray meets the grid plane, or a
/// point ahead of the eye when it meets it behind or far away.
fn view_target(proj: &crate::view::Proj, kind: ViewKind) -> Vec3 {
    let n = overlay::grid_axis(kind);
    let ahead = if proj.ortho { proj.half_h } else { proj.half_h / proj.tan_half.max(1e-6) };
    let t = if proj.fwd[n].abs() > 1e-4 { -proj.eye[n] / proj.fwd[n] } else { -1.0 };
    let t = match (t > 0.0, proj.ortho) {
        (true, true) => t,
        (true, false) => t.min(ahead * 4.0),
        (false, _) => ahead,
    };
    proj.eye + proj.fwd * t
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::overlay::SELECTED;
    use crate::render::BACKGROUND;
    use pt_core::math::Vec3;
    use std::path::PathBuf;

    fn scenes() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scenes")
    }

    /// Every example scene draws in every shading mode and both layouts, and every object
    /// of it shows somewhere.
    #[test]
    fn every_scene_draws() {
        let mut n = 0;
        for entry in std::fs::read_dir(scenes()).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_none_or(|e| e != "toml") {
                continue;
            }
            let mut ws = Workspace::open(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            for (i, b) in ws.display.object_bounds.iter().enumerate() {
                assert!(!b.is_empty(), "{}: object {i} has no geometry to show", path.display());
            }
            for quad in [false, true] {
                ws.quad = quad;
                for shading in Shading::ALL {
                    ws.shading = shading;
                    let px = ws.compose(160, 100);
                    let drawn = px.iter().filter(|&&p| p != BACKGROUND).count();
                    assert!(drawn > 160 * 100 / 20, "{} {shading:?}: almost empty", path.display());
                }
            }
            n += 1;
        }
        assert!(n >= 10);
    }

    struct Scene {
        ws: Workspace,
        path: PathBuf,
    }

    impl Drop for Scene {
        fn drop(&mut self) {
            std::fs::remove_file(&self.path).ok();
        }
    }

    /// A sphere (object 0) left of the origin, a box (object 1) right of it, and a point
    /// light above, seen from the front.
    fn test_scene(name: &str) -> Scene {
        let path = std::env::temp_dir().join(format!("ptedit-{name}-{}.toml", std::process::id()));
        std::fs::write(
            &path,
            "[camera]\nposition = [0, 0, 10]\nlook_at = [0, 0, 0]\nfov = 40\n\
             [[lights]]\ntype = \"point\"\nposition = [0, 2.5, 0]\n\
             [[objects]]\ntype = \"sphere\"\ncenter = [-2, 0, 0]\n\
             [[objects]]\ntype = \"box\"\ncenter = [2, 0, 0]\nsize = [1.5, 1.5, 1.5]\n",
        )
        .unwrap();
        Scene { ws: Workspace::open(&path).unwrap(), path }
    }

    #[test]
    fn clicks_pick_what_is_drawn() {
        let mut s = test_scene("pick");
        let ws = &mut s.ws;
        ws.grid = false;
        let (w, h) = (320, 200);
        ws.compose(w, h);
        let r = ws.rects(w, h)[0];
        let proj = ws.proj(&r);
        let at = |p: Vec3| proj.to_screen(p).unwrap();
        for (p, id) in [(Vec3::new(-2.0, 0.0, 0.0), 1), (Vec3::new(2.0, 0.0, 0.0), 2)] {
            let (x, y, _) = at(p);
            assert_eq!(ws.pick(&r, x, y), Some(id), "the BVH finds object {id}");
            assert_eq!(ws.targets[r.view].id[y as usize * w + x as usize], id, "and the rasterizer drew it there");
        }
        let (x, y, _) = at(Vec3::new(0.0, 2.5, 0.0));
        assert_eq!(ws.pick(&r, x + 3.0, y - 2.0), Some(LIGHT_ID), "light icons are picked near them");
        assert_eq!(ws.pick(&r, 2.0, 2.0), None);

        let all = ws.ids_in(&r, 0, 0, w as i32, h as i32);
        assert_eq!(all, vec![1, 2, LIGHT_ID]);
        let (x, _, _) = at(Vec3::new(0.0, 0.0, 0.0));
        assert_eq!(ws.ids_in(&r, x as i32 + 4, 0, w as i32, h as i32), vec![2], "only the box is right of centre");
    }

    #[test]
    fn selection_is_outlined_and_framed() {
        let mut s = test_scene("select");
        let ws = &mut s.ws;
        let (w, h) = (320, 200);
        let before = ws.compose(w, h);
        assert!(!before.contains(&SELECTED));
        ws.sel.set_id(2, true);
        let after = ws.compose(w, h);
        assert!(after.iter().filter(|&&p| p == SELECTED).count() > 50, "outline around the box");

        ws.frame(PERSPECTIVE);
        let r = ws.rects(w, h)[0];
        let (x, y, _) = ws.proj(&r).to_screen(Vec3::new(2.0, 0.0, 0.0)).unwrap();
        assert!((x - w as f32 / 2.0).abs() < 1.0 && (y - h as f32 / 2.0).abs() < 1.0, "framed at ({x}, {y})");
        // Framed so the box fills the view without leaving it.
        ws.compose(w, h);
        let covered = ws.targets[r.view].id.iter().filter(|&&id| id == 2).count();
        assert!(covered > w * h / 8, "{covered} pixels");
    }

    #[test]
    fn quad_layout_tiles_the_window() {
        let mut s = test_scene("quad");
        let ws = &mut s.ws;
        ws.quad = true;
        let rects = ws.rects(101, 61);
        assert_eq!(rects.len(), 4);
        let area: usize = rects.iter().map(|r| r.w * r.h).sum();
        assert_eq!(area + 101 + 61 - 1, 101 * 61, "four views and the separators cover the window");
        ws.maximized = Some(1);
        assert_eq!(ws.rects(101, 61), vec![Rect { view: 1, x: 0, y: 0, w: 101, h: 61 }]);
    }
}
