//! `ptedit`: the scene editor. For now a skeleton: it opens a scene as an editable
//! document, shows its outline in a window, and saves it (Ctrl+S) with undo / redo
//! (Ctrl+Z, Ctrl+Y) of the document's edit history.

use pt_core::document::{Item, SceneDoc};
use pt_core::platform::{Event, Key, Window};
use std::path::{Path, PathBuf};
use std::time::Duration;

const USAGE: &str = "\
usage: ptedit <scene.toml>           open the scene in the editor
       ptedit --check <scene.toml>   print the scene's outline and problems, without a window

editor keys: Ctrl+S save, Ctrl+Z undo, Ctrl+Y or Ctrl+Shift+Z redo, Esc quit";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["--check", scene] => check(Path::new(scene)),
        [scene] if !scene.starts_with('-') => run(Path::new(scene)),
        _ => {
            eprintln!("{USAGE}");
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

struct Editor {
    doc: SceneDoc,
    window: Window,
    status: String,
    dirty_view: bool,
}

fn run(path: &Path) -> Result<(), String> {
    let doc = SceneDoc::open(path)?;
    for p in doc.validate()? {
        eprintln!("warning: {p}");
    }
    let window = Window::new("ptedit", 1100, 760)?;
    let mut ed =
        Editor { doc, window, status: "Ctrl+S save   Ctrl+Z undo   Ctrl+Y redo   Esc quit".into(), dirty_view: true };
    ed.main_loop();
    if ed.doc.is_dirty() {
        eprintln!("warning: quit with unsaved changes");
    }
    Ok(())
}

impl Editor {
    fn main_loop(&mut self) {
        loop {
            for ev in self.window.poll_events() {
                match ev {
                    Event::Close | Event::KeyDown { key: Key::Escape, .. } => return,
                    Event::Resized(..) | Event::Paint => self.dirty_view = true,
                    Event::KeyDown { key: Key::Char(c), mods, .. } if mods.ctrl => self.command(c, mods.shift),
                    _ => {}
                }
            }
            if self.dirty_view {
                self.dirty_view = false;
                self.draw();
            }
            std::thread::sleep(Duration::from_millis(16));
        }
    }

    fn command(&mut self, c: char, shift: bool) {
        let result = match (c, shift) {
            ('s', _) => self.doc.save().map(|written| match written.len() {
                0 => "nothing to save".to_owned(),
                _ => format!("saved {}", list(&written)),
            }),
            ('z', false) => self.doc.undo().map(|done| if done { "undone" } else { "nothing to undo" }.to_owned()),
            ('y', _) | ('z', true) => {
                self.doc.redo().map(|done| if done { "redone" } else { "nothing to redo" }.to_owned())
            }
            _ => return,
        };
        self.status = result.unwrap_or_else(|e| format!("error: {e}"));
        self.dirty_view = true;
    }

    fn draw(&mut self) {
        let (w, h) = self.window.client_size();
        let (w, h) = (w.max(1) as usize, h.max(1) as usize);
        let name = self.doc.path().file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        let star = if self.doc.is_dirty() { " *" } else { "" };
        self.window.set_title(&format!("ptedit — {name}{star}"));
        let mut lines = vec![self.status.clone(), String::new()];
        lines.extend(outline(&self.doc));
        // The viewport arrives in E1; until then a plain background behind the outline.
        let pixels = vec![0x0020_2328u32; w * h];
        self.window.present(&pixels, w, h, &lines);
    }
}

fn list(paths: &[PathBuf]) -> String {
    paths
        .iter()
        .map(|p| p.file_name().map_or_else(|| p.display().to_string(), |n| n.to_string_lossy().into_owned()))
        .collect::<Vec<_>>()
        .join(", ")
}
