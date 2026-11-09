//! Builds a `Scene` from a TOML scene description. See `scenes/*.toml` for examples.

use crate::camera::CameraDesc;
use crate::environment::{EnvSource, Environment};
use crate::filter::FilterKind;
use crate::geometry::{NO_NORMALS, NO_UVS, Sphere, Triangle};
use crate::image::Tonemap;
use crate::imageio;
use crate::lights::{Light, LightSamplerKind};
use crate::material::{METAL_PRESETS, Material, MaterialKind, MetalFresnel, Principled, metal_preset};
use crate::math::{PI, Transform, Vec3};
use crate::mesh;
use crate::mtl::{self, MtlMap, MtlMaterial};
use crate::output::{Aov, Format, OutputSettings};
use crate::scene::{Geometry, RenderSettings, Scene};
use crate::sky::{SUN_RADIUS, Sky};
use crate::spectrum::blackbody_rgb;
use crate::texture::{Channel, ImageTex, TexImage, Texture, Wrap};
use crate::toml::{self, Table, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

pub fn load_scene(path: &Path) -> Result<Scene, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("cannot read '{}': {e}", path.display()))?;
    let doc = toml::parse(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    let base_dir = path.parent().unwrap_or(Path::new("."));

    let empty = Table::new();
    let section = |name: &str| section(&doc, &empty, name);

    let settings = parse_render(section("render")?)?;
    let camera = parse_camera(section("camera")?)?;
    let output = parse_output(section("output")?)?;

    let mut b = Builder::default();
    let mut lights = b.parse_background(section("background")?, base_dir)?;
    if let Some(ls) = doc.get("lights") {
        let ls = ls.as_array().ok_or("'lights' must be an array of tables ([[lights]])")?;
        for (i, l) in ls.iter().enumerate() {
            let ctx = format!("[[lights]] #{}", i + 1);
            let t = l.as_table().ok_or_else(|| format!("{ctx} must be a table"))?;
            lights.push(parse_light(t, &ctx)?);
        }
    }
    for (name, v) in section("materials")? {
        let ctx = format!("[materials.{name}]");
        let t = v.as_table().ok_or_else(|| format!("{ctx} must be a table"))?;
        let m = b.parse_material(t, &ctx, base_dir)?;
        b.names.insert(name.clone(), b.materials.len() as u32);
        b.materials.push(m);
    }

    if let Some(objs) = doc.get("objects") {
        let objs = objs.as_array().ok_or("'objects' must be an array of tables ([[objects]])")?;
        for (i, o) in objs.iter().enumerate() {
            let ctx = format!("[[objects]] #{}", i + 1);
            let t = o.as_table().ok_or_else(|| format!("{ctx} must be a table"))?;
            b.add_object(t, &ctx, base_dir)?;
        }
    }

    if !settings.spectral && b.materials.iter().any(Material::is_dispersive) {
        eprintln!("warning: dispersion ('abbe') needs spectral rendering ([render] spectral = true or --spectral)");
    }
    if b.triangles.is_empty() && b.spheres.is_empty() {
        eprintln!("warning: scene contains no objects");
    }
    let mut files = vec![path.to_path_buf()];
    files.append(&mut b.files);
    let geo =
        Geometry { positions: b.positions, normals: b.normals, uvs: b.uvs, triangles: b.triangles, spheres: b.spheres };
    let mut scene = Scene::new(settings, camera, lights, b.materials, geo);
    scene.files = files;
    scene.output = output;
    Ok(scene)
}

// ------------------------------------------------------------------ value helpers

fn section<'a>(doc: &'a Table, empty: &'a Table, name: &str) -> Result<&'a Table, String> {
    match doc.get(name) {
        None => Ok(empty),
        Some(v) => v.as_table().ok_or_else(|| format!("'{name}' must be a table")),
    }
}

fn num(t: &Table, key: &str, ctx: &str) -> Result<Option<f64>, String> {
    match t.get(key) {
        None => Ok(None),
        Some(v) => {
            v.as_f64().map(Some).ok_or_else(|| format!("{ctx}: '{key}' must be a number, got {}", v.type_name()))
        }
    }
}

fn f32_or(t: &Table, key: &str, default: f32, ctx: &str) -> Result<f32, String> {
    Ok(num(t, key, ctx)?.map_or(default, |v| v as f32))
}

fn u32_or(t: &Table, key: &str, default: u32, ctx: &str) -> Result<u32, String> {
    match num(t, key, ctx)? {
        None => Ok(default),
        Some(v) if v >= 0.0 && v.fract() == 0.0 && v <= f64::from(u32::MAX) => Ok(v as u32),
        Some(v) => Err(format!("{ctx}: '{key}' must be a non-negative integer, got {v}")),
    }
}

fn bool_or(t: &Table, key: &str, default: bool, ctx: &str) -> Result<bool, String> {
    match t.get(key) {
        None => Ok(default),
        Some(v) => v.as_bool().ok_or_else(|| format!("{ctx}: '{key}' must be true or false")),
    }
}

fn str_opt<'a>(t: &'a Table, key: &str, ctx: &str) -> Result<Option<&'a str>, String> {
    match t.get(key) {
        None => Ok(None),
        Some(v) => v.as_str().map(Some).ok_or_else(|| format!("{ctx}: '{key}' must be a string")),
    }
}

/// Accepts `[x, y, z]` or a single number (broadcast to all components).
fn vec3_value(v: &Value, key: &str, ctx: &str) -> Result<Vec3, String> {
    if let Some(n) = v.as_f64() {
        return Ok(Vec3::splat(n as f32));
    }
    match v.as_array() {
        Some([x, y, z]) => match (x.as_f64(), y.as_f64(), z.as_f64()) {
            (Some(x), Some(y), Some(z)) => Ok(Vec3::new(x as f32, y as f32, z as f32)),
            _ => Err(format!("{ctx}: '{key}' must contain numbers")),
        },
        _ => Err(format!("{ctx}: '{key}' must be [x, y, z] or a number")),
    }
}

fn vec3_or(t: &Table, key: &str, default: Vec3, ctx: &str) -> Result<Vec3, String> {
    t.get(key).map_or(Ok(default), |v| vec3_value(v, key, ctx))
}

fn vec3_req(t: &Table, key: &str, ctx: &str) -> Result<Vec3, String> {
    let v = t.get(key).ok_or_else(|| format!("{ctx}: missing '{key}'"))?;
    vec3_value(v, key, ctx)
}

// ------------------------------------------------------------------ sections

fn parse_render(t: &Table) -> Result<RenderSettings, String> {
    let ctx = "[render]";
    let d = RenderSettings::default();
    let tonemap = match str_opt(t, "tonemap", ctx)? {
        None => d.tonemap,
        Some(s) => Tonemap::parse(s).ok_or_else(|| format!("{ctx}: unknown tonemap '{s}' (none, reinhard, aces)"))?,
    };
    let filter = match str_opt(t, "filter", ctx)? {
        None => d.filter,
        Some(s) => FilterKind::parse(s)
            .ok_or_else(|| format!("{ctx}: unknown filter '{s}' (box, tent, gaussian, blackman-harris)"))?,
    };
    let filter_radius = f32_or(t, "filter_radius", filter.default_radius(), ctx)?;
    if !(filter_radius > 0.0 && filter_radius <= 8.0) {
        return Err(format!("{ctx}: 'filter_radius' must be in (0, 8] pixels"));
    }
    let noise_threshold = f32_or(t, "noise_threshold", d.noise_threshold, ctx)?;
    let time_limit = f32_or(t, "time_limit", d.time_limit, ctx)?;
    if noise_threshold < 0.0 || time_limit < 0.0 {
        return Err(format!("{ctx}: 'noise_threshold' and 'time_limit' must not be negative"));
    }
    let s = RenderSettings {
        filter,
        filter_radius,
        noise_threshold,
        time_limit,
        min_spp: u32_or(t, "min_spp", d.min_spp, ctx)?,
        width: u32_or(t, "width", d.width, ctx)?,
        height: u32_or(t, "height", d.height, ctx)?,
        spp: u32_or(t, "spp", d.spp, ctx)?.max(1),
        max_depth: u32_or(t, "max_depth", d.max_depth, ctx)?,
        threads: u32_or(t, "threads", d.threads as u32, ctx)? as usize,
        initial_block: u32_or(t, "initial_block", d.initial_block, ctx)?.clamp(1, 256),
        exposure: f32_or(t, "exposure", d.exposure, ctx)?,
        tonemap,
        clamp: f32_or(t, "clamp", d.clamp, ctx)?,
        seed: u64::from(u32_or(t, "seed", 0, ctx)?),
        spectral: bool_or(t, "spectral", d.spectral, ctx)?,
        light_sampler: match str_opt(t, "light_sampler", ctx)? {
            None => d.light_sampler,
            Some(s) => LightSamplerKind::parse(s)
                .ok_or_else(|| format!("{ctx}: unknown light_sampler '{s}' (bvh, power, uniform)"))?,
        },
    };
    if s.width == 0 || s.height == 0 {
        return Err(format!("{ctx}: width and height must be positive"));
    }
    Ok(s)
}

/// A string or an array of strings, each parsed by `parse`.
fn str_list<T>(
    t: &Table,
    key: &str,
    ctx: &str,
    parse: fn(&str) -> Option<T>,
    allowed: &str,
) -> Result<Option<Vec<T>>, String> {
    let items: Vec<&Value> = match t.get(key) {
        None => return Ok(None),
        Some(v @ Value::String(_)) => vec![v],
        Some(v) => v
            .as_array()
            .ok_or_else(|| format!("{ctx}: '{key}' must be a string or an array of strings"))?
            .iter()
            .collect(),
    };
    items
        .into_iter()
        .map(|v| {
            let s = v.as_str().ok_or_else(|| format!("{ctx}: '{key}' must contain strings"))?;
            parse(s).ok_or_else(|| format!("{ctx}: unknown {key} entry '{s}' ({allowed})"))
        })
        .collect::<Result<Vec<T>, String>>()
        .map(Some)
}

fn parse_output(t: &Table) -> Result<OutputSettings, String> {
    let ctx = "[output]";
    let d = OutputSettings::default();
    let formats = str_list(t, "formats", ctx, Format::parse, "png, hdr, pfm")?.unwrap_or(d.formats);
    if formats.is_empty() {
        return Err(format!("{ctx}: 'formats' must not be empty"));
    }
    Ok(OutputSettings {
        path: str_opt(t, "path", ctx)?.map(str::to_owned),
        formats,
        save_on_finish: bool_or(t, "save_on_finish", d.save_on_finish, ctx)?,
        denoise: bool_or(t, "denoise", d.denoise, ctx)?,
        aovs: str_list(t, "aovs", ctx, Aov::parse, "albedo, normal, depth")?.unwrap_or(d.aovs),
    })
}

fn parse_camera(t: &Table) -> Result<CameraDesc, String> {
    let ctx = "[camera]";
    let d = CameraDesc::default();
    Ok(CameraDesc {
        position: vec3_or(t, "position", d.position, ctx)?,
        look_at: vec3_or(t, "look_at", d.look_at, ctx)?,
        up: vec3_or(t, "up", d.up, ctx)?,
        fov: f32_or(t, "fov", d.fov, ctx)?,
        aperture: f32_or(t, "aperture", d.aperture, ctx)?,
        focus_distance: num(t, "focus_distance", ctx)?.map(|v| v as f32),
    })
}

/// `color` (default white) times the black-body colour of `temperature` (Kelvin), if given.
fn light_color(t: &Table, ctx: &str) -> Result<Vec3, String> {
    Ok(vec3_or(t, "color", Vec3::ONE, ctx)? * temperature(t, ctx)?.unwrap_or(Vec3::ONE))
}

/// Black-body colour (luminance 1) of `temperature` in Kelvin.
fn temperature(t: &Table, ctx: &str) -> Result<Option<Vec3>, String> {
    match num(t, "temperature", ctx)? {
        None => Ok(None),
        Some(k) if (500.0..=50_000.0).contains(&k) => Ok(Some(blackbody_rgb(k as f32))),
        Some(_) => Err(format!("{ctx}: 'temperature' must be in [500, 50000] Kelvin")),
    }
}

/// A unit direction from `key` (must not be zero).
fn direction(t: &Table, key: &str, ctx: &str) -> Result<Option<Vec3>, String> {
    match t.get(key) {
        None => Ok(None),
        Some(v) => {
            let d = vec3_value(v, key, ctx)?;
            if d.length_squared() > 0.0 && d.is_finite() {
                Ok(Some(d.normalize()))
            } else {
                Err(format!("{ctx}: '{key}' must not be zero"))
            }
        }
    }
}

/// Direction from elevation / azimuth in degrees: azimuth 0 looks down -Z, 90 down +X.
fn from_angles(elevation: f32, azimuth: f32) -> Vec3 {
    let (se, ce) = elevation.to_radians().sin_cos();
    let (sa, ca) = azimuth.to_radians().sin_cos();
    Vec3::new(sa * ce, se, -ca * ce)
}

/// `[[lights]]`: point, spot and directional lights.
fn parse_light(t: &Table, ctx: &str) -> Result<Light, String> {
    let ty = str_opt(t, "type", ctx)?.ok_or_else(|| format!("{ctx}: missing 'type'"))?;
    let color = light_color(t, ctx)?;
    let intensity = f32_or(t, "intensity", 1.0, ctx)?;
    // Point and spot lights also accept the emitted power of an isotropic source.
    let radiant_intensity = || -> Result<Vec3, String> {
        Ok(match num(t, "power", ctx)? {
            Some(p) => color * (p as f32 / (4.0 * PI)),
            None => color * intensity,
        })
    };
    match ty {
        "point" => Ok(Light::Point { pos: vec3_req(t, "position", ctx)?, intensity: radiant_intensity()? }),
        "spot" | "spotlight" => {
            let pos = vec3_req(t, "position", ctx)?;
            let dir = match (direction(t, "direction", ctx)?, t.get("look_at")) {
                (Some(d), _) => d,
                (None, Some(v)) => {
                    let d = vec3_value(v, "look_at", ctx)? - pos;
                    if d.length_squared() == 0.0 {
                        return Err(format!("{ctx}: 'look_at' equals 'position'"));
                    }
                    d.normalize()
                }
                (None, None) => return Err(format!("{ctx}: needs 'direction' or 'look_at'")),
            };
            let outer = f32_or(t, "angle", 30.0, ctx)?;
            let inner = f32_or(t, "inner_angle", outer * 0.8, ctx)?;
            if !(outer > 0.0 && outer <= 180.0) || inner < 0.0 || inner > outer {
                return Err(format!("{ctx}: need 0 <= inner_angle <= angle <= 180 (half angles in degrees)"));
            }
            Ok(Light::Spot {
                pos,
                dir,
                intensity: radiant_intensity()?,
                cos_inner: inner.to_radians().cos(),
                cos_outer: outer.to_radians().cos(),
            })
        }
        "directional" | "distant" | "sun" => {
            let travel = match direction(t, "direction", ctx)? {
                Some(d) => d,
                None => -from_angles(f32_or(t, "elevation", 45.0, ctx)?, f32_or(t, "azimuth", 0.0, ctx)?),
            };
            // `intensity` is the irradiance on a surface facing the light.
            let irradiance = color * intensity;
            let angle = f32_or(t, "angle", 0.0, ctx)?;
            if !(0.0..=90.0).contains(&angle) {
                return Err(format!("{ctx}: 'angle' (angular diameter, degrees) must be in [0, 90]"));
            }
            if angle == 0.0 {
                return Ok(Light::Distant { dir: -travel, radiance: irradiance, cos_max: 1.0 });
            }
            let r = (angle * 0.5).to_radians();
            Ok(Light::Distant { dir: -travel, radiance: irradiance * (1.0 / (PI * r.sin().powi(2))), cos_max: r.cos() })
        }
        other => Err(format!("{ctx}: unknown light type '{other}' (point, spot, directional)")),
    }
}

// ------------------------------------------------------------------ builder

/// How a texture is used: decides the default color space and scalar channel.
#[derive(Clone, Copy, PartialEq)]
enum TexKind {
    /// Reflectance / emission: 8- and 16-bit images are sRGB by default.
    Color,
    /// Roughness, metallic, bump heights ...: linear, gray or luminance.
    Scalar,
    /// Coverage: linear, the alpha channel if the image has one.
    Alpha,
    /// Tangent-space normals: linear RGB.
    Normal,
}

#[derive(Default)]
struct Builder {
    materials: Vec<Material>,
    names: HashMap<String, u32>,
    positions: Vec<Vec3>,
    normals: Vec<Vec3>,
    uvs: Vec<[f32; 2]>,
    triangles: Vec<Triangle>,
    spheres: Vec<Sphere>,
    files: Vec<PathBuf>,
    /// Decoded images by (path, sRGB).
    images: HashMap<(PathBuf, bool), Arc<TexImage>>,
    /// Parsed material libraries by path.
    mtl_libs: HashMap<PathBuf, Rc<Vec<MtlMaterial>>>,
    /// Scene material ids of MTL materials already converted, by (library, name).
    mtl_ids: HashMap<(PathBuf, String), u32>,
}

impl Builder {
    fn track(&mut self, p: &Path) {
        if !self.files.iter().any(|f| f == p) {
            self.files.push(p.to_path_buf());
        }
    }

    // -------------------------------------------------------------- environment

    /// `[background]`: the environment light, plus the sun for `type = "sky"`.
    fn parse_background(&mut self, t: &Table, dir: &Path) -> Result<Vec<Light>, String> {
        let ctx = "[background]";
        let strength = f32_or(t, "intensity", 1.0, ctx)?;
        let rotate = match t.get("rotate") {
            None => Vec3::ZERO,
            Some(v) if v.as_f64().is_some() => Vec3::new(0.0, v.as_f64().unwrap_or(0.0) as f32, 0.0),
            Some(v) => vec3_value(v, "rotate", ctx)?,
        };
        let mut lights = Vec::new();
        let source = match str_opt(t, "type", ctx)?.unwrap_or(if t.contains_key("file") { "image" } else { "color" }) {
            "color" | "constant" => EnvSource::Color(vec3_or(t, "color", Vec3::ZERO, ctx)? * strength),
            "gradient" => EnvSource::Gradient {
                zenith: vec3_or(t, "zenith", Vec3::new(0.25, 0.45, 0.9), ctx)? * strength,
                horizon: vec3_or(t, "horizon", Vec3::new(0.9, 0.95, 1.0), ctx)? * strength,
                ground: vec3_or(t, "ground", Vec3::new(0.3, 0.28, 0.25), ctx)? * strength,
            },
            "image" | "envmap" | "hdri" => {
                let file = str_opt(t, "file", ctx)?.ok_or_else(|| format!("{ctx}: missing 'file'"))?;
                let path = dir.join(file);
                let raw = imageio::load(&path).map_err(|e| format!("{ctx}: {e}"))?;
                self.track(&path);
                // Integer images are display-referred sRGB; float images are linear.
                let srgb = !matches!(raw.pixels, imageio::Pixels::F32(_));
                let img = Arc::new(TexImage::new(raw, srgb));
                EnvSource::Image { img, scale: vec3_or(t, "color", Vec3::ONE, ctx)? * strength }
            }
            "sky" | "daylight" | "preetham" => {
                let sun = match direction(t, "sun_direction", ctx)? {
                    Some(d) => d,
                    None => from_angles(f32_or(t, "sun_elevation", 45.0, ctx)?, f32_or(t, "sun_azimuth", 0.0, ctx)?),
                };
                let turbidity = f32_or(t, "turbidity", 3.0, ctx)?;
                if !(1.7..=10.0).contains(&turbidity) {
                    return Err(format!("{ctx}: 'turbidity' must be in [1.7, 10]"));
                }
                let ground = vec3_or(t, "ground", Vec3::splat(0.2), ctx)?;
                let sky = Sky::new(sun, turbidity, strength, ground);
                if bool_or(t, "sun", true, ctx)? {
                    let size = f32_or(t, "sun_size", 1.0, ctx)?;
                    if !(size > 0.0 && size <= 100.0) {
                        return Err(format!("{ctx}: 'sun_size' must be in (0, 100]"));
                    }
                    // A larger disk keeps the sun's irradiance (softer shadows, less noise).
                    let r = SUN_RADIUS * size;
                    let radiance = sky.sun_radiance() * ((1.0 - SUN_RADIUS.cos()) / (1.0 - r.cos()));
                    if !radiance.is_black() {
                        lights.push(Light::Distant { dir: sky.sun_direction(), radiance, cos_max: r.cos() });
                    }
                }
                EnvSource::Sky(Box::new(sky))
            }
            other => return Err(format!("{ctx}: unknown type '{other}' (color, gradient, image, sky)")),
        };
        let env = Environment::new(source, rotate);
        if !env.is_black() {
            lights.insert(0, Light::Environment(Box::new(env)));
        }
        Ok(lights)
    }

    // -------------------------------------------------------------- textures

    fn image(&mut self, path: &Path, srgb: bool) -> Result<Arc<TexImage>, String> {
        let key = (path.to_path_buf(), srgb);
        if let Some(img) = self.images.get(&key) {
            return Ok(img.clone());
        }
        self.track(path);
        let img = Arc::new(TexImage::new(imageio::load(path)?, srgb));
        self.images.insert(key, img.clone());
        Ok(img)
    }

    #[expect(clippy::too_many_arguments, reason = "mirrors the independent TOML/MTL texture options")]
    fn image_texture(
        &mut self,
        file: &Path,
        kind: TexKind,
        opts: &Table,
        scale: [f32; 2],
        offset: [f32; 2],
        wrap: Wrap,
        ctx: &str,
    ) -> Result<Texture, String> {
        let srgb = match str_opt(opts, "colorspace", ctx)? {
            None => kind == TexKind::Color,
            Some("srgb") => true,
            Some("linear" | "raw" | "non-color") => false,
            Some(s) => return Err(format!("{ctx}: unknown colorspace '{s}' (srgb, linear)")),
        };
        let img = self.image(file, srgb).map_err(|e| format!("{ctx}: {e}"))?;
        let nearest = match str_opt(opts, "filter", ctx)? {
            None | Some("bilinear" | "linear") => false,
            Some("nearest" | "point" | "closest") => true,
            Some(s) => return Err(format!("{ctx}: unknown texture filter '{s}' (bilinear, nearest)")),
        };
        let channel = match str_opt(opts, "channel", ctx)? {
            Some(s) => {
                Channel::parse(s).ok_or_else(|| format!("{ctx}: unknown channel '{s}' (r, g, b, a, luminance)"))?
            }
            None if kind == TexKind::Alpha && (img.channels == 2 || img.channels == 4) => Channel::A,
            None => Channel::Auto,
        };
        Ok(Texture::Image(Box::new(ImageTex { img, scale, offset, wrap, nearest, channel })))
    }

    /// A texturable parameter: a number, `[r, g, b]`, an image path, or a table
    /// (`{ file = ... }`, `{ type = "checker", ... }`, `{ type = "noise", ... }`).
    fn texture(&mut self, v: &Value, key: &str, kind: TexKind, ctx: &str, dir: &Path) -> Result<Texture, String> {
        let ctx = &format!("{ctx} '{key}'");
        match v {
            Value::String(s) => {
                self.image_texture(&dir.join(s), kind, &Table::new(), [1.0; 2], [0.0; 2], Wrap::Repeat, ctx)
            }
            Value::Table(t) => {
                let ty = str_opt(t, "type", ctx)?.unwrap_or(if t.contains_key("file") { "image" } else { "" });
                let tex = match ty {
                    "image" | "file" => {
                        let file = str_opt(t, "file", ctx)?.ok_or_else(|| format!("{ctx}: missing 'file'"))?;
                        let wrap = match str_opt(t, "wrap", ctx)? {
                            None => Wrap::Repeat,
                            Some(s) => Wrap::parse(s)
                                .ok_or_else(|| format!("{ctx}: unknown wrap mode '{s}' (repeat, clamp, mirror)"))?,
                        };
                        let scale = uv_pair(t, "scale", [1.0; 2], ctx)?;
                        let offset = uv_pair(t, "offset", [0.0; 2], ctx)?;
                        self.image_texture(&dir.join(file), kind, t, scale, offset, wrap, ctx)?
                    }
                    "checker" | "checkerboard" => Texture::Checker {
                        even: Box::new(self.tex_or(t, &["even", "color1"], Vec3::splat(0.8), kind, ctx, dir)?),
                        odd: Box::new(self.tex_or(t, &["odd", "color2"], Vec3::splat(0.2), kind, ctx, dir)?),
                        scale: f32_or(t, "scale", 8.0, ctx)?,
                        world: space(t, false, ctx)?,
                    },
                    "noise" | "fbm" | "turbulence" => Texture::Noise {
                        low: Box::new(self.tex_or(t, &["low", "color1"], Vec3::ZERO, kind, ctx, dir)?),
                        high: Box::new(self.tex_or(t, &["high", "color2"], Vec3::ONE, kind, ctx, dir)?),
                        scale: f32_or(t, "scale", 4.0, ctx)?,
                        octaves: u32_or(t, "octaves", 5, ctx)?.clamp(1, 16),
                        turbulence: ty == "turbulence" || bool_or(t, "turbulence", false, ctx)?,
                        world: space(t, true, ctx)?,
                    },
                    "" => return Err(format!("{ctx}: texture table needs 'file' or 'type'")),
                    other => {
                        return Err(format!(
                            "{ctx}: unknown texture type '{other}' (image, checker, noise, turbulence)"
                        ));
                    }
                };
                Ok(match t.get("multiplier").or_else(|| t.get("multiply")) {
                    Some(m) => Texture::Scaled(Box::new(tex), vec3_value(m, "multiplier", ctx)?),
                    None => tex,
                })
            }
            _ => Ok(Texture::Const(vec3_value(v, key, ctx)?)),
        }
    }

    /// First present key of `keys` as a texture, else a constant.
    fn tex_or(
        &mut self,
        t: &Table,
        keys: &[&str],
        default: Vec3,
        kind: TexKind,
        ctx: &str,
        dir: &Path,
    ) -> Result<Texture, String> {
        for k in keys {
            if let Some(v) = t.get(*k) {
                return self.texture(v, k, kind, ctx, dir);
            }
        }
        Ok(Texture::Const(default))
    }

    // -------------------------------------------------------------- materials

    fn parse_material(&mut self, t: &Table, ctx: &str, dir: &Path) -> Result<Material, String> {
        use TexKind::{Alpha, Color, Normal, Scalar};
        let ty = str_opt(t, "type", ctx)?.unwrap_or("diffuse");
        let splat = Vec3::splat;
        let kind = match ty {
            "diffuse" | "lambert" => {
                MaterialKind::Diffuse { albedo: self.tex_or(t, &["albedo", "color"], splat(0.8), Color, ctx, dir)? }
            }
            "conductor" | "metal" => {
                let fresnel = if let Some(p) = str_opt(t, "preset", ctx)? {
                    let (eta, k) = metal_preset(&p.to_ascii_lowercase())
                        .ok_or_else(|| format!("{ctx}: unknown metal preset '{p}' ({METAL_PRESETS})"))?;
                    MetalFresnel::complex(eta, k)
                } else if t.contains_key("eta") || t.contains_key("k") {
                    MetalFresnel::complex(vec3_req(t, "eta", ctx)?, vec3_req(t, "k", ctx)?)
                } else {
                    MetalFresnel::Schlick
                };
                let default_color = if matches!(fresnel, MetalFresnel::Schlick) { 0.9 } else { 1.0 };
                MaterialKind::Conductor {
                    color: self.tex_or(t, &["color", "albedo"], splat(default_color), Color, ctx, dir)?,
                    roughness: self.tex_or(t, &["roughness"], splat(0.0), Scalar, ctx, dir)?,
                    fresnel,
                    multiscatter: bool_or(t, "multiscatter", true, ctx)?,
                }
            }
            "dielectric" | "glass" => {
                if t.contains_key("tint") {
                    return Err(format!(
                        "{ctx}: 'tint' was replaced by volume absorption: 'color' is the transmittance \
                         after 'distance' scene units (default 1)"
                    ));
                }
                let ior = f32_or(t, "ior", 1.5, ctx)?;
                if ior <= 0.0 {
                    return Err(format!("{ctx}: 'ior' must be positive"));
                }
                let absorption = if let Some(v) = t.get("absorption") {
                    vec3_value(v, "absorption", ctx)?.max(Vec3::ZERO)
                } else {
                    let c = vec3_or(t, "color", Vec3::ONE, ctx)?;
                    let d = f32_or(t, "distance", 1.0, ctx)?;
                    if d <= 0.0 {
                        return Err(format!("{ctx}: 'distance' must be positive"));
                    }
                    let s = |c: f32| -c.clamp(1e-6, 1.0).ln() / d;
                    Vec3::new(s(c.x), s(c.y), s(c.z))
                };
                let roughness = self.tex_or(t, &["roughness"], splat(0.0), Scalar, ctx, dir)?;
                let abbe = f32_or(t, "abbe", 0.0, ctx)?;
                if abbe < 0.0 {
                    return Err(format!("{ctx}: 'abbe' must be positive (0 = no dispersion)"));
                }
                MaterialKind::Dielectric { ior, abbe, roughness, absorption }
            }
            "plastic" | "coated" => {
                let ior = f32_or(t, "ior", 1.5, ctx)?;
                if ior < 1.0 {
                    return Err(format!("{ctx}: 'ior' must be at least 1"));
                }
                MaterialKind::Plastic {
                    color: self.tex_or(t, &["color", "albedo"], splat(0.8), Color, ctx, dir)?,
                    roughness: self.tex_or(t, &["roughness"], splat(0.0), Scalar, ctx, dir)?,
                    ior,
                }
            }
            "principled" | "pbr" | "disney" | "uber" => {
                let ior = num(t, "ior", ctx)?.map(|v| v as f32);
                if ior.is_some_and(|i| i < 1.0) {
                    return Err(format!("{ctx}: 'ior' must be at least 1"));
                }
                let mut s = |keys: &[&str], d: f32| self.tex_or(t, keys, splat(d), Scalar, ctx, dir);
                let p = Principled {
                    metallic: s(&["metallic"], 0.0)?,
                    roughness: s(&["roughness"], 0.5)?,
                    specular: s(&["specular"], 0.5)?,
                    transmission: s(&["transmission"], 0.0)?,
                    clearcoat: s(&["clearcoat"], 0.0)?,
                    clearcoat_roughness: s(&["clearcoat_roughness"], 0.03)?,
                    sheen: s(&["sheen"], 0.0)?,
                    sheen_tint: s(&["sheen_tint"], 0.5)?,
                    ior,
                    base_color: self.tex_or(t, &["base_color", "color", "albedo"], splat(0.8), Color, ctx, dir)?,
                };
                MaterialKind::Principled(Box::new(p))
            }
            "emissive" | "light" | "emitter" => {
                let e = self.tex_or(t, &["emission", "color"], Vec3::ONE, Color, ctx, dir)?;
                // A black-body colour (luminance 1) multiplies any explicit colour.
                let k = Vec3::splat(f32_or(t, "intensity", 1.0, ctx)?) * temperature(t, ctx)?.unwrap_or(Vec3::ONE);
                let emission = match e.constant() {
                    Some(c) => Texture::Const(c * k),
                    None if k == Vec3::ONE => e,
                    None => Texture::Scaled(Box::new(e), k),
                };
                MaterialKind::Emissive { emission, two_sided: bool_or(t, "two_sided", false, ctx)? }
            }
            other => {
                return Err(format!(
                    "{ctx}: unknown material type '{other}' (diffuse, metal, glass, plastic, principled, light)"
                ));
            }
        };
        let mut m = Material::new(kind);
        if let Some(v) = t.get("normal_map") {
            let tex = self.texture(v, "normal_map", Normal, ctx, dir)?;
            m.normal_map = Some((tex, f32_or(t, "normal_strength", 1.0, ctx)?));
        }
        if let Some(v) = t.get("bump") {
            let tex = self.texture(v, "bump", Scalar, ctx, dir)?;
            m.bump = Some((tex, f32_or(t, "bump_scale", 0.01, ctx)?));
        }
        if let Some(v) = t.get("alpha") {
            m.alpha = Some(self.texture(v, "alpha", Alpha, ctx, dir)?);
        }
        Ok(m)
    }

    fn default_material(&mut self) -> u32 {
        if let Some(&id) = self.names.get("default") {
            return id;
        }
        let id = self.materials.len() as u32;
        self.materials.push(Material::diffuse(Vec3::splat(0.8)));
        self.names.insert("default".into(), id);
        id
    }

    /// `material = "name"` or an inline table `material = { type = "...", ... }`.
    fn material(&mut self, t: &Table, ctx: &str, dir: &Path) -> Result<Option<u32>, String> {
        match t.get("material") {
            None => Ok(None),
            Some(Value::String(name)) => {
                self.names.get(name).copied().map(Some).ok_or_else(|| format!("{ctx}: unknown material '{name}'"))
            }
            Some(Value::Table(mt)) => {
                let m = self.parse_material(mt, &format!("{ctx} material"), dir)?;
                self.materials.push(m);
                Ok(Some(self.materials.len() as u32 - 1))
            }
            Some(_) => Err(format!("{ctx}: 'material' must be a name or an inline table")),
        }
    }

    // -------------------------------------------------------------- MTL import

    fn mtl_lib(&mut self, path: &Path) -> Result<Rc<Vec<MtlMaterial>>, String> {
        if let Some(l) = self.mtl_libs.get(path) {
            return Ok(l.clone());
        }
        let lib = match std::fs::read_to_string(path) {
            Ok(text) => {
                self.track(path);
                mtl::parse(&text).map_err(|e| format!("{}: {e}", path.display()))?
            }
            Err(e) => {
                eprintln!("warning: cannot read material library '{}': {e}", path.display());
                Vec::new()
            }
        };
        let lib = Rc::new(lib);
        self.mtl_libs.insert(path.to_path_buf(), lib.clone());
        Ok(lib)
    }

    /// Finds material `name` in the mesh's material libraries and converts it.
    fn mtl_material_id(&mut self, mesh_dir: &Path, libs: &[String], name: &str) -> Result<Option<u32>, String> {
        for lib in libs {
            let path = mesh_dir.join(lib);
            let key = (path.clone(), name.to_string());
            if let Some(&id) = self.mtl_ids.get(&key) {
                return Ok(Some(id));
            }
            let mats = self.mtl_lib(&path)?;
            if let Some(m) = mats.iter().find(|m| m.name == name) {
                let dir = path.parent().unwrap_or(Path::new(".")).to_path_buf();
                let ctx = format!("{}: material '{name}'", path.display());
                let mat = self.convert_mtl(m, &dir, &ctx)?;
                let id = self.materials.len() as u32;
                self.materials.push(mat);
                self.mtl_ids.insert(key, id);
                return Ok(Some(id));
            }
        }
        Ok(None)
    }

    fn mtl_map(&mut self, m: &MtlMap, kind: TexKind, dir: &Path, ctx: &str) -> Result<Texture, String> {
        let wrap = if m.clamp { Wrap::Clamp } else { Wrap::Repeat };
        self.image_texture(&dir.join(&m.file), kind, &Table::new(), m.scale, m.offset, wrap, ctx)
    }

    /// Maps a Wavefront material onto ours: `Ke` -> light, `illum` 4/6/7/9 -> glass,
    /// `Pr`/`Pm` -> principled, `Ks` -> plastic (or metal if `Kd` is black), else diffuse.
    fn convert_mtl(&mut self, m: &MtlMaterial, dir: &Path, ctx: &str) -> Result<Material, String> {
        use TexKind::{Alpha, Color, Normal, Scalar};
        let opt =
            |b: &mut Self, map: &Option<MtlMap>, kind| map.as_ref().map(|mm| b.mtl_map(mm, kind, dir, ctx)).transpose();
        let color = match opt(self, &m.map_kd, Color)? {
            Some(t) => t,
            None => Texture::Const(m.kd.unwrap_or(Vec3::splat(0.8))),
        };
        // Blender's exporter writes Ns = (1 - roughness)^2 * 1000.
        let roughness = match (opt(self, &m.map_pr, Scalar)?, m.pr, m.ns) {
            (Some(t), _, _) => t,
            (None, Some(r), _) => Texture::Const(Vec3::splat(r)),
            (None, None, Some(ns)) => Texture::Const(Vec3::splat(1.0 - (ns / 1000.0).clamp(0.0, 1.0).sqrt())),
            _ => Texture::Const(Vec3::splat(0.5)),
        };
        let ke = m.ke.unwrap_or(Vec3::ZERO);
        let ks = m.ks.unwrap_or(Vec3::ZERO);
        let glass = matches!(m.illum, Some(4 | 6 | 7 | 9));
        let ior = m.ni.filter(|&n| n >= 1.0).unwrap_or(1.5);
        let kind = if !ke.is_black() || m.map_ke.is_some() {
            let emission = match (opt(self, &m.map_ke, Color)?, ke.is_black()) {
                (Some(t), true) => t,
                (Some(t), false) => Texture::Scaled(Box::new(t), ke),
                (None, _) => Texture::Const(ke),
            };
            MaterialKind::Emissive { emission, two_sided: false }
        } else if glass {
            MaterialKind::Dielectric {
                ior: m.ni.filter(|&n| n > 0.0).unwrap_or(1.5),
                abbe: 0.0,
                roughness: Texture::Const(Vec3::ZERO),
                absorption: Vec3::ZERO,
            }
        } else if m.pr.is_some() || m.pm.is_some() || m.map_pr.is_some() || m.map_pm.is_some() {
            let metallic = match opt(self, &m.map_pm, Scalar)? {
                Some(t) => t,
                None => Texture::Const(Vec3::splat(m.pm.unwrap_or(0.0))),
            };
            MaterialKind::Principled(Box::new(Principled {
                base_color: color,
                metallic,
                roughness,
                specular: Texture::Const(Vec3::splat(0.5)),
                ior: m.ni.filter(|&n| n > 1.0),
                transmission: Texture::Const(Vec3::ZERO),
                clearcoat: Texture::Const(Vec3::ZERO),
                clearcoat_roughness: Texture::Const(Vec3::splat(0.03)),
                sheen: Texture::Const(Vec3::ZERO),
                sheen_tint: Texture::Const(Vec3::splat(0.5)),
            }))
        } else if !ks.is_black() || m.map_ks.is_some() {
            let kd_black = m.map_kd.is_none() && m.kd.is_none_or(Vec3::is_black);
            if kd_black {
                let color = match opt(self, &m.map_ks, Color)? {
                    Some(t) => t,
                    None => Texture::Const(ks),
                };
                MaterialKind::Conductor { color, roughness, fresnel: MetalFresnel::Schlick, multiscatter: true }
            } else {
                MaterialKind::Plastic { color, roughness, ior }
            }
        } else {
            MaterialKind::Diffuse { albedo: color }
        };
        let mut mat = Material::new(kind);
        if let Some(t) = opt(self, &m.map_d, Alpha)? {
            mat.alpha = Some(t);
        } else if let Some(d) = m.d.filter(|&d| d < 1.0 && !glass) {
            mat.alpha = Some(Texture::Const(Vec3::splat(d.max(0.0))));
        }
        if let Some(n) = &m.map_norm {
            mat.normal_map = Some((self.mtl_map(n, Normal, dir, ctx)?, 1.0));
        } else if let Some(b) = &m.map_bump {
            // Normal maps are often exported as bump maps; guess from the file name.
            let f = b.file.to_ascii_lowercase();
            if ["normal", "nrm", "_n.", "_nor"].iter().any(|s| f.contains(s)) {
                mat.normal_map = Some((self.mtl_map(b, Normal, dir, ctx)?, b.bump_mult));
            } else {
                mat.bump = Some((self.mtl_map(b, Scalar, dir, ctx)?, 0.01 * b.bump_mult));
            }
        }
        Ok(mat)
    }

    // -------------------------------------------------------------- geometry

    fn add_object(&mut self, t: &Table, ctx: &str, base_dir: &Path) -> Result<(), String> {
        let ty = str_opt(t, "type", ctx)?.ok_or_else(|| format!("{ctx}: missing 'type'"))?;
        let mat = self.material(t, ctx, base_dir)?;
        match ty {
            "sphere" => {
                let radius = f32_or(t, "radius", 1.0, ctx)?;
                if radius <= 0.0 {
                    return Err(format!("{ctx}: 'radius' must be positive"));
                }
                let material = mat.unwrap_or_else(|| self.default_material());
                self.spheres.push(Sphere { center: vec3_or(t, "center", Vec3::ZERO, ctx)?, radius, material });
            }
            "quad" => {
                // Parallelogram origin, origin+u, origin+u+v, origin+v. Normal = u x v.
                // Texture coordinates run (0,0) at the origin to (1,1) at origin+u+v.
                let o = vec3_req(t, "origin", ctx)?;
                let u = vec3_req(t, "u", ctx)?;
                let v = vec3_req(t, "v", ctx)?;
                let material = mat.unwrap_or_else(|| self.default_material());
                let base = self.positions.len() as u32;
                let tb = self.uvs.len() as u32;
                self.positions.extend([o, o + u, o + u + v, o + v]);
                self.uvs.extend([[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]]);
                self.push_tri([base, base + 1, base + 2], None, Some([tb, tb + 1, tb + 2]), material);
                self.push_tri([base, base + 2, base + 3], None, Some([tb, tb + 2, tb + 3]), material);
            }
            "triangle" => {
                let ps = [vec3_req(t, "p0", ctx)?, vec3_req(t, "p1", ctx)?, vec3_req(t, "p2", ctx)?];
                let material = mat.unwrap_or_else(|| self.default_material());
                let base = self.positions.len() as u32;
                self.positions.extend(ps);
                self.push_tri([base, base + 1, base + 2], None, None, material);
            }
            "mesh" => {
                let file = str_opt(t, "file", ctx)?.ok_or_else(|| format!("{ctx}: missing 'file'"))?;
                let file = base_dir.join(file);
                self.track(&file);
                let data = mesh::load(&file)?;
                let xf = parse_transform(t, ctx)?;
                let mesh_dir = file.parent().unwrap_or(Path::new(".")).to_path_buf();
                self.add_mesh(&data, &xf, mat, &mesh_dir, t, ctx)?;
            }
            other => {
                return Err(format!("{ctx}: unknown object type '{other}' (sphere, quad, triangle, mesh)"));
            }
        }
        Ok(())
    }

    fn add_mesh(
        &mut self,
        m: &mesh::MeshData,
        xf: &Transform,
        mat: Option<u32>,
        mesh_dir: &Path,
        t: &Table,
        ctx: &str,
    ) -> Result<(), String> {
        let smooth = bool_or(t, "smooth", true, ctx)?;
        // Map each material group in the file to a scene material: the object's
        // `material` wins, then a scene material of the same name, then the mesh's
        // material library, then the default.
        let mut group_mat = Vec::with_capacity(m.groups.len());
        for g in &m.groups {
            let id = match (mat, self.names.get(g)) {
                (Some(id), _) | (None, Some(&id)) => id,
                (None, None) => {
                    if let Some(id) = self.mtl_material_id(mesh_dir, &m.mtllibs, g)? {
                        id
                    } else {
                        if !g.is_empty() {
                            eprintln!("warning: {ctx}: material '{g}' not defined, using default");
                        }
                        self.default_material()
                    }
                }
            };
            group_mat.push(id);
        }
        let vbase = self.positions.len() as u32;
        let nbase = self.normals.len() as u32;
        let tbase = self.uvs.len() as u32;
        self.positions.extend(m.positions.iter().map(|&p| xf.point(p)));
        if smooth {
            self.normals.extend(m.normals.iter().map(|&n| xf.normal(n)));
        }
        self.uvs.extend_from_slice(&m.uvs);
        let flip = xf.flips_handedness();
        for tri in &m.triangles {
            let mut v = tri.v.map(|i| i + vbase);
            let mut n = tri.n.filter(|_| smooth).map(|n| n.map(|i| i + nbase));
            let mut uv = tri.uv.map(|t| t.map(|i| i + tbase));
            if flip {
                v.swap(1, 2);
                if let Some(n) = n.as_mut() {
                    n.swap(1, 2);
                }
                if let Some(uv) = uv.as_mut() {
                    uv.swap(1, 2);
                }
            }
            self.push_tri(v, n, uv, group_mat[tri.group as usize]);
        }
        Ok(())
    }

    fn push_tri(&mut self, v: [u32; 3], n: Option<[u32; 3]>, uv: Option<[u32; 3]>, material: u32) {
        let (p0, p1, p2) =
            (self.positions[v[0] as usize], self.positions[v[1] as usize], self.positions[v[2] as usize]);
        if (p1 - p0).cross(p2 - p0).length_squared() == 0.0 {
            return; // degenerate
        }
        self.triangles.push(Triangle { v, n: n.unwrap_or([NO_NORMALS; 3]), uv: uv.unwrap_or([NO_UVS; 3]), material });
    }
}

/// `[u, v]` or a single number for both.
fn uv_pair(t: &Table, key: &str, default: [f32; 2], ctx: &str) -> Result<[f32; 2], String> {
    match t.get(key) {
        None => Ok(default),
        Some(v) => {
            if let Some(n) = v.as_f64() {
                return Ok([n as f32; 2]);
            }
            match v.as_array() {
                Some([a, b]) => match (a.as_f64(), b.as_f64()) {
                    (Some(a), Some(b)) => Ok([a as f32, b as f32]),
                    _ => Err(format!("{ctx}: '{key}' must contain numbers")),
                },
                _ => Err(format!("{ctx}: '{key}' must be [u, v] or a number")),
            }
        }
    }
}

/// `space = "uv"` or `"world"` for procedural textures.
fn space(t: &Table, default_world: bool, ctx: &str) -> Result<bool, String> {
    match str_opt(t, "space", ctx)? {
        None => Ok(default_world),
        Some("world" | "object") => Ok(true),
        Some("uv") => Ok(false),
        Some(s) => Err(format!("{ctx}: unknown space '{s}' (uv, world)")),
    }
}

fn parse_transform(t: &Table, ctx: &str) -> Result<Transform, String> {
    let translate = vec3_or(t, "translate", Vec3::ZERO, ctx)?;
    let rotate = vec3_or(t, "rotate", Vec3::ZERO, ctx)?;
    let scale = vec3_or(t, "scale", Vec3::ONE, ctx)?;
    Transform::new(translate, rotate, scale).map_err(|e| format!("{ctx}: {e}"))
}
