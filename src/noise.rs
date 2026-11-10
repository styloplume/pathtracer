//! Solid noise with analytic gradients, and the fractals built from it: fBm and
//! turbulence, Musgrave's multifractals (ridged, hybrid, heterogeneous terrain), domain
//! warping, and the classic marble and wood patterns (Ebert et al., *Texturing & Modeling:
//! A Procedural Approach*, ch. 2 and 16).
//!
//! Every pattern is written once, generic over [`Scalar`]: plain `f32` evaluates the
//! value, and [`Dual`] carries the gradient along (forward-mode differentiation). Bump
//! mapping uses the gradient to tilt normals, so it needs no UVs and no finite differences.
//!
//! Antialiasing: a fractal seen through a pixel whose footprint is `footprint` scene
//! units wide keeps only the octaves whose features are larger than that, and fades the
//! last one in smoothly. The dropped octaves of fBm average zero. Those of turbulence are
//! replaced by their mean, so the pattern converges to its average instead of aliasing
//! (Perlin's "clamping").

use crate::math::Vec3;
use std::ops::{Add, Mul, Sub};
use std::sync::OnceLock;

// ---------------------------------------------------------------------------- Perlin noise

fn permutation() -> &'static [u8; 512] {
    static PERM: OnceLock<[u8; 512]> = OnceLock::new();
    PERM.get_or_init(|| {
        let mut p: [u8; 256] = std::array::from_fn(|i| i as u8);
        let mut rng = crate::rng::Rng::new(0x5EED, 7);
        for i in (1..256).rev() {
            let j = (rng.next_u32() as usize) % (i + 1);
            p.swap(i, j);
        }
        std::array::from_fn(|i| p[i & 255])
    })
}

/// Dot product of corner gradient `hash` with the offset (x, y, z) (Perlin 2002).
#[inline]
fn grad(hash: u8, x: f32, y: f32, z: f32) -> f32 {
    let h = hash & 15;
    let u = if h < 8 { x } else { y };
    let v = if h < 4 {
        y
    } else if h == 12 || h == 14 {
        x
    } else {
        z
    };
    (if h & 1 == 0 { u } else { -u }) + (if h & 2 == 0 { v } else { -v })
}

/// The gradient vector that `grad` dots with.
#[inline]
fn grad_vec(hash: u8) -> Vec3 {
    let h = hash & 15;
    let mut g = [0.0f32; 3];
    let u = usize::from(h >= 8);
    let v = if h < 4 {
        1
    } else if h == 12 || h == 14 {
        0
    } else {
        2
    };
    g[u] = if h & 1 == 0 { 1.0 } else { -1.0 };
    g[v] = if h & 2 == 0 { 1.0 } else { -1.0 };
    Vec3::new(g[0], g[1], g[2])
}

/// The lattice cell around `p`: corner hashes in the order (000, 100, 010, 110, 001, 101,
/// 011, 111) and the position within the cell.
#[inline]
fn cell(p: Vec3) -> ([u8; 8], Vec3) {
    let perm = permutation();
    let (fx, fy, fz) = (p.x.floor(), p.y.floor(), p.z.floor());
    let (xi, yi, zi) = ((fx as i64 & 255) as usize, (fy as i64 & 255) as usize, (fz as i64 & 255) as usize);
    let a = perm[xi] as usize + yi;
    let (aa, ab) = (perm[a] as usize + zi, perm[a + 1] as usize + zi);
    let b = perm[xi + 1] as usize + yi;
    let (ba, bb) = (perm[b] as usize + zi, perm[b + 1] as usize + zi);
    let h = [perm[aa], perm[ba], perm[ab], perm[bb], perm[aa + 1], perm[ba + 1], perm[ab + 1], perm[bb + 1]];
    (h, Vec3::new(p.x - fx, p.y - fy, p.z - fz))
}

#[inline]
fn fade(t: f32) -> f32 {
    t * t * t * (t * (t * 6.0 - 15.0) + 10.0)
}

#[inline]
fn fade_deriv(t: f32) -> f32 {
    30.0 * t * t * (t - 1.0) * (t - 1.0)
}

#[inline]
fn lerp(t: f32, a: f32, b: f32) -> f32 {
    a + t * (b - a)
}

/// The eight corner values of the cell (gradients dotted with the offsets).
#[inline]
fn corner_values(h: [u8; 8], f: Vec3) -> [f32; 8] {
    let (x, y, z) = (f.x, f.y, f.z);
    [
        grad(h[0], x, y, z),
        grad(h[1], x - 1.0, y, z),
        grad(h[2], x, y - 1.0, z),
        grad(h[3], x - 1.0, y - 1.0, z),
        grad(h[4], x, y, z - 1.0),
        grad(h[5], x - 1.0, y, z - 1.0),
        grad(h[6], x, y - 1.0, z - 1.0),
        grad(h[7], x - 1.0, y - 1.0, z - 1.0),
    ]
}

#[inline]
fn interpolate(c: &[f32; 8], u: f32, v: f32, w: f32) -> f32 {
    lerp(w, lerp(v, lerp(u, c[0], c[1]), lerp(u, c[2], c[3])), lerp(v, lerp(u, c[4], c[5]), lerp(u, c[6], c[7])))
}

/// Improved Perlin noise, roughly in [-1, 1]; zero at lattice points.
pub fn perlin(p: Vec3) -> f32 {
    let (h, f) = cell(p);
    interpolate(&corner_values(h, f), fade(f.x), fade(f.y), fade(f.z))
}

/// Perlin noise and its gradient.
pub fn perlin_grad(p: Vec3) -> (f32, Vec3) {
    let (h, f) = cell(p);
    let c = corner_values(h, f);
    let (u, v, w) = (fade(f.x), fade(f.y), fade(f.z));
    let value = interpolate(&c, u, v, w);
    // value = k0 + k1 u + k2 v + k3 w + k4 uv + k5 vw + k6 wu + k7 uvw; its gradient is the
    // same blend of the corner gradient vectors plus the fade terms.
    let k1 = c[1] - c[0];
    let k2 = c[2] - c[0];
    let k3 = c[4] - c[0];
    let k4 = c[0] - c[1] - c[2] + c[3];
    let k5 = c[0] - c[2] - c[4] + c[6];
    let k6 = c[0] - c[1] - c[4] + c[5];
    let k7 = -c[0] + c[1] + c[2] - c[3] + c[4] - c[5] - c[6] + c[7];
    let g = h.map(grad_vec);
    let gi = g[0]
        + (g[1] - g[0]) * u
        + (g[2] - g[0]) * v
        + (g[4] - g[0]) * w
        + (g[0] - g[1] - g[2] + g[3]) * (u * v)
        + (g[0] - g[2] - g[4] + g[6]) * (v * w)
        + (g[0] - g[1] - g[4] + g[5]) * (w * u)
        + (g[1] + g[2] + g[4] + g[7] - g[0] - g[3] - g[5] - g[6]) * (u * v * w);
    let d = Vec3::new(
        fade_deriv(f.x) * (k1 + k4 * v + k6 * w + k7 * v * w),
        fade_deriv(f.y) * (k2 + k5 * w + k4 * u + k7 * w * u),
        fade_deriv(f.z) * (k3 + k6 * u + k5 * v + k7 * u * v),
    );
    (value, gi + d)
}

/// Mean of |`perlin`| over space (measured; see the tests). Stands in for the octaves
/// of turbulence that are too fine to show.
pub const PERLIN_MEAN_ABS: f32 = 0.220;

// ---------------------------------------------------------------------------- scalars

/// A value with its gradient with respect to the evaluation point.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Dual {
    pub v: f32,
    pub g: Vec3,
}

impl Add for Dual {
    type Output = Dual;
    #[inline]
    fn add(self, o: Dual) -> Dual {
        Dual { v: self.v + o.v, g: self.g + o.g }
    }
}
impl Sub for Dual {
    type Output = Dual;
    #[inline]
    fn sub(self, o: Dual) -> Dual {
        Dual { v: self.v - o.v, g: self.g - o.g }
    }
}
impl Mul for Dual {
    type Output = Dual;
    #[inline]
    fn mul(self, o: Dual) -> Dual {
        Dual { v: self.v * o.v, g: self.g * o.v + o.g * self.v }
    }
}
impl Mul<f32> for Dual {
    type Output = Dual;
    #[inline]
    fn mul(self, s: f32) -> Dual {
        Dual { v: self.v * s, g: self.g * s }
    }
}
impl Add<f32> for Dual {
    type Output = Dual;
    #[inline]
    fn add(self, s: f32) -> Dual {
        Dual { v: self.v + s, g: self.g }
    }
}

/// What the generic pattern code computes with: `f32` (value only) or [`Dual`].
pub trait Scalar:
    Copy + Add<Output = Self> + Sub<Output = Self> + Mul<Output = Self> + Mul<f32, Output = Self> + Add<f32, Output = Self>
{
    fn constant(v: f32) -> Self;
    /// A linear function of the evaluation point: `v` here, changing by `g` per unit.
    fn linear(v: f32, g: Vec3) -> Self;
    /// Perlin noise at `q`, a point moving `freq` times as fast as the evaluation point.
    fn perlin(q: Vec3, freq: f32) -> Self;
    fn abs(self) -> Self;
    /// Clamped to [lo, hi] (no slope where clamped).
    fn clamp(self, lo: f32, hi: f32) -> Self;
    fn sin(self) -> Self;
    fn sqrt(self) -> Self;
    /// The fractional part (slope unchanged).
    fn fract(self) -> Self;
}

impl Scalar for f32 {
    #[inline]
    fn constant(v: f32) -> f32 {
        v
    }
    #[inline]
    fn linear(v: f32, _: Vec3) -> f32 {
        v
    }

    #[inline]
    fn perlin(q: Vec3, _: f32) -> f32 {
        perlin(q)
    }
    #[inline]
    fn abs(self) -> f32 {
        f32::abs(self)
    }
    #[inline]
    fn clamp(self, lo: f32, hi: f32) -> f32 {
        self.max(lo).min(hi)
    }
    #[inline]
    fn sin(self) -> f32 {
        f32::sin(self)
    }
    #[inline]
    fn sqrt(self) -> f32 {
        f32::sqrt(self)
    }
    #[inline]
    fn fract(self) -> f32 {
        self - self.floor()
    }
}

impl Scalar for Dual {
    #[inline]
    fn constant(v: f32) -> Dual {
        Dual { v, g: Vec3::ZERO }
    }
    #[inline]
    fn linear(v: f32, g: Vec3) -> Dual {
        Dual { v, g }
    }

    #[inline]
    fn perlin(q: Vec3, freq: f32) -> Dual {
        let (v, g) = perlin_grad(q);
        Dual { v, g: g * freq }
    }
    #[inline]
    fn abs(self) -> Dual {
        if self.v < 0.0 { self * -1.0 } else { self }
    }
    #[inline]
    fn clamp(self, lo: f32, hi: f32) -> Dual {
        if self.v < lo {
            Dual::constant(lo)
        } else if self.v > hi {
            Dual::constant(hi)
        } else {
            self
        }
    }
    #[inline]
    fn sin(self) -> Dual {
        let (s, c) = self.v.sin_cos();
        Dual { v: s, g: self.g * c }
    }
    #[inline]
    fn sqrt(self) -> Dual {
        let r = self.v.max(0.0).sqrt();
        Dual { v: r, g: if r > 0.0 { self.g * (0.5 / r) } else { Vec3::ZERO } }
    }
    #[inline]
    fn fract(self) -> Dual {
        Dual { v: self.v - self.v.floor(), g: self.g }
    }
}

// ---------------------------------------------------------------------------- fractals

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FractalKind {
    /// Fractional Brownian motion: a sum of noise octaves. Mean 0.5.
    Fbm,
    /// Sum of |noise| octaves: billowy, with creases. Mean ≈ 0.4.
    Turbulence,
    /// Musgrave's ridged multifractal: sharp ridges, smooth valleys.
    Ridged,
    /// Musgrave's hybrid multifractal: smooth low areas, rough peaks.
    Hybrid,
    /// Musgrave's heterogeneous terrain: roughness grows with altitude.
    Terrain,
}

impl FractalKind {
    pub const NAMES: &'static str = "fbm, turbulence, ridged, hybrid, terrain";

    pub fn parse(s: &str) -> Option<FractalKind> {
        match s {
            "fbm" | "noise" => Some(FractalKind::Fbm),
            "turbulence" => Some(FractalKind::Turbulence),
            "ridged" | "ridged_multifractal" => Some(FractalKind::Ridged),
            "hybrid" | "hybrid_multifractal" => Some(FractalKind::Hybrid),
            "terrain" | "hetero_terrain" => Some(FractalKind::Terrain),
            _ => None,
        }
    }

    fn multiplicative(self) -> bool {
        matches!(self, FractalKind::Hybrid | FractalKind::Terrain)
    }
}

/// A fractal sum of Perlin noise octaves. Values are normalized to roughly [0, 1].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fractal {
    pub kind: FractalKind,
    /// Frequency of the first octave (features per scene unit).
    pub scale: f32,
    /// Number of octaves; a fractional last octave is blended in.
    pub octaves: f32,
    /// Frequency ratio of successive octaves.
    pub lacunarity: f32,
    /// Amplitude ratio of successive octaves (`lacunarity^-H` in Musgrave's terms).
    pub gain: f32,
    /// Musgrave's offset (ridged, hybrid, terrain).
    pub offset: f32,
    /// How strongly a ridge suppresses the detail on the next octave (ridged).
    pub sharpness: f32,
    /// Domain warp: the point is displaced by fBm before the fractal is evaluated, this
    /// many first-octave wavelengths at most.
    pub warp: f32,
}

/// Where the three warp components sample their fBm (decorrelated copies).
const WARP_SHIFT: [Vec3; 3] = [Vec3::new(5.2, 1.3, 2.8), Vec3::new(1.7, 9.2, 4.1), Vec3::new(8.3, 2.8, 6.5)];
/// Shift between octaves (keeps lattice points of successive octaves apart).
const OCTAVE_SHIFT: f32 = 17.17;

impl Fractal {
    /// Musgrave's suggested parameters for `kind`.
    pub fn new(kind: FractalKind) -> Fractal {
        let (gain, offset) = match kind {
            FractalKind::Fbm | FractalKind::Turbulence => (0.5, 0.0),
            FractalKind::Ridged => (0.5, 1.0),
            FractalKind::Hybrid => (0.84, 0.7),
            FractalKind::Terrain => (0.5, 0.3),
        };
        Fractal { kind, scale: 1.0, octaves: 5.0, lacunarity: 2.0, gain, offset, sharpness: 2.0, warp: 0.0 }
    }

    /// Octaves fine enough to show at `footprint` (the width in scene units that one pixel
    /// covers): those whose frequency stays below the pixel's Nyquist limit. 0 = all.
    pub fn octaves_for(&self, footprint: f32) -> f32 {
        if footprint <= 0.0 || !footprint.is_finite() {
            return self.octaves;
        }
        let n = (0.5 / (footprint * self.scale)).ln() / self.lacunarity.ln() + 1.0;
        let min = if self.kind.multiplicative() || self.kind == FractalKind::Ridged { 1.0 } else { 0.0 };
        n.clamp(min, self.octaves.max(min))
    }

    pub fn eval(&self, p: Vec3, footprint: f32) -> f32 {
        let n = self.octaves_for(footprint);
        if self.warp == 0.0 {
            return self.sum::<f32>(self.kind, p, n);
        }
        let k = 2.0 * self.warp / self.scale;
        let w = WARP_SHIFT.map(|s| self.sum::<f32>(FractalKind::Fbm, p + s * (1.0 / self.scale), n) - 0.5);
        self.sum::<f32>(self.kind, p + Vec3::new(w[0], w[1], w[2]) * k, n)
    }

    /// The value and its gradient.
    pub fn eval_grad(&self, p: Vec3, footprint: f32) -> (f32, Vec3) {
        let n = self.octaves_for(footprint);
        if self.warp == 0.0 {
            let d = self.sum::<Dual>(self.kind, p, n);
            return (d.v, d.g);
        }
        // Chain rule through the warp: grad = J^T grad', with J = I + k [grad w_i]^T.
        let k = 2.0 * self.warp / self.scale;
        let w = WARP_SHIFT.map(|s| self.sum::<Dual>(FractalKind::Fbm, p + s * (1.0 / self.scale), n));
        let f = self.sum::<Dual>(self.kind, p + Vec3::new(w[0].v - 0.5, w[1].v - 0.5, w[2].v - 0.5) * k, n);
        (f.v, f.g + (w[0].g * f.g.x + w[1].g * f.g.y + w[2].g * f.g.z) * k)
    }

    /// The fractal sum of `n` octaves (normalized for all of `self.octaves`).
    fn sum<S: Scalar>(&self, kind: FractalKind, p: Vec3, n: f32) -> S {
        let mut q = p * self.scale;
        let mut freq = self.scale;
        let full = self.octaves.ceil() as u32;
        let (offset, gain) = (self.offset, self.gain);
        // Weight of octave i in the configured sum, and in the footprint-limited one.
        let full_w = |i: u32| (self.octaves - i as f32).clamp(0.0, 1.0);
        let shown_w = |i: u32| (n - i as f32).clamp(0.0, 1.0);
        let mut amp = 1.0f32;
        let mut norm = 0.0f32;
        let mut sum = S::constant(0.0);
        // Ridged and multiplicative kinds carry a weight from octave to octave.
        let mut weight = S::constant(1.0);
        for i in 0..full {
            let (wf, ws) = (full_w(i), shown_w(i));
            norm += amp * wf;
            if ws > 0.0 {
                let noise = S::perlin(q, freq);
                match kind {
                    FractalKind::Fbm => sum = sum + noise * (amp * ws),
                    FractalKind::Turbulence => {
                        sum = sum + noise.abs() * (amp * ws) + amp * (wf - ws) * PERLIN_MEAN_ABS;
                    }
                    FractalKind::Ridged => {
                        let s = noise.abs() * -1.0 + offset;
                        let signal = s * s * weight;
                        weight = (signal * self.sharpness).clamp(0.0, 1.0);
                        sum = sum + signal * (amp * ws);
                    }
                    FractalKind::Hybrid => {
                        let signal = (noise + offset) * (amp * ws);
                        if i == 0 {
                            sum = signal;
                            weight = signal;
                        } else {
                            weight = weight.clamp(f32::NEG_INFINITY, 1.0);
                            sum = sum + weight * signal;
                            weight = weight * signal;
                        }
                    }
                    FractalKind::Terrain => {
                        if i == 0 {
                            sum = noise + offset;
                        } else {
                            sum = sum + (noise + offset) * sum * (amp * ws);
                        }
                    }
                }
            } else if kind == FractalKind::Turbulence {
                sum = sum + amp * wf * PERLIN_MEAN_ABS;
            }
            q = q * self.lacunarity + Vec3::splat(OCTAVE_SHIFT);
            freq *= self.lacunarity;
            amp *= gain;
        }
        let norm = norm.max(1e-6);
        match kind {
            FractalKind::Fbm => sum * (1.0 / norm) + 0.5,
            FractalKind::Turbulence => sum * (2.0 / norm),
            FractalKind::Ridged => sum * (1.0 / (norm * (offset * offset).max(1e-6))),
            FractalKind::Hybrid | FractalKind::Terrain => sum * (1.0 / (norm * (1.0 + offset.abs()))),
        }
    }

    /// Marble: stripes across x, bent by turbulence `amount`, in [0, 1].
    pub fn marble(&self, p: Vec3, amount: f32, footprint: f32) -> f32 {
        self.marble_g::<f32>(p, amount, self.octaves_for(footprint))
    }

    pub fn marble_grad(&self, p: Vec3, amount: f32, footprint: f32) -> (f32, Vec3) {
        let d = self.marble_g::<Dual>(p, amount, self.octaves_for(footprint));
        (d.v, d.g)
    }

    fn marble_g<S: Scalar>(&self, p: Vec3, amount: f32, n: f32) -> S {
        let x = S::linear(p.x * self.scale, Vec3::new(self.scale, 0.0, 0.0));
        let turb = self.sum::<S>(FractalKind::Turbulence, p, n);
        (x + turb * amount).sin() * 0.5 + 0.5
    }

    /// Wood: rings around the y axis, `scale` per unit, wobbled by fBm `amount`, in [0, 1).
    pub fn wood(&self, p: Vec3, amount: f32, footprint: f32) -> f32 {
        self.wood_g::<f32>(p, amount, self.octaves_for(footprint))
    }

    pub fn wood_grad(&self, p: Vec3, amount: f32, footprint: f32) -> (f32, Vec3) {
        let d = self.wood_g::<Dual>(p, amount, self.octaves_for(footprint));
        (d.v, d.g)
    }

    fn wood_g<S: Scalar>(&self, p: Vec3, amount: f32, n: f32) -> S {
        let s = self.scale;
        let x = S::linear(p.x * s, Vec3::new(s, 0.0, 0.0));
        let z = S::linear(p.z * s, Vec3::new(0.0, 0.0, s));
        let r = (x * x + z * z).sqrt();
        let wobble = self.sum::<S>(FractalKind::Fbm, p, n) + -0.5;
        (r + wobble * amount).fract()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rng::Rng;

    fn random_points(n: usize, seed: u64) -> Vec<Vec3> {
        let mut rng = Rng::new(seed, 1);
        (0..n).map(|_| Vec3::new(rng.f32() * 40.0 - 20.0, rng.f32() * 40.0 - 20.0, rng.f32() * 40.0 - 20.0)).collect()
    }

    #[test]
    fn noise_is_bounded_and_varies() {
        let mut lo = f32::MAX;
        let mut hi = f32::MIN;
        for i in 0..2000 {
            let p = Vec3::new(i as f32 * 0.137, i as f32 * 0.071, i as f32 * 0.029);
            let n = perlin(p);
            lo = lo.min(n);
            hi = hi.max(n);
        }
        assert!(lo > -1.1 && hi < 1.1 && hi - lo > 0.8, "{lo} {hi}");
        assert_eq!(perlin(Vec3::new(3.0, 4.0, 5.0)), 0.0);
    }

    #[test]
    fn mean_abs_matches_constant() {
        let pts = random_points(400_000, 3);
        let mean = pts.iter().map(|&p| perlin(p).abs()).sum::<f32>() / pts.len() as f32;
        assert!((mean - PERLIN_MEAN_ABS).abs() < 0.003, "mean |noise| = {mean}");
    }

    /// Analytic gradients agree with central differences, for the noise and every fractal
    /// (with and without domain warping, and the patterns).
    #[test]
    fn gradients_match_finite_differences() {
        let fd = |f: &dyn Fn(Vec3) -> f32, p: Vec3, h: f32| {
            let d = |e: Vec3| (f(p + e * h) - f(p - e * h)) / (2.0 * h);
            Vec3::new(d(Vec3::new(1.0, 0.0, 0.0)), d(Vec3::new(0.0, 1.0, 0.0)), d(Vec3::Z))
        };
        let check = |name: &str, f: &dyn Fn(Vec3) -> f32, g: &dyn Fn(Vec3) -> (f32, Vec3)| {
            let (mut checked, mut off) = (0, 0);
            // Near the origin, where f32 positions resolve the difference steps finely.
            for p in random_points(300, 5).into_iter().map(|p| p * 0.15) {
                let (v, an) = g(p);
                assert!((v - f(p)).abs() < 1e-4, "{name}: value differs at {p:?}");
                let num = fd(f, p, 2.5e-4);
                // Skip points next to a kink (|noise| at zero, clamped weights, fract jumps):
                // there, differences over two step sizes disagree.
                let kink = (fd(f, p, 1e-3) - num).length() > 0.01 * (1.0 + num.length());
                if kink {
                    continue;
                }
                checked += 1;
                if (an - num).length() / (1.0 + num.length()) > 0.02 {
                    off += 1;
                }
            }
            // A wrong formula is off almost everywhere; a kink closer than the difference
            // steps can still slip past the kink test now and then.
            assert!(checked > 150, "{name}: too few smooth points ({checked})");
            assert!(off * 50 <= checked, "{name}: analytic gradient off at {off} of {checked} points");
        };
        check("perlin", &|p| perlin(p), &perlin_grad);
        for kind in
            [FractalKind::Fbm, FractalKind::Turbulence, FractalKind::Ridged, FractalKind::Hybrid, FractalKind::Terrain]
        {
            for warp in [0.0, 0.7] {
                let fr = Fractal { scale: 0.7, octaves: 4.5, warp, ..Fractal::new(kind) };
                let name = format!("{kind:?} warp {warp}");
                check(&name, &|p| fr.eval(p, 0.0), &|p| fr.eval_grad(p, 0.0));
                // Footprint-limited sums too (2.3 octaves).
                let fp = 0.5 / (fr.scale * 2f32.powf(1.3));
                check(&format!("{name} clamped"), &|p| fr.eval(p, fp), &|p| fr.eval_grad(p, fp));
            }
        }
        let fr = Fractal { scale: 0.4, octaves: 3.0, ..Fractal::new(FractalKind::Fbm) };
        check("marble", &|p| fr.marble(p, 3.0, 0.0), &|p| fr.marble_grad(p, 3.0, 0.0));
        check("wood", &|p| fr.wood(p, 0.5, 0.0), &|p| fr.wood_grad(p, 0.5, 0.0));
    }

    /// Fractals stay roughly in [0, 1], and the old fBm / turbulence textures are reproduced.
    #[test]
    fn fractal_ranges_and_compatibility() {
        let pts = random_points(20_000, 9);
        for kind in
            [FractalKind::Fbm, FractalKind::Turbulence, FractalKind::Ridged, FractalKind::Hybrid, FractalKind::Terrain]
        {
            let fr = Fractal { octaves: 6.0, ..Fractal::new(kind) };
            let (mut lo, mut hi, mut mean) = (f32::MAX, f32::MIN, 0.0);
            for &p in &pts {
                let v = fr.eval(p, 0.0);
                lo = lo.min(v);
                hi = hi.max(v);
                mean += v / pts.len() as f32;
            }
            assert!(
                lo > -0.5 && hi < 1.5 && hi - lo > 0.3 && (0.05..0.95).contains(&mean),
                "{kind:?}: {lo} {hi} {mean}"
            );
        }
        // The fBm of the previous texture code: octaves of noise at q, 2q + 17.17, ...
        let old_fbm = |p: Vec3, oct: u32| {
            let (mut sum, mut amp, mut norm, mut q) = (0.0, 1.0, 0.0, p);
            for _ in 0..oct {
                sum += amp * perlin(q);
                norm += amp;
                amp *= 0.5;
                q = q * 2.0 + Vec3::splat(17.17);
            }
            sum / norm
        };
        let fr = Fractal { octaves: 5.0, scale: 3.0, ..Fractal::new(FractalKind::Fbm) };
        for &p in pts.iter().take(100) {
            assert!((fr.eval(p, 0.0) - (0.5 + old_fbm(p * 3.0, 5))).abs() < 1e-5);
        }
    }

    /// Clamping by footprint keeps the mean (no darkening or brightening in the distance)
    /// and drops the fine octaves' variance.
    #[test]
    fn footprint_clamping_keeps_the_mean() {
        let pts = random_points(40_000, 11);
        for kind in [FractalKind::Fbm, FractalKind::Turbulence] {
            let fr = Fractal { octaves: 8.0, scale: 1.0, ..Fractal::new(kind) };
            let stats = |fp: f32| {
                let v: Vec<f32> = pts.iter().map(|&p| fr.eval(p, fp)).collect();
                let m = v.iter().sum::<f32>() / v.len() as f32;
                (m, v.iter().map(|x| (x - m) * (x - m)).sum::<f32>() / v.len() as f32)
            };
            let (m0, v0) = stats(0.0);
            assert_eq!(fr.octaves_for(0.0), 8.0);
            assert!((fr.octaves_for(0.5 / 8.0) - 4.0).abs() < 1e-4);
            for fp in [0.02, 0.1, 0.5, 4.0] {
                let (m, v) = stats(fp);
                assert!((m - m0).abs() < 0.02, "{kind:?} footprint {fp}: mean {m} vs {m0}");
                assert!(v <= v0 * 1.01, "{kind:?} footprint {fp}: variance grew");
            }
            assert!(stats(4.0).1 < 0.1 * v0, "{kind:?}: a huge footprint averages the pattern out");
        }
    }
}
