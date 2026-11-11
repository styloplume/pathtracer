//! Reference-image regression tests: every example scene is rendered small, with a fixed
//! seed and sample count, and compared against a stored reference in
//! `testdata/regression/<scene>.hdr`.
//!
//! Renders are deterministic on one machine, but not across platforms (`libm` differs, and
//! path guiding trains concurrently), and any intended change to sampling reshuffles the
//! noise. So the comparison does not look at single pixels: both images are tonemapped
//! as the scene's `[render]` settings say, averaged over `BLOCK`x`BLOCK` blocks, and the
//! test fails if the block averages differ by more than noise can explain. That catches
//! anything that shifts brightness or color, or moves features, while being insensitive
//! to the noise pattern.
//!
//! How much noise remains differs a lot between scenes (spectral caustics vs. a diffuse
//! room), so each scene's tolerance is calibrated when its reference is made: a second
//! render with another seed is compared to the reference, and `TOLERANCE` times that
//! seed-to-seed difference (RMS and worst block) is stored in `<scene>.tol`.
//!
//! Regenerate the references after an intended change with
//! `PT_BLESS=1 cargo test --release regression` (and look at them first!). A failing test,
//! and blessing, writes the render, the reference and a difference image to
//! `target/regression/`.

use crate::image::{Encoder, Tonemap, encode_hdr, packed_to_rgb, write_png};
use crate::imageio::{self, Pixels};
use crate::math::Vec3;
use crate::renderer::{Pixel, Progress, render_progressive};
use std::path::{Path, PathBuf};

/// Width of the regression renders; the height follows the scene's aspect ratio.
const WIDTH: u32 = 128;
/// Side of the averaged blocks, in pixels.
const BLOCK: usize = 8;
/// Allowed difference relative to the calibrated seed-to-seed difference.
const TOLERANCE: f32 = 2.0;
/// Lower bounds of the stored limits (block averages of 8-bit display values, 0..255),
/// so a scene that happens to converge very well does not fail on rounding.
const MIN_RMS: f32 = 1.0;
const MIN_BLOCK: f32 = 4.0;
/// A scene noisier than this (seed-to-seed RMS) cannot catch much; blessing it fails.
const MAX_CALIBRATED_RMS: f32 = 3.0;
/// Per-sample luminance clamp of the regression renders (scenes may set a lower one).
const CLAMP: f32 = 4.0;

struct Quiet;
impl Progress for Quiet {}

fn root() -> PathBuf {
    crate::repo_root()
}

/// Renders `scene` (a path relative to the crate root) at the regression settings.
/// Returns (linear pixels, width, height, display encoder).
fn render(scene: &str, spp: u32, seed: u64) -> (Vec<Vec3>, usize, usize, Encoder) {
    let mut s = crate::loader::load_scene(&root().join(scene)).unwrap_or_else(|e| panic!("{scene}: {e}"));
    let st = &mut s.settings;
    let aspect = st.height as f32 / st.width.max(1) as f32;
    st.width = WIDTH;
    st.height = ((WIDTH as f32 * aspect).round() as u32).max(BLOCK as u32);
    st.spp = spp;
    st.noise_threshold = 0.0;
    st.time_limit = 0.0;
    st.seed = seed;
    // Fireflies (caustics, guiding's early iterations) would dominate the comparison of a
    // small render; clamping changes what is compared, not whether it is reproducible.
    if st.clamp <= 0.0 {
        st.clamp = CLAMP;
    }
    // Guiding trains on a quarter of the samples, as a long render would.
    st.guiding_spp = st.guiding_spp.min(spp / 4);
    let (w, h) = (st.width as usize, st.height as usize);
    let pixels = render_progressive(&s, &s.camera, w, h, &Quiet).expect("not cancelled");
    let enc = Encoder::new(s.settings.exposure, s.settings.tonemap);
    (pixels.iter().map(Pixel::mean).collect(), w, h, enc)
}

/// Display values (0..255 per channel), averaged over `BLOCK`x`BLOCK` blocks.
fn blocks(img: &[Vec3], w: usize, h: usize, enc: &Encoder) -> Vec<[f32; 3]> {
    let (bw, bh) = (w / BLOCK, h / BLOCK);
    let mut out = vec![[0.0f32; 3]; bw * bh];
    for y in 0..bh * BLOCK {
        for x in 0..bw * BLOCK {
            let p = enc.encode(img[y * w + x]);
            let b = &mut out[(y / BLOCK) * bw + x / BLOCK];
            for (c, shift) in [16, 8, 0].into_iter().enumerate() {
                b[c] += ((p >> shift) & 0xFF) as f32;
            }
        }
    }
    let n = (BLOCK * BLOCK) as f32;
    for b in &mut out {
        for v in b.iter_mut() {
            *v /= n;
        }
    }
    out
}

/// (RMS, largest) difference of the block averages over all blocks and channels.
fn compare(a: &[[f32; 3]], b: &[[f32; 3]]) -> (f32, f32) {
    let (mut sum, mut max) = (0.0f32, 0.0f32);
    for (x, y) in a.iter().flatten().zip(b.iter().flatten()) {
        let d = (x - y).abs();
        sum += d * d;
        max = max.max(d);
    }
    ((sum / (a.len() * 3).max(1) as f32).sqrt(), max)
}

fn load_reference(path: &Path) -> Option<(Vec<Vec3>, usize, usize)> {
    let img = imageio::load(path).ok()?;
    let Pixels::F32(px) = img.pixels else { return None };
    let rgb = px.chunks_exact(img.channels).map(|c| Vec3::new(c[0], c[1], c[2])).collect();
    Some((rgb, img.width, img.height))
}

fn save_png(path: &Path, img: &[Vec3], w: usize, h: usize, enc: &Encoder) {
    let packed: Vec<u32> = img.iter().map(|&c| enc.encode(c)).collect();
    write_png(path, w, h, &packed_to_rgb(&packed)).expect("write PNG");
}

/// Parses a `.tol` file: `rms = <limit>` and `max = <limit>` lines.
fn parse_tolerance(text: &str) -> Option<(f32, f32)> {
    let get = |key: &str| {
        text.lines().find_map(|l| {
            let (k, v) = l.split_once('=')?;
            (k.trim() == key).then(|| v.trim().parse::<f32>().ok()).flatten()
        })
    };
    Some((get("rms")?, get("max")?))
}

fn check(name: &str, spp: u32) {
    let scene = format!("scenes/{name}.toml");
    let dir = root().join("testdata/regression");
    let (reference, tolerance) = (dir.join(format!("{name}.hdr")), dir.join(format!("{name}.tol")));
    let (img, w, h, enc) = render(&scene, spp, 0);

    if std::env::var_os("PT_BLESS").is_some() {
        let (other, ..) = render(&scene, spp, 1);
        let (rms, max) = compare(&blocks(&img, w, h, &enc), &blocks(&other, w, h, &enc));
        let limits = ((TOLERANCE * rms).max(MIN_RMS), (TOLERANCE * max).max(MIN_BLOCK));
        println!("{name}: seed-to-seed rms {rms:.3}, max {max:.3} -> limits {:.3}, {:.3}", limits.0, limits.1);
        save_images(name, &img, &other, w, h, &enc);
        assert!(rms <= MAX_CALIBRATED_RMS, "{name}: too noisy to catch regressions (rms {rms}); raise its spp");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&reference, encode_hdr(w, h, &img)).unwrap();
        let note = format!(
            "# Limits for the block differences of the '{name}' regression test: {TOLERANCE}x the difference\n\
             # between two renders with different seeds ({spp} spp). Written by PT_BLESS=1.\n\
             rms = {:.3}\nmax = {:.3}\n",
            limits.0, limits.1
        );
        std::fs::write(&tolerance, note).unwrap();
        return;
    }

    let Some((ref_img, rw, rh)) = load_reference(&reference) else {
        panic!("{name}: missing reference {} (create it with PT_BLESS=1)", reference.display());
    };
    let (max_rms, max_block) = std::fs::read_to_string(&tolerance)
        .ok()
        .and_then(|t| parse_tolerance(&t))
        .unwrap_or_else(|| panic!("{name}: missing or malformed {} (re-bless)", tolerance.display()));
    assert_eq!((rw, rh), (w, h), "{name}: reference size differs; re-bless");
    let (rms, max) = compare(&blocks(&img, w, h, &enc), &blocks(&ref_img, w, h, &enc));
    if rms > max_rms || max > max_block {
        let out = save_images(name, &img, &ref_img, w, h, &enc);
        panic!(
            "{name}: render differs from the reference: block rms {rms:.3} (limit {max_rms}), worst block {max:.3} \
             (limit {max_block}); images in {}",
            out.display()
        );
    }
}

/// Writes `<name>-actual.png`, `<name>-reference.png` and the block differences
/// (amplified 8x) as `<name>-diff.png` to `target/regression/`; returns that directory.
fn save_images(name: &str, img: &[Vec3], reference: &[Vec3], w: usize, h: usize, enc: &Encoder) -> PathBuf {
    let out = root().join("target/regression");
    save_png(&out.join(format!("{name}-actual.png")), img, w, h, enc);
    save_png(&out.join(format!("{name}-reference.png")), reference, w, h, enc);
    let (a, b) = (blocks(img, w, h, enc), blocks(reference, w, h, enc));
    let bw = w / BLOCK;
    let diff: Vec<Vec3> = (0..w * h)
        .map(|i| {
            let (x, y) = (i % w, i / w);
            let k = (y / BLOCK) * bw + x / BLOCK;
            if x / BLOCK >= bw || k >= a.len() {
                return Vec3::ZERO;
            }
            Vec3::new(a[k][0] - b[k][0], a[k][1] - b[k][1], a[k][2] - b[k][2]).abs() * (8.0 / 255.0)
        })
        .collect();
    save_png(&out.join(format!("{name}-diff.png")), &diff, w, h, &Encoder::new(0.0, Tonemap::None));
    out
}

macro_rules! regression_tests {
    ($($name:ident: $spp:expr),* $(,)?) => {
        $(
            #[test]
            fn $name() {
                check(stringify!($name), $spp);
            }
        )*

        /// Every example scene has a regression test.
        #[test]
        fn all_scenes_covered() {
            let covered = [$(stringify!($name)),*];
            for e in std::fs::read_dir(root().join("scenes")).unwrap() {
                let p = e.unwrap().path();
                if p.extension().is_some_and(|x| x == "toml") {
                    let stem = p.file_stem().unwrap().to_string_lossy().into_owned();
                    assert!(covered.contains(&stem.as_str()), "scenes/{stem}.toml has no regression test");
                }
            }
        }
    };
}

// Scene name: samples per pixel.
regression_tests!(
    cornell: 128,
    dispersion: 1024,
    envmap: 256,
    fractals: 64,
    guiding: 1024,
    hypertexture: 256,
    implicit: 128,
    instancing: 64,
    lights: 512,
    materials: 64,
    media: 512,
    shapes: 64,
    sky: 64,
    spheres: 64,
    subsurface: 128,
);
