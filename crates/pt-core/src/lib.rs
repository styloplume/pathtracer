//! The path tracer's core, shared by the `pathtracer` and `ptedit` binaries: the scene
//! model and its TOML format, file loaders, BVH, renderer, and the native platform layer.

#[cfg(test)]
mod alloc_count;
pub mod bvh;
pub mod camera;
pub mod controls;
pub mod deflate;
pub mod denoise;
pub mod document;
pub mod environment;
pub mod filter;
pub mod geometry;
pub mod gltf;
pub mod guiding;
pub mod image;
pub mod imageio;
pub mod implicit;
pub mod integrator;
pub mod jpeg;
pub mod json;
pub mod lights;
pub mod loader;
pub mod material;
pub mod math;
pub mod medium;
pub mod mesh;
pub mod mtl;
pub mod noise;
pub mod output;
pub mod platform;
#[cfg(test)]
mod regression;
pub mod renderer;
pub mod rng;
pub mod sampler;
pub mod sampling;
pub mod scene;
pub mod schema;
pub mod shapes;
pub mod sky;
pub mod spectrum;
pub mod texture;
pub mod toml;

/// The repository root, where `scenes/` and `testdata/` live (for tests).
#[cfg(test)]
pub(crate) fn repo_root() -> std::path::PathBuf {
    let crate_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    crate_dir.parent().and_then(std::path::Path::parent).expect("crates/pt-core is two levels deep").to_path_buf()
}
