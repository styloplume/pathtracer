//! `[[objects]]` and `[prototypes]`: geometry, object transforms, motion blur and
//! instancing, and the import of mesh files (OBJ, PLY, STL, glTF).
//!
//! Every object takes `translate`, `rotate` (degrees, X then Y then Z) and `scale`,
//! applied to its geometry as written, and `motion = { translate, rotate, scale }`, the
//! transform at the end of the frame. Static geometry is transformed into world space
//! once; moving objects, prototypes and shapes a transform cannot be baked into become
//! instances of their own geometry group.

use super::include::dir_of;
use super::procedural;
use super::{Builder, TexKind, bool_or, direction, f32_or, str_opt, u32_or, vec3_or, vec3_req};
use crate::geometry::{NO_NORMALS, NO_UVS, Triangle};
use crate::gltf::{self, AlphaMode, Gltf, GltfImage, GltfMaterial};
use crate::material::{Material, MaterialKind, Principled};
use crate::math::{Frame, Transform, Trs, Vec3};
use crate::mesh;
use crate::rng::Rng;
use crate::scene::{InstanceDesc, InstanceXf};
use crate::shapes::{Shape, ShapeKind};
use crate::texture::{Channel, ImageTex, Texture};
use crate::toml::{Table, Value};
use std::path::Path;
use std::sync::Arc;

const OBJECT_TYPES: &str =
    "sphere, disk, cylinder, box, quad, triangle, mesh, instance, sdf, hypertexture, julia, mandelbulb, menger";

fn parse_trs(t: &Table, ctx: &str) -> Result<Trs, String> {
    let trs = Trs {
        translate: vec3_or(t, "translate", Vec3::ZERO, ctx)?,
        rotate: vec3_or(t, "rotate", Vec3::ZERO, ctx)?,
        scale: vec3_or(t, "scale", Vec3::ONE, ctx)?,
    };
    trs.transform().map_err(|e| format!("{ctx}: {e}"))?;
    Ok(trs)
}

/// The end-of-frame transform from `motion = { ... }`; unset fields keep their start value.
fn parse_motion(t: &Table, start: &Trs, ctx: &str) -> Result<Option<Trs>, String> {
    let Some(v) = t.get("motion") else { return Ok(None) };
    let ctx = &format!("{ctx} motion");
    let m = v.as_table().ok_or_else(|| format!("{ctx}: must be a table {{ translate, rotate, scale }}"))?;
    let end = Trs {
        translate: vec3_or(m, "translate", start.translate, ctx)?,
        rotate: vec3_or(m, "rotate", start.rotate, ctx)?,
        scale: vec3_or(m, "scale", start.scale, ctx)?,
    };
    end.transform().map_err(|e| format!("{ctx}: {e}"))?;
    Ok((end != *start).then_some(end))
}

/// `scatter = { count, min, max, rotate, scale, seed }`: `count` random placements
/// around `base`: translations offset uniformly within [`min`, `max`], rotations by up to
/// `rotate` degrees per axis, scales multiplied by a factor in `scale` = [lo, hi].
fn scatter(v: &Value, base: &Trs, ctx: &str) -> Result<Vec<Trs>, String> {
    let s = v.as_table().ok_or_else(|| format!("{ctx}: must be a table {{ count, min, max, rotate, scale, seed }}"))?;
    let count = u32_or(s, "count", 0, ctx)?;
    if count == 0 || count > 50_000_000 {
        return Err(format!("{ctx}: 'count' must be in [1, 50000000]"));
    }
    let (lo, hi) = (vec3_or(s, "min", Vec3::ZERO, ctx)?, vec3_or(s, "max", Vec3::ZERO, ctx)?);
    let rot = vec3_or(s, "rotate", Vec3::ZERO, ctx)?;
    let (s_lo, s_hi) = match s.get("scale") {
        None => (1.0, 1.0),
        Some(v) => match (v.as_f64(), v.as_array()) {
            (Some(k), _) => (k as f32, k as f32),
            (None, Some([a, b])) if a.as_f64().is_some() && b.as_f64().is_some() => {
                (a.as_f64().unwrap_or(1.0) as f32, b.as_f64().unwrap_or(1.0) as f32)
            }
            _ => return Err(format!("{ctx}: 'scale' must be [lo, hi] or a number")),
        },
    };
    if !(s_lo > 0.0 && s_hi >= s_lo) {
        return Err(format!("{ctx}: 'scale' needs 0 < lo <= hi"));
    }
    let mut rng = Rng::new(u64::from(u32_or(s, "seed", 0, ctx)?), 0x5ca7);
    let mut r3 = || Vec3::new(rng.f32(), rng.f32(), rng.f32());
    Ok((0..count)
        .map(|_| {
            let (t, r) = (r3(), r3());
            let k = s_lo + (s_hi - s_lo) * r3().x;
            Trs { translate: base.translate + lo + (hi - lo) * t, rotate: base.rotate + rot * r, scale: base.scale * k }
        })
        .collect())
}

fn static_xf(trs: &Trs) -> InstanceXf {
    if *trs == Trs::IDENTITY { InstanceXf::Identity } else { InstanceXf::Static(trs.transform_unchecked()) }
}

fn object_type<'a>(t: &'a Table, ctx: &str) -> Result<&'a str, String> {
    match str_opt(t, "type", ctx)? {
        Some(ty) => Ok(ty),
        None if t.contains_key("file") => Ok("mesh"),
        None => Err(format!("{ctx}: missing 'type'")),
    }
}

impl Builder {
    fn new_group(&mut self) -> u32 {
        self.n_groups += 1;
        self.n_groups - 1
    }

    /// Runs `f` with new geometry going into `group`.
    fn with_group<T>(&mut self, group: u32, f: impl FnOnce(&mut Self) -> Result<T, String>) -> Result<T, String> {
        let prev = std::mem::replace(&mut self.group, group);
        let r = f(self);
        self.group = prev;
        r
    }

    /// `[prototypes.NAME]`: geometry that is only rendered where `type = "instance"`
    /// objects place it. Either a list `[[prototypes.NAME.objects]]` or a single object
    /// described by the prototype table itself.
    pub(super) fn parse_prototypes(&mut self, protos: &Table, base_dir: &Path) -> Result<(), String> {
        for (name, v) in protos {
            let ctx = format!("[prototypes.{name}]");
            let pt = v.as_table().ok_or_else(|| format!("{ctx} must be a table"))?;
            let dir = dir_of(pt, base_dir);
            let group = self.new_group();
            self.with_group(group, |b| match pt.get("objects") {
                Some(objs) => {
                    let objs = objs.as_array().ok_or_else(|| format!("{ctx}: 'objects' must be an array of tables"))?;
                    for (i, o) in objs.iter().enumerate() {
                        let ctx = format!("{ctx} object #{}", i + 1);
                        let ot = o.as_table().ok_or_else(|| format!("{ctx} must be a table"))?;
                        b.add_prototype_part(ot, &ctx, &dir_of(ot, &dir))?;
                    }
                    Ok(())
                }
                None => b.add_prototype_part(pt, &ctx, &dir),
            })?;
            self.prototypes.insert(name.clone(), group);
        }
        Ok(())
    }

    fn add_prototype_part(&mut self, t: &Table, ctx: &str, dir: &Path) -> Result<(), String> {
        let ty = object_type(t, ctx)?;
        if ty == "instance" || t.contains_key("motion") {
            return Err(format!("{ctx}: prototypes cannot contain instances or moving objects"));
        }
        let xf = parse_trs(t, ctx)?.transform_unchecked();
        self.add_geometry(t, ty, ctx, dir, &xf)
    }

    /// One `[[objects]]` entry.
    pub(super) fn add_object(&mut self, t: &Table, ctx: &str, base_dir: &Path) -> Result<(), String> {
        let dir = dir_of(t, base_dir);
        let ty = object_type(t, ctx)?;
        let trs = parse_trs(t, ctx)?;
        let motion = parse_motion(t, &trs, ctx)?;
        if ty == "instance" {
            let name = str_opt(t, "prototype", ctx)?.ok_or_else(|| format!("{ctx}: missing 'prototype'"))?;
            if t.contains_key("material") {
                return Err(format!("{ctx}: instances use their prototype's materials"));
            }
            let group = *self.prototypes.get(name).ok_or_else(|| {
                let mut known: Vec<&str> = self.prototypes.keys().map(String::as_str).collect();
                known.sort_unstable();
                format!("{ctx}: unknown prototype '{name}' (defined: {})", known.join(", "))
            })?;
            if let Some(s) = t.get("scatter") {
                if motion.is_some() {
                    return Err(format!("{ctx}: 'scatter' and 'motion' cannot be combined"));
                }
                for trs in scatter(s, &trs, &format!("{ctx} scatter"))? {
                    self.instances.push(InstanceDesc { group, xf: static_xf(&trs) });
                }
                return Ok(());
            }
            let xf = motion.map_or_else(|| static_xf(&trs), |end| InstanceXf::Motion(trs, end));
            self.instances.push(InstanceDesc { group, xf });
            return Ok(());
        }
        if let Some(end) = motion {
            let group = self.new_group();
            self.with_group(group, |b| b.add_geometry(t, ty, ctx, &dir, &Transform::IDENTITY))?;
            self.instances.push(InstanceDesc { group, xf: InstanceXf::Motion(trs, end) });
            return Ok(());
        }
        self.add_geometry(t, ty, ctx, &dir, &trs.transform_unchecked())
    }

    /// Adds the geometry of object `t` transformed by `xf` to the current group.
    fn add_geometry(&mut self, t: &Table, ty: &str, ctx: &str, dir: &Path, xf: &Transform) -> Result<(), String> {
        let mat = self.material(t, ctx, dir)?;
        let up = Vec3::new(0.0, 1.0, 0.0);
        let positive = |key: &str, default: f32| -> Result<f32, String> {
            let v = f32_or(t, key, default, ctx)?;
            if v > 0.0 && v.is_finite() { Ok(v) } else { Err(format!("{ctx}: '{key}' must be positive")) }
        };
        match ty {
            "sphere" => {
                let kind = ShapeKind::Sphere {
                    center: vec3_or(t, "center", Vec3::ZERO, ctx)?,
                    radius: positive("radius", 1.0)?,
                };
                let material = mat.unwrap_or_else(|| self.default_material());
                self.add_shape(kind, material, xf, ctx)?;
            }
            "disk" | "disc" => {
                let radius = positive("radius", 1.0)?;
                let inner = f32_or(t, "inner_radius", 0.0, ctx)?;
                if !(0.0..radius).contains(&inner) {
                    return Err(format!("{ctx}: need 0 <= inner_radius < radius"));
                }
                let n = direction(t, "normal", ctx)?.unwrap_or(up);
                let center = vec3_or(t, "center", Vec3::ZERO, ctx)?;
                let kind = ShapeKind::Disk { center, frame: Frame::from_normal(n), radius, inner };
                let material = mat.unwrap_or_else(|| self.default_material());
                self.add_shape(kind, material, xf, ctx)?;
            }
            "cylinder" | "tube" => {
                let (radius, height) = (positive("radius", 1.0)?, positive("height", 1.0)?);
                let axis = direction(t, "axis", ctx)?.unwrap_or(up);
                let base = vec3_or(t, "base", Vec3::ZERO, ctx)?;
                let material = mat.unwrap_or_else(|| self.default_material());
                let frame = Frame::from_normal(axis);
                self.add_shape(ShapeKind::Cylinder { base, frame, radius, height }, material, xf, ctx)?;
                if bool_or(t, "capped", true, ctx)? {
                    for (center, n) in [(base, -axis), (base + axis * height, axis)] {
                        let disk = ShapeKind::Disk { center, frame: Frame::from_normal(n), radius, inner: 0.0 };
                        self.add_shape(disk, material, xf, ctx)?;
                    }
                }
            }
            "box" | "cube" => {
                let (center, size) = match (t.get("min"), t.get("max")) {
                    (Some(_), Some(_)) => {
                        let (lo, hi) = (vec3_req(t, "min", ctx)?, vec3_req(t, "max", ctx)?);
                        ((lo + hi) * 0.5, hi - lo)
                    }
                    _ => (vec3_or(t, "center", Vec3::ZERO, ctx)?, vec3_or(t, "size", Vec3::ONE, ctx)?),
                };
                if !(size.x > 0.0 && size.y > 0.0 && size.z > 0.0) {
                    return Err(format!("{ctx}: box size must be positive (max > min)"));
                }
                let frame = Frame { t: Vec3::new(1.0, 0.0, 0.0), b: up, n: Vec3::Z };
                let material = mat.unwrap_or_else(|| self.default_material());
                self.add_shape(ShapeKind::Box { center, frame, half: size * 0.5 }, material, xf, ctx)?;
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
                self.positions.extend([o, o + u, o + u + v, o + v].map(|p| xf.point(p)));
                self.uvs.extend([[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]]);
                // A mirroring transform flips the winding, keeping the normal on the mirrored side.
                let w = |a: u32, b: u32, c: u32| if xf.flips_handedness() { [a, c, b] } else { [a, b, c] };
                self.push_tri(w(base, base + 1, base + 2), None, Some(w(tb, tb + 1, tb + 2)), material);
                self.push_tri(w(base, base + 2, base + 3), None, Some(w(tb, tb + 2, tb + 3)), material);
            }
            "triangle" => {
                let ps = [vec3_req(t, "p0", ctx)?, vec3_req(t, "p1", ctx)?, vec3_req(t, "p2", ctx)?];
                let material = mat.unwrap_or_else(|| self.default_material());
                let base = self.positions.len() as u32;
                self.positions.extend(ps.map(|p| xf.point(p)));
                let v = if xf.flips_handedness() { [base, base + 2, base + 1] } else { [base, base + 1, base + 2] };
                self.push_tri(v, None, None, material);
            }
            "mesh" | "gltf" | "stl" => {
                let file = str_opt(t, "file", ctx)?.ok_or_else(|| format!("{ctx}: missing 'file'"))?;
                let file = dir.join(file);
                self.track(&file);
                let ext = file.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
                let smooth = bool_or(t, "smooth", true, ctx)?;
                if ext == "gltf" || ext == "glb" {
                    let g = gltf::load(&file)?;
                    self.add_gltf(&g, &file, xf, mat, smooth, ctx)?;
                } else {
                    let data = mesh::load(&file)?;
                    let mesh_dir = file.parent().unwrap_or(Path::new(".")).to_path_buf();
                    let group_mat = self.mesh_materials(&data, mat, &mesh_dir, ctx)?;
                    self.push_mesh(&data, xf, &group_mat, smooth);
                }
            }
            "sdf" | "implicit" | "hypertexture" | "julia" | "quaternion_julia" | "mandelbulb" | "menger" => {
                let im = procedural::implicit(t, ty, ctx)?;
                let material = mat.unwrap_or_else(|| self.default_material());
                if self.materials[material as usize].is_emissive() {
                    eprintln!(
                        "warning: {ctx}: implicit surfaces are not sampled as lights (they only add light \
                         when hit by chance: expect noise)"
                    );
                }
                self.add_shape(ShapeKind::Implicit(Arc::new(im)), material, xf, ctx)?;
            }
            other => return Err(format!("{ctx}: unknown object type '{other}' ({OBJECT_TYPES})")),
        }
        Ok(())
    }

    /// Adds a shape transformed by `xf`; where the transform does not keep the shape's
    /// kind (e.g. a non-uniformly scaled sphere) it becomes an instance.
    fn add_shape(&mut self, kind: ShapeKind, material: u32, xf: &Transform, ctx: &str) -> Result<(), String> {
        let (kind, group) = match kind.transformed(xf) {
            Some(k) => (k, self.group),
            None if self.group == 0 => {
                let g = self.new_group();
                self.instances.push(InstanceDesc { group: g, xf: InstanceXf::Static(*xf) });
                (kind, g)
            }
            // Inside prototypes an implicit surface takes the transform into its field.
            None if let ShapeKind::Implicit(im) = &kind => {
                let moved = im.transformed(xf).map_err(|e| format!("{ctx}: {e}"))?;
                (ShapeKind::Implicit(Arc::new(moved)), self.group)
            }
            None => {
                return Err(format!(
                    "{ctx}: a non-uniform scale of spheres, disks and cylinders is not supported inside \
                     prototypes (use a mesh, or scale the instance)"
                ));
            }
        };
        self.shapes.push(Shape { kind, material });
        self.shape_group.push(group);
        Ok(())
    }

    /// Scene material of each material group of a mesh file: the object's `material`
    /// wins, then a scene material of the same name, then the mesh's material library,
    /// then the default.
    fn mesh_materials(
        &mut self,
        m: &mesh::MeshData,
        mat: Option<u32>,
        mesh_dir: &Path,
        ctx: &str,
    ) -> Result<Vec<u32>, String> {
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
        Ok(group_mat)
    }

    fn push_mesh(&mut self, m: &mesh::MeshData, xf: &Transform, group_mat: &[u32], smooth: bool) {
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
    }

    pub(super) fn push_tri(&mut self, v: [u32; 3], n: Option<[u32; 3]>, uv: Option<[u32; 3]>, material: u32) {
        let (p0, p1, p2) =
            (self.positions[v[0] as usize], self.positions[v[1] as usize], self.positions[v[2] as usize]);
        if (p1 - p0).cross(p2 - p0).length_squared() == 0.0 {
            return; // degenerate
        }
        self.triangles.push(Triangle { v, n: n.unwrap_or([NO_NORMALS; 3]), uv: uv.unwrap_or([NO_UVS; 3]), material });
        self.tri_group.push(self.group);
    }

    // -------------------------------------------------------------- glTF

    /// Adds every placed mesh of a glTF file, flattened into the object's space.
    fn add_gltf(
        &mut self,
        g: &Gltf,
        file: &Path,
        xf: &Transform,
        mat: Option<u32>,
        smooth: bool,
        ctx: &str,
    ) -> Result<(), String> {
        for (mi, world) in &g.placements {
            let mesh = &g.meshes[*mi];
            let group_mat = mesh
                .materials
                .iter()
                .map(|&gm| match mat {
                    Some(id) => Ok(id),
                    None => self.gltf_material(g, gm, file, ctx),
                })
                .collect::<Result<Vec<u32>, String>>()?;
            self.push_mesh(&mesh.data, &world.then(xf), &group_mat, smooth);
        }
        Ok(())
    }

    /// Scene material for glTF material `idx`: a scene material of the same name wins,
    /// primitives without a material get the default.
    fn gltf_material(&mut self, g: &Gltf, idx: Option<usize>, file: &Path, ctx: &str) -> Result<u32, String> {
        let Some(idx) = idx else { return Ok(self.default_material()) };
        let gm = g.materials.get(idx).ok_or_else(|| format!("{ctx}: invalid glTF material index {idx}"))?;
        if let Some(&id) = self.names.get(&gm.name) {
            return Ok(id);
        }
        let key = (file.to_path_buf(), idx);
        if let Some(&id) = self.gltf_ids.get(&key) {
            return Ok(id);
        }
        let ctx = format!("{}: material '{}'", file.display(), gm.name);
        let m = self.convert_gltf(g, gm, file, &ctx)?;
        let id = self.materials.len() as u32;
        self.materials.push(m);
        self.gltf_ids.insert(key, id);
        Ok(id)
    }

    fn gltf_texture(
        &mut self,
        g: &Gltf,
        index: usize,
        kind: TexKind,
        channel: Channel,
        file: &Path,
        ctx: &str,
    ) -> Result<Option<Texture>, String> {
        let Some(tex) = g.textures.get(index).copied().flatten() else {
            eprintln!("warning: {ctx}: texture {index} has no supported image; ignoring it");
            return Ok(None);
        };
        let srgb = kind == TexKind::Color;
        let img = match g.images.get(tex.image) {
            Some(GltfImage::File(p)) => self.image(p, srgb).map_err(|e| format!("{ctx}: {e}"))?,
            Some(GltfImage::Embedded(bytes)) => {
                let key = file.with_extension(format!("image{}", tex.image));
                self.image_bytes(&key, bytes, srgb).map_err(|e| format!("{ctx}: image {}: {e}", tex.image))?
            }
            None => return Err(format!("{ctx}: invalid image index {}", tex.image)),
        };
        Ok(Some(Texture::Image(Box::new(ImageTex {
            img,
            scale: [1.0, 1.0],
            offset: [0.0, 0.0],
            wrap: tex.wrap,
            nearest: tex.nearest,
            channel,
        }))))
    }

    /// Maps a glTF metallic-roughness material onto ours: emissive materials become
    /// lights, everything else the principled material.
    fn convert_gltf(&mut self, g: &Gltf, m: &GltfMaterial, file: &Path, ctx: &str) -> Result<Material, String> {
        let scaled = |tex: Option<Texture>, k: Vec3| match tex {
            Some(t) if k == Vec3::ONE => t,
            Some(t) => Texture::Scaled(Box::new(t), k),
            None => Texture::Const(k),
        };
        let tex = |b: &mut Self, idx: Option<usize>, kind, channel| -> Result<Option<Texture>, String> {
            match idx {
                Some(i) => b.gltf_texture(g, i, kind, channel, file, ctx),
                None => Ok(None),
            }
        };
        let base = Vec3::new(m.base_color[0], m.base_color[1], m.base_color[2]);
        let kind = if m.emissive.is_black() {
            let mr = |b: &mut Self, ch| tex(b, m.metallic_roughness_tex, TexKind::Scalar, ch);
            let metallic = mr(self, Channel::B)?;
            let roughness = mr(self, Channel::G)?;
            let base_color = tex(self, m.base_color_tex, TexKind::Color, Channel::Auto)?;
            let c = |v: f32| Texture::Const(Vec3::splat(v));
            MaterialKind::Principled(Box::new(Principled {
                base_color: scaled(base_color, base),
                metallic: scaled(metallic, Vec3::splat(m.metallic)),
                roughness: scaled(roughness, Vec3::splat(m.roughness)),
                specular: c(0.5),
                ior: m.ior.filter(|&i| i >= 1.0),
                transmission: c(m.transmission.clamp(0.0, 1.0)),
                clearcoat: c(0.0),
                clearcoat_roughness: c(0.03),
                sheen: c(0.0),
                sheen_tint: c(0.5),
            }))
        } else {
            let e = tex(self, m.emissive_tex, TexKind::Color, Channel::Auto)?;
            MaterialKind::Emissive { emission: scaled(e, m.emissive), two_sided: false }
        };
        let mut mat = Material::new(kind);
        if let Some((n, scale)) = m.normal_tex
            && let Some(t) = tex(self, Some(n), TexKind::Normal, Channel::Auto)?
        {
            mat.normal_map = Some((t, scale));
        }
        if m.alpha != AlphaMode::Opaque {
            // Masks are resolved stochastically like blending (no hard cutoff).
            let a = tex(self, m.base_color_tex, TexKind::Alpha, Channel::A)?;
            if a.is_some() || m.base_color[3] < 1.0 {
                mat.alpha = Some(scaled(a, Vec3::splat(m.base_color[3])));
            }
        }
        Ok(mat)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Table `[o]` of a TOML snippet.
    fn value_table(src: &str) -> Table {
        match crate::toml::parse(src).unwrap().remove("o") {
            Some(Value::Table(t)) => t,
            _ => panic!("test input must define [o]"),
        }
    }

    #[test]
    fn motion_defaults_to_start_values() {
        let t = value_table("[o]\ntranslate = [1, 2, 3]\nscale = 2\nmotion = { translate = [4, 2, 3] }\n");
        let start = parse_trs(&t, "o").unwrap();
        assert_eq!(start.scale, Vec3::splat(2.0));
        let end = parse_motion(&t, &start, "o").unwrap().unwrap();
        assert_eq!((end.translate, end.scale), (Vec3::new(4.0, 2.0, 3.0), Vec3::splat(2.0)));
        let still = value_table("[o]\nmotion = { scale = 1 }\n");
        assert!(parse_motion(&still, &Trs::IDENTITY, "o").unwrap().is_none());
        assert!(parse_trs(&value_table("[o]\nscale = [1, 0, 1]\n"), "o").is_err());
    }
}
