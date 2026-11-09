//! Progressive, multi-threaded image rendering.
//!
//! Rendering proceeds in two phases:
//!
//! 1. **Coarse-to-fine preview.** With a block size `B` starting at `initial_block`
//!    (a power of two), one sample is traced for every pixel whose coordinates are
//!    multiples of `B`; on display that "anchor" pixel's color fills its whole BxB
//!    block. `B` is then halved, and only anchors not already traced at a coarser
//!    level are traced. When `B` reaches 1, every pixel holds exactly one sample, so
//!    no work is wasted on the preview.
//! 2. **Refinement.** Whole-image passes add one sample per pixel until the
//!    target sample count is reached.
//!
//! Progress is reported through [`Progress`]: per finished row (so long passes can
//! be displayed while they run) and per finished pass.

use crate::camera::{Camera, CameraDesc};
use crate::image::Encoder;
use crate::integrator::radiance;
use crate::math::Vec3;
use crate::rng::{Rng, hash64};
use crate::scene::Scene;
use std::ops::Range;
use std::sync::Mutex;
use std::time::Duration;

#[derive(Clone, Copy, Default)]
pub struct Pixel {
    pub sum: Vec3,
    pub n: u32,
}

#[derive(Clone, Copy, Debug)]
pub struct PassInfo {
    /// Current block size (1 once full resolution has been reached).
    pub block: u32,
    /// Samples per pixel accumulated once this pass completes (1 during the preview phase).
    pub spp: u32,
    pub target_spp: u32,
    /// True for the last pass.
    pub finished: bool,
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
    fn begin_pass(&self, _info: &PassInfo) {}
    /// Row `y` has been updated; `samples` is the number of samples just traced in it.
    fn row_done(&self, _y: usize, _row: &[Pixel], _samples: u64) {}
    fn end_pass(&self, _pixels: &[Pixel], _info: &PassInfo) {}
}

pub fn thread_count(scene: &Scene) -> usize {
    match scene.settings.threads {
        0 => std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4),
        n => n,
    }
}

/// Renders `w`x`h` progressively from `camera`. Returns `false` if cancelled.
pub fn render_progressive(scene: &Scene, camera: &CameraDesc, w: usize, h: usize, progress: &dyn Progress) -> bool {
    let camera = Camera::new(camera, w as f32 / h as f32);
    let threads = thread_count(scene);
    let target = scene.settings.spp.max(1);
    let mut pixels = vec![Pixel::default(); w * h];

    let mut block = (scene.settings.initial_block.max(1) as usize).next_power_of_two();
    let mut first = true;
    loop {
        let info = PassInfo { block: block as u32, spp: 1, target_spp: target, finished: block == 1 && target == 1 };
        progress.begin_pass(&info);
        run_pass(scene, &camera, &mut pixels, w, h, threads, Some((block, first)), 0, progress);
        if progress.cancelled() {
            return false;
        }
        progress.end_pass(&pixels, &info);
        if block == 1 {
            break;
        }
        block /= 2;
        first = false;
    }

    for s in 1..target {
        let info = PassInfo { block: 1, spp: s + 1, target_spp: target, finished: s + 1 == target };
        progress.begin_pass(&info);
        run_pass(scene, &camera, &mut pixels, w, h, threads, None, s, progress);
        if progress.cancelled() {
            return false;
        }
        progress.end_pass(&pixels, &info);
    }
    true
}

/// `level = Some((block, is_first_level))` for preview passes, `None` for a full pass.
#[allow(clippy::too_many_arguments)]
fn run_pass(
    scene: &Scene,
    camera: &Camera,
    pixels: &mut [Pixel],
    w: usize,
    h: usize,
    threads: usize,
    level: Option<(usize, bool)>,
    sample: u32,
    progress: &dyn Progress,
) {
    let rows = Mutex::new(pixels.chunks_mut(w).enumerate());
    let step = level.map_or(1, |(b, _)| b);
    std::thread::scope(|s| {
        for _ in 0..threads {
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
                    let mut traced = 0u64;
                    for x in (0..w).step_by(step) {
                        // Skip anchors already traced at a coarser level.
                        if let Some((b, false)) = level
                            && x % (2 * b) == 0
                            && y % (2 * b) == 0
                        {
                            continue;
                        }
                        let c = trace_pixel(scene, camera, x, y, w, h, sample);
                        row[x].sum += c;
                        row[x].n += 1;
                        traced += 1;
                    }
                    progress.row_done(y, row, traced);
                }
            });
        }
    });
}

#[inline]
fn trace_pixel(scene: &Scene, camera: &Camera, x: usize, y: usize, w: usize, h: usize, sample: u32) -> Vec3 {
    let pixel_index = (y * w + x) as u64;
    let seed = hash64(hash64(pixel_index ^ (scene.settings.seed << 40)) ^ sample as u64);
    let mut rng = Rng::new(seed, pixel_index);
    let s = (x as f32 + rng.f32()) / w as f32;
    let t = (y as f32 + rng.f32()) / h as f32;
    let ray = camera.ray(s, t, &mut rng);
    let mut c = radiance(scene, ray, &mut rng);
    if !c.is_finite() {
        c = Vec3::ZERO;
    }
    let clamp = scene.settings.clamp;
    if clamp > 0.0 {
        let lum = c.luminance();
        if lum > clamp {
            c *= clamp / lum;
        }
    }
    c
}

/// Largest block size the display fallback will look at.
const MAX_BLOCK: usize = 1 << 12;

/// The anchor pixel that colors (x, y) at block size `block`. If that anchor has not
/// been traced yet (a preview pass in progress), falls back to coarser anchors.
#[inline]
fn anchor(pixels: &[Pixel], w: usize, x: usize, y: usize, block: usize) -> Pixel {
    let mut b = block;
    loop {
        let p = pixels[(y / b * b) * w + x / b * b];
        if p.n > 0 || b >= MAX_BLOCK {
            return p;
        }
        b *= 2;
    }
}

/// Re-resolves the display rows affected by changes to accumulation rows `rows`,
/// writing packed 0x00RRGGBB pixels into `out` (`w`x`h`). Each anchor pixel fills
/// its `block`x`block` square.
pub fn resolve_rows(pixels: &[Pixel], w: usize, h: usize, block: usize, rows: Range<usize>, enc: &Encoder, out: &mut [u32]) {
    let block = block.max(1);
    let start = rows.start / block * block;
    let mut anchor_row = vec![0u32; w];
    for ay in (start..rows.end.min(h)).step_by(block) {
        for ax in (0..w).step_by(block) {
            let p = anchor(pixels, w, ax, ay, block);
            let c = if p.n > 0 { p.sum / p.n as f32 } else { Vec3::ZERO };
            let end = (ax + block).min(w);
            anchor_row[ax..end].fill(enc.encode(c));
        }
        for y in ay..(ay + block).min(h) {
            out[y * w..(y + 1) * w].copy_from_slice(&anchor_row);
        }
    }
}

/// Resolves the whole accumulation buffer to display pixels.
pub fn resolve_display(pixels: &[Pixel], w: usize, h: usize, block: usize, enc: &Encoder) -> Vec<u32> {
    let mut out = vec![0u32; w * h];
    resolve_rows(pixels, w, h, block, 0..h, enc, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image::Tonemap;

    fn px(v: f32) -> Pixel {
        Pixel { sum: Vec3::splat(v), n: 1 }
    }

    #[test]
    fn resolve_falls_back_to_coarser_anchors() {
        // 4x4 image: only the 4-anchor (0,0) and the 2-anchor (2,0) have samples.
        let (w, h) = (4, 4);
        let mut pixels = vec![Pixel::default(); w * h];
        pixels[0] = px(1.0);
        pixels[2] = px(0.0);
        let enc = Encoder::new(0.0, Tonemap::None);
        let out = resolve_display(&pixels, w, h, 2, &enc);
        let white = enc.encode(Vec3::splat(1.0));
        let black = enc.encode(Vec3::ZERO);
        assert_eq!(out[0], white);
        assert_eq!(out[3], black); // traced 2-anchor at (2,0)
        assert_eq!(out[2 * w], white); // untraced (0,2) -> falls back to (0,0)
        assert_eq!(out[2 * w + 2], white); // untraced (2,2) -> falls back to (0,0)
    }
}
