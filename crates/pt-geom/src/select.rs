//! What is selected in a mesh, and the ways of selecting more: edge loops and rings, face
//! loops, growing and shrinking, and everything linked.
//!
//! A selection holds vertices, edges and faces together and keeps them consistent the way
//! the current mode says: in vertex mode the vertices decide (an edge is selected when both
//! its ends are, a face when all its corners are), in edge mode the edges decide, in face
//! mode the faces.

use crate::mesh::{Mesh, edge_key};
use std::collections::HashSet;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Mode {
    #[default]
    Vertex,
    Edge,
    Face,
}

impl Mode {
    pub fn name(self) -> &'static str {
        match self {
            Mode::Vertex => "vertex",
            Mode::Edge => "edge",
            Mode::Face => "face",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Selection {
    pub verts: Vec<bool>,
    pub edges: HashSet<(u32, u32)>,
    pub faces: Vec<bool>,
}

impl Selection {
    pub fn new(m: &Mesh) -> Selection {
        Selection { verts: vec![false; m.vertex_count()], edges: HashSet::new(), faces: vec![false; m.face_count()] }
    }

    pub fn all(m: &Mesh) -> Selection {
        Selection {
            verts: vec![true; m.vertex_count()],
            edges: m.edges().into_iter().collect(),
            faces: vec![true; m.face_count()],
        }
    }

    pub fn is_empty(&self) -> bool {
        !self.verts.iter().any(|&s| s)
    }

    pub fn vert_list(&self) -> Vec<u32> {
        self.verts.iter().enumerate().filter(|(_, s)| **s).map(|(v, _)| v as u32).collect()
    }

    pub fn face_list(&self) -> Vec<u32> {
        self.faces.iter().enumerate().filter(|(_, s)| **s).map(|(f, _)| f as u32).collect()
    }

    pub fn edge_list(&self) -> Vec<(u32, u32)> {
        let mut e: Vec<(u32, u32)> = self.edges.iter().copied().collect();
        e.sort_unstable();
        e
    }

    pub fn has_vert(&self, v: u32) -> bool {
        self.verts.get(v as usize).copied().unwrap_or(false)
    }

    pub fn has_face(&self, f: u32) -> bool {
        self.faces.get(f as usize).copied().unwrap_or(false)
    }

    pub fn has_edge(&self, a: u32, b: u32) -> bool {
        self.edges.contains(&edge_key(a, b))
    }

    /// Selections of vertices only (the rest follows by `flush`).
    pub fn of_verts(m: &Mesh, verts: impl IntoIterator<Item = u32>) -> Selection {
        let mut s = Selection::new(m);
        for v in verts {
            if let Some(x) = s.verts.get_mut(v as usize) {
                *x = true;
            }
        }
        s.flush(m, Mode::Vertex);
        s
    }

    pub fn of_edges(m: &Mesh, edges: impl IntoIterator<Item = (u32, u32)>) -> Selection {
        let mut s = Selection::new(m);
        s.edges = edges.into_iter().map(|(a, b)| edge_key(a, b)).filter(|&(a, b)| m.has_edge(a, b)).collect();
        s.flush(m, Mode::Edge);
        s
    }

    pub fn of_faces(m: &Mesh, faces: impl IntoIterator<Item = u32>) -> Selection {
        let mut s = Selection::new(m);
        for f in faces {
            if let Some(x) = s.faces.get_mut(f as usize) {
                *x = true;
            }
        }
        s.flush(m, Mode::Face);
        s
    }

    /// Makes the other two element kinds follow the kind `mode` edits.
    pub fn flush(&mut self, m: &Mesh, mode: Mode) {
        self.verts.resize(m.vertex_count(), false);
        self.faces.resize(m.face_count(), false);
        match mode {
            Mode::Vertex => {
                self.edges = m.edges().into_iter().filter(|&(a, b)| self.has_vert(a) && self.has_vert(b)).collect();
                for f in 0..m.face_count() as u32 {
                    self.faces[f as usize] = m.face_verts(f).iter().all(|&v| self.has_vert(v));
                }
            }
            Mode::Edge => {
                self.edges.retain(|&(a, b)| m.has_edge(a, b));
                self.verts.iter_mut().for_each(|v| *v = false);
                for &(a, b) in &self.edges {
                    self.verts[a as usize] = true;
                    self.verts[b as usize] = true;
                }
                for f in 0..m.face_count() as u32 {
                    self.faces[f as usize] = m.face_edges(f).all(|e| self.edges.contains(&e));
                }
            }
            Mode::Face => {
                self.verts.iter_mut().for_each(|v| *v = false);
                self.edges.clear();
                for f in 0..m.face_count() as u32 {
                    if self.faces[f as usize] {
                        for &v in m.face_verts(f) {
                            self.verts[v as usize] = true;
                        }
                        self.edges.extend(m.face_edges(f));
                    }
                }
            }
        }
    }
}

/// The half-edge `a`→`b`, else `b`→`a` turned around (None: no such edge).
fn any_half_edge(m: &Mesh, a: u32, b: u32) -> Option<u32> {
    m.half_edge(a, b).or_else(|| m.half_edge(b, a))
}

/// Whether vertex `v` is surrounded by exactly four quads... or at least four edges with
/// faces on both sides: where an edge loop goes straight on.
fn loop_passes(m: &Mesh, v: u32) -> bool {
    let out = m.outgoing(v);
    out.len() == 4 && out.iter().all(|&h| m.twin(h).is_some())
}

/// The edge loop through edge `a`–`b`: on through vertices with four edges, taking the
/// edge opposite the one it came along; along the boundary for a boundary edge.
pub fn edge_loop(m: &Mesh, a: u32, b: u32) -> Vec<(u32, u32)> {
    let Some(h0) = any_half_edge(m, a, b) else { return Vec::new() };
    if m.twin(h0).is_none() {
        return boundary_loop(m, h0).into_iter().map(|h| edge_key(m.origin(h), m.dest(h))).collect();
    }
    let mut out = vec![edge_key(a, b)];
    let start = edge_key(a, b);
    for h in [h0, m.twin(h0).unwrap_or(h0)] {
        let mut h = h;
        for _ in 0..m.half_edge_count() {
            let v = m.dest(h);
            if !loop_passes(m, v) {
                break;
            }
            let Some(t) = m.twin(m.next(h)) else { break };
            h = m.next(t);
            let e = edge_key(m.origin(h), m.dest(h));
            if e == start || out.contains(&e) {
                return out;
            }
            out.push(e);
        }
    }
    out
}

/// The boundary half-edges in a row with `h` (a boundary half-edge), around the hole.
pub fn boundary_loop(m: &Mesh, h: u32) -> Vec<u32> {
    let mut out = vec![h];
    let mut cur = h;
    for _ in 0..m.half_edge_count() {
        // The next boundary half-edge leaves where this one ends.
        let v = m.dest(cur);
        let Some(&n) = m.outgoing(v).iter().find(|&&x| m.twin(x).is_none() && !out.contains(&x)) else { break };
        if n == h {
            break;
        }
        cur = n;
        out.push(cur);
    }
    out
}

/// The edge ring through edge `a`–`b` (across quads, from each edge to the opposite one),
/// and the faces it crosses.
pub fn edge_ring(m: &Mesh, a: u32, b: u32) -> (Vec<(u32, u32)>, Vec<u32>) {
    let Some(h0) = any_half_edge(m, a, b) else { return (Vec::new(), Vec::new()) };
    let start = edge_key(a, b);
    let mut edges = vec![start];
    let mut faces = Vec::new();
    let starts = [Some(h0), m.twin(h0)];
    for h in starts.into_iter().flatten() {
        let mut h = h;
        for _ in 0..=m.face_count() {
            let f = m.face(h);
            if m.face_verts(f).len() != 4 || faces.contains(&f) {
                break;
            }
            faces.push(f);
            let opp = m.next(m.next(h));
            let e = edge_key(m.origin(opp), m.dest(opp));
            if e == start {
                return (edges, faces);
            }
            edges.push(e);
            match m.twin(opp) {
                Some(t) => h = t,
                None => break,
            }
        }
    }
    (edges, faces)
}

/// Vertices one edge further out.
pub fn grow(m: &Mesh, s: &mut Selection, mode: Mode) {
    if mode == Mode::Face {
        let verts = s.verts.clone();
        for f in 0..m.face_count() as u32 {
            if m.face_verts(f).iter().any(|&v| verts[v as usize]) {
                s.faces[f as usize] = true;
            }
        }
        s.flush(m, Mode::Face);
        return;
    }
    let old = s.verts.clone();
    for (v, &on) in old.iter().enumerate() {
        if on {
            for n in m.neighbors(v as u32) {
                s.verts[n as usize] = true;
            }
        }
    }
    s.flush(m, Mode::Vertex);
}

/// The outermost selected vertices (or faces) let go.
pub fn shrink(m: &Mesh, s: &mut Selection, mode: Mode) {
    if mode == Mode::Face {
        // Vertices any unselected face touches, and the mesh's boundary.
        let mut touched: Vec<bool> = (0..m.vertex_count() as u32).map(|v| m.is_boundary_vertex(v)).collect();
        for f in 0..m.face_count() as u32 {
            if !s.faces[f as usize] {
                for &v in m.face_verts(f) {
                    touched[v as usize] = true;
                }
            }
        }
        for f in 0..m.face_count() as u32 {
            if m.face_verts(f).iter().any(|&v| touched[v as usize]) {
                s.faces[f as usize] = false;
            }
        }
        s.flush(m, Mode::Face);
        return;
    }
    let old = s.verts.clone();
    for (v, &on) in old.iter().enumerate() {
        if on && (m.neighbors(v as u32).iter().any(|&n| !old[n as usize]) || m.is_boundary_vertex(v as u32)) {
            s.verts[v] = false;
        }
    }
    s.flush(m, Mode::Vertex);
}

/// Every vertex connected to `seeds` by edges.
pub fn linked(m: &Mesh, seeds: &[u32]) -> Vec<u32> {
    let mut seen = vec![false; m.vertex_count()];
    let mut stack: Vec<u32> = seeds.iter().copied().filter(|&v| (v as usize) < seen.len()).collect();
    let mut out = Vec::new();
    while let Some(v) = stack.pop() {
        if std::mem::replace(&mut seen[v as usize], true) {
            continue;
        }
        out.push(v);
        stack.extend(m.neighbors(v).into_iter().filter(|&n| !seen[n as usize]));
    }
    out.sort_unstable();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::primitives;
    use pt_core::math::Vec3;

    /// A 4 x 4 grid of quads on the ground: vertex (i, j) is `i + 5 j`.
    pub fn grid() -> Mesh {
        primitives::grid(4, 4, 1.0)
    }

    #[test]
    fn loops_run_straight_through_the_grid() {
        let m = grid();
        // The horizontal interior edge between (1, 2) and (2, 2).
        let l = edge_loop(&m, 11, 12);
        assert_eq!(l.len(), 4, "{l:?}");
        assert!(l.iter().all(|&(a, b)| a / 5 == 2 && b / 5 == 2), "along row 2: {l:?}");
        // A boundary edge: the whole border.
        assert_eq!(edge_loop(&m, 0, 1).len(), 16);
        // The ring across it: the parallel edges of column 1, through the four faces there.
        let (ring, faces) = edge_ring(&m, 11, 12);
        assert_eq!((ring.len(), faces.len()), (5, 4), "{ring:?}");
        assert!(ring.iter().all(|&(a, b)| b == a + 1 && a % 5 == 1), "{ring:?}");
    }

    /// Around an open tube: the rim is a boundary loop, and the ring across the vertical
    /// edges closes.
    #[test]
    fn rings_and_loops_close_around_tubes() {
        let m = primitives::cylinder(Vec3::ZERO, Vec3::new(0.0, 1.0, 0.0), 1.0, 1.0, 8, false);
        let f = m.face_verts(0).to_vec();
        assert_eq!(edge_loop(&m, f[0], f[1]).len(), 8, "the rim");
        let (ring, faces) = edge_ring(&m, f[1], f[2]);
        assert_eq!((ring.len(), faces.len()), (8, 8));
    }

    #[test]
    fn grow_shrink_and_linked() {
        let m = grid();
        let mut s = Selection::of_verts(&m, [12]);
        grow(&m, &mut s, Mode::Vertex);
        assert_eq!(s.vert_list(), vec![7, 11, 12, 13, 17]);
        shrink(&m, &mut s, Mode::Vertex);
        assert_eq!(s.vert_list(), vec![12]);
        let mut f = Selection::of_faces(&m, [5]);
        grow(&m, &mut f, Mode::Face);
        assert_eq!(f.face_list().len(), 9);
        shrink(&m, &mut f, Mode::Face);
        assert_eq!(f.face_list(), vec![5]);
        assert_eq!(linked(&m, &[0]).len(), 25);
        // Flushing: two corners of a face are an edge, not the face.
        let s = Selection::of_verts(&m, [0, 1]);
        assert_eq!((s.edges.len(), s.face_list().len()), (1, 0));
        let s = Selection::of_edges(&m, [(0, 1), (1, 6), (6, 5), (5, 0)]);
        assert_eq!(s.face_list(), vec![0]);
    }
}
