//! Interactive windowed front end: a UI thread (input, camera, display, hot reload)
//! and a render thread that restarts the progressive render whenever the view changes.

use crate::controls::CameraController;
use crate::denoise::denoise;
use crate::image::{Encoder, Tonemap};
use crate::math::Vec3;
use crate::output::Snapshot;
use crate::platform::{Event, Key, Modifiers, MouseButton, Window};
use crate::renderer::{self, PassInfo, Pixel, Progress, Shader, View};
use crate::scene::Scene;
use crate::{Args, apply_overrides, loader, pass_summary, save_outputs, scene_stem};
use std::collections::HashSet;
use std::ops::Range;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

const HELP: &[&str] = &[
    "mouse  L-drag orbit | Shift+L or M-drag pan | R-drag look | wheel dolly | Shift+wheel FOV",
    "move   W/A/S/D or arrows, Q/E down/up (hold Shift: fast) | Home: reset camera",
    "view   +/- exposure, 0 reset | T tonemap | V view mode | N denoise | 1-4 resolution scale",
    "misc   Space pause | R restart | H HUD | F1 help | Ctrl+S save | Esc quit",
];

/// Minimum time between interactive denoiser runs while samples keep arriving.
const DENOISE_INTERVAL: Duration = Duration::from_millis(500);

/// Accumulated render time that can be paused.
#[derive(Default)]
struct Stopwatch {
    acc: Duration,
    since: Option<Instant>,
}

impl Stopwatch {
    fn reset(&mut self, running: bool) {
        self.acc = Duration::ZERO;
        self.since = running.then(Instant::now);
    }
    fn stop(&mut self) {
        if let Some(s) = self.since.take() {
            self.acc += s.elapsed();
        }
    }
    fn start(&mut self) {
        self.since.get_or_insert_with(Instant::now);
    }
    fn elapsed(&self) -> Duration {
        self.acc + self.since.map_or(Duration::ZERO, |s| s.elapsed())
    }
}

/// The render thread's accumulation buffer as seen by the UI thread.
struct Frame {
    generation: u64,
    w: usize,
    h: usize,
    accum: Vec<Pixel>,
    /// Block size of the pass in progress.
    block: usize,
    /// Last completed pass.
    info: Option<PassInfo>,
    /// Accumulation rows changed since the UI last resolved them.
    dirty: Option<Range<usize>>,
}

struct Shared {
    scene: Mutex<Arc<Scene>>,
    camera: Mutex<crate::camera::CameraDesc>,
    /// Render resolution.
    size: Mutex<(usize, usize)>,
    /// Bumped to make the render thread restart.
    generation: AtomicU64,
    frame: Mutex<Frame>,
    /// Bumped whenever `frame` changes.
    version: AtomicU64,
    /// Samples traced in the current generation.
    samples: AtomicU64,
    clock: Mutex<Stopwatch>,
    paused: AtomicBool,
    finished: AtomicBool,
    quit: AtomicBool,
}

fn mark_dirty(d: &mut Option<Range<usize>>, r: Range<usize>) {
    *d = Some(match d.take() {
        Some(o) => o.start.min(r.start)..o.end.max(r.end),
        None => r,
    });
}

/// Progress sink of one render generation; stale generations write nothing.
struct Sink<'a> {
    shared: &'a Shared,
    generation: u64,
    w: usize,
}

impl Progress for Sink<'_> {
    fn cancelled(&self) -> bool {
        self.shared.quit.load(Ordering::Relaxed) || self.shared.generation.load(Ordering::Relaxed) != self.generation
    }

    fn paused(&self) -> bool {
        self.shared.paused.load(Ordering::Relaxed)
    }

    fn render_time(&self) -> Option<Duration> {
        Some(self.shared.clock.lock().unwrap().elapsed())
    }

    fn begin_pass(&self, info: &PassInfo) {
        let mut f = self.shared.frame.lock().unwrap();
        if f.generation == self.generation {
            f.block = info.block as usize;
        }
    }

    fn row_done(&self, y: usize, row: &[Pixel], samples: u64) {
        self.shared.samples.fetch_add(samples, Ordering::Relaxed);
        let mut f = self.shared.frame.lock().unwrap();
        if f.generation != self.generation {
            return;
        }
        let w = self.w;
        f.accum[y * w..(y + 1) * w].copy_from_slice(row);
        mark_dirty(&mut f.dirty, y..y + 1);
        drop(f);
        self.shared.version.fetch_add(1, Ordering::Release);
    }

    fn end_pass(&self, pixels: &[Pixel], info: &PassInfo) {
        let mut f = self.shared.frame.lock().unwrap();
        if f.generation != self.generation {
            return;
        }
        f.accum.copy_from_slice(pixels);
        f.info = Some(*info);
        let h = f.h;
        mark_dirty(&mut f.dirty, 0..h);
        if info.finished {
            self.shared.finished.store(true, Ordering::SeqCst);
            self.shared.clock.lock().unwrap().stop();
        }
        drop(f);
        self.shared.version.fetch_add(1, Ordering::Release);
    }
}

fn render_loop(shared: &Shared) {
    let mut rendered_generation = 0;
    while !shared.quit.load(Ordering::SeqCst) {
        let generation = shared.generation.load(Ordering::SeqCst);
        if generation == rendered_generation {
            std::thread::sleep(Duration::from_millis(4));
            continue;
        }
        rendered_generation = generation;
        let scene = shared.scene.lock().unwrap().clone();
        let camera = shared.camera.lock().unwrap().clone();
        let (w, h) = *shared.size.lock().unwrap();
        {
            let mut f = shared.frame.lock().unwrap();
            f.generation = generation;
            if (f.w, f.h) != (w, h) {
                // Same size: keep the old image visible under the new coarse preview.
                f.w = w;
                f.h = h;
                f.accum = vec![Pixel::default(); w * h];
                f.dirty = Some(0..h);
            }
            f.block = (scene.settings.initial_block.max(1) as usize).next_power_of_two();
            f.info = None;
        }
        shared.samples.store(0, Ordering::SeqCst);
        shared.finished.store(false, Ordering::SeqCst);
        shared.clock.lock().unwrap().reset(!shared.paused.load(Ordering::SeqCst));
        shared.version.fetch_add(1, Ordering::Release);
        renderer::render_progressive(&scene, &camera, w, h, &Sink { shared, generation, w });
    }
}

/// Polls file modification times and reports a change once writes have settled.
struct Watcher {
    files: Vec<(PathBuf, Option<SystemTime>)>,
    next_check: Instant,
    pending: Option<Instant>,
}

fn mtime(p: &std::path::Path) -> Option<SystemTime> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
}

impl Watcher {
    fn new(files: &[PathBuf], previous: Option<&Watcher>) -> Watcher {
        let files = files
            .iter()
            .map(|p| {
                // Keep the old timestamp for already-watched files so edits made during a reload are not missed.
                let known = previous.and_then(|w| w.files.iter().find(|(q, _)| q == p)).map(|(_, t)| *t);
                (p.clone(), known.unwrap_or_else(|| mtime(p)))
            })
            .collect();
        Watcher { files, next_check: Instant::now(), pending: None }
    }

    fn poll(&mut self) -> bool {
        let now = Instant::now();
        if now >= self.next_check {
            self.next_check = now + Duration::from_millis(250);
            for (p, t) in &mut self.files {
                let m = mtime(p);
                if m != *t {
                    *t = m;
                    self.pending = Some(now);
                }
            }
        }
        match self.pending {
            Some(t) if t.elapsed() >= Duration::from_millis(200) => {
                self.pending = None;
                true
            }
            _ => false,
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Drag {
    Orbit,
    Pan,
    Look,
}

struct App<'a> {
    args: &'a Args,
    window: Window,
    shared: Arc<Shared>,
    scene: Arc<Scene>,
    controller: CameraController,
    fly_speed: f32,

    exposure: f32,
    tonemap: Tonemap,
    encoder: Encoder,
    view: View,
    scale: u32,
    client: (u32, u32),

    /// Denoised view: toggled with N, recomputed after passes (throttled).
    denoise: bool,
    showing_denoised: bool,
    denoised: Vec<Vec3>,
    /// (generation, spp) the cached denoised image was computed from.
    denoise_key: Option<(u64, u32)>,
    last_denoise: Instant,

    display: Vec<u32>,
    display_size: (usize, usize),
    full_resolve: bool,
    needs_present: bool,
    last_version: u64,
    last_present: Instant,
    last_title: Instant,
    reported_generation: u64,

    hud: bool,
    help: bool,
    message: Option<(String, Instant)>,

    drag: Option<(Drag, i32, i32)>,
    held: HashSet<Key>,
    camera_dirty: bool,
    last_tick: Instant,
    pending_resize: Option<Instant>,

    watcher: Watcher,
    reload: Option<Receiver<Result<(Scene, Duration), String>>>,
}

fn fly_speed(scene: &Scene) -> f32 {
    let b = scene.bounds();
    let diag = (b.max - b.min).length();
    if diag.is_finite() && diag > 0.0 { 0.3 * diag } else { 1.0 }
}

fn render_size((cw, ch): (u32, u32), scale: u32) -> (usize, usize) {
    (cw.div_ceil(scale).max(1) as usize, ch.div_ceil(scale).max(1) as usize)
}

pub fn run(scene: Scene, args: &Args) -> Result<(), String> {
    let window = Window::new("PathTracer", scene.settings.width, scene.settings.height)?;
    let client = window.client_size();
    let scale = args.scale.max(1);
    let scene = Arc::new(scene);
    let controller = CameraController::new(&scene.camera);
    let shared = Arc::new(Shared {
        scene: Mutex::new(scene.clone()),
        camera: Mutex::new(controller.desc()),
        size: Mutex::new(render_size(client, scale)),
        generation: AtomicU64::new(1),
        frame: Mutex::new(Frame { generation: 0, w: 0, h: 0, accum: Vec::new(), block: 1, info: None, dirty: None }),
        version: AtomicU64::new(0),
        samples: AtomicU64::new(0),
        clock: Mutex::new(Stopwatch::default()),
        paused: AtomicBool::new(false),
        finished: AtomicBool::new(false),
        quit: AtomicBool::new(false),
    });

    let render_thread = {
        let shared = shared.clone();
        std::thread::spawn(move || render_loop(&shared))
    };

    let now = Instant::now();
    let mut app = App {
        args,
        window,
        shared: shared.clone(),
        controller,
        fly_speed: fly_speed(&scene),
        exposure: scene.settings.exposure,
        tonemap: scene.settings.tonemap,
        encoder: Encoder::new(scene.settings.exposure, scene.settings.tonemap),
        view: View::Color,
        scale,
        client,
        denoise: scene.output.denoise,
        showing_denoised: false,
        denoised: Vec::new(),
        denoise_key: None,
        last_denoise: now,
        display: Vec::new(),
        display_size: (0, 0),
        full_resolve: true,
        needs_present: true,
        last_version: 0,
        last_present: now,
        last_title: now - Duration::from_secs(1),
        reported_generation: 0,
        hud: true,
        help: false,
        message: Some(("F1: controls".into(), now)),
        drag: None,
        held: HashSet::new(),
        camera_dirty: false,
        last_tick: now,
        pending_resize: None,
        watcher: Watcher::new(&scene.files, None),
        reload: None,
        scene,
    };
    app.main_loop();

    shared.quit.store(true, Ordering::SeqCst);
    render_thread.join().map_err(|_| "render thread panicked")?;
    Ok(())
}

impl App<'_> {
    fn main_loop(&mut self) {
        loop {
            for ev in self.window.poll_events() {
                if !self.handle_event(ev) {
                    return;
                }
            }
            self.tick_fly();
            if self.camera_dirty {
                self.camera_dirty = false;
                *self.shared.camera.lock().unwrap() = self.controller.desc();
                self.restart();
            }
            if let Some(t) = self.pending_resize
                && t.elapsed() > Duration::from_millis(200)
            {
                self.pending_resize = None;
                self.apply_size();
            }
            self.poll_reload();
            self.update_display();
            let interacting = self.drag.is_some() || !self.held.is_empty();
            std::thread::sleep(Duration::from_millis(if interacting { 4 } else { 8 }));
        }
    }

    /// Restarts rendering with the current shared camera / scene / size.
    fn restart(&mut self) {
        self.shared.paused.store(false, Ordering::SeqCst);
        self.shared.generation.fetch_add(1, Ordering::SeqCst);
    }

    fn apply_size(&mut self) {
        let size = render_size(self.client, self.scale);
        let mut s = self.shared.size.lock().unwrap();
        if *s != size {
            *s = size;
            drop(s);
            self.restart();
        }
    }

    fn notify(&mut self, msg: impl Into<String>) {
        self.message = Some((msg.into(), Instant::now()));
        self.needs_present = true;
    }

    fn set_view(&mut self, exposure: f32, tonemap: Tonemap) {
        self.exposure = exposure;
        self.tonemap = tonemap;
        self.encoder = Encoder::new(exposure, tonemap);
        self.full_resolve = true;
    }

    /// Returns false to quit.
    fn handle_event(&mut self, ev: Event) -> bool {
        match ev {
            Event::Close => return false,
            Event::Resized(w, h) => {
                self.client = (w, h);
                self.pending_resize = Some(Instant::now());
                self.needs_present = true;
            }
            Event::Paint => self.needs_present = true,
            Event::FocusLost => {
                self.held.clear();
                self.drag = None;
            }
            Event::KeyUp { key, .. } => {
                self.held.remove(&key);
            }
            Event::KeyDown { key, mods, repeat } => return self.key_down(key, mods, repeat),
            Event::MouseDown { button, x, y, mods } => {
                if self.drag.is_none() {
                    let mode = match button {
                        MouseButton::Left if mods.shift => Drag::Pan,
                        MouseButton::Left => Drag::Orbit,
                        MouseButton::Middle => Drag::Pan,
                        MouseButton::Right => Drag::Look,
                    };
                    self.drag = Some((mode, x, y));
                }
            }
            Event::MouseUp { .. } => self.drag = None,
            Event::MouseMove { x, y, .. } => {
                if let Some((mode, lx, ly)) = self.drag {
                    let (dx, dy) = ((x - lx) as f32, (y - ly) as f32);
                    if dx != 0.0 || dy != 0.0 {
                        match mode {
                            Drag::Orbit => self.controller.orbit(dx, dy),
                            Drag::Look => self.controller.look(dx, dy),
                            Drag::Pan => self.controller.pan(dx, dy, self.client.1 as f32),
                        }
                        self.camera_dirty = true;
                    }
                    self.drag = Some((mode, x, y));
                }
            }
            Event::Wheel { delta, mods } => {
                if mods.shift {
                    self.controller.zoom_fov(delta);
                } else {
                    self.controller.dolly(delta);
                }
                self.camera_dirty = true;
            }
        }
        true
    }

    fn key_down(&mut self, key: Key, mods: Modifiers, repeat: bool) -> bool {
        const FLY_KEYS: [Key; 10] = [
            Key::Char('w'),
            Key::Char('a'),
            Key::Char('s'),
            Key::Char('d'),
            Key::Char('q'),
            Key::Char('e'),
            Key::Up,
            Key::Down,
            Key::Left,
            Key::Right,
        ];
        if mods.ctrl {
            if key == Key::Char('s') && !repeat {
                self.save();
            }
            return true;
        }
        if FLY_KEYS.contains(&key) || key == Key::Shift {
            self.held.insert(key);
            return true;
        }
        match key {
            Key::Escape => return false,
            Key::Plus => self.set_view(self.exposure + 0.5, self.tonemap),
            Key::Minus => self.set_view(self.exposure - 0.5, self.tonemap),
            _ if repeat => {}
            Key::Char('0') => self.set_view(self.scene.settings.exposure, self.tonemap),
            Key::Char('t') => self.set_view(self.exposure, self.tonemap.next()),
            Key::Char('v') => {
                self.view = self.view.next();
                self.full_resolve = true;
                self.notify(format!("view: {}", self.view.name()));
            }
            Key::Char('n') => {
                self.denoise = !self.denoise;
                self.full_resolve = true;
                self.notify(if self.denoise { "denoiser on" } else { "denoiser off" });
            }
            Key::Char(c @ '1'..='4') => {
                self.scale = c as u32 - '0' as u32;
                self.apply_size();
                self.notify(format!("resolution scale 1/{}", self.scale));
            }
            Key::Char('r') => self.restart(),
            Key::Char('h') => {
                self.hud = !self.hud;
                self.needs_present = true;
            }
            Key::F(1) => {
                self.help = !self.help;
                self.needs_present = true;
            }
            Key::Home => {
                self.controller = CameraController::new(&self.scene.camera);
                self.camera_dirty = true;
            }
            Key::Space => self.toggle_pause(),
            _ => {}
        }
        true
    }

    fn toggle_pause(&mut self) {
        let paused = !self.shared.paused.load(Ordering::SeqCst);
        self.shared.paused.store(paused, Ordering::SeqCst);
        let mut clock = self.shared.clock.lock().unwrap();
        if paused {
            clock.stop();
        } else if !self.shared.finished.load(Ordering::SeqCst) {
            clock.start();
        }
        drop(clock);
        self.notify(if paused { "paused" } else { "resumed" });
    }

    fn tick_fly(&mut self) {
        let dt = self.last_tick.elapsed().as_secs_f32().min(0.1);
        self.last_tick = Instant::now();
        let held = |k: Key| self.held.contains(&k) as i32 as f32;
        let c = Key::Char;
        let local = crate::math::Vec3::new(
            held(c('d')) + held(Key::Right) - held(c('a')) - held(Key::Left),
            held(c('e')) - held(c('q')),
            held(c('w')) + held(Key::Up) - held(c('s')) - held(Key::Down),
        );
        if local.length_squared() > 0.0 {
            let speed = self.fly_speed * if self.held.contains(&Key::Shift) { 4.0 } else { 1.0 };
            self.controller.fly(local * (speed * dt));
            self.camera_dirty = true;
        }
    }

    /// Saves all configured outputs of the current accumulation buffer.
    fn save(&mut self) {
        let f = self.shared.frame.lock().unwrap();
        if f.info.is_none() {
            drop(f);
            self.notify("nothing to save yet");
            return;
        }
        let pixels = f.accum.clone();
        let (w, h, block) = (f.w, f.h, f.block);
        drop(f);
        let snap = Snapshot { pixels: &pixels, w, h, block };
        match save_outputs(&self.scene, self.args, &snap, &self.encoder, "renders/{scene}_{time}") {
            Ok(files) => {
                let first = files.first().map(|p| p.display().to_string()).unwrap_or_default();
                let more = if files.len() > 1 { format!(" (+{} more)", files.len() - 1) } else { String::new() };
                self.notify(format!("saved {first}{more}"));
            }
            Err(e) => {
                eprintln!("error: {e}");
                self.notify(e);
            }
        }
    }

    /// Distance mapped to black in the depth view: the far side of the scene, but
    /// not much beyond the camera's focus region (huge ground planes would wash it out).
    fn depth_scale(&self) -> f32 {
        let cam = self.controller.desc();
        let b = self.scene.bounds();
        let mut far = 0.0f32;
        for i in 0..8 {
            let corner = Vec3::new(
                if i & 1 == 0 { b.min.x } else { b.max.x },
                if i & 2 == 0 { b.min.y } else { b.max.y },
                if i & 4 == 0 { b.min.z } else { b.max.z },
            );
            far = far.max((corner - cam.position).length());
        }
        let focus = (cam.look_at - cam.position).length() * 3.0;
        let s = if focus > 0.0 { far.min(focus) } else { far };
        if s.is_finite() && s > 0.0 { s } else { 1.0 }
    }

    fn poll_reload(&mut self) {
        if let Some(rx) = &self.reload {
            match rx.try_recv() {
                Err(TryRecvError::Empty) => return,
                Err(TryRecvError::Disconnected) => {
                    self.reload = None;
                    self.notify("reload failed (loader panicked)");
                }
                Ok(result) => {
                    self.reload = None;
                    match result {
                        Ok((scene, took)) => self.apply_reload(scene, took),
                        Err(e) => {
                            eprintln!("reload error: {e}");
                            self.notify(format!("reload error: {e}"));
                        }
                    }
                }
            }
        }
        if self.reload.is_none() && self.watcher.poll() {
            let (tx, rx) = std::sync::mpsc::channel();
            let args = self.args.clone();
            std::thread::spawn(move || {
                let t0 = Instant::now();
                let r = loader::load_scene(&args.scene).map(|mut s| {
                    apply_overrides(&mut s, &args);
                    (s, t0.elapsed())
                });
                let _ = tx.send(r);
            });
            self.reload = Some(rx);
            self.notify("reloading...");
        }
    }

    fn apply_reload(&mut self, scene: Scene, took: Duration) {
        let old = &self.scene.settings;
        // Keep interactive changes unless the file itself changed that setting.
        if scene.camera != self.scene.camera {
            self.controller = CameraController::new(&scene.camera);
            *self.shared.camera.lock().unwrap() = self.controller.desc();
        }
        let exposure = if scene.settings.exposure != old.exposure { scene.settings.exposure } else { self.exposure };
        let tonemap = if scene.settings.tonemap != old.tonemap { scene.settings.tonemap } else { self.tonemap };
        self.set_view(exposure, tonemap);
        self.fly_speed = fly_speed(&scene);
        self.watcher = Watcher::new(&scene.files, Some(&self.watcher));
        println!("reloaded {} in {:.2}s: {}", self.args.scene.display(), took.as_secs_f32(), scene.stats());
        let scene = Arc::new(scene);
        *self.shared.scene.lock().unwrap() = scene.clone();
        self.scene = scene;
        self.restart();
        self.notify(format!("reloaded in {:.2}s", took.as_secs_f32()));
    }

    fn update_display(&mut self) {
        let version = self.shared.version.load(Ordering::Acquire);
        let changed = version != self.last_version || self.full_resolve;
        let throttle = self.last_present.elapsed();
        let refresh_hud = self.hud && throttle > Duration::from_millis(500);
        if !(self.needs_present || refresh_hud || (changed && throttle >= Duration::from_millis(30))) {
            return self.update_title();
        }

        let depth_scale = self.depth_scale();
        let threshold = self.scene.settings.noise_threshold;
        let threads = renderer::thread_count(&self.scene);
        let (w, h, block, info) = {
            let mut f = self.shared.frame.lock().unwrap();
            if (f.w, f.h) != self.display_size {
                self.display = vec![0; f.w * f.h];
                self.display_size = (f.w, f.h);
                self.full_resolve = true;
            }
            let (w, h, block, info) = (f.w, f.h, f.block, f.info);
            // The denoiser needs every pixel sampled, i.e. a completed full-resolution pass.
            let denoise_view = self.denoise && self.view == View::Color && info.is_some_and(|i| i.block == 1);
            if denoise_view != self.showing_denoised {
                self.showing_denoised = denoise_view;
                self.full_resolve = true;
            }
            if let (true, Some(i)) = (denoise_view, info) {
                f.dirty = None;
                let key = (f.generation, i.spp);
                let stale = self.denoise_key.is_none_or(|k| k.0 != key.0);
                if self.denoise_key != Some(key)
                    && (stale || i.finished || self.last_denoise.elapsed() >= DENOISE_INTERVAL)
                {
                    let pixels = f.accum.clone();
                    drop(f);
                    self.denoised = denoise(&pixels, w, h, threads);
                    self.denoise_key = Some(key);
                    self.last_denoise = Instant::now();
                    self.full_resolve = true;
                }
                if self.full_resolve {
                    for (d, c) in self.display.iter_mut().zip(&self.denoised) {
                        *d = self.encoder.encode(*c);
                    }
                }
            } else {
                let rows = if self.full_resolve { Some(0..h) } else { f.dirty.clone() };
                f.dirty = None;
                if let Some(rows) = rows {
                    let shader = Shader {
                        enc: &self.encoder,
                        view: self.view,
                        depth_scale,
                        error_scale: if threshold > 0.0 { threshold } else { 0.02 },
                        max_spp: info.map_or(1, |i| i.spp),
                    };
                    renderer::resolve_rows(&f.accum, w, h, block, rows, &shader, &mut self.display);
                }
            }
            (w, h, block, info)
        };
        self.full_resolve = false;
        self.last_version = version;

        let generation = self.shared.generation.load(Ordering::SeqCst);
        if let Some(i) = info
            && i.finished
            && self.reported_generation != generation
        {
            self.reported_generation = generation;
            let secs = self.shared.clock.lock().unwrap().elapsed().as_secs_f32();
            let samples = self.shared.samples.load(Ordering::Relaxed);
            let rate = samples as f32 / secs.max(1e-6) / 1e6;
            let avg = samples as f32 / (w * h).max(1) as f32;
            println!("done: {w}x{h}, {} in {secs:.2}s, {avg:.1} spp average ({rate:.2} Msamples/s)", pass_summary(&i));
            if self.scene.output.save_on_finish {
                self.save();
            }
        }

        let overlay = self.overlay(w, h, block, info);
        self.window.present(&self.display, w, h, &overlay);
        self.needs_present = false;
        self.last_present = Instant::now();
        self.update_title();
    }

    fn progress_text(block: usize, info: Option<PassInfo>, target: u32) -> String {
        match info {
            _ if block > 1 => format!("preview 1/{block}"),
            Some(i) => pass_summary(&i),
            None => format!("0/{target} spp"),
        }
    }

    fn overlay(&mut self, w: usize, h: usize, block: usize, info: Option<PassInfo>) -> Vec<String> {
        let mut lines = Vec::new();
        if self.hud {
            let secs = self.shared.clock.lock().unwrap().elapsed().as_secs_f32();
            let rate = self.shared.samples.load(Ordering::Relaxed) as f32 / secs.max(1e-6) / 1e6;
            let scale = if self.scale > 1 { format!(" (1/{} res)", self.scale) } else { String::new() };
            let paused = if self.shared.paused.load(Ordering::Relaxed) { "  [paused]" } else { "" };
            lines.push(format!("{w}x{h}{scale}  {}", Self::progress_text(block, info, self.scene.settings.spp)));
            lines.push(format!("{secs:.1}s  {rate:.2} Msamples/s{paused}"));
            let denoise = match (self.denoise, self.showing_denoised) {
                (false, _) => "",
                (true, true) => "  denoised",
                (true, false) if self.view == View::Color => "  denoise: waiting for full res",
                (true, false) => "  denoise: color view only",
            };
            lines.push(format!(
                "exposure {:+.1} EV  tonemap {}  view {}{denoise}",
                self.exposure,
                self.tonemap.name(),
                self.view.name()
            ));
        }
        if let Some((msg, t)) = &self.message {
            if t.elapsed() < Duration::from_secs(4) {
                lines.push(msg.clone());
            } else {
                self.message = None;
            }
        }
        if self.help {
            lines.extend(HELP.iter().map(|s| s.to_string()));
        }
        lines
    }

    fn update_title(&mut self) {
        if self.last_title.elapsed() < Duration::from_millis(250) {
            return;
        }
        self.last_title = Instant::now();
        let (w, h, block, info) = {
            let f = self.shared.frame.lock().unwrap();
            (f.w, f.h, f.block, f.info)
        };
        let secs = self.shared.clock.lock().unwrap().elapsed().as_secs_f32();
        let progress = Self::progress_text(block, info, self.scene.settings.spp);
        let paused = if self.shared.paused.load(Ordering::Relaxed) { " — paused" } else { "" };
        self.window
            .set_title(&format!("PathTracer — {} — {w}x{h} — {progress} — {secs:.1}s{paused}", scene_stem(self.args)));
    }
}
