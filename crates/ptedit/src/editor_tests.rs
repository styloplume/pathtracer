//! The editor driven headless by synthetic input: inspector edits, the outliner's
//! renaming, reordering, hiding and drag and drop, and the dialogs.

use crate::editor::{Editor, Section, Target};
use crate::settings::Settings;
use crate::ui::draw::Shape;
use crate::ui::{Input, Rect};
use crate::workspace::Workspace;
use pt_core::document::Kind;
use pt_core::platform::{Key, Modifiers};
use std::path::PathBuf;

const W: i32 = 1200;
const H: i32 = 800;
const NONE: Modifiers = Modifiers { shift: false, ctrl: false, alt: false };

struct Harness {
    ed: Editor,
    mouse: (i32, i32),
    down: bool,
    dir: PathBuf,
}

impl Drop for Harness {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.dir).ok();
    }
}

impl Harness {
    fn new(name: &str, scene: &str) -> Harness {
        let dir = std::env::temp_dir().join(format!("ptedit-ed-{name}-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("scene.toml");
        std::fs::write(&path, scene).unwrap();
        let ws = Workspace::open(&path).unwrap();
        let settings = Settings { font: "built-in".into(), ..Settings::default() };
        let mut h = Harness { ed: Editor::new(ws, settings, None, 1.0), mouse: (0, 0), down: false, dir };
        h.frame(Input::default());
        h.frame(Input::default());
        h
    }

    fn text(&self) -> String {
        self.ed.ws.doc.files[0].doc.text().to_owned()
    }

    fn frame(&mut self, mut i: Input) {
        i.mouse = self.mouse;
        i.down[0] = self.down;
        self.ed.frame(i, &[], W, H);
    }

    fn idle(&mut self) {
        self.frame(Input::default());
    }

    fn rect(&self, probe: &str) -> Rect {
        self.ed.probe_rect(probe).unwrap_or_else(|| {
            let names: Vec<&str> = self.ed.probes.iter().map(|(n, _)| n.as_str()).collect();
            panic!("no probe '{probe}' among {names:?}")
        })
    }

    fn move_to(&mut self, x: i32, y: i32) {
        self.mouse = (x, y);
        self.idle();
    }

    fn press(&mut self, clicks: u8) {
        self.down = true;
        let mut i = Input::default();
        i.pressed[0] = true;
        i.clicks = clicks;
        self.frame(i);
    }

    fn release(&mut self) {
        self.down = false;
        let mut i = Input::default();
        i.released[0] = true;
        self.frame(i);
    }

    fn click_at(&mut self, x: i32, y: i32, clicks: u8) {
        self.move_to(x, y);
        self.press(clicks);
        self.release();
    }

    /// Clicks probe `name` (at `fx` of its width), or double-clicks it.
    fn click(&mut self, name: &str, fx: f32, clicks: u8) {
        let r = self.rect(name);
        let (x, y) = (r.x + (r.w as f32 * fx) as i32, r.y + r.h / 2);
        for c in 1..=clicks {
            self.click_at(x, y, c);
        }
    }

    fn drag(&mut self, from: (i32, i32), to: (i32, i32)) {
        self.move_to(from.0, from.1);
        self.press(1);
        for k in 1..=4 {
            let t = k as f32 / 4.0;
            self.move_to(from.0 + ((to.0 - from.0) as f32 * t) as i32, from.1 + ((to.1 - from.1) as f32 * t) as i32);
        }
        self.release();
    }

    fn keys(&mut self, keys: &[(Key, Modifiers)], text: &str) {
        let i = Input { keys: keys.to_vec(), text: text.into(), ..Input::default() };
        self.frame(i);
    }

    /// Whether the last frame drew text containing `s`.
    fn shows(&self, s: &str) -> bool {
        self.ed.ui.cmds.iter().any(|c| matches!(&c.shape, Shape::Text { text, .. } if text.contains(s)))
    }
}

const SPHERES: &str = "\
# two spheres
[camera]
position = [0, 1, 6]

[[objects]]
type = \"sphere\"
center = [-1.5, 0, 0]   # left
radius = 0.5

[[objects]]
type = \"sphere\"
name = \"right\"
center = [1.5, 0, 0]

[materials.red]
albedo = [0.8, 0.1, 0.1]
";

#[test]
fn inspector_drags_and_types_values() {
    let mut h = Harness::new("inspect", SPHERES);
    h.ed.inspect = Some(Target::Item(Kind::Object, 0));
    h.idle();
    // Drag the radius 40 px right: (∞-range) steps of 1% of the value per pixel.
    let r = h.rect("inspector:radius");
    h.drag((r.x + r.w / 2, r.y + r.h / 2), (r.x + r.w / 2 + 40, r.y + r.h / 2));
    let text = h.text();
    assert!(text.contains("radius = 0.7\n"), "{text}");
    // One undo step for the whole drag.
    h.ed.run(crate::editor::Cmd::Undo);
    h.idle();
    assert_eq!(h.text(), SPHERES);
    // Type a name: the outliner follows.
    h.click("inspector:name", 0.5, 1);
    h.keys(&[], "ball");
    h.keys(&[(Key::Enter, NONE)], "");
    h.idle();
    assert!(h.text().contains("radius = 0.5\nname = \"ball\"\n"), "{}", h.text());
    assert!(h.ed.probe_rect("outliner:object:ball").is_some());
    // A vector component, typed as an expression.
    h.click("inspector:center", 0.2, 1);
    h.keys(&[(Key::Char('a'), Modifiers { ctrl: true, ..NONE })], "-3/2*2");
    h.keys(&[(Key::Enter, NONE)], "");
    assert!(h.text().contains("center = [-3, 0, 0]   # left"), "{}", h.text());
}

#[test]
fn defaults_are_written_when_changed() {
    let mut h = Harness::new("defaults", SPHERES);
    h.ed.inspect = Some(Target::Section(Section::Render));
    h.idle();
    assert!(h.shows("spp"));
    // `[render]` is not in the file: changing spp writes the section.
    h.click("inspector:spp", 0.5, 1);
    h.keys(&[(Key::Char('a'), Modifiers { ctrl: true, ..NONE })], "64");
    h.keys(&[(Key::Enter, NONE)], "");
    h.idle();
    assert!(h.text().ends_with("\n[render]\nspp = 64\n"), "{}", h.text());
    // A choice from a drop-down.
    let r = h.rect("inspector:tonemap");
    h.click_at(r.x + 10, r.y + r.h / 2, 1);
    h.idle();
    // The options list below the field: aces, reinhard, none.
    let rh = h.ed.ui.row_h();
    h.click_at(r.x + 20, r.bottom() + h.ed.ui.px(4.0) + rh + rh / 2, 1);
    h.idle();
    assert!(h.text().contains("tonemap = \"reinhard\""), "{}", h.text());
    // The camera's position is set in the file; its defaults are not.
    h.ed.inspect = Some(Target::Section(Section::Camera));
    h.idle();
    assert!(h.ed.probe_rect("inspector:fov").is_some());
}

#[test]
fn problems_show_next_to_their_fields() {
    let scene = "[[objects]]\ntype = \"sphere\"\nradius = -1\nradiuss = 2\n\n[[objects]]\ntype = \"quad\"\n";
    let mut h = Harness::new("problems", scene);
    h.ed.inspect = Some(Target::Item(Kind::Object, 0));
    h.idle();
    // Wrapped over the panel's width.
    assert!(h.shows("'radius' must be") && h.shows("(0, ∞)"));
    assert!(h.ed.status.contains("does not load"), "the scene opened anyway: {}", h.ed.status);
    assert!(h.shows("radiuss"), "unknown keys are listed");
    h.ed.inspect = Some(Target::Item(Kind::Object, 1));
    h.idle();
    assert!(h.shows("'origin' is"));
}

#[test]
fn outliner_renames_reorders_and_hides() {
    let mut h = Harness::new("outliner", SPHERES);
    // Click selects in the viewport too.
    h.click("outliner:object:sphere", 0.3, 1);
    assert!(h.ed.ws.sel.objects[0] && !h.ed.ws.sel.objects[1]);
    assert_eq!(h.ed.inspect, Some(Target::Item(Kind::Object, 0)));
    // Double-click renames.
    h.click("outliner:object:sphere", 0.3, 2);
    h.idle();
    h.keys(&[(Key::Char('a'), Modifiers { ctrl: true, ..NONE })], "left");
    h.keys(&[(Key::Enter, NONE)], "");
    h.idle();
    assert!(h.text().contains("radius = 0.5\nname = \"left\"\n"), "{}", h.text());
    // Drag "right" above "left": the [[objects]] blocks swap, comments and all.
    let (a, b) = (h.rect("outliner:object:right"), h.rect("outliner:object:left"));
    h.drag((a.x + 30, a.y + a.h / 2), (b.x + 30, b.y + 4));
    h.idle();
    let labels: Vec<String> = h.ed.ws.doc.tree.objects.iter().map(|o| o.label.clone()).collect();
    assert_eq!(labels, ["right", "left"]);
    assert!(h.text().contains("center = [-1.5, 0, 0]   # left"));
    // A material dropped on an object is assigned to it.
    let (m, o) = (h.rect("outliner:material:red"), h.rect("outliner:object:left"));
    h.drag((m.x + 20, m.y + m.h / 2), (o.x + 40, o.y + o.h / 2));
    h.idle();
    assert!(h.text().contains("name = \"left\"\nmaterial = \"red\"\n"), "{}", h.text());
    // Renaming a material renames its references.
    h.click("outliner:material:red", 0.3, 2);
    h.idle();
    h.keys(&[(Key::Char('a'), Modifiers { ctrl: true, ..NONE })], "crimson");
    h.keys(&[(Key::Enter, NONE)], "");
    h.idle();
    assert!(h.text().contains("material = \"crimson\"") && h.text().contains("[materials.crimson]"), "{}", h.text());
    // The eye hides an object from the views (not from the file).
    let o = h.rect("outliner:object:right");
    h.click_at(o.right() - h.ed.ui.row_h() / 2, o.y + o.h / 2, 1);
    assert!(h.ed.ws.hidden.objects[0]);
    let before = h.text();
    h.ed.run(crate::editor::Cmd::ShowAll);
    assert!(!h.ed.ws.hidden.objects[0]);
    assert_eq!(h.text(), before);
}

#[test]
fn keyboard_walks_the_outliner_and_tabs_through_fields() {
    let mut h = Harness::new("keys", SPHERES);
    h.click("outliner:object:sphere", 0.3, 1);
    h.keys(&[(Key::Down, NONE)], "");
    h.idle();
    assert_eq!(h.ed.inspect, Some(Target::Item(Kind::Object, 1)));
    h.keys(&[(Key::Down, NONE)], "");
    h.idle();
    assert_eq!(h.ed.inspect, Some(Target::Item(Kind::Material, 0)), "past the group header");
    // Shortcuts do not fire while typing.
    h.ed.inspect = Some(Target::Item(Kind::Object, 0));
    h.idle();
    h.click("inspector:name", 0.5, 1);
    let grid = h.ed.ws.grid;
    h.keys(&[(Key::Char('g'), NONE)], "g");
    assert_eq!(h.ed.ws.grid, grid);
    h.keys(&[(Key::Escape, NONE)], "");
    h.keys(&[(Key::Char('g'), NONE)], "g");
    assert_eq!(h.ed.ws.grid, !grid);
}

#[test]
fn menus_and_dialogs() {
    let mut h = Harness::new("menus", SPHERES);
    // View > Light Theme, then Ctrl+O opens the file browser, Escape closes it.
    let dark = h.ed.ui.theme.panel;
    h.ed.run(crate::editor::Cmd::Theme);
    h.idle();
    assert_ne!(h.ed.ui.theme.panel, dark);
    h.keys(&[(Key::Char('o'), Modifiers { ctrl: true, ..NONE })], "");
    h.idle();
    assert!(h.ed.ui.modal_open());
    assert!(h.shows("scene.toml"), "the browser lists the scene's folder");
    h.keys(&[(Key::Escape, NONE)], "");
    h.idle();
    h.idle();
    assert!(!h.ed.ui.modal_open());
    // Quitting with unsaved changes asks first.
    h.ed.run(crate::editor::Cmd::Hide);
    let p = vec![pt_core::toml::Seg::from("camera"), pt_core::toml::Seg::from("fov")];
    h.ed.apply(0, &p, &pt_core::toml::Value::Number(30.0), None, true);
    h.ed.run(crate::editor::Cmd::Quit);
    h.idle();
    assert!(!h.ed.quit && h.shows("Save the changes to scene.toml"));
}
