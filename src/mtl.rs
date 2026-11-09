//! Wavefront `.mtl` material library parser. Produces raw records; the scene loader
//! maps them onto our materials.

use crate::math::Vec3;

#[derive(Clone, Debug, PartialEq)]
pub struct MtlMap {
    /// Path as written in the file (relative to the .mtl file's directory).
    pub file: String,
    /// `-bm`: bump multiplier.
    pub bump_mult: f32,
    /// `-s` / `-o`: UV scale and offset.
    pub scale: [f32; 2],
    pub offset: [f32; 2],
    /// `-clamp on`.
    pub clamp: bool,
}

#[derive(Clone, Debug, Default)]
pub struct MtlMaterial {
    pub name: String,
    pub kd: Option<Vec3>,
    pub ks: Option<Vec3>,
    pub ke: Option<Vec3>,
    pub tf: Option<Vec3>,
    pub ns: Option<f32>,
    pub ni: Option<f32>,
    /// Dissolve (opacity); `Tr` is converted to `1 - Tr`.
    pub d: Option<f32>,
    pub illum: Option<u32>,
    /// PBR extension: roughness and metallic.
    pub pr: Option<f32>,
    pub pm: Option<f32>,
    pub map_kd: Option<MtlMap>,
    pub map_ks: Option<MtlMap>,
    pub map_ke: Option<MtlMap>,
    pub map_d: Option<MtlMap>,
    pub map_bump: Option<MtlMap>,
    pub map_norm: Option<MtlMap>,
    pub map_pr: Option<MtlMap>,
    pub map_pm: Option<MtlMap>,
}

pub fn parse(text: &str) -> Result<Vec<MtlMaterial>, String> {
    let mut out: Vec<MtlMaterial> = Vec::new();
    for (lineno, line) in text.lines().enumerate() {
        let err = |msg: &str| format!("line {}: {msg}", lineno + 1);
        let line = line.split('#').next().unwrap_or("").trim();
        let mut it = line.split_whitespace();
        let Some(key) = it.next() else { continue };
        let rest: Vec<&str> = it.collect();
        if key == "newmtl" {
            out.push(MtlMaterial { name: rest.join(" "), ..Default::default() });
            continue;
        }
        let Some(m) = out.last_mut() else {
            continue; // statements before the first newmtl
        };
        let f = |i: usize| rest.get(i).and_then(|s| s.parse::<f32>().ok());
        let color = || -> Result<Vec3, String> {
            if rest.first() == Some(&"spectral") || rest.first() == Some(&"xyz") {
                return Err(err("spectral / xyz colors are not supported"));
            }
            let r = f(0).ok_or_else(|| err(&format!("malformed '{key}'")))?;
            Ok(Vec3::new(r, f(1).unwrap_or(r), f(2).unwrap_or(r)))
        };
        let scalar = || f(0).ok_or_else(|| err(&format!("malformed '{key}'")));
        match key.to_ascii_lowercase().as_str() {
            "kd" => m.kd = Some(color()?),
            "ks" => m.ks = Some(color()?),
            "ke" => m.ke = Some(color()?),
            "tf" => m.tf = Some(color()?),
            "ns" => m.ns = Some(scalar()?),
            "ni" => m.ni = Some(scalar()?),
            "d" => m.d = Some(f(if rest.first() == Some(&"-halo") { 1 } else { 0 }).ok_or_else(|| err("malformed 'd'"))?),
            "tr" => m.d = Some(1.0 - scalar()?),
            "illum" => m.illum = Some(scalar()? as u32),
            "pr" => m.pr = Some(scalar()?),
            "pm" => m.pm = Some(scalar()?),
            "map_kd" => m.map_kd = Some(parse_map(&rest).ok_or_else(|| err("missing file name"))?),
            "map_ks" => m.map_ks = Some(parse_map(&rest).ok_or_else(|| err("missing file name"))?),
            "map_ke" => m.map_ke = Some(parse_map(&rest).ok_or_else(|| err("missing file name"))?),
            "map_d" => m.map_d = Some(parse_map(&rest).ok_or_else(|| err("missing file name"))?),
            "map_bump" | "bump" => m.map_bump = Some(parse_map(&rest).ok_or_else(|| err("missing file name"))?),
            "norm" | "map_norm" | "map_kn" => {
                m.map_norm = Some(parse_map(&rest).ok_or_else(|| err("missing file name"))?)
            }
            "map_pr" => m.map_pr = Some(parse_map(&rest).ok_or_else(|| err("missing file name"))?),
            "map_pm" => m.map_pm = Some(parse_map(&rest).ok_or_else(|| err("missing file name"))?),
            _ => {} // Ka, map_Ka, sharpness, refl, ...
        }
    }
    Ok(out)
}

/// Parses `[-option args ...] file name`.
fn parse_map(tok: &[&str]) -> Option<MtlMap> {
    let mut m = MtlMap { file: String::new(), bump_mult: 1.0, scale: [1.0, 1.0], offset: [0.0, 0.0], clamp: false };
    let mut i = 0;
    let num = |i: usize| tok.get(i).and_then(|s| s.parse::<f32>().ok());
    while i < tok.len() && tok[i].starts_with('-') && tok[i].len() > 1 && num(i).is_none() {
        let opt = tok[i].to_ascii_lowercase();
        i += 1;
        match opt.as_str() {
            "-bm" | "-boost" | "-texres" => {
                if opt == "-bm" {
                    m.bump_mult = num(i).unwrap_or(1.0);
                }
                i += 1;
            }
            "-s" | "-o" | "-t" => {
                let mut v = Vec::new();
                while v.len() < 3 && num(i).is_some() {
                    v.push(num(i).unwrap());
                    i += 1;
                }
                let x = v.first().copied();
                let y = v.get(1).copied().or(x);
                match (opt.as_str(), x, y) {
                    ("-s", Some(x), Some(y)) => m.scale = [x, y],
                    ("-o", Some(x), Some(y)) => m.offset = [x, y],
                    _ => {}
                }
            }
            "-mm" => i += 2,
            "-clamp" => {
                m.clamp = tok.get(i).is_some_and(|s| s.eq_ignore_ascii_case("on"));
                i += 1;
            }
            // on/off or single-token options: blendu, blendv, cc, imfchan, type
            _ => i += 1,
        }
    }
    let file = tok.get(i..)?.join(" ").replace('\\', "/");
    if file.is_empty() {
        return None;
    }
    m.file = file;
    Some(m)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_materials_and_map_options() {
        let src = "\
# comment
newmtl Wood Floor
Kd 0.8 0.6 0.4
Ks 0.1
Ns 250
d 0.5
illum 2
map_Kd -s 2 2 1 -o 0.5 0 -bm 0.3 textures\\wood floor.png
map_Bump -bm 0.05 bump.jpg
newmtl glass
Ni 1.45
Tr 0.1
illum 7
";
        let m = parse(src).unwrap();
        assert_eq!(m.len(), 2);
        assert_eq!(m[0].name, "Wood Floor");
        assert_eq!(m[0].kd, Some(Vec3::new(0.8, 0.6, 0.4)));
        assert_eq!(m[0].ks, Some(Vec3::splat(0.1)));
        let kd = m[0].map_kd.as_ref().unwrap();
        assert_eq!(kd.file, "textures/wood floor.png");
        assert_eq!((kd.scale, kd.offset), ([2.0, 2.0], [0.5, 0.0]));
        assert_eq!(m[0].map_bump.as_ref().unwrap().bump_mult, 0.05);
        assert_eq!(m[1].illum, Some(7));
        assert!((m[1].d.unwrap() - 0.9).abs() < 1e-6);
    }
}
