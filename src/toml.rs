//! A small, dependency-free parser for the commonly used subset of TOML:
//! tables `[a.b]`, arrays of tables `[[a]]`, dotted keys, basic/literal strings,
//! numbers, booleans, (multi-line) arrays and inline tables. Not supported:
//! multi-line strings and date/time values.

use std::collections::BTreeMap;

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
    let mut p = Parser { chars: src.chars().collect(), pos: 0, line: 1 };
    p.document()
}

struct Parser {
    chars: Vec<char>,
    pos: usize,
    line: usize,
}

impl Parser {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.pos += 1;
        if c == '\n' {
            self.line += 1;
        }
        Some(c)
    }

    fn err<T>(&self, msg: impl std::fmt::Display) -> Result<T, String> {
        Err(format!("line {}: {}", self.line, msg))
    }

    fn expect(&mut self, c: char) -> Result<(), String> {
        match self.bump() {
            Some(x) if x == c => Ok(()),
            Some(x) => self.err(format!("expected '{c}', found '{x}'")),
            None => self.err(format!("expected '{c}', found end of file")),
        }
    }

    /// Skips spaces and tabs (not newlines).
    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(' ' | '\t')) {
            self.pos += 1;
        }
    }

    fn skip_comment(&mut self) {
        if self.peek() == Some('#') {
            while !matches!(self.peek(), None | Some('\n')) {
                self.pos += 1;
            }
        }
    }

    fn skip_blank(&mut self) {
        loop {
            self.skip_ws();
            self.skip_comment();
            match self.peek() {
                Some('\n' | '\r') => {
                    self.bump();
                }
                _ => return,
            }
        }
    }

    fn end_of_line(&mut self) -> Result<(), String> {
        self.skip_ws();
        self.skip_comment();
        if self.peek() == Some('\r') {
            self.bump();
        }
        match self.peek() {
            None => Ok(()),
            Some('\n') => {
                self.bump();
                Ok(())
            }
            Some(c) => self.err(format!("unexpected '{c}' after value")),
        }
    }

    fn document(&mut self) -> Result<Table, String> {
        let mut root = Table::new();
        let mut current: Vec<String> = Vec::new();
        loop {
            self.skip_blank();
            match self.peek() {
                None => return Ok(root),
                Some('[') => {
                    self.bump();
                    let is_array = self.peek() == Some('[');
                    if is_array {
                        self.bump();
                    }
                    self.skip_ws();
                    let path = self.key_path()?;
                    self.skip_ws();
                    self.expect(']')?;
                    if is_array {
                        self.expect(']')?;
                    }
                    self.end_of_line()?;
                    let res =
                        if is_array { push_array_table(&mut root, &path) } else { walk(&mut root, &path).map(|_| ()) };
                    if let Err(e) = res {
                        return self.err(e);
                    }
                    current = path;
                }
                Some(_) => {
                    let path = self.key_path()?;
                    self.skip_ws();
                    self.expect('=')?;
                    self.skip_ws();
                    let value = self.value()?;
                    self.end_of_line()?;
                    let res = walk(&mut root, &current).and_then(|t| insert(t, &path, value));
                    if let Err(e) = res {
                        return self.err(e);
                    }
                }
            }
        }
    }

    fn key_path(&mut self) -> Result<Vec<String>, String> {
        let mut path = Vec::new();
        loop {
            self.skip_ws();
            path.push(self.key()?);
            self.skip_ws();
            if self.peek() == Some('.') {
                self.bump();
            } else {
                return Ok(path);
            }
        }
    }

    fn key(&mut self) -> Result<String, String> {
        match self.peek() {
            Some('"') => self.basic_string(),
            Some('\'') => self.literal_string(),
            _ => {
                let start = self.pos;
                while matches!(self.peek(), Some(c) if c.is_ascii_alphanumeric() || c == '_' || c == '-') {
                    self.pos += 1;
                }
                if start == self.pos {
                    return match self.peek() {
                        Some(c) => self.err(format!("expected a key, found '{c}'")),
                        None => self.err("expected a key, found end of file"),
                    };
                }
                Ok(self.chars[start..self.pos].iter().collect())
            }
        }
    }

    fn value(&mut self) -> Result<Value, String> {
        match self.peek() {
            Some('"') => {
                if self.chars.get(self.pos..self.pos + 3) == Some(&['"', '"', '"']) {
                    return self.err("multi-line strings are not supported");
                }
                Ok(Value::String(self.basic_string()?))
            }
            Some('\'') => Ok(Value::String(self.literal_string()?)),
            Some('[') => self.array(),
            Some('{') => self.inline_table(),
            Some(_) => self.scalar(),
            None => self.err("expected a value, found end of file"),
        }
    }

    fn basic_string(&mut self) -> Result<String, String> {
        self.expect('"')?;
        let mut s = String::new();
        loop {
            match self.bump() {
                None | Some('\n') => return self.err("unterminated string"),
                Some('"') => return Ok(s),
                Some('\\') => {
                    let c = match self.bump() {
                        Some('n') => '\n',
                        Some('t') => '\t',
                        Some('r') => '\r',
                        Some('b') => '\u{8}',
                        Some('f') => '\u{c}',
                        Some('"') => '"',
                        Some('\\') => '\\',
                        Some(u @ ('u' | 'U')) => {
                            let n = if u == 'u' { 4 } else { 8 };
                            let hex: String = (0..n).filter_map(|_| self.bump()).collect();
                            match u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                                Some(c) => c,
                                None => return self.err(format!("invalid unicode escape '\\{u}{hex}'")),
                            }
                        }
                        Some(c) => return self.err(format!("invalid escape '\\{c}'")),
                        None => return self.err("unterminated string"),
                    };
                    s.push(c);
                }
                Some(c) => s.push(c),
            }
        }
    }

    fn literal_string(&mut self) -> Result<String, String> {
        self.expect('\'')?;
        let mut s = String::new();
        loop {
            match self.bump() {
                None | Some('\n') => return self.err("unterminated string"),
                Some('\'') => return Ok(s),
                Some(c) => s.push(c),
            }
        }
    }

    fn array(&mut self) -> Result<Value, String> {
        self.expect('[')?;
        let mut items = Vec::new();
        loop {
            self.skip_blank();
            if self.peek() == Some(']') {
                self.bump();
                return Ok(Value::Array(items));
            }
            items.push(self.value()?);
            self.skip_blank();
            match self.bump() {
                Some(',') => {}
                Some(']') => return Ok(Value::Array(items)),
                Some(c) => return self.err(format!("expected ',' or ']' in array, found '{c}'")),
                None => return self.err("unterminated array"),
            }
        }
    }

    fn inline_table(&mut self) -> Result<Value, String> {
        self.expect('{')?;
        let mut t = Table::new();
        loop {
            self.skip_blank();
            if self.peek() == Some('}') {
                self.bump();
                return Ok(Value::Table(t));
            }
            let path = self.key_path()?;
            self.skip_ws();
            self.expect('=')?;
            self.skip_ws();
            let v = self.value()?;
            if let Err(e) = insert(&mut t, &path, v) {
                return self.err(e);
            }
            self.skip_blank();
            match self.bump() {
                Some(',') => {}
                Some('}') => return Ok(Value::Table(t)),
                Some(c) => return self.err(format!("expected ',' or '}}' in inline table, found '{c}'")),
                None => return self.err("unterminated inline table"),
            }
        }
    }

    fn scalar(&mut self) -> Result<Value, String> {
        let start = self.pos;
        while matches!(self.peek(), Some(c) if c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.' | '_')) {
            self.pos += 1;
        }
        let tok: String = self.chars[start..self.pos].iter().filter(|&&c| c != '_').collect();
        match tok.as_str() {
            "true" => return Ok(Value::Bool(true)),
            "false" => return Ok(Value::Bool(false)),
            "inf" | "+inf" => return Ok(Value::Number(f64::INFINITY)),
            "-inf" => return Ok(Value::Number(f64::NEG_INFINITY)),
            "nan" | "+nan" | "-nan" => return Ok(Value::Number(f64::NAN)),
            "" => {
                return match self.peek() {
                    Some(c) => self.err(format!("unexpected '{c}'")),
                    None => self.err("expected a value"),
                };
            }
            _ => {}
        }
        match tok.parse::<f64>() {
            Ok(n) => Ok(Value::Number(n)),
            Err(_) => self.err(format!("invalid value '{tok}' (strings must be quoted)")),
        }
    }
}

/// Walks (creating as needed) to the table at `path`. Arrays of tables resolve to their last element.
fn walk<'a>(mut t: &'a mut Table, path: &[String]) -> Result<&'a mut Table, String> {
    for key in path {
        let entry = t.entry(key.clone()).or_insert_with(|| Value::Table(Table::new()));
        t = match entry {
            Value::Table(inner) => inner,
            Value::Array(arr) => match arr.last_mut() {
                Some(Value::Table(inner)) => inner,
                _ => return Err(format!("'{key}' is not a table")),
            },
            other => return Err(format!("'{key}' is a {}, not a table", other.type_name())),
        };
    }
    Ok(t)
}

fn push_array_table(root: &mut Table, path: &[String]) -> Result<(), String> {
    let (last, parent) = path.split_last().expect("key path is never empty");
    let parent = walk(root, parent)?;
    match parent.entry(last.clone()).or_insert_with(|| Value::Array(Vec::new())) {
        Value::Array(arr) => {
            arr.push(Value::Table(Table::new()));
            Ok(())
        }
        other => Err(format!("'{last}' is a {}, not an array of tables", other.type_name())),
    }
}

fn insert(t: &mut Table, path: &[String], value: Value) -> Result<(), String> {
    let (last, parent) = path.split_last().expect("key path is never empty");
    let parent = walk(t, parent)?;
    if parent.contains_key(last) {
        return Err(format!("duplicate key '{last}'"));
    }
    parent.insert(last.clone(), value);
    Ok(())
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
}
