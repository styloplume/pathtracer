//! Scene editing commands: adding primitives and lights under the cursor (on the surface
//! there, else on the ground plane), deleting, duplicating, cut / copy / paste between
//! scenes (as TOML text on the clipboard, with the materials and prototypes the copies use
//! and file paths that still work from the other scene's folder), turning the selection
//! into a prototype, assigning and importing materials, and the render and output presets.
//!
//! Duplicates and pastes copy the entries' text, comments included.

use crate::editor::{Editor, Target};
use crate::files;
use crate::overlay::LIGHT_ID;
use crate::workspace::Rect as ViewRect;
use crate::xform::{self, vec_value};
use pt_core::document::{Item, Kind};
use pt_core::geometry::Aabb;
use pt_core::math::{Frame, Vec3};
use pt_core::toml::{Document, Node, NodeKind, Seg, Table, Value, format_key, format_value};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// Things the Add menu makes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Add {
    Sphere,
    Box,
    Cylinder,
    Disk,
    Quad,
    Sdf,
    Mesh,
    PointLight,
    SpotLight,
    DirectionalLight,
}

impl Add {
    pub const OBJECTS: [(Add, &'static str); 7] = [
        (Add::Sphere, "Sphere"),
        (Add::Box, "Box"),
        (Add::Cylinder, "Cylinder"),
        (Add::Disk, "Disk"),
        (Add::Quad, "Quad"),
        (Add::Sdf, "SDF (rounded box)"),
        (Add::Mesh, "Mesh File…"),
    ];
    pub const LIGHTS: [(Add, &'static str); 3] = [
        (Add::PointLight, "Point Light"),
        (Add::SpotLight, "Spot Light"),
        (Add::DirectionalLight, "Directional Light"),
    ];
}

/// Render presets: name, then keys of `[render]`.
pub const RENDER_PRESETS: &[(&str, &[(&str, f64)])] = &[
    ("Draft", &[("spp", 16.0), ("max_depth", 4.0), ("noise_threshold", 0.0)]),
    ("Preview", &[("spp", 64.0), ("max_depth", 6.0), ("noise_threshold", 0.0)]),
    ("Final", &[("spp", 1024.0), ("max_depth", 12.0), ("noise_threshold", 0.01)]),
    ("Production", &[("spp", 4096.0), ("max_depth", 16.0), ("noise_threshold", 0.005)]),
];

pub const SIZE_PRESETS: &[(&str, u32, u32)] = &[
    ("640 × 360", 640, 360),
    ("1280 × 720", 1280, 720),
    ("1920 × 1080", 1920, 1080),
    ("3840 × 2160", 3840, 2160),
    ("1024 × 1024", 1024, 1024),
    ("1080 × 1350 (portrait)", 1080, 1350),
];

/// Output presets: name, formats, denoise, AOVs.
pub const OUTPUT_PRESETS: &[(&str, &[&str], bool, &[&str])] = &[
    ("PNG", &["png"], false, &[]),
    ("PNG + HDR", &["png", "hdr"], false, &[]),
    ("PNG, denoised too", &["png"], true, &[]),
    ("Compositing (PNG, PFM, AOVs)", &["png", "pfm"], false, &["albedo", "normal", "depth"]),
];

/// A round number near `v`: 1, 2 or 5 times a power of ten.
pub fn nice(v: f32) -> f32 {
    if !(v > 0.0 && v.is_finite()) {
        return 1.0;
    }
    let p = 10f32.powf(v.log10().floor());
    let m = v / p;
    p * if m < 1.5 {
        1.0
    } else if m < 3.5 {
        2.0
    } else if m < 7.5 {
        5.0
    } else {
        10.0
    }
}

/// The string values of a table (recursively) whose text names a file: `rewrite` gets each
/// one and returns its replacement.
fn rewrite_paths(doc: &mut Document, rewrite: &dyn Fn(&str) -> Option<String>) {
    let mut found = Vec::new();
    strings(doc.root(), &mut Vec::new(), &mut found);
    for (path, s) in found {
        if let Some(new) = rewrite(&s) {
            doc.set(&path, &Value::String(new)).ok();
        }
    }
}

fn strings(n: &Node, path: &mut Vec<Seg>, out: &mut Vec<(Vec<Seg>, String)>) {
    match &n.kind {
        NodeKind::Scalar(Value::String(s)) => out.push((path.clone(), s.clone())),
        NodeKind::Scalar(_) => {}
        NodeKind::Table(t) => {
            for e in &t.entries {
                path.push(Seg::from(e.key.as_str()));
                strings(&e.node, path, out);
                path.pop();
            }
        }
        NodeKind::Array(items) | NodeKind::TableArray(items) => {
            for (i, c) in items.iter().enumerate() {
                path.push(Seg::Index(i));
                strings(c, path, out);
                path.pop();
            }
        }
    }
}

/// Paths in `block` made absolute (relative ones that name a file beside `dir`).
fn absolutize(block: &str, dir: &Path) -> String {
    let Ok(mut d) = Document::parse(block) else { return block.to_owned() };
    rewrite_paths(&mut d, &|s| {
        let p = Path::new(s);
        let full = dir.join(p);
        (!s.is_empty() && p.is_relative() && full.is_file())
            .then(|| pt_core::loader::canonical(&full).display().to_string().trim_start_matches(r"\\?\").to_owned())
    });
    d.text().to_owned()
}

/// Absolute paths in `block` that exist made relative to `dir`.
fn relativize(block: &str, dir: &Path) -> String {
    let Ok(mut d) = Document::parse(block) else { return block.to_owned() };
    rewrite_paths(&mut d, &|s| {
        let p = Path::new(s);
        (p.is_absolute() && p.is_file()).then(|| files::relative(p, dir))
    });
    d.text().to_owned()
}

/// `header` and the table's keys, `type` first, one per line.
fn table_block(header: &str, t: &Table) -> String {
    let mut items: Vec<(&String, &Value)> = t.iter().filter(|(k, _)| !k.starts_with('$')).collect();
    items.sort_by_key(|(k, _)| *k != "type");
    let mut s = format!("{header}\n");
    for (k, v) in items {
        let _ = writeln!(s, "{} = {}", format_key(k), format_value(v));
    }
    s
}

fn dir_of(p: &Path) -> PathBuf {
    p.parent().filter(|d| !d.as_os_str().is_empty()).map_or_else(|| PathBuf::from("."), Path::to_path_buf)
}

impl Editor {
    /// The text of entry `item` as sections (`[[objects]]` and what belongs to it).
    fn item_block(&self, item: &Item) -> Option<String> {
        let doc = &self.ws.doc.files[item.file].doc;
        if let Some(b) = doc.block_text(&item.path) {
            return Some(b);
        }
        let t = self.ws.doc.item_table(item)?;
        let header = match item.kind {
            Kind::Light => "[[lights]]".to_owned(),
            Kind::Object => "[[objects]]".to_owned(),
            Kind::Material => format!("[materials.{}]", format_key(&item.label)),
            Kind::Medium => format!("[media.{}]", format_key(&item.label)),
            Kind::Prototype => format!("[prototypes.{}]", format_key(&item.label)),
        };
        Some(table_block(&header, &t))
    }

    fn file_dir(&self, file: usize) -> PathBuf {
        dir_of(&self.ws.doc.files[file].path)
    }

    /// Where something added at view-local (`x`, `y`) of view `r` goes: the surface there
    /// (with its normal), else the ground plane, else a point ahead; and a size that looks
    /// right at that distance.
    pub fn place_at(&self, r: &ViewRect, x: f32, y: f32) -> (Vec3, Vec3, f32) {
        let proj = self.ws.proj(r);
        let up = Vec3::new(0.0, 1.0, 0.0);
        let hit = |x: f32, y: f32| {
            let ray = proj.ray(x, y);
            self.ws.scene.intersect(&ray, f32::INFINITY).map(|h| ray.o + ray.d * h.t)
        };
        let ray = proj.ray(x, y);
        let (p, n) = if let Some(p) = hit(x, y) {
            // The surface's normal from two neighbouring rays, facing the viewer.
            let n = match (hit(x + 2.0, y), hit(x, y + 2.0)) {
                (Some(a), Some(b)) => (a - p).cross(b - p).normalize(),
                _ => up,
            };
            let n = if n.is_finite() { n } else { up };
            (p, if n.dot(ray.d) > 0.0 { -n } else { n })
        } else if let Some(g) = crate::gizmo::plane_hit(Vec3::ZERO, up, ray.o, ray.d).filter(|g| {
            (*g - ray.o).dot(ray.d) > 0.0
                && (*g - ray.o).length() < 50.0 * crate::view::sphere(&self.ws.display.bounds).1.max(1.0)
        }) {
            (g, up)
        } else {
            let d = if proj.ortho { proj.half_h } else { proj.half_h / proj.tan_half.max(1e-6) };
            (ray.o + ray.d * d.max(1.0), up)
        };
        let size = nice(proj.pixel_size(p) * 60.0 * self.ui.scale.max(1.0));
        (p, n, size)
    }

    /// The point under the mouse (or the middle of the active view) for adding things.
    pub fn add_point(&self) -> (Vec3, Vec3, f32) {
        let (vw, vh) = (self.vp.w.max(1) as usize, self.vp.h.max(1) as usize);
        let (mx, my) = self.add_at.unwrap_or((self.vp.x + self.vp.w / 2, self.vp.y + self.vp.h / 2));
        let r = self.ws.rect_at(vw, vh, mx - self.vp.x, my - self.vp.y).unwrap_or(crate::workspace::Rect {
            view: self.ws.active,
            x: 0,
            y: 0,
            w: vw,
            h: vh,
        });
        let (lx, ly) = ((mx - self.vp.x - r.x as i32) as f32 + 0.5, (my - self.vp.y - r.y as i32) as f32 + 0.5);
        self.place_at(&r, lx, ly)
    }

    /// Adds `what` at the add point; Mesh asks for a file first.
    pub fn add(&mut self, what: Add) {
        let (p, n, s) = self.add_point();
        let v = vec_value;
        let num = |x: f32| Value::Number(xform::tidy(x));
        let str = |x: &str| Value::String(x.into());
        let entries: Vec<(&str, Value)> = match what {
            Add::Sphere => vec![("type", str("sphere")), ("center", v(p + n * (s * 0.5))), ("radius", num(s * 0.5))],
            Add::Box => vec![("type", str("box")), ("center", v(p + n * (s * 0.5))), ("size", num(s))],
            Add::Cylinder => {
                let mut e =
                    vec![("type", str("cylinder")), ("base", v(p)), ("radius", num(s * 0.4)), ("height", num(s))];
                if (n - Vec3::new(0.0, 1.0, 0.0)).length() > 1e-3 {
                    e.push(("axis", v(n)));
                }
                e
            }
            Add::Disk => {
                let mut e = vec![("type", str("disk")), ("center", v(p + n * (s * 1e-3))), ("radius", num(s * 0.5))];
                if (n - Vec3::new(0.0, 1.0, 0.0)).length() > 1e-3 {
                    e.push(("normal", v(n)));
                }
                e
            }
            Add::Quad => {
                let f = Frame::from_normal(n);
                let (mut u, mut w) = (f.t * s, f.b * s);
                if u.cross(w).dot(n) < 0.0 {
                    std::mem::swap(&mut u, &mut w);
                }
                let origin = p + n * (s * 1e-3) - (u + w) * 0.5;
                vec![("type", str("quad")), ("origin", v(origin)), ("u", v(u)), ("v", v(w))]
            }
            Add::Sdf => {
                let mut sdf = Table::new();
                sdf.insert("type".into(), str("box"));
                sdf.insert("size".into(), num(s));
                sdf.insert("round".into(), num(s * 0.1));
                vec![("type", str("sdf")), ("sdf", Value::Table(sdf)), ("translate", v(p + n * (s * 0.5)))]
            }
            Add::Mesh => {
                self.add_mesh_at = Some(p);
                let dir = self.file_dir(0);
                let d = files::FileDialog::new(files::Mode::Pick, "Add a Mesh", &dir, crate::inspector::MESHES);
                self.open_dialog(d);
                return;
            }
            Add::PointLight => {
                let at = p + n * (s * 2.0);
                vec![("type", str("point")), ("position", v(at)), ("intensity", num(10.0 * (s * 2.0).powi(2)))]
            }
            Add::SpotLight => {
                let at = p + n * (s * 3.0);
                vec![
                    ("type", str("spot")),
                    ("position", v(at)),
                    ("look_at", v(p)),
                    ("angle", num(30.0)),
                    ("intensity", num(20.0 * (s * 3.0).powi(2))),
                ]
            }
            Add::DirectionalLight => {
                vec![
                    ("type", str("directional")),
                    ("elevation", num(45.0)),
                    ("azimuth", num(30.0)),
                    ("intensity", num(3.0)),
                ]
            }
        };
        let kind = if matches!(what, Add::PointLight | Add::SpotLight | Add::DirectionalLight) {
            Kind::Light
        } else {
            Kind::Object
        };
        let entries: Vec<(String, Value)> = entries.into_iter().map(|(k, v)| (k.to_owned(), v)).collect();
        self.push_entry(kind, &entries);
    }

    /// Adds a mesh object for `file` (picked in the browser) where the add point was.
    pub fn add_mesh(&mut self, file: &Path, at: Vec3) {
        let rel = files::relative(file, &self.file_dir(0));
        let entries = vec![("file".to_owned(), Value::String(rel)), ("translate".to_owned(), vec_value(at))];
        self.push_entry(Kind::Object, &entries);
    }

    /// Places an instance of prototype `name` at the add point.
    pub fn add_instance(&mut self, name: &str) {
        let (p, _, _) = self.add_point();
        let entries = vec![
            ("type".to_owned(), Value::String("instance".into())),
            ("prototype".to_owned(), Value::String(name.into())),
            ("translate".to_owned(), vec_value(p)),
        ];
        self.push_entry(Kind::Object, &entries);
    }

    /// Appends an entry to the scene file and selects it.
    fn push_entry(&mut self, kind: Kind, entries: &[(String, Value)]) {
        let list = if kind == Kind::Light { "lights" } else { "objects" };
        let ty = entries.iter().find(|(k, _)| k == "type").and_then(|(_, v)| v.as_str().map(str::to_owned));
        match self.ws.doc.push_table(0, &[Seg::from(list)], entries) {
            Ok(_) => {
                let n = self.list(kind).len();
                let id = if kind == Kind::Light { LIGHT_ID | (n as u32 - 1) } else { n as u32 };
                self.pending_select = Some(vec![id]);
                self.inspect = Some(Target::Item(kind, n - 1));
                self.scene_stale = true;
                self.notify(format!("added {}", ty.unwrap_or_else(|| "a mesh".into())));
            }
            Err(e) => self.notify(format!("error: {e}")),
        }
    }

    /// Deletes the selected objects and lights (one undo step).
    pub fn delete_selection(&mut self) {
        let mut items: Vec<Item> = self.selected().iter().map(|&(k, i)| self.list(k)[i].clone()).collect();
        if items.is_empty() {
            if let Some(Target::Item(kind, i)) = self.inspect {
                self.delete_item(kind, i);
            }
            return;
        }
        // Back to front within each file, so the indices still hold.
        items.sort_by(|a, b| (b.file, list_name(b), index(b)).cmp(&(a.file, list_name(a), index(a))));
        let n = items.len();
        let r = self.ws.doc.group(&format!("delete {n} items"), |doc| {
            for it in &items {
                doc.remove(it.file, &it.path)?;
            }
            Ok(())
        });
        match r {
            Ok(()) => {
                self.ws.sel.clear();
                self.inspect = None;
                self.scene_stale = true;
                self.notify(format!("deleted {n}"));
            }
            Err(e) => self.notify(format!("error: {e}")),
        }
    }

    /// Deletes a material, medium or prototype nothing uses (or any light or object).
    pub fn delete_item(&mut self, kind: Kind, i: usize) {
        let Some(item) = self.list(kind).get(i).cloned() else { return };
        if matches!(kind, Kind::Material | Kind::Medium | Kind::Prototype) {
            let keys: &[&str] = match kind {
                Kind::Material => &["material"],
                Kind::Medium => &["interior", "medium", "exterior"],
                _ => &["prototype"],
            };
            let uses: usize = self
                .ws
                .doc
                .files
                .iter()
                .map(|f| {
                    let mut out = Vec::new();
                    strings(f.doc.root(), &mut Vec::new(), &mut out);
                    out.iter()
                        .filter(|(p, s)| {
                            *s == item.label && matches!(p.last(), Some(Seg::Key(k)) if keys.contains(&k.as_str()))
                        })
                        .count()
                })
                .sum();
            if uses > 0 {
                self.notify(format!("{} is used {uses} times: assign something else first", item.label));
                return;
            }
        }
        match self.ws.doc.remove(item.file, &item.path) {
            Ok(_) => {
                self.inspect = None;
                self.ws.sel.clear();
                self.scene_stale = true;
                self.notify(format!("deleted {}", item.label));
            }
            Err(e) => self.notify(format!("error: {e}")),
        }
    }

    /// Copies the selected objects and lights next to themselves (in their own files) and
    /// selects the copies.
    pub fn duplicate(&mut self) {
        let sel = self.selected();
        if sel.is_empty() {
            self.notify("nothing selected to duplicate");
            return;
        }
        let before: Vec<(Kind, Vec<Item>)> =
            [Kind::Object, Kind::Light].iter().map(|&k| (k, self.list(k).to_vec())).collect();
        let blocks: Vec<(usize, &'static str, String)> = sel
            .iter()
            .filter_map(|&(k, i)| {
                let it = &self.list(k)[i];
                let list = if k == Kind::Light { "lights" } else { "objects" };
                Some((it.file, list, self.item_block(it)?))
            })
            .collect();
        let r = self.ws.doc.group(&format!("duplicate {} items", blocks.len()), |doc| {
            for (file, list, b) in &blocks {
                doc.insert_block(*file, &[Seg::from(*list)], b)?;
            }
            Ok(())
        });
        match r {
            Ok(()) => {
                self.select_new(&before);
                self.scene_stale = true;
                self.notify(format!("duplicated {}", blocks.len()));
            }
            Err(e) => self.notify(format!("error: {e}")),
        }
    }

    /// Selects the objects and lights that are not in `before` (by file and position).
    fn select_new(&mut self, before: &[(Kind, Vec<Item>)]) {
        let mut ids = Vec::new();
        for (kind, old) in before {
            let count = |f: usize| old.iter().filter(|it| it.file == f).count();
            for (i, it) in self.list(*kind).iter().enumerate() {
                if index(it) >= count(it.file) {
                    ids.push(if *kind == Kind::Light { LIGHT_ID | i as u32 } else { i as u32 + 1 });
                }
            }
        }
        self.pending_select = Some(ids);
    }

    /// The selection as clipboard text: its entries, and the materials and prototypes they
    /// name, with file paths made absolute.
    pub fn copy_text(&self) -> Option<String> {
        let sel = self.selected();
        if sel.is_empty() {
            return None;
        }
        let mut parts = vec!["# ptedit clipboard".to_owned()];
        let mut defs: Vec<Item> = Vec::new();
        for (k, i) in sel {
            let it = &self.list(k)[i];
            let Some(block) = self.item_block(it) else { continue };
            parts.push(absolutize(&block, &self.file_dir(it.file)));
            let Some(t) = self.ws.doc.item_table(it) else { continue };
            for (key, list) in [("material", Kind::Material), ("prototype", Kind::Prototype)] {
                if let Some(Value::String(name)) = t.get(key)
                    && let Some(d) = self.list(list).iter().find(|m| m.label == *name)
                    && !defs.contains(d)
                {
                    defs.push(d.clone());
                }
            }
        }
        for d in &defs {
            if let Some(block) = self.item_block(d) {
                parts.push(absolutize(&block, &self.file_dir(d.file)));
            }
        }
        Some(parts.join("\n\n") + "\n")
    }

    pub fn copy(&mut self) {
        match self.copy_text() {
            Some(t) => {
                let n = self.ws.sel.count();
                self.clip = Some(t.clone());
                self.ui.copy = Some(t);
                self.notify(format!("copied {n}"));
            }
            None => self.notify("nothing selected to copy"),
        }
    }

    pub fn cut(&mut self) {
        if let Some(t) = self.copy_text() {
            self.clip = Some(t.clone());
            self.ui.copy = Some(t);
            self.delete_selection();
        }
    }

    /// Pastes clipboard text: its objects and lights into the scene file, and the
    /// materials, media and prototypes it defines that the scene does not have.
    pub fn paste(&mut self, text: &str) {
        let Ok(clip) = Document::parse(text) else {
            self.notify("the clipboard holds no scene entries");
            return;
        };
        let dir = self.file_dir(0);
        let mut blocks: Vec<(&'static str, String)> = Vec::new();
        let NodeKind::Table(root) = &clip.root().kind else { return };
        for e in &root.entries {
            match (e.key.as_str(), &e.node.kind) {
                ("objects" | "lights", NodeKind::TableArray(items)) => {
                    let list = if e.key == "lights" { "lights" } else { "objects" };
                    for i in 0..items.len() {
                        if let Some(b) = clip.block_text(&[Seg::from(list), Seg::Index(i)]) {
                            blocks.push((list, relativize(&b, &dir)));
                        }
                    }
                }
                (k @ ("materials" | "media" | "prototypes"), NodeKind::Table(defs)) => {
                    let (kind, list) = match k {
                        "materials" => (Kind::Material, "materials"),
                        "media" => (Kind::Medium, "media"),
                        _ => (Kind::Prototype, "prototypes"),
                    };
                    for d in &defs.entries {
                        if self.list(kind).iter().any(|it| it.label == d.key) {
                            continue;
                        }
                        if let Some(b) = clip.block_text(&[Seg::from(list), Seg::from(d.key.as_str())]) {
                            blocks.push((list, relativize(&b, &dir)));
                        }
                    }
                }
                _ => {}
            }
        }
        if blocks.is_empty() {
            self.notify("the clipboard holds no scene entries");
            return;
        }
        let before: Vec<(Kind, Vec<Item>)> =
            [Kind::Object, Kind::Light].iter().map(|&k| (k, self.list(k).to_vec())).collect();
        let n = blocks.len();
        // Definitions first, so the objects that use them load.
        blocks.sort_by_key(|(l, _)| matches!(*l, "objects" | "lights"));
        let r = self.ws.doc.group("paste", |doc| {
            for (list, b) in &blocks {
                doc.insert_block(0, &[Seg::from(*list)], b)?;
            }
            Ok(())
        });
        match r {
            Ok(()) => {
                self.select_new(&before);
                self.scene_stale = true;
                self.notify(format!("pasted {n} entries"));
            }
            Err(e) => self.notify(format!("error: {e}")),
        }
    }

    /// Turns the selected objects into a prototype placed by one instance where they were
    /// (the parts are moved so that the prototype's origin is under their middle).
    pub fn make_prototype(&mut self) {
        let objs: Vec<usize> = self.selected().iter().filter(|(k, _)| *k == Kind::Object).map(|&(_, i)| i).collect();
        if objs.is_empty() {
            self.notify("select the objects that make the prototype");
            return;
        }
        let mut b = Aabb::EMPTY;
        let mut parts = Vec::new();
        for &i in &objs {
            let it = self.list(Kind::Object)[i].clone();
            let Some(t) = self.ws.doc.item_table(&it) else { continue };
            if t.contains_key("motion") || xform::object_type(&t) == "instance" {
                self.notify(format!("{}: prototypes cannot hold moving objects or instances", it.label));
                return;
            }
            b.grow(&self.ws.display.object_bounds[i]);
            parts.push((it, t));
        }
        let c = b.centroid();
        let origin = Vec3::new(c.x, if b.is_empty() { 0.0 } else { b.min.y }, c.z);
        let names: Vec<String> = self.list(Kind::Prototype).iter().map(|p| p.label.clone()).collect();
        // One of these is free: there are fewer names taken.
        let name = (1..=names.len() + 1)
            .map(|k| if k == 1 { "prototype".to_owned() } else { format!("prototype_{k}") })
            .find(|n| !names.contains(n))
            .unwrap_or_else(|| "prototype".into());
        // Each part, moved so the origin is at `origin`, under the prototype's header.
        let header = format!("prototypes.{name}.objects");
        let mut text = format!("[prototypes.{name}]\n");
        for (it, t) in &parts {
            let Some(block) = self.item_block(it) else { continue };
            let mut d = match Document::parse(block.as_str()) {
                Ok(d) => d,
                Err(e) => {
                    self.notify(format!("error: {e}"));
                    return;
                }
            };
            let tr = xform::read_vec(t, "translate").unwrap_or(Vec3::ZERO) - origin;
            let path = [Seg::from("objects"), Seg::Index(0), Seg::from("translate")];
            if let Err(e) = d.set(&path, &vec_value(tr)) {
                self.notify(format!("error: {e}"));
                return;
            }
            let moved: Vec<String> = absolutize(d.text(), &self.file_dir(it.file))
                .lines()
                .map(|l| {
                    let s = l.trim_start();
                    if let Some(rest) = s.strip_prefix("[[objects]]") {
                        format!("[[{header}]]{rest}")
                    } else if let Some(rest) = s.strip_prefix("[objects.") {
                        format!("[{header}.{rest}")
                    } else {
                        l.to_owned()
                    }
                })
                .collect();
            text += "\n";
            text += &relativize(&(moved.join("\n") + "\n"), &self.file_dir(0));
        }
        let mut items: Vec<Item> = parts.iter().map(|(it, _)| it.clone()).collect();
        items.sort_by_key(|it| std::cmp::Reverse((it.file, index(it))));
        let instance = vec![
            ("type".to_owned(), Value::String("instance".into())),
            ("prototype".to_owned(), Value::String(name.clone())),
            ("translate".to_owned(), vec_value(origin)),
        ];
        let r = self.ws.doc.group(&format!("make prototype {name}"), |doc| {
            doc.insert_block(0, &[Seg::from("prototypes")], &text)?;
            for it in &items {
                doc.remove(it.file, &it.path)?;
            }
            doc.push_table(0, &[Seg::from("objects")], &instance)?;
            Ok(())
        });
        match r {
            Ok(()) => {
                self.scene_stale = true;
                let n = self.list(Kind::Object).len();
                self.pending_select = Some(vec![n as u32]);
                if let Some(p) = self.list(Kind::Prototype).iter().position(|p| p.label == name) {
                    self.inspect = Some(Target::Item(Kind::Prototype, p));
                    self.renaming = Some(Target::Item(Kind::Prototype, p));
                    self.rename_focused = false;
                }
                self.notify(format!("made prototype {name} of {} objects: type its name", items.len()));
            }
            Err(e) => self.notify(format!("error: {e}")),
        }
    }

    /// Sets object `i`'s material to `name`.
    pub fn assign_material_name(&mut self, i: usize, name: &str) {
        let Some(obj) = self.list(Kind::Object).get(i).cloned() else { return };
        if self.ws.doc.item_table(&obj).is_some_and(|t| xform::object_type(&t) == "instance") {
            self.notify("instances take their materials from their prototype");
            return;
        }
        let mut path = obj.path.clone();
        path.push(Seg::from("material"));
        self.apply(obj.file, &path, &Value::String(name.to_owned()), None, true);
        self.notify(format!("{} now uses {name}", obj.label));
    }

    /// Copies material `name` of library file `lib` into the scene file (unless the scene
    /// defines it already); returns whether it is there now.
    pub fn import_material(&mut self, lib: &Path, name: &str) -> bool {
        if self.list(Kind::Material).iter().any(|m| m.label == name) {
            return true;
        }
        let block = std::fs::read_to_string(lib)
            .ok()
            .and_then(|t| Document::parse(t).ok())
            .and_then(|d| d.block_text(&[Seg::from("materials"), Seg::from(name)]));
        let Some(block) = block else {
            self.notify(format!("{name} is not in {}", lib.display()));
            return false;
        };
        // Its comments in the library describe the group, not the material.
        let block: String =
            block.lines().skip_while(|l| l.trim_start().starts_with('#')).collect::<Vec<_>>().join("\n");
        let block = relativize(&absolutize(&block, &dir_of(lib)), &self.file_dir(0));
        match self.ws.doc.insert_block(0, &[Seg::from("materials")], &block) {
            Ok(_) => {
                self.scene_stale = true;
                true
            }
            Err(e) => {
                self.notify(format!("error: {e}"));
                false
            }
        }
    }

    /// Writes render preset `i` into `[render]` (one undo step).
    pub fn render_preset(&mut self, i: usize) {
        let (name, keys) = RENDER_PRESETS[i];
        let file = self.setting_file("render");
        let values: Vec<(String, Value)> = keys.iter().map(|(k, v)| ((*k).to_owned(), Value::Number(*v))).collect();
        self.write_section(file, "render", &values, &format!("render preset {name}"));
    }

    pub fn size_preset(&mut self, i: usize) {
        let (name, w, h) = SIZE_PRESETS[i];
        let file = self.setting_file("render");
        let values =
            vec![("width".to_owned(), Value::Number(f64::from(w))), ("height".to_owned(), Value::Number(f64::from(h)))];
        self.write_section(file, "render", &values, &format!("image size {name}"));
    }

    pub fn output_preset(&mut self, i: usize) {
        let (name, formats, denoise, aovs) = OUTPUT_PRESETS[i];
        let file = self.setting_file("output");
        let list = |v: &[&str]| Value::Array(v.iter().map(|s| Value::String((*s).into())).collect());
        let mut values = vec![("formats".to_owned(), list(formats)), ("denoise".to_owned(), Value::Bool(denoise))];
        values.push(("aovs".to_owned(), list(aovs)));
        self.write_section(file, "output", &values, &format!("output preset {name}"));
    }

    /// The file whose `[section]` the scene's settings come from (the scene file if none).
    fn setting_file(&self, section: &str) -> usize {
        let t = &self.ws.doc.tree;
        t.settings.iter().rev().find(|s| s.section == section).map_or(0, |s| s.file)
    }

    fn write_section(&mut self, file: usize, section: &str, values: &[(String, Value)], label: &str) {
        let r = self.ws.doc.group(label, |doc| {
            // A new section starts with the first key; the others follow in order.
            let mut rest = values;
            if doc.value(file, &[Seg::from(section)]).is_none()
                && let Some((first, more)) = values.split_first()
            {
                doc.set(file, &[Seg::from(section)], &Value::Table(std::iter::once(first.clone()).collect()))?;
                rest = more;
            }
            for (k, v) in rest {
                let path = [Seg::from(section), Seg::from(k.as_str())];
                let is_empty_list = matches!(v, Value::Array(a) if a.is_empty());
                if is_empty_list {
                    if doc.value(file, &path).is_some() {
                        doc.remove(file, &path)?;
                    }
                } else {
                    doc.set(file, &path, v)?;
                }
            }
            Ok(())
        });
        match r {
            Ok(()) => {
                self.scene_stale = true;
                self.notify(label.to_owned());
            }
            Err(e) => self.notify(format!("error: {e}")),
        }
    }

    /// The object under view-local (`x`, `y`) of view `r` (for drops).
    pub fn object_at(&self, r: &ViewRect, x: f32, y: f32) -> Option<usize> {
        let id = self.ws.pick(r, x, y)?;
        (id & LIGHT_ID == 0).then(|| id as usize - 1)
    }
}

/// An entry's position in its file's list.
fn index(it: &Item) -> usize {
    match it.path.get(1) {
        Some(Seg::Index(i)) => *i,
        _ => 0,
    }
}

/// The list an entry is in (`objects`, `lights`).
fn list_name(it: &Item) -> &str {
    match it.path.first() {
        Some(Seg::Key(k)) => k,
        _ => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nice_numbers() {
        assert_eq!(nice(0.013), 0.01);
        assert_eq!(nice(0.3), 0.2);
        assert_eq!(nice(4.0), 5.0);
        assert_eq!(nice(80.0), 100.0);
    }

    #[test]
    fn paths_travel_between_folders() {
        let d = std::env::temp_dir().join(format!("ptedit-paths-{}", std::process::id()));
        std::fs::create_dir_all(d.join("a/models")).unwrap();
        std::fs::create_dir_all(d.join("b")).unwrap();
        std::fs::write(d.join("a/models/m.obj"), "").unwrap();
        let block = "[[objects]]\nfile = \"models/m.obj\"  # the mesh\n";
        let abs = absolutize(block, &d.join("a"));
        assert!(!abs.contains("\"models/m.obj\"") && abs.contains("# the mesh"), "{abs}");
        let rel = relativize(&abs, &d.join("b"));
        assert!(rel.contains("file = \"../a/models/m.obj\"  # the mesh"), "{rel}");
        std::fs::remove_dir_all(&d).ok();
    }
}
