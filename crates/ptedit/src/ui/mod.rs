//! An immediate-mode UI toolkit: every frame the program calls widget functions in drawing
//! order; each one checks the input against its rectangle and records shapes to draw. The
//! toolkit remembers only what has to outlive a frame: which widget is under the mouse
//! (hot), pressed (active) and has the keyboard (focus), the text being edited, open popups,
//! scroll offsets and open sections.
//!
//! Widgets are identified by [`Id`]s, hashes of a parent id and a label or index. Popups,
//! modal dialogs and tooltips live on higher layers: they are painted over the base layer,
//! and only the topmost layer under the mouse takes mouse input.

pub mod color;
pub mod draw;
pub mod font;
mod font8x8;
pub mod text_edit;
pub mod theme;
pub mod ttf;
pub mod widgets;

pub use draw::Rect;
pub use widgets::NumOpts;

use draw::{Cmd, Shape};
use font::{Font, Target};
use pt_core::platform::{Cursor, Key, Modifiers};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::time::{Duration, Instant};
use text_edit::TextEdit;
use theme::Theme;

pub type Id = u64;

pub const LAYER_BASE: u8 = 0;
pub const LAYER_MODAL: u8 = 10;
pub const LAYER_TOOLTIP: u8 = 30;
pub const LAYER_DRAG: u8 = 31;

/// How long the mouse rests on a widget before its tooltip shows.
const TOOLTIP_DELAY: Duration = Duration::from_millis(550);
const CARET_BLINK: Duration = Duration::from_millis(530);

/// One frame's input.
#[derive(Clone, Debug, Default)]
pub struct Input {
    pub mouse: (i32, i32),
    /// Left, right, middle.
    pub down: [bool; 3],
    pub pressed: [bool; 3],
    pub released: [bool; 3],
    /// Clicks in a row of the left press this frame (2: a double click).
    pub clicks: u8,
    pub wheel: f32,
    pub text: String,
    /// Keys pressed (with repeats), in order.
    pub keys: Vec<(Key, Modifiers)>,
    pub mods: Modifiers,
    /// The clipboard's text, fetched when a paste shortcut was pressed.
    pub paste: Option<String>,
}

impl Input {
    /// Whether `key` was pressed without Ctrl or Alt.
    pub fn key(&self, key: Key) -> bool {
        self.keys.iter().any(|&(k, m)| k == key && !m.ctrl && !m.alt)
    }

    pub fn wants_paste(&self) -> bool {
        self.keys.iter().any(|&(k, m)| (k == Key::Char('v') && m.ctrl) || (k == Key::Insert && m.shift))
    }
}

/// What happened to a widget this frame.
#[derive(Clone, Copy, Debug, Default)]
#[expect(clippy::struct_excessive_bools, reason = "independent facts about one frame of input")]
pub struct Response {
    pub id: Id,
    pub hovered: bool,
    /// The left button went down on it.
    pub pressed: bool,
    /// Pressed and released on it (or Enter / Space while it has the focus).
    pub clicked: bool,
    pub double_clicked: bool,
    pub right_clicked: bool,
    /// Held down after a press on it.
    pub dragging: bool,
    /// Released after a press on it (wherever the mouse is).
    pub released: bool,
    pub focused: bool,
}

struct Popup {
    id: Id,
    /// Where it opened (it moves to stay on screen).
    at: (i32, i32),
    /// What opened it: the popup flips above it when it does not fit below.
    opener: Rect,
    layer: u8,
    size: (i32, i32),
}

/// A popup being filled: a column of rows.
pub struct PopupFrame {
    id: Id,
    pub x: i32,
    pub y: i32,
    pub w: i32,
    top: i32,
    bg_at: usize,
    outer_layer: u8,
    want_w: i32,
}

impl PopupFrame {
    /// The next row of `h` pixels.
    pub fn row(&mut self, h: i32) -> Rect {
        let r = Rect::new(self.x, self.y, self.w, h);
        self.y += h;
        r
    }

    /// Asks for the popup to be at least `w` wide (inside its padding).
    pub fn want(&mut self, w: i32) {
        self.want_w = self.want_w.max(w);
    }
}

/// A vertical run of rows, as panels lay out their contents.
#[derive(Clone, Copy, Debug)]
pub struct Column {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub gap: i32,
}

impl Column {
    pub fn new(r: Rect, gap: i32) -> Column {
        Column { x: r.x, y: r.y, w: r.w, gap }
    }

    pub fn row(&mut self, h: i32) -> Rect {
        let r = Rect::new(self.x, self.y, self.w, h);
        self.y += h + self.gap;
        r
    }

    pub fn space(&mut self, h: i32) {
        self.y += h;
    }

    pub fn indent(&mut self, d: i32) {
        self.x += d;
        self.w -= d;
    }
}

pub struct Scroll {
    id: Id,
    rect: Rect,
    top: i32,
}

/// A number being dragged (or a scrollbar thumb, a splitter): where it started.
#[derive(Clone, Copy, Debug)]
struct DragState {
    id: Id,
    mouse: (i32, i32),
    value: f64,
    moved: bool,
}

/// Something dragged for dropping elsewhere (an outliner row).
#[derive(Clone, Debug)]
pub struct Dnd {
    pub payload: u64,
    pub label: String,
    start: (i32, i32),
    /// Past the threshold: really dragging, not just a press.
    pub active: bool,
}

/// Text being edited in a field.
#[derive(Clone, Debug)]
pub struct EditState {
    pub id: Id,
    pub te: TextEdit,
    scroll: f32,
    blink: Instant,
}

/// The color picker's own state while it is open: hue and saturation survive values where
/// RGB loses them (black, grey).
#[derive(Clone, Copy, Debug)]
struct Picker {
    id: Id,
    hsv: [f64; 3],
    mode: u8,
    kelvin: f64,
}

#[expect(clippy::struct_excessive_bools, reason = "independent per-frame flags")]
pub struct Ui {
    pub theme: Theme,
    pub font: Font,
    /// Physical pixels per 96-dpi pixel.
    pub scale: f32,
    pub input: Input,
    pub size: (i32, i32),
    pub now: Instant,
    /// Under the mouse (this frame so far).
    hot: Id,
    /// Pressed and held.
    pub active: Id,
    /// Has the keyboard.
    pub focus: Id,
    focus_order: Vec<(Id, u8)>,
    prev_focus_order: Vec<(Id, u8)>,
    /// The widget under the mouse and since when (tooltips).
    hover: (Id, Instant),
    layer: u8,
    regions: Vec<(u8, Rect)>,
    prev_regions: Vec<(u8, Rect)>,
    /// The topmost layer under the mouse: only its widgets take the mouse.
    pub mouse_layer: u8,
    pub cmds: Vec<Cmd>,
    clips: Vec<Rect>,
    open: HashMap<Id, bool>,
    scroll: HashMap<Id, i32>,
    popups: Vec<Popup>,
    modal: bool,
    prev_modal: bool,
    pub edit: Option<EditState>,
    /// Text of fields that lost the focus to another field before they could commit it.
    pending: HashMap<Id, String>,
    edit_seen: bool,
    drag: Option<DragState>,
    pub dnd: Option<Dnd>,
    picker: Option<Picker>,
    pub cursor: Cursor,
    /// Text for the clipboard, set by copy and cut.
    pub copy: Option<String>,
    repaint: Option<Instant>,
    tooltip: Option<String>,
    press_taken: bool,
    /// This frame's press only closed popups (and must not reach what is under it).
    pub swallowed_press: bool,
}

/// Hashes `v` into an id under `parent`.
pub fn id_of(parent: Id, v: impl Hash) -> Id {
    let mut h = std::hash::DefaultHasher::new();
    parent.hash(&mut h);
    v.hash(&mut h);
    h.finish()
}

impl Ui {
    pub fn new(theme: Theme, font: Font, scale: f32) -> Ui {
        let now = Instant::now();
        Ui {
            theme,
            font,
            scale,
            input: Input::default(),
            size: (0, 0),
            now,
            hot: 0,
            active: 0,
            focus: 0,
            focus_order: Vec::new(),
            prev_focus_order: Vec::new(),
            hover: (0, now),
            layer: LAYER_BASE,
            regions: Vec::new(),
            prev_regions: Vec::new(),
            mouse_layer: LAYER_BASE,
            cmds: Vec::new(),
            clips: Vec::new(),
            open: HashMap::new(),
            scroll: HashMap::new(),
            popups: Vec::new(),
            modal: false,
            prev_modal: false,
            edit: None,
            pending: HashMap::new(),
            edit_seen: false,
            drag: None,
            dnd: None,
            picker: None,
            cursor: Cursor::Arrow,
            copy: None,
            repaint: None,
            tooltip: None,
            press_taken: false,
            swallowed_press: false,
        }
    }

    // ------------------------------------------------------------------ metrics

    /// `v` 96-dpi pixels in physical pixels.
    pub fn px(&self, v: f32) -> i32 {
        (v * self.scale).round() as i32
    }

    /// Height of a widget row.
    pub fn row_h(&self) -> i32 {
        self.font.line_height() + self.px(8.0)
    }

    pub fn pad(&self) -> i32 {
        self.px(6.0)
    }

    pub fn text_w(&self, s: &str) -> i32 {
        self.font.width(s).ceil() as i32
    }

    /// `s` broken into lines at most `w` pixels wide (at spaces; overlong words are cut).
    pub fn wrap(&self, s: &str, w: i32) -> Vec<String> {
        let mut lines = Vec::new();
        let mut line = String::new();
        for word in s.split(' ') {
            let candidate = if line.is_empty() { word.to_owned() } else { format!("{line} {word}") };
            if self.text_w(&candidate) <= w || line.is_empty() {
                line = candidate;
            } else {
                lines.push(std::mem::replace(&mut line, word.to_owned()));
            }
            while self.text_w(&line) > w && line.chars().count() > 1 {
                // A word longer than the line: cut it.
                let mut cut = String::new();
                for c in line.chars() {
                    if self.text_w(&format!("{cut}{c}")) > w && !cut.is_empty() {
                        break;
                    }
                    cut.push(c);
                }
                let rest = line[cut.len()..].to_owned();
                lines.push(cut);
                line = rest;
            }
        }
        if !line.is_empty() || lines.is_empty() {
            lines.push(line);
        }
        lines
    }

    // ------------------------------------------------------------------ frames

    /// Starts a frame for a `w`x`h` window.
    pub fn begin(&mut self, input: Input, w: i32, h: i32) {
        self.input = input;
        self.size = (w, h);
        self.now = Instant::now();
        self.hot = 0;
        self.cmds.clear();
        self.clips.clear();
        self.clips.push(Rect::new(0, 0, w, h));
        self.layer = LAYER_BASE;
        self.prev_regions = std::mem::take(&mut self.regions);
        self.prev_focus_order = std::mem::take(&mut self.focus_order);
        self.prev_modal = self.modal;
        self.modal = false;
        self.cursor = Cursor::Arrow;
        self.copy = None;
        self.repaint = None;
        self.tooltip = None;
        self.press_taken = false;
        self.swallowed_press = false;
        self.edit_seen = false;
        let (mx, my) = self.input.mouse;
        self.mouse_layer =
            self.prev_regions.iter().filter(|(_, r)| r.contains(mx, my)).map(|&(l, _)| l).max().unwrap_or(LAYER_BASE);
        if self.prev_modal {
            self.mouse_layer = self.mouse_layer.max(LAYER_MODAL);
        }
        // A press outside the open popups closes them, and does nothing else.
        if self.input.pressed.iter().any(|&p| p)
            && let Some(top) = self.popups.last()
            && self.mouse_layer < top.layer
        {
            let keep = self.mouse_layer;
            self.popups.retain(|p| p.layer <= keep);
            self.input.pressed = [false; 3];
            self.input.clicks = 0;
            self.swallowed_press = true;
        }
        if !self.popups.is_empty() && self.take_key(Key::Escape) {
            self.popups.pop();
        }
        // Tab and Shift+Tab move the focus along the focusable widgets of the top layer.
        let tab = self.input.keys.iter().position(|&(k, m)| k == Key::Tab && !m.ctrl && !m.alt);
        if let Some(i) = tab {
            let back = self.input.keys[i].1.shift;
            let floor = if self.prev_modal { LAYER_MODAL } else { LAYER_BASE };
            let order: Vec<Id> = self.prev_focus_order.iter().filter(|(_, l)| *l >= floor).map(|(id, _)| *id).collect();
            if !order.is_empty() {
                self.input.keys.remove(i);
                let n = order.len();
                let next = match order.iter().position(|&id| id == self.focus) {
                    Some(k) if back => order[(k + n - 1) % n],
                    Some(k) => order[(k + 1) % n],
                    None if back => order[n - 1],
                    None => order[0],
                };
                self.focus = next;
            }
        }
        let threshold = self.px(5.0);
        if let Some(d) = &mut self.dnd
            && !d.active
        {
            let (sx, sy) = d.start;
            if (mx - sx).abs() + (my - sy).abs() > threshold {
                d.active = true;
            }
        }
    }

    /// Ends the frame: tooltips and the dragged item are drawn, and state that only lives
    /// while its widget is shown is dropped.
    pub fn end(&mut self) {
        if self.input.pressed[0] && !self.press_taken {
            self.focus = 0;
        }
        if self.input.released[0] {
            self.active = 0;
            self.drag = None;
            self.dnd = None;
        }
        if !self.edit_seen {
            self.edit = None;
        }
        if self.hot != self.hover.0 {
            self.hover = (self.hot, self.now);
        }
        if let Some(text) = self.tooltip.take() {
            self.draw_tooltip(&text);
        }
        if let Some(d) = &self.dnd
            && d.active
        {
            let label = d.label.clone();
            let (mx, my) = self.input.mouse;
            let w = self.text_w(&label) + 2 * self.pad();
            let r = Rect::new(mx + self.px(14.0), my + self.px(4.0), w, self.row_h());
            self.with_layer(LAYER_DRAG, |ui| {
                ui.fill_rounded(r, ui.theme.accent, 200, ui.px(3.0));
                ui.text(r.x + ui.pad(), r.y + (r.h - ui.font.line_height()) / 2, &label, ui.theme.accent_text);
            });
        }
        if let Some(e) = &self.edit
            && self.focus == e.id
        {
            let since = self.now.duration_since(e.blink).as_millis() as u64;
            let wait = CARET_BLINK.as_millis() as u64 - since % CARET_BLINK.as_millis() as u64;
            self.repaint_after(Duration::from_millis(wait));
        }
    }

    /// Paints the frame's shapes over `px` (a `w`x`h` buffer).
    pub fn paint(&mut self, px: &mut [u32], w: usize, h: usize) {
        let mut cmds = std::mem::take(&mut self.cmds);
        draw::paint(&mut cmds, &mut self.font, &mut Target { px, w, h });
        self.cmds = cmds;
    }

    /// When the UI needs drawing again without new input (a tooltip appearing, the caret
    /// blinking, a popup that learned its size); None: not until something happens.
    pub fn repaint_at(&self) -> Option<Instant> {
        self.repaint
    }

    pub fn repaint_after(&mut self, d: Duration) {
        let t = self.now + d;
        self.repaint = Some(self.repaint.map_or(t, |r| r.min(t)));
    }

    pub fn repaint_now(&mut self) {
        self.repaint = Some(self.now);
    }

    /// Whether keys should go to the UI rather than to shortcuts: text is being edited or a
    /// modal dialog is open.
    pub fn wants_keyboard(&self) -> bool {
        self.edit.as_ref().is_some_and(|e| e.id == self.focus) || self.prev_modal || self.modal
    }

    pub fn modal_open(&self) -> bool {
        self.prev_modal || self.modal
    }

    /// Removes `key` (pressed without Ctrl or Alt) from the input; returns whether it was there.
    pub fn take_key(&mut self, key: Key) -> bool {
        match self.input.keys.iter().position(|&(k, m)| k == key && !m.ctrl && !m.alt) {
            Some(i) => {
                self.input.keys.remove(i);
                true
            }
            None => false,
        }
    }

    // ------------------------------------------------------------------ interaction

    fn clip(&self) -> Rect {
        *self.clips.last().expect("the screen clip is always there")
    }

    pub fn push_clip(&mut self, r: Rect) {
        let c = self.clip().intersect(&r);
        self.clips.push(c);
    }

    pub fn pop_clip(&mut self) {
        if self.clips.len() > 1 {
            self.clips.pop();
        }
    }

    fn with_layer<R>(&mut self, layer: u8, f: impl FnOnce(&mut Ui) -> R) -> R {
        let (old, clips) = (self.layer, std::mem::take(&mut self.clips));
        self.layer = layer;
        self.clips.push(Rect::new(0, 0, self.size.0, self.size.1));
        let r = f(self);
        self.layer = old;
        self.clips = clips;
        r
    }

    /// Whether the mouse is over `r` on this layer (not covered by a popup, not clipped away).
    pub fn mouse_in(&self, r: Rect) -> bool {
        let (mx, my) = self.input.mouse;
        self.layer == self.mouse_layer && r.intersect(&self.clip()).contains(mx, my)
    }

    /// The input of a widget at `r`.
    pub fn interact(&mut self, id: Id, r: Rect, focusable: bool) -> Response {
        let inside = self.mouse_in(r);
        let hovered = inside && (self.active == 0 || self.active == id) && self.dnd.as_ref().is_none_or(|d| !d.active);
        let mut resp = Response { id, hovered, ..Response::default() };
        if hovered {
            self.hot = id;
        }
        if hovered && self.input.pressed[0] {
            self.active = id;
            self.press_taken = true;
            self.focus = if focusable { id } else { 0 };
            resp.pressed = true;
            resp.double_clicked = self.input.clicks >= 2;
        }
        if inside && self.input.pressed[1] {
            resp.right_clicked = true;
            self.press_taken = true;
        }
        if self.active == id {
            resp.dragging = self.input.down[0];
            if self.input.released[0] {
                resp.released = true;
                resp.clicked = inside;
                self.active = 0;
            }
        }
        if focusable {
            self.focus_order.push((id, self.layer));
        }
        if self.focus == id {
            resp.focused = true;
            if !self.wants_keyboard() && (self.take_key(Key::Enter) || self.take_key(Key::Space)) {
                resp.clicked = true;
            }
        }
        resp
    }

    /// Holds the mouse for `id` (a viewport drag) so no widget reacts to it.
    pub fn capture(&mut self, id: Id) {
        self.active = id;
    }

    // ------------------------------------------------------------------ drawing

    fn push(&mut self, shape: Shape) {
        let clip = self.clip();
        self.cmds.push(Cmd { layer: self.layer, clip, shape });
    }

    pub fn fill(&mut self, r: Rect, color: u32) {
        self.push(Shape::Fill { r, color, alpha: 255, radius: 0 });
    }

    pub fn fill_rounded(&mut self, r: Rect, color: u32, alpha: u8, radius: i32) {
        self.push(Shape::Fill { r, color, alpha, radius });
    }

    pub fn border(&mut self, r: Rect, color: u32, radius: i32) {
        self.push(Shape::Border { r, color, radius });
    }

    /// Text with its line's top at `y`.
    pub fn text(&mut self, x: i32, y: i32, s: &str, color: u32) {
        self.push(Shape::Text { x: x as f32, y, text: s.to_owned(), color });
    }

    /// Text centred vertically in `r`, left-aligned after the padding, cut with "…" to fit.
    pub fn label(&mut self, r: Rect, s: &str, color: u32) {
        let pad = self.pad();
        let s = self.font.elide(s, (r.w - pad) as f32);
        let y = r.y + (r.h - self.font.line_height()) / 2;
        self.push_clip(r);
        self.text(r.x + pad / 2, y, &s, color);
        self.pop_clip();
    }

    /// Text centred in `r`.
    pub fn label_centered(&mut self, r: Rect, s: &str, color: u32) {
        let s = self.font.elide(s, (r.w - 2) as f32);
        let w = self.text_w(&s);
        let y = r.y + (r.h - self.font.line_height()) / 2;
        self.push_clip(r);
        self.text(r.x + (r.w - w) / 2, y, &s, color);
        self.pop_clip();
    }

    /// Text right-aligned in `r`.
    pub fn label_right(&mut self, r: Rect, s: &str, color: u32) {
        let w = self.text_w(s);
        let y = r.y + (r.h - self.font.line_height()) / 2;
        self.push_clip(r);
        self.text(r.right() - w - self.pad() / 2, y, s, color);
        self.pop_clip();
    }

    pub fn tri(&mut self, p: [(f32, f32); 3], color: u32) {
        self.push(Shape::Tri { p, color });
    }

    /// A thick line as two triangles.
    pub fn line(&mut self, a: (f32, f32), b: (f32, f32), width: f32, color: u32) {
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let len = dx.hypot(dy).max(1e-6);
        let (nx, ny) = (-dy / len * width * 0.5, dx / len * width * 0.5);
        let q = [(a.0 + nx, a.1 + ny), (b.0 + nx, b.1 + ny), (b.0 - nx, b.1 - ny), (a.0 - nx, a.1 - ny)];
        self.tri([q[0], q[1], q[2]], color);
        self.tri([q[0], q[2], q[3]], color);
    }

    pub fn image(&mut self, r: Rect, w: usize, h: usize, px: Vec<u32>) {
        self.push(Shape::Image { r, w, h, px });
    }

    /// A small triangle pointing right (closed) or down (open), centred in `r`.
    pub fn arrow(&mut self, r: Rect, open: bool, color: u32) {
        let (cx, cy) = (r.x as f32 + r.w as f32 / 2.0, r.y as f32 + r.h as f32 / 2.0);
        let s = 3.5 * self.scale;
        let p = if open {
            [(cx - s, cy - s * 0.6), (cx + s, cy - s * 0.6), (cx, cy + s * 0.8)]
        } else {
            [(cx - s * 0.6, cy - s), (cx + s * 0.8, cy), (cx - s * 0.6, cy + s)]
        };
        self.tri(p, color);
    }

    // ------------------------------------------------------------------ tooltips

    /// Shows `text` near the mouse once it has rested on `resp`'s widget for a moment.
    pub fn tooltip(&mut self, resp: &Response, text: &str) {
        if !resp.hovered || self.active != 0 || text.is_empty() || self.input.down.iter().any(|&d| d) {
            return;
        }
        if self.hover.0 == resp.id {
            let shown_at = self.hover.1 + TOOLTIP_DELAY;
            if self.now >= shown_at {
                self.tooltip = Some(text.to_owned());
            } else {
                self.repaint = Some(self.repaint.map_or(shown_at, |r| r.min(shown_at)));
            }
        } else {
            self.repaint_after(TOOLTIP_DELAY);
        }
    }

    fn draw_tooltip(&mut self, text: &str) {
        let lines: Vec<&str> = text.lines().collect();
        let pad = self.pad();
        let lh = self.font.line_height();
        let w = lines.iter().map(|l| self.text_w(l)).max().unwrap_or(0) + 2 * pad;
        let h = lh * lines.len() as i32 + pad;
        let (mx, my) = self.input.mouse;
        let x = (mx + self.px(12.0)).min(self.size.0 - w - 2).max(0);
        let y = if my + self.px(20.0) + h > self.size.1 { my - h - self.px(4.0) } else { my + self.px(20.0) };
        let r = Rect::new(x, y.max(0), w, h);
        self.with_layer(LAYER_TOOLTIP, |ui| {
            ui.fill_rounded(r, ui.theme.tooltip, 255, ui.px(3.0));
            ui.border(r, ui.theme.border, ui.px(3.0));
            for (i, l) in lines.iter().enumerate() {
                ui.text(r.x + pad, r.y + pad / 2 + i as i32 * lh, l, ui.theme.tooltip_text);
            }
        });
    }

    // ------------------------------------------------------------------ popups

    /// Opens popup `id` at (`x`, `y`), over whatever layer is current; `opener` is the widget
    /// that opened it. Popups opened from popups stack; others close.
    pub fn open_popup(&mut self, id: Id, x: i32, y: i32, opener: Rect) {
        let layer = self.layer + 1;
        self.popups.retain(|p| p.layer < layer);
        let size = (self.px(180.0), self.row_h());
        self.popups.push(Popup { id, at: (x, y), opener, layer, size });
        self.repaint_now();
    }

    pub fn is_popup_open(&self, id: Id) -> bool {
        self.popups.iter().any(|p| p.id == id)
    }

    pub fn close_popup(&mut self, id: Id) {
        if let Some(i) = self.popups.iter().position(|p| p.id == id) {
            self.popups.truncate(i);
        }
    }

    pub fn close_popups(&mut self) {
        self.popups.clear();
    }

    /// Starts drawing popup `id` if it is open.
    pub fn begin_popup(&mut self, id: Id) -> Option<PopupFrame> {
        let p = self.popups.iter().find(|p| p.id == id)?;
        let (w, h) = p.size;
        let (sw, sh) = self.size;
        let x = p.at.0.min(sw - w - 1).max(0);
        let y = if p.at.1 + h > sh && p.opener.y - h >= 0 { p.opener.y - h } else { p.at.1.min(sh - h).max(0) };
        let layer = p.layer;
        let outer_layer = self.layer;
        self.layer = layer;
        self.clips.push(Rect::new(0, 0, sw, sh));
        let bg_at = self.cmds.len();
        let pad = self.px(4.0);
        Some(PopupFrame { id, x: x + pad, y: y + pad, w: w - 2 * pad, top: y, bg_at, outer_layer, want_w: 0 })
    }

    /// Finishes a popup: its background goes under what was drawn, and its size is kept
    /// for the next frame.
    #[expect(clippy::needless_pass_by_value, reason = "ends the popup: the frame cannot be used after")]
    pub fn end_popup(&mut self, f: PopupFrame) {
        let pad = self.px(4.0);
        let r = Rect::new(f.x - pad, f.top, f.w + 2 * pad, f.y - f.top + pad);
        let radius = self.px(4.0);
        let clip = self.clip();
        let (popup, border) = (self.theme.popup, self.theme.border);
        let bg = [
            Cmd {
                layer: self.layer,
                clip,
                shape: Shape::Fill { r: Rect { x: r.x + 2, y: r.y + 3, ..r }, color: 0, alpha: 60, radius },
            },
            Cmd { layer: self.layer, clip, shape: Shape::Fill { r, color: popup, alpha: 255, radius } },
            Cmd { layer: self.layer, clip, shape: Shape::Border { r, color: border, radius } },
        ];
        self.cmds.splice(f.bg_at..f.bg_at, bg);
        self.regions.push((self.layer, r));
        let want = ((f.want_w.max(f.w) + 2 * pad).max(self.px(120.0)), r.h);
        if let Some(p) = self.popups.iter_mut().find(|p| p.id == f.id)
            && p.size != want
        {
            p.size = want;
            self.repaint = Some(self.now);
        }
        self.clips.pop();
        self.layer = f.outer_layer;
    }

    // ------------------------------------------------------------------ modal dialogs

    /// Draws a modal dialog frame of `w`x`h` (96-dpi pixels, at most the window) in the middle
    /// of the window, dimming everything behind it; returns its content area. Until the
    /// next `end_modal`, widgets are on the modal layer.
    pub fn begin_modal(&mut self, title: &str, w: f32, h: f32) -> Rect {
        self.modal = true;
        self.layer = LAYER_MODAL;
        let (sw, sh) = self.size;
        let screen = Rect::new(0, 0, sw, sh);
        self.clips.push(screen);
        self.regions.push((LAYER_MODAL, screen));
        self.fill_rounded(screen, 0, self.theme.modal_dim, 0);
        let (w, h) = (self.px(w).min(sw - 20), self.px(h).min(sh - 20));
        let r = Rect::new((sw - w) / 2, (sh - h) / 2, w, h);
        let radius = self.px(5.0);
        self.fill_rounded(Rect { x: r.x + 3, y: r.y + 5, ..r }, 0, 80, radius);
        self.fill_rounded(r, self.theme.panel, 255, radius);
        self.border(r, self.theme.border, radius);
        let (bar, rest) = r.split_top(self.row_h() + self.px(4.0));
        self.fill_rounded(bar.shrink(1), self.theme.bar, 255, radius);
        self.label(bar.shrink2(self.pad(), 0), title, self.theme.text);
        rest.shrink(self.pad() + self.px(2.0))
    }

    pub fn end_modal(&mut self) {
        self.clips.pop();
        self.layer = LAYER_BASE;
    }

    // ------------------------------------------------------------------ scrolling

    /// Starts a scrolled area: rows are laid out from the returned column and clipped to
    /// `r`; `end_scroll` takes where they ended.
    pub fn begin_scroll(&mut self, id: Id, r: Rect) -> (Scroll, Column) {
        let off = self.scroll.get(&id).copied().unwrap_or(0);
        let bar = self.px(10.0);
        self.push_clip(r);
        let top = r.y - off;
        (Scroll { id, rect: r, top }, Column { x: r.x, y: top, w: r.w - bar, gap: 0 })
    }

    #[expect(clippy::needless_pass_by_value, reason = "ends the area: it cannot be used after")]
    pub fn end_scroll(&mut self, s: Scroll, bottom: i32) {
        let r = s.rect;
        let content = bottom - s.top;
        let max = (content - r.h).max(0);
        let mut off = (r.y - s.top).clamp(0, max);
        if self.input.wheel != 0.0 && self.mouse_in(r) {
            off = (off - (self.input.wheel * 3.0 * self.row_h() as f32) as i32).clamp(0, max);
            self.input.wheel = 0.0;
        }
        if max > 0 {
            let bar = self.px(10.0);
            let track = Rect::new(r.right() - bar, r.y, bar, r.h);
            let th = (r.h * r.h / content.max(1)).max(self.px(24.0)).min(r.h);
            let ty = r.y + (i64::from(r.h - th) * i64::from(off) / i64::from(max)) as i32;
            let thumb = Rect::new(track.x + self.px(2.0), ty, bar - self.px(4.0), th);
            let tid = id_of(s.id, "thumb");
            let resp = self.interact(tid, track, false);
            if resp.pressed {
                // Clicking the track jumps there; then the thumb follows the mouse.
                let at = if thumb.contains(self.input.mouse.0, self.input.mouse.1) {
                    off
                } else {
                    let rel = (self.input.mouse.1 - r.y - th / 2) as f32 / (r.h - th).max(1) as f32;
                    (rel * max as f32) as i32
                };
                self.drag = Some(DragState { id: tid, mouse: self.input.mouse, value: f64::from(at), moved: false });
            }
            if resp.dragging
                && let Some(d) = self.drag.filter(|d| d.id == tid)
            {
                let dy = self.input.mouse.1 - d.mouse.1;
                off = (d.value as i32 + (dy as f32 * max as f32 / (r.h - th).max(1) as f32) as i32).clamp(0, max);
            }
            let color = if resp.hovered || resp.dragging { self.theme.text_disabled } else { self.theme.scrollbar };
            self.fill_rounded(thumb, color, 255, self.px(3.0));
        }
        if self.scroll.get(&s.id).copied().unwrap_or(0) != off {
            self.repaint_now();
        }
        self.scroll.insert(s.id, off);
        self.pop_clip();
    }

    /// Scrolls area `id` (of height `h`, content starting at `top` when unscrolled) so that
    /// the rows `y0..y1` (in unscrolled content coordinates) show.
    pub fn scroll_to(&mut self, id: Id, h: i32, y0: i32, y1: i32) {
        let off = self.scroll.get(&id).copied().unwrap_or(0);
        let new = if y0 < off {
            y0
        } else if y1 > off + h {
            y1 - h
        } else {
            off
        };
        if new != off {
            self.scroll.insert(id, new.max(0));
            self.repaint_now();
        }
    }

    // ------------------------------------------------------------------ drag and drop

    /// Makes `resp`'s widget a drag source of `payload`: once pressed and moved, it is dragged.
    pub fn drag_source(&mut self, resp: &Response, payload: u64, label: &str) {
        if resp.pressed {
            self.dnd = Some(Dnd { payload, label: label.to_owned(), start: self.input.mouse, active: false });
        }
    }

    /// The payload being dragged over `r` (to highlight a drop target).
    pub fn drag_over(&self, r: Rect) -> Option<u64> {
        let d = self.dnd.as_ref().filter(|d| d.active)?;
        let (mx, my) = self.input.mouse;
        (self.layer == self.mouse_layer && r.intersect(&self.clip()).contains(mx, my)).then_some(d.payload)
    }

    /// The payload dropped on `r` this frame.
    pub fn dropped(&mut self, r: Rect) -> Option<u64> {
        if !self.input.released[0] {
            return None;
        }
        self.drag_over(r)
    }

    // ------------------------------------------------------------------ open states

    /// Whether section `id` is open (`default` until toggled).
    pub fn is_open(&self, id: Id, default: bool) -> bool {
        self.open.get(&id).copied().unwrap_or(default)
    }

    pub fn set_open(&mut self, id: Id, open: bool) {
        self.open.insert(id, open);
    }
}

#[cfg(test)]
mod tests;
