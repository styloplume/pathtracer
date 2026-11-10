//! `pathtracer bench`: ray-tracing throughput and build times per scene.
//!
//! For each scene this measures the load time, the BVH build time, and the throughput
//! of four kinds of work, each repeated until it has run for a minimum time:
//!
//! - **primary**: one closest-hit camera ray per pixel (coherent rays);
//! - **diffuse**: one cosine-distributed closest-hit ray from every primary hit
//!   (incoherent rays, as in the second bounce of a path);
//! - **shadow**: one any-hit ray from every primary hit towards a sampled light point;
//! - **paths**: full path-traced samples (shading, light sampling, media, ...).
//!
//! Work is spread over the render threads with an atomic row counter, as in rendering.

use crate::camera::Camera;
use crate::loader;
use crate::math::{Frame, Ray, Vec3, offset_origin};
use crate::sampler::Sampler;
use crate::sampling::cosine_hemisphere;
use crate::scene::Scene;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

pub const USAGE: &str = "\
usage: pathtracer bench [scene.toml ...] [options]

Measures load / BVH build time and rays per second for each scene (default: every
scenes/*.toml). Rays/s count primary, diffuse and shadow rays separately; paths/s are
complete path-traced samples.

options:
  --size WxH          image size (default: 640 wide, scene aspect ratio)
  --threads N         worker threads (default: the scene's setting, 0 = all cores)
  --time SECONDS      minimum measuring time per test (default 1)
  --csv FILE          also write the results as CSV";

struct Options {
    scenes: Vec<PathBuf>,
    size: Option<(u32, u32)>,
    threads: Option<usize>,
    min_time: Duration,
    csv: Option<PathBuf>,
}

fn parse(args: &[String]) -> Result<Options, String> {
    let mut o = Options { scenes: Vec::new(), size: None, threads: None, min_time: Duration::from_secs(1), csv: None };
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let mut value = || it.next().ok_or_else(|| format!("{arg} needs a value"));
        match arg.as_str() {
            "-h" | "--help" => return Err(String::new()),
            "--size" => {
                let v = value()?;
                let (w, h) = v.split_once(['x', 'X']).ok_or("--size expects WxH")?;
                let (w, h): (u32, u32) =
                    (w.parse().map_err(|_| "--size expects WxH")?, h.parse().map_err(|_| "--size expects WxH")?);
                if w == 0 || h == 0 {
                    return Err("--size must be positive".into());
                }
                o.size = Some((w, h));
            }
            "--threads" => o.threads = Some(value()?.parse().map_err(|_| "--threads expects an integer")?),
            "--time" => {
                let t: f32 = value()?.parse().map_err(|_| "--time expects a number of seconds")?;
                if !(t > 0.0 && t.is_finite()) {
                    return Err("--time must be positive".into());
                }
                o.min_time = Duration::from_secs_f32(t);
            }
            "--csv" => o.csv = Some(PathBuf::from(value()?)),
            s if s.starts_with('-') => return Err(format!("unknown option '{s}'")),
            s => o.scenes.push(PathBuf::from(s)),
        }
    }
    if o.scenes.is_empty() {
        let dir = std::fs::read_dir("scenes").map_err(|e| format!("no scenes given and cannot list scenes/: {e}"))?;
        o.scenes = dir
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "toml"))
            .collect();
        o.scenes.sort();
    }
    Ok(o)
}

/// Results for one scene. Rates are per second.
struct Row {
    name: String,
    prims: usize,
    load: Duration,
    bvh: Duration,
    primary: f64,
    diffuse: f64,
    shadow: f64,
    paths: f64,
}

pub fn run(args: &[String]) -> Result<(), String> {
    let o = match parse(args) {
        Ok(o) => o,
        Err(e) => {
            return Err(if e.is_empty() { USAGE.into() } else { format!("{e}\n\n{USAGE}") });
        }
    };
    println!(
        "{:<16} {:>9} {:>8} {:>8} {:>10} {:>10} {:>10} {:>10}",
        "scene", "prims", "load", "bvh", "primary", "diffuse", "shadow", "paths"
    );
    println!(
        "{:<16} {:>9} {:>8} {:>8} {:>10} {:>10} {:>10} {:>10}",
        "", "", "s", "ms", "Mray/s", "Mray/s", "Mray/s", "Mpath/s"
    );
    let mut rows = Vec::new();
    for path in &o.scenes {
        let t0 = Instant::now();
        let mut scene = loader::load_scene(path)?;
        let load = t0.elapsed();
        if let Some(t) = o.threads {
            scene.settings.threads = t;
        }
        let (w, h) = o.size.unwrap_or_else(|| {
            let aspect = scene.settings.height as f32 / scene.settings.width.max(1) as f32;
            (640, ((640.0 * aspect).round() as u32).max(1))
        });
        let name = path.file_stem().map_or_else(String::new, |s| s.to_string_lossy().into_owned());
        let row = bench_scene(&scene, name, load, w as usize, h as usize, o.min_time);
        println!(
            "{:<16} {:>9} {:>8.2} {:>8.1} {:>10.2} {:>10.2} {:>10.2} {:>10.3}",
            row.name,
            row.prims,
            row.load.as_secs_f64(),
            row.bvh.as_secs_f64() * 1e3,
            row.primary / 1e6,
            row.diffuse / 1e6,
            row.shadow / 1e6,
            row.paths / 1e6
        );
        rows.push(row);
    }
    if let Some(csv) = &o.csv {
        let mut s = String::from(
            "scene,prims,load_s,bvh_ms,primary_rays_per_s,diffuse_rays_per_s,shadow_rays_per_s,paths_per_s\n",
        );
        for r in &rows {
            // Writing to a String cannot fail.
            let _ = writeln!(
                s,
                "{},{},{:.4},{:.3},{:.0},{:.0},{:.0},{:.0}",
                r.name,
                r.prims,
                r.load.as_secs_f64(),
                r.bvh.as_secs_f64() * 1e3,
                r.primary,
                r.diffuse,
                r.shadow,
                r.paths
            );
        }
        std::fs::write(csv, s).map_err(|e| format!("cannot write '{}': {e}", csv.display()))?;
        println!("wrote {}", csv.display());
    }
    Ok(())
}

/// A primary hit: where the diffuse and shadow rays start.
#[derive(Clone, Copy)]
struct Origin {
    p: Vec3,
    /// Geometric normal facing the camera ray.
    n: Vec3,
    time: f32,
}

fn bench_scene(scene: &Scene, name: String, load: Duration, w: usize, h: usize, min_time: Duration) -> Row {
    let threads = crate::renderer::thread_count(scene);
    let camera = Camera::new(&scene.camera, w as f32 / h as f32);
    let cone = camera.cone(h);
    let kind = scene.settings.sampler;
    let seed = scene.settings.seed;
    let sampler = |x: usize, y: usize, pass: u32| Sampler::new(kind, x as u32, y as u32, w as u32, pass, seed);
    let camera_ray = |x: usize, y: usize, pass: u32| {
        let mut s = sampler(x, y, pass);
        let (u1, u2) = s.get2d();
        camera.ray((x as f32 + u1) / w as f32, (y as f32 + u2) / h as f32, &mut s).map(|r| (r, s))
    };

    // Primary hits, kept for the secondary-ray tests.
    let mut origins: Vec<Option<Origin>> = vec![None; w * h];
    for (i, o) in origins.iter_mut().enumerate() {
        if let Some((ray, _)) = camera_ray(i % w, i / w, 0)
            && let Some(hit) = scene.intersect(&ray, f32::INFINITY)
        {
            let si = scene.interaction(&ray, &hit);
            let n = if si.ng.dot(ray.d) > 0.0 { -si.ng } else { si.ng };
            *o = Some(Origin { p: si.p, n, time: ray.time });
        }
    }

    let primary = measure(threads, h, min_time, |y, pass| {
        let mut rays = 0;
        for x in 0..w {
            if let Some((ray, _)) = camera_ray(x, y, pass) {
                std::hint::black_box(scene.intersect(&ray, f32::INFINITY));
                rays += 1;
            }
        }
        rays
    });
    let diffuse = measure(threads, h, min_time, |y, pass| {
        let mut rays = 0;
        for x in 0..w {
            if let Some(o) = origins[y * w + x] {
                let (u1, u2) = sampler(x, y, pass).get2d();
                let d = Frame::from_normal(o.n).to_world(cosine_hemisphere(u1, u2));
                let ray = Ray::new(offset_origin(o.p, o.n, d), d, o.time);
                std::hint::black_box(scene.intersect(&ray, f32::INFINITY));
                rays += 1;
            }
        }
        rays
    });
    let shadow = measure(threads, h, min_time, |y, pass| {
        let mut rays = 0;
        for x in 0..w {
            if let Some(o) = origins[y * w + x]
                && let Some(ls) = scene.sample_light(o.p, o.n, &mut sampler(x, y, pass))
            {
                std::hint::black_box(scene.occluded_light(o.p, o.n, &ls, o.time));
                rays += 1;
            }
        }
        rays
    });
    let paths = measure(threads, h, min_time, |y, pass| {
        for x in 0..w {
            if let Some((ray, mut s)) = camera_ray(x, y, pass) {
                std::hint::black_box(crate::integrator::radiance(scene, ray, cone, &mut s, None));
            }
        }
        w as u64
    });
    Row { name, prims: scene.num_prims(), load, bvh: scene.bvh_build_time, primary, diffuse, shadow, paths }
}

/// Runs `row(y, pass)` over all rows of the image, pass after pass, until `min_time` has
/// elapsed; returns the work items (as counted by `row`) per second.
fn measure(threads: usize, h: usize, min_time: Duration, row: impl Fn(usize, u32) -> u64 + Sync) -> f64 {
    let done = AtomicU64::new(0);
    let start = Instant::now();
    let mut pass = 1;
    while start.elapsed() < min_time {
        let next = AtomicUsize::new(0);
        std::thread::scope(|s| {
            for _ in 0..threads {
                s.spawn(|| {
                    let mut n = 0;
                    loop {
                        let y = next.fetch_add(1, Ordering::Relaxed);
                        if y >= h {
                            break;
                        }
                        n += row(y, pass);
                    }
                    done.fetch_add(n, Ordering::Relaxed);
                });
            }
        });
        pass += 1;
    }
    done.into_inner() as f64 / start.elapsed().as_secs_f64()
}
