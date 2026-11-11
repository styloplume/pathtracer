//! Edit mode driven headless: entering it on shapes and mesh files, selecting by
//! clicking, rectangles and loops, moving vertices with the gizmo (proportionally too),
//! extruding, insetting and bevelling with the mouse, the loop cut, the knife, undo in edit
//! mode, and leaving it, which writes the mesh next to the scene.

use crate::editor::Cmd;
use crate::editor_tests::{H, Harness, NONE, W};
use crate::gizmo::Tool;
use crate::modeling::MeshCmd;
use crate::tools::ToolDrag;
use pt_core::math::Vec3;
use pt_core::platform::{Event, Key, Modifiers, MouseButton};
use pt_geom::Mode;

/// A box seen from the front and a little above, and a mesh file object.
const SCENE: &str = "\
[camera]
position = [2, 3, 6]
look_at = [0, 0, 0]
fov = 40

[[objects]]
type = \"box\"
name = \"block\"
center = [0, 0, 0]
size = 2
";

fn edit(h: &mut Harness) {
    h.ed.ws.sel.clear();
    h.ed.ws.sel.objects[0] = true;
    h.idle();
    h.key_event(Key::Tab, NONE);
    h.idle();
    // The depth the selection tests use.
    h.ed.render(W as usize, H as usize);
    assert!(h.ed.ws.edit.is_some(), "{}", h.ed.status);
}

fn mode(h: &mut Harness, m: Mode) {
    h.ed.run(Cmd::Mesh(MeshCmd::Mode(m)));
    h.idle();
}

/// The +Y face of the box being edited.
fn top_face(h: &Harness) -> u32 {
    let m = &h.ed.ws.edit.as_ref().unwrap().mesh;
    (0..m.face_count() as u32).find(|&f| m.face_normal(f).y > 0.9).unwrap()
}

fn mesh(h: &Harness) -> &pt_geom::Mesh {
    &h.ed.ws.edit.as_ref().unwrap().mesh
}

fn ctrl() -> Modifiers {
    Modifiers { ctrl: true, ..NONE }
}

#[test]
fn a_box_becomes_a_mesh_file_when_edited() {
    let mut h = Harness::new("edit-box", SCENE);
    edit(&mut h);
    assert_eq!((mesh(&h).vertex_count(), mesh(&h).face_count()), (8, 6));
    assert!(h.ed.ws.sel.is_empty(), "objects are not selected while editing");
    // Leaving without a change writes nothing.
    h.key_event(Key::Tab, NONE);
    h.idle();
    assert!(h.ed.ws.edit.is_none());
    assert!(!h.text().contains("file ="));
    assert!(h.ed.ws.sel.objects[0], "the object is selected again");

    // Click the top face in face mode, extrude it by typing 1, and leave.
    edit(&mut h);
    mode(&mut h, Mode::Face);
    let at = h.screen_of(Vec3::new(0.0, 1.0, 0.0));
    h.events(vec![Event::MouseMove { x: at.0, y: at.1, mods: NONE }]);
    h.events(vec![Event::MouseDown { button: MouseButton::Left, x: at.0, y: at.1, mods: NONE }]);
    h.events(vec![Event::MouseUp { button: MouseButton::Left, x: at.0, y: at.1, mods: NONE }]);
    let top = top_face(&h);
    assert_eq!(h.ed.ws.edit.as_ref().unwrap().sel.face_list(), vec![top], "{}", h.ed.status);
    h.key_event(Key::Char('e'), ctrl());
    assert!(matches!(h.ed.tool_drag, Some(ToolDrag::Modal(_))));
    h.key_event(Key::Char('1'), NONE);
    h.key_event(Key::Enter, NONE);
    assert!(h.ed.tool_drag.is_none());
    assert_eq!((mesh(&h).vertex_count(), mesh(&h).face_count()), (12, 10));
    let top_y = mesh(&h).positions.iter().map(|p| p.y).fold(f32::MIN, f32::max);
    assert!((top_y - 2.0).abs() < 1e-5, "extruded by 1: {top_y}");
    h.key_event(Key::Tab, NONE);
    h.idle();
    assert!(h.ed.ws.edit.is_none(), "{}", h.ed.status);
    let text = h.text();
    assert!(text.contains("type = \"mesh\"") && text.contains("file = \"block.ply\""), "{text}");
    assert!(!text.contains("size =") && !text.contains("center ="), "{text}");
    assert!(h.ed.ws.load_error.is_none(), "{:?}", h.ed.ws.load_error);
    assert!(h.dir.join("block.ply").is_file());
    let b = h.ed.ws.display.object_bounds[0];
    assert!((b.max.y - 2.0).abs() < 1e-4, "the scene loads the new mesh: {b:?}");
    // One undo step in the scene: back to the box.
    h.ed.run(Cmd::Undo);
    h.idle();
    assert_eq!(h.text(), SCENE);
    h.ed.run(Cmd::Redo);
    h.idle();

    // Editing the mesh file again writes another file, so undo keeps the old geometry.
    edit(&mut h);
    assert_eq!(mesh(&h).face_count(), 10, "read back from block.ply");
    mode(&mut h, Mode::Face);
    h.ed.run(Cmd::SelectAll);
    h.ed.run(Cmd::Mesh(MeshCmd::Triangulate));
    h.key_event(Key::Tab, NONE);
    h.idle();
    assert!(h.text().contains("file = \"block-2.ply\""), "{}", h.text());
    assert_eq!(pt_geom::io::load(&h.dir.join("block-2.ply")).unwrap().face_count(), 20);
}

/// Undo and redo inside edit mode, and leaving with everything undone writes nothing.
#[test]
fn edit_mode_has_its_own_undo() {
    let mut h = Harness::new("edit-undo", SCENE);
    edit(&mut h);
    h.ed.run(Cmd::SelectAll);
    h.ed.run(Cmd::Mesh(MeshCmd::Triangulate));
    assert_eq!(mesh(&h).face_count(), 12);
    h.key_event(Key::Char('z'), ctrl());
    assert_eq!(mesh(&h).face_count(), 6);
    h.key_event(Key::Char('y'), ctrl());
    assert_eq!(mesh(&h).face_count(), 12);
    h.key_event(Key::Char('z'), ctrl());
    h.key_event(Key::Tab, NONE);
    h.idle();
    assert_eq!(h.text(), SCENE, "nothing to write");
    assert!(h.ed.ws.doc.history().undo_label().is_none());
}

/// Clicks pick vertices in sight; a rectangle takes the visible ones only; double-clicks
/// take loops.
#[test]
fn clicks_and_rectangles_select_what_is_in_sight() {
    let mut h = Harness::new("edit-pick", SCENE);
    edit(&mut h);
    let e = h.ed.ws.edit.as_ref().unwrap();
    // The corner nearest the camera, and the one hidden behind the box.
    let near = (0..8u32).min_by(|&a, &b| {
        let d = |v: u32| (e.world(v) - Vec3::new(2.0, 3.0, 6.0)).length();
        d(a).total_cmp(&d(b))
    });
    let near = near.unwrap();
    let far = (0..8u32).find(|&v| (e.world(v) + e.world(near)).length() < 1e-5).unwrap();
    let p = h.screen_of(e.world(near));
    h.drag_events(MouseButton::Left, p, p, NONE);
    assert_eq!(h.ed.ws.edit.as_ref().unwrap().sel.vert_list(), vec![near]);
    // Everything in a rectangle around the box: the hidden corner is left out.
    h.ed.render(W as usize, H as usize);
    let corners: Vec<(i32, i32)> = (0..8).map(|v| h.screen_of(h.ed.ws.edit.as_ref().unwrap().world(v))).collect();
    let lo = (corners.iter().map(|c| c.0).min().unwrap() - 20, corners.iter().map(|c| c.1).min().unwrap() - 20);
    let hi = (corners.iter().map(|c| c.0).max().unwrap() + 20, corners.iter().map(|c| c.1).max().unwrap() + 20);
    h.drag_events(MouseButton::Left, lo, hi, NONE);
    let sel = h.ed.ws.edit.as_ref().unwrap().sel.vert_list();
    assert_eq!(sel.len(), 7, "{sel:?}");
    assert!(!sel.contains(&far));
    // Ctrl takes one away.
    h.drag_events(MouseButton::Left, p, p, ctrl());
    assert_eq!(h.ed.ws.edit.as_ref().unwrap().sel.vert_list().len(), 6);

    // Ctrl+double-click on an edge of the box: the ring of four edges around it (a loop
    // would stop at once: the box's corners have three edges).
    mode(&mut h, Mode::Edge);
    h.ed.run(Cmd::ClearSelection);
    // (The move gizmo would sit where the second click goes.)
    h.ed.run(Cmd::Tool(Tool::Select));
    h.ed.render(W as usize, H as usize);
    let e = h.ed.ws.edit.as_ref().unwrap();
    let (a, b) = e.mesh.edges().into_iter().find(|&(a, b)| (a == near) != (b == near)).unwrap();
    let mid = h.screen_of(e.world(a).lerp(e.world(b), 0.5));
    h.events(vec![Event::MouseMove { x: mid.0, y: mid.1, mods: ctrl() }]);
    for _ in 0..2 {
        h.events(vec![Event::MouseDown { button: MouseButton::Left, x: mid.0, y: mid.1, mods: ctrl() }]);
        h.events(vec![Event::MouseUp { button: MouseButton::Left, x: mid.0, y: mid.1, mods: ctrl() }]);
    }
    let e = h.ed.ws.edit.as_ref().unwrap();
    let ring = e.sel.edge_list();
    assert_eq!(ring.len(), 4, "{ring:?}");
    let d = |(a, b): (u32, u32)| e.mesh.positions[b as usize] - e.mesh.positions[a as usize];
    assert!(ring.iter().all(|&x| d(x).cross(d((a, b))).length() < 1e-5), "parallel edges");
}

/// The move gizmo moves the selected vertices; with proportional editing the neighbours
/// follow less.
#[test]
fn gizmos_move_vertices_proportionally() {
    let scene = "[camera]\nposition = [0, 6, 6]\nlook_at = [2, 0, 2]\n\n[[objects]]\ntype = \"quad\"\n\
                 origin = [0, 0, 0]\nu = [4, 0, 0]\nv = [0, 0, 4]\n";
    let mut h = Harness::new("edit-move", scene);
    edit(&mut h);
    // Cut the quad into a grid: two loop cuts each way.
    let e = h.ed.ws.edit.as_ref().unwrap();
    assert_eq!(e.mesh.face_count(), 1);
    h.ed.ws.edit.as_mut().unwrap().mesh = pt_geom::primitives::grid(4, 4, 1.0);
    h.ed.ws.edit.as_mut().unwrap().sel = pt_geom::Selection::new(mesh(&h));
    h.ed.edit_changed();
    h.ed.ws.edit.as_mut().unwrap().sel = pt_geom::Selection::of_verts(mesh(&h), [12]);
    h.ed.edit_changed();
    h.ed.run(Cmd::Tool(Tool::Move));
    h.idle();
    let g = h.ed.ws.decor.gizmo.expect("a gizmo on the vertex");
    assert!((g.pivot - Vec3::new(2.0, 0.0, 2.0)).length() < 1e-5);
    // Up the Y arrow by typing 1.
    let arm = h.screen_of(g.pivot + Vec3::new(0.0, 1.0, 0.0) * (crate::gizmo::size(&proj(&h), g.pivot, 1.0) * 0.7));
    h.events(vec![Event::MouseMove { x: arm.0, y: arm.1, mods: NONE }]);
    h.events(vec![Event::MouseDown { button: MouseButton::Left, x: arm.0, y: arm.1, mods: NONE }]);
    h.events(vec![Event::MouseMove { x: arm.0, y: arm.1 - 20, mods: NONE }]);
    h.key_event(Key::Char('1'), NONE);
    h.key_event(Key::Enter, NONE);
    h.events(vec![Event::MouseUp { button: MouseButton::Left, x: arm.0, y: arm.1 - 20, mods: NONE }]);
    let m = mesh(&h);
    assert!((m.positions[12].y - 1.0).abs() < 1e-5, "{:?}", m.positions[12]);
    assert_eq!(m.positions[11].y, 0.0, "only the selected vertex");
    h.key_event(Key::Char('z'), ctrl());
    assert_eq!(mesh(&h).positions[12].y, 0.0);

    // Proportionally, within 1.5: the neighbours (1 away) rise less, the corners not.
    h.ed.ws.edit.as_mut().unwrap().radius = 1.5;
    h.key_event(Key::Char('o'), Modifiers { shift: true, ..NONE });
    assert!(h.ed.ws.edit.as_ref().unwrap().proportional);
    h.events(vec![Event::MouseMove { x: arm.0, y: arm.1, mods: NONE }]);
    h.events(vec![Event::MouseDown { button: MouseButton::Left, x: arm.0, y: arm.1, mods: NONE }]);
    h.events(vec![Event::MouseMove { x: arm.0, y: arm.1 - 20, mods: NONE }]);
    h.key_event(Key::Char('1'), NONE);
    h.key_event(Key::Enter, NONE);
    let m = mesh(&h);
    let (y12, y11, y0) = (m.positions[12].y, m.positions[11].y, m.positions[0].y);
    assert!((y12 - 1.0).abs() < 1e-5 && y11 > 0.05 && y11 < 0.5 && y0 == 0.0, "{y12} {y11} {y0}");
}

/// Vertex snapping lands on the mesh's own other vertices; the loop command follows
/// straight rows of the grid.
#[test]
fn snapping_and_loops_in_edit_mode() {
    let scene = "[camera]\nposition = [2, 7, 5]\nlook_at = [2, 0, 2]\n\n[[objects]]\ntype = \"quad\"\n\
                 origin = [0, 0, 0]\nu = [4, 0, 0]\nv = [0, 0, 4]\n";
    let mut h = Harness::new("edit-snap", scene);
    edit(&mut h);
    let e = h.ed.ws.edit.as_mut().unwrap();
    e.mesh = pt_geom::primitives::grid(4, 4, 1.0);
    e.sel = pt_geom::Selection::of_edges(&e.mesh, [(11, 12)]);
    h.ed.run(Cmd::Mesh(MeshCmd::SelectLoop));
    assert_eq!(h.ed.ws.edit.as_ref().unwrap().sel.edges.len(), 4);

    let e = h.ed.ws.edit.as_mut().unwrap();
    e.sel = pt_geom::Selection::of_verts(&e.mesh, [12]);
    h.ed.edit_changed();
    h.ed.run(Cmd::Snap(crate::tools::Snap::Vertex));
    h.ed.run(Cmd::Tool(Tool::Move));
    h.idle();
    let g = h.ed.ws.decor.gizmo.expect("a gizmo");
    let arm = h.screen_of(g.pivot + Vec3::new(1.0, 0.0, 0.0) * (crate::gizmo::size(&proj(&h), g.pivot, 1.0) * 0.7));
    let target = h.screen_of(Vec3::new(3.0, 0.0, 2.0));
    h.drag_events(MouseButton::Left, arm, (target.0 + 3, target.1 + 2), NONE);
    let p = mesh(&h).positions[12];
    assert!((p - Vec3::new(3.0, 0.0, 2.0)).length() < 1e-5, "snapped onto vertex 13: {p:?}");
}

fn proj(h: &Harness) -> crate::view::Proj {
    let vp = h.ed.vp;
    let r = h.ed.ws.rects(vp.w as usize, vp.h as usize)[0];
    h.ed.ws.proj(&r)
}

/// Inset and bevel follow the mouse away from the selection; the loop cut and the knife
/// cut where they point.
#[test]
fn modal_operations_loop_cut_and_knife() {
    let mut h = Harness::new("edit-ops", SCENE);
    edit(&mut h);
    mode(&mut h, Mode::Face);
    let top = top_face(&h);
    h.ed.ws.edit.as_mut().unwrap().sel = pt_geom::Selection::of_faces(mesh(&h), [top]);
    h.ed.edit_changed();
    // Inset: moving the mouse away from the face's middle insets it.
    let c = h.screen_of(Vec3::new(0.0, 1.0, 0.0));
    h.events(vec![Event::MouseMove { x: c.0 + 10, y: c.1, mods: NONE }]);
    h.key_event(Key::Char('i'), NONE);
    h.events(vec![Event::MouseMove { x: c.0 + 60, y: c.1, mods: NONE }]);
    h.events(vec![Event::MouseDown { button: MouseButton::Left, x: c.0 + 60, y: c.1, mods: NONE }]);
    h.events(vec![Event::MouseUp { button: MouseButton::Left, x: c.0 + 60, y: c.1, mods: NONE }]);
    let m = mesh(&h);
    assert_eq!(m.face_count(), 10);
    let inner = m.face_points(top);
    assert!(inner.iter().all(|p| p.x.abs() < 0.99 && p.x.abs() > 0.1 && (p.y - 1.0).abs() < 1e-5), "{inner:?}");
    // Esc cancels an operation entirely.
    h.key_event(Key::Char('e'), ctrl());
    h.events(vec![Event::MouseMove { x: c.0, y: c.1 - 50, mods: NONE }]);
    h.key_event(Key::Escape, NONE);
    assert_eq!(mesh(&h).face_count(), 10);

    // Bevel every edge of a fresh box by typing the width.
    h.key_event(Key::Char('z'), ctrl());
    assert_eq!(mesh(&h).face_count(), 6);
    mode(&mut h, Mode::Edge);
    h.ed.run(Cmd::SelectAll);
    h.key_event(Key::Char('b'), ctrl());
    for k in ['0', '.', '2'] {
        h.key_event(Key::Char(k), NONE);
    }
    h.key_event(Key::Enter, NONE);
    assert_eq!((mesh(&h).vertex_count(), mesh(&h).face_count()), (24, 26));
    let b = mesh(&h).bounds();
    assert!((b.max.x - 1.0).abs() < 1e-5);
    h.key_event(Key::Char('z'), ctrl());

    // Loop cut: point at an edge of the top face, two cuts with the wheel, click.
    h.ed.render(W as usize, H as usize);
    h.key_event(Key::Char('r'), ctrl());
    let e = h.ed.ws.edit.as_ref().unwrap();
    let (a, b) = e.mesh.face_edges(top).next().unwrap();
    let mid = h.screen_of(e.world(a).lerp(e.world(b), 0.5));
    h.events(vec![Event::MouseMove { x: mid.0, y: mid.1, mods: NONE }]);
    assert!(matches!(&h.ed.tool_drag, Some(ToolDrag::LoopCut(l)) if l.edge.is_some()));
    h.events(vec![Event::Wheel { delta: 1.0, mods: NONE }]);
    assert!(!h.ed.ws.decor.lines.is_empty(), "the cuts are previewed");
    h.events(vec![Event::MouseDown { button: MouseButton::Left, x: mid.0, y: mid.1, mods: NONE }]);
    h.events(vec![Event::MouseUp { button: MouseButton::Left, x: mid.0, y: mid.1, mods: NONE }]);
    assert!(h.ed.tool_drag.is_none());
    assert_eq!((mesh(&h).vertex_count(), mesh(&h).face_count()), (16, 14), "{}", h.ed.status);
    assert_eq!(h.ed.ws.edit.as_ref().unwrap().sel.edges.len(), 8, "the new loops are selected");

    // The knife across the whole view, left to right through the middle.
    h.key_event(Key::Char('z'), ctrl());
    h.ed.run(Cmd::ClearSelection);
    h.key_event(Key::Char('k'), NONE);
    let vp = h.ed.vp;
    let y = h.screen_of(Vec3::new(0.0, 0.0, 1.0)).1;
    h.drag_events(MouseButton::Left, (vp.x + 5, y), (vp.x + vp.w - 5, y), NONE);
    let m = mesh(&h);
    assert!(m.face_count() > 6 && m.vertex_count() > 8, "{} faces: {}", m.face_count(), h.ed.status);
    assert!(h.ed.tool_drag.is_none());
}

/// Objects that are not meshes say so; edit mode keeps objects' commands away.
#[test]
fn what_cannot_be_edited() {
    let scene = "[[objects]]\ntype = \"sdf\"\nsdf = { type = \"sphere\", radius = 1 }\n\n\
                 [[objects]]\ntype = \"sphere\"\nradius = 0.5\ntranslate = [3, 0, 0]\nscale = 2\n";
    let mut h = Harness::new("edit-not", scene);
    h.ed.ws.sel.objects[0] = true;
    h.key_event(Key::Tab, NONE);
    assert!(h.ed.ws.edit.is_none());
    assert!(h.ed.status.contains("not meshes"), "{}", h.ed.status);
    // A scaled sphere: the mesh is in the sphere's own space.
    h.ed.ws.sel.clear();
    h.ed.ws.sel.objects[1] = true;
    h.key_event(Key::Tab, NONE);
    let e = h.ed.ws.edit.as_ref().expect("a sphere is edited as a mesh");
    assert!((e.mesh.bounds().max.y - 0.5).abs() < 1e-5);
    assert!((e.bounds(false).max.y - 1.0).abs() < 1e-4, "in the world it is scaled");
    h.key_event(Key::Char('d'), ctrl());
    assert!(h.ed.status.contains("Tab leaves edit mode"), "{}", h.ed.status);
    // Saving leaves edit mode (writing nothing: nothing changed).
    h.key_event(Key::Char('s'), ctrl());
    assert!(h.ed.ws.edit.is_none());
}
