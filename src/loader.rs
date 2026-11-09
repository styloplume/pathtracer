//! Builds a `Scene` from a TOML scene description. See `scenes/*.toml` for examples.

use crate::camera::CameraDesc;
use crate::geometry::{NO_NORMALS, Sphere, Triangle};
use crate::image::Tonemap;
use crate::material::Material;
use crate::math::{Transform, Vec3};
use crate::mesh;
use crate::scene::{Background, RenderSettings, Scene};
use crate::toml::{self, Table, Value};
use std::collections::HashMap;
use std::path::Path;

pub fn load_scene(path: &Path) -> Result<Scene, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("cannot read '{}': {e}", path.display()))?;
    let doc = toml::parse(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    let base_dir = path.parent().unwrap_or(Path::new("."));

    let empty = Table::new();
    let section = |name: &str| section(&doc, &empty, name);

    let settings = parse_render(section("render")?)?;
    let camera = parse_camera(section("camera")?)?;
    let background = parse_background(section("background")?)?;

    let mut b = Builder::default();
    for (name, v) in section("materials")? {
        let ctx = format!("[materials.{name}]");
        let t = v.as_table().ok_or_else(|| format!("{ctx} must be a table"))?;
        let m = parse_material(t, &ctx)?;
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

    if b.triangles.is_empty() && b.spheres.is_empty() {
        eprintln!("warning: scene contains no objects");
    }
    Ok(Scene::new(settings, camera, background, b.materials, b.positions, b.normals, b.triangles, b.spheres))
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
        Some(v) => v.as_f64().map(Some).ok_or_else(|| format!("{ctx}: '{key}' must be a number, got {}", v.type_name())),
    }
}

fn f32_or(t: &Table, key: &str, default: f32, ctx: &str) -> Result<f32, String> {
    Ok(num(t, key, ctx)?.map(|v| v as f32).unwrap_or(default))
}

fn u32_or(t: &Table, key: &str, default: u32, ctx: &str) -> Result<u32, String> {
    match num(t, key, ctx)? {
        None => Ok(default),
        Some(v) if v >= 0.0 && v.fract() == 0.0 && v <= u32::MAX as f64 => Ok(v as u32),
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
    t.get(key).map(|v| vec3_value(v, key, ctx)).unwrap_or(Ok(default))
}

/// First of several alternative key names that is present.
fn vec3_any(t: &Table, keys: &[&str], default: Vec3, ctx: &str) -> Result<Vec3, String> {
    for k in keys {
        if let Some(v) = t.get(*k) {
            return vec3_value(v, k, ctx);
        }
    }
    Ok(default)
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
    let s = RenderSettings {
        width: u32_or(t, "width", d.width, ctx)?,
        height: u32_or(t, "height", d.height, ctx)?,
        spp: u32_or(t, "spp", d.spp, ctx)?.max(1),
        max_depth: u32_or(t, "max_depth", d.max_depth, ctx)?,
        threads: u32_or(t, "threads", d.threads as u32, ctx)? as usize,
        initial_block: u32_or(t, "initial_block", d.initial_block, ctx)?.clamp(1, 256),
        exposure: f32_or(t, "exposure", d.exposure, ctx)?,
        tonemap,
        clamp: f32_or(t, "clamp", d.clamp, ctx)?,
        seed: u32_or(t, "seed", 0, ctx)? as u64,
    };
    if s.width == 0 || s.height == 0 {
        return Err(format!("{ctx}: width and height must be positive"));
    }
    Ok(s)
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

fn parse_background(t: &Table) -> Result<Background, String> {
    let ctx = "[background]";
    let strength = f32_or(t, "intensity", 1.0, ctx)?;
    match str_opt(t, "type", ctx)?.unwrap_or("color") {
        "color" | "constant" => Ok(Background::Color(vec3_or(t, "color", Vec3::ZERO, ctx)? * strength)),
        "gradient" | "sky" => Ok(Background::Gradient {
            zenith: vec3_or(t, "zenith", Vec3::new(0.25, 0.45, 0.9), ctx)? * strength,
            horizon: vec3_or(t, "horizon", Vec3::new(0.9, 0.95, 1.0), ctx)? * strength,
            ground: vec3_or(t, "ground", Vec3::new(0.3, 0.28, 0.25), ctx)? * strength,
        }),
        other => Err(format!("{ctx}: unknown type '{other}' (color, gradient)")),
    }
}

fn parse_material(t: &Table, ctx: &str) -> Result<Material, String> {
    let ty = str_opt(t, "type", ctx)?.unwrap_or("diffuse");
    Ok(match ty {
        "diffuse" | "lambert" => Material::Diffuse { albedo: vec3_any(t, &["albedo", "color"], Vec3::splat(0.8), ctx)? },
        "conductor" | "metal" => Material::Conductor {
            color: vec3_any(t, &["color", "albedo"], Vec3::splat(0.9), ctx)?,
            roughness: f32_or(t, "roughness", 0.0, ctx)?.clamp(0.0, 1.0),
        },
        "dielectric" | "glass" => {
            let ior = f32_or(t, "ior", 1.5, ctx)?;
            if ior <= 0.0 {
                return Err(format!("{ctx}: 'ior' must be positive"));
            }
            Material::Dielectric { ior, tint: vec3_any(t, &["tint", "color"], Vec3::ONE, ctx)? }
        }
        "emissive" | "light" | "emitter" => Material::Emissive {
            radiance: vec3_any(t, &["emission", "color"], Vec3::ONE, ctx)? * f32_or(t, "intensity", 1.0, ctx)?,
            two_sided: bool_or(t, "two_sided", false, ctx)?,
        },
        other => return Err(format!("{ctx}: unknown material type '{other}' (diffuse, metal, glass, light)")),
    })
}

// ------------------------------------------------------------------ geometry

#[derive(Default)]
struct Builder {
    materials: Vec<Material>,
    names: HashMap<String, u32>,
    positions: Vec<Vec3>,
    normals: Vec<Vec3>,
    triangles: Vec<Triangle>,
    spheres: Vec<Sphere>,
}

impl Builder {
    fn default_material(&mut self) -> u32 {
        if let Some(&id) = self.names.get("default") {
            return id;
        }
        let id = self.materials.len() as u32;
        self.materials.push(Material::Diffuse { albedo: Vec3::splat(0.8) });
        self.names.insert("default".into(), id);
        id
    }

    /// `material = "name"` or an inline table `material = { type = "...", ... }`.
    fn material(&mut self, t: &Table, ctx: &str) -> Result<Option<u32>, String> {
        match t.get("material") {
            None => Ok(None),
            Some(Value::String(name)) => {
                self.names.get(name).copied().map(Some).ok_or_else(|| format!("{ctx}: unknown material '{name}'"))
            }
            Some(Value::Table(mt)) => {
                let m = parse_material(mt, &format!("{ctx} material"))?;
                self.materials.push(m);
                Ok(Some(self.materials.len() as u32 - 1))
            }
            Some(_) => Err(format!("{ctx}: 'material' must be a name or an inline table")),
        }
    }

    fn add_object(&mut self, t: &Table, ctx: &str, base_dir: &Path) -> Result<(), String> {
        let ty = str_opt(t, "type", ctx)?.ok_or_else(|| format!("{ctx}: missing 'type'"))?;
        let mat = self.material(t, ctx)?;
        match ty {
            "sphere" => {
                let radius = f32_or(t, "radius", 1.0, ctx)?;
                if radius <= 0.0 {
                    return Err(format!("{ctx}: 'radius' must be positive"));
                }
                let material = mat.map_or_else(|| self.default_material(), |m| m);
                self.spheres.push(Sphere { center: vec3_or(t, "center", Vec3::ZERO, ctx)?, radius, material });
            }
            "quad" => {
                // Parallelogram origin, origin+u, origin+u+v, origin+v. Normal = u x v.
                let o = vec3_req(t, "origin", ctx)?;
                let u = vec3_req(t, "u", ctx)?;
                let v = vec3_req(t, "v", ctx)?;
                let material = mat.map_or_else(|| self.default_material(), |m| m);
                let base = self.positions.len() as u32;
                self.positions.extend([o, o + u, o + u + v, o + v]);
                self.push_tri([base, base + 1, base + 2], None, material);
                self.push_tri([base, base + 2, base + 3], None, material);
            }
            "triangle" => {
                let ps = [vec3_req(t, "p0", ctx)?, vec3_req(t, "p1", ctx)?, vec3_req(t, "p2", ctx)?];
                let material = mat.map_or_else(|| self.default_material(), |m| m);
                let base = self.positions.len() as u32;
                self.positions.extend(ps);
                self.push_tri([base, base + 1, base + 2], None, material);
            }
            "mesh" => {
                let file = str_opt(t, "file", ctx)?.ok_or_else(|| format!("{ctx}: missing 'file'"))?;
                let data = mesh::load(&base_dir.join(file))?;
                let xf = parse_transform(t, ctx)?;
                self.add_mesh(&data, &xf, mat, t, ctx)?;
            }
            other => return Err(format!("{ctx}: unknown object type '{other}' (sphere, quad, triangle, mesh)")),
        }
        Ok(())
    }

    fn add_mesh(&mut self, m: &mesh::MeshData, xf: &Transform, mat: Option<u32>, t: &Table, ctx: &str) -> Result<(), String> {
        let smooth = bool_or(t, "smooth", true, ctx)?;
        // Map each material group in the file to a scene material.
        let mut group_mat = Vec::with_capacity(m.groups.len());
        for g in &m.groups {
            let id = match (mat, self.names.get(g)) {
                (Some(id), _) => id,
                (None, Some(&id)) => id,
                (None, None) => {
                    if !g.is_empty() {
                        eprintln!("warning: {ctx}: material '{g}' not defined in scene, using default");
                    }
                    self.default_material()
                }
            };
            group_mat.push(id);
        }
        let vbase = self.positions.len() as u32;
        let nbase = self.normals.len() as u32;
        self.positions.extend(m.positions.iter().map(|&p| xf.point(p)));
        if smooth {
            self.normals.extend(m.normals.iter().map(|&n| xf.normal(n)));
        }
        let flip = xf.flips_handedness();
        for tri in &m.triangles {
            let mut v = tri.v.map(|i| i + vbase);
            let mut n = tri.n.filter(|_| smooth).map(|n| n.map(|i| i + nbase));
            if flip {
                v.swap(1, 2);
                if let Some(n) = n.as_mut() {
                    n.swap(1, 2);
                }
            }
            self.push_tri(v, n, group_mat[tri.group as usize]);
        }
        Ok(())
    }

    fn push_tri(&mut self, v: [u32; 3], n: Option<[u32; 3]>, material: u32) {
        let (p0, p1, p2) = (self.positions[v[0] as usize], self.positions[v[1] as usize], self.positions[v[2] as usize]);
        if (p1 - p0).cross(p2 - p0).length_squared() == 0.0 {
            return; // degenerate
        }
        self.triangles.push(Triangle { v, n: n.unwrap_or([NO_NORMALS; 3]), material });
    }
}

fn parse_transform(t: &Table, ctx: &str) -> Result<Transform, String> {
    let translate = vec3_or(t, "translate", Vec3::ZERO, ctx)?;
    let rotate = vec3_or(t, "rotate", Vec3::ZERO, ctx)?;
    let scale = vec3_or(t, "scale", Vec3::ONE, ctx)?;
    Transform::new(translate, rotate, scale).map_err(|e| format!("{ctx}: {e}"))
}
