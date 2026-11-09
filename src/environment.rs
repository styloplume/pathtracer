//! The environment: radiance arriving from infinitely far away along rays that leave
//! the scene. Every kind (constant, gradient, equirectangular image, analytic sky) is
//! importance sampled through a tabulated latitude-longitude distribution of its
//! luminance, so all of them take part in next-event estimation with MIS.
//!
//! Equirectangular convention (Y up): the image centre looks down -Z, u grows towards
//! +X, the top row is the zenith.

use crate::math::{Mat3, PI, Vec3};
use crate::sampling::Distribution2D;
use crate::sky::Sky;
use crate::texture::TexImage;
use std::sync::Arc;

pub enum EnvSource {
    Color(Vec3),
    /// Blends horizon -> zenith above the horizon and horizon -> ground below.
    Gradient {
        zenith: Vec3,
        horizon: Vec3,
        ground: Vec3,
    },
    /// Equirectangular image, multiplied by `scale`.
    Image {
        img: Arc<TexImage>,
        scale: Vec3,
    },
    Sky(Box<Sky>),
}

pub struct Environment {
    source: EnvSource,
    /// World -> environment rotation (and back).
    to_env: Mat3,
    to_world: Mat3,
    /// Sampling distribution over (u, v); None if the environment is black.
    dist: Option<Distribution2D>,
}

/// Unit direction for equirectangular coordinates (v = 1 at the zenith).
pub fn dir_from_uv(uv: [f32; 2]) -> (Vec3, f32) {
    let phi = (uv[0] - 0.5) * 2.0 * PI;
    let theta = (1.0 - uv[1]) * PI;
    let (st, ct) = theta.sin_cos();
    let (sp, cp) = phi.sin_cos();
    (Vec3::new(st * sp, ct, -st * cp), st)
}

/// Equirectangular coordinates of a unit direction.
pub fn uv_from_dir(d: Vec3) -> [f32; 2] {
    let phi = d.x.atan2(-d.z);
    let theta = d.y.clamp(-1.0, 1.0).acos();
    [(0.5 + phi * (0.5 / PI)).rem_euclid(1.0), 1.0 - theta / PI]
}

/// Bilinear lookup with horizontal wrap-around and vertical clamping.
fn lookup(img: &TexImage, uv: [f32; 2]) -> Vec3 {
    let (w, h) = (img.width, img.height);
    let x = uv[0] * w as f32 - 0.5;
    let y = (1.0 - uv[1]) * h as f32 - 0.5;
    if !(x.is_finite() && y.is_finite()) {
        return Vec3::ZERO;
    }
    let (x0, y0) = (x.floor(), y.floor());
    let (fx, fy) = (x - x0, y - y0);
    let (x0, y0) = (x0 as i64, y0 as i64);
    let wrap = |i: i64| i.rem_euclid(w as i64) as usize;
    let clamp = |i: i64| i.clamp(0, h as i64 - 1) as usize;
    let t = |x: i64, y: i64| {
        let p = img.texel(wrap(x), clamp(y));
        Vec3::new(p[0], p[1], p[2])
    };
    (t(x0, y0) * (1.0 - fx) + t(x0 + 1, y0) * fx) * (1.0 - fy)
        + (t(x0, y0 + 1) * (1.0 - fx) + t(x0 + 1, y0 + 1) * fx) * fy
}

/// Resolution of the sampling table for analytic sources.
const ANALYTIC_RES: (usize, usize) = (256, 128);
/// Images wider than this are box-filtered down for the sampling table.
const MAX_TABLE_WIDTH: usize = 2048;

impl Environment {
    /// `rotate` is in degrees, applied X, then Y, then Z (like object transforms).
    pub fn new(source: EnvSource, rotate: Vec3) -> Environment {
        let to_world = Mat3::rotate_z(rotate.z).mul(&Mat3::rotate_y(rotate.y)).mul(&Mat3::rotate_x(rotate.x));
        let mut env = Environment { source, to_env: to_world.transpose(), to_world, dist: None };
        env.dist = env.build_distribution();
        env
    }

    fn eval_local(&self, d: Vec3) -> Vec3 {
        match &self.source {
            EnvSource::Color(c) => *c,
            EnvSource::Gradient { zenith, horizon, ground } => {
                if d.y >= 0.0 {
                    horizon.lerp(*zenith, d.y.sqrt())
                } else {
                    horizon.lerp(*ground, (-d.y * 8.0).min(1.0))
                }
            }
            EnvSource::Image { img, scale } => lookup(img, uv_from_dir(d)) * *scale,
            EnvSource::Sky(sky) => sky.eval(d),
        }
    }

    /// Radiance arriving from world direction `d` (unit).
    #[inline]
    pub fn eval(&self, d: Vec3) -> Vec3 {
        self.eval_local(self.to_env.mul_vec(d))
    }

    /// True if the environment emits nothing (it is then not a light).
    pub fn is_black(&self) -> bool {
        self.dist.is_none()
    }

    fn build_distribution(&self) -> Option<Distribution2D> {
        let (w, h, f) = if let EnvSource::Image { img, scale } = &self.source {
            let k = img.width.div_ceil(MAX_TABLE_WIDTH).max(1);
            let (w, h) = (img.width.div_ceil(k), img.height.div_ceil(k).max(1));
            let mut f = vec![0f32; w * h];
            for y in 0..img.height {
                for x in 0..img.width {
                    let t = img.texel(x, y);
                    f[(y / k) * w + x / k] += (Vec3::new(t[0], t[1], t[2]) * *scale).luminance().max(0.0);
                }
            }
            (w, h, f)
        } else {
            let (w, h) = ANALYTIC_RES;
            let f = (0..w * h)
                .map(|i| {
                    let uv = [((i % w) as f32 + 0.5) / w as f32, 1.0 - ((i / w) as f32 + 0.5) / h as f32];
                    self.eval_local(dir_from_uv(uv).0).luminance().max(0.0)
                })
                .collect();
            (w, h, f)
        };
        // Rows run top (v = 1) to bottom; the distribution's v runs bottom to top. Weight
        // by sin(theta), the area of a texel on the sphere.
        let mut table = vec![0f32; w * h];
        for row in 0..h {
            let sin_theta = ((row as f32 + 0.5) / h as f32 * PI).sin();
            let dst = h - 1 - row;
            for x in 0..w {
                table[dst * w + x] = f[row * w + x] * sin_theta;
            }
        }
        let dist = Distribution2D::new(&table, w, h);
        (dist.integral() > 0.0).then_some(dist)
    }

    /// Samples a world direction proportionally to the tabulated luminance.
    /// Returns (direction, solid-angle pdf).
    pub fn sample(&self, u1: f32, u2: f32) -> Option<(Vec3, f32)> {
        let (uv, pdf_uv) = self.dist.as_ref()?.sample(u1, u2);
        let (d, sin_theta) = dir_from_uv(uv);
        if sin_theta <= 0.0 || pdf_uv <= 0.0 {
            return None;
        }
        Some((self.to_world.mul_vec(d), pdf_uv / (2.0 * PI * PI * sin_theta)))
    }

    /// Solid-angle pdf of `sample` returning world direction `d`.
    pub fn pdf(&self, d: Vec3) -> f32 {
        let Some(dist) = &self.dist else { return 0.0 };
        let local = self.to_env.mul_vec(d);
        let sin_theta = (1.0 - local.y * local.y).max(0.0).sqrt();
        if sin_theta <= 0.0 {
            return 0.0;
        }
        dist.pdf(uv_from_dir(local)) / (2.0 * PI * PI * sin_theta)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::imageio::{Pixels, RawImage};
    use crate::rng::Rng;

    #[test]
    fn uv_direction_roundtrip() {
        for uv in [[0.5, 0.5], [0.1, 0.8], [0.9, 0.2], [0.3, 0.55]] {
            let (d, _) = dir_from_uv(uv);
            let back = uv_from_dir(d);
            assert!((back[0] - uv[0]).abs() < 1e-5 && (back[1] - uv[1]).abs() < 1e-5, "{uv:?} -> {back:?}");
        }
        assert!((dir_from_uv([0.5, 0.5]).0 - Vec3::new(0.0, 0.0, -1.0)).length() < 1e-6);
        assert!(dir_from_uv([0.5, 1.0]).0.y > 0.999);
    }

    /// Sampling must be consistent with `pdf` and integrate the environment correctly:
    /// E[L(w) cos / pdf] over the upper hemisphere equals a brute-force estimate.
    #[test]
    fn importance_sampling_is_unbiased() {
        // 8x4 image with one hot texel.
        let mut px = vec![0.1f32; 8 * 4 * 3];
        for c in 0..3 {
            px[(8 + 5) * 3 + c] = 50.0;
        }
        let raw = RawImage { width: 8, height: 4, channels: 3, pixels: Pixels::F32(px) };
        let img = Arc::new(TexImage::new(raw, false));
        let env = Environment::new(EnvSource::Image { img, scale: Vec3::ONE }, Vec3::new(0.0, 30.0, 0.0));
        let mut rng = Rng::new(9, 9);
        let n = 400_000;
        let (mut is, mut uni, mut mismatch) = (0.0f64, 0.0f64, 0);
        for _ in 0..n {
            let (d, pdf) = env.sample(rng.f32(), rng.f32()).unwrap();
            // Round-off may move a direction into the neighbouring bin.
            if (env.pdf(d) - pdf).abs() > 1e-3 * pdf.max(1.0) {
                mismatch += 1;
            }
            if d.y > 0.0 {
                is += f64::from(env.eval(d).luminance() * d.y / pdf);
            }
            let d = crate::sampling::uniform_sphere(rng.f32(), rng.f32());
            if d.y > 0.0 {
                uni += f64::from(env.eval(d).luminance() * d.y * 4.0 * PI);
            }
        }
        assert!(mismatch < n / 1000, "{mismatch} pdf mismatches");
        let (is, uni) = (is / f64::from(n), uni / f64::from(n));
        assert!((is - uni).abs() < 0.03 * uni, "{is} vs {uni}");
    }
}
