//! Viewport cameras, separate from the scene's camera: a perspective view that orbits,
//! pans, dollies and flies with the tracer's `CameraController` (and can be shown
//! orthographically), and orthographic top, front and side views that pan and zoom.
//! A `Proj` is one view's camera for one frame: clip-space transform, rays for picking,
//! screen positions for overlays.

use pt_core::camera::CameraDesc;
use pt_core::controls::CameraController;
use pt_core::geometry::Aabb;
use pt_core::math::{Ray, Transform, Vec3};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ViewKind {
    Top,
    Front,
    Side,
    Perspective,
}

impl ViewKind {
    pub fn name(self) -> &'static str {
        match self {
            ViewKind::Top => "TOP",
            ViewKind::Front => "FRONT",
            ViewKind::Side => "RIGHT",
            ViewKind::Perspective => "PERSPECTIVE",
        }
    }

    /// (forward, up) of the fixed views; the scene is Y-up.
    fn axes(self) -> Option<(Vec3, Vec3)> {
        match self {
            ViewKind::Top => Some((Vec3::new(0.0, -1.0, 0.0), Vec3::new(0.0, 0.0, -1.0))),
            ViewKind::Front => Some((Vec3::new(0.0, 0.0, -1.0), Vec3::new(0.0, 1.0, 0.0))),
            ViewKind::Side => Some((Vec3::new(-1.0, 0.0, 0.0), Vec3::new(0.0, 1.0, 0.0))),
            ViewKind::Perspective => None,
        }
    }
}

pub struct View {
    pub kind: ViewKind,
    /// The perspective view's camera.
    ctl: CameraController,
    /// The perspective view drawn with an orthographic projection.
    pub ortho: bool,
    /// Fixed views: what the view is centred on, and half the visible height.
    center: Vec3,
    half_h: f32,
}

impl View {
    pub fn new(kind: ViewKind, camera: &CameraDesc, scene: &Aabb) -> View {
        let mut v = View { kind, ctl: CameraController::new(camera), ortho: false, center: Vec3::ZERO, half_h: 1.0 };
        if kind != ViewKind::Perspective {
            // What the scene camera looks at, with the camera in view (a huge ground plane
            // would make the whole scene's bounds useless).
            let r = (camera.position - camera.look_at).length();
            let (c, r) = if r.is_finite() && r > 0.0 { (camera.look_at, r) } else { sphere(scene) };
            v.frame(c, r);
        }
        v
    }

    /// Back to the scene camera's pose (perspective view) or what it looks at (fixed views).
    pub fn reset(&mut self, camera: &CameraDesc, scene: &Aabb) {
        let ortho = self.ortho;
        *self = View::new(self.kind, camera, scene);
        self.ortho = ortho;
    }

    pub fn is_fixed(&self) -> bool {
        self.kind != ViewKind::Perspective
    }

    pub fn is_ortho(&self) -> bool {
        self.is_fixed() || self.ortho
    }

    pub fn orbit(&mut self, dx: f32, dy: f32) {
        if !self.is_fixed() {
            self.ctl.orbit(dx, dy);
        }
    }

    pub fn look(&mut self, dx: f32, dy: f32) {
        if !self.is_fixed() {
            self.ctl.look(dx, dy);
        }
    }

    /// Moves the view so the point under the cursor follows it; `h` is the view's height.
    pub fn pan(&mut self, dx: f32, dy: f32, h: f32) {
        match self.kind.axes() {
            None => self.ctl.pan(dx, dy, h),
            Some((fwd, up)) => {
                let right = fwd.cross(up);
                self.center += (up * dy - right * dx) * (2.0 * self.half_h / h.max(1.0));
            }
        }
    }

    /// Wheel: dolly (perspective) or zoom (fixed views).
    pub fn zoom(&mut self, notches: f32) {
        if self.is_fixed() {
            self.half_h = (self.half_h * 0.85f32.powf(notches)).clamp(1e-4, 1e7);
        } else {
            self.ctl.dolly(notches);
        }
    }

    /// Moves by `local` = (right, up, forward) world units (perspective view only).
    pub fn fly(&mut self, local: Vec3) {
        if !self.is_fixed() {
            self.ctl.fly(local);
        }
    }

    /// Shows the sphere around `center` with `radius`.
    pub fn frame(&mut self, center: Vec3, radius: f32) {
        let radius = if radius.is_finite() && radius > 0.0 { radius } else { 1.0 };
        if self.is_fixed() {
            self.center = center;
            self.half_h = radius * 1.1;
        } else {
            self.ctl.frame(center, radius * 1.05);
        }
    }

    /// The view's camera for a `w`x`h` viewport, with depth range covering `scene`.
    pub fn proj(&self, w: usize, h: usize, scene: &Aabb) -> Proj {
        let aspect = w.max(1) as f32 / h.max(1) as f32;
        let (sc, sr) = sphere(scene);
        let (target, fwd, up, half_h, tan_half) = if let Some((fwd, up)) = self.kind.axes() {
            (self.center, fwd, up, self.half_h, 0.0)
        } else {
            let d = self.ctl.desc();
            let fwd = (d.look_at - d.position).normalize();
            let tan_half = (self.ctl.fov().to_radians() * 0.5).tan();
            (self.ctl.target(), fwd, d.up, self.ctl.distance() * tan_half, tan_half)
        };
        let right = fwd.cross(up).normalize();
        let up = right.cross(fwd);
        if self.is_ortho() {
            // Eye far enough back that the whole scene and the target are in front.
            let back = (sc - target).length() + sr + half_h + 1.0;
            let eye = target - fwd * back;
            let far = 2.0 * back + sr;
            return Proj { eye, right, up, fwd, ortho: true, half_h, tan_half, aspect, near: 0.0, far, w, h };
        }
        let eye = self.ctl.position();
        let near = (self.ctl.distance() * 1e-3).max(1e-5);
        Proj { eye, right, up, fwd, ortho: false, half_h, tan_half, aspect, near, far: f32::INFINITY, w, h }
    }
}

/// Bounding sphere of a box (the unit sphere for an empty box).
pub fn sphere(b: &Aabb) -> (Vec3, f32) {
    if b.is_empty() || !(b.max - b.min).is_finite() {
        return (Vec3::ZERO, 1.0);
    }
    (b.centroid(), ((b.max - b.min).length() * 0.5).max(1e-3))
}

/// One view's camera for one frame.
#[derive(Clone, Debug)]
pub struct Proj {
    pub eye: Vec3,
    pub right: Vec3,
    pub up: Vec3,
    pub fwd: Vec3,
    pub ortho: bool,
    /// Orthographic: half the visible height.
    pub half_h: f32,
    /// Perspective: tan(fov / 2).
    pub tan_half: f32,
    pub aspect: f32,
    pub near: f32,
    pub far: f32,
    pub w: usize,
    pub h: usize,
}

/// Rows of an affine map from 3D to clip space: `clip[k] = r[k] · (x, y, z, 1)`.
pub type ClipMap = [[f32; 4]; 4];

impl Proj {
    /// The world-to-clip map.
    pub fn clip_map(&self) -> ClipMap {
        let row = |a: Vec3, s: f32, c: f32| [a.x * s, a.y * s, a.z * s, c - a.dot(self.eye) * s];
        if self.ortho {
            let (sx, sy) = (1.0 / (self.half_h * self.aspect), 1.0 / self.half_h);
            // z = (far - d) / (far - near): 1 at the near plane, 0 at the far one.
            let k = 1.0 / (self.far - self.near);
            let z = row(self.fwd, -k, self.far * k);
            return [row(self.right, sx, 0.0), row(self.up, sy, 0.0), z, [0.0, 0.0, 0.0, 1.0]];
        }
        let (sx, sy) = (1.0 / (self.tan_half * self.aspect), 1.0 / self.tan_half);
        [row(self.right, sx, 0.0), row(self.up, sy, 0.0), [0.0, 0.0, 0.0, self.near], row(self.fwd, 1.0, 0.0)]
    }

    /// The clip-space map of a part placed by `xf`.
    pub fn clip_map_of(&self, xf: &Transform) -> ClipMap {
        compose(&self.clip_map(), xf)
    }

    pub fn clip(&self, p: Vec3) -> [f32; 4] {
        apply(&self.clip_map(), p)
    }

    /// Pixel position and depth of `p`; None behind the eye.
    pub fn to_screen(&self, p: Vec3) -> Option<(f32, f32, f32)> {
        let c = self.clip(p);
        if c[3] <= 0.0 || c[3] - c[2] < 0.0 {
            return None;
        }
        let iw = 1.0 / c[3];
        Some(((c[0] * iw * 0.5 + 0.5) * self.w as f32, (0.5 - c[1] * iw * 0.5) * self.h as f32, c[2] * iw))
    }

    /// The ray through pixel position (`x`, `y`).
    pub fn ray(&self, x: f32, y: f32) -> Ray {
        let sx = (2.0 * x / self.w.max(1) as f32 - 1.0) * self.aspect;
        let sy = 1.0 - 2.0 * y / self.h.max(1) as f32;
        if self.ortho {
            let o = self.eye + self.right * (sx * self.half_h) + self.up * (sy * self.half_h);
            return Ray::new(o, self.fwd, 0.0);
        }
        let d = self.fwd + self.right * (sx * self.tan_half) + self.up * (sy * self.tan_half);
        Ray::new(self.eye, d.normalize(), 0.0)
    }

    /// World size of one pixel at `p`.
    pub fn pixel_size(&self, p: Vec3) -> f32 {
        let h = self.h.max(1) as f32;
        if self.ortho {
            return 2.0 * self.half_h / h;
        }
        let d = (p - self.eye).dot(self.fwd).max(self.near);
        2.0 * d * self.tan_half / h
    }
}

/// Whether a box is entirely outside the view whose world-to-clip map is `m`.
pub fn culls(m: &ClipMap, b: &Aabb) -> bool {
    if b.is_empty() {
        return true;
    }
    let mut outside = [true; 5];
    for i in 0..8 {
        let pick = |bit: usize, lo: f32, hi: f32| if i & bit == 0 { lo } else { hi };
        let p = Vec3::new(pick(1, b.min.x, b.max.x), pick(2, b.min.y, b.max.y), pick(4, b.min.z, b.max.z));
        let [x, y, z, w] = apply(m, p);
        for (o, d) in outside.iter_mut().zip([w - z, w + x, w - x, w + y, w - y]) {
            *o &= d < 0.0;
        }
    }
    outside.iter().any(|&o| o)
}

#[inline]
pub fn apply(m: &ClipMap, p: Vec3) -> [f32; 4] {
    std::array::from_fn(|k| m[k][0] * p.x + m[k][1] * p.y + m[k][2] * p.z + m[k][3])
}

/// `m` after the affine transform `xf`.
pub fn compose(m: &ClipMap, xf: &Transform) -> ClipMap {
    let a = &xf.m.m;
    std::array::from_fn(|k| {
        let r = &m[k];
        let mut out = [0.0; 4];
        for (j, o) in out.iter_mut().take(3).enumerate() {
            *o = r[0] * a[0][j] + r[1] * a[1][j] + r[2] * a[2][j];
        }
        out[3] = r[0] * xf.t.x + r[1] * xf.t.y + r[2] * xf.t.z + r[3];
        out
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scene() -> Aabb {
        Aabb { min: Vec3::splat(-1.0), max: Vec3::splat(1.0) }
    }

    /// The pixel a point projects to and the ray through that pixel agree, in every view.
    #[test]
    fn rays_go_through_their_pixels() {
        let cam = CameraDesc {
            position: Vec3::new(3.0, 2.0, 4.0),
            look_at: Vec3::new(0.2, 0.1, 0.0),
            ..CameraDesc::default()
        };
        for kind in [ViewKind::Top, ViewKind::Front, ViewKind::Side, ViewKind::Perspective] {
            for ortho in [false, true] {
                let mut v = View::new(kind, &cam, &scene());
                v.ortho = ortho;
                let proj = v.proj(320, 200, &scene());
                let p = Vec3::new(0.3, -0.4, 0.5);
                let (x, y, z) = proj.to_screen(p).unwrap();
                assert!(z > 0.0 && z <= 1.0, "{kind:?} depth {z}");
                let ray = proj.ray(x, y);
                let along = (p - ray.o).dot(ray.d);
                let miss = (ray.o + ray.d * along - p).length();
                assert!(along > 0.0 && miss < 1e-3, "{kind:?} ortho {ortho}: misses by {miss}");
                // Nearer points have larger depth.
                let (_, _, z2) = proj.to_screen(p - ray.d * 0.1).unwrap();
                assert!(z2 > z, "{kind:?}");
            }
        }
    }

    #[test]
    fn fixed_views_look_along_the_axes() {
        let cam = CameraDesc::default();
        let top = View::new(ViewKind::Top, &cam, &scene()).proj(100, 100, &scene());
        assert_eq!((top.right, top.fwd), (Vec3::new(1.0, 0.0, 0.0), Vec3::new(0.0, -1.0, 0.0)));
        let side = View::new(ViewKind::Side, &cam, &scene()).proj(100, 100, &scene());
        assert_eq!(side.right, Vec3::new(0.0, 0.0, -1.0));
        assert!(culls(&top.clip_map(), &Aabb { min: Vec3::splat(50.0), max: Vec3::splat(51.0) }));
        assert!(!culls(&top.clip_map(), &scene()));
    }
}
