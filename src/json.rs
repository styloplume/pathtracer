//! A small JSON parser (RFC 8259), enough for glTF: objects, arrays, strings with
//! escapes (including surrogate pairs), numbers, booleans and null.

use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Array(Vec<Json>),
    Object(BTreeMap<String, Json>),
}

static NULL: Json = Json::Null;

impl Json {
    /// Member `key` of an object; `Null` if absent or not an object.
    pub fn get(&self, key: &str) -> &Json {
        match self {
            Json::Object(m) => m.get(key).unwrap_or(&NULL),
            _ => &NULL,
        }
    }
    pub fn is_null(&self) -> bool {
        matches!(self, Json::Null)
    }
    pub fn as_f64(&self) -> Option<f64> {
        if let Json::Number(n) = self { Some(*n) } else { None }
    }
    pub fn as_f32(&self) -> Option<f32> {
        self.as_f64().map(|v| v as f32)
    }
    /// A non-negative integer that fits an index.
    pub fn as_usize(&self) -> Option<usize> {
        self.as_f64().filter(|v| *v >= 0.0 && v.fract() == 0.0 && *v <= 9.0e15).map(|v| v as usize)
    }
    pub fn as_str(&self) -> Option<&str> {
        if let Json::String(s) = self { Some(s) } else { None }
    }
    pub fn as_bool(&self) -> Option<bool> {
        if let Json::Bool(b) = self { Some(*b) } else { None }
    }
    pub fn as_array(&self) -> &[Json] {
        if let Json::Array(a) = self { a } else { &[] }
    }
    /// An array of exactly `N` numbers.
    pub fn as_f32s<const N: usize>(&self) -> Option<[f32; N]> {
        let a = self.as_array();
        if a.len() != N {
            return None;
        }
        let mut out = [0.0; N];
        for (o, v) in out.iter_mut().zip(a) {
            *o = v.as_f32()?;
        }
        Some(out)
    }
}

pub fn parse(src: &str) -> Result<Json, String> {
    let mut p = Parser { s: src.as_bytes(), pos: 0 };
    p.ws();
    let v = p.value(0)?;
    p.ws();
    if p.pos != p.s.len() {
        return p.err("trailing characters after the document");
    }
    Ok(v)
}

struct Parser<'a> {
    s: &'a [u8],
    pos: usize,
}

/// Nesting limit (keeps hostile files from overflowing the stack).
const MAX_DEPTH: usize = 256;

impl Parser<'_> {
    fn err<T>(&self, msg: &str) -> Result<T, String> {
        let line = self.s[..self.pos.min(self.s.len())].split(|&c| c == b'\n').count();
        Err(format!("JSON line {line}: {msg}"))
    }

    fn peek(&self) -> Option<u8> {
        self.s.get(self.pos).copied()
    }

    fn ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.pos += 1;
        }
    }

    fn literal(&mut self, word: &str, v: Json) -> Result<Json, String> {
        if self.s[self.pos..].starts_with(word.as_bytes()) {
            self.pos += word.len();
            Ok(v)
        } else {
            self.err("invalid literal")
        }
    }

    fn value(&mut self, depth: usize) -> Result<Json, String> {
        if depth > MAX_DEPTH {
            return self.err("nested too deeply");
        }
        match self.peek() {
            Some(b'{') => {
                self.pos += 1;
                let mut m = BTreeMap::new();
                self.ws();
                if self.peek() == Some(b'}') {
                    self.pos += 1;
                    return Ok(Json::Object(m));
                }
                loop {
                    self.ws();
                    if self.peek() != Some(b'"') {
                        return self.err("expected a member name");
                    }
                    let k = self.string()?;
                    self.ws();
                    if self.peek() != Some(b':') {
                        return self.err("expected ':'");
                    }
                    self.pos += 1;
                    self.ws();
                    let v = self.value(depth + 1)?;
                    m.insert(k, v);
                    self.ws();
                    match self.peek() {
                        Some(b',') => self.pos += 1,
                        Some(b'}') => {
                            self.pos += 1;
                            return Ok(Json::Object(m));
                        }
                        _ => return self.err("expected ',' or '}'"),
                    }
                }
            }
            Some(b'[') => {
                self.pos += 1;
                let mut a = Vec::new();
                self.ws();
                if self.peek() == Some(b']') {
                    self.pos += 1;
                    return Ok(Json::Array(a));
                }
                loop {
                    self.ws();
                    a.push(self.value(depth + 1)?);
                    self.ws();
                    match self.peek() {
                        Some(b',') => self.pos += 1,
                        Some(b']') => {
                            self.pos += 1;
                            return Ok(Json::Array(a));
                        }
                        _ => return self.err("expected ',' or ']'"),
                    }
                }
            }
            Some(b'"') => Ok(Json::String(self.string()?)),
            Some(b't') => self.literal("true", Json::Bool(true)),
            Some(b'f') => self.literal("false", Json::Bool(false)),
            Some(b'n') => self.literal("null", Json::Null),
            Some(b'-' | b'0'..=b'9') => {
                let start = self.pos;
                while matches!(self.peek(), Some(b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9')) {
                    self.pos += 1;
                }
                let tok = std::str::from_utf8(&self.s[start..self.pos]).unwrap_or("");
                match tok.parse::<f64>() {
                    Ok(n) => Ok(Json::Number(n)),
                    Err(_) => self.err(&format!("invalid number '{tok}'")),
                }
            }
            Some(_) => self.err("unexpected character"),
            None => self.err("unexpected end of input"),
        }
    }

    fn hex4(&mut self) -> Result<u32, String> {
        let h = self.s.get(self.pos..self.pos + 4).and_then(|h| std::str::from_utf8(h).ok());
        let v = h.and_then(|h| u32::from_str_radix(h, 16).ok());
        match v {
            Some(v) => {
                self.pos += 4;
                Ok(v)
            }
            None => self.err("invalid \\u escape"),
        }
    }

    fn string(&mut self) -> Result<String, String> {
        self.pos += 1; // opening quote
        let mut out = Vec::new();
        loop {
            let Some(c) = self.peek() else { return self.err("unterminated string") };
            self.pos += 1;
            match c {
                b'"' => return String::from_utf8(out).or_else(|_| self.err("invalid UTF-8 in string")),
                b'\\' => {
                    let Some(e) = self.peek() else { return self.err("unterminated string") };
                    self.pos += 1;
                    let ch = match e {
                        b'"' => '"',
                        b'\\' => '\\',
                        b'/' => '/',
                        b'b' => '\u{8}',
                        b'f' => '\u{c}',
                        b'n' => '\n',
                        b'r' => '\r',
                        b't' => '\t',
                        b'u' => {
                            let hi = self.hex4()?;
                            let code = if (0xD800..0xDC00).contains(&hi) && self.s[self.pos..].starts_with(b"\\u") {
                                self.pos += 2;
                                let lo = self.hex4()?;
                                0x10000 + ((hi - 0xD800) << 10) + (lo.wrapping_sub(0xDC00) & 0x3FF)
                            } else {
                                hi
                            };
                            char::from_u32(code).unwrap_or('\u{FFFD}')
                        }
                        _ => return self.err("invalid escape"),
                    };
                    let mut buf = [0u8; 4];
                    out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                }
                _ => out.push(c),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_gltf_like_document() {
        let doc = parse(
            r#" { "asset": {"version": "2.0"}, "nodes": [ {"mesh": 0, "translation": [1, -2.5e1, 3.0]} ],
                  "name": "a\"b\\cé😀", "flag": true, "none": null, "empty": [], "obj": {} } "#,
        )
        .unwrap();
        assert_eq!(doc.get("asset").get("version").as_str(), Some("2.0"));
        let n = &doc.get("nodes").as_array()[0];
        assert_eq!(n.get("mesh").as_usize(), Some(0));
        assert_eq!(n.get("translation").as_f32s::<3>(), Some([1.0, -25.0, 3.0]));
        assert_eq!(doc.get("name").as_str(), Some("a\"b\\c\u{e9}\u{1F600}"));
        assert_eq!(doc.get("flag").as_bool(), Some(true));
        assert!(doc.get("none").is_null() && doc.get("missing").is_null());
        assert_eq!(doc.get("empty").as_array(), []);
    }

    #[test]
    fn rejects_malformed_input() {
        for bad in ["{", "[1,]", "{\"a\" 1}", "tru", "\"abc", "1 2", "{\"a\":1,}"] {
            assert!(parse(bad).is_err(), "{bad}");
        }
        assert!(parse(&"[".repeat(1000)).is_err());
    }
}
