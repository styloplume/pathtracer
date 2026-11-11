//! The in-app file browser: a modal dialog to open a scene, save it under another name, or
//! pick a texture or mesh file. Folders first, then the files with the wanted extensions.

use crate::ui::{Column, Rect, Ui, id_of};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Open,
    SaveAs,
    /// Choose a file for a key of the scene.
    Pick,
}

#[derive(Clone, Debug)]
struct Entry {
    name: String,
    dir: bool,
}

pub struct FileDialog {
    pub mode: Mode,
    pub title: String,
    pub dir: PathBuf,
    pub name: String,
    /// Lower-case extensions shown (empty: every file).
    exts: Vec<String>,
    entries: Vec<Entry>,
    selected: Option<usize>,
    pub error: Option<String>,
    /// Bumped when the listing changes, so the list scrolls back to the top.
    generation: u64,
}

pub enum Outcome {
    Open,
    Cancel,
    Chosen(PathBuf),
}

const ID: u64 = 0xF11E_D1A1;

impl FileDialog {
    pub fn new(mode: Mode, title: &str, dir: &Path, exts: &[&str]) -> FileDialog {
        let mut d = FileDialog {
            mode,
            title: title.to_owned(),
            dir: dir.to_path_buf(),
            name: String::new(),
            exts: exts.iter().map(|e| e.to_ascii_lowercase()).collect(),
            entries: Vec::new(),
            selected: None,
            error: None,
            generation: 0,
        };
        d.refresh();
        d
    }

    /// Reads the folder again.
    pub fn refresh(&mut self) {
        self.entries.clear();
        self.selected = None;
        self.generation += 1;
        let rd = match std::fs::read_dir(&self.dir) {
            Ok(rd) => rd,
            Err(e) => {
                self.error = Some(format!("cannot read '{}': {e}", self.dir.display()));
                return;
            }
        };
        self.error = None;
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            let dir = e.file_type().is_ok_and(|t| t.is_dir()) || (e.path().is_dir());
            let shown = dir
                || self.exts.is_empty()
                || Path::new(&name)
                    .extension()
                    .is_some_and(|x| self.exts.contains(&x.to_string_lossy().to_ascii_lowercase()));
            if shown {
                self.entries.push(Entry { name, dir });
            }
        }
        self.entries.sort_by(|a, b| b.dir.cmp(&a.dir).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    }

    fn enter(&mut self, dir: PathBuf) {
        self.dir = dir;
        self.refresh();
    }

    fn choose(&mut self) -> Outcome {
        let name = self.name.trim();
        if name.is_empty() {
            return Outcome::Open;
        }
        let mut path = self.dir.join(name);
        if path.is_dir() {
            self.name.clear();
            self.enter(path);
            return Outcome::Open;
        }
        if self.mode == Mode::SaveAs && path.extension().is_none() {
            path.set_extension("toml");
        }
        if self.mode != Mode::SaveAs && !path.is_file() {
            self.error = Some(format!("'{}' does not exist", path.display()));
            return Outcome::Open;
        }
        Outcome::Chosen(path)
    }

    /// Draws the dialog; returns what the user did.
    pub fn ui(&mut self, ui: &mut Ui) -> Outcome {
        let area = ui.begin_modal(&self.title, 620.0, 460.0);
        let rh = ui.row_h();
        let gap = ui.px(6.0);
        let mut col = Column::new(area, gap);
        let mut out = Outcome::Open;

        // Folder: up, and the path (type another to go there).
        let top = col.row(rh);
        let (up, path) = top.split_left(ui.px(60.0), gap);
        let typing = ui.edit.is_some();
        if (ui.button(id_of(ID, "up"), up, "Up") || (!typing && ui.take_key(pt_core::platform::Key::Backspace)))
            && let Some(p) = self.dir.parent().map(Path::to_path_buf)
        {
            self.enter(p);
        }
        let shown = self.dir.display().to_string();
        if let Some(t) = ui.text_field(id_of(ID, "dir"), path, &shown, "folder") {
            let p = PathBuf::from(t.trim());
            if p.is_dir() {
                self.enter(p);
            } else {
                self.error = Some(format!("'{}' is not a folder", p.display()));
            }
        }

        // The listing.
        let bottom_h = 2 * rh + gap * 2 + if self.error.is_some() { rh } else { 0 };
        let list_r = Rect::new(area.x, col.y, area.w, (area.bottom() - col.y - bottom_h).max(rh));
        ui.fill(list_r, ui.theme.field);
        ui.border(list_r, ui.theme.border, 0);
        let sid = id_of(ID, ("list", self.generation));
        let (scroll, mut lc) = ui.begin_scroll(sid, list_r.shrink(1));
        let mut enter = None;
        for (i, e) in self.entries.iter().enumerate() {
            let r = lc.row(rh);
            if r.bottom() < list_r.y || r.y > list_r.bottom() {
                continue;
            }
            let label = if e.dir { format!("{}/", e.name) } else { e.name.clone() };
            let color = if e.dir { ui.theme.text } else { ui.theme.text_dim };
            let t = ui.tree_row(id_of(sid, i), r, 0, None, self.selected == Some(i), &label, "", color);
            if t.resp.clicked || t.resp.pressed {
                self.selected = Some(i);
                if !e.dir {
                    self.name.clone_from(&e.name);
                }
            }
            if t.resp.double_clicked {
                enter = Some(i);
            }
        }
        if self.entries.is_empty() {
            let r = lc.row(rh);
            ui.label(r, "(empty)", ui.theme.text_disabled);
        }
        ui.end_scroll(scroll, lc.y);
        if let Some(i) = enter {
            let e = self.entries[i].clone();
            if e.dir {
                self.enter(self.dir.join(&e.name));
            } else {
                self.name = e.name;
                out = self.choose();
            }
        }
        col.y = list_r.bottom() + gap;

        if let Some(err) = self.error.clone() {
            let r = col.row(rh);
            ui.label(r, &err, ui.theme.error);
        }
        let name_r = col.row(rh);
        let (label, field) = name_r.split_left(ui.px(80.0), gap);
        ui.label(label, "File name", ui.theme.text_dim);
        if let Some(t) = ui.text_field(id_of(ID, "name"), field, &self.name.clone(), "") {
            self.name = t;
            if ui.focus == 0 {
                // Enter in the field.
                out = self.choose();
            }
        }
        let buttons = col.row(rh);
        let bw = ui.px(96.0);
        let (ok_r, rest) = buttons.split_right(bw, gap);
        let (cancel_r, _) = rest.split_right(bw, gap);
        let verb = match self.mode {
            Mode::Open => "Open",
            Mode::SaveAs => "Save",
            Mode::Pick => "Choose",
        };
        if ui.button(id_of(ID, "cancel"), cancel_r, "Cancel") || ui.take_key(pt_core::platform::Key::Escape) {
            out = Outcome::Cancel;
        }
        if ui.button_primary(id_of(ID, "ok"), ok_r, verb) {
            out = match self.selected.map(|i| self.entries[i].clone()) {
                Some(e) if e.dir && self.name.trim().is_empty() => {
                    self.enter(self.dir.join(&e.name));
                    Outcome::Open
                }
                _ => self.choose(),
            };
        }
        ui.end_modal();
        out
    }
}

/// `path` relative to folder `base`, with forward slashes (as scenes write paths), when it
/// is inside it or near it; else absolute.
pub fn relative(path: &Path, base: &Path) -> String {
    // Canonical (a file that does not exist yet: its folder), without Windows' verbatim prefix.
    let canon = |p: &Path| {
        let c = match (p.exists(), p.parent(), p.file_name()) {
            (false, Some(dir), Some(name)) => pt_core::loader::canonical(dir).join(name),
            _ => pt_core::loader::canonical(p),
        };
        let s = c.display().to_string();
        PathBuf::from(s.strip_prefix(r"\\?\").unwrap_or(&s))
    };
    let (p, b) = (canon(path), canon(base));
    let pc: Vec<_> = p.components().collect();
    let bc: Vec<_> = b.components().collect();
    let common = pc.iter().zip(&bc).take_while(|(a, b)| a == b).count();
    if common == 0 || bc.len() - common > 3 {
        return p.display().to_string().trim_start_matches(r"\\?\").to_owned();
    }
    let mut parts: Vec<String> = std::iter::repeat_n("..".to_owned(), bc.len() - common).collect();
    parts.extend(pc[common..].iter().map(|c| c.as_os_str().to_string_lossy().into_owned()));
    parts.join("/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_folders_and_wanted_files() {
        let d = std::env::temp_dir().join(format!("ptedit-files-{}", std::process::id()));
        std::fs::create_dir_all(d.join("sub")).unwrap();
        for f in ["b.toml", "A.TOML", "c.png", ".hidden.toml"] {
            std::fs::write(d.join(f), "").unwrap();
        }
        let fd = FileDialog::new(Mode::Open, "Open", &d, &["toml"]);
        let names: Vec<&str> = fd.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["sub", "A.TOML", "b.toml"]);
        assert_eq!(relative(&d.join("sub").join("x.png"), &d), "sub/x.png");
        assert_eq!(relative(&d.join("b.toml"), &d.join("sub")), "../b.toml");
        std::fs::remove_dir_all(&d).ok();
    }
}
