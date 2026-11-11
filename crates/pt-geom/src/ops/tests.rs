//! The operations on small meshes, checked by counts, closedness, the Euler
//! characteristic and positions.

use super::*;
use crate::primitives::{cube, cylinder, grid};

fn closed(m: &Mesh) -> bool {
    (0..m.half_edge_count() as u32).all(|h| m.twin(h).is_some())
}

/// V - E + F.
fn euler(m: &Mesh) -> i64 {
    m.vertex_count() as i64 - m.edges().len() as i64 + m.face_count() as i64
}

fn unit_cube() -> Mesh {
    cube(Vec3::ZERO, Vec3::ONE)
}

/// The cube's face facing +Y.
fn top(m: &Mesh) -> u32 {
    (0..m.face_count() as u32).find(|&f| m.face_normal(f).y > 0.9).unwrap()
}

#[test]
fn extruding_faces_keeps_the_surface_closed() {
    let m = unit_cube();
    let e = extrude_faces(&m, &[top(&m)]);
    let x = &e.done.mesh;
    assert_eq!((x.vertex_count(), x.face_count()), (12, 10));
    assert!(closed(x) && euler(x) == 2);
    assert_eq!(e.moved.len(), 4);
    assert!(e.normals.iter().all(|n| (*n - Vec3::new(0.0, 1.0, 0.0)).length() < 1e-5));
    let p = e.place(&x.positions, &e.normals, 1.0);
    assert!(e.moved.iter().all(|&v| (p[v as usize].y - 1.5).abs() < 1e-5));
    assert_eq!(e.done.sel.face_list(), vec![top(&m)]);
    // Insetting is the same topology, moved inwards in the face.
    let q = e.place(&x.positions, &e.inset, 0.1);
    assert!(
        e.moved.iter().all(|&v| (q[v as usize].x.abs() - 0.4).abs() < 1e-5 && (q[v as usize].y - 0.5).abs() < 1e-6)
    );
}

#[test]
fn extruding_a_border_edge_adds_a_quad() {
    let m = grid(2, 2, 1.0);
    let e = extrude_edges(&m, &[(0, 1)]);
    let x = &e.done.mesh;
    assert_eq!((x.vertex_count(), x.face_count()), (11, 5));
    // The new edge moves out of the grid, away from it (towards -Z).
    let p = e.place(&x.positions, &e.normals, 1.0);
    assert!(e.moved.iter().all(|&v| (p[v as usize].z + 1.0).abs() < 1e-5), "{p:?}");
    let n = x.face_normal(4);
    assert!((n - Vec3::new(0.0, 1.0, 0.0)).length() < 1e-5 || x.face_area(4) == 0.0);
    assert_eq!(e.done.sel.edges.len(), 1);
}

#[test]
fn bevels_chamfer_edges_and_corners() {
    let m = unit_cube();
    let one = bevel(&m, &[(0, 4)], &[], 0.1).unwrap();
    assert_eq!((one.mesh.vertex_count(), one.mesh.face_count()), (10, 7));
    assert!(closed(&one.mesh) && euler(&one.mesh) == 2);
    assert_eq!(one.sel.face_list().len(), 1);
    let all = bevel(&m, &m.edges(), &[], 0.1).unwrap();
    assert_eq!((all.mesh.vertex_count(), all.mesh.face_count()), (24, 26));
    assert!(closed(&all.mesh) && euler(&all.mesh) == 2);
    // The corner triangles sit 0.1 in from each face.
    let b = all.mesh.bounds();
    assert!((b.max.x - 0.5).abs() < 1e-5);
    assert!(all.mesh.positions.iter().all(|p| (p.x.abs() - 0.4).abs() < 1e-5 || (p.x.abs() - 0.5).abs() < 1e-5));
    let corner = bevel(&m, &[], &[7], 0.25).unwrap();
    assert_eq!((corner.mesh.vertex_count(), corner.mesh.face_count()), (10, 7));
    assert!(closed(&corner.mesh));
    assert!(bevel(&grid(1, 1, 1.0), &[(0, 1)], &[], 0.1).is_err(), "border edges do not bevel");
}

#[test]
fn loop_cuts_go_around() {
    let m = unit_cube();
    let c = loop_cut(&m, 0, 1, 1).unwrap();
    assert_eq!((c.mesh.vertex_count(), c.mesh.face_count()), (12, 10));
    assert!(closed(&c.mesh) && euler(&c.mesh) == 2);
    assert_eq!(c.sel.edges.len(), 4);
    // The cut is halfway along X.
    assert!(c.sel.vert_list().iter().all(|&v| c.mesh.positions[v as usize].x.abs() < 1e-6));
    let two = loop_cut(&m, 0, 1, 2).unwrap();
    assert_eq!((two.mesh.vertex_count(), two.mesh.face_count()), (16, 14));
    // On a grid the ring ends at the border.
    let g = grid(4, 4, 1.0);
    let c = loop_cut(&g, 11, 12, 1).unwrap();
    assert_eq!((c.mesh.vertex_count(), c.mesh.face_count()), (30, 20));
    assert_eq!(c.sel.edges.len(), 4);
}

#[test]
fn the_knife_cuts_along_a_plane_within_its_reach() {
    let g = grid(4, 4, 1.0);
    let d = cut(&g, None, &|p: Vec3| p.x - 1.5, &|_| true);
    assert_eq!((d.mesh.vertex_count(), d.mesh.face_count()), (30, 20));
    assert_eq!(d.sel.vert_list().len(), 5);
    // Only where the knife reached (z below 2): two faces cut, the third crossed edge
    // just gets its new vertex.
    let d = cut(&g, None, &|p: Vec3| p.x - 1.5, &|p| p.z <= 2.0);
    assert_eq!((d.mesh.vertex_count(), d.mesh.face_count()), (28, 18));
    // Through vertices: a diagonal through the corners of faces.
    let d = cut(&g, None, &|p: Vec3| p.x - p.z, &|_| true);
    assert_eq!(d.mesh.face_count(), 20);
    assert_eq!(d.mesh.vertex_count(), 25);
}

#[test]
fn bridging_joins_loops_and_face_regions() {
    // The cube's top and bottom inset, then bridged: a hole through it.
    let mut m = unit_cube();
    for up in [true, false] {
        let f =
            (0..m.face_count() as u32).find(|&f| (m.face_normal(f).y > 0.9) == up && m.face_normal(f).y.abs() > 0.9);
        let e = extrude_faces(&m, &[f.unwrap()]);
        let mut x = e.done.mesh.clone();
        x.positions = e.place(&x.positions, &e.inset, 0.2);
        m = x;
    }
    let ends: Vec<u32> = (0..m.face_count() as u32)
        .filter(|&f| {
            m.face_normal(f).y.abs() > 0.9 && m.face_center(f).x.abs() < 1e-5 && m.face_center(f).z.abs() < 1e-5
        })
        .collect();
    let t = bridge(&m, &[], &ends).unwrap();
    assert_eq!((t.mesh.vertex_count(), t.mesh.face_count()), (16, 16));
    assert!(closed(&t.mesh));
    assert_eq!(euler(&t.mesh), 0, "a hole through the cube");
    // Two squares, one facing down at y = 0 and one up at y = 1 (turned a little): their
    // borders bridged make a closed box.
    let mut p: Vec<Vec3> =
        [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)].iter().map(|&(x, z)| Vec3::new(x, 0.0, z)).collect();
    p.extend([(0.1, 0.0), (1.0, 0.1), (0.9, 1.0), (0.0, 0.9)].iter().map(|&(x, z)| Vec3::new(x, 1.0, z)));
    let squares = Mesh::build(p, vec![Polygon::new(vec![0, 1, 2, 3], 0), Polygon::new(vec![4, 7, 6, 5], 0)]);
    let rims = squares.edges();
    let b = bridge(&squares, &rims, &[]).unwrap();
    assert_eq!(b.mesh.face_count(), 6);
    assert!(closed(&b.mesh) && euler(&b.mesh) == 2);
    assert!((2..6).all(|f| b.mesh.face_normal(f).y.abs() < 0.2), "sides");
    assert!(bridge(&squares, &rims[..6], &[]).is_err(), "a loop and a chain");
    // An open tube's rims lie on its own sides: still joined, without faces in the way.
    let tube = cylinder(Vec3::ZERO, Vec3::new(0.0, 1.0, 0.0), 1.0, 1.0, 6, false);
    let rims: Vec<(u32, u32)> = tube.edges().into_iter().filter(|&(a, b)| tube.is_boundary_edge(a, b)).collect();
    assert_eq!(bridge(&tube, &rims, &[]).unwrap().mesh.face_count(), 12);
}

#[test]
fn merging_and_welding() {
    let m = unit_cube();
    let top_verts: Vec<u32> = m.face_verts(top(&m)).to_vec();
    let d = merge(&m, &top_verts, Vec3::new(0.0, 0.5, 0.0)).unwrap();
    assert_eq!((d.mesh.vertex_count(), d.mesh.face_count()), (5, 5));
    assert!(closed(&d.mesh));
    // Two quads apart, touching along an edge: welded into one surface.
    let p = vec![
        Vec3::new(0.0, 0.0, 0.0),
        Vec3::new(1.0, 0.0, 0.0),
        Vec3::new(1.0, 1.0, 0.0),
        Vec3::new(0.0, 1.0, 0.0),
        Vec3::new(1.0, 0.0, 0.0),
        Vec3::new(2.0, 0.0, 0.0),
        Vec3::new(2.0, 1.0, 0.0),
        Vec3::new(1.0, 1.0001, 0.0),
    ];
    let two = Mesh::build(p, vec![Polygon::new(vec![0, 1, 2, 3], 0), Polygon::new(vec![4, 5, 6, 7], 0)]);
    let (d, removed) = weld(&two, None, 0.001);
    assert_eq!((removed, d.mesh.vertex_count()), (2, 6));
    assert_eq!(d.mesh.edges().len(), 7);
}

#[test]
fn dissolving_joins_faces() {
    let g = grid(4, 4, 1.0);
    let d = dissolve_edges(&g, &[(1, 6)]);
    assert_eq!(d.mesh.face_count(), 15);
    assert_eq!(d.sel.face_list().len(), 1);
    assert_eq!(d.mesh.face_verts(d.sel.face_list()[0]).len(), 6);
    let all: Vec<u32> = (0..16).collect();
    let d = dissolve_faces(&g, &all);
    assert_eq!((d.mesh.face_count(), d.mesh.face_verts(0).len()), (1, 16));
    // The middle vertex: its four faces become one and it goes.
    let d = dissolve_verts(&g, &[12]);
    assert_eq!((d.mesh.face_count(), d.mesh.vertex_count()), (13, 24));
    // A vertex on the border between two edges in a row goes from its face.
    let d = dissolve_verts(&g, &[2]);
    assert_eq!(d.mesh.vertex_count(), 24);
}

#[test]
fn delete_then_fill_the_hole() {
    let m = unit_cube();
    let t = top(&m);
    let sel = Selection::of_faces(&m, [t]);
    let d = delete(&m, &sel, Mode::Face);
    assert_eq!((d.mesh.face_count(), d.mesh.vertex_count()), (5, 8));
    assert!(!closed(&d.mesh));
    let open = d.mesh;
    let e = open.edges().into_iter().find(|&(a, b)| open.is_boundary_edge(a, b)).unwrap();
    let f = fill(&open, &Selection::of_edges(&open, [e])).unwrap();
    assert!(closed(&f.mesh));
    let new = f.sel.face_list()[0];
    assert!(f.mesh.face_normal(new).y > 0.99, "the new face faces out");
    // Deleting a vertex takes its three faces.
    let d = delete(&m, &Selection::of_verts(&m, [0]), Mode::Vertex);
    assert_eq!((d.mesh.face_count(), d.mesh.vertex_count()), (3, 7));
}

#[test]
fn mirror_flip_and_triangulate() {
    let g = grid(2, 2, 1.0);
    let d = mirror(&g, 0, None, 1e-4);
    assert_eq!((d.mesh.vertex_count(), d.mesh.face_count()), (15, 8));
    assert!((0..8).all(|f| d.mesh.face_normal(f).y > 0.99), "mirrored faces still face up");
    // The halves are joined along x = 0.
    assert!(!d.mesh.is_boundary_edge(0, 3));
    let f = flip(&g, &[0]);
    assert!(f.mesh.face_normal(0).y < -0.99);
    let m = unit_cube();
    let all: Vec<u32> = (0..6).collect();
    let t = triangulate(&m, &all);
    assert_eq!(t.mesh.face_count(), 12);
    assert!(closed(&t.mesh));
    assert_eq!(t.sel.face_list().len(), 12);
}

/// Texture coordinates follow cuts.
#[test]
fn texture_coordinates_are_interpolated() {
    let g = grid(1, 1, 1.0);
    let d = cut(&g, None, &|p: Vec3| p.x - 0.25, &|_| true);
    let m = &d.mesh;
    for f in 0..m.face_count() as u32 {
        for h in m.face_half_edges(f) {
            let p = m.positions[m.origin(h) as usize];
            let uv = m.uv(h);
            assert!((uv[0] - p.x).abs() < 1e-5 && (uv[1] - p.z).abs() < 1e-5, "{p:?} {uv:?}");
        }
    }
}
