//! A small, dependency-free TOML reader and format-preserving editor for the commonly used
//! subset: tables `[a.b]`, arrays of tables `[[a]]`, dotted keys, basic/literal strings,
//! numbers, booleans, (multi-line) arrays and inline tables. Not supported: multi-line
//! strings and date/time values.
//!
//! [`parse`] gives plain values; [`Document`] keeps the text and where every value is in it,
//! so edits leave comments, order and formatting alone.

mod document;

pub use document::{Document, Entry, Node, NodeKind, Seg, Span, Splice, Style, TableNode, path_name};

use std::collections::BTreeMap;
use std::fmt::Write as _;

pub type Table = BTreeMap<String, Value>;

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    String(String),
    Number(f64),
    Bool(bool),
    Array(Vec<Value>),
    Table(Table),
}

impl Value {
    pub fn as_f64(&self) -> Option<f64> {
        if let Value::Number(n) = self { Some(*n) } else { None }
    }
    pub fn as_str(&self) -> Option<&str> {
        if let Value::String(s) = self { Some(s) } else { None }
    }
    pub fn as_bool(&self) -> Option<bool> {
        if let Value::Bool(b) = self { Some(*b) } else { None }
    }
    pub fn as_array(&self) -> Option<&[Value]> {
        if let Value::Array(a) = self { Some(a) } else { None }
    }
    pub fn as_table(&self) -> Option<&Table> {
        if let Value::Table(t) = self { Some(t) } else { None }
    }
    pub fn type_name(&self) -> &'static str {
        match self {
            Value::String(_) => "string",
            Value::Number(_) => "number",
            Value::Bool(_) => "boolean",
            Value::Array(_) => "array",
            Value::Table(_) => "table",
        }
    }
}

pub fn parse(src: &str) -> Result<Table, String> {
    Ok(Document::parse(src)?.to_table())
}

/// A key as written in TOML: bare if it can be, else quoted.
pub fn format_key(k: &str) -> String {
    if !k.is_empty() && k.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-') {
        k.to_owned()
    } else {
        format_string(k)
    }
}

fn format_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c if c.is_control() => {
                // Writing to a String cannot fail.
                let _ = write!(out, "\\u{:04X}", u32::from(c));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Shortest text that reads back as exactly `n`; integers without a fraction.
pub fn format_number(n: f64) -> String {
    if n.is_nan() {
        "nan".into()
    } else if n.is_infinite() {
        if n > 0.0 { "inf".into() } else { "-inf".into() }
    } else if n.fract() == 0.0 && n.abs() < 1e15 {
        format!("{}", n as i64)
    } else {
        format!("{n}")
    }
}

/// A value as inline TOML: `1.5`, `"text"`, `[1, 2, 3]`, `{ type = "metal", roughness = 0.1 }`
/// (tables list `type` first).
pub fn format_value(v: &Value) -> String {
    match v {
        Value::String(s) => format_string(s),
        Value::Number(n) => format_number(*n),
        Value::Bool(b) => b.to_string(),
        Value::Array(a) => format!("[{}]", a.iter().map(format_value).collect::<Vec<_>>().join(", ")),
        Value::Table(t) if t.is_empty() => "{}".into(),
        Value::Table(t) => {
            let mut items: Vec<(&String, &Value)> = t.iter().collect();
            items.sort_by_key(|(k, _)| *k != "type");
            let items: Vec<String> =
                items.iter().map(|(k, v)| format!("{} = {}", format_key(k), format_value(v))).collect();
            format!("{{ {} }}", items.join(", "))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_scene_like_document() {
        let src = r#"
# comment
title = "demo" # trailing
[render]
width = 1_280
exposure = -0.5
enabled = true

[materials.red]
albedo = [0.8, 0.1,
          0.1,]  # multi-line

[[objects]]
type = 'sphere'
material = { type = "glass", ior = 1.5 }

[[objects]]
type = "mesh"
pos.x = 1
"#;
        let t = parse(src).unwrap();
        assert_eq!(t["title"].as_str(), Some("demo"));
        let r = t["render"].as_table().unwrap();
        assert_eq!(r["width"].as_f64(), Some(1280.0));
        assert_eq!(r["exposure"].as_f64(), Some(-0.5));
        assert_eq!(r["enabled"].as_bool(), Some(true));
        let red = t["materials"].as_table().unwrap()["red"].as_table().unwrap();
        assert_eq!(red["albedo"].as_array().unwrap().len(), 3);
        let objs = t["objects"].as_array().unwrap();
        assert_eq!(objs.len(), 2);
        let m = objs[0].as_table().unwrap()["material"].as_table().unwrap();
        assert_eq!(m["ior"].as_f64(), Some(1.5));
        assert_eq!(objs[1].as_table().unwrap()["pos"].as_table().unwrap()["x"].as_f64(), Some(1.0));
    }

    #[test]
    fn reports_errors_with_line_numbers() {
        let e = parse("a = 1\nb = oops\n").unwrap_err();
        assert!(e.starts_with("line 2"), "{e}");
        assert!(parse("a = 1\na = 2").is_err());
    }

    #[test]
    fn formats_values_that_read_back() {
        let mut t = Table::new();
        t.insert("x y".into(), Value::Number(0.1));
        t.insert("type".into(), Value::String("a\"b\\c\n\u{1}".into()));
        t.insert("n".into(), Value::Array(vec![Value::Number(1e20), Value::Number(-3.0), Value::Number(1e-7)]));
        t.insert("i".into(), Value::Number(f64::INFINITY));
        let v = Value::Table(t);
        let text = format_value(&v);
        assert!(text.starts_with("{ type = "), "{text}");
        assert_eq!(parse(&format!("v = {text}")).unwrap()["v"], v);
        assert_eq!(format_number(2.0), "2");
        assert_eq!(format_number(0.3), "0.3");
    }
}
