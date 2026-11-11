# PathTracer

A progressive, physically based path tracer in Rust with **zero external crates** — only `std`.

```
cargo run --release -p pathtracer -- scenes/cornell.toml              # interactive window
cargo run --release -p pathtracer -- scenes/spheres.toml --headless   # render to renders/spheres.png
cargo run --release -p pathtracer -- bench                            # rays/s and build times for every example scene
cargo run --release -p ptedit -- scenes/cornell.toml                  # the scene editor (see EDITOR_ROADMAP.md)
```

The scene format is documented in [`docs/scene-format.md`](docs/scene-format.md).

Options: `--headless`, `-o FILE` (`.png` / `.hdr` / `.pfm` picks the format), `--spp N`, `--size WxH`, `--threads N`,
`--time SECONDS` (time limit), `--noise T` (adaptive sampling threshold, e.g. `0.02`), `--denoise` (also save a
denoised image), `--spectral` (hero-wavelength spectral rendering), `--sampler NAME` (`independent`, `sobol`,
`bluenoise`), `--guiding` (path guiding), `--autosave` (window: save when a render completes), `--scale N` (render at
1/N window resolution).

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

### Editor

`ptedit scene.toml` opens the scene in a window with a menu bar, an **outliner** (left), a rasterized **viewport**
(middle; own multithreaded software rasterizer, one view or top / front / side / perspective), an **inspector** (right)
and a status bar. The UI is our own immediate-mode toolkit, with text drawn by our own TrueType rasterizer from a
system font (or a built-in bitmap font).

- The outliner lists the settings sections, lights, objects, materials, media and prototypes. It filters by name,
  selects (in the viewport too), renames (double-click or `F2`; renaming a material, medium or prototype renames its
  references), reorders by dragging, assigns a material dropped on an object, and shows and hides objects and lights
  in the views. Entries from included files name their file.
- The inspector shows every key the format knows for the selection, from the schema. Defaults are greyed out until
  set; `x` removes a key again. Numbers are dragged sideways (`Shift`: finer, `Ctrl`: coarser) or typed (simple
  expressions such as `1/3` work). Colors have a picker (RGB / HSV / Kelvin), paths a file browser. Validation
  problems show under their field, and unknown keys are listed.
- Edits keep the files' formatting, comments and includes. Every edit can be undone; a drag is one step. A scene the
  loader rejects still opens, so it can be fixed in the editor.
- Theme (dark / light), panel widths, layout and recent files are kept per user in `settings.toml` in the user's
  configuration folder (`PTEDIT_SETTINGS` names another file).

Scene editing:

- **Add** (menu, or `Shift+A` under the mouse) primitives, a mesh file, an instance of a prototype, or a light: placed
  on the surface under the cursor (else on the ground plane), at a size that suits the view. `Delete`, `Ctrl+D`
  duplicate (comments and all), `Ctrl+C` / `Ctrl+X` / `Ctrl+V` copy, cut and paste, also between scenes: the
  clipboard holds the entries as TOML with the materials and prototypes they use, and file paths are rewritten to
  work from the other scene's folder.
- **Gizmos** (`W` move, `E` rotate, `R` scale, `Q` none; `T` world / local axes): arrows, planes and a view-plane
  square; rings and a view ring; axes and a uniform square. While dragging, type a number (a distance, degrees or a
  factor), press `X` / `Y` / `Z` (with `Shift`: the plane) to change the axis, `Ctrl` to toggle snapping, `Enter` to
  keep, `Esc` to cancel. Snapping (`N`): the grid's lines with 15° and 0.1 steps, vertices of other objects, or the
  surface under the cursor.
- Changes are written as the file already writes them: a sphere placed by its `center` has its center moved, a box
  given by `min` / `max` has both moved, others get `translate`; rotations become `rotate` angles close to the old
  ones, a uniform `scale` stays one number; lights get `position`, `direction` / `look_at` or `elevation` / `azimuth`.
  Moving things does not load the scene again: the geometry moves in place and the BVHs are refitted.
- **Handles**: a spot light's cone and inner cone and where it points; the sun of a directional light or of the sky
  (with the background selected) dragged on the sky dome; the faces of an instance's `scatter` box; the camera's
  focus plane. The environment backdrop of the shaded view previews `[background] rotate` while it is dragged.
- **Camera**: `C` looks through the scene camera (the rest of the view is dimmed outside its image), `Shift+C`
  locks it to the view so that navigating moves the camera; `Ctrl+F` and a click sets the focus distance; the camera's
  inspector shows focal length, focus and aperture and draws the focus plane.
- **Materials** tab: the scene's materials and those of `lib/*.toml` files near the scene (such as
  `scenes/lib/materials.toml`), each with a small path-traced preview; drag one onto an object in the viewport or the
  outliner (a library material is copied into the scene file), double-click to give it to the selection. The
  inspector shows a preview of the material being edited and thumbnails of texture images; the file browser shows
  thumbnails of images.
- `Ctrl+P` turns the selection into a prototype (placed by one instance where the objects were); `[render]` and
  `[output]` have presets in the inspector.
- **Path-traced view** (`P`): the tracer's progressive renderer draws the perspective view, restarted on every change,
  with the gizmos and the selection outline on top; `Ctrl+B` drags a render region, `Ctrl+Alt+B` clears it.
- **Hot reload**: a scene file changed by another program comes in as an undoable edit (the editor asks first if it
  has unsaved changes there); changed meshes and textures reload the scene. `F5` opens the scene in the tracer's
  window, which reloads it whenever the editor saves.

Polygon modeling (`Tab` on a selected object; the Mesh menu replaces Add):

- Shapes (sphere, box, cylinder, disk, quad, triangle) and OBJ / PLY / STL mesh files become an editable polygon mesh
  (the `pt-geom` crate: half-edge connectivity, texture coordinates per corner, creases, material groups). `Tab`
  again writes it next to the scene as a new PLY file (OBJ when it has texture coordinates or material groups) and
  points the object at it, as one undoable edit; the old file stays, so undo brings the old geometry back.
- `1` / `2` / `3` select vertices, edges or faces (selected faces are tinted). Click (`Shift` adds, `Ctrl` toggles),
  drag a rectangle (only what is in sight), double-click an edge for its loop (`Ctrl`: its ring; faces: the face
  loop), `Ctrl+A` all, `Ctrl+Plus` / `Ctrl+Minus` grow / shrink, `Ctrl+L` linked. The gizmos move, rotate and scale
  the selection, with snapping (also onto the mesh's own vertices) and proportional editing (`Shift+O`; the wheel
  sets the radius while dragging).
- `Ctrl+E` extrude (faces as a region, or edges; `X` / `Y` / `Z` along an axis), `I` inset, `Ctrl+B` bevel edges
  (`Ctrl+Shift+B` vertices): they follow the mouse, take typed values, and end with a click or `Enter` (`Esc`
  cancels). `Ctrl+R` loop cut (point at an edge; the wheel adds cuts), `K` knife (drag a line: it cuts the selected
  faces, or all, under it). The Mesh menu adds bridge (two edge loops, or two face regions), merge, weld by
  distance, dissolve, fill hole, mirror, flip normals, triangulate and creases. `Delete` deletes, `Ctrl+Delete`
  dissolves; `Ctrl+Z` / `Ctrl+Y` undo and redo inside edit mode.

Clicking in the viewport picks through the tracer's BVH. `ptedit --snapshot scene.toml out.png [--size WxH] [--quad]
[--shading wire|solid|shaded|wiresolid] [--select N] [--frames N] [--rendered SPP]` draws the viewport into a PNG
without a window and prints the frame time (`--rendered`: path traced to SPP samples per pixel); `--edit N` shows
object N in edit mode; `--ui [--theme dark|light] [--scale S]` draws the whole editor window instead. Planned work is in
[`EDITOR_ROADMAP.md`](EDITOR_ROADMAP.md).

| Input | Action |
|---|---|
| Left click / drag | select / rectangle select (`Shift` adds, `Ctrl` toggles) |
| Middle drag, `Alt` + left drag | orbit (pan in the top / front / side views) |
| `Shift` + middle drag, `Alt` + `Shift` + left drag | pan |
| Right drag, with `W A S D Q E` | look around and fly (`Shift`: 4× speed) |
| Wheel | dolly / zoom |
| `F` / `Home` | frame the selection (or the scene) / reset the view |
| `O` | perspective / orthographic (perspective view) |
| `1`–`4`, `Z` | wireframe / solid / shaded (material colors) / wireframe on solid |
| `L` / `Space` | one view or four / maximize the view under the mouse |
| `G` / `B` | grid / bounding boxes of all objects |
| `H` / `Alt+H` | hide the selection / show everything |
| `Ctrl+A`, `Esc` | select all; clear the selection |
| `Tab` / `Shift+Tab`, `Enter`, `Esc` | next / previous field, commit, cancel |
| `Ctrl+O`, `Ctrl+S`, `Ctrl+Shift+S`, `Ctrl+Q` | open, save, save as (same folder), quit |
| `Ctrl+Z`, `Ctrl+Y` / `Ctrl+Shift+Z`, `F1` | undo, redo, help |

## Features

- **Progressive display** filling the whole window: coarse 32×32 blocks refine by halving down to 1×1 (each level only traces pixels not yet traced, so the preview costs nothing extra), then whole-image passes accumulate samples per pixel.
- **Integrator:** unidirectional volumetric path tracing, next-event estimation, MIS (power heuristic), Russian roulette; RGB or spectral.
- **Participating media** (`[media.NAME]`, `scenes/media.toml`): homogeneous (`albedo` + `density`, or `sigma_a` / `sigma_s`; Henyey–Greenstein `g`) or heterogeneous, with the density from a world-space procedural `field` (noise, turbulence, checker) or a Mitsuba `.vol` voxel `grid`. A material's `interior` / `exterior` name the media on either side of its surfaces (`type = "interface"` is an invisible boundary; glass can hold a medium too). Homogeneous media sample distances analytically, heterogeneous ones by delta tracking (absorption as a weight) and ratio tracking for shadow rays; both use hero-channel spectral MIS, so strongly coloured media converge. Point and spot lights inside media are sampled equi-angularly. `max_volume_depth` caps scattering events.
- **Subsurface scattering** (`type = "subsurface"`, `scenes/subsurface.toml`): random walk below a smooth or rough dielectric interface; `color` is the overall albedo (inverted to single-scattering coefficients after Chiang et al. 2016) and `radius` × `scale` the per-channel mean free path. Textured `color` works (coefficients come from the entry point).
- **Path guiding** (`[render] guiding = true`, `scenes/guiding.toml`): an SD-tree (Müller et al. 2017, "Practical Path Guiding") learns the incident radiance per region of space during the first `guiding_spp` samples per pixel (iterations of 1, 2, 4, ... spp) and surface vertices then sample a 50/50 mixture of it and the BSDF. Helps scenes lit by strong indirect light (≈2× less variance at equal time in a Cornell box lit by a spot aimed at its ceiling); costs ~20% per sample otherwise.
- **Low-discrepancy sampling** (`[render] sampler`): `sobol` (default) — Owen-scrambled Sobol points with shuffled sample order (Burley 2020), valid for any sample count, adaptive sampling and time limits (≈25% lower error than independent sampling at equal spp in the Cornell box); `bluenoise` — the same points shifted per pixel by a void-and-cluster blue-noise mask, so low-spp noise is high-frequency; `independent` — plain PCG32.
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
  - Every material takes `normal_map` (tangent space), `bump` (height texture; solid noise patterns bump by their analytic gradient, without UVs) and `alpha` (cut-out / stochastic coverage).
- **Textures:** any color or scalar parameter can be a constant, an image (bilinear / nearest, repeat / clamp / mirror, UV scale / offset, sRGB or linear, channel select, or triplanar projection for surfaces without UVs), a checker, or a solid noise pattern: fBm, turbulence, Musgrave's ridged / hybrid multifractals and heterogeneous terrain, domain warping, marble and wood (`scenes/implicit.toml`). Noise has analytic gradients; its octaves fade out where they would be finer than a pixel (the footprint follows each path as a ray cone), so distant surfaces stay smooth instead of aliasing. Texture coordinates come from OBJ `vt`, PLY `u v` / `s t`, quads (0..1) and spheres (latitude-longitude).
- **Image loaders** (own code): PNG (all color types and bit depths, palette, tRNS, Adam7), baseline JPEG (any chroma subsampling, restart markers), TGA (raw / RLE), PPM / PGM, PFM, Radiance HDR.
- **Geometry:** analytic spheres, disks / annuli, cylinders (open or capped) and oriented boxes, all usable as area lights; quads, triangles and meshes with watertight ray/triangle intersection (Woop et al.). Every object takes `translate` / `rotate` / `scale`.
- **Implicit surfaces** (sphere tracing, Hart 1996): signed distance field trees (sphere, box, torus, cylinder, capsule; union / intersection / difference with smooth blends; round, shell, noise displacement), hypertextures (Perlin & Hoffert: noise-eroded soft objects, as surfaces or as the density of a medium, `scenes/hypertexture.toml`) and deterministic fractals: quaternion Julia sets with a configurable 4D slice, the Mandelbulb and the Menger sponge (`scenes/fractals.toml`). Steps are bounded by each field's Lipschitz constant (estimated by sampling for displaced and hypertextured fields); overshoots are caught by the sign change and bisected, and rays leaving a surface take their side from the gradient, so f32 precision gives neither holes nor self-intersections.
- **Acceleration:** two-level BVH. Each geometry group has a 4-wide BVH (binned SAH, built in parallel, children's bounds stored as structure-of-arrays so one node visit tests four boxes); a top-level BVH holds the instances.
- **Instancing:** `[prototypes.NAME]` defines geometry once; `type = "instance"` objects place it, one at a time or thousands at once with `scatter` (random offsets / rotations / scales). Emitters inside instances are not light-sampled (they only add light when hit).
- **Motion blur:** objects and instances take `motion = { translate, rotate, scale }` (the transform at the end of the frame; rotations interpolate per axis, so large angles spin), the camera `motion = { position, look_at, up }`; `[camera] shutter = [open, close]`.
- **Cameras:** `projection = "perspective"` (thin lens), `"orthographic"`, `"fisheye"` (equidistant, circular) or `"equirect"` (360° panorama). Physical parameters: `focal_length` (mm, `sensor_height` 24 mm) instead of `fov`, `f_stop` for the lens aperture (scene units are meters), and `iso` / `shutter_speed` for exposure (scene values scaled by t·S / (120·N²), the saturation-based metering of real cameras, which only gives sensible brightness for scenes in physical units).
- **Formats:** scenes in TOML (own parser) with `include = [...]` for shared settings and material libraries; meshes in OBJ (polygons, normals, UVs, negative indices, `usemtl`, `mtllib`), PLY (ASCII / binary LE / BE), STL (ASCII / binary) and glTF 2.0 (`.gltf` with external or embedded buffers, `.glb`; own JSON parser; node hierarchy, triangle lists / strips / fans, metallic-roughness materials with base color / metal-rough / normal / emissive textures, alpha modes, `KHR_materials_emissive_strength`, `_transmission`, `_ior`). OBJ `.mtl` libraries are imported (`Kd Ks Ke Ns Ni d Tr illum Pr Pm`, `map_Kd map_Ks map_Ke map_d map_Bump norm map_Pr map_Pm` with `-s -o -bm -clamp`); a scene material with the same name overrides an MTL or glTF material.
- **Sampling:** reconstruction filters (box, tent, Gaussian, Blackman-Harris) via filter importance sampling; per-pixel variance estimate; adaptive sampling (converged pixels stop, the rest keep going) with noise-threshold, time-limit and spp stop criteria.
- **Denoiser:** edge-aware à-trous wavelet filter (SVGF-style, spatial), guided by albedo / normal / depth AOVs and the per-pixel variance; albedo is demodulated so material detail stays sharp.
- **Output:** ACES / Reinhard / clamp tonemapping, exposure, sRGB; PNG (own DEFLATE: LZ77 + fixed/dynamic Huffman, row filters), Radiance `.hdr` (RLE RGBE) and `.pfm`; denoised copy and albedo / normal / depth AOV images; optional save on completion.
- **Window:** raw FFI on every platform: Win32 + GDI on Windows; X11 on Linux / BSD (Xlib loaded at run time with `dlopen`, so headless rendering needs no X libraries); Cocoa on macOS (Objective-C runtime calls, the image shown through a Core Animation layer).
- **Benchmarks:** `pathtracer bench [scene.toml ...] [--size WxH] [--threads N] [--time S] [--csv FILE]` reports load and BVH build times and the throughput of primary, diffuse and shadow rays and of full paths for each scene (default: all of `scenes/`).

## Layout

| File | Purpose |
|---|---|
| `crates/pathtracer/src/main.rs` | the `pathtracer` binary: CLI, headless rendering |
| `crates/pathtracer/src/bench.rs` | `pathtracer bench`: build times and ray throughput |
| `crates/pathtracer/src/app.rs` | windowed front end: UI thread, render thread, input, HUD, hot reload |
| `crates/ptedit/src/main.rs` | the `ptedit` scene editor: command line, `--check`, `--snapshot` |
| `crates/ptedit/src/editor.rs` | the editor window: panel layout, menus, status bar, viewport input, commands and shortcuts, dialogs, the event loop |
| `crates/ptedit/src/outliner.rs` | the outliner: tree of sections and items, select, rename, reorder, drag and drop, show / hide |
| `crates/ptedit/src/inspector.rs` | the inspector, generated from the schema: a widget per key type, defaults, aliases, problems |
| `crates/ptedit/src/files.rs` | in-app file browser (open, save as, pick files), with image thumbnails |
| `crates/ptedit/src/edit.rs` | editing commands: add under the cursor, delete, duplicate, copy / paste between scenes, make a prototype, assign and import materials, presets |
| `crates/ptedit/src/xform.rs` | placements and how moves, rotations and scales are written back in each item's own keys |
| `crates/ptedit/src/gizmo.rs` | transform gizmos and handles: drawing and hit testing |
| `crates/ptedit/src/tools.rs` | gizmo and handle drags, snapping, typed values, click-to-focus, render region, camera locked to the view |
| `crates/ptedit/src/modeling.rs` | edit mode: objects as meshes, element selection and picking, operations following the mouse, loop cut, knife, writing the mesh file |
| `crates/pt-geom/src/` | the modeling kernel: half-edge meshes (`mesh.rs`), selection helpers (`select.rs`), operations (`ops.rs`), primitives, ear clipping, PLY / OBJ writers and conversion from and to the tracer's meshes (`io.rs`) |
| `crates/ptedit/src/ptview.rs` | the path-traced view: the progressive renderer on its own thread, restarted on changes |
| `crates/ptedit/src/library.rs` | the Materials tab and its path-traced previews, image thumbnails (a worker thread) |
| `crates/ptedit/src/watch.rs` | hot reload: files changed by other programs |
| `crates/ptedit/src/settings.rs` | per-user settings: theme, font, layout, recent files |
| `crates/ptedit/src/ui/` | immediate-mode UI toolkit: ids, hot / active / focus, layers (popups, modals, tooltips), scrolling, widgets, themes, TrueType reader and rasterizer, 8×8 bitmap font |
| `crates/ptedit/src/workspace.rs` | the editor's state: document, scene, views and layout, selection, picking; draws all views |
| `crates/ptedit/src/raster.rs` | software rasterizer: clipping, tile binning, multithreaded triangles and lines, reversed depth, id buffer |
| `crates/ptedit/src/display.rs` | what the viewport draws: meshes per object, shapes as canonical meshes at several levels of detail, boxes |
| `crates/ptedit/src/render.rs` | one view's frame: culling, level of detail, triangle budget, shading modes, selection outline |
| `crates/ptedit/src/view.rs` | viewport cameras (perspective / orthographic, top / front / side), projection, picking rays |
| `crates/ptedit/src/overlay.rs` | grid, axes, axis indicator, camera frustum, light helpers, bounding boxes, selection state |
| `crates/ptedit/src/paint.rs` | 2D drawing and a 5×7 bitmap font for labels in headless snapshots |
| `crates/pt-core/src/document/` | the editor's scene document: the scene's files as editable TOML, the tree of what the merged scene contains and where each item is written, undo / redo |
| `crates/pt-core/src/schema.rs` | the scene format as data (sections, keys, types, defaults, ranges, aliases); validation with typo suggestions |
| `crates/pt-core/src/controls.rs` | orbit / pan / look / fly camera controller |
| `crates/pt-core/src/renderer.rs` | progressive coarse-to-fine scheduling, adaptive sampling, threading, progress callbacks, display views |
| `crates/pt-core/src/integrator.rs` | volumetric path tracing with NEE + MIS, subsurface walks, guided sampling, AOVs; generic over RGB / spectral |
| `crates/pt-core/src/medium.rs` | media, free-flight sampling, transmittance, phase function, equi-angular sampling, `.vol` grids |
| `crates/pt-core/src/guiding.rs` | path guiding SD-tree |
| `crates/pt-core/src/sampler.rs` | independent / Owen-scrambled Sobol / blue-noise samplers |
| `crates/pt-core/src/lights.rs` | light types, light sampling (sphere cones, spherical triangles), light BVH |
| `crates/pt-core/src/environment.rs` | environment maps and their importance sampling |
| `crates/pt-core/src/sky.rs` | Preetham sky and sun |
| `crates/pt-core/src/spectrum.rs` | wavelength sampling, colour matching, RGB uplift, black bodies |
| `crates/pt-core/src/filter.rs` | reconstruction filters |
| `crates/pt-core/src/denoise.rs` | à-trous denoiser |
| `crates/pt-core/src/output.rs` | output naming, formats, denoised / AOV images |
| `crates/pt-core/src/deflate.rs` | DEFLATE / zlib compressor and decompressor |
| `crates/pt-core/src/material.rs` | materials, BSDF lobes, Fresnel, energy compensation tables |
| `crates/pt-core/src/texture.rs` | image / procedural / triplanar textures, solid gradients for bump mapping |
| `crates/pt-core/src/noise.rs` | Perlin noise with gradients, fractals (fBm, turbulence, multifractals, warping), marble / wood |
| `crates/pt-core/src/implicit.rs` | signed distance fields, smooth CSG, hypertexture, fractal distance estimators, sphere tracing |
| `crates/pt-core/src/scene.rs` | scene data, geometry groups and instances, two-level traversal, alpha testing, UVs / tangents |
| `crates/pt-core/src/bvh.rs` | parallel binned-SAH build, 4-wide BVH and its traversal |
| `crates/pt-core/src/geometry.rs`, `crates/pt-core/src/shapes.rs` | bounds, watertight triangles; spheres, disks, cylinders, boxes, implicit surfaces |
| `crates/pt-core/src/camera.rs` | perspective / orthographic / fisheye / equirect cameras, shutter and camera motion, pixel ray cones |
| `crates/pt-core/src/loader.rs` | TOML → scene: sections, materials, textures, MTL → material mapping |
| `crates/pt-core/src/loader/objects.rs` | objects, transforms, motion, prototypes / instances, mesh and glTF import |
| `crates/pt-core/src/loader/include.rs` | scene includes |
| `crates/pt-core/src/loader/procedural.rs` | noise fractal parameters, SDF trees, hypertextures, fractal objects |
| `crates/pt-core/src/imageio.rs`, `crates/pt-core/src/jpeg.rs` | image decoders |
| `crates/pt-core/src/gltf.rs`, `crates/pt-core/src/json.rs` | glTF 2.0 reader and its JSON parser |
| `crates/pt-core/src/toml/` | TOML reader and format-preserving editor (spans for every value, edits as text splices) |
| `crates/pt-core/src/mesh.rs`, `crates/pt-core/src/mtl.rs`, `crates/pt-core/src/image.rs` | parsers / writers (mesh.rs: OBJ, PLY, STL) |
| `crates/pt-core/src/platform/` | native windows: `win32.rs`, `x11.rs`, `macos.rs` |
| `crates/pt-core/src/regression.rs` | reference-image regression tests (`testdata/regression/`) |
| `crates/pt-core/src/alloc_count.rs` | counting allocator for tests; checks the path-tracing hot path never allocates |
| `docs/scene-format.md` | scene file reference |

## Development

The toolchain is pinned in `rust-toolchain.toml`; rustup installs it with `rustfmt` and `clippy`.

| Command | Checks |
|---|---|
| `cargo fmt --all` | formatting (`rustfmt.toml`: 120 columns) |
| `cargo lint` | clippy over every crate and its tests, warnings as errors |
| `cargo lint-fix` | applies clippy's machine-applicable fixes |
| `cargo test --release` | unit tests, the zero-allocation check and reference-image regression tests |
| `PT_BLESS=1 cargo test --release regression` | re-creates the regression references after an intended change |
| `cargo run --release -p pathtracer -- bench` | performance: build times, rays/s, paths/s |

The repository is a Cargo workspace: `pt-core` (library: scene model and format, loaders, BVH, renderer,
platform layer), `pt-geom` (library: the modeling kernel) and the `pathtracer` and `ptedit` binaries. Lint policy is in the root `Cargo.toml` under
`[workspace.lints]`: `clippy::pedantic` plus a few restriction lints, with
numeric-cast lints turned off as noise for renderer code. `unsafe` is denied everywhere except
the platform backends in `crates/pt-core/src/platform/` and the test-only counting allocator, and every `unsafe` block needs a
`// SAFETY:` comment. Silence a lint at a site
with `#[expect(lint, reason = "...")]`; plain `#[allow]` and a missing reason are both flagged.

Enable the pre-commit hook (fmt, lint, rustdoc) once per clone: `git config core.hooksPath .githooks`.

CI (`.github/workflows/ci.yml`) runs formatting, clippy, rustdoc and all tests on Linux, Windows and macOS (plus a
clippy pass for Intel macOS), and records a benchmark CSV on Linux. A failing regression test uploads its render,
reference and difference images.

Regression tests render each example scene at 128 pixels wide and compare 8×8 block averages of the tonemapped
image with the stored reference. Each scene's tolerance is twice the difference between two renders with different
seeds, measured when the reference is blessed and stored in `testdata/regression/<scene>.tol`. That ignores the noise
pattern, which changes between platforms, but catches small biases: a 3% error in the diffuse lobe's direct
lighting fails several scenes.

See `scenes/cornell.toml` for an annotated scene file, `scenes/materials.toml` for materials and textures,
`scenes/lights.toml` for light types and many-light sampling, `scenes/sky.toml` for daylight, `scenes/envmap.toml`
for image-based lighting, `scenes/dispersion.toml` for spectral rendering, `scenes/shapes.toml` for analytic shapes,
glTF / STL import, motion blur and camera models, and `scenes/instancing.toml` for prototypes, instances and
`scatter` (56 000 instances), `scenes/media.toml` for participating media, `scenes/subsurface.toml` for subsurface
scattering, `scenes/guiding.toml` for path guiding, `scenes/implicit.toml` for distance fields, solid textures and
fractal bump mapping, `scenes/fractals.toml` for 3D fractals and `scenes/hypertexture.toml` for hypertextures.
`scenes/lib/materials.toml` is a material library to `include`.

Note: the glass `tint` parameter was replaced by Beer–Lambert absorption (`color` = transmittance after
`distance` scene units); loading a scene that still uses `tint` reports this. `[background] type = "sky"` used to be
an alias of `gradient`; it now selects the analytic daylight model.
