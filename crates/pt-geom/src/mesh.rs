//! The polygon mesh with half-edge connectivity.
//!
//! Each face owns a contiguous run of half-edges, one per corner: half-edge `h` leaves
//! vertex `origin(h)` towards the next corner of its face. `twin(h)` is the half-edge going
//! the other way in the neighbouring face, `NONE` on the boundary (and where more than two
//! faces share an edge, or two faces share one the same way: such edges are left unpaired
//! and behave like boundaries). Every vertex lists its outgoing half-edges, so vertices
//! where the surface is not a disk (two cones touching at their tips) still work.
//!
//! Attributes: a texture coordinate per corner (when any face has them), a material group
//! per face (indexing `materials`, the names of a mesh file's groups), and a crease weight
//! per edge, kept by vertex pair.

use crate::triangulate::{ear_clip, newell};
use pt_core::geometry::Aabb;
use pt_core::math::Vec3;
use std::collections::{HashMap, HashSet};

/// No half-edge (a boundary twin), no vertex, no face.
pub const NONE: u32 = u32::MAX;

/// A face as a list of corners: what meshes are built from and what operations make.
#[derive(Clone, Debug, PartialEq)]
pub struct Polygon {
    pub verts: Vec<u32>,
    /// Texture coordinates per corner (the mesh has them when any polygon does).
    pub uvs: Option<Vec<[f32; 2]>>,
    pub material: u32,
}

impl Polygon {
    pub fn new(verts: Vec<u32>, material: u32) -> Polygon {
        Polygon { verts, uvs: None, material }
    }

    /// The same polygon turned the other way (its normal flipped).
    pub fn reversed(mut self) -> Polygon {
        self.verts.reverse();
        if let Some(u) = &mut self.uvs {
            u.reverse();
        }
        self
    }
}

/// An undirected edge as a sorted vertex pair.
pub fn edge_key(a: u32, b: u32) -> (u32, u32) {
    (a.min(b), a.max(b))
}

#[derive(Clone, Debug, Default)]
pub struct Mesh {
    pub positions: Vec<Vec3>,
    /// Half-edges of face `f`: `face_start[f]..face_start[f + 1]`.
    face_start: Vec<u32>,
    /// Origin vertex of each half-edge.
    corner: Vec<u32>,
    face_of: Vec<u32>,
    twin: Vec<u32>,
    /// Per half-edge (the corner it leaves), or empty.
    uv: Vec<[f32; 2]>,
    face_material: Vec<u32>,
    /// Material group names (`usemtl` of OBJ files); faces index them.
    pub materials: Vec<String>,
    /// Material libraries the mesh's groups come from (absolute paths).
    pub mtllibs: Vec<String>,
    creases: HashMap<(u32, u32), f32>,
    /// Outgoing half-edges of each vertex: `out_list[out_start[v]..out_start[v + 1]]`.
    out_start: Vec<u32>,
    out_list: Vec<u32>,
}

impl Mesh {
    /// The mesh of `polygons` over `positions`. Corners naming missing vertices, repeated
    /// corners in a row and polygons left with fewer than three corners are dropped.
    pub fn build(positions: Vec<Vec3>, polygons: Vec<Polygon>) -> Mesh {
        let nv = positions.len() as u32;
        let with_uv = polygons.iter().any(|p| p.uvs.is_some());
        let mut m = Mesh { positions, face_start: vec![0], ..Mesh::default() };
        for p in polygons {
            let uv = |i: usize| p.uvs.as_ref().and_then(|u| u.get(i).copied()).unwrap_or([0.0; 2]);
            let mut vs: Vec<(u32, [f32; 2])> =
                p.verts.iter().enumerate().filter(|&(_, &v)| v < nv).map(|(i, &v)| (v, uv(i))).collect();
            vs.dedup_by_key(|c| c.0);
            while vs.len() > 1 && vs[0].0 == vs[vs.len() - 1].0 {
                vs.pop();
            }
            if vs.len() < 3 {
                continue;
            }
            let f = m.face_material.len() as u32;
            for (v, t) in vs {
                m.corner.push(v);
                m.face_of.push(f);
                if with_uv {
                    m.uv.push(t);
                }
            }
            m.face_start.push(m.corner.len() as u32);
            m.face_material.push(p.material);
        }
        m.connect();
        m
    }

    /// Twins and the vertices' outgoing lists.
    fn connect(&mut self) {
        let n = self.corner.len();
        self.twin = vec![NONE; n];
        let mut first: HashMap<(u32, u32), u32> = HashMap::with_capacity(n);
        for h in 0..n as u32 {
            first.entry((self.origin(h), self.dest(h))).or_insert(h);
        }
        for h in 0..n as u32 {
            let (a, b) = (self.origin(h), self.dest(h));
            if self.twin[h as usize] != NONE || first[&(a, b)] != h {
                continue;
            }
            if let Some(&t) = first.get(&(b, a))
                && t != h
                && self.twin[t as usize] == NONE
            {
                self.twin[h as usize] = t;
                self.twin[t as usize] = h;
            }
        }
        let nv = self.positions.len();
        let mut count = vec![0u32; nv + 1];
        for &v in &self.corner {
            count[v as usize + 1] += 1;
        }
        for i in 0..nv {
            count[i + 1] += count[i];
        }
        let mut fill = count.clone();
        self.out_list = vec![0; n];
        for (h, &v) in self.corner.iter().enumerate() {
            self.out_list[fill[v as usize] as usize] = h as u32;
            fill[v as usize] += 1;
        }
        self.out_start = count;
        let creases = std::mem::take(&mut self.creases);
        self.creases = creases.into_iter().filter(|&((a, b), w)| w > 0.0 && self.has_edge(a, b)).collect();
    }

    /// The same attributes (materials, creases) over new geometry; creases stay on the
    /// vertex pairs that still are edges.
    pub fn rebuild(&self, positions: Vec<Vec3>, polygons: Vec<Polygon>) -> Mesh {
        let mut m = Mesh::build(positions, polygons);
        m.materials.clone_from(&self.materials);
        m.mtllibs.clone_from(&self.mtllibs);
        m.creases.clone_from(&self.creases);
        m.connect();
        m
    }

    // ------------------------------------------------------------------ queries

    pub fn vertex_count(&self) -> usize {
        self.positions.len()
    }

    pub fn face_count(&self) -> usize {
        self.face_material.len()
    }

    pub fn half_edge_count(&self) -> usize {
        self.corner.len()
    }

    pub fn has_uvs(&self) -> bool {
        !self.uv.is_empty()
    }

    /// The half-edges of face `f`.
    pub fn face_half_edges(&self, f: u32) -> std::ops::Range<u32> {
        self.face_start[f as usize]..self.face_start[f as usize + 1]
    }

    /// The corners of face `f`, in order.
    pub fn face_verts(&self, f: u32) -> &[u32] {
        let r = self.face_half_edges(f);
        &self.corner[r.start as usize..r.end as usize]
    }

    pub fn face_material(&self, f: u32) -> u32 {
        self.face_material[f as usize]
    }

    pub fn origin(&self, h: u32) -> u32 {
        self.corner[h as usize]
    }

    pub fn dest(&self, h: u32) -> u32 {
        self.corner[self.next(h) as usize]
    }

    pub fn face(&self, h: u32) -> u32 {
        self.face_of[h as usize]
    }

    pub fn next(&self, h: u32) -> u32 {
        let f = self.face_of[h as usize] as usize;
        if h + 1 == self.face_start[f + 1] { self.face_start[f] } else { h + 1 }
    }

    pub fn prev(&self, h: u32) -> u32 {
        let f = self.face_of[h as usize] as usize;
        if h == self.face_start[f] { self.face_start[f + 1] - 1 } else { h - 1 }
    }

    pub fn twin(&self, h: u32) -> Option<u32> {
        let t = self.twin[h as usize];
        (t != NONE).then_some(t)
    }

    /// Texture coordinates of the corner half-edge `h` leaves.
    pub fn uv(&self, h: u32) -> [f32; 2] {
        self.uv.get(h as usize).copied().unwrap_or([0.0; 2])
    }

    /// The half-edges leaving vertex `v`.
    pub fn outgoing(&self, v: u32) -> &[u32] {
        match (self.out_start.get(v as usize), self.out_start.get(v as usize + 1)) {
            (Some(&a), Some(&b)) => &self.out_list[a as usize..b as usize],
            _ => &[],
        }
    }

    /// The half-edge from `a` to `b`, if a face has one.
    pub fn half_edge(&self, a: u32, b: u32) -> Option<u32> {
        self.outgoing(a).iter().copied().find(|&h| self.dest(h) == b)
    }

    pub fn has_edge(&self, a: u32, b: u32) -> bool {
        self.half_edge(a, b).is_some() || self.half_edge(b, a).is_some()
    }

    /// Every half-edge along edge `a`–`b` (either way).
    pub fn edge_half_edges(&self, a: u32, b: u32) -> Vec<u32> {
        let out = |x: u32, y: u32| self.outgoing(x).iter().copied().filter(move |&h| self.dest(h) == y);
        out(a, b).chain(out(b, a)).collect()
    }

    /// Faces on edge `a`–`b`.
    pub fn edge_faces(&self, a: u32, b: u32) -> Vec<u32> {
        self.edge_half_edges(a, b).into_iter().map(|h| self.face(h)).collect()
    }

    /// An edge with one face.
    pub fn is_boundary_edge(&self, a: u32, b: u32) -> bool {
        self.edge_half_edges(a, b).len() == 1
    }

    /// Every edge once, as sorted vertex pairs, in the order of their first half-edge.
    pub fn edges(&self) -> Vec<(u32, u32)> {
        let mut seen = HashSet::with_capacity(self.corner.len());
        let mut out = Vec::with_capacity(self.corner.len() / 2 + 1);
        for h in 0..self.corner.len() as u32 {
            let k = edge_key(self.origin(h), self.dest(h));
            if seen.insert(k) {
                out.push(k);
            }
        }
        out
    }

    /// The edges of face `f`, as sorted pairs.
    pub fn face_edges(&self, f: u32) -> impl Iterator<Item = (u32, u32)> + '_ {
        self.face_half_edges(f).map(move |h| edge_key(self.origin(h), self.dest(h)))
    }

    /// The faces around vertex `v` (once each).
    pub fn vertex_faces(&self, v: u32) -> Vec<u32> {
        let mut f: Vec<u32> = self.outgoing(v).iter().map(|&h| self.face(h)).collect();
        f.sort_unstable();
        f.dedup();
        f
    }

    /// The vertices joined to `v` by an edge.
    pub fn neighbors(&self, v: u32) -> Vec<u32> {
        let mut n: Vec<u32> =
            self.outgoing(v).iter().flat_map(|&h| [self.dest(h), self.origin(self.prev(h))]).collect();
        n.sort_unstable();
        n.dedup();
        n
    }

    /// A vertex on the boundary (or where the surface is not closed around it).
    pub fn is_boundary_vertex(&self, v: u32) -> bool {
        self.outgoing(v).iter().any(|&h| self.twin(h).is_none() || self.twin(self.prev(h)).is_none())
    }

    pub fn face_points(&self, f: u32) -> Vec<Vec3> {
        self.face_verts(f).iter().map(|&v| self.positions[v as usize]).collect()
    }

    /// Unit normal of face `f` (zero for a degenerate face).
    pub fn face_normal(&self, f: u32) -> Vec3 {
        let n = newell(&self.face_points(f));
        if n.length_squared() > 0.0 { n.normalize() } else { Vec3::ZERO }
    }

    pub fn face_area(&self, f: u32) -> f32 {
        newell(&self.face_points(f)).length() * 0.5
    }

    pub fn face_center(&self, f: u32) -> Vec3 {
        let vs = self.face_verts(f);
        vs.iter().fold(Vec3::ZERO, |a, &v| a + self.positions[v as usize]) * (1.0 / vs.len().max(1) as f32)
    }

    /// The area-weighted mean normal of the faces around `v` (unit, or zero).
    pub fn vertex_normal(&self, v: u32) -> Vec3 {
        let n = self.vertex_faces(v).into_iter().fold(Vec3::ZERO, |a, f| a + newell(&self.face_points(f)));
        if n.length_squared() > 0.0 { n.normalize() } else { Vec3::ZERO }
    }

    pub fn bounds(&self) -> Aabb {
        let mut b = Aabb::EMPTY;
        for &p in &self.positions {
            b.grow_point(p);
        }
        b
    }

    // ------------------------------------------------------------------ attributes

    /// The crease weight of edge `a`–`b` (0: smooth).
    pub fn crease(&self, a: u32, b: u32) -> f32 {
        self.creases.get(&edge_key(a, b)).copied().unwrap_or(0.0)
    }

    pub fn set_crease(&mut self, a: u32, b: u32, w: f32) {
        if w > 0.0 && self.has_edge(a, b) {
            self.creases.insert(edge_key(a, b), w);
        } else {
            self.creases.remove(&edge_key(a, b));
        }
    }

    /// Every creased edge and its weight, sorted.
    pub fn creases(&self) -> Vec<((u32, u32), f32)> {
        let mut c: Vec<((u32, u32), f32)> = self.creases.iter().map(|(&k, &w)| (k, w)).collect();
        c.sort_by_key(|c| c.0);
        c
    }

    /// Creases moved to other vertex numbers (`map[old]`, `NONE`: gone).
    pub fn remap_creases(&mut self, map: &[u32]) {
        let old = std::mem::take(&mut self.creases);
        for ((a, b), w) in old {
            let (a, b) = (map.get(a as usize).copied().unwrap_or(NONE), map.get(b as usize).copied().unwrap_or(NONE));
            if a != NONE && b != NONE && a != b && self.has_edge(a, b) {
                self.creases.insert(edge_key(a, b), w);
            }
        }
    }

    // ------------------------------------------------------------------ polygons

    pub fn polygon(&self, f: u32) -> Polygon {
        let uvs = self.has_uvs().then(|| self.face_half_edges(f).map(|h| self.uv(h)).collect());
        Polygon { verts: self.face_verts(f).to_vec(), uvs, material: self.face_material(f) }
    }

    pub fn polygons(&self) -> Vec<Polygon> {
        (0..self.face_count() as u32).map(|f| self.polygon(f)).collect()
    }

    /// The mesh without the vertices no face uses; and where each old vertex went
    /// (`NONE`: removed).
    pub fn compact(&self) -> (Mesh, Vec<u32>) {
        let mut used = vec![false; self.vertex_count()];
        for &v in &self.corner {
            used[v as usize] = true;
        }
        let mut map = vec![NONE; self.vertex_count()];
        let mut positions = Vec::new();
        for (v, &u) in used.iter().enumerate() {
            if u {
                map[v] = positions.len() as u32;
                positions.push(self.positions[v]);
            }
        }
        let polys = self
            .polygons()
            .into_iter()
            .map(|mut p| {
                p.verts.iter_mut().for_each(|v| *v = map[*v as usize]);
                p
            })
            .collect();
        let mut m = self.rebuild(positions, polys);
        m.creases.clear();
        for ((a, b), w) in self.creases() {
            m.set_crease(map[a as usize], map[b as usize], w);
        }
        (m, map)
    }

    /// Triangles covering every face, as half-edge triples (their origins are the
    /// triangle's corners), with the face each comes from.
    pub fn triangles(&self) -> Vec<([u32; 3], u32)> {
        let mut out = Vec::with_capacity(self.corner.len());
        for f in 0..self.face_count() as u32 {
            let hs: Vec<u32> = self.face_half_edges(f).collect();
            if hs.len() == 3 {
                out.push(([hs[0], hs[1], hs[2]], f));
                continue;
            }
            for t in ear_clip(&self.face_points(f)) {
                out.push((t.map(|i| hs[i]), f));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two quads side by side: 0-1-2-3 and 1-4-5-2.
    pub fn two_quads() -> Mesh {
        let p = vec![
            Vec3::new(0.0, 0.0, 0.0),
            Vec3::new(1.0, 0.0, 0.0),
            Vec3::new(1.0, 1.0, 0.0),
            Vec3::new(0.0, 1.0, 0.0),
            Vec3::new(2.0, 0.0, 0.0),
            Vec3::new(2.0, 1.0, 0.0),
        ];
        Mesh::build(p, vec![Polygon::new(vec![0, 1, 2, 3], 0), Polygon::new(vec![1, 4, 5, 2], 0)])
    }

    #[test]
    fn half_edges_connect_faces() {
        let m = two_quads();
        assert_eq!((m.face_count(), m.half_edge_count(), m.edges().len()), (2, 8, 7));
        let h = m.half_edge(1, 2).unwrap();
        let t = m.twin(h).unwrap();
        assert_eq!((m.origin(t), m.dest(t), m.face(t)), (2, 1, 1));
        assert_eq!(m.dest(m.next(h)), 3);
        assert_eq!(m.origin(m.prev(h)), 0);
        assert!(m.is_boundary_edge(0, 1) && !m.is_boundary_edge(1, 2));
        assert_eq!(m.neighbors(1), vec![0, 2, 4]);
        assert_eq!(m.vertex_faces(2), vec![0, 1]);
        assert!(m.is_boundary_vertex(1));
        assert_eq!(m.face_normal(0), Vec3::Z);
        assert_eq!(m.triangles().len(), 4);
    }

    #[test]
    fn building_cleans_polygons_and_compacting_renumbers() {
        let p = vec![Vec3::ZERO, Vec3::new(1.0, 0.0, 0.0), Vec3::new(0.0, 1.0, 0.0), Vec3::new(5.0, 5.0, 5.0)];
        let polys = vec![Polygon::new(vec![0, 1, 1, 2, 0], 0), Polygon::new(vec![0, 9, 1], 0)];
        let mut m = Mesh::build(p, polys);
        assert_eq!(m.face_count(), 1, "the second polygon names a missing vertex");
        assert_eq!(m.face_verts(0), &[0, 1, 2]);
        m.set_crease(1, 2, 0.5);
        m.set_crease(0, 3, 1.0);
        assert_eq!(m.creases(), vec![((1, 2), 0.5)], "only edges take creases");
        let (c, map) = m.compact();
        assert_eq!((c.vertex_count(), map[3]), (3, NONE));
        assert_eq!(c.crease(2, 1), 0.5);
    }
}
