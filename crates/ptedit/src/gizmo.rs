//! Transform gizmos and handles, drawn over every view at a constant size on screen:
//! arrows, plane squares and a centre square to move; rings (and a ring facing the view)
//! to rotate; axes with boxes and a centre square to scale. Handles are small squares on
//! things that can be dragged on their own: a spot light's cone and aim, the sun on the sky
//! dome, the faces of a scatter box, the camera's focus plane.
//!
//! This module only draws and hit-tests; the editor turns drags into edits.

use crate::overlay::{AXIS_COLORS, Line2, Line3, Overlays};
use crate::raster::LineStyle;
use crate::view::Proj;
use pt_core::math::{PI, Vec3};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Tool {
    #[default]
    Select,
    Move,
    Rotate,
    Scale,
}

impl Tool {
    pub fn name(self) -> &'static str {
        match self {
            Tool::Select => "select",
            Tool::Move => "move",
            Tool::Rotate => "rotate",
            Tool::Scale => "scale",
        }
    }
}

/// A part of the gizmo.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Part {
    /// Along gizmo axis k.
    Axis(usize),
    /// In the plane whose normal is gizmo axis k.
    Plane(usize),
    /// In the view plane (move) or all axes (scale).
    Center,
    /// About gizmo axis k.
    Ring(usize),
    /// About the view direction.
    ViewRing,
}

pub const HOT: u32 = 0xff_e0_40;
const VIEW_RING: u32 = 0xc8_c8_d0;
/// Gizmo size in pixels (at UI scale 1).
const SIZE_PX: f32 = 80.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Gizmo {
    pub tool: Tool,
    pub pivot: Vec3,
    pub axes: [Vec3; 3],
    pub hot: Option<Part>,
    /// The part being dragged (drawn highlighted; the others are hidden).
    pub active: Option<Part>,
    /// UI scale (gizmos grow with the font).
    pub scale: f32,
}

/// What a handle changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HandleKind {
    /// A spot light's cone (`angle`) or full-intensity cone (`inner_angle`), by light entry.
    SpotAngle(usize),
    SpotInner(usize),
    /// Where a spot light points.
    SpotAim(usize),
    /// The sun: of the sky (None) or a directional light (its entry).
    Sun(Option<usize>),
    /// A face of an instance's scatter box: object, axis, max side.
    Scatter(usize, usize, bool),
    /// The camera's focus distance.
    Focus,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Handle {
    pub kind: HandleKind,
    pub pos: Vec3,
    pub color: u32,
}

/// Everything drawn over the views besides the scene's own helpers.
#[derive(Clone, Debug, Default)]
pub struct Decor {
    pub gizmo: Option<Gizmo>,
    pub handles: Vec<Handle>,
    pub hot_handle: Option<usize>,
    /// Helper lines (cones, the sky dome, scatter boxes, the focus plane).
    pub lines: Vec<(Vec3, Vec3, u32)>,
    pub scale: f32,
}

fn dim(c: u32) -> u32 {
    let ch = |s: u32| ((c >> s & 0xff) * 2 / 5) << s;
    ch(16) | ch(8) | ch(0)
}

/// World length of the gizmo's arms at `pivot`.
pub fn size(proj: &Proj, pivot: Vec3, scale: f32) -> f32 {
    proj.pixel_size(pivot) * SIZE_PX * scale.max(0.5)
}

enum Shape {
    /// Screen polyline.
    Line(Vec<[f32; 2]>, bool),
    /// Screen quadrilateral (hit inside).
    Quad([[f32; 2]; 4]),
}

/// The parts of `g` as seen through `proj`, with their colors.
fn shapes(g: &Gizmo, proj: &Proj) -> Vec<(Part, Shape, u32)> {
    let mut out = Vec::new();
    let Some(c) = proj.to_screen(g.pivot) else { return out };
    let s = size(proj, g.pivot, g.scale);
    let scr = |p: Vec3| proj.to_screen(p).map(|(x, y, _)| [x, y]);
    let px = g.scale.max(0.5);
    let c2 = [c.0, c.1];
    match g.tool {
        Tool::Select => {}
        Tool::Move | Tool::Scale => {
            for (k, (&a, &color)) in g.axes.iter().zip(&AXIS_COLORS).enumerate() {
                let Some(end) = scr(g.pivot + a * s) else { continue };
                // Axes pointing at the viewer collapse to a dot: skip them.
                if (end[0] - c.0).hypot(end[1] - c.1) < 6.0 * px {
                    continue;
                }
                out.push((Part::Axis(k), Shape::Line(vec![c2, end], false), color));
                let (dx, dy) = (end[0] - c.0, end[1] - c.1);
                let len = dx.hypot(dy).max(1e-3);
                let (ux, uy) = (dx / len, dy / len);
                let h = 7.0 * px;
                if g.tool == Tool::Move {
                    let tip = [end[0] + ux * h * 1.6, end[1] + uy * h * 1.6];
                    let l = [end[0] - uy * h * 0.6, end[1] + ux * h * 0.6];
                    let r = [end[0] + uy * h * 0.6, end[1] - ux * h * 0.6];
                    out.push((Part::Axis(k), Shape::Line(vec![l, tip, r, l], false), color));
                } else {
                    let q = [
                        [end[0] - h * 0.6, end[1] - h * 0.6],
                        [end[0] + h * 0.6, end[1] - h * 0.6],
                        [end[0] + h * 0.6, end[1] + h * 0.6],
                        [end[0] - h * 0.6, end[1] + h * 0.6],
                    ];
                    out.push((Part::Axis(k), Shape::Quad(q), color));
                }
            }
            if g.tool == Tool::Move {
                for (k, &color) in AXIS_COLORS.iter().enumerate() {
                    let (u, v) = (g.axes[(k + 1) % 3], g.axes[(k + 2) % 3]);
                    let corners = [(0.22, 0.22), (0.42, 0.22), (0.42, 0.42), (0.22, 0.42)]
                        .map(|(a, b)| scr(g.pivot + u * (s * a) + v * (s * b)));
                    if let [Some(a), Some(b), Some(cc), Some(d)] = corners {
                        let area = ((b[0] - a[0]) * (d[1] - a[1]) - (b[1] - a[1]) * (d[0] - a[0])).abs();
                        if area > 20.0 * px * px {
                            out.push((Part::Plane(k), Shape::Quad([a, b, cc, d]), color));
                        }
                    }
                }
            }
            let h = 5.0 * px;
            let q = [[c.0 - h, c.1 - h], [c.0 + h, c.1 - h], [c.0 + h, c.1 + h], [c.0 - h, c.1 + h]];
            out.push((Part::Center, Shape::Quad(q), VIEW_RING));
        }
        Tool::Rotate => {
            const N: usize = 48;
            for (k, &color) in AXIS_COLORS.iter().enumerate() {
                let (u, v) = (g.axes[(k + 1) % 3], g.axes[(k + 2) % 3]);
                let pts: Vec<[f32; 2]> = (0..=N)
                    .filter_map(|i| {
                        let t = 2.0 * PI * i as f32 / N as f32;
                        scr(g.pivot + (u * t.cos() + v * t.sin()) * s)
                    })
                    .collect();
                if pts.len() == N + 1 {
                    out.push((Part::Ring(k), Shape::Line(pts, true), color));
                }
            }
            let r = s / proj.pixel_size(g.pivot) * 1.18;
            let pts: Vec<[f32; 2]> = (0..=N)
                .map(|i| {
                    let t = 2.0 * PI * i as f32 / N as f32;
                    [c.0 + r * t.cos(), c.1 + r * t.sin()]
                })
                .collect();
            out.push((Part::ViewRing, Shape::Line(pts, true), VIEW_RING));
        }
    }
    out
}

fn seg_dist(p: [f32; 2], a: [f32; 2], b: [f32; 2]) -> f32 {
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    let l2 = dx * dx + dy * dy;
    let t = if l2 > 0.0 { (((p[0] - a[0]) * dx + (p[1] - a[1]) * dy) / l2).clamp(0.0, 1.0) } else { 0.0 };
    (p[0] - a[0] - dx * t).hypot(p[1] - a[1] - dy * t)
}

fn inside(p: [f32; 2], q: &[[f32; 2]; 4]) -> bool {
    let mut sign = 0.0f32;
    for i in 0..4 {
        let (a, b) = (q[i], q[(i + 1) % 4]);
        let c = (b[0] - a[0]) * (p[1] - a[1]) - (b[1] - a[1]) * (p[0] - a[0]);
        if c.abs() > 0.0 {
            if sign.abs() > 0.0 && c.signum() * sign < 0.0 {
                return false;
            }
            sign = c.signum();
        }
    }
    true
}

/// The part under view-local pixel (`x`, `y`).
pub fn hit(g: &Gizmo, proj: &Proj, x: f32, y: f32) -> Option<Part> {
    let reach = 7.0 * g.scale.max(0.5);
    let mut best: Option<(f32, Part)> = None;
    for (part, shape, _) in shapes(g, proj) {
        let d = match &shape {
            Shape::Line(pts, _) => pts.windows(2).map(|w| seg_dist([x, y], w[0], w[1])).fold(f32::INFINITY, f32::min),
            // Inside a square counts as right on it.
            Shape::Quad(q) if inside([x, y], q) => 0.0,
            Shape::Quad(q) => (0..4).map(|i| seg_dist([x, y], q[i], q[(i + 1) % 4])).fold(f32::INFINITY, f32::min),
        };
        // Squares win over the lines through them.
        let d = if matches!(shape, Shape::Quad(_)) { d - 2.0 } else { d };
        if d <= reach && best.is_none_or(|b| d < b.0) {
            best = Some((d, part));
        }
    }
    best.map(|b| b.1)
}

fn push(o: &mut Overlays, a: [f32; 2], b: [f32; 2], color: u32) {
    o.screen.push(Line2 { a, b, color });
}

pub fn draw(o: &mut Overlays, g: &Gizmo, proj: &Proj) {
    let eye_side = |p: Vec3| if proj.ortho { -proj.fwd } else { proj.eye - p };
    for (part, shape, color) in shapes(g, proj) {
        if g.active.is_some_and(|a| a != part) {
            continue;
        }
        let lit = g.hot == Some(part) || g.active == Some(part);
        let color = if lit { HOT } else { color };
        match shape {
            Shape::Line(pts, ring) => {
                // Rings: the half facing away is dim.
                let back = |i: usize| {
                    if !ring || !matches!(part, Part::Ring(_)) {
                        return false;
                    }
                    let Part::Ring(k) = part else { return false };
                    let (u, v) = (g.axes[(k + 1) % 3], g.axes[(k + 2) % 3]);
                    let t = 2.0 * PI * (i as f32 + 0.5) / (pts.len() - 1) as f32;
                    let dir = u * t.cos() + v * t.sin();
                    dir.dot(eye_side(g.pivot)) < 0.0
                };
                // Two pixels wide (three when lit): copies shifted across the line.
                let width = if lit { 3 } else { 2 };
                for (i, w) in pts.windows(2).enumerate() {
                    let c = if back(i) && !lit { dim(color) } else { color };
                    let steep = (w[1][1] - w[0][1]).abs() > (w[1][0] - w[0][0]).abs();
                    for k in 0..width {
                        let d = k as f32 - (width - 1) as f32 * 0.5;
                        let (dx, dy) = if steep { (d, 0.0) } else { (0.0, d) };
                        push(o, [w[0][0] + dx, w[0][1] + dy], [w[1][0] + dx, w[1][1] + dy], c);
                    }
                }
            }
            Shape::Quad(q) => {
                // A filled-looking square: its outline and a few lines across.
                for i in 0..4 {
                    push(o, q[i], q[(i + 1) % 4], color);
                }
                let n = 4;
                for j in 1..n {
                    let t = j as f32 / n as f32;
                    let a = [q[0][0] + (q[3][0] - q[0][0]) * t, q[0][1] + (q[3][1] - q[0][1]) * t];
                    let b = [q[1][0] + (q[2][0] - q[1][0]) * t, q[1][1] + (q[2][1] - q[1][1]) * t];
                    push(o, a, b, color);
                }
            }
        }
    }
}

/// The handle under view-local pixel (`x`, `y`).
pub fn hit_handle(d: &Decor, proj: &Proj, x: f32, y: f32) -> Option<usize> {
    let reach = 7.0 * d.scale.max(0.5);
    let mut best: Option<(f32, usize)> = None;
    for (i, h) in d.handles.iter().enumerate() {
        let Some((sx, sy, _)) = proj.to_screen(h.pos) else { continue };
        let dist = (sx - x).abs().max((sy - y).abs());
        if dist <= reach && best.is_none_or(|b| dist < b.0) {
            best = Some((dist, i));
        }
    }
    best.map(|b| b.1)
}

/// The helper lines, handles and gizmo of `d`.
pub fn draw_decor(o: &mut Overlays, d: &Decor, proj: &Proj) {
    for &(a, b, color) in &d.lines {
        o.world.push(Line3 { a, b, style: LineStyle::overlay(color), id: 0 });
    }
    let h = 4.0 * d.scale.max(0.5);
    for (i, hd) in d.handles.iter().enumerate() {
        let Some((x, y, _)) = proj.to_screen(hd.pos) else { continue };
        let color = if d.hot_handle == Some(i) { HOT } else { hd.color };
        let q = [[x - h, y - h], [x + h, y - h], [x + h, y + h], [x - h, y + h]];
        for k in 0..4 {
            push(o, q[k], q[(k + 1) % 4], color);
            push(o, [q[k][0], q[k][1]], [q[(k + 2) % 4][0], q[(k + 2) % 4][1]], color);
        }
    }
    if let Some(g) = &d.gizmo {
        draw(o, g, proj);
    }
}

/// The parameter along the line through `p` with unit direction `a` closest to the ray
/// (`o`, `d`); None when they are nearly parallel.
pub fn line_param(p: Vec3, a: Vec3, o: Vec3, d: Vec3) -> Option<f32> {
    let b = a.dot(d);
    let denom = 1.0 - b * b;
    if denom < 1e-4 {
        return None;
    }
    let w = p - o;
    Some((b * d.dot(w) - a.dot(w)) / denom)
}

/// Where the ray (`o`, `d`) meets the plane through `p` with normal `n`.
pub fn plane_hit(p: Vec3, n: Vec3, o: Vec3, d: Vec3) -> Option<Vec3> {
    let dn = d.dot(n);
    if dn.abs() < 1e-6 {
        return None;
    }
    let t = (p - o).dot(n) / dn;
    (t > 0.0 || t.is_finite()).then(|| o + d * t)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::view::{View, ViewKind};
    use pt_core::camera::CameraDesc;
    use pt_core::geometry::Aabb;

    fn proj() -> Proj {
        let cam = CameraDesc { position: Vec3::new(3.0, 2.0, 5.0), look_at: Vec3::ZERO, ..CameraDesc::default() };
        let b = Aabb { min: Vec3::splat(-1.0), max: Vec3::ONE };
        View::new(ViewKind::Perspective, &cam, &b).proj(400, 300, &b)
    }

    fn gizmo(tool: Tool) -> Gizmo {
        let axes = [Vec3::new(1.0, 0.0, 0.0), Vec3::new(0.0, 1.0, 0.0), Vec3::Z];
        Gizmo { tool, pivot: Vec3::ZERO, axes, hot: None, active: None, scale: 1.0 }
    }

    /// Points along each arm (and on each ring) hit that part.
    #[test]
    fn parts_are_hit_where_they_are_drawn() {
        let p = proj();
        let g = gizmo(Tool::Move);
        let s = size(&p, g.pivot, 1.0);
        for k in 0..3 {
            let (x, y, _) = p.to_screen(g.axes[k] * (s * 0.7)).unwrap();
            assert_eq!(hit(&g, &p, x, y), Some(Part::Axis(k)), "axis {k}");
            let (u, v) = (g.axes[(k + 1) % 3], g.axes[(k + 2) % 3]);
            let (x, y, _) = p.to_screen((u + v) * (s * 0.32)).unwrap();
            assert_eq!(hit(&g, &p, x, y), Some(Part::Plane(k)), "plane {k}");
        }
        let (cx, cy, _) = p.to_screen(Vec3::ZERO).unwrap();
        assert_eq!(hit(&g, &p, cx, cy), Some(Part::Center));
        assert_eq!(hit(&g, &p, cx + 150.0, cy), None);
        let r = gizmo(Tool::Rotate);
        for k in 0..3 {
            // Halfway between the axes (where the rings cross each other).
            let (u, v) = (r.axes[(k + 1) % 3], r.axes[(k + 2) % 3]);
            let (x, y, _) = p.to_screen((u + v) * (s * std::f32::consts::FRAC_1_SQRT_2)).unwrap();
            assert_eq!(hit(&r, &p, x, y), Some(Part::Ring(k)), "ring {k}");
        }
        let mut o = Overlays::default();
        draw(&mut o, &r, &p);
        assert!(o.screen.len() > 100);
    }

    #[test]
    fn rays_meet_lines_and_planes() {
        let o = Vec3::new(0.0, 5.0, 0.0);
        let d = Vec3::new(0.0, -1.0, 0.0);
        let t = line_param(Vec3::new(-3.0, 0.0, 0.0), Vec3::new(1.0, 0.0, 0.0), o, d).unwrap();
        assert!((t - 3.0).abs() < 1e-5);
        let h = plane_hit(Vec3::ZERO, Vec3::new(0.0, 1.0, 0.0), o, d).unwrap();
        assert!(h.length() < 1e-5);
        assert!(line_param(Vec3::ZERO, d, o, d).is_none());
    }
}
