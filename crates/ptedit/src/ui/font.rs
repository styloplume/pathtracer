//! Text for the UI: a TrueType font from the system (or a file) rasterized by our own
//! `ttf` module into a glyph cache, or, when none can be read, the embedded 8x8 bitmap font
//! scaled by whole pixels. Both measure and draw the same way.

use super::font8x8;
use super::ttf::{Bitmap, TrueType, rasterize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Sub-pixel positions per pixel that glyphs are cached at.
const SUBPIXEL: f32 = 4.0;

enum Kind {
    /// The 8x8 font, each font pixel `scale` screen pixels wide.
    Bitmap {
        scale: i32,
    },
    TrueType(Box<TrueType>),
}

pub struct Font {
    kind: Kind,
    /// The em size in pixels.
    pub size: f32,
    /// Where it came from: a file path, or "built-in".
    pub source: String,
    cache: HashMap<(u16, u8), Bitmap>,
    /// Coverage to opacity (a slight boost keeps thin strokes from looking washed out).
    ramp: [u8; 256],
}

/// A clip rectangle: pixels with x0 <= x < x1, y0 <= y < y1 are drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Clip {
    pub x0: i32,
    pub y0: i32,
    pub x1: i32,
    pub y1: i32,
}

/// A 0x00RRGGBB pixel buffer.
pub struct Target<'a> {
    pub px: &'a mut [u32],
    pub w: usize,
    pub h: usize,
}

/// Mixes `src` over `dst` with opacity `a` (0-255).
#[inline]
pub fn blend(dst: u32, src: u32, a: u32) -> u32 {
    if a >= 255 {
        return src;
    }
    let mix = |shift: u32| {
        let (d, s) = ((dst >> shift) & 0xFF, (src >> shift) & 0xFF);
        ((d * (255 - a) + s * a + 127) / 255) << shift
    };
    mix(16) | mix(8) | mix(0)
}

/// Font files tried, in order, when no font is configured.
fn system_candidates() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if cfg!(windows) {
        let dir = std::env::var_os("WINDIR").map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from);
        for f in ["segoeui.ttf", "tahoma.ttf", "arial.ttf"] {
            v.push(dir.join("Fonts").join(f));
        }
    } else if cfg!(target_os = "macos") {
        for f in [
            "/System/Library/Fonts/Supplemental/Arial.ttf",
            "/Library/Fonts/Arial.ttf",
            "/System/Library/Fonts/Geneva.ttf",
            "/System/Library/Fonts/Helvetica.ttc",
        ] {
            v.push(PathBuf::from(f));
        }
    } else {
        for f in [
            "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
            "/usr/share/fonts/TTF/DejaVuSans.ttf",
            "/usr/share/fonts/dejavu/DejaVuSans.ttf",
            "/usr/share/fonts/dejavu-sans-fonts/DejaVuSans.ttf",
            "/usr/share/fonts/truetype/noto/NotoSans-Regular.ttf",
            "/usr/share/fonts/noto/NotoSans-Regular.ttf",
            "/usr/share/fonts/truetype/liberation/LiberationSans-Regular.ttf",
            "/usr/share/fonts/liberation/LiberationSans-Regular.ttf",
            "/usr/share/fonts/truetype/freefont/FreeSans.ttf",
        ] {
            v.push(PathBuf::from(f));
        }
    }
    v
}

impl Font {
    fn with(kind: Kind, size: f32, source: String) -> Font {
        let ramp = std::array::from_fn(|i| ((i as f32 / 255.0).powf(0.85) * 255.0 + 0.5) as u8);
        Font { kind, size, source, cache: HashMap::new(), ramp }
    }

    /// The embedded 8x8 font at the whole-pixel scale nearest to `size`.
    pub fn bitmap(size: f32) -> Font {
        let scale = (size / 8.0).round().max(1.0) as i32;
        Font::with(Kind::Bitmap { scale }, (8 * scale) as f32, "built-in".into())
    }

    pub fn from_file(path: &Path, size: f32) -> Result<Font, String> {
        let data = std::fs::read(path).map_err(|e| format!("cannot read '{}': {e}", path.display()))?;
        let tt = TrueType::parse(data, 0).map_err(|e| format!("'{}': {e}", path.display()))?;
        Ok(Font::with(Kind::TrueType(Box::new(tt)), size, path.display().to_string()))
    }

    /// `choice`: a font file, "built-in", or empty for the first system font found (else
    /// the built-in font).
    pub fn load(choice: &str, size: f32) -> Font {
        if choice == "built-in" {
            return Font::bitmap(size);
        }
        if !choice.is_empty() {
            match Font::from_file(Path::new(choice), size) {
                Ok(f) => return f,
                Err(e) => eprintln!("warning: {e}; using a system font"),
            }
        }
        system_candidates()
            .iter()
            .filter(|p| p.is_file())
            .find_map(|p| Font::from_file(p, size).ok())
            .unwrap_or_else(|| Font::bitmap(size))
    }

    #[cfg(test)]
    pub fn is_bitmap(&self) -> bool {
        matches!(self.kind, Kind::Bitmap { .. })
    }

    fn px_per_unit(tt: &TrueType, size: f32) -> f32 {
        size / tt.units_per_em
    }

    /// Distance from the top of a line to the baseline.
    pub fn ascent(&self) -> i32 {
        match &self.kind {
            Kind::Bitmap { scale } => 7 * scale,
            Kind::TrueType(tt) => (tt.ascent * Font::px_per_unit(tt, self.size)).round() as i32,
        }
    }

    /// Height of a line of text.
    pub fn line_height(&self) -> i32 {
        match &self.kind {
            Kind::Bitmap { scale } => 8 * scale,
            Kind::TrueType(tt) => {
                ((tt.ascent - tt.descent + tt.line_gap.max(0.0)) * Font::px_per_unit(tt, self.size)).ceil() as i32
            }
        }
    }

    pub fn advance(&self, c: char) -> f32 {
        match &self.kind {
            Kind::Bitmap { scale } => (8 * scale) as f32,
            Kind::TrueType(tt) => tt.advance(tt.glyph_index(c)) * Font::px_per_unit(tt, self.size),
        }
    }

    pub fn width(&self, s: &str) -> f32 {
        s.chars().map(|c| self.advance(c)).sum()
    }

    /// The x offset of every char boundary of `s` (one more than its chars), for carets.
    pub fn offsets(&self, s: &str) -> Vec<f32> {
        let mut out = Vec::with_capacity(s.len() + 1);
        let mut x = 0.0;
        out.push(0.0);
        for c in s.chars() {
            x += self.advance(c);
            out.push(x);
        }
        out
    }

    /// `s` cut to fit `max` pixels, ending in "…" when cut.
    pub fn elide(&self, s: &str, max: f32) -> String {
        if self.width(s) <= max {
            return s.to_owned();
        }
        let dots = self.width("…");
        let mut w = 0.0;
        let mut out = String::new();
        for c in s.chars() {
            w += self.advance(c);
            if w + dots > max {
                break;
            }
            out.push(c);
        }
        out.push('…');
        out
    }

    /// Draws `s` with its line's top at `y` and its pen starting at `x`.
    pub fn draw(&mut self, t: &mut Target<'_>, clip: Clip, x: f32, y: i32, s: &str, color: u32) {
        let clip =
            Clip { x0: clip.x0.max(0), y0: clip.y0.max(0), x1: clip.x1.min(t.w as i32), y1: clip.y1.min(t.h as i32) };
        if clip.x0 >= clip.x1 || clip.y0 >= clip.y1 {
            return;
        }
        let baseline = y + self.ascent();
        let mut pen = x;
        match &self.kind {
            Kind::Bitmap { scale } => {
                let scale = *scale;
                for c in s.chars() {
                    let gx = pen.round() as i32;
                    pen += (8 * scale) as f32;
                    if gx >= clip.x1 || gx + 8 * scale <= clip.x0 {
                        continue;
                    }
                    for (row, bits) in font8x8::glyph(c).iter().enumerate() {
                        for col in 0..8 {
                            if bits & (1 << col) != 0 {
                                let (px, py) = (gx + col * scale, y + row as i32 * scale);
                                for yy in py.max(clip.y0)..(py + scale).min(clip.y1) {
                                    for xx in px.max(clip.x0)..(px + scale).min(clip.x1) {
                                        t.px[yy as usize * t.w + xx as usize] = color;
                                    }
                                }
                            }
                        }
                    }
                }
            }
            Kind::TrueType(tt) => {
                let k = Font::px_per_unit(tt, self.size);
                for c in s.chars() {
                    let g = tt.glyph_index(c);
                    let adv = tt.advance(g) * k;
                    let whole = pen.floor();
                    let sub = ((pen - whole) * SUBPIXEL).floor().min(SUBPIXEL - 1.0);
                    if whole as i32 >= clip.x1 {
                        break;
                    }
                    let bm = self
                        .cache
                        .entry((g, sub as u8))
                        .or_insert_with(|| rasterize(&tt.outline(g), k, sub / SUBPIXEL));
                    let (ox, oy) = (whole as i32 + bm.left, baseline + bm.top);
                    for gy in 0..bm.h as i32 {
                        let yy = oy + gy;
                        if yy < clip.y0 || yy >= clip.y1 {
                            continue;
                        }
                        for gx in 0..bm.w as i32 {
                            let xx = ox + gx;
                            if xx < clip.x0 || xx >= clip.x1 {
                                continue;
                            }
                            let a = bm.coverage[(gy * bm.w as i32 + gx) as usize];
                            if a > 0 {
                                let d = &mut t.px[yy as usize * t.w + xx as usize];
                                *d = blend(*d, color, u32::from(self.ramp[a as usize]));
                            }
                        }
                    }
                    pen += adv;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draw(f: &mut Font, s: &str) -> (Vec<u32>, usize, usize) {
        let (w, h) = (120, 40);
        let mut px = vec![0u32; w * h];
        let clip = Clip { x0: 0, y0: 0, x1: w as i32, y1: h as i32 };
        f.draw(&mut Target { px: &mut px, w, h }, clip, 2.0, 2, s, 0xFF_FFFF);
        (px, w, h)
    }

    #[test]
    fn bitmap_font_measures_and_draws() {
        let mut f = Font::bitmap(16.0);
        assert_eq!((f.line_height(), f.width("ab")), (16, 32.0));
        assert_eq!(f.offsets("ab"), vec![0.0, 16.0, 32.0]);
        let (px, ..) = draw(&mut f, "I");
        assert!(px.iter().filter(|&&p| p != 0).count() > 30);
        assert_eq!(f.elide("abcdefgh", 50.0), "ab…");
    }

    #[test]
    fn truetype_font_draws_cached_glyphs() {
        let dir = std::env::temp_dir().join(format!("ptedit-font-{}.ttf", std::process::id()));
        std::fs::write(&dir, super::super::ttf::tests::test_font()).unwrap();
        let mut f = Font::from_file(&dir, 20.0).unwrap();
        std::fs::remove_file(&dir).ok();
        assert_eq!(f.ascent(), 16);
        assert_eq!(f.line_height(), 20);
        assert!((f.width("AB") - 37.0).abs() < 1e-3);
        let (px, w, _) = draw(&mut f, "A");
        let lit: Vec<usize> = (0..px.len()).filter(|&i| px[i] != 0).collect();
        assert_ne!(lit, [] as [usize; 0]);
        // The square sits on the baseline (y = 2 + 16), from x = 2 + 2 to 2 + 16.
        let (x0, y1) = (lit.iter().map(|i| i % w).min().unwrap(), lit.iter().map(|i| i / w).max().unwrap());
        assert_eq!((x0, y1), (4, 17));
        assert_eq!(f.cache.len(), 1);
        draw(&mut f, "AA");
        assert_eq!(f.cache.len(), 1, "same glyph, same sub-pixel position");
    }

    #[test]
    fn a_system_font_loads_where_there_is_one() {
        let f = Font::load("", 14.0);
        if !f.is_bitmap() {
            assert!(f.width("Hello") > 20.0 && f.width("Hello") < 60.0, "{}", f.width("Hello"));
            assert!((14..=22).contains(&f.line_height()));
        }
    }
}
