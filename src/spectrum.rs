//! Spectral rendering support: hero-wavelength sampling (Wilkie et al. 2014), an
//! analytic fit of the CIE 1931 colour matching functions (Wyman et al. 2013), and a
//! linear RGB -> spectrum uplift.
//!
//! The renderer's colour space is linear sRGB, white-balanced so that a constant
//! spectrum of 1 maps to RGB (1, 1, 1). RGB parameters are uplifted with three smooth
//! basis spectra that form a partition of unity and project exactly onto the RGB
//! primaries, so uplifting is linear and a single bounce reproduces RGB rendering;
//! spectral effects come from products of spectra and from wavelength-dependent IORs.
//!
//! The integrator is generic over [`Channels`]: plain RGB ([`Rgb`]) or four sampled
//! wavelengths ([`Wavelengths`]).

use crate::math::Vec3;
use std::ops::{Add, AddAssign, Mul, MulAssign};
use std::sync::OnceLock;

pub const LAMBDA_MIN: f32 = 360.0;
pub const LAMBDA_MAX: f32 = 830.0;
/// Wavelengths carried per path.
pub const N: usize = 4;
const TABLE_LEN: usize = (LAMBDA_MAX - LAMBDA_MIN) as usize + 1;

// ---------------------------------------------------------------------------- colour traits

/// Arithmetic needed by the integrator for path throughput and radiance.
pub trait Color:
    Copy
    + Default
    + Add<Output = Self>
    + AddAssign
    + Mul<Output = Self>
    + MulAssign
    + Mul<f32, Output = Self>
    + MulAssign<f32>
{
    /// Number of channels.
    const N: usize;
    fn splat(v: f32) -> Self;
    fn max_elem(self) -> f32;
    fn is_black(self) -> bool;
    fn is_finite(self) -> bool;
    fn channel(self, i: usize) -> f32;
    fn map(self, f: impl Fn(f32) -> f32) -> Self;
    /// Mean over the channels.
    fn avg(self) -> f32;
}

impl Color for Vec3 {
    const N: usize = 3;
    fn splat(v: f32) -> Self {
        Vec3::splat(v)
    }
    #[inline]
    fn channel(self, i: usize) -> f32 {
        self[i]
    }
    #[inline]
    fn map(self, f: impl Fn(f32) -> f32) -> Self {
        Vec3::new(f(self.x), f(self.y), f(self.z))
    }
    #[inline]
    fn avg(self) -> f32 {
        (self.x + self.y + self.z) * (1.0 / 3.0)
    }
    fn max_elem(self) -> f32 {
        Vec3::max_elem(self)
    }
    fn is_black(self) -> bool {
        Vec3::is_black(self)
    }
    fn is_finite(self) -> bool {
        Vec3::is_finite(self)
    }
}

/// How a path carries colour: RGB triples or sampled wavelengths.
pub trait Channels {
    type C: Color;
    /// Converts an RGB reflectance / radiance to this representation.
    fn uplift(&self, rgb: Vec3) -> Self::C;
    /// Beer–Lambert transmittance `exp(-sigma t)` for an RGB absorption coefficient.
    fn transmittance(&self, sigma: Vec3, t: f32) -> Self::C;
    /// The hero wavelength in nm (spectral mode only).
    fn hero(&self) -> Option<f32>;
    /// Continue with the hero wavelength only (after wavelength-dependent scattering).
    fn terminate_secondary(&mut self);
    fn to_rgb(&self, c: Self::C) -> Vec3;
}

/// Plain RGB rendering.
pub struct Rgb;

impl Channels for Rgb {
    type C = Vec3;
    #[inline]
    fn uplift(&self, rgb: Vec3) -> Vec3 {
        rgb
    }
    #[inline]
    fn transmittance(&self, sigma: Vec3, t: f32) -> Vec3 {
        Vec3::new((-sigma.x * t).exp(), (-sigma.y * t).exp(), (-sigma.z * t).exp())
    }
    fn hero(&self) -> Option<f32> {
        None
    }
    fn terminate_secondary(&mut self) {}
    fn to_rgb(&self, c: Vec3) -> Vec3 {
        c
    }
}

// ---------------------------------------------------------------------------- sampled spectra

/// Values at the `N` sampled wavelengths of a path.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Spectrum(pub [f32; N]);

impl Add for Spectrum {
    type Output = Spectrum;
    #[inline]
    fn add(self, o: Spectrum) -> Spectrum {
        Spectrum(std::array::from_fn(|i| self.0[i] + o.0[i]))
    }
}
impl AddAssign for Spectrum {
    #[inline]
    fn add_assign(&mut self, o: Spectrum) {
        *self = *self + o;
    }
}
impl Mul for Spectrum {
    type Output = Spectrum;
    #[inline]
    fn mul(self, o: Spectrum) -> Spectrum {
        Spectrum(std::array::from_fn(|i| self.0[i] * o.0[i]))
    }
}
impl MulAssign for Spectrum {
    #[inline]
    fn mul_assign(&mut self, o: Spectrum) {
        *self = *self * o;
    }
}
impl Mul<f32> for Spectrum {
    type Output = Spectrum;
    #[inline]
    fn mul(self, s: f32) -> Spectrum {
        Spectrum(self.0.map(|v| v * s))
    }
}
impl MulAssign<f32> for Spectrum {
    #[inline]
    fn mul_assign(&mut self, s: f32) {
        *self = *self * s;
    }
}

impl Color for Spectrum {
    const N: usize = N;
    fn splat(v: f32) -> Self {
        Spectrum([v; N])
    }
    #[inline]
    fn channel(self, i: usize) -> f32 {
        self.0[i]
    }
    #[inline]
    fn map(self, f: impl Fn(f32) -> f32) -> Self {
        Spectrum(self.0.map(f))
    }
    #[inline]
    fn avg(self) -> f32 {
        self.0.iter().sum::<f32>() * (1.0 / N as f32)
    }
    fn max_elem(self) -> f32 {
        self.0.iter().copied().fold(f32::NEG_INFINITY, f32::max)
    }
    fn is_black(self) -> bool {
        self.0.iter().all(|&v| v <= 0.0)
    }
    fn is_finite(self) -> bool {
        self.0.iter().all(|v| v.is_finite())
    }
}

/// The wavelengths carried by one path and their sampling densities.
#[derive(Clone, Copy, Debug)]
pub struct Wavelengths {
    pub lambda: [f32; N],
    pub pdf: [f32; N],
}

/// Samples a wavelength proportionally to the visual response (pbrt-v4).
pub fn sample_visible(u: f32) -> f32 {
    538.0 - 138.888_89 * (0.856_910_6 - 1.827_502 * u).atanh()
}

pub fn visible_pdf(lambda: f32) -> f32 {
    if !(LAMBDA_MIN..=LAMBDA_MAX).contains(&lambda) {
        return 0.0;
    }
    let c = (0.0072 * (lambda - 538.0)).cosh();
    0.003_939_804 / (c * c)
}

impl Wavelengths {
    /// Hero wavelength from `u`, the others at equal offsets in sample space.
    pub fn sample(u: f32) -> Wavelengths {
        let lambda = std::array::from_fn(|i| {
            let up = (u + i as f32 / N as f32).fract();
            sample_visible(up).clamp(LAMBDA_MIN, LAMBDA_MAX)
        });
        Wavelengths { lambda, pdf: lambda.map(visible_pdf) }
    }

    pub fn secondary_terminated(&self) -> bool {
        self.pdf[1..].iter().all(|&p| p == 0.0)
    }
}

impl Channels for Wavelengths {
    type C = Spectrum;

    #[inline]
    fn uplift(&self, rgb: Vec3) -> Spectrum {
        let t = tables();
        Spectrum(self.lambda.map(|l| lookup(&t.basis, l).dot(rgb).max(0.0)))
    }

    #[inline]
    fn transmittance(&self, sigma: Vec3, t: f32) -> Spectrum {
        Spectrum(self.uplift(sigma).0.map(|s| (-s * t).exp()))
    }

    fn hero(&self) -> Option<f32> {
        Some(self.lambda[0])
    }

    fn terminate_secondary(&mut self) {
        if self.secondary_terminated() {
            return;
        }
        for p in &mut self.pdf[1..] {
            *p = 0.0;
        }
        self.pdf[0] /= N as f32;
    }

    fn to_rgb(&self, c: Spectrum) -> Vec3 {
        let t = tables();
        let mut rgb = Vec3::ZERO;
        for i in 0..N {
            if self.pdf[i] > 0.0 {
                rgb += lookup(&t.rgb, self.lambda[i]) * (c.0[i] / self.pdf[i]);
            }
        }
        rgb * (1.0 / N as f32)
    }
}

// ---------------------------------------------------------------------------- colorimetry

/// Piecewise Gaussian used by the colour matching function fit.
fn g(x: f32, mu: f32, s1: f32, s2: f32) -> f32 {
    let t = (x - mu) / if x < mu { s1 } else { s2 };
    (-0.5 * t * t).exp()
}

/// CIE 1931 2° colour matching functions (multi-lobe fit, Wyman, Sloan & Shirley 2013).
pub fn cie_xyz(l: f32) -> Vec3 {
    Vec3::new(
        1.056 * g(l, 599.8, 37.9, 31.0) + 0.362 * g(l, 442.0, 16.0, 26.7) - 0.065 * g(l, 501.1, 20.4, 26.2),
        0.821 * g(l, 568.8, 46.9, 40.5) + 0.286 * g(l, 530.9, 16.3, 31.1),
        1.217 * g(l, 437.0, 11.8, 36.0) + 0.681 * g(l, 459.0, 26.0, 13.8),
    )
}

/// CIE XYZ to linear sRGB (D65).
pub fn xyz_to_srgb(c: Vec3) -> Vec3 {
    Vec3::new(
        3.240_454_2 * c.x - 1.537_138_5 * c.y - 0.498_531_4 * c.z,
        -0.969_266 * c.x + 1.876_010_8 * c.y + 0.041_556 * c.z,
        0.055_643_4 * c.x - 0.204_025_9 * c.y + 1.057_225_2 * c.z,
    )
}

/// Planck's law: spectral radiance of a black body (arbitrary units), `lambda` in nm.
pub fn blackbody(lambda: f32, kelvin: f32) -> f32 {
    let l = f64::from(lambda) * 1e-9;
    let (c, h, kb) = (299_792_458.0f64, 6.626_070_15e-34f64, 1.380_649e-23f64);
    let v = 2.0 * h * c * c / (l.powi(5) * ((h * c / (l * kb * f64::from(kelvin))).exp() - 1.0));
    (v * 1e-12) as f32
}

/// RGB colour of a black body at `kelvin`, normalized to luminance 1.
pub fn blackbody_rgb(kelvin: f32) -> Vec3 {
    let rgb = spectrum_to_rgb(|l| blackbody(l, kelvin));
    rgb * (1.0 / rgb.luminance().max(1e-12))
}

/// Integrates a spectral function (1 nm steps) to our white-balanced linear RGB.
pub fn spectrum_to_rgb(f: impl Fn(f32) -> f32) -> Vec3 {
    let t = tables();
    let mut rgb = Vec3::ZERO;
    for (i, w) in t.rgb.iter().enumerate() {
        rgb += *w * f(LAMBDA_MIN + i as f32);
    }
    rgb
}

struct Tables {
    /// Per-nm RGB response: integrating a spectrum against it gives its RGB colour.
    rgb: [Vec3; TABLE_LEN],
    /// Uplift basis: the spectrum of RGB (r, g, b) is `dot(basis(lambda), (r, g, b))`.
    basis: [Vec3; TABLE_LEN],
}

#[inline]
fn lookup(t: &[Vec3; TABLE_LEN], lambda: f32) -> Vec3 {
    let x = (lambda - LAMBDA_MIN).clamp(0.0, (TABLE_LEN - 1) as f32);
    let i = (x as usize).min(TABLE_LEN - 2);
    let f = x - i as f32;
    t[i] * (1.0 - f) + t[i + 1] * f
}

fn sigmoid(x: f32) -> f32 {
    1.0 / (1.0 + (-x).exp())
}

/// Raw (blue, green, red) partition-of-unity basis spectra.
fn raw_basis(l: f32) -> Vec3 {
    let (sb, sr) = (sigmoid((l - 490.0) / 9.0), sigmoid((l - 585.0) / 9.0));
    Vec3::new(sr, sb - sr, 1.0 - sb)
}

fn tables() -> &'static Tables {
    static T: OnceLock<Tables> = OnceLock::new();
    T.get_or_init(|| {
        let lambdas = || (0..TABLE_LEN).map(|i| LAMBDA_MIN + i as f32);
        let y_int: f32 = lambdas().map(|l| cie_xyz(l).y).sum();
        // RGB of a constant spectrum of 1 before white balancing.
        let white = xyz_to_srgb(lambdas().map(cie_xyz).fold(Vec3::ZERO, |a, b| a + b) * (1.0 / y_int));
        let rgb: [Vec3; TABLE_LEN] = std::array::from_fn(|i| {
            let c = xyz_to_srgb(cie_xyz(LAMBDA_MIN + i as f32) * (1.0 / y_int));
            Vec3::new(c.x / white.x, c.y / white.y, c.z / white.z)
        });
        // m[i][j] = RGB channel i of raw basis spectrum j.
        let mut m = [[0f32; 3]; 3];
        for (k, w) in rgb.iter().enumerate() {
            let b = raw_basis(LAMBDA_MIN + k as f32);
            for i in 0..3 {
                for j in 0..3 {
                    m[i][j] += w[i] * b[j];
                }
            }
        }
        let inv = crate::math::Mat3 { m }.inverse().expect("basis spectra are independent");
        // basis(l)_k = sum_j raw_j(l) inv[j][k]
        let basis = std::array::from_fn(|k| {
            let b = raw_basis(LAMBDA_MIN + k as f32);
            Vec3::new(
                b.x * inv.m[0][0] + b.y * inv.m[1][0] + b.z * inv.m[2][0],
                b.x * inv.m[0][1] + b.y * inv.m[1][1] + b.z * inv.m[2][1],
                b.x * inv.m[0][2] + b.y * inv.m[1][2] + b.z * inv.m[2][2],
            )
        });
        Tables { rgb, basis }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rng::Rng;

    #[test]
    fn visible_sampling_pdf_matches() {
        // The pdf integrates to ~1 and matches the sampler's histogram.
        let total: f32 = (0..4700).map(|i| visible_pdf(LAMBDA_MIN + i as f32 * 0.1) * 0.1).sum();
        assert!((total - 1.0).abs() < 0.01, "{total}");
        let mut rng = Rng::new(4, 4);
        let n = 100_000;
        let inside = (0..n).filter(|_| (500.0..600.0).contains(&sample_visible(rng.f32()))).count();
        let expect: f32 = (0..1000).map(|i| visible_pdf(500.0 + i as f32 * 0.1) * 0.1).sum();
        assert!((inside as f32 / n as f32 - expect).abs() < 0.01);
    }

    #[test]
    fn uplift_round_trips() {
        let mut rng = Rng::new(8, 1);
        for rgb in [Vec3::ONE, Vec3::new(0.73, 0.73, 0.73), Vec3::new(0.65, 0.05, 0.05), Vec3::new(0.12, 0.45, 0.15)]
            .into_iter()
            .chain([Vec3::new(0.2, 0.3, 0.8), Vec3::new(1.0, 0.78, 0.34), Vec3::new(17.0, 12.0, 4.0)])
        {
            let mut acc = Vec3::ZERO;
            let n = 40_000;
            for _ in 0..n {
                let wl = Wavelengths::sample(rng.f32());
                acc += wl.to_rgb(wl.uplift(rgb));
            }
            let got = acc * (1.0 / n as f32);
            let tol = 0.03 * rgb.max_elem();
            assert!((got - rgb).abs().max_elem() < tol, "{rgb:?} -> {got:?}");
        }
        // A constant spectrum is white, and white uplifts to a constant spectrum.
        let w = spectrum_to_rgb(|_| 1.0);
        assert!((w - Vec3::ONE).abs().max_elem() < 1e-3, "{w:?}");
        let s = Wavelengths::sample(0.3).uplift(Vec3::ONE);
        assert!(s.0.iter().all(|v| (v - 1.0).abs() < 1e-3), "{s:?}");
    }

    #[test]
    fn blackbody_colors() {
        let warm = blackbody_rgb(2700.0);
        let cool = blackbody_rgb(9000.0);
        assert!(warm.x > warm.y && warm.y > warm.z, "{warm:?}");
        assert!(cool.z > cool.x, "{cool:?}");
        assert!((warm.luminance() - 1.0).abs() < 1e-4);
    }

    #[test]
    fn terminating_secondaries_keeps_the_estimate() {
        let mut rng = Rng::new(3, 3);
        let rgb = Vec3::new(0.3, 0.6, 0.2);
        let mut acc = Vec3::ZERO;
        let n = 60_000;
        for _ in 0..n {
            let mut wl = Wavelengths::sample(rng.f32());
            let s = wl.uplift(rgb);
            wl.terminate_secondary();
            acc += wl.to_rgb(s);
        }
        let got = acc * (1.0 / n as f32);
        assert!((got - rgb).abs().max_elem() < 0.03, "{got:?}");
    }
}
