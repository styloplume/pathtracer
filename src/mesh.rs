//! Triangle mesh loaders: Wavefront OBJ and Stanford PLY (ASCII and binary).

use crate::math::Vec3;

#[derive(Clone, Debug, Default)]
pub struct MeshData {
    pub positions: Vec<Vec3>,
    pub normals: Vec<Vec3>,
    pub triangles: Vec<MeshTriangle>,
    /// Material names referenced by triangles (`usemtl` in OBJ). Index 0 is "" (none).
    pub groups: Vec<String>,
}

#[derive(Clone, Copy, Debug)]
pub struct MeshTriangle {
    pub v: [u32; 3],
    pub n: Option<[u32; 3]>,
    pub group: u32,
}

pub fn load(path: &std::path::Path) -> Result<MeshData, String> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    let bytes = std::fs::read(path).map_err(|e| format!("cannot read '{}': {e}", path.display()))?;
    let res = match ext.as_str() {
        "obj" => load_obj(&String::from_utf8_lossy(&bytes)),
        "ply" => load_ply(&bytes),
        _ => Err(format!("unsupported mesh format '.{ext}' (supported: .obj, .ply)")),
    };
    res.map_err(|e| format!("{}: {e}", path.display()))
}

pub fn load_obj(text: &str) -> Result<MeshData, String> {
    let mut m = MeshData { groups: vec![String::new()], ..Default::default() };
    let mut group = 0u32;
    let mut corners: Vec<(u32, Option<u32>)> = Vec::new();
    for (lineno, line) in text.lines().enumerate() {
        let err = |msg: String| format!("line {}: {msg}", lineno + 1);
        let line = line.split('#').next().unwrap_or("").trim();
        let mut it = line.split_whitespace();
        match it.next() {
            Some("v") => m.positions.push(parse_vec3(&mut it).ok_or_else(|| err("malformed vertex".into()))?),
            Some("vn") => m.normals.push(parse_vec3(&mut it).ok_or_else(|| err("malformed normal".into()))?),
            Some("usemtl") => {
                let name = it.collect::<Vec<_>>().join(" ");
                group = match m.groups.iter().position(|g| *g == name) {
                    Some(i) => i as u32,
                    None => {
                        m.groups.push(name);
                        (m.groups.len() - 1) as u32
                    }
                };
            }
            Some("f") => {
                corners.clear();
                for tok in it {
                    let mut parts = tok.split('/');
                    let v = resolve_obj_index(parts.next().unwrap_or(""), m.positions.len()).map_err(&err)?;
                    let _vt = parts.next();
                    let n = match parts.next() {
                        Some(s) if !s.is_empty() => Some(resolve_obj_index(s, m.normals.len()).map_err(&err)?),
                        _ => None,
                    };
                    corners.push((v, n));
                }
                if corners.len() < 3 {
                    return Err(err("face with fewer than 3 vertices".into()));
                }
                let all_normals = corners.iter().all(|c| c.1.is_some());
                for i in 1..corners.len() - 1 {
                    let (a, b, c) = (corners[0], corners[i], corners[i + 1]);
                    m.triangles.push(MeshTriangle {
                        v: [a.0, b.0, c.0],
                        n: all_normals.then(|| [a.1.unwrap(), b.1.unwrap(), c.1.unwrap()]),
                        group,
                    });
                }
            }
            _ => {} // vt, o, g, s, mtllib, ... are ignored for now
        }
    }
    Ok(m)
}

fn parse_vec3<'a>(it: &mut impl Iterator<Item = &'a str>) -> Option<Vec3> {
    let mut f = || it.next()?.parse::<f32>().ok();
    Some(Vec3::new(f()?, f()?, f()?))
}

fn resolve_obj_index(s: &str, len: usize) -> Result<u32, String> {
    let i: i64 = s.parse().map_err(|_| format!("invalid index '{s}'"))?;
    let idx = if i > 0 { i - 1 } else { len as i64 + i };
    if i == 0 || idx < 0 || idx >= len as i64 {
        return Err(format!("index {i} out of range (have {len})"));
    }
    Ok(idx as u32)
}

// ---------------------------------------------------------------------------- PLY

#[derive(Clone, Copy, Debug)]
enum Ty {
    I8,
    U8,
    I16,
    U16,
    I32,
    U32,
    F32,
    F64,
}

impl Ty {
    fn parse(s: &str) -> Result<Ty, String> {
        Ok(match s {
            "char" | "int8" => Ty::I8,
            "uchar" | "uint8" => Ty::U8,
            "short" | "int16" => Ty::I16,
            "ushort" | "uint16" => Ty::U16,
            "int" | "int32" => Ty::I32,
            "uint" | "uint32" => Ty::U32,
            "float" | "float32" => Ty::F32,
            "double" | "float64" => Ty::F64,
            _ => return Err(format!("unknown PLY type '{s}'")),
        })
    }
    fn size(self) -> usize {
        match self {
            Ty::I8 | Ty::U8 => 1,
            Ty::I16 | Ty::U16 => 2,
            Ty::I32 | Ty::U32 | Ty::F32 => 4,
            Ty::F64 => 8,
        }
    }
}

enum PropKind {
    Scalar(Ty),
    List(Ty, Ty),
}

struct Element {
    name: String,
    count: usize,
    props: Vec<(String, PropKind)>,
}

#[derive(Clone, Copy, PartialEq)]
enum Format {
    Ascii,
    BinaryLe,
    BinaryBe,
}

struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
    format: Format,
}

impl Reader<'_> {
    fn read(&mut self, ty: Ty) -> Result<f64, String> {
        if self.format == Format::Ascii {
            while self.pos < self.data.len() && self.data[self.pos].is_ascii_whitespace() {
                self.pos += 1;
            }
            let start = self.pos;
            while self.pos < self.data.len() && !self.data[self.pos].is_ascii_whitespace() {
                self.pos += 1;
            }
            let tok = std::str::from_utf8(&self.data[start..self.pos]).map_err(|_| "invalid ASCII data")?;
            return tok.parse::<f64>().map_err(|_| format!("invalid number '{tok}'"));
        }
        let n = ty.size();
        let b = self.data.get(self.pos..self.pos + n).ok_or("unexpected end of PLY data")?;
        self.pos += n;
        let le = self.format == Format::BinaryLe;
        macro_rules! num {
            ($t:ty) => {{
                let arr = b.try_into().unwrap();
                (if le { <$t>::from_le_bytes(arr) } else { <$t>::from_be_bytes(arr) }) as f64
            }};
        }
        Ok(match ty {
            Ty::I8 => b[0] as i8 as f64,
            Ty::U8 => b[0] as f64,
            Ty::I16 => num!(i16),
            Ty::U16 => num!(u16),
            Ty::I32 => num!(i32),
            Ty::U32 => num!(u32),
            Ty::F32 => num!(f32),
            Ty::F64 => num!(f64),
        })
    }
}

pub fn load_ply(data: &[u8]) -> Result<MeshData, String> {
    let marker = b"end_header";
    let hpos = data.windows(marker.len()).position(|w| w == marker).ok_or("missing 'end_header'")?;
    let mut body = hpos + marker.len();
    while body < data.len() && data[body] != b'\n' {
        body += 1;
    }
    body += 1;
    let header = std::str::from_utf8(&data[..hpos]).map_err(|_| "header is not valid text")?;

    let mut lines = header.lines();
    if lines.next().map(str::trim) != Some("ply") {
        return Err("not a PLY file".into());
    }
    let mut format = None;
    let mut elements: Vec<Element> = Vec::new();
    for line in lines {
        let t: Vec<&str> = line.split_whitespace().collect();
        match t.first().copied() {
            Some("format") => {
                format = Some(match t.get(1).copied() {
                    Some("ascii") => Format::Ascii,
                    Some("binary_little_endian") => Format::BinaryLe,
                    Some("binary_big_endian") => Format::BinaryBe,
                    other => return Err(format!("unknown PLY format {other:?}")),
                })
            }
            Some("element") => {
                let (Some(name), Some(count)) = (t.get(1), t.get(2).and_then(|c| c.parse().ok())) else {
                    return Err(format!("malformed element line '{line}'"));
                };
                elements.push(Element { name: name.to_string(), count, props: Vec::new() });
            }
            Some("property") => {
                let el = elements.last_mut().ok_or("property before element")?;
                let (kind, name) = if t.get(1) == Some(&"list") {
                    if t.len() < 5 {
                        return Err(format!("malformed property line '{line}'"));
                    }
                    (PropKind::List(Ty::parse(t[2])?, Ty::parse(t[3])?), t[4])
                } else {
                    if t.len() < 3 {
                        return Err(format!("malformed property line '{line}'"));
                    }
                    (PropKind::Scalar(Ty::parse(t[1])?), t[2])
                };
                el.props.push((name.to_string(), kind));
            }
            _ => {} // comment, obj_info
        }
    }
    let format = format.ok_or("missing PLY format line")?;

    let mut r = Reader { data: data.get(body..).unwrap_or(&[]), pos: 0, format };
    let mut m = MeshData { groups: vec![String::new()], ..Default::default() };
    let mut has_normals = false;
    let mut list = Vec::new();
    for el in &elements {
        let find = |n: &str| el.props.iter().position(|p| p.0 == n);
        let (px, py, pz) = (find("x"), find("y"), find("z"));
        let (nx, ny, nz) = (find("nx"), find("ny"), find("nz"));
        let face_prop = find("vertex_indices").or_else(|| find("vertex_index"));
        let is_vertex = el.name == "vertex";
        if is_vertex {
            has_normals = nx.is_some() && ny.is_some() && nz.is_some();
        }
        let mut vals = vec![0.0f64; el.props.len()];
        for _ in 0..el.count {
            for (pi, (_, kind)) in el.props.iter().enumerate() {
                match *kind {
                    PropKind::Scalar(ty) => vals[pi] = r.read(ty)?,
                    PropKind::List(cty, ity) => {
                        let n = r.read(cty)? as usize;
                        list.clear();
                        for _ in 0..n {
                            list.push(r.read(ity)?);
                        }
                        if el.name == "face" && Some(pi) == face_prop {
                            if n < 3 {
                                continue;
                            }
                            for k in 1..n - 1 {
                                let v = [list[0] as u32, list[k] as u32, list[k + 1] as u32];
                                m.triangles.push(MeshTriangle { v, n: None, group: 0 });
                            }
                        }
                    }
                }
            }
            if is_vertex {
                let g = |i: Option<usize>| i.map(|i| vals[i] as f32).unwrap_or(0.0);
                m.positions.push(Vec3::new(g(px), g(py), g(pz)));
                if has_normals {
                    m.normals.push(Vec3::new(g(nx), g(ny), g(nz)));
                }
            }
        }
    }
    let nv = m.positions.len() as u32;
    for t in &mut m.triangles {
        if t.v.iter().any(|&i| i >= nv) {
            return Err(format!("face index out of range (have {nv} vertices)"));
        }
        if has_normals {
            t.n = Some(t.v);
        }
    }
    Ok(m)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn obj_quads_and_negative_indices() {
        let src = "v 0 0 0\nv 1 0 0\nv 1 1 0\nv 0 1 0\nvn 0 0 1\nusemtl red\nf -4//1 -3//1 -2//1 -1//1\n";
        let m = load_obj(src).unwrap();
        assert_eq!(m.triangles.len(), 2);
        assert_eq!(m.triangles[1].v, [0, 2, 3]);
        assert_eq!(m.triangles[0].n, Some([0, 0, 0]));
        assert_eq!(m.groups[m.triangles[0].group as usize], "red");
    }

    #[test]
    fn ply_ascii_and_binary() {
        let ascii = "ply\nformat ascii 1.0\nelement vertex 3\nproperty float x\nproperty float y\nproperty float z\nelement face 1\nproperty list uchar int vertex_indices\nend_header\n0 0 0\n1 0 0\n0 1 0\n3 0 1 2\n";
        let m = load_ply(ascii.as_bytes()).unwrap();
        assert_eq!((m.positions.len(), m.triangles.len()), (3, 1));

        let mut bin = b"ply\nformat binary_little_endian 1.0\nelement vertex 3\nproperty float x\nproperty float y\nproperty float z\nelement face 1\nproperty list uchar int vertex_indices\nend_header\n".to_vec();
        for v in [0.0f32, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0] {
            bin.extend_from_slice(&v.to_le_bytes());
        }
        bin.push(3);
        for i in [0i32, 1, 2] {
            bin.extend_from_slice(&i.to_le_bytes());
        }
        let m = load_ply(&bin).unwrap();
        assert_eq!(m.positions[1], Vec3::new(1.0, 0.0, 0.0));
        assert_eq!(m.triangles[0].v, [0, 1, 2]);
    }
}
