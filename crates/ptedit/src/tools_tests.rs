//! Scene editing driven headless: adding, deleting, duplicating, copy and paste between
//! scenes, the gizmos (with typed values and snapping), handles, the camera, prototypes,
//! the material library, presets, the path-traced view and hot reload.

use crate::edit::Add;
use crate::editor::{Cmd, Section, Target};
use crate::editor_tests::{Harness, NONE};
use crate::gizmo::{self, HandleKind, Tool};
use crate::render::KEY;
use crate::tools::Snap;
use crate::ui::Input;
use pt_core::document::Kind;
use pt_core::math::Vec3;
use pt_core::platform::{Event, Key, Modifiers, MouseButton};
use pt_core::toml::{Seg, Value};

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

fn value(h: &Harness, path: &[&str]) -> Option<Value> {
    let p: Vec<Seg> = path.iter().map(|&s| s.parse::<usize>().map_or_else(|_| Seg::from(s), Seg::Index)).collect();
    h.ed.ws.doc.value(0, &p)
}

fn vec_at(h: &Harness, path: &[&str]) -> Vec3 {
    let v = value(h, path).unwrap_or_else(|| panic!("{path:?} in {}", h.text()));
    let n: Vec<f32> = v.as_array().unwrap().iter().map(|x| x.as_f64().unwrap() as f32).collect();
    Vec3::new(n[0], n[1], n[2])
}

fn select(h: &mut Harness, probe: &str) {
    h.click(probe, 0.3, 1);
    h.idle();
}

/// Window pixel of a point of the gizmo, at `t` of its size from the pivot along `dir`.
fn gizmo_point(h: &Harness, dir: Vec3, t: f32) -> (i32, i32) {
    let g = h.ed.ws.decor.gizmo.expect("a gizmo");
    let vp = h.ed.vp;
    let r = h.ed.ws.rects(vp.w as usize, vp.h as usize)[0];
    let proj = h.ed.ws.proj(&r);
    let s = gizmo::size(&proj, g.pivot, h.ed.ui.scale);
    h.screen_of(g.pivot + dir * (s * t))
}

#[test]
fn add_delete_and_duplicate() {
    let mut h = Harness::new("add", SPHERES);
    h.ed.run(Cmd::Add(Add::Box));
    h.idle();
    assert_eq!(h.ed.ws.doc.tree.objects.len(), 3);
    assert!(h.text().contains("[[objects]]\ntype = \"box\"\ncenter = ["), "{}", h.text());
    assert!(h.ed.ws.sel.objects[2], "the new box is selected");
    assert!(h.ed.ws.load_error.is_none());
    h.ed.run(Cmd::Add(Add::SpotLight));
    h.idle();
    assert_eq!(h.ed.ws.doc.tree.lights.len(), 1);
    assert!(h.ed.ws.sel.lights[0] && !h.ed.ws.sel.objects[2]);

    // The left sphere, duplicated with its comment, then the copy deleted.
    select(&mut h, "outliner:object:sphere");
    h.key_event(Key::Char('d'), Modifiers { ctrl: true, ..NONE });
    h.idle();
    assert_eq!(h.ed.ws.doc.tree.objects.len(), 4);
    assert_eq!(h.text().matches("center = [-1.5, 0, 0]   # left").count(), 2, "{}", h.text());
    assert!(h.ed.ws.sel.objects[3] && !h.ed.ws.sel.objects[0], "the copy is selected");
    h.key_event(Key::Delete, NONE);
    h.idle();
    assert_eq!(h.ed.ws.doc.tree.objects.len(), 3);
    assert_eq!(h.text().matches("# left").count(), 1);
    h.ed.run(Cmd::Undo);
    h.idle();
    assert_eq!(h.ed.ws.doc.tree.objects.len(), 4);
    for _ in 0..3 {
        h.ed.run(Cmd::Undo);
        h.idle();
    }
    assert_eq!(h.text(), SPHERES);
}

#[test]
fn copy_and_paste_between_scenes() {
    let mut a =
        Harness::new("copy-a", &format!("{SPHERES}\n[[objects]]\nfile = \"models/tri.obj\"\nmaterial = \"red\"\n"));
    std::fs::create_dir_all(a.dir.join("models")).unwrap();
    std::fs::write(a.dir.join("models/tri.obj"), "v 0 0 0\nv 1 0 0\nv 0 1 0\nf 1 2 3\n").unwrap();
    a.ed.ws.doc.set(0, &[Seg::from("camera"), Seg::from("fov")], &Value::Number(40.0)).unwrap();
    a.ed.scene_stale = true;
    a.idle();
    a.ed.ws.sel.clear();
    a.ed.ws.sel.objects[2] = true;
    let clip = a.ed.copy_text().unwrap();
    assert!(clip.contains("[materials.red]") && clip.contains("[[objects]]"), "{clip}");

    let mut b = Harness::new("copy-b", "[camera]\nposition = [0, 0, 5]\n");
    b.ed.paste(&clip);
    b.idle();
    let text = b.text();
    assert!(text.contains("[materials.red]\nalbedo = [0.8, 0.1, 0.1]"), "{text}");
    let a_name = a.dir.file_name().unwrap().to_string_lossy().into_owned();
    assert!(text.contains(&format!("file = \"../{a_name}/models/tri.obj\"")), "{text}");
    assert!(b.ed.ws.load_error.is_none(), "{:?}", b.ed.ws.load_error);
    assert_eq!(b.ed.ws.doc.tree.objects.len(), 1);
    assert!(b.ed.ws.sel.objects[0], "the pasted object is selected");
    // Pasting again does not define the material twice.
    b.ed.paste(&clip);
    b.idle();
    assert_eq!(b.text().matches("[materials.red]").count(), 1);
    assert_eq!(b.ed.ws.doc.tree.objects.len(), 2);
}

/// Dragging the X arrow moves the sphere's `center` (its own convention) along X only, in
/// place (the same scene, refitted), as one undo step.
#[test]
fn gizmo_moves_along_an_axis() {
    let mut h = Harness::new("move", SPHERES);
    select(&mut h, "outliner:object:sphere");
    assert_eq!(h.ed.tool, Tool::Move);
    let before = std::sync::Arc::as_ptr(&h.ed.ws.scene);
    let gen0 = h.ed.ws.scene_gen;
    let from = gizmo_point(&h, Vec3::new(1.0, 0.0, 0.0), 0.7);
    h.drag_events(MouseButton::Left, from, (from.0 + 60, from.1), NONE);
    let c = vec_at(&h, &["objects", "0", "center"]);
    assert!(c.x > -1.4 && c.y == 0.0 && c.z == 0.0, "{c:?}");
    assert!(h.text().contains("]   # left"), "the comment stays: {}", h.text());
    assert!(h.ed.ws.scene_gen > gen0);
    assert_eq!(std::sync::Arc::as_ptr(&h.ed.ws.scene), before, "moved in place, not loaded again");
    // The scene has the sphere where the file says.
    let (x, y) = h.screen_of(c);
    let vp = h.ed.vp;
    let r = h.ed.ws.rects(vp.w as usize, vp.h as usize)[0];
    assert_eq!(h.ed.ws.pick(&r, (x - vp.x) as f32, (y - vp.y) as f32), Some(1));
    h.ed.run(Cmd::Undo);
    h.idle();
    assert_eq!(h.text(), SPHERES);

    // A typed distance, then Enter.
    let from = gizmo_point(&h, Vec3::new(1.0, 0.0, 0.0), 0.7);
    h.events(vec![Event::MouseMove { x: from.0, y: from.1, mods: NONE }]);
    h.events(vec![Event::MouseDown { button: MouseButton::Left, x: from.0, y: from.1, mods: NONE }]);
    h.key_event(Key::Char('2'), NONE);
    h.key_event(Key::Enter, NONE);
    h.events(vec![Event::MouseUp { button: MouseButton::Left, x: from.0, y: from.1, mods: NONE }]);
    h.idle();
    assert_eq!(vec_at(&h, &["objects", "0", "center"]), Vec3::new(0.5, 0.0, 0.0), "{}", h.text());
    assert_eq!(h.ed.ws.shading, crate::render::Shading::Solid, "'2' went to the gizmo, not the shading");
}

/// Typing an angle on the Y ring: the sphere's center becomes its translation and it
/// turns about it; Esc during a drag puts everything back.
#[test]
fn gizmo_rotates_and_cancels() {
    let mut h = Harness::new("rotate", SPHERES);
    select(&mut h, "outliner:object:right");
    h.ed.run(Cmd::Tool(Tool::Rotate));
    h.idle();
    let from = gizmo_point(&h, (Vec3::new(-1.0, 0.0, 0.0) + Vec3::Z).normalize(), 1.0);
    h.events(vec![Event::MouseMove { x: from.0, y: from.1, mods: NONE }]);
    h.events(vec![Event::MouseDown { button: MouseButton::Left, x: from.0, y: from.1, mods: NONE }]);
    for c in ['9', '0'] {
        h.key_event(Key::Char(c), NONE);
    }
    h.key_event(Key::Enter, NONE);
    h.events(vec![Event::MouseUp { button: MouseButton::Left, x: from.0, y: from.1, mods: NONE }]);
    h.idle();
    let t = h.text();
    assert!(t.contains("name = \"right\"\ntranslate = [1.5, 0, 0]\nrotate = [0, 90, 0]\n"), "{t}");
    assert!(!t.contains("center = [1.5"), "{t}");

    // A drag cancelled with Esc.
    let before = h.text();
    let from = gizmo_point(&h, (Vec3::new(-1.0, 0.0, 0.0) + Vec3::Z).normalize(), 1.0);
    h.events(vec![Event::MouseMove { x: from.0, y: from.1, mods: NONE }]);
    h.events(vec![Event::MouseDown { button: MouseButton::Left, x: from.0, y: from.1, mods: NONE }]);
    h.events(vec![Event::MouseMove { x: from.0 + 30, y: from.1 + 40, mods: NONE }]);
    assert_ne!(h.text(), before);
    h.key_event(Key::Escape, NONE);
    h.events(vec![Event::MouseUp { button: MouseButton::Left, x: from.0 + 30, y: from.1 + 40, mods: NONE }]);
    h.idle();
    assert_eq!(h.text(), before);
    assert!(h.ed.ws.sel.objects[1], "Esc cancelled the drag, not the selection");
}

#[test]
fn moves_snap_to_the_grid_and_scales_keep_numbers() {
    let mut h = Harness::new("snap", SPHERES);
    select(&mut h, "outliner:object:sphere");
    h.ed.run(Cmd::Snap(Snap::Grid));
    let from = gizmo_point(&h, Vec3::new(1.0, 0.0, 0.0), 0.7);
    h.drag_events(MouseButton::Left, from, (from.0 + 47, from.1), NONE);
    let x = vec_at(&h, &["objects", "0", "center"]).x;
    let vp = h.ed.vp;
    let r = h.ed.ws.rects(vp.w as usize, vp.h as usize)[0];
    let step = crate::tools::grid_step(&h.ed.ws.proj(&r), Vec3::new(-1.5, 0.0, 0.0));
    let k = x / step;
    assert!((k - k.round()).abs() < 1e-3 && (x + 1.5).abs() > 1e-3, "{x} on a grid of {step}");

    // Uniform scale with a typed factor: a number.
    h.ed.run(Cmd::Tool(Tool::Scale));
    h.idle();
    let g = h.ed.ws.decor.gizmo.unwrap();
    let c = h.screen_of(g.pivot);
    h.events(vec![Event::MouseMove { x: c.0, y: c.1, mods: NONE }]);
    h.events(vec![Event::MouseDown { button: MouseButton::Left, x: c.0, y: c.1, mods: NONE }]);
    h.key_event(Key::Char('3'), NONE);
    h.key_event(Key::Enter, NONE);
    h.events(vec![Event::MouseUp { button: MouseButton::Left, x: c.0, y: c.1, mods: NONE }]);
    h.idle();
    assert_eq!(value(&h, &["objects", "0", "scale"]), Some(Value::Number(3.0)), "{}", h.text());
    assert!(h.ed.ws.load_error.is_none());
}

const LIGHTS: &str = "\
[camera]
position = [0, 2, 8]
look_at = [0, 0, 0]

[background]
type = \"sky\"
sun_elevation = 30
sun_azimuth = 0

[[lights]]
type = \"spot\"
name = \"spot\"
position = [0, 3, 0]
direction = [0, -1, 0]
angle = 30

[[objects]]
type = \"quad\"
origin = [-3, 0, -3]
u = [0, 0, 6]
v = [6, 0, 0]
";

fn handle(h: &Harness, kind: HandleKind) -> Vec3 {
    h.ed.ws.decor.handles.iter().find(|x| x.kind == kind).unwrap_or_else(|| panic!("{kind:?}")).pos
}

#[test]
fn light_handles_and_the_sun() {
    let mut h = Harness::new("handles", LIGHTS);
    select(&mut h, "outliner:light:spot");
    // The cone's handle dragged away from the axis widens it.
    // Away from the cone's axis (straight down from the light at x = z = 0).
    let at = handle(&h, HandleKind::SpotAngle(0));
    let out = at + Vec3::new(at.x, 0.0, at.z) * 0.6;
    let (a, b) = (h.screen_of(at), h.screen_of(out));
    h.drag_events(MouseButton::Left, a, b, NONE);
    let angle = value(&h, &["lights", "0", "angle"]).and_then(|v| v.as_f64()).unwrap();
    assert!(angle > 33.0, "{angle}");
    let spot = h.ed.ws.scene.provenance.lights.start as usize;
    let pt_core::lights::Light::Spot { cos_outer, .. } = h.ed.ws.scene.lights()[spot] else { panic!("a spot") };
    assert!((cos_outer.acos().to_degrees() - angle as f32).abs() < 0.01, "the scene's light follows");

    // The sun, with the background inspected: dragged up, it rises.
    h.ed.ws.sel.clear();
    h.ed.inspect = Some(Target::Section(Section::Background));
    h.idle();
    let sun = handle(&h, HandleKind::Sun(None));
    let a = h.screen_of(sun);
    h.drag_events(MouseButton::Left, a, (a.0, a.1 - 40), NONE);
    let el = value(&h, &["background", "sun_elevation"]).and_then(|v| v.as_f64()).unwrap();
    assert!(el > 31.0, "{el}: {}", h.text());
    assert!(h.ed.ws.load_error.is_none());
}

/// Looking through the camera with it locked: orbiting moves the camera in the file, as
/// one undo step; click-to-focus sets the focus distance.
#[test]
fn camera_follows_the_view_and_focuses_on_clicks() {
    let mut h = Harness::new("camera", SPHERES);
    h.ed.run(Cmd::LockCamera);
    h.idle();
    assert!(h.ed.ws.through_camera && h.ed.ws.lock_camera);
    h.drag_events(MouseButton::Middle, (600, 400), (660, 380), NONE);
    let pos = vec_at(&h, &["camera", "position"]);
    assert!((pos - Vec3::new(0.0, 1.0, 6.0)).length() > 0.1, "{pos:?}");
    assert!(value(&h, &["camera", "look_at"]).is_some());
    assert!((h.ed.ws.scene.camera.position - pos).length() < 1e-4, "the scene's camera too");
    h.ed.run(Cmd::Undo);
    h.idle();
    assert_eq!(h.text(), SPHERES);

    h.ed.run(Cmd::LockCamera);
    h.ed.run(Cmd::PickFocus);
    let c = h.screen_of(Vec3::new(1.5, 0.0, 0.0));
    h.events(vec![Event::MouseMove { x: c.0, y: c.1, mods: NONE }]);
    h.events(vec![Event::MouseDown { button: MouseButton::Left, x: c.0, y: c.1, mods: NONE }]);
    h.events(vec![Event::MouseUp { button: MouseButton::Left, x: c.0, y: c.1, mods: NONE }]);
    h.idle();
    let fd = value(&h, &["camera", "focus_distance"]).and_then(|v| v.as_f64()).unwrap();
    assert!(fd > 4.0 && fd < 6.5, "{fd}");
    assert!(h.ed.ws.sel.is_empty(), "the focus click selected nothing");
}

#[test]
fn selections_become_prototypes() {
    let mut h = Harness::new("proto", SPHERES);
    h.ed.run(Cmd::SelectAll);
    h.ed.run(Cmd::MakePrototype);
    h.keys(&[(Key::Escape, NONE)], "");
    h.idle();
    let t = h.text();
    assert!(t.contains("[prototypes.prototype]\n"), "{t}");
    assert_eq!(t.matches("[[prototypes.prototype.objects]]").count(), 2, "{t}");
    assert!(t.contains("[[objects]]\ntype = \"instance\"\nprototype = \"prototype\"\ntranslate = ["), "{t}");
    assert!(t.contains("# left"), "the parts keep their comments");
    assert!(h.ed.ws.load_error.is_none(), "{:?}", h.ed.ws.load_error);
    assert_eq!(h.ed.ws.doc.tree.objects.len(), 1);
    // Both spheres are still where they were.
    let vp = h.ed.vp;
    let r = h.ed.ws.rects(vp.w as usize, vp.h as usize)[0];
    for p in [Vec3::new(-1.5, 0.0, 0.0), Vec3::new(1.5, 0.0, 0.0)] {
        let (x, y) = h.screen_of(p);
        assert_eq!(h.ed.ws.pick(&r, (x - vp.x) as f32, (y - vp.y) as f32), Some(1), "{p:?}");
    }
    // Add > Instance of: another one under the mouse.
    h.ed.add_instance("prototype");
    h.idle();
    assert_eq!(h.ed.ws.doc.tree.objects.len(), 2);
}

#[test]
fn library_materials_are_dropped_onto_objects() {
    let mut h = Harness::new("library", SPHERES);
    std::fs::create_dir_all(h.dir.join("lib")).unwrap();
    std::fs::write(h.dir.join("lib/mats.toml"), "# metals\n[materials.gold]\ntype = \"metal\"\npreset = \"gold\"\n")
        .unwrap();
    h.click("tab:materials", 0.5, 1);
    h.idle();
    let tile = h.rect("library:mats.toml:gold");
    let target = h.screen_of(Vec3::new(-1.5, 0.0, 0.0));
    h.drag((tile.x + 10, tile.y + tile.h / 2), target);
    h.idle();
    let t = h.text();
    assert!(t.contains("[materials.gold]\ntype = \"metal\"\npreset = \"gold\""), "{t}");
    assert!(t.contains("radius = 0.5\nmaterial = \"gold\""), "{t}");
    assert!(h.ed.ws.load_error.is_none());
    // Previews come from the worker.
    let table = pt_core::toml::parse("type = \"metal\"\npreset = \"gold\"\n").unwrap();
    h.ed.library.material(&table, &h.dir.clone(), 24);
    h.ed.library.finish();
    let p = h.ed.library.material(&table, &h.dir.clone(), 24).expect("rendered");
    assert_eq!(p.px.len(), 24 * 24);
    // The material editor shows a preview.
    h.ed.inspect = Some(Target::Item(Kind::Material, 0));
    h.idle();
    assert!(h.ed.probe_rect("inspector:preview").is_some());
}

#[test]
fn presets_write_settings() {
    let mut h = Harness::new("presets", SPHERES);
    h.ed.render_preset(2);
    h.ed.size_preset(1);
    h.idle();
    let t = h.text();
    assert!(
        t.contains("[render]\nspp = 1024\nmax_depth = 12\nnoise_threshold = 0.01\nwidth = 1280\nheight = 720\n"),
        "{t}"
    );
    h.ed.output_preset(3);
    h.idle();
    assert!(h.text().contains("[output]\nformats = [\"png\", \"pfm\"]"), "{}", h.text());
    assert!(h.ed.ws.load_error.is_none());
    h.ed.run(Cmd::Undo);
    h.ed.run(Cmd::Undo);
    h.ed.run(Cmd::Undo);
    h.idle();
    assert_eq!(h.text(), SPHERES);
}

const LIT: &str = "\
[camera]
position = [0, 1, 6]
look_at = [0, 0, 0]

[render]
spp = 8

[background]
type = \"gradient\"
zenith = [0.3, 0.4, 0.8]
horizon = [0.8, 0.8, 0.8]

[[objects]]
type = \"sphere\"
center = [0, 0, 0]
material = { type = \"diffuse\", albedo = [0.8, 0.2, 0.2] }
";

#[test]
fn the_view_is_path_traced_and_follows_moves() {
    let mut h = Harness::new("traced", LIT);
    h.ed.run(Cmd::Rendered);
    h.idle();
    let (w, hh) = (crate::editor_tests::W as usize, crate::editor_tests::H as usize);
    // Drawing the view starts the render.
    h.ed.render(w, hh);
    h.ed.ws.pt.wait_for(8, std::time::Duration::from_secs(120));
    h.idle();
    assert!(h.ed.vp_dirty, "new samples redraw the view");
    let px = h.ed.render(w, hh);
    let at = |p: Vec3| {
        let (x, y) = h.screen_of(p);
        px[y as usize * w + x as usize]
    };
    let ball = at(Vec3::new(0.0, 0.3, 0.9));
    let (r, g, b) = (ball >> 16 & 0xff, ball >> 8 & 0xff, ball & 0xff);
    assert!(r > g + 20 && r > b + 20, "the red ball, path traced: {ball:06x}");
    assert!(!px.contains(&KEY));

    // A move refits the scene the render uses.
    select(&mut h, "outliner:object:sphere");
    let before = std::sync::Arc::as_ptr(&h.ed.ws.scene);
    let from = gizmo_point(&h, Vec3::new(1.0, 0.0, 0.0), 0.7);
    h.drag_events(MouseButton::Left, from, (from.0 + 50, from.1), NONE);
    assert_eq!(std::sync::Arc::as_ptr(&h.ed.ws.scene), before);
    assert!(vec_at(&h, &["objects", "0", "center"]).x > 0.1);
    assert!(h.ed.ws.pt.running() || h.ed.ws.pt.progress().1);
}

#[test]
fn changes_on_disk_are_taken_in() {
    let mut h = Harness::new("hot", SPHERES);
    let path = h.dir.join("scene.toml");
    std::thread::sleep(std::time::Duration::from_millis(20));
    std::fs::write(&path, SPHERES.replace("position = [0, 1, 6]", "position = [0, 2, 6]")).unwrap();
    h.ed.watch.now();
    h.idle();
    assert!(h.text().contains("position = [0, 2, 6]"), "{}", h.text());
    assert!(!h.ed.ws.doc.is_dirty());
    assert_eq!(h.ed.ws.scene.camera.position, Vec3::new(0.0, 2.0, 6.0));
    // Our own save is not a change from elsewhere.
    h.ed.apply(0, &[Seg::from("camera"), Seg::from("fov")], &Value::Number(30.0), None, true);
    h.ed.run(Cmd::Save);
    h.ed.watch.now();
    h.idle();
    assert!(!h.ed.ui.modal_open());
    // Unsaved changes here and a change there: the editor asks.
    h.ed.apply(0, &[Seg::from("camera"), Seg::from("fov")], &Value::Number(35.0), None, true);
    std::thread::sleep(std::time::Duration::from_millis(20));
    std::fs::write(&path, SPHERES).unwrap();
    h.ed.watch.now();
    h.idle();
    h.idle();
    assert!(h.shows("Changed on Disk"), "a dialog asks");
}

#[test]
fn environment_rotation_shows_before_it_loads() {
    let mut h = Harness::new("envrot", LIT);
    h.ed.ws.shading = crate::render::Shading::Shaded;
    let gen0 = h.ed.ws.scene_gen;
    h.ed.apply(0, &[Seg::from("background"), Seg::from("rotate")], &Value::Number(90.0), Some(7), false);
    h.down = true;
    h.frame(Input { down: [true, false, false], ..Input::default() });
    assert_eq!(h.ed.ws.env_preview, Some(Vec3::new(0.0, 90.0, 0.0)));
    assert_eq!(h.ed.ws.scene_gen, gen0, "not loaded while dragging");
    h.down = false;
    h.idle();
    assert!(h.ed.ws.env_preview.is_none() && h.ed.ws.scene_gen > gen0);
}
