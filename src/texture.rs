//! Textures: images (bilinear or nearest lookup, wrap modes, sRGB decoding) and
//! procedural patterns (checker, Perlin fBm / turbulence). Textures are evaluated at a
//! `TexCtx` holding the surface UV (OBJ convention: v = 0 at the bottom of the image)
//! and the world-space position.

use crate::imageio::{Pixels, RawImage};
use crate::math::Vec3;
use std::sync::{Arc, OnceLock};

#[derive(Clone, Copy, Debug, Default)]
pub struct TexCtx {
    pub uv: [f32; 2],
    pub p: Vec3,
}

// ---------------------------------------------------------------------------- images

enum Texels {
    /// 8-bit storage, decoded through a 256-entry table on lookup.
    U8 { data: Vec<u8>, srgb: bool },
    F32(Vec<f32>),
}

pub struct TexImage {
    pub width: usize,
    pub height: usize,
    /// 1 = gray, 2 = gray + alpha, 3 = RGB, 4 = RGBA.
    pub channels: usize,
    texels: Texels,
    /// Average linear color.
    pub mean: Vec3,
}

fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

fn lut(srgb: bool) -> &'static [f32; 256] {
    static SRGB: OnceLock<[f32; 256]> = OnceLock::new();
    static LINEAR: OnceLock<[f32; 256]> = OnceLock::new();
    if srgb {
        SRGB.get_or_init(|| std::array::from_fn(|i| srgb_to_linear(i as f32 / 255.0)))
    } else {
        LINEAR.get_or_init(|| std::array::from_fn(|i| i as f32 / 255.0))
    }
}

impl TexImage {
    /// `srgb` decodes the color channels of integer images (never alpha, never float images).
    pub fn new(raw: RawImage, srgb: bool) -> TexImage {
        let ch = raw.channels;
        let color_ch = if ch >= 3 { 3 } else { 1 };
        let texels = match raw.pixels {
            Pixels::U8(data) => Texels::U8 { data, srgb },
            Pixels::U16(data) => Texels::F32(
                data.iter()
                    .enumerate()
                    .map(|(i, &v)| {
                        let x = v as f32 / 65535.0;
                        if srgb && i % ch < color_ch { srgb_to_linear(x) } else { x }
                    })
                    .collect(),
            ),
            Pixels::F32(data) => Texels::F32(data),
        };
        let mut img = TexImage { width: raw.width, height: raw.height, channels: ch, texels, mean: Vec3::ZERO };
        let mut sum = [0f64; 3];
        for y in 0..img.height {
            for x in 0..img.width {
                let t = img.texel(x, y);
                for k in 0..3 {
                    sum[k] += t[k] as f64;
                }
            }
        }
        let n = (img.width * img.height) as f64;
        img.mean = Vec3::new((sum[0] / n) as f32, (sum[1] / n) as f32, (sum[2] / n) as f32);
        img
    }

    /// Linear RGBA of one texel (gray is replicated, missing alpha is 1).
    #[inline]
    pub fn texel(&self, x: usize, y: usize) -> [f32; 4] {
        let ch = self.channels;
        let i = (y * self.width + x) * ch;
        let mut v = [0f32; 4];
        match &self.texels {
            Texels::U8 { data, srgb } => {
                let color = lut(*srgb);
                let linear = lut(false);
                for k in 0..ch {
                    let is_color = k < if ch >= 3 { 3 } else { 1 };
                    v[k] = if is_color { color[data[i + k] as usize] } else { linear[data[i + k] as usize] };
                }
            }
            Texels::F32(data) => v[..ch].copy_from_slice(&data[i..i + ch]),
        }
        match ch {
            1 => [v[0], v[0], v[0], 1.0],
            2 => [v[0], v[0], v[0], v[1]],
            3 => [v[0], v[1], v[2], 1.0],
            _ => v,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Wrap {
    Repeat,
    Clamp,
    Mirror,
}

impl Wrap {
    pub fn parse(s: &str) -> Option<Wrap> {
        match s {
            "repeat" | "wrap" => Some(Wrap::Repeat),
            "clamp" | "edge" => Some(Wrap::Clamp),
            "mirror" => Some(Wrap::Mirror),
            _ => None,
        }
    }
    #[inline]
    fn apply(self, i: i64, n: usize) -> usize {
        let n = n as i64;
        (match self {
            Wrap::Repeat => i.rem_euclid(n),
            Wrap::Clamp => i.clamp(0, n - 1),
            Wrap::Mirror => {
                let m = i.rem_euclid(2 * n);
                if m < n { m } else { 2 * n - 1 - m }
            }
        }) as usize
    }
}

/// Which value a scalar parameter (roughness, metallic, alpha, bump ...) reads.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Channel {
    /// The gray channel of gray images, luminance of color images.
    Auto,
    R,
    G,
    B,
    A,
    Luminance,
}

impl Channel {
    pub fn parse(s: &str) -> Option<Channel> {
        match s {
            "r" | "red" => Some(Channel::R),
            "g" | "green" => Some(Channel::G),
            "b" | "blue" => Some(Channel::B),
            "a" | "alpha" => Some(Channel::A),
            "luminance" | "lum" | "y" => Some(Channel::Luminance),
            "auto" => Some(Channel::Auto),
            _ => None,
        }
    }
}

pub struct ImageTex {
    pub img: Arc<TexImage>,
    pub scale: [f32; 2],
    pub offset: [f32; 2],
    pub wrap: Wrap,
    pub nearest: bool,
    pub channel: Channel,
}

impl ImageTex {
    pub fn lookup(&self, uv: [f32; 2]) -> [f32; 4] {
        let img = &*self.img;
        let (w, h) = (img.width, img.height);
        let u = uv[0] * self.scale[0] + self.offset[0];
        let v = uv[1] * self.scale[1] + self.offset[1];
        // Texel centers sit at half-integers; image row 0 is the top (v = 1).
        let x = u * w as f32 - 0.5;
        let y = (1.0 - v) * h as f32 - 0.5;
        if !(x.is_finite() && y.is_finite()) {
            return [0.0; 4];
        }
        if self.nearest {
            return img.texel(self.wrap.apply(x.round() as i64, w), self.wrap.apply(y.round() as i64, h));
        }
        let (x0, y0) = (x.floor(), y.floor());
        let (fx, fy) = (x - x0, y - y0);
        let (x0, y0) = (x0 as i64, y0 as i64);
        let (xa, xb) = (self.wrap.apply(x0, w), self.wrap.apply(x0 + 1, w));
        let (ya, yb) = (self.wrap.apply(y0, h), self.wrap.apply(y0 + 1, h));
        let (a, b, c, d) = (img.texel(xa, ya), img.texel(xb, ya), img.texel(xa, yb), img.texel(xb, yb));
        std::array::from_fn(|k| {
            (a[k] * (1.0 - fx) + b[k] * fx) * (1.0 - fy) + (c[k] * (1.0 - fx) + d[k] * fx) * fy
        })
    }

    fn scalar(&self, px: [f32; 4]) -> f32 {
        let lum = || Vec3::new(px[0], px[1], px[2]).luminance();
        match self.channel {
            Channel::R => px[0],
            Channel::G => px[1],
            Channel::B => px[2],
            Channel::A => px[3],
            Channel::Luminance => lum(),
            Channel::Auto => {
                if self.img.channels <= 2 {
                    px[0]
                } else {
                    lum()
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------- textures

pub enum Texture {
    Const(Vec3),
    Image(Box<ImageTex>),
    /// A texture multiplied by a constant.
    Scaled(Box<Texture>, Vec3),
    Checker { even: Box<Texture>, odd: Box<Texture>, scale: f32, world: bool },
    /// Blends `low` -> `high` by Perlin fBm (or turbulence).
    Noise { low: Box<Texture>, high: Box<Texture>, scale: f32, octaves: u32, turbulence: bool, world: bool },
}

impl Texture {
    pub fn constant(&self) -> Option<Vec3> {
        match self {
            Texture::Const(c) => Some(*c),
            _ => None,
        }
    }

    pub fn eval(&self, c: &TexCtx) -> Vec3 {
        match self {
            Texture::Const(v) => *v,
            Texture::Image(t) => {
                let px = t.lookup(c.uv);
                Vec3::new(px[0], px[1], px[2])
            }
            Texture::Scaled(t, s) => t.eval(c) * *s,
            Texture::Checker { even, odd, .. } => {
                if self.checker_odd(c) {
                    odd.eval(c)
                } else {
                    even.eval(c)
                }
            }
            Texture::Noise { low, high, .. } => low.eval(c).lerp(high.eval(c), self.noise_t(c)),
        }
    }

    /// Scalar value: the selected channel of images, the first component otherwise.
    pub fn eval_f(&self, c: &TexCtx) -> f32 {
        match self {
            Texture::Const(v) => v.x,
            Texture::Image(t) => t.scalar(t.lookup(c.uv)),
            Texture::Scaled(t, s) => t.eval_f(c) * s.x,
            Texture::Checker { even, odd, .. } => {
                if self.checker_odd(c) {
                    odd.eval_f(c)
                } else {
                    even.eval_f(c)
                }
            }
            Texture::Noise { low, high, .. } => {
                let t = self.noise_t(c);
                low.eval_f(c) * (1.0 - t) + high.eval_f(c) * t
            }
        }
    }

    /// Approximate average color (light power estimates).
    pub fn mean(&self) -> Vec3 {
        match self {
            Texture::Const(v) => *v,
            Texture::Image(t) => t.img.mean,
            Texture::Scaled(t, s) => t.mean() * *s,
            Texture::Checker { even, odd, .. } => (even.mean() + odd.mean()) * 0.5,
            Texture::Noise { low, high, .. } => (low.mean() + high.mean()) * 0.5,
        }
    }

    /// A UV step suitable for finite differences (bump mapping).
    pub fn du(&self) -> f32 {
        match self {
            Texture::Image(t) => 0.5 / (t.img.width.max(t.img.height) as f32 * t.scale[0].abs().max(t.scale[1].abs()).max(1e-6)),
            Texture::Scaled(t, _) => t.du(),
            _ => 1e-3,
        }
    }

    fn pattern_point(c: &TexCtx, scale: f32, world: bool) -> Vec3 {
        if world { c.p * scale } else { Vec3::new(c.uv[0] * scale, c.uv[1] * scale, 0.0) }
    }

    fn checker_odd(&self, c: &TexCtx) -> bool {
        let Texture::Checker { scale, world, .. } = self else { return false };
        let p = Self::pattern_point(c, *scale, *world);
        // A tiny bias keeps axis-aligned planes at integer coordinates from flickering.
        let f = |v: f32| (v + 1e-4).floor() as i64;
        (f(p.x) + f(p.y) + f(p.z)) & 1 == 1
    }

    fn noise_t(&self, c: &TexCtx) -> f32 {
        let Texture::Noise { scale, octaves, turbulence, world, .. } = self else { return 0.0 };
        let p = Self::pattern_point(c, *scale, *world);
        if *turbulence { (2.0 * turbulence_fn(p, *octaves)).min(1.0) } else { (0.5 + fbm(p, *octaves)).clamp(0.0, 1.0) }
    }
}

// ---------------------------------------------------------------------------- noise

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

/// Improved Perlin noise, roughly in [-1, 1].
pub fn perlin(p: Vec3) -> f32 {
    let perm = permutation();
    let (fx, fy, fz) = (p.x.floor(), p.y.floor(), p.z.floor());
    let (x, y, z) = (p.x - fx, p.y - fy, p.z - fz);
    let (xi, yi, zi) = ((fx as i64 & 255) as usize, (fy as i64 & 255) as usize, (fz as i64 & 255) as usize);
    let fade = |t: f32| t * t * t * (t * (t * 6.0 - 15.0) + 10.0);
    let (u, v, w) = (fade(x), fade(y), fade(z));
    let lerp = |t: f32, a: f32, b: f32| a + t * (b - a);
    let a = perm[xi] as usize + yi;
    let (aa, ab) = (perm[a] as usize + zi, perm[a + 1] as usize + zi);
    let b = perm[xi + 1] as usize + yi;
    let (ba, bb) = (perm[b] as usize + zi, perm[b + 1] as usize + zi);
    lerp(
        w,
        lerp(
            v,
            lerp(u, grad(perm[aa], x, y, z), grad(perm[ba], x - 1.0, y, z)),
            lerp(u, grad(perm[ab], x, y - 1.0, z), grad(perm[bb], x - 1.0, y - 1.0, z)),
        ),
        lerp(
            v,
            lerp(u, grad(perm[aa + 1], x, y, z - 1.0), grad(perm[ba + 1], x - 1.0, y, z - 1.0)),
            lerp(u, grad(perm[ab + 1], x, y - 1.0, z - 1.0), grad(perm[bb + 1], x - 1.0, y - 1.0, z - 1.0)),
        ),
    )
}

/// Fractional Brownian motion, normalized so most values fall in [-0.5, 0.5].
fn fbm(p: Vec3, octaves: u32) -> f32 {
    let (mut sum, mut amp, mut norm, mut q) = (0.0, 1.0, 0.0, p);
    for _ in 0..octaves.max(1) {
        sum += amp * perlin(q);
        norm += amp;
        amp *= 0.5;
        q = q * 2.0 + Vec3::splat(17.17);
    }
    sum / norm
}

fn turbulence_fn(p: Vec3, octaves: u32) -> f32 {
    let (mut sum, mut amp, mut norm, mut q) = (0.0, 1.0, 0.0, p);
    for _ in 0..octaves.max(1) {
        sum += amp * perlin(q).abs();
        norm += amp;
        amp *= 0.5;
        q = q * 2.0 + Vec3::splat(17.17);
    }
    sum / norm
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tex(data: Vec<u8>, w: usize, h: usize, ch: usize, wrap: Wrap) -> ImageTex {
        let raw = RawImage { width: w, height: h, channels: ch, pixels: Pixels::U8(data) };
        ImageTex {
            img: Arc::new(TexImage::new(raw, false)),
            scale: [1.0, 1.0],
            offset: [0.0, 0.0],
            wrap,
            nearest: false,
            channel: Channel::Auto,
        }
    }

    #[test]
    fn bilinear_and_wrap_modes() {
        // 2x1 gray: 0, 255. Top-left origin, v = 1 - row.
        let t = tex(vec![0, 255], 2, 1, 1, Wrap::Clamp);
        assert_eq!(t.lookup([0.25, 0.5])[0], 0.0); // texel center of x = 0
        assert!((t.lookup([0.5, 0.5])[0] - 0.5).abs() < 1e-6); // halfway between centers
        assert_eq!(t.lookup([2.0, 0.5])[0], 1.0); // clamped
        let r = tex(vec![0, 255], 2, 1, 1, Wrap::Repeat);
        assert!((r.lookup([1.0, 0.5])[0] - 0.5).abs() < 1e-6); // wraps 255 -> 0 at the seam
        let m = tex(vec![0, 255], 2, 1, 1, Wrap::Mirror);
        assert_eq!(m.lookup([1.25, 0.5])[0], 1.0); // mirrored copy: x=0.5 texel -> 255

        // Row 0 is the top of the image (v = 1).
        let v = tex(vec![255, 0], 1, 2, 1, Wrap::Clamp);
        assert_eq!(v.lookup([0.5, 0.75])[0], 1.0);
        assert_eq!(v.lookup([0.5, 0.25])[0], 0.0);
    }

    #[test]
    fn srgb_applies_to_color_only() {
        let raw = RawImage { width: 1, height: 1, channels: 4, pixels: Pixels::U8(vec![128, 128, 128, 128]) };
        let img = TexImage::new(raw, true);
        let px = img.texel(0, 0);
        assert!((px[0] - 0.2158).abs() < 1e-3);
        assert!((px[3] - 128.0 / 255.0).abs() < 1e-6);
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
        // Zero at lattice points.
        assert_eq!(perlin(Vec3::new(3.0, 4.0, 5.0)), 0.0);
    }
}
