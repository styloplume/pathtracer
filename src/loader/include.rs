//! Scene includes: `include = "file.toml"` (or a list) at the top of a scene file pulls
//! in other scene files, typically material libraries or shared settings.
//!
//! Merging: `[[objects]]` and `[[lights]]` lists are concatenated (included files
//! first); tables such as `[render]`, `[camera]`, `[materials]` and `[prototypes]` are
//! merged one level deep, so the including file overrides single settings or whole
//! named materials; anything else (including `[background]`) is replaced. Relative paths
//! inside an included file stay relative to that file: its entries remember their
//! directory under a hidden key.

use crate::toml::{self, Table, Value};
use std::path::{Path, PathBuf};

/// Hidden key holding the directory an entry's relative paths resolve against.
pub(super) const DIR_KEY: &str = "$dir";

/// The directory relative paths of entry `t` resolve against.
pub(super) fn dir_of(t: &Table, default: &Path) -> PathBuf {
    match t.get(DIR_KEY) {
        Some(Value::String(d)) => PathBuf::from(d),
        _ => default.to_path_buf(),
    }
}

/// Parses `path` with everything it includes, recursively. Included files are appended
/// to `files`; `stack` holds the files being included (for cycle detection).
pub(super) fn load_doc(path: &Path, files: &mut Vec<PathBuf>, stack: &mut Vec<PathBuf>) -> Result<Table, String> {
    let canon = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    if stack.contains(&canon) {
        return Err(format!("include cycle: '{}' includes itself", path.display()));
    }
    let text = std::fs::read_to_string(path).map_err(|e| format!("cannot read '{}': {e}", path.display()))?;
    let mut doc = toml::parse(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    let includes: Vec<String> = match doc.remove("include") {
        None => Vec::new(),
        Some(Value::String(s)) => vec![s],
        Some(Value::Array(a)) => a
            .iter()
            .map(|v| v.as_str().map(str::to_owned))
            .collect::<Option<_>>()
            .ok_or_else(|| format!("{}: 'include' must be a string or an array of strings", path.display()))?,
        Some(_) => return Err(format!("{}: 'include' must be a string or an array of strings", path.display())),
    };
    if includes.is_empty() {
        return Ok(doc);
    }
    let dir = path.parent().unwrap_or(Path::new("."));
    stack.push(canon);
    let mut merged = Table::new();
    for inc in includes {
        let p = dir.join(&inc);
        if !files.contains(&p) {
            files.push(p.clone());
        }
        let mut sub = load_doc(&p, files, stack).map_err(|e| format!("{e}\n  (included from '{}')", path.display()))?;
        annotate(&mut sub, p.parent().unwrap_or(Path::new(".")));
        merge(&mut merged, sub);
    }
    stack.pop();
    merge(&mut merged, doc);
    Ok(merged)
}

/// Marks the path-carrying entries of an included document with their directory
/// (entries from deeper includes already carry theirs).
fn annotate(doc: &mut Table, dir: &Path) {
    let dir = Value::String(dir.to_string_lossy().into_owned());
    let mark = |t: &mut Table| {
        t.entry(DIR_KEY.to_owned()).or_insert_with(|| dir.clone());
    };
    for (key, v) in doc.iter_mut() {
        match (key.as_str(), v) {
            ("background", Value::Table(t)) => mark(t),
            ("materials" | "media" | "prototypes", Value::Table(entries)) => {
                for e in entries.values_mut() {
                    if let Value::Table(t) = e {
                        mark(t);
                    }
                }
            }
            ("objects" | "lights", Value::Array(items)) => {
                for e in items {
                    if let Value::Table(t) = e {
                        mark(t);
                    }
                }
            }
            _ => {}
        }
    }
}

/// Merges `over` into `base` (see the module docs).
fn merge(base: &mut Table, over: Table) {
    for (k, v) in over {
        let merged = match (base.remove(&k), v) {
            (Some(Value::Array(mut a)), Value::Array(b)) => {
                a.extend(b);
                Value::Array(a)
            }
            (Some(Value::Table(mut a)), Value::Table(b)) if k != "background" => {
                a.extend(b);
                Value::Table(a)
            }
            (_, v) => v,
        };
        base.insert(k, merged);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("pathtracer-include-{name}-{}", std::process::id()));
        std::fs::create_dir_all(d.join("lib")).unwrap();
        d
    }

    #[test]
    fn merges_includes_and_remembers_directories() {
        let d = temp_dir("merge");
        std::fs::write(
            d.join("lib/materials.toml"),
            "[render]\nspp = 16\nwidth = 100\n[materials.wood]\ntype = \"diffuse\"\nalbedo = \"wood.png\"\n\
             [materials.red]\nalbedo = [1, 0, 0]\n[media.smoke]\ngrid = \"smoke.vol\"\n[[objects]]\ntype = \"sphere\"\n",
        )
        .unwrap();
        std::fs::write(
            d.join("scene.toml"),
            "include = [\"lib/materials.toml\"]\n[render]\nspp = 64\n[materials.red]\nalbedo = [0.5, 0, 0]\n\
             [[objects]]\ntype = \"quad\"\n",
        )
        .unwrap();
        let mut files = Vec::new();
        let doc = load_doc(&d.join("scene.toml"), &mut files, &mut Vec::new()).unwrap();
        assert_eq!(files, [d.join("lib/materials.toml")]);
        let render = doc["render"].as_table().unwrap();
        assert_eq!((render["spp"].as_f64(), render["width"].as_f64()), (Some(64.0), Some(100.0)));
        let mats = doc["materials"].as_table().unwrap();
        let wood = mats["wood"].as_table().unwrap();
        assert_eq!(dir_of(wood, Path::new("x")), d.join("lib"));
        let smoke = doc["media"].as_table().unwrap()["smoke"].as_table().unwrap();
        assert_eq!(dir_of(smoke, Path::new("x")), d.join("lib"), "media grids resolve next to their file");
        // The scene's own definition replaces the library's, and keeps the scene directory.
        let red = mats["red"].as_table().unwrap();
        assert_eq!(red["albedo"].as_array().unwrap()[0].as_f64(), Some(0.5));
        assert!(!red.contains_key(DIR_KEY));
        let objs = doc["objects"].as_array().unwrap();
        assert_eq!(objs.len(), 2);
        assert_eq!(objs[0].as_table().unwrap()["type"].as_str(), Some("sphere"));
        std::fs::remove_dir_all(d).ok();
    }

    #[test]
    fn detects_cycles() {
        let d = temp_dir("cycle");
        std::fs::write(d.join("a.toml"), "include = \"b.toml\"\n").unwrap();
        std::fs::write(d.join("b.toml"), "include = \"a.toml\"\n").unwrap();
        let e = load_doc(&d.join("a.toml"), &mut Vec::new(), &mut Vec::new()).unwrap_err();
        assert!(e.contains("cycle"), "{e}");
        std::fs::remove_dir_all(d).ok();
    }
}
