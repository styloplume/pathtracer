//! `ptedit`: the scene editor. It opens a scene as an editable document and shows it in
//! a rasterized viewport (one view, or top / front / side / perspective), with selection by
//! clicking and dragging a rectangle, and saves it (Ctrl+S) with undo / redo of the
//! document's edit history.

mod display;
mod overlay;
mod paint;
mod raster;
mod render;
mod view;
mod workspace;

use pt_core::document::{Item, SceneDoc};
use pt_core::math::Vec3;
use pt_core::platform::{Event, Key, Modifiers, MouseButton, Window};
use render::Shading;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use workspace::{PERSPECTIVE, Rect, Workspace};

const USAGE: &str = "\
usage: ptedit <scene.toml>           open the scene in the editor
       ptedit --check <scene.toml>   print the scene's outline and problems, without a window
       ptedit --snapshot <scene.toml> <out.png> [--size WxH] [--quad] [--shading MODE]
                     [--select N]... [--frames N]
                                     draw the viewport into a PNG, without a window
                                     (MODE: wire, solid, shaded, wiresolid; N: object index)

editor keys (F1 shows them in the window):";

const HELP: &[&str] = &[
    "left click: select  (Shift: add, Ctrl: toggle)     left drag: rectangle select",
    "middle drag or Alt+left drag: orbit     Shift+middle or Alt+Shift+left: pan",
    "wheel: zoom     right drag: look, with W A S D Q E to fly (Shift: faster)",
    "F: frame selection (or all)     Home: reset view     O: perspective / orthographic",
    "1-4 or Z: wireframe / solid / shaded / wire on solid     G: grid     B: all bounds",
    "L: four views / one view     Space: maximize the view under the mouse",
    "Ctrl+A: select all     Esc: clear selection, then quit",
    "Ctrl+S save     Ctrl+Z undo     Ctrl+Y or Ctrl+Shift+Z redo     F1: this help",
];

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["--check", scene] => check(Path::new(scene)),
        ["--snapshot", scene, out, rest @ ..] => snapshot(Path::new(scene), Path::new(out), rest),
        [scene] if !scene.starts_with('-') => run(Path::new(scene)),
        _ => {
            eprintln!("{USAGE}");
            for line in HELP {
                eprintln!("  {line}");
            }
            std::process::exit(2);
        }
    };
    if let Err(e) = result {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

/// `--check`: the outline and every schema problem; fails if there are problems.
fn check(path: &Path) -> Result<(), String> {
    let doc = SceneDoc::open(path)?;
    for line in outline(&doc) {
        println!("{line}");
    }
    let problems = doc.validate()?;
    for p in &problems {
        println!("problem: {p}");
    }
    doc.to_scene()?;
    if problems.is_empty() { Ok(()) } else { Err(format!("{} problems", problems.len())) }
}

/// `--snapshot`: the viewport as the editor would show it, saved as a PNG.
fn snapshot(path: &Path, out: &Path, opts: &[&str]) -> Result<(), String> {
    let mut ws = Workspace::open(path)?;
    let (mut w, mut h, mut frames) = (1280usize, 720usize, 1usize);
    let mut it = opts.iter();
    while let Some(&o) = it.next() {
        let mut value = || it.next().copied().ok_or_else(|| format!("{o} needs a value"));
        match o {
            "--size" => {
                let v = value()?;
                let (a, b) = v.split_once('x').ok_or("--size takes WxH")?;
                (w, h) = (a.parse().map_err(|_| "bad width")?, b.parse().map_err(|_| "bad height")?);
            }
            "--quad" => ws.quad = true,
            "--shading" => ws.shading = Shading::parse(value()?).ok_or("unknown shading")?,
            "--select" => {
                let i: u32 = value()?.parse().map_err(|_| "--select takes an object index")?;
                ws.sel.set_id(i + 1, true);
            }
            "--frames" => frames = value()?.parse().map_err(|_| "--frames takes a count")?,
            _ => return Err(format!("unknown option '{o}'")),
        }
    }
    let mut times = Vec::new();
    let mut px = Vec::new();
    for _ in 0..frames.max(1) {
        px = ws.compose(w, h);
        times.push(ws.frame_ms);
    }
    times.sort_by(f32::total_cmp);
    eprintln!(
        "{w}x{h}: {} triangles, {} boxes; {:.1} ms per frame (median of {})",
        ws.stats.triangles,
        ws.stats.boxes,
        times[times.len() / 2],
        times.len()
    );
    pt_core::image::write_png(out, w, h, &pt_core::image::packed_to_rgb(&px))
        .map_err(|e| format!("cannot write '{}': {e}", out.display()))
}

/// The scene's contents as text lines: files, settings, and every item with the file it
/// comes from.
fn outline(doc: &SceneDoc) -> Vec<String> {
    let name = |i: usize| -> String {
        let p = &doc.files[i].path;
        let rel = p.strip_prefix(doc.path().parent().unwrap_or(Path::new(""))).unwrap_or(p);
        rel.display().to_string()
    };
    let n = doc.files.len();
    let mut out = vec![format!("{}  ({n} file{})", name(0), if n == 1 { "" } else { "s" })];
    for (i, f) in doc.files.iter().enumerate().skip(1) {
        out.push(format!("  includes {}{}", name(i), if f.is_dirty() { " *" } else { "" }));
    }
    let t = &doc.tree;
    let from = |item: &Item| if item.file == 0 { String::new() } else { format!("   ({})", name(item.file)) };
    let mut sections: Vec<&str> = t.settings.iter().map(|s| s.section).collect();
    sections.dedup();
    for section in sections {
        let keys: Vec<&str> = t.settings.iter().filter(|s| s.section == section).map(|s| s.key.as_str()).collect();
        out.push(format!("[{section}] {}", keys.join(", ")));
    }
    if let Some(f) = t.background {
        let ty = doc.value(f, &["background".into(), "type".into()]);
        let ty = ty.as_ref().and_then(|v| v.as_str()).unwrap_or("color");
        out.push(format!("[background] {ty}{}", if f == 0 { String::new() } else { format!("   ({})", name(f)) }));
    }
    let lists: [(&str, &[Item]); 5] = [
        ("lights", &t.lights),
        ("objects", &t.objects),
        ("materials", &t.materials),
        ("media", &t.media),
        ("prototypes", &t.prototypes),
    ];
    for (title, items) in lists {
        if items.is_empty() {
            continue;
        }
        out.push(format!("{title} ({})", items.len()));
        // Huge scenes: the first entries are enough for an overview.
        for item in items.iter().take(40) {
            out.push(format!("  {}{}", item.label, from(item)));
        }
        if items.len() > 40 {
            out.push(format!("  ... {} more", items.len() - 40));
        }
    }
    out
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum DragMode {
    Orbit,
    Pan,
    Look,
    /// Left button: a click or a selection rectangle, from where it started.
    Select {
        x0: i32,
        y0: i32,
        mods: Modifiers,
    },
}

#[derive(Clone, Copy, Debug)]
struct Drag {
    mode: DragMode,
    rect: Rect,
    x: i32,
    y: i32,
}

struct Editor {
    ws: Workspace,
    window: Window,
    status: String,
    help: bool,
    dirty: bool,
    drag: Option<Drag>,
    mouse: (i32, i32),
    held: HashSet<Key>,
    last_tick: Instant,
    /// The size the last frame was drawn at, and when the window last changed size.
    size: (usize, usize),
}

fn run(path: &Path) -> Result<(), String> {
    let ws = Workspace::open(path)?;
    for p in ws.doc.validate()? {
        eprintln!("warning: {p}");
    }
    let window = Window::new("ptedit", 1280, 800)?;
    let mut ed = Editor {
        ws,
        window,
        status: "F1: help".into(),
        help: false,
        dirty: true,
        drag: None,
        mouse: (0, 0),
        held: HashSet::new(),
        last_tick: Instant::now(),
        size: (0, 0),
    };
    ed.main_loop();
    if ed.ws.doc.is_dirty() {
        eprintln!("warning: quit with unsaved changes");
    }
    Ok(())
}

const FLY_KEYS: [Key; 6] =
    [Key::Char('w'), Key::Char('a'), Key::Char('s'), Key::Char('d'), Key::Char('q'), Key::Char('e')];

impl Editor {
    fn main_loop(&mut self) {
        loop {
            for ev in self.window.poll_events() {
                if !self.handle(ev) {
                    return;
                }
            }
            self.tick_fly();
            let (w, h) = self.window.client_size();
            if (w as usize, h as usize) != self.size {
                self.dirty = true;
            }
            if self.dirty {
                self.dirty = false;
                self.draw();
            }
            let busy = self.drag.is_some();
            std::thread::sleep(Duration::from_millis(if busy { 2 } else { 8 }));
        }
    }

    fn window_size(&self) -> (usize, usize) {
        let (w, h) = self.window.client_size();
        (w.max(1) as usize, h.max(1) as usize)
    }

    fn rect_at(&self, x: i32, y: i32) -> Option<Rect> {
        let (w, h) = self.window_size();
        self.ws.rect_at(w, h, x, y)
    }

    /// Returns false to quit.
    fn handle(&mut self, ev: Event) -> bool {
        match ev {
            Event::Close => return false,
            Event::Resized(..) | Event::Paint => self.dirty = true,
            Event::FocusLost => {
                self.held.clear();
                self.drag = None;
                self.dirty = true;
            }
            Event::KeyDown { key, mods, repeat } => return self.key(key, mods, repeat),
            Event::KeyUp { key, .. } => {
                self.held.remove(&key);
            }
            Event::MouseDown { button, x, y, mods } => {
                let Some(rect) = self.rect_at(x, y) else { return true };
                self.set_active(rect.view);
                let mode = match button {
                    MouseButton::Left if mods.alt && mods.shift => DragMode::Pan,
                    MouseButton::Left if mods.alt => DragMode::Orbit,
                    MouseButton::Left => DragMode::Select { x0: x, y0: y, mods },
                    MouseButton::Middle if mods.shift => DragMode::Pan,
                    MouseButton::Middle => DragMode::Orbit,
                    MouseButton::Right => DragMode::Look,
                };
                if self.drag.is_none() {
                    self.drag = Some(Drag { mode, rect, x, y });
                }
            }
            Event::MouseUp { button, x, y, .. } => {
                if let Some(d) = self.drag.take() {
                    if let (DragMode::Select { x0, y0, mods }, MouseButton::Left) = (d.mode, button) {
                        self.select(&d.rect, x0, y0, x, y, mods);
                    }
                    if d.mode == DragMode::Look {
                        self.held.clear();
                    }
                    self.dirty = true;
                }
            }
            Event::MouseMove { x, y, .. } => {
                self.mouse = (x, y);
                match self.drag {
                    Some(mut d) => {
                        let (dx, dy) = ((x - d.x) as f32, (y - d.y) as f32);
                        let view = &mut self.ws.views[d.rect.view];
                        match d.mode {
                            DragMode::Orbit if view.is_fixed() => view.pan(dx, dy, d.rect.h as f32),
                            DragMode::Orbit => view.orbit(dx, dy),
                            DragMode::Pan => view.pan(dx, dy, d.rect.h as f32),
                            DragMode::Look => view.look(dx, dy),
                            DragMode::Select { .. } => {}
                        }
                        (d.x, d.y) = (x, y);
                        self.drag = Some(d);
                        self.dirty = true;
                    }
                    None => {
                        if let Some(r) = self.rect_at(x, y) {
                            self.set_active(r.view);
                        }
                    }
                }
            }
            Event::Wheel { delta, .. } => {
                let (x, y) = self.mouse;
                if let Some(r) = self.rect_at(x, y) {
                    self.ws.views[r.view].zoom(delta);
                    self.dirty = true;
                }
            }
        }
        true
    }

    fn set_active(&mut self, v: usize) {
        if self.ws.active != v {
            self.ws.active = v;
            self.dirty = true;
        }
    }

    fn notify(&mut self, s: impl Into<String>) {
        self.status = s.into();
        self.dirty = true;
    }

    fn key(&mut self, key: Key, mods: Modifiers, repeat: bool) -> bool {
        if mods.ctrl {
            if let Key::Char(c) = key
                && !repeat
            {
                self.command(c, mods.shift);
            }
            return true;
        }
        let looking = self.drag.is_some_and(|d| d.mode == DragMode::Look);
        if looking && (FLY_KEYS.contains(&key) || key == Key::Shift) {
            self.held.insert(key);
            return true;
        }
        if key == Key::Shift {
            self.held.insert(key);
        }
        if repeat {
            return true;
        }
        let v = self.ws.active;
        match key {
            Key::Escape if self.ws.sel.is_empty() => return false,
            Key::Escape => {
                self.ws.sel.clear();
                self.notify("selection cleared");
            }
            Key::F(1) => {
                self.help = !self.help;
                self.dirty = true;
            }
            Key::Char(c @ '1'..='4') => self.set_shading(Shading::ALL[c as usize - '1' as usize]),
            Key::Char('z') => {
                let i = Shading::ALL.iter().position(|&s| s == self.ws.shading).unwrap_or(0);
                self.set_shading(Shading::ALL[(i + 1) % 4]);
            }
            Key::Char('o') => {
                let view = &mut self.ws.views[v];
                if view.is_fixed() {
                    self.notify("the top, front and side views are always orthographic");
                } else {
                    view.ortho = !view.ortho;
                    let what = if view.ortho { "orthographic" } else { "perspective" };
                    self.notify(what);
                }
            }
            Key::Char('f') => {
                self.ws.frame(v);
                self.notify(if self.ws.sel.is_empty() { "framed the scene" } else { "framed the selection" });
            }
            Key::Home => {
                let (cam, b) = (self.ws.scene.camera.clone(), self.ws.display.bounds);
                self.ws.views[v].reset(&cam, &b);
                self.notify(if v == PERSPECTIVE { "view reset to the scene camera" } else { "view reset" });
            }
            Key::Char('g') => {
                self.ws.grid = !self.ws.grid;
                self.dirty = true;
            }
            Key::Char('b') => {
                self.ws.all_bounds = !self.ws.all_bounds;
                self.dirty = true;
            }
            Key::Char('l') => {
                self.ws.quad = !self.ws.quad;
                self.ws.maximized = None;
                self.ws.active = PERSPECTIVE;
                self.notify(if self.ws.quad { "four views" } else { "one view" });
            }
            Key::Space if self.ws.quad => {
                self.ws.maximized = match self.ws.maximized {
                    Some(_) => None,
                    None => Some(v),
                };
                self.dirty = true;
            }
            _ => {}
        }
        true
    }

    fn set_shading(&mut self, s: Shading) {
        self.ws.shading = s;
        self.notify(format!("shading: {}", s.name()));
    }

    fn command(&mut self, c: char, shift: bool) {
        let result = match (c, shift) {
            ('s', _) => self.ws.doc.save().map(|written| match written.len() {
                0 => "nothing to save".to_owned(),
                _ => format!("saved {}", list(&written)),
            }),
            ('z', false) => self.history(true),
            ('y', _) | ('z', true) => self.history(false),
            ('a', _) => {
                let ws = &mut self.ws;
                ws.sel.objects.iter_mut().chain(ws.sel.lights.iter_mut()).for_each(|s| *s = true);
                Ok(format!("selected {}", ws.sel.count()))
            }
            _ => return,
        };
        self.notify(result.unwrap_or_else(|e| format!("error: {e}")));
    }

    fn history(&mut self, undo: bool) -> Result<String, String> {
        let done = if undo { self.ws.doc.undo()? } else { self.ws.doc.redo()? };
        if !done {
            return Ok(if undo { "nothing to undo" } else { "nothing to redo" }.into());
        }
        self.ws.reload()?;
        Ok(if undo { "undone" } else { "redone" }.into())
    }

    /// A click (picks through the BVH) or a rectangle (what is drawn inside it).
    fn select(&mut self, r: &Rect, x0: i32, y0: i32, x1: i32, y1: i32, mods: Modifiers) {
        let (lx0, ly0, lx1, ly1) = (x0 - r.x as i32, y0 - r.y as i32, x1 - r.x as i32, y1 - r.y as i32);
        let ids = if (x1 - x0).abs() + (y1 - y0).abs() < 4 {
            self.ws.pick(r, lx1 as f32 + 0.5, ly1 as f32 + 0.5).into_iter().collect()
        } else {
            self.ws.ids_in(r, lx0, ly0, lx1, ly1)
        };
        let sel = &mut self.ws.sel;
        if !mods.shift && !mods.ctrl {
            sel.clear();
        }
        for &id in &ids {
            let on = !(mods.ctrl && sel.has_id(id));
            sel.set_id(id, on);
        }
        self.status = match (ids.as_slice(), sel.count()) {
            (_, 0) => "nothing selected".into(),
            ([id], 1) => format!("selected {}", self.ws.label(*id)),
            (_, n) => format!("{n} selected"),
        };
    }

    fn tick_fly(&mut self) {
        let dt = self.last_tick.elapsed().as_secs_f32().min(0.1);
        self.last_tick = Instant::now();
        let looking = self.drag.filter(|d| d.mode == DragMode::Look);
        let Some(d) = looking else { return };
        let held = |c: char| f32::from(u8::from(self.held.contains(&Key::Char(c))));
        let local = Vec3::new(held('d') - held('a'), held('e') - held('q'), held('w') - held('s'));
        if local.length_squared() > 0.0 {
            let b = self.ws.display.bounds;
            let diag = (b.max - b.min).length();
            let speed = if diag.is_finite() && diag > 0.0 { 0.3 * diag } else { 1.0 };
            let fast = if self.held.contains(&Key::Shift) { 4.0 } else { 1.0 };
            self.ws.views[d.rect.view].fly(local * (speed * fast * dt));
            self.dirty = true;
        }
    }

    fn draw(&mut self) {
        let (w, h) = self.window_size();
        self.size = (w, h);
        let name = self.ws.doc.path().file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        let star = if self.ws.doc.is_dirty() { " *" } else { "" };
        self.window.set_title(&format!("ptedit — {name}{star}"));
        let mut px = self.ws.compose(w, h);
        if let Some(Drag { mode: DragMode::Select { x0, y0, .. }, x, y, .. }) = self.drag
            && (x - x0).abs() + (y - y0).abs() >= 4
        {
            let mut c = paint::Canvas { px: &mut px, w, h };
            c.rect(x0, y0, x, y, 0xe0_e0_e8);
        }
        let s = &self.ws.stats;
        let mut lines = vec![
            self.status.clone(),
            format!(
                "{} · {} triangles{} · {:.1} ms",
                self.ws.shading.name(),
                s.triangles,
                if s.boxes > 0 { format!(", {} boxes", s.boxes) } else { String::new() },
                self.ws.frame_ms
            ),
        ];
        if self.help {
            lines.push(String::new());
            lines.extend(HELP.iter().map(|&l| l.to_owned()));
        }
        self.window.present(&px, w, h, &lines);
    }
}

fn list(paths: &[PathBuf]) -> String {
    paths
        .iter()
        .map(|p| p.file_name().map_or_else(|| p.display().to_string(), |n| n.to_string_lossy().into_owned()))
        .collect::<Vec<_>>()
        .join(", ")
}
