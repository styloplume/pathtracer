//! The inspector: every key the schema knows for the selected item or settings section,
//! with a widget for its type. Keys that are not set show their default greyed out;
//! changing one writes it to the file, and x removes it again. Aliases show under their
//! canonical name and are edited where they are written. Validation problems show under
//! the field they are about; keys the format does not know are listed with a warning.

use crate::editor::{Editor, Section, Target};
use crate::ui::{Column, Id, NumOpts, Rect, id_of};
use pt_core::document::Kind;
use pt_core::schema::{self, Choice, DefaultValue, Key, Schema, Ty};
use pt_core::toml::{Seg, Table, Value, format_value};
use std::fmt::Write as _;

const ID: Id = 0x0069_6e73_7065_6374;

/// What a field asks for.
enum Edit {
    None,
    Set {
        value: Value,
        merge: bool,
        done: bool,
    },
    /// A drag ended without a change this frame: close its undo step.
    Seal,
    Remove,
    /// Open the file browser for this key, for files with these extensions.
    Pick(&'static [&'static str]),
}

/// The table being inspected and where its keys are written.
#[derive(Clone)]
struct Ctx {
    schema: &'static Schema,
    /// The file keys go to.
    file: usize,
    /// Keys written in another file (a section's settings from an included file).
    key_files: Vec<(String, usize)>,
    base: Vec<Seg>,
    table: Table,
    id: Id,
}

impl Ctx {
    fn file_of(&self, key: &str) -> usize {
        self.key_files.iter().find(|(k, _)| k == key).map_or(self.file, |&(_, f)| f)
    }

    fn path(&self, key: &str) -> Vec<Seg> {
        let mut p = self.base.clone();
        p.push(Seg::from(key));
        p
    }
}

pub const IMAGES: &[&str] = &["png", "jpg", "jpeg", "hdr", "pfm", "exr", "tga", "bmp", "ppm", "pgm"];
pub const MESHES: &[&str] = &["obj", "ply", "stl", "gltf", "glb"];
const ANY: &[&str] = &[];

/// The files a path key takes.
fn exts_for(schema: &Schema, key: &str) -> &'static [&'static str] {
    match (schema.name, key) {
        ("[[objects]]" | "[prototypes.NAME]", "file") => MESHES,
        (_, "grid") => ANY,
        _ => IMAGES,
    }
}

/// A default as a value, when it is one.
fn default_value(d: DefaultValue, n: usize) -> Option<Value> {
    match d {
        DefaultValue::Number(x) => Some(Value::Number(x)),
        DefaultValue::Bool(b) => Some(Value::Bool(b)),
        DefaultValue::Str(s) => Some(Value::String(s.into())),
        DefaultValue::Vec3(v) => Some(Value::Array(v.iter().take(n.max(3)).map(|&x| Value::Number(x)).collect())),
        // "[-1, 0.2, 0, 0]" and the like read as values.
        DefaultValue::Derived(text) => {
            pt_core::toml::parse(&format!("v = {text}")).ok().and_then(|t| t.get("v").cloned())
        }
        DefaultValue::None | DefaultValue::Required => None,
    }
}

fn default_text(d: DefaultValue) -> String {
    match d {
        DefaultValue::None => "not set".into(),
        DefaultValue::Required => "required".into(),
        DefaultValue::Derived(t) => t.into(),
        other => default_value(other, 3).map(|v| format_value(&v)).unwrap_or_default(),
    }
}

/// The numbers of a vector value: an array, or one number for every component.
fn numbers(v: &Value, n: usize) -> Option<Vec<f64>> {
    match v {
        Value::Number(x) => Some(vec![*x; n]),
        Value::Array(a) if a.len() == n => a.iter().map(Value::as_f64).collect(),
        _ => None,
    }
}

fn canonical<'a>(choices: &'a [Choice], s: &str) -> Option<&'a Choice> {
    let s = s.to_ascii_lowercase();
    choices.iter().find(|c| c.name == s || c.aliases.contains(&s.as_str()))
}

fn opts_of(key: &Key) -> NumOpts {
    let mut o = NumOpts { integer: matches!(key.ty, Ty::Integer), ..NumOpts::default() };
    if let Some(r) = key.range {
        // Open bounds: stay just inside.
        let nudge = |v: f64| if v.abs() > 1.0 { v.abs() * 1e-6 } else { 1e-4 };
        o.min = if r.min_open { r.min + nudge(r.min) } else { r.min };
        o.max = if r.max_open { r.max - nudge(r.max) } else { r.max };
    }
    if matches!(key.ty, Ty::Integer) && o.min < 0.0 {
        o.min = 0.0;
    }
    o
}

impl Editor {
    /// The context of the inspected target: schema, file, path and current table.
    fn target_ctx(&self, t: Target) -> Option<(Ctx, String, String)> {
        let doc = &self.ws.doc;
        Some(match t {
            Target::Section(s) => {
                let sch: &'static Schema = match s {
                    Section::Camera => &schema::CAMERA,
                    Section::Render => &schema::RENDER,
                    Section::Output => &schema::OUTPUT,
                    Section::Background => &schema::BACKGROUND,
                };
                let base = vec![Seg::from(s.key())];
                let (table, file, key_files) = if s == Section::Background {
                    let f = doc.tree.background.unwrap_or(0);
                    let t = doc.value(f, &base).and_then(|v| v.as_table().cloned()).unwrap_or_default();
                    (t, f, Vec::new())
                } else {
                    let mut t = Table::new();
                    let mut kf = Vec::new();
                    for st in doc.tree.settings.iter().filter(|st| st.section == s.key()) {
                        if let Some(v) = doc.value(st.file, &[Seg::from(s.key()), Seg::from(st.key.as_str())]) {
                            t.insert(st.key.clone(), v);
                            kf.push((st.key.clone(), st.file));
                        }
                    }
                    (t, 0, kf)
                };
                let where_ = if file == 0 { String::new() } else { doc.files[file].path.display().to_string() };
                (Ctx { schema: sch, file, key_files, base, table, id: id_of(ID, s) }, s.title().to_owned(), where_)
            }
            Target::Item(kind, i) => {
                let item = self.list(kind).get(i)?.clone();
                let sch: &'static Schema = match kind {
                    Kind::Light => &schema::LIGHT,
                    Kind::Object => &schema::OBJECT,
                    Kind::Material => &schema::MATERIAL,
                    Kind::Medium => &schema::MEDIUM,
                    Kind::Prototype => &schema::PROTOTYPE,
                };
                let table = doc.item_table(&item)?;
                let what = match kind {
                    Kind::Light => "light",
                    Kind::Object => "object",
                    Kind::Material => "material",
                    Kind::Medium => "medium",
                    Kind::Prototype => "prototype",
                };
                let where_ = format!("{what} in {}", doc.files[item.file].path.display());
                let id = id_of(ID, (kind, &item.path));
                (
                    Ctx { schema: sch, file: item.file, key_files: Vec::new(), base: item.path.clone(), table, id },
                    item.label,
                    where_,
                )
            }
        })
    }

    pub fn inspector(&mut self, r: Rect) {
        self.ui.fill(r, self.ui.theme.panel);
        self.ui.fill(Rect::new(r.x, r.y, 1, r.h), self.ui.theme.separator);
        let rh = self.ui.row_h();
        let pad = self.ui.pad();
        let Some(t) = self.inspect else {
            let mut col = Column::new(r.shrink(pad), 0);
            let row = col.row(rh);
            self.ui.label(row, "Select something to inspect it.", self.ui.theme.text_disabled);
            return;
        };
        let Some((ctx, title, where_)) = self.target_ctx(t) else {
            self.inspect = None;
            return;
        };
        let (head, body) = r.split_top(rh * 2 + pad);
        self.ui.fill(head, self.ui.theme.bar);
        let mut hc = Column::new(head.shrink2(pad, pad / 2), 0);
        let tr = hc.row(rh);
        self.ui.label(tr, &title, self.ui.theme.text);
        let wr = hc.row(rh);
        self.ui.label(wr, &where_, self.ui.theme.text_disabled);
        let sid = id_of(ctx.id, "scroll");
        let (scroll, mut col) = self.ui.begin_scroll(sid, body.shrink2(0, self.ui.px(2.0)));
        col.indent(pad);
        col.w -= pad / 2;
        col.gap = self.ui.px(3.0);
        self.inspector_extras(&mut col, t, &ctx);
        self.inspect_table(&mut col, &ctx, 0);
        col.space(rh);
        let bottom = col.y;
        self.ui.end_scroll(scroll, bottom);
    }

    /// What the inspector shows above the keys: a material's preview, the presets of
    /// `[render]` and `[output]`, the camera's tools.
    fn inspector_extras(&mut self, col: &mut Column, t: Target, ctx: &Ctx) {
        let rh = self.ui.row_h();
        let gap = self.ui.px(6.0);
        let label_w = ((col.w as f32 * 0.38) as i32).clamp(self.ui.px(70.0), self.ui.px(150.0));
        let dir = self.ws.doc.files[ctx.file].path.parent().map(std::path::Path::to_path_buf).unwrap_or_default();
        let material = match t {
            Target::Item(Kind::Material, _) => Some(ctx.table.clone()),
            Target::Item(Kind::Object, _) => ctx.table.get("material").and_then(Value::as_table).cloned(),
            _ => None,
        };
        if let Some(m) = material {
            let size = col.w.min(self.ui.px(150.0));
            let r = col.row(size);
            let pr = Rect::new(r.x + (r.w - size) / 2, r.y, size, size);
            self.probe("inspector:preview", pr);
            self.preview_tile(pr, &m, &dir);
            col.space(self.ui.px(4.0));
        }
        let combo = |ed: &mut Editor, col: &mut Column, label: &str, names: Vec<String>| -> Option<usize> {
            let r = col.row(rh);
            let (lr, vr) = r.split_left(label_w, gap);
            ed.ui.label(lr, label, ed.ui.theme.accent);
            ed.probe(format!("inspector:preset {label}"), vr);
            ed.ui.combo(id_of(ID, ("preset", label)), vr, "choose…", &names, ed.ui.theme.text_dim)
        };
        match t {
            Target::Section(Section::Render) => {
                let names = crate::edit::RENDER_PRESETS.iter().map(|p| p.0.to_owned()).collect();
                if let Some(i) = combo(self, col, "quality preset", names) {
                    self.render_preset(i);
                }
                let names = crate::edit::SIZE_PRESETS.iter().map(|p| p.0.to_owned()).collect();
                if let Some(i) = combo(self, col, "size preset", names) {
                    self.size_preset(i);
                }
                col.space(self.ui.px(4.0));
            }
            Target::Section(Section::Output) => {
                let names = crate::edit::OUTPUT_PRESETS.iter().map(|p| p.0.to_owned()).collect();
                if let Some(i) = combo(self, col, "preset", names) {
                    self.output_preset(i);
                }
                col.space(self.ui.px(4.0));
            }
            Target::Section(Section::Camera) => {
                let r = col.row(rh);
                let cells = r.columns(3, self.ui.px(4.0));
                let through = if self.ws.through_camera { "Leave View" } else { "Look Through" };
                if self.ui.button(id_of(ID, "cam-through"), cells[0], through) {
                    self.run(crate::editor::Cmd::CameraView);
                }
                let lock = if self.ws.lock_camera { "Unlock" } else { "Lock to View" };
                if self.ui.button(id_of(ID, "cam-lock"), cells[1], lock) {
                    self.run(crate::editor::Cmd::LockCamera);
                }
                self.probe("inspector:pick focus", cells[2]);
                if self.ui.button(id_of(ID, "cam-focus"), cells[2], "Pick Focus") {
                    self.run(crate::editor::Cmd::PickFocus);
                }
                let r = col.row(rh);
                let cam = &self.ws.scene.camera;
                let fd = cam.focus_distance.unwrap_or_else(|| (cam.look_at - cam.position).length());
                let mm = 12.0 / (cam.fov.to_radians() * 0.5).tan();
                let text = format!("{mm:.0} mm (24 mm sensor) · focus at {fd:.3} · aperture {:.3}", cam.aperture);
                self.ui.label(r, &text, self.ui.theme.text_dim);
                col.space(self.ui.px(4.0));
            }
            _ => {}
        }
    }

    /// The rows of a table: its type, its keys, and keys the format does not know.
    fn inspect_table(&mut self, col: &mut Column, ctx: &Ctx, depth: i32) {
        let rh = self.ui.row_h();
        let gap = self.ui.px(6.0);
        let label_w = ((col.w as f32 * 0.38) as i32).clamp(self.ui.px(70.0), self.ui.px(150.0));
        let sch = ctx.schema;
        let variant = sch.variant_of(&ctx.table);
        // Problems of the table itself.
        for d in schema::check_entries(sch, &ctx.table) {
            if d.message.starts_with("unknown key") || d.message.starts_with("missing '") {
                continue; // shown at their rows
            }
            let r = col.row(rh);
            self.ui.label(r, &d.message, self.ui.theme.error);
        }
        if !sch.variants.is_empty() {
            let r = col.row(rh);
            let (lr, vr) = r.split_left(label_w, gap);
            self.ui.label(lr, "type", self.ui.theme.text);
            let names: Vec<String> = sch.variants.iter().map(|v| v.name.to_owned()).collect();
            let written = ctx.table.get("type").and_then(Value::as_str);
            let current = match (written, variant) {
                (_, Some(v)) => v.name.to_owned(),
                (Some(s), None) => s.to_owned(),
                (None, None) => String::new(),
            };
            let color = if written.is_some() { self.ui.theme.text } else { self.ui.theme.text_default };
            self.probe(probe_name(ctx, "type"), vr);
            if let Some(i) = self.ui.combo(id_of(ctx.id, "type"), vr, &current, &names, color) {
                let path = ctx.path("type");
                self.apply_key(ctx.file_of("type"), &path, Value::String(names[i].clone()), None, true);
            }
            if let Some(v) = variant
                && !v.doc.is_empty()
            {
                let r = col.row(rh);
                self.ui.label(r, v.doc, self.ui.theme.text_dim);
            }
        }
        let keys: Vec<&'static Key> = sch.keys(variant).filter(|k| k.name != "type").collect();
        for key in keys {
            self.key_row(col, ctx, key, label_w, depth);
        }
        // Keys the format does not know.
        let known = |k: &str| k.starts_with('$') || k == "type" || sch.key(variant, k).is_some();
        let unknown: Vec<(String, Value)> =
            ctx.table.iter().filter(|(k, _)| !known(k)).map(|(k, v)| (k.clone(), v.clone())).collect();
        if !unknown.is_empty() && (variant.is_some() || sch.variants.is_empty()) {
            let diags = schema::check_entries(sch, &ctx.table);
            for (k, v) in unknown {
                let r = col.row(rh);
                let (lr, vr) = r.split_left(label_w, gap);
                let (xr, vr) = vr.split_right(rh, 0);
                let lresp = self.ui.interact(id_of(ctx.id, ("unknown", &k)), lr, false);
                self.ui.label(lr, &k, self.ui.theme.warning);
                let msg = diags.iter().find(|d| d.message.contains(&format!("'{k}'"))).map(|d| d.message.clone());
                self.ui.tooltip(&lresp, msg.as_deref().unwrap_or("not a key of the format (ignored)"));
                self.ui.label(vr, &format_value(&v), self.ui.theme.text_dim);
                if self.ui.flat_button(id_of(ctx.id, ("remove", &k)), xr, "x", self.ui.theme.text_dim).clicked {
                    self.remove(ctx.file_of(&k), &ctx.path(&k));
                }
            }
        }
    }

    fn key_row(&mut self, col: &mut Column, ctx: &Ctx, key: &'static Key, label_w: i32, depth: i32) {
        let rh = self.ui.row_h();
        let gap = self.ui.px(6.0);
        let present = std::iter::once(key.name)
            .chain(key.aliases.iter().copied())
            .find_map(|n| ctx.table.get(n).map(|v| (n.to_owned(), v.clone())));
        let written_as = present.as_ref().map_or(key.name, |(n, _)| n.as_str()).to_owned();
        let file = ctx.file_of(&written_as);
        let path = ctx.path(&written_as);
        let id = id_of(ctx.id, key.name);
        let r = col.row(rh);
        let (lr, vr) = r.split_left(label_w, gap);
        let (xr, vr) = vr.split_right(rh, 0);
        // The label, with the key's documentation.
        let lresp = self.ui.interact(id_of(id, "label"), lr, false);
        let tc = if present.is_some() { self.ui.theme.text } else { self.ui.theme.text_dim };
        self.ui.label(lr, key.name, tc);
        let mut tip = key.doc.to_owned();
        if let Some(rg) = key.range {
            let _ = write!(tip, "\nrange {rg}");
        }
        let _ = write!(tip, "\ndefault: {}", default_text(key.default));
        if !key.aliases.is_empty() {
            let _ = write!(tip, "\nalso written: {}", key.aliases.join(", "));
        }
        if file != 0 {
            let _ = write!(tip, "\nin {}", self.ws.doc.files[file].path.display());
        }
        self.ui.tooltip(&lresp, &tip);
        self.probe(probe_name(ctx, key.name), vr);
        let value = present.as_ref().map(|(_, v)| v.clone());
        let edit = self.value_widget(id, vr, key, value.as_ref(), col, ctx, &path, depth);
        if present.is_some() {
            let x = self.ui.flat_button(id_of(id, "reset"), xr, "x", self.ui.theme.text_disabled);
            self.ui.tooltip(&x, "remove: back to the default");
            if x.clicked {
                self.remove(file, &path);
            }
        }
        match edit {
            Edit::None => {}
            Edit::Set { value, merge, done } => self.apply_key(file, &path, value, merge.then_some(id), done),
            Edit::Seal => self.ws.doc.seal(),
            Edit::Remove => self.remove(file, &path),
            Edit::Pick(exts) => {
                let current = value.as_ref().and_then(Value::as_str).map(str::to_owned);
                self.pick_file(file, path.clone(), exts, current.as_deref());
            }
        }
        // Problems with this key.
        let mut problems: Vec<String> = Vec::new();
        if let Some(v) = &value {
            for d in schema::check_key(key, v) {
                problems.push(if d.path == "scene" { d.message } else { d.to_string() });
            }
        } else if key.default == DefaultValue::Required && !ctx.table.is_empty() {
            problems.push(format!("'{}' is required", key.name));
        }
        let lh = self.ui.font.line_height();
        for p in problems {
            let indent = self.ui.px(10.0);
            let lines = self.ui.wrap(&p, col.w - indent - self.ui.pad());
            let r = col.row(lh * lines.len() as i32 + self.ui.px(4.0));
            for (k, line) in lines.iter().enumerate() {
                self.ui.text(r.x + indent, r.y + self.ui.px(2.0) + k as i32 * lh, line, self.ui.theme.error);
            }
        }
    }

    /// The widget for `key`'s value; nested tables add their rows to `col`.
    #[expect(clippy::too_many_arguments, reason = "the row's whole context")]
    #[expect(clippy::too_many_lines, reason = "one arm per value type of the schema")]
    fn value_widget(
        &mut self,
        id: Id,
        r: Rect,
        key: &'static Key,
        value: Option<&Value>,
        col: &mut Column,
        ctx: &Ctx,
        path: &[Seg],
        depth: i32,
    ) -> Edit {
        let set = value.is_some();
        let color = if set { self.ui.theme.text } else { self.ui.theme.text_default };
        let rh = self.ui.row_h();
        match key.ty {
            Ty::Number | Ty::Integer => {
                let o = opts_of(key);
                let current =
                    value.and_then(Value::as_f64).or_else(|| default_value(key.default, 1).and_then(|v| v.as_f64()));
                self.number_field(id, r, current, &o, color, key)
            }
            Ty::Bool => {
                let on = value.and_then(Value::as_bool).or(match key.default {
                    DefaultValue::Bool(b) => Some(b),
                    _ => None,
                });
                match self.ui.checkbox(id, r, on.unwrap_or(false), if set { "" } else { "(default)" }) {
                    Some(b) => Edit::Set { value: Value::Bool(b), merge: false, done: true },
                    None => Edit::None,
                }
            }
            Ty::String => self.string_field(id, r, value, &default_text(key.default)),
            Ty::Path => {
                let (br, fr) = r.split_right(rh, self.ui.px(3.0));
                let e = self.string_field(id, fr, value, &default_text(key.default));
                if self.ui.button(id_of(id, "pick"), br, "…") {
                    return Edit::Pick(exts_for(ctx.schema, key.name));
                }
                e
            }
            Ty::Ref(kind) => {
                let list = match kind {
                    schema::RefKind::Medium => &self.ws.doc.tree.media,
                    schema::RefKind::Prototype => &self.ws.doc.tree.prototypes,
                };
                let mut names: Vec<String> = vec!["(none)".into()];
                names.extend(list.iter().map(|i| i.label.clone()));
                let current = value.and_then(Value::as_str).unwrap_or("(none)").to_owned();
                match self.ui.combo(id, r, &current, &names, color) {
                    Some(0) => Edit::Remove,
                    Some(i) => Edit::Set { value: Value::String(names[i].clone()), merge: false, done: true },
                    None => Edit::None,
                }
            }
            Ty::Choice(choices) => {
                let names: Vec<String> = choices.iter().map(|c| c.name.to_owned()).collect();
                let current = value
                    .and_then(Value::as_str)
                    .map(|s| canonical(choices, s).map_or_else(|| s.to_owned(), |c| c.name.to_owned()))
                    .or_else(|| default_value(key.default, 1).and_then(|v| v.as_str().map(str::to_owned)))
                    .unwrap_or_default();
                match self.ui.combo(id, r, &current, &names, color) {
                    Some(i) => Edit::Set { value: Value::String(names[i].clone()), merge: false, done: true },
                    None => Edit::None,
                }
            }
            Ty::Choices(choices) => {
                let on: Vec<String> = match value {
                    Some(Value::String(s)) => vec![s.clone()],
                    Some(Value::Array(a)) => a.iter().filter_map(Value::as_str).map(str::to_owned).collect(),
                    _ => match key.default {
                        DefaultValue::Str(s) => vec![s.to_owned()],
                        _ => Vec::new(),
                    },
                };
                let on: Vec<&str> = on.iter().filter_map(|s| canonical(choices, s)).map(|c| c.name).collect();
                let mut out = Edit::None;
                for (i, cell) in r.columns(choices.len(), self.ui.px(2.0)).into_iter().enumerate() {
                    let c = choices[i].name;
                    if let Some(b) = self.ui.checkbox(id_of(id, c), cell, on.contains(&c), c) {
                        let mut new: Vec<&str> = choices
                            .iter()
                            .map(|x| x.name)
                            .filter(|&n| if n == c { b } else { on.contains(&n) })
                            .collect();
                        new.dedup();
                        out = if new.is_empty() {
                            Edit::Remove
                        } else {
                            Edit::Set {
                                value: Value::Array(new.into_iter().map(|s| Value::String(s.into())).collect()),
                                merge: false,
                                done: true,
                            }
                        };
                    }
                }
                out
            }
            Ty::Strings => {
                let shown = match value {
                    Some(Value::String(s)) => s.clone(),
                    Some(Value::Array(a)) => a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", "),
                    _ => String::new(),
                };
                match self.ui.text_field(id, r, &shown, "comma-separated") {
                    Some(t) => {
                        let parts: Vec<Value> = t
                            .split(',')
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                            .map(|s| Value::String(s.into()))
                            .collect();
                        match parts.len() {
                            0 => Edit::Remove,
                            1 => Edit::Set { value: parts[0].clone(), merge: false, done: true },
                            _ => Edit::Set { value: Value::Array(parts), merge: false, done: true },
                        }
                    }
                    None => Edit::None,
                }
            }
            Ty::Vec2 | Ty::Pair | Ty::Vec3 | Ty::Vec4 | Ty::Color => {
                let n = match key.ty {
                    Ty::Vec2 | Ty::Pair => 2,
                    Ty::Vec4 => 4,
                    _ => 3,
                };
                self.vector_field(id, r, key, value, n, color)
            }
            Ty::ColorTexture | Ty::ScalarTexture => {
                let color_kind = matches!(key.ty, Ty::ColorTexture);
                let (mr, vr) = r.split_right(rh, self.ui.px(3.0));
                let mut out = match value {
                    Some(Value::Table(t)) => {
                        self.ui.label(
                            vr,
                            &format!("texture: {}", t.get("type").and_then(Value::as_str).unwrap_or("?")),
                            color,
                        );
                        Edit::None
                    }
                    Some(Value::String(s)) => {
                        let (br, fr) = vr.split_right(rh, self.ui.px(3.0));
                        // The image's thumbnail before its name.
                        let (tr, fr) = fr.split_left(rh, self.ui.px(3.0));
                        let base =
                            self.ws.doc.files[ctx.file_of(key.name)].path.parent().map(std::path::Path::to_path_buf);
                        let file = base.unwrap_or_default().join(s);
                        self.probe(probe_name(ctx, &format!("{} thumbnail", key.name)), tr);
                        self.image_tile(tr, &file);
                        let e = self.string_field(id, fr, value, "image file");
                        if self.ui.button(id_of(id, "pick"), br, "…") {
                            return Edit::Pick(IMAGES);
                        }
                        e
                    }
                    _ if color_kind => self.vector_field(id, vr, key, value, 3, color),
                    _ => {
                        let current = value
                            .and_then(Value::as_f64)
                            .or_else(|| default_value(key.default, 1).and_then(|v| v.as_f64()));
                        self.number_field(id, vr, current, &NumOpts::default(), color, key)
                    }
                };
                // How the value is given: a value, an image, or a procedural texture.
                let pid = id_of(id, "form");
                let fb = self.ui.flat_button(id_of(id, "form-button"), mr, "", self.ui.theme.text_dim);
                self.ui.arrow(mr, true, self.ui.theme.text_dim);
                self.ui.tooltip(&fb, "a value, an image file, or a procedural texture");
                if fb.clicked {
                    self.ui.open_popup(pid, mr.x, mr.bottom(), mr);
                }
                if let Some(mut f) = self.ui.begin_popup(pid) {
                    let plain = if color_kind { "Color" } else { "Number" };
                    if self.ui.menu_item(&mut f, plain, "", true) {
                        let d =
                            default_value(key.default, 3).filter(|v| !matches!(v, Value::Table(_) | Value::String(_)));
                        out = Edit::Set {
                            value: d.unwrap_or(Value::Number(if color_kind { 0.8 } else { 0.5 })),
                            merge: false,
                            done: true,
                        };
                    }
                    if self.ui.menu_item(&mut f, "Image File…", "", true) {
                        out = Edit::Pick(IMAGES);
                    }
                    if self.ui.menu_item(&mut f, "Procedural Texture", "", true) {
                        let mut t = Table::new();
                        t.insert("type".into(), Value::String("checker".into()));
                        out = Edit::Set { value: Value::Table(t), merge: false, done: true };
                    }
                    self.ui.end_popup(f);
                }
                if let Some(Value::Table(t)) = value {
                    self.nested(col, ctx, id, key.name, &schema::TEXTURE, t, depth, path);
                }
                out
            }
            Ty::Material => {
                let mut names: Vec<String> = vec!["default".into(), "(inline)".into()];
                names.extend(self.ws.doc.tree.materials.iter().map(|m| m.label.clone()));
                let current = match value {
                    Some(Value::String(s)) => s.clone(),
                    Some(Value::Table(_)) => "(inline)".into(),
                    _ => "default".into(),
                };
                let out = match self.ui.combo(id, r, &current, &names, color) {
                    Some(0) => Edit::Remove,
                    Some(1) => {
                        let mut t = Table::new();
                        t.insert("type".into(), Value::String("diffuse".into()));
                        Edit::Set { value: Value::Table(t), merge: false, done: true }
                    }
                    Some(i) => Edit::Set { value: Value::String(names[i].clone()), merge: false, done: true },
                    None => Edit::None,
                };
                if let Some(Value::Table(t)) = value {
                    self.nested(col, ctx, id, key.name, &schema::MATERIAL, t, depth, path);
                }
                out
            }
            Ty::Table(sub) => match value {
                Some(Value::Table(t)) => {
                    self.ui.label(r, &format!("{} keys", t.len()), self.ui.theme.text_dim);
                    self.nested(col, ctx, id, key.name, sub, t, depth, path);
                    Edit::None
                }
                Some(other) => {
                    self.ui.label(r, &format_value(other), self.ui.theme.error);
                    Edit::None
                }
                None => {
                    if self.ui.button(id, r, "Add") {
                        Edit::Set { value: Value::Table(Table::new()), merge: false, done: true }
                    } else {
                        Edit::None
                    }
                }
            },
            Ty::Tables(sub) => {
                if let Some(Value::Array(items)) = value {
                    self.ui.label(r, &format!("{} entries", items.len()), self.ui.theme.text_dim);
                    let open_id = id_of(id, "open");
                    let header = col.row(rh);
                    let header = Rect { x: header.x + self.ui.px(10.0), w: header.w - self.ui.px(10.0), ..header };
                    if self.ui.collapsing(open_id, header, key.name, false) {
                        for (i, item) in items.iter().enumerate() {
                            let Some(t) = item.as_table() else { continue };
                            let mut base = path.to_vec();
                            base.push(Seg::Index(i));
                            let sub_ctx = Ctx {
                                schema: sub,
                                file: ctx.file_of(key.name),
                                key_files: Vec::new(),
                                base,
                                table: t.clone(),
                                id: id_of(id, ("item", i)),
                            };
                            let h = col.row(rh);
                            let h = Rect { x: h.x + self.ui.px(16.0), w: h.w - self.ui.px(16.0), ..h };
                            let label = format!("#{} {}", i + 1, t.get("type").and_then(Value::as_str).unwrap_or(""));
                            if self.ui.collapsing(id_of(sub_ctx.id, "open"), h, &label, false) {
                                let indent = self.ui.px(16.0);
                                col.indent(indent);
                                self.inspect_table(col, &sub_ctx, depth + 1);
                                col.indent(-indent);
                            }
                        }
                    }
                    Edit::None
                } else {
                    self.ui.label(r, "none", self.ui.theme.text_disabled);
                    Edit::None
                }
            }
            Ty::Named(_) => Edit::None,
        }
    }

    /// The rows of a nested table, in a collapsible section under its key.
    #[expect(clippy::too_many_arguments, reason = "the row's whole context")]
    fn nested(
        &mut self,
        col: &mut Column,
        ctx: &Ctx,
        id: Id,
        name: &str,
        sub: &'static Schema,
        t: &Table,
        depth: i32,
        path: &[Seg],
    ) {
        let rh = self.ui.row_h();
        let indent = self.ui.px(12.0);
        let h = col.row(rh);
        let h = Rect { x: h.x + indent, w: h.w - indent, ..h };
        if !self.ui.collapsing(id_of(id, "nested"), h, name, true) {
            return;
        }
        let sub_ctx = Ctx {
            schema: sub,
            file: ctx.file_of(name),
            key_files: Vec::new(),
            base: path.to_vec(),
            table: t.clone(),
            id: id_of(id, "sub"),
        };
        col.indent(indent);
        self.inspect_table(col, &sub_ctx, depth + 1);
        col.indent(-indent);
    }

    fn number_field(&mut self, id: Id, r: Rect, current: Option<f64>, o: &NumOpts, color: u32, key: &Key) -> Edit {
        let Some(v) = current else {
            // No default number: type one.
            return match self.ui.text_field(id, r, "", &default_text(key.default)) {
                Some(t) => {
                    if let Some(x) = crate::ui::widgets::eval(&t) {
                        Edit::Set { value: Value::Number(x), merge: false, done: true }
                    } else {
                        self.notify(format!("'{t}' is not a number"));
                        Edit::None
                    }
                }
                None => Edit::None,
            };
        };
        let slider = o.min.is_finite() && o.max.is_finite() && o.max - o.min <= 1000.0;
        let resp = if slider { self.ui.slider(id, r, v, o, color) } else { self.ui.drag_number(id, r, v, o, color) };
        match (resp.value, resp.done) {
            (Some(x), done) => Edit::Set { value: Value::Number(x), merge: true, done },
            (None, true) => Edit::Seal,
            (None, false) => Edit::None,
        }
    }

    fn string_field(&mut self, id: Id, r: Rect, value: Option<&Value>, hint: &str) -> Edit {
        let shown = value.and_then(Value::as_str).unwrap_or("").to_owned();
        match self.ui.text_field(id, r, &shown, hint) {
            Some(t) if t.trim().is_empty() => Edit::Remove,
            Some(t) => Edit::Set { value: Value::String(t), merge: false, done: true },
            None => Edit::None,
        }
    }

    fn vector_field(&mut self, id: Id, r: Rect, key: &Key, value: Option<&Value>, n: usize, color: u32) -> Edit {
        let rh = self.ui.row_h();
        let o = opts_of(key);
        let is_color = matches!(key.ty, Ty::Color | Ty::ColorTexture);
        let current =
            value.and_then(|v| numbers(v, n)).or_else(|| default_value(key.default, n).and_then(|v| numbers(&v, n)));
        let Some(cur) = current else {
            return match self.ui.text_field(id, r, "", &default_text(key.default)) {
                Some(t) => match pt_core::toml::parse(&format!("v = {t}")).ok().and_then(|x| x.get("v").cloned()) {
                    Some(v) if numbers(&v, n).is_some() => Edit::Set { value: v, merge: false, done: true },
                    _ => {
                        self.notify(format!("'{t}' is not a vector of {n}"));
                        Edit::None
                    }
                },
                None => Edit::None,
            };
        };
        let mut out = Edit::None;
        let mut fields = r;
        if is_color && n == 3 {
            let (sw, rest) = r.split_left(rh * 3 / 2, self.ui.px(4.0));
            fields = rest;
            let c = self.ui.color_edit(id_of(id, "swatch"), sw, [cur[0], cur[1], cur[2]], value.is_none());
            if let Some(v) = c.value {
                out = Edit::Set {
                    value: Value::Array(v.iter().map(|&x| Value::Number(x)).collect()),
                    merge: true,
                    done: c.done,
                };
            } else if c.done {
                out = Edit::Seal;
            }
        }
        // A single number standing for every component stays one number.
        if let Some(Value::Number(x)) = value {
            let resp = self.ui.drag_number(id_of(id, "scalar"), fields, *x, &o, color);
            return match (resp.value, resp.done) {
                (Some(v), done) => Edit::Set { value: Value::Number(v), merge: true, done },
                (None, true) => Edit::Seal,
                _ => out,
            };
        }
        let (v, done) = self.ui.vector(id_of(id, "vec"), fields, &cur, &o, color);
        match v {
            Some(v) => Edit::Set { value: Value::Array(v.into_iter().map(Value::Number).collect()), merge: true, done },
            None if done => Edit::Seal,
            None => out,
        }
    }

    /// Sets `path` of `file`, creating the tables on the way that do not exist yet (a
    /// `[render]` section the scene file does not have).
    fn apply_key(&mut self, file: usize, path: &[Seg], value: Value, merge: Option<Id>, done: bool) {
        let mut m = path.len() - 1;
        while m > 0 && self.ws.doc.value(file, &path[..m]).is_none() {
            m -= 1;
        }
        let mut v = value;
        for seg in path[m + 1..].iter().rev() {
            if let Seg::Key(k) = seg {
                let mut t = Table::new();
                t.insert(k.clone(), v);
                v = Value::Table(t);
            }
        }
        self.apply(file, &path[..=m], &v, merge, done);
    }
}

/// Probe names: `inspector:<key>` at the top level, `inspector:<parent> <key>` nested.
fn probe_name(ctx: &Ctx, key: &str) -> String {
    let nested: Vec<String> = ctx
        .base
        .iter()
        .skip(2)
        .map(|s| match s {
            Seg::Key(k) => k.clone(),
            Seg::Index(i) => i.to_string(),
        })
        .collect();
    if nested.is_empty() { format!("inspector:{key}") } else { format!("inspector:{} {key}", nested.join(" ")) }
}
