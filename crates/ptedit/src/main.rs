//! `ptedit`: the scene editor. It opens a scene as an editable document and shows it in a
//! rasterized viewport (one view, or top / front / side / perspective) between an outliner
//! of the scene's contents and an inspector of the selected item's keys, built with our own
//! immediate-mode UI toolkit. Edits keep the files' formatting and can be undone.

mod display;
mod editor;
#[cfg(test)]
mod editor_tests;
mod files;
mod inspector;
mod outliner;
mod overlay;
mod paint;
mod raster;
mod render;
mod settings;
mod ui;
mod view;
mod workspace;

use editor::{Editor, Target};
use pt_core::document::{Item, Kind, SceneDoc};
use render::Shading;
use settings::Settings;
use std::path::Path;
use workspace::Workspace;

const USAGE: &str = "\
usage: ptedit <scene.toml>           open the scene in the editor
       ptedit --check <scene.toml>   print the scene's outline and problems, without a window
       ptedit --snapshot <scene.toml> <out.png> [--size WxH] [--quad] [--shading MODE]
                     [--select N]... [--frames N] [--ui] [--theme dark|light] [--scale S]
                                     draw the viewport (with --ui: the whole editor) into a
                                     PNG, without a window (MODE: wire, solid, shaded,
                                     wiresolid; N: object index)

the editor's keys (F1 shows them in the window):";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["--check", scene] => check(Path::new(scene)),
        ["--snapshot", scene, out, rest @ ..] => snapshot(Path::new(scene), Path::new(out), rest),
        [scene] if !scene.starts_with('-') => editor::run(Path::new(scene)),
        _ => {
            eprintln!("{USAGE}");
            for line in editor::HELP {
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

/// `--snapshot`: the viewport (or the whole editor) as it would show, saved as a PNG.
fn snapshot(path: &Path, out: &Path, opts: &[&str]) -> Result<(), String> {
    let mut ws = Workspace::open(path)?;
    let (mut w, mut h, mut frames) = (1280usize, 720usize, 1usize);
    let (mut with_ui, mut theme, mut scale) = (false, None, 1.0f32);
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
            "--ui" => with_ui = true,
            "--theme" => theme = Some(value()?.to_owned()),
            "--scale" => scale = value()?.parse().map_err(|_| "--scale takes a number")?,
            _ => return Err(format!("unknown option '{o}'")),
        }
    }
    if with_ui {
        let mut s = Settings { quad: ws.quad, ..Settings::default() };
        if let Some(t) = theme {
            s.theme = t;
        }
        let quad = ws.quad;
        let mut ed = Editor::new(ws, s, None, scale);
        ed.ws.quad = quad;
        if let Some(i) = ed.ws.sel.objects.iter().position(|&x| x) {
            ed.inspect = Some(Target::Item(Kind::Object, i));
        }
        // Twice: popups learn their sizes, the status bar the viewport's statistics.
        let (wi, hi) = (w as i32, h as i32);
        ed.frame(ui::Input::default(), &[], wi, hi);
        ed.render(w, h);
        ed.frame(ui::Input::default(), &[], wi, hi);
        let px = ed.render(w, h);
        eprintln!("{w}x{h}: font {} at {} px", ed.ui.font.source, ed.ui.font.size);
        return pt_core::image::write_png(out, w, h, &pt_core::image::packed_to_rgb(&px))
            .map_err(|e| format!("cannot write '{}': {e}", out.display()));
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
