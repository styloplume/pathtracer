mod bvh;
mod camera;
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

use platform::{Event, Key, Window};
use renderer::{PassInfo, resolve_display};
use scene::Scene;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const USAGE: &str = "\
usage: pathtracer <scene.toml> [options]

options:
  --headless          render without a window and write a PNG
  -o, --output FILE   output PNG path (headless default: renders/<scene>.png)
  --spp N             override samples per pixel
  --size WxH          override resolution (e.g. 1280x720)
  --threads N         override worker thread count (0 = all cores)

window keys:
  Esc  quit      S  save PNG      R  restart render";

struct Args {
    scene: PathBuf,
    headless: bool,
    output: Option<PathBuf>,
    spp: Option<u32>,
    size: Option<(u32, u32)>,
    threads: Option<usize>,
}

fn parse_args() -> Result<Args, String> {
    let mut args = std::env::args().skip(1);
    let mut a = Args { scene: PathBuf::new(), headless: false, output: None, spp: None, size: None, threads: None };
    let mut scene = None;
    while let Some(arg) = args.next() {
        let mut value = |name: &str| args.next().ok_or_else(|| format!("{name} needs a value"));
        match arg.as_str() {
            "-h" | "--help" => return Err(String::new()),
            "--headless" => a.headless = true,
            "-o" | "--output" => a.output = Some(PathBuf::from(value(&arg)?)),
            "--spp" => a.spp = Some(value(&arg)?.parse().map_err(|_| "--spp expects an integer")?),
            "--threads" => a.threads = Some(value(&arg)?.parse().map_err(|_| "--threads expects an integer")?),
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

fn run(args: Args) -> Result<(), String> {
    let t0 = Instant::now();
    let mut scene = loader::load_scene(&args.scene)?;
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
    println!("loaded {} in {:.2}s: {}", args.scene.display(), t0.elapsed().as_secs_f32(), scene.stats());
    println!("rendering with {} threads", renderer::thread_count(&scene));
    let scene = Arc::new(scene);
    if args.headless { run_headless(&scene, &args) } else { run_windowed(scene, &args) }
}

fn scene_stem(args: &Args) -> String {
    args.scene.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "render".into())
}

fn save_png(path: &Path, pixels: &[u32], w: usize, h: usize) -> Result<(), String> {
    image::write_png(path, w, h, &image::packed_to_rgb(pixels)).map_err(|e| format!("cannot write '{}': {e}", path.display()))?;
    println!("saved {}", path.display());
    Ok(())
}

fn run_headless(scene: &Scene, args: &Args) -> Result<(), String> {
    let (w, h) = (scene.settings.width as usize, scene.settings.height as usize);
    let start = Instant::now();
    let mut last_report = Instant::now();
    let mut result = None;
    renderer::render_progressive(scene, w, h, &|| false, &mut |pixels, info| {
        if info.finished || last_report.elapsed() > Duration::from_secs(2) {
            last_report = Instant::now();
            println!("  {}/{} spp  {:.1}s", info.spp, info.target_spp, start.elapsed().as_secs_f32());
        }
        if info.finished {
            result = Some(resolve_display(pixels, w, h, 1, scene.settings.exposure, scene.settings.tonemap));
        }
    });
    let pixels = result.expect("render always finishes when not cancelled");
    let path = args.output.clone().unwrap_or_else(|| PathBuf::from(format!("renders/{}.png", scene_stem(args))));
    println!("done in {:.2}s", start.elapsed().as_secs_f32());
    save_png(&path, &pixels, w, h)
}

/// The latest displayable image, written by the render thread and read by the UI thread.
struct Frame {
    w: usize,
    h: usize,
    pixels: Vec<u32>,
    info: Option<PassInfo>,
    started: Instant,
    elapsed: Duration,
}

struct Shared {
    frame: Mutex<Frame>,
    /// Bumped whenever `frame` changes.
    version: AtomicU64,
    /// Bumped to make the render thread restart (new size, user request).
    generation: AtomicU64,
    size: Mutex<(usize, usize)>,
    quit: AtomicBool,
}

fn run_windowed(scene: Arc<Scene>, args: &Args) -> Result<(), String> {
    let mut window = Window::new("PathTracer", scene.settings.width, scene.settings.height)?;
    let (cw, ch) = window.client_size();
    let shared = Arc::new(Shared {
        frame: Mutex::new(Frame { w: 0, h: 0, pixels: Vec::new(), info: None, started: Instant::now(), elapsed: Duration::ZERO }),
        version: AtomicU64::new(0),
        generation: AtomicU64::new(1),
        size: Mutex::new((cw.max(1) as usize, ch.max(1) as usize)),
        quit: AtomicBool::new(false),
    });

    let render_thread = {
        let shared = shared.clone();
        let scene = scene.clone();
        std::thread::spawn(move || render_loop(&scene, &shared))
    };

    let mut last_version = 0;
    let mut last_title = Instant::now() - Duration::from_secs(1);
    let mut pending_resize: Option<((u32, u32), Instant)> = None;
    let mut needs_paint = false;
    'ui: loop {
        for ev in window.poll_events() {
            match ev {
                Event::Close | Event::KeyDown(Key::Escape) => break 'ui,
                Event::Resized(w, h) => {
                    pending_resize = Some(((w, h), Instant::now()));
                    needs_paint = true;
                }
                Event::Paint => needs_paint = true,
                Event::KeyDown(Key::Char('r')) => {
                    shared.generation.fetch_add(1, Ordering::SeqCst);
                }
                Event::KeyDown(Key::Char('s')) => {
                    let f = shared.frame.lock().unwrap();
                    if f.info.is_some() {
                        let secs = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_secs())
                            .unwrap_or(0);
                        let path = args.output.clone().unwrap_or_else(|| PathBuf::from(format!("renders/{}_{secs}.png", scene_stem(args))));
                        if let Err(e) = save_png(&path, &f.pixels, f.w, f.h) {
                            eprintln!("error: {e}");
                        }
                    }
                }
                Event::KeyDown(_) => {}
            }
        }

        // Restart at the new resolution once the user has stopped resizing.
        if let Some(((w, h), t)) = pending_resize {
            if t.elapsed() > Duration::from_millis(200) {
                pending_resize = None;
                let mut size = shared.size.lock().unwrap();
                if *size != (w as usize, h as usize) {
                    *size = (w as usize, h as usize);
                    shared.generation.fetch_add(1, Ordering::SeqCst);
                }
            }
        }

        let version = shared.version.load(Ordering::Acquire);
        if version != last_version || needs_paint || last_title.elapsed() > Duration::from_millis(250) {
            let f = shared.frame.lock().unwrap();
            if version != last_version || needs_paint {
                window.present(&f.pixels, f.w, f.h);
            }
            if let Some(info) = f.info {
                let elapsed = if info.finished { f.elapsed } else { f.started.elapsed() };
                let progress = if info.block > 1 {
                    format!("preview 1/{}", info.block)
                } else {
                    format!("{}/{} spp", info.spp, info.target_spp)
                };
                let done = if info.finished { " — done" } else { "" };
                window.set_title(&format!("PathTracer — {}x{} — {progress} — {:.1}s{done}", f.w, f.h, elapsed.as_secs_f32()));
            }
            last_version = version;
            needs_paint = false;
            last_title = Instant::now();
        }
        std::thread::sleep(Duration::from_millis(8));
    }

    shared.quit.store(true, Ordering::SeqCst);
    render_thread.join().map_err(|_| "render thread panicked")?;
    Ok(())
}

fn render_loop(scene: &Scene, shared: &Shared) {
    let mut rendered_generation = 0;
    while !shared.quit.load(Ordering::SeqCst) {
        let generation = shared.generation.load(Ordering::SeqCst);
        if generation == rendered_generation {
            std::thread::sleep(Duration::from_millis(20));
            continue;
        }
        rendered_generation = generation;
        let (w, h) = *shared.size.lock().unwrap();
        let started = Instant::now();
        let cancel = || shared.quit.load(Ordering::Relaxed) || shared.generation.load(Ordering::Relaxed) != generation;
        renderer::render_progressive(scene, w, h, &cancel, &mut |pixels, info| {
            let display = resolve_display(pixels, w, h, info.block as usize, scene.settings.exposure, scene.settings.tonemap);
            let mut f = shared.frame.lock().unwrap();
            *f = Frame { w, h, pixels: display, info: Some(*info), started, elapsed: started.elapsed() };
            drop(f);
            shared.version.fetch_add(1, Ordering::Release);
        });
    }
}
