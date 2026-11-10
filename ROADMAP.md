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

## M3 — Materials & textures ✅

- [x] Texture coordinates through the pipeline (OBJ `vt`, PLY `u v`/`s t`, quad and sphere UVs, dp/du dp/dv tangents)
- [x] Image loaders, dependency free: PPM/PGM, PFM, PNG (all color types / depths, palette, tRNS, Adam7), Radiance HDR, TGA (raw / RLE), baseline JPEG
- [x] Texture sampling (bilinear / nearest, repeat / clamp / mirror, sRGB decoding, channel select), procedural textures (checker, Perlin fBm / turbulence)
- [x] OBJ `.mtl` import (Kd, Ks, Ke, Ns, Ni, d/Tr, illum, Pr/Pm, map_Kd/Ks/Ke/d/Bump/norm/Pr/Pm with -s -o -bm -clamp) mapped to our materials
- [x] Normal / bump mapping
- [x] Rough dielectric (GGX transmission, Walter et al. with VNDF sampling)
- [x] Beer–Lambert absorption inside dielectrics (replaces `tint`)
- [x] Coated / plastic material (dielectric layer over diffuse), energy-conserving
- [x] Measured conductor IORs (complex Fresnel for gold, silver, copper, aluminium, iron, chromium, platinum)
- [x] Multiple-scattering energy compensation for rough GGX conductors (Kulla–Conty)
- [x] Principled / uber material (base color, metallic, roughness, specular, transmission, clearcoat, sheen)
- [x] Textured emission
- [x] Alpha / cut-out masks (stochastic for fractional alpha, also in shadow rays)

Possible follow-ups: progressive JPEG; texture filtering with ray differentials / mipmaps; energy
compensation for rough dielectrics and coats; MikkTSpace per-vertex tangents; named reusable textures;
emitter sampling that respects textured emission and alpha masks.

## M4 — Lighting ✅

- [x] Environment maps (equirectangular HDR) with importance sampling + MIS; every background kind is a sampled light
- [x] Analytic sun + sky model (Preetham, with an air-mass-attenuated black-body sun disk and a lit ground)
- [x] Point, spot, and directional lights (directional optionally with an angular size), black-body `temperature`
- [x] Solid-angle sphere-light sampling (replace area sampling)
- [x] Spherical-triangle sampling for triangle lights
- [x] Light BVH / many-light sampling for scenes with thousands of emitters (`light_sampler` = bvh / power / uniform)
- [x] Spectral rendering (hero wavelength) for dispersion and accurate color; Abbe-number dispersion for glass
- [x] Fix: shadow rays aim from the offset origin at the light point (near lights were self-occluded)

Possible follow-ups: Hosek-Wilkie sky; spectral measured-IOR conductors and spectral textures (Jakob-Hanika
uplift); bilinear-warped (cosine-aware) spherical-triangle sampling; light sampling for quads as spherical
rectangles; textured-emission-aware light power; MIS-compensated environment sampling; light-path guiding for
caustics through dispersive glass.

## M5 — Geometry & scenes ✅

- [x] Instancing with a two-level BVH: `[prototypes]` placed by `type = "instance"` objects, `scatter` for thousands at once
- [x] Scene includes (`include = [...]`, merged with per-entry directories) and a reusable material library (`scenes/lib/materials.toml`)
- [x] More formats: glTF 2.0 (`.gltf` / `.glb`, own JSON parser, metallic-roughness materials and textures) and STL (ASCII / binary)
- [x] Watertight ray/triangle intersection (Woop, Benthin & Wald 2013)
- [x] 4-wide BVH with structure-of-arrays child bounds (auto-vectorized slab tests); parallel binned-SAH build
- [x] Analytic primitives: disks / annuli, cylinders, oriented boxes (all area-light capable); transforms for every object
- [x] Motion blur: object / instance TRS keys and camera motion over the shutter interval
- [x] Camera models: orthographic, fisheye, equirectangular; physical focal length, f-stop, ISO, shutter speed

Possible follow-ups: light sampling for emitters inside instances; keeping glTF mesh reuse as instances (now
flattened); glTF cameras, `KHR_lights_punctual`, `KHR_texture_transform`, hard alpha cutoff for `MASK`; more than two
motion keys and deformation blur; exact (analytic) motion bounds; SIMD intrinsics / BVH8 and spatial splits (SBVH);
per-instance material overrides; cones and curves.

## M6 — Advanced transport ✅

- [x] Participating media: homogeneous (analytic free flight) and heterogeneous (delta / ratio tracking) volumes; procedural density fields and Mitsuba `.vol` grids; `interface` boundaries, `interior` / `exterior` media on any material
- [x] Hero-channel spectral MIS for distance sampling (bounded weights for chromatic media and long random walks)
- [x] Equi-angular sampling of point and spot lights inside media
- [x] Subsurface scattering: random walk under a dielectric interface, albedo inversion (Chiang et al. 2016), textured colour
- [x] Path guiding (Practical Path Guiding SD-tree, trained progressively, BSDF / guide mixture) — chosen over BDPT as it fits the progressive renderer, every camera model and media
- [x] Low-discrepancy sampling: Owen-scrambled, shuffled Sobol (default) and a blue-noise variant (void-and-cluster mask)

Possible follow-ups: guiding of volume vertices and product (BSDF × radiance) guiding, a learned BSDF / guide
fraction, spatial filtering of guiding records; majorant grids (DDA) for sparse heterogeneous media; emissive media;
equi-angular sampling for area lights and MIS with distance sampling; nested media with priorities; glass-bounded
media reached through specular chains (BDPT / manifold walks); SSS with internal Fresnel reflection at exit points.

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
