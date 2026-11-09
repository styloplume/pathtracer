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

use crate::camera::Camera;
use crate::image::encode_pixel;
use crate::integrator::radiance;
use crate::math::Vec3;
use crate::rng::{Rng, hash64};
use crate::scene::Scene;
use std::sync::Mutex;

#[derive(Clone, Copy, Default)]
pub struct Pixel {
    pub sum: Vec3,
    pub n: u32,
}

#[derive(Clone, Copy, Debug)]
pub struct PassInfo {
    /// Current block size (1 once full resolution has been reached).
    pub block: u32,
    /// Samples per pixel accumulated so far (1 during the preview phase).
    pub spp: u32,
    pub target_spp: u32,
    pub finished: bool,
}

pub fn thread_count(scene: &Scene) -> usize {
    match scene.settings.threads {
        0 => std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4),
        n => n,
    }
}

/// Renders `w`x`h` progressively, calling `on_pass` after every pass. Returns `false`
/// if `cancel` interrupted the render.
pub fn render_progressive(
    scene: &Scene,
    w: usize,
    h: usize,
    cancel: &(dyn Fn() -> bool + Sync),
    on_pass: &mut dyn FnMut(&[Pixel], &PassInfo),
) -> bool {
    let camera = Camera::new(&scene.camera, w as f32 / h as f32);
    let threads = thread_count(scene);
    let target = scene.settings.spp.max(1);
    let mut pixels = vec![Pixel::default(); w * h];

    let mut block = (scene.settings.initial_block.max(1) as usize).next_power_of_two();
    let mut first = true;
    loop {
        run_pass(scene, &camera, &mut pixels, w, h, threads, Some((block, first)), 0, cancel);
        if cancel() {
            return false;
        }
        let info = PassInfo { block: block as u32, spp: 1, target_spp: target, finished: block == 1 && target == 1 };
        on_pass(&pixels, &info);
        if block == 1 {
            break;
        }
        block /= 2;
        first = false;
    }

    for s in 1..target {
        run_pass(scene, &camera, &mut pixels, w, h, threads, None, s, cancel);
        if cancel() {
            return false;
        }
        on_pass(&pixels, &PassInfo { block: 1, spp: s + 1, target_spp: target, finished: s + 1 == target });
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
    cancel: &(dyn Fn() -> bool + Sync),
) {
    let rows = Mutex::new(pixels.chunks_mut(w).enumerate());
    let step = level.map_or(1, |(b, _)| b);
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| {
                loop {
                    if cancel() {
                        return;
                    }
                    let next = rows.lock().unwrap().find(|(y, _)| y % step == 0);
                    let Some((y, row)) = next else { return };
                    for x in (0..w).step_by(step) {
                        // Skip anchors already traced at a coarser level.
                        if let Some((b, false)) = level {
                            if x % (2 * b) == 0 && y % (2 * b) == 0 {
                                continue;
                            }
                        }
                        let c = trace_pixel(scene, camera, x, y, w, h, sample);
                        row[x].sum += c;
                        row[x].n += 1;
                    }
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

/// Converts the accumulation buffer into packed 0x00RRGGBB display pixels, expanding
/// each anchor pixel to fill its `block`x`block` square.
pub fn resolve_display(pixels: &[Pixel], w: usize, h: usize, block: usize, exposure: f32, tm: crate::image::Tonemap) -> Vec<u32> {
    let scale = exposure.exp2();
    let mut out = vec![0u32; w * h];
    let mut anchor_row = vec![0u32; w];
    for ay in (0..h).step_by(block) {
        for ax in (0..w).step_by(block) {
            let p = pixels[ay * w + ax];
            let c = if p.n > 0 { p.sum / p.n as f32 } else { Vec3::ZERO };
            let packed = encode_pixel(c, scale, tm);
            let end = (ax + block).min(w);
            anchor_row[ax..end].fill(packed);
        }
        for y in ay..(ay + block).min(h) {
            out[y * w..(y + 1) * w].copy_from_slice(&anchor_row);
        }
    }
    out
}
