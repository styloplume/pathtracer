//! Where an object or light is, and how a move, rotation or scale of it is written back in
//! the scene's own conventions: an object with a `translate` key gets its `translate`
//! changed; a sphere placed by its `center` (and nothing else) has its `center` moved; a
//! box given by `min` / `max` has both moved; a light its `position`, its `direction` or
//! its `elevation` / `azimuth`, whichever it is written with. Rotations are written as
//! `rotate` (degrees about X, then Y, then Z) close to the angles already there, and a
//! uniform `scale` written as one number stays one number.
//!
//! Every edit is computed from the item's table as it was when the gesture started, so a
//! drag can be rewritten each frame without drifting.

use pt_core::document::Kind;
use pt_core::geometry::Aabb;
use pt_core::math::{Frame, Mat3, Transform, Vec3};
use pt_core::toml::{Table, Value};

/// A change of the item's keys (relative to the item's table).
#[derive(Clone, Debug, PartialEq)]
pub enum Op {
    Set(String, Value),
    Remove(String),
}

/// What the gizmo does, in world space.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Gesture {
    Move(Vec3),
    /// About `axis` (unit length) through the pivot, in degrees (right-handed).
    Rotate {
        axis: Vec3,
        degrees: f32,
    },
    /// Factors along the item's local axes, about the pivot.
    Scale(Vec3),
}

/// An item's pivot and local axes, and what can be done to it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placement {
    pub pivot: Vec3,
    /// Unit local axes (columns of the item's rotation).
    pub axes: [Vec3; 3],
    pub can_move: bool,
    pub can_rotate: bool,
    pub can_scale: bool,
}

/// The world map of `g` about `pivot`, with local axes `axes` (for scales).
pub fn gesture_map(g: &Gesture, pivot: Vec3, axes: &[Vec3; 3]) -> Transform {
    let about = |m: Mat3| {
        // x -> m (x - p) + p
        Transform::from_matrix(m, pivot - m.mul_vec(pivot)).unwrap_or(Transform::IDENTITY)
    };
    match *g {
        Gesture::Move(d) => Transform::translation(d),
        Gesture::Rotate { axis, degrees } => about(axis_angle(axis, degrees)),
        Gesture::Scale(k) => {
            let r = columns(axes);
            about(r.mul(&Mat3::scale(k)).mul(&r.transpose()))
        }
    }
}

fn columns(a: &[Vec3; 3]) -> Mat3 {
    Mat3 { m: [[a[0].x, a[1].x, a[2].x], [a[0].y, a[1].y, a[2].y], [a[0].z, a[1].z, a[2].z]] }
}

/// Rotation by `degrees` about unit `axis` (Rodrigues).
pub fn axis_angle(axis: Vec3, degrees: f32) -> Mat3 {
    let (s, c) = degrees.to_radians().sin_cos();
    let (x, y, z) = (axis.x, axis.y, axis.z);
    let t = 1.0 - c;
    Mat3 {
        m: [
            [t * x * x + c, t * x * y - s * z, t * x * z + s * y],
            [t * x * y + s * z, t * y * y + c, t * y * z - s * x],
            [t * x * z - s * y, t * y * z + s * x, t * z * z + c],
        ],
    }
}

/// Angles (degrees, about X, then Y, then Z) of rotation `m`, of the two equivalent sets
/// the one closest to `near` (each angle shifted by whole turns towards it).
pub fn euler_xyz(m: &Mat3, near: Vec3) -> Vec3 {
    let r = &m.m;
    let r20 = r[2][0].clamp(-1.0, 1.0);
    let b = (-r20).asin();
    let (a, c) = if r20.abs() < 0.999_99 {
        (r[2][1].atan2(r[2][2]), r[1][0].atan2(r[0][0]))
    } else {
        // Gimbal lock: only a ± c is defined; keep c.
        ((-r[1][2]).atan2(r[1][1]), 0.0)
    };
    let first = Vec3::new(a.to_degrees(), b.to_degrees(), c.to_degrees());
    let second = Vec3::new(first.x + 180.0, 180.0 - first.y, first.z + 180.0);
    let wrap = |v: f32, to: f32| v + ((to - v) / 360.0).round() * 360.0;
    let fit = |e: Vec3| Vec3::new(wrap(e.x, near.x), wrap(e.y, near.y), wrap(e.z, near.z));
    let (p, q) = (fit(first), fit(second));
    let cost = |e: Vec3| (e - near).abs().dot(Vec3::ONE);
    if cost(q) < cost(p) { q } else { p }
}

/// `x` rounded to 6 significant digits (no float noise like 0.30000001 in files).
pub fn tidy(x: f32) -> f64 {
    let x = f64::from(x);
    if x.abs() < 1e-7 || !x.is_finite() {
        return 0.0;
    }
    let digits = (5 - x.abs().log10().floor() as i32).clamp(0, 9);
    let k = 10f64.powi(digits);
    (x * k).round() / k
}

pub fn vec_value(v: Vec3) -> Value {
    Value::Array(vec![Value::Number(tidy(v.x)), Value::Number(tidy(v.y)), Value::Number(tidy(v.z))])
}

/// A vector written as `[x, y, z]` or one number for all three.
pub fn read_vec(t: &Table, key: &str) -> Option<Vec3> {
    match t.get(key)? {
        Value::Number(n) => Some(Vec3::splat(*n as f32)),
        Value::Array(a) if a.len() == 3 => {
            let n: Vec<f32> = a.iter().filter_map(Value::as_f64).map(|x| x as f32).collect();
            (n.len() == 3).then(|| Vec3::new(n[0], n[1], n[2]))
        }
        _ => None,
    }
}

fn read_num(t: &Table, key: &str) -> Option<f32> {
    t.get(key).and_then(Value::as_f64).map(|x| x as f32)
}

/// The object's type as the loader reads it.
pub fn object_type(t: &Table) -> &str {
    match t.get("type").and_then(Value::as_str) {
        Some(ty) => ty,
        None if t.contains_key("file") => "mesh",
        None => "",
    }
}

/// Keys that place an object's shape before its transform, by type.
fn point_key(ty: &str, t: &Table) -> Option<&'static str> {
    match ty {
        "sphere" | "disk" | "disc" => Some("center"),
        "cylinder" | "tube" => Some("base"),
        "box" | "cube" if !t.contains_key("min") && !t.contains_key("max") => Some("center"),
        "quad" => Some("origin"),
        _ => None,
    }
}

/// The point point keys of `ty` can be folded into `translate` (a quad's origin cannot:
/// it is required).
fn convertible(key: &str) -> bool {
    key != "origin"
}

struct ObjectForm {
    ty: String,
    translate: Option<Vec3>,
    rotate: Option<Vec3>,
    scale: Option<Vec3>,
    /// Whether `scale` is written as one number.
    scale_number: bool,
}

impl ObjectForm {
    fn read(t: &Table) -> ObjectForm {
        ObjectForm {
            ty: object_type(t).to_owned(),
            translate: read_vec(t, "translate"),
            rotate: read_vec(t, "rotate"),
            scale: read_vec(t, "scale"),
            scale_number: matches!(t.get("scale"), Some(Value::Number(_))),
        }
    }

    fn rot(&self) -> Mat3 {
        Mat3::rotate_xyz(self.rotate.unwrap_or(Vec3::ZERO))
    }

    fn m(&self) -> (Mat3, Vec3) {
        let s = self.scale.unwrap_or(Vec3::ONE);
        (self.rot().mul(&Mat3::scale(s)), self.translate.unwrap_or(Vec3::ZERO))
    }

    fn plain(&self) -> bool {
        self.translate.is_none() && self.rotate.is_none() && self.scale.is_none()
    }
}

/// The middle of the object's shape before its transform, from its own keys.
fn local_center(ty: &str, t: &Table) -> Option<Vec3> {
    let v = |k: &str| read_vec(t, k);
    match ty {
        "box" | "cube" => match (v("min"), v("max")) {
            (Some(a), Some(b)) => Some((a + b) * 0.5),
            _ => Some(v("center").unwrap_or(Vec3::ZERO)),
        },
        "quad" => Some(v("origin")? + (v("u")? + v("v")?) * 0.5),
        "triangle" => Some((v("p0")? + v("p1")? + v("p2")?) * (1.0 / 3.0)),
        "cylinder" | "tube" => {
            let axis = v("axis").unwrap_or(Vec3::new(0.0, 1.0, 0.0));
            let axis = if axis.length_squared() > 0.0 { axis.normalize() } else { Vec3::new(0.0, 1.0, 0.0) };
            Some(v("base").unwrap_or(Vec3::ZERO) + axis * (read_num(t, "height").unwrap_or(1.0) * 0.5))
        }
        "sphere" | "disk" | "disc" => Some(v("center").unwrap_or(Vec3::ZERO)),
        _ => None,
    }
}

fn frame_axes(dir: Vec3) -> [Vec3; 3] {
    let f = Frame::from_normal(dir);
    [f.t, f.b, f.n]
}

/// Where a light's direction is written, and the direction (unit) it gives.
fn light_dir(t: &Table) -> Option<Vec3> {
    let ty = t.get("type").and_then(Value::as_str).unwrap_or("");
    match ty {
        "spot" | "spotlight" => match (read_vec(t, "direction"), read_vec(t, "look_at"), read_vec(t, "position")) {
            (Some(d), ..) => Some(d),
            (None, Some(a), Some(p)) => Some(a - p),
            _ => None,
        }
        .filter(|d| d.length_squared() > 0.0)
        .map(Vec3::normalize),
        "directional" | "distant" | "sun" => Some(match read_vec(t, "direction") {
            Some(d) if d.length_squared() > 0.0 => d.normalize(),
            _ => -from_angles(read_num(t, "elevation").unwrap_or(45.0), read_num(t, "azimuth").unwrap_or(0.0)),
        }),
        _ => None,
    }
}

/// The direction towards a light at `elevation` / `azimuth` (degrees; as the loader).
pub fn from_angles(elevation: f32, azimuth: f32) -> Vec3 {
    let (se, ce) = elevation.to_radians().sin_cos();
    let (sa, ca) = azimuth.to_radians().sin_cos();
    Vec3::new(sa * ce, se, -ca * ce)
}

/// Elevation and azimuth (degrees) of the direction `d` towards a light, the azimuth near
/// `near_azimuth` (whole turns added).
pub fn to_angles(d: Vec3, near_azimuth: f32) -> (f32, f32) {
    let d = d.normalize();
    let el = d.y.clamp(-1.0, 1.0).asin().to_degrees();
    let az = if d.x.abs() + d.z.abs() < 1e-6 { near_azimuth } else { d.x.atan2(-d.z).to_degrees() };
    (el, az + ((near_azimuth - az) / 360.0).round() * 360.0)
}

/// Where the item is and what can be done to it. `anchor` is where a directional light's
/// icon is (it has no position of its own); `bounds` the object's world bounds.
pub fn placement(kind: Kind, t: &Table, bounds: &Aabb, anchor: Option<Vec3>) -> Option<Placement> {
    match kind {
        Kind::Object => {
            let f = ObjectForm::read(t);
            if f.ty.is_empty() {
                return None;
            }
            let (m, tr) = f.m();
            let pivot = match local_center(&f.ty, t) {
                Some(c) => m.mul_vec(c) + tr,
                None if f.translate.is_some() || bounds.is_empty() => tr,
                // Meshes, implicit surfaces: their origin, unless they are far from it.
                None => {
                    let c = bounds.centroid();
                    if (c - tr).length() <= (bounds.max - bounds.min).length() { tr } else { c }
                }
            };
            let r = f.rot();
            let axes = [r.col(0), r.col(1), r.col(2)].map(Vec3::normalize);
            Some(Placement { pivot, axes, can_move: true, can_rotate: true, can_scale: true })
        }
        Kind::Light => {
            let ty = t.get("type").and_then(Value::as_str).unwrap_or("");
            let dir = light_dir(t);
            let axes = dir.map_or([Vec3::new(1.0, 0.0, 0.0), Vec3::new(0.0, 1.0, 0.0), Vec3::Z], frame_axes);
            match ty {
                "point" => Some(Placement {
                    pivot: read_vec(t, "position")?,
                    axes,
                    can_move: true,
                    can_rotate: false,
                    can_scale: false,
                }),
                "spot" | "spotlight" => Some(Placement {
                    pivot: read_vec(t, "position")?,
                    axes,
                    can_move: true,
                    can_rotate: dir.is_some(),
                    can_scale: false,
                }),
                "directional" | "distant" | "sun" => {
                    Some(Placement { pivot: anchor?, axes, can_move: false, can_rotate: true, can_scale: false })
                }
                _ => None,
            }
        }
        _ => None,
    }
}

/// The edits that do `g` to the item whose table is `t` (as it was when the gesture
/// started); `pivot` is its placement's pivot and `axes` its local axes.
pub fn edits(kind: Kind, t: &Table, g: &Gesture, pivot: Vec3, axes: &[Vec3; 3]) -> Vec<Op> {
    match kind {
        Kind::Object => object_edits(t, g, pivot),
        Kind::Light => light_edits(t, g, pivot, axes),
        _ => Vec::new(),
    }
}

fn set(out: &mut Vec<Op>, key: &str, v: Value) {
    out.push(Op::Set(key.to_owned(), v));
}

fn object_edits(t: &Table, g: &Gesture, pivot: Vec3) -> Vec<Op> {
    let f = ObjectForm::read(t);
    let mut out = Vec::new();
    let point = point_key(&f.ty, t);
    match *g {
        Gesture::Move(d) => {
            if let Some(tr) = f.translate {
                set(&mut out, "translate", vec_value(tr + d));
            } else if f.rotate.is_none() && f.scale.is_none() {
                match (point, f.ty.as_str()) {
                    (Some(k), _) => set(&mut out, k, vec_value(read_vec(t, k).unwrap_or(Vec3::ZERO) + d)),
                    (None, "box" | "cube") => {
                        for k in ["min", "max"] {
                            if let Some(v) = read_vec(t, k) {
                                set(&mut out, k, vec_value(v + d));
                            }
                        }
                    }
                    (None, "triangle") => {
                        for k in ["p0", "p1", "p2"] {
                            if let Some(v) = read_vec(t, k) {
                                set(&mut out, k, vec_value(v + d));
                            }
                        }
                    }
                    _ => set(&mut out, "translate", vec_value(d)),
                }
            } else {
                set(&mut out, "translate", vec_value(d));
            }
        }
        Gesture::Rotate { .. } | Gesture::Scale(_) => {
            // A shape placed by its own point: that point becomes the translation, so it
            // turns and grows about itself and the file reads as a person would write it.
            let (mut tr, mut rot, scale) =
                (f.translate.unwrap_or(Vec3::ZERO), f.rotate.unwrap_or(Vec3::ZERO), f.scale.unwrap_or(Vec3::ONE));
            let mut write_t = f.translate.is_some();
            if let Some(k) = point.filter(|&k| convertible(k))
                && f.plain()
            {
                let c = read_vec(t, k).unwrap_or(Vec3::ZERO);
                if t.contains_key(k) {
                    out.push(Op::Remove(k.to_owned()));
                }
                tr = c;
                write_t = c != Vec3::ZERO;
            }
            let r = Mat3::rotate_xyz(rot);
            let moved = |t2: Vec3| write_t || (t2 - tr).length() > 1e-6 * (1.0 + tr.length());
            // Written translate, rotate, scale: the order people read them in.
            match *g {
                Gesture::Rotate { axis, degrees } => {
                    let q = axis_angle(axis, degrees);
                    let t2 = q.mul_vec(tr - pivot) + pivot;
                    if moved(t2) {
                        set(&mut out, "translate", vec_value(t2));
                    }
                    rot = euler_xyz(&q.mul(&r), rot);
                    set(&mut out, "rotate", vec_value(rot));
                }
                Gesture::Scale(k) => {
                    let s2 = scale * k;
                    let rk = r.mul(&Mat3::scale(k)).mul(&r.transpose());
                    let t2 = pivot + rk.mul_vec(tr - pivot);
                    if moved(t2) {
                        set(&mut out, "translate", vec_value(t2));
                    }
                    let uniform = (s2.x - s2.y).abs() <= 1e-6 * s2.x.abs() && (s2.x - s2.z).abs() <= 1e-6 * s2.x.abs();
                    let as_number = uniform && (f.scale.is_none() || f.scale_number);
                    set(&mut out, "scale", if as_number { Value::Number(tidy(s2.x)) } else { vec_value(s2) });
                }
                Gesture::Move(_) => unreachable!(),
            }
        }
    }
    out
}

fn light_edits(t: &Table, g: &Gesture, pivot: Vec3, _axes: &[Vec3; 3]) -> Vec<Op> {
    let ty = t.get("type").and_then(Value::as_str).unwrap_or("");
    let mut out = Vec::new();
    let directional = matches!(ty, "directional" | "distant" | "sun");
    match *g {
        Gesture::Move(d) if !directional => {
            if let Some(p) = read_vec(t, "position") {
                set(&mut out, "position", vec_value(p + d));
            }
            if let Some(a) = read_vec(t, "look_at") {
                set(&mut out, "look_at", vec_value(a + d));
            }
        }
        Gesture::Rotate { axis, degrees } => {
            let q = axis_angle(axis, degrees);
            let Some(dir) = light_dir(t) else { return out };
            let new = q.mul_vec(dir).normalize();
            if directional {
                if t.contains_key("direction") {
                    set(&mut out, "direction", vec_value(new));
                } else {
                    let (el, az) = to_angles(-new, read_num(t, "azimuth").unwrap_or(0.0));
                    set(&mut out, "elevation", Value::Number(tidy(el)));
                    set(&mut out, "azimuth", Value::Number(tidy(az)));
                }
            } else if let (Some(a), false) = (read_vec(t, "look_at"), t.contains_key("direction")) {
                set(&mut out, "look_at", vec_value(pivot + q.mul_vec(a - pivot)));
            } else {
                set(&mut out, "direction", vec_value(new));
            }
        }
        _ => {}
    }
    out
}

/// Sets a light's direction (towards where it shines, for spots; the direction its light
/// travels, for directional lights) in the form the light is written with.
pub fn aim_light(t: &Table, dir: Vec3) -> Vec<Op> {
    let ty = t.get("type").and_then(Value::as_str).unwrap_or("");
    let mut out = Vec::new();
    let dir = dir.normalize();
    if matches!(ty, "directional" | "distant" | "sun") && !t.contains_key("direction") {
        let (el, az) = to_angles(-dir, read_num(t, "azimuth").unwrap_or(0.0));
        set(&mut out, "elevation", Value::Number(tidy(el)));
        set(&mut out, "azimuth", Value::Number(tidy(az)));
    } else if let (Some(a), Some(p), false) =
        (read_vec(t, "look_at"), read_vec(t, "position"), t.contains_key("direction"))
    {
        let len = (a - p).length().max(1e-3);
        set(&mut out, "look_at", vec_value(p + dir * len));
    } else {
        set(&mut out, "direction", vec_value(dir));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(src: &str) -> Table {
        pt_core::toml::parse(src).unwrap()
    }

    fn close(a: Vec3, b: Vec3) -> bool {
        (a - b).length() < 1e-3
    }

    fn apply(t: &Table, ops: &[Op]) -> Table {
        let mut t = t.clone();
        for op in ops {
            match op {
                Op::Set(k, v) => {
                    t.insert(k.clone(), v.clone());
                }
                Op::Remove(k) => {
                    t.remove(k);
                }
            }
        }
        t
    }

    /// The world transform the loader gives an object written as `t` (translate, rotate,
    /// scale after the shape's own point).
    fn world(t: &Table, local: Vec3) -> Vec3 {
        let f = ObjectForm::read(t);
        let (m, tr) = f.m();
        let c = point_key(&f.ty, t).and_then(|k| read_vec(t, k)).unwrap_or(Vec3::ZERO);
        m.mul_vec(c + local) + tr
    }

    #[test]
    fn euler_angles_round_trip_near_the_old_ones() {
        for e in [Vec3::new(10.0, 20.0, 30.0), Vec3::new(-170.0, 80.0, 45.0), Vec3::new(0.0, 90.0, 0.0)] {
            let m = Mat3::rotate_xyz(e);
            let back = euler_xyz(&m, e);
            let m2 = Mat3::rotate_xyz(back);
            for i in 0..3 {
                assert!(close(m.col(i), m2.col(i)), "{e:?} -> {back:?}");
            }
        }
        // Spinning past 180 keeps counting up rather than jumping to -179.
        let near = Vec3::new(0.0, 179.0, 0.0);
        let e = euler_xyz(&Mat3::rotate_y(182.0), near);
        assert!((e.y - 182.0).abs() < 1e-3 || (e.x.abs() > 170.0), "{e:?}");
        assert_eq!(tidy(0.1 + 0.2), 0.3);
        assert_eq!(tidy(29.999_998), 30.0);
    }

    #[test]
    fn moves_keep_the_way_objects_are_written() {
        let sphere = table("type = \"sphere\"\ncenter = [1, 0, 0]\n");
        let ops = edits(Kind::Object, &sphere, &Gesture::Move(Vec3::new(0.5, 1.0, 0.0)), Vec3::ZERO, &[Vec3::ZERO; 3]);
        assert_eq!(ops, [Op::Set("center".into(), vec_value(Vec3::new(1.5, 1.0, 0.0)))]);
        let mesh = table("file = \"a.obj\"\nrotate = [0, 90, 0]\n");
        let ops = edits(Kind::Object, &mesh, &Gesture::Move(Vec3::new(1.0, 0.0, 0.0)), Vec3::ZERO, &[Vec3::ZERO; 3]);
        assert_eq!(ops, [Op::Set("translate".into(), vec_value(Vec3::new(1.0, 0.0, 0.0)))]);
        let bx = table("type = \"box\"\nmin = [0, 0, 0]\nmax = [1, 1, 1]\n");
        let ops = edits(Kind::Object, &bx, &Gesture::Move(Vec3::new(0.0, 2.0, 0.0)), Vec3::ZERO, &[Vec3::ZERO; 3]);
        assert_eq!(ops.len(), 2);
        let light = table("type = \"spot\"\nposition = [0, 2, 0]\nlook_at = [0, 0, 0]\n");
        let ops = edits(Kind::Light, &light, &Gesture::Move(Vec3::new(1.0, 0.0, 0.0)), Vec3::ZERO, &[Vec3::ZERO; 3]);
        assert_eq!(ops.len(), 2, "the spot keeps pointing the same way");
    }

    /// Rotations and scales about the pivot: every point of the object ends where the
    /// gesture's world map takes it.
    #[test]
    fn rotations_and_scales_follow_the_gesture() {
        let cases = [
            "type = \"sphere\"\ncenter = [1, 2, 3]\n",
            "type = \"box\"\ncenter = [0, 1, 0]\nsize = [1, 2, 3]\ntranslate = [2, 0, 0]\nrotate = [10, 20, 30]\nscale = 2\n",
            "file = \"m.obj\"\ntranslate = [0, 0, 1]\nscale = [1, 2, 1]\n",
            "type = \"quad\"\norigin = [0, 0, 0]\nu = [1, 0, 0]\nv = [0, 0, 1]\n",
        ];
        let gestures = [
            Gesture::Rotate { axis: Vec3::new(0.0, 1.0, 0.0), degrees: 37.0 },
            Gesture::Rotate { axis: Vec3::new(1.0, 1.0, 0.0).normalize(), degrees: -120.0 },
            Gesture::Scale(Vec3::new(2.0, 2.0, 2.0)),
            Gesture::Scale(Vec3::new(1.0, 0.5, 3.0)),
        ];
        for src in cases {
            let t = table(src);
            let p = placement(Kind::Object, &t, &Aabb::EMPTY, None).unwrap();
            for g in &gestures {
                let ops = edits(Kind::Object, &t, g, p.pivot, &p.axes);
                let after = apply(&t, &ops);
                let map = gesture_map(g, p.pivot, &p.axes);
                for local in [Vec3::ZERO, Vec3::new(0.3, -0.7, 0.2), Vec3::new(1.0, 1.0, -1.0)] {
                    let want = map.point(world(&t, local));
                    let got = world(&after, local);
                    assert!(close(want, got), "{src} {g:?}: {want:?} vs {got:?} ({ops:?})");
                }
            }
        }
        // The sphere turns about its center: its center becomes the translation.
        let t = table("type = \"sphere\"\ncenter = [1, 2, 3]\n");
        let ops = edits(Kind::Object, &t, &gestures[0], Vec3::new(1.0, 2.0, 3.0), &[Vec3::ZERO; 3]);
        assert!(ops.contains(&Op::Remove("center".into())));
        assert!(ops.contains(&Op::Set("translate".into(), vec_value(Vec3::new(1.0, 2.0, 3.0)))));
        // A uniform scale written as a number stays a number.
        let t = table("type = \"sphere\"\nscale = 2\n");
        let ops = edits(Kind::Object, &t, &Gesture::Scale(Vec3::splat(1.5)), Vec3::ZERO, &[Vec3::ZERO; 3]);
        assert_eq!(ops, [Op::Set("scale".into(), Value::Number(3.0))]);
    }

    #[test]
    fn lights_turn_in_their_own_terms() {
        let sun = table("type = \"directional\"\nelevation = 30\nazimuth = 10\n");
        let travel = light_dir(&sun).unwrap();
        let g = Gesture::Rotate { axis: Vec3::new(0.0, 1.0, 0.0), degrees: -20.0 };
        let ops = edits(Kind::Light, &sun, &g, Vec3::ZERO, &[Vec3::ZERO; 3]);
        let after = apply(&sun, &ops);
        assert!(after.contains_key("elevation") && !after.contains_key("direction"));
        assert!(close(light_dir(&after).unwrap(), axis_angle(Vec3::new(0.0, 1.0, 0.0), -20.0).mul_vec(travel)));
        assert_eq!(after["elevation"], Value::Number(30.0));
        let spot = table("type = \"spot\"\nposition = [0, 2, 0]\ndirection = [0, -1, 0]\n");
        let ops = aim_light(&spot, Vec3::new(1.0, -1.0, 0.0));
        let after = apply(&spot, &ops);
        assert!(close(light_dir(&after).unwrap(), Vec3::new(1.0, -1.0, 0.0).normalize()));
        let (el, az) = to_angles(from_angles(20.0, 370.0), 360.0);
        assert!((el - 20.0).abs() < 1e-3 && (az - 370.0).abs() < 1e-3, "{el} {az}");
    }
}
