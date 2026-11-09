//! Tone mapping, display encoding and image writers (PNG, PFM, Radiance HDR).

use crate::deflate;
use crate::math::Vec3;
use std::io::Write;
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Tonemap {
    /// Plain clamp.
    None,
    Reinhard,
    /// Narkowicz's ACES filmic fit.
    Aces,
}

impl Tonemap {
    pub fn parse(s: &str) -> Option<Tonemap> {
        match s.to_ascii_lowercase().as_str() {
            "none" | "clamp" | "linear" => Some(Tonemap::None),
            "reinhard" => Some(Tonemap::Reinhard),
            "aces" | "filmic" => Some(Tonemap::Aces),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Tonemap::None => "clamp",
            Tonemap::Reinhard => "Reinhard",
            Tonemap::Aces => "ACES",
        }
    }

    /// Cycles through the available operators (for the interactive toggle).
    pub fn next(self) -> Tonemap {
        match self {
            Tonemap::Aces => Tonemap::Reinhard,
            Tonemap::Reinhard => Tonemap::None,
            Tonemap::None => Tonemap::Aces,
        }
    }

    #[inline]
    fn apply(self, x: f32) -> f32 {
        let x = x.max(0.0);
        match self {
            Tonemap::None => x,
            Tonemap::Reinhard => x / (1.0 + x),
            Tonemap::Aces => (x * (2.51 * x + 0.03)) / (x * (2.43 * x + 0.59) + 0.14),
        }
    }
}

#[inline]
fn srgb_encode(c: f32) -> f32 {
    let c = c.clamp(0.0, 1.0);
    if c <= 0.003_130_8 { 12.92 * c } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 }
}

const SRGB_LUT_SIZE: usize = 1 << 14;

/// Packs [0, 1] components into 0x00RRGGBB without any transfer function.
#[inline]
pub fn pack_unorm(c: Vec3) -> u32 {
    let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u32;
    (q(c.x) << 16) | (q(c.y) << 8) | q(c.z)
}

/// Converts linear radiance to packed 0x00RRGGBB display pixels: exposure, tone
/// mapping, then sRGB encoding through a lookup table (cheap enough to re-resolve
/// the whole image interactively when exposure or tonemap change).
#[derive(Clone)]
pub struct Encoder {
    scale: f32,
    tm: Tonemap,
    lut: Vec<u8>,
}

impl Encoder {
    pub fn new(exposure: f32, tm: Tonemap) -> Encoder {
        let n = SRGB_LUT_SIZE - 1;
        let lut = (0..SRGB_LUT_SIZE).map(|i| (srgb_encode(i as f32 / n as f32) * 255.0 + 0.5) as u8).collect();
        Encoder { scale: exposure.exp2(), tm, lut }
    }

    #[inline]
    fn srgb(&self, v: f32) -> u32 {
        let n = (SRGB_LUT_SIZE - 1) as f32;
        // NaN and negatives map to index 0 (`as usize` saturates).
        u32::from(self.lut[((v.min(1.0) * n + 0.5) as usize).min(SRGB_LUT_SIZE - 1)])
    }

    /// Exposure + tone mapping + sRGB.
    #[inline]
    pub fn encode(&self, c: Vec3) -> u32 {
        let q = |v: f32| self.srgb(self.tm.apply(v * self.scale));
        (q(c.x) << 16) | (q(c.y) << 8) | q(c.z)
    }

    /// sRGB only (for reflectance-like values such as albedo).
    #[inline]
    pub fn encode_srgb(&self, c: Vec3) -> u32 {
        (self.srgb(c.x) << 16) | (self.srgb(c.y) << 8) | self.srgb(c.z)
    }
}

pub fn packed_to_rgb(px: &[u32]) -> Vec<u8> {
    px.iter().flat_map(|&p| [(p >> 16) as u8, (p >> 8) as u8, p as u8]).collect()
}

fn write_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent()
        && !dir.as_os_str().is_empty()
    {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::File::create(path)?.write_all(bytes)
}

// ------------------------------------------------------------------ PNG

#[inline]
pub(crate) fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let p = i16::from(a) + i16::from(b) - i16::from(c);
    let (pa, pb, pc) = ((p - i16::from(a)).abs(), (p - i16::from(b)).abs(), (p - i16::from(c)).abs());
    if pa <= pb && pa <= pc {
        a
    } else if pb <= pc {
        b
    } else {
        c
    }
}

/// Applies PNG row filters, picking per row the filter with the smallest sum of
/// absolute (signed) residuals — the standard libpng heuristic.
fn filter_rows(data: &[u8], stride: usize, bpp: usize) -> Vec<u8> {
    let rows = data.len() / stride;
    let mut out = Vec::with_capacity((stride + 1) * rows);
    let zero = vec![0u8; stride];
    let mut cand = vec![vec![0u8; stride]; 5];
    for y in 0..rows {
        let cur = &data[y * stride..(y + 1) * stride];
        let up = if y > 0 { &data[(y - 1) * stride..y * stride] } else { &zero[..] };
        for i in 0..stride {
            let a = if i >= bpp { cur[i - bpp] } else { 0 };
            let c = if i >= bpp { up[i - bpp] } else { 0 };
            let (x, b) = (cur[i], up[i]);
            cand[0][i] = x;
            cand[1][i] = x.wrapping_sub(a);
            cand[2][i] = x.wrapping_sub(b);
            cand[3][i] = x.wrapping_sub(u16::midpoint(u16::from(a), u16::from(b)) as u8);
            cand[4][i] = x.wrapping_sub(paeth(a, b, c));
        }
        let score = |r: &[u8]| r.iter().map(|&v| u32::from((v as i8).unsigned_abs())).sum::<u32>();
        let best = (0..5).min_by_key(|&f| score(&cand[f])).unwrap();
        out.push(best as u8);
        out.extend_from_slice(&cand[best]);
    }
    out
}

/// Writes an 8-bit RGB PNG (filtered, DEFLATE-compressed).
pub fn write_png(path: &Path, width: usize, height: usize, rgb: &[u8]) -> std::io::Result<()> {
    write_file(path, &encode_png(width, height, rgb))
}

pub fn encode_png(width: usize, height: usize, rgb: &[u8]) -> Vec<u8> {
    assert_eq!(rgb.len(), width * height * 3);
    let z = deflate::zlib_compress(&filter_rows(rgb, width * 3, 3));

    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&(width as u32).to_be_bytes());
    ihdr.extend_from_slice(&(height as u32).to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]); // 8-bit, RGB, deflate, adaptive filtering, no interlace

    let mut out = Vec::with_capacity(z.len() + 64);
    out.extend_from_slice(&[137, 80, 78, 71, 13, 10, 26, 10]);
    write_chunk(&mut out, *b"IHDR", &ihdr);
    write_chunk(&mut out, *b"IDAT", &z);
    write_chunk(&mut out, *b"IEND", &[]);
    out
}

fn write_chunk(out: &mut Vec<u8>, kind: [u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let start = out.len();
    out.extend_from_slice(&kind);
    out.extend_from_slice(data);
    let crc = crc32(&out[start..]);
    out.extend_from_slice(&crc.to_be_bytes());
}

fn crc32(data: &[u8]) -> u32 {
    let mut table = [0u32; 256];
    for (i, e) in table.iter_mut().enumerate() {
        let mut c = i as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
        }
        *e = c;
    }
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc = table[((crc ^ u32::from(b)) & 0xFF) as usize] ^ (crc >> 8);
    }
    crc ^ 0xFFFF_FFFF
}

// ------------------------------------------------------------------ HDR formats

/// Portable float map: little-endian RGB f32, rows stored bottom to top.
pub fn write_pfm(path: &Path, width: usize, height: usize, rgb: &[Vec3]) -> std::io::Result<()> {
    assert_eq!(rgb.len(), width * height);
    let mut out = format!("PF\n{width} {height}\n-1.0\n").into_bytes();
    out.reserve(width * height * 12);
    for row in rgb.chunks(width).rev() {
        for c in row {
            for v in [c.x, c.y, c.z] {
                out.extend_from_slice(&v.to_le_bytes());
            }
        }
    }
    write_file(path, &out)
}

/// Shared-exponent RGBE encoding (Ward). Negative components clamp to zero.
fn rgbe(c: Vec3) -> [u8; 4] {
    let c = c.max(Vec3::ZERO);
    let v = c.max_elem();
    if v.is_nan() || v <= 1e-32 || v.is_infinite() {
        return [0; 4];
    }
    // v = m * 2^e with m in [0.5, 1).
    let mut e = v.log2().floor() as i32 + 1;
    if v >= (e as f32).exp2() {
        e += 1;
    } else if v < ((e - 1) as f32).exp2() {
        e -= 1;
    }
    let e = e.clamp(-127, 127);
    let scale = 256.0 / (e as f32).exp2();
    let q = |x: f32| (x * scale).min(255.0) as u8;
    [q(c.x), q(c.y), q(c.z), (e + 128) as u8]
}

/// Appends one RGBE channel of a scanline using the "new" Radiance run-length scheme.
fn rle_channel(out: &mut Vec<u8>, data: &[u8]) {
    let n = data.len();
    let mut i = 0;
    while i < n {
        // Find the next run of at least 4 equal bytes.
        let (mut j, mut run) = (i, 0);
        while j < n {
            run = 1;
            while j + run < n && run < 127 && data[j + run] == data[j] {
                run += 1;
            }
            if run >= 4 {
                break;
            }
            j += run;
        }
        let mut k = i;
        while k < j {
            let c = (j - k).min(128);
            out.push(c as u8);
            out.extend_from_slice(&data[k..k + c]);
            k += c;
        }
        if j < n {
            out.push(128 + run as u8);
            out.push(data[j]);
            i = j + run;
        } else {
            i = n;
        }
    }
}

/// Radiance `.hdr` (RGBE, run-length encoded scanlines when the width allows it).
pub fn write_hdr(path: &Path, width: usize, height: usize, rgb: &[Vec3]) -> std::io::Result<()> {
    write_file(path, &encode_hdr(width, height, rgb))
}

pub fn encode_hdr(width: usize, height: usize, rgb: &[Vec3]) -> Vec<u8> {
    assert_eq!(rgb.len(), width * height);
    let mut out = format!("#?RADIANCE\nFORMAT=32-bit_rle_rgbe\n\n-Y {height} +X {width}\n").into_bytes();
    let rle = (8..=0x7FFF).contains(&width);
    let mut chan = vec![0u8; width];
    for row in rgb.chunks(width) {
        let px: Vec<[u8; 4]> = row.iter().map(|&c| rgbe(c)).collect();
        if !rle {
            out.extend(px.iter().flatten());
            continue;
        }
        out.extend_from_slice(&[2, 2, (width >> 8) as u8, width as u8]);
        for ch in 0..4 {
            for (d, p) in chan.iter_mut().zip(&px) {
                *d = p[ch];
            }
            rle_channel(&mut out, &chan);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksums() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn png_roundtrips_through_filters_and_deflate() {
        let (w, h) = (37, 23);
        let rgb: Vec<u8> = (0..w * h * 3).map(|i| ((i * 7) ^ (i / 97)) as u8).collect();
        let png = encode_png(w, h, &rgb);
        // Pull the IDAT payload back out and undo the filters.
        let idat_len = u32::from_be_bytes(png[33..37].try_into().unwrap()) as usize;
        assert_eq!(&png[37..41], b"IDAT");
        let raw = deflate::zlib_decompress(&png[41..41 + idat_len]).unwrap();
        let stride = w * 3;
        let mut img = vec![0u8; stride * h];
        for y in 0..h {
            let f = raw[y * (stride + 1)];
            let src = &raw[y * (stride + 1) + 1..(y + 1) * (stride + 1)];
            for i in 0..stride {
                let a = if i >= 3 { img[y * stride + i - 3] } else { 0 };
                let b = if y > 0 { img[(y - 1) * stride + i] } else { 0 };
                let c = if i >= 3 && y > 0 { img[(y - 1) * stride + i - 3] } else { 0 };
                let pred = match f {
                    0 => 0,
                    1 => a,
                    2 => b,
                    3 => u16::midpoint(u16::from(a), u16::from(b)) as u8,
                    _ => paeth(a, b, c),
                };
                img[y * stride + i] = src[i].wrapping_add(pred);
            }
        }
        assert_eq!(img, rgb);
    }

    fn decode_hdr(data: &[u8]) -> (usize, usize, Vec<Vec3>) {
        let text_end = data.windows(2).position(|w| w == b"\n\n").unwrap() + 2;
        let line_end = text_end + data[text_end..].iter().position(|&b| b == b'\n').unwrap();
        let res = std::str::from_utf8(&data[text_end..line_end]).unwrap();
        let p: Vec<&str> = res.split_whitespace().collect();
        let (h, w): (usize, usize) = (p[1].parse().unwrap(), p[3].parse().unwrap());
        let mut pos = line_end + 1;
        let mut out = Vec::new();
        for _ in 0..h {
            let mut px = vec![[0u8; 4]; w];
            if data[pos] == 2 && data[pos + 1] == 2 {
                pos += 4;
                for ch in 0..4 {
                    let mut x = 0;
                    while x < w {
                        let c = data[pos] as usize;
                        pos += 1;
                        if c > 128 {
                            for p in &mut px[x..x + c - 128] {
                                p[ch] = data[pos];
                            }
                            pos += 1;
                            x += c - 128;
                        } else {
                            for (k, p) in px[x..x + c].iter_mut().enumerate() {
                                p[ch] = data[pos + k];
                            }
                            pos += c;
                            x += c;
                        }
                    }
                }
            } else {
                for p in &mut px {
                    p.copy_from_slice(&data[pos..pos + 4]);
                    pos += 4;
                }
            }
            out.extend(px.iter().map(|p| {
                if p[3] == 0 {
                    return Vec3::ZERO;
                }
                let f = ((i32::from(p[3]) - 136) as f32).exp2();
                Vec3::new((f32::from(p[0]) + 0.5) * f, (f32::from(p[1]) + 0.5) * f, (f32::from(p[2]) + 0.5) * f)
            }));
        }
        assert_eq!(pos, data.len());
        (w, h, out)
    }

    #[test]
    fn hdr_roundtrip() {
        for w in [5, 40] {
            let h = 3;
            let px: Vec<Vec3> = (0..w * h)
                .map(|i| if i % 9 < 5 { Vec3::new(1.0, 0.5, 0.25) } else { Vec3::new(i as f32 * 3.7, 0.01, 1e3) })
                .collect();
            let (dw, dh, back) = decode_hdr(&encode_hdr(w, h, &px));
            assert_eq!((dw, dh), (w, h));
            for (a, b) in px.iter().zip(&back) {
                let m = a.max_elem();
                assert!((*a - *b).abs().max_elem() <= m * 0.01, "{a:?} vs {b:?}");
            }
        }
    }
}
