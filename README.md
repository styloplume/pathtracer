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
- **Materials** (BSDFs are weighted mixtures of lobes):
  - `diffuse` (Lambert);
  - `metal`: GGX with VNDF sampling, Schlick F0 or measured complex IOR (`preset = "gold"`, silver, copper, aluminium, iron, chromium, platinum, or `eta` / `k`), Kulla–Conty multiple-scattering energy compensation;
  - `glass`: smooth or GGX-rough reflection + transmission, Beer–Lambert absorption (`color` after `distance`);
  - `plastic`: energy-conserving diffuse base under a smooth or rough dielectric coat;
  - `principled`: base color, metallic, roughness, specular / IOR, transmission, clearcoat, sheen;
  - `light`: area emitter, optionally textured, one- or two-sided.
  - Every material takes `normal_map` (tangent space), `bump` (height texture) and `alpha` (cut-out / stochastic coverage).
- **Textures:** any color or scalar parameter can be a constant, an image (bilinear / nearest, repeat / clamp / mirror, UV scale / offset, sRGB or linear, channel select), a checker or Perlin noise / turbulence (UV or world space). Texture coordinates come from OBJ `vt`, PLY `u v` / `s t`, quads (0..1) and spheres (latitude-longitude).
- **Image loaders** (own code): PNG (all color types and bit depths, palette, tRNS, Adam7), baseline JPEG (any chroma subsampling, restart markers), TGA (raw / RLE), PPM / PGM, PFM, Radiance HDR.
- **Geometry:** spheres, quads, triangles, meshes; binned-SAH BVH with alpha testing.
- **Formats:** scenes in TOML (own parser); meshes in OBJ (polygons, normals, UVs, negative indices, `usemtl`, `mtllib`) and PLY (ASCII / binary LE / BE). OBJ `.mtl` libraries are imported (`Kd Ks Ke Ns Ni d Tr illum Pr Pm`, `map_Kd map_Ks map_Ke map_d map_Bump norm map_Pr map_Pm` with `-s -o -bm -clamp`); a scene material with the same name overrides an MTL material.
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
| `src/material.rs` | materials, BSDF lobes, Fresnel, energy compensation tables |
| `src/texture.rs` | image / procedural textures, Perlin noise |
| `src/scene.rs` | scene data, BVH traversal, alpha testing, UVs / tangents, light sampling |
| `src/bvh.rs` | SAH BVH builder |
| `src/loader.rs` | TOML → scene, texture cache, MTL → material mapping |
| `src/imageio.rs`, `src/jpeg.rs` | image decoders |
| `src/toml.rs`, `src/mesh.rs`, `src/mtl.rs`, `src/image.rs` | parsers / writers |
| `src/platform/` | native window |

See `scenes/cornell.toml` for an annotated scene file and `scenes/materials.toml` for materials and textures.

Note: the glass `tint` parameter was replaced by Beer–Lambert absorption (`color` = transmittance after
`distance` scene units); loading a scene that still uses `tint` reports this.
