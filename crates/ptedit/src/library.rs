//! The material library: the scene's materials and those of library files (the `lib`
//! folders near the scene, such as `scenes/lib/materials.toml`), each with a small
//! path-traced preview sphere. Materials are dragged onto objects in the viewport or the
//! outliner to assign them; a library material is copied into the scene file first.
//!
//! Previews (and image thumbnails for texture pickers and the file browser) are made on a
//! worker thread and cached; the panels ask for them every frame and show them once there.

use crate::editor::{Editor, Target};
use crate::ui::{Id, Rect, id_of};
use pt_core::document::Kind;
use pt_core::renderer::Progress;
use pt_core::toml::{Document, NodeKind, Table, Value, format_key, format_value};
use std::collections::HashMap;
use std::fmt::Write as _;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

const ID: Id = 0x6c69_6272_6172_7900;
/// Drag payloads of library materials: this tag in the high bits, the entry below.
pub const LIB_TAG: u64 = 0x100;
const MATERIAL_TAG: u64 = Kind::Material as u64 + 1;

enum Job {
    /// A preview scene's text, the folder its paths are relative to, and the image size.
    Material(String, PathBuf, usize),
    Image(PathBuf, usize),
}

#[derive(Default)]
struct Shared {
    queue: Mutex<Vec<(u64, Job)>>,
    done: Mutex<Vec<(u64, Option<Thumb>)>>,
    active: AtomicUsize,
    quit: AtomicBool,
}

/// A small image (0x00RRGGBB).
#[derive(Clone, Debug)]
pub struct Thumb {
    pub w: usize,
    pub h: usize,
    pub px: Vec<u32>,
}

#[derive(Default)]
pub struct Library {
    shared: Arc<Shared>,
    worker: Option<std::thread::JoinHandle<()>>,
    /// Finished previews and thumbnails (None: it could not be made).
    cache: HashMap<u64, Option<Thumb>>,
    asked: std::collections::HashSet<u64>,
    /// Library files found for this scene, with their material names.
    pub files: Vec<(PathBuf, Vec<String>)>,
    /// (file, material) of drag payload `LIB_TAG << 32 | index`.
    pub entries: Vec<(PathBuf, String)>,
    scanned_for: Option<PathBuf>,
}

fn key_of(v: impl Hash) -> u64 {
    let mut h = std::hash::DefaultHasher::new();
    v.hash(&mut h);
    h.finish()
}

struct Quiet;
impl Progress for Quiet {}

fn work(shared: &Shared) {
    loop {
        let job = shared.queue.lock().expect("queue").pop();
        let Some((key, job)) = job else {
            if shared.quit.load(Ordering::SeqCst) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(15));
            continue;
        };
        shared.active.fetch_add(1, Ordering::SeqCst);
        let out = match job {
            Job::Material(text, dir, size) => render_preview(&text, &dir, size).ok(),
            Job::Image(path, size) => image_thumb(&path, size).ok(),
        };
        shared.done.lock().expect("done").push((key, out));
        shared.active.fetch_sub(1, Ordering::SeqCst);
    }
}

impl Drop for Library {
    fn drop(&mut self) {
        // The worker finishes what it is doing and stops.
        self.shared.queue.lock().expect("queue").clear();
        self.shared.quit.store(true, Ordering::SeqCst);
        if let Some(w) = self.worker.take() {
            w.join().ok();
        }
    }
}

/// The preview scene of a material: a sphere on a checkered floor under a sky gradient
/// and a sun. Media are left out (the preview has none).
pub fn preview_text(material: &Table, size: usize, spp: u32) -> String {
    let mut m: Vec<(&String, &Value)> = material
        .iter()
        .filter(|(k, _)| !k.starts_with('$') && !matches!(k.as_str(), "interior" | "medium" | "exterior"))
        .collect();
    m.sort_by_key(|(k, _)| *k != "type");
    let mut s = format!(
        "[render]\nwidth = {size}\nheight = {size}\nspp = {spp}\nmax_depth = 8\nthreads = 2\nfilter = \"tent\"\n\
         [camera]\nposition = [0, 0.75, 3.3]\nlook_at = [0, 0.45, 0]\nfov = 24\n\
         [background]\ntype = \"gradient\"\nzenith = [0.35, 0.45, 0.65]\nhorizon = [0.85, 0.85, 0.85]\nground = [0.2, 0.2, 0.2]\n\
         [[lights]]\ntype = \"directional\"\nelevation = 55\nazimuth = 35\nintensity = 2.5\nangle = 4\n\
         [materials.floor]\nalbedo = {{ type = \"checker\", even = 0.55, odd = 0.2, scale = 3, space = \"world\" }}\n\
         [[objects]]\ntype = \"quad\"\norigin = [-6, 0, -6]\nu = [0, 0, 12]\nv = [12, 0, 0]\nmaterial = \"floor\"\n\
         [[objects]]\ntype = \"sphere\"\ncenter = [0, 0.5, 0]\nradius = 0.5\nmaterial = \"preview\"\n\
         [materials.preview]\n"
    );
    for (k, v) in m {
        let _ = writeln!(s, "{} = {}", format_key(k), format_value(v));
    }
    s
}

/// Renders preview scene `text` (paths relative to `dir`).
pub fn render_preview(text: &str, dir: &Path, size: usize) -> Result<Thumb, String> {
    let path = dir.join("__ptedit_preview__.toml");
    let scene = pt_core::loader::load_scene_from(&path, &|p| {
        if p == path { Ok(text.to_owned()) } else { pt_core::loader::read_file(p) }
    })?;
    let px = pt_core::renderer::render_progressive(&scene, &scene.camera, size, size, &Quiet).ok_or("cancelled")?;
    let enc = pt_core::image::Encoder::new(scene.settings.exposure, scene.settings.tonemap);
    Ok(Thumb { w: size, h: size, px: px.iter().map(|p| enc.encode(p.mean())).collect() })
}

/// An image file shrunk to fit `size` x `size` (box filter; HDR images tone mapped).
pub fn image_thumb(path: &Path, size: usize) -> Result<Thumb, String> {
    use pt_core::imageio::Pixels;
    let img = pt_core::imageio::load(path)?;
    let (w, h, c) = (img.width, img.height, img.channels.max(1));
    if w == 0 || h == 0 {
        return Err("empty image".into());
    }
    let scale = (w.max(h) as f32 / size as f32).max(1.0);
    let (tw, th) = (((w as f32 / scale) as usize).max(1), ((h as f32 / scale) as usize).max(1));
    let value = |i: usize| -> f32 {
        match &img.pixels {
            Pixels::U8(v) => f32::from(v[i]) / 255.0,
            Pixels::U16(v) => f32::from(v[i]) / 65535.0,
            // Linear HDR values, mapped for display.
            Pixels::F32(v) => {
                let x = v[i].max(0.0);
                (x / (1.0 + x)).powf(1.0 / 2.2)
            }
        }
    };
    let mut px = Vec::with_capacity(tw * th);
    for ty in 0..th {
        for tx in 0..tw {
            let (x0, x1) = (tx * w / tw, ((tx + 1) * w / tw).max(tx * w / tw + 1));
            let (y0, y1) = (ty * h / th, ((ty + 1) * h / th).max(ty * h / th + 1));
            let mut sum = [0.0f32; 3];
            let mut n = 0.0f32;
            // At most 8x8 samples per thumbnail pixel.
            let sx = ((x1 - x0) / 8).max(1);
            let sy = ((y1 - y0) / 8).max(1);
            for y in (y0..y1.min(h)).step_by(sy) {
                for x in (x0..x1.min(w)).step_by(sx) {
                    let i = (y * w + x) * c;
                    let g = value(i);
                    let rgb = if c >= 3 { [g, value(i + 1), value(i + 2)] } else { [g; 3] };
                    for k in 0..3 {
                        sum[k] += rgb[k];
                    }
                    n += 1.0;
                }
            }
            let ch = |v: f32| ((v / n.max(1.0)).clamp(0.0, 1.0) * 255.0 + 0.5) as u32;
            px.push(ch(sum[0]) << 16 | ch(sum[1]) << 8 | ch(sum[2]));
        }
    }
    Ok(Thumb { w: tw, h: th, px })
}

impl Library {
    fn ensure_worker(&mut self) {
        if self.worker.is_none() {
            let shared = Arc::clone(&self.shared);
            self.worker = std::thread::Builder::new().name("ptedit-previews".into()).spawn(move || work(&shared)).ok();
        }
    }

    /// Takes finished work into the cache.
    pub fn poll(&mut self) {
        let done: Vec<(u64, Option<Thumb>)> = std::mem::take(&mut *self.shared.done.lock().expect("done"));
        for (k, t) in done {
            self.cache.insert(k, t);
        }
    }

    fn request(&mut self, key: u64, job: impl FnOnce() -> Job) -> Option<&Thumb> {
        if !self.cache.contains_key(&key) && self.asked.insert(key) {
            self.ensure_worker();
            self.shared.queue.lock().expect("queue").push((key, job()));
        }
        self.cache.get(&key).and_then(Option::as_ref)
    }

    /// The preview of material `table` (whose paths are relative to `dir`), once made.
    pub fn material(&mut self, table: &Table, dir: &Path, size: usize) -> Option<&Thumb> {
        let text = preview_text(table, size, if size > 96 { 48 } else { 24 });
        let key = key_of(("material", &text, dir));
        let dir = dir.to_path_buf();
        self.request(key, move || Job::Material(text, dir, size))
    }

    /// A thumbnail of image file `path`, once made (and made again when the file changes).
    pub fn image(&mut self, path: &Path, size: usize) -> Option<&Thumb> {
        let mtime = std::fs::metadata(path).and_then(|m| m.modified()).ok();
        let key = key_of(("image", path, mtime, size));
        let p = path.to_path_buf();
        self.request(key, move || Job::Image(p, size))
    }

    /// Whether previews are still being made.
    pub fn busy(&self) -> bool {
        self.worker.is_some()
            && (!self.shared.queue.lock().expect("queue").is_empty()
                || self.shared.active.load(Ordering::SeqCst) > 0
                || !self.shared.done.lock().expect("done").is_empty())
    }

    /// Waits (tests) until every requested preview is made.
    #[cfg(test)]
    pub fn finish(&mut self) {
        let start = std::time::Instant::now();
        while self.busy() && start.elapsed() < std::time::Duration::from_secs(60) {
            std::thread::sleep(std::time::Duration::from_millis(5));
            self.poll();
        }
        self.poll();
    }

    /// Finds the library files for the scene at `scene`: `.toml` files of `lib` folders
    /// in its folder and up to three folders above (and `scenes/lib` there).
    pub fn scan(&mut self, scene: &Path) {
        if self.scanned_for.as_deref() == Some(scene) {
            return;
        }
        self.scanned_for = Some(scene.to_path_buf());
        self.files.clear();
        self.entries.clear();
        let mut seen = Vec::new();
        let mut dir = scene.parent().map(Path::to_path_buf);
        for _ in 0..4 {
            let Some(d) = dir.clone() else { break };
            for lib in [d.join("lib"), d.join("scenes").join("lib")] {
                let Ok(rd) = std::fs::read_dir(&lib) else { continue };
                let mut paths: Vec<PathBuf> =
                    rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "toml")).collect();
                paths.sort();
                for p in paths {
                    let c = pt_core::loader::canonical(&p);
                    if seen.contains(&c) || pt_core::loader::canonical(scene) == c {
                        continue;
                    }
                    seen.push(c);
                    let names = material_names(&p);
                    if !names.is_empty() {
                        for n in &names {
                            self.entries.push((p.clone(), n.clone()));
                        }
                        self.files.push((p, names));
                    }
                }
            }
            dir = d.parent().map(Path::to_path_buf);
        }
    }
}

/// The `[materials.NAME]` names of a file.
fn material_names(path: &Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(path) else { return Vec::new() };
    let Ok(doc) = Document::parse(text) else { return Vec::new() };
    match doc.get(&["materials".into()]).map(|n| &n.kind) {
        Some(NodeKind::Table(t)) => t.entries.iter().map(|e| e.key.clone()).collect(),
        _ => Vec::new(),
    }
}

/// Material `name`'s table in library file `path`.
fn library_table(path: &Path, name: &str) -> Option<Table> {
    let doc = Document::parse(std::fs::read_to_string(path).ok()?).ok()?;
    doc.value(&["materials".into(), name.into()])?.as_table().cloned()
}

impl Editor {
    /// The name of the material a drag payload carries (a library material is copied into
    /// the scene first).
    pub fn material_payload(&mut self, p: u64) -> Option<String> {
        match p >> 32 {
            MATERIAL_TAG => self.list(Kind::Material).get((p & 0xFFFF_FFFF) as usize).map(|m| m.label.clone()),
            LIB_TAG => {
                let (file, name) = self.library.entries.get((p & 0xFFFF_FFFF) as usize)?.clone();
                self.import_material(&file, &name).then_some(name)
            }
            _ => None,
        }
    }

    /// The preview of a material table defined in file `file` (`size` pixels), drawn into
    /// `r` when it is ready (a placeholder until then).
    pub fn preview_tile(&mut self, r: Rect, table: &Table, dir: &Path) {
        let size = r.w.max(r.h).max(8) as usize;
        self.library.poll();
        if let Some(t) = self.library.material(table, dir, size).cloned() {
            self.ui.image(r, t.w, t.h, t.px);
        } else {
            self.ui.fill(r, self.ui.theme.field);
            // A dot that says "coming".
            let c = Rect::new(r.x + r.w / 2 - 2, r.y + r.h / 2 - 2, 4, 4);
            self.ui.fill_rounded(c, self.ui.theme.text_disabled, 255, 2);
            self.ui.repaint_after(std::time::Duration::from_millis(50));
        }
    }

    /// A thumbnail of image `path` in `r` (a placeholder until it is made).
    pub fn image_tile(&mut self, r: Rect, path: &Path) {
        self.library.poll();
        let size = r.w.max(r.h).max(8) as usize;
        if let Some(t) = self.library.image(path, size).cloned() {
            let (w, h) = (t.w as i32, t.h as i32);
            let ir = Rect::new(r.x + (r.w - w).max(0) / 2, r.y + (r.h - h).max(0) / 2, w.min(r.w), h.min(r.h));
            self.ui.image(ir, t.w, t.h, t.px);
        } else {
            self.ui.fill(r, self.ui.theme.field);
            self.ui.repaint_after(std::time::Duration::from_millis(50));
        }
    }

    /// The Materials tab: the scene's materials, then each library file's.
    pub fn library_panel(&mut self, r: Rect) {
        self.ui.fill(r, self.ui.theme.panel);
        let path = self.ws.doc.path().to_path_buf();
        self.library.scan(&path);
        let rh = self.ui.row_h();
        let pad = self.ui.pad();
        let tile = rh * 2;
        let sid = id_of(ID, "scroll");
        let (scroll, mut col) = self.ui.begin_scroll(sid, r.shrink2(0, self.ui.px(2.0)));
        col.indent(pad / 2);
        col.gap = self.ui.px(2.0);
        let hint = col.row(rh);
        self.ui.label(hint, "drag onto an object to assign", self.ui.theme.text_disabled);
        // The scene's own materials.
        let header = col.row(rh);
        let open_id = id_of(ID, "scene");
        if self.ui.collapsing(open_id, header, "This scene", true) {
            let mats = self.ws.doc.tree.materials.clone();
            if mats.is_empty() {
                let rr = col.row(rh);
                self.ui.label(rr, "no materials yet", self.ui.theme.text_disabled);
            }
            for (i, m) in mats.iter().enumerate() {
                let rr = col.row(tile);
                if rr.bottom() < r.y || rr.y > r.bottom() {
                    continue;
                }
                let dir = self.ws.doc.files[m.file].path.parent().map(Path::to_path_buf).unwrap_or_default();
                let table = self.ws.doc.item_table(m).unwrap_or_default();
                let selected = self.inspect == Some(Target::Item(Kind::Material, i));
                let resp = self.tile_row(id_of(ID, ("scene", i)), rr, &m.label, &table, &dir, selected);
                self.probe(format!("library:scene:{}", m.label), rr);
                self.ui.drag_source(&resp, (MATERIAL_TAG << 32) | i as u64, &m.label);
                if resp.clicked {
                    self.inspect = Some(Target::Item(Kind::Material, i));
                }
                if resp.double_clicked {
                    self.assign_to_selection(&m.label.clone());
                }
            }
        }
        let files = self.library.files.clone();
        let mut entry = 0usize;
        for (file, names) in &files {
            let label = file.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned());
            let header = col.row(rh);
            let open = self.ui.collapsing(id_of(ID, ("lib", file)), header, &label, true);
            if !open {
                entry += names.len();
                continue;
            }
            let dir = file.parent().map(Path::to_path_buf).unwrap_or_default();
            for name in names {
                let rr = col.row(tile);
                let e = entry;
                entry += 1;
                if rr.bottom() < r.y || rr.y > r.bottom() {
                    continue;
                }
                let table = library_table(file, name).unwrap_or_default();
                let in_scene = self.ws.doc.tree.materials.iter().any(|m| m.label == *name);
                let resp = self.tile_row(id_of(ID, ("lib", e)), rr, name, &table, &dir, false);
                self.probe(format!("library:{label}:{name}"), rr);
                self.ui.tooltip(
                    &resp,
                    &if in_scene {
                        format!("{name}: the scene has its own {name}, which is used")
                    } else {
                        format!(
                            "{name}, from {}\ndrag onto an object, or double-click to give it to the selection",
                            file.display()
                        )
                    },
                );
                self.ui.drag_source(&resp, (LIB_TAG << 32) | e as u64, name);
                if resp.double_clicked && self.import_material(file, name) {
                    self.assign_to_selection(name);
                }
            }
        }
        if files.is_empty() {
            let rr = col.row(rh * 2);
            self.ui.label(rr, "no lib/*.toml files near the scene", self.ui.theme.text_disabled);
        }
        let bottom = col.y;
        self.ui.end_scroll(scroll, bottom);
    }

    /// One material: preview and name.
    fn tile_row(
        &mut self,
        id: Id,
        r: Rect,
        name: &str,
        table: &Table,
        dir: &Path,
        selected: bool,
    ) -> crate::ui::Response {
        let resp = self.ui.interact(id, r, false);
        if selected {
            self.ui.fill(r, self.ui.theme.row_selected);
        } else if resp.hovered {
            self.ui.fill(r, self.ui.theme.row_hover);
        }
        let (pr, lr) = r.split_left(r.h, self.ui.px(6.0));
        self.preview_tile(pr.shrink(1), table, dir);
        let ty = table.get("type").and_then(Value::as_str).unwrap_or("diffuse").to_owned();
        let (top, bottom) = lr.split_top(lr.h / 2);
        self.ui.label(top, name, self.ui.theme.text);
        self.ui.label(bottom, &ty, self.ui.theme.text_disabled);
        resp
    }

    /// Gives material `name` to every selected object.
    fn assign_to_selection(&mut self, name: &str) {
        let objs: Vec<usize> = self.selected().iter().filter(|(k, _)| *k == Kind::Object).map(|&(_, i)| i).collect();
        if objs.is_empty() {
            self.notify("select objects to give the material to");
            return;
        }
        let r = self.ws.doc.group(&format!("assign {name}"), |doc| {
            for &i in &objs {
                let it = doc.tree.objects[i].clone();
                let mut p = it.path.clone();
                p.push("material".into());
                doc.set(it.file, &p, &Value::String(name.to_owned()))?;
            }
            Ok(())
        });
        match r {
            Ok(()) => {
                self.scene_stale = true;
                self.notify(format!("{} objects now use {name}", objs.len()));
            }
            Err(e) => self.notify(format!("error: {e}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A preview of a red plastic is mostly red where the sphere is.
    #[test]
    fn previews_show_the_material() {
        let t = pt_core::toml::parse("type = \"plastic\"\ncolor = [0.8, 0.05, 0.05]\nroughness = 0.3\n").unwrap();
        let p = render_preview(&preview_text(&t, 32, 8), Path::new("."), 32).unwrap();
        let c = p.px[16 * 32 + 16];
        let (r, g) = (c >> 16 & 0xff, c >> 8 & 0xff);
        assert!(r > 2 * g && r > 60, "{c:06x}");
    }
}
