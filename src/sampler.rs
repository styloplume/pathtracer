//! Sample generation for the light transport: independent random numbers or
//! low-discrepancy sequences.
//!
//! A [`Sampler`] hands out the random numbers of one path, one *dimension* at a time
//! (`get1d`, or two dimensions forming a 2D pattern with `get2d`). The low-discrepancy
//! samplers index a sequence by the pixel's sample number, so the samples a pixel
//! accumulates over the progressive passes are well stratified in every dimension:
//!
//! - **Sobol**: the first two Sobol dimensions, with the sample index shuffled and the
//!   points Owen-scrambled by a hash of (pixel, dimension) (Burley 2020, "Practical
//!   Hash-based Owen Scrambling"). Every power-of-two prefix of a pixel's samples is a
//!   (0, m, 2)-net, so it works with any sample count, adaptive sampling and time limits.
//! - **Blue noise**: the same scrambled Sobol points, but shared by all pixels and
//!   toroidally shifted per pixel by a blue-noise mask (Georgiev & Fajardo 2016), so that
//!   at low sample counts the remaining error is spread as high-frequency blue noise
//!   instead of white noise. Converges like Sobol at high sample counts.

use crate::rng::{Rng, hash64};
use std::sync::OnceLock;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SamplerKind {
    /// Independent uniform random numbers (PCG32).
    Independent,
    /// Owen-scrambled Sobol points, decorrelated per pixel.
    #[default]
    Sobol,
    /// Sobol points shared by all pixels, shifted by a blue-noise mask.
    BlueNoise,
}

impl SamplerKind {
    pub fn parse(s: &str) -> Option<SamplerKind> {
        Some(match s {
            "independent" | "random" | "uniform" => SamplerKind::Independent,
            "sobol" | "owen" | "low-discrepancy" => SamplerKind::Sobol,
            "bluenoise" | "blue-noise" | "blue_noise" | "blue" => SamplerKind::BlueNoise,
            _ => return None,
        })
    }
}

pub const SAMPLER_NAMES: &str = "independent, sobol, bluenoise";

/// Random numbers for one camera path.
#[derive(Clone, Debug)]
pub struct Sampler {
    kind: SamplerKind,
    rng: Rng,
    /// Per pixel (Sobol) or per image (blue noise) scrambling seed.
    seed: u64,
    /// The pixel's sample number: the index into the sequence.
    index: u32,
    /// Next dimension to hand out.
    dim: u32,
    /// Pixel coordinates (blue-noise mask lookups).
    px: [u32; 2],
}

const DIM_MUL: u64 = 0x9E37_79B9_7F4A_7C15;

impl Sampler {
    /// The sampler for sample number `sample` of pixel (`x`, `y`) of a `width` wide image.
    pub fn new(kind: SamplerKind, x: u32, y: u32, width: u32, sample: u32, seed: u64) -> Sampler {
        let pixel = u64::from(y) * u64::from(width) + u64::from(x);
        let pixel_seed = hash64(pixel ^ (seed << 40));
        let (rng, seed) = match kind {
            SamplerKind::Independent => (Rng::new(hash64(pixel_seed ^ u64::from(sample)), pixel), 0),
            SamplerKind::Sobol => (Rng::new(0, 0), pixel_seed),
            SamplerKind::BlueNoise => (Rng::new(0, 0), hash64(seed ^ 0xB1E5_5EED)),
        };
        Sampler { kind, rng, seed, index: sample, dim: 0, px: [x, y] }
    }

    /// Independent random numbers from a PCG stream.
    #[cfg(test)]
    pub fn independent(seed: u64, stream: u64) -> Sampler {
        Sampler { kind: SamplerKind::Independent, rng: Rng::new(seed, stream), seed: 0, index: 0, dim: 0, px: [0; 2] }
    }

    #[inline]
    fn next_dim(&mut self) -> u64 {
        let d = self.dim;
        self.dim = self.dim.wrapping_add(1);
        hash64(self.seed ^ u64::from(d).wrapping_mul(DIM_MUL))
    }

    /// One uniform number in [0, 1).
    #[inline]
    pub fn get1d(&mut self) -> f32 {
        if self.kind == SamplerKind::Independent {
            return self.rng.f32();
        }
        let h = self.next_dim();
        let i = nested_uniform_scramble(self.index, h as u32);
        let x = to_unit(nested_uniform_scramble(i.reverse_bits(), (h >> 32) as u32));
        if self.kind == SamplerKind::BlueNoise { shift(x, self.mask(h, 0)) } else { x }
    }

    /// Two uniform numbers in [0, 1) forming a well-distributed 2D pattern.
    #[inline]
    pub fn get2d(&mut self) -> (f32, f32) {
        if self.kind == SamplerKind::Independent {
            return (self.rng.f32(), self.rng.f32());
        }
        let h = self.next_dim();
        let i = nested_uniform_scramble(self.index, h as u32);
        let (x, y) = sobol2(i);
        let h2 = hash64(h);
        let x = to_unit(nested_uniform_scramble(x, (h >> 32) as u32));
        let y = to_unit(nested_uniform_scramble(y, h2 as u32));
        if self.kind == SamplerKind::BlueNoise {
            (shift(x, self.mask(h, 0)), shift(y, self.mask(h2, 1)))
        } else {
            (x, y)
        }
    }

    /// The blue-noise mask value of this pixel, toroidally offset by a hash of the dimension.
    #[inline]
    fn mask(&self, h: u64, k: u32) -> f32 {
        let h = h >> (16 + 12 * k);
        let x = (self.px[0] as usize + (h as usize & (BN_SIZE - 1))) & (BN_SIZE - 1);
        let y = (self.px[1] as usize + ((h >> 6) as usize & (BN_SIZE - 1))) & (BN_SIZE - 1);
        blue_noise()[y * BN_SIZE + x]
    }
}

#[inline]
fn shift(x: f32, by: f32) -> f32 {
    let v = x + by;
    if v >= 1.0 { (v - 1.0).min(crate::sampling::ONE_MINUS_EPSILON) } else { v }
}

/// 32-bit fixed point to [0, 1).
#[inline]
fn to_unit(x: u32) -> f32 {
    (x >> 8) as f32 * (1.0 / 16_777_216.0)
}

/// Laine–Karras style permutation: every output bit depends only on the same and lower
/// input bits (constants by Nathan Vegdahl, improving on Burley 2020).
#[inline]
fn lk_permute(mut x: u32, seed: u32) -> u32 {
    x ^= x.wrapping_mul(0x3d20_adea);
    x = x.wrapping_add(seed);
    x = x.wrapping_mul((seed >> 16) | 1);
    x ^= x.wrapping_mul(0x0552_6c56);
    x ^= x.wrapping_mul(0x53a2_2864);
    x
}

/// Owen scrambling of a 32-bit fixed-point number (each bit is flipped depending on the
/// more significant bits). Applied to a sample index it shuffles the sequence order
/// while keeping every power-of-two prefix an aligned block of the original sequence.
#[inline]
fn nested_uniform_scramble(x: u32, seed: u32) -> u32 {
    lk_permute(x.reverse_bits(), seed).reverse_bits()
}

/// The first two dimensions of the Sobol sequence at index `i`, as 32-bit fixed point.
#[inline]
fn sobol2(mut i: u32) -> (u32, u32) {
    let x = i.reverse_bits();
    // Direction numbers of the second dimension (primitive polynomial x + 1).
    let (mut y, mut v) = (0u32, 1u32 << 31);
    while i != 0 {
        if i & 1 != 0 {
            y ^= v;
        }
        i >>= 1;
        v ^= v >> 1;
    }
    (x, y)
}

// ---------------------------------------------------------------------------- blue noise

const BN_SIZE: usize = 64;

/// A tileable 64x64 blue-noise mask with values `(rank + 0.5) / 4096`, generated once with
/// Ulichney's void-and-cluster method (Gaussian energy, sigma 1.9, toroidal).
fn blue_noise() -> &'static [f32] {
    static MASK: OnceLock<Vec<f32>> = OnceLock::new();
    MASK.get_or_init(|| {
        void_and_cluster(BN_SIZE, 0x5EED_B1E5)
            .into_iter()
            .map(|r| (r as f32 + 0.5) / (BN_SIZE * BN_SIZE) as f32)
            .collect()
    })
}

/// Ranks 0..n*n of an `n`x`n` (power of two) blue-noise dither array.
fn void_and_cluster(n: usize, seed: u64) -> Vec<u32> {
    const R: i32 = 9;
    let sigma = 1.9f32;
    let total = n * n;
    let mask = n - 1;
    let kernel: Vec<(i32, i32, f32)> = (-R..=R)
        .flat_map(|dy| (-R..=R).map(move |dx| (dx, dy)))
        .map(|(dx, dy)| (dx, dy, (-((dx * dx + dy * dy) as f32) / (2.0 * sigma * sigma)).exp()))
        .collect();
    let splat = |energy: &mut [f32], i: usize, sign: f32| {
        let (x, y) = ((i % n) as i32, (i / n) as i32);
        for &(dx, dy, w) in &kernel {
            let j = ((y + dy) as usize & mask) * n + ((x + dx) as usize & mask);
            energy[j] += sign * w;
        }
    };
    // Tightest cluster among the set pixels / largest void among the empty ones.
    let extreme = |energy: &[f32], on: &[bool], want_on: bool| -> usize {
        let mut best = (usize::MAX, if want_on { f32::NEG_INFINITY } else { f32::INFINITY });
        for (i, (&e, &o)) in energy.iter().zip(on).enumerate() {
            if o == want_on && (if want_on { e > best.1 } else { e < best.1 }) {
                best = (i, e);
            }
        }
        best.0
    };

    // Initial binary pattern: 10% random points, relaxed to be evenly spread.
    let mut rng = Rng::new(seed, 1);
    let mut on = vec![false; total];
    let mut energy = vec![0.0f32; total];
    let ones = total / 10;
    let mut placed = 0;
    while placed < ones {
        let i = rng.next_u32() as usize % total;
        if !on[i] {
            on[i] = true;
            splat(&mut energy, i, 1.0);
            placed += 1;
        }
    }
    for _ in 0..total * 4 {
        let c = extreme(&energy, &on, true);
        on[c] = false;
        splat(&mut energy, c, -1.0);
        let v = extreme(&energy, &on, false);
        on[v] = true;
        splat(&mut energy, v, 1.0);
        if v == c {
            break;
        }
    }

    let mut rank = vec![0u32; total];
    // Phase 1: remove the tightest clusters of the initial pattern, ranking downwards.
    let (mut on1, mut energy1) = (on.clone(), energy.clone());
    for r in (0..ones).rev() {
        let c = extreme(&energy1, &on1, true);
        on1[c] = false;
        splat(&mut energy1, c, -1.0);
        rank[c] = r as u32;
    }
    // Phases 2 and 3: fill the largest voids, ranking upwards. (Past half full, the
    // tightest cluster of empty pixels is the largest void of the set ones.)
    for r in ones..total {
        let v = extreme(&energy, &on, false);
        on[v] = true;
        splat(&mut energy, v, 1.0);
        rank[v] = r as u32;
    }
    rank
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every elementary interval of area 1/2^m holds exactly one of the first 2^m points.
    fn assert_net(points: &[(f32, f32)]) {
        let n = points.len();
        let m = n.trailing_zeros();
        for kx in 0..=m {
            let (nx, ny) = (1usize << kx, 1usize << (m - kx));
            let mut seen = vec![false; n];
            for &(x, y) in points {
                let cell = (y * ny as f32) as usize * nx + (x * nx as f32) as usize;
                assert!(!seen[cell], "cell {cell} of {nx}x{ny} hit twice");
                seen[cell] = true;
            }
        }
    }

    #[test]
    fn scrambled_sobol_prefixes_are_nets() {
        for pixel in [0u32, 17, 123_456] {
            for dim in 0..4 {
                for m in [4u32, 7] {
                    let pts: Vec<(f32, f32)> = (0..1u32 << m)
                        .map(|i| {
                            let mut s = Sampler::new(SamplerKind::Sobol, pixel, 3, 1000, i, 5);
                            for _ in 0..dim {
                                s.get2d();
                            }
                            s.get2d()
                        })
                        .collect();
                    assert_net(&pts);
                }
            }
        }
        // Unscrambled: the classic sequence.
        let pts: Vec<(f32, f32)> = (0..4).map(sobol2).map(|(x, y)| (to_unit(x), to_unit(y))).collect();
        assert_eq!(pts, vec![(0.0, 0.0), (0.5, 0.5), (0.25, 0.75), (0.75, 0.25)]);
    }

    #[test]
    fn samplers_are_uniform_and_decorrelated() {
        for kind in [SamplerKind::Independent, SamplerKind::Sobol, SamplerKind::BlueNoise] {
            // Mean of products of different dimensions ~ 1/4, means ~ 1/2.
            let (mut m, mut prod, mut n) = (0.0f64, 0.0f64, 0);
            for px in 0..64 {
                for i in 0..64 {
                    let mut s = Sampler::new(kind, px, px / 8, 8, i, 1);
                    let a = s.get1d();
                    let (b, c) = s.get2d();
                    assert!((0.0..1.0).contains(&a) && (0.0..1.0).contains(&b) && (0.0..1.0).contains(&c));
                    m += f64::from(a + b + c) / 3.0;
                    prod += f64::from(a * c);
                    n += 1;
                }
            }
            let (m, prod) = (m / f64::from(n), prod / f64::from(n));
            assert!((m - 0.5).abs() < 0.01 && (prod - 0.25).abs() < 0.01, "{kind:?}: mean {m}, E[ac] {prod}");
        }
    }

    #[test]
    fn blue_noise_mask_is_a_permutation_without_low_frequencies() {
        let mask = blue_noise();
        let mut ranks: Vec<u32> = mask.iter().map(|v| (v * 4096.0) as u32).collect();
        ranks.sort_unstable();
        assert!(ranks.iter().enumerate().all(|(i, &r)| r == i as u32));
        // Means of 4x4 blocks: white noise has variance (1/12) / 16; blue noise far less.
        let mut var = 0.0;
        for by in 0..16 {
            for bx in 0..16 {
                let mut s = 0.0;
                for y in 0..4 {
                    for x in 0..4 {
                        s += mask[(by * 4 + y) * BN_SIZE + bx * 4 + x];
                    }
                }
                var += (s / 16.0 - 0.5).powi(2);
            }
        }
        var /= 256.0;
        assert!(var < 0.25 * (1.0 / 12.0) / 16.0, "block variance {var}");
    }
}
