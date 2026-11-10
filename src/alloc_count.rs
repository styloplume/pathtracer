//! Test-only global allocator that counts allocations per thread, used to check that the
//! rendering hot path (tracing a path: intersection, shading, light sampling, media,
//! guiding) never touches the heap: an allocation per path costs more than many of the
//! operations it is made of, and the allocator's locks would serialize the workers.

#![expect(unsafe_code, reason = "implementing GlobalAlloc requires unsafe; it only forwards to System")]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

thread_local! {
    static ALLOCATIONS: Cell<u64> = const { Cell::new(0) };
}

struct Counting;

impl Counting {
    fn count() {
        // `try_with`: allocations during thread teardown, after the counter is gone, are not counted.
        let _ = ALLOCATIONS.try_with(|c| c.set(c.get() + 1));
    }
}

// SAFETY: every method forwards its arguments unchanged to the system allocator, which upholds
// the GlobalAlloc contract; counting touches only a const-initialized thread-local `Cell`,
// which never allocates.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        Self::count();
        // SAFETY: forwarded from our caller, who upholds `alloc`'s contract.
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        Self::count();
        // SAFETY: as for `alloc`.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        Self::count();
        // SAFETY: `ptr` / `layout` come from this allocator (i.e. from System), per our caller.
        unsafe { System.realloc(ptr, layout, new_size) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: as for `realloc`.
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

/// Runs `f` and returns its result with the number of heap allocations it made on this thread.
pub fn count_allocations<R>(f: impl FnOnce() -> R) -> (R, u64) {
    let before = ALLOCATIONS.with(Cell::get);
    let r = f();
    (r, ALLOCATIONS.with(Cell::get) - before)
}

#[cfg(test)]
mod tests {
    use super::count_allocations;
    use crate::camera::Camera;
    use crate::guiding::Guide;
    use crate::integrator::radiance;
    use crate::sampler::Sampler;

    #[test]
    fn counter_sees_allocations() {
        let (v, n) = count_allocations(|| vec![1u8; 100]);
        assert_eq!(v.len(), 100);
        assert!(n >= 1);
        assert_eq!(count_allocations(|| 2 + 2).1, 0);
    }

    /// Tracing paths in every example scene, in RGB and spectral mode, plain and with a
    /// recording and a trained path guide, allocates nothing.
    #[test]
    fn path_tracing_does_not_allocate() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("scenes");
        let mut scenes: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.extension().is_some_and(|e| e == "toml"))
            .collect();
        scenes.sort();
        assert_ne!(scenes.len(), 0);
        for path in scenes {
            let mut scene = crate::loader::load_scene(&path).unwrap();
            let (w, h) = (24u32, 16u32);
            let camera = Camera::new(&scene.camera, w as f32 / h as f32);
            for spectral in [false, true] {
                scene.settings.spectral = spectral;
                let mut guide = Guide::new(&scene.bounds());
                for phase in 0..3 {
                    // 0: no guide, 1: recording guide, 2: trained guide (sampling and recording).
                    let g = (phase > 0).then_some(&guide);
                    let ((), n) = count_allocations(|| {
                        for y in 0..h {
                            for x in 0..w {
                                let mut s = Sampler::new(scene.settings.sampler, x, y, w, phase, 0);
                                let (u1, u2) = s.get2d();
                                let st = ((x as f32 + u1) / w as f32, (y as f32 + u2) / h as f32);
                                if let Some(ray) = camera.ray(st.0, st.1, &mut s) {
                                    std::hint::black_box(radiance(&scene, ray, &mut s, g));
                                }
                            }
                        }
                    });
                    assert_eq!(n, 0, "{}: {n} allocations (spectral {spectral}, phase {phase})", path.display());
                    if phase == 1 {
                        guide.end_iteration(1);
                    }
                }
            }
        }
    }
}
