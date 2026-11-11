//! Interactive camera: orbit / pan / dolly around a target point, mouse-look, and
//! WASD fly movement. Produces a [`CameraDesc`] for the renderer.

use crate::camera::CameraDesc;
use crate::math::Vec3;

/// Radians per pixel of mouse movement.
const ROTATE_SPEED: f32 = 0.005;
const MAX_PITCH: f32 = 89.0 * std::f32::consts::PI / 180.0;

#[derive(Clone, Debug)]
pub struct CameraController {
    target: Vec3,
    distance: f32,
    yaw: f32,
    pitch: f32,
    fov: f32,
    /// World up (unit length) and two horizontal axes spanning the yaw plane.
    up: Vec3,
    ref_z: Vec3,
    ref_x: Vec3,
    aperture: f32,
    /// Fixed focus distance from the scene file; `None` keeps the target in focus.
    focus_distance: Option<f32>,
    /// The scene file's camera: projection, shutter and motion carry over, the motion
    /// only while the camera has not been moved.
    base: CameraDesc,
}

impl CameraController {
    pub fn new(desc: &CameraDesc) -> CameraController {
        let up = if desc.up.length_squared() > 0.0 { desc.up.normalize() } else { Vec3::new(0.0, 1.0, 0.0) };
        let offset = desc.position - desc.look_at;
        let distance = offset.length().max(1e-4);
        let dir = if offset.length_squared() > 0.0 { offset * (1.0 / offset.length()) } else { Vec3::Z };
        let pitch = dir.dot(up).clamp(-1.0, 1.0).asin().clamp(-MAX_PITCH, MAX_PITCH);
        // Horizontal reference axis = the initial viewing direction projected onto the ground plane.
        let mut horizontal = dir - up * dir.dot(up);
        if horizontal.length_squared() < 1e-8 {
            horizontal = if up.x.abs() < 0.9 { Vec3::new(1.0, 0.0, 0.0) } else { Vec3::new(0.0, 0.0, 1.0) };
            horizontal = horizontal - up * horizontal.dot(up);
        }
        let ref_z = horizontal.normalize();
        let ref_x = up.cross(ref_z);
        CameraController {
            target: desc.look_at,
            distance,
            yaw: 0.0,
            pitch,
            fov: desc.fov,
            up,
            ref_z,
            ref_x,
            aperture: desc.aperture,
            focus_distance: desc.focus_distance,
            base: desc.clone(),
        }
    }

    /// Unit vector from the target towards the camera.
    fn dir(&self) -> Vec3 {
        let (sy, cy) = self.yaw.sin_cos();
        let (sp, cp) = self.pitch.sin_cos();
        (self.ref_z * cy + self.ref_x * sy) * cp + self.up * sp
    }

    pub fn position(&self) -> Vec3 {
        self.target + self.dir() * self.distance
    }

    /// The point orbited around.
    pub fn target(&self) -> Vec3 {
        self.target
    }

    /// Distance from the camera to the target.
    pub fn distance(&self) -> f32 {
        self.distance
    }

    /// Vertical field of view in degrees.
    pub fn fov(&self) -> f32 {
        self.fov
    }

    /// Keeps the viewing direction and moves so the sphere at `center` with `radius` just
    /// fills the view vertically.
    pub fn frame(&mut self, center: Vec3, radius: f32) {
        let half = (self.fov.to_radians() * 0.5).clamp(0.01, 1.5);
        self.target = center;
        self.distance = (radius.max(1e-4) / half.sin()).max(1e-3);
    }

    /// Camera right and up vectors (same construction as `Camera::new`).
    fn basis(&self) -> (Vec3, Vec3) {
        let w = self.dir();
        let u = self.up.cross(w).normalize();
        (u, w.cross(u))
    }

    pub fn desc(&self) -> CameraDesc {
        let (position, look_at) = (self.position(), self.target);
        let b = &self.base;
        let moved = (position - b.position).length() > 1e-4 * (1.0 + b.position.length())
            || (look_at - b.look_at).length() > 1e-4 * (1.0 + b.look_at.length());
        CameraDesc {
            position,
            look_at,
            up: self.up,
            fov: self.fov,
            aperture: self.aperture,
            focus_distance: self.focus_distance,
            motion: if moved { None } else { b.motion },
            ..b.clone()
        }
    }

    /// Turntable rotation around the target; the image follows the mouse.
    pub fn orbit(&mut self, dx: f32, dy: f32) {
        self.yaw -= dx * ROTATE_SPEED;
        self.pitch = (self.pitch + dy * ROTATE_SPEED).clamp(-MAX_PITCH, MAX_PITCH);
    }

    /// Rotates the view direction around the camera position (first-person look).
    pub fn look(&mut self, dx: f32, dy: f32) {
        let pos = self.position();
        self.orbit(dx, dy);
        self.target = pos - self.dir() * self.distance;
    }

    /// Moves camera and target parallel to the image plane so the point under the
    /// cursor (at target depth) follows the mouse. `viewport_h` is the window height in pixels.
    pub fn pan(&mut self, dx: f32, dy: f32, viewport_h: f32) {
        let world_per_px = 2.0 * self.distance * (self.fov.to_radians() * 0.5).tan() / viewport_h.max(1.0);
        let (right, up) = self.basis();
        self.target += (up * dy - right * dx) * world_per_px;
    }

    /// Moves towards (positive notches) or away from the target.
    pub fn dolly(&mut self, notches: f32) {
        self.distance = (self.distance * 0.85f32.powf(notches)).max(1e-3);
    }

    pub fn zoom_fov(&mut self, notches: f32) {
        self.fov = (self.fov * 0.9f32.powf(notches)).clamp(1.0, 170.0);
    }

    /// Translates camera and target. `local` = (right, world up, forward) amounts in world units.
    pub fn fly(&mut self, local: Vec3) {
        let (right, _) = self.basis();
        let forward = -self.dir();
        self.target += right * local.x + self.up * local.y + forward * local.z;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: Vec3, b: Vec3) -> bool {
        (a - b).length() < 1e-4
    }

    #[test]
    fn round_trips_scene_camera() {
        let desc = CameraDesc {
            position: Vec3::new(1.0, 2.0, 5.0),
            look_at: Vec3::new(0.0, 1.0, 0.0),
            ..CameraDesc::default()
        };
        let c = CameraController::new(&desc);
        let out = c.desc();
        assert!(close(out.position, desc.position));
        assert!(close(out.look_at, desc.look_at));
    }

    #[test]
    fn look_keeps_position_and_orbit_keeps_target() {
        let mut c = CameraController::new(&CameraDesc::default());
        let pos = c.position();
        c.look(40.0, -25.0);
        assert!(close(c.position(), pos));
        let target = c.target;
        c.orbit(-70.0, 30.0);
        assert!(close(c.target, target));
    }

    #[test]
    fn pitch_is_clamped_short_of_the_pole() {
        let mut c = CameraController::new(&CameraDesc::default());
        c.orbit(0.0, 1e6);
        let d = c.desc();
        assert!(d.position.is_finite());
        assert!((d.position - d.look_at).normalize().dot(Vec3::new(0.0, 1.0, 0.0)) < 0.9999);
    }
}
