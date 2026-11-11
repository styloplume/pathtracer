//! The outliner: the scene's settings sections, lights, objects, materials, media and
//! prototypes as a tree. Rows select (objects and lights also in the viewport), rename
//! (double-click or F2), reorder by dragging, take a material dropped on an object, show
//! and hide, and name the included file an entry comes from.

use crate::editor::{Editor, Section, Target};
use crate::ui::{Id, Rect, id_of};
use pt_core::document::{Item, Kind};
use pt_core::platform::Key;
use pt_core::toml::{Seg, Value};

const ID: Id = 0x6f75_746c_696e_6572;

/// One row of the tree.
struct Row {
    /// A group header (its id) or an entry.
    group: Option<Id>,
    target: Option<Target>,
    depth: i32,
    label: String,
    suffix: String,
}

const GROUPS: [(Kind, &str); 5] = [
    (Kind::Light, "Lights"),
    (Kind::Object, "Objects"),
    (Kind::Material, "Materials"),
    (Kind::Medium, "Media"),
    (Kind::Prototype, "Prototypes"),
];

/// Drag payloads: the kind in the high bits, the index below.
fn payload(kind: Kind, i: usize) -> u64 {
    ((kind as u64 + 1) << 32) | i as u64
}

fn unpack(p: u64) -> Option<(Kind, usize)> {
    let kind = match p >> 32 {
        1 => Kind::Light,
        2 => Kind::Object,
        3 => Kind::Material,
        4 => Kind::Medium,
        5 => Kind::Prototype,
        _ => return None,
    };
    Some((kind, (p & 0xFFFF_FFFF) as usize))
}

impl Editor {
    fn file_name(&self, file: usize) -> String {
        let p = &self.ws.doc.files[file].path;
        p.file_name().map_or_else(|| p.display().to_string(), |n| n.to_string_lossy().into_owned())
    }

    fn rows(&self, filter: &str) -> Vec<Row> {
        let filter = filter.to_lowercase();
        let matches = |s: &str| filter.is_empty() || s.to_lowercase().contains(&filter);
        let mut rows = Vec::new();
        let scene_id = id_of(ID, "scene");
        let sections: Vec<Row> = Section::ALL
            .iter()
            .filter_map(|&s| {
                let mut label = s.title().to_owned();
                if s == Section::Background {
                    let f = self.ws.doc.tree.background.unwrap_or(0);
                    let ty = self.ws.doc.value(f, &[Seg::from("background"), Seg::from("type")]);
                    let ty = ty.as_ref().and_then(Value::as_str).map(str::to_owned);
                    let file = self.ws.doc.value(f, &[Seg::from("background"), Seg::from("file")]);
                    let ty = ty.or_else(|| file.map(|_| "image".into())).unwrap_or_else(|| "color".into());
                    label = format!("Background ({ty})");
                }
                matches(&label).then(|| Row {
                    group: None,
                    target: Some(Target::Section(s)),
                    depth: 1,
                    label,
                    suffix: String::new(),
                })
            })
            .collect();
        if !sections.is_empty() {
            rows.push(Row {
                group: Some(scene_id),
                target: None,
                depth: 0,
                label: "Scene".into(),
                suffix: String::new(),
            });
            if self.ui.is_open(scene_id, true) {
                rows.extend(sections);
            }
        }
        for (kind, title) in GROUPS {
            let items = self.list(kind);
            let shown: Vec<(usize, &Item)> = items.iter().enumerate().filter(|(_, it)| matches(&it.label)).collect();
            if items.is_empty() || shown.is_empty() {
                continue;
            }
            let gid = id_of(ID, title);
            rows.push(Row {
                group: Some(gid),
                target: None,
                depth: 0,
                label: format!("{title} ({})", items.len()),
                suffix: String::new(),
            });
            if !self.ui.is_open(gid, true) {
                continue;
            }
            for (i, it) in shown {
                let suffix = if it.file == 0 { String::new() } else { self.file_name(it.file) };
                rows.push(Row {
                    group: None,
                    target: Some(Target::Item(kind, i)),
                    depth: 1,
                    label: it.label.clone(),
                    suffix,
                });
            }
        }
        rows
    }

    fn is_selected(&self, t: Target) -> bool {
        match t {
            Target::Item(kind @ (Kind::Object | Kind::Light), i) => {
                Editor::view_id(kind, i).is_some_and(|id| self.ws.sel.has_id(id))
            }
            _ => self.inspect == Some(t),
        }
    }

    fn is_hidden(&self, kind: Kind, i: usize) -> bool {
        Editor::view_id(kind, i).is_some_and(|id| self.ws.hidden.has_id(id))
    }

    /// Selects `t` (in the viewport too, for objects and lights) and shows it in the inspector.
    pub fn select_target(&mut self, t: Target, add: bool, toggle: bool) {
        if let Target::Item(kind, i) = t
            && let Some(id) = Editor::view_id(kind, i)
        {
            if !add && !toggle {
                self.ws.sel.clear();
            }
            let on = !(toggle && self.ws.sel.has_id(id));
            self.ws.sel.set_id(id, on);
        }
        self.inspect = Some(t);
    }

    fn toggle_hidden(&mut self, kind: Kind, i: usize) {
        if let Some(id) = Editor::view_id(kind, i) {
            let h = self.ws.hidden.has_id(id);
            self.ws.hidden.set_id(id, !h);
            if !h {
                self.ws.sel.set_id(id, false);
            }
            self.vp_dirty = true;
        }
    }

    /// Frames entry `i` of `kind` in the active view.
    fn frame_item(&mut self, kind: Kind, i: usize) {
        if let Some(id) = Editor::view_id(kind, i) {
            self.ws.sel.clear();
            self.ws.sel.set_id(id, true);
            self.ws.frame(self.ws.active);
            self.vp_dirty = true;
        }
    }

    /// Moves `from` (a light or object) to where `to` is, in their file.
    fn move_entry(&mut self, kind: Kind, from: usize, to: usize) {
        let (a, b) = (self.list(kind)[from].clone(), self.list(kind)[to].clone());
        if a.file != b.file {
            self.notify(format!("{} and {} are in different files", a.label, b.label));
            return;
        }
        let Some(Seg::Index(target)) = b.path.get(1).cloned() else { return };
        match self.ws.doc.move_item(&a, target) {
            Ok(true) => {
                self.scene_stale = true;
                self.ws.sel.clear();
                // The moved entry is where the target was.
                self.inspect = Some(Target::Item(kind, to));
                self.notify(format!("moved {}", a.label));
            }
            Ok(false) => {}
            Err(e) => self.notify(format!("error: {e}")),
        }
    }

    fn assign_material(&mut self, object: usize, material: usize) {
        let (obj, mat) = (self.list(Kind::Object)[object].clone(), self.list(Kind::Material)[material].clone());
        let mut path = obj.path.clone();
        path.push(Seg::from("material"));
        self.apply(obj.file, &path, &Value::String(mat.label.clone()), None, true);
        self.notify(format!("{} now uses {}", obj.label, mat.label));
    }

    fn rename_commit(&mut self, t: Target, text: &str) {
        let Target::Item(kind, i) = t else { return };
        let item = self.list(kind)[i].clone();
        match self.ws.doc.rename(&item, text) {
            Ok(true) => {
                self.scene_stale = true;
                self.notify(format!("renamed {} to {}", item.label, text.trim()));
            }
            Ok(false) => {}
            Err(e) => self.notify(format!("error: {e}")),
        }
    }

    #[expect(clippy::too_many_lines, reason = "the rows and everything they do, in drawing order")]
    pub fn outliner(&mut self, r: Rect) {
        self.ui.fill(r, self.ui.theme.panel);
        let rh = self.ui.row_h();
        let pad = self.ui.pad();
        let (head, body) = r.split_top(rh + 2 * pad);
        self.ui.fill(head, self.ui.theme.bar);
        let field = head.shrink2(pad, pad);
        let filter_id = id_of(ID, "filter");
        let filter = self.outliner_filter.clone();
        if let Some(f) = self.ui.text_field(filter_id, field, &filter, "Filter") {
            self.outliner_filter = f;
        }
        // Filter while typing, not only once the text is committed.
        if let Some(e) = self.ui.edit.as_ref().filter(|e| e.id == filter_id) {
            self.outliner_filter = e.te.text();
        }
        let rows = self.rows(&self.outliner_filter.clone());
        let sid = id_of(ID, "scroll");
        let (scroll, mut col) = self.ui.begin_scroll(sid, body);
        let row_ids: Vec<Id> = rows.iter().map(|row| id_of(ID, (row.group, row.target))).collect();
        let mut nav: Option<isize> = None;
        let mut focused_row = None;
        for (k, row) in rows.iter().enumerate() {
            let rr = col.row(rh);
            if rr.bottom() < body.y || rr.y > body.bottom() {
                continue;
            }
            let id = row_ids[k];
            if let Some(gid) = row.group {
                let open = self.ui.is_open(gid, true);
                let t = self.ui.tree_row(id, rr, 0, Some(open), false, &row.label, "", self.ui.theme.text_dim);
                if t.toggled || t.resp.clicked {
                    self.ui.set_open(gid, !open);
                }
                continue;
            }
            let Some(target) = row.target else { continue };
            let name = target_probe(target);
            self.probe(format!("outliner:{name}:{}", row.label), rr);
            // Objects and lights: a show / hide toggle at the right.
            let mut main = rr;
            if let Target::Item(kind @ (Kind::Object | Kind::Light), i) = target {
                let (eye, rest) = rr.split_right(rh, 0);
                main = rest;
                let hidden = self.is_hidden(kind, i);
                if self.is_selected(target) {
                    self.ui.fill(eye, self.ui.theme.row_selected);
                }
                let er = self.ui.interact(id_of(id, "eye"), eye, false);
                let c = if er.hovered { self.ui.theme.text } else { self.ui.theme.text_disabled };
                let s = self.ui.px(9.0);
                let dot = Rect::new(eye.x + (eye.w - s) / 2, eye.y + (eye.h - s) / 2, s, s);
                if hidden {
                    self.ui.border(dot, c, s / 2);
                } else {
                    self.ui.fill_rounded(dot, c, 255, s / 2);
                }
                self.ui.tooltip(&er, if hidden { "show in the viewport" } else { "hide in the viewport" });
                if er.clicked {
                    self.toggle_hidden(kind, i);
                }
            }
            if self.renaming == Some(target) {
                let fid = id_of(id, "rename");
                if !self.rename_focused {
                    self.ui.focus = fid;
                    self.rename_focused = true;
                }
                let indent = self.ui.px(14.0) + self.ui.px(16.0);
                let fr = Rect::new(main.x + indent, main.y + 1, main.w - indent - 2, main.h - 2);
                if let Some(text) = self.ui.text_field(fid, fr, &row.label, "name") {
                    self.rename_commit(target, &text);
                }
                if self.ui.focus != fid {
                    self.renaming = None;
                }
                continue;
            }
            let (kind_i, hidden) = match target {
                Target::Item(kind, i) => (Some((kind, i)), self.is_hidden(kind, i)),
                Target::Section(_) => (None, false),
            };
            let color = if hidden { self.ui.theme.text_disabled } else { self.ui.theme.text };
            let selected = self.is_selected(target);
            let t = self.ui.tree_row(id, main, row.depth, None, selected, &row.label, &row.suffix, color);
            let resp = t.resp;
            if !row.suffix.is_empty() {
                let path =
                    kind_i.map(|(kind, i)| self.ws.doc.files[self.list(kind)[i].file].path.display().to_string());
                if let Some(p) = path {
                    self.ui.tooltip(&resp, &format!("from {p}"));
                }
            }
            if resp.pressed {
                let m = self.ui.input.mods;
                self.select_target(target, m.shift, m.ctrl);
            }
            if resp.double_clicked && kind_i.is_some() {
                self.renaming = Some(target);
                self.rename_focused = false;
            }
            if resp.focused {
                focused_row = Some(k);
                if self.ui.take_key(Key::Up) {
                    nav = Some(-1);
                } else if self.ui.take_key(Key::Down) {
                    nav = Some(1);
                }
            }
            if let Some((kind, i)) = kind_i {
                // Drag to reorder (lights, objects) or to assign (materials).
                if matches!(kind, Kind::Light | Kind::Object | Kind::Material) {
                    self.ui.drag_source(&resp, payload(kind, i), &row.label);
                }
                if let Some(p) = self.ui.drag_over(main).and_then(unpack)
                    && p != (kind, i)
                {
                    let assign = p.0 == Kind::Material && kind == Kind::Object;
                    let reorder = p.0 == kind && matches!(kind, Kind::Light | Kind::Object);
                    if assign {
                        self.ui.border(main, self.ui.theme.drop_target, 0);
                    } else if reorder {
                        self.ui.fill(Rect::new(main.x, main.y, main.w, self.ui.px(2.0)), self.ui.theme.drop_target);
                    }
                    if self.ui.dropped(main).is_some() {
                        if assign {
                            self.assign_material(i, p.1);
                        } else if reorder {
                            self.move_entry(kind, p.1, i);
                        }
                    }
                }
                let menu_id = id_of(id, "menu");
                if let Some(mut f) = self.ui.context_menu(menu_id, &resp) {
                    if self.ui.menu_item(&mut f, "Rename", "F2", true) {
                        self.inspect = Some(target);
                        self.renaming = Some(target);
                        self.rename_focused = false;
                    }
                    if matches!(kind, Kind::Light | Kind::Object) {
                        let label = if hidden { "Show" } else { "Hide" };
                        if self.ui.menu_item(&mut f, label, "", true) {
                            self.toggle_hidden(kind, i);
                        }
                        if self.ui.menu_item(&mut f, "Frame", "", !hidden) {
                            self.frame_item(kind, i);
                        }
                        self.ui.menu_separator(&mut f);
                        let n = self.list(kind).len();
                        let same_file = |ed: &Editor, j: usize| ed.list(kind)[j].file == ed.list(kind)[i].file;
                        let up = i > 0 && same_file(self, i - 1);
                        let down = i + 1 < n && same_file(self, i + 1);
                        if self.ui.menu_item(&mut f, "Move Up", "", up) {
                            self.move_entry(kind, i, i - 1);
                        }
                        if self.ui.menu_item(&mut f, "Move Down", "", down) {
                            self.move_entry(kind, i, i + 1);
                        }
                    }
                    self.ui.end_popup(f);
                }
            }
        }
        if rows.is_empty() {
            let rr = col.row(rh);
            self.ui.label(rr, "nothing matches", self.ui.theme.text_disabled);
        }
        let bottom = col.y;
        self.ui.end_scroll(scroll, bottom);
        // Up / Down walk the entries (group headers are skipped).
        if let (Some(d), Some(k)) = (nav, focused_row) {
            let mut j = k as isize + d;
            while j >= 0 && (j as usize) < rows.len() && rows[j as usize].target.is_none() {
                j += d;
            }
            if j >= 0
                && let Some(row) = rows.get(j as usize)
                && let Some(t) = row.target
            {
                self.select_target(t, false, false);
                self.ui.focus = row_ids[j as usize];
                let y = j as i32 * rh;
                self.ui.scroll_to(sid, body.h, y, y + rh);
            }
        }
    }
}

/// A stable name of a target for probes.
fn target_probe(t: Target) -> &'static str {
    match t {
        Target::Section(s) => s.key(),
        Target::Item(Kind::Light, _) => "light",
        Target::Item(Kind::Object, _) => "object",
        Target::Item(Kind::Material, _) => "material",
        Target::Item(Kind::Medium, _) => "medium",
        Target::Item(Kind::Prototype, _) => "prototype",
    }
}
