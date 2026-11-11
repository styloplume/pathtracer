//! The scene format as data: every section, key, type, default, range and alias. The
//! loader turns tables into a `Scene` and stays the authority on what is valid; this table
//! describes the same format so that it can be validated without loading (unknown keys are
//! reported, with suggestions), so that the editor can generate its inspector, and so that
//! tests can check it against `docs/scene-format.md` and against the loader's parsers.

use crate::toml::{Table, Value};

/// The type of a key's value.
#[derive(Clone, Copy, Debug)]
pub enum Ty {
    Number,
    /// A non-negative integer.
    Integer,
    Bool,
    String,
    /// A file path, relative to the file it is written in.
    Path,
    /// `[u, v]` or a number for both.
    Vec2,
    /// `[x, y, z]` or a number for all three.
    Vec3,
    /// A vec3 of linear RGB.
    Color,
    /// `[x, y, z, w]`.
    Vec4,
    /// `[lo, hi]` or a single number.
    Pair,
    /// One of a set of names.
    Choice(&'static [Choice]),
    /// A name or a list of names from a set.
    Choices(&'static [Choice]),
    /// A string or a list of strings.
    Strings,
    /// A color parameter that also takes an image or a procedural texture.
    ColorTexture,
    /// A scalar parameter that also takes an image or a procedural texture.
    ScalarTexture,
    /// A material's name, or an inline material table.
    Material,
    /// The name of something defined elsewhere in the scene.
    Ref(RefKind),
    /// A table.
    Table(&'static Schema),
    /// An array of tables.
    Tables(&'static Schema),
    /// A table of named tables (`[materials.NAME]`).
    Named(&'static Schema),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefKind {
    Medium,
    Prototype,
}

#[derive(Clone, Copy, Debug)]
pub struct Choice {
    pub name: &'static str,
    pub aliases: &'static [&'static str],
}

impl Choice {
    fn matches(&self, s: &str) -> bool {
        let s = s.to_ascii_lowercase();
        self.name == s || self.aliases.contains(&s.as_str())
    }
}

/// What a key is when it is not set.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum DefaultValue {
    /// No default: the key is optional and has no effect when missing.
    None,
    /// The key must be given.
    Required,
    Number(f64),
    Bool(bool),
    Str(&'static str),
    Vec3([f64; 3]),
    /// Computed or context dependent, described in words.
    Derived(&'static str),
}

/// Allowed values of a number (or of every component of a vector).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Range {
    pub min: f64,
    pub max: f64,
    pub min_open: bool,
    pub max_open: bool,
}

impl Range {
    pub fn contains(&self, v: f64) -> bool {
        let above = if self.min_open { v > self.min } else { v >= self.min };
        let below = if self.max_open { v < self.max } else { v <= self.max };
        above && below
    }
}

impl std::fmt::Display for Range {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (lo, hi) = (if self.min_open { '(' } else { '[' }, if self.max_open { ')' } else { ']' });
        let n = |v: f64| if v.is_infinite() { "∞".to_owned() } else { crate::toml::format_number(v) };
        let min = if self.min == f64::NEG_INFINITY { "-∞".to_owned() } else { n(self.min) };
        let hi = if self.max.is_infinite() { ')' } else { hi };
        write!(f, "{lo}{min}, {}{hi}", n(self.max))
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Key {
    pub name: &'static str,
    pub aliases: &'static [&'static str],
    pub ty: Ty,
    pub default: DefaultValue,
    pub range: Option<Range>,
    pub doc: &'static str,
}

impl Key {
    fn is(&self, name: &str) -> bool {
        self.name == name || self.aliases.contains(&name)
    }
}

/// One `type` of a section with variants (a material type, an object type, ...).
#[derive(Clone, Copy, Debug)]
pub struct Variant {
    pub name: &'static str,
    pub aliases: &'static [&'static str],
    pub keys: &'static [&'static [Key]],
    pub doc: &'static str,
}

/// How a section's `type` key selects its variant.
#[derive(Clone, Copy, Debug)]
pub struct TypeRule {
    /// The variant when `type` is missing.
    pub default: Option<&'static str>,
    /// The variant when `type` is missing but `file` is given.
    pub with_file: Option<&'static str>,
    /// No variant is needed when this key is given (a prototype with `objects`).
    pub optional_with: Option<&'static str>,
}

/// A table of the format: its keys, and the variants its `type` selects.
#[derive(Debug)]
pub struct Schema {
    pub name: &'static str,
    /// Keys every variant accepts.
    pub keys: &'static [&'static [Key]],
    pub variants: &'static [Variant],
    pub types: TypeRule,
}

impl Schema {
    pub fn variant(&self, name: &str) -> Option<&'static Variant> {
        self.variants.iter().find(|v| v.name == name || v.aliases.contains(&name))
    }

    /// The keys of `variant` (or the common keys only).
    pub fn keys(&self, variant: Option<&Variant>) -> impl Iterator<Item = &'static Key> {
        let own = variant.map_or(&[][..], |v| v.keys);
        self.keys.iter().chain(own).flat_map(|k| k.iter())
    }

    pub fn key(&self, variant: Option<&Variant>, name: &str) -> Option<&'static Key> {
        self.keys(variant).find(|k| k.is(name))
    }

    /// The variant table `t` selects, by its `type` (or the type rule).
    pub fn variant_of(&self, t: &Table) -> Option<&'static Variant> {
        match t.get("type") {
            Some(Value::String(s)) => self.variant(s),
            Some(_) => None,
            None if self.types.with_file.is_some() && t.contains_key("file") => {
                self.types.with_file.and_then(|n| self.variant(n))
            }
            None => self.types.default.and_then(|n| self.variant(n)),
        }
    }
}

// ------------------------------------------------------------------ construction helpers

const fn key(name: &'static str, ty: Ty, default: DefaultValue, doc: &'static str) -> Key {
    Key { name, aliases: &[], ty, default, range: None, doc }
}

const fn choice(name: &'static str, aliases: &'static [&'static str]) -> Choice {
    Choice { name, aliases }
}

const fn variant(name: &'static str, aliases: &'static [&'static str], keys: &'static [&'static [Key]]) -> Variant {
    Variant { name, aliases, keys, doc: "" }
}

const NO_TYPES: TypeRule = TypeRule { default: None, with_file: None, optional_with: None };
const INF: f64 = f64::INFINITY;

impl Key {
    const fn alias(mut self, aliases: &'static [&'static str]) -> Key {
        self.aliases = aliases;
        self
    }
    const fn range(mut self, min: f64, max: f64) -> Key {
        self.range = Some(Range { min, max, min_open: false, max_open: false });
        self
    }
    /// Greater than zero.
    const fn positive(mut self) -> Key {
        self.range = Some(Range { min: 0.0, max: INF, min_open: true, max_open: false });
        self
    }
    const fn non_negative(self) -> Key {
        self.range(0.0, INF)
    }
    const fn open(mut self, min_open: bool, max_open: bool) -> Key {
        if let Some(r) = &mut self.range {
            r.min_open = min_open;
            r.max_open = max_open;
        }
        self
    }
}

use DefaultValue::{Derived, Required};
use Ty::{
    Bool, Choices, Color, ColorTexture, Integer, Material, Named, Number, Pair, Path, Ref, ScalarTexture, Strings,
    Tables, Vec2, Vec3, Vec4,
};

const fn num(v: f64) -> DefaultValue {
    DefaultValue::Number(v)
}
const fn vec3(x: f64, y: f64, z: f64) -> DefaultValue {
    DefaultValue::Vec3([x, y, z])
}
const fn flag(b: bool) -> DefaultValue {
    DefaultValue::Bool(b)
}
const fn name(s: &'static str) -> DefaultValue {
    DefaultValue::Str(s)
}
const NONE: DefaultValue = DefaultValue::None;

// ------------------------------------------------------------------ the scene

pub static SCENE: Schema = Schema {
    name: "scene",
    keys: &[&[
        key("include", Strings, NONE, "other scene files to merge in"),
        key("render", Ty::Table(&RENDER), NONE, "resolution, sampling, integrator settings"),
        key("camera", Ty::Table(&CAMERA), NONE, "camera position, lens, projection, motion"),
        key("output", Ty::Table(&OUTPUT), NONE, "where and how renders are saved"),
        key("background", Ty::Table(&BACKGROUND), NONE, "environment light"),
        key("lights", Tables(&LIGHT), NONE, "point, spot and directional lights"),
        key("media", Named(&MEDIUM), NONE, "participating media"),
        key("materials", Named(&MATERIAL), NONE, "named materials"),
        key("prototypes", Named(&PROTOTYPE), NONE, "geometry placed by instances"),
        key("objects", Tables(&OBJECT), NONE, "the scene's geometry"),
    ]],
    variants: &[],
    types: NO_TYPES,
};

pub static TONEMAPS: &[Choice] =
    &[choice("aces", &["filmic"]), choice("reinhard", &[]), choice("none", &["clamp", "linear"])];
pub static FILTERS: &[Choice] = &[
    choice("box", &[]),
    choice("tent", &["triangle"]),
    choice("gaussian", &["gauss"]),
    choice("blackman-harris", &["blackmanharris", "bh"]),
];
pub static SAMPLERS: &[Choice] = &[
    choice("sobol", &["owen", "low-discrepancy"]),
    choice("bluenoise", &["blue-noise", "blue_noise", "blue"]),
    choice("independent", &["random", "uniform"]),
];
pub static LIGHT_SAMPLERS: &[Choice] = &[choice("bvh", &[]), choice("power", &[]), choice("uniform", &[])];

pub static RENDER: Schema = Schema {
    name: "[render]",
    keys: &[&[
        key("width", Integer, num(960.0), "image width in pixels").range(1.0, INF),
        key("height", Integer, num(540.0), "image height in pixels").range(1.0, INF),
        key("spp", Integer, num(256.0), "samples per pixel; the maximum with adaptive sampling").range(1.0, INF),
        key("max_depth", Integer, num(8.0), "maximum number of surface bounces"),
        key("max_volume_depth", Integer, num(256.0), "maximum scattering events in media and subsurface walks"),
        key("threads", Integer, num(0.0), "worker threads, 0 = one per logical CPU"),
        key("initial_block", Integer, num(32.0), "size of the coarsest preview blocks").range(1.0, 256.0),
        key("exposure", Number, num(0.0), "exposure in stops"),
        key("tonemap", Ty::Choice(TONEMAPS), name("aces"), "tone mapping curve"),
        key("clamp", Number, num(0.0), "clamp each sample's luminance (firefly removal); 0 = off").non_negative(),
        key("seed", Integer, num(0.0), "random seed"),
        key("filter", Ty::Choice(FILTERS), name("gaussian"), "pixel reconstruction filter"),
        key("filter_radius", Number, Derived("0.5 / 1 / 1.5 / 1.5, by filter"), "filter radius in pixels")
            .range(0.0, 8.0)
            .open(true, false),
        key("noise_threshold", Number, num(0.0), "adaptive sampling: relative noise at which pixels stop; 0 = off")
            .non_negative(),
        key("min_spp", Integer, num(16.0), "samples every pixel gets before adaptive sampling may stop it"),
        key("time_limit", Number, num(0.0), "stop after this many seconds; 0 = no limit").non_negative(),
        key("spectral", Bool, flag(false), "hero-wavelength spectral rendering (needed for dispersion)"),
        key("sampler", Ty::Choice(SAMPLERS), name("sobol"), "sample generator"),
        key("light_sampler", Ty::Choice(LIGHT_SAMPLERS), name("bvh"), "how a light is picked for direct lighting"),
        key("guiding", Bool, flag(false), "path guiding"),
        key("guiding_spp", Integer, num(128.0), "samples per pixel spent training the guiding distribution")
            .range(1.0, INF),
    ]],
    variants: &[],
    types: NO_TYPES,
};

pub static PROJECTIONS: &[Choice] = &[
    choice("perspective", &["pinhole", "thinlens"]),
    choice("orthographic", &["ortho"]),
    choice("fisheye", &[]),
    choice("equirect", &["equirectangular", "spherical", "panorama", "360"]),
];

static CAMERA_POSE: &[Key] = &[
    key("position", Vec3, vec3(0.0, 0.0, 5.0), "eye position"),
    key("look_at", Vec3, vec3(0.0, 0.0, 0.0), "point the camera looks at"),
    key("up", Vec3, vec3(0.0, 1.0, 0.0), "up direction"),
];

pub static CAMERA_MOTION: Schema =
    Schema { name: "[camera] motion", keys: &[CAMERA_POSE], variants: &[], types: NO_TYPES };

pub static CAMERA: Schema = Schema {
    name: "[camera]",
    keys: &[
        CAMERA_POSE,
        &[
            key("projection", Ty::Choice(PROJECTIONS), name("perspective"), "camera projection").alias(&["type"]),
            key("fov", Number, Derived("40 (fisheye: 180)"), "vertical field of view in degrees")
                .range(0.0, 360.0)
                .open(true, false),
            key("focal_length", Number, NONE, "lens focal length in mm; overrides fov").positive(),
            key("sensor_height", Number, num(24.0), "sensor height in mm, for focal_length").positive(),
            key("aperture", Number, num(0.0), "lens diameter in scene units; 0 = pinhole").non_negative(),
            key("f_stop", Number, NONE, "f-number: sets the aperture from the focal length").positive(),
            key("focus_distance", Number, Derived("distance to look_at"), "distance of the plane in focus"),
            key("ortho_height", Number, Derived("from fov and distance"), "height of the orthographic view").positive(),
            key("iso", Number, num(100.0), "physical exposure: sensor sensitivity (needs f_stop)").positive(),
            key("shutter_speed", Number, num(0.01), "physical exposure: seconds (needs f_stop)").positive(),
            key("shutter", Pair, Derived("[0, 1]"), "shutter [open, close] time, or the close time"),
            key("motion", Ty::Table(&CAMERA_MOTION), NONE, "camera at shutter close; unset fields keep their value"),
        ],
    ],
    variants: &[],
    types: NO_TYPES,
};

pub static FORMATS: &[Choice] = &[choice("png", &[]), choice("hdr", &["rgbe"]), choice("pfm", &[])];
pub static AOVS: &[Choice] = &[choice("albedo", &[]), choice("normal", &["normals"]), choice("depth", &["z"])];

pub static OUTPUT: Schema = Schema {
    name: "[output]",
    keys: &[&[
        key("path", Ty::String, Derived("renders/{scene}"), "output path without extension"),
        key("formats", Choices(FORMATS), name("png"), "image formats to write"),
        key("denoise", Bool, flag(false), "also write a denoised image"),
        key("aovs", Choices(AOVS), Derived("none"), "also write these feature images"),
        key("save_on_finish", Bool, flag(false), "window mode: save whenever a render finishes"),
    ]],
    variants: &[],
    types: NO_TYPES,
};

pub static BACKGROUND: Schema = Schema {
    name: "[background]",
    keys: &[&[
        key("type", Ty::String, Derived("image if file is given, else color"), "kind of environment"),
        key("intensity", Number, num(1.0), "multiplier"),
        key("rotate", Vec3, num(0.0), "degrees: a number rotates about +Y, a vec3 about X, then Y, then Z"),
    ]],
    variants: &[
        variant("color", &["constant"], &[&[key("color", Color, vec3(0.0, 0.0, 0.0), "constant color")]]),
        variant(
            "gradient",
            &[],
            &[&[
                key("zenith", Color, vec3(0.25, 0.45, 0.9), "color straight up"),
                key("horizon", Color, vec3(0.9, 0.95, 1.0), "color at the horizon"),
                key("ground", Color, vec3(0.3, 0.28, 0.25), "color below the horizon"),
            ]],
        ),
        variant(
            "image",
            &["envmap", "hdri"],
            &[&[
                key("file", Path, Required, "equirectangular image"),
                key("color", Color, vec3(1.0, 1.0, 1.0), "multiplier"),
            ]],
        ),
        variant(
            "sky",
            &["daylight", "preetham"],
            &[&[
                key("sun_elevation", Number, num(45.0), "sun height above the horizon, degrees"),
                key("sun_azimuth", Number, num(0.0), "sun direction: 0 = towards -Z, 90 = towards +X"),
                key("sun_direction", Vec3, NONE, "direction towards the sun (instead of the angles)"),
                key("turbidity", Number, num(3.0), "haziness").range(1.7, 10.0),
                key("ground", Color, vec3(0.2, 0.2, 0.2), "ground albedo"),
                key("sun", Bool, flag(true), "add the sun disk as a light"),
                key("sun_size", Number, num(1.0), "1 = the real sun; larger is softer at the same irradiance")
                    .range(0.0, 100.0)
                    .open(true, false),
            ]],
        ),
    ],
    types: TypeRule { default: Some("color"), with_file: Some("image"), optional_with: None },
};

static LIGHT_POWER: &[Key] = &[
    key("intensity", Number, num(1.0), "radiant intensity"),
    key("power", Number, NONE, "total emitted power (instead of intensity)"),
];

pub static LIGHT: Schema = Schema {
    name: "[[lights]]",
    keys: &[&[
        key("type", Ty::String, Required, "kind of light"),
        key("name", Ty::String, NONE, "label shown in the editor"),
        key("color", Color, vec3(1.0, 1.0, 1.0), "color, multiplied by the temperature's"),
        key("temperature", Number, NONE, "black-body color temperature in Kelvin").range(500.0, 50_000.0),
    ]],
    variants: &[
        variant("point", &[], &[&[key("position", Vec3, Required, "light position")], LIGHT_POWER]),
        variant(
            "spot",
            &["spotlight"],
            &[
                &[
                    key("position", Vec3, Required, "light position"),
                    key("direction", Vec3, NONE, "direction the light points"),
                    key("look_at", Vec3, NONE, "point the light points at (instead of direction)"),
                    key("angle", Number, num(30.0), "half angle of the cone, degrees")
                        .range(0.0, 180.0)
                        .open(true, false),
                    key("inner_angle", Number, Derived("0.8 × angle"), "full intensity inside this half angle")
                        .non_negative(),
                ],
                LIGHT_POWER,
            ],
        ),
        variant(
            "directional",
            &["distant", "sun"],
            &[&[
                key("direction", Vec3, NONE, "direction the light travels"),
                key("elevation", Number, num(45.0), "height of the light above the horizon, degrees"),
                key("azimuth", Number, num(0.0), "direction of the light: 0 = towards -Z, 90 = towards +X"),
                key("intensity", Number, num(1.0), "irradiance on a surface facing the light"),
                key("angle", Number, num(0.0), "angular diameter in degrees; 0 = hard shadows").range(0.0, 90.0),
            ]],
        ),
    ],
    types: TypeRule { default: None, with_file: None, optional_with: None },
};

pub static MEDIUM: Schema = Schema {
    name: "[media.NAME]",
    keys: &[&[
        key("albedo", Color, num(0.9), "single-scattering albedo").alias(&["color"]).range(0.0, 1.0),
        key("density", Color, num(1.0), "extinction coefficient per scene unit"),
        key("sigma_a", Color, NONE, "absorption coefficient (instead of albedo and density)").non_negative(),
        key("sigma_s", Color, NONE, "scattering coefficient (instead of albedo and density)").non_negative(),
        key("scale", Number, num(1.0), "multiplies the coefficients"),
        key("g", Number, num(0.0), "Henyey-Greenstein anisotropy").range(-1.0, 1.0).open(true, true),
        key("grid", Path, NONE, "Mitsuba .vol voxel grid scaling the density"),
        key("min", Vec3, NONE, "grid bounds: minimum corner (default: the file's)"),
        key("max", Vec3, NONE, "grid bounds: maximum corner (default: the file's)"),
        key("field", ScalarTexture, NONE, "world-space procedural texture scaling the density"),
    ]],
    variants: &[],
    types: NO_TYPES,
};

pub static METAL_PRESETS: &[Choice] = &[
    choice("gold", &["au"]),
    choice("silver", &["ag"]),
    choice("copper", &["cu"]),
    choice("aluminium", &["aluminum", "al"]),
    choice("iron", &["fe"]),
    choice("chromium", &["chrome", "cr"]),
    choice("platinum", &["pt"]),
];

static MATERIAL_ROUGHNESS: Key = key("roughness", ScalarTexture, num(0.0), "GGX roughness");

pub static MATERIAL: Schema = Schema {
    name: "[materials.NAME]",
    keys: &[&[
        key("type", Ty::String, name("diffuse"), "kind of material"),
        key("normal_map", ColorTexture, NONE, "tangent-space normal map"),
        key("normal_strength", Number, num(1.0), "scales the normal map"),
        key("bump", ScalarTexture, NONE, "height texture"),
        key("bump_scale", Number, num(0.01), "height of value 1 in scene units"),
        key("alpha", ScalarTexture, NONE, "coverage: 0 = cut out"),
        key("interior", Ref(RefKind::Medium), NONE, "medium inside closed surfaces").alias(&["medium"]),
        key("exterior", Ref(RefKind::Medium), NONE, "medium outside closed surfaces"),
    ]],
    variants: &[
        variant("diffuse", &["lambert"], &[&[key("albedo", ColorTexture, num(0.8), "reflectance").alias(&["color"])]]),
        variant(
            "metal",
            &["conductor"],
            &[&[
                key("color", ColorTexture, Derived("0.9; 1 with a preset or eta / k"), "reflectance tint")
                    .alias(&["albedo"]),
                MATERIAL_ROUGHNESS,
                key("preset", Ty::Choice(METAL_PRESETS), NONE, "measured complex IOR of a metal"),
                key("eta", Color, NONE, "complex IOR, real part per channel (with k)"),
                key("k", Color, NONE, "complex IOR, imaginary part per channel (with eta)"),
                key("multiscatter", Bool, flag(true), "energy compensation for rough metals"),
            ]],
        ),
        variant(
            "glass",
            &["dielectric"],
            &[&[
                key("ior", Number, num(1.5), "index of refraction").positive(),
                MATERIAL_ROUGHNESS,
                key("abbe", Number, num(0.0), "Abbe number; 0 = no dispersion").non_negative(),
                key("color", Color, num(1.0), "transmittance after 'distance' scene units").range(0.0, 1.0),
                key("distance", Number, num(1.0), "distance at which 'color' is reached").positive(),
                key("absorption", Color, NONE, "absorption coefficient per unit (instead of color)"),
            ]],
        ),
        variant(
            "plastic",
            &["coated"],
            &[&[
                key("color", ColorTexture, num(0.8), "base color under the coat").alias(&["albedo"]),
                MATERIAL_ROUGHNESS,
                key("ior", Number, num(1.5), "index of refraction of the coat").range(1.0, INF),
            ]],
        ),
        variant(
            "principled",
            &["pbr", "disney", "uber"],
            &[&[
                key("base_color", ColorTexture, num(0.8), "base color").alias(&["color", "albedo"]),
                key("metallic", ScalarTexture, num(0.0), "0 = dielectric, 1 = metal"),
                key("roughness", ScalarTexture, num(0.5), "GGX roughness"),
                key("specular", ScalarTexture, num(0.5), "dielectric reflectance"),
                key("transmission", ScalarTexture, num(0.0), "glass-like transmission"),
                key("clearcoat", ScalarTexture, num(0.0), "clear coat layer"),
                key("clearcoat_roughness", ScalarTexture, num(0.03), "roughness of the clear coat"),
                key("sheen", ScalarTexture, num(0.0), "cloth-like rim reflection"),
                key("sheen_tint", ScalarTexture, num(0.5), "tints the sheen towards the base color"),
                key("ior", Number, NONE, "index of refraction (overrides specular)").range(1.0, INF),
            ]],
        ),
        variant(
            "subsurface",
            &["sss"],
            &[&[
                key("color", ColorTexture, num(0.8), "overall albedo").alias(&["albedo"]),
                key("radius", Color, vec3(1.0, 0.45, 0.25), "mean free path per channel (times scale)"),
                key("scale", Number, num(0.1), "multiplies radius").positive(),
                key("ior", Number, num(1.4), "index of refraction").range(1.0, INF),
                key("roughness", ScalarTexture, num(0.3), "roughness of the interface"),
                key("g", Number, num(0.0), "phase function anisotropy").range(-1.0, 1.0).open(true, true),
            ]],
        ),
        variant(
            "light",
            &["emissive", "emitter"],
            &[&[
                key("emission", ColorTexture, num(1.0), "emitted radiance").alias(&["color"]),
                key("intensity", Number, num(1.0), "multiplier"),
                key("temperature", Number, NONE, "black-body color temperature in Kelvin").range(500.0, 50_000.0),
                key("two_sided", Bool, flag(false), "emit on both sides"),
            ]],
        ),
        variant("interface", &["null", "boundary"], &[]),
    ],
    types: TypeRule { default: Some("diffuse"), with_file: None, optional_with: None },
};

pub static WRAPS: &[Choice] = &[choice("repeat", &["wrap"]), choice("clamp", &["edge"]), choice("mirror", &[])];
pub static TEXTURE_FILTERS: &[Choice] = &[choice("bilinear", &["linear"]), choice("nearest", &["point", "closest"])];
pub static COLORSPACES: &[Choice] = &[choice("srgb", &[]), choice("linear", &["raw", "non-color"])];
pub static CHANNELS: &[Choice] = &[
    choice("r", &["red"]),
    choice("g", &["green"]),
    choice("b", &["blue"]),
    choice("a", &["alpha"]),
    choice("luminance", &["lum", "y"]),
    choice("auto", &[]),
];
pub static SPACES: &[Choice] = &[choice("uv", &[]), choice("world", &["object"])];
pub static UV_PROJECTIONS: &[Choice] = &[choice("uv", &[]), choice("triplanar", &[])];
pub static FRACTALS: &[Choice] = &[
    choice("fbm", &["noise"]),
    choice("turbulence", &[]),
    choice("ridged", &["ridged_multifractal"]),
    choice("hybrid", &["hybrid_multifractal"]),
    choice("terrain", &["hetero_terrain"]),
];

/// Noise fractal parameters (without `scale`, whose default depends on the use).
static FRACTAL: &[Key] = &[
    key("octaves", Number, num(5.0), "number of octaves, may be fractional").range(1.0, 16.0),
    key("lacunarity", Number, num(2.0), "frequency ratio of successive octaves").range(1.0, INF).open(true, false),
    key("gain", Number, Derived("per type"), "amplitude ratio of successive octaves").positive(),
    key("h", Number, NONE, "fractal increment H (instead of gain: gain = lacunarity^-H)"),
    key("offset", Number, Derived("per type"), "Musgrave's offset"),
    key("sharpness", Number, num(2.0), "ridged: how strongly a ridge suppresses detail").non_negative(),
    key("warp", Number, num(0.0), "domain warping, in first-octave wavelengths"),
];

static PATTERN: &[Key] = &[
    key("low", ColorTexture, num(0.0), "value where the noise is 0").alias(&["color1"]),
    key("high", ColorTexture, num(1.0), "value where the noise is 1").alias(&["color2"]),
    key("space", Ty::Choice(SPACES), name("world"), "coordinates the pattern is evaluated in"),
    key("scale", Number, num(4.0), "frequency of the first octave, per scene unit").positive(),
];

pub static NOISE: Schema = Schema {
    name: "noise",
    keys: &[
        &[
            key("type", Ty::Choice(FRACTALS), Derived("per use"), "kind of fractal"),
            key("scale", Number, Derived("per use"), "frequency of the first octave, per scene unit").positive(),
        ],
        FRACTAL,
    ],
    variants: &[],
    types: NO_TYPES,
};

static HYPERTEXTURE: &[Key] = &[
    key("shape", Ty::Table(&SDF), Derived("unit sphere"), "the soft object: an SDF node"),
    key("softness", Number, num(0.3), "width of the density ramp, in scene units").positive(),
    key("noise", Ty::Table(&NOISE), Derived("turbulence, scale 3, 6 octaves"), "noise fractal"),
    key("amount", Number, num(0.6), "noise strength"),
    key("bias", Number, num(0.5), "Perlin's bias").range(0.0, 1.0).open(true, true),
    key("gain", Number, num(0.5), "Perlin's gain").range(0.0, 1.0).open(true, true),
];

pub static TEXTURE: Schema = Schema {
    name: "texture",
    keys: &[&[
        key("type", Ty::String, Derived("image if file is given"), "kind of texture"),
        key("multiplier", Color, NONE, "multiplies the texture").alias(&["multiply"]),
    ]],
    variants: &[
        variant(
            "image",
            &["file"],
            &[&[
                key("file", Path, Required, "image file"),
                key("wrap", Ty::Choice(WRAPS), name("repeat"), "outside [0, 1]"),
                key("scale", Vec2, num(1.0), "UV scale (triplanar: repeats per scene unit)"),
                key("offset", Vec2, num(0.0), "UV offset"),
                key("filter", Ty::Choice(TEXTURE_FILTERS), name("bilinear"), "lookup filter"),
                key("colorspace", Ty::Choice(COLORSPACES), Derived("sRGB for colors, else linear"), "pixel encoding"),
                key("channel", Ty::Choice(CHANNELS), Derived("gray, or luminance of RGB"), "channel of scalar uses"),
                key("projection", Ty::Choice(UV_PROJECTIONS), name("uv"), "how the image is mapped"),
                key("sharpness", Number, num(4.0), "triplanar blend sharpness").non_negative(),
            ]],
        ),
        variant(
            "checker",
            &["checkerboard"],
            &[&[
                key("even", ColorTexture, num(0.8), "first color").alias(&["color1"]),
                key("odd", ColorTexture, num(0.2), "second color").alias(&["color2"]),
                key("scale", Number, num(8.0), "squares per UV unit or per scene unit"),
                key("space", Ty::Choice(SPACES), name("uv"), "coordinates the pattern is evaluated in"),
            ]],
        ),
        variant(
            "noise",
            &["fbm"],
            &[PATTERN, FRACTAL, &[key("turbulence", Bool, flag(false), "turbulence instead of fBm")]],
        ),
        variant("turbulence", &[], &[PATTERN, FRACTAL]),
        variant("ridged", &[], &[PATTERN, FRACTAL]),
        variant("hybrid", &[], &[PATTERN, FRACTAL]),
        variant("terrain", &["hetero_terrain"], &[PATTERN, FRACTAL]),
        variant("marble", &[], &[PATTERN, FRACTAL, &[key("amount", Number, num(4.0), "vein distortion")]]),
        variant("wood", &[], &[PATTERN, FRACTAL, &[key("amount", Number, num(0.5), "ring wobble")]]),
        variant("hypertexture", &[], &[HYPERTEXTURE]),
    ],
    types: TypeRule { default: None, with_file: Some("image"), optional_with: None },
};

static SDF_CHILDREN: &[Key] = &[
    key("children", Tables(&SDF), Required, "child nodes"),
    key("blend", Number, num(0.0), "width of the smooth blend between children").non_negative(),
];

pub static SDF: Schema = Schema {
    name: "SDF node",
    keys: &[&[
        key("type", Ty::String, Required, "kind of node"),
        key("translate", Vec3, num(0.0), "translation"),
        key("rotate", Vec3, num(0.0), "rotation in degrees about X, then Y, then Z"),
        key("scale", Vec3, num(1.0), "scale"),
    ]],
    variants: &[
        variant("sphere", &[], &[&[key("radius", Number, num(1.0), "radius").positive()]]),
        variant(
            "box",
            &["cube"],
            &[&[
                key("size", Vec3, num(1.0), "edge lengths").positive(),
                key("round", Number, num(0.0), "edge radius").non_negative(),
            ]],
        ),
        variant(
            "torus",
            &[],
            &[&[
                key("major", Number, num(1.0), "ring radius").positive(),
                key("minor", Number, num(0.25), "tube radius").positive(),
            ]],
        ),
        variant(
            "cylinder",
            &[],
            &[&[
                key("radius", Number, num(0.5), "radius").positive(),
                key("height", Number, num(1.0), "height, centred, along y").positive(),
            ]],
        ),
        variant(
            "capsule",
            &[],
            &[&[
                key("a", Vec3, vec3(0.0, -0.5, 0.0), "first end point"),
                key("b", Vec3, vec3(0.0, 0.5, 0.0), "second end point"),
                key("radius", Number, num(0.25), "radius").positive(),
            ]],
        ),
        variant("union", &[], &[SDF_CHILDREN]),
        variant("intersection", &[], &[SDF_CHILDREN]),
        variant("difference", &["subtract"], &[SDF_CHILDREN]),
        variant(
            "round",
            &[],
            &[&[
                key("child", Ty::Table(&SDF), Required, "the node to inflate"),
                key("radius", Number, num(0.05), "inflation").non_negative(),
            ]],
        ),
        variant(
            "shell",
            &["onion"],
            &[&[
                key("child", Ty::Table(&SDF), Required, "the node to hollow out"),
                key("thickness", Number, num(0.05), "thickness of the skin").positive(),
            ]],
        ),
        variant(
            "displace",
            &[],
            &[&[
                key("child", Ty::Table(&SDF), Required, "the node to displace"),
                key("noise", Ty::Table(&NOISE), Derived("fbm, scale 4"), "displacement noise"),
                key("amount", Number, num(0.1), "displacement: amount × (noise - 1/2)"),
            ]],
        ),
    ],
    types: TypeRule { default: None, with_file: None, optional_with: None },
};

pub static SCATTER: Schema = Schema {
    name: "scatter",
    keys: &[&[
        key("count", Integer, Required, "number of copies").range(1.0, 50_000_000.0),
        key("min", Vec3, num(0.0), "translation offsets are uniform in [min, max]"),
        key("max", Vec3, num(0.0), "translation offsets are uniform in [min, max]"),
        key("rotate", Vec3, num(0.0), "each axis rotates by a uniform angle in [0, value] degrees"),
        key("scale", Pair, num(1.0), "a uniform factor in [lo, hi]").positive(),
        key("seed", Integer, num(0.0), "random seed"),
    ]],
    variants: &[],
    types: NO_TYPES,
};

static TRANSFORM: &[Key] = &[
    key("translate", Vec3, num(0.0), "translation"),
    key("rotate", Vec3, num(0.0), "rotation in degrees about X, then Y, then Z"),
    key("scale", Vec3, num(1.0), "scale (a number, or per axis; not zero)"),
];

pub static OBJECT_MOTION: Schema = Schema { name: "motion", keys: &[TRANSFORM], variants: &[], types: NO_TYPES };

static IMPLICIT: &[Key] = &[
    key("epsilon", Number, Derived("2e-5 of the object's size; 2e-4 for fractals and noisy fields"), "hit tolerance")
        .positive(),
    key("lipschitz", Number, Derived("1, or estimated for displacements"), "bound of the field's slope").positive(),
    key("max_steps", Integer, Derived("512 (fractals 1024)"), "marching steps per ray"),
];

static OBJECT_COMMON: &[Key] = &[
    key("type", Ty::String, Derived("mesh if file is given"), "kind of object"),
    key("name", Ty::String, NONE, "label shown in the editor"),
    key("material", Material, Derived("default"), "a material name or an inline material"),
];

static OBJECT_VARIANTS: &[Variant] = &[
    variant(
        "sphere",
        &[],
        &[&[key("center", Vec3, num(0.0), "center"), key("radius", Number, num(1.0), "radius").positive()]],
    ),
    variant(
        "disk",
        &["disc"],
        &[&[
            key("center", Vec3, num(0.0), "center"),
            key("radius", Number, num(1.0), "radius").positive(),
            key("inner_radius", Number, num(0.0), "an annulus if above 0").non_negative(),
            key("normal", Vec3, vec3(0.0, 1.0, 0.0), "facing direction"),
        ]],
    ),
    variant(
        "cylinder",
        &["tube"],
        &[&[
            key("base", Vec3, num(0.0), "center of the bottom"),
            key("axis", Vec3, vec3(0.0, 1.0, 0.0), "direction of the axis"),
            key("radius", Number, num(1.0), "radius").positive(),
            key("height", Number, num(1.0), "height").positive(),
            key("capped", Bool, flag(true), "closed by two disks"),
        ]],
    ),
    variant(
        "box",
        &["cube"],
        &[&[
            key("min", Vec3, NONE, "minimum corner (with max)"),
            key("max", Vec3, NONE, "maximum corner (with min)"),
            key("center", Vec3, num(0.0), "center (with size)"),
            key("size", Vec3, num(1.0), "edge lengths (with center)").positive(),
        ]],
    ),
    variant(
        "quad",
        &[],
        &[&[
            key("origin", Vec3, Required, "first corner"),
            key("u", Vec3, Required, "first edge"),
            key("v", Vec3, Required, "second edge; the normal is u × v"),
        ]],
    ),
    variant(
        "triangle",
        &[],
        &[&[
            key("p0", Vec3, Required, "first corner"),
            key("p1", Vec3, Required, "second corner"),
            key("p2", Vec3, Required, "third corner"),
        ]],
    ),
    variant(
        "mesh",
        &["gltf", "stl"],
        &[&[
            key("file", Path, Required, ".obj, .ply, .stl, .gltf or .glb file"),
            key("smooth", Bool, flag(true), "use the file's vertex normals"),
        ]],
    ),
    variant(
        "instance",
        &[],
        &[&[
            key("prototype", Ref(RefKind::Prototype), Required, "name of the prototype to place"),
            key("scatter", Ty::Table(&SCATTER), NONE, "place many randomized copies"),
        ]],
    ),
    variant("sdf", &["implicit"], &[&[key("sdf", Ty::Table(&SDF), Required, "the SDF tree")], IMPLICIT]),
    variant(
        "hypertexture",
        &[],
        &[
            HYPERTEXTURE,
            &[key("level", Number, num(0.5), "density of the surface").range(0.0, 1.0).open(true, true)],
            IMPLICIT,
        ],
    ),
    variant(
        "julia",
        &["quaternion_julia"],
        &[
            &[
                key("c", Vec4, Derived("[-1, 0.2, 0, 0]"), "quaternion constant [real, i, j, k]"),
                key("iterations", Integer, num(12.0), "iterations").range(1.0, 256.0),
                key("slice_normal", Vec4, Derived("[0, 0, 0, 1]"), "normal of the 3D slice of 4D space"),
                key("slice", Number, num(0.0), "offset of the 3D slice"),
            ],
            IMPLICIT,
        ],
    ),
    variant(
        "mandelbulb",
        &[],
        &[
            &[
                key("power", Number, num(8.0), "power of the iteration").range(2.0, 32.0),
                key("iterations", Integer, num(10.0), "iterations").range(1.0, 256.0),
            ],
            IMPLICIT,
        ],
    ),
    variant("menger", &[], &[&[key("iterations", Integer, num(4.0), "iterations").range(0.0, 10.0)], IMPLICIT]),
];

pub static OBJECT: Schema = Schema {
    name: "[[objects]]",
    keys: &[
        OBJECT_COMMON,
        TRANSFORM,
        &[key("motion", Ty::Table(&OBJECT_MOTION), NONE, "the transform at shutter close")],
    ],
    variants: OBJECT_VARIANTS,
    types: TypeRule { default: None, with_file: Some("mesh"), optional_with: None },
};

pub static PROTOTYPE: Schema = Schema {
    name: "[prototypes.NAME]",
    keys: &[OBJECT_COMMON, TRANSFORM, &[key("objects", Tables(&OBJECT), NONE, "the parts, instead of one object")]],
    variants: OBJECT_VARIANTS,
    types: TypeRule { default: None, with_file: Some("mesh"), optional_with: Some("objects") },
};

// ------------------------------------------------------------------ validation

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Problem {
    /// A key the format does not know: probably a typo, ignored by the loader.
    UnknownKey,
    /// A value the loader will reject or misread.
    Invalid,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Diagnostic {
    /// Where, in the loader's notation: `[[objects]] #3 material`.
    pub path: String,
    pub message: String,
    pub problem: Problem,
}

impl std::fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.path, self.message)
    }
}

/// Checks a scene document (with its includes merged) against the schema.
pub fn validate(doc: &Table) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    check_table(&SCENE, doc, "", &mut out);
    out
}

/// Problems of one key's value, nested tables included (for showing next to a field).
pub fn check_key(key: &Key, v: &Value) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    check_value(key, v, key.name, &mut out);
    out
}

/// Problems of a table of `schema` itself: its type, unknown keys and missing keys (not
/// the values; see `check_key`).
pub fn check_entries(schema: &Schema, t: &Table) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    check_table(schema, t, "", &mut out);
    // Value problems of the table's own keys read "'key' must ..."; nested ones have paths.
    out.retain(|d| d.path == "scene" && !d.message.starts_with('\''));
    out
}

fn push(out: &mut Vec<Diagnostic>, path: &str, problem: Problem, message: String) {
    let path = if path.is_empty() { "scene".to_owned() } else { path.to_owned() };
    out.push(Diagnostic { path, message, problem });
}

fn check_table(schema: &Schema, t: &Table, ctx: &str, out: &mut Vec<Diagnostic>) {
    let mut variant = None;
    let mut know_keys = true;
    if !schema.variants.is_empty() {
        match t.get("type") {
            Some(Value::String(s)) => {
                variant = schema.variant(s);
                if variant.is_none() {
                    let names: Vec<&str> = schema.variants.iter().map(|v| v.name).collect();
                    push(out, ctx, Problem::Invalid, format!("unknown type '{s}' ({})", names.join(", ")));
                    know_keys = false;
                }
            }
            Some(v) => {
                push(out, ctx, Problem::Invalid, format!("'type' must be a string, got {}", v.type_name()));
                know_keys = false;
            }
            None => {
                variant = schema.variant_of(t);
                let optional = schema.types.optional_with.is_some_and(|k| t.contains_key(k));
                if variant.is_none() && !optional {
                    push(out, ctx, Problem::Invalid, "missing 'type'".into());
                    know_keys = false;
                }
            }
        }
    }
    for (k, v) in t {
        if k.starts_with('$') {
            continue; // bookkeeping added by the loader (`$dir`)
        }
        match schema.key(variant, k) {
            Some(key) => check_value(key, v, &join(ctx, k), out),
            None if know_keys => {
                let mut msg = format!("unknown key '{k}' (ignored)");
                if let Some(s) = suggest(
                    k,
                    schema.keys(variant).flat_map(|key| std::iter::once(key.name).chain(key.aliases.iter().copied())),
                ) {
                    msg = format!("unknown key '{k}' (ignored; did you mean '{s}'?)");
                }
                push(out, ctx, Problem::UnknownKey, msg);
            }
            None => {}
        }
    }
    if !know_keys {
        return;
    }
    for key in schema.keys(variant) {
        let given = std::iter::once(key.name).chain(key.aliases.iter().copied()).any(|n| t.contains_key(n));
        if key.default == Required && key.name != "type" && !given {
            push(out, ctx, Problem::Invalid, format!("missing '{}'", key.name));
        }
    }
}

fn join(ctx: &str, key: &str) -> String {
    if ctx.is_empty() { key.to_owned() } else { format!("{ctx} {key}") }
}

/// The context of an entry, in the loader's notation.
fn entry_ctx(ctx: &str, key: &str, sep: &str) -> String {
    if ctx.is_empty() { format!("[{key}{sep}") } else { format!("{ctx} {key}{sep}") }
}

fn check_value(key: &Key, v: &Value, ctx: &str, out: &mut Vec<Diagnostic>) {
    let invalid = |out: &mut Vec<Diagnostic>, msg: String| {
        let (path, name) = ctx.rsplit_once(' ').unwrap_or(("", ctx));
        push(out, path, Problem::Invalid, format!("'{name}' {msg}"));
    };
    let numbers = |v: &Value, n: Option<usize>| -> Option<Vec<f64>> {
        if let Some(x) = v.as_f64()
            && n != Some(4)
        {
            return Some(vec![x]);
        }
        let a = v.as_array()?;
        if n.is_some_and(|n| a.len() != n) {
            return None;
        }
        a.iter().map(Value::as_f64).collect()
    };
    let in_range = |out: &mut Vec<Diagnostic>, xs: &[f64]| {
        if let Some(r) = key.range
            && xs.iter().any(|x| !r.contains(*x))
        {
            invalid(out, format!("must be in {r}"));
        }
    };
    let (path_ctx, name) = ctx.rsplit_once(' ').map_or(("", ctx), |(p, n)| (p, n));
    match key.ty {
        Number => match v.as_f64() {
            Some(x) => in_range(out, &[x]),
            None => invalid(out, format!("must be a number, got {}", v.type_name())),
        },
        Integer => match v.as_f64() {
            Some(x) if x >= 0.0 && x.fract() == 0.0 => in_range(out, &[x]),
            _ => invalid(out, "must be a non-negative integer".into()),
        },
        Bool => {
            if v.as_bool().is_none() {
                invalid(out, "must be true or false".into());
            }
        }
        Ty::String | Path | Ref(_) => {
            if v.as_str().is_none() {
                invalid(out, format!("must be a string, got {}", v.type_name()));
            }
        }
        Vec2 | Pair => match numbers(v, Some(2)) {
            Some(xs) => in_range(out, &xs),
            None => invalid(out, "must be [a, b] or a number".into()),
        },
        Vec3 | Color => match numbers(v, Some(3)) {
            Some(xs) => in_range(out, &xs),
            None => invalid(out, "must be [x, y, z] or a number".into()),
        },
        Vec4 => {
            if numbers(v, Some(4)).is_none() {
                invalid(out, "must be [x, y, z, w]".into());
            }
        }
        Ty::Choice(choices) => match v.as_str() {
            Some(s) if choices.iter().any(|c| c.matches(s)) => {}
            Some(s) => invalid(out, format!("is '{s}', not one of {}", names(choices))),
            None => invalid(out, format!("must be one of {}", names(choices))),
        },
        Choices(choices) => {
            let items: Vec<&Value> = match v {
                Value::Array(a) => a.iter().collect(),
                v => vec![v],
            };
            for item in items {
                match item.as_str() {
                    Some(s) if choices.iter().any(|c| c.matches(s)) => {}
                    _ => invalid(out, format!("must be one or more of {}", names(choices))),
                }
            }
        }
        Strings => {
            let ok = match v {
                Value::String(_) => true,
                Value::Array(a) => a.iter().all(|x| x.as_str().is_some()),
                _ => false,
            };
            if !ok {
                invalid(out, "must be a string or an array of strings".into());
            }
        }
        ColorTexture | ScalarTexture => match v {
            Value::Table(t) => check_table(&TEXTURE, t, ctx, out),
            Value::String(_) => {}
            v => {
                if numbers(v, Some(3)).is_none() {
                    invalid(out, "must be a number, a color, an image path or a texture table".into());
                }
            }
        },
        Material => match v {
            Value::String(_) => {}
            Value::Table(t) => check_table(&MATERIAL, t, ctx, out),
            _ => invalid(out, "must be a material name or an inline table".into()),
        },
        Ty::Table(schema) => match v.as_table() {
            Some(t) if path_ctx.is_empty() => check_table(schema, t, &format!("[{name}]"), out),
            Some(t) => check_table(schema, t, ctx, out),
            None => invalid(out, format!("must be a table, got {}", v.type_name())),
        },
        Tables(schema) => match v.as_array() {
            Some(items) => {
                for (i, item) in items.iter().enumerate() {
                    let c = if path_ctx.is_empty() {
                        format!("[[{name}]] #{}", i + 1)
                    } else {
                        format!("{path_ctx} {name} #{}", i + 1)
                    };
                    match item.as_table() {
                        Some(t) => check_table(schema, t, &c, out),
                        None => push(out, &c, Problem::Invalid, "must be a table".into()),
                    }
                }
            }
            None => invalid(out, "must be an array of tables".into()),
        },
        Named(schema) => match v.as_table() {
            Some(entries) => {
                for (n, item) in entries {
                    let c = entry_ctx(path_ctx, &format!("{name}.{n}"), "]");
                    match item.as_table() {
                        Some(t) => check_table(schema, t, &c, out),
                        None => push(out, &c, Problem::Invalid, "must be a table".into()),
                    }
                }
            }
            None => invalid(out, "must be a table of named tables".into()),
        },
    }
}

fn names(choices: &[Choice]) -> String {
    choices.iter().map(|c| c.name).collect::<Vec<_>>().join(", ")
}

/// The closest of `candidates` to `word`, if it is a plausible typo.
fn suggest<'a>(word: &str, candidates: impl Iterator<Item = &'a str>) -> Option<&'a str> {
    let (best, d) = candidates.map(|c| (c, edit_distance(word, c))).min_by_key(|&(_, d)| d)?;
    (d <= 2 && d < word.len()).then_some(best)
}

fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut diag = row[0];
        row[0] = i + 1;
        for (j, &cb) in b.iter().enumerate() {
            let next = (diag + usize::from(ca != cb)).min(row[j] + 1).min(row[j + 1] + 1);
            diag = row[j + 1];
            row[j + 1] = next;
        }
    }
    row[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn diagnostics(src: &str) -> Vec<String> {
        validate(&crate::toml::parse(src).unwrap()).iter().map(ToString::to_string).collect()
    }

    #[test]
    fn example_scenes_are_clean() {
        let dir = crate::repo_root().join("scenes");
        for e in std::fs::read_dir(&dir).unwrap() {
            let p = e.unwrap().path();
            if p.extension().is_some_and(|x| x == "toml") {
                let doc = crate::loader::load_document(&p).unwrap();
                let d = validate(&doc);
                assert!(d.is_empty(), "{}: {:#?}", p.display(), d);
            }
        }
    }

    #[test]
    fn reports_problems() {
        let d = diagnostics(
            "[render]\nsppp = 4\ntonemap = \"agx\"\n[[objects]]\ntype = \"sphere\"\nradius = -1\n\
             material = { type = \"metal\", roughnes = 0.2 }\n[[objects]]\ntype = \"blob\"\nfoo = 1\n\
             [materials.m]\ntype = \"glass\"\nior = \"x\"\n[[objects]]\ntype = \"quad\"\n",
        );
        let expect = [
            "[render]: unknown key 'sppp' (ignored; did you mean 'spp'?)",
            "[render]: 'tonemap' is 'agx', not one of aces, reinhard, none",
            "[[objects]] #1 material: unknown key 'roughnes' (ignored; did you mean 'roughness'?)",
            "[[objects]] #1: 'radius' must be in (0, ∞)",
            "[[objects]] #2: unknown type 'blob' (sphere, disk, cylinder, box, quad, triangle, mesh, instance, sdf, \
             hypertexture, julia, mandelbulb, menger)",
            "[[objects]] #3: missing 'origin'",
            "[materials.m]: 'ior' must be a number, got string",
        ];
        for e in expect {
            assert!(d.iter().any(|x| x == e), "missing {e:?} in {d:#?}");
        }
        assert!(!d.iter().any(|x| x.contains("foo")), "keys of an unknown type are not reported");
        assert_eq!(diagnostics("[prototypes.p]\n[[prototypes.p.objects]]\ntype = \"sphere\"\n"), Vec::<String>::new());
    }

    /// Every name the schema offers is accepted by the loader's parser for it.
    #[test]
    fn choices_match_the_parsers() {
        fn all(cs: &[Choice]) -> impl Iterator<Item = &'static str> + '_ {
            cs.iter().flat_map(|c| std::iter::once(c.name).chain(c.aliases.iter().copied()))
        }
        use crate::{camera, filter, image, lights, noise, output, sampler, texture};
        assert!(all(TONEMAPS).all(|s| image::Tonemap::parse(s).is_some()));
        assert!(all(FILTERS).all(|s| filter::FilterKind::parse(s).is_some()));
        assert!(all(SAMPLERS).all(|s| sampler::SamplerKind::parse(s).is_some()));
        assert!(all(LIGHT_SAMPLERS).all(|s| lights::LightSamplerKind::parse(s).is_some()));
        assert!(all(PROJECTIONS).all(|s| camera::Projection::parse(s).is_some()));
        assert!(all(FORMATS).all(|s| output::Format::parse(s).is_some()));
        assert!(all(AOVS).all(|s| output::Aov::parse(s).is_some()));
        assert!(all(WRAPS).all(|s| texture::Wrap::parse(s).is_some()));
        assert!(all(CHANNELS).all(|s| texture::Channel::parse(s).is_some()));
        assert!(all(FRACTALS).all(|s| noise::FractalKind::parse(s).is_some()));
        assert!(all(METAL_PRESETS).all(|s| crate::material::metal_preset(s).is_some()));
    }

    #[test]
    fn render_defaults_match_the_renderer() {
        let d = crate::scene::RenderSettings::default();
        let get = |k: &str| match RENDER.key(None, k).unwrap().default {
            DefaultValue::Number(n) => n,
            other => panic!("{k}: {other:?}"),
        };
        let pairs = [
            ("width", f64::from(d.width)),
            ("height", f64::from(d.height)),
            ("spp", f64::from(d.spp)),
            ("max_depth", f64::from(d.max_depth)),
            ("max_volume_depth", f64::from(d.max_volume_depth)),
            ("threads", d.threads as f64),
            ("initial_block", f64::from(d.initial_block)),
            ("exposure", f64::from(d.exposure)),
            ("clamp", f64::from(d.clamp)),
            ("noise_threshold", f64::from(d.noise_threshold)),
            ("min_spp", f64::from(d.min_spp)),
            ("time_limit", f64::from(d.time_limit)),
            ("guiding_spp", f64::from(d.guiding_spp)),
        ];
        for (k, v) in pairs {
            assert_eq!(get(k), v, "[render] {k}");
        }
    }

    /// Every name of the schema appears in docs/scene-format.md, in the section about it, and
    /// every key and type named in the first column of a table there is in the schema.
    #[test]
    fn docs_match_the_schema() {
        // Git may check the doc out with CRLF endings (core.autocrlf on Windows runners).
        let text =
            std::fs::read_to_string(crate::repo_root().join("docs/scene-format.md")).unwrap().replace("\r\n", "\n");
        let section = |heading: &str| -> String {
            let start = text.find(&format!("\n## {heading}\n")).unwrap_or_else(|| panic!("no section '{heading}'"));
            let body = &text[start + 1..];
            let end = body[3..].find("\n## ").map_or(body.len(), |i| i + 3);
            body[..end].to_owned()
        };
        let words = |s: &str| -> BTreeSet<String> {
            s.split('`')
                .skip(1)
                .step_by(2)
                .flat_map(|code| code.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-')))
                .filter(|w| !w.is_empty())
                .map(str::to_owned)
                .collect()
        };
        let mut sections: Vec<(&str, Vec<&Schema>)> = vec![
            ("include", vec![]),
            ("[render]", vec![&RENDER]),
            ("[camera]", vec![&CAMERA, &CAMERA_MOTION]),
            ("[output]", vec![&OUTPUT]),
            ("[background]", vec![&BACKGROUND]),
            ("[[lights]]", vec![&LIGHT]),
            ("[media.NAME]", vec![&MEDIUM]),
            ("[materials.NAME]", vec![&MATERIAL]),
            ("Textures", vec![&TEXTURE, &NOISE]),
            ("[[objects]]", vec![&OBJECT, &PROTOTYPE, &OBJECT_MOTION, &SCATTER, &SDF]),
        ];
        let mut failures = Vec::new();
        for (heading, schemas) in &mut sections {
            let body = section(heading);
            let documented = words(&body);
            let mut known = BTreeSet::new();
            let mut add = |n: &'static str| {
                known.insert(n.to_owned());
            };
            if *heading == "include" {
                add("include");
            }
            for s in schemas.iter() {
                for k in s.keys(None).chain(s.variants.iter().flat_map(|v| s.keys(Some(v)))) {
                    add(k.name);
                    for a in k.aliases {
                        add(a);
                    }
                    let choices = match k.ty {
                        Ty::Choice(c) | Choices(c) => c,
                        _ => &[],
                    };
                    for c in choices {
                        add(c.name);
                    }
                }
                for v in s.variants {
                    add(v.name);
                    for a in v.aliases {
                        add(a);
                    }
                }
            }
            for n in &known {
                if !documented.contains(n) {
                    failures.push(format!("{heading}: '{n}' is not documented"));
                }
            }
            // First cells of table rows (not header rows).
            let lines: Vec<&str> = body.lines().collect();
            for (i, l) in lines.iter().enumerate() {
                let is_header = lines.get(i + 1).is_some_and(|n| n.starts_with("|---"));
                if !l.starts_with('|') || l.starts_with("|---") || is_header {
                    continue;
                }
                let first = l.split('|').nth(1).unwrap_or("");
                for w in words(first) {
                    if !known.contains(&w) {
                        failures.push(format!("{heading}: '{w}' is documented but not in the schema"));
                    }
                }
            }
        }
        assert!(failures.is_empty(), "docs/scene-format.md and the schema disagree:\n{}", failures.join("\n"));
    }
}
