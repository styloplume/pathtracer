//! Baseline JPEG decoder (sequential DCT, Huffman, 8-bit): gray or YCbCr with any
//! chroma subsampling, restart markers, interleaved and non-interleaved scans.
//! Progressive and arithmetic-coded files are rejected with a clear error.

use crate::imageio::{Pixels, RawImage};
use std::f32::consts::PI;

/// Zigzag index -> natural (row-major) index.
const ZIGZAG: [usize; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20, 13, 6, 7, 14, 21,
    28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58, 59, 52, 45, 38, 31, 39, 46, 53, 60, 61, 54,
    47, 55, 62, 63,
];

#[derive(Clone, Default)]
struct Huffman {
    /// Largest code of each length (-1 if none), first code, and offset into `vals`.
    maxcode: [i32; 17],
    mincode: [i32; 17],
    valptr: [i32; 17],
    vals: Vec<u8>,
}

impl Huffman {
    fn new(counts: &[u8; 16], vals: Vec<u8>) -> Huffman {
        let mut h = Huffman { maxcode: [-1; 17], vals, ..Default::default() };
        let (mut code, mut k) = (0i32, 0i32);
        for len in 1..=16 {
            let n = i32::from(counts[len - 1]);
            if n > 0 {
                h.valptr[len] = k;
                h.mincode[len] = code;
                code += n;
                k += n;
                h.maxcode[len] = code - 1;
            }
            code <<= 1;
        }
        h
    }
}

struct Component {
    id: u8,
    h: usize,
    v: usize,
    tq: usize,
    td: usize,
    ta: usize,
    /// Sample plane covering whole MCUs.
    stride: usize,
    plane: Vec<u8>,
    dc_pred: i32,
}

struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
    acc: u32,
    n: u32,
}

impl Bits<'_> {
    fn fill(&mut self) {
        while self.n <= 24 {
            let mut b = 0u8;
            if self.pos < self.data.len() {
                b = self.data[self.pos];
                if b == 0xFF {
                    let next = self.data.get(self.pos + 1).copied().unwrap_or(0);
                    if next == 0 {
                        self.pos += 2;
                    } else {
                        b = 0; // a marker: pad with zeros, don't consume it
                    }
                } else {
                    self.pos += 1;
                }
            }
            self.acc |= u32::from(b) << (24 - self.n);
            self.n += 8;
        }
    }
    #[inline]
    fn bits(&mut self, k: u32) -> u32 {
        if k == 0 {
            return 0;
        }
        if self.n < k {
            self.fill();
        }
        let v = self.acc >> (32 - k);
        self.acc <<= k;
        self.n -= k;
        v
    }
    fn decode(&mut self, h: &Huffman) -> Result<u8, String> {
        let mut code = 0i32;
        for len in 1..=16 {
            code = (code << 1) | self.bits(1) as i32;
            if code <= h.maxcode[len] {
                return h
                    .vals
                    .get((h.valptr[len] + code - h.mincode[len]) as usize)
                    .copied()
                    .ok_or_else(|| "bad Huffman code".into());
            }
        }
        Err("bad Huffman code".into())
    }
    #[inline]
    fn receive_extend(&mut self, s: u32) -> i32 {
        if s == 0 {
            return 0;
        }
        let v = self.bits(s) as i32;
        if v < 1 << (s - 1) { v - (1 << s) + 1 } else { v }
    }
    /// Drops buffered bits and skips an `RSTn` marker if one follows.
    fn restart(&mut self) {
        self.acc = 0;
        self.n = 0;
        while self.pos + 1 < self.data.len() && !(self.data[self.pos] == 0xFF && self.data[self.pos + 1] != 0) {
            self.pos += 1;
        }
        if self.pos + 1 < self.data.len() && (0xD0..=0xD7).contains(&self.data[self.pos + 1]) {
            self.pos += 2;
        }
    }
}

/// `IDCT[x][u] = C(u) / 2 * cos((2x + 1) u pi / 16)`.
fn idct_table() -> [[f32; 8]; 8] {
    let mut t = [[0f32; 8]; 8];
    for (x, row) in t.iter_mut().enumerate() {
        for (u, v) in row.iter_mut().enumerate() {
            let c = if u == 0 { std::f32::consts::FRAC_1_SQRT_2 } else { 1.0 };
            *v = 0.5 * c * (((2 * x + 1) as f32 * u as f32 * PI) / 16.0).cos();
        }
    }
    t
}

fn idct_block(coef: &[f32; 64], t: &[[f32; 8]; 8], out: &mut [u8], stride: usize) {
    let mut tmp = [0f32; 64];
    // Rows: tmp[v][x] = sum_u coef[v][u] * t[x][u]
    for v in 0..8 {
        let row = &coef[v * 8..v * 8 + 8];
        if row[1..].iter().all(|&c| c == 0.0) {
            let dc = row[0] * t[0][0];
            tmp[v * 8..v * 8 + 8].fill(dc);
            continue;
        }
        for x in 0..8 {
            tmp[v * 8 + x] = (0..8).map(|u| row[u] * t[x][u]).sum();
        }
    }
    // Columns.
    for x in 0..8 {
        for y in 0..8 {
            let s: f32 = (0..8).map(|v| tmp[v * 8 + x] * t[y][v]).sum();
            out[y * stride + x] = (s + 128.0).round().clamp(0.0, 255.0) as u8;
        }
    }
}

#[expect(clippy::too_many_lines, reason = "one flat match over JPEG markers reads best in one place")]
pub fn decode(data: &[u8]) -> Result<RawImage, String> {
    let mut qt = [[0u16; 64]; 4];
    let mut dc_tables: [Huffman; 4] = Default::default();
    let mut ac_tables: [Huffman; 4] = Default::default();
    let mut comps: Vec<Component> = Vec::new();
    let (mut width, mut height) = (0usize, 0usize);
    let (mut hmax, mut vmax) = (1usize, 1usize);
    let mut restart_interval = 0usize;
    let mut adobe_transform: Option<u8> = None;
    let mut frame_seen = false;
    let idct = idct_table();

    let mut pos = 2;
    loop {
        // Find the next marker.
        while pos < data.len() && data[pos] != 0xFF {
            pos += 1;
        }
        while pos < data.len() && data[pos] == 0xFF {
            pos += 1;
        }
        let Some(&marker) = data.get(pos) else { return Err("unexpected end of file (no EOI)".into()) };
        pos += 1;
        if marker == 0xD9 {
            break; // EOI
        }
        if (0xD0..=0xD7).contains(&marker) || marker == 0x01 {
            continue; // stray RST / TEM: no payload
        }
        let len = data.get(pos..pos + 2).map(|b| u16::from_be_bytes([b[0], b[1]]) as usize).ok_or("truncated")?;
        let seg = data.get(pos + 2..pos + len).ok_or("truncated segment")?;
        let seg_end = pos + len;
        match marker {
            0xC0 | 0xC1 => {
                if seg.len() < 6 || seg[0] != 8 {
                    return Err("only 8-bit JPEG is supported".into());
                }
                height = u16::from_be_bytes([seg[1], seg[2]]) as usize;
                width = u16::from_be_bytes([seg[3], seg[4]]) as usize;
                let nc = seg[5] as usize;
                if height == 0 {
                    return Err("JPEG with DNL height is not supported".into());
                }
                if nc != 1 && nc != 3 {
                    return Err(format!("unsupported JPEG component count {nc} (CMYK?)"));
                }
                for i in 0..nc {
                    let c = seg.get(6 + i * 3..9 + i * 3).ok_or("truncated SOF")?;
                    let (h, v) = ((c[1] >> 4) as usize, (c[1] & 15) as usize);
                    if !(1..=4).contains(&h) || !(1..=4).contains(&v) || c[2] > 3 {
                        return Err("bad JPEG component parameters".into());
                    }
                    comps.push(Component {
                        id: c[0],
                        h,
                        v,
                        tq: c[2] as usize,
                        td: 0,
                        ta: 0,
                        stride: 0,
                        plane: Vec::new(),
                        dc_pred: 0,
                    });
                }
                hmax = comps.iter().map(|c| c.h).max().unwrap_or(1);
                vmax = comps.iter().map(|c| c.v).max().unwrap_or(1);
                let (mcux, mcuy) = (width.div_ceil(8 * hmax), height.div_ceil(8 * vmax));
                for c in &mut comps {
                    c.stride = mcux * c.h * 8;
                    c.plane = vec![0; c.stride * mcuy * c.v * 8];
                }
                frame_seen = true;
            }
            0xC2 | 0xC6 | 0xCA | 0xCE => return Err("progressive JPEG is not supported (baseline only)".into()),
            0xC3 | 0xC5 | 0xC7 | 0xC9..=0xCB | 0xCD | 0xCF => {
                return Err("lossless / hierarchical / arithmetic JPEG is not supported".into());
            }
            0xC4 => {
                let mut p = 0;
                while p < seg.len() {
                    let tc = seg[p] >> 4;
                    let th = (seg[p] & 15) as usize;
                    let counts: [u8; 16] = seg.get(p + 1..p + 17).ok_or("truncated DHT")?.try_into().unwrap();
                    let total: usize = counts.iter().map(|&c| c as usize).sum();
                    let vals = seg.get(p + 17..p + 17 + total).ok_or("truncated DHT")?.to_vec();
                    if th > 3 {
                        return Err("bad Huffman table id".into());
                    }
                    let t = Huffman::new(&counts, vals);
                    if tc == 0 {
                        dc_tables[th] = t;
                    } else {
                        ac_tables[th] = t;
                    }
                    p += 17 + total;
                }
            }
            0xDB => {
                let mut p = 0;
                while p < seg.len() {
                    let (pq, tq) = (seg[p] >> 4, (seg[p] & 15) as usize);
                    if tq > 3 {
                        return Err("bad quantization table id".into());
                    }
                    for (k, q) in qt[tq].iter_mut().enumerate() {
                        *q = if pq == 0 {
                            u16::from(*seg.get(p + 1 + k).ok_or("truncated DQT")?)
                        } else {
                            let b = seg.get(p + 1 + 2 * k..p + 3 + 2 * k).ok_or("truncated DQT")?;
                            u16::from_be_bytes([b[0], b[1]])
                        };
                    }
                    p += 1 + if pq == 0 { 64 } else { 128 };
                }
            }
            0xDD => restart_interval = u16::from_be_bytes([seg[0], *seg.get(1).ok_or("truncated DRI")?]) as usize,
            0xEE => {
                if seg.len() >= 12 && seg.starts_with(b"Adobe") {
                    adobe_transform = Some(seg[11]);
                }
            }
            0xDA => {
                if !frame_seen {
                    return Err("scan before frame header".into());
                }
                let ns = *seg.first().ok_or("truncated SOS")? as usize;
                let mut scan = Vec::with_capacity(ns);
                for i in 0..ns {
                    let s = seg.get(1 + i * 2..3 + i * 2).ok_or("truncated SOS")?;
                    let ci = comps.iter().position(|c| c.id == s[0]).ok_or("scan references unknown component")?;
                    comps[ci].td = (s[1] >> 4) as usize & 3;
                    comps[ci].ta = (s[1] & 15) as usize & 3;
                    scan.push(ci);
                }
                pos = decode_scan(
                    data,
                    seg_end,
                    &mut comps,
                    &scan,
                    (&dc_tables, &ac_tables, &qt, &idct),
                    (width, height, hmax, vmax),
                    restart_interval,
                )?;
                continue;
            }
            _ => {} // APPn, COM, ...
        }
        pos = seg_end;
    }
    if !frame_seen {
        return Err("no frame header".into());
    }

    // Upsample subsampled planes bilinearly between sample centers (like libjpeg's
    // "fancy" upsampling) and convert to RGB.
    let channels = if comps.len() == 3 { 3 } else { 1 };
    let ycc = channels == 3 && adobe_transform != Some(0);
    let mut out = vec![0u8; width * height * channels];
    let sample = |c: &Component, x: usize, y: usize| -> f32 {
        if c.h == hmax && c.v == vmax {
            return f32::from(c.plane[y * c.stride + x]);
        }
        let (cw, ch) = ((width * c.h).div_ceil(hmax), (height * c.v).div_ceil(vmax));
        let fx = ((x as f32 + 0.5) * c.h as f32 / hmax as f32 - 0.5).clamp(0.0, (cw - 1) as f32);
        let fy = ((y as f32 + 0.5) * c.v as f32 / vmax as f32 - 0.5).clamp(0.0, (ch - 1) as f32);
        let (x0, y0) = (fx as usize, fy as usize);
        let (x1, y1) = ((x0 + 1).min(cw - 1), (y0 + 1).min(ch - 1));
        let (tx, ty) = (fx - x0 as f32, fy - y0 as f32);
        let p = |x: usize, y: usize| f32::from(c.plane[y * c.stride + x]);
        (p(x0, y0) * (1.0 - tx) + p(x1, y0) * tx) * (1.0 - ty) + (p(x0, y1) * (1.0 - tx) + p(x1, y1) * tx) * ty
    };
    for y in 0..height {
        for x in 0..width {
            let s = |c: &Component| sample(c, x, y);
            let o = (y * width + x) * channels;
            if channels == 1 {
                out[o] = s(&comps[0]) as u8;
            } else if ycc {
                let (yy, cb, cr) = (s(&comps[0]), s(&comps[1]) - 128.0, s(&comps[2]) - 128.0);
                let q = |v: f32| v.round().clamp(0.0, 255.0) as u8;
                out[o] = q(yy + 1.402 * cr);
                out[o + 1] = q(yy - 0.344_136 * cb - 0.714_136 * cr);
                out[o + 2] = q(yy + 1.772 * cb);
            } else {
                for (k, c) in comps.iter().enumerate() {
                    out[o + k] = s(c) as u8;
                }
            }
        }
    }
    Ok(RawImage { width, height, channels, pixels: Pixels::U8(out) })
}

type Tables<'a> = (&'a [Huffman; 4], &'a [Huffman; 4], &'a [[u16; 64]; 4], &'a [[f32; 8]; 8]);

/// Decodes one scan's entropy-coded data; returns the position of the marker after it.
fn decode_scan(
    data: &[u8],
    start: usize,
    comps: &mut [Component],
    scan: &[usize],
    (dc, ac, qt, idct): Tables<'_>,
    (width, height, hmax, vmax): (usize, usize, usize, usize),
    restart_interval: usize,
) -> Result<usize, String> {
    let mut bits = Bits { data, pos: start, acc: 0, n: 0 };
    for &ci in scan {
        comps[ci].dc_pred = 0;
    }
    // A non-interleaved scan has one block per MCU over the component's own extent.
    let single = scan.len() == 1;
    let (mcux, mcuy) = if single {
        let c = &comps[scan[0]];
        ((width * c.h).div_ceil(hmax).div_ceil(8), (height * c.v).div_ceil(vmax).div_ceil(8))
    } else {
        (width.div_ceil(8 * hmax), height.div_ceil(8 * vmax))
    };
    let mut coef = [0f32; 64];
    let mut mcu_count = 0usize;
    for my in 0..mcuy {
        for mx in 0..mcux {
            if restart_interval > 0 && mcu_count > 0 && mcu_count.is_multiple_of(restart_interval) {
                bits.restart();
                for &ci in scan {
                    comps[ci].dc_pred = 0;
                }
            }
            mcu_count += 1;
            for &ci in scan {
                let c = &mut comps[ci];
                let (bh, bv) = if single { (1, 1) } else { (c.h, c.v) };
                for by in 0..bv {
                    for bx in 0..bh {
                        coef.fill(0.0);
                        let q = &qt[c.tq];
                        let t = u32::from(bits.decode(&dc[c.td])?);
                        if t > 11 {
                            return Err("bad DC coefficient".into());
                        }
                        c.dc_pred += bits.receive_extend(t);
                        coef[0] = (c.dc_pred * i32::from(q[0])) as f32;
                        let mut k = 1;
                        while k < 64 {
                            let rs = bits.decode(&ac[c.ta])?;
                            let (r, s) = ((rs >> 4) as usize, u32::from(rs & 15));
                            if s == 0 {
                                if r == 15 {
                                    k += 16;
                                    continue;
                                }
                                break; // EOB
                            }
                            k += r;
                            if k > 63 {
                                return Err("bad AC run length".into());
                            }
                            coef[ZIGZAG[k]] = (bits.receive_extend(s) * i32::from(q[k])) as f32;
                            k += 1;
                        }
                        let (x, y) = if single { (mx * 8, my * 8) } else { ((mx * c.h + bx) * 8, (my * c.v + by) * 8) };
                        if y * c.stride + x < c.plane.len() && x + 8 <= c.stride {
                            let stride = c.stride;
                            idct_block(&coef, idct, &mut c.plane[y * stride + x..], stride);
                        }
                    }
                }
            }
        }
    }
    // Position of the next marker (the bit reader never consumes markers).
    let mut p = bits.pos;
    while p + 1 < data.len() && !(data[p] == 0xFF && data[p + 1] != 0 && !(0xD0..=0xD7).contains(&data[p + 1])) {
        p += 1;
    }
    Ok(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Compares against the Windows (GDI+) decode of a 4:2:0 baseline file. Decoders
    /// differ in IDCT precision and chroma upsampling, so only small errors are allowed.
    #[test]
    fn matches_reference_decoder() {
        let img = decode(include_bytes!("../testdata/gradient.jpg")).unwrap();
        let reference = include_bytes!("../testdata/gradient.rgb");
        assert_eq!((img.width, img.height, img.channels), (45, 33, 3));
        let Pixels::U8(px) = &img.pixels else { panic!() };
        let (mut sum, mut max) = (0u64, 0u8);
        for (a, b) in px.iter().zip(reference.iter()) {
            let d = a.abs_diff(*b);
            sum += u64::from(d);
            max = max.max(d);
        }
        let mean = sum as f64 / px.len() as f64;
        assert!(mean < 1.0 && max < 12, "mean {mean}, max {max}");
    }

    #[test]
    fn rejects_progressive() {
        let data = [0xFF, 0xD8, 0xFF, 0xC2, 0x00, 0x0B, 8, 0, 1, 0, 1, 1, 1, 0x11, 0, 0xFF, 0xD9];
        assert!(decode(&data).err().unwrap().contains("progressive"));
    }
}
