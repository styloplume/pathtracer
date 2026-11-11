//! Per-user settings: theme, font, panel layout and recent files, kept in a TOML file in
//! the user's configuration folder (`%APPDATA%\ptedit`, `~/Library/Application Support/ptedit`,
//! `$XDG_CONFIG_HOME/ptedit` or `~/.config/ptedit`; `PTEDIT_SETTINGS` names another file).
//! The file is edited in place like a scene, so comments written into it stay.

use pt_core::toml::{Document, Seg, Value};
use std::path::{Path, PathBuf};

pub const MAX_RECENT: usize = 10;

#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    /// "dark" or "light".
    pub theme: String,
    /// A font file, "built-in", or empty for a system font.
    pub font: String,
    /// Text size in 96-dpi pixels.
    pub font_size: f32,
    pub outliner: bool,
    pub inspector: bool,
    /// Panel widths in 96-dpi pixels.
    pub outliner_width: f32,
    pub inspector_width: f32,
    pub quad: bool,
    /// Most recent first.
    pub recent: Vec<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            theme: "dark".into(),
            font: String::new(),
            font_size: 13.0,
            outliner: true,
            inspector: true,
            outliner_width: 240.0,
            inspector_width: 330.0,
            quad: false,
            recent: Vec::new(),
        }
    }
}

/// Where the settings live.
pub fn path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("PTEDIT_SETTINGS") {
        return Some(PathBuf::from(p));
    }
    let env = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
    let dir = if cfg!(windows) {
        env("APPDATA")?
    } else if cfg!(target_os = "macos") {
        env("HOME")?.join("Library/Application Support")
    } else {
        env("XDG_CONFIG_HOME").or_else(|| env("HOME").map(|h| h.join(".config")))?
    };
    Some(dir.join("ptedit").join("settings.toml"))
}

impl Settings {
    /// Reads the settings file (missing or unreadable values keep their defaults).
    pub fn load(path: Option<&Path>) -> Settings {
        let mut s = Settings::default();
        let Some(text) = path.and_then(|p| std::fs::read_to_string(p).ok()) else { return s };
        let Ok(doc) = Document::parse(text) else {
            eprintln!("warning: cannot read the settings file; using defaults");
            return s;
        };
        let t = doc.to_table();
        let num = |k: &str| t.get(k).and_then(Value::as_f64).map(|v| v as f32);
        let flag = |k: &str| t.get(k).and_then(Value::as_bool);
        if let Some(v) = t.get("theme").and_then(Value::as_str) {
            v.clone_into(&mut s.theme);
        }
        if let Some(v) = t.get("font").and_then(Value::as_str) {
            v.clone_into(&mut s.font);
        }
        s.font_size = num("font_size").filter(|v| (6.0..=48.0).contains(v)).unwrap_or(s.font_size);
        s.outliner = flag("outliner").unwrap_or(s.outliner);
        s.inspector = flag("inspector").unwrap_or(s.inspector);
        s.outliner_width = num("outliner_width").filter(|v| *v >= 80.0).unwrap_or(s.outliner_width);
        s.inspector_width = num("inspector_width").filter(|v| *v >= 120.0).unwrap_or(s.inspector_width);
        s.quad = flag("quad").unwrap_or(s.quad);
        if let Some(r) = t.get("recent").and_then(Value::as_array) {
            s.recent = r.iter().filter_map(Value::as_str).map(str::to_owned).take(MAX_RECENT).collect();
        }
        s
    }

    /// Writes the settings, changing only the values that differ from the file's.
    pub fn save(&self, path: &Path) -> Result<(), String> {
        let text = std::fs::read_to_string(path).unwrap_or_default();
        let mut doc = Document::parse(text.clone())
            .or_else(|_| Document::parse(String::new()))
            .map_err(|e| format!("settings: {e}"))?;
        if text.is_empty() {
            doc = Document::parse("# ptedit settings (written by the editor; comments are kept)\n")?;
        }
        let entries: [(&str, Value); 9] = [
            ("theme", Value::String(self.theme.clone())),
            ("font", Value::String(self.font.clone())),
            ("font_size", Value::Number(f64::from(self.font_size))),
            ("outliner", Value::Bool(self.outliner)),
            ("inspector", Value::Bool(self.inspector)),
            ("outliner_width", Value::Number(f64::from(self.outliner_width.round()))),
            ("inspector_width", Value::Number(f64::from(self.inspector_width.round()))),
            ("quad", Value::Bool(self.quad)),
            ("recent", Value::Array(self.recent.iter().cloned().map(Value::String).collect())),
        ];
        for (k, v) in entries {
            doc.set(&[Seg::from(k)], &v)?;
        }
        if doc.text() == text {
            return Ok(());
        }
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("cannot create '{}': {e}", dir.display()))?;
        }
        std::fs::write(path, doc.text()).map_err(|e| format!("cannot write '{}': {e}", path.display()))
    }

    /// Puts `file` first in the recent files.
    pub fn add_recent(&mut self, file: &Path) {
        let s = pt_core::loader::canonical(file).display().to_string();
        // Windows' canonical paths carry a verbatim prefix that nobody wants to read.
        let s = s.strip_prefix(r"\\?\").map(str::to_owned).unwrap_or(s);
        self.recent.retain(|r| *r != s);
        self.recent.insert(0, s);
        self.recent.truncate(MAX_RECENT);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_round_trip_and_keep_comments() {
        let p = std::env::temp_dir().join(format!("ptedit-settings-{}.toml", std::process::id()));
        std::fs::write(&p, "# mine\ntheme = \"light\"   # I like it\nfont_size = 15\nunknown = 1\n").unwrap();
        let mut s = Settings::load(Some(&p));
        assert_eq!((s.theme.as_str(), s.font_size, s.outliner), ("light", 15.0, true));
        s.outliner_width = 300.0;
        s.add_recent(&p);
        s.save(&p).unwrap();
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(text.starts_with("# mine\ntheme = \"light\"   # I like it\nfont_size = 15\nunknown = 1\n"), "{text}");
        let back = Settings::load(Some(&p));
        assert_eq!(back, s);
        assert_eq!(back.recent.len(), 1);
        std::fs::remove_file(&p).ok();
        assert_eq!(Settings::load(Some(&p)), Settings::default());
    }
}
