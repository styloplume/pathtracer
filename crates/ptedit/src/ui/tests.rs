//! The toolkit driven by synthetic input, frame by frame.

use super::*;

pub fn test_ui() -> Ui {
    Ui::new(Theme::dark(), Font::bitmap(8.0), 1.0)
}

/// Runs frames of a UI under scripted input.
pub struct Driver {
    pub ui: Ui,
    mouse: (i32, i32),
    down: [bool; 3],
}

impl Driver {
    pub fn new() -> Driver {
        Driver { ui: test_ui(), mouse: (0, 0), down: [false; 3] }
    }

    fn input(&self) -> Input {
        Input { mouse: self.mouse, down: self.down, ..Input::default() }
    }

    /// One frame with `input`, running `f`.
    pub fn frame_with<R>(&mut self, input: Input, f: impl FnOnce(&mut Ui) -> R) -> R {
        self.ui.begin(input, 400, 300);
        let r = f(&mut self.ui);
        self.ui.end();
        r
    }

    pub fn frame<R>(&mut self, f: impl FnOnce(&mut Ui) -> R) -> R {
        let i = self.input();
        self.frame_with(i, f)
    }

    pub fn move_to<R>(&mut self, x: i32, y: i32, f: impl FnOnce(&mut Ui) -> R) -> R {
        self.mouse = (x, y);
        self.frame(f)
    }

    pub fn press<R>(&mut self, clicks: u8, f: impl FnOnce(&mut Ui) -> R) -> R {
        self.down[0] = true;
        let mut i = self.input();
        i.pressed[0] = true;
        i.clicks = clicks;
        self.frame_with(i, f)
    }

    pub fn release<R>(&mut self, f: impl FnOnce(&mut Ui) -> R) -> R {
        self.down[0] = false;
        let mut i = self.input();
        i.released[0] = true;
        self.frame_with(i, f)
    }

    pub fn keys<R>(&mut self, keys: &[(Key, Modifiers)], text: &str, f: impl FnOnce(&mut Ui) -> R) -> R {
        let mut i = self.input();
        i.keys = keys.to_vec();
        i.text = text.into();
        self.frame_with(i, f)
    }
}

const NONE: Modifiers = Modifiers { shift: false, ctrl: false, alt: false };
const R: Rect = Rect::new(10, 10, 100, 20);

#[test]
fn buttons_click_on_release_inside() {
    let mut d = Driver::new();
    let b = |ui: &mut Ui| ui.button(1, R, "OK");
    assert!(!d.move_to(20, 15, b));
    assert!(!d.press(1, b));
    assert!(d.release(b));
    // Pressed inside, released outside: no click.
    d.press(1, b);
    d.mouse = (300, 200);
    assert!(!d.release(b));
    // Keyboard: Tab to it, Enter.
    d.ui.focus = 0;
    d.frame(b);
    assert!(!d.keys(&[(Key::Tab, NONE)], "", b));
    assert_eq!(d.ui.focus, 1);
    assert!(d.keys(&[(Key::Enter, NONE)], "", b));
}

#[test]
fn text_fields_edit_and_commit() {
    let mut d = Driver::new();
    let mut value = String::from("old");
    // Runs a frame of the field through driver method `$call`, keeping what it commits.
    macro_rules! field {
        ($call:ident $(, $arg:expr)*) => {{
            let v = value.clone();
            let out = d.$call($($arg,)* |ui| ui.text_field(7, R, &v, ""));
            if let Some(n) = &out {
                value.clone_from(n);
            }
            out
        }};
    }
    field!(move_to, 30, 20);
    field!(press, 1);
    field!(release);
    assert!(d.ui.wants_keyboard());
    // Clicking placed the caret; select everything and type over it.
    let ctrl = Modifiers { ctrl: true, ..NONE };
    field!(keys, &[(Key::Char('a'), ctrl)], "");
    field!(keys, &[], "new value");
    field!(keys, &[(Key::Backspace, ctrl)], "");
    let committed = field!(keys, &[(Key::Enter, NONE)], "");
    assert_eq!(committed.as_deref(), Some("new "));
    assert!(!d.ui.wants_keyboard());
    // Escape cancels.
    field!(press, 1);
    field!(release);
    field!(keys, &[], "xyz");
    assert_eq!(field!(keys, &[(Key::Escape, NONE)], ""), None);
    assert_eq!(value, "new ");
    // Copy goes to the clipboard, paste comes from it.
    field!(press, 1);
    field!(release);
    field!(keys, &[(Key::Char('a'), ctrl), (Key::Char('c'), ctrl)], "");
    assert_eq!(d.ui.copy.as_deref(), Some("new "));
    let mut i = Input { mouse: (30, 20), keys: vec![(Key::End, NONE), (Key::Char('v'), ctrl)], ..Input::default() };
    i.paste = Some("pasted".into());
    d.frame_with(i, |ui| ui.text_field(7, R, "new ", ""));
    // Clicking elsewhere commits.
    d.mouse = (300, 200);
    d.press(1, |ui| ui.text_field(7, R, "new ", ""));
    let out = d.release(|ui| ui.text_field(7, R, "new ", ""));
    assert_eq!(out.as_deref(), Some("new pasted"));
}

#[test]
fn numbers_drag_and_type() {
    let mut d = Driver::new();
    let o = NumOpts::range(0.0, 10.0);
    let mut v = 5.0;
    let run = |d: &mut Driver, v: &mut f64, kind: u8, x: i32| {
        d.mouse = (x, 20);
        let cur = *v;
        let f = |ui: &mut Ui| ui.drag_number(3, R, cur, &o, 0x00FF_FFFF);
        let r = match kind {
            0 => d.frame(f),
            1 => d.press(1, f),
            _ => d.release(f),
        };
        if let Some(x) = r.value {
            *v = x;
        }
        r
    };
    run(&mut d, &mut v, 0, 50);
    run(&mut d, &mut v, 1, 50);
    let r = run(&mut d, &mut v, 0, 70);
    assert!(r.value.is_some() && !r.done);
    assert!((v - 6.0).abs() < 1e-9, "20 px at (10 - 0) / 200 per px: {v}");
    let r = run(&mut d, &mut v, 2, 70);
    assert!(r.done);
    // A click without moving types the value.
    run(&mut d, &mut v, 1, 70);
    run(&mut d, &mut v, 2, 70);
    assert!(d.ui.wants_keyboard());
    let ctrl = Modifiers { ctrl: true, ..NONE };
    d.keys(&[(Key::Char('a'), ctrl)], "2*1.5", |ui| ui.drag_number(3, R, v, &o, 0));
    let r = d.keys(&[(Key::Enter, NONE)], "", |ui| ui.drag_number(3, R, v, &o, 0));
    assert_eq!((r.value, r.done), (Some(3.0), true));
    assert!(widgets::eval("(1 + 2) * -2e-1").is_some_and(|v| (v + 0.6).abs() < 1e-12));
    assert_eq!(widgets::eval("1 +"), None);
}

#[test]
fn combos_pick_from_a_popup_that_closes_outside() {
    let mut d = Driver::new();
    let opts: Vec<String> = ["red", "green", "blue"].map(String::from).to_vec();
    let c = |ui: &mut Ui| ui.combo(4, R, "red", &opts, 0x00FF_FFFF);
    d.move_to(30, 20, c);
    d.press(1, c);
    d.release(c);
    assert!(d.ui.is_popup_open(id_of(4, "popup")));
    d.frame(c); // learns its size
    // Below the combo: rows of 16 px (8 px font + 8), after 4 px of padding.
    d.move_to(30, 30 + 4 + 16 + 8, c);
    d.press(1, c);
    assert_eq!(d.release(c), Some(1));
    assert!(!d.ui.is_popup_open(id_of(4, "popup")));
    // Opened again, a press elsewhere closes it without reaching what is there.
    d.move_to(30, 20, c);
    d.press(1, c);
    d.release(c);
    let b = |ui: &mut Ui| {
        ui.combo(4, R, "red", &opts, 0x00FF_FFFF);
        ui.button(9, Rect::new(300, 250, 50, 20), "x")
    };
    d.move_to(310, 260, b);
    d.press(1, b);
    assert!(!d.release(b), "the press only closed the popup");
    assert!(!d.ui.is_popup_open(id_of(4, "popup")));
}

#[test]
fn modals_block_the_widgets_behind_them() {
    let mut d = Driver::new();
    let f = |ui: &mut Ui| {
        let behind = ui.button(1, R, "behind");
        let r = ui.begin_modal("Dialog", 200.0, 100.0);
        let ok = ui.button(2, Rect::new(r.x, r.y, 60, 20), "OK");
        ui.end_modal();
        (behind, ok, r)
    };
    let (_, _, r) = d.frame(f);
    d.move_to(20, 15, f);
    d.press(1, f);
    assert!(!d.release(f).0);
    d.move_to(r.x + 5, r.y + 5, f);
    d.press(1, f);
    assert!(d.release(f).1);
    assert!(d.ui.wants_keyboard());
}

#[test]
fn scroll_areas_scroll_and_clip() {
    let mut d = Driver::new();
    let area = Rect::new(0, 0, 100, 50);
    let f = |ui: &mut Ui| {
        let (s, mut col) = ui.begin_scroll(5, area);
        let mut hits = Vec::new();
        for i in 0..20 {
            let r = col.row(10);
            if ui.interact(id_of(5, i), r, false).hovered {
                hits.push(i);
            }
        }
        ui.end_scroll(s, col.y);
        hits
    };
    assert_eq!(d.move_to(20, 5, f), vec![0]);
    let mut i = Input { mouse: (20, 5), wheel: -1.0, ..Input::default() };
    d.frame_with(i.clone(), f);
    i.wheel = 0.0;
    // Three rows of 16 px per notch: 48 px down.
    assert_eq!(d.frame_with(i, f), vec![5]);
    // Rows scrolled out of the area are clipped away.
    assert_eq!(d.move_to(20, 60, f), Vec::<i32>::new());
}

#[test]
fn rows_drag_and_drop() {
    let mut d = Driver::new();
    let a = Rect::new(0, 0, 100, 16);
    let b = Rect::new(0, 40, 100, 16);
    let f = |ui: &mut Ui| {
        let ra = ui.interact(1, a, false);
        ui.drag_source(&ra, 42, "row a");
        ui.interact(2, b, false);
        ui.dropped(b)
    };
    d.move_to(10, 5, f);
    d.press(1, f);
    d.move_to(10, 30, f);
    d.move_to(10, 45, f);
    assert!(d.ui.dnd.as_ref().is_some_and(|x| x.active));
    assert_eq!(d.release(f), Some(42));
    assert!(d.ui.dnd.is_none());
}

#[test]
fn tooltips_wait_for_the_mouse_to_rest() {
    let mut d = Driver::new();
    let f = |ui: &mut Ui| {
        let r = ui.interact(1, R, false);
        ui.tooltip(&r, "help");
    };
    d.move_to(20, 15, f);
    d.frame(f);
    assert!(d.ui.repaint_at().is_some(), "asks to be drawn when the tooltip is due");
    d.ui.hover.1 -= TOOLTIP_DELAY;
    d.frame(f);
    assert!(d.ui.cmds.iter().any(|c| c.layer == LAYER_TOOLTIP));
}
