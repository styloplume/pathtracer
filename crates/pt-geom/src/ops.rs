//! Modeling operations. Each reads a mesh and returns a new one with what should be
//! selected afterwards; none changes its input.
//!
//! - extrude: faces as a region (side quads along the region's border; the region's
//!   vertices then move along their normals or an axis), or edges (a quad from each);
//! - inset: the same topology as extruding faces, the border moved into the faces;
//! - bevel: edges or vertices chamfered with one segment (width measured along the
//!   edges at the ends, across the faces where two bevelled edges meet), with a polygon
//!   capping each corner where three or more meet;
//! - loop cut: edges across a ring of quads, `n` at a time;
//! - cut (the knife): faces cut along a plane, where a test on the cut points allows;
//! - bridge: two edge loops or chains (or the borders of two face regions) joined by quads;
//! - merge (at a point), weld (by distance), dissolve (vertices, edges, faces), delete,
//!   fill (holes), mirror, flip, triangulate.
//!
//! Texture coordinates follow the corners they come from (interpolated along cut edges);
//! new faces take the material of the face they grow from.

use crate::mesh::{Mesh, NONE, Polygon, edge_key};
use crate::select::{Mode, Selection, boundary_loop, edge_ring};
use crate::triangulate::{ear_clip, newell};
use pt_core::math::Vec3;
use std::collections::{HashMap, HashSet};

/// The result of an operation: the new mesh and what to select in it.
#[derive(Clone, Debug)]
pub struct Done {
    pub mesh: Mesh,
    pub sel: Selection,
}

fn lerp2(a: [f32; 2], b: [f32; 2], t: f32) -> [f32; 2] {
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t]
}

fn unit(v: Vec3) -> Vec3 {
    if v.length_squared() > 0.0 && v.is_finite() { v.normalize() } else { Vec3::ZERO }
}

/// Builds a polygon from corners (vertex, texture coordinates).
fn poly(corners: &[(u32, [f32; 2])], uvs: bool, material: u32) -> Polygon {
    Polygon {
        verts: corners.iter().map(|c| c.0).collect(),
        uvs: uvs.then(|| corners.iter().map(|c| c.1).collect()),
        material,
    }
}

/// Corners of face `f`: (vertex, texture coordinates).
fn corners(m: &Mesh, f: u32) -> Vec<(u32, [f32; 2])> {
    m.face_half_edges(f).map(|h| (m.origin(h), m.uv(h))).collect()
}

// ---------------------------------------------------------------------- extrude, inset

/// A region extruded: the mesh, its selection, and the vertices that move with how.
#[derive(Clone, Debug)]
pub struct Extrusion {
    pub done: Done,
    /// The vertices that move (the region's, after the operation).
    pub moved: Vec<u32>,
    /// For each moved vertex: its direction "along the normal" (unit, or zero).
    pub normals: Vec<Vec3>,
    /// For each moved vertex: where it goes for an inset of thickness 1 (zero inside the
    /// region).
    pub inset: Vec<Vec3>,
}

impl Extrusion {
    /// Positions with the moved vertices at `base + dir * amount`.
    pub fn place(&self, base: &[Vec3], dirs: &[Vec3], amount: f32) -> Vec<Vec3> {
        let mut p = base.to_vec();
        for (&v, &d) in self.moved.iter().zip(dirs) {
            p[v as usize] = base[v as usize] + d * amount;
        }
        p
    }
}

/// Extrudes whatever the selection holds: faces as a region, else edges.
pub fn extrude(m: &Mesh, sel: &Selection) -> Result<Extrusion, String> {
    let faces = sel.face_list();
    if !faces.is_empty() {
        return Ok(extrude_faces(m, &faces));
    }
    let edges = sel.edge_list();
    if edges.is_empty() {
        return Err("select faces or edges to extrude".into());
    }
    Ok(extrude_edges(m, &edges))
}

/// The faces as one region pulled out of the mesh: the vertices on its border are doubled
/// (the region keeps the new ones) and quads join the two along the border.
pub fn extrude_faces(m: &Mesh, faces: &[u32]) -> Extrusion {
    let mut region = vec![false; m.face_count()];
    for &f in faces {
        region[f as usize] = true;
    }
    let border: Vec<u32> = faces
        .iter()
        .flat_map(|&f| m.face_half_edges(f))
        .filter(|&h| m.twin(h).is_none_or(|t| !region[m.face(t) as usize]))
        .collect();
    let mut positions = m.positions.clone();
    let mut new_of: HashMap<u32, u32> = HashMap::new();
    for &h in &border {
        for v in [m.origin(h), m.dest(h)] {
            new_of.entry(v).or_insert_with(|| {
                positions.push(m.positions[v as usize]);
                positions.len() as u32 - 1
            });
        }
    }
    let uvs = m.has_uvs();
    let mut polys = m.polygons();
    for &f in faces {
        for v in &mut polys[f as usize].verts {
            *v = new_of.get(v).copied().unwrap_or(*v);
        }
    }
    for &h in &border {
        let (a, b) = (m.origin(h), m.dest(h));
        let (ua, ub) = (m.uv(h), m.uv(m.next(h)));
        let side = [(a, ua), (b, ub), (new_of[&b], ub), (new_of[&a], ua)];
        polys.push(poly(&side, uvs, m.face_material(m.face(h))));
    }
    let mesh = m.rebuild(positions, polys);
    let mut moved: Vec<u32> = faces.iter().flat_map(|&f| mesh.face_verts(f).to_vec()).collect();
    moved.sort_unstable();
    moved.dedup();
    // Normals: the region's faces around each vertex. Inset: into the faces, from the border.
    let mut normal: HashMap<u32, Vec3> = HashMap::new();
    for &f in faces {
        let n = newell(&mesh.face_points(f));
        for &v in mesh.face_verts(f) {
            *normal.entry(v).or_insert(Vec3::ZERO) += n;
        }
    }
    let mut inward: HashMap<u32, Vec<Vec3>> = HashMap::new();
    for &h in &border {
        let f = m.face(h);
        let (a, b) = (new_of[&m.origin(h)], new_of[&m.dest(h)]);
        let n = unit(newell(&mesh.face_points(f)));
        let d = unit(n.cross(mesh.positions[b as usize] - mesh.positions[a as usize]));
        inward.entry(a).or_default().push(d);
        inward.entry(b).or_default().push(d);
    }
    let inset = moved
        .iter()
        .map(|v| match inward.get(v) {
            None => Vec3::ZERO,
            Some(ds) => {
                let dir = unit(ds.iter().fold(Vec3::ZERO, |a, &d| a + d));
                let along = ds.iter().map(|&d| dir.dot(d)).fold(1.0f32, f32::min).max(0.25);
                dir * (1.0 / along)
            }
        })
        .collect();
    let normals = moved.iter().map(|v| unit(normal.get(v).copied().unwrap_or(Vec3::ZERO))).collect();
    let sel = Selection::of_faces(&mesh, faces.iter().copied());
    Extrusion { done: Done { mesh, sel }, moved, normals, inset }
}

/// A quad grown from each edge (sharing new vertices where the edges meet); the new
/// vertices move outwards in the plane of the faces the edges border.
pub fn extrude_edges(m: &Mesh, edges: &[(u32, u32)]) -> Extrusion {
    let mut positions = m.positions.clone();
    let mut new_of: HashMap<u32, u32> = HashMap::new();
    let mut out: HashMap<u32, Vec3> = HashMap::new();
    let uvs = m.has_uvs();
    let mut polys = m.polygons();
    let mut new_edges = Vec::new();
    for &(a, b) in edges {
        let Some(h) = m.half_edge(a, b).or_else(|| m.half_edge(b, a)) else { continue };
        let (a, b) = (m.origin(h), m.dest(h));
        let mut fresh = |v: u32| {
            *new_of.entry(v).or_insert_with(|| {
                positions.push(m.positions[v as usize]);
                positions.len() as u32 - 1
            })
        };
        let (na, nb) = (fresh(a), fresh(b));
        let (ua, ub) = (m.uv(h), m.uv(m.next(h)));
        polys.push(poly(&[(b, ub), (a, ua), (na, ua), (nb, ub)], uvs, m.face_material(m.face(h))));
        new_edges.push((na, nb));
        let n = m.face_normal(m.face(h));
        let o = unit((m.positions[b as usize] - m.positions[a as usize]).cross(n));
        *out.entry(na).or_insert(Vec3::ZERO) += o;
        *out.entry(nb).or_insert(Vec3::ZERO) += o;
    }
    let mesh = m.rebuild(positions, polys);
    let mut moved: Vec<u32> = new_of.values().copied().collect();
    moved.sort_unstable();
    let normals = moved.iter().map(|v| unit(out[v])).collect();
    let inset = vec![Vec3::ZERO; moved.len()];
    let sel = Selection::of_edges(&mesh, new_edges);
    Extrusion { done: Done { mesh, sel }, moved, normals, inset }
}

// ---------------------------------------------------------------------- bevel

/// Bevels `edges` (those with a face on each side) and `verts` by `width`, one segment.
pub fn bevel(m: &Mesh, edges: &[(u32, u32)], verts: &[u32], width: f32) -> Result<Done, String> {
    let bevelled: HashSet<(u32, u32)> = edges
        .iter()
        .map(|&(a, b)| edge_key(a, b))
        .filter(|&(a, b)| {
            let hs = m.edge_half_edges(a, b);
            hs.len() == 2 && m.twin(hs[0]) == Some(hs[1])
        })
        .collect();
    let mut active = vec![false; m.vertex_count()];
    for &(a, b) in &bevelled {
        active[a as usize] = true;
        active[b as usize] = true;
    }
    for &v in verts {
        if let Some(x) = active.get_mut(v as usize) {
            *x = true;
        }
    }
    if !active.iter().any(|&a| a) {
        return Err("select edges between two faces, or vertices, to bevel".into());
    }
    let width = width.max(0.0);
    let uvs = m.has_uvs();
    let p = |v: u32| m.positions[v as usize];
    let mut positions = m.positions.clone();
    let mut slid: HashMap<(u32, u32), u32> = HashMap::new();
    // Corner replacements, by the half-edge leaving the corner.
    let mut repl: Vec<Vec<(u32, [f32; 2])>> = vec![Vec::new(); m.half_edge_count()];
    for h in 0..m.half_edge_count() as u32 {
        let v = m.origin(h);
        if !active[v as usize] {
            repl[h as usize] = vec![(v, m.uv(h))];
            continue;
        }
        let (hp, hn) = (m.prev(h), m.next(h));
        let (u, w) = (m.origin(hp), m.dest(h));
        let (b_in, b_out) = (bevelled.contains(&edge_key(u, v)), bevelled.contains(&edge_key(v, w)));
        // A point slid from `v` towards `x` (shared by the faces on that edge).
        let mut slide = |positions: &mut Vec<Vec3>, x: u32, uv_x: [f32; 2]| {
            let len = (p(x) - p(v)).length();
            let t = if len > 0.0 { width.min(0.45 * len) / len } else { 0.0 };
            let i = *slid.entry((v, x)).or_insert_with(|| {
                positions.push(p(v).lerp(p(x), t));
                positions.len() as u32 - 1
            });
            (i, lerp2(m.uv(h), uv_x, t))
        };
        repl[h as usize] = match (b_in, b_out) {
            (true, true) => {
                // Width away from both edges, in the face.
                let (du, dw) = (unit(p(u) - p(v)), unit(p(w) - p(v)));
                let s = du.cross(dw).length();
                let reach = 0.45 * (p(u) - p(v)).length().min((p(w) - p(v)).length());
                let a = if s > 0.05 { width / s } else { width }.min(reach);
                positions.push(p(v) + (du + dw) * a);
                vec![(positions.len() as u32 - 1, m.uv(h))]
            }
            (true, false) => vec![slide(&mut positions, w, m.uv(hn))],
            (false, true) => vec![slide(&mut positions, u, m.uv(hp))],
            (false, false) => vec![slide(&mut positions, u, m.uv(hp)), slide(&mut positions, w, m.uv(hn))],
        };
    }
    let mut polys: Vec<Polygon> = (0..m.face_count() as u32)
        .map(|f| {
            let c: Vec<(u32, [f32; 2])> = m.face_half_edges(f).flat_map(|h| repl[h as usize].clone()).collect();
            poly(&c, uvs, m.face_material(f))
        })
        .collect();
    let first = polys.len();
    for &(a, b) in &bevelled {
        let Some(h1) = m.half_edge(a, b) else { continue };
        let Some(h2) = m.twin(h1) else { continue };
        // Each face's replacement corners next to the edge, at both ends.
        let first = |h: u32| repl[h as usize][0];
        let last = |h: u32| repl[h as usize][repl[h as usize].len() - 1];
        let quad = [first(m.next(h1)), last(h1), first(m.next(h2)), last(h2)];
        polys.push(poly(&quad, uvs, m.face_material(m.face(h1))));
    }
    // Caps: around each active vertex the surface closes around.
    for v in 0..m.vertex_count() as u32 {
        if !active[v as usize] || m.is_boundary_vertex(v) {
            continue;
        }
        let Some(&h0) = m.outgoing(v).first() else { continue };
        let mut ring: Vec<(u32, [f32; 2])> = Vec::new();
        let mut h = h0;
        let mut closed = false;
        for _ in 0..=m.outgoing(v).len() {
            ring.extend(repl[h as usize].iter().copied());
            let Some(t) = m.twin(h) else { break };
            h = m.next(t);
            if h == h0 {
                closed = true;
                break;
            }
        }
        ring.dedup_by_key(|c| c.0);
        while ring.len() > 1 && ring[0].0 == ring[ring.len() - 1].0 {
            ring.pop();
        }
        if closed && ring.len() >= 3 {
            ring.reverse();
            polys.push(poly(&ring, uvs, m.face_material(m.face(h0))));
        }
    }
    // Compacting keeps the faces in order.
    let mesh = m.rebuild(positions, polys);
    let new: Vec<u32> = (first as u32..mesh.face_count() as u32).collect();
    let (mesh, _) = mesh.compact();
    let sel = Selection::of_faces(&mesh, new);
    Ok(Done { mesh, sel })
}

// ---------------------------------------------------------------------- loop cut

/// `cuts` evenly spaced edge loops across the ring of quads through edge `a`–`b`.
pub fn loop_cut(m: &Mesh, a: u32, b: u32, cuts: u32) -> Result<Done, String> {
    let (_, ring_faces) = edge_ring(m, a, b);
    if ring_faces.is_empty() {
        return Err("no quads across that edge to cut".into());
    }
    let n = cuts.max(1);
    let in_ring: HashSet<u32> = ring_faces.iter().copied().collect();
    let mut positions = m.positions.clone();
    // Cut points per edge, from the edge's lower vertex.
    let mut points: HashMap<(u32, u32), Vec<u32>> = HashMap::new();
    let mut cut = |x: u32, y: u32, positions: &mut Vec<Vec3>| -> Vec<u32> {
        let k = edge_key(x, y);
        let ids = points
            .entry(k)
            .or_insert_with(|| {
                (1..=n)
                    .map(|i| {
                        let t = i as f32 / (n + 1) as f32;
                        positions.push(m.positions[k.0 as usize].lerp(m.positions[k.1 as usize], t));
                        positions.len() as u32 - 1
                    })
                    .collect()
            })
            .clone();
        if x == k.0 { ids } else { ids.into_iter().rev().collect() }
    };
    // The edges the ring crosses: two of each ring face's sides.
    let mut ring_edges: HashMap<u32, (u32, u32)> = HashMap::new();
    let (edges, _) = edge_ring(m, a, b);
    for &f in &ring_faces {
        let hs: Vec<u32> = m.face_half_edges(f).collect();
        let i = (0..4).find(|&i| edges.contains(&edge_key(m.origin(hs[i]), m.dest(hs[i])))).unwrap_or(0);
        ring_edges.insert(f, (hs[i], hs[(i + 2) % 4]));
    }
    let cut_edges: HashSet<(u32, u32)> = edges.iter().copied().collect();
    let uvs = m.has_uvs();
    let mut polys = Vec::new();
    let mut new_edges = Vec::new();
    for f in 0..m.face_count() as u32 {
        let mat = m.face_material(f);
        if in_ring.contains(&f) {
            let (h, opp) = ring_edges[&f];
            let (p0, p1) = (m.origin(h), m.dest(h));
            let (p2, p3) = (m.origin(opp), m.dest(opp));
            let (u0, u1, u2, u3) = (m.uv(h), m.uv(m.next(h)), m.uv(opp), m.uv(m.next(opp)));
            let side_a: Vec<(u32, [f32; 2])> = std::iter::once((p0, u0))
                .chain(
                    cut(p0, p1, &mut positions)
                        .into_iter()
                        .enumerate()
                        .map(|(i, v)| (v, lerp2(u0, u1, (i + 1) as f32 / (n + 1) as f32))),
                )
                .chain(std::iter::once((p1, u1)))
                .collect();
            let side_b: Vec<(u32, [f32; 2])> = std::iter::once((p3, u3))
                .chain(
                    cut(p3, p2, &mut positions)
                        .into_iter()
                        .enumerate()
                        .map(|(i, v)| (v, lerp2(u3, u2, (i + 1) as f32 / (n + 1) as f32))),
                )
                .chain(std::iter::once((p2, u2)))
                .collect();
            for k in 0..=n as usize {
                polys.push(poly(&[side_a[k], side_a[k + 1], side_b[k + 1], side_b[k]], uvs, mat));
                if k > 0 {
                    new_edges.push((side_a[k].0, side_b[k].0));
                }
            }
            continue;
        }
        let mut c = Vec::new();
        for h in m.face_half_edges(f) {
            let (x, y) = (m.origin(h), m.dest(h));
            c.push((x, m.uv(h)));
            if cut_edges.contains(&edge_key(x, y)) {
                let (ux, uy) = (m.uv(h), m.uv(m.next(h)));
                for (i, v) in cut(x, y, &mut positions).into_iter().enumerate() {
                    c.push((v, lerp2(ux, uy, (i + 1) as f32 / (n + 1) as f32)));
                }
            }
        }
        polys.push(poly(&c, uvs, mat));
    }
    let mesh = m.rebuild(positions, polys);
    let sel = Selection::of_edges(&mesh, new_edges);
    Ok(Done { mesh, sel })
}

// ---------------------------------------------------------------------- knife

/// Cuts faces along the plane where `dist` (a signed distance of positions) is zero: the
/// faces in `faces` (all when None) that it crosses twice are split in two. `keep` says
/// which cut points count (the knife's line on screen). The new vertices are selected.
pub fn cut(m: &Mesh, faces: Option<&[bool]>, dist: &dyn Fn(Vec3) -> f32, keep: &dyn Fn(Vec3) -> bool) -> Done {
    let scale = m.bounds().max - m.bounds().min;
    let eps = 1e-6 * scale.length().max(1e-6);
    let d: Vec<f32> = m.positions.iter().map(|&p| dist(p)).collect();
    let on = |v: u32| d[v as usize].abs() <= eps && keep(m.positions[v as usize]);
    let cuttable = |f: u32| faces.is_none_or(|s| s.get(f as usize).copied().unwrap_or(false));
    let mut positions = m.positions.clone();
    let mut split: HashMap<(u32, u32), u32> = HashMap::new();
    for f in 0..m.face_count() as u32 {
        if !cuttable(f) {
            continue;
        }
        for h in m.face_half_edges(f) {
            let (x, y) = (m.origin(h), m.dest(h));
            let (dx, dy) = (d[x as usize], d[y as usize]);
            if dx.abs() <= eps || dy.abs() <= eps || (dx > 0.0) == (dy > 0.0) {
                continue;
            }
            let k = edge_key(x, y);
            if split.contains_key(&k) {
                continue;
            }
            let t = d[k.0 as usize] / (d[k.0 as usize] - d[k.1 as usize]);
            let p = m.positions[k.0 as usize].lerp(m.positions[k.1 as usize], t);
            if keep(p) {
                positions.push(p);
                split.insert(k, positions.len() as u32 - 1);
            }
        }
    }
    let uvs = m.has_uvs();
    let mut polys = Vec::new();
    let mut chosen: Vec<u32> = split.values().copied().collect();
    for f in 0..m.face_count() as u32 {
        let mut c = Vec::new();
        let mut at = Vec::new();
        for h in m.face_half_edges(f) {
            let (x, y) = (m.origin(h), m.dest(h));
            if on(x) {
                at.push(c.len());
            }
            c.push((x, m.uv(h)));
            let k = edge_key(x, y);
            if let Some(&v) = split.get(&k) {
                let t = (positions[v as usize] - m.positions[x as usize]).length()
                    / (m.positions[y as usize] - m.positions[x as usize]).length().max(1e-30);
                at.push(c.len());
                c.push((v, lerp2(m.uv(h), m.uv(m.next(h)), t)));
            }
        }
        let mat = m.face_material(f);
        let n = c.len();
        if cuttable(f) && at.len() == 2 {
            let (i, j) = (at[0], at[1]);
            if j - i > 1 && j - i < n - 1 {
                polys.push(poly(&c[i..=j], uvs, mat));
                let rest: Vec<(u32, [f32; 2])> = c[j..].iter().chain(&c[..=i]).copied().collect();
                polys.push(poly(&rest, uvs, mat));
                chosen.extend([c[i].0, c[j].0]);
                continue;
            }
        }
        polys.push(poly(&c, uvs, mat));
    }
    let mesh = m.rebuild(positions, polys);
    let sel = Selection::of_verts(&mesh, chosen);
    Done { mesh, sel }
}

// ---------------------------------------------------------------------- bridge

/// Chains of boundary half-edges along `edges`: closed loops and open chains.
fn chains(m: &Mesh, edges: &[(u32, u32)]) -> Result<Vec<(Vec<u32>, bool)>, String> {
    let mut hs = Vec::new();
    for &(a, b) in edges {
        let all = m.edge_half_edges(a, b);
        if all.len() != 1 {
            return Err("bridging needs edges on the border of the mesh (or two face regions)".into());
        }
        hs.push(all[0]);
    }
    let from: HashMap<u32, u32> = hs.iter().map(|&h| (m.origin(h), h)).collect();
    let to: HashMap<u32, u32> = hs.iter().map(|&h| (m.dest(h), h)).collect();
    if from.len() != hs.len() || to.len() != hs.len() {
        return Err("the edges to bridge branch".into());
    }
    let mut used = HashSet::new();
    let mut out = Vec::new();
    for &h in &hs {
        if used.contains(&h) {
            continue;
        }
        // Back to the start of an open chain (or once around a loop).
        let mut s = h;
        while let Some(&p) = to.get(&m.origin(s)) {
            if p == h {
                break;
            }
            s = p;
        }
        let mut chain = vec![s];
        used.insert(s);
        let mut cur = s;
        let mut closed = false;
        while let Some(&n) = from.get(&m.dest(cur)) {
            if n == s {
                closed = true;
                break;
            }
            if !used.insert(n) {
                break;
            }
            chain.push(n);
            cur = n;
        }
        out.push((chain, closed));
    }
    Ok(out)
}

/// Joins two edge loops or chains on the border with quads. With faces given, those faces
/// are removed first and the borders of the holes are joined.
pub fn bridge(m: &Mesh, edges: &[(u32, u32)], faces: &[u32]) -> Result<Done, String> {
    if !faces.is_empty() {
        let gone: HashSet<u32> = faces.iter().copied().collect();
        let border: Vec<(u32, u32)> = faces
            .iter()
            .flat_map(|&f| m.face_half_edges(f))
            .filter(|&h| m.twin(h).is_some_and(|t| !gone.contains(&m.face(t))))
            .map(|h| edge_key(m.origin(h), m.dest(h)))
            .collect();
        let polys = m
            .polygons()
            .into_iter()
            .enumerate()
            .filter(|(f, _)| !gone.contains(&(*f as u32)))
            .map(|(_, p)| p)
            .collect();
        let open = m.rebuild(m.positions.clone(), polys);
        let done = bridge(&open, &border, &[])?;
        let (mesh, _) = done.mesh.compact();
        let sel = Selection::of_faces(&mesh, done.sel.face_list());
        return Ok(Done { mesh, sel });
    }
    let cs = chains(m, edges)?;
    let [(a, a_closed), (b, b_closed)] = cs.as_slice() else {
        return Err(format!("select two edge loops to bridge (found {})", cs.len()));
    };
    if a_closed != b_closed || a.len() != b.len() {
        return Err(format!("the two loops differ ({} and {} edges)", a.len(), b.len()));
    }
    let n = a.len();
    let va: Vec<u32> = a.iter().map(|&h| m.origin(h)).chain((!a_closed).then(|| m.dest(a[n - 1]))).collect();
    let vb: Vec<u32> = b.iter().map(|&h| m.origin(h)).chain((!b_closed).then(|| m.dest(b[n - 1]))).collect();
    if va.iter().any(|v| vb.contains(v)) {
        return Err("the loops to bridge share vertices".into());
    }
    let p = |v: u32| m.positions[v as usize];
    let k = vb.len();
    // Pairing a_i with b_(s - i): the only one that keeps the faces turned the same way.
    let pick = |s: usize, i: usize| vb[(s + k * 2 - i % k) % k];
    let s = if *a_closed {
        (0..k)
            .min_by(|&x, &y| {
                let cost = |s: usize| (0..k).map(|i| (p(va[i]) - p(pick(s, i))).length()).sum::<f32>();
                cost(x).total_cmp(&cost(y))
            })
            .unwrap_or(0)
    } else {
        k - 1
    };
    let uvs = m.has_uvs();
    let mut polys = m.polygons();
    let first = polys.len() as u32;
    for (i, &h) in a.iter().enumerate() {
        let (a0, a1) = (va[i], va[(i + 1) % va.len()]);
        let quad = [(a1, m.uv(m.next(h))), (a0, m.uv(h)), (pick(s, i), [0.0; 2]), (pick(s, i + 1), [0.0; 2])];
        polys.push(poly(&quad, uvs, m.face_material(m.face(h))));
    }
    let mesh = m.rebuild(m.positions.clone(), polys);
    let sel = Selection::of_faces(&mesh, first..mesh.face_count() as u32);
    Ok(Done { mesh, sel })
}

// ---------------------------------------------------------------------- merge, weld

/// The mesh with vertex `v` replaced by `map[v]`; faces that collapse are dropped and so
/// are vertices nothing uses.
fn remap(m: &Mesh, positions: Vec<Vec3>, map: &[u32]) -> (Mesh, Vec<u32>) {
    let polys = m
        .polygons()
        .into_iter()
        .map(|mut p| {
            p.verts.iter_mut().for_each(|v| *v = map[*v as usize]);
            p
        })
        .collect();
    let mut mesh = Mesh::build(positions, polys);
    mesh.materials.clone_from(&m.materials);
    mesh.mtllibs.clone_from(&m.mtllibs);
    for ((a, b), w) in m.creases() {
        mesh.set_crease(map[a as usize], map[b as usize], w);
    }
    let (mesh, cmap) = mesh.compact();
    let full = map.iter().map(|&v| cmap[v as usize]).collect();
    (mesh, full)
}

/// Every vertex of `verts` merged into one at `at`.
pub fn merge(m: &Mesh, verts: &[u32], at: Vec3) -> Result<Done, String> {
    let Some(&keep) = verts.iter().min() else { return Err("select the vertices to merge".into()) };
    let mut map: Vec<u32> = (0..m.vertex_count() as u32).collect();
    for &v in verts {
        map[v as usize] = keep;
    }
    let mut positions = m.positions.clone();
    positions[keep as usize] = at;
    let (mesh, full) = remap(m, positions, &map);
    let sel = Selection::of_verts(&mesh, [full[keep as usize]]);
    Ok(Done { mesh, sel })
}

/// Vertices closer than `dist` merged (at their mean), among `only` when given; returns
/// how many vertices went.
pub fn weld(m: &Mesh, only: Option<&[bool]>, dist: f32) -> (Done, usize) {
    let n = m.vertex_count();
    let take = |v: usize| only.is_none_or(|s| s.get(v).copied().unwrap_or(false));
    let cell = dist.max(1e-12);
    let key = |p: Vec3| ((p.x / cell).floor() as i64, (p.y / cell).floor() as i64, (p.z / cell).floor() as i64);
    let mut grid: HashMap<(i64, i64, i64), Vec<u32>> = HashMap::new();
    for v in (0..n).filter(|&v| take(v)) {
        grid.entry(key(m.positions[v])).or_default().push(v as u32);
    }
    let mut parent: Vec<u32> = (0..n as u32).collect();
    for v in (0..n).filter(|&v| take(v)) {
        let (x, y, z) = key(m.positions[v]);
        for dx in -1..=1 {
            for dy in -1..=1 {
                for dz in -1..=1 {
                    let Some(others) = grid.get(&(x + dx, y + dy, z + dz)) else { continue };
                    for &o in others {
                        if (o as usize) > v && (m.positions[o as usize] - m.positions[v]).length() <= dist {
                            union(&mut parent, v as u32, o);
                        }
                    }
                }
            }
        }
    }
    let map: Vec<u32> = (0..n as u32).map(|v| find(&mut parent, v)).collect();
    let mut sum: HashMap<u32, (Vec3, f32)> = HashMap::new();
    for (v, &r) in map.iter().enumerate() {
        let e = sum.entry(r).or_insert((Vec3::ZERO, 0.0));
        e.0 += m.positions[v];
        e.1 += 1.0;
    }
    let mut positions = m.positions.clone();
    let mut removed = 0;
    for (&r, &(s, c)) in &sum {
        positions[r as usize] = s * (1.0 / c);
        removed += c as usize - 1;
    }
    let (mesh, full) = remap(m, positions, &map);
    let kept: Vec<u32> = (0..n).filter(|&v| take(v)).map(|v| full[v]).filter(|&v| v != NONE).collect();
    let sel = Selection::of_verts(&mesh, kept);
    (Done { mesh, sel }, removed)
}

// ---------------------------------------------------------------------- dissolve

/// Union-find: the root of `v` (paths halved on the way).
fn find(p: &mut [u32], mut v: u32) -> u32 {
    while p[v as usize] != v {
        p[v as usize] = p[p[v as usize] as usize];
        v = p[v as usize];
    }
    v
}

/// Union-find: `a` and `b` in one set, the lower root on top.
fn union(p: &mut [u32], a: u32, b: u32) {
    let (x, y) = (find(p, a), find(p, b));
    if x != y {
        p[x.max(y) as usize] = x.min(y);
    }
}

/// Faces joined across `edges` (those with a face on each side) into one polygon per
/// connected group, where the group's border is a single loop.
pub fn dissolve_edges(m: &Mesh, edges: &[(u32, u32)]) -> Done {
    let (polys, merged) = join_faces(m, edges);
    let (mesh, _) = m.rebuild(m.positions.clone(), polys).compact();
    let sel = Selection::of_faces(&mesh, merged);
    Done { mesh, sel }
}

/// The polygons with faces joined across `edges`, and which of them are joined ones.
fn join_faces(m: &Mesh, edges: &[(u32, u32)]) -> (Vec<Polygon>, Vec<u32>) {
    let nf = m.face_count();
    let mut parent: Vec<u32> = (0..nf as u32).collect();
    for &(a, b) in edges {
        let hs = m.edge_half_edges(a, b);
        if hs.len() == 2 && m.twin(hs[0]) == Some(hs[1]) {
            union(&mut parent, m.face(hs[0]), m.face(hs[1]));
        }
    }
    let group: Vec<u32> = (0..nf as u32).map(|f| find(&mut parent, f)).collect();
    let mut members: HashMap<u32, Vec<u32>> = HashMap::new();
    for (f, &g) in group.iter().enumerate() {
        members.entry(g).or_default().push(f as u32);
    }
    let uvs = m.has_uvs();
    let mut polys = Vec::new();
    let mut merged = Vec::new();
    for f in 0..nf as u32 {
        let g = group[f as usize];
        let fs = &members[&g];
        if fs.len() == 1 {
            polys.push(m.polygon(f));
            continue;
        }
        if g != f {
            continue; // emitted with its group's first face
        }
        let border: Vec<u32> = fs
            .iter()
            .flat_map(|&x| m.face_half_edges(x))
            .filter(|&h| m.twin(h).is_none_or(|t| group[m.face(t) as usize] != g))
            .collect();
        if border.is_empty() {
            // The whole of a closed surface: no single polygon can replace it.
            polys.extend(fs.iter().map(|&x| m.polygon(x)));
            continue;
        }
        let from: HashMap<u32, u32> = border.iter().map(|&h| (m.origin(h), h)).collect();
        let mut chain = vec![border[0]];
        let mut cur = border[0];
        while let Some(&n) = from.get(&m.dest(cur)) {
            if n == border[0] || chain.len() > border.len() {
                break;
            }
            chain.push(n);
            cur = n;
        }
        if from.len() == border.len() && chain.len() == border.len() {
            let c: Vec<(u32, [f32; 2])> = chain.iter().map(|&h| (m.origin(h), m.uv(h))).collect();
            merged.push(polys.len() as u32);
            polys.push(poly(&c, uvs, m.face_material(f)));
        } else {
            // A ring of faces (a hole inside): left as it is.
            polys.extend(fs.iter().map(|&x| m.polygon(x)));
        }
    }
    (polys, merged)
}

/// The edges inside the region of `faces` dissolved.
pub fn dissolve_faces(m: &Mesh, faces: &[u32]) -> Done {
    let set: HashSet<u32> = faces.iter().copied().collect();
    let edges: Vec<(u32, u32)> = faces
        .iter()
        .flat_map(|&f| m.face_half_edges(f))
        .filter(|&h| m.twin(h).is_some_and(|t| set.contains(&m.face(t))))
        .map(|h| edge_key(m.origin(h), m.dest(h)))
        .collect();
    dissolve_edges(m, &edges)
}

/// Each vertex removed with the edges around it: its faces become one; a vertex left
/// between just two edges is taken out of its faces.
pub fn dissolve_verts(m: &Mesh, verts: &[u32]) -> Done {
    let edges: Vec<(u32, u32)> =
        verts.iter().flat_map(|&v| m.outgoing(v).iter().map(move |&h| edge_key(m.origin(h), m.dest(h)))).collect();
    let (polys, _) = join_faces(m, &edges);
    // Still numbered as `m`.
    let mesh = m.rebuild(m.positions.clone(), polys);
    let gone: HashSet<u32> = verts.iter().copied().filter(|&v| mesh.neighbors(v).len() == 2).collect();
    let polys = mesh
        .polygons()
        .into_iter()
        .map(|p| {
            let keep: Vec<usize> = (0..p.verts.len()).filter(|&i| !gone.contains(&p.verts[i])).collect();
            Polygon {
                verts: keep.iter().map(|&i| p.verts[i]).collect(),
                uvs: p.uvs.as_ref().map(|u| keep.iter().map(|&i| u[i]).collect()),
                material: p.material,
            }
        })
        .collect();
    let (mesh, _) = mesh.rebuild(mesh.positions.clone(), polys).compact();
    let sel = Selection::new(&mesh);
    Done { mesh, sel }
}

// ---------------------------------------------------------------------- delete, fill

/// Removes what the selection holds in `mode`: faces using a selected vertex or edge, or
/// the selected faces; then vertices nothing uses.
pub fn delete(m: &Mesh, sel: &Selection, mode: Mode) -> Done {
    let gone = |f: u32| match mode {
        Mode::Vertex => m.face_verts(f).iter().any(|&v| sel.has_vert(v)),
        Mode::Edge => m.face_edges(f).any(|e| sel.edges.contains(&e)),
        Mode::Face => sel.has_face(f),
    };
    let polys = (0..m.face_count() as u32).filter(|&f| !gone(f)).map(|f| m.polygon(f)).collect();
    let (mesh, _) = m.rebuild(m.positions.clone(), polys).compact();
    let sel = Selection::new(&mesh);
    Done { mesh, sel }
}

/// A face over every hole whose border has a selected edge.
pub fn fill(m: &Mesh, sel: &Selection) -> Result<Done, String> {
    let mut seen = HashSet::new();
    let mut polys = m.polygons();
    let first = polys.len() as u32;
    for h in 0..m.half_edge_count() as u32 {
        if m.twin(h).is_some() || seen.contains(&h) {
            continue;
        }
        let ring = boundary_loop(m, h);
        seen.extend(ring.iter().copied());
        let closed = ring.len() >= 3 && m.dest(ring[ring.len() - 1]) == m.origin(ring[0]);
        if !closed || !ring.iter().any(|&x| sel.has_edge(m.origin(x), m.dest(x))) {
            continue;
        }
        let mut c: Vec<(u32, [f32; 2])> = ring.iter().map(|&x| (m.origin(x), m.uv(x))).collect();
        c.reverse();
        polys.push(poly(&c, m.has_uvs(), m.face_material(m.face(h))));
    }
    if polys.len() as u32 == first {
        return Err("select an edge of the hole to fill".into());
    }
    let mesh = m.rebuild(m.positions.clone(), polys);
    let sel = Selection::of_faces(&mesh, first..mesh.face_count() as u32);
    Ok(Done { mesh, sel })
}

// ---------------------------------------------------------------------- mirror, flip, triangulate

/// The faces (all when None) copied mirrored across the plane `axis` = 0, sharing the
/// vertices within `eps` of the plane (moved onto it).
pub fn mirror(m: &Mesh, axis: usize, faces: Option<&[u32]>, eps: f32) -> Done {
    let all: Vec<u32> = (0..m.face_count() as u32).collect();
    let faces = faces.unwrap_or(&all);
    let mut positions = m.positions.clone();
    let mut twin_of: HashMap<u32, u32> = HashMap::new();
    for &f in faces {
        for &v in m.face_verts(f) {
            twin_of.entry(v).or_insert_with(|| {
                let p = m.positions[v as usize];
                if p[axis].abs() <= eps {
                    positions[v as usize][axis] = 0.0;
                    v
                } else {
                    let mut q = p;
                    q[axis] = -q[axis];
                    positions.push(q);
                    positions.len() as u32 - 1
                }
            });
        }
    }
    let mut polys = m.polygons();
    let first = polys.len() as u32;
    for &f in faces {
        let mut p = m.polygon(f);
        p.verts.iter_mut().for_each(|v| *v = twin_of[v]);
        polys.push(p.reversed());
    }
    let mut mesh = m.rebuild(positions, polys);
    for ((a, b), w) in m.creases() {
        if let (Some(&x), Some(&y)) = (twin_of.get(&a), twin_of.get(&b)) {
            mesh.set_crease(x, y, w);
        }
    }
    let sel = Selection::of_faces(&mesh, faces.iter().copied().chain(first..mesh.face_count() as u32));
    Done { mesh, sel }
}

/// The faces turned the other way.
pub fn flip(m: &Mesh, faces: &[u32]) -> Done {
    let set: HashSet<u32> = faces.iter().copied().collect();
    let polys = m
        .polygons()
        .into_iter()
        .enumerate()
        .map(|(f, p)| if set.contains(&(f as u32)) { p.reversed() } else { p })
        .collect();
    let mesh = m.rebuild(m.positions.clone(), polys);
    let sel = Selection::of_faces(&mesh, faces.iter().copied());
    Done { mesh, sel }
}

/// The faces cut into triangles (ears clipped in their planes).
pub fn triangulate(m: &Mesh, faces: &[u32]) -> Done {
    let set: HashSet<u32> = faces.iter().copied().collect();
    let uvs = m.has_uvs();
    let mut polys = Vec::new();
    let mut new = Vec::new();
    for f in 0..m.face_count() as u32 {
        if !set.contains(&f) || m.face_verts(f).len() == 3 {
            if set.contains(&f) {
                new.push(polys.len() as u32);
            }
            polys.push(m.polygon(f));
            continue;
        }
        let c = corners(m, f);
        for t in ear_clip(&m.face_points(f)) {
            new.push(polys.len() as u32);
            polys.push(poly(&t.map(|i| c[i]), uvs, m.face_material(f)));
        }
    }
    let mesh = m.rebuild(m.positions.clone(), polys);
    let sel = Selection::of_faces(&mesh, new);
    Done { mesh, sel }
}

#[cfg(test)]
mod tests;
