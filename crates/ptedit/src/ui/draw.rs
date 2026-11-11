//! What the UI draws: a list of shapes with their clip rectangles and layers, recorded while
//! the widgets run and painted afterwards, layer by layer (so popups opened by a widget
//! cover the widgets after it). Painting is plain software: rectangles with antialiased
//! rounded corners, triangles with 2x2 supersampling, text through the font, images.

use super::font::{Clip, Font, Target, blend};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub const fn new(x: i32, y: i32, w: i32, h: i32) -> Rect {
        Rect { x, y, w, h }
    }

    pub fn right(&self) -> i32 {
        self.x + self.w
    }

    pub fn bottom(&self) -> i32 {
        self.y + self.h
    }

    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x && y >= self.y && x < self.right() && y < self.bottom()
    }

    pub fn is_empty(&self) -> bool {
        self.w <= 0 || self.h <= 0
    }

    pub fn intersect(&self, o: &Rect) -> Rect {
        let (x0, y0) = (self.x.max(o.x), self.y.max(o.y));
        let (x1, y1) = (self.right().min(o.right()), self.bottom().min(o.bottom()));
        Rect { x: x0, y: y0, w: (x1 - x0).max(0), h: (y1 - y0).max(0) }
    }

    /// Smaller by `d` on every side.
    pub fn shrink(&self, d: i32) -> Rect {
        Rect { x: self.x + d, y: self.y + d, w: (self.w - 2 * d).max(0), h: (self.h - 2 * d).max(0) }
    }

    pub fn shrink2(&self, dx: i32, dy: i32) -> Rect {
        Rect { x: self.x + dx, y: self.y + dy, w: (self.w - 2 * dx).max(0), h: (self.h - 2 * dy).max(0) }
    }

    /// The left `w` pixels, and the rest after a `gap`.
    pub fn split_left(&self, w: i32, gap: i32) -> (Rect, Rect) {
        let w = w.clamp(0, self.w);
        (Rect { w, ..*self }, Rect { x: self.x + w + gap, w: (self.w - w - gap).max(0), ..*self })
    }

    /// The right `w` pixels, and the rest before a `gap`.
    pub fn split_right(&self, w: i32, gap: i32) -> (Rect, Rect) {
        let w = w.clamp(0, self.w);
        (Rect { x: self.right() - w, w, ..*self }, Rect { w: (self.w - w - gap).max(0), ..*self })
    }

    /// The top `h` pixels, and the rest.
    pub fn split_top(&self, h: i32) -> (Rect, Rect) {
        let h = h.clamp(0, self.h);
        (Rect { h, ..*self }, Rect { y: self.y + h, h: self.h - h, ..*self })
    }

    /// The bottom `h` pixels, and the rest.
    pub fn split_bottom(&self, h: i32) -> (Rect, Rect) {
        let h = h.clamp(0, self.h);
        (Rect { y: self.bottom() - h, h, ..*self }, Rect { h: self.h - h, ..*self })
    }

    /// `n` equal columns separated by `gap`.
    pub fn columns(&self, n: usize, gap: i32) -> Vec<Rect> {
        let n = n.max(1) as i32;
        let inner = self.w - gap * (n - 1);
        (0..n)
            .map(|i| {
                let x0 = self.x + i * (inner / n + gap) + (i * (inner % n) / n);
                let x1 = self.x + (i + 1) * (inner / n) + i * gap + ((i + 1) * (inner % n) / n);
                Rect { x: x0, w: (x1 - x0).max(0), ..*self }
            })
            .collect()
    }

    fn clip(&self) -> Clip {
        Clip { x0: self.x, y0: self.y, x1: self.right(), y1: self.bottom() }
    }
}

#[derive(Clone, Debug)]
pub enum Shape {
    Fill {
        r: Rect,
        color: u32,
        alpha: u8,
        radius: i32,
    },
    /// A one-pixel outline.
    Border {
        r: Rect,
        color: u32,
        radius: i32,
    },
    /// Text with its line's top at `y`.
    Text {
        x: f32,
        y: i32,
        text: String,
        color: u32,
    },
    Tri {
        p: [(f32, f32); 3],
        color: u32,
    },
    /// `w`x`h` pixels stretched over `r` (nearest).
    Image {
        r: Rect,
        w: usize,
        h: usize,
        px: Vec<u32>,
    },
}

#[derive(Clone, Debug)]
pub struct Cmd {
    pub layer: u8,
    pub clip: Rect,
    pub shape: Shape,
}

/// Paints the commands in layer order (stable within a layer).
pub fn paint(cmds: &mut [Cmd], font: &mut Font, t: &mut Target<'_>) {
    cmds.sort_by_key(|c| c.layer);
    let screen = Rect::new(0, 0, t.w as i32, t.h as i32);
    for c in cmds.iter() {
        let clip = c.clip.intersect(&screen);
        if clip.is_empty() {
            continue;
        }
        match &c.shape {
            Shape::Fill { r, color, alpha, radius } => fill(t, &clip, r, *color, *alpha, *radius),
            Shape::Border { r, color, radius } => border(t, &clip, r, *color, *radius),
            Shape::Text { x, y, text, color } => font.draw(t, clip.clip(), *x, *y, text, *color),
            Shape::Tri { p, color } => tri(t, &clip, p, *color),
            Shape::Image { r, w, h, px } => image(t, &clip, r, *w, *h, px),
        }
    }
}

/// Coverage (0-1) of pixel (`x`, `y`) by a rounded rectangle: 1 away from the corners.
fn corner_coverage(r: &Rect, radius: i32, x: i32, y: i32) -> f32 {
    if radius <= 0 {
        return 1.0;
    }
    let rad = radius as f32;
    let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
    let cx = if x < r.x + radius {
        r.x as f32 + rad
    } else if x >= r.right() - radius {
        r.right() as f32 - rad
    } else {
        return 1.0;
    };
    let cy = if y < r.y + radius {
        r.y as f32 + rad
    } else if y >= r.bottom() - radius {
        r.bottom() as f32 - rad
    } else {
        return 1.0;
    };
    (rad - (fx - cx).hypot(fy - cy) + 0.5).clamp(0.0, 1.0)
}

fn put(t: &mut Target<'_>, x: i32, y: i32, color: u32, a: f32) {
    let i = y as usize * t.w + x as usize;
    t.px[i] = blend(t.px[i], color, (a * 255.0 + 0.5) as u32);
}

fn fill(t: &mut Target<'_>, clip: &Rect, r: &Rect, color: u32, alpha: u8, radius: i32) {
    let area = r.intersect(clip);
    let radius = radius.min(r.w / 2).min(r.h / 2);
    let a = f32::from(alpha) / 255.0;
    for y in area.y..area.bottom() {
        let near_edge = y < r.y + radius || y >= r.bottom() - radius;
        let row = y as usize * t.w;
        if !near_edge && alpha == 255 {
            t.px[row + area.x as usize..row + area.right() as usize].fill(color);
            continue;
        }
        for x in area.x..area.right() {
            let c = if near_edge { corner_coverage(r, radius, x, y) } else { 1.0 };
            if c > 0.0 {
                put(t, x, y, color, c * a);
            }
        }
    }
}

fn border(t: &mut Target<'_>, clip: &Rect, r: &Rect, color: u32, radius: i32) {
    let area = r.intersect(clip);
    let radius = radius.min(r.w / 2).min(r.h / 2);
    let inner = r.shrink(1);
    let inner_radius = (radius - 1).max(0);
    for y in area.y..area.bottom() {
        let band = radius.max(1);
        let rows_edge = y < r.y + band || y >= r.bottom() - band;
        let xs: Vec<i32> = if rows_edge {
            (area.x..area.right()).collect()
        } else {
            (area.x..area.right().min(r.x + band)).chain((area.x.max(r.right() - band))..area.right()).collect()
        };
        for x in xs {
            let outer = corner_coverage(r, radius, x, y);
            let inside = if inner.contains(x, y) { corner_coverage(&inner, inner_radius, x, y) } else { 0.0 };
            let c = (outer - inside).clamp(0.0, 1.0);
            if c > 0.0 {
                put(t, x, y, color, c);
            }
        }
    }
}

fn tri(t: &mut Target<'_>, clip: &Rect, p: &[(f32, f32); 3], color: u32) {
    let (x0, x1) = (p.iter().map(|q| q.0).fold(f32::MAX, f32::min), p.iter().map(|q| q.0).fold(f32::MIN, f32::max));
    let (y0, y1) = (p.iter().map(|q| q.1).fold(f32::MAX, f32::min), p.iter().map(|q| q.1).fold(f32::MIN, f32::max));
    let area = Rect::new(x0.floor() as i32, y0.floor() as i32, 0, 0);
    let area = Rect { w: x1.ceil() as i32 - area.x + 1, h: y1.ceil() as i32 - area.y + 1, ..area }.intersect(clip);
    let edge = |a: (f32, f32), b: (f32, f32), x: f32, y: f32| (b.0 - a.0) * (y - a.1) - (b.1 - a.1) * (x - a.0);
    let area2 = edge(p[0], p[1], p[2].0, p[2].1);
    if area2.abs() < 1e-6 {
        return;
    }
    let s = area2.signum();
    for y in area.y..area.bottom() {
        for x in area.x..area.right() {
            let mut n = 0;
            for (sx, sy) in [(0.25, 0.25), (0.75, 0.25), (0.25, 0.75), (0.75, 0.75)] {
                let (fx, fy) = (x as f32 + sx, y as f32 + sy);
                if (0..3).all(|i| edge(p[i], p[(i + 1) % 3], fx, fy) * s >= 0.0) {
                    n += 1;
                }
            }
            if n > 0 {
                put(t, x, y, color, n as f32 / 4.0);
            }
        }
    }
}

fn image(t: &mut Target<'_>, clip: &Rect, r: &Rect, w: usize, h: usize, px: &[u32]) {
    if w == 0 || h == 0 || px.len() < w * h || r.is_empty() {
        return;
    }
    let area = r.intersect(clip);
    for y in area.y..area.bottom() {
        let sy = ((y - r.y) as usize * h / r.h as usize).min(h - 1);
        for x in area.x..area.right() {
            let sx = ((x - r.x) as usize * w / r.w as usize).min(w - 1);
            t.px[y as usize * t.w + x as usize] = px[sy * w + sx];
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rects_split_and_tile() {
        let r = Rect::new(10, 20, 101, 30);
        let cols = r.columns(3, 4);
        assert_eq!(cols.iter().map(|c| c.w).sum::<i32>() + 8, 101);
        assert_eq!((cols[0].x, cols[2].right()), (10, 111));
        let (a, b) = r.split_left(40, 2);
        assert_eq!((a.w, b.x, b.w), (40, 52, 59));
        assert_eq!(r.intersect(&Rect::new(0, 0, 15, 25)), Rect::new(10, 20, 5, 5));
    }

    #[test]
    fn paints_shapes_in_layer_order_within_clips() {
        let (w, h) = (40, 30);
        let mut px = vec![0u32; w * h];
        let mut font = Font::bitmap(8.0);
        let full = Rect::new(0, 0, 40, 30);
        let mut cmds = vec![
            Cmd {
                layer: 1,
                clip: full,
                shape: Shape::Fill { r: Rect::new(5, 5, 10, 10), color: 0xFF, alpha: 255, radius: 0 },
            },
            Cmd {
                layer: 0,
                clip: full,
                shape: Shape::Fill { r: Rect::new(0, 0, 40, 30), color: 0x10, alpha: 255, radius: 0 },
            },
            Cmd {
                layer: 0,
                clip: Rect::new(0, 0, 20, 30),
                shape: Shape::Fill { r: Rect::new(18, 20, 10, 5), color: 0xFF_0000, alpha: 255, radius: 0 },
            },
            Cmd {
                layer: 0,
                clip: full,
                shape: Shape::Fill { r: Rect::new(25, 2, 10, 10), color: 0xFF_FFFF, alpha: 255, radius: 4 },
            },
        ];
        paint(&mut cmds, &mut font, &mut Target { px: &mut px, w, h });
        assert_eq!(px[7 * w + 7], 0xFF, "the later layer covers the background");
        assert_eq!(px[22 * w + 19], 0xFF_0000);
        assert_eq!(px[22 * w + 20], 0x10, "clipped");
        assert_eq!(px[2 * w + 25], 0x10, "rounded corner");
        assert_eq!(px[6 * w + 30], 0xFF_FFFF);
    }
}
