//! Edge-aware à-trous wavelet denoiser (Dammertz et al. 2010) with the per-pixel
//! variance guidance of SVGF (Schied et al. 2017), spatial only.
//!
//! The color is first demodulated by the albedo AOV, so texture/material detail is
//! not blurred — only the (smoother) incident lighting is filtered. Five iterations
//! of a 5x5 B3-spline kernel with growing gaps cover a 61x61 footprint. Each tap is
//! weighted by normal similarity, relative depth difference and luminance difference
//! scaled by the local noise estimate, so converged pixels are left nearly untouched.

use crate::math::Vec3;
use crate::renderer::Pixel;

const ITERATIONS: u32 = 5;
const KERNEL: [f32; 5] = [1.0 / 16.0, 1.0 / 4.0, 3.0 / 8.0, 1.0 / 4.0, 1.0 / 16.0];
/// Luminance edge-stopping in units of the local standard deviation.
const SIGMA_LUMINANCE: f32 = 4.0;
/// Exponent of the normal similarity weight (cos^128).
const NORMAL_POWER_LOG2: u32 = 7;
/// Allowed relative depth change per pixel of distance.
const SIGMA_DEPTH: f32 = 0.03;

struct Guides {
    w: usize,
    h: usize,
    normal: Vec<Vec3>,
    depth: Vec<f32>,
}

/// Albedo used to demodulate; near-black channels are left alone (divide by 1).
#[inline]
fn demod(a: Vec3) -> Vec3 {
    let f = |v: f32| if v > 0.02 { v } else { 1.0 };
    Vec3::new(f(a.x), f(a.y), f(a.z))
}

/// Denoises a fully sampled accumulation buffer (every pixel has `n >= 1`).
pub fn denoise(pixels: &[Pixel], w: usize, h: usize, threads: usize) -> Vec<Vec3> {
    assert_eq!(pixels.len(), w * h);
    let albedo: Vec<Vec3> = pixels.iter().map(|p| demod(p.albedo())).collect();
    let mut color: Vec<Vec3> = pixels.iter().zip(&albedo).map(|(p, a)| p.mean() / *a).collect();
    let var: Vec<f32> = pixels
        .iter()
        .zip(&albedo)
        .map(|(p, a)| {
            let la = a.luminance().max(0.02);
            p.variance() / (la * la)
        })
        .collect();
    let mut var = blur_variance(&var, w, h);
    let g = Guides {
        w,
        h,
        normal: pixels.iter().map(Pixel::normal).collect(),
        depth: pixels.iter().map(Pixel::depth).collect(),
    };
    for i in 0..ITERATIONS {
        (color, var) = atrous(&color, &var, &g, 1 << i, threads.max(1));
    }
    color.iter().zip(&albedo).map(|(c, a)| *c * *a).collect()
}

/// 3x3 Gaussian prefilter of the variance (more stable edge-stopping).
fn blur_variance(var: &[f32], w: usize, h: usize) -> Vec<f32> {
    const K: [f32; 3] = [0.25, 0.5, 0.25];
    let mut out = vec![0.0; w * h];
    for y in 0..h {
        for x in 0..w {
            let (mut sum, mut wsum) = (0.0, 0.0);
            for (dy, ky) in K.iter().enumerate() {
                for (dx, kx) in K.iter().enumerate() {
                    let (xx, yy) = (x as isize + dx as isize - 1, y as isize + dy as isize - 1);
                    if xx >= 0 && yy >= 0 && (xx as usize) < w && (yy as usize) < h {
                        let k = kx * ky;
                        sum += k * var[yy as usize * w + xx as usize];
                        wsum += k;
                    }
                }
            }
            out[y * w + x] = sum / wsum;
        }
    }
    out
}

fn atrous(color: &[Vec3], var: &[f32], g: &Guides, step: usize, threads: usize) -> (Vec<Vec3>, Vec<f32>) {
    let (w, h) = (g.w, g.h);
    let mut out_c = vec![Vec3::ZERO; w * h];
    let mut out_v = vec![0.0f32; w * h];
    let rows_per = h.div_ceil(threads).max(1);
    std::thread::scope(|s| {
        for (band, (oc, ov)) in out_c.chunks_mut(rows_per * w).zip(out_v.chunks_mut(rows_per * w)).enumerate() {
            s.spawn(move || {
                let y0 = band * rows_per;
                for (k, (c_out, v_out)) in oc.iter_mut().zip(ov.iter_mut()).enumerate() {
                    let (x, y) = (k % w, y0 + k / w);
                    (*c_out, *v_out) = filter_pixel(color, var, g, x, y, step);
                }
            });
        }
    });
    (out_c, out_v)
}

#[inline]
fn filter_pixel(color: &[Vec3], var: &[f32], g: &Guides, x: usize, y: usize, step: usize) -> (Vec3, f32) {
    let w = g.w;
    let i = y * w + x;
    let (cp, np, zp) = (color[i], g.normal[i], g.depth[i]);
    let lp = cp.luminance();
    let inv_sigma_l = 1.0 / (SIGMA_LUMINANCE * var[i].max(0.0).sqrt() + 1e-6);
    let (mut sum_c, mut sum_v, mut sum_w) = (Vec3::ZERO, 0.0f32, 0.0f32);
    for (ky, &hy) in KERNEL.iter().enumerate() {
        let yy = y as isize + (ky as isize - 2) * step as isize;
        if yy < 0 || yy >= g.h as isize {
            continue;
        }
        for (kx, &hx) in KERNEL.iter().enumerate() {
            let xx = x as isize + (kx as isize - 2) * step as isize;
            if xx < 0 || xx >= w as isize {
                continue;
            }
            let j = yy as usize * w + xx as usize;
            let nq = g.normal[j];
            // Normals: both background (zero) counts as identical.
            let mut wn = if np == Vec3::ZERO && nq == Vec3::ZERO { 1.0 } else { np.dot(nq).max(0.0) };
            for _ in 0..NORMAL_POWER_LOG2 {
                wn *= wn;
            }
            if wn <= 0.0 {
                continue;
            }
            let zq = g.depth[j];
            let dist = step as f32 * (((kx as f32 - 2.0).powi(2) + (ky as f32 - 2.0).powi(2)).sqrt());
            #[expect(clippy::float_cmp, reason = "both pixels carry the same no-hit depth sentinel")]
            let ez = if zp > 0.0 && zq > 0.0 {
                (zp - zq).abs() / (SIGMA_DEPTH * zp.min(zq) * dist + 1e-6)
            } else if zp == zq {
                0.0
            } else {
                continue;
            };
            let el = (lp - color[j].luminance()).abs() * inv_sigma_l;
            let wt = hx * hy * wn * (-(ez + el)).exp();
            sum_c += color[j] * wt;
            sum_v += wt * wt * var[j];
            sum_w += wt;
        }
    }
    // The center tap always has weight > 0.
    (sum_c / sum_w, sum_v / (sum_w * sum_w))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::integrator::PathSample;
    use crate::rng::Rng;

    fn image(w: usize, h: usize, f: impl Fn(usize, usize, &mut Rng) -> (f32, Vec3)) -> Vec<Pixel> {
        let mut rng = Rng::new(3, 4);
        let mut px = vec![Pixel::default(); w * h];
        for y in 0..h {
            for x in 0..w {
                for _ in 0..4 {
                    let (v, n) = f(x, y, &mut rng);
                    px[y * w + x].add(&PathSample {
                        radiance: Vec3::splat(v),
                        albedo: Vec3::ONE,
                        normal: n,
                        depth: 1.0,
                    });
                }
            }
        }
        px
    }

    fn stddev(v: &[f32]) -> f32 {
        let m = v.iter().sum::<f32>() / v.len() as f32;
        (v.iter().map(|x| (x - m) * (x - m)).sum::<f32>() / v.len() as f32).sqrt()
    }

    #[test]
    fn removes_noise_but_keeps_geometric_edges() {
        let (w, h) = (64, 64);
        // Left half faces +Z and is dark, right half faces +X and is bright; both noisy.
        let px = image(w, h, |x, _, rng| {
            let (base, n) = if x < w / 2 { (0.2, Vec3::Z) } else { (0.8, Vec3::new(1.0, 0.0, 0.0)) };
            (base * (0.5 + rng.f32()), n)
        });
        let out = denoise(&px, w, h, 4);
        let half = |img: &dyn Fn(usize) -> f32, right: bool| -> Vec<f32> {
            (0..w * h).filter(|i| (i % w >= w / 2) == right && (i % w).abs_diff(w / 2) > 2).map(img).collect()
        };
        for right in [false, true] {
            let before = half(&|i| px[i].mean().x, right);
            let after = half(&|i| out[i].x, right);
            assert!(stddev(&after) < 0.25 * stddev(&before), "noise {} -> {}", stddev(&before), stddev(&after));
            // Means stay on their own side of the edge.
            let m = after.iter().sum::<f32>() / after.len() as f32;
            let expect = if right { 0.8 } else { 0.2 };
            assert!((m - expect).abs() < 0.05 * expect + 0.01, "mean {m} expected {expect}");
        }
    }
}
