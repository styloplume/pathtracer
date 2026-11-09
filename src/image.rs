//! Tone mapping, display encoding and a minimal PNG writer.

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

/// Converts linear radiance to packed 0x00RRGGBB display pixels: exposure, tone
/// mapping, then sRGB encoding through a lookup table (cheap enough to re-resolve
/// the whole image interactively when exposure or tonemap change).
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
    pub fn encode(&self, c: Vec3) -> u32 {
        let n = (SRGB_LUT_SIZE - 1) as f32;
        // NaN and negatives map to index 0 (`as usize` saturates).
        let q = |v: f32| self.lut[((self.tm.apply(v * self.scale).min(1.0) * n + 0.5) as usize).min(SRGB_LUT_SIZE - 1)] as u32;
        (q(c.x) << 16) | (q(c.y) << 8) | q(c.z)
    }
}

pub fn packed_to_rgb(px: &[u32]) -> Vec<u8> {
    px.iter().flat_map(|&p| [(p >> 16) as u8, (p >> 8) as u8, p as u8]).collect()
}

/// Writes an 8-bit RGB PNG. Uses uncompressed (stored) deflate blocks, so files are
/// large-ish but the encoder is tiny and dependency free.
pub fn write_png(path: &Path, width: usize, height: usize, rgb: &[u8]) -> std::io::Result<()> {
    assert_eq!(rgb.len(), width * height * 3);
    let mut raw = Vec::with_capacity((width * 3 + 1) * height);
    for row in rgb.chunks(width * 3) {
        raw.push(0); // filter: none
        raw.extend_from_slice(row);
    }

    let mut z = vec![0x78, 0x01];
    let blocks: Vec<&[u8]> = raw.chunks(65535).collect();
    for (i, block) in blocks.iter().enumerate() {
        z.push((i + 1 == blocks.len()) as u8);
        let len = block.len() as u16;
        z.extend_from_slice(&len.to_le_bytes());
        z.extend_from_slice(&(!len).to_le_bytes());
        z.extend_from_slice(block);
    }
    z.extend_from_slice(&adler32(&raw).to_be_bytes());

    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&(width as u32).to_be_bytes());
    ihdr.extend_from_slice(&(height as u32).to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]); // 8-bit, RGB, deflate, no filter, no interlace

    let mut out = Vec::with_capacity(z.len() + 64);
    out.extend_from_slice(&[137, 80, 78, 71, 13, 10, 26, 10]);
    write_chunk(&mut out, b"IHDR", &ihdr);
    write_chunk(&mut out, b"IDAT", &z);
    write_chunk(&mut out, b"IEND", &[]);

    if let Some(dir) = path.parent() {
        if !dir.as_os_str().is_empty() {
            std::fs::create_dir_all(dir)?;
        }
    }
    std::fs::File::create(path)?.write_all(&out)
}

fn write_chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let start = out.len();
    out.extend_from_slice(kind);
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
        crc = table[((crc ^ b as u32) & 0xFF) as usize] ^ (crc >> 8);
    }
    crc ^ 0xFFFF_FFFF
}

fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for chunk in data.chunks(5552) {
        for &x in chunk {
            a += x as u32;
            b += a;
        }
        a %= 65521;
        b %= 65521;
    }
    (b << 16) | a
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksums() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(adler32(b"Wikipedia"), 0x11E6_0398);
    }
}
