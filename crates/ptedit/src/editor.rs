//! The editor window: a menu bar, the outliner (left), the viewport (middle), the
//! inspector (right) and a status bar, with splitters between the panels. Every frame runs
//! the UI over one batch of input; mouse input the panels do not take goes to the viewport,
//! and keys nothing else wants are shortcuts.
//!
//! The viewport image is drawn only when something in it changed; the panels are drawn
//! over it every frame.

use crate::edit::Add;
use crate::files::{self, FileDialog, Mode, Outcome};
use crate::gizmo::{Decor, Tool};
use crate::modeling::MeshCmd;
use crate::overlay::{LIGHT_ID, Selection};
use crate::render::Shading;
use crate::settings::{self, Settings};
use crate::tools::{DecorKey, Snap, ToolDrag};
use crate::ui::font::Font;
use crate::ui::theme::Theme;
use crate::ui::{Column, Id, Input, PopupFrame, Rect, Ui, id_of};
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
    "Editing",
    "  Q W E R: select / move / rotate / scale     T: world / local axes     N: snapping",
    "  drag the gizmo; while dragging type a number, X Y Z (Shift: plane) pick the axis,",
    "    Ctrl snaps, Enter keeps, Esc cancels     handles: spot cones, sun, scatter, focus",
    "  Shift+A: add under the mouse     Delete: delete     Ctrl+D: duplicate",
    "  Ctrl+C / X / V: copy, cut, paste (also between scenes)     Ctrl+P: make a prototype",
    "  C: look through the camera     Shift+C: lock the camera to the view     Ctrl+F: pick focus",
    "  P: path-traced view     Ctrl+B: draw a render region (Ctrl+Alt+B: clear it)     F5: tracer",
    "Modeling (Tab: edit the selected object as a mesh, Tab again: write it next to the scene)",
    "  1 2 3: vertices / edges / faces     click, Shift / Ctrl: select     double-click: loop (Ctrl: ring)",
    "  Ctrl+A: all     Ctrl+Plus / Minus: grow / shrink     Ctrl+L: linked     Ctrl+Z: undo in edit mode",
    "  Ctrl+E: extrude (X Y Z: along an axis)     I: inset     Ctrl+B: bevel (Shift: vertices)",
    "  Ctrl+R: loop cut (wheel: more cuts)     K: knife (drag a line)     Alt+M: merge     Alt+F: fill",
    "  Delete: delete     Ctrl+Delete: dissolve     Alt+N: flip     Ctrl+T: triangulate     Shift+E: crease",
    "  Shift+O: proportional editing (wheel while dragging: radius)     the Mesh menu has the rest",
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
    Tool(Tool),
    ToggleSpace,
    CycleSnap,
    Snap(Snap),
    AddMenu,
    Add(Add),
    Delete,
    Duplicate,
    Copy,
    Cut,
    Paste,
    MakePrototype,
    CameraView,
    LockCamera,
    PickFocus,
    Rendered,
    RenderRegion,
    ClearRegion,
    RenderInTracer,
    /// Into or out of edit mode.
    EditMode,
    Mesh(MeshCmd),
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
    (Key::Char('q'), false, false, false, Cmd::Tool(Tool::Select)),
    (Key::Char('w'), false, false, false, Cmd::Tool(Tool::Move)),
    (Key::Char('e'), false, false, false, Cmd::Tool(Tool::Rotate)),
    (Key::Char('r'), false, false, false, Cmd::Tool(Tool::Scale)),
    (Key::Char('t'), false, false, false, Cmd::ToggleSpace),
    (Key::Char('n'), false, false, false, Cmd::CycleSnap),
    (Key::Char('a'), false, true, false, Cmd::AddMenu),
    (Key::Delete, false, false, false, Cmd::Delete),
    (Key::Char('d'), true, false, false, Cmd::Duplicate),
    (Key::Char('c'), true, false, false, Cmd::Copy),
    (Key::Char('x'), true, false, false, Cmd::Cut),
    (Key::Char('v'), true, false, false, Cmd::Paste),
    (Key::Char('p'), true, false, false, Cmd::MakePrototype),
    (Key::Char('c'), false, false, false, Cmd::CameraView),
    (Key::Char('c'), false, true, false, Cmd::LockCamera),
    (Key::Char('f'), true, false, false, Cmd::PickFocus),
    (Key::Char('p'), false, false, false, Cmd::Rendered),
    (Key::Char('b'), true, false, false, Cmd::RenderRegion),
    (Key::Char('b'), true, false, true, Cmd::ClearRegion),
    (Key::F(5), false, false, false, Cmd::RenderInTracer),
    (Key::Tab, false, false, false, Cmd::EditMode),
];

/// Edit mode's keys, looked at before the others there.
const EDIT_SHORTCUTS: &[(Key, bool, bool, bool, Cmd)] = &[
    (Key::Char('1'), false, false, false, Cmd::Mesh(MeshCmd::Mode(pt_geom::Mode::Vertex))),
    (Key::Char('2'), false, false, false, Cmd::Mesh(MeshCmd::Mode(pt_geom::Mode::Edge))),
    (Key::Char('3'), false, false, false, Cmd::Mesh(MeshCmd::Mode(pt_geom::Mode::Face))),
    (Key::Char('e'), true, false, false, Cmd::Mesh(MeshCmd::Extrude)),
    (Key::Char('i'), false, false, false, Cmd::Mesh(MeshCmd::Inset)),
    (Key::Char('b'), true, false, false, Cmd::Mesh(MeshCmd::Bevel)),
    (Key::Char('b'), true, true, false, Cmd::Mesh(MeshCmd::BevelVertices)),
    (Key::Char('r'), true, false, false, Cmd::Mesh(MeshCmd::LoopCut)),
    (Key::Char('k'), false, false, false, Cmd::Mesh(MeshCmd::Knife)),
    (Key::Char('m'), false, false, true, Cmd::Mesh(MeshCmd::Merge)),
    (Key::Char('f'), false, false, true, Cmd::Mesh(MeshCmd::Fill)),
    (Key::Char('n'), false, false, true, Cmd::Mesh(MeshCmd::Flip)),
    (Key::Char('t'), true, false, false, Cmd::Mesh(MeshCmd::Triangulate)),
    (Key::Char('l'), true, false, false, Cmd::Mesh(MeshCmd::Linked)),
    (Key::Plus, true, false, false, Cmd::Mesh(MeshCmd::Grow)),
    (Key::Minus, true, false, false, Cmd::Mesh(MeshCmd::Shrink)),
    (Key::Delete, true, false, false, Cmd::Mesh(MeshCmd::Dissolve)),
    (Key::Char('e'), false, true, false, Cmd::Mesh(MeshCmd::Crease)),
    (Key::Char('o'), false, true, false, Cmd::Mesh(MeshCmd::Proportional)),
];

fn shortcut(cmd: Cmd) -> String {
    let Some(&(key, ctrl, shift, alt, _)) = SHORTCUTS.iter().chain(EDIT_SHORTCUTS).find(|s| s.4 == cmd) else {
        return String::new();
    };
    let name = match key {
        Key::Char(c) => c.to_ascii_uppercase().to_string(),
        Key::F(n) => format!("F{n}"),
        Key::Escape => "Esc".into(),
        Key::Home => "Home".into(),
        Key::Space => "Space".into(),
        Key::Delete => "Delete".into(),
        Key::Tab => "Tab".into(),
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

/// The menu bar's menus (in edit mode Mesh takes the place of Add).
const MENUS: [&str; 4] = ["File", "Edit", "Add", "View"];
const EDIT_MENUS: [&str; 4] = ["File", "Edit", "Mesh", "View"];

const FLY_KEYS: [Key; 6] =
    [Key::Char('w'), Key::Char('a'), Key::Char('s'), Key::Char('d'), Key::Char('q'), Key::Char('e')];

#[derive(Clone, Copy, Debug, PartialEq)]
enum DragMode {
    Orbit,
    Pan,
    Look,
    /// Left button: a click or a selection rectangle, from where it started (window pixels),
    /// and clicks in a row.
    Select {
        x0: i32,
        y0: i32,
        mods: Modifiers,
        clicks: u8,
    },
    /// Left button on the gizmo, a handle, or picking a region (`Editor::tool_drag`).
    Tool,
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
    /// A file changed on disk while it has unsaved changes here.
    Changed(usize),
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
    pub tool: Tool,
    /// Gizmos along the items' own axes, not the world's.
    pub local_space: bool,
    pub snap: Snap,
    pub tool_drag: Option<ToolDrag>,
    /// The next click in a view sets the camera's focus distance / starts a render region.
    pub focus_pick: bool,
    pub region_pick: bool,
    /// Counter for undo merge keys of drags.
    pub groups: u64,
    pub camera_moves: u64,
    pub decor_key: Option<DecorKey>,
    pub decor_cache: Option<Decor>,
    /// A tool drag was ended from the keyboard.
    pub drag_done: bool,
    /// Ids to select once the scene is rebuilt (things just added).
    pub pending_select: Option<Vec<u32>>,
    /// Where Shift+A was pressed (window pixels); None: the middle of the view.
    pub add_at: Option<(i32, i32)>,
    /// A mesh to add once its file is picked: where.
    pub add_mesh_at: Option<Vec3>,
    /// The left panel's tab: 0 outliner, 1 materials.
    pub left_tab: usize,
    pub library: crate::library::Library,
    pub watch: crate::watch::Watch,
    /// A prototype chosen in the Add menu.
    chosen_instance: Option<String>,
    /// The last text copied (pasting from the menu, which cannot read the clipboard).
    pub clip: Option<String>,
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
            tool: Tool::Move,
            local_space: false,
            snap: Snap::Off,
            tool_drag: None,
            focus_pick: false,
            region_pick: false,
            groups: 0,
            camera_moves: 0,
            decor_key: None,
            decor_cache: None,
            drag_done: false,
            pending_select: None,
            add_at: None,
            add_mesh_at: None,
            left_tab: 0,
            library: crate::library::Library::default(),
            watch: crate::watch::Watch::default(),
            chosen_instance: None,
            clip: None,
        };
        ed.watch.track(&ed.ws);
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
        // Tab moves between fields once one has the focus; otherwise it is edit mode's key.
        let mut input = input;
        let mut tab = false;
        if self.ui.focus == 0
            && self.dialog.is_none()
            && !self.ui.modal_open()
            && let Some(i) = input.keys.iter().position(|&(k, m)| k == Key::Tab && !m.ctrl && !m.alt && !m.shift)
        {
            input.keys.remove(i);
            tab = true;
        }
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
            let (tabs, body) = l.split_top(rh + self.ui.px(4.0));
            self.ui.fill(tabs, self.ui.theme.bar);
            let tr = tabs.shrink2(self.ui.px(4.0), self.ui.px(2.0));
            self.probe("tab:outliner", Rect { w: tr.w / 2, ..tr });
            self.probe("tab:materials", Rect { x: tr.x + tr.w / 2, w: tr.w / 2, ..tr });
            if let Some(t) = self.ui.tabs(id_of(VIEWPORT, "left-tabs"), tr, &["Scene", "Materials"], self.left_tab) {
                self.left_tab = t;
            }
            if self.left_tab == 0 {
                self.outliner(body);
            } else {
                self.library_panel(body);
            }
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
        self.toolbar();
        self.watch_files();
        if self.ws.rendered && self.ws.pt.changed() {
            self.vp_dirty = true;
        }
        self.viewport_input(events);
        self.viewport_drop();
        self.viewport_overlay();
        self.add_popup();
        self.dialogs();
        if tab {
            self.run(Cmd::EditMode);
        }
        if !self.tool_keys() {
            self.shortcuts();
        }
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
            self.watch.track(&self.ws);
        }
        if self.ws.env_preview.is_some() && !self.ui.input.down[0] {
            // The environment's rotation was let go: load it.
            match self.ws.reload() {
                Ok(()) => self.vp_dirty = true,
                Err(e) => self.status = format!("the scene does not load: {e}"),
            }
            self.ws.env_preview = None;
        }
        if let Some(ids) = self.pending_select.take() {
            self.ws.sel.clear();
            for id in ids {
                self.ws.sel.set_id(id, true);
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
        self.update_decor();
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
        if flying {
            return Some(Instant::now());
        }
        // The path-traced view and the material previews: show them as they come.
        let busy = (self.ws.rendered && self.ws.pt.running()) || self.library.busy();
        let soon = busy.then(|| Instant::now() + Duration::from_millis(40));
        let watch = Some(self.watch.next_check());
        [self.ui.repaint_at(), soon, watch].into_iter().flatten().min()
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
        let menus = if self.ws.edit.is_some() { EDIT_MENUS } else { MENUS };
        let ids: Vec<Id> = menus.iter().map(|t| id_of(VIEWPORT, ("menu", *t))).collect();
        let mut x = r.x + ui.px(4.0);
        for (k, title) in menus.into_iter().enumerate() {
            let w = self.ui.text_w(title) + 2 * self.ui.pad() + self.ui.px(6.0);
            let tr = Rect::new(x, r.y + self.ui.px(2.0), w, r.h - self.ui.px(4.0));
            x += w;
            self.probe(format!("menu:{title}"), tr);
            let Some(mut f) = self.ui.menu(ids[k], tr, title, &ids) else { continue };
            match title {
                "File" => self.file_menu(&mut f),
                "Edit" => self.edit_menu(&mut f),
                "Add" => {
                    self.add_at = None;
                    self.add_items(&mut f);
                }
                "Mesh" => self.mesh_menu(&mut f),
                _ => self.view_menu(&mut f),
            }
            self.ui.end_popup(f);
        }
        if let Some(c) = self.chosen.take() {
            self.run(c);
        }
    }

    /// A menu item that runs `cmd`, showing its shortcut.
    fn item(&mut self, f: &mut PopupFrame, label: &str, cmd: Cmd, enabled: bool) {
        if self.ui.menu_item(f, label, &shortcut(cmd), enabled) {
            self.chosen = Some(cmd);
        }
    }

    /// A menu item with a check mark that runs `cmd`.
    fn check(&mut self, f: &mut PopupFrame, label: &str, cmd: Cmd, on: bool) {
        if self.ui.menu_check(f, label, &shortcut(cmd), on) {
            self.chosen = Some(cmd);
        }
    }

    fn file_menu(&mut self, f: &mut PopupFrame) {
        self.item(f, "Open…", Cmd::Open, true);
        let recent: Vec<String> = self.settings.recent.clone();
        if let Some(mut sub) = self.ui.submenu(f, "Open Recent", !recent.is_empty()) {
            for path in &recent {
                if self.ui.menu_item(&mut sub, path, "", true) {
                    self.request_open(PathBuf::from(path));
                }
            }
            self.ui.end_popup(sub);
        }
        self.ui.menu_separator(f);
        let dirty = self.ws.doc.is_dirty();
        self.item(f, "Save", Cmd::Save, dirty);
        self.item(f, "Save As…", Cmd::SaveAs, true);
        self.ui.menu_separator(f);
        self.item(f, "Render in the Tracer", Cmd::RenderInTracer, true);
        self.ui.menu_separator(f);
        self.item(f, "Quit", Cmd::Quit, true);
    }

    fn edit_menu(&mut self, f: &mut PopupFrame) {
        let (undo, redo) = match &self.ws.edit {
            Some(e) => (e.undo_label().map(|l| format!("Undo {l}")), e.redo_label().map(|l| format!("Redo {l}"))),
            None => (
                self.ws.doc.history().undo_label().map(|l| format!("Undo {l}")),
                self.ws.doc.history().redo_label().map(|l| format!("Redo {l}")),
            ),
        };
        self.item(f, undo.as_deref().unwrap_or("Undo"), Cmd::Undo, undo.is_some());
        self.item(f, redo.as_deref().unwrap_or("Redo"), Cmd::Redo, redo.is_some());
        self.ui.menu_separator(f);
        let editing = self.ws.edit.is_some();
        self.check(f, "Edit Mesh", Cmd::EditMode, editing);
        if editing {
            self.item(f, "Select All", Cmd::SelectAll, true);
            self.item(f, "Clear Selection", Cmd::ClearSelection, true);
            return;
        }
        self.ui.menu_separator(f);
        let some = !self.ws.sel.is_empty();
        self.item(f, "Select All", Cmd::SelectAll, true);
        self.item(f, "Clear Selection", Cmd::ClearSelection, some);
        self.item(f, "Rename", Cmd::Rename, matches!(self.inspect, Some(Target::Item(..))));
        self.ui.menu_separator(f);
        self.item(f, "Hide Selection", Cmd::Hide, some);
        self.item(f, "Show All", Cmd::ShowAll, !self.ws.hidden.is_empty());
        self.ui.menu_separator(f);
        self.item(f, "Delete", Cmd::Delete, some || self.inspect.is_some());
        self.item(f, "Duplicate", Cmd::Duplicate, some);
        self.item(f, "Cut", Cmd::Cut, some);
        self.item(f, "Copy", Cmd::Copy, some);
        self.item(f, "Paste", Cmd::Paste, true);
        self.item(f, "Make Prototype", Cmd::MakePrototype, some);
        self.ui.menu_separator(f);
        for (t, label) in
            [(Tool::Select, "Select"), (Tool::Move, "Move"), (Tool::Rotate, "Rotate"), (Tool::Scale, "Scale")]
        {
            self.check(f, label, Cmd::Tool(t), self.tool == t);
        }
        self.check(f, "Local Axes", Cmd::ToggleSpace, self.local_space);
        if let Some(mut sub) = self.ui.submenu(f, "Snap", true) {
            for s in Snap::ALL {
                if self.ui.menu_check(&mut sub, s.name(), "", self.snap == s) {
                    self.chosen = Some(Cmd::Snap(s));
                }
            }
            self.ui.end_popup(sub);
        }
    }

    /// Edit mode's operations.
    fn mesh_menu(&mut self, f: &mut PopupFrame) {
        let mode = self.ws.edit.as_ref().map_or(pt_geom::Mode::Vertex, |e| e.mode);
        for (m, label) in
            [(pt_geom::Mode::Vertex, "Vertices"), (pt_geom::Mode::Edge, "Edges"), (pt_geom::Mode::Face, "Faces")]
        {
            self.check(f, label, Cmd::Mesh(MeshCmd::Mode(m)), mode == m);
        }
        self.ui.menu_separator(f);
        let items: [(&str, MeshCmd); 5] = [
            ("Select Loops", MeshCmd::SelectLoop),
            ("Select Rings", MeshCmd::SelectRing),
            ("Grow Selection", MeshCmd::Grow),
            ("Shrink Selection", MeshCmd::Shrink),
            ("Select Linked", MeshCmd::Linked),
        ];
        for (label, c) in items {
            self.item(f, label, Cmd::Mesh(c), true);
        }
        self.ui.menu_separator(f);
        let ops: [(&str, MeshCmd); 6] = [
            ("Extrude", MeshCmd::Extrude),
            ("Inset", MeshCmd::Inset),
            ("Bevel", MeshCmd::Bevel),
            ("Bevel Vertices", MeshCmd::BevelVertices),
            ("Loop Cut", MeshCmd::LoopCut),
            ("Knife", MeshCmd::Knife),
        ];
        for (label, c) in ops {
            self.item(f, label, Cmd::Mesh(c), true);
        }
        self.ui.menu_separator(f);
        let more: [(&str, MeshCmd); 7] = [
            ("Bridge", MeshCmd::Bridge),
            ("Fill Hole", MeshCmd::Fill),
            ("Merge at Center", MeshCmd::Merge),
            ("Weld by Distance", MeshCmd::Weld),
            ("Dissolve", MeshCmd::Dissolve),
            ("Delete", MeshCmd::Delete),
            ("Crease Edges", MeshCmd::Crease),
        ];
        for (label, c) in more {
            self.item(f, label, Cmd::Mesh(c), true);
        }
        if let Some(mut sub) = self.ui.submenu(f, "Mirror", true) {
            for (k, label) in ["Across X", "Across Y", "Across Z"].into_iter().enumerate() {
                if self.ui.menu_item(&mut sub, label, "", true) {
                    self.chosen = Some(Cmd::Mesh(MeshCmd::Mirror(k)));
                }
            }
            self.ui.end_popup(sub);
        }
        self.item(f, "Flip Normals", Cmd::Mesh(MeshCmd::Flip), true);
        self.item(f, "Triangulate", Cmd::Mesh(MeshCmd::Triangulate), true);
        self.ui.menu_separator(f);
        let prop = self.ws.edit.as_ref().is_some_and(|e| e.proportional);
        self.check(f, "Proportional Editing", Cmd::Mesh(MeshCmd::Proportional), prop);
        self.item(f, "Leave Edit Mode", Cmd::EditMode, true);
    }

    fn view_menu(&mut self, f: &mut PopupFrame) {
        for s in Shading::ALL {
            let label = match s {
                Shading::Wireframe => "Wireframe",
                Shading::Solid => "Solid",
                Shading::Shaded => "Shaded",
                Shading::WireSolid => "Wireframe on Solid",
                Shading::Rendered => "Rendered",
            };
            self.check(f, label, Cmd::Shading(s), self.ws.shading == s);
        }
        self.check(f, "Path Traced", Cmd::Rendered, self.ws.rendered);
        self.item(f, "Render Region", Cmd::RenderRegion, true);
        self.item(f, "Clear Render Region", Cmd::ClearRegion, self.ws.region.is_some());
        self.ui.menu_separator(f);
        self.check(f, "Through the Camera", Cmd::CameraView, self.ws.through_camera);
        self.check(f, "Lock Camera to View", Cmd::LockCamera, self.ws.lock_camera);
        self.item(f, "Pick Focus…", Cmd::PickFocus, true);
        self.ui.menu_separator(f);
        self.check(f, "Grid", Cmd::Grid, self.ws.grid);
        self.check(f, "All Bounds", Cmd::Bounds, self.ws.all_bounds);
        self.check(f, "Four Views", Cmd::Layout, self.ws.quad);
        self.check(f, "Orthographic", Cmd::ToggleOrtho, self.ws.views[self.ws.active].ortho);
        self.item(f, "Frame Selection", Cmd::Frame, true);
        self.item(f, "Reset View", Cmd::ResetView, true);
        self.ui.menu_separator(f);
        let panels = [
            ("Outliner", Cmd::Outliner, self.settings.outliner),
            ("Inspector", Cmd::Inspector, self.settings.inspector),
            ("Light Theme", Cmd::Theme, self.settings.theme == "light"),
        ];
        for (label, cmd, on) in panels {
            if self.ui.menu_check(f, label, "", on) {
                self.chosen = Some(cmd);
            }
        }
        self.ui.menu_separator(f);
        self.item(f, "Help", Cmd::Help, true);
    }

    fn status_bar(&mut self, r: Rect) {
        let ui = &mut self.ui;
        ui.fill(r, ui.theme.bar);
        ui.fill(Rect::new(r.x, r.y, r.w, 1), ui.theme.separator);
        let s = &self.ws.stats;
        let edit = self.ws.edit.as_ref().map_or_else(String::new, |e| {
            format!(
                "editing {}: {} vertices, {} faces, {} mode{} · ",
                e.label,
                e.mesh.vertex_count(),
                e.mesh.face_count(),
                e.mode.name(),
                if e.proportional { ", proportional" } else { "" }
            )
        });
        let stats = format!(
            "{edit}{} {} · snap {} · {} · {} triangles{} · {:.1} ms",
            self.tool.name(),
            if self.local_space { "local" } else { "world" },
            self.snap.name(),
            if self.ws.rendered { "path traced" } else { self.ws.shading.name() },
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
        if self.drag_done {
            // A tool drag ended from the keyboard.
            self.drag_done = false;
            self.drag = None;
            if self.ui.active == VIEWPORT {
                self.ui.active = 0;
            }
        }
        if !modal && free(&self.ui) && self.vp.contains(mx, my) && self.ui.input.wheel != 0.0 {
            if self.ws.edit.is_some() && self.edit_wheel(self.ui.input.wheel) {
                // Loop cuts or the proportional radius.
            } else if let Some(r) = self.view_at(mx, my) {
                self.ws.views[r.view].zoom(self.ui.input.wheel);
                self.view_moved(r.view);
                self.view_move_done();
                self.vp_dirty = true;
            }
            self.ui.input.wheel = 0.0;
        }
        for ev in events {
            match *ev {
                Event::FocusLost => {
                    self.held.clear();
                    if self.drag.take().is_some_and(|d| d.mode == DragMode::Tool) {
                        let (x, y) = self.ui.input.mouse;
                        self.tool_release(x, y);
                    }
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
                    if self.modal_press(button, x, y) {
                        continue;
                    }
                    let clicks = self.ui.input.clicks;
                    let mode = match button {
                        MouseButton::Left if mods.alt && mods.shift => DragMode::Pan,
                        MouseButton::Left if mods.alt => DragMode::Orbit,
                        MouseButton::Left if self.tool_press(&view, x, y) => DragMode::Tool,
                        MouseButton::Left => DragMode::Select { x0: x, y0: y, mods, clicks },
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
                        DragMode::Select { .. } | DragMode::Tool => button == MouseButton::Left,
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
                    match d.mode {
                        DragMode::Select { x0, y0, mods, clicks } if self.ws.edit.is_some() => {
                            self.edit_select(&d.view, x0, y0, x, y, mods, clicks);
                        }
                        DragMode::Select { x0, y0, mods, .. } => self.select(&d.view, x0, y0, x, y, mods),
                        DragMode::Tool => self.tool_release(x, y),
                        DragMode::Look => {
                            self.held.clear();
                            self.view_move_done();
                        }
                        DragMode::Orbit | DragMode::Pan => self.view_move_done(),
                    }
                    self.vp_dirty = true;
                }
                // An operation following the mouse, or the loop cut looking for an edge.
                Event::MouseMove { x, y, mods }
                    if self.drag.is_none() && !modal && self.modal_mouse(x, y, mods.ctrl) => {}
                Event::MouseMove { x, y, mods } => match self.drag {
                    Some(mut d) => {
                        let (dx, dy) = ((x - d.x) as f32, (y - d.y) as f32);
                        let view = &mut self.ws.views[d.view.view];
                        match d.mode {
                            DragMode::Orbit if view.is_fixed() => view.pan(dx, dy, d.view.h as f32),
                            DragMode::Orbit => view.orbit(dx, dy),
                            DragMode::Pan => view.pan(dx, dy, d.view.h as f32),
                            DragMode::Look => view.look(dx, dy),
                            DragMode::Select { .. } | DragMode::Tool => {}
                        }
                        (d.x, d.y) = (x, y);
                        self.drag = Some(d);
                        match d.mode {
                            DragMode::Select { .. } => {}
                            DragMode::Tool => self.tool_move(x, y, mods),
                            _ => {
                                self.view_moved(d.view.view);
                                self.vp_dirty = true;
                            }
                        }
                    }
                    None => {
                        if !modal
                            && free(&self.ui)
                            && self.vp.contains(x, y)
                            && let Some(r) = self.view_at(x, y)
                        {
                            self.set_active(r.view);
                            self.tool_hover(&r, x, y);
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
            self.view_moved(d.view.view);
            self.vp_dirty = true;
        }
    }

    /// The tool bar over the top left of the viewport: tools, axes, snapping, the
    /// path-traced view and the camera.
    fn toolbar(&mut self) {
        let rh = self.ui.row_h();
        let pad = self.ui.px(4.0);
        let mut x = self.vp.x + pad;
        let y = self.vp.y + pad;
        let id = id_of(VIEWPORT, "toolbar");
        let right = self.vp.right() - pad;
        let button = |ed: &mut Editor, x: &mut i32, name: &str, label: &str, on: bool, tip: &str| -> bool {
            let w = ed.ui.text_w(label) + 2 * ed.ui.pad();
            let r = Rect::new(*x, y, w, rh);
            *x += w + ed.ui.px(2.0);
            if r.right() > right {
                return false; // a narrow viewport: the shortcuts still work
            }
            ed.probe(format!("toolbar:{name}"), r);
            ed.ui.fill_rounded(r, if on { ed.ui.theme.accent } else { ed.ui.theme.panel }, 230, ed.ui.px(3.0));
            let resp = ed.ui.flat_button(id_of(id, name), r, label, if on { 0xff_ff_ff } else { ed.ui.theme.text });
            ed.ui.tooltip(&resp, tip);
            resp.clicked
        };
        let tools = [
            (Tool::Select, "select", "Select", "Q: select only"),
            (Tool::Move, "move", "Move", "W: move gizmo"),
            (Tool::Rotate, "rotate", "Rotate", "E: rotate gizmo"),
            (Tool::Scale, "scale", "Scale", "R: scale gizmo"),
        ];
        for (t, name, label, tip) in tools {
            if button(self, &mut x, name, label, self.tool == t, tip) {
                self.run(Cmd::Tool(t));
            }
        }
        x += pad;
        let space = if self.local_space { "Local" } else { "World" };
        if button(self, &mut x, "space", space, false, "T: gizmo axes of the world or of the item") {
            self.run(Cmd::ToggleSpace);
        }
        let snap = format!("Snap: {}", self.snap.name());
        if button(self, &mut x, "snap", &snap, self.snap != Snap::Off, "N: snapping (Ctrl while dragging flips it)") {
            self.run(Cmd::CycleSnap);
        }
        x += pad;
        if button(self, &mut x, "rendered", "Path Traced", self.ws.rendered, "P: path trace the perspective view") {
            self.run(Cmd::Rendered);
        }
        if button(self, &mut x, "camera", "Camera", self.ws.through_camera, "C: look through the scene camera") {
            self.run(Cmd::CameraView);
        }
        if self.ws.through_camera
            && button(self, &mut x, "lock", "Lock", self.ws.lock_camera, "Shift+C: the camera follows the view")
        {
            self.run(Cmd::LockCamera);
        }
    }

    /// Something dragged from the outliner or the material library let go over a view:
    /// a material is assigned to the object under it.
    fn viewport_drop(&mut self) {
        if self.ui.modal_open() || !self.vp.contains(self.ui.input.mouse.0, self.ui.input.mouse.1) {
            return;
        }
        let vp = self.vp;
        let Some(p) = self.ui.dropped(vp) else { return };
        let (mx, my) = self.ui.input.mouse;
        let Some(r) = self.view_at(mx, my) else { return };
        let (lx, ly) = ((mx - self.vp.x - r.x as i32) as f32 + 0.5, (my - self.vp.y - r.y as i32) as f32 + 0.5);
        let Some(obj) = self.object_at(&r, lx, ly) else {
            self.notify("drop materials on an object");
            return;
        };
        if let Some(name) = self.material_payload(p) {
            self.assign_material_name(obj, &name);
        }
    }

    /// The selection rectangle and the help, over the viewport.
    fn viewport_overlay(&mut self) {
        // The scene camera's frame: what is outside the image is dimmed.
        if self.ws.through_camera {
            let (vw, vh) = (self.vp.w.max(1) as usize, self.vp.h.max(1) as usize);
            if let Some(r) = self.ws.rects(vw, vh).into_iter().find(|r| r.view == PERSPECTIVE) {
                let s = &self.ws.scene.settings;
                let aspect = s.width as f32 / s.height.max(1) as f32;
                let fw = ((r.h as f32 * aspect) as i32).min(r.w as i32);
                let fh = ((fw as f32 / aspect) as i32).min(r.h as i32);
                let (x0, y0) = (self.vp.x + r.x as i32, self.vp.y + r.y as i32);
                let f = Rect::new(x0 + (r.w as i32 - fw) / 2, y0 + (r.h as i32 - fh) / 2, fw, fh);
                self.ui.push_clip(self.vp);
                for side in [
                    Rect::new(x0, y0, r.w as i32, f.y - y0),
                    Rect::new(x0, f.bottom(), r.w as i32, y0 + r.h as i32 - f.bottom()),
                    Rect::new(x0, f.y, f.x - x0, f.h),
                    Rect::new(f.right(), f.y, x0 + r.w as i32 - f.right(), f.h),
                ] {
                    if side.w > 0 && side.h > 0 {
                        self.ui.fill_rounded(side, 0, 110, 0);
                    }
                }
                self.ui.border(f, 0x8c_c4_ff, 0);
                self.ui.pop_clip();
            }
        }
        if let Some(ToolDrag::Knife(k)) = &self.tool_drag
            && let Some(r) = k.view
        {
            let (ox, oy) = ((self.vp.x + r.x as i32) as f32, (self.vp.y + r.y as i32) as f32);
            let (a, b) = ((ox + k.from.0, oy + k.from.1), (ox + k.to.0, oy + k.to.1));
            self.ui.push_clip(self.vp);
            self.ui.line(a, b, 2.0, 0xff_e0_40);
            self.ui.pop_clip();
        }
        if let Some(ToolDrag::Region(r, (x0, y0))) = &self.tool_drag {
            let (ox, oy) = (self.vp.x + r.x as i32, self.vp.y + r.y as i32);
            let (mx, my) = self.ui.input.mouse;
            let (ax, ay) = (ox + x0, oy + y0);
            let rr = Rect::new(ax.min(mx), ay.min(my), (mx - ax).abs() + 1, (my - ay).abs() + 1);
            self.ui.push_clip(self.vp);
            self.ui.border(rr, 0xd0_a0_40, 0);
            self.ui.pop_clip();
        }
        if self.focus_pick || self.region_pick {
            let hint = if self.focus_pick { "click what should be in focus" } else { "drag the region to render" };
            let w = self.ui.text_w(hint) + 2 * self.ui.pad();
            let r = Rect::new(self.vp.x + (self.vp.w - w) / 2, self.vp.y + self.ui.row_h() * 2, w, self.ui.row_h());
            self.ui.fill_rounded(r, self.ui.theme.panel, 230, self.ui.px(3.0));
            self.ui.label_centered(r, hint, self.ui.theme.accent);
        }
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
            let editing: &[_] = if self.ws.edit.is_some() { EDIT_SHORTCUTS } else { &[] };
            let found = editing
                .iter()
                .chain(SHORTCUTS)
                .find(|s| s.0 == key && s.1 == mods.ctrl && s.2 == mods.shift && s.3 == mods.alt);
            // Unshifted single keys also work with Shift held (Shift+F keeps framing).
            let found = found.or_else(|| {
                (!mods.ctrl && !mods.alt)
                    .then(|| editing.iter().chain(SHORTCUTS).find(|s| s.0 == key && !s.1 && !s.3))
                    .flatten()
            });
            if let Some(&(.., cmd)) = found {
                self.run(cmd);
            }
        }
    }

    /// Does `cmd`.
    #[expect(clippy::too_many_lines, reason = "one arm per command")]
    pub fn run(&mut self, cmd: Cmd) {
        let v = self.ws.active;
        if self.ws.edit.is_some() {
            // Edit mode: selection, undo and deleting work on the mesh; files are written
            // with the mesh; what works on objects waits.
            match cmd {
                Cmd::Undo | Cmd::Redo => return self.edit_undo(cmd == Cmd::Undo),
                Cmd::SelectAll => return self.edit_select_all(),
                Cmd::ClearSelection if !self.help => return self.edit_clear(),
                Cmd::Frame => return self.edit_frame(v),
                Cmd::Delete => return self.mesh_cmd(MeshCmd::Delete),
                Cmd::Save | Cmd::SaveAs | Cmd::RenderInTracer => self.leave_edit(),
                Cmd::Hide
                | Cmd::ShowAll
                | Cmd::Duplicate
                | Cmd::Copy
                | Cmd::Cut
                | Cmd::Paste
                | Cmd::MakePrototype
                | Cmd::Rename
                | Cmd::AddMenu
                | Cmd::Add(_) => return self.notify("that works on objects: Tab leaves edit mode first"),
                _ => {}
            }
        }
        match cmd {
            Cmd::EditMode => self.toggle_edit(),
            Cmd::Mesh(c) => self.mesh_cmd(c),
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
            Cmd::Tool(t) => {
                self.tool = t;
                self.notify(format!("tool: {}", t.name()));
            }
            Cmd::ToggleSpace => {
                self.local_space = !self.local_space;
                self.notify(if self.local_space { "local axes" } else { "world axes" });
            }
            Cmd::CycleSnap => {
                let i = Snap::ALL.iter().position(|&s| s == self.snap).unwrap_or(0);
                self.run(Cmd::Snap(Snap::ALL[(i + 1) % Snap::ALL.len()]));
            }
            Cmd::Snap(s) => {
                self.snap = s;
                self.notify(format!("snap: {}", s.name()));
            }
            Cmd::AddMenu => {
                let (mx, my) = self.ui.input.mouse;
                self.add_at = self.vp.contains(mx, my).then_some((mx, my));
                let at = self.add_at.unwrap_or((self.vp.x + self.vp.w / 2, self.vp.y + self.vp.h / 2));
                self.ui.open_popup(id_of(VIEWPORT, "add-popup"), at.0, at.1, Rect::new(at.0, at.1, 1, 1));
            }
            Cmd::Add(a) => self.add(a),
            Cmd::Delete => self.delete_selection(),
            Cmd::Duplicate => self.duplicate(),
            Cmd::Copy => self.copy(),
            Cmd::Cut => self.cut(),
            Cmd::Paste => match self.ui.input.paste.take().or_else(|| self.clip.clone()) {
                Some(t) => self.paste(&t),
                None => self.notify("the clipboard is empty"),
            },
            Cmd::MakePrototype => self.make_prototype(),
            Cmd::CameraView => {
                self.ws.through_camera = !self.ws.through_camera;
                if self.ws.through_camera {
                    let cam = self.ws.scene.camera.clone();
                    self.ws.views[PERSPECTIVE].set_camera(&cam);
                    if self.ws.quad && self.ws.maximized.is_none() {
                        self.ws.active = PERSPECTIVE;
                    }
                    self.notify("looking through the scene camera (Shift+C locks it to the view)");
                } else {
                    self.ws.lock_camera = false;
                    self.notify("left the camera view");
                }
                self.vp_dirty = true;
            }
            Cmd::LockCamera => {
                self.ws.lock_camera = !self.ws.lock_camera;
                if self.ws.lock_camera && !self.ws.through_camera {
                    self.run(Cmd::CameraView);
                }
                self.notify(if self.ws.lock_camera {
                    "the camera follows the view"
                } else {
                    "the camera stays where it is"
                });
                self.vp_dirty = true;
            }
            Cmd::PickFocus => {
                self.focus_pick = true;
                self.notify("click what should be in focus");
            }
            Cmd::Rendered => {
                self.ws.rendered = !self.ws.rendered;
                if !self.ws.rendered {
                    self.ws.pt.release();
                }
                self.notify(if self.ws.rendered { "path-traced view" } else { "raster view" });
                self.vp_dirty = true;
            }
            Cmd::RenderRegion => {
                self.region_pick = true;
                self.notify("drag the region to render in the perspective view");
            }
            Cmd::ClearRegion => {
                self.ws.region = None;
                self.vp_dirty = true;
                self.notify("render region cleared");
            }
            Cmd::RenderInTracer => self.render_in_tracer(),
        }
    }

    /// Starts the tracer's window on the scene (as saved), beside the editor.
    fn render_in_tracer(&mut self) {
        let name = if cfg!(windows) { "pathtracer.exe" } else { "pathtracer" };
        let exe = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.join(name)));
        let Some(exe) = exe.filter(|p| p.is_file()) else {
            self.notify(format!("cannot find {name} next to ptedit"));
            return;
        };
        let scene = self.ws.doc.path().to_path_buf();
        match std::process::Command::new(&exe).arg(&scene).spawn() {
            Ok(_) => self.notify(if self.ws.doc.is_dirty() {
                "the tracer shows the scene as saved: save to update it (it reloads by itself)"
            } else {
                "the tracer opens the scene and reloads it when it is saved"
            }),
            Err(e) => self.notify(format!("error: cannot start the tracer: {e}")),
        }
    }

    pub fn open_dialog(&mut self, d: FileDialog) {
        self.dialog = Some(Dialog::Files(d));
    }

    /// The Add menu's items (menu bar or Shift+A).
    pub fn add_items(&mut self, f: &mut PopupFrame) {
        for (a, label) in Add::OBJECTS {
            if self.ui.menu_item(f, label, "", true) {
                self.chosen = Some(Cmd::Add(a));
            }
        }
        let protos: Vec<String> = self.ws.doc.tree.prototypes.iter().map(|p| p.label.clone()).collect();
        if let Some(mut sub) = self.ui.submenu(f, "Instance of", !protos.is_empty()) {
            for p in &protos {
                if self.ui.menu_item(&mut sub, p, "", true) {
                    self.chosen_instance = Some(p.clone());
                }
            }
            self.ui.end_popup(sub);
        }
        self.ui.menu_separator(f);
        for (a, label) in Add::LIGHTS {
            if self.ui.menu_item(f, label, "", true) {
                self.chosen = Some(Cmd::Add(a));
            }
        }
    }

    /// The Shift+A popup.
    fn add_popup(&mut self) {
        let id = id_of(VIEWPORT, "add-popup");
        if let Some(mut f) = self.ui.begin_popup(id) {
            self.add_items(&mut f);
            self.ui.end_popup(f);
        }
        if let Some(name) = self.chosen_instance.take() {
            self.ui.close_popups();
            self.add_instance(&name);
        }
        if let Some(c) = self.chosen.take() {
            self.ui.close_popups();
            self.run(c);
        }
    }

    // ------------------------------------------------------------------ files and dialogs

    fn request_quit(&mut self) {
        self.leave_edit();
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
        self.leave_edit();
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
                self.watch.track(&self.ws);
                self.tool_drag = None;
                self.decor_key = None;
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
            Dialog::Files(mut fd) => match fd.ui(&mut self.ui, &mut self.library) {
                Outcome::Open => self.dialog = Some(Dialog::Files(fd)),
                Outcome::Cancel => {
                    self.pick = None;
                    self.add_mesh_at = None;
                }
                Outcome::Chosen(path) => match fd.mode {
                    Mode::Open => self.request_open(path),
                    Mode::Pick if self.add_mesh_at.is_some() => {
                        let at = self.add_mesh_at.take().unwrap_or(Vec3::ZERO);
                        self.add_mesh(&path, at);
                    }
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
            Dialog::Changed(file) => {
                let name = self.ws.doc.files.get(file).map_or_else(String::new, |f| f.path.display().to_string());
                let area = self.ui.begin_modal("Changed on Disk", 600.0, 170.0);
                let rh = self.ui.row_h();
                let mut col = Column::new(area, self.ui.px(6.0));
                let r = col.row(rh);
                self.ui.label(r, &format!("{name} was changed by another program."), self.ui.theme.text);
                let r = col.row(rh);
                self.ui.label(r, "It also has changes here that are not saved.", self.ui.theme.text_dim);
                let bw = self.ui.px(150.0);
                let gap = self.ui.px(6.0);
                let buttons = Rect::new(area.x, area.bottom() - rh, area.w, rh);
                let (load_r, rest) = buttons.split_right(bw, gap);
                let (keep_r, _) = rest.split_right(bw, gap);
                let id = id_of(VIEWPORT, "changed");
                let mut open = true;
                if self.ui.button_primary(id_of(id, "load"), load_r, "Load Theirs") {
                    open = false;
                    self.reload_file(file);
                }
                if open && (self.ui.button(id_of(id, "keep"), keep_r, "Keep Mine") || self.ui.take_key(Key::Escape)) {
                    open = false;
                    self.notify("kept the changes made here; saving will overwrite the file");
                }
                self.ui.end_modal();
                if open {
                    self.dialog = Some(Dialog::Changed(file));
                }
            }
        }
    }

    /// Takes file `file` as it is on disk (an undoable edit).
    pub fn reload_file(&mut self, file: usize) {
        match self.ws.doc.reload_file(file) {
            Ok(true) => {
                self.scene_stale = true;
                let name = self.ws.doc.files[file].path.file_name().map(|n| n.to_string_lossy().into_owned());
                self.notify(format!("reloaded {} (changed on disk)", name.unwrap_or_default()));
            }
            Ok(false) => {}
            Err(e) => self.notify(format!("error: {e}")),
        }
    }

    /// Asks what to do about file `file`, changed on disk while it has unsaved changes.
    pub fn ask_changed(&mut self, file: usize) {
        if self.dialog.is_none() {
            self.dialog = Some(Dialog::Changed(file));
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
        // Turning the environment: shown at once over the views, loaded when let go.
        let env_rotate = matches!(path, [Seg::Key(a), Seg::Key(b)] if a == "background" && b == "rotate");
        match r {
            Ok(true) if env_rotate && merge.is_some() && !done => {
                self.ws.env_preview = Some(crate::workspace::env_rotation(Some(value)));
                self.vp_dirty = true;
            }
            Ok(true) => {
                self.scene_stale = true;
                self.status = format!("set {}", pt_core::toml::path_name(path));
            }
            Ok(false) if env_rotate && done => self.scene_stale = true,
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
