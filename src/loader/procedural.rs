//! Procedural and implicit geometry: noise fractal parameters, signed distance field
//! trees, hypertextures and the fractal objects (`sdf`, `hypertexture`, `julia`,
//! `mandelbulb`, `menger`).

use super::{f32_or, num, str_opt, u32_or, vec3_or};
use crate::implicit::{Field, Hypertexture, Implicit, Julia, Options, Sdf};
use crate::math::{Trs, Vec3};
use crate::noise::{Fractal, FractalKind};
use crate::toml::{Table, Value};

const SDF_TYPES: &str =
    "sphere, box, torus, cylinder, capsule, union, intersection, difference, round, shell, displace";

fn positive(t: &Table, key: &str, default: f32, ctx: &str) -> Result<f32, String> {
    let v = f32_or(t, key, default, ctx)?;
    if v > 0.0 && v.is_finite() { Ok(v) } else { Err(format!("{ctx}: '{key}' must be positive")) }
}

fn non_negative(t: &Table, key: &str, default: f32, ctx: &str) -> Result<f32, String> {
    let v = f32_or(t, key, default, ctx)?;
    if v >= 0.0 && v.is_finite() { Ok(v) } else { Err(format!("{ctx}: '{key}' must not be negative")) }
}

fn unit_interval(t: &Table, key: &str, default: f32, ctx: &str) -> Result<f32, String> {
    let v = f32_or(t, key, default, ctx)?;
    if v > 0.0 && v < 1.0 { Ok(v) } else { Err(format!("{ctx}: '{key}' must be in (0, 1)")) }
}

fn vec4_or(t: &Table, key: &str, default: [f32; 4], ctx: &str) -> Result<[f32; 4], String> {
    let Some(v) = t.get(key) else { return Ok(default) };
    match v.as_array() {
        Some(a) if a.len() == 4 && a.iter().all(|x| x.as_f64().is_some()) => {
            Ok(std::array::from_fn(|i| a[i].as_f64().unwrap_or(0.0) as f32))
        }
        _ => Err(format!("{ctx}: '{key}' must be [x, y, z, w]")),
    }
}

/// Fractal parameters of a noise table: `scale` (`default_scale`), `octaves` (5, may be
/// fractional), `lacunarity` (2), `gain` or `h` (amplitude ratio, or the fractal increment
/// H with gain = lacunarity^-H), `offset`, `sharpness` and `warp`.
pub(super) fn fractal(t: &Table, kind: FractalKind, default_scale: f32, ctx: &str) -> Result<Fractal, String> {
    let mut f = Fractal::new(kind);
    f.scale = positive(t, "scale", default_scale, ctx)?;
    f.octaves = f32_or(t, "octaves", 5.0, ctx)?.clamp(1.0, 16.0);
    f.lacunarity = f32_or(t, "lacunarity", 2.0, ctx)?;
    if !(f.lacunarity > 1.0 && f.lacunarity.is_finite()) {
        return Err(format!("{ctx}: 'lacunarity' must be above 1"));
    }
    match (num(t, "gain", ctx)?, num(t, "h", ctx)?) {
        (Some(_), Some(_)) => return Err(format!("{ctx}: give 'gain' or 'h', not both")),
        (Some(g), None) => f.gain = g as f32,
        (None, Some(h)) => f.gain = f.lacunarity.powf(-h as f32),
        (None, None) => {}
    }
    if !(f.gain > 0.0 && f.gain.is_finite()) {
        return Err(format!("{ctx}: 'gain' must be positive"));
    }
    f.offset = f32_or(t, "offset", f.offset, ctx)?;
    f.sharpness = non_negative(t, "sharpness", f.sharpness, ctx)?;
    f.warp = f32_or(t, "warp", 0.0, ctx)?;
    Ok(f)
}

/// A `noise = { type = ..., ... }` table (default: `default_kind` at `default_scale`).
fn noise_table(v: Option<&Value>, default: Fractal, ctx: &str) -> Result<Fractal, String> {
    let Some(v) = v else { return Ok(default) };
    let t = v.as_table().ok_or_else(|| format!("{ctx}: 'noise' must be a table {{ type, scale, octaves, ... }}"))?;
    let kind = match str_opt(t, "type", ctx)? {
        None => default.kind,
        Some(s) => {
            FractalKind::parse(s).ok_or_else(|| format!("{ctx}: unknown noise type '{s}' ({})", FractalKind::NAMES))?
        }
    };
    let mut f = fractal(t, kind, default.scale, ctx)?;
    if !t.contains_key("octaves") {
        f.octaves = default.octaves;
    }
    Ok(f)
}

/// The `translate` / `rotate` / `scale` of a node.
fn node_trs(t: &Table, ctx: &str) -> Result<Trs, String> {
    let trs = Trs {
        translate: vec3_or(t, "translate", Vec3::ZERO, ctx)?,
        rotate: vec3_or(t, "rotate", Vec3::ZERO, ctx)?,
        scale: vec3_or(t, "scale", Vec3::ONE, ctx)?,
    };
    trs.transform().map_err(|e| format!("{ctx}: {e}"))?;
    Ok(trs)
}

/// One node of a signed distance field tree: `{ type = ..., translate, rotate, scale, ... }`.
pub(super) fn sdf(v: &Value, ctx: &str) -> Result<Sdf, String> {
    let t = v.as_table().ok_or_else(|| format!("{ctx}: an SDF node must be a table {{ type = ... }}"))?;
    let ty = str_opt(t, "type", ctx)?.ok_or_else(|| format!("{ctx}: an SDF node needs a 'type' ({SDF_TYPES})"))?;
    let ctx = &format!("{ctx} > {ty}");
    let children = || -> Result<Vec<Sdf>, String> {
        let list =
            t.get("children").and_then(Value::as_array).ok_or_else(|| format!("{ctx}: missing 'children' = [...]"))?;
        list.iter().enumerate().map(|(i, c)| sdf(c, &format!("{ctx} #{}", i + 1))).collect()
    };
    let child = || -> Result<Box<Sdf>, String> {
        let c = t.get("child").ok_or_else(|| format!("{ctx}: missing 'child' = {{ ... }}"))?;
        Ok(Box::new(sdf(c, ctx)?))
    };
    let blend = || non_negative(t, "blend", 0.0, ctx);
    let node = match ty {
        "sphere" => Sdf::Sphere { radius: positive(t, "radius", 1.0, ctx)? },
        "box" | "cube" => {
            let size = vec3_or(t, "size", Vec3::ONE, ctx)?;
            if !(size.x > 0.0 && size.y > 0.0 && size.z > 0.0) {
                return Err(format!("{ctx}: 'size' must be positive"));
            }
            let half = size * 0.5;
            let round = non_negative(t, "round", 0.0, ctx)?;
            if round > half.x.min(half.y).min(half.z) {
                return Err(format!("{ctx}: 'round' must not exceed half the smallest side"));
            }
            Sdf::Box { half, round }
        }
        "torus" => Sdf::Torus { major: positive(t, "major", 1.0, ctx)?, minor: positive(t, "minor", 0.25, ctx)? },
        "cylinder" => Sdf::Cylinder {
            radius: positive(t, "radius", 0.5, ctx)?,
            half_height: 0.5 * positive(t, "height", 1.0, ctx)?,
        },
        "capsule" => Sdf::Capsule {
            a: vec3_or(t, "a", Vec3::new(0.0, -0.5, 0.0), ctx)?,
            b: vec3_or(t, "b", Vec3::new(0.0, 0.5, 0.0), ctx)?,
            radius: positive(t, "radius", 0.25, ctx)?,
        },
        "union" | "intersection" => {
            let children = children()?;
            if children.is_empty() {
                return Err(format!("{ctx}: needs at least one child"));
            }
            let blend = blend()?;
            if ty == "union" { Sdf::Union { children, blend } } else { Sdf::Intersection { children, blend } }
        }
        "difference" | "subtract" => {
            let mut children = children()?;
            if children.len() < 2 {
                return Err(format!("{ctx}: needs a base and at least one child to cut away"));
            }
            let base = Box::new(children.remove(0));
            Sdf::Difference { base, cuts: children, blend: blend()? }
        }
        "round" => Sdf::Round { child: child()?, radius: non_negative(t, "radius", 0.05, ctx)? },
        "shell" | "onion" => Sdf::Shell { child: child()?, thickness: positive(t, "thickness", 0.05, ctx)? },
        "displace" => {
            let default = Fractal { scale: 4.0, ..Fractal::new(FractalKind::Fbm) };
            Sdf::Displace {
                child: child()?,
                noise: noise_table(t.get("noise"), default, ctx)?,
                amount: f32_or(t, "amount", 0.1, ctx)?,
            }
        }
        other => return Err(format!("{ctx}: unknown SDF type '{other}' ({SDF_TYPES})")),
    };
    node.transformed(&node_trs(t, ctx)?).map_err(|e| format!("{ctx}: {e}"))
}

/// A hypertexture: `shape` (an SDF node; default a unit sphere), `softness` (0.3),
/// `noise` (turbulence, scale 3, 6 octaves), `amount` (0.6), `bias` and `gain` (0.5).
pub(super) fn hypertexture(t: &Table, ctx: &str) -> Result<Hypertexture, String> {
    let base = match t.get("shape") {
        Some(v) => sdf(v, &format!("{ctx} shape"))?,
        None => Sdf::Sphere { radius: 1.0 },
    };
    let default = Fractal { scale: 3.0, octaves: 6.0, ..Fractal::new(FractalKind::Turbulence) };
    Ok(Hypertexture {
        base,
        softness: positive(t, "softness", 0.3, ctx)?,
        noise: noise_table(t.get("noise"), default, ctx)?,
        amount: f32_or(t, "amount", 0.6, ctx)?,
        bias: unit_interval(t, "bias", 0.5, ctx)?,
        gain: unit_interval(t, "gain", 0.5, ctx)?,
    })
}

/// An implicit surface object of type `ty` (sdf, hypertexture, julia, mandelbulb, menger), with the shared
/// options `epsilon`, `lipschitz` and `max_steps`.
pub(super) fn implicit(t: &Table, ty: &str, ctx: &str) -> Result<Implicit, String> {
    let field = match ty {
        "sdf" | "implicit" => {
            let v = t.get("sdf").ok_or_else(|| format!("{ctx}: missing 'sdf' = {{ type = ... }}"))?;
            Field::Sdf(sdf(v, &format!("{ctx} sdf"))?)
        }
        "hypertexture" => Field::Hypertexture { h: hypertexture(t, ctx)?, level: unit_interval(t, "level", 0.5, ctx)? },
        "julia" | "quaternion_julia" => {
            let iterations = u32_or(t, "iterations", 12, ctx)?;
            if !(1..=256).contains(&iterations) {
                return Err(format!("{ctx}: 'iterations' must be in [1, 256]"));
            }
            // Quaternions are written (real, i, j, k) and stored (i, j, k, real).
            let internal = |q: [f32; 4]| [q[1], q[2], q[3], q[0]];
            let c = internal(vec4_or(t, "c", [-1.0, 0.2, 0.0, 0.0], ctx)?);
            let normal = internal(vec4_or(t, "slice_normal", [0.0, 0.0, 0.0, 1.0], ctx)?);
            let julia =
                Julia::new(c, iterations, normal, f32_or(t, "slice", 0.0, ctx)?).map_err(|e| format!("{ctx}: {e}"))?;
            Field::Julia(julia)
        }
        "mandelbulb" => {
            let power = f32_or(t, "power", 8.0, ctx)?;
            if !(2.0..=32.0).contains(&power) {
                return Err(format!("{ctx}: 'power' must be in [2, 32]"));
            }
            Field::Mandelbulb { power, iterations: u32_or(t, "iterations", 10, ctx)?.clamp(1, 256) }
        }
        "menger" => {
            let iterations = u32_or(t, "iterations", 4, ctx)?;
            if iterations > 10 {
                return Err(format!("{ctx}: 'iterations' must be at most 10"));
            }
            Field::Menger { iterations }
        }
        other => unreachable!("not an implicit type: {other}"),
    };
    let opts = Options {
        epsilon: num(t, "epsilon", ctx)?.map(|v| v as f32),
        lipschitz: num(t, "lipschitz", ctx)?.map(|v| v as f32),
        max_steps: t.get("max_steps").map(|_| u32_or(t, "max_steps", 0, ctx)).transpose()?,
    };
    Implicit::new(field, &opts).map_err(|e| format!("{ctx}: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(src: &str) -> Table {
        match crate::toml::parse(src).unwrap().remove("o") {
            Some(Value::Table(t)) => t,
            _ => panic!("test input must define [o]"),
        }
    }

    #[test]
    fn parses_sdf_trees() {
        let t = table(
            r#"
[o]
sdf = { type = "difference", blend = 0.05, children = [
    { type = "union", blend = 0.2, children = [
        { type = "box", size = [1, 1, 1], round = 0.1 },
        { type = "torus", major = 0.6, minor = 0.15, rotate = [90, 0, 0] },
    ] },
    { type = "sphere", radius = 0.6, translate = [0, 0.5, 0] },
] }
"#,
        );
        let im = implicit(&t, "sdf", "o").unwrap();
        assert!(im.eval(Vec3::new(0.0, -0.3, 0.0)) < 0.0, "inside the box, below the cut");
        assert!(im.eval(Vec3::new(0.0, 0.4, 0.0)) > 0.0, "cut away by the sphere");
        let b = im.bounds();
        assert!(b.max.x >= 0.75 && b.max.x < 1.5, "{b:?}");
        let bad = |src: &str| implicit(&table(src), "sdf", "o").unwrap_err();
        assert!(bad("[o]\nsdf = { type = \"blob\" }\n").contains("unknown SDF type"));
        assert!(bad("[o]\nsdf = { type = \"difference\", children = [{ type = \"sphere\" }] }\n").contains("base"));
        assert!(bad("[o]\nsdf = { type = \"box\", size = 1, round = 0.7 }\n").contains("round"));
    }

    #[test]
    fn parses_fractals_and_noise() {
        let t = table("[o]\nscale = 2\noctaves = 3.5\nh = 1\nlacunarity = 3\nwarp = 0.5\n");
        let f = fractal(&t, FractalKind::Ridged, 1.0, "o").unwrap();
        assert!((f.gain - 1.0 / 3.0).abs() < 1e-6);
        assert_eq!((f.octaves, f.warp, f.offset), (3.5, 0.5, 1.0));
        assert!(fractal(&table("[o]\ngain = 0.5\nh = 1\n"), FractalKind::Fbm, 1.0, "o").is_err());
        let im =
            implicit(&table("[o]\nc = [0.1, 0.2, 0.3, 0.4]\nslice = 0.1\niterations = 8\n"), "julia", "o").unwrap();
        assert!(im.bounds().max.x > 1.0);
        assert!(implicit(&table("[o]\nc = [0.1, 0.2]\n"), "julia", "o").is_err());
        assert!(
            implicit(&table("[o]\nepsilon = 0.001\nlipschitz = 2\n"), "menger", "o")
                .is_ok_and(|m| (m.lipschitz() - 2.0).abs() < 1e-6)
        );
        let h = implicit(
            &table("[o]\nnoise = { type = \"ridged\", scale = 5 }\namount = 0.4\nlevel = 0.4\n"),
            "hypertexture",
            "o",
        );
        assert!(h.unwrap().lipschitz() > 0.0);
    }
}
