//! Helpers drawn over the scene: the ground grid (adapting its spacing to the zoom),
//! world axes, the axis indicator in the corner, the scene camera's frustum, light icons
//! and their cones and directions, and bounding boxes.

use crate::display::Display;
use crate::raster::LineStyle;
use crate::view::{Proj, ViewKind, sphere};
use pt_core::camera::Projection;
use pt_core::geometry::Aabb;
use pt_core::lights::Light;
use pt_core::math::{PI, Vec3};
use pt_core::scene::Scene;

/// Ids in the id buffer: objects are `index + 1`; lights set this bit over their index
/// among the `[[lights]]` entries.
pub const LIGHT_ID: u32 = 0x8000_0000;

pub const SELECTED: u32 = 0xff_9b_2f;
const LIGHT: u32 = 0xe8_d0_70;
const CAMERA: u32 = 0x8c_c4_ff;
const BOUNDS: u32 = 0x90_90_a0;
pub const AXIS_COLORS: [u32; 3] = [0xe0_50_50, 0x60_c0_50, 0x50_80_e8];

pub struct Line3 {
    pub a: Vec3,
    pub b: Vec3,
    pub style: LineStyle,
    pub id: u32,
}

/// A line in pixel coordinates.
pub struct Line2 {
    pub a: [f32; 2],
    pub b: [f32; 2],
    pub color: u32,
}

pub struct Label {
    pub x: i32,
    pub y: i32,
    pub text: String,
    pub color: u32,
}

#[derive(Default)]
pub struct Overlays {
    /// Depth-tested against the scene, drawn with the wireframes.
    pub grid: Vec<Line3>,
    /// Drawn after everything else.
    pub world: Vec<Line3>,
    pub screen: Vec<Line2>,
    pub labels: Vec<Label>,
}

/// What is selected, by `[[objects]]` and `[[lights]]` index.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Selection {
    pub objects: Vec<bool>,
    pub lights: Vec<bool>,
}

impl Selection {
    pub fn new(objects: usize, lights: usize) -> Selection {
        Selection { objects: vec![false; objects], lights: vec![false; lights] }
    }

    pub fn is_empty(&self) -> bool {
        !self.objects.iter().chain(&self.lights).any(|&s| s)
    }

    pub fn count(&self) -> usize {
        self.objects.iter().chain(&self.lights).filter(|&&s| s).count()
    }

    /// Whether the thing with id-buffer value `id` is selected.
    pub fn has_id(&self, id: u32) -> bool {
        match id {
            0 => false,
            _ if id & LIGHT_ID != 0 => self.lights.get((id & !LIGHT_ID) as usize).copied().unwrap_or(false),
            _ => self.objects.get(id as usize - 1).copied().unwrap_or(false),
        }
    }

    pub fn set_id(&mut self, id: u32, on: bool) {
        let slot = match id {
            0 => None,
            _ if id & LIGHT_ID != 0 => self.lights.get_mut((id & !LIGHT_ID) as usize),
            _ => self.objects.get_mut(id as usize - 1),
        };
        if let Some(s) = slot {
            *s = on;
        }
    }

    pub fn clear(&mut self) {
        self.objects.iter_mut().chain(self.lights.iter_mut()).for_each(|s| *s = false);
    }
}

fn style(color: u32, alpha: f32, bias: f32) -> LineStyle {
    LineStyle { color, alpha, depth_test: true, write: false, bias }
}

/// The normal of the grid plane a view shows: the ground, or the plane facing a side view.
pub fn grid_axis(kind: ViewKind) -> usize {
    match kind {
        ViewKind::Top | ViewKind::Perspective => 1,
        ViewKind::Front => 2,
        ViewKind::Side => 0,
    }
}

fn axis(k: usize) -> Vec3 {
    let mut v = Vec3::ZERO;
    match k {
        0 => v.x = 1.0,
        1 => v.y = 1.0,
        _ => v.z = 1.0,
    }
    v
}

/// Grid lines on the plane through the origin facing `grid_axis`, spaced by a power of
/// ten that follows the zoom, the finer lines fading out as they get dense, plus the world
/// axes in their colors.
pub fn grid(o: &mut Overlays, proj: &Proj, kind: ViewKind, target: Vec3) {
    let n = grid_axis(kind);
    let (u, v) = ((n + 1) % 3, (n + 2) % 3);
    let (au, av) = (axis(u), axis(v));
    // Half the visible height at the target: the scale the spacing follows.
    let size = if proj.ortho { proj.half_h } else { (target - proj.eye).length() * proj.tan_half }.max(1e-6);
    let l = (size / 2.0).log10();
    let major = 10f32.powf(l.floor() + 1.0);
    let minor = major / 10.0;
    let fade = 1.0 - (l - l.floor());
    // Extent: the view (orthographic), or far towards the horizon (perspective).
    let extent = if proj.ortho { size * (proj.aspect + 0.2) } else { size * 30.0 };
    let centre = target - axis(n) * target[n];
    let snap = |x: f32| (x / major).round() * major;
    let (cu, cv) = (snap(centre[u]), snap(centre[v]));
    // Perspective: minor lines only near the target, majors towards the horizon.
    let minor_extent = if proj.ortho { extent } else { extent / 5.0 };
    let steps = (minor_extent / minor).ceil() as i32;
    let major_steps = (extent / major).ceil() as i32;
    // Perspective grids fade with distance; lines are cut into pieces so they can.
    let pieces = if proj.ortho { 1 } else { 16 };
    let fade_at = |p: Vec3, lim: f32| {
        if proj.ortho { 1.0 } else { (1.0 - (p - centre).length() / lim).max(0.0).powi(2) }
    };
    let bias = -2e-4;
    let line = |o: &mut Overlays, along: Vec3, across: Vec3, c_along: f32, x: f32, lim: f32, color, alpha: f32| {
        for k in 0..pieces {
            let t0 = -lim + 2.0 * lim * k as f32 / pieces as f32;
            let t1 = t0 + 2.0 * lim / pieces as f32;
            let p = |t: f32| across * x + along * (c_along + t);
            let a = alpha * fade_at(p(f32::midpoint(t0, t1)), lim);
            if a > 0.02 {
                o.grid.push(Line3 { a: p(t0), b: p(t1), style: style(color, a, bias), id: 0 });
            }
        }
    };
    for ((c_along, c_across), (a_along, a_across)) in [((cv, cu), (av, au)), ((cu, cv), (au, av))] {
        let on_axis = |x: f32| x.abs() < minor * 0.01;
        for i in -major_steps..=major_steps {
            let x = c_across + i as f32 * major;
            if !on_axis(x) {
                line(o, a_along, a_across, c_along, x, extent, 0x7a_7a_80, 0.55);
            }
        }
        if fade > 0.05 {
            for i in -steps..=steps {
                let x = c_across + i as f32 * minor;
                if !on_axis(x) && ((x / major).round() * major - x).abs() > minor * 0.01 {
                    line(o, a_along, a_across, c_along, x, minor_extent, 0x6a_6a_70, 0.45 * fade);
                }
            }
        }
    }
    // World axes lying in the plane.
    for k in [u, v] {
        let a = axis(k);
        for piece in 0..pieces.max(2) {
            let t0 = -extent + 2.0 * extent * piece as f32 / pieces.max(2) as f32;
            let t1 = t0 + 2.0 * extent / pieces.max(2) as f32;
            let p = |t: f32| a * (centre[k] + t);
            let alpha = 0.9 * fade_at(p(f32::midpoint(t0, t1)), extent).sqrt();
            if alpha > 0.02 {
                o.grid.push(Line3 { a: p(t0), b: p(t1), style: style(AXIS_COLORS[k], alpha, bias), id: 0 });
            }
        }
    }
}

/// The world axes as seen by the view, in the bottom-left corner of a `w`x`h` view.
pub fn axis_indicator(o: &mut Overlays, proj: &Proj, scale: i32) {
    let len = 22.0 * scale as f32;
    let c = [len + 14.0, proj.h as f32 - len - 14.0];
    let mut axes: Vec<(f32, usize)> = (0..3).map(|k| (proj.fwd[k], k)).collect();
    // Far axes first so near ones draw over them.
    axes.sort_by(|a, b| b.0.total_cmp(&a.0));
    for (_, k) in axes {
        let a = axis(k);
        let d = [proj.right.dot(a), -proj.up.dot(a)];
        let end = [c[0] + d[0] * len, c[1] + d[1] * len];
        o.screen.push(Line2 { a: c, b: end, color: AXIS_COLORS[k] });
        let (cw, ch) = (crate::paint::CELL.0 as f32 * scale as f32, crate::paint::CELL.1 as f32 * scale as f32);
        o.labels.push(Label {
            x: (end[0] + d[0] * 8.0 - cw * 0.4) as i32,
            y: (end[1] + d[1] * 8.0 - ch * 0.4) as i32,
            text: ["X", "Y", "Z"][k].into(),
            color: AXIS_COLORS[k],
        });
    }
}

fn wire(o: &mut Overlays, a: Vec3, b: Vec3, color: u32, id: u32) {
    o.world.push(Line3 { a, b, style: LineStyle::overlay(color), id });
}

pub fn wire_box(o: &mut Overlays, b: &Aabb, color: u32, id: u32) {
    if b.is_empty() {
        return;
    }
    let corner = |i: u32| {
        Vec3::new(
            if i & 1 == 0 { b.min.x } else { b.max.x },
            if i & 2 == 0 { b.min.y } else { b.max.y },
            if i & 4 == 0 { b.min.z } else { b.max.z },
        )
    };
    for i in 0..8u32 {
        for bit in [1, 2, 4] {
            if i & bit == 0 {
                wire(o, corner(i), corner(i | bit), color, id);
            }
        }
    }
}

/// The scene camera: a pyramid from the eye to the image rectangle at the look-at
/// distance, with a triangle marking up.
/// Not drawn in a view looking from the camera itself.
pub fn camera(o: &mut Overlays, proj: &Proj, scene: &Scene, display: &Display) {
    let cam = &scene.camera;
    let fwd = cam.look_at - cam.position;
    let (_, r) = sphere(&display.bounds);
    if !proj.ortho && (proj.eye - cam.position).length() < 1e-3 * r {
        return;
    }
    let dist = cam.focus_distance.unwrap_or_else(|| fwd.length()).clamp(1e-3, r.max(1e-3) * 0.6);
    let Some(fwd) = (fwd.length() > 0.0).then(|| fwd.normalize()) else { return };
    let right = fwd.cross(cam.up).normalize();
    if !right.is_finite() {
        return;
    }
    let up = right.cross(fwd);
    let aspect = scene.settings.width as f32 / scene.settings.height.max(1) as f32;
    let (hw, hh, apex) = if cam.projection == Projection::Orthographic {
        let h = cam.ortho_height.unwrap_or(2.0 * (cam.fov.to_radians() * 0.5).tan() * fwd.length()) * 0.5;
        (h * aspect, h, None)
    } else {
        let t = (cam.fov.min(160.0).to_radians() * 0.5).tan() * dist;
        (t * aspect, t, Some(cam.position))
    };
    let c = cam.position + fwd * dist;
    let corners =
        [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)].map(|(x, y)| c + right * (x * hw) + up * (y * hh));
    for i in 0..4 {
        wire(o, corners[i], corners[(i + 1) % 4], CAMERA, 0);
        let from = apex.unwrap_or(cam.position + right * (corners[i] - c).dot(right) + up * (corners[i] - c).dot(up));
        wire(o, from, corners[i], CAMERA, 0);
    }
    let top = c + up * (hh * 1.1);
    let tip = c + up * (hh * 1.1 + hw * 0.35);
    wire(o, top - right * (hw * 0.3), tip, CAMERA, 0);
    wire(o, tip, top + right * (hw * 0.3), CAMERA, 0);
    wire(o, top + right * (hw * 0.3), top - right * (hw * 0.3), CAMERA, 0);
}

/// Where a light's icon sits: its position, or for distant lights a point beyond the
/// scene in the light's direction.
pub fn light_anchor(light: &Light, display: &Display) -> Option<Vec3> {
    match light {
        Light::Point { pos, .. } | Light::Spot { pos, .. } => Some(*pos),
        Light::Distant { dir, .. } => {
            let (c, r) = sphere(&display.bounds);
            Some(c + *dir * (r * 1.15))
        }
        _ => None,
    }
}

/// Light icons (screen-sized), spot cones and distant-light directions. The `[[lights]]`
/// entries carry their ids; a sky's sun is drawn but cannot be selected.
pub fn lights(o: &mut Overlays, proj: &Proj, scene: &Scene, display: &Display, sel: &Selection, scale: i32) {
    let (_, r) = sphere(&display.bounds);
    let entries = scene.provenance.lights.clone();
    for (i, light) in scene.lights().iter().enumerate() {
        let entry = entries.contains(&(i as u32)).then(|| i as u32 - entries.start);
        let id = entry.map_or(0, |j| LIGHT_ID | j);
        let color = if sel.has_id(id) { SELECTED } else { LIGHT };
        let Some(anchor) = light_anchor(light, display) else { continue };
        match light {
            Light::Spot { pos, dir, cos_outer, .. } => {
                let len = r * 0.3;
                let tip = *pos + *dir * len;
                let radius = len * cos_outer.clamp(-1.0, 1.0).acos().min(1.5).tan();
                let f = pt_core::math::Frame::from_normal(*dir);
                let ring = |k: usize| {
                    let a = 2.0 * PI * k as f32 / 16.0;
                    tip + (f.t * a.cos() + f.b * a.sin()) * radius
                };
                for k in 0..16 {
                    wire(o, ring(k), ring(k + 1), color, id);
                    if k % 4 == 0 {
                        wire(o, *pos, ring(k), color, id);
                    }
                }
            }
            Light::Distant { dir, .. } => {
                // Parallel rays from the icon towards the scene.
                wire(o, anchor, anchor - *dir * (r * 0.5), color, id);
            }
            _ => {}
        }
        let Some((x, y, _)) = proj.to_screen(anchor) else { continue };
        let s = 5.0 * scale as f32;
        let circle = |k: usize| {
            let a = 2.0 * PI * k as f32 / 12.0;
            [x + a.cos() * s, y + a.sin() * s]
        };
        for k in 0..12 {
            o.screen.push(Line2 { a: circle(k), b: circle(k + 1), color });
        }
        for k in 0..8 {
            let a = 2.0 * PI * k as f32 / 8.0;
            let (c, sn) = (a.cos(), a.sin());
            o.screen.push(Line2 {
                a: [x + c * s * 1.5, y + sn * s * 1.5],
                b: [x + c * s * 2.2, y + sn * s * 2.2],
                color,
            });
        }
    }
}

/// Bounding boxes of the selected objects (and of every object with `all`).
pub fn bounds(o: &mut Overlays, display: &Display, sel: &Selection, all: bool) {
    for (i, b) in display.object_bounds.iter().enumerate() {
        let selected = sel.objects.get(i).copied().unwrap_or(false);
        if selected || all {
            wire_box(o, b, if selected { SELECTED } else { BOUNDS }, i as u32 + 1);
        }
    }
}
