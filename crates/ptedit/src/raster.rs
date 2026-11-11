//! The viewport's software rasterizer: triangles and lines in homogeneous clip space,
//! clipped, binned into tiles of the target and rasterized by all cores.
//!
//! Depth is reversed: the clip-space `z / w` is 1 at the near plane and falls towards 0
//! (perspective: `near / distance`, linear in screen space and precise in f32 far away;
//! orthographic: linear in distance). The depth buffer clears to 0 and nearer wins.
//! Colors are interpolated perspective-correctly (attributes over `w`, divided by the
//! interpolated `1 / w`).
//!
//! A draw runs a list of passes. Each pass runs its jobs on every thread; a job emits
//! primitives, which are clipped, set up in screen space and binned into the tiles they
//! touch. After a barrier every thread takes whole tiles and rasterizes everything binned
//! there, in emission order per thread. The tiles are stored one after another (each one
//! contiguous), so every thread owns the pixels it writes. Passes are separated by
//! barriers, so a later pass sees the depth an earlier one wrote (lines tested against
//! finished surfaces). Finally the tiles are copied into the row-major target.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Barrier, Mutex, RwLock};

/// Tile size in pixels: the unit of binning and of parallel rasterization. Wide and
/// short, so a band of dense geometry (a horizon) still spreads over many tiles.
pub const TILE_W: usize = 32;
pub const TILE_H: usize = 8;
const TILE_PX: usize = TILE_W * TILE_H;
/// Triangles within ±`GUARD`·w of the view are rasterized without clipping their sides
/// (the bounding box does that); only the near and far planes always clip.
const GUARD: f32 = 4.0;

/// A color, depth and id buffer, row by row.
pub struct Target {
    pub w: usize,
    pub h: usize,
    /// 0x00RRGGBB.
    pub color: Vec<u32>,
    /// Reversed depth: larger is nearer, 0 is empty.
    pub depth: Vec<f32>,
    /// What covers each pixel (0 = nothing).
    pub id: Vec<u32>,
}

impl Target {
    pub fn new(w: usize, h: usize) -> Target {
        Target { w, h, color: vec![0; w * h], depth: vec![0.0; w * h], id: vec![0; w * h] }
    }

    pub fn resize(&mut self, w: usize, h: usize) {
        if (w, h) != (self.w, self.h) {
            *self = Target::new(w, h);
        }
    }
}

/// A vertex in clip space with a display color (0..1 per channel).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Vertex {
    pub p: [f32; 4],
    pub c: [f32; 3],
}

/// How a line is drawn.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LineStyle {
    pub color: u32,
    /// Coverage: below 1 the line is blended over what is there.
    pub alpha: f32,
    /// Hidden where the depth buffer holds something nearer.
    pub depth_test: bool,
    /// Writes depth and id.
    pub write: bool,
    /// Relative depth tolerance of the test: positive draws lines lying on a surface over
    /// it, negative hides them there.
    pub bias: f32,
}

impl LineStyle {
    pub const fn overlay(color: u32) -> LineStyle {
        LineStyle { color, alpha: 1.0, depth_test: false, write: false, bias: 0.0 }
    }
}

/// A triangle set up in screen space.
#[derive(Clone, Copy)]
struct ScreenTri {
    x: [f32; 3],
    y: [f32; 3],
    z: [f32; 3],
    /// 1 / w per vertex.
    iw: [f32; 3],
    /// Vertex colors divided by w.
    cw: [[f32; 3]; 3],
    /// Twice the signed area (positive: the vertices are ordered so it is).
    area: f32,
    /// Constant color, used when `smooth` is false.
    flat: u32,
    smooth: bool,
    id: u32,
    /// Inclusive pixel bounds.
    x0: i32,
    x1: i32,
    y0: i32,
    y1: i32,
}

#[derive(Clone, Copy)]
struct ScreenLine {
    a: [f32; 3],
    b: [f32; 3],
    style: LineStyle,
    id: u32,
}

/// One thread's binned primitives.
#[derive(Default)]
struct Local {
    tris: Vec<ScreenTri>,
    lines: Vec<ScreenLine>,
    tri_bins: Vec<Vec<u32>>,
    line_bins: Vec<Vec<u32>>,
}

impl Local {
    fn reset(&mut self, tiles: usize) {
        self.tris.clear();
        self.lines.clear();
        for bins in [&mut self.tri_bins, &mut self.line_bins] {
            bins.resize_with(tiles, Vec::new);
            bins.iter_mut().for_each(Vec::clear);
        }
    }
}

/// Receives the primitives of a job.
pub struct Emitter<'a> {
    local: &'a mut Local,
    w: usize,
    h: usize,
    tiles_x: usize,
}

/// Signed distances of a clip-space point to the clip planes (inside where >= 0): near,
/// far, then the four sides of the guard band.
#[inline]
fn planes(p: &[f32; 4]) -> [f32; 6] {
    let [x, y, z, w] = *p;
    [w - z, z, GUARD * w + x, GUARD * w - x, GUARD * w + y, GUARD * w - y]
}

fn lerp_vertex(a: &Vertex, b: &Vertex, t: f32) -> Vertex {
    let l = |x: f32, y: f32| x + (y - x) * t;
    Vertex { p: std::array::from_fn(|i| l(a.p[i], b.p[i])), c: std::array::from_fn(|i| l(a.c[i], b.c[i])) }
}

#[inline]
pub fn pack(c: [f32; 3]) -> u32 {
    let ch = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u32;
    (ch(c[0]) << 16) | (ch(c[1]) << 8) | ch(c[2])
}

#[inline]
fn unpack(c: u32) -> [f32; 3] {
    [((c >> 16) & 255) as f32, ((c >> 8) & 255) as f32, (c & 255) as f32]
}

#[inline]
fn blend(dst: u32, src: u32, alpha: f32) -> u32 {
    let (d, s) = (unpack(dst), unpack(src));
    let m = |i: usize| (d[i] + (s[i] - d[i]) * alpha + 0.5) as u32;
    (m(0) << 16) | (m(1) << 8) | m(2)
}

impl Emitter<'_> {
    /// A triangle with per-vertex colors (`flat` = None) or one constant color.
    pub fn tri(&mut self, v: [Vertex; 3], id: u32, flat: Option<u32>) {
        let d = v.map(|v| planes(&v.p));
        let mut clip = false;
        for out in (0..6).map(|k| d.map(|d| d[k] < 0.0)) {
            if out.iter().all(|&o| o) {
                return;
            }
            clip |= out.iter().any(|&o| o);
        }
        if !clip {
            self.setup(&v, id, flat);
            return;
        }
        // Sutherland–Hodgman against every plane: at most 3 + 6 vertices.
        let mut poly: Vec<Vertex> = v.to_vec();
        for k in 0..6 {
            if poly.len() < 3 {
                return;
            }
            let mut out = Vec::with_capacity(poly.len() + 1);
            for i in 0..poly.len() {
                let (a, b) = (&poly[i], &poly[(i + 1) % poly.len()]);
                let (da, db) = (planes(&a.p)[k], planes(&b.p)[k]);
                if da >= 0.0 {
                    out.push(*a);
                }
                if (da >= 0.0) != (db >= 0.0) {
                    out.push(lerp_vertex(a, b, da / (da - db)));
                }
            }
            poly = out;
        }
        for i in 1..poly.len().saturating_sub(1) {
            self.setup(&[poly[0], poly[i], poly[i + 1]], id, flat);
        }
    }

    fn screen(&self, p: &[f32; 4]) -> [f32; 4] {
        let iw = 1.0 / p[3];
        let (w, h) = (self.w as f32, self.h as f32);
        [(p[0] * iw * 0.5 + 0.5) * w, (0.5 - p[1] * iw * 0.5) * h, p[2] * iw, iw]
    }

    fn setup(&mut self, v: &[Vertex; 3], id: u32, flat: Option<u32>) {
        let s = [self.screen(&v[0].p), self.screen(&v[1].p), self.screen(&v[2].p)];
        let area = (s[1][0] - s[0][0]) * (s[2][1] - s[0][1]) - (s[2][0] - s[0][0]) * (s[1][1] - s[0][1]);
        if area == 0.0 || !area.is_finite() {
            return;
        }
        // Two-sided: order the vertices so the area is positive.
        let order = if area > 0.0 { [0, 1, 2] } else { [0, 2, 1] };
        let (minx, maxx) = (s[0][0].min(s[1][0]).min(s[2][0]), s[0][0].max(s[1][0]).max(s[2][0]));
        let (miny, maxy) = (s[0][1].min(s[1][1]).min(s[2][1]), s[0][1].max(s[1][1]).max(s[2][1]));
        // Pixels whose centres the bounds contain.
        let x0 = ((minx - 0.5).ceil() as i32).max(0);
        let x1 = ((maxx - 0.5).floor() as i32).min(self.w as i32 - 1);
        let y0 = ((miny - 0.5).ceil() as i32).max(0);
        let y1 = ((maxy - 0.5).floor() as i32).min(self.h as i32 - 1);
        if x0 > x1 || y0 > y1 {
            return;
        }
        let pick = |f: &dyn Fn(usize) -> f32| order.map(f);
        let tri = ScreenTri {
            x: pick(&|i| s[i][0]),
            y: pick(&|i| s[i][1]),
            z: pick(&|i| s[i][2]),
            iw: pick(&|i| s[i][3]),
            cw: order.map(|i| v[i].c.map(|c| c * s[i][3])),
            area: area.abs(),
            flat: flat.unwrap_or(0),
            smooth: flat.is_none(),
            id,
            x0,
            x1,
            y0,
            y1,
        };
        let index = self.local.tris.len() as u32;
        self.local.tris.push(tri);
        let (tx0, tx1) = (x0 as usize / TILE_W, x1 as usize / TILE_W);
        for ty in y0 as usize / TILE_H..=y1 as usize / TILE_H {
            for tx in tx0..=tx1 {
                self.local.tri_bins[ty * self.tiles_x + tx].push(index);
            }
        }
    }

    /// A line between two clip-space points.
    pub fn line(&mut self, a: [f32; 4], b: [f32; 4], style: LineStyle, id: u32) {
        let (da, db) = (planes(&a), planes(&b));
        let (mut t0, mut t1) = (0.0f32, 1.0f32);
        for k in 0..6 {
            match (da[k] < 0.0, db[k] < 0.0) {
                (true, true) => return,
                (true, false) => t0 = t0.max(da[k] / (da[k] - db[k])),
                (false, true) => t1 = t1.min(da[k] / (da[k] - db[k])),
                (false, false) => {}
            }
        }
        if t0 > t1 {
            return;
        }
        let at = |t: f32| -> [f32; 4] { std::array::from_fn(|i| a[i] + (b[i] - a[i]) * t) };
        let (sa, sb) = (self.screen(&at(t0)), self.screen(&at(t1)));
        let (w, h) = (self.w as f32, self.h as f32);
        let (ya, yb) = (sa[1].min(sb[1]), sa[1].max(sb[1]));
        if yb < 0.0 || ya >= h || sa[0].max(sb[0]) < 0.0 || sa[0].min(sb[0]) >= w {
            return;
        }
        let index = self.local.lines.len() as u32;
        self.local.lines.push(ScreenLine { a: [sa[0], sa[1], sa[2]], b: [sb[0], sb[1], sb[2]], style, id });
        // The tiles along the line: per row of tiles, the columns its x spans there.
        let tile = |v: f32, size: usize, max: f32| (v.clamp(0.0, max - 1.0) as usize) / size;
        let (dx, dy) = (sb[0] - sa[0], sb[1] - sa[1]);
        for ty in tile(ya, TILE_H, h)..=tile(yb, TILE_H, h) {
            let (r0, r1) = ((ty * TILE_H) as f32 - 1.0, ((ty + 1) * TILE_H) as f32 + 1.0);
            let (xa, xb) = if dy.abs() > 1e-6 {
                let x_at = |y: f32| sa[0] + (y.clamp(ya, yb) - sa[1]) / dy * dx;
                (x_at(r0), x_at(r1))
            } else {
                (sa[0], sb[0])
            };
            let (xa, xb) = (xa.min(xb) - 1.0, xa.max(xb) + 1.0);
            for tx in tile(xa, TILE_W, w)..=tile(xb, TILE_W, w) {
                self.local.line_bins[ty * self.tiles_x + tx].push(index);
            }
        }
    }
}

/// One tile of the target: `TILE_W` x `TILE_H` pixels at (`x0`, `y0`), the part inside
/// the target `w` x `h`.
struct Tile<'a> {
    x0: usize,
    y0: usize,
    w: usize,
    h: usize,
    color: &'a mut [u32],
    depth: &'a mut [f32],
    id: &'a mut [u32],
    /// The farthest depth in the tile, recomputed after writes.
    far: f32,
    far_dirty: bool,
}

impl Tile<'_> {
    /// Last pixel column and row (inclusive) of the tile inside the target.
    fn ends(&self) -> (i32, i32) {
        (((self.x0 + TILE_W).min(self.w) - 1) as i32, ((self.y0 + TILE_H).min(self.h) - 1) as i32)
    }

    #[inline]
    fn index(&self, x: i32, y: i32) -> usize {
        (y as usize - self.y0) * TILE_W + (x as usize - self.x0)
    }

    /// The farthest depth in the tile (0 while any pixel is empty).
    fn far(&mut self) -> f32 {
        if self.far_dirty {
            self.far = self.depth.iter().fold(f32::INFINITY, |m, &d| m.min(d));
            self.far_dirty = false;
        }
        self.far
    }

    fn tri(&mut self, t: &ScreenTri) {
        let (xe, ye) = self.ends();
        let (xs, ys) = (self.x0 as i32, self.y0 as i32);
        let (y_lo, y_hi) = (t.y0.max(ys), t.y1.min(ye));
        let (bx_lo, bx_hi) = (t.x0.max(xs), t.x1.min(xe));
        // Hierarchical depth: a large triangle behind everything already in the tile is
        // skipped whole (small ones are cheaper to just draw).
        let covered = (y_hi - y_lo + 1) * (bx_hi - bx_lo + 1);
        if covered >= 32 && t.z[0].max(t.z[1]).max(t.z[2]) <= self.far() {
            return;
        }
        let inv_area = 1.0 / t.area;
        // Edge i is opposite vertex i; its function is positive inside and changes by
        // -dy per pixel to the right.
        let edge = |i: usize| {
            let (a, b) = ((i + 1) % 3, (i + 2) % 3);
            (t.x[a], t.y[a], t.x[b] - t.x[a], t.y[b] - t.y[a])
        };
        let edges = [edge(0), edge(1), edge(2)];
        // Depth and the colors over w are affine in screen x: their steps per pixel.
        let step = |v: [f32; 3]| -(edges[0].3 * v[0] + edges[1].3 * v[1] + edges[2].3 * v[2]) * inv_area;
        let dz = step(t.z);
        let mut wrote = false;
        for py in y_lo..=y_hi {
            let cy = py as f32 + 0.5;
            // The span of x where every edge function is >= 0, widened by a pixel and
            // checked per pixel below.
            let (mut lo, mut hi) = (bx_lo as f32, bx_hi as f32);
            for &(ax, ay, dx, dy) in &edges {
                // e(x) = dx (cy - ay) - dy (x + 0.5 - ax)
                let e0 = dx * (cy - ay) + dy * ax;
                if dy > 0.0 {
                    hi = hi.min(e0 / dy - 0.5 + 1.0);
                } else if dy < 0.0 {
                    lo = lo.max(e0 / dy - 0.5 - 1.0);
                } else if e0 < 0.0 {
                    lo = f32::INFINITY;
                }
            }
            if lo > hi {
                continue;
            }
            let (x_lo, x_hi) = ((lo.ceil() as i32).max(bx_lo), (hi.floor() as i32).min(bx_hi));
            if x_lo > x_hi {
                continue;
            }
            let cx = x_lo as f32 + 0.5;
            let mut w: [f32; 3] = std::array::from_fn(|i| {
                let (ax, ay, dx, dy) = edges[i];
                dx * (cy - ay) - dy * (cx - ax)
            });
            let mut z = (w[0] * t.z[0] + w[1] * t.z[1] + w[2] * t.z[2]) * inv_area;
            let row = (py as usize - self.y0) * TILE_W;
            for px in x_lo..=x_hi {
                let i = row + (px as usize - self.x0);
                if w[0] >= 0.0 && w[1] >= 0.0 && w[2] >= 0.0 && z > self.depth[i] {
                    self.depth[i] = z;
                    self.id[i] = t.id;
                    self.color[i] = if t.smooth {
                        let b = w.map(|w| w * inv_area);
                        let k = 1.0 / (b[0] * t.iw[0] + b[1] * t.iw[1] + b[2] * t.iw[2]);
                        pack(std::array::from_fn(|c| (b[0] * t.cw[0][c] + b[1] * t.cw[1][c] + b[2] * t.cw[2][c]) * k))
                    } else {
                        t.flat
                    };
                    wrote = true;
                }
                for k in 0..3 {
                    w[k] -= edges[k].3;
                }
                z += dz;
            }
        }
        self.far_dirty |= wrote;
    }

    #[inline]
    fn plot(&mut self, px: i32, py: i32, z: f32, l: &ScreenLine) {
        let (xe, ye) = self.ends();
        if px < self.x0 as i32 || py < self.y0 as i32 || px > xe || py > ye {
            return;
        }
        let i = self.index(px, py);
        let s = &l.style;
        if s.depth_test && z * (1.0 + s.bias) < self.depth[i] {
            return;
        }
        self.color[i] = if s.alpha >= 1.0 { s.color } else { blend(self.color[i], s.color, s.alpha) };
        if s.write {
            self.depth[i] = z;
            self.id[i] = l.id;
            self.far_dirty = true;
        }
    }

    fn line(&mut self, l: &ScreenLine) {
        let ([x0, y0, z0], [x1, y1, z1]) = (l.a, l.b);
        let (dx, dy) = (x1 - x0, y1 - y0);
        let (xe, ye) = self.ends();
        let (left, right) = (self.x0 as f32, xe as f32 + 1.0);
        let (top, bottom) = (self.y0 as f32, ye as f32 + 1.0);
        if dx.abs() >= dy.abs() {
            if dx.abs() < 1e-6 {
                self.plot(x0.floor() as i32, y0.floor() as i32, z0.max(z1), l);
                return;
            }
            // Pixel columns whose centre the segment passes, limited to this tile.
            let (mut lo, mut hi) = (x0.min(x1).max(left), x0.max(x1).min(right));
            if dy.abs() > 1e-6 {
                let (ta, tb) = ((top - 1.0 - y0) / dy, (bottom + 1.0 - y0) / dy);
                let (xa, xb) = (x0 + ta * dx, x0 + tb * dx);
                lo = lo.max(xa.min(xb));
                hi = hi.min(xa.max(xb));
            }
            for px in (lo - 0.5).ceil() as i32..=(hi - 0.5).floor() as i32 {
                let t = (px as f32 + 0.5 - x0) / dx;
                self.plot(px, (y0 + t * dy).floor() as i32, z0 + t * (z1 - z0), l);
            }
        } else {
            let (mut lo, mut hi) = (y0.min(y1).max(top), y0.max(y1).min(bottom));
            if dx.abs() > 1e-6 {
                let (ta, tb) = ((left - 1.0 - x0) / dx, (right + 1.0 - x0) / dx);
                let (ya, yb) = (y0 + ta * dy, y0 + tb * dy);
                lo = lo.max(ya.min(yb));
                hi = hi.min(ya.max(yb));
            }
            for py in (lo - 0.5).ceil() as i32..=(hi - 0.5).floor() as i32 {
                let t = (py as f32 + 0.5 - y0) / dy;
                self.plot((x0 + t * dx).floor() as i32, py, z0 + t * (z1 - z0), l);
            }
        }
    }
}

/// Jobs a pass runs: `jobs` calls of `run(job index, emitter)`, spread over the threads.
pub struct Pass<'a> {
    pub jobs: usize,
    pub run: &'a (dyn Fn(usize, &mut Emitter<'_>) + Sync),
}

pub struct Raster {
    locals: Vec<RwLock<Local>>,
    /// Tile-major color, depth and id buffers.
    color: Vec<u32>,
    depth: Vec<f32>,
    id: Vec<u32>,
}

/// Rows `TILE_H` high of the target: color, depth, id.
type Band<'a> = (&'a mut [u32], &'a mut [f32], &'a mut [u32]);

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.try_lock().map_err(|_| ()).expect("each tile is taken by one thread at a time")
}

impl Raster {
    pub fn new(threads: usize) -> Raster {
        Raster {
            locals: (0..threads.max(1)).map(|_| RwLock::default()).collect(),
            color: Vec::new(),
            depth: Vec::new(),
            id: Vec::new(),
        }
    }

    pub fn with_all_cores() -> Raster {
        Raster::new(std::thread::available_parallelism().map_or(4, usize::from))
    }

    /// Clears `target` to `clear` (depth 0, id 0) and draws `passes` in order.
    pub fn draw(&mut self, target: &mut Target, clear: u32, passes: &[Pass<'_>]) {
        let (w, h) = (target.w, target.h);
        if w == 0 || h == 0 {
            return;
        }
        let (tiles_x, tiles_y) = (w.div_ceil(TILE_W), h.div_ceil(TILE_H));
        let n = tiles_x * tiles_y;
        self.color.resize(n * TILE_PX, 0);
        self.depth.resize(n * TILE_PX, 0.0);
        self.id.resize(n * TILE_PX, 0);
        let tiles: Vec<Mutex<Tile<'_>>> = self
            .color
            .chunks_mut(TILE_PX)
            .zip(self.depth.chunks_mut(TILE_PX))
            .zip(self.id.chunks_mut(TILE_PX))
            .take(n)
            .enumerate()
            .map(|(i, ((color, depth), id))| {
                let (x0, y0) = ((i % tiles_x) * TILE_W, (i / tiles_x) * TILE_H);
                Mutex::new(Tile { x0, y0, w, h, color, depth, id, far: 0.0, far_dirty: false })
            })
            .collect();
        // Rows of tiles in the target, for the final copy.
        let bands: Vec<Mutex<Band<'_>>> = target
            .color
            .chunks_mut(w * TILE_H)
            .zip(target.depth.chunks_mut(w * TILE_H))
            .zip(target.id.chunks_mut(w * TILE_H))
            .map(|((c, d), i)| Mutex::new((c, d, i)))
            .collect();
        // Few tiles: no point waking every core.
        let threads = self.locals.len().min(n.div_ceil(4).max(1));
        let locals = &self.locals[..threads];
        let counters: Vec<AtomicUsize> = (0..2 + 2 * passes.len()).map(|_| AtomicUsize::new(0)).collect();
        let barrier = Barrier::new(threads);
        let work = |me: usize| {
            let next = |c: &AtomicUsize| c.fetch_add(1, Ordering::Relaxed);
            loop {
                let s = next(&counters[0]);
                if s >= n {
                    break;
                }
                let mut t = lock(&tiles[s]);
                t.color.fill(clear);
                t.depth.fill(0.0);
                t.id.fill(0);
                // Pixels past the target's edge never get drawn: they must not hold the
                // tile's farthest depth at 0.
                let (xe, ye) = t.ends();
                for y in 0..TILE_H {
                    for x in 0..TILE_W {
                        if (t.x0 + x) as i32 > xe || (t.y0 + y) as i32 > ye {
                            t.depth[y * TILE_W + x] = f32::INFINITY;
                        }
                    }
                }
                (t.far, t.far_dirty) = (0.0, true);
            }
            for (k, pass) in passes.iter().enumerate() {
                {
                    let mut local = locals[me].write().expect("no thread panics while holding it");
                    local.reset(n);
                    let mut em = Emitter { local: &mut local, w, h, tiles_x };
                    loop {
                        let j = next(&counters[2 + 2 * k]);
                        if j >= pass.jobs {
                            break;
                        }
                        (pass.run)(j, &mut em);
                    }
                }
                barrier.wait();
                let all: Vec<_> = locals.iter().map(|l| l.read().expect("no thread panics while holding it")).collect();
                loop {
                    let s = next(&counters[3 + 2 * k]);
                    if s >= n {
                        break;
                    }
                    if all.iter().all(|l| l.tri_bins[s].is_empty() && l.line_bins[s].is_empty()) {
                        continue;
                    }
                    let mut t = lock(&tiles[s]);
                    for l in &all {
                        for &i in &l.tri_bins[s] {
                            t.tri(&l.tris[i as usize]);
                        }
                        for &i in &l.line_bins[s] {
                            t.line(&l.lines[i as usize]);
                        }
                    }
                }
                drop(all);
                barrier.wait();
            }
            // Copy each band of tiles into the row-major target.
            loop {
                let ty = next(&counters[1]);
                if ty >= tiles_y {
                    break;
                }
                let mut band = lock(&bands[ty]);
                let (color, depth, id) = &mut *band;
                for tx in 0..tiles_x {
                    let t = lock(&tiles[ty * tiles_x + tx]);
                    let (x0, cols) = (tx * TILE_W, TILE_W.min(w - tx * TILE_W));
                    for r in 0..color.len() / w {
                        let (src, dst) = (r * TILE_W, r * w + x0);
                        color[dst..dst + cols].copy_from_slice(&t.color[src..src + cols]);
                        depth[dst..dst + cols].copy_from_slice(&t.depth[src..src + cols]);
                        id[dst..dst + cols].copy_from_slice(&t.id[src..src + cols]);
                    }
                }
            }
        };
        if threads == 1 {
            work(0);
        } else {
            std::thread::scope(|scope| {
                for me in 1..threads {
                    let work = &work;
                    scope.spawn(move || work(me));
                }
                work(0);
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(x: f32, y: f32, z: f32, w: f32, c: [f32; 3]) -> Vertex {
        Vertex { p: [x * w, y * w, z * w, w], c }
    }

    fn draw(t: &mut Target, threads: usize, f: &(dyn Fn(usize, &mut Emitter<'_>) + Sync)) {
        Raster::new(threads).draw(t, 0x10_10_10, &[Pass { jobs: 1, run: f }]);
    }

    #[test]
    fn nearer_triangle_wins_regardless_of_order() {
        for threads in [1, 3] {
            let mut t = Target::new(64, 40);
            draw(&mut t, threads, &|_, e: &mut Emitter<'_>| {
                let quad = |e: &mut Emitter<'_>, z: f32, id: u32| {
                    let c = [0.0; 3];
                    e.tri([v(-1.0, -1.0, z, 1.0, c), v(1.0, -1.0, z, 1.0, c), v(1.0, 1.0, z, 1.0, c)], id, Some(id));
                    e.tri([v(-1.0, -1.0, z, 1.0, c), v(1.0, 1.0, z, 1.0, c), v(-1.0, 1.0, z, 1.0, c)], id, Some(id));
                };
                quad(e, 0.5, 1);
                quad(e, 0.8, 2);
                quad(e, 0.2, 3);
            });
            assert!(t.id.iter().all(|&i| i == 2), "a full-screen quad covers every pixel exactly");
            assert!(t.color.iter().all(|&c| c == 2));
        }
    }

    /// Two triangles sharing an edge leave no gap and cover each pixel once.
    #[test]
    fn shared_edges_are_watertight() {
        let mut t = Target::new(97, 61);
        draw(&mut t, 2, &|_, e: &mut Emitter<'_>| {
            let c = [1.0; 3];
            let (a, b, cc, d) = (
                v(-0.93, -0.71, 0.5, 1.0, c),
                v(0.87, -0.9, 0.5, 1.0, c),
                v(0.77, 0.95, 0.5, 1.0, c),
                v(-0.6, 0.8, 0.5, 1.0, c),
            );
            e.tri([a, b, cc], 1, Some(1));
            e.tri([a, cc, d], 2, Some(2));
        });
        // Every pixel inside the quad (sampled along the horizontal centre line) is covered.
        let row = t.h / 2;
        let covered: Vec<bool> = (0..t.w).map(|x| t.id[row * t.w + x] != 0).collect();
        let first = covered.iter().position(|&c| c).unwrap();
        let last = covered.iter().rposition(|&c| c).unwrap();
        assert!(covered[first..=last].iter().all(|&c| c), "gap in {covered:?}");
    }

    /// A triangle reaching behind the camera is clipped at the near plane, not mirrored.
    #[test]
    fn near_plane_clipping() {
        let mut t = Target::new(40, 40);
        draw(&mut t, 1, &|_, e: &mut Emitter<'_>| {
            let c = [1.0; 3];
            // Perspective: z_clip = near (0.1), w = distance. A floor triangle one unit
            // below the eye, from distance 2 to behind the eye.
            let p = |x: f32, d: f32| Vertex { p: [x, -1.0, 0.1, d], c };
            e.tri([p(-1.0, 2.0), p(1.0, 2.0), p(0.0, -1.0)], 7, Some(7));
        });
        let ids: usize = t.id.iter().filter(|&&i| i == 7).count();
        assert!(ids > 0, "the part in front of the eye is drawn");
        // It lies below its far edge (screen y = 30); unclipped, the vertex behind the eye
        // would project to the top of the view.
        assert!(t.id[..40 * 29].iter().all(|&i| i == 0));
    }

    /// Colors interpolate perspective-correctly: halfway along a receding edge in screen
    /// space is nearer than halfway in the attribute.
    #[test]
    fn perspective_correct_colors() {
        let mut t = Target::new(101, 3);
        draw(&mut t, 1, &|_, e: &mut Emitter<'_>| {
            // A horizontal strip from distance 1 (left, black) to 3 (right, white).
            let near = 0.01;
            let p = |x: f32, y: f32, d: f32, c: f32| Vertex { p: [x, y, near, d], c: [c; 3] };
            let (a, b, c, d) =
                (p(-1.0, -1.0, 1.0, 0.0), p(3.0, -3.0, 3.0, 1.0), p(3.0, 3.0, 3.0, 1.0), p(-1.0, 1.0, 1.0, 0.0));
            e.tri([a, b, c], 1, None);
            e.tri([a, c, d], 1, None);
        });
        let mid = (t.color[t.w + 50] & 255) as f32 / 255.0;
        // Screen x halfway between the edges: 1/w = (1 + 1/3) / 2 = 2/3, so the attribute
        // is (0/1 + 1/3) / 2 / (2/3) = 0.25.
        assert!((mid - 0.25).abs() < 0.03, "{mid}");
    }

    #[test]
    fn lines_are_depth_tested_and_clipped() {
        let mut t = Target::new(50, 50);
        Raster::new(2).draw(
            &mut t,
            0,
            &[
                Pass {
                    jobs: 1,
                    run: &|_, e: &mut Emitter<'_>| {
                        let c = [0.0; 3];
                        e.tri(
                            [v(-1.0, -1.0, 0.5, 1.0, c), v(0.0, -1.0, 0.5, 1.0, c), v(0.0, 1.0, 0.5, 1.0, c)],
                            1,
                            Some(0x0011_1111),
                        );
                        e.tri(
                            [v(-1.0, -1.0, 0.5, 1.0, c), v(0.0, 1.0, 0.5, 1.0, c), v(-1.0, 1.0, 0.5, 1.0, c)],
                            1,
                            Some(0x0011_1111),
                        );
                    },
                },
                Pass {
                    jobs: 1,
                    run: &|_, e: &mut Emitter<'_>| {
                        let style =
                            LineStyle { color: 0x00ff_0000, alpha: 1.0, depth_test: true, write: false, bias: 0.0 };
                        // Behind the left half; endpoints far outside the view.
                        e.line([-30.0, 0.0, 0.25, 1.0], [30.0, 0.0, 0.25, 1.0], style, 0);
                    },
                },
            ],
        );
        let row = &t.color[25 * 50..26 * 50];
        assert!(row[..24].iter().all(|&c| c == 0x0011_1111), "hidden behind the surface");
        assert!(row[26..].iter().all(|&c| c == 0x00ff_0000), "drawn across the empty half");
    }
}
