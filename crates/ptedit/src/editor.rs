//! The editor window: a menu bar, the outliner (left), the viewport (middle), the
//! inspector (right) and a status bar, with splitters between the panels. Every frame runs
//! the UI over one batch of input; mouse input the panels do not take goes to the viewport,
//! and keys nothing else wants are shortcuts.
//!
//! The viewport image is drawn only when something in it changed; the panels are drawn
//! over it every frame.

use crate::files::{self, FileDialog, Mode, Outcome};
use crate::overlay::{LIGHT_ID, Selection};
use crate::render::Shading;
use crate::settings::{self, Settings};
use crate::ui::font::Font;
use crate::ui::theme::Theme;
use crate::ui::{Column, Id, Input, Rect, Ui, id_of};
use crate::workspace::{PERSPECTIVE, Workspace};
use pt_core::document::Kind;
use pt_core::math::Vec3;
use pt_core::platform::{Event, Key, Modifiers, MouseButton, Window};
use pt_core::toml::Seg;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub const VIEWPORT: Id = 0x7669_6577_706f_7274;

/// The scene's settings sections, shown in the outliner above the lists.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Section {
    Camera,
    Render,
    Output,
    Background,
}

impl Section {
    pub const ALL: [Section; 4] = [Section::Camera, Section::Render, Section::Output, Section::Background];

    pub fn key(self) -> &'static str {
        match self {
            Section::Camera => "camera",
            Section::Render => "render",
            Section::Output => "output",
            Section::Background => "background",
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Section::Camera => "Camera",
            Section::Render => "Render",
            Section::Output => "Output",
            Section::Background => "Background",
        }
    }
}

/// What the outliner can select and the inspector shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Target {
    Section(Section),
    /// An entry of one of the document tree's lists, by index.
    Item(Kind, usize),
}

pub const HELP: &[&str] = &[
    "Viewport",
    "  left click: select  (Shift: add, Ctrl: toggle)     left drag: rectangle select",
    "  middle drag or Alt+left drag: orbit     Shift+middle or Alt+Shift+left: pan",
    "  wheel: zoom     right drag: look, with W A S D Q E to fly (Shift: faster)",
    "  F: frame selection (or all)     Home: reset view     O: perspective / orthographic",
    "  1-4 or Z: wireframe / solid / shaded / wire on solid     G: grid     B: all bounds",
    "  L: four views / one view     Space: maximize the view under the mouse",
    "  H: hide the selection     Alt+H: show everything     Esc: clear the selection",
    "Panels",
    "  outliner: click to select, double-click or F2 to rename, drag to reorder,",
    "    drag a material onto an object to assign it, right-click for more",
    "  inspector: drag numbers sideways (Shift: finer, Ctrl: coarser), click to type,",
    "    grey values are defaults: changing one writes it, x removes it again",
    "  Tab / Shift+Tab: next / previous field     Enter: commit     Esc: cancel",
    "Files",
    "  Ctrl+O open     Ctrl+S save     Ctrl+Shift+S save as     Ctrl+Q quit",
    "  Ctrl+Z undo     Ctrl+Y or Ctrl+Shift+Z redo     F1: this help",
];

/// Things menus and shortcuts do.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Cmd {
    Open,
    Save,
    SaveAs,
    Quit,
    Undo,
    Redo,
    SelectAll,
    ClearSelection,
    Rename,
    Hide,
    ShowAll,
    Frame,
    ResetView,
    ToggleOrtho,
    Shading(Shading),
    CycleShading,
    Grid,
    Bounds,
    Layout,
    Maximize,
    Outliner,
    Inspector,
    Theme,
    Help,
}

/// Keyboard shortcuts: key, Ctrl, Shift, Alt, command.
const SHORTCUTS: &[(Key, bool, bool, bool, Cmd)] = &[
    (Key::Char('o'), true, false, false, Cmd::Open),
    (Key::Char('s'), true, false, false, Cmd::Save),
    (Key::Char('s'), true, true, false, Cmd::SaveAs),
    (Key::Char('q'), true, false, false, Cmd::Quit),
    (Key::Char('z'), true, false, false, Cmd::Undo),
    (Key::Char('y'), true, false, false, Cmd::Redo),
    (Key::Char('z'), true, true, false, Cmd::Redo),
    (Key::Char('a'), true, false, false, Cmd::SelectAll),
    (Key::Escape, false, false, false, Cmd::ClearSelection),
    (Key::F(2), false, false, false, Cmd::Rename),
    (Key::Char('h'), false, false, false, Cmd::Hide),
    (Key::Char('h'), false, false, true, Cmd::ShowAll),
    (Key::Char('f'), false, false, false, Cmd::Frame),
    (Key::Home, false, false, false, Cmd::ResetView),
    (Key::Char('o'), false, false, false, Cmd::ToggleOrtho),
    (Key::Char('1'), false, false, false, Cmd::Shading(Shading::Wireframe)),
    (Key::Char('2'), false, false, false, Cmd::Shading(Shading::Solid)),
    (Key::Char('3'), false, false, false, Cmd::Shading(Shading::Shaded)),
    (Key::Char('4'), false, false, false, Cmd::Shading(Shading::WireSolid)),
    (Key::Char('z'), false, false, false, Cmd::CycleShading),
    (Key::Char('g'), false, false, false, Cmd::Grid),
    (Key::Char('b'), false, false, false, Cmd::Bounds),
    (Key::Char('l'), false, false, false, Cmd::Layout),
    (Key::Space, false, false, false, Cmd::Maximize),
    (Key::F(1), false, false, false, Cmd::Help),
];

fn shortcut(cmd: Cmd) -> String {
    let Some(&(key, ctrl, shift, alt, _)) = SHORTCUTS.iter().find(|s| s.4 == cmd) else { return String::new() };
    let name = match key {
        Key::Char(c) => c.to_ascii_uppercase().to_string(),
        Key::F(n) => format!("F{n}"),
        Key::Escape => "Esc".into(),
        Key::Home => "Home".into(),
        Key::Space => "Space".into(),
        other => format!("{other:?}"),
    };
    let mut s = String::new();
    for (on, m) in [(ctrl, "Ctrl+"), (shift, "Shift+"), (alt, "Alt+")] {
        if on {
            s += m;
        }
    }
    s + &name
}

const FLY_KEYS: [Key; 6] =
    [Key::Char('w'), Key::Char('a'), Key::Char('s'), Key::Char('d'), Key::Char('q'), Key::Char('e')];

#[derive(Clone, Copy, Debug, PartialEq)]
enum DragMode {
    Orbit,
    Pan,
    Look,
    /// Left button: a click or a selection rectangle, from where it started (window pixels).
    Select {
        x0: i32,
        y0: i32,
        mods: Modifiers,
    },
}

#[derive(Clone, Copy, Debug)]
struct Drag {
    mode: DragMode,
    view: crate::workspace::Rect,
    x: i32,
    y: i32,
}

/// What to do once unsaved changes are dealt with.
#[derive(Clone, Debug)]
enum After {
    Quit,
    Open(PathBuf),
}

enum Dialog {
    Files(FileDialog),
    Confirm(After),
}

#[expect(clippy::struct_excessive_bools, reason = "independent UI toggles and dirty flags")]
pub struct Editor {
    pub ws: Workspace,
    pub ui: Ui,
    pub settings: Settings,
    settings_path: Option<PathBuf>,
    pub status: String,
    help: bool,
    /// The viewport's place in the window, and its last drawn image.
    pub vp: Rect,
    vp_px: Vec<u32>,
    vp_size: (usize, usize),
    pub vp_dirty: bool,
    drag: Option<Drag>,
    held: HashSet<Key>,
    last_tick: Instant,
    pub inspect: Option<Target>,
    pub renaming: Option<Target>,
    dialog: Option<Dialog>,
    /// The key a file picked in the browser goes to: file and path.
    pick: Option<(usize, Vec<Seg>)>,
    /// The document changed: the scene is rebuilt at the end of the frame.
    pub scene_stale: bool,
    pub quit: bool,
    /// Named widget rectangles of the last frame (for tests and scripted input).
    pub probes: Vec<(String, Rect)>,
    last_sel: Selection,
    /// Panel widths changed by a splitter: the settings are saved when it is let go.
    layout_changed: bool,
    /// A menu command chosen this frame.
    chosen: Option<Cmd>,
    pub outliner_filter: String,
    /// The rename field has been given the focus.
    pub rename_focused: bool,
}

impl Editor {
    pub fn new(ws: Workspace, settings: Settings, settings_path: Option<PathBuf>, scale: f32) -> Editor {
        let font = Font::load(&settings.font, settings.font_size * scale);
        let ui = Ui::new(Theme::named(&settings.theme), font, scale);
        let mut ws = ws;
        ws.quad = settings.quad;
        ws.labels_apart = true;
        let last_sel = ws.sel.clone();
        let mut ed = Editor {
            ws,
            ui,
            settings,
            settings_path,
            status: "F1: help".into(),
            help: false,
            vp: Rect::default(),
            vp_px: Vec::new(),
            vp_size: (0, 0),
            vp_dirty: true,
            drag: None,
            held: HashSet::new(),
            last_tick: Instant::now(),
            inspect: None,
            renaming: None,
            dialog: None,
            pick: None,
            scene_stale: false,
            quit: false,
            probes: Vec::new(),
            last_sel,
            layout_changed: false,
            chosen: None,
            outliner_filter: String::new(),
            rename_focused: false,
        };
        let path = ed.ws.doc.path().to_path_buf();
        ed.settings.add_recent(&path);
        ed.save_settings();
        if let Some(e) = &ed.ws.load_error {
            ed.status = format!("the scene does not load: {e}");
        }
        ed
    }

    pub fn notify(&mut self, s: impl Into<String>) {
        self.status = s.into();
    }

    fn save_settings(&mut self) {
        if let Some(p) = &self.settings_path
            && let Err(e) = self.settings.save(p)
        {
            self.status = format!("cannot save the settings: {e}");
        }
    }

    pub fn probe(&mut self, name: impl Into<String>, r: Rect) {
        self.probes.push((name.into(), r));
    }

    /// The rectangle of probe `name` from the last frame.
    #[cfg(test)]
    pub fn probe_rect(&self, name: &str) -> Option<Rect> {
        self.probes.iter().find(|(n, _)| n == name).map(|&(_, r)| r)
    }

    // ------------------------------------------------------------------ frames

    /// Runs one frame of the UI over `input` (and the raw `events` it was made from, which
    /// the viewport reads) in a `w`x`h` window.
    pub fn frame(&mut self, input: Input, events: &[Event], w: i32, h: i32) {
        self.probes.clear();
        self.ui.begin(input, w, h);
        let rh = self.ui.row_h();
        let screen = Rect::new(0, 0, w, h);
        let (menu_r, rest) = screen.split_top(rh + self.ui.px(4.0));
        let (status_r, mut center) = rest.split_bottom(rh);
        let split = self.ui.px(5.0);
        let scale = self.ui.scale;
        let mut left = None;
        let mut right = None;
        if self.settings.outliner {
            let lw = self.ui.px(self.settings.outliner_width).clamp(self.ui.px(80.0), (center.w / 2).max(1));
            let (l, c) = center.split_left(lw, 0);
            left = Some(l);
            center = c;
        }
        if self.settings.inspector {
            let iw = self.ui.px(self.settings.inspector_width).clamp(self.ui.px(120.0), (center.w * 2 / 3).max(1));
            let (r, c) = center.split_right(iw, 0);
            right = Some(r);
            center = c;
        }
        self.vp = center;
        self.menu_bar(menu_r);
        if let Some(l) = left {
            self.outliner(l);
        }
        if let Some(r) = right {
            self.inspector(r);
        }
        // Splitters over the panel edges.
        if let Some(l) = left {
            let d = self.ui.splitter(
                id_of(VIEWPORT, "split-left"),
                Rect::new(l.right() - split / 2, l.y, split, l.h),
                true,
            );
            if d != 0 {
                self.settings.outliner_width = (self.settings.outliner_width + d as f32 / scale).max(80.0);
                self.layout_changed = true;
            }
        }
        if let Some(r) = right {
            let d = self.ui.splitter(id_of(VIEWPORT, "split-right"), Rect::new(r.x - split / 2, r.y, split, r.h), true);
            if d != 0 {
                self.settings.inspector_width = (self.settings.inspector_width - d as f32 / scale).max(120.0);
                self.layout_changed = true;
            }
        }
        if self.layout_changed && !self.ui.input.down[0] {
            self.layout_changed = false;
            self.save_settings();
        }
        self.status_bar(status_r);
        self.viewport_input(events);
        self.viewport_overlay();
        self.dialogs();
        self.shortcuts();
        self.ui.end();
        self.after_frame();
    }

    /// Rebuilds the scene after edits and follows selection changes.
    fn after_frame(&mut self) {
        if self.scene_stale {
            self.scene_stale = false;
            match self.ws.reload() {
                Ok(()) => self.vp_dirty = true,
                Err(e) => self.status = format!("the scene does not load: {e}"),
            }
        }
        if let Some(Target::Item(kind, i)) = self.inspect
            && i >= self.list(kind).len()
        {
            self.inspect = None;
        }
        if self.ws.sel != self.last_sel {
            self.vp_dirty = true;
            if self.ws.sel.count() == 1 {
                let i = self.ws.sel.objects.iter().position(|&s| s);
                let j = self.ws.sel.lights.iter().position(|&s| s);
                self.inspect = match (i, j) {
                    (Some(i), _) => Some(Target::Item(Kind::Object, i)),
                    (_, Some(j)) => Some(Target::Item(Kind::Light, j)),
                    _ => self.inspect,
                };
            }
            self.last_sel = self.ws.sel.clone();
        }
    }

    /// The window's pixels: the viewport (redrawn if it changed) under the panels.
    pub fn render(&mut self, w: usize, h: usize) -> Vec<u32> {
        let (vw, vh) = (self.vp.w.max(1) as usize, self.vp.h.max(1) as usize);
        if self.vp_dirty || self.vp_size != (vw, vh) {
            self.vp_px = self.ws.compose(vw, vh);
            self.vp_size = (vw, vh);
            self.vp_dirty = false;
        }
        let mut px = vec![self.ui.theme.window; w * h];
        let vp = self.vp.intersect(&Rect::new(0, 0, w as i32, h as i32));
        for y in 0..vp.h.min(vh as i32) {
            let src = &self.vp_px[y as usize * vw..y as usize * vw + vp.w.min(vw as i32) as usize];
            let at = (vp.y + y) as usize * w + vp.x as usize;
            px[at..at + src.len()].copy_from_slice(src);
        }
        // The views' labels, in the UI font, centred where the view placed them.
        let font = &mut self.ui.font;
        let clip = crate::ui::font::Clip { x0: vp.x, y0: vp.y, x1: vp.right(), y1: vp.bottom() };
        let mut t = crate::ui::font::Target { px: &mut px, w, h };
        let lh = font.line_height();
        for l in &self.ws.labels {
            let tw = font.width(&l.text);
            let x = (vp.x as f32 + l.x as f32 - tw / 2.0).min(vp.right() as f32 - tw - 4.0);
            let y = vp.y + l.y - lh / 2;
            font.draw(&mut t, clip, x + 1.0, y + 1, &l.text, 0);
            font.draw(&mut t, clip, x, y, &l.text, l.color);
        }
        self.ui.paint(&mut px, w, h);
        px
    }

    /// Whether frames must keep coming without input (flying, a repaint the UI asked for).
    pub fn next_frame_at(&self) -> Option<Instant> {
        let flying =
            self.drag.is_some_and(|d| d.mode == DragMode::Look) && FLY_KEYS.iter().any(|k| self.held.contains(k));
        if flying { Some(Instant::now()) } else { self.ui.repaint_at() }
    }

    pub fn title(&self) -> String {
        let name = self.ws.doc.path().file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        let star = if self.ws.doc.is_dirty() { " *" } else { "" };
        format!("ptedit — {name}{star}")
    }

    // ------------------------------------------------------------------ bars

    fn menu_bar(&mut self, r: Rect) {
        let ui = &mut self.ui;
        ui.fill(r, ui.theme.bar);
        ui.fill(Rect::new(r.x, r.bottom() - 1, r.w, 1), ui.theme.separator);
        let ids: Vec<Id> = ["File", "Edit", "View"].iter().map(|t| id_of(VIEWPORT, ("menu", *t))).collect();
        let mut x = r.x + ui.px(4.0);
        for (k, title) in ["File", "Edit", "View"].into_iter().enumerate() {
            let w = self.ui.text_w(title) + 2 * self.ui.pad() + self.ui.px(6.0);
            let tr = Rect::new(x, r.y + self.ui.px(2.0), w, r.h - self.ui.px(4.0));
            x += w;
            let Some(mut f) = self.ui.menu(ids[k], tr, title, &ids) else { continue };
            let item = |ed: &mut Editor, f: &mut crate::ui::PopupFrame, label: &str, cmd: Cmd, enabled: bool| {
                if ed.ui.menu_item(f, label, &shortcut(cmd), enabled) {
                    ed.chosen = Some(cmd);
                }
            };
            match k {
                0 => {
                    item(self, &mut f, "Open…", Cmd::Open, true);
                    let recent: Vec<String> = self.settings.recent.clone();
                    if let Some(mut sub) = self.ui.submenu(&mut f, "Open Recent", !recent.is_empty()) {
                        for path in &recent {
                            if self.ui.menu_item(&mut sub, path, "", true) {
                                self.request_open(PathBuf::from(path));
                            }
                        }
                        self.ui.end_popup(sub);
                    }
                    self.ui.menu_separator(&mut f);
                    let dirty = self.ws.doc.is_dirty();
                    item(self, &mut f, "Save", Cmd::Save, dirty);
                    item(self, &mut f, "Save As…", Cmd::SaveAs, true);
                    self.ui.menu_separator(&mut f);
                    item(self, &mut f, "Quit", Cmd::Quit, true);
                }
                1 => {
                    let undo = self.ws.doc.history().undo_label().map(|l| format!("Undo {l}"));
                    let redo = self.ws.doc.history().redo_label().map(|l| format!("Redo {l}"));
                    item(self, &mut f, undo.as_deref().unwrap_or("Undo"), Cmd::Undo, undo.is_some());
                    item(self, &mut f, redo.as_deref().unwrap_or("Redo"), Cmd::Redo, redo.is_some());
                    self.ui.menu_separator(&mut f);
                    item(self, &mut f, "Select All", Cmd::SelectAll, true);
                    item(self, &mut f, "Clear Selection", Cmd::ClearSelection, !self.ws.sel.is_empty());
                    item(self, &mut f, "Rename", Cmd::Rename, matches!(self.inspect, Some(Target::Item(..))));
                    self.ui.menu_separator(&mut f);
                    item(self, &mut f, "Hide Selection", Cmd::Hide, !self.ws.sel.is_empty());
                    item(self, &mut f, "Show All", Cmd::ShowAll, !self.ws.hidden.is_empty());
                }
                _ => {
                    for s in Shading::ALL {
                        let on = self.ws.shading == s;
                        let label = match s {
                            Shading::Wireframe => "Wireframe",
                            Shading::Solid => "Solid",
                            Shading::Shaded => "Shaded",
                            Shading::WireSolid => "Wireframe on Solid",
                        };
                        if self.ui.menu_check(&mut f, label, &shortcut(Cmd::Shading(s)), on) {
                            self.chosen = Some(Cmd::Shading(s));
                        }
                    }
                    self.ui.menu_separator(&mut f);
                    let checks = [
                        ("Grid", Cmd::Grid, self.ws.grid),
                        ("All Bounds", Cmd::Bounds, self.ws.all_bounds),
                        ("Four Views", Cmd::Layout, self.ws.quad),
                        ("Orthographic", Cmd::ToggleOrtho, self.ws.views[self.ws.active].ortho),
                    ];
                    for (label, cmd, on) in checks {
                        if self.ui.menu_check(&mut f, label, &shortcut(cmd), on) {
                            self.chosen = Some(cmd);
                        }
                    }
                    item(self, &mut f, "Frame Selection", Cmd::Frame, true);
                    item(self, &mut f, "Reset View", Cmd::ResetView, true);
                    self.ui.menu_separator(&mut f);
                    let panels = [
                        ("Outliner", Cmd::Outliner, self.settings.outliner),
                        ("Inspector", Cmd::Inspector, self.settings.inspector),
                        ("Light Theme", Cmd::Theme, self.settings.theme == "light"),
                    ];
                    for (label, cmd, on) in panels {
                        if self.ui.menu_check(&mut f, label, "", on) {
                            self.chosen = Some(cmd);
                        }
                    }
                    self.ui.menu_separator(&mut f);
                    item(self, &mut f, "Help", Cmd::Help, true);
                }
            }
            self.ui.end_popup(f);
        }
        if let Some(c) = self.chosen.take() {
            self.run(c);
        }
    }

    fn status_bar(&mut self, r: Rect) {
        let ui = &mut self.ui;
        ui.fill(r, ui.theme.bar);
        ui.fill(Rect::new(r.x, r.y, r.w, 1), ui.theme.separator);
        let s = &self.ws.stats;
        let stats = format!(
            "{} · {} triangles{} · {:.1} ms",
            self.ws.shading.name(),
            s.triangles,
            if s.boxes > 0 { format!(", {} boxes", s.boxes) } else { String::new() },
            self.ws.frame_ms
        );
        let sw = self.ui.text_w(&stats) + 2 * self.ui.pad();
        let (right, left) = r.split_right(sw, 0);
        let color = if self.status.starts_with("error") || self.status.contains("does not load") {
            self.ui.theme.error
        } else {
            self.ui.theme.text_dim
        };
        let status = self.status.clone();
        self.ui.label(left.shrink2(self.ui.pad(), 0), &status, color);
        self.ui.label(right, &stats, self.ui.theme.text_disabled);
    }

    // ------------------------------------------------------------------ viewport

    /// Viewport-local view rectangle under window pixel (`x`, `y`).
    fn view_at(&self, x: i32, y: i32) -> Option<crate::workspace::Rect> {
        let (vw, vh) = (self.vp.w.max(1) as usize, self.vp.h.max(1) as usize);
        self.ws.rect_at(vw, vh, x - self.vp.x, y - self.vp.y)
    }

    fn viewport_input(&mut self, events: &[Event]) {
        let modal = self.ui.modal_open() || self.dialog.is_some();
        let free = |ui: &Ui| ui.mouse_layer == crate::ui::LAYER_BASE && (ui.active == 0 || ui.active == VIEWPORT);
        let (mx, my) = self.ui.input.mouse;
        if !modal && free(&self.ui) && self.vp.contains(mx, my) && self.ui.input.wheel != 0.0 {
            if let Some(r) = self.view_at(mx, my) {
                self.ws.views[r.view].zoom(self.ui.input.wheel);
                self.vp_dirty = true;
            }
            self.ui.input.wheel = 0.0;
        }
        for ev in events {
            match *ev {
                Event::FocusLost => {
                    self.held.clear();
                    self.drag = None;
                }
                Event::KeyUp { key, .. } => {
                    self.held.remove(&key);
                }
                Event::MouseDown { button, x, y, mods } => {
                    if modal
                        || self.drag.is_some()
                        || self.ui.swallowed_press
                        || !free(&self.ui)
                        || !self.vp.contains(x, y)
                    {
                        continue;
                    }
                    let Some(view) = self.view_at(x, y) else { continue };
                    self.set_active(view.view);
                    let mode = match button {
                        MouseButton::Left if mods.alt && mods.shift => DragMode::Pan,
                        MouseButton::Left if mods.alt => DragMode::Orbit,
                        MouseButton::Left => DragMode::Select { x0: x, y0: y, mods },
                        MouseButton::Middle if mods.shift => DragMode::Pan,
                        MouseButton::Middle => DragMode::Orbit,
                        MouseButton::Right => DragMode::Look,
                    };
                    self.drag = Some(Drag { mode, view, x, y });
                    self.ui.capture(VIEWPORT);
                }
                Event::MouseUp { button, x, y, .. } => {
                    let Some(d) = self.drag else { continue };
                    let ends = match d.mode {
                        DragMode::Select { .. } => button == MouseButton::Left,
                        DragMode::Look => button == MouseButton::Right,
                        DragMode::Orbit | DragMode::Pan => true,
                    };
                    if !ends {
                        continue;
                    }
                    self.drag = None;
                    if self.ui.active == VIEWPORT {
                        self.ui.active = 0;
                    }
                    if let DragMode::Select { x0, y0, mods } = d.mode {
                        self.select(&d.view, x0, y0, x, y, mods);
                    }
                    if d.mode == DragMode::Look {
                        self.held.clear();
                    }
                    self.vp_dirty = true;
                }
                Event::MouseMove { x, y, .. } => match self.drag {
                    Some(mut d) => {
                        let (dx, dy) = ((x - d.x) as f32, (y - d.y) as f32);
                        let view = &mut self.ws.views[d.view.view];
                        match d.mode {
                            DragMode::Orbit if view.is_fixed() => view.pan(dx, dy, d.view.h as f32),
                            DragMode::Orbit => view.orbit(dx, dy),
                            DragMode::Pan => view.pan(dx, dy, d.view.h as f32),
                            DragMode::Look => view.look(dx, dy),
                            DragMode::Select { .. } => {}
                        }
                        (d.x, d.y) = (x, y);
                        self.drag = Some(d);
                        if !matches!(d.mode, DragMode::Select { .. }) {
                            self.vp_dirty = true;
                        }
                    }
                    None => {
                        if !modal
                            && free(&self.ui)
                            && self.vp.contains(x, y)
                            && let Some(r) = self.view_at(x, y)
                        {
                            self.set_active(r.view);
                        }
                    }
                },
                _ => {}
            }
        }
        if self.drag.is_some() {
            self.ui.capture(VIEWPORT);
        }
        self.tick_fly();
    }

    fn set_active(&mut self, v: usize) {
        if self.ws.active != v {
            self.ws.active = v;
            self.vp_dirty = true;
        }
    }

    /// A click (picks through the BVH) or a rectangle (what is drawn inside it); window pixels.
    fn select(&mut self, r: &crate::workspace::Rect, x0: i32, y0: i32, x1: i32, y1: i32, mods: Modifiers) {
        let (ox, oy) = (self.vp.x + r.x as i32, self.vp.y + r.y as i32);
        let (lx0, ly0, lx1, ly1) = (x0 - ox, y0 - oy, x1 - ox, y1 - oy);
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
        let Some(d) = self.drag.filter(|d| d.mode == DragMode::Look) else { return };
        let held = |c: char| f32::from(u8::from(self.held.contains(&Key::Char(c))));
        let local = Vec3::new(held('d') - held('a'), held('e') - held('q'), held('w') - held('s'));
        if local.length_squared() > 0.0 {
            let b = self.ws.display.bounds;
            let diag = (b.max - b.min).length();
            let speed = if diag.is_finite() && diag > 0.0 { 0.3 * diag } else { 1.0 };
            let fast = if self.held.contains(&Key::Shift) { 4.0 } else { 1.0 };
            self.ws.views[d.view.view].fly(local * (speed * fast * dt));
            self.vp_dirty = true;
        }
    }

    /// The selection rectangle and the help, over the viewport.
    fn viewport_overlay(&mut self) {
        if let Some(Drag { mode: DragMode::Select { x0, y0, .. }, x, y, .. }) = self.drag
            && (x - x0).abs() + (y - y0).abs() >= 4
        {
            let r = Rect::new(x0.min(x), y0.min(y), (x - x0).abs() + 1, (y - y0).abs() + 1);
            self.ui.push_clip(self.vp);
            self.ui.fill_rounded(r, self.ui.theme.accent, 40, 0);
            self.ui.border(r, 0xe0_e0_e8, 0);
            self.ui.pop_clip();
        }
        if self.help {
            let lh = self.ui.font.line_height();
            let pad = self.ui.pad();
            let w = HELP.iter().map(|l| self.ui.text_w(l)).max().unwrap_or(0) + 3 * pad;
            let r = Rect::new(
                self.vp.x + pad,
                self.vp.y + pad,
                w.min(self.vp.w - 2 * pad),
                lh * HELP.len() as i32 + 2 * pad,
            );
            self.ui.push_clip(self.vp);
            self.ui.fill_rounded(r, self.ui.theme.panel, 235, self.ui.px(4.0));
            self.ui.border(r, self.ui.theme.border, self.ui.px(4.0));
            for (i, l) in HELP.iter().enumerate() {
                let c = if l.starts_with(' ') { self.ui.theme.text } else { self.ui.theme.accent };
                self.ui.text(r.x + pad, r.y + pad + i as i32 * lh, l, c);
            }
            self.ui.pop_clip();
        }
    }

    // ------------------------------------------------------------------ commands

    fn shortcuts(&mut self) {
        if self.dialog.is_some() || self.ui.modal_open() {
            return;
        }
        let looking = self.drag.is_some_and(|d| d.mode == DragMode::Look);
        let typing = self.ui.wants_keyboard();
        for (key, mods) in std::mem::take(&mut self.ui.input.keys) {
            if looking && (FLY_KEYS.contains(&key) || key == Key::Shift) {
                self.held.insert(key);
                continue;
            }
            if key == Key::Shift {
                self.held.insert(key);
            }
            if typing && !(mods.ctrl && matches!(key, Key::Char('s' | 'o' | 'q'))) {
                continue;
            }
            let found =
                SHORTCUTS.iter().find(|s| s.0 == key && s.1 == mods.ctrl && s.2 == mods.shift && s.3 == mods.alt);
            // Unshifted single keys also work with Shift held (Shift+F keeps framing).
            let found = found.or_else(|| {
                (!mods.ctrl && !mods.alt).then(|| SHORTCUTS.iter().find(|s| s.0 == key && !s.1 && !s.3)).flatten()
            });
            if let Some(&(.., cmd)) = found {
                self.run(cmd);
            }
        }
    }

    /// Does `cmd`.
    pub fn run(&mut self, cmd: Cmd) {
        let v = self.ws.active;
        match cmd {
            Cmd::Open => {
                let dir = self.ws.doc.path().parent().map_or_else(|| PathBuf::from("."), Path::to_path_buf);
                self.dialog = Some(Dialog::Files(FileDialog::new(Mode::Open, "Open Scene", &dir, &["toml"])));
            }
            Cmd::Save => {
                let r = self.ws.doc.save().map(|written| match written.len() {
                    0 => "nothing to save".to_owned(),
                    _ => format!("saved {}", list(&written)),
                });
                self.notify(r.unwrap_or_else(|e| format!("error: {e}")));
            }
            Cmd::SaveAs => {
                let path = self.ws.doc.path().to_path_buf();
                let dir = path.parent().map_or_else(|| PathBuf::from("."), Path::to_path_buf);
                let mut d = FileDialog::new(Mode::SaveAs, "Save Scene As", &dir, &["toml"]);
                d.name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                self.dialog = Some(Dialog::Files(d));
            }
            Cmd::Quit => self.request_quit(),
            Cmd::Undo | Cmd::Redo => {
                let undo = cmd == Cmd::Undo;
                let label = if undo { self.ws.doc.history().undo_label() } else { self.ws.doc.history().redo_label() };
                let label = label.map(str::to_owned);
                let r = if undo { self.ws.doc.undo() } else { self.ws.doc.redo() };
                match (r, label) {
                    (Ok(true), Some(l)) => {
                        self.scene_stale = true;
                        self.notify(format!("{} {l}", if undo { "undid" } else { "redid" }));
                    }
                    (Ok(_), _) => self.notify(if undo { "nothing to undo" } else { "nothing to redo" }),
                    (Err(e), _) => self.notify(format!("error: {e}")),
                }
            }
            Cmd::SelectAll => {
                let ws = &mut self.ws;
                ws.sel.objects.iter_mut().chain(ws.sel.lights.iter_mut()).for_each(|s| *s = true);
                let n = ws.sel.count();
                self.notify(format!("selected {n}"));
            }
            Cmd::ClearSelection => {
                if self.help {
                    self.help = false;
                } else {
                    self.ws.sel.clear();
                    self.notify("selection cleared");
                }
            }
            Cmd::Rename => {
                if let Some(t @ Target::Item(..)) = self.inspect {
                    self.renaming = Some(t);
                    self.rename_focused = false;
                }
            }
            Cmd::Hide => {
                let ws = &mut self.ws;
                let n = ws.sel.count();
                for (h, s) in ws
                    .hidden
                    .objects
                    .iter_mut()
                    .zip(&ws.sel.objects)
                    .chain(ws.hidden.lights.iter_mut().zip(&ws.sel.lights))
                {
                    *h |= *s;
                }
                ws.sel.clear();
                self.vp_dirty = true;
                self.notify(format!("hid {n}"));
            }
            Cmd::ShowAll => {
                self.ws.hidden.clear();
                self.vp_dirty = true;
                self.notify("everything shown");
            }
            Cmd::Frame => {
                self.ws.frame(v);
                self.vp_dirty = true;
                self.notify(if self.ws.sel.is_empty() { "framed the scene" } else { "framed the selection" });
            }
            Cmd::ResetView => {
                let (cam, b) = (self.ws.scene.camera.clone(), self.ws.display.bounds);
                self.ws.views[v].reset(&cam, &b);
                self.vp_dirty = true;
                self.notify(if v == PERSPECTIVE { "view reset to the scene camera" } else { "view reset" });
            }
            Cmd::ToggleOrtho => {
                let view = &mut self.ws.views[v];
                if view.is_fixed() {
                    self.notify("the top, front and side views are always orthographic");
                } else {
                    view.ortho = !view.ortho;
                    let what = if view.ortho { "orthographic" } else { "perspective" };
                    self.vp_dirty = true;
                    self.notify(what);
                }
            }
            Cmd::Shading(s) => {
                self.ws.shading = s;
                self.vp_dirty = true;
                self.notify(format!("shading: {}", s.name()));
            }
            Cmd::CycleShading => {
                let i = Shading::ALL.iter().position(|&s| s == self.ws.shading).unwrap_or(0);
                self.run(Cmd::Shading(Shading::ALL[(i + 1) % 4]));
            }
            Cmd::Grid => {
                self.ws.grid = !self.ws.grid;
                self.vp_dirty = true;
            }
            Cmd::Bounds => {
                self.ws.all_bounds = !self.ws.all_bounds;
                self.vp_dirty = true;
            }
            Cmd::Layout => {
                self.ws.quad = !self.ws.quad;
                self.ws.maximized = None;
                self.ws.active = PERSPECTIVE;
                self.settings.quad = self.ws.quad;
                self.save_settings();
                self.vp_dirty = true;
                self.notify(if self.ws.quad { "four views" } else { "one view" });
            }
            Cmd::Maximize => {
                if self.ws.quad {
                    self.ws.maximized = match self.ws.maximized {
                        Some(_) => None,
                        None => Some(v),
                    };
                    self.vp_dirty = true;
                }
            }
            Cmd::Outliner => {
                self.settings.outliner = !self.settings.outliner;
                self.save_settings();
            }
            Cmd::Inspector => {
                self.settings.inspector = !self.settings.inspector;
                self.save_settings();
            }
            Cmd::Theme => {
                self.settings.theme = if self.settings.theme == "light" { "dark" } else { "light" }.into();
                self.ui.theme = Theme::named(&self.settings.theme);
                self.save_settings();
            }
            Cmd::Help => self.help = !self.help,
        }
    }

    // ------------------------------------------------------------------ files and dialogs

    fn request_quit(&mut self) {
        if self.ws.doc.is_dirty() {
            self.dialog = Some(Dialog::Confirm(After::Quit));
        } else {
            self.quit = true;
        }
    }

    /// The window's close button.
    pub fn close_requested(&mut self) {
        self.request_quit();
    }

    fn request_open(&mut self, path: PathBuf) {
        if self.ws.doc.is_dirty() {
            self.dialog = Some(Dialog::Confirm(After::Open(path)));
        } else {
            self.open(&path);
        }
    }

    fn open(&mut self, path: &Path) {
        match Workspace::open(path) {
            Ok(mut ws) => {
                ws.quad = self.ws.quad;
                ws.shading = self.ws.shading;
                ws.grid = self.ws.grid;
                ws.labels_apart = true;
                self.ws = ws;
                self.inspect = None;
                self.renaming = None;
                self.last_sel = self.ws.sel.clone();
                self.vp_dirty = true;
                self.settings.add_recent(path);
                self.save_settings();
                match &self.ws.load_error {
                    Some(e) => self.status = format!("the scene does not load: {e}"),
                    None => self.notify(format!("opened {}", path.display())),
                }
            }
            Err(e) => {
                self.settings.recent.retain(|r| Path::new(r) != path);
                self.notify(format!("error: {e}"));
            }
        }
    }

    fn after(&mut self, a: After) {
        match a {
            After::Quit => self.quit = true,
            After::Open(p) => self.open(&p),
        }
    }

    /// Starts picking a file for key `path` of `file` (`exts`: the kinds of file).
    pub fn pick_file(&mut self, file: usize, path: Vec<Seg>, exts: &[&str], current: Option<&str>) {
        let base = self.ws.doc.files[file].path.parent().map_or_else(|| PathBuf::from("."), Path::to_path_buf);
        let mut dir = base.clone();
        if let Some(c) = current
            && let Some(p) = base.join(c).parent()
            && p.is_dir()
        {
            dir = p.to_path_buf();
        }
        let mut d = FileDialog::new(Mode::Pick, "Choose a File", &dir, exts);
        if let Some(c) = current {
            d.name = Path::new(c).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        }
        self.pick = Some((file, path));
        self.dialog = Some(Dialog::Files(d));
    }

    fn dialogs(&mut self) {
        let Some(dialog) = self.dialog.take() else { return };
        match dialog {
            Dialog::Files(mut fd) => match fd.ui(&mut self.ui) {
                Outcome::Open => self.dialog = Some(Dialog::Files(fd)),
                Outcome::Cancel => self.pick = None,
                Outcome::Chosen(path) => match fd.mode {
                    Mode::Open => self.request_open(path),
                    Mode::SaveAs => match self.ws.doc.save_as(&path) {
                        Ok(written) => {
                            self.settings.add_recent(&path);
                            self.save_settings();
                            self.notify(format!("saved {}", list(&written)));
                        }
                        Err(e) => {
                            fd.error = Some(e);
                            self.dialog = Some(Dialog::Files(fd));
                        }
                    },
                    Mode::Pick => {
                        if let Some((file, key)) = self.pick.take() {
                            let base = self.ws.doc.files[file]
                                .path
                                .parent()
                                .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
                            let rel = files::relative(&path, &base);
                            self.apply(file, &key, &pt_core::toml::Value::String(rel), None, true);
                        }
                    }
                },
            },
            Dialog::Confirm(after) => {
                let name =
                    self.ws.doc.path().file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned());
                let area = self.ui.begin_modal("Unsaved Changes", 560.0, 150.0);
                let rh = self.ui.row_h();
                let mut col = Column::new(area, self.ui.px(6.0));
                let msg = format!("Save the changes to {name} first?");
                let r = col.row(rh);
                self.ui.label(r, &msg, self.ui.theme.text);
                let bw = self.ui.px(96.0);
                let gap = self.ui.px(6.0);
                let buttons = Rect::new(area.x, area.bottom() - rh, area.w, rh);
                let (save_r, rest) = buttons.split_right(bw, gap);
                let (discard_r, rest) = rest.split_right(bw, gap);
                let (cancel_r, _) = rest.split_right(bw, gap);
                let id = id_of(VIEWPORT, "confirm");
                let mut keep = true;
                if self.ui.button_primary(id_of(id, "save"), save_r, "Save") {
                    match self.ws.doc.save() {
                        Ok(_) => {
                            keep = false;
                            self.after(after.clone());
                        }
                        Err(e) => self.notify(format!("error: {e}")),
                    }
                }
                if keep && self.ui.button(id_of(id, "discard"), discard_r, "Discard") {
                    keep = false;
                    self.after(after.clone());
                }
                if keep && (self.ui.button(id_of(id, "cancel"), cancel_r, "Cancel") || self.ui.take_key(Key::Escape)) {
                    keep = false;
                }
                self.ui.end_modal();
                if keep {
                    self.dialog = Some(Dialog::Confirm(after));
                }
            }
        }
    }

    // ------------------------------------------------------------------ editing

    /// Sets the value at `path` of `file`. With `merge`, consecutive edits with the same key
    /// are one undo step (a drag) until `done`.
    pub fn apply(&mut self, file: usize, path: &[Seg], value: &pt_core::toml::Value, merge: Option<Id>, done: bool) {
        let r = match merge {
            Some(m) => self.ws.doc.set_merging(file, path, value, &format!("ui {m:x}")),
            None => self.ws.doc.set(file, path, value),
        };
        if done {
            self.ws.doc.seal();
        }
        match r {
            Ok(true) => {
                self.scene_stale = true;
                self.status = format!("set {}", pt_core::toml::path_name(path));
            }
            Ok(false) => {}
            Err(e) => self.status = format!("error: {e}"),
        }
    }

    pub fn remove(&mut self, file: usize, path: &[Seg]) {
        match self.ws.doc.remove(file, path) {
            Ok(true) => {
                self.scene_stale = true;
                self.status = format!("removed {}", pt_core::toml::path_name(path));
            }
            Ok(false) => {}
            Err(e) => self.status = format!("error: {e}"),
        }
    }

    /// The document tree's list of `kind`.
    pub fn list(&self, kind: Kind) -> &[pt_core::document::Item] {
        let t = &self.ws.doc.tree;
        match kind {
            Kind::Light => &t.lights,
            Kind::Object => &t.objects,
            Kind::Material => &t.materials,
            Kind::Medium => &t.media,
            Kind::Prototype => &t.prototypes,
        }
    }

    /// The viewport id of a light or object entry.
    pub fn view_id(kind: Kind, i: usize) -> Option<u32> {
        match kind {
            Kind::Object => Some(i as u32 + 1),
            Kind::Light => Some(LIGHT_ID | i as u32),
            _ => None,
        }
    }
}

fn list(paths: &[PathBuf]) -> String {
    paths
        .iter()
        .map(|p| p.file_name().map_or_else(|| p.display().to_string(), |n| n.to_string_lossy().into_owned()))
        .collect::<Vec<_>>()
        .join(", ")
}

// ---------------------------------------------------------------------- the window

/// Turns window events into frames of UI input: mouse state, clicks in a row, keys, text.
pub struct InputState {
    mouse: (i32, i32),
    down: [bool; 3],
    mods: Modifiers,
    last_click: Option<(Instant, (i32, i32), u8)>,
    double_click: Duration,
}

impl InputState {
    pub fn new(double_click: Duration) -> InputState {
        InputState { mouse: (0, 0), down: [false; 3], mods: Modifiers::default(), last_click: None, double_click }
    }

    /// Splits `events` into batches with at most one button change each, so a press and
    /// its release are seen in different frames.
    pub fn batches(events: Vec<Event>) -> Vec<Vec<Event>> {
        let mut out: Vec<Vec<Event>> = vec![Vec::new()];
        for e in events {
            let button = matches!(e, Event::MouseDown { .. } | Event::MouseUp { .. });
            let cur = out.last_mut().expect("never empty");
            if button && cur.iter().any(|x| matches!(x, Event::MouseDown { .. } | Event::MouseUp { .. })) {
                out.push(vec![e]);
            } else {
                cur.push(e);
            }
        }
        out
    }

    pub fn input(&mut self, events: &[Event]) -> Input {
        let mut i = Input::default();
        let idx = |b: MouseButton| match b {
            MouseButton::Left => 0,
            MouseButton::Right => 1,
            MouseButton::Middle => 2,
        };
        for e in events {
            match *e {
                Event::MouseMove { x, y, mods } => {
                    self.mouse = (x, y);
                    self.mods = mods;
                }
                Event::MouseDown { button, x, y, mods } => {
                    self.mouse = (x, y);
                    self.mods = mods;
                    let b = idx(button);
                    self.down[b] = true;
                    i.pressed[b] = true;
                    if b == 0 {
                        let now = Instant::now();
                        let n = match self.last_click {
                            Some((t, (px, py), n))
                                if now.duration_since(t) <= self.double_click
                                    && (px - x).abs() + (py - y).abs() <= 6 =>
                            {
                                n + 1
                            }
                            _ => 1,
                        };
                        self.last_click = Some((now, (x, y), n));
                        i.clicks = n;
                    }
                }
                Event::MouseUp { button, x, y, mods } => {
                    self.mouse = (x, y);
                    self.mods = mods;
                    let b = idx(button);
                    self.down[b] = false;
                    i.released[b] = true;
                }
                Event::Wheel { delta, mods } => {
                    i.wheel += delta;
                    self.mods = mods;
                }
                Event::KeyDown { key, mods, .. } => {
                    self.mods = mods;
                    i.keys.push((key, mods));
                }
                Event::KeyUp { mods, .. } => self.mods = mods,
                Event::Text(c) => i.text.push(c),
                Event::FocusLost => {
                    self.down = [false; 3];
                    self.mods = Modifiers::default();
                }
                _ => {}
            }
        }
        i.mouse = self.mouse;
        i.down = self.down;
        i.mods = self.mods;
        i
    }
}

/// Runs the editor in a window until it quits.
pub fn run(path: &Path) -> Result<(), String> {
    let ws = Workspace::open(path)?;
    for p in ws.doc.validate()? {
        eprintln!("warning: {p}");
    }
    let mut window = Window::new("ptedit", 1440, 900)?;
    let settings_path = settings::path();
    let settings = Settings::load(settings_path.as_deref());
    let mut ed = Editor::new(ws, settings, settings_path, window.scale_factor());
    let mut input = InputState::new(window.double_click_time());
    let mut size = (0, 0);
    let mut title = String::new();
    let mut cursor = pt_core::platform::Cursor::Arrow;
    let mut needs_frame = true;
    // PTEDIT_TRACE=1 prints every window event (debugging input on a platform).
    let trace = std::env::var_os("PTEDIT_TRACE").is_some();
    loop {
        let events = window.poll_events();
        let (w, h) = window.client_size();
        let (w, h) = (w.max(1) as i32, h.max(1) as i32);
        if (w, h) != size {
            size = (w, h);
            needs_frame = true;
        }
        let due = ed.next_frame_at().is_some_and(|t| t <= Instant::now());
        if events.is_empty() && !needs_frame && !due {
            std::thread::sleep(Duration::from_millis(5));
            continue;
        }
        needs_frame = false;
        if trace {
            for e in &events {
                eprintln!("event: {e:?}");
            }
        }
        for batch in InputState::batches(events) {
            if batch.iter().any(|e| matches!(e, Event::Close)) {
                ed.close_requested();
            }
            let mut i = input.input(&batch);
            if i.wants_paste() {
                i.paste = window.clipboard_text();
            }
            ed.frame(i, &batch, w, h);
            if let Some(text) = ed.ui.copy.take() {
                window.set_clipboard_text(&text);
            }
            if ed.quit {
                break;
            }
        }
        if ed.quit {
            break;
        }
        if ed.ui.cursor != cursor {
            cursor = ed.ui.cursor;
            window.set_cursor(cursor);
        }
        let t = ed.title();
        if t != title {
            window.set_title(&t);
            title = t;
        }
        let px = ed.render(w as usize, h as usize);
        window.present(&px, w as usize, h as usize, &[]);
    }
    if ed.ws.doc.is_dirty() {
        eprintln!("warning: quit with unsaved changes");
    }
    ed.save_settings();
    Ok(())
}
