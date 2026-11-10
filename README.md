# PathTracer

A progressive, physically based path tracer in Rust with **zero external crates** — only `std`.

```
cargo run --release -- scenes/cornell.toml               # interactive window
cargo run --release -- scenes/spheres.toml --headless    # render to renders/spheres.png
```

Options: `--headless`, `-o FILE` (`.png` / `.hdr` / `.pfm` picks the format), `--spp N`, `--size WxH`, `--threads N`,
`--time SECONDS` (time limit), `--noise T` (adaptive sampling threshold, e.g. `0.02`), `--denoise` (also save a
denoised image), `--spectral` (hero-wavelength spectral rendering), `--autosave` (window: save when a render
completes), `--scale N` (render at 1/N window resolution).

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
- **Integrator:** unidirectional path tracing, next-event estimation, MIS (power heuristic), Russian roulette; RGB or spectral.
- **Lights:**
  - area lights: any emissive sphere (sampled uniformly in the cone it subtends) or triangle (sampled uniformly in its spherical triangle, area sampling for tiny / huge solid angles);
  - `[[lights]]`: `point`, `spot` (smooth falloff between `inner_angle` and `angle`) and `directional` (delta, or with an angular diameter for soft shadows); `power` or `intensity`, `color` and/or a black-body `temperature`;
  - environment (`[background]`): constant, gradient, equirectangular image (`.hdr`, `.pfm`, `.png`, ...; rotatable) or `sky`: Preetham analytic daylight with a physically sized sun whose colour follows the air mass (`sun_elevation` / `sun_azimuth`, `turbidity`, `sun_size`). Every environment is importance sampled through a tabulated luminance distribution;
  - light selection with a light BVH (bounds + emission cones, as in pbrt-v4): thousands of emitters cost little extra noise. `light_sampler = "power"` / `uniform` for comparison.
- **Spectral rendering** (`[render] spectral = true`): hero wavelength sampling with four wavelengths per path, CIE 1931 colour matching (analytic fit), a smooth linear RGB → spectrum uplift; glass with an Abbe number (`abbe`) disperses light. Lights and emissive materials take a black-body `temperature`.
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
- **Geometry:** analytic spheres, disks / annuli, cylinders (open or capped) and oriented boxes, all usable as area lights; quads, triangles and meshes with watertight ray/triangle intersection (Woop et al.). Every object takes `translate` / `rotate` / `scale`.
- **Acceleration:** two-level BVH. Each geometry group has a 4-wide BVH (binned SAH, built in parallel, children's bounds stored as structure-of-arrays so one node visit tests four boxes); a top-level BVH holds the instances.
- **Instancing:** `[prototypes.NAME]` defines geometry once; `type = "instance"` objects place it, one at a time or thousands at once with `scatter` (random offsets / rotations / scales). Emitters inside instances are not light-sampled (they only add light when hit).
- **Motion blur:** objects and instances take `motion = { translate, rotate, scale }` (the transform at the end of the frame; rotations interpolate per axis, so large angles spin), the camera `motion = { position, look_at, up }`; `[camera] shutter = [open, close]`.
- **Cameras:** `projection = "perspective"` (thin lens), `"orthographic"`, `"fisheye"` (equidistant, circular) or `"equirect"` (360° panorama). Physical parameters: `focal_length` (mm, `sensor_height` 24 mm) instead of `fov`, `f_stop` for the lens aperture (scene units are meters), and `iso` / `shutter_speed` for exposure (scene values scaled by t·S / (120·N²), the saturation-based metering of real cameras, which only gives sensible brightness for scenes in physical units).
- **Formats:** scenes in TOML (own parser) with `include = [...]` for shared settings and material libraries; meshes in OBJ (polygons, normals, UVs, negative indices, `usemtl`, `mtllib`), PLY (ASCII / binary LE / BE), STL (ASCII / binary) and glTF 2.0 (`.gltf` with external or embedded buffers, `.glb`; own JSON parser; node hierarchy, triangle lists / strips / fans, metallic-roughness materials with base color / metal-rough / normal / emissive textures, alpha modes, `KHR_materials_emissive_strength`, `_transmission`, `_ior`). OBJ `.mtl` libraries are imported (`Kd Ks Ke Ns Ni d Tr illum Pr Pm`, `map_Kd map_Ks map_Ke map_d map_Bump norm map_Pr map_Pm` with `-s -o -bm -clamp`); a scene material with the same name overrides an MTL or glTF material.
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
| `src/integrator.rs` | path tracing with NEE + MIS, AOVs; generic over RGB / spectral |
| `src/lights.rs` | light types, light sampling (sphere cones, spherical triangles), light BVH |
| `src/environment.rs` | environment maps and their importance sampling |
| `src/sky.rs` | Preetham sky and sun |
| `src/spectrum.rs` | wavelength sampling, colour matching, RGB uplift, black bodies |
| `src/filter.rs` | reconstruction filters |
| `src/denoise.rs` | à-trous denoiser |
| `src/output.rs` | output naming, formats, denoised / AOV images |
| `src/deflate.rs` | DEFLATE / zlib compressor and decompressor |
| `src/material.rs` | materials, BSDF lobes, Fresnel, energy compensation tables |
| `src/texture.rs` | image / procedural textures, Perlin noise |
| `src/scene.rs` | scene data, geometry groups and instances, two-level traversal, alpha testing, UVs / tangents |
| `src/bvh.rs` | parallel binned-SAH build, 4-wide BVH and its traversal |
| `src/geometry.rs`, `src/shapes.rs` | bounds, watertight triangles; spheres, disks, cylinders, boxes |
| `src/camera.rs` | perspective / orthographic / fisheye / equirect cameras, shutter and camera motion |
| `src/loader.rs` | TOML → scene: sections, materials, textures, MTL → material mapping |
| `src/loader/objects.rs` | objects, transforms, motion, prototypes / instances, mesh and glTF import |
| `src/loader/include.rs` | scene includes |
| `src/imageio.rs`, `src/jpeg.rs` | image decoders |
| `src/gltf.rs`, `src/json.rs` | glTF 2.0 reader and its JSON parser |
| `src/toml.rs`, `src/mesh.rs`, `src/mtl.rs`, `src/image.rs` | parsers / writers (mesh.rs: OBJ, PLY, STL) |
| `src/platform/` | native window |

## Development

The toolchain is pinned in `rust-toolchain.toml`; rustup installs it with `rustfmt` and `clippy`.

| Command | Checks |
|---|---|
| `cargo fmt --all` | formatting (`rustfmt.toml`: 120 columns) |
| `cargo lint` | clippy over the binary and tests, warnings as errors |
| `cargo lint-fix` | applies clippy's machine-applicable fixes |
| `cargo test --release` | unit tests and render checksums |

Lint policy is in `Cargo.toml` under `[lints]`: `clippy::pedantic` plus a few restriction lints, with
numeric-cast lints turned off as noise for renderer code. `unsafe` is denied everywhere except
`src/platform/win32.rs`, and every `unsafe` block needs a `// SAFETY:` comment. Silence a lint at a site
with `#[expect(lint, reason = "...")]`; plain `#[allow]` and a missing reason are both flagged.

Enable the pre-commit hook (fmt, lint, rustdoc) once per clone: `git config core.hooksPath .githooks`.

See `scenes/cornell.toml` for an annotated scene file, `scenes/materials.toml` for materials and textures,
`scenes/lights.toml` for light types and many-light sampling, `scenes/sky.toml` for daylight, `scenes/envmap.toml`
for image-based lighting, `scenes/dispersion.toml` for spectral rendering, `scenes/shapes.toml` for analytic shapes,
glTF / STL import, motion blur and camera models, and `scenes/instancing.toml` for prototypes, instances and
`scatter` (56 000 instances). `scenes/lib/materials.toml` is a material library to `include`.

Note: the glass `tint` parameter was replaced by Beer–Lambert absorption (`color` = transmittance after
`distance` scene units); loading a scene that still uses `tint` reports this. `[background] type = "sky"` used to be
an alias of `gradient`; it now selects the analytic daylight model.
