//! The widgets: buttons, check boxes, drop-downs, text fields, numbers dragged or typed,
//! sliders, vectors, the color picker, collapsible sections, tree rows, menus and splitters.

use super::color::{self, from_srgb, hsv_to_rgb, rgb_to_hsv, to_srgb};
use super::text_edit::TextEdit;
use super::{DragState, EditState, Id, Picker, PopupFrame, Rect, Response, Ui, id_of};
use pt_core::platform::{Cursor, Key};

/// How a number is shown and changed.
#[derive(Clone, Copy, Debug)]
pub struct NumOpts {
    pub min: f64,
    pub max: f64,
    /// Change per pixel dragged; 0: chosen from the value.
    pub speed: f64,
    pub integer: bool,
}

impl Default for NumOpts {
    fn default() -> Self {
        NumOpts { min: f64::NEG_INFINITY, max: f64::INFINITY, speed: 0.0, integer: false }
    }
}

impl NumOpts {
    pub fn range(min: f64, max: f64) -> NumOpts {
        NumOpts { min, max, ..NumOpts::default() }
    }

    fn clamp(&self, v: f64) -> f64 {
        let v = if self.integer { v.round() } else { v };
        v.clamp(self.min, self.max)
    }

    /// Change per pixel for a drag starting at `v`.
    fn step(&self, v: f64) -> f64 {
        if self.speed > 0.0 {
            return self.speed;
        }
        if self.integer {
            return 0.1;
        }
        if self.min.is_finite() && self.max.is_finite() {
            return ((self.max - self.min) / 200.0).max(1e-4);
        }
        (v.abs() * 0.01).max(0.005)
    }
}

/// A number widget's result.
#[derive(Clone, Copy, Debug, Default)]
pub struct NumResp {
    /// The new value, when it changed this frame.
    pub value: Option<f64>,
    /// A drag or a typed entry ended: the change is complete (one undo step).
    pub done: bool,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ColorResp {
    pub value: Option<[f64; 3]>,
    pub done: bool,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct TreeResp {
    pub resp: Response,
    /// The open / closed arrow was clicked.
    pub toggled: bool,
}

/// A number as text: short, without float noise.
pub fn format_num(v: f64) -> String {
    let r = (v * 1e6).round() / 1e6;
    pt_core::toml::format_number(if r == 0.0 { 0.0 } else { r })
}

/// Whether an edit changed a number (exactly: any change is a change).
fn differs(a: f64, b: f64) -> bool {
    a.to_bits() != b.to_bits()
}

/// Rounds a dragged value to the precision the drag speed gives.
fn round_to_step(v: f64, step: f64) -> f64 {
    let k = step.log10().floor();
    if k < 0.0 {
        // Divide by an exact power of ten: 700 / 1000 is 0.7, 700 * 0.001 is not.
        let inv = 10f64.powf(-k);
        (v * inv).round() / inv
    } else {
        let q = 10f64.powf(k);
        (v / q).round() * q
    }
}

impl Ui {
    fn widget_bg(&self, resp: &Response, enabled: bool) -> u32 {
        let t = &self.theme;
        if !enabled {
            t.widget
        } else if resp.dragging || (resp.hovered && self.input.down[0]) {
            t.widget_active
        } else if resp.hovered {
            t.widget_hover
        } else {
            t.widget
        }
    }

    fn focus_ring(&mut self, resp: &Response, r: Rect) {
        if resp.focused {
            self.border(r, self.theme.focus, self.px(3.0));
        }
    }

    pub fn button(&mut self, id: Id, r: Rect, text: &str) -> bool {
        self.button_enabled(id, r, text, true)
    }

    pub fn button_enabled(&mut self, id: Id, r: Rect, text: &str, enabled: bool) -> bool {
        let resp = if enabled { self.interact(id, r, true) } else { Response::default() };
        let bg = self.widget_bg(&resp, enabled);
        self.fill_rounded(r, bg, 255, self.px(3.0));
        self.border(r, self.theme.border, self.px(3.0));
        self.focus_ring(&resp, r);
        let color = if enabled { self.theme.text } else { self.theme.text_disabled };
        self.label_centered(r, text, color);
        enabled && resp.clicked
    }

    /// A button that stands out (the default action of a dialog).
    pub fn button_primary(&mut self, id: Id, r: Rect, text: &str) -> bool {
        let resp = self.interact(id, r, true);
        let a = if resp.hovered { 230 } else { 255 };
        self.fill_rounded(r, self.theme.accent, a, self.px(3.0));
        self.focus_ring(&resp, r);
        self.label_centered(r, text, self.theme.accent_text);
        resp.clicked
    }

    /// A flat button for toolbars and rows: only its hover shows.
    pub fn flat_button(&mut self, id: Id, r: Rect, text: &str, color: u32) -> Response {
        let resp = self.interact(id, r, false);
        if resp.hovered {
            self.fill_rounded(r, self.theme.widget_hover, 255, self.px(3.0));
        }
        self.label_centered(r, text, color);
        resp
    }

    /// A check box with a label; returns the new state when clicked.
    pub fn checkbox(&mut self, id: Id, r: Rect, on: bool, label: &str) -> Option<bool> {
        let resp = self.interact(id, r, true);
        let s = (self.font.line_height() as f32 * 0.9) as i32;
        let b = Rect::new(r.x + self.pad() / 2, r.y + (r.h - s) / 2, s, s);
        let bg = if on { self.theme.accent } else { self.widget_bg(&resp, true) };
        self.fill_rounded(b, bg, 255, self.px(3.0));
        self.border(b, if on { self.theme.accent } else { self.theme.border }, self.px(3.0));
        if on {
            let (x, y, k) = (b.x as f32, b.y as f32, s as f32);
            let c = self.theme.accent_text;
            let w = 1.6 * self.scale;
            self.line((x + 0.22 * k, y + 0.52 * k), (x + 0.42 * k, y + 0.72 * k), w, c);
            self.line((x + 0.42 * k, y + 0.72 * k), (x + 0.78 * k, y + 0.3 * k), w, c);
        }
        self.focus_ring(&resp, b);
        if !label.is_empty() {
            let rest = Rect::new(b.right() + self.pad() / 2, r.y, r.right() - b.right() - self.pad() / 2, r.h);
            self.label(rest, label, self.theme.text);
        }
        resp.clicked.then_some(!on)
    }

    /// A drop-down: shows `current`; returns the index of the option picked.
    pub fn combo(&mut self, id: Id, r: Rect, current: &str, options: &[String], color: u32) -> Option<usize> {
        let resp = self.interact(id, r, true);
        let bg = self.widget_bg(&resp, true);
        self.fill_rounded(r, bg, 255, self.px(3.0));
        self.border(r, self.theme.border, self.px(3.0));
        self.focus_ring(&resp, r);
        let arrow_w = self.row_h() * 3 / 4;
        let (arrow, text) = r.split_right(arrow_w, 0);
        self.label(text.shrink2(self.px(2.0), 0), current, color);
        self.arrow(arrow, true, self.theme.text_dim);
        let pid = id_of(id, "popup");
        if resp.clicked {
            if self.is_popup_open(pid) {
                self.close_popup(pid);
            } else {
                self.open_popup(pid, r.x, r.bottom(), r);
            }
        }
        let mut picked = None;
        if let Some(mut f) = self.begin_popup(pid) {
            f.want(r.w - self.px(8.0));
            let rh = self.row_h();
            // Long lists scroll.
            let max_rows = ((self.size.1 / 2) / rh).max(4) as usize;
            let list = options.len().min(max_rows) as i32 * rh;
            let area = f.row(list);
            let sid = id_of(id, "scroll");
            let (scroll, mut col) = self.begin_scroll(sid, area);
            for (i, opt) in options.iter().enumerate() {
                let w = self.text_w(opt) + 2 * self.pad() + self.px(12.0);
                f.want(w);
                let row = col.row(rh);
                if row.bottom() < area.y || row.y > area.bottom() {
                    continue;
                }
                let item = self.interact(id_of(id, ("opt", i)), row, false);
                let on = opt == current;
                if item.hovered {
                    self.fill_rounded(row, self.theme.accent, 255, self.px(3.0));
                } else if on {
                    self.fill_rounded(row, self.theme.row_selected, 255, self.px(3.0));
                }
                let c = if item.hovered { self.theme.accent_text } else { self.theme.text };
                self.label(row.shrink2(self.px(4.0), 0), opt, c);
                if item.clicked {
                    picked = Some(i);
                }
            }
            self.end_scroll(scroll, col.y);
            self.end_popup(f);
            if picked.is_some() {
                self.close_popup(pid);
            }
        }
        picked
    }

    /// A row of tabs; returns the one clicked.
    pub fn tabs(&mut self, id: Id, r: Rect, labels: &[&str], current: usize) -> Option<usize> {
        let mut out = None;
        for (i, (cell, label)) in r.columns(labels.len(), self.px(2.0)).into_iter().zip(labels).enumerate() {
            let resp = self.interact(id_of(id, i), cell, false);
            let bg = if i == current { self.theme.accent } else { self.widget_bg(&resp, true) };
            self.fill_rounded(cell, bg, 255, self.px(3.0));
            let c = if i == current { self.theme.accent_text } else { self.theme.text };
            self.label_centered(cell, label, c);
            if resp.clicked {
                out = Some(i);
            }
        }
        out
    }

    // ------------------------------------------------------------------ text

    /// Starts editing field `id` holding `value`, unless it already is; text another field
    /// was editing is kept for it to commit.
    fn start_edit(&mut self, id: Id, value: &str) {
        if self.edit.as_ref().is_some_and(|e| e.id == id) {
            return;
        }
        if let Some(old) = self.edit.take() {
            self.pending.insert(old.id, old.te.text());
        }
        self.edit = Some(EditState { id, te: TextEdit::new(value), scroll: 0.0, blink: self.now });
    }

    /// The caret position under the mouse in field `inner` (text scrolled by `scroll`).
    fn char_at(&self, inner: Rect, text: &str, scroll: f32) -> usize {
        let x = (self.input.mouse.0 - inner.x) as f32 + scroll;
        let offs = self.font.offsets(text);
        let mut best = 0;
        for (i, &o) in offs.iter().enumerate() {
            if (o - x).abs() < (offs[best] - x).abs() {
                best = i;
            }
        }
        best
    }

    /// Applies this frame's keys and typed text to the edit; returns Some(true) on Enter
    /// (commit) and Some(false) on Escape (cancel).
    fn edit_keys(&mut self) -> Option<bool> {
        let mut e = self.edit.take()?;
        let mut done = None;
        let keys = std::mem::take(&mut self.input.keys);
        let mut rest = Vec::new();
        let mut changed = false;
        for (k, m) in keys {
            let (word, sel) = (m.ctrl, m.shift);
            match k {
                Key::Left => e.te.left(word, sel),
                Key::Right => e.te.right(word, sel),
                Key::Home => e.te.home(sel),
                Key::End => e.te.end(sel),
                Key::Backspace => e.te.backspace(word),
                Key::Delete if m.shift => self.copy = Some(e.te.cut()),
                Key::Delete => e.te.delete(word),
                Key::Insert if m.ctrl => self.copy = Some(e.te.selected_text()),
                Key::Insert if m.shift => {
                    if let Some(p) = &self.input.paste {
                        e.te.insert(p);
                    }
                }
                Key::Enter => done = Some(true),
                Key::Escape => done = Some(false),
                Key::Char('a') if m.ctrl => e.te.select_all(),
                Key::Char('c') if m.ctrl => {
                    if e.te.has_selection() {
                        self.copy = Some(e.te.selected_text());
                    }
                }
                Key::Char('x') if m.ctrl => {
                    if e.te.has_selection() {
                        self.copy = Some(e.te.cut());
                    }
                }
                Key::Char('v') if m.ctrl => {
                    if let Some(p) = &self.input.paste {
                        e.te.insert(p);
                    }
                }
                // Typing keys arrive as text; their key presses are the field's too.
                Key::Char(_) | Key::Space | Key::Plus | Key::Minus | Key::Other(_) if !m.ctrl && !m.alt => {}
                _ => {
                    rest.push((k, m));
                    continue;
                }
            }
            changed = true;
        }
        self.input.keys = rest;
        if !self.input.text.is_empty() {
            let t = std::mem::take(&mut self.input.text);
            e.te.insert(&t);
            changed = true;
        }
        if changed {
            e.blink = self.now;
        }
        self.edit = Some(e);
        done
    }

    /// Draws field `inner` with the edit's text, selection and caret, scrolled to the caret.
    fn draw_edit(&mut self, inner: Rect) {
        let Some(mut e) = self.edit.take() else { return };
        let text = e.te.text();
        let offs = self.font.offsets(&text);
        let caret = offs[e.te.cursor];
        let room = (inner.w - self.px(2.0)) as f32;
        if caret - e.scroll > room {
            e.scroll = caret - room;
        }
        if caret < e.scroll {
            e.scroll = caret;
        }
        e.scroll = e.scroll.min((offs[offs.len() - 1] - room).max(0.0)).max(0.0);
        let y = inner.y + (inner.h - self.font.line_height()) / 2;
        let x0 = inner.x as f32 - e.scroll;
        self.push_clip(inner);
        let (a, b) = e.te.selection();
        if a != b {
            let r = Rect::new((x0 + offs[a]) as i32, y, (offs[b] - offs[a]).ceil() as i32, self.font.line_height());
            self.fill(r, self.theme.text_selection);
        }
        self.push(super::Shape::Text { x: x0, y, text, color: self.theme.text });
        let blink = self.now.duration_since(e.blink).as_millis() / super::CARET_BLINK.as_millis();
        if blink.is_multiple_of(2) {
            let cx = (x0 + caret).round() as i32;
            self.fill(Rect::new(cx, y, self.px(1.0).max(1), self.font.line_height()), self.theme.text);
        }
        self.pop_clip();
        self.edit = Some(e);
    }

    /// A single-line text field showing `value`; returns the new text when an edit is
    /// committed (Enter, or the focus moving elsewhere) and differs. Escape cancels.
    pub fn text_field(&mut self, id: Id, r: Rect, value: &str, hint: &str) -> Option<String> {
        let resp = self.interact(id, r, true);
        if resp.hovered {
            self.cursor = Cursor::Text;
        }
        let inner = r.shrink2(self.pad() / 2 + self.px(1.0), 0);
        let mut out = self.pending.remove(&id).filter(|t| t != value);
        if self.focus == id {
            if self.edit.as_ref().is_none_or(|e| e.id != id) {
                self.start_edit(id, value);
                if resp.pressed {
                    let i = self.char_at(inner, value, 0.0);
                    if let Some(e) = &mut self.edit {
                        e.te.set_cursor(i, false);
                    }
                }
            } else if resp.pressed || resp.dragging {
                let (text, scroll) = self.edit.as_ref().map(|e| (e.te.text(), e.scroll)).unwrap_or_default();
                let i = self.char_at(inner, &text, scroll);
                let extend = resp.dragging && !resp.pressed || (resp.pressed && self.input.mods.shift);
                if let Some(e) = &mut self.edit {
                    if resp.double_clicked {
                        e.te.select_word(i);
                    } else if !(resp.dragging && !resp.pressed && i == e.te.cursor) {
                        e.te.set_cursor(i, extend);
                    }
                    e.blink = self.now;
                }
            }
            self.edit_seen = true;
            match self.edit_keys() {
                Some(true) => {
                    let text = self.edit.take().map(|e| e.te.text()).unwrap_or_default();
                    self.focus = 0;
                    out = (text != value).then_some(text);
                }
                Some(false) => {
                    self.edit = None;
                    self.focus = 0;
                }
                None => {}
            }
        } else if self.edit.as_ref().is_some_and(|e| e.id == id) {
            // The focus went elsewhere: keep what was typed.
            let text = self.edit.take().map(|e| e.te.text()).unwrap_or_default();
            out = (text != value).then_some(text);
        }
        let editing = self.edit.as_ref().is_some_and(|e| e.id == id);
        self.fill_rounded(r, self.theme.field, 255, self.px(3.0));
        let border = if editing { self.theme.focus } else { self.theme.border };
        self.border(r, border, self.px(3.0));
        if editing {
            self.draw_edit(inner);
        } else {
            let (s, c) = if value.is_empty() { (hint, self.theme.text_disabled) } else { (value, self.theme.text) };
            self.push_clip(inner);
            self.text(inner.x, inner.y + (inner.h - self.font.line_height()) / 2, s, c);
            self.pop_clip();
        }
        out
    }

    // ------------------------------------------------------------------ numbers

    /// A number: drag left or right to change it (Shift: finer, Ctrl: coarser), click or
    /// Tab to type it. `color` is the text color (dimmed for defaults).
    pub fn drag_number(&mut self, id: Id, r: Rect, value: f64, o: &NumOpts, color: u32) -> NumResp {
        let mut out = NumResp::default();
        let editing = self.edit.as_ref().is_some_and(|e| e.id == id) || self.pending.contains_key(&id);
        if editing || (self.focus == id && self.active != id) {
            let shown = format_num(value);
            if let Some(t) = self.text_field(id, r, &shown, "") {
                match eval(&t) {
                    Some(v) => {
                        let v = o.clamp(v);
                        out.value = differs(v, value).then_some(v);
                    }
                    None => self.repaint_now(),
                }
                out.done = true;
            }
            return out;
        }
        let resp = self.interact(id, r, true);
        if resp.hovered || resp.dragging {
            self.cursor = Cursor::ResizeH;
        }
        if resp.pressed {
            self.drag = Some(DragState { id, mouse: self.input.mouse, value, moved: false });
        }
        if let Some(mut d) = self.drag.filter(|d| d.id == id) {
            let dx = self.input.mouse.0 - d.mouse.0;
            if dx.abs() > self.px(3.0) {
                d.moved = true;
            }
            if resp.dragging && d.moved {
                let k = if self.input.mods.shift {
                    0.1
                } else if self.input.mods.ctrl {
                    10.0
                } else {
                    1.0
                };
                let step = o.step(d.value) * k;
                let v = o.clamp(round_to_step(d.value + f64::from(dx) * step, step));
                if differs(v, value) {
                    out.value = Some(v);
                }
            }
            if resp.released {
                if d.moved {
                    out.done = true;
                    self.focus = 0;
                } else {
                    // A click: type the value.
                    self.focus = id;
                    self.start_edit(id, &format_num(value));
                    self.edit_seen = true;
                }
                self.drag = None;
            } else {
                self.drag = Some(d);
            }
        }
        let bg = self.widget_bg(&resp, true);
        self.fill_rounded(r, bg, 255, self.px(3.0));
        self.border(r, self.theme.border, self.px(3.0));
        self.focus_ring(&resp, r);
        let shown = out.value.unwrap_or(value);
        self.label_centered(r, &format_num(shown), color);
        out
    }

    /// A slider over [`o.min`, `o.max`]; click to jump, drag to slide, double-click to type.
    pub fn slider(&mut self, id: Id, r: Rect, value: f64, o: &NumOpts, color: u32) -> NumResp {
        let mut out = NumResp::default();
        let editing = self.edit.as_ref().is_some_and(|e| e.id == id) || self.pending.contains_key(&id);
        if editing {
            return self.drag_number(id, r, value, o, color);
        }
        let resp = self.interact(id, r, true);
        if resp.double_clicked || (resp.focused && self.input.key(Key::Enter)) {
            self.focus = id;
            self.start_edit(id, &format_num(value));
            self.edit_seen = true;
            self.active = 0;
            return out;
        }
        let span = (o.max - o.min).max(1e-12);
        if resp.dragging {
            let t = f64::from(self.input.mouse.0 - r.x) / f64::from(r.w.max(1));
            let step = span / 1000.0;
            let v = o.clamp(round_to_step(o.min + t.clamp(0.0, 1.0) * span, step));
            if differs(v, value) {
                out.value = Some(v);
            }
        }
        if resp.released {
            out.done = true;
        }
        let bg = self.widget_bg(&resp, true);
        self.fill_rounded(r, bg, 255, self.px(3.0));
        let shown = out.value.unwrap_or(value);
        let t = ((shown - o.min) / span).clamp(0.0, 1.0);
        let fill = Rect { w: (f64::from(r.w) * t) as i32, ..r };
        self.fill_rounded(fill, self.theme.accent, 150, self.px(3.0));
        self.border(r, self.theme.border, self.px(3.0));
        self.focus_ring(&resp, r);
        self.label_centered(r, &format_num(shown), color);
        out
    }

    /// Two to four numbers side by side with axis-colored marks; returns the changed vector.
    pub fn vector(&mut self, id: Id, r: Rect, v: &[f64], o: &NumOpts, color: u32) -> (Option<Vec<f64>>, bool) {
        let mut out = None;
        let mut done = false;
        let cells = r.columns(v.len(), self.px(3.0));
        for (i, cell) in cells.into_iter().enumerate() {
            let resp = self.drag_number(id_of(id, i), cell, v[i], o, color);
            let mark = Rect::new(cell.x + self.px(1.0), cell.y + self.px(3.0), self.px(2.0), cell.h - self.px(6.0));
            self.fill(mark, self.theme.axes[i.min(3)]);
            if let Some(x) = resp.value {
                let mut nv = v.to_vec();
                nv[i] = x;
                out = Some(nv);
            }
            done |= resp.done;
        }
        (out, done)
    }

    // ------------------------------------------------------------------ color

    /// A color swatch; clicking it opens a picker (sRGB HSV square and hue strip, RGB / HSV /
    /// Kelvin entry). `rgb` is linear, as scenes store it.
    pub fn color_edit(&mut self, id: Id, r: Rect, rgb: [f64; 3], dim: bool) -> ColorResp {
        let mut out = ColorResp::default();
        let resp = self.interact(id, r, true);
        self.fill_rounded(r, color::pixel(rgb), 255, self.px(3.0));
        // Values above 1 (emission) show a mark: the swatch is clamped.
        if rgb.iter().any(|&c| c > 1.0) {
            let s = self.px(6.0) as f32;
            let (x, y) = (r.right() as f32, r.y as f32);
            self.tri([(x - s, y), (x, y), (x, y + s)], self.theme.warning);
        }
        self.border(r, if dim { self.theme.text_disabled } else { self.theme.border }, self.px(3.0));
        self.focus_ring(&resp, r);
        let pid = id_of(id, "picker");
        if resp.clicked {
            let enc = rgb.map(|c| to_srgb(c).clamp(0.0, 1.0));
            self.picker = Some(Picker { id, hsv: rgb_to_hsv(enc), mode: 0, kelvin: 6500.0 });
            self.open_popup(pid, r.x, r.bottom() + self.px(2.0), r);
        }
        let Some(mut f) = self.begin_popup(pid) else { return out };
        let mut p = self.picker.filter(|p| p.id == id).unwrap_or(Picker {
            id,
            hsv: rgb_to_hsv(rgb.map(|c| to_srgb(c).clamp(0.0, 1.0))),
            mode: 0,
            kelvin: 6500.0,
        });
        // The brightness beyond 1 is kept aside and reapplied, so the picker edits the hue
        // of an emission color without losing its strength.
        let gain = rgb.iter().copied().fold(1.0f64, f64::max);
        let w = self.px(220.0);
        f.want(w);
        let gap = self.px(4.0);
        let sq = f.row(self.px(150.0));
        let (hue_r, sv_r) = sq.split_right(self.px(18.0), gap);
        // Saturation / value square for the current hue.
        let n = 48usize;
        let px: Vec<u32> = (0..n * n)
            .map(|i| {
                let (x, y) = ((i % n) as f64 / (n - 1) as f64, 1.0 - (i / n) as f64 / (n - 1) as f64);
                let c = hsv_to_rgb([p.hsv[0], x, y]);
                let b = |v: f64| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u32;
                (b(c[0]) << 16) | (b(c[1]) << 8) | b(c[2])
            })
            .collect();
        self.image(sv_r, n, n, px);
        let hue: Vec<u32> = (0..64)
            .map(|i| {
                let c = hsv_to_rgb([f64::from(i) / 63.0, 1.0, 1.0]);
                let b = |v: f64| (v * 255.0 + 0.5) as u32;
                (b(c[0]) << 16) | (b(c[1]) << 8) | b(c[2])
            })
            .collect();
        self.image(hue_r, 1, 64, hue);
        let mut new_hsv = None;
        let sv = self.interact(id_of(id, "sv"), sv_r, false);
        if sv.dragging || sv.pressed {
            let x = f64::from(self.input.mouse.0 - sv_r.x) / f64::from(sv_r.w.max(1));
            let y = 1.0 - f64::from(self.input.mouse.1 - sv_r.y) / f64::from(sv_r.h.max(1));
            new_hsv = Some([p.hsv[0], x.clamp(0.0, 1.0), y.clamp(0.0, 1.0)]);
        }
        out.done |= sv.released;
        let hr = self.interact(id_of(id, "hue"), hue_r, false);
        if hr.dragging || hr.pressed {
            let t = f64::from(self.input.mouse.1 - hue_r.y) / f64::from(hue_r.h.max(1));
            new_hsv = Some([t.clamp(0.0, 0.9999), p.hsv[1], p.hsv[2]]);
        }
        out.done |= hr.released;
        // Markers.
        let mx = sv_r.x + (p.hsv[1] * f64::from(sv_r.w - 1)) as i32;
        let my = sv_r.y + ((1.0 - p.hsv[2]) * f64::from(sv_r.h - 1)) as i32;
        let m = self.px(5.0);
        self.border(Rect::new(mx - m, my - m, 2 * m + 1, 2 * m + 1), 0xFF_FFFF, m);
        self.border(Rect::new(mx - m - 1, my - m - 1, 2 * m + 3, 2 * m + 3), 0, m + 1);
        let hy = hue_r.y + (p.hsv[0] * f64::from(hue_r.h - 1)) as i32;
        self.border(Rect::new(hue_r.x - 1, hy - 2, hue_r.w + 2, 5), 0xFF_FFFF, 1);
        f.row(gap);
        let rh = self.row_h();
        if let Some(mode) = self.tabs(id_of(id, "mode"), f.row(rh), &["RGB", "HSV", "Kelvin"], p.mode as usize) {
            p.mode = mode as u8;
        }
        f.row(gap);
        let fields = f.row(rh);
        let mut new_rgb = None;
        match p.mode {
            0 => {
                let (v, d) =
                    self.vector(id_of(id, "rgb"), fields, &rgb, &NumOpts::range(0.0, f64::INFINITY), self.theme.text);
                if let Some(v) = v {
                    new_rgb = Some([v[0], v[1], v[2]]);
                }
                out.done |= d;
            }
            1 => {
                let shown = [p.hsv[0] * 360.0, p.hsv[1], p.hsv[2]];
                let cells = fields.columns(3, self.px(3.0));
                let opts = [NumOpts::range(0.0, 360.0), NumOpts::range(0.0, 1.0), NumOpts::range(0.0, 1.0)];
                for i in 0..3 {
                    let resp = self.drag_number(id_of(id, ("hsv", i)), cells[i], shown[i], &opts[i], self.theme.text);
                    if let Some(x) = resp.value {
                        let mut h = p.hsv;
                        h[i] = if i == 0 { x / 360.0 } else { x };
                        new_hsv = Some(h);
                    }
                    out.done |= resp.done;
                }
            }
            _ => {
                let resp = self.slider(
                    id_of(id, "kelvin"),
                    fields,
                    p.kelvin,
                    &NumOpts::range(1000.0, 12000.0),
                    self.theme.text,
                );
                if let Some(k) = resp.value {
                    p.kelvin = k.round();
                    new_rgb = Some(color::kelvin(p.kelvin).map(|c| c * gain));
                }
                out.done |= resp.done;
            }
        }
        if let Some(h) = new_hsv {
            p.hsv = h;
            let enc = hsv_to_rgb(h);
            new_rgb = Some(enc.map(|c| from_srgb(c) * gain));
        } else if let Some(c) = new_rgb
            && p.mode != 1
        {
            let g = c.iter().copied().fold(1.0f64, f64::max);
            let hsv = rgb_to_hsv(c.map(|x| to_srgb(x / g).clamp(0.0, 1.0)));
            // Keep the hue of greys.
            p.hsv = if hsv[1] > 0.0 { hsv } else { [p.hsv[0], hsv[1], hsv[2]] };
        }
        if let Some(c) = new_rgb {
            let c = c.map(|x| (x * 1e5).round() / 1e5);
            if c.iter().zip(&rgb).any(|(a, b)| differs(*a, *b)) {
                out.value = Some(c);
            }
        }
        self.picker = Some(p);
        self.end_popup(f);
        out
    }

    // ------------------------------------------------------------------ sections and trees

    /// A collapsible section header; returns whether the section is open.
    pub fn collapsing(&mut self, id: Id, r: Rect, label: &str, default_open: bool) -> bool {
        let open = self.is_open(id, default_open);
        let resp = self.interact(id, r, true);
        if resp.clicked {
            self.set_open(id, !open);
        }
        let open = self.is_open(id, default_open);
        let bg = if resp.hovered { self.theme.widget_hover } else { self.theme.bar };
        self.fill(r, bg);
        self.focus_ring(&resp, r);
        let a = self.row_h() * 3 / 4;
        let (arrow, text) = r.split_left(a, 0);
        self.arrow(arrow, open, self.theme.text_dim);
        self.label(text, label, self.theme.text);
        open
    }

    /// A tree row at `depth`: an open / closed arrow when it has children (`open`), the
    /// label, and a dim suffix. Selected rows are highlighted.
    #[expect(clippy::too_many_arguments, reason = "a row's state and texts")]
    pub fn tree_row(
        &mut self,
        id: Id,
        r: Rect,
        depth: i32,
        open: Option<bool>,
        selected: bool,
        label: &str,
        suffix: &str,
        color: u32,
    ) -> TreeResp {
        let resp = self.interact(id, r, true);
        let indent = self.px(14.0) * depth;
        let a = self.px(16.0);
        let arrow = Rect::new(r.x + indent, r.y, a, r.h);
        let mut toggled = false;
        if let Some(o) = open {
            let ar = self.interact(id_of(id, "arrow"), arrow, false);
            toggled =
                ar.clicked || (resp.focused && ((o && self.take_key(Key::Left)) || (!o && self.take_key(Key::Right))));
            self.arrow(arrow, o, self.theme.text_dim);
        }
        let bg = if selected {
            Some(self.theme.row_selected)
        } else if resp.hovered {
            Some(self.theme.row_hover)
        } else {
            None
        };
        if let Some(bg) = bg {
            // Under the arrow and text: insert before them.
            let clip = self.clip();
            let at = self.cmds.len().saturating_sub(usize::from(open.is_some()));
            self.cmds.insert(
                at,
                super::Cmd {
                    layer: self.layer,
                    clip,
                    shape: super::Shape::Fill { r, color: bg, alpha: 255, radius: 0 },
                },
            );
        }
        if resp.focused {
            self.border(r, self.theme.focus, 0);
        }
        let text = Rect::new(arrow.right(), r.y, r.right() - arrow.right(), r.h);
        let sw = if suffix.is_empty() { 0 } else { self.text_w(suffix).min(text.w / 2) + self.pad() };
        let (suffix_r, main) = text.split_right(sw, 0);
        self.label(main, label, color);
        if !suffix.is_empty() {
            self.label_right(suffix_r, suffix, self.theme.text_disabled);
        }
        TreeResp { resp, toggled }
    }

    // ------------------------------------------------------------------ menus

    /// A menu title in a menu bar at `r`: opens its menu on click, or on hover while another
    /// menu of the bar is open. Returns the menu's popup to fill when it is open.
    pub fn menu(&mut self, id: Id, r: Rect, title: &str, bar: &[Id]) -> Option<PopupFrame> {
        let resp = self.interact(id, r, false);
        let pid = id_of(id, "menu");
        let open = self.is_popup_open(pid);
        let other_open = bar.iter().any(|&m| m != id && self.is_popup_open(id_of(m, "menu")));
        if (resp.pressed && !open) || (resp.hovered && other_open) {
            self.close_popups();
            self.open_popup(pid, r.x, r.bottom(), r);
        } else if resp.pressed && open {
            self.close_popup(pid);
        }
        let open = self.is_popup_open(pid);
        if open || resp.hovered {
            let c = if open { self.theme.widget_active } else { self.theme.widget_hover };
            self.fill_rounded(r, c, 255, self.px(3.0));
        }
        self.label_centered(r, title, self.theme.text);
        self.begin_popup(pid)
    }

    /// A menu entry with a shortcut hint; returns true (and closes the menus) when chosen.
    pub fn menu_item(&mut self, f: &mut PopupFrame, label: &str, shortcut: &str, enabled: bool) -> bool {
        let rh = self.row_h();
        let pad = self.pad();
        f.want(self.text_w(label) + self.text_w(shortcut) + 5 * pad + self.px(20.0));
        let r = f.row(rh);
        let id = id_of(f.id, label);
        let resp = if enabled { self.interact(id, r, false) } else { Response::default() };
        if resp.hovered {
            self.fill_rounded(r, self.theme.accent, 255, self.px(3.0));
        }
        let c = match (enabled, resp.hovered) {
            (false, _) => self.theme.text_disabled,
            (true, true) => self.theme.accent_text,
            (true, false) => self.theme.text,
        };
        let (keys, text) = r.split_right(self.text_w(shortcut) + 2 * pad, 0);
        self.label(text.shrink2(self.px(14.0), 0), label, c);
        if !shortcut.is_empty() {
            let sc = if resp.hovered { self.theme.accent_text } else { self.theme.text_disabled };
            self.label_right(keys, shortcut, sc);
        }
        if resp.clicked {
            self.close_popups();
        }
        resp.clicked
    }

    /// A menu entry with a check mark.
    pub fn menu_check(&mut self, f: &mut PopupFrame, label: &str, shortcut: &str, on: bool) -> bool {
        let y = f.y;
        let clicked = self.menu_item(f, label, shortcut, true);
        if on {
            let s = self.px(14.0) as f32;
            let (x, y) = (f.x as f32 + 2.0, y as f32 + (self.row_h() as f32 - s) / 2.0);
            let w = 1.5 * self.scale;
            self.line((x + 0.2 * s, y + 0.5 * s), (x + 0.42 * s, y + 0.72 * s), w, self.theme.text);
            self.line((x + 0.42 * s, y + 0.72 * s), (x + 0.8 * s, y + 0.28 * s), w, self.theme.text);
        }
        clicked
    }

    pub fn menu_separator(&mut self, f: &mut PopupFrame) {
        let r = f.row(self.px(7.0));
        self.fill(
            Rect::new(r.x + self.px(4.0), r.y + r.h / 2, r.w - self.px(8.0), self.px(1.0).max(1)),
            self.theme.separator,
        );
    }

    /// An entry that opens a submenu to its right while hovered.
    pub fn submenu(&mut self, f: &mut PopupFrame, label: &str, enabled: bool) -> Option<PopupFrame> {
        let rh = self.row_h();
        f.want(self.text_w(label) + self.px(48.0));
        let r = f.row(rh);
        let id = id_of(f.id, label);
        let pid = id_of(id, "sub");
        let resp = if enabled { self.interact(id, r, false) } else { Response::default() };
        if resp.hovered && !self.is_popup_open(pid) {
            self.open_popup(pid, r.right() + self.px(4.0), r.y - self.px(4.0), r);
        }
        let open = self.is_popup_open(pid);
        if resp.hovered || open {
            self.fill_rounded(r, self.theme.accent, 255, self.px(3.0));
        }
        let c = if !enabled {
            self.theme.text_disabled
        } else if resp.hovered || open {
            self.theme.accent_text
        } else {
            self.theme.text
        };
        self.label(r.shrink2(self.px(14.0), 0), label, c);
        let (arrow, _) = r.split_right(rh * 3 / 4, 0);
        self.arrow(arrow, false, c);
        self.begin_popup(pid)
    }

    /// Opens popup `id` at the mouse when `resp`'s widget is right-clicked; returns it to fill.
    pub fn context_menu(&mut self, id: Id, resp: &Response) -> Option<PopupFrame> {
        if resp.right_clicked {
            let (x, y) = self.input.mouse;
            self.open_popup(id, x, y, Rect::new(x, y, 1, 1));
        }
        self.begin_popup(id)
    }

    // ------------------------------------------------------------------ splitters

    /// A draggable separator; returns how far it moved this frame.
    pub fn splitter(&mut self, id: Id, r: Rect, vertical: bool) -> i32 {
        let resp = self.interact(id, r, false);
        if resp.hovered || resp.dragging {
            self.cursor = if vertical { Cursor::ResizeH } else { Cursor::ResizeV };
        }
        if resp.pressed {
            self.drag = Some(DragState { id, mouse: self.input.mouse, value: 0.0, moved: false });
        }
        let mut delta = 0;
        if resp.dragging
            && let Some(d) = &mut self.drag
            && d.id == id
        {
            let (mx, my) = self.input.mouse;
            delta = if vertical { mx - d.mouse.0 } else { my - d.mouse.1 };
            d.mouse = (mx, my);
        }
        let line = if vertical { Rect::new(r.x + r.w / 2, r.y, 1, r.h) } else { Rect::new(r.x, r.y + r.h / 2, r.w, 1) };
        let c = if resp.hovered || resp.dragging { self.theme.accent } else { self.theme.separator };
        self.fill(line, c);
        delta
    }
}

/// Reads a typed number: a plain number, or a simple expression of + - * / and parentheses
/// ("2*3.5", "1/3").
pub fn eval(s: &str) -> Option<f64> {
    struct P<'a> {
        b: &'a [u8],
        i: usize,
    }
    impl P<'_> {
        fn ws(&mut self) {
            while self.b.get(self.i).is_some_and(u8::is_ascii_whitespace) {
                self.i += 1;
            }
        }
        fn expr(&mut self) -> Option<f64> {
            let mut v = self.term()?;
            loop {
                self.ws();
                match self.b.get(self.i) {
                    Some(b'+') => {
                        self.i += 1;
                        v += self.term()?;
                    }
                    Some(b'-') => {
                        self.i += 1;
                        v -= self.term()?;
                    }
                    _ => return Some(v),
                }
            }
        }
        fn term(&mut self) -> Option<f64> {
            let mut v = self.atom()?;
            loop {
                self.ws();
                match self.b.get(self.i) {
                    Some(b'*') => {
                        self.i += 1;
                        v *= self.atom()?;
                    }
                    Some(b'/') => {
                        self.i += 1;
                        v /= self.atom()?;
                    }
                    _ => return Some(v),
                }
            }
        }
        fn atom(&mut self) -> Option<f64> {
            self.ws();
            match self.b.get(self.i)? {
                b'-' => {
                    self.i += 1;
                    Some(-self.atom()?)
                }
                b'(' => {
                    self.i += 1;
                    let v = self.expr()?;
                    self.ws();
                    (self.b.get(self.i) == Some(&b')')).then(|| self.i += 1)?;
                    Some(v)
                }
                _ => {
                    let start = self.i;
                    while self
                        .b
                        .get(self.i)
                        .is_some_and(|c| c.is_ascii_digit() || matches!(c, b'.' | b'e' | b'E' | b'_'))
                        || (self.i > start
                            && matches!(self.b.get(self.i), Some(b'+' | b'-'))
                            && matches!(self.b.get(self.i - 1), Some(b'e' | b'E')))
                    {
                        self.i += 1;
                    }
                    std::str::from_utf8(&self.b[start..self.i]).ok()?.replace('_', "").parse().ok()
                }
            }
        }
    }
    let mut p = P { b: s.trim().as_bytes(), i: 0 };
    let v = p.expr()?;
    p.ws();
    (p.i == p.b.len() && v.is_finite()).then_some(v)
}
