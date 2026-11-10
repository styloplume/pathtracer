//! Progressive, multi-threaded image rendering.
//!
//! Rendering proceeds in two phases:
//!
//! 1. **Coarse-to-fine preview.** With a block size `B` starting at `initial_block`
//!    (a power of two), one sample is traced for every pixel whose coordinates are
//!    multiples of `B`; on display that "anchor" pixel's color fills its whole `BxB`
//!    block. `B` is then halved, and only anchors not already traced at a coarser
//!    level are traced. When `B` reaches 1, every pixel holds exactly one sample, so
//!    no work is wasted on the preview.
//! 2. **Refinement.** Whole-image passes add one sample per pixel until a stop
//!    criterion is met: the sample budget, a time limit, or (with adaptive sampling)
//!    every pixel's noise estimate dropping below the threshold. Adaptive sampling
//!    skips converged pixels, so later passes concentrate on the noisy ones.
//!
//! Progress is reported through [`Progress`]: per finished row (so long passes can
//! be displayed while they run) and per finished pass.

use crate::camera::{Camera, CameraDesc};
use crate::filter::Filter;
use crate::guiding::Guide;
use crate::image::{Encoder, pack_unorm};
use crate::integrator::{PathSample, radiance};
use crate::math::Vec3;
use crate::sampler::Sampler;
use crate::scene::Scene;
use std::ops::Range;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Per-pixel accumulator: radiance, luminance second moment, and AOV sums.
#[derive(Clone, Copy, Debug, Default)]
pub struct Pixel {
    pub sum: Vec3,
    /// Sum of squared sample luminances (for the variance estimate).
    pub lum_sq: f32,
    pub n: u32,
    pub albedo: Vec3,
    pub normal: Vec3,
    pub depth: f32,
}

/// Brightness floor of the relative error metric, so near-black pixels converge.
const ERROR_FLOOR: f32 = 1e-3;

impl Pixel {
    #[inline]
    pub fn add(&mut self, s: &PathSample) {
        let l = s.radiance.luminance();
        self.sum += s.radiance;
        self.lum_sq += l * l;
        self.n += 1;
        self.albedo += s.albedo;
        self.normal += s.normal;
        self.depth += s.depth;
    }

    #[inline]
    fn inv_n(&self) -> f32 {
        if self.n > 0 { 1.0 / self.n as f32 } else { 0.0 }
    }

    #[inline]
    pub fn mean(&self) -> Vec3 {
        self.sum * self.inv_n()
    }

    #[inline]
    pub fn albedo(&self) -> Vec3 {
        self.albedo * self.inv_n()
    }

    /// Average shading normal, renormalized (zero for background pixels).
    #[inline]
    pub fn normal(&self) -> Vec3 {
        let len = self.normal.length();
        if len > 1e-6 * self.n as f32 { self.normal * (1.0 / len) } else { Vec3::ZERO }
    }

    #[inline]
    pub fn depth(&self) -> f32 {
        self.depth * self.inv_n()
    }

    /// Estimated variance of the mean luminance. With fewer than two samples the
    /// squared mean stands in (i.e. 100% relative error).
    #[inline]
    pub fn variance(&self) -> f32 {
        let n = self.n as f32;
        let mean = self.sum.luminance() * self.inv_n();
        if self.n < 2 {
            return mean * mean;
        }
        let sample_var = ((self.lum_sq - n * mean * mean) / (n - 1.0)).max(0.0);
        sample_var / n
    }

    /// Perceptual noise estimate: standard error of the mean luminance relative to
    /// the square root of the luminance (roughly the noise after display encoding).
    #[inline]
    pub fn error(&self) -> f32 {
        if self.n < 2 {
            return f32::INFINITY;
        }
        let mean = self.sum.luminance() * self.inv_n();
        self.variance().sqrt() / mean.max(ERROR_FLOOR).sqrt()
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Stop {
    Running,
    /// Reached the samples-per-pixel budget.
    Samples,
    TimeLimit,
    /// Adaptive sampling: every pixel is below the noise threshold.
    Converged,
}

impl Stop {
    pub fn describe(self) -> &'static str {
        match self {
            Stop::Running => "running",
            Stop::Samples => "done",
            Stop::TimeLimit => "time limit",
            Stop::Converged => "converged",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct PassInfo {
    /// Current block size (1 once full resolution has been reached).
    pub block: u32,
    /// Maximum samples per pixel once this pass completes (1 during the preview phase).
    pub spp: u32,
    pub target_spp: u32,
    /// True for the last pass.
    pub finished: bool,
    pub stop: Stop,
    /// Fraction of pixels still being sampled (1 without adaptive sampling).
    pub active: f32,
    /// Mean per-pixel noise estimate (see [`Pixel::error`]); NaN until known.
    pub error: f32,
}

/// Receives render progress and controls cancellation / pausing. Row callbacks are
/// invoked concurrently from worker threads.
pub trait Progress: Sync {
    fn cancelled(&self) -> bool {
        false
    }
    /// While true, workers idle between rows.
    fn paused(&self) -> bool {
        false
    }
    /// Render time so far for the time limit (excluding pauses); `None` uses wall time.
    fn render_time(&self) -> Option<Duration> {
        None
    }
    fn begin_pass(&self, _info: &PassInfo) {}
    /// Row `y` has been updated; `samples` is the number of samples just traced in it.
    fn row_done(&self, _y: usize, _row: &[Pixel], _samples: u64) {}
    fn end_pass(&self, _pixels: &[Pixel], _info: &PassInfo) {}
}

pub fn thread_count(scene: &Scene) -> usize {
    match scene.settings.threads {
        0 => std::thread::available_parallelism().map_or(4, std::num::NonZero::get),
        n => n,
    }
}

struct Ctx<'a> {
    scene: &'a Scene,
    camera: Camera,
    filter: Filter,
    w: usize,
    h: usize,
    threads: usize,
}

/// Renders `w`x`h` progressively from `camera`. Returns the accumulated pixels, or
/// `None` if cancelled.
pub fn render_progressive(
    scene: &Scene,
    camera: &CameraDesc,
    w: usize,
    h: usize,
    progress: &dyn Progress,
) -> Option<Vec<Pixel>> {
    let st = &scene.settings;
    let ctx = Ctx {
        scene,
        camera: Camera::new(camera, w as f32 / h as f32),
        filter: Filter::new(st.filter, st.filter_radius),
        w,
        h,
        threads: thread_count(scene),
    };
    let target = st.spp.max(1);
    let start = Instant::now();
    let time_up = || {
        st.time_limit > 0.0 && progress.render_time().unwrap_or_else(|| start.elapsed()).as_secs_f32() >= st.time_limit
    };
    let mut pixels = vec![Pixel::default(); w * h];
    let mut block = (st.initial_block.max(1) as usize).next_power_of_two();
    let mut info = PassInfo {
        block: block as u32,
        spp: 1,
        target_spp: target,
        finished: false,
        stop: Stop::Running,
        active: 1.0,
        error: f32::NAN,
    };

    // Path guiding trains in iterations of 1, 2, 4, ... samples per pixel (the preview
    // counts as the first), then keeps its last distribution.
    let mut guide = st.guiding.then(|| Guide::new(&scene.bounds()));
    let end_guiding_iteration = |guide: &mut Option<Guide>, done: u32| {
        if let Some(g) = guide
            && g.recording()
            && (done + 1).is_power_of_two()
        {
            g.end_iteration(done.div_ceil(2));
            if done + 1 >= st.guiding_spp {
                g.stop_recording();
            }
        }
    };

    let mut first = true;
    loop {
        info.block = block as u32;
        progress.begin_pass(&info);
        run_pass(&ctx, &mut pixels, Some((block, first)), 0, None, guide.as_ref(), progress);
        if progress.cancelled() {
            return None;
        }
        if block == 1 {
            info.stop = if target == 1 {
                Stop::Samples
            } else if time_up() {
                Stop::TimeLimit
            } else {
                Stop::Running
            };
            info.finished = info.stop != Stop::Running;
        }
        progress.end_pass(&pixels, &info);
        if block == 1 {
            end_guiding_iteration(&mut guide, 1);
            break;
        }
        block /= 2;
        first = false;
    }

    let threshold = st.noise_threshold;
    let min_spp = st.min_spp.max(2);
    let mut mask: Option<Vec<bool>> = None;
    let mut sample = 1;
    while !info.finished {
        info.spp = sample + 1;
        progress.begin_pass(&info);
        run_pass(&ctx, &mut pixels, None, sample, mask.as_deref(), guide.as_ref(), progress);
        if progress.cancelled() {
            return None;
        }
        end_guiding_iteration(&mut guide, sample + 1);
        let errors: Vec<f32> = pixels.iter().map(Pixel::error).collect();
        info.error = errors.iter().map(|e| e.min(1.0)).sum::<f32>() / errors.len().max(1) as f32;
        if threshold > 0.0 && info.spp >= min_spp {
            let m = active_mask(&errors, w, h, threshold);
            info.active = m.iter().filter(|&&a| a).count() as f32 / m.len().max(1) as f32;
            mask = Some(m);
        }
        info.stop = if info.active == 0.0 {
            Stop::Converged
        } else if info.spp >= target {
            Stop::Samples
        } else if time_up() {
            Stop::TimeLimit
        } else {
            Stop::Running
        };
        info.finished = info.stop != Stop::Running;
        progress.end_pass(&pixels, &info);
        sample += 1;
    }
    Some(pixels)
}

/// Pixels that still need samples: those above the threshold, dilated by one pixel
/// so that an isolated noisy pixel keeps its neighbours sampling too.
fn active_mask(errors: &[f32], w: usize, h: usize, threshold: f32) -> Vec<bool> {
    let noisy: Vec<bool> = errors.iter().map(|&e| e.is_nan() || e > threshold).collect();
    let mut horiz = vec![false; w * h];
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            horiz[i] = noisy[i] || (x > 0 && noisy[i - 1]) || (x + 1 < w && noisy[i + 1]);
        }
    }
    (0..w * h).map(|i| horiz[i] || (i >= w && horiz[i - w]) || (i + w < w * h && horiz[i + w])).collect()
}

/// `level = Some((block, is_first_level))` for preview passes, `None` for a full pass.
fn run_pass(
    ctx: &Ctx<'_>,
    pixels: &mut [Pixel],
    level: Option<(usize, bool)>,
    sample: u32,
    mask: Option<&[bool]>,
    guide: Option<&Guide>,
    progress: &dyn Progress,
) {
    let w = ctx.w;
    let rows = Mutex::new(pixels.chunks_mut(w).enumerate());
    let step = level.map_or(1, |(b, _)| b);
    std::thread::scope(|s| {
        for _ in 0..ctx.threads {
            s.spawn(|| {
                loop {
                    while progress.paused() && !progress.cancelled() {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    if progress.cancelled() {
                        return;
                    }
                    let next = rows.lock().unwrap().find(|(y, _)| y % step == 0);
                    let Some((y, row)) = next else { return };
                    let row_mask = mask.map(|m| &m[y * w..(y + 1) * w]);
                    let mut traced = 0u64;
                    for x in (0..w).step_by(step) {
                        // Skip anchors already traced at a coarser level.
                        if let Some((b, false)) = level
                            && x % (2 * b) == 0
                            && y % (2 * b) == 0
                        {
                            continue;
                        }
                        if row_mask.is_some_and(|m| !m[x]) {
                            continue;
                        }
                        row[x].add(&trace_pixel(ctx, x, y, sample, guide));
                        traced += 1;
                    }
                    if traced > 0 {
                        progress.row_done(y, row, traced);
                    }
                }
            });
        }
    });
}

#[inline]
fn trace_pixel(ctx: &Ctx<'_>, x: usize, y: usize, sample: u32, guide: Option<&Guide>) -> PathSample {
    let scene = ctx.scene;
    let st = &scene.settings;
    let mut smp = Sampler::new(st.sampler, x as u32, y as u32, ctx.w as u32, sample, st.seed);
    // Filter importance sampling: the offset from the pixel center follows the filter.
    let (u1, u2) = smp.get2d();
    let s = (x as f32 + 0.5 + ctx.filter.sample(u1)) / ctx.w as f32;
    let t = (y as f32 + 0.5 + ctx.filter.sample(u2)) / ctx.h as f32;
    // Outside a fisheye's image circle: black.
    let Some(ray) = ctx.camera.ray(s, t, &mut smp) else { return PathSample::default() };
    let mut ps = radiance(scene, ray, &mut smp, guide);
    if !ps.radiance.is_finite() {
        ps.radiance = Vec3::ZERO;
    }
    let clamp = scene.settings.clamp;
    if clamp > 0.0 {
        let lum = ps.radiance.luminance();
        if lum > clamp {
            ps.radiance *= clamp / lum;
        }
    }
    ps
}

// ------------------------------------------------------------------ display

/// What the display shows of the accumulation buffer.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum View {
    Color,
    Albedo,
    Normal,
    Depth,
    /// Per-pixel noise estimate as a heat map.
    Error,
    /// Samples per pixel as a heat map.
    Samples,
}

impl View {
    pub fn next(self) -> View {
        match self {
            View::Color => View::Albedo,
            View::Albedo => View::Normal,
            View::Normal => View::Depth,
            View::Depth => View::Error,
            View::Error => View::Samples,
            View::Samples => View::Color,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            View::Color => "color",
            View::Albedo => "albedo",
            View::Normal => "normal",
            View::Depth => "depth",
            View::Error => "noise",
            View::Samples => "samples",
        }
    }
}

/// Blue → cyan → green → yellow → red for t in [0, 1].
pub fn heatmap(t: f32) -> Vec3 {
    const STOPS: [Vec3; 5] = [
        Vec3::new(0.05, 0.05, 0.35),
        Vec3::new(0.1, 0.45, 0.95),
        Vec3::new(0.15, 0.8, 0.35),
        Vec3::new(1.0, 0.85, 0.1),
        Vec3::new(0.9, 0.1, 0.05),
    ];
    let f = if t.is_nan() { 1.0 } else { t.clamp(0.0, 1.0) } * (STOPS.len() - 1) as f32;
    let i = (f as usize).min(STOPS.len() - 2);
    STOPS[i].lerp(STOPS[i + 1], f - i as f32)
}

/// Maps accumulator pixels to packed display colors for a [`View`].
pub struct Shader<'a> {
    pub enc: &'a Encoder,
    pub view: View,
    /// Depth mapped to black (distance to the far end of the scene).
    pub depth_scale: f32,
    /// Noise level shown mid-scale in the noise view (the adaptive threshold).
    pub error_scale: f32,
    /// Sample count shown at the top of the samples heat map.
    pub max_spp: u32,
}

impl Shader<'_> {
    #[inline]
    pub fn shade(&self, p: &Pixel) -> u32 {
        if p.n == 0 {
            return 0;
        }
        match self.view {
            View::Color => self.enc.encode(p.mean()),
            View::Albedo => self.enc.encode_srgb(p.albedo()),
            View::Normal => pack_unorm(p.normal() * 0.5 + Vec3::splat(0.5)),
            View::Depth => {
                let d = p.depth();
                if d <= 0.0 { 0 } else { pack_unorm(Vec3::splat(1.0 - 0.9 * (d / self.depth_scale).min(1.0))) }
            }
            View::Error => pack_unorm(heatmap(p.error() / (2.0 * self.error_scale))),
            View::Samples => pack_unorm(heatmap(p.n as f32 / self.max_spp.max(1) as f32)),
        }
    }
}

/// Largest block size the display fallback will look at.
const MAX_BLOCK: usize = 1 << 12;

/// The anchor pixel that colors (x, y) at block size `block`. If that anchor has not
/// been traced yet (a preview pass in progress), falls back to coarser anchors.
#[inline]
fn anchor(pixels: &[Pixel], w: usize, x: usize, y: usize, block: usize) -> &Pixel {
    let mut b = block.max(1);
    loop {
        let p = &pixels[(y / b * b) * w + x / b * b];
        if p.n > 0 || b >= MAX_BLOCK {
            return p;
        }
        b *= 2;
    }
}

/// Re-resolves the display rows affected by changes to accumulation rows `rows`,
/// writing packed 0x00RRGGBB pixels into `out` (`w`x`h`). Each anchor pixel fills
/// its `block`x`block` square.
pub fn resolve_rows(
    pixels: &[Pixel],
    w: usize,
    h: usize,
    block: usize,
    rows: Range<usize>,
    shader: &Shader<'_>,
    out: &mut [u32],
) {
    let block = block.max(1);
    let start = rows.start / block * block;
    let mut anchor_row = vec![0u32; w];
    for ay in (start..rows.end.min(h)).step_by(block) {
        for ax in (0..w).step_by(block) {
            let end = (ax + block).min(w);
            anchor_row[ax..end].fill(shader.shade(anchor(pixels, w, ax, ay, block)));
        }
        for y in ay..(ay + block).min(h) {
            out[y * w..(y + 1) * w].copy_from_slice(&anchor_row);
        }
    }
}

/// Resolves the whole accumulation buffer to display pixels.
#[cfg(test)]
pub fn resolve_display(pixels: &[Pixel], w: usize, h: usize, block: usize, shader: &Shader<'_>) -> Vec<u32> {
    let mut out = vec![0u32; w * h];
    resolve_rows(pixels, w, h, block, 0..h, shader, &mut out);
    out
}

/// Per-pixel values of the accumulation buffer (with the preview's anchor fallback).
pub fn resolve_values(pixels: &[Pixel], w: usize, h: usize, block: usize, f: impl Fn(&Pixel) -> Vec3) -> Vec<Vec3> {
    (0..w * h).map(|i| f(anchor(pixels, w, i % w, i / w, block))).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image::Tonemap;

    fn px(v: f32) -> Pixel {
        let mut p = Pixel::default();
        p.add(&PathSample { radiance: Vec3::splat(v), ..Default::default() });
        p
    }

    #[test]
    fn resolve_falls_back_to_coarser_anchors() {
        // 4x4 image: only the 4-anchor (0,0) and the 2-anchor (2,0) have samples.
        let (w, h) = (4, 4);
        let mut pixels = vec![Pixel::default(); w * h];
        pixels[0] = px(1.0);
        pixels[2] = px(0.0);
        let enc = Encoder::new(0.0, Tonemap::None);
        let shader = Shader { enc: &enc, view: View::Color, depth_scale: 1.0, error_scale: 0.02, max_spp: 1 };
        let out = resolve_display(&pixels, w, h, 2, &shader);
        let white = enc.encode(Vec3::splat(1.0));
        let black = enc.encode(Vec3::ZERO);
        assert_eq!(out[0], white);
        assert_eq!(out[3], black); // traced 2-anchor at (2,0)
        assert_eq!(out[2 * w], white); // untraced (0,2) -> falls back to (0,0)
        assert_eq!(out[2 * w + 2], white); // untraced (2,2) -> falls back to (0,0)
    }

    #[test]
    fn variance_estimate() {
        let mut p = Pixel::default();
        // Alternating 0 / 2 luminance, compared against the two-pass formula.
        let vals: Vec<f32> = (0..100).map(|i| if i % 2 == 0 { 0.0 } else { 2.0 }).collect();
        for &v in &vals {
            p.add(&PathSample { radiance: Vec3::splat(v), ..Default::default() });
        }
        let n = vals.len() as f32;
        let mean = vals.iter().sum::<f32>() / n;
        let var = vals.iter().map(|v| (v - mean) * (v - mean)).sum::<f32>() / (n - 1.0) / n;
        assert!((p.variance() - var).abs() < 1e-4 * var, "{} vs {var}", p.variance());
        // A constant pixel is converged.
        let mut c = Pixel::default();
        for _ in 0..16 {
            c.add(&PathSample { radiance: Vec3::splat(0.3), ..Default::default() });
        }
        assert!(c.error() < 1e-3);
    }

    #[test]
    fn mask_dilates() {
        let (w, h) = (5, 5);
        let mut e = vec![0.0f32; w * h];
        e[2 * w + 2] = 1.0;
        let m = active_mask(&e, w, h, 0.1);
        assert_eq!(m.iter().filter(|&&a| a).count(), 9);
        assert!(m[w + 1] && m[3 * w + 3] && !m[0]);
    }
}
