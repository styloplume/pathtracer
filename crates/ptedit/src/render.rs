//! Draws one view: culls the display's draws against the view, picks each one's level of
//! detail, cuts the triangles and edges into jobs and runs the rasterizer's passes
//! (surfaces; then wireframes, grid and bounds tested against them; then overlays). The
//! selection is outlined from the id buffer afterwards.

use crate::display::{CUBE, Display, LEVELS, Part, box_xf};
use crate::overlay::{Overlays, SELECTED, Selection};
use crate::paint::Canvas;
use crate::raster::{Emitter, LineStyle, Pass, Raster, Target, Vertex, pack};
use crate::view::{ClipMap, Proj, apply};
use pt_core::geometry::Aabb;
use pt_core::math::{Transform, Vec3};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shading {
    Wireframe,
    /// Flat faces lit by a headlight, in one color.
    Solid,
    /// Material base colors, smooth normals where the geometry has them.
    Shaded,
    /// Solid with the wireframe on top.
    WireSolid,
    /// Surfaces (and the background) in the key color, for the path-traced image to replace;
    /// overlays and the selection outline on top.
    Rendered,
}

/// The color `Shading::Rendered` fills surfaces and the background with.
pub const KEY: u32 = 0x01_02_03;

impl Shading {
    pub const ALL: [Shading; 4] = [Shading::Wireframe, Shading::Solid, Shading::Shaded, Shading::WireSolid];

    pub fn name(self) -> &'static str {
        match self {
            Shading::Wireframe => "wireframe",
            Shading::Solid => "solid",
            Shading::Shaded => "shaded",
            Shading::WireSolid => "wireframe on solid",
            Shading::Rendered => "rendered",
        }
    }

    pub fn parse(s: &str) -> Option<Shading> {
        match s {
            "wire" | "wireframe" => Some(Shading::Wireframe),
            "solid" => Some(Shading::Solid),
            "shaded" => Some(Shading::Shaded),
            "wiresolid" | "wire-solid" => Some(Shading::WireSolid),
            _ => None,
        }
    }
}

pub const BACKGROUND: u32 = 0x2a_2b_2f;
const SOLID_GREY: [f32; 3] = [0.78, 0.78, 0.8];
/// Visible triangles beyond which the smallest objects (on screen) become boxes.
pub const TRIANGLE_BUDGET: usize = 400_000;
/// Meshes smaller than this many pixels across show as boxes.
const TINY_PX: f32 = 3.0;
/// Triangles or edges per job.
const CHUNK: usize = 4096;

/// How one draw is shown this frame.
#[derive(Clone, Copy)]
struct Visible {
    draw: u32,
    /// The display mesh drawn (a stand-in cube for proxies and boxed meshes), or None for
    /// a wire box.
    mesh: Option<u32>,
    /// Where the mesh goes: the draw's placement, or the box around the draw.
    xf: Transform,
    map: ClipMap,
    /// Triangles carry their own materials (else the draw's material colors them all).
    per_tri: bool,
    id: u32,
    selected: bool,
    /// Distance along the view direction, pixels across, and triangles of a real mesh
    /// (0 for shapes and stand-ins), for ordering and the budget.
    depth: f32,
    px: f32,
    triangles: usize,
}

impl Visible {
    /// The same draw shown as a solid box of its bounds.
    fn boxed(&self, bounds: &Aabb, proj: &Proj) -> Visible {
        let xf = box_xf(bounds);
        Visible { mesh: Some(CUBE), xf, map: proj.clip_map_of(&xf), per_tri: false, triangles: 0, ..*self }
    }
}

/// Counts for the status line.
#[derive(Clone, Copy, Debug, Default)]
pub struct Stats {
    pub triangles: usize,
    /// Objects drawn as boxes: implicit surfaces, scatter instances, and meshes too small
    /// on screen or past the triangle budget.
    pub boxes: usize,
}

/// Headlight shading: light from the viewing direction, a little ambient.
#[inline]
fn shade(base: [f32; 3], n: Vec3, light: Vec3) -> [f32; 3] {
    let k = 0.3 + 0.7 * n.dot(light).abs();
    base.map(|c| c * k)
}

/// Draws `display` as seen through `proj` into `target`.
#[expect(clippy::too_many_arguments, reason = "what one view shows and how")]
pub fn render(
    raster: &mut Raster,
    target: &mut Target,
    display: &Display,
    proj: &Proj,
    shading: Shading,
    sel: &Selection,
    hidden: &Selection,
    overlays: &Overlays,
) -> Stats {
    let mut stats = Stats::default();
    let vis = visible(display, proj, sel, hidden);
    // Front to back, so hidden surfaces fail the depth tests early.
    let mut order: Vec<usize> = (0..vis.len()).collect();
    order.sort_unstable_by(|&a, &b| vis[a].depth.total_cmp(&vis[b].depth));
    // Jobs: (visible index, first, end) over triangles and over edges.
    let mut tri_jobs = Vec::new();
    let mut edge_jobs = Vec::new();
    let wire = matches!(shading, Shading::Wireframe | Shading::WireSolid);
    for &k in &order {
        let v = &vis[k];
        let Some(m) = v.mesh else {
            stats.boxes += 1;
            continue;
        };
        if m == CUBE && !v.per_tri && !matches!(display.draws[v.draw as usize].part, Part::Shape(_)) {
            stats.boxes += 1;
        }
        let mesh = &display.meshes[m as usize];
        stats.triangles += mesh.triangles.len();
        if shading != Shading::Wireframe {
            for s in (0..mesh.triangles.len()).step_by(CHUNK) {
                tri_jobs.push((k, s, (s + CHUNK).min(mesh.triangles.len())));
            }
        }
        if wire {
            for s in (0..mesh.edges.len()).step_by(CHUNK) {
                edge_jobs.push((k, s, (s + CHUNK).min(mesh.edges.len())));
            }
        }
    }
    let boxes: Vec<usize> = (0..vis.len()).filter(|&k| vis[k].mesh.is_none()).collect();
    let light = -proj.fwd;
    let (tri_items, edge_items) = (tri_jobs, edge_jobs);
    let (tri_jobs, edge_jobs) = (batch(&tri_items), batch(&edge_items));

    let tri_item = |e: &mut Emitter<'_>, k: usize, s: usize, end: usize| {
        let v = &vis[k];
        let d = &display.draws[v.draw as usize];
        let mesh = &display.meshes[v.mesh.expect("only meshes get triangle jobs") as usize];
        for t in &mesh.triangles[s..end] {
            let p = t.v.map(|i| mesh.positions[i as usize]);
            let clip = p.map(|p| apply(&v.map, p));
            if shading == Shading::Rendered {
                e.tri(clip.map(|p| Vertex { p, c: [0.0; 3] }), v.id, Some(KEY));
                continue;
            }
            let base = match shading {
                Shading::Shaded => {
                    let m = if v.per_tri { t.material } else { d.material };
                    display.colors[m as usize % display.colors.len()]
                }
                _ => SOLID_GREY,
            };
            if shading == Shading::Shaded && t.has_normals() {
                let c = t.n.map(|n| shade(base, v.xf.normal(mesh.normals[n as usize]), light));
                e.tri(std::array::from_fn(|i| Vertex { p: clip[i], c: c[i] }), v.id, None);
            } else {
                let w = p.map(|p| v.xf.point(p));
                let n = (w[1] - w[0]).cross(w[2] - w[0]).normalize();
                let n = if n.is_finite() { n } else { light };
                e.tri(clip.map(|p| Vertex { p, c: [0.0; 3] }), v.id, Some(pack(shade(base, n, light))));
            }
        }
    };

    let tris = |j: usize, e: &mut Emitter<'_>| {
        for &(k, s, end) in &tri_items[tri_jobs[j].clone()] {
            tri_item(e, k, s, end);
        }
    };
    let edge_style = |selected: bool| match (shading, selected) {
        (_, true) => LineStyle {
            color: SELECTED,
            alpha: 1.0,
            depth_test: shading != Shading::Wireframe,
            write: shading == Shading::Wireframe,
            bias: 2e-3,
        },
        (Shading::Wireframe, false) => {
            LineStyle { color: 0xb4_b8_c0, alpha: 1.0, depth_test: false, write: true, bias: 0.0 }
        }
        _ => LineStyle { color: 0x16_16_18, alpha: 0.7, depth_test: true, write: false, bias: 2e-3 },
    };
    let box_style = |selected: bool| LineStyle {
        color: if selected { SELECTED } else { 0x9a_9a_b0 },
        alpha: 1.0,
        depth_test: shading != Shading::Wireframe,
        write: true,
        bias: 1e-3,
    };
    // Second pass: wireframes, wire boxes and the grid, against the finished surfaces.
    let lines = |j: usize, e: &mut Emitter<'_>| {
        if j < edge_jobs.len() {
            for &(k, s, end) in &edge_items[edge_jobs[j].clone()] {
                let v = &vis[k];
                let mesh = &display.meshes[v.mesh.expect("only meshes get edge jobs") as usize];
                let style = edge_style(v.selected);
                for &[a, b] in &mesh.edges[s..end] {
                    let (a, b) = (mesh.positions[a as usize], mesh.positions[b as usize]);
                    e.line(apply(&v.map, a), apply(&v.map, b), style, v.id);
                }
            }
            return;
        }
        // Last job: wire boxes, then the grid.
        let cube = &display.meshes[CUBE as usize];
        for &k in &boxes {
            let v = &vis[k];
            for &[a, b] in &cube.edges {
                let (a, b) = (cube.positions[a as usize], cube.positions[b as usize]);
                e.line(apply(&v.map, a), apply(&v.map, b), box_style(v.selected), v.id);
            }
        }
        for l in &overlays.grid {
            e.line(proj.clip(l.a), proj.clip(l.b), l.style, l.id);
        }
    };
    let over = |_: usize, e: &mut Emitter<'_>| {
        for l in &overlays.world {
            e.line(proj.clip(l.a), proj.clip(l.b), l.style, l.id);
        }
        let (w, h) = (proj.w as f32, proj.h as f32);
        let ndc = |p: [f32; 2]| [p[0] / w * 2.0 - 1.0, 1.0 - p[1] / h * 2.0, 1.0, 1.0];
        for l in &overlays.screen {
            e.line(ndc(l.a), ndc(l.b), LineStyle::overlay(l.color), 0);
        }
    };
    raster.draw(
        target,
        if shading == Shading::Rendered { KEY } else { BACKGROUND },
        &[
            Pass { jobs: tri_jobs.len(), run: &tris },
            Pass { jobs: edge_jobs.len() + 1, run: &lines },
            Pass { jobs: 1, run: &over },
        ],
    );
    if shading != Shading::Wireframe && !sel.is_empty() {
        outline(target, sel);
    }
    let scale = label_scale(proj.h);
    let mut canvas = Canvas { px: &mut target.color, w: target.w, h: target.h };
    for l in &overlays.labels {
        canvas.text(l.x, l.y, &l.text, l.color, scale);
    }
    stats
}

/// The draws in view and how each is shown (with its level of detail), classified in
/// parallel; past the triangle budget the smallest meshes on screen become boxes.
fn visible(display: &Display, proj: &Proj, sel: &Selection, hidden: &Selection) -> Vec<Visible> {
    let world_map = proj.clip_map();
    let classify = |i: usize| -> Option<Visible> {
        let d = &display.draws[i];
        let id = d.object.map_or(0, |o| o + 1);
        if hidden.has_id(id) || crate::view::culls(&world_map, &d.bounds) {
            return None;
        }
        let c = d.bounds.centroid();
        let px = (d.bounds.max - d.bounds.min).length() / proj.pixel_size(c);
        let v = Visible {
            draw: i as u32,
            mesh: None,
            xf: d.xf,
            map: proj.clip_map_of(&d.xf),
            per_tri: false,
            id,
            selected: sel.has_id(id),
            depth: (c - proj.eye).dot(proj.fwd),
            px,
            triangles: 0,
        };
        Some(match d.part {
            Part::Bounds => v,
            Part::Proxy => Visible { mesh: Some(CUBE), ..v },
            Part::Mesh(m) => {
                let n = display.meshes[m as usize].triangles.len();
                if px < TINY_PX && n > 12 {
                    v.boxed(&d.bounds, proj)
                } else {
                    Visible { mesh: Some(m), per_tri: true, triangles: n, ..v }
                }
            }
            Part::Shape(c) => {
                // About one segment per 3.5 pixels around (`px` is the bounds' diagonal).
                let level = LEVELS.iter().position(|&n| n as f32 >= px * 0.5).unwrap_or(LEVELS.len() - 1);
                Visible { mesh: Some(display.canonical(c, level)), ..v }
            }
        })
    };
    let n = display.draws.len();
    let threads = std::thread::available_parallelism().map_or(4, usize::from);
    let chunk = n.div_ceil(threads).max(1024);
    let mut vis: Vec<Visible> = if n <= chunk {
        (0..n).filter_map(classify).collect()
    } else {
        std::thread::scope(|scope| {
            let classify = &classify;
            let parts: Vec<_> = (0..n)
                .step_by(chunk)
                .map(|s| scope.spawn(move || (s..(s + chunk).min(n)).filter_map(classify).collect::<Vec<_>>()))
                .collect();
            parts.into_iter().flat_map(|p| p.join().expect("classifying draws does not panic")).collect()
        })
    };
    // Over budget: the smallest meshes on screen become boxes.
    let total: usize = vis.iter().map(|v| v.triangles).sum();
    if total > TRIANGLE_BUDGET {
        let mut order: Vec<usize> = (0..vis.len()).filter(|&k| vis[k].triangles > 0).collect();
        order.sort_by(|&a, &b| vis[a].px.total_cmp(&vis[b].px));
        let mut left = total;
        for k in order {
            if left <= TRIANGLE_BUDGET {
                break;
            }
            left -= vis[k].triangles;
            let b = display.draws[vis[k].draw as usize].bounds;
            vis[k] = vis[k].boxed(&b, proj);
        }
    }
    vis
}

/// Groups consecutive `(item, first, end)` ranges into jobs of about `CHUNK` elements
/// (small draws share a job; big ones were already cut into `CHUNK`-sized items).
fn batch(items: &[(usize, usize, usize)]) -> Vec<std::ops::Range<usize>> {
    let mut jobs = Vec::new();
    let (mut start, mut size) = (0, 0);
    for (i, &(_, s, e)) in items.iter().enumerate() {
        if size > 0 && size + (e - s) > CHUNK {
            jobs.push(start..i);
            (start, size) = (i, 0);
        }
        size += e - s;
    }
    if start < items.len() {
        jobs.push(start..items.len());
    }
    jobs
}

/// Font scale for a view `h` pixels high.
pub fn label_scale(h: usize) -> i32 {
    if h >= 900 { 2 } else { 1 }
}

/// Paints a two-pixel outline around the silhouettes of the selected items: pixels next
/// to a selected one that are empty or farther away (not where something in front of
/// the selection hides it, which would outline every object standing on a selected floor).
fn outline(t: &mut Target, sel: &Selection) {
    const AROUND: [(i32, i32); 8] = [(1, 0), (-1, 0), (0, 1), (0, -1), (2, 0), (-2, 0), (0, 2), (0, -2)];
    let (w, h) = (t.w, t.h);
    let mask: Vec<bool> = t.id.iter().map(|&id| sel.has_id(id)).collect();
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            if mask[i] {
                continue;
            }
            let behind = |dx: i32, dy: i32| {
                let (nx, ny) = (x as i32 + dx, y as i32 + dy);
                if nx < 0 || ny < 0 || nx as usize >= w || ny as usize >= h {
                    return false;
                }
                let j = ny as usize * w + nx as usize;
                mask[j] && (t.id[i] == 0 || t.depth[i] < t.depth[j])
            };
            if AROUND.iter().any(|&(dx, dy)| behind(dx, dy)) {
                t.color[i] = SELECTED;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A selected floor (id 1) behind an object (id 2) is outlined along its own edge
    /// against the background, not around the object in front of it; an object behind the
    /// floor's edge is outlined.
    #[test]
    fn outline_follows_silhouettes() {
        let (w, h) = (12, 8);
        let mut t = Target::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let i = y * w + x;
                if y >= 3 {
                    (t.id[i], t.depth[i]) = (1, 0.5);
                }
                if (4..8).contains(&x) && (4..7).contains(&y) {
                    (t.id[i], t.depth[i]) = (2, 0.9);
                }
            }
        }
        let mut sel = Selection::new(2, 0);
        sel.set_id(1, true);
        outline(&mut t, &sel);
        let lit = |x: usize, y: usize| t.color[y * w + x] == SELECTED;
        assert!(lit(0, 2) && lit(5, 1), "above the floor's far edge");
        assert!(!lit(0, 0), "two pixels wide");
        assert!((4..8).all(|x| (4..7).all(|y| !lit(x, y))), "the object in front is not outlined");

        // The same object behind the floor: its pixels next to the floor are.
        for i in 0..w * h {
            if t.id[i] == 2 {
                t.depth[i] = 0.1;
            }
        }
        t.color.fill(0);
        outline(&mut t, &sel);
        assert_eq!(t.color[4 * w + 4], SELECTED);
    }
}
