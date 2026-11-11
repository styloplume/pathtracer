# Scene format

Scenes are [TOML](https://toml.io) files. The parser (`crates/pt-core/src/toml/`) supports the common subset: tables `[a.b]`,
arrays of tables `[[a]]`, dotted keys, basic and literal strings, integers and floats, booleans, (multi-line) arrays
and inline tables. Multi-line strings and dates are not supported. Errors report the file and line.

Conventions used below:

- **vec3**: `[x, y, z]`, or a single number used for all three components (`albedo = 0.8`).
- **color**: a vec3 of linear RGB values.
- **texture**: a color or scalar parameter that also accepts an image or a procedural pattern (see [Textures](#textures)).
- Angles are in degrees. Scene units are arbitrary, except that the physical camera parameters assume meters.
- Right-handed coordinates, +Y up. Relative paths are relative to the file that contains them (also inside
  included files).
- Unknown keys are ignored with a warning (that suggests the closest known key); invalid values (wrong type, out of
  range, unknown names) are errors.

A scene has these top-level sections, all optional:

| Section | Purpose |
|---|---|
| `include` | other scene files to merge in |
| `[render]` | resolution, sampling, integrator settings |
| `[camera]` | camera position, lens, projection, motion |
| `[output]` | where and how renders are saved |
| `[background]` | environment light (color, gradient, image, sky) |
| `[[lights]]` | point, spot and directional lights |
| `[media.NAME]` | participating media |
| `[materials.NAME]` | named materials |
| `[prototypes.NAME]` | geometry that is placed by instances |
| `[[objects]]` | the scene's geometry |

Example scenes in `scenes/` cover every feature; `scenes/cornell.toml` is annotated.

## include

```toml
include = "lib/materials.toml"          # or a list: ["a.toml", "b.toml"]
```

Included files are loaded first (recursively; cycles are an error), then merged with the including file:

- `[[objects]]` and `[[lights]]` are concatenated, included entries first;
- `[render]`, `[camera]`, `[output]`, `[materials]`, `[media]` and `[prototypes]` are merged one level deep, so the
  including file can override single settings or whole named materials;
- anything else, such as `[background]`, is replaced.

Included files are watched for hot reload like the scene itself.

## [render]

| Key | Default | Meaning |
|---|---|---|
| `width`, `height` | 960, 540 | image size in pixels (`--size WxH` overrides) |
| `spp` | 256 | samples per pixel; the maximum when adaptive sampling is on (`--spp`) |
| `max_depth` | 8 | maximum number of surface bounces |
| `max_volume_depth` | 256 | maximum scattering events in media and subsurface random-walk steps |
| `threads` | 0 | worker threads, 0 = one per logical CPU (`--threads`) |
| `initial_block` | 32 | size of the coarsest preview blocks (rounded up to a power of two, 1–256) |
| `exposure` | 0 | exposure in stops |
| `tonemap` | `"aces"` | `aces` (alias `filmic`), `reinhard`, `none` (aliases `clamp`, `linear`) |
| `clamp` | 0 | clamp each sample's luminance to this value (biased firefly removal); 0 = off |
| `seed` | 0 | random seed (integer) |
| `filter` | `"gaussian"` | pixel filter: `box`, `tent`, `gaussian`, `blackman-harris` |
| `filter_radius` | 0.5 / 1 / 1.5 / 1.5 | filter radius in pixels, in (0, 8]; default depends on the filter |
| `noise_threshold` | 0 | adaptive sampling: pixels whose relative noise is below this stop (e.g. `0.02`); 0 = off (`--noise`) |
| `min_spp` | 16 | samples every pixel gets before adaptive sampling may stop it |
| `time_limit` | 0 | stop after this many seconds; 0 = no limit (`--time`) |
| `spectral` | false | hero-wavelength spectral rendering (needed for dispersion) (`--spectral`) |
| `sampler` | `"sobol"` | `sobol`, `bluenoise`, `independent` (`--sampler`) |
| `light_sampler` | `"bvh"` | how a light is picked for direct lighting: `bvh`, `power`, `uniform` |
| `guiding` | false | path guiding (`--guiding`) |
| `guiding_spp` | 128 | samples per pixel spent training the guiding distribution |

## [camera]

| Key | Default | Meaning |
|---|---|---|
| `position` | `[0, 0, 5]` | eye position |
| `look_at` | `[0, 0, 0]` | point the camera looks at |
| `up` | `[0, 1, 0]` | up direction |
| `projection` (alias `type`) | `"perspective"` | `perspective`, `orthographic` (`ortho`), `fisheye`, `equirect` (360° panorama) |
| `fov` | 40 (fisheye: 180) | vertical field of view in degrees; for fisheye, the image circle's angle (up to 360) |
| `focal_length` | — | lens focal length in mm; overrides `fov` |
| `sensor_height` | 24 | sensor height in mm, for `focal_length` |
| `aperture` | 0 | lens diameter in scene units (depth of field); 0 = pinhole |
| `f_stop` | — | f-number: sets `aperture` = focal length / f-number (in meters) |
| `focus_distance` | distance to `look_at` | distance of the plane in focus |
| `ortho_height` | from `fov` and distance | height of the orthographic view in scene units |
| `iso`, `shutter_speed` | 100, 0.01 | physical exposure: scene values are scaled by t·S / (120·N²); needs `f_stop` |
| `shutter` | `[0, 1]` | shutter `[open, close]` time (or just the close time) for motion blur |
| `motion` | — | camera at shutter close: `{ position, look_at, up }`; unset fields keep their value |

## [output]

| Key | Default | Meaning |
|---|---|---|
| `path` | `renders/{scene}` (window saves: `renders/{scene}_{time}`) | output path without extension; `{scene}` is the scene file's name, `{time}` a Unix timestamp |
| `formats` | `["png"]` | any of `png` (tonemapped 8-bit sRGB), `hdr` (Radiance RGBE), `pfm` (float); a string or a list |
| `denoise` | false | also write `<path>_denoised.<ext>` (`--denoise`) |
| `aovs` | `[]` | also write `albedo`, `normal`, `depth` images as `<path>_<aov>.<ext>` |
| `save_on_finish` | false | window mode: save whenever a render finishes (`--autosave`) |

`-o FILE` on the command line sets the path; a `.png`, `.hdr` or `.pfm` extension also selects that single format.

## [background]

The environment seen by escaping rays. It is importance-sampled as a light. Common keys: `intensity` (multiplier,
default 1) and `rotate` (degrees: a number rotates about +Y, a vec3 about X, then Y, then Z).

| `type` | Keys |
|---|---|
| `color` (default; alias `constant`) | `color` (default black) |
| `gradient` | `zenith`, `horizon`, `ground` colors |
| `image` (default if `file` is given; aliases `envmap`, `hdri`) | `file`: equirectangular image (`.hdr`, `.pfm`, `.png`, `.jpg`, ...; 8/16-bit images are decoded as sRGB); `color`: multiplier |
| `sky` (aliases `daylight`, `preetham`) | `sun_elevation` (45), `sun_azimuth` (0; 0 = sun towards −Z, 90 = towards +X) or `sun_direction`; `turbidity` (3, in [1.7, 10]); `ground` albedo (0.2); `sun` (true: add the sun disk as a light); `sun_size` (1 = the real sun; larger gives softer shadows with the same irradiance) |

## [[lights]]

Lights without geometry. Every light takes `color` (default white) and/or a black-body `temperature` in Kelvin
(500–50000), which multiply, and an optional `name`, a label for the editor.

| `type` | Keys |
|---|---|
| `point` | `position`; `intensity` (radiant intensity, default 1) or `power` (total emitted power) |
| `spot` (alias `spotlight`) | `position`; `direction` or `look_at`; `angle` (half angle of the cone, default 30); `inner_angle` (full intensity inside, default 0.8 × `angle`); `intensity` or `power` |
| `directional` (aliases `distant`, `sun`) | `direction` (the direction light travels) or `elevation` / `azimuth` of the light's position (as for the sky); `intensity` = irradiance on a surface facing the light; `angle`: angular diameter in degrees (0 = hard shadows, up to 90) |

Emissive geometry (`type = "light"` materials) is an area light. It is sampled automatically.

## [media.NAME]

Participating media, referenced by materials' `interior` / `exterior` keys.

| Key | Default | Meaning |
|---|---|---|
| `albedo` (alias `color`) | 0.9 | single-scattering albedo per channel, in [0, 1] |
| `density` | 1 | extinction coefficient (per scene unit) per channel |
| `sigma_a`, `sigma_s` | — | absorption and scattering coefficients; used instead of `albedo` / `density` if either is given |
| `scale` | 1 | multiplies the coefficients |
| `g` | 0 | Henyey–Greenstein anisotropy, in (−1, 1) (positive = forward scattering) |
| `grid` | — | heterogeneous: a Mitsuba `.vol` voxel grid (float32 or uint8) scaling the density; `min` / `max` override its bounds |
| `field` | — | heterogeneous: a world-space procedural texture scaling the density (a noise fractal, `hypertexture`, or `checker` with `space = "world"`); a hypertexture field also limits free-flight sampling to its bounds |

## [materials.NAME]

Objects reference materials by name (`material = "NAME"`) or define one inline (`material = { type = "metal" }`).
Objects without a material get a gray diffuse `default`. Defining `[materials.default]` replaces it.

| `type` | Keys (defaults) |
|---|---|
| `diffuse` (default; alias `lambert`) | `albedo` / `color` texture (0.8) |
| `metal` (alias `conductor`) | `color` / `albedo` texture (0.9; 1 with a preset); `roughness` texture (0, GGX); Fresnel: `preset` (`gold`, `silver`, `copper`, `aluminium`, `iron`, `chromium`, `platinum`) or `eta` + `k` (complex IOR per channel), else Schlick with `color` as F0; `multiscatter` (true: energy compensation for rough metals) |
| `glass` (alias `dielectric`) | `ior` (1.5); `roughness` texture (0); `abbe` (0 = no dispersion; lower = stronger, needs spectral rendering); absorption: `color` = transmittance after `distance` (1) scene units, or `absorption` = coefficient per unit |
| `plastic` (alias `coated`) | `color` / `albedo` texture (0.8) under a dielectric coat; `roughness` texture (0); `ior` (1.5) |
| `principled` (aliases `pbr`, `disney`, `uber`) | textures: `base_color` / `color` / `albedo` (0.8), `metallic` (0), `roughness` (0.5), `specular` (0.5), `transmission` (0), `clearcoat` (0), `clearcoat_roughness` (0.03), `sheen` (0), `sheen_tint` (0.5); `ior` (overrides `specular`) |
| `subsurface` (alias `sss`) | `color` / `albedo` texture (0.8): overall albedo; `radius` (`[1, 0.45, 0.25]`) × `scale` (0.1): mean free path per channel; `ior` (1.4); `roughness` texture (0.3) of the interface; `g` (0) |
| `light` (aliases `emissive`, `emitter`) | `emission` / `color` texture (1) × `intensity` (1) × black-body `temperature`; `two_sided` (false: emits on the side of the surface normal) |
| `interface` (aliases `null`, `boundary`) | invisible surface that only separates media; needs `interior` and/or `exterior` |

Keys every material accepts:

| Key | Meaning |
|---|---|
| `normal_map` | tangent-space normal map texture (linear by default); `normal_strength` (1) scales it |
| `bump` | height texture; `bump_scale` (0.01) is the height of value 1 in scene units. Solid patterns between constant `low` / `high` (noise fractals, `marble`, `wood` in world space) tilt the normal by their analytic gradient and need no UVs; other textures use finite differences in UV |
| `alpha` | coverage texture: 0 = cut out, fractions are resolved stochastically (also in shadows); uses the image's alpha channel if it has one |
| `interior` (alias `medium`), `exterior` | names of media inside / outside closed surfaces (not for `subsurface`, which has its own) |

## Textures

Wherever a parameter above is marked *texture*, it can be:

- a number or a color: a constant;
- a string: an image file;
- a table:

| `type` | Keys |
|---|---|
| `image` (default if `file` is given) | `file`; `wrap`: `repeat` (default), `clamp`, `mirror`; `scale`, `offset`: UV transform (`[u, v]` or a number); `filter`: `bilinear` (default) or `nearest`; `colorspace`: `srgb` or `linear` (default: sRGB for colors, linear for scalars, normal maps and alpha); `channel`: `r`, `g`, `b`, `a`, `luminance` (for scalar parameters; default `auto`: gray, or luminance of RGB) |
| `checker` (alias `checkerboard`) | `even` / `color1` (0.8), `odd` / `color2` (0.2): textures; `scale` (8: squares per UV unit or per scene unit); `space`: `uv` (default) or `world` |
| `noise` (alias `fbm`), `turbulence`, `ridged`, `hybrid`, `terrain` (alias `hetero_terrain`) | `low` / `color1` (0), `high` / `color2` (1): textures blended by a noise fractal (see below); `turbulence` (true: turbulence for `type = "noise"`); `space`: `world` (default) or `uv` |
| `marble` | as the fractals; veins across x, bent by turbulence `amount` (4) |
| `wood` | as the fractals; rings around the y axis, `scale` per unit, wobbled by fBm `amount` (0.5) |
| `hypertexture` | the density of a hypertexture, in [0, 1], for media fields: `shape`, `softness`, `noise`, `amount`, `bias`, `gain` as for the object (see [Implicit surfaces](#implicit-surfaces)) |

Any texture table also takes `multiplier` (alias `multiply`): a number or color it is multiplied by.

Images can also be projected onto surfaces without UVs: `projection = "triplanar"` maps the texture-space position
(object space for instances, world space otherwise) along the three axes, blended by the normal with `sharpness` (4);
`scale` then repeats the image per scene unit.

**Noise fractals** are sums of Perlin noise octaves, normalized to roughly [0, 1]. Keys (also of `noise = { ... }`
tables in implicit surfaces):

| Key | Default | Meaning |
|---|---|---|
| `scale` | 4 | frequency of the first octave (per scene unit) |
| `octaves` | 5 | number of octaves (1–16, may be fractional) |
| `lacunarity` | 2 | frequency ratio of successive octaves |
| `gain` or `h` | per type | amplitude ratio of successive octaves, or the fractal increment H (gain = lacunarity^−H) |
| `offset` | per type | Musgrave's offset (`ridged` 1, `hybrid` 0.7, `terrain` 0.3) |
| `sharpness` | 2 | `ridged`: how strongly a ridge suppresses detail on the next octave |
| `warp` | 0 | domain warping: the position is displaced by fBm, up to this many first-octave wavelengths |

`fbm` (mean ½) and `turbulence` (sum of |noise|, billowy) use gain 0.5. `ridged` (sharp crests), `hybrid` (smooth valleys,
rough peaks) and `terrain` (roughness grows with height) are Musgrave's multifractals. World-space fractals are
antialiased: octaves finer than the pixel's footprint (tracked along the path as a ray cone, widened at glossy and
diffuse bounces) fade out, replaced by their average.

Image formats: PNG (any color type and bit depth), baseline JPEG, TGA, PPM / PGM, PFM, Radiance HDR. Texture
coordinates follow the OBJ convention (v = 0 at the bottom of the image).

## [[objects]]

Every object has a `type`, an optional `material`, an optional `name` (a label for the editor), and a transform. The transform is applied as `scale`, then
`rotate` (degrees about X, then Y, then Z), then `translate`:

| Key | Default | Meaning |
|---|---|---|
| `translate` | `[0, 0, 0]` | translation |
| `rotate` | `[0, 0, 0]` | rotation in degrees about X, then Y, then Z |
| `scale` | 1 | scale (a number, or per axis; must not be zero) |
| `motion` | — | the transform at shutter close: `{ translate, rotate, scale }`; unset fields keep their start values. Rotations interpolate per axis, so `rotate = [0, 720, 0]` spins twice |

| `type` | Keys (defaults) |
|---|---|
| `sphere` | `center` (origin), `radius` (1) |
| `disk` (alias `disc`) | `center`, `radius` (1), `inner_radius` (0: an annulus if > 0), `normal` (`[0, 1, 0]`) |
| `cylinder` (alias `tube`) | `base` (origin), `axis` (`[0, 1, 0]`), `radius` (1), `height` (1), `capped` (true: closed by two disks) |
| `box` (alias `cube`) | `min` and `max`, or `center` (origin) and `size` (1) |
| `quad` | `origin`, `u`, `v`: the parallelogram origin, origin + u, origin + u + v, origin + v. Its normal is u × v; UVs run from (0, 0) to (1, 1) |
| `triangle` | `p0`, `p1`, `p2` |
| `mesh` (default if `file` is given; aliases `gltf`, `stl`) | `file`: `.obj`, `.ply`, `.stl`, `.gltf`, `.glb`; `smooth` (true: use the file's vertex normals). Polygons are fanned into triangles; a PLY `edge` element with `vertex1`, `vertex2`, `crease` (what the editor writes) holds edge creases, which the editor keeps (the tracer does not use them yet) |
| `instance` | `prototype`: name of a `[prototypes.NAME]`; `scatter` (see below). Instances cannot set a material |
| `sdf` (alias `implicit`), `hypertexture`, `julia`, `mandelbulb`, `menger` | implicit surfaces, see below |

Any of these shapes can be an area light by giving it a `light` material. Exceptions: emitters inside instances and
moving objects, and implicit surfaces, are not sampled as lights, so they only add light when a path hits them by chance.

**Mesh materials.** The object's `material` wins. Otherwise each material group of the file (OBJ `usemtl`, glTF
materials) uses the scene material with the same name if there is one, then the file's own material (OBJ `.mtl`
libraries and glTF metallic-roughness materials are converted), then `default`.

### [prototypes.NAME] and instances

A prototype is geometry that is only rendered where instances place it. A prototype is either one object, described
by the table itself, or a list of objects:

```toml
[prototypes.tree]
file = "models/tree.obj"
scale = 0.5

[prototypes.lamp]
[[prototypes.lamp.objects]]
type = "cylinder"
radius = 0.05
height = 2
[[prototypes.lamp.objects]]
type = "sphere"
center = [0, 2, 0]
radius = 0.2
material = "bulb"

[[objects]]
type = "instance"
prototype = "tree"
translate = [3, 0, 0]
```

Prototypes cannot contain instances or moving objects. `scatter` places many copies at once, each with a random
offset, rotation and scale on top of the instance's own transform:

```toml
[[objects]]
type = "instance"
prototype = "tree"
scatter = { count = 10000, min = [-50, 0, -50], max = [50, 0, 50], rotate = [0, 360, 0], scale = [0.7, 1.3], seed = 1 }
```

| `scatter` key | Meaning |
|---|---|
| `count` | number of copies (1 – 50 000 000) |
| `min`, `max` | translation offsets are uniform in this box |
| `rotate` | each axis rotates by a uniform angle in [0, value] degrees |
| `scale` | a uniform factor in `[lo, hi]` (or a number) |
| `seed` | random seed |

`scatter` cannot be combined with `motion`.

### Implicit surfaces

Surfaces defined by a field that is negative inside, found by sphere tracing: a ray steps by the field value divided
by the field's Lipschitz bound, which cannot cross the surface. They have no UVs (use solid textures, triplanar images
and solid bump maps), their first crossing only is found (alpha masks cannot reveal a far side), and transforms place
them as instances, so textures stay in object space. Every implicit object takes:

| Key | Default | Meaning |
|---|---|---|
| `epsilon` | 2·10⁻⁵ of the object's size; 2·10⁻⁴ for fractals and noisy fields | hit tolerance in object units; for fractals it sets the detail shown |
| `lipschitz` | 1 for distance fields and fractals; estimated for displacements and hypertextures | bound of the field's slope; raise it if a surface shows holes or speckles |
| `max_steps` | 512 (fractals 1024) | marching steps per ray |

**`sdf`**: `sdf = { type = ..., ... }`, a tree of nodes. Every node takes `translate`, `rotate` and `scale` (non-uniform
scales are allowed; distances then shrink by the smallest scale, which stays a safe bound).

| Node `type` | Keys (defaults) |
|---|---|
| `sphere` | `radius` (1) |
| `box` (alias `cube`) | `size` (1), `round` (0: edge radius) |
| `torus` | `major` (1), `minor` (0.25); the ring lies in the xz plane |
| `cylinder` | `radius` (0.5), `height` (1), centred, along y |
| `capsule` | `a` (`[0, -0.5, 0]`), `b` (`[0, 0.5, 0]`), `radius` (0.25) |
| `union`, `intersection` | `children = [ ... ]`; `blend` (0): width of the smooth blend between children |
| `difference` (alias `subtract`) | `children = [base, cut, ...]`: the base minus every cut; `blend` (0) |
| `round` | `child = { ... }`, `radius` (0.05): inflates the child |
| `shell` (alias `onion`) | `child`, `thickness` (0.05): a hollow skin |
| `displace` | `child`, `noise = { type, scale, octaves, ... }` (fbm, scale 4), `amount` (0.1): moves the surface out by `amount` × (noise − ½) |

```toml
[[objects]]
type = "sdf"
material = "marble"
sdf = { type = "difference", blend = 0.04, children = [
    { type = "union", blend = 0.15, children = [
        { type = "box", size = [1.1, 0.7, 1.1], round = 0.08 },
        { type = "torus", major = 0.62, minor = 0.12 },
    ] },
    { type = "sphere", radius = 0.45, translate = [0, 0.45, 0] },
] }
```

**`hypertexture`** (Perlin & Hoffert): the density of a soft object, `D0` = ½ − sdf / `softness` (1 inside, 0
outside, a ramp of width `softness` across the surface), is perturbed by noise and shaped:
density = gain(bias(clamp(D0 + `amount` × (noise − ½)))). The surface is where the density reaches `level`.

| Key | Default | Meaning |
|---|---|---|
| `shape` | unit sphere | the soft object: an SDF node as above |
| `softness` | 0.3 | width of the density ramp, in scene units |
| `noise` | turbulence, scale 3, 6 octaves | noise fractal table |
| `amount` | 0.6 | noise strength |
| `bias`, `gain` | 0.5, 0.5 | Perlin's bias (where ½ goes) and gain (S-curve contrast), in (0, 1); 0.5 = unchanged |
| `level` | 0.5 | density of the surface, in (0, 1) |

The same keys make a `type = "hypertexture"` texture, whose density can fill a medium (`field = { type =
"hypertexture", ... }`; its shape is in world space, inside the medium's boundary object).

**Fractals** use distance estimators and are as detailed as `epsilon`:

| `type` | Keys (defaults) |
|---|---|
| `julia` (alias `quaternion_julia`) | `c` (`[-1, 0.2, 0, 0]`): the quaternion constant of q → q² + c as `[real, i, j, k]`; `iterations` (12); the 3D slice of 4D space is the hyperplane q · `slice_normal` = `slice`, with `slice_normal` (`[0, 0, 0, 1]`, the k axis: scene x, y, z are then real, i, j) and `slice` (0) |
| `mandelbulb` | `power` (8, 2–32), `iterations` (10) |
| `menger` | `iterations` (4, at most 10): a Menger sponge filling [-1, 1]³ |
