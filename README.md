# PathTracer

A progressive, physically based path tracer in Rust with **zero external crates** — only `std`.

```
cargo run --release -- scenes/cornell.toml               # interactive window
cargo run --release -- scenes/spheres.toml --headless    # render to renders/spheres.png
```

Options: `--headless`, `-o FILE`, `--spp N`, `--size WxH`, `--threads N`.
Window keys: `Esc` quit · `S` save PNG · `R` restart. Resizing the window restarts the render at the new resolution.

## Features

- **Progressive display** filling the whole window: coarse 32×32 blocks refine by halving down to 1×1 (each level only traces pixels not yet traced, so the preview costs nothing extra), then whole-image passes accumulate samples per pixel.
- **Integrator:** unidirectional path tracing, next-event estimation with power-proportional light selection, MIS (power heuristic), Russian roulette.
- **Materials:** Lambertian `diffuse`, GGX `metal` (VNDF sampling, Schlick Fresnel, roughness 0 = mirror), smooth `glass` (exact Fresnel), area `light` (one- or two-sided).
- **Geometry:** spheres, quads, triangles, meshes; binned-SAH BVH.
- **Formats:** scenes in TOML (own parser); meshes in OBJ (polygons, normals, negative indices, `usemtl` → scene material by name) and PLY (ASCII / binary LE / BE).
- **Output:** ACES / Reinhard / clamp tonemapping, exposure, sRGB; PNG writer.
- **Window:** raw Win32 FFI + GDI blitting (Windows only for now; `--headless` works everywhere).

## Layout

| File | Purpose |
|---|---|
| `src/main.rs` | CLI, UI thread / render thread wiring |
| `src/renderer.rs` | progressive coarse-to-fine scheduling, threading, display resolve |
| `src/integrator.rs` | path tracing with NEE + MIS |
| `src/material.rs` | BSDFs |
| `src/scene.rs` | scene data, BVH traversal, light sampling |
| `src/bvh.rs` | SAH BVH builder |
| `src/loader.rs` | TOML → scene |
| `src/toml.rs`, `src/mesh.rs`, `src/image.rs` | parsers / writers |
| `src/platform/` | native window |

See `scenes/cornell.toml` for an annotated scene file.
