//! A TrueType reader and rasterizer: the `cmap` (formats 4 and 12), `hmtx` and `glyf`
//! tables (simple and composite glyphs), font collections (`.ttc`), and quadratic outlines
//! flattened to lines whose exact signed area coverage is accumulated per pixel, which
//! antialiases without supersampling. No hinting; fonts with CFF outlines (most `.otf`)
//! are not read.

use std::ops::Range;

pub struct TrueType {
    data: Vec<u8>,
    pub units_per_em: f32,
    pub ascent: f32,
    pub descent: f32,
    pub line_gap: f32,
    glyf: Range<usize>,
    loca: Range<usize>,
    hmtx: Range<usize>,
    long_loca: bool,
    num_glyphs: u16,
    num_hmetrics: u16,
    /// The chosen character map subtable and its format.
    cmap: (usize, u16),
}

fn u16_at(d: &[u8], i: usize) -> Option<u16> {
    Some(u16::from_be_bytes([*d.get(i)?, *d.get(i + 1)?]))
}

fn i16_at(d: &[u8], i: usize) -> Option<i16> {
    u16_at(d, i).map(|v| v as i16)
}

fn u32_at(d: &[u8], i: usize) -> Option<u32> {
    Some(u32::from_be_bytes([*d.get(i)?, *d.get(i + 1)?, *d.get(i + 2)?, *d.get(i + 3)?]))
}

/// A point of an outline, in font units (y up).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

/// A glyph's outline: closed contours of line and quadratic segments.
#[derive(Clone, Debug, Default)]
pub struct Outline {
    pub segments: Vec<Segment>,
}

#[derive(Clone, Copy, Debug)]
pub enum Segment {
    Line(Point, Point),
    Quad(Point, Point, Point),
}

impl TrueType {
    /// Reads font `index` of a `.ttf` or `.ttc` file's bytes.
    pub fn parse(data: Vec<u8>, index: usize) -> Result<TrueType, String> {
        let bad = || "not a TrueType font".to_owned();
        let d = &data[..];
        let base = if d.starts_with(b"ttcf") {
            let n = u32_at(d, 8).ok_or_else(bad)? as usize;
            if index >= n {
                return Err(format!("the collection has {n} fonts"));
            }
            u32_at(d, 12 + 4 * index).ok_or_else(bad)? as usize
        } else {
            0
        };
        let version = u32_at(d, base).ok_or_else(bad)?;
        if version != 0x0001_0000 && version != u32::from_be_bytes(*b"true") {
            return Err(if version == u32::from_be_bytes(*b"OTTO") {
                "CFF (PostScript) outlines are not supported".into()
            } else {
                bad()
            });
        }
        let tables = u16_at(d, base + 4).ok_or_else(bad)? as usize;
        let table = |tag: &[u8; 4]| -> Option<Range<usize>> {
            (0..tables).find_map(|i| {
                let rec = base + 12 + 16 * i;
                if d.get(rec..rec + 4)? != tag {
                    return None;
                }
                let (off, len) = (u32_at(d, rec + 8)? as usize, u32_at(d, rec + 12)? as usize);
                (off + len <= d.len()).then_some(off..off + len)
            })
        };
        let need =
            |tag: &[u8; 4]| table(tag).ok_or_else(|| format!("missing '{}' table", String::from_utf8_lossy(tag)));
        let (head, hhea, maxp, cmap) = (need(b"head")?, need(b"hhea")?, need(b"maxp")?, need(b"cmap")?);
        let (hmtx, loca, glyf) = (need(b"hmtx")?, need(b"loca")?, need(b"glyf")?);
        let units_per_em = f32::from(u16_at(d, head.start + 18).ok_or_else(bad)?.max(1));
        let long_loca = i16_at(d, head.start + 50).ok_or_else(bad)? != 0;
        let ascent = f32::from(i16_at(d, hhea.start + 4).ok_or_else(bad)?);
        let descent = f32::from(i16_at(d, hhea.start + 6).ok_or_else(bad)?);
        let line_gap = f32::from(i16_at(d, hhea.start + 8).ok_or_else(bad)?);
        let num_hmetrics = u16_at(d, hhea.start + 34).ok_or_else(bad)?.max(1);
        let num_glyphs = u16_at(d, maxp.start + 4).ok_or_else(bad)?;

        // Unicode subtables: full repertoire (format 12) first, then the BMP (format 4).
        let n = u16_at(d, cmap.start + 2).ok_or_else(bad)? as usize;
        let mut best: Option<(u8, usize, u16)> = None;
        for i in 0..n {
            let rec = cmap.start + 4 + 8 * i;
            let (platform, encoding) = (u16_at(d, rec).ok_or_else(bad)?, u16_at(d, rec + 2).ok_or_else(bad)?);
            let off = cmap.start + u32_at(d, rec + 4).ok_or_else(bad)? as usize;
            let format = u16_at(d, off).ok_or_else(bad)?;
            let rank = match (platform, encoding, format) {
                (3, 10, 12) | (0, 4 | 6, 12) => 3,
                (3, 1, 4) | (0, 3, 4) => 2,
                (0, _, 4) => 1,
                _ => continue,
            };
            if best.is_none_or(|b| rank > b.0) {
                best = Some((rank, off, format));
            }
        }
        let (_, off, format) = best.ok_or("no Unicode character map")?;
        Ok(TrueType {
            units_per_em,
            ascent,
            descent,
            line_gap,
            glyf,
            loca,
            hmtx,
            long_loca,
            num_glyphs,
            num_hmetrics,
            cmap: (off, format),
            data,
        })
    }

    /// The glyph for `c` (0, the missing glyph, if the font has none).
    pub fn glyph_index(&self, c: char) -> u16 {
        let d = &self.data[..];
        let c = c as u32;
        let (off, format) = self.cmap;
        let found = if format == 12 {
            (|| {
                let groups = u32_at(d, off + 12)? as usize;
                let (mut lo, mut hi) = (0, groups);
                while lo < hi {
                    let mid = usize::midpoint(lo, hi);
                    let g = off + 16 + 12 * mid;
                    let (start, end) = (u32_at(d, g)?, u32_at(d, g + 4)?);
                    if c < start {
                        hi = mid;
                    } else if c > end {
                        lo = mid + 1;
                    } else {
                        return u16::try_from(u32_at(d, g + 8)? + (c - start)).ok();
                    }
                }
                None
            })()
        } else {
            (|| {
                let c = u16::try_from(c).ok()?;
                let seg_x2 = u16_at(d, off + 6)? as usize;
                let ends = off + 14;
                let starts = ends + seg_x2 + 2;
                let deltas = starts + seg_x2;
                let ranges = deltas + seg_x2;
                // The first segment whose end is at or after `c`.
                let (mut lo, mut hi) = (0, seg_x2 / 2);
                while lo < hi {
                    let mid = usize::midpoint(lo, hi);
                    if u16_at(d, ends + 2 * mid)? < c {
                        lo = mid + 1;
                    } else {
                        hi = mid;
                    }
                }
                let i = lo;
                let start = u16_at(d, starts + 2 * i)?;
                if c < start {
                    return None;
                }
                let delta = u16_at(d, deltas + 2 * i)?;
                let range = u16_at(d, ranges + 2 * i)? as usize;
                if range == 0 {
                    return Some(c.wrapping_add(delta));
                }
                let at = ranges + 2 * i + range + 2 * (c - start) as usize;
                let g = u16_at(d, at)?;
                (g != 0).then(|| g.wrapping_add(delta))
            })()
        };
        found.filter(|&g| g < self.num_glyphs).unwrap_or(0)
    }

    /// Advance width in font units.
    pub fn advance(&self, glyph: u16) -> f32 {
        let i = glyph.min(self.num_hmetrics - 1) as usize;
        f32::from(u16_at(&self.data, self.hmtx.start + 4 * i).unwrap_or(0))
    }

    fn glyph_range(&self, glyph: u16) -> Option<Range<usize>> {
        let d = &self.data[..];
        let i = glyph as usize;
        let (a, b) = if self.long_loca {
            (u32_at(d, self.loca.start + 4 * i)? as usize, u32_at(d, self.loca.start + 4 * i + 4)? as usize)
        } else {
            (2 * u16_at(d, self.loca.start + 2 * i)? as usize, 2 * u16_at(d, self.loca.start + 2 * i + 2)? as usize)
        };
        let r = self.glyf.start + a..self.glyf.start + b;
        (a < b && r.end <= self.glyf.end).then_some(r)
    }

    /// The outline of `glyph` in font units (empty for blank glyphs and malformed data).
    pub fn outline(&self, glyph: u16) -> Outline {
        let mut out = Outline::default();
        self.append_outline(glyph, [1.0, 0.0, 0.0, 1.0, 0.0, 0.0], 0, &mut out);
        out
    }

    /// Adds `glyph` transformed by `m` (`[a, b, c, d, dx, dy]`: x' = a x + c y + dx).
    fn append_outline(&self, glyph: u16, m: [f32; 6], depth: u32, out: &mut Outline) -> Option<()> {
        let d = &self.data[..];
        let r = self.glyph_range(glyph)?;
        let contours = i16_at(d, r.start)?;
        let xf = |x: f32, y: f32| Point { x: m[0] * x + m[2] * y + m[4], y: m[1] * x + m[3] * y + m[5] };
        if contours >= 0 {
            let n = contours as usize;
            let ends: Vec<usize> =
                (0..n).map(|i| u16_at(d, r.start + 10 + 2 * i).map(usize::from)).collect::<Option<_>>()?;
            let points = ends.last().map_or(0, |&e| e + 1);
            let ins = r.start + 10 + 2 * n;
            let mut p = ins + 2 + u16_at(d, ins)? as usize;
            let mut flags = Vec::with_capacity(points);
            while flags.len() < points {
                let f = *d.get(p)?;
                p += 1;
                flags.push(f);
                if f & 8 != 0 {
                    let repeat = *d.get(p)?;
                    p += 1;
                    flags.extend(std::iter::repeat_n(f, repeat as usize));
                }
            }
            flags.truncate(points);
            let coords = |short: u8, same: u8, p: &mut usize| -> Option<Vec<f32>> {
                let mut v = 0i32;
                let mut out = Vec::with_capacity(points);
                for &f in &flags {
                    if f & short != 0 {
                        let dv = i32::from(*d.get(*p)?);
                        *p += 1;
                        v += if f & same != 0 { dv } else { -dv };
                    } else if f & same == 0 {
                        v += i32::from(i16_at(d, *p)?);
                        *p += 2;
                    }
                    out.push(v as f32);
                }
                Some(out)
            };
            let xs = coords(2, 16, &mut p)?;
            let ys = coords(4, 32, &mut p)?;
            let mut start = 0;
            for &end in &ends {
                if end < start || end >= points {
                    return None;
                }
                let pts: Vec<(Point, bool)> = (start..=end).map(|i| (xf(xs[i], ys[i]), flags[i] & 1 != 0)).collect();
                contour(&pts, out);
                start = end + 1;
            }
        } else if depth < 8 {
            // Composite: components placed by offsets and optional scales.
            let mut p = r.start + 10;
            loop {
                let flags = u16_at(d, p)?;
                let child = u16_at(d, p + 2)?;
                p += 4;
                let (dx, dy) = if flags & 1 != 0 {
                    let v = (i16_at(d, p)?, i16_at(d, p + 2)?);
                    p += 4;
                    (f32::from(v.0), f32::from(v.1))
                } else {
                    let v = (d.get(p).copied()? as i8, d.get(p + 1).copied()? as i8);
                    p += 2;
                    (f32::from(v.0), f32::from(v.1))
                };
                // Point-matched placement (args are point numbers) is rare; place at the origin.
                let (dx, dy) = if flags & 2 != 0 { (dx, dy) } else { (0.0, 0.0) };
                let f2dot14 = |i: usize| i16_at(d, i).map(|v| f32::from(v) / 16384.0);
                let (a, b, c, e) = if flags & 0x08 != 0 {
                    let s = f2dot14(p)?;
                    p += 2;
                    (s, 0.0, 0.0, s)
                } else if flags & 0x40 != 0 {
                    let v = (f2dot14(p)?, f2dot14(p + 2)?);
                    p += 4;
                    (v.0, 0.0, 0.0, v.1)
                } else if flags & 0x80 != 0 {
                    let v = (f2dot14(p)?, f2dot14(p + 2)?, f2dot14(p + 4)?, f2dot14(p + 6)?);
                    p += 8;
                    v
                } else {
                    (1.0, 0.0, 0.0, 1.0)
                };
                // Child transform, then ours.
                let cm = [
                    m[0] * a + m[2] * b,
                    m[1] * a + m[3] * b,
                    m[0] * c + m[2] * e,
                    m[1] * c + m[3] * e,
                    m[0] * dx + m[2] * dy + m[4],
                    m[1] * dx + m[3] * dy + m[5],
                ];
                self.append_outline(child, cm, depth + 1, out);
                if flags & 0x20 == 0 {
                    break;
                }
            }
        }
        Some(())
    }
}

/// One closed contour from TrueType points (on-curve or quadratic control points; two
/// control points in a row imply an on-curve point midway).
fn contour(pts: &[(Point, bool)], out: &mut Outline) {
    if pts.is_empty() {
        return;
    }
    let mid = |a: Point, b: Point| Point { x: f32::midpoint(a.x, b.x), y: f32::midpoint(a.y, b.y) };
    // Start on an on-curve point and go round back to it, or (all control points) start
    // between the last and the first.
    let n = pts.len();
    let (start, order): (Point, Vec<(Point, bool)>) = match pts.iter().position(|p| p.1) {
        Some(i) => (pts[i].0, (1..=n).map(|k| pts[(i + k) % n]).collect()),
        None => (mid(pts[n - 1].0, pts[0].0), pts.to_vec()),
    };
    let mut cur = start;
    let mut ctrl: Option<Point> = None;
    for (p, on) in order {
        match (on, ctrl) {
            (true, None) => {
                out.segments.push(Segment::Line(cur, p));
                cur = p;
            }
            (true, Some(c)) => {
                out.segments.push(Segment::Quad(cur, c, p));
                cur = p;
                ctrl = None;
            }
            (false, None) => ctrl = Some(p),
            (false, Some(c)) => {
                let m = mid(c, p);
                out.segments.push(Segment::Quad(cur, c, m));
                cur = m;
                ctrl = Some(p);
            }
        }
    }
    match ctrl {
        Some(c) => out.segments.push(Segment::Quad(cur, c, start)),
        None if cur != start => out.segments.push(Segment::Line(cur, start)),
        None => {}
    }
}

/// A rasterized glyph: coverage 0-255, placed `left` pixels right of the pen and `top`
/// pixels below the baseline (negative: above).
#[derive(Clone, Debug, Default)]
pub struct Bitmap {
    pub w: usize,
    pub h: usize,
    pub left: i32,
    pub top: i32,
    pub coverage: Vec<u8>,
}

/// Rasterizes `outline` at `scale` pixels per font unit, shifted right by `dx` pixels
/// (sub-pixel placement).
pub fn rasterize(outline: &Outline, scale: f32, dx: f32) -> Bitmap {
    if outline.segments.is_empty() {
        return Bitmap::default();
    }
    // Pixel space: y down.
    let px = |p: Point| Point { x: p.x * scale + dx, y: -p.y * scale };
    let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    let mut lines: Vec<(Point, Point)> = Vec::new();
    for s in &outline.segments {
        match *s {
            Segment::Line(a, b) => lines.push((px(a), px(b))),
            Segment::Quad(a, c, b) => {
                let (a, c, b) = (px(a), px(c), px(b));
                let dev = (a.x - 2.0 * c.x + b.x).hypot(a.y - 2.0 * c.y + b.y);
                let n = if dev < 0.333 { 1 } else { 1 + (3.0 * dev).sqrt().sqrt() as usize };
                let mut prev = a;
                for i in 1..=n {
                    let t = i as f32 / n as f32;
                    let u = 1.0 - t;
                    let p = Point {
                        x: u * u * a.x + 2.0 * u * t * c.x + t * t * b.x,
                        y: u * u * a.y + 2.0 * u * t * c.y + t * t * b.y,
                    };
                    lines.push((prev, p));
                    prev = p;
                }
            }
        }
    }
    for &(a, b) in &lines {
        x0 = x0.min(a.x).min(b.x);
        y0 = y0.min(a.y).min(b.y);
        x1 = x1.max(a.x).max(b.x);
        y1 = y1.max(a.y).max(b.y);
    }
    let (left, top) = (x0.floor() as i32, y0.floor() as i32);
    let (w, h) = ((x1.ceil() as i32 - left).max(1) as usize, (y1.ceil() as i32 - top).max(1) as usize);
    // Two spare columns take the contributions right of the last pixel.
    let stride = w + 2;
    let mut acc = vec![0f32; stride * h];
    for &(a, b) in &lines {
        let a = Point { x: a.x - left as f32, y: a.y - top as f32 };
        let b = Point { x: b.x - left as f32, y: b.y - top as f32 };
        line(&mut acc, stride, h, a, b);
    }
    let mut coverage = vec![0u8; w * h];
    for y in 0..h {
        let mut sum = 0.0;
        for x in 0..w {
            sum += acc[y * stride + x];
            coverage[y * w + x] = (sum.abs().min(1.0) * 255.0 + 0.5) as u8;
        }
    }
    Bitmap { w, h, left, top, coverage }
}

/// Adds the signed area a line covers to the accumulation buffer: per row, each pixel gets
/// the change in coverage that the line causes there; a running sum along the row gives the
/// coverage (the nonzero winding rule, antialiased by exact area).
fn line(acc: &mut [f32], stride: usize, h: usize, p0: Point, p1: Point) {
    if (p0.y - p1.y).abs() < 1e-6 {
        return;
    }
    let (dir, p0, p1) = if p0.y < p1.y { (1.0, p0, p1) } else { (-1.0, p1, p0) };
    let dxdy = (p1.x - p0.x) / (p1.y - p0.y);
    let mut x = p0.x;
    let y_start = p0.y.max(0.0) as usize;
    if p0.y < 0.0 {
        x -= p0.y * dxdy;
    }
    let y_end = (p1.y.ceil().max(0.0) as usize).min(h);
    for y in y_start..y_end {
        let row = y * stride;
        let dy = ((y + 1) as f32).min(p1.y) - (y as f32).max(p0.y);
        let xnext = x + dxdy * dy;
        let d = dy * dir;
        let (xa, xb) = if x < xnext { (x, xnext) } else { (xnext, x) };
        let xa = xa.max(0.0);
        let xb = xb.max(0.0);
        let xa_floor = xa.floor();
        let ia = xa_floor as usize;
        let ib = xb.ceil() as usize;
        let at = |i: usize| row + i.min(stride - 1);
        if ib <= ia + 1 {
            // Within one pixel: split by the line's mean position in it.
            let xm = f32::midpoint(xa, xb) - xa_floor;
            acc[at(ia)] += d - d * xm;
            acc[at(ia + 1)] += d * xm;
        } else {
            let s = (xb - xa).recip();
            let fa = xa - xa_floor;
            let a0 = 0.5 * s * (1.0 - fa) * (1.0 - fa);
            let fb = xb - xb.ceil() + 1.0;
            let am = 0.5 * s * fb * fb;
            acc[at(ia)] += d * a0;
            if ib == ia + 2 {
                acc[at(ia + 1)] += d * (1.0 - a0 - am);
            } else {
                let a1 = s * (1.5 - fa);
                acc[at(ia + 1)] += d * (a1 - a0);
                for i in ia + 2..ib - 1 {
                    acc[at(i)] += d * s;
                }
                let a2 = a1 + (ib - ia - 3) as f32 * s;
                acc[at(ib - 1)] += d * (1.0 - a2 - am);
            }
            acc[at(ib)] += d * am;
        }
        x = xnext;
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A minimal TrueType font: glyph 0 empty, glyph 1 ('A') a square with a square hole
    /// (two contours of opposite direction), glyph 2 ('B') a diamond of control points
    /// (all off-curve: a rounded shape), glyph 3 ('C') a composite of glyph 1 shifted right
    /// and scaled by one half. 1000 units per em.
    pub fn test_font() -> Vec<u8> {
        fn be16(v: &mut Vec<u8>, x: i32) {
            v.extend_from_slice(&(x as i16).to_be_bytes());
        }
        fn simple(contours: &[&[(i32, i32, bool)]]) -> Vec<u8> {
            let mut g = Vec::new();
            be16(&mut g, contours.len() as i32);
            for _ in 0..4 {
                be16(&mut g, 0); // bounds (not used)
            }
            let mut end = -1;
            for c in contours {
                end += c.len() as i32;
                be16(&mut g, end);
            }
            be16(&mut g, 0); // no instructions
            let pts: Vec<&(i32, i32, bool)> = contours.iter().flat_map(|c| c.iter()).collect();
            g.extend(pts.iter().map(|p| u8::from(p.2)));
            let (mut px, mut py) = (0, 0);
            for p in &pts {
                be16(&mut g, p.0 - px);
                px = p.0;
            }
            for p in &pts {
                be16(&mut g, p.1 - py);
                py = p.1;
            }
            g
        }
        let a = simple(&[
            &[(100, 0, true), (100, 700, true), (800, 700, true), (800, 0, true)],
            &[(300, 200, true), (600, 200, true), (600, 500, true), (300, 500, true)],
        ]);
        let b = simple(&[&[(450, 0, false), (900, 350, false), (450, 700, false), (0, 350, false)]]);
        let mut c = Vec::new();
        be16(&mut c, -1);
        for _ in 0..4 {
            be16(&mut c, 0);
        }
        // ARG_1_AND_2_ARE_WORDS | ARGS_ARE_XY_VALUES | WE_HAVE_A_SCALE, glyph 1, (500, 0), 0.5
        c.extend_from_slice(&[0x00, 0x0B, 0x00, 0x01]);
        be16(&mut c, 500);
        be16(&mut c, 0);
        be16(&mut c, 0x2000);
        let glyphs = [Vec::new(), a, b, c];
        let mut glyf = Vec::new();
        let mut loca = Vec::new();
        for g in &glyphs {
            loca.extend_from_slice(&(glyf.len() as u32).to_be_bytes());
            glyf.extend_from_slice(g);
            while glyf.len() % 4 != 0 {
                glyf.push(0);
            }
        }
        loca.extend_from_slice(&(glyf.len() as u32).to_be_bytes());
        let mut head = vec![0u8; 54];
        head[18..20].copy_from_slice(&1000u16.to_be_bytes());
        head[50..52].copy_from_slice(&1u16.to_be_bytes()); // long loca
        let mut hhea = vec![0u8; 36];
        hhea[4..6].copy_from_slice(&800i16.to_be_bytes());
        hhea[6..8].copy_from_slice(&(-200i16).to_be_bytes());
        hhea[34..36].copy_from_slice(&4u16.to_be_bytes());
        let mut maxp = vec![0u8; 6];
        maxp[4..6].copy_from_slice(&4u16.to_be_bytes());
        let mut hmtx = Vec::new();
        for adv in [500u16, 900, 950, 900] {
            hmtx.extend_from_slice(&adv.to_be_bytes());
            hmtx.extend_from_slice(&0u16.to_be_bytes());
        }
        // cmap format 4: 'A'..'C' -> 1..3, plus the 0xFFFF terminator segment.
        let mut sub = Vec::new();
        for v in [4u16, 0, 0, 4, 0, 0, 0, b'C'.into(), 0xFFFF, 0, b'A'.into(), 0xFFFF] {
            sub.extend_from_slice(&v.to_be_bytes());
        }
        sub.extend_from_slice(&(1u16.wrapping_sub(b'A'.into())).to_be_bytes());
        sub.extend_from_slice(&1u16.to_be_bytes());
        sub.extend_from_slice(&[0, 0, 0, 0]);
        let len = sub.len() as u16;
        sub[2..4].copy_from_slice(&len.to_be_bytes());
        let mut cmap = Vec::new();
        for v in [0u16, 1, 3, 1] {
            cmap.extend_from_slice(&v.to_be_bytes());
        }
        cmap.extend_from_slice(&12u32.to_be_bytes());
        cmap.extend_from_slice(&sub);

        let tables: [(&[u8; 4], &Vec<u8>); 7] = [
            (b"cmap", &cmap),
            (b"glyf", &glyf),
            (b"head", &head),
            (b"hhea", &hhea),
            (b"hmtx", &hmtx),
            (b"loca", &loca),
            (b"maxp", &maxp),
        ];
        let mut font = Vec::new();
        font.extend_from_slice(&0x0001_0000u32.to_be_bytes());
        font.extend_from_slice(&(tables.len() as u16).to_be_bytes());
        font.extend_from_slice(&[0; 6]);
        let mut offset = 12 + 16 * tables.len();
        let mut body = Vec::new();
        for (tag, data) in tables {
            font.extend_from_slice(tag);
            font.extend_from_slice(&[0; 4]);
            font.extend_from_slice(&(offset as u32).to_be_bytes());
            font.extend_from_slice(&(data.len() as u32).to_be_bytes());
            body.extend_from_slice(data);
            while body.len() % 4 != 0 {
                body.push(0);
            }
            offset = 12 + 16 * tables.len() + body.len();
        }
        font.extend_from_slice(&body);
        font
    }

    fn at(b: &Bitmap, x: i32, y: i32) -> u8 {
        b.coverage[(y - b.top) as usize * b.w + (x - b.left) as usize]
    }

    #[test]
    fn reads_metrics_and_maps_characters() {
        let f = TrueType::parse(test_font(), 0).unwrap();
        assert_eq!((f.units_per_em, f.ascent, f.descent), (1000.0, 800.0, -200.0));
        assert_eq!(['A', 'B', 'C', 'Z', 'é'].map(|c| f.glyph_index(c)), [1, 2, 3, 0, 0]);
        assert_eq!(f.advance(1), 900.0);
        assert!(TrueType::parse(b"OTTO\0\0\0\0".to_vec(), 0).err().unwrap_or_default().contains("CFF"));
        assert!(TrueType::parse(vec![1, 2, 3], 0).is_err());
    }

    #[test]
    fn rasterizes_with_holes_and_antialiasing() {
        let f = TrueType::parse(test_font(), 0).unwrap();
        // 0.02 px per unit: the square spans x 2..16, y -14..0; the hole x 6..12, y -10..-4.
        let b = rasterize(&f.outline(1), 0.02, 0.0);
        assert_eq!((b.left, b.top, b.w, b.h), (2, -14, 14, 14));
        assert_eq!(at(&b, 3, -2), 255, "inside");
        assert_eq!(at(&b, 8, -7), 0, "the hole");
        // Half a pixel to the right: the edge pixels are half covered.
        let b = rasterize(&f.outline(1), 0.02, 0.5);
        let edge = at(&b, 2, -7);
        assert!((120..=135).contains(&edge), "{edge}");
        // A row sums to the shape's width there (area is exact).
        let row: u32 = (0..b.w).map(|x| u32::from(b.coverage[b.w + x])).sum();
        assert!((row as f32 / 255.0 - 14.0).abs() < 0.05, "{row}");
    }

    #[test]
    fn curves_and_composites() {
        let f = TrueType::parse(test_font(), 0).unwrap();
        let o = f.outline(2);
        assert_eq!(o.segments.len(), 4);
        assert!(o.segments.iter().all(|s| matches!(s, Segment::Quad(..))));
        let b = rasterize(&o, 0.02, 0.0);
        // A rounded diamond: full in the middle, empty in the bounding box's corners.
        assert_eq!(at(&b, 9, -7), 255);
        assert_eq!(b.coverage[0], 0);
        // The composite: the square at half size, 500 units right.
        let c = f.outline(3);
        let b = rasterize(&c, 0.02, 0.0);
        assert_eq!((b.left, b.top, b.w, b.h), (11, -7, 7, 7));
    }
}
