//! Analytic daylight: the Preetham, Shirley & Smits (1999) sky model and a matching
//! sun whose colour comes from Rayleigh and aerosol (Ångström) attenuation of a
//! 5778 K black body along the air mass.
//!
//! Radiance is computed in kcd/m² and multiplied by [`SKY_UNIT`] so that with
//! `intensity = 1` a white Lambertian surface under a high sun is about 1.

use crate::math::{PI, Vec3};
use crate::spectrum::{blackbody, cie_xyz, xyz_to_srgb};

/// Scene radiance per kcd/m².
pub const SKY_UNIT: f32 = 1.0 / 40.0;
/// Angular radius of the sun, radians.
pub const SUN_RADIUS: f32 = 0.004_65;
/// Luminance of the sun outside the atmosphere, kcd/m².
const SUN_LUMINANCE: f32 = 2.0e6;

pub struct Sky {
    /// Unit vector towards the sun.
    sun: Vec3,
    /// Perez coefficients A..E for Y, x, y.
    perez: [[f32; 5]; 3],
    /// Zenith Y (kcd/m²), x, y.
    zenith: [f32; 3],
    /// Perez function at the zenith, per channel (normalization).
    f0: [f32; 3],
    scale: f32,
    /// Radiance below the horizon: a Lambertian ground lit by sun and sky.
    ground: Vec3,
}

fn perez_fn(c: &[f32; 5], cos_theta: f32, gamma: f32) -> f32 {
    let cos_g = gamma.cos();
    (1.0 + c[0] * (c[1] / cos_theta.max(1e-3)).exp()) * (1.0 + c[2] * (c[3] * gamma).exp() + c[4] * cos_g * cos_g)
}

impl Sky {
    /// `sun` points towards the sun; `intensity` scales sky and sun; `ground_albedo`
    /// colours the lower hemisphere.
    pub fn new(sun: Vec3, turbidity: f32, intensity: f32, ground_albedo: Vec3) -> Sky {
        let t = turbidity.clamp(1.7, 10.0);
        let sun = sun.normalize();
        // The model is only defined for a sun above the horizon.
        let theta_s = sun.y.clamp(0.0, 1.0).acos().min(PI / 2.0 - 1e-3);
        let perez = [
            [
                0.1787 * t - 1.4630,
                -0.3554 * t + 0.4275,
                -0.0227 * t + 5.3251,
                0.1206 * t - 2.5771,
                -0.0670 * t + 0.3703,
            ],
            [
                -0.0193 * t - 0.2592,
                -0.0665 * t + 0.0008,
                -0.0004 * t + 0.2125,
                -0.0641 * t - 0.8989,
                -0.0033 * t + 0.0452,
            ],
            [
                -0.0167 * t - 0.2608,
                -0.0950 * t + 0.0092,
                -0.0079 * t + 0.2102,
                -0.0441 * t - 1.6537,
                -0.0109 * t + 0.0529,
            ],
        ];
        let chi = (4.0 / 9.0 - t / 120.0) * (PI - 2.0 * theta_s);
        let yz = ((4.0453 * t - 4.9710) * chi.tan() - 0.2155 * t + 2.4192).max(0.0);
        let (t2, s) = (t * t, [theta_s.powi(3), theta_s * theta_s, theta_s, 1.0]);
        let poly = |m: [[f32; 4]; 3]| -> f32 {
            let row = |r: [f32; 4]| r.iter().zip(s).map(|(a, b)| a * b).sum::<f32>();
            t2 * row(m[0]) + t * row(m[1]) + row(m[2])
        };
        let xz = poly([
            [0.00166, -0.00375, 0.00209, 0.0],
            [-0.02903, 0.06377, -0.03202, 0.00394],
            [0.11693, -0.21196, 0.06052, 0.25886],
        ]);
        let yz_c = poly([
            [0.00275, -0.00610, 0.00317, 0.0],
            [-0.04214, 0.08970, -0.04153, 0.00516],
            [0.15346, -0.26756, 0.06670, 0.26688],
        ]);
        let f0 = std::array::from_fn(|i| perez_fn(&perez[i], 1.0, theta_s));
        let mut sky =
            Sky { sun, perez, zenith: [yz, xz, yz_c], f0, scale: SKY_UNIT * intensity.max(0.0), ground: Vec3::ZERO };
        // Horizontal irradiance from the sky (midpoint rule) and the sun.
        let (nt, np) = (32, 64);
        let mut e = Vec3::ZERO;
        for i in 0..nt {
            let theta = (i as f32 + 0.5) / nt as f32 * PI / 2.0;
            let (st, ct) = theta.sin_cos();
            for j in 0..np {
                let phi = (j as f32 + 0.5) / np as f32 * 2.0 * PI;
                let d = Vec3::new(st * phi.cos(), ct, st * phi.sin());
                e += sky.sky(d) * (ct * st * (PI / 2.0 / nt as f32) * (2.0 * PI / np as f32));
            }
        }
        let sun_solid_angle = 2.0 * PI * (1.0 - SUN_RADIUS.cos());
        e += sky.sun_radiance() * (sun_solid_angle * sun.y.max(0.0));
        sky.ground = Vec3::new(ground_albedo.x * e.x, ground_albedo.y * e.y, ground_albedo.z * e.z) * (1.0 / PI);
        sky
    }

    /// Sky radiance above the horizon (without the sun disk).
    fn sky(&self, d: Vec3) -> Vec3 {
        let cos_theta = d.y.max(0.0);
        let gamma = crate::sampling::angle_between(d, self.sun);
        let v: [f32; 3] =
            std::array::from_fn(|i| self.zenith[i] * perez_fn(&self.perez[i], cos_theta, gamma) / self.f0[i]);
        let (lum, x, y) = (v[0], v[1], v[2].max(1e-4));
        let xyz = Vec3::new(x / y * lum, lum, (1.0 - x - y) / y * lum);
        xyz_to_srgb(xyz).max(Vec3::ZERO) * self.scale
    }

    /// Radiance seen in direction `d` (unit), excluding the sun disk.
    pub fn eval(&self, d: Vec3) -> Vec3 {
        if d.y >= 0.0 { self.sky(d) } else { self.ground }
    }

    pub fn sun_direction(&self) -> Vec3 {
        self.sun
    }

    /// Radiance of the sun disk as seen from the ground (zero below the horizon).
    pub fn sun_radiance(&self) -> Vec3 {
        let elev = self.sun.y.clamp(-1.0, 1.0).asin().to_degrees();
        if elev < -0.5 {
            return Vec3::ZERO;
        }
        let theta_deg = 90.0 - elev.max(0.0);
        // Relative optical air mass (Kasten & Young).
        let m = 1.0 / (theta_deg.to_radians().cos() + 0.15 * (93.885 - theta_deg).powf(-1.253));
        let t = self.turbidity();
        let beta = 0.046_083_66 * t - 0.045_860_26;
        let (mut xyz, mut xyz0) = (Vec3::ZERO, Vec3::ZERO);
        for i in 0..=80 {
            let l = 380.0 + i as f32 * 5.0;
            let um = l / 1000.0;
            let tau_r = (-0.008_735 * um.powf(-4.08) * m).exp();
            let tau_a = (-beta * um.powf(-1.3) * m).exp();
            let b = blackbody(l, 5778.0);
            xyz += cie_xyz(l) * (b * tau_r * tau_a);
            xyz0 += cie_xyz(l) * b;
        }
        xyz_to_srgb(xyz * (SUN_LUMINANCE / xyz0.y)).max(Vec3::ZERO) * self.scale
    }

    /// Turbidity recovered from the stored coefficients (`A_Y = 0.1787 T - 1.4630`).
    fn turbidity(&self) -> f32 {
        (self.perez[0][0] + 1.4630) / 0.1787
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sky_is_plausible() {
        let sun = Vec3::new(0.0, 0.5f32.sqrt(), 0.5f32.sqrt());
        let sky = Sky::new(sun, 3.0, 1.0, Vec3::splat(0.2));
        let zenith = sky.eval(Vec3::new(0.0, 1.0, 0.0));
        let horizon_away = sky.eval(Vec3::new(0.0, 0.05, -1.0).normalize());
        // Blue zenith, brighter towards the horizon, brightest around the sun.
        assert!(zenith.z > zenith.x, "{zenith:?}");
        assert!(horizon_away.luminance() > zenith.luminance() * 0.8);
        assert!(sky.eval(Vec3::new(0.0, 0.6, 0.8).normalize()).luminance() > zenith.luminance());
        // The sun is far brighter than the sky, and warmer at low elevation.
        let s = sky.sun_radiance();
        assert!(s.luminance() > 1e4 * zenith.luminance(), "{s:?}");
        let low = Sky::new(Vec3::new(0.0, 0.05, 1.0), 3.0, 1.0, Vec3::splat(0.2)).sun_radiance();
        assert!(low.x / low.z > s.x / s.z && low.luminance() < s.luminance());
        // A white diffuser under a 45 degree sun is around 1 with intensity 1.
        let e_sun = s * (2.0 * PI * (1.0 - SUN_RADIUS.cos()) * sun.y);
        assert!(sky.ground.luminance() / 0.2 > e_sun.luminance() / PI);
        let white = sky.ground.luminance() / 0.2;
        assert!((0.4..2.5).contains(&white), "{white}");
    }
}
