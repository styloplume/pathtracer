# PathTracer

A progressive, physically based path tracer in Rust with **zero external crates** — only `std`.

```
cargo run --release -- scenes/cornell.toml               # interactive window
cargo run --release -- scenes/spheres.toml --headless    # render to renders/spheres.png
```

Options: `--headless`, `-o FILE` (`.png` / `.hdr` / `.pfm` picks the format), `--spp N`, `--size WxH`, `--threads N`,
`--time SECONDS` (time limit), `--noise T` (adaptive sampling threshold, e.g. `0.02`), `--denoise` (also save a
denoised image), `--autosave` (window: save when a render completes), `--scale N` (render at 1/N window resolution).

Window controls (`F1` shows them in the window):

| Input | Action |
|---|---|
| Left drag | orbit around the target |
| Shift + left drag / middle drag | pan |
| Right drag | look around (camera stays in place) |
| Wheel / Shift + wheel | dolly / change field of view |
| `W A S D`, arrows, `Q E` | fly (hold `Shift` for 4× speed) |
| `Home` | reset to the scene's camera |
| `+` / `-` / `0` | exposure ±½ stop / reset (no re-render) |
| `T` | cycle tonemapper (no re-render) |
| `V` | cycle view: color / albedo / normal / depth / noise heat map / samples heat map |
| `N` | toggle the denoised view (updates after each pass once full resolution is reached) |
| `1`–`4` | resolution scale 1/1 … 1/4 |
| `Space` | pause / resume |
| `R` | restart render |
| `H` | toggle HUD (resolution, spp, time, samples/s) |
| `Ctrl+S` | save all configured outputs (with the current exposure / tonemap) |
| `Esc` | quit |

Any camera change restarts the progressive render. Rows appear as they finish, so long passes are visible
while they run. Saving the scene file (or a mesh it references) reloads it automatically. Interactive camera,
exposure and tonemap changes are kept unless that part of the file changed.

## Features

- **Progressive display** filling the whole window: coarse 32×32 blocks refine by halving down to 1×1 (each level only traces pixels not yet traced, so the preview costs nothing extra), then whole-image passes accumulate samples per pixel.
- **Integrator:** unidirectional path tracing, next-event estimation with power-proportional light selection, MIS (power heuristic), Russian roulette.
- **Materials:** Lambertian `diffuse`, GGX `metal` (VNDF sampling, Schlick Fresnel, roughness 0 = mirror), smooth `glass` (exact Fresnel), area `light` (one- or two-sided).
- **Geometry:** spheres, quads, triangles, meshes; binned-SAH BVH.
- **Formats:** scenes in TOML (own parser); meshes in OBJ (polygons, normals, negative indices, `usemtl` → scene material by name) and PLY (ASCII / binary LE / BE).
- **Sampling:** reconstruction filters (box, tent, Gaussian, Blackman-Harris) via filter importance sampling; per-pixel variance estimate; adaptive sampling (converged pixels stop, the rest keep going) with noise-threshold, time-limit and spp stop criteria.
- **Denoiser:** edge-aware à-trous wavelet filter (SVGF-style, spatial), guided by albedo / normal / depth AOVs and the per-pixel variance; albedo is demodulated so material detail stays sharp.
- **Output:** ACES / Reinhard / clamp tonemapping, exposure, sRGB; PNG (own DEFLATE: LZ77 + fixed/dynamic Huffman, row filters), Radiance `.hdr` (RLE RGBE) and `.pfm`; denoised copy and albedo / normal / depth AOV images; optional save on completion.
- **Window:** raw Win32 FFI + GDI blitting (Windows only for now; `--headless` works everywhere).

## Layout

| File | Purpose |
|---|---|
| `src/main.rs` | CLI, headless rendering |
| `src/app.rs` | windowed front end: UI thread, render thread, input, HUD, hot reload |
| `src/controls.rs` | orbit / pan / look / fly camera controller |
| `src/renderer.rs` | progressive coarse-to-fine scheduling, adaptive sampling, threading, progress callbacks, display views |
| `src/integrator.rs` | path tracing with NEE + MIS, AOVs |
| `src/filter.rs` | reconstruction filters |
| `src/denoise.rs` | à-trous denoiser |
| `src/output.rs` | output naming, formats, denoised / AOV images |
| `src/deflate.rs` | DEFLATE / zlib compressor and decompressor |
| `src/material.rs` | BSDFs |
| `src/scene.rs` | scene data, BVH traversal, light sampling |
| `src/bvh.rs` | SAH BVH builder |
| `src/loader.rs` | TOML → scene |
| `src/toml.rs`, `src/mesh.rs`, `src/image.rs` | parsers / writers |
| `src/platform/` | native window |

See `scenes/cornell.toml` for an annotated scene file.
