//! Thin-lens perspective camera.

use crate::math::{Ray, Vec3};
use crate::rng::Rng;
use crate::sampling::concentric_disk;

#[derive(Clone, Debug, PartialEq)]
pub struct CameraDesc {
    pub position: Vec3,
    pub look_at: Vec3,
    pub up: Vec3,
    /// Vertical field of view in degrees.
    pub fov: f32,
    /// Lens diameter; 0 means a pinhole camera.
    pub aperture: f32,
    /// Distance to the plane in perfect focus; defaults to |`look_at` - position|.
    pub focus_distance: Option<f32>,
}

impl Default for CameraDesc {
    fn default() -> Self {
        CameraDesc {
            position: Vec3::new(0.0, 0.0, 5.0),
            look_at: Vec3::ZERO,
            up: Vec3::new(0.0, 1.0, 0.0),
            fov: 40.0,
            aperture: 0.0,
            focus_distance: None,
        }
    }
}

pub struct Camera {
    origin: Vec3,
    upper_left: Vec3,
    horizontal: Vec3,
    vertical: Vec3,
    u: Vec3,
    v: Vec3,
    lens_radius: f32,
}

impl Camera {
    pub fn new(desc: &CameraDesc, aspect: f32) -> Camera {
        let h = (desc.fov.to_radians() * 0.5).tan();
        let viewport_h = 2.0 * h;
        let viewport_w = aspect * viewport_h;
        let w = (desc.position - desc.look_at).normalize();
        let u = desc.up.cross(w).normalize();
        let v = w.cross(u);
        let focus = desc.focus_distance.unwrap_or_else(|| (desc.look_at - desc.position).length()).max(1e-4);
        let horizontal = u * (viewport_w * focus);
        let vertical = v * (viewport_h * focus);
        let upper_left = desc.position - horizontal * 0.5 + vertical * 0.5 - w * focus;
        Camera { origin: desc.position, upper_left, horizontal, vertical, u, v, lens_radius: desc.aperture * 0.5 }
    }

    /// `s` goes left to right and `t` top to bottom, both in [0, 1].
    #[inline]
    pub fn ray(&self, s: f32, t: f32, rng: &mut Rng) -> Ray {
        let offset = if self.lens_radius > 0.0 {
            let (dx, dy) = concentric_disk(rng.f32(), rng.f32());
            self.u * (dx * self.lens_radius) + self.v * (dy * self.lens_radius)
        } else {
            Vec3::ZERO
        };
        let target = self.upper_left + self.horizontal * s - self.vertical * t;
        let o = self.origin + offset;
        Ray::new(o, (target - o).normalize())
    }
}
