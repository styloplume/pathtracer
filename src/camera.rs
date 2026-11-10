//! Camera models: thin-lens perspective, orthographic (optionally with a lens),
//! equidistant fisheye and equirectangular 360° panoramas. The camera may move over
//! the frame for motion blur; each ray gets a time inside the shutter interval.

use crate::math::{PI, Ray, Vec3};
use crate::sampler::Sampler;
use crate::sampling::concentric_disk;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Projection {
    #[default]
    Perspective,
    Orthographic,
    /// Equidistant fisheye: a circular image fitting the image height, `fov` across it.
    Fisheye,
    /// Latitude-longitude panorama: 360° across, 180° up and down.
    Equirect,
}

impl Projection {
    pub fn parse(s: &str) -> Option<Projection> {
        match s {
            "perspective" | "pinhole" | "thinlens" => Some(Projection::Perspective),
            "orthographic" | "ortho" => Some(Projection::Orthographic),
            "fisheye" => Some(Projection::Fisheye),
            "equirect" | "equirectangular" | "spherical" | "panorama" | "360" => Some(Projection::Equirect),
            _ => None,
        }
    }
}

/// Where the camera is and what it looks at.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pose {
    pub position: Vec3,
    pub look_at: Vec3,
    pub up: Vec3,
}

impl Pose {
    fn lerp(&self, o: &Pose, t: f32) -> Pose {
        Pose {
            position: self.position.lerp(o.position, t),
            look_at: self.look_at.lerp(o.look_at, t),
            up: self.up.lerp(o.up, t),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct CameraDesc {
    pub position: Vec3,
    pub look_at: Vec3,
    pub up: Vec3,
    /// Vertical field of view in degrees (fisheye: across the image circle).
    pub fov: f32,
    /// Lens diameter; 0 means a pinhole camera.
    pub aperture: f32,
    /// Distance to the plane in perfect focus; defaults to |`look_at` - position|.
    pub focus_distance: Option<f32>,
    pub projection: Projection,
    /// Orthographic view height in scene units; by default the height a perspective
    /// camera with this `fov` sees at the `look_at` distance.
    pub ortho_height: Option<f32>,
    /// The pose at the end of the frame (time 1), for camera motion blur.
    pub motion: Option<Pose>,
    /// Shutter open and close time; motion keys sit at times 0 and 1.
    pub shutter: [f32; 2],
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
            projection: Projection::Perspective,
            ortho_height: None,
            motion: None,
            shutter: [0.0, 1.0],
        }
    }
}

impl CameraDesc {
    pub fn pose(&self) -> Pose {
        Pose { position: self.position, look_at: self.look_at, up: self.up }
    }
}

/// The camera frame at one instant.
#[derive(Clone, Copy)]
struct Basis {
    origin: Vec3,
    u: Vec3,
    v: Vec3,
    w: Vec3,
    focus: f32,
    ortho_h: f32,
}

pub struct Camera {
    projection: Projection,
    aspect: f32,
    /// tan(fov / 2) for perspective cameras, fov / 2 in radians for fisheyes.
    half_fov: f32,
    lens_radius: f32,
    focus_distance: Option<f32>,
    ortho_height: Option<f32>,
    start: Pose,
    end: Option<Pose>,
    basis: Basis,
    shutter: [f32; 2],
}

impl Camera {
    pub fn new(desc: &CameraDesc, aspect: f32) -> Camera {
        let half = desc.fov.to_radians() * 0.5;
        let mut cam = Camera {
            projection: desc.projection,
            aspect,
            half_fov: if desc.projection == Projection::Fisheye { half } else { half.tan() },
            lens_radius: desc.aperture * 0.5,
            focus_distance: desc.focus_distance,
            ortho_height: desc.ortho_height,
            start: desc.pose(),
            end: desc.motion,
            basis: Basis { origin: Vec3::ZERO, u: Vec3::ZERO, v: Vec3::ZERO, w: Vec3::Z, focus: 1.0, ortho_h: 1.0 },
            shutter: desc.shutter,
        };
        cam.basis = cam.basis_for(&cam.start);
        cam
    }

    fn basis_for(&self, pose: &Pose) -> Basis {
        let offset = pose.position - pose.look_at;
        let dist = offset.length();
        let w = if dist > 0.0 { offset * (1.0 / dist) } else { Vec3::Z };
        let mut u = pose.up.cross(w);
        if u.length_squared() == 0.0 {
            u = Vec3::new(0.0, 1.0, 0.0).cross(w);
        }
        let u = u.normalize();
        Basis {
            origin: pose.position,
            u,
            v: w.cross(u),
            w,
            focus: self.focus_distance.unwrap_or(dist).max(1e-4),
            ortho_h: self.ortho_height.unwrap_or_else(|| 2.0 * (self.half_fov * dist).max(1e-6)),
        }
    }

    fn lens_offset(&self, b: &Basis, s: &mut Sampler) -> Vec3 {
        if self.lens_radius > 0.0 {
            let (u1, u2) = s.get2d();
            let (dx, dy) = concentric_disk(u1, u2);
            b.u * (dx * self.lens_radius) + b.v * (dy * self.lens_radius)
        } else {
            Vec3::ZERO
        }
    }

    /// The ray through image position (`s`, `t`): `s` goes left to right and `t` top to
    /// bottom, both in [0, 1]. None outside a fisheye's image circle.
    #[inline]
    pub fn ray(&self, s: f32, t: f32, smp: &mut Sampler) -> Option<Ray> {
        let time = self.shutter[0] + (self.shutter[1] - self.shutter[0]) * smp.get1d();
        let b = match &self.end {
            None => self.basis,
            Some(end) => self.basis_for(&self.start.lerp(end, time)),
        };
        let (x, y) = (2.0 * s - 1.0, 1.0 - 2.0 * t);
        let (o, d) = match self.projection {
            Projection::Perspective => {
                let target =
                    b.origin + (b.u * (x * self.aspect * self.half_fov) + b.v * (y * self.half_fov) - b.w) * b.focus;
                let o = b.origin + self.lens_offset(&b, smp);
                (o, (target - o).normalize())
            }
            Projection::Orthographic => {
                let h = b.ortho_h * 0.5;
                let o = b.origin + b.u * (x * self.aspect * h) + b.v * (y * h);
                if self.lens_radius > 0.0 {
                    let target = o - b.w * b.focus;
                    let o = o + self.lens_offset(&b, smp);
                    (o, (target - o).normalize())
                } else {
                    (o, -b.w)
                }
            }
            Projection::Fisheye => {
                let x = x * self.aspect;
                let r = x.hypot(y);
                if r > 1.0 {
                    return None;
                }
                let (st, ct) = (r * self.half_fov).sin_cos();
                let side = if r > 0.0 { (b.u * x + b.v * y) * (st / r) } else { Vec3::ZERO };
                (b.origin, (side - b.w * ct).normalize())
            }
            Projection::Equirect => {
                let (sp, cp) = (x * PI).sin_cos();
                let (st, ct) = (y * PI * 0.5).sin_cos();
                (b.origin, ((b.u * sp - b.w * cp) * ct + b.v * st).normalize())
            }
        };
        Some(Ray::new(o, d, time))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn desc(projection: Projection) -> CameraDesc {
        CameraDesc {
            position: Vec3::new(0.0, 1.0, 5.0),
            look_at: Vec3::new(0.0, 1.0, 0.0),
            projection,
            ..CameraDesc::default()
        }
    }

    #[test]
    fn center_rays_look_at_the_target() {
        let mut rng = Sampler::independent(1, 2);
        let fwd = Vec3::new(0.0, 0.0, -1.0);
        for p in [Projection::Perspective, Projection::Orthographic, Projection::Fisheye, Projection::Equirect] {
            let cam = Camera::new(&desc(p), 2.0);
            let r = cam.ray(0.5, 0.5, &mut rng).unwrap();
            assert!(r.d.dot(fwd) > 0.9999, "{p:?}: {:?}", r.d);
            assert!((0.0..=1.0).contains(&r.time));
        }
    }

    #[test]
    fn projections_spread_rays_as_expected() {
        let mut rng = Sampler::independent(3, 4);
        // Orthographic: parallel rays, origins spread over the view height (2 tan(20°) 5).
        let cam = Camera::new(&desc(Projection::Orthographic), 1.0);
        let (a, b) = (cam.ray(0.5, 0.0, &mut rng).unwrap(), cam.ray(0.5, 1.0, &mut rng).unwrap());
        assert!((a.d - b.d).length() < 1e-6);
        assert!(((a.o - b.o).length() - 10.0 * 20f32.to_radians().tan()).abs() < 1e-4);
        // Equirect: the left edge looks backwards, the top straight up.
        let cam = Camera::new(&desc(Projection::Equirect), 2.0);
        assert!(cam.ray(0.0, 0.5, &mut rng).unwrap().d.z > 0.9999);
        assert!(cam.ray(0.5, 0.0, &mut rng).unwrap().d.y > 0.9999);
        // Fisheye (180°): the top of the circle looks straight up, the corners are outside.
        let cam = Camera::new(&CameraDesc { fov: 180.0, ..desc(Projection::Fisheye) }, 1.5);
        assert!(cam.ray(0.5, 0.0, &mut rng).unwrap().d.y > 0.9999);
        assert!(cam.ray(0.0, 0.0, &mut rng).is_none());
    }

    #[test]
    fn camera_motion_follows_time() {
        let mut d = desc(Projection::Perspective);
        d.motion = Some(Pose { position: Vec3::new(2.0, 1.0, 5.0), look_at: Vec3::new(2.0, 1.0, 0.0), up: d.up });
        d.shutter = [0.25, 0.25];
        let cam = Camera::new(&d, 1.0);
        let r = cam.ray(0.5, 0.5, &mut Sampler::independent(0, 0)).unwrap();
        assert!((r.time - 0.25).abs() < 1e-6 && (r.o.x - 0.5).abs() < 1e-5, "{r:?}");
    }
}
