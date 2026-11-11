//! Image decoders without dependencies: PNG (all color types and bit depths, palette,
//! tRNS, Adam7), baseline JPEG (see `jpeg.rs`), TGA (raw / RLE), PPM/PGM, PFM and
//! Radiance HDR. The format is detected from the file contents (TGA, which has no
//! signature, from the extension).

use crate::deflate;
use crate::image::paeth;
use std::path::Path;

pub enum Pixels {
    U8(Vec<u8>),
    U16(Vec<u16>),
    /// Linear floating point (HDR formats).
    F32(Vec<f32>),
}

/// A decoded image. Rows run top to bottom, channels are interleaved.
pub struct RawImage {
    pub width: usize,
    pub height: usize,
    /// 1 = gray, 2 = gray + alpha, 3 = RGB, 4 = RGBA.
    pub channels: usize,
    pub pixels: Pixels,
}

pub fn load(path: &Path) -> Result<RawImage, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("cannot read '{}': {e}", path.display()))?;
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    decode(&bytes, &ext).map_err(|e| format!("{}: {e}", path.display()))
}

pub fn decode(data: &[u8], ext_hint: &str) -> Result<RawImage, String> {
    let img = if data.starts_with(&PNG_SIGNATURE) {
        decode_png(data)?
    } else if data.starts_with(&[0xFF, 0xD8]) {
        crate::jpeg::decode(data)?
    } else if data.starts_with(b"#?") {
        decode_hdr(data)?
    } else if data.starts_with(b"PF") || data.starts_with(b"Pf") {
        decode_pfm(data)?
    } else if data.len() > 2 && data[0] == b'P' && matches!(data[1], b'2' | b'3' | b'5' | b'6') {
        decode_pnm(data)?
    } else if ext_hint == "tga" {
        decode_tga(data)?
    } else {
        return Err("unrecognized image format (supported: png, jpg, tga, ppm, pgm, pfm, hdr)".into());
    };
    if img.width == 0 || img.height == 0 {
        return Err("image has zero size".into());
    }
    Ok(img)
}

// ---------------------------------------------------------------------------- PNG

const PNG_SIGNATURE: [u8; 8] = [137, 80, 78, 71, 13, 10, 26, 10];

/// Adam7 passes: (x0, y0, dx, dy).
const ADAM7: [(usize, usize, usize, usize); 7] =
    [(0, 0, 8, 8), (4, 0, 8, 8), (0, 4, 4, 8), (2, 0, 4, 4), (0, 2, 2, 4), (1, 0, 2, 2), (0, 1, 1, 2)];

fn be32(d: &[u8], pos: usize) -> Result<u32, String> {
    d.get(pos..pos + 4).map(|b| u32::from_be_bytes(b.try_into().unwrap())).ok_or_else(|| "truncated file".into())
}

pub fn decode_png(data: &[u8]) -> Result<RawImage, String> {
    let mut pos = 8;
    let mut ihdr = None;
    let mut palette: Vec<[u8; 3]> = Vec::new();
    let mut trns: Option<Vec<u8>> = None;
    let mut idat = Vec::new();
    loop {
        let len = be32(data, pos)? as usize;
        let kind = data.get(pos + 4..pos + 8).ok_or("truncated chunk")?;
        let body = data.get(pos + 8..pos + 8 + len).ok_or("truncated chunk")?;
        pos += 12 + len;
        match kind {
            b"IHDR" => {
                if body.len() != 13 {
                    return Err("bad IHDR".into());
                }
                let (w, h) = (be32(body, 0)? as usize, be32(body, 4)? as usize);
                ihdr = Some((w, h, body[8], body[9], body[12]));
            }
            b"PLTE" => palette = body.as_chunks::<3>().0.to_vec(),
            b"tRNS" => trns = Some(body.to_vec()),
            b"IDAT" => idat.extend_from_slice(body),
            b"IEND" => break,
            _ if kind[0].is_ascii_uppercase() => {
                return Err(format!("unsupported critical chunk '{}'", String::from_utf8_lossy(kind)));
            }
            _ => {} // ancillary (gAMA, sRGB, text, ...) ignored
        }
        if pos >= data.len() {
            break;
        }
    }
    let (w, h, depth, ctype, interlace) = ihdr.ok_or("missing IHDR")?;
    let spp = match ctype {
        0 | 3 => 1, // gray, palette index
        2 => 3,
        4 => 2,
        6 => 4,
        _ => return Err(format!("invalid PNG color type {ctype}")),
    };
    let depth_ok = match ctype {
        0 => matches!(depth, 1 | 2 | 4 | 8 | 16),
        3 => matches!(depth, 1 | 2 | 4 | 8),
        _ => matches!(depth, 8 | 16),
    };
    if !depth_ok {
        return Err(format!("invalid bit depth {depth} for color type {ctype}"));
    }
    if ctype == 3 && palette.is_empty() {
        return Err("palette image without PLTE".into());
    }
    if w.checked_mul(h).is_none_or(|n| n > (1 << 28)) {
        return Err("image too large".into());
    }
    let raw = deflate::zlib_decompress(&idat)?;
    let depth = depth as usize;
    let bits_pp = spp * depth;
    let bpp = bits_pp.div_ceil(8);

    // Decode all samples into u16 (full range of the bit depth).
    let mut samples = vec![0u16; w * h * spp];
    let passes: &[(usize, usize, usize, usize)] = if interlace == 1 { &ADAM7 } else { &[(0, 0, 1, 1)] };
    let mut off = 0;
    for &(x0, y0, dx, dy) in passes {
        let pw = if w > x0 { (w - x0).div_ceil(dx) } else { 0 };
        let ph = if h > y0 { (h - y0).div_ceil(dy) } else { 0 };
        if pw == 0 || ph == 0 {
            continue;
        }
        let stride = (pw * bits_pp).div_ceil(8);
        let mut prev = vec![0u8; stride];
        let mut cur = vec![0u8; stride];
        for py in 0..ph {
            let f = *raw.get(off).ok_or("image data too short")?;
            let src = raw.get(off + 1..off + 1 + stride).ok_or("image data too short")?;
            off += 1 + stride;
            for i in 0..stride {
                let a = if i >= bpp { cur[i - bpp] } else { 0 };
                let b = prev[i];
                let c = if i >= bpp { prev[i - bpp] } else { 0 };
                let p = match f {
                    0 => 0,
                    1 => a,
                    2 => b,
                    3 => u16::midpoint(u16::from(a), u16::from(b)) as u8,
                    4 => paeth(a, b, c),
                    _ => return Err(format!("invalid filter type {f}")),
                };
                cur[i] = src[i].wrapping_add(p);
            }
            let y = y0 + py * dy;
            for px in 0..pw {
                let x = x0 + px * dx;
                for c in 0..spp {
                    let idx = px * spp + c;
                    let v = match depth {
                        16 => u16::from_be_bytes([cur[idx * 2], cur[idx * 2 + 1]]),
                        8 => u16::from(cur[idx]),
                        _ => {
                            let bit = idx * depth;
                            u16::from((cur[bit / 8] >> (8 - depth - bit % 8)) & ((1 << depth) - 1) as u8)
                        }
                    };
                    samples[(y * w + x) * spp + c] = v;
                }
            }
            std::mem::swap(&mut prev, &mut cur);
        }
    }

    // Expand palette / color keys and normalize sub-byte depths to 8 bits.
    let n = w * h;
    if ctype == 3 {
        let alpha = trns.as_deref().unwrap_or(&[]);
        let ch = if alpha.is_empty() { 3 } else { 4 };
        let mut out = Vec::with_capacity(n * ch);
        for &i in &samples {
            let i = i as usize;
            let c = palette.get(i).copied().unwrap_or([0, 0, 0]);
            out.extend_from_slice(&c);
            if ch == 4 {
                out.push(alpha.get(i).copied().unwrap_or(255));
            }
        }
        return Ok(RawImage { width: w, height: h, channels: ch, pixels: Pixels::U8(out) });
    }
    let maxv = ((1u32 << depth) - 1) as u16;
    // Color key transparency for gray / RGB.
    let key: Option<Vec<u16>> = trns.filter(|_| ctype == 0 || ctype == 2).and_then(|t| {
        let k: Vec<u16> = t.as_chunks::<2>().0.iter().map(|&c| u16::from_be_bytes(c)).collect();
        (k.len() == spp).then_some(k)
    });
    let (channels, samples) = match key {
        None => (spp, samples),
        Some(k) => {
            let mut out = Vec::with_capacity(n * (spp + 1));
            for px in samples.chunks_exact(spp) {
                out.extend_from_slice(px);
                out.push(if px == &k[..] { 0 } else { maxv });
            }
            (spp + 1, out)
        }
    };
    let pixels = if depth == 16 {
        Pixels::U16(samples)
    } else if depth == 8 {
        Pixels::U8(samples.into_iter().map(|v| v as u8).collect())
    } else {
        Pixels::U8(samples.into_iter().map(|v| (u32::from(v) * 255 / u32::from(maxv)) as u8).collect())
    };
    Ok(RawImage { width: w, height: h, channels, pixels })
}

// ---------------------------------------------------------------------------- PPM / PGM / PFM

/// Reads whitespace-separated header tokens, skipping `#` comments.
struct Tokens<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Tokens<'a> {
    fn next(&mut self) -> Result<&'a str, String> {
        loop {
            while self.pos < self.data.len() && self.data[self.pos].is_ascii_whitespace() {
                self.pos += 1;
            }
            if self.data.get(self.pos) == Some(&b'#') {
                while self.pos < self.data.len() && self.data[self.pos] != b'\n' {
                    self.pos += 1;
                }
                continue;
            }
            break;
        }
        let start = self.pos;
        while self.pos < self.data.len() && !self.data[self.pos].is_ascii_whitespace() {
            self.pos += 1;
        }
        if start == self.pos {
            return Err("unexpected end of header".into());
        }
        std::str::from_utf8(&self.data[start..self.pos]).map_err(|_| "invalid header".into())
    }
    fn num<T: std::str::FromStr>(&mut self) -> Result<T, String> {
        let t = self.next()?;
        t.parse().map_err(|_| format!("invalid number '{t}'"))
    }
    /// Skips the single whitespace byte that ends a binary header.
    fn binary_start(&self) -> usize {
        self.pos + 1
    }
}

fn decode_pnm(data: &[u8]) -> Result<RawImage, String> {
    let mut tk = Tokens { data, pos: 0 };
    let magic = tk.next()?;
    let (w, h): (usize, usize) = (tk.num()?, tk.num()?);
    let maxv: u32 = tk.num()?;
    if maxv == 0 || maxv > 65535 {
        return Err(format!("invalid maxval {maxv}"));
    }
    let channels = if magic == "P2" || magic == "P5" { 1 } else { 3 };
    let n = w.checked_mul(h).and_then(|n| n.checked_mul(channels)).ok_or("image too large")?;
    let mut vals: Vec<u32> = Vec::with_capacity(n);
    if magic == "P2" || magic == "P3" {
        for _ in 0..n {
            vals.push(tk.num()?);
        }
    } else {
        let body = &data[tk.binary_start().min(data.len())..];
        if maxv < 256 {
            vals.extend(body.iter().take(n).map(|&b| u32::from(b)));
        } else {
            vals.extend(body.as_chunks::<2>().0.iter().take(n).map(|&c| u32::from(u16::from_be_bytes(c))));
        }
        if vals.len() < n {
            return Err("pixel data too short".into());
        }
    }
    let pixels = if maxv == 255 {
        Pixels::U8(vals.into_iter().map(|v| v.min(255) as u8).collect())
    } else if maxv < 256 {
        Pixels::U8(vals.into_iter().map(|v| (v.min(maxv) * 255 / maxv) as u8).collect())
    } else {
        Pixels::U16(vals.into_iter().map(|v| (v.min(maxv) * 65535 / maxv) as u16).collect())
    };
    Ok(RawImage { width: w, height: h, channels, pixels })
}

fn decode_pfm(data: &[u8]) -> Result<RawImage, String> {
    let mut tk = Tokens { data, pos: 0 };
    let channels = if tk.next()? == "PF" { 3 } else { 1 };
    let (w, h): (usize, usize) = (tk.num()?, tk.num()?);
    let scale: f32 = tk.num()?;
    let le = scale < 0.0;
    let n = w.checked_mul(h).and_then(|n| n.checked_mul(channels)).ok_or("image too large")?;
    let body = &data[tk.binary_start().min(data.len())..];
    if body.len() < n * 4 {
        return Err("pixel data too short".into());
    }
    let vals: Vec<f32> = body
        .as_chunks::<4>()
        .0
        .iter()
        .take(n)
        .map(|&b| if le { f32::from_le_bytes(b) } else { f32::from_be_bytes(b) })
        .collect();
    // Rows are stored bottom to top.
    let row = w * channels;
    let mut out = Vec::with_capacity(n);
    for r in vals.chunks_exact(row).rev() {
        out.extend_from_slice(r);
    }
    Ok(RawImage { width: w, height: h, channels, pixels: Pixels::F32(out) })
}

// ---------------------------------------------------------------------------- Radiance HDR

fn decode_hdr(data: &[u8]) -> Result<RawImage, String> {
    // Header: text lines up to an empty line, then the resolution line.
    let mut pos = 0;
    let line = |pos: &mut usize| -> Result<&str, String> {
        let start = *pos;
        let end = start + data[start..].iter().position(|&b| b == b'\n').ok_or("truncated header")?;
        *pos = end + 1;
        std::str::from_utf8(&data[start..end]).map(|s| s.trim_end_matches('\r')).map_err(|_| "bad header".into())
    };
    loop {
        let l = line(&mut pos)?;
        if l.is_empty() {
            break;
        }
        if let Some(f) = l.strip_prefix("FORMAT=")
            && f.trim() != "32-bit_rle_rgbe"
        {
            return Err(format!("unsupported HDR pixel format '{f}'"));
        }
    }
    let res: Vec<&str> = line(&mut pos)?.split_whitespace().collect();
    let (flip, h, w) = match res.as_slice() {
        ["-Y", h, "+X", w] => (false, h, w),
        ["+Y", h, "+X", w] => (true, h, w),
        _ => return Err(format!("unsupported HDR orientation '{}'", res.join(" "))),
    };
    let (w, h): (usize, usize) = (w.parse().map_err(|_| "bad width")?, h.parse().map_err(|_| "bad height")?);
    if w.checked_mul(h).is_none_or(|n| n > (1 << 28)) {
        return Err("image too large".into());
    }
    let mut out = vec![0f32; w * h * 3];
    let mut scan = vec![[0u8; 4]; w];
    let byte = |p: usize| data.get(p).copied().ok_or_else(|| "pixel data too short".to_string());
    for y in 0..h {
        let new_rle = (8..0x8000).contains(&w)
            && data.get(pos..pos + 2) == Some(&[2, 2])
            && data.get(pos + 2).is_some_and(|&b| b & 0x80 == 0);
        if new_rle {
            if ((byte(pos + 2)? as usize) << 8 | byte(pos + 3)? as usize) != w {
                return Err("scanline width mismatch".into());
            }
            pos += 4;
            for ch in 0..4 {
                let mut x = 0;
                while x < w {
                    let c = byte(pos)? as usize;
                    pos += 1;
                    if c > 128 {
                        let n = c - 128;
                        let v = byte(pos)?;
                        pos += 1;
                        for p in scan.get_mut(x..x + n).ok_or("bad run length")? {
                            p[ch] = v;
                        }
                        x += n;
                    } else {
                        if c == 0 || x + c > w {
                            return Err("bad run length".into());
                        }
                        for p in &mut scan[x..x + c] {
                            p[ch] = byte(pos)?;
                            pos += 1;
                        }
                        x += c;
                    }
                }
            }
        } else {
            // Flat pixels, possibly with old-style (1,1,1,n) repeat runs.
            let mut x = 0;
            let mut shift = 0;
            while x < w {
                let p = [byte(pos)?, byte(pos + 1)?, byte(pos + 2)?, byte(pos + 3)?];
                pos += 4;
                if p[0] == 1 && p[1] == 1 && p[2] == 1 && x > 0 {
                    let n = (p[3] as usize) << shift;
                    let prev = scan[x - 1];
                    for q in scan.get_mut(x..x + n).ok_or("bad run length")? {
                        *q = prev;
                    }
                    x += n;
                    shift += 8;
                } else {
                    scan[x] = p;
                    x += 1;
                    shift = 0;
                }
            }
        }
        let row = if flip { h - 1 - y } else { y };
        for (x, p) in scan.iter().enumerate() {
            let o = (row * w + x) * 3;
            if p[3] != 0 {
                let f = ((i32::from(p[3]) - 136) as f32).exp2();
                out[o] = (f32::from(p[0]) + 0.5) * f;
                out[o + 1] = (f32::from(p[1]) + 0.5) * f;
                out[o + 2] = (f32::from(p[2]) + 0.5) * f;
            }
        }
    }
    Ok(RawImage { width: w, height: h, channels: 3, pixels: Pixels::F32(out) })
}

// ---------------------------------------------------------------------------- TGA

fn decode_tga(data: &[u8]) -> Result<RawImage, String> {
    let hd = data.get(..18).ok_or("truncated TGA header")?;
    let id_len = hd[0] as usize;
    let cmap_type = hd[1];
    let ty = hd[2];
    let cmap_first = u16::from_le_bytes([hd[3], hd[4]]) as usize;
    let cmap_len = u16::from_le_bytes([hd[5], hd[6]]) as usize;
    let cmap_bits = hd[7] as usize;
    let w = u16::from_le_bytes([hd[12], hd[13]]) as usize;
    let h = u16::from_le_bytes([hd[14], hd[15]]) as usize;
    let bits = hd[16] as usize;
    let desc = hd[17];
    let rle = ty & 8 != 0;
    let base = ty & !8;
    if !matches!(base, 1..=3) {
        return Err(format!("unsupported TGA image type {ty}"));
    }
    let mut pos = 18 + id_len;
    // Color map.
    let mut cmap: Vec<[u8; 4]> = Vec::new();
    if cmap_type == 1 {
        let eb = cmap_bits.div_ceil(8);
        for i in 0..cmap_len {
            let b = data.get(pos + i * eb..pos + (i + 1) * eb).ok_or("truncated color map")?;
            cmap.push(tga_color(b, cmap_bits)?);
        }
        pos += cmap_len * eb;
    }
    let pb = bits.div_ceil(8);
    if pb == 0 || pb > 4 {
        return Err(format!("unsupported TGA pixel depth {bits}"));
    }
    let alpha_bits = desc & 15;
    let decode_px = |b: &[u8]| -> Result<[u8; 4], String> {
        match base {
            1 => {
                let i = if pb == 1 { b[0] as usize } else { u16::from_le_bytes([b[0], b[1]]) as usize };
                cmap.get(i.wrapping_sub(cmap_first)).copied().ok_or_else(|| "color map index out of range".into())
            }
            3 => Ok([b[0], b[0], b[0], if pb == 2 { b[1] } else { 255 }]),
            _ => tga_color(b, bits),
        }
    };
    let n = w * h;
    let mut px: Vec<[u8; 4]> = Vec::with_capacity(n);
    while px.len() < n {
        if rle {
            let c = *data.get(pos).ok_or("truncated pixel data")? as usize;
            pos += 1;
            let count = (c & 127) + 1;
            if c & 128 != 0 {
                let p = decode_px(data.get(pos..pos + pb).ok_or("truncated pixel data")?)?;
                pos += pb;
                px.extend(std::iter::repeat_n(p, count.min(n - px.len())));
            } else {
                for _ in 0..count.min(n - px.len()) {
                    px.push(decode_px(data.get(pos..pos + pb).ok_or("truncated pixel data")?)?);
                    pos += pb;
                }
            }
        } else {
            px.push(decode_px(data.get(pos..pos + pb).ok_or("truncated pixel data")?)?);
            pos += pb;
        }
    }
    let top_down = desc & 0x20 != 0;
    let right_left = desc & 0x10 != 0;
    let gray = base == 3;
    let has_alpha = match base {
        3 => pb == 2,
        1 => cmap_bits == 32 || (cmap_bits == 16 && alpha_bits > 0),
        _ => bits == 32 || (bits == 16 && alpha_bits > 0),
    };
    let channels = match (gray, has_alpha) {
        (true, false) => 1,
        (true, true) => 2,
        (false, false) => 3,
        (false, true) => 4,
    };
    let mut out = Vec::with_capacity(n * channels);
    for y in 0..h {
        let sy = if top_down { y } else { h - 1 - y };
        for x in 0..w {
            let sx = if right_left { w - 1 - x } else { x };
            let p = px[sy * w + sx];
            match channels {
                1 => out.push(p[0]),
                2 => out.extend_from_slice(&[p[0], p[3]]),
                3 => out.extend_from_slice(&p[..3]),
                _ => out.extend_from_slice(&p),
            }
        }
    }
    Ok(RawImage { width: w, height: h, channels, pixels: Pixels::U8(out) })
}

/// BGR(A) or A1R5G5B5 to RGBA.
fn tga_color(b: &[u8], bits: usize) -> Result<[u8; 4], String> {
    Ok(match bits {
        15 | 16 => {
            let v = u16::from_le_bytes([b[0], b[1]]);
            let c = |s: u16| (u32::from((v >> s) & 31) * 255 / 31) as u8;
            [c(10), c(5), c(0), if bits == 16 && v & 0x8000 == 0 { 0 } else { 255 }]
        }
        24 => [b[2], b[1], b[0], 255],
        32 => [b[2], b[1], b[0], b[3]],
        8 => [b[0], b[0], b[0], 255],
        _ => return Err(format!("unsupported TGA color depth {bits}")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::math::Vec3;

    fn u8s(img: &RawImage) -> &[u8] {
        match &img.pixels {
            Pixels::U8(v) => v,
            _ => panic!("expected 8-bit pixels"),
        }
    }

    fn chunk(out: &mut Vec<u8>, kind: [u8; 4], body: &[u8]) {
        out.extend_from_slice(&(body.len() as u32).to_be_bytes());
        out.extend_from_slice(&kind);
        out.extend_from_slice(body);
        out.extend_from_slice(&[0; 4]); // CRC is not checked by the decoder
    }

    /// Builds a PNG from already-filtered scanline data.
    fn png(w: u32, h: u32, depth: u8, ctype: u8, interlace: u8, raw: &[u8], extra: &[(&[u8; 4], Vec<u8>)]) -> Vec<u8> {
        let mut out = PNG_SIGNATURE.to_vec();
        let mut ihdr = w.to_be_bytes().to_vec();
        ihdr.extend_from_slice(&h.to_be_bytes());
        ihdr.extend_from_slice(&[depth, ctype, 0, 0, interlace]);
        chunk(&mut out, *b"IHDR", &ihdr);
        for (k, b) in extra {
            chunk(&mut out, **k, b);
        }
        chunk(&mut out, *b"IDAT", &deflate::zlib_compress(raw));
        chunk(&mut out, *b"IEND", &[]);
        out
    }

    #[test]
    fn png_rgb_roundtrip_with_own_encoder() {
        let (w, h) = (19, 7);
        let rgb: Vec<u8> = (0..w * h * 3).map(|i| (i * 13 % 251) as u8).collect();
        let img = decode(&crate::image::encode_png(w, h, &rgb), "png").unwrap();
        assert_eq!((img.width, img.height, img.channels), (w, h, 3));
        assert_eq!(u8s(&img), &rgb[..]);
    }

    #[test]
    fn png_palette_trns_and_low_bit_depths() {
        // 2-bit palette, 3x2 pixels: indices 0 1 2 / 3 0 1; each row 1 byte, filter 0.
        let raw = [0, 0b00_01_10_00, 0, 0b11_00_01_00];
        let pal = vec![255, 0, 0, 0, 255, 0, 0, 0, 255, 9, 9, 9];
        let p = png(3, 2, 2, 3, 0, &raw, &[(b"PLTE", pal), (b"tRNS", vec![128])]);
        let img = decode(&p, "").unwrap();
        assert_eq!(img.channels, 4);
        let px = u8s(&img);
        assert_eq!(&px[0..4], &[255, 0, 0, 128]);
        assert_eq!(&px[8..12], &[0, 0, 255, 255]);
        assert_eq!(&px[12..16], &[9, 9, 9, 255]);

        // 1-bit gray scales to 0 / 255.
        let img = decode(&png(4, 1, 1, 0, 0, &[0, 0b1010_0000], &[]), "").unwrap();
        assert_eq!(u8s(&img), &[255, 0, 255, 0]);
    }

    #[test]
    fn png_16bit_and_adam7() {
        // 16-bit gray+alpha, 2x1, with an Up filter on a single row (prior row is zero).
        let raw = [2, 0x12, 0x34, 0xFF, 0xFF, 0xAB, 0xCD, 0x00, 0x00];
        let img = decode(&png(2, 1, 16, 4, 0, &raw, &[]), "").unwrap();
        let Pixels::U16(v) = &img.pixels else { panic!() };
        assert_eq!(v, &[0x1234, 0xFFFF, 0xABCD, 0]);

        // Interlaced 8-bit gray 5x5 where pixel value = y * 5 + x.
        let (w, h) = (5usize, 5usize);
        let mut raw = Vec::new();
        for &(x0, y0, dx, dy) in &ADAM7 {
            for y in (y0..h).step_by(dy) {
                let xs: Vec<u8> = (x0..w).step_by(dx).map(|x| (y * w + x) as u8).collect();
                if !xs.is_empty() {
                    raw.push(0);
                    raw.extend(xs);
                }
            }
        }
        let img = decode(&png(5, 5, 8, 0, 1, &raw, &[]), "").unwrap();
        assert_eq!(u8s(&img), &(0..25).collect::<Vec<u8>>()[..]);
    }

    #[test]
    fn pnm_and_pfm() {
        let img = decode(b"P3\n# comment\n2 1\n15\n15 0 0  0 15 0\n", "").unwrap();
        assert_eq!(u8s(&img), &[255, 0, 0, 0, 255, 0]);
        let mut p5 = b"P5 2 2 255\n".to_vec();
        p5.extend_from_slice(&[1, 2, 3, 4]);
        let img = decode(&p5, "").unwrap();
        assert_eq!((img.channels, u8s(&img)), (1, &[1u8, 2, 3, 4][..]));

        let dir = std::env::temp_dir().join(format!("pt_imageio_{}", std::process::id()));
        let path = dir.join("t.pfm");
        let px = [Vec3::new(1.0, 2.0, 3.0), Vec3::new(4.0, 5.0, 6.0)];
        crate::image::write_pfm(&path, 1, 2, &px).unwrap();
        let img = load(&path).unwrap();
        let Pixels::F32(v) = &img.pixels else { panic!() };
        assert_eq!(v, &[1.0, 2.0, 3.0, 4.0, 5.0, 6.0]); // top row first
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn hdr_roundtrip() {
        for w in [3usize, 20] {
            let px: Vec<Vec3> = (0..w * 2).map(|i| Vec3::new(i as f32 * 0.37 + 0.01, 1.0, 50.0)).collect();
            let img = decode(&crate::image::encode_hdr(w, 2, &px), "hdr").unwrap();
            let Pixels::F32(v) = &img.pixels else { panic!() };
            for (i, c) in px.iter().enumerate() {
                for k in 0..3 {
                    assert!((v[i * 3 + k] - c[k]).abs() <= c.max_elem() * 0.01);
                }
            }
        }
    }

    #[test]
    fn tga_rle_bottom_up() {
        // 2x2 RLE truecolor 24-bit, bottom-left origin: one run of 3 blue, one raw red.
        let mut d = vec![0u8, 0, 10, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2, 0, 2, 0, 24, 0];
        d.extend_from_slice(&[0x82, 255, 0, 0]); // run of 3 BGR(255,0,0) = blue
        d.extend_from_slice(&[0x00, 0, 0, 255]); // 1 raw: red
        let img = decode(&d, "tga").unwrap();
        // Bottom row (stored first) = blue, blue; top row = blue, red.
        assert_eq!(u8s(&img), &[0, 0, 255, 255, 0, 0, 0, 0, 255, 0, 0, 255]);
        assert_eq!(img.channels, 3);
    }
}
