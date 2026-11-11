//! glTF 2.0 reader: `.gltf` (JSON with external or `data:` URI buffers) and `.glb`
//! (binary container). Reads the node hierarchy of the default scene, triangle meshes
//! (lists, strips, fans; positions, normals, first UV set) and metallic-roughness
//! materials with their textures, plus the emissive-strength, transmission and IOR
//! extensions. Mapping materials onto ours is the loader's job.

use crate::json::{self, Json};
use crate::math::{Mat3, Transform, Vec3};
use crate::mesh::{MeshData, MeshTriangle};
use crate::texture::Wrap;
use std::path::{Path, PathBuf};

pub struct Gltf {
    pub meshes: Vec<GltfMesh>,
    /// Every placed mesh: (mesh index, node-to-world transform).
    pub placements: Vec<(usize, Transform)>,
    pub materials: Vec<GltfMaterial>,
    pub textures: Vec<Option<GltfTexture>>,
    pub images: Vec<GltfImage>,
}

pub struct GltfMesh {
    pub data: MeshData,
    /// Material of each of `data.groups`; None means the glTF default material.
    pub materials: Vec<Option<usize>>,
}

pub enum GltfImage {
    File(PathBuf),
    Embedded(Vec<u8>),
}

#[derive(Clone, Copy)]
pub struct GltfTexture {
    pub image: usize,
    pub wrap: Wrap,
    pub nearest: bool,
}

#[derive(Clone, Copy, PartialEq)]
pub enum AlphaMode {
    Opaque,
    Mask,
    Blend,
}

pub struct GltfMaterial {
    pub name: String,
    pub base_color: [f32; 4],
    pub base_color_tex: Option<usize>,
    pub metallic: f32,
    pub roughness: f32,
    /// Roughness in green, metalness in blue.
    pub metallic_roughness_tex: Option<usize>,
    pub normal_tex: Option<(usize, f32)>,
    /// Emissive factor times `KHR_materials_emissive_strength`.
    pub emissive: Vec3,
    pub emissive_tex: Option<usize>,
    pub alpha: AlphaMode,
    pub transmission: f32,
    pub ior: Option<f32>,
}

pub fn load(path: &Path) -> Result<Gltf, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("cannot read '{}': {e}", path.display()))?;
    let dir = path.parent().unwrap_or(Path::new("."));
    parse(&bytes, dir).map_err(|e| format!("{}: {e}", path.display()))
}

/// Splits a GLB container into its JSON text and binary chunk.
fn glb_chunks(b: &[u8]) -> Result<(&str, Option<&[u8]>), String> {
    let u32_at = |i: usize| b.get(i..i + 4).map(|s| u32::from_le_bytes(s.try_into().unwrap()) as usize);
    if u32_at(4) != Some(2) {
        return Err("unsupported GLB version (need 2)".into());
    }
    let total = u32_at(8).ok_or("truncated GLB header")?.min(b.len());
    let (mut pos, mut json, mut bin) = (12, None, None);
    while pos + 8 <= total {
        let len = u32_at(pos).ok_or("truncated chunk")?;
        let ty = u32_at(pos + 4).ok_or("truncated chunk")?;
        let data = b.get(pos + 8..pos + 8 + len).ok_or("truncated GLB chunk")?;
        match ty {
            0x4E4F_534A => json = Some(std::str::from_utf8(data).map_err(|_| "GLB JSON chunk is not UTF-8")?),
            0x004E_4942 => bin = Some(data),
            _ => {}
        }
        pos += 8 + len.div_ceil(4) * 4;
    }
    Ok((json.ok_or("GLB without a JSON chunk")?, bin))
}

fn parse(bytes: &[u8], dir: &Path) -> Result<Gltf, String> {
    let (text, bin) = if bytes.starts_with(b"glTF") {
        glb_chunks(bytes)?
    } else {
        (std::str::from_utf8(bytes).map_err(|_| "not a glTF file (neither GLB nor UTF-8 JSON)")?, None)
    };
    let doc = json::parse(text.trim_start_matches('\u{feff}'))?;
    let version = doc.get("asset").get("version").as_str().unwrap_or("");
    if !version.starts_with('2') {
        return Err(format!("unsupported glTF version '{version}' (need 2.x)"));
    }
    for ext in doc.get("extensionsRequired").as_array() {
        let e = ext.as_str().unwrap_or("");
        if !matches!(e, "KHR_materials_emissive_strength" | "KHR_materials_transmission" | "KHR_materials_ior") {
            return Err(format!("required extension '{e}' is not supported"));
        }
    }

    let mut buffers = Vec::new();
    for (i, b) in doc.get("buffers").as_array().iter().enumerate() {
        let data = match b.get("uri").as_str() {
            Some(uri) => load_uri(uri, dir).map_err(|e| format!("buffer {i}: {e}"))?,
            None if i == 0 => bin.ok_or("buffer 0 has no uri and there is no GLB binary chunk")?.to_vec(),
            None => return Err(format!("buffer {i} has no uri")),
        };
        buffers.push(data);
    }
    let ctx = Ctx { doc: &doc, buffers: &buffers };

    let mut meshes = Vec::new();
    let mut skipped = 0;
    for (i, m) in doc.get("meshes").as_array().iter().enumerate() {
        meshes.push(ctx.mesh(m, &mut skipped).map_err(|e| format!("mesh {i}: {e}"))?);
    }
    if skipped > 0 {
        eprintln!("warning: skipped {skipped} non-triangle glTF primitives (points / lines)");
    }

    let nodes = doc.get("nodes").as_array();
    let roots: Vec<usize> =
        if let Some(s) = doc.get("scene").as_usize().or(doc.get("scenes").as_array().first().map(|_| 0)) {
            doc.get("scenes")
                .as_array()
                .get(s)
                .ok_or("invalid scene index")?
                .get("nodes")
                .as_array()
                .iter()
                .filter_map(Json::as_usize)
                .collect()
        } else {
            // No scenes: every node that is nobody's child is a root.
            let mut child = vec![false; nodes.len()];
            for n in nodes {
                for c in n.get("children").as_array().iter().filter_map(Json::as_usize) {
                    if let Some(f) = child.get_mut(c) {
                        *f = true;
                    }
                }
            }
            (0..nodes.len()).filter(|&i| !child[i]).collect()
        };
    let mut placements = Vec::new();
    let mut visits = 0;
    for r in roots {
        place(nodes, r, &Transform::IDENTITY, meshes.len(), &mut placements, &mut visits)?;
    }

    let materials = doc.get("materials").as_array().iter().enumerate().map(|(i, m)| material(m, i)).collect();
    let textures = doc
        .get("textures")
        .as_array()
        .iter()
        .map(|t| {
            let image = t.get("source").as_usize()?;
            let s = doc.get("samplers").as_array().get(t.get("sampler").as_usize()?).unwrap_or(&Json::Null);
            let wrap = match s.get("wrapS").as_usize() {
                Some(33071) => Wrap::Clamp,
                Some(33648) => Wrap::Mirror,
                _ => Wrap::Repeat,
            };
            Some(GltfTexture { image, wrap, nearest: s.get("magFilter").as_usize() == Some(9728) })
        })
        .collect();
    let mut images = Vec::new();
    for (i, img) in doc.get("images").as_array().iter().enumerate() {
        let image = match (img.get("uri").as_str(), img.get("bufferView").as_usize()) {
            (Some(uri), _) if !uri.starts_with("data:") => GltfImage::File(dir.join(percent_decode(uri))),
            (Some(uri), _) => GltfImage::Embedded(load_uri(uri, dir)?),
            (None, Some(v)) => GltfImage::Embedded(ctx.view(v).map_err(|e| format!("image {i}: {e}"))?.0.to_vec()),
            (None, None) => return Err(format!("image {i} has neither uri nor bufferView")),
        };
        images.push(image);
    }
    Ok(Gltf { meshes, placements, materials, textures, images })
}

/// Collects mesh placements below node `i`.
fn place(
    nodes: &[Json],
    i: usize,
    parent: &Transform,
    n_meshes: usize,
    out: &mut Vec<(usize, Transform)>,
    visits: &mut usize,
) -> Result<(), String> {
    *visits += 1;
    if *visits > 1_000_000 {
        return Err("node hierarchy has a cycle".into());
    }
    let n = nodes.get(i).ok_or_else(|| format!("invalid node index {i}"))?;
    let local = if let Some(m) = n.get("matrix").as_f32s::<16>() {
        // Column-major 4x4.
        let m3 = Mat3 { m: std::array::from_fn(|r| std::array::from_fn(|c| m[c * 4 + r])) };
        Transform::from_matrix(m3, Vec3::new(m[12], m[13], m[14]))
    } else {
        let t = n.get("translation").as_f32s::<3>().unwrap_or([0.0; 3]);
        let q = n.get("rotation").as_f32s::<4>().unwrap_or([0.0, 0.0, 0.0, 1.0]);
        let s = n.get("scale").as_f32s::<3>().unwrap_or([1.0; 3]);
        let len = q.iter().map(|v| v * v).sum::<f32>().sqrt();
        let q = if len > 0.0 { q.map(|v| v / len) } else { [0.0, 0.0, 0.0, 1.0] };
        Transform::from_matrix(
            Mat3::from_quat(q).mul(&Mat3::scale(Vec3::new(s[0], s[1], s[2]))),
            Vec3::new(t[0], t[1], t[2]),
        )
    };
    // A degenerate (e.g. zero-scale) node hides its subtree.
    let Ok(local) = local else { return Ok(()) };
    let world = local.then(parent);
    if let Some(m) = n.get("mesh").as_usize() {
        if m >= n_meshes {
            return Err(format!("node {i}: invalid mesh index {m}"));
        }
        out.push((m, world));
    }
    for c in n.get("children").as_array().iter().filter_map(Json::as_usize) {
        place(nodes, c, &world, n_meshes, out, visits)?;
    }
    Ok(())
}

fn tex_index(info: &Json) -> Option<usize> {
    if info.get("texCoord").as_usize().unwrap_or(0) != 0 {
        eprintln!("warning: glTF textures on a second UV set are not supported; ignoring one");
        return None;
    }
    info.get("index").as_usize()
}

fn material(m: &Json, i: usize) -> GltfMaterial {
    let pbr = m.get("pbrMetallicRoughness");
    let ext = m.get("extensions");
    let strength = ext.get("KHR_materials_emissive_strength").get("emissiveStrength").as_f32().unwrap_or(1.0);
    let e = m.get("emissiveFactor").as_f32s::<3>().unwrap_or([0.0; 3]);
    GltfMaterial {
        name: m.get("name").as_str().map_or_else(|| format!("material {i}"), str::to_owned),
        base_color: pbr.get("baseColorFactor").as_f32s::<4>().unwrap_or([1.0; 4]),
        base_color_tex: tex_index(pbr.get("baseColorTexture")),
        metallic: pbr.get("metallicFactor").as_f32().unwrap_or(1.0),
        roughness: pbr.get("roughnessFactor").as_f32().unwrap_or(1.0),
        metallic_roughness_tex: tex_index(pbr.get("metallicRoughnessTexture")),
        normal_tex: tex_index(m.get("normalTexture"))
            .map(|t| (t, m.get("normalTexture").get("scale").as_f32().unwrap_or(1.0))),
        emissive: Vec3::new(e[0], e[1], e[2]) * strength,
        emissive_tex: tex_index(m.get("emissiveTexture")),
        alpha: match m.get("alphaMode").as_str() {
            Some("MASK") => AlphaMode::Mask,
            Some("BLEND") => AlphaMode::Blend,
            _ => AlphaMode::Opaque,
        },
        transmission: ext.get("KHR_materials_transmission").get("transmissionFactor").as_f32().unwrap_or(0.0),
        ior: ext.get("KHR_materials_ior").get("ior").as_f32(),
    }
}

struct Ctx<'a> {
    doc: &'a Json,
    buffers: &'a [Vec<u8>],
}

/// A typed view of accessor data.
struct Accessor<'a> {
    data: &'a [u8],
    stride: usize,
    count: usize,
    comps: usize,
    ctype: usize,
    normalized: bool,
}

impl Accessor<'_> {
    fn csize(ctype: usize) -> Option<usize> {
        match ctype {
            5120 | 5121 => Some(1),
            5122 | 5123 => Some(2),
            5125 | 5126 => Some(4),
            _ => None,
        }
    }

    /// Component `c` of element `i` (normalized integers map to [0, 1] / [-1, 1]).
    fn get(&self, i: usize, c: usize) -> f64 {
        if self.data.is_empty() {
            return 0.0;
        }
        let size = Self::csize(self.ctype).unwrap_or(4);
        let at = i * self.stride + c * size;
        let b = &self.data[at..at + size];
        let (v, max) = match self.ctype {
            5120 => (f64::from(b[0] as i8), 127.0),
            5121 => (f64::from(b[0]), 255.0),
            5122 => (f64::from(i16::from_le_bytes([b[0], b[1]])), 32767.0),
            5123 => (f64::from(u16::from_le_bytes([b[0], b[1]])), 65535.0),
            5125 => (f64::from(u32::from_le_bytes([b[0], b[1], b[2], b[3]])), 1.0),
            _ => return f64::from(f32::from_le_bytes([b[0], b[1], b[2], b[3]])),
        };
        if self.normalized { (v / max).max(-1.0) } else { v }
    }

    fn vec3(&self, i: usize) -> Vec3 {
        Vec3::new(self.get(i, 0) as f32, self.get(i, 1) as f32, self.get(i, 2) as f32)
    }
}

impl Ctx<'_> {
    /// Bytes of buffer view `i` and its stride (0 = tightly packed).
    fn view(&self, i: usize) -> Result<(&[u8], usize), String> {
        let v = self.doc.get("bufferViews").as_array().get(i).ok_or_else(|| format!("invalid bufferView {i}"))?;
        let buf = self
            .buffers
            .get(v.get("buffer").as_usize().ok_or("bufferView without buffer")?)
            .ok_or("invalid buffer index")?;
        let off = v.get("byteOffset").as_usize().unwrap_or(0);
        let len = v.get("byteLength").as_usize().ok_or("bufferView without byteLength")?;
        let data = buf.get(off..off + len).ok_or_else(|| format!("bufferView {i} exceeds its buffer"))?;
        Ok((data, v.get("byteStride").as_usize().unwrap_or(0)))
    }

    fn accessor(&self, i: usize) -> Result<Accessor<'_>, String> {
        let a = self.doc.get("accessors").as_array().get(i).ok_or_else(|| format!("invalid accessor {i}"))?;
        if !a.get("sparse").is_null() {
            return Err("sparse accessors are not supported".into());
        }
        let comps = match a.get("type").as_str() {
            Some("SCALAR") => 1,
            Some("VEC2") => 2,
            Some("VEC3") => 3,
            Some("VEC4") => 4,
            t => return Err(format!("accessor {i}: unsupported type {t:?}")),
        };
        let ctype = a.get("componentType").as_usize().unwrap_or(0);
        let size = Accessor::csize(ctype).ok_or_else(|| format!("accessor {i}: invalid componentType {ctype}"))?;
        let count = a.get("count").as_usize().ok_or_else(|| format!("accessor {i}: missing count"))?;
        let normalized = a.get("normalized").as_bool().unwrap_or(false);
        let Some(view) = a.get("bufferView").as_usize() else {
            // No data: all zeros.
            return Ok(Accessor { data: &[], stride: 0, count, comps, ctype, normalized });
        };
        let (data, stride) = self.view(view)?;
        let off = a.get("byteOffset").as_usize().unwrap_or(0);
        let stride = if stride == 0 { comps * size } else { stride };
        let data = data.get(off..).unwrap_or(&[]);
        if count > 0 && (count - 1) * stride + comps * size > data.len() {
            return Err(format!("accessor {i} exceeds its bufferView"));
        }
        Ok(Accessor { data, stride, count, comps, ctype, normalized })
    }

    fn mesh(&self, m: &Json, skipped: &mut usize) -> Result<GltfMesh, String> {
        let mut out = GltfMesh { data: MeshData::default(), materials: Vec::new() };
        for p in m.get("primitives").as_array() {
            let mode = p.get("mode").as_usize().unwrap_or(4);
            if !(4..=6).contains(&mode) {
                *skipped += 1;
                continue;
            }
            let attrs = p.get("attributes");
            let pos = self.accessor(attrs.get("POSITION").as_usize().ok_or("primitive without POSITION")?)?;
            if pos.comps != 3 {
                return Err("POSITION must be VEC3".into());
            }
            let nrm = attrs.get("NORMAL").as_usize().map(|i| self.accessor(i)).transpose()?;
            let uv = attrs.get("TEXCOORD_0").as_usize().map(|i| self.accessor(i)).transpose()?;
            let n = pos.count;
            if nrm.as_ref().is_some_and(|a| a.count < n || a.comps != 3) || uv.as_ref().is_some_and(|a| a.count < n) {
                return Err("vertex attributes have mismatched counts".into());
            }
            let indices: Vec<u32> = match p.get("indices").as_usize() {
                Some(i) => {
                    let a = self.accessor(i)?;
                    (0..a.count).map(|k| a.get(k, 0) as u32).collect()
                }
                None => (0..n as u32).collect(),
            };
            if let Some(&bad) = indices.iter().find(|&&i| i as usize >= n) {
                return Err(format!("index {bad} out of range (have {n} vertices)"));
            }

            let d = &mut out.data;
            let material = p.get("material").as_usize();
            let group = if let Some(g) = out.materials.iter().position(|&g| g == material) {
                g
            } else {
                out.materials.push(material);
                d.groups.push(material.map_or_else(String::new, |m| format!("glTF material {m}")));
                out.materials.len() - 1
            } as u32;
            let (vb, nb, tb) = (d.positions.len() as u32, d.normals.len() as u32, d.uvs.len() as u32);
            d.positions.extend((0..n).map(|i| pos.vec3(i)));
            if let Some(a) = &nrm {
                d.normals.extend((0..n).map(|i| a.vec3(i)));
            }
            if let Some(a) = &uv {
                // glTF puts v = 0 at the top of the image, we at the bottom.
                d.uvs.extend((0..n).map(|i| [a.get(i, 0) as f32, 1.0 - a.get(i, 1) as f32]));
            }
            let mut tri = |a: u32, b: u32, c: u32| {
                d.triangles.push(MeshTriangle {
                    v: [vb + a, vb + b, vb + c],
                    n: nrm.as_ref().map(|_| [nb + a, nb + b, nb + c]),
                    uv: uv.as_ref().map(|_| [tb + a, tb + b, tb + c]),
                    group,
                });
            };
            let k = indices.len();
            match mode {
                4 => indices.as_chunks::<3>().0.iter().for_each(|t| tri(t[0], t[1], t[2])),
                5 => (2..k).for_each(|i| {
                    let (a, b, c) = (indices[i - 2], indices[i - 1], indices[i]);
                    if i % 2 == 0 { tri(a, b, c) } else { tri(b, a, c) }
                }),
                _ => (2..k).for_each(|i| tri(indices[0], indices[i - 1], indices[i])),
            }
        }
        Ok(out)
    }
}

/// Contents of a buffer or image URI: a base64 `data:` URI or a file next to the glTF.
fn load_uri(uri: &str, dir: &Path) -> Result<Vec<u8>, String> {
    if let Some(rest) = uri.strip_prefix("data:") {
        let (_, data) = rest.split_once(";base64,").ok_or("only base64 data URIs are supported")?;
        return base64(data);
    }
    let path = dir.join(percent_decode(uri));
    std::fs::read(&path).map_err(|e| format!("cannot read '{}': {e}", path.display()))
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex =
            b.get(i + 1..i + 3).and_then(|h| std::str::from_utf8(h).ok()).and_then(|h| u8::from_str_radix(h, 16).ok());
        match (b[i], hex) {
            (b'%', Some(v)) => {
                out.push(v);
                i += 3;
            }
            (c, _) => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn base64(s: &str) -> Result<Vec<u8>, String> {
    let val = |c: u8| -> Option<u32> {
        Some(u32::from(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            _ => return None,
        }))
    };
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    let (mut acc, mut bits) = (0u32, 0);
    for &c in s.as_bytes() {
        if c == b'=' || c.is_ascii_whitespace() {
            continue;
        }
        acc = (acc << 6) | val(c).ok_or("invalid base64 data")?;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_and_percent_decoding() {
        assert_eq!(base64("SGVsbG8sIHdvcmxkIQ==").unwrap(), b"Hello, world!");
        assert_eq!(base64("AAEC/w").unwrap(), [0, 1, 2, 255]);
        assert_eq!(percent_decode("my%20model.bin"), "my model.bin");
    }

    /// Builds a glTF with an embedded buffer: an indexed quad (u16 indices, normals,
    /// UVs) placed by two nodes, one a child of a translated, rotated parent.
    pub fn quad_gltf() -> String {
        let positions = [0.0f32, 0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 1.0, 0.0, 0.0, 1.0, 0.0];
        let normals = [0.0f32, 0.0, 1.0].repeat(4);
        let uvs = [0.0f32, 1.0, 1.0, 1.0, 1.0, 0.0, 0.0, 0.0];
        let mut bin: Vec<u8> = positions.iter().chain(&normals).chain(&uvs).flat_map(|v| v.to_le_bytes()).collect();
        for i in [0u16, 1, 2, 0, 2, 3] {
            bin.extend_from_slice(&i.to_le_bytes());
        }
        let b64 = {
            const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
            let mut s = String::new();
            for c in bin.chunks(3) {
                let n = (u32::from(c[0]) << 16)
                    | (u32::from(*c.get(1).unwrap_or(&0)) << 8)
                    | u32::from(*c.get(2).unwrap_or(&0));
                for k in 0..=c.len() {
                    s.push(T[(n >> (18 - 6 * k) & 63) as usize] as char);
                }
            }
            s
        };
        format!(
            r#"{{"asset": {{"version": "2.0"}}, "scene": 0, "scenes": [{{"nodes": [0]}}],
            "nodes": [{{"translation": [0, 0, -2], "rotation": [0, 0.7071068, 0, 0.7071068], "children": [1], "mesh": 0}},
                      {{"matrix": [2,0,0,0, 0,2,0,0, 0,0,2,0, 5,0,0,1], "mesh": 0}}],
            "meshes": [{{"primitives": [{{"attributes": {{"POSITION": 0, "NORMAL": 1, "TEXCOORD_0": 2}}, "indices": 3, "material": 0}}]}}],
            "materials": [{{"name": "red", "pbrMetallicRoughness": {{"baseColorFactor": [1, 0, 0, 1], "metallicFactor": 0}},
                           "emissiveFactor": [1, 1, 1], "extensions": {{"KHR_materials_emissive_strength": {{"emissiveStrength": 5}}}}}}],
            "buffers": [{{"byteLength": {len}, "uri": "data:application/octet-stream;base64,{b64}"}}],
            "bufferViews": [{{"buffer": 0, "byteOffset": 0, "byteLength": 48}}, {{"buffer": 0, "byteOffset": 48, "byteLength": 48}},
                            {{"buffer": 0, "byteOffset": 96, "byteLength": 32}}, {{"buffer": 0, "byteOffset": 128, "byteLength": 12}}],
            "accessors": [{{"bufferView": 0, "componentType": 5126, "count": 4, "type": "VEC3"}},
                          {{"bufferView": 1, "componentType": 5126, "count": 4, "type": "VEC3"}},
                          {{"bufferView": 2, "componentType": 5126, "count": 4, "type": "VEC2"}},
                          {{"bufferView": 3, "componentType": 5123, "count": 6, "type": "SCALAR"}}]}}"#,
            len = bin.len()
        )
    }

    #[test]
    fn reads_meshes_nodes_and_materials() {
        let g = parse(quad_gltf().as_bytes(), Path::new(".")).unwrap();
        assert_eq!(g.meshes.len(), 1);
        let m = &g.meshes[0];
        assert_eq!((m.data.positions.len(), m.data.triangles.len()), (4, 2));
        assert_eq!(m.data.triangles[1].v, [0, 2, 3]);
        assert_eq!(m.data.uvs[0], [0.0, 0.0]); // flipped from glTF's (0, 1)
        assert_eq!(m.materials, [Some(0)]);
        assert_eq!(g.placements.len(), 2);
        // Parent: rotate 90° about Y, then translate: (1, 0, 0) -> (0, 0, -3).
        let p = g.placements[0].1.point(Vec3::new(1.0, 0.0, 0.0));
        assert!((p - Vec3::new(0.0, 0.0, -3.0)).length() < 1e-5, "{p:?}");
        // Child: scale 2, translate (5, 0, 0), then the parent: (1, 0, 0) -> (7, 0, 0) -> (0, 0, -9).
        let p = g.placements[1].1.point(Vec3::new(1.0, 0.0, 0.0));
        assert!((p - Vec3::new(0.0, 0.0, -9.0)).length() < 1e-4, "{p:?}");
        let mat = &g.materials[0];
        assert_eq!(mat.name, "red");
        assert_eq!((mat.base_color, mat.metallic, mat.roughness), ([1.0, 0.0, 0.0, 1.0], 0.0, 1.0));
        assert_eq!(mat.emissive, Vec3::splat(5.0));
    }

    #[test]
    fn reads_glb_container() {
        let json = quad_gltf();
        let mut glb = b"glTF".to_vec();
        glb.extend_from_slice(&2u32.to_le_bytes());
        let padded = json.len().div_ceil(4) * 4;
        glb.extend_from_slice(&((12 + 8 + padded) as u32).to_le_bytes());
        glb.extend_from_slice(&(padded as u32).to_le_bytes());
        glb.extend_from_slice(&0x4E4F_534Au32.to_le_bytes());
        glb.extend_from_slice(json.as_bytes());
        glb.resize(12 + 8 + padded, b' ');
        let g = parse(&glb, Path::new(".")).unwrap();
        assert_eq!(g.placements.len(), 2);
    }
}
