//! The path-traced view: the tracer's progressive renderer drawing the perspective view on
//! a thread of its own, restarted whenever the scene, the view's camera, its size or the
//! render region changes. Edits that only move things refit the scene in place (see
//! `Scene::transform_object`); for that the render lets go of the scene first (`release`).

use pt_core::camera::CameraDesc;
use pt_core::image::Encoder;
use pt_core::renderer::{self, PassInfo, Pixel, Progress, Region, Shader, View};
use pt_core::scene::Scene;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

struct Job {
    scene: Arc<Scene>,
    camera: CameraDesc,
    full: (usize, usize),
    region: Region,
}

/// The render's pixels, written by the workers row by row.
struct Frame {
    generation: u64,
    pixels: Vec<Pixel>,
    region: Region,
    block: usize,
    spp: u32,
    finished: bool,
}

struct Shared {
    job: Mutex<Option<Job>>,
    generation: AtomicU64,
    /// The render thread holds a scene.
    holding: AtomicBool,
    frame: Mutex<Frame>,
    /// Bumped whenever `frame` changes.
    version: AtomicU64,
    quit: AtomicBool,
}

struct Sink<'a> {
    shared: &'a Shared,
    generation: u64,
}

impl Progress for Sink<'_> {
    fn cancelled(&self) -> bool {
        self.shared.quit.load(Ordering::Relaxed) || self.shared.generation.load(Ordering::Relaxed) != self.generation
    }

    fn begin_pass(&self, info: &PassInfo) {
        let mut f = self.shared.frame.lock().expect("frame lock");
        if f.generation == self.generation {
            f.block = info.block as usize;
        }
    }

    fn row_done(&self, y: usize, row: &[Pixel], _samples: u64) {
        let mut f = self.shared.frame.lock().expect("frame lock");
        if f.generation != self.generation {
            return;
        }
        let w = f.region.w;
        if let Some(dst) = f.pixels.get_mut(y * w..(y + 1) * w) {
            dst.copy_from_slice(row);
        }
        drop(f);
        self.shared.version.fetch_add(1, Ordering::Release);
    }

    fn end_pass(&self, pixels: &[Pixel], info: &PassInfo) {
        let mut f = self.shared.frame.lock().expect("frame lock");
        if f.generation != self.generation {
            return;
        }
        if f.pixels.len() == pixels.len() {
            f.pixels.copy_from_slice(pixels);
        }
        f.spp = info.spp;
        f.finished = info.finished;
        drop(f);
        self.shared.version.fetch_add(1, Ordering::Release);
    }
}

fn render_loop(shared: &Shared) {
    let mut done = 0;
    while !shared.quit.load(Ordering::SeqCst) {
        let generation = shared.generation.load(Ordering::SeqCst);
        if generation == done {
            std::thread::sleep(Duration::from_millis(3));
            continue;
        }
        done = generation;
        let job = {
            let mut j = shared.job.lock().expect("job lock");
            let job = j.take();
            // Set while the lock is held, so `release` cannot miss it.
            shared.holding.store(job.is_some(), Ordering::SeqCst);
            job
        };
        let Some(job) = job else { continue };
        {
            let mut f = shared.frame.lock().expect("frame lock");
            f.generation = generation;
            if f.region != job.region {
                // The same region keeps showing the last image under the new preview.
                f.pixels = vec![Pixel::default(); job.region.w * job.region.h];
                f.region = job.region;
            }
            f.block = (job.scene.settings.initial_block.max(1) as usize).next_power_of_two();
            f.spp = 0;
            f.finished = false;
        }
        shared.version.fetch_add(1, Ordering::Release);
        let mut camera = job.camera.clone();
        camera.motion = None;
        renderer::render_region(&job.scene, &camera, job.full, job.region, &Sink { shared, generation });
        drop(job);
        shared.holding.store(false, Ordering::SeqCst);
    }
}

/// What a render is of: if any of it changes, the render starts again.
#[derive(Clone, Debug, PartialEq)]
struct Key {
    scene: u64,
    camera: CameraDesc,
    full: (usize, usize),
    region: Region,
}

pub struct PtView {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
    key: Option<Key>,
    /// The resolved image of `region`, and the frame version it shows.
    pub image: Vec<u32>,
    pub region: Region,
    seen: u64,
    exposure: f32,
    tonemap: pt_core::image::Tonemap,
}

impl Default for PtView {
    fn default() -> PtView {
        PtView {
            shared: Arc::new(Shared {
                job: Mutex::new(None),
                generation: AtomicU64::new(0),
                holding: AtomicBool::new(false),
                frame: Mutex::new(Frame {
                    generation: 0,
                    pixels: Vec::new(),
                    region: Region { x: 0, y: 0, w: 0, h: 0 },
                    block: 1,
                    spp: 0,
                    finished: false,
                }),
                version: AtomicU64::new(0),
                quit: AtomicBool::new(false),
            }),
            thread: None,
            key: None,
            image: Vec::new(),
            region: Region { x: 0, y: 0, w: 0, h: 0 },
            seen: u64::MAX,
            exposure: 0.0,
            tonemap: pt_core::image::Tonemap::Aces,
        }
    }
}

impl PtView {
    /// Renders `scene` (whose version is `scene_gen`) from `camera` into `region` of a
    /// `full`-sized image, unless that is what is being rendered already.
    pub fn update(
        &mut self,
        scene: &Arc<Scene>,
        scene_gen: u64,
        camera: CameraDesc,
        full: (usize, usize),
        region: Region,
    ) {
        let key = Key { scene: scene_gen, camera, full, region };
        if self.key.as_ref() == Some(&key) {
            return;
        }
        if self.thread.is_none() {
            let shared = Arc::clone(&self.shared);
            self.thread =
                std::thread::Builder::new().name("ptedit-render".into()).spawn(move || render_loop(&shared)).ok();
        }
        self.exposure = scene.settings.exposure;
        self.tonemap = scene.settings.tonemap;
        *self.shared.job.lock().expect("job lock") =
            Some(Job { scene: Arc::clone(scene), camera: key.camera.clone(), full, region });
        self.shared.generation.fetch_add(1, Ordering::SeqCst);
        self.key = Some(key);
    }

    /// Stops the render and waits until the render thread no longer holds the scene.
    pub fn release(&mut self) {
        if self.key.take().is_none() && !self.shared.holding.load(Ordering::SeqCst) {
            return;
        }
        self.shared.generation.fetch_add(1, Ordering::SeqCst);
        *self.shared.job.lock().expect("job lock") = None;
        let start = Instant::now();
        while self.shared.holding.load(Ordering::SeqCst) && start.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_micros(200));
        }
    }

    /// Whether the image changed since `refresh` last ran.
    pub fn changed(&self) -> bool {
        self.key.is_some() && self.shared.version.load(Ordering::Acquire) != self.seen
    }

    /// Resolves the render's pixels into `image` (if they changed).
    pub fn refresh(&mut self) {
        let version = self.shared.version.load(Ordering::Acquire);
        if version == self.seen {
            return;
        }
        self.seen = version;
        let (pixels, region, block) = {
            let f = self.shared.frame.lock().expect("frame lock");
            (f.pixels.clone(), f.region, f.block)
        };
        let enc = Encoder::new(self.exposure, self.tonemap);
        let shader = Shader { enc: &enc, view: View::Color, depth_scale: 1.0, error_scale: 1.0, max_spp: 1 };
        self.image.resize(region.w * region.h, 0);
        if pixels.len() == region.w * region.h && region.w > 0 {
            renderer::resolve_rows(&pixels, region.w, region.h, block, 0..region.h, &shader, &mut self.image);
        }
        self.region = region;
    }

    /// Samples per pixel so far and whether the render is done.
    pub fn progress(&self) -> (u32, bool) {
        let f = self.shared.frame.lock().expect("frame lock");
        (f.spp, f.finished)
    }

    /// Whether a render is under way (frames should keep coming to show it).
    pub fn running(&self) -> bool {
        self.key.is_some() && !self.progress().1
    }

    /// Waits (for tests and snapshots) until the render reaches `spp` samples or ends.
    pub fn wait_for(&self, spp: u32, timeout: Duration) {
        let start = Instant::now();
        while start.elapsed() < timeout {
            let (s, done) = self.progress();
            if done || s >= spp {
                return;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}

impl Drop for PtView {
    fn drop(&mut self) {
        self.shared.quit.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            t.join().ok();
        }
    }
}
