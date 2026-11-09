mod app;
mod bvh;
mod camera;
mod controls;
mod geometry;
mod image;
mod integrator;
mod loader;
mod material;
mod math;
mod mesh;
mod platform;
mod renderer;
mod rng;
mod sampling;
mod scene;
mod toml;

use image::Encoder;
use renderer::{PassInfo, Pixel, Progress, resolve_display};
use scene::Scene;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

const USAGE: &str = "\
usage: pathtracer <scene.toml> [options]

options:
  --headless          render without a window and write a PNG
  -o, --output FILE   output PNG path (headless default: renders/<scene>.png)
  --spp N             override samples per pixel
  --size WxH          override resolution (e.g. 1280x720)
  --threads N         override worker thread count (0 = all cores)
  --scale N           window mode: render at 1/N of the window resolution

window controls (F1 in the window shows them too):
  mouse  L-drag orbit, Shift+L or M-drag pan, R-drag look, wheel dolly, Shift+wheel FOV
  move   W/A/S/D or arrows, Q/E down/up (hold Shift: fast), Home resets the camera
  view   +/- exposure, 0 reset, T tonemap, 1-4 resolution scale
  misc   Space pause, R restart, H HUD, Ctrl+S save PNG, Esc quit
  The scene file (and its meshes) are reloaded automatically when saved.";

#[derive(Clone)]
pub(crate) struct Args {
    pub scene: PathBuf,
    pub headless: bool,
    pub output: Option<PathBuf>,
    pub spp: Option<u32>,
    pub size: Option<(u32, u32)>,
    pub threads: Option<usize>,
    pub scale: u32,
}

fn parse_args() -> Result<Args, String> {
    let mut args = std::env::args().skip(1);
    let mut a =
        Args { scene: PathBuf::new(), headless: false, output: None, spp: None, size: None, threads: None, scale: 1 };
    let mut scene = None;
    while let Some(arg) = args.next() {
        let mut value = |name: &str| args.next().ok_or_else(|| format!("{name} needs a value"));
        match arg.as_str() {
            "-h" | "--help" => return Err(String::new()),
            "--headless" => a.headless = true,
            "-o" | "--output" => a.output = Some(PathBuf::from(value(&arg)?)),
            "--spp" => a.spp = Some(value(&arg)?.parse().map_err(|_| "--spp expects an integer")?),
            "--threads" => a.threads = Some(value(&arg)?.parse().map_err(|_| "--threads expects an integer")?),
            "--scale" => {
                a.scale = value(&arg)?.parse().map_err(|_| "--scale expects an integer")?;
                if a.scale == 0 {
                    return Err("--scale must be at least 1".into());
                }
            }
            "--size" => {
                let v = value(&arg)?;
                let (w, h) = v.split_once(['x', 'X']).ok_or("--size expects WxH")?;
                let w: u32 = w.parse().map_err(|_| "--size expects WxH")?;
                let h: u32 = h.parse().map_err(|_| "--size expects WxH")?;
                if w == 0 || h == 0 {
                    return Err("--size must be positive".into());
                }
                a.size = Some((w, h));
            }
            s if s.starts_with('-') => return Err(format!("unknown option '{s}'")),
            s => {
                if scene.replace(PathBuf::from(s)).is_some() {
                    return Err("only one scene file may be given".into());
                }
            }
        }
    }
    a.scene = scene.ok_or("no scene file given")?;
    Ok(a)
}

fn main() {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            if !e.is_empty() {
                eprintln!("error: {e}\n");
            }
            eprintln!("{USAGE}");
            std::process::exit(2);
        }
    };
    if let Err(e) = run(args) {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

/// Applies command-line overrides to a freshly loaded scene (also used on hot reload).
pub(crate) fn apply_overrides(scene: &mut Scene, args: &Args) {
    if let Some(spp) = args.spp {
        scene.settings.spp = spp.max(1);
    }
    if let Some((w, h)) = args.size {
        scene.settings.width = w;
        scene.settings.height = h;
    }
    if let Some(t) = args.threads {
        scene.settings.threads = t;
    }
}

fn run(args: Args) -> Result<(), String> {
    let t0 = Instant::now();
    let mut scene = loader::load_scene(&args.scene)?;
    apply_overrides(&mut scene, &args);
    println!("loaded {} in {:.2}s: {}", args.scene.display(), t0.elapsed().as_secs_f32(), scene.stats());
    println!("rendering with {} threads", renderer::thread_count(&scene));
    if args.headless { run_headless(&scene, &args) } else { app::run(scene, &args) }
}

pub(crate) fn scene_stem(args: &Args) -> String {
    args.scene.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "render".into())
}

pub(crate) fn save_png(path: &Path, pixels: &[u32], w: usize, h: usize) -> Result<(), String> {
    image::write_png(path, w, h, &image::packed_to_rgb(pixels)).map_err(|e| format!("cannot write '{}': {e}", path.display()))?;
    println!("saved {}", path.display());
    Ok(())
}

/// Console progress for headless renders; keeps the final image.
struct Headless {
    w: usize,
    h: usize,
    encoder: Encoder,
    start: Instant,
    last_report: Mutex<Instant>,
    result: Mutex<Option<Vec<u32>>>,
}

impl Progress for Headless {
    fn end_pass(&self, pixels: &[Pixel], info: &PassInfo) {
        let mut last = self.last_report.lock().unwrap();
        if info.finished || last.elapsed() > Duration::from_secs(2) {
            *last = Instant::now();
            println!("  {}/{} spp  {:.1}s", info.spp, info.target_spp, self.start.elapsed().as_secs_f32());
        }
        if info.finished {
            *self.result.lock().unwrap() = Some(resolve_display(pixels, self.w, self.h, 1, &self.encoder));
        }
    }
}

fn run_headless(scene: &Scene, args: &Args) -> Result<(), String> {
    let (w, h) = (scene.settings.width as usize, scene.settings.height as usize);
    let progress = Headless {
        w,
        h,
        encoder: Encoder::new(scene.settings.exposure, scene.settings.tonemap),
        start: Instant::now(),
        last_report: Mutex::new(Instant::now()),
        result: Mutex::new(None),
    };
    renderer::render_progressive(scene, &scene.camera, w, h, &progress);
    let pixels = progress.result.into_inner().unwrap().expect("render always finishes when not cancelled");
    let path = args.output.clone().unwrap_or_else(|| PathBuf::from(format!("renders/{}.png", scene_stem(args))));
    println!("done in {:.2}s", progress.start.elapsed().as_secs_f32());
    save_png(&path, &pixels, w, h)
}
