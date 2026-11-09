# Roadmap

Guiding constraints: **zero external crates** (std only), progressive full-window display
(coarse blocks → full resolution → sample accumulation), physically based light transport,
human-editable TOML scenes, standard mesh formats.

Legend: `[x]` done · `[ ]` planned. Order within a milestone is a suggestion, not a contract.

---

## M0 — Foundation ✅

- [x] Math core: `Vec3`, rays, orthonormal frames, affine transforms (scale / rotate XYZ / translate)
- [x] PCG32 RNG, deterministic per-pixel/per-sample seeding
- [x] Primitives: spheres, triangles, quads, meshes
- [x] Binned-SAH BVH, ordered stack traversal, any-hit shadow rays
- [x] Integrator: path tracing + next-event estimation + MIS (power heuristic) + Russian roulette
- [x] Materials: Lambertian, GGX conductor (VNDF sampling), smooth dielectric, area emitters (one/two-sided)
- [x] Backgrounds: constant color, sky gradient
- [x] Progressive renderer: coarse-to-fine preview without wasted samples, then spp accumulation
- [x] Multi-threaded rendering (scoped std threads, row work queue), cancellable
- [x] Win32 window via raw FFI, GDI blit, resize → restart, title-bar progress
- [x] TOML-subset parser with line-numbered errors
- [x] Scene loader: render / camera / background / materials (named + inline) / objects
- [x] Mesh loaders: OBJ (polygons, normals, negative indices, `usemtl`), PLY (ASCII, binary LE/BE)
- [x] Tonemapping (ACES, Reinhard, clamp), exposure, sRGB; PNG writer
- [x] CLI: headless mode, output path, spp / size / thread overrides
- [x] Unit tests for parsers, intersections, BSDF sampling consistency, checksums

## M1 — Interactivity & UX ✅

- [x] Camera controls: orbit / pan / zoom with mouse, WASD fly mode; restart render on move
- [x] Mouse + modifier input events in the platform layer
- [x] Intra-pass display updates (show rows as they finish during long passes)
- [x] Hot-reload: watch the scene file and reload on save
- [x] Runtime toggles: exposure / tonemap keys without re-rendering (re-resolve only)
- [x] On-screen overlay or console HUD: spp, samples/s, render time
- [x] Pause / resume rendering
- [x] Optional resolution scale (render below window res, upscale) for heavy scenes

## M2 — Image pipeline ✅

- [x] HDR output: `.pfm` and Radiance `.hdr` (RLE RGBE)
- [x] Compressed PNG (own deflate: LZ77 + lazy matching, stored/fixed/dynamic Huffman per block, package-merge code lengths, PNG row filters); inflate too, ready for M3 image loading
- [x] Save on completion automatically (`[output] save_on_finish`, `--autosave`)
- [x] Reconstruction filters (box / tent / Gaussian / Blackman-Harris) via filter importance sampling
- [x] Adaptive sampling: stop converged pixels (dilated mask), concentrate effort on noisy ones
- [x] Variance / convergence estimate per pixel; time-limit and noise-threshold stop criteria; noise / samples heat-map views
- [x] Simple denoiser: SVGF-style edge-aware à-trous with albedo demodulation, normal / depth / variance guides; interactive toggle
- [x] AOV outputs: albedo, normal, depth (also viewable in the window)

Possible follow-ups: splatting filters with negative lobes (Mitchell, Lanczos); OpenEXR output;
running the interactive denoiser on a background thread.

## M3 — Materials & textures

- [ ] Texture coordinates through the pipeline (OBJ `vt`, PLY `u v`/`s t`, sphere UVs)
- [ ] Image loaders, dependency free: PPM/PFM first, then PNG (inflate), then HDR, maybe TGA/JPEG baseline
- [ ] Texture sampling (bilinear, wrap modes), procedural textures (checker, noise)
- [ ] OBJ `.mtl` import (Kd, Ks, Ns, Ni, d, Ke, map_Kd …) mapped to our materials
- [ ] Normal / bump mapping
- [ ] Rough dielectric (GGX transmission)
- [ ] Beer–Lambert absorption inside dielectrics (replace `tint`)
- [ ] Coated / plastic material (dielectric layer over diffuse), energy-conserving
- [ ] Measured conductor IORs (complex Fresnel for gold, copper, aluminium …)
- [ ] Multiple-scattering energy compensation for rough GGX
- [ ] Principled / uber material (base color, metallic, roughness, specular, transmission, clearcoat, sheen)
- [ ] Textured emission
- [ ] Alpha / cut-out masks

## M4 — Lighting

- [ ] Environment maps (equirectangular HDR) with importance sampling + MIS
- [ ] Analytic sun + sky model (e.g. Preetham / Hosek-Wilkie)
- [ ] Point, spot, and directional lights
- [ ] Solid-angle sphere-light sampling (replace area sampling)
- [ ] Spherical-triangle sampling for triangle lights
- [ ] Light BVH / many-light sampling for scenes with thousands of emitters
- [ ] Spectral rendering (hero wavelength) for dispersion and accurate color

## M5 — Geometry & scenes

- [ ] Instancing (one mesh, many transforms) with a two-level BVH
- [ ] Scene includes / reusable material libraries
- [ ] More formats: glTF 2.0 (JSON + binary; own JSON parser), maybe STL
- [ ] Watertight ray/triangle intersection
- [ ] Wider BVH (BVH4/8) and SIMD-friendly layout; BVH build parallelization
- [ ] Analytic primitives: disks, cylinders, boxes
- [ ] Motion blur (camera + object transforms over shutter interval)
- [ ] Camera models: orthographic, fisheye/equirect, physical (f-stop, ISO, shutter)

## M6 — Advanced transport

- [ ] Participating media: homogeneous volumes, then heterogeneous (delta tracking)
- [ ] Subsurface scattering (random-walk)
- [ ] Bidirectional path tracing or path guiding for caustics/indoor scenes
- [ ] Low-discrepancy sampling (Sobol / blue-noise) instead of pure random

## M7 — Platform & engineering

- [ ] Window backends for Linux (X11 via raw Xlib FFI) and macOS (Cocoa via objc FFI)
- [ ] Reference-image regression tests (render small scenes, compare against stored results)
- [ ] Benchmarks: rays/s per scene, BVH build time
- [ ] CI (build, test, clippy, fmt)
- [ ] Profiling pass: memory layout, lock contention, allocation-free hot paths
- [ ] Scene format documentation (`docs/scene-format.md`)

---

Ideas not yet placed: network/distributed rendering, GPU backend, animation sequences,
light-path expressions / render passes.
