mod app;
mod bvh;
mod camera;
mod controls;
mod deflate;
mod denoise;
mod environment;
mod filter;
mod geometry;
mod gltf;
mod guiding;
mod image;
mod imageio;
mod integrator;
mod jpeg;
mod json;
mod lights;
mod loader;
mod material;
mod math;
mod medium;
mod mesh;
mod mtl;
mod output;
mod platform;
mod renderer;
mod rng;
mod sampler;
mod sampling;
mod scene;
mod shapes;
mod sky;
mod spectrum;
mod texture;
mod toml;

use image::Encoder;
use output::Snapshot;
use renderer::{PassInfo, Pixel, Progress};
use scene::Scene;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

const USAGE: &str = "\
usage: pathtracer <scene.toml> [options]

options:
  --headless          render without a window and save the result
  -o, --output FILE   output path; a .png/.hdr/.pfm extension selects that single format
                      (default: [output] in the scene, else renders/<scene>.png)
  --spp N             override samples per pixel (the maximum, with adaptive sampling)
  --size WxH          override resolution (e.g. 1280x720)
  --threads N         override worker thread count (0 = all cores)
  --time SECONDS      stop after this much render time
  --noise THRESHOLD   adaptive sampling: stop pixels below this noise level (e.g. 0.02)
  --denoise           also save a denoised image
  --spectral          hero-wavelength spectral rendering (dispersion, spectral colour mixing)
  --sampler NAME      sample generator: independent, sobol (default), bluenoise
  --guiding           path guiding (learns where light comes from; for indoor scenes, caustics)
  --autosave          window mode: save whenever a render finishes
  --scale N           window mode: render at 1/N of the window resolution

window controls (F1 in the window shows them too):
  mouse  L-drag orbit, Shift+L or M-drag pan, R-drag look, wheel dolly, Shift+wheel FOV
  move   W/A/S/D or arrows, Q/E down/up (hold Shift: fast), Home resets the camera
  view   +/- exposure, 0 reset, T tonemap, V view (color/albedo/normal/depth/noise/samples),
         N denoise, 1-4 resolution scale
  misc   Space pause, R restart, H HUD, Ctrl+S save, Esc quit
  The scene file (and its meshes) are reloaded automatically when saved.";

#[derive(Clone)]
#[expect(clippy::struct_excessive_bools, reason = "independent command-line flags")]
pub(crate) struct Args {
    pub scene: PathBuf,
    pub headless: bool,
    pub output: Option<PathBuf>,
    pub spp: Option<u32>,
    pub size: Option<(u32, u32)>,
    pub threads: Option<usize>,
    pub time: Option<f32>,
    pub noise: Option<f32>,
    pub denoise: bool,
    pub spectral: bool,
    pub sampler: Option<sampler::SamplerKind>,
    pub guiding: bool,
    pub autosave: bool,
    pub scale: u32,
}

fn parse_args() -> Result<Args, String> {
    let mut args = std::env::args().skip(1);
    let mut a = Args {
        scene: PathBuf::new(),
        headless: false,
        output: None,
        spp: None,
        size: None,
        threads: None,
        time: None,
        noise: None,
        denoise: false,
        spectral: false,
        sampler: None,
        guiding: false,
        autosave: false,
        scale: 1,
    };
    let mut scene = None;
    while let Some(arg) = args.next() {
        let mut value = |name: &str| args.next().ok_or_else(|| format!("{name} needs a value"));
        let non_negative = |v: String, name: &str| match v.parse::<f32>() {
            Ok(x) if x >= 0.0 => Ok(x),
            _ => Err(format!("{name} expects a non-negative number")),
        };
        match arg.as_str() {
            "-h" | "--help" => return Err(String::new()),
            "--headless" => a.headless = true,
            "--denoise" => a.denoise = true,
            "--spectral" => a.spectral = true,
            "--guiding" => a.guiding = true,
            "--sampler" => {
                let v = value(&arg)?;
                a.sampler = Some(
                    sampler::SamplerKind::parse(&v)
                        .ok_or_else(|| format!("unknown sampler '{v}' ({})", sampler::SAMPLER_NAMES))?,
                );
            }
            "--autosave" => a.autosave = true,
            "-o" | "--output" => a.output = Some(PathBuf::from(value(&arg)?)),
            "--spp" => a.spp = Some(value(&arg)?.parse().map_err(|_| "--spp expects an integer")?),
            "--threads" => a.threads = Some(value(&arg)?.parse().map_err(|_| "--threads expects an integer")?),
            "--time" => a.time = Some(non_negative(value(&arg)?, &arg)?),
            "--noise" => a.noise = Some(non_negative(value(&arg)?, &arg)?),
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
    if let Err(e) = run(&args) {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

/// Applies command-line overrides to a freshly loaded scene (also used on hot reload).
pub(crate) fn apply_overrides(scene: &mut Scene, args: &Args) {
    let s = &mut scene.settings;
    if let Some(spp) = args.spp {
        s.spp = spp.max(1);
    }
    if let Some((w, h)) = args.size {
        s.width = w;
        s.height = h;
    }
    if let Some(t) = args.threads {
        s.threads = t;
    }
    if let Some(t) = args.time {
        s.time_limit = t;
    }
    if let Some(n) = args.noise {
        s.noise_threshold = n;
    }
    s.spectral |= args.spectral;
    s.guiding |= args.guiding;
    if let Some(k) = args.sampler {
        s.sampler = k;
    }
    let o = &mut scene.output;
    if let Some(file) = &args.output {
        o.set_output_arg(file);
    }
    o.denoise |= args.denoise;
    o.save_on_finish |= args.autosave;
}

fn run(args: &Args) -> Result<(), String> {
    let t0 = Instant::now();
    let mut scene = loader::load_scene(&args.scene)?;
    apply_overrides(&mut scene, args);
    println!("loaded {} in {:.2}s: {}", args.scene.display(), t0.elapsed().as_secs_f32(), scene.stats());
    println!("rendering with {} threads", renderer::thread_count(&scene));
    if args.headless { run_headless(&scene, args) } else { app::run(scene, args) }
}

pub(crate) fn scene_stem(args: &Args) -> String {
    args.scene.file_stem().map_or_else(|| "render".into(), |s| s.to_string_lossy().into_owned())
}

/// Saves every output configured for `scene` (color, denoised, AOVs).
pub(crate) fn save_outputs(
    scene: &Scene,
    args: &Args,
    snap: &Snapshot<'_>,
    enc: &Encoder,
    default_base: &str,
) -> Result<Vec<PathBuf>, String> {
    let base = scene.output.base_path(&scene_stem(args), default_base);
    output::save(snap, &scene.output, &base, enc, renderer::thread_count(scene))
}

pub(crate) fn pass_summary(info: &PassInfo) -> String {
    use std::fmt::Write as _;
    let mut s = format!("{}/{} spp", info.spp, info.target_spp);
    // Writing to a String cannot fail.
    if info.error.is_finite() {
        let _ = write!(s, "  noise {:.2}%", info.error * 100.0);
    }
    if info.active < 1.0 {
        let _ = write!(s, "  active {:.1}%", info.active * 100.0);
    }
    if info.finished {
        let _ = write!(s, "  ({})", info.stop.describe());
    }
    s
}

/// Console progress for headless renders.
struct Headless {
    start: Instant,
    last_report: Mutex<Instant>,
}

impl Progress for Headless {
    fn end_pass(&self, _pixels: &[Pixel], info: &PassInfo) {
        let mut last = self.last_report.lock().unwrap();
        if info.finished || last.elapsed() > Duration::from_secs(2) {
            *last = Instant::now();
            println!("  {}  {:.1}s", pass_summary(info), self.start.elapsed().as_secs_f32());
        }
    }
}

fn run_headless(scene: &Scene, args: &Args) -> Result<(), String> {
    let (w, h) = (scene.settings.width as usize, scene.settings.height as usize);
    let progress = Headless { start: Instant::now(), last_report: Mutex::new(Instant::now()) };
    let pixels = renderer::render_progressive(scene, &scene.camera, w, h, &progress)
        .expect("render always finishes when not cancelled");
    let total: u64 = pixels.iter().map(|p| u64::from(p.n)).sum();
    let secs = progress.start.elapsed().as_secs_f32();
    println!(
        "done in {secs:.2}s, {:.1} spp average ({:.2} Msamples/s)",
        total as f32 / (w * h) as f32,
        total as f32 / secs / 1e6
    );
    let enc = Encoder::new(scene.settings.exposure, scene.settings.tonemap);
    save_outputs(scene, args, &Snapshot { pixels: &pixels, w, h, block: 1 }, &enc, "renders/{scene}")?;
    Ok(())
}
