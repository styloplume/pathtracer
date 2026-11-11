//! Format-preserving TOML: a parsed document keeps its source text, and every value,
//! table and entry in the tree records where it is in that text. Edits are splices of the
//! text, so whatever an edit does not touch (comments, blank lines, key order, number
//! formatting, quoting) stays as it was, and an unedited document saves byte for byte.
//!
//! After every edit the text is parsed again, which keeps the tree's spans correct and
//! guarantees that an edit can never produce a file the parser rejects.

use super::{Table, Value, format_key, format_value};
use std::fmt;
use std::fmt::Write as _;
use std::ops::Range;

/// A byte range of the document text.
pub type Span = Range<usize>;

/// One step of a path into a document: a table key or an array index.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Seg {
    Key(String),
    Index(usize),
}

impl From<&str> for Seg {
    fn from(k: &str) -> Seg {
        Seg::Key(k.to_owned())
    }
}

impl From<String> for Seg {
    fn from(k: String) -> Seg {
        Seg::Key(k)
    }
}

impl From<usize> for Seg {
    fn from(i: usize) -> Seg {
        Seg::Index(i)
    }
}

impl fmt::Display for Seg {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Seg::Key(k) => f.write_str(k),
            Seg::Index(i) => write!(f, "#{}", i + 1),
        }
    }
}

/// A path for messages: `objects #3 > radius`.
pub fn path_name(path: &[Seg]) -> String {
    if path.is_empty() {
        return "the document root".into();
    }
    path.iter().map(ToString::to_string).collect::<Vec<_>>().join(" > ")
}

/// A value in the document, with where it is written.
#[derive(Clone, Debug)]
pub struct Node {
    /// The value's text: a scalar, `[...]` or `{...}`. For a table opened by a header it is
    /// the header line; for the root and implicitly created tables it is empty.
    pub span: Span,
    pub kind: NodeKind,
}

#[derive(Clone, Debug)]
pub enum NodeKind {
    /// A string, number or boolean.
    Scalar(Value),
    /// An inline array `[...]`.
    Array(Vec<Node>),
    Table(TableNode),
    /// An array of tables written as `[[name]]` sections.
    TableArray(Vec<Node>),
}

#[derive(Clone, Debug)]
pub struct TableNode {
    pub style: Style,
    /// In source order.
    pub entries: Vec<Entry>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Style {
    /// The document itself. New keys go at `insert_at`.
    Root { insert_at: usize },
    /// A `[name]` section or a `[[name]]` element. `block` runs from the comment lines
    /// directly above the header to the next section; new keys go at `insert_at`, after the
    /// last key of the section.
    Header { block: Span, insert_at: usize },
    /// Created by a dotted key or as the parent of a section (`[a.b]` creates `a`).
    Implicit,
    /// `{ ... }`.
    Inline,
}

#[derive(Clone, Debug)]
pub struct Entry {
    pub key: String,
    pub node: Node,
    /// The whole `key = value` line(s), from the line start through the line break, for
    /// entries written as a line of a section (or of the root).
    pub line: Option<Span>,
    /// `key = value`, without surrounding whitespace or commas.
    pub kv: Span,
}

impl Node {
    /// The plain value of this node.
    pub fn value(&self) -> Value {
        match &self.kind {
            NodeKind::Scalar(v) => v.clone(),
            NodeKind::Array(items) | NodeKind::TableArray(items) => {
                Value::Array(items.iter().map(Node::value).collect())
            }
            NodeKind::Table(t) => Value::Table(t.to_table()),
        }
    }

    pub fn as_table(&self) -> Option<&TableNode> {
        if let NodeKind::Table(t) = &self.kind { Some(t) } else { None }
    }

    /// The child at `seg`: a table entry or an array element.
    pub fn child(&self, seg: &Seg) -> Option<&Node> {
        match (&self.kind, seg) {
            (NodeKind::Table(t), Seg::Key(k)) => t.get(k),
            (NodeKind::Array(items) | NodeKind::TableArray(items), Seg::Index(i)) => items.get(*i),
            _ => None,
        }
    }

    fn child_mut(&mut self, seg: &Seg) -> Option<&mut Node> {
        match (&mut self.kind, seg) {
            (NodeKind::Table(t), Seg::Key(k)) => t.entries.iter_mut().find(|e| e.key == *k).map(|e| &mut e.node),
            (NodeKind::Array(items) | NodeKind::TableArray(items), Seg::Index(i)) => items.get_mut(*i),
            _ => None,
        }
    }

    pub fn type_name(&self) -> &'static str {
        match &self.kind {
            NodeKind::Scalar(v) => v.type_name(),
            NodeKind::Array(_) | NodeKind::TableArray(_) => "array",
            NodeKind::Table(_) => "table",
        }
    }
}

impl TableNode {
    fn new(style: Style) -> TableNode {
        TableNode { style, entries: Vec::new() }
    }

    pub fn get(&self, key: &str) -> Option<&Node> {
        self.entry(key).map(|e| &e.node)
    }

    pub fn entry(&self, key: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| e.key == key)
    }

    pub fn to_table(&self) -> Table {
        self.entries.iter().map(|e| (e.key.clone(), e.node.value())).collect()
    }
}

/// A text replacement: `old` at byte `start` became `new`. Undoing swaps the two.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Splice {
    pub start: usize,
    pub old: String,
    pub new: String,
}

impl Splice {
    /// The smallest splice that turns `a` into `b`.
    pub fn diff(a: &str, b: &str) -> Splice {
        let mut p = a.bytes().zip(b.bytes()).take_while(|(x, y)| x == y).count();
        while !a.is_char_boundary(p) || !b.is_char_boundary(p) {
            p -= 1;
        }
        let max_suffix = a.len().min(b.len()) - p;
        let mut s = a.bytes().rev().zip(b.bytes().rev()).take(max_suffix).take_while(|(x, y)| x == y).count();
        while !a.is_char_boundary(a.len() - s) || !b.is_char_boundary(b.len() - s) {
            s -= 1;
        }
        Splice { start: p, old: a[p..a.len() - s].to_owned(), new: b[p..b.len() - s].to_owned() }
    }

    pub fn inverse(&self) -> Splice {
        Splice { start: self.start, old: self.new.clone(), new: self.old.clone() }
    }
}

/// A parsed TOML document that can be edited without disturbing its formatting.
#[derive(Clone, Debug)]
pub struct Document {
    text: String,
    root: Node,
}

impl Document {
    pub fn parse(text: impl Into<String>) -> Result<Document, String> {
        let text = text.into();
        let root = Parser::new(&text).document()?;
        Ok(Document { text, root })
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn root(&self) -> &Node {
        &self.root
    }

    pub fn get(&self, path: &[Seg]) -> Option<&Node> {
        path.iter().try_fold(&self.root, |n, seg| n.child(seg))
    }

    /// The plain value at `path`.
    pub fn value(&self, path: &[Seg]) -> Option<Value> {
        self.get(path).map(Node::value)
    }

    /// The whole document as plain values.
    pub fn to_table(&self) -> Table {
        match &self.root.kind {
            NodeKind::Table(t) => t.to_table(),
            _ => unreachable!("the root is a table"),
        }
    }

    /// The line break the file uses.
    fn newline(&self) -> &'static str {
        if self.text.contains("\r\n") { "\r\n" } else { "\n" }
    }

    /// Applies `s` (whose `old` text must be at `s.start`).
    pub fn apply(&mut self, s: &Splice) -> Result<(), String> {
        let end = s.start + s.old.len();
        if self.text.get(s.start..end) != Some(s.old.as_str()) {
            return Err("the document changed since this edit was made".into());
        }
        let mut text = self.text.clone();
        text.replace_range(s.start..end, &s.new);
        self.replace_text(text).map(|_| ())
    }

    /// Replaces the whole text (which must parse); returns the splice it amounts to.
    pub fn replace_text(&mut self, text: String) -> Result<Option<Splice>, String> {
        if text == self.text {
            return Ok(None);
        }
        let root = Parser::new(&text).document().map_err(|e| format!("the edit would make the file invalid: {e}"))?;
        let splice = Splice::diff(&self.text, &text);
        self.text = text;
        self.root = root;
        Ok(Some(splice))
    }

    fn splice(&mut self, edits: Vec<(Span, String)>) -> Result<Option<Splice>, String> {
        let mut text = self.text.clone();
        let mut edits = edits;
        edits.sort_by_key(|(r, _)| std::cmp::Reverse(r.start));
        for (r, s) in edits {
            text.replace_range(r, &s);
        }
        self.replace_text(text)
    }

    /// Sets the value at `path`: an existing value is rewritten in place (unless it is already
    /// equal, which keeps its formatting); a new key is added after the last key of its table
    /// (or as a new `[section]`, for a table in a table of sections).
    pub fn set(&mut self, path: &[Seg], value: &Value) -> Result<Option<Splice>, String> {
        let (last, parent_path) = path.split_last().ok_or("cannot replace the whole document")?;
        let parent = self.get(parent_path).ok_or_else(|| format!("'{}' does not exist", path_name(parent_path)))?;
        if let Some(node) = parent.child(last) {
            if node.value() == *value {
                return Ok(None);
            }
            return match &node.kind {
                NodeKind::Scalar(_) | NodeKind::Array(_) => self.splice(vec![(node.span.clone(), format_value(value))]),
                NodeKind::Table(t) if t.style == Style::Inline => {
                    self.splice(vec![(node.span.clone(), format_value(value))])
                }
                _ => Err(format!("'{}' is written as sections; set its keys one at a time", path_name(path))),
            };
        }
        let nl = self.newline();
        let outer = parent.span.clone();
        match (&parent.kind, last) {
            (NodeKind::Table(t), Seg::Key(key)) => match &t.style {
                Style::Root { .. } | Style::Implicit if matches!(value, Value::Table(_)) => {
                    let entries = match value {
                        Value::Table(t) => ordered(t),
                        _ => unreachable!(),
                    };
                    let header = format!("[{}]", header_path(&self.root, path)?);
                    self.insert_section(parent_path, &header, &entries)
                }
                Style::Root { insert_at } | Style::Header { insert_at, .. } => {
                    let line = format!("{} = {}", format_key(key), format_value(value));
                    let at = *insert_at;
                    let before_header = matches!(t.style, Style::Root { .. })
                        && t.entries.iter().all(|e| e.line.is_none())
                        && at < self.text.len();
                    let indent = self.indent_before(at);
                    let mut s = format!("{indent}{line}{nl}");
                    if before_header {
                        s.push_str(nl);
                    }
                    self.insert_line(at, &s)
                }
                Style::Inline => {
                    let item = format!("{} = {}", format_key(key), format_value(value));
                    self.append_item(&outer, t.entries.last().map(|e| e.kv.end), &item)
                }
                Style::Implicit => {
                    Err(format!("cannot add '{key}' to '{}' (a dotted-key table)", path_name(parent_path)))
                }
            },
            (NodeKind::Array(items), Seg::Index(i)) if *i == items.len() => {
                self.append_item(&outer, items.last().map(|n| n.span.end), &format_value(value))
            }
            (NodeKind::TableArray(items), Seg::Index(i)) if *i == items.len() => match value {
                Value::Table(t) => self.push_table(parent_path, &ordered(t)),
                _ => Err(format!("'{}' holds tables", path_name(parent_path))),
            },
            _ => Err(format!("'{}' does not exist", path_name(path))),
        }
    }

    /// Appends a table to the array of tables at `path`: a new `[[path]]` section after the
    /// last one (or an inline table, if the array is written inline). Creates the array if
    /// it does not exist.
    pub fn push_table(&mut self, path: &[Seg], entries: &[(String, Value)]) -> Result<Option<Splice>, String> {
        let (_, parent_path) = path.split_last().ok_or("cannot replace the whole document")?;
        match self.get(path).map(|n| &n.kind) {
            None => {
                let header = format!("[[{}]]", header_path(&self.root, path)?);
                self.insert_section(parent_path, &header, entries)
            }
            Some(NodeKind::TableArray(items)) => {
                let last = items.last().ok_or("empty array of tables")?;
                let end = blocks_end(last).unwrap_or(self.text.len());
                let header = format!("[[{}]]", header_path(&self.root, path)?);
                self.insert_block(end, &header, entries)
            }
            Some(NodeKind::Array(items)) => {
                let table: Table = entries.iter().cloned().collect();
                let last = items.last().map(|n| n.span.end);
                let span = self.get(path).map(|n| n.span.clone()).unwrap_or_default();
                self.append_item(&span, last, &format_value(&Value::Table(table)))
            }
            Some(_) => Err(format!("'{}' is not an array of tables", path_name(path))),
        }
    }

    /// Removes the value at `path`: its line, its sections (with their sub-sections), or,
    /// inside an inline array or table, the item and its comma.
    pub fn remove(&mut self, path: &[Seg]) -> Result<Option<Splice>, String> {
        let (last, parent_path) = path.split_last().ok_or("cannot remove the whole document")?;
        let parent = self.get(parent_path).ok_or_else(|| format!("'{}' does not exist", path_name(parent_path)))?;
        let missing = || format!("'{}' does not exist", path_name(path));
        let items: Vec<Span> = match (&parent.kind, last) {
            (NodeKind::Table(t), Seg::Key(k)) if t.style == Style::Inline => {
                let i = t.entries.iter().position(|e| e.key == *k).ok_or_else(missing)?;
                if t.entries[i].node.as_table().is_some_and(|t| t.style == Style::Implicit) {
                    return Err(format!(
                        "'{}' is a dotted key inside an inline table; edit it by hand",
                        path_name(path)
                    ));
                }
                let spans: Vec<Span> = t.entries.iter().map(|e| e.kv.clone()).collect();
                return self.splice(vec![(list_item_range(&self.text, &spans, i), String::new())]);
            }
            (NodeKind::Array(items), Seg::Index(i)) => {
                if *i >= items.len() {
                    return Err(missing());
                }
                let spans: Vec<Span> = items.iter().map(|n| n.span.clone()).collect();
                return self.splice(vec![(list_item_range(&self.text, &spans, *i), String::new())]);
            }
            (NodeKind::Table(t), Seg::Key(k)) => {
                let e = t.entry(k).ok_or_else(missing)?;
                let mut r = Vec::new();
                r.extend(e.line.clone());
                ranges(&e.node, &mut r);
                r
            }
            (NodeKind::TableArray(items), Seg::Index(i)) => {
                let mut r = Vec::new();
                ranges(items.get(*i).ok_or_else(missing)?, &mut r);
                r
            }
            _ => return Err(missing()),
        };
        let mut merged: Vec<Span> = Vec::new();
        let mut items = items;
        items.sort_by_key(|r| r.start);
        for r in items {
            match merged.last_mut() {
                Some(m) if r.start <= m.end => m.end = m.end.max(r.end),
                _ => merged.push(r),
            }
        }
        // A section removed from the end of the file takes the blank lines before it along.
        if let Some(m) = merged.last_mut()
            && m.end == self.text.len()
        {
            let keep = self.text[..m.start].trim_end().len();
            if let Some(nl) = self.text[keep..m.start].find('\n') {
                m.start = keep + nl + 1;
            }
        }
        self.splice(merged.into_iter().map(|r| (r, String::new())).collect())
    }

    /// Whitespace that starts the line containing the byte before `at` (the indentation of
    /// the previous entry).
    fn indent_before(&self, at: usize) -> String {
        let before = &self.text[..at];
        let body = before.strip_suffix('\n').unwrap_or(before);
        let start = body.rfind('\n').map_or(0, |i| i + 1);
        let line = &body[start..];
        if line.trim_start().starts_with('[') {
            return String::new();
        }
        line[..line.len() - line.trim_start().len()].to_owned()
    }

    fn insert_line(&mut self, at: usize, s: &str) -> Result<Option<Splice>, String> {
        let mut s = s.to_owned();
        if at > 0 && !self.text[..at].ends_with('\n') {
            s.insert_str(0, self.newline());
        }
        self.splice(vec![(at..at, s)])
    }

    /// Adds `item` after the last item (which ends at `last_end`) of the inline array or table
    /// spanning `outer`.
    fn append_item(&mut self, outer: &Span, last_end: Option<usize>, item: &str) -> Result<Option<Splice>, String> {
        if let Some(end) = last_end {
            return self.splice(vec![(end..end, format!(", {item}"))]);
        }
        let open = &self.text[outer.clone()];
        let s = if open.starts_with('{') { format!("{{ {item} }}") } else { format!("[{item}]") };
        self.splice(vec![(outer.clone(), s)])
    }

    /// Inserts a new section `header` among the sections of table `parent_path`: after the
    /// last of them, or at the end of the file.
    fn insert_section(
        &mut self,
        parent_path: &[Seg],
        header: &str,
        entries: &[(String, Value)],
    ) -> Result<Option<Splice>, String> {
        let at = self.get(parent_path).and_then(blocks_end).unwrap_or(self.text.len());
        self.insert_block(at, header, entries)
    }

    /// Writes a section at `at`, separated from its neighbours by blank lines.
    fn insert_block(&mut self, at: usize, header: &str, entries: &[(String, Value)]) -> Result<Option<Splice>, String> {
        let nl = self.newline();
        let before = &self.text[..at];
        let mut s = String::new();
        if !before.is_empty() {
            if !before.ends_with('\n') {
                s.push_str(nl);
            }
            if !before.trim_end_matches(['\r', '\n']).is_empty() && !blank_line_before(before) {
                s.push_str(nl);
            }
        }
        s.push_str(header);
        s.push_str(nl);
        for (k, v) in entries {
            // Writing to a String cannot fail.
            let _ = write!(s, "{} = {}{nl}", format_key(k), format_value(v));
        }
        if at < self.text.len() {
            s.push_str(nl);
        }
        self.splice(vec![(at..at, s)])
    }
}

/// Whether `text` ends with an empty line (two line breaks with only whitespace between).
fn blank_line_before(text: &str) -> bool {
    let t = text.trim_end_matches([' ', '\t', '\r']);
    let Some(t) = t.strip_suffix('\n') else { return false };
    t.trim_end_matches([' ', '\t', '\r']).ends_with('\n')
}

/// The text ranges that make up `node` (its lines and sections, with all sub-sections).
fn ranges(node: &Node, out: &mut Vec<Span>) {
    match &node.kind {
        NodeKind::Table(t) => {
            if let Style::Header { block, .. } = &t.style {
                out.push(block.clone());
            }
            if t.style != Style::Inline {
                for e in &t.entries {
                    out.extend(e.line.clone());
                    ranges(&e.node, out);
                }
            }
        }
        NodeKind::TableArray(items) => items.iter().for_each(|n| ranges(n, out)),
        NodeKind::Scalar(_) | NodeKind::Array(_) => {}
    }
}

/// Where the last section of `node` (or of its sub-tables) ends.
fn blocks_end(node: &Node) -> Option<usize> {
    let mut r = Vec::new();
    ranges(node, &mut r);
    r.iter().map(|r| r.end).max()
}

/// The range to delete to remove item `i` of a comma-separated list, with one comma.
fn list_item_range(text: &str, items: &[Span], i: usize) -> Span {
    if i + 1 < items.len() {
        items[i].start..items[i + 1].start
    } else if i > 0 {
        items[i - 1].end..items[i].end
    } else {
        let rest = &text[items[i].end..];
        let ws = rest.len() - rest.trim_start_matches([' ', '\t']).len();
        let end = if rest[ws..].starts_with(',') { items[i].end + ws + 1 } else { items[i].end };
        items[i].start..end
    }
}

/// `a.b."c d"` for a section header; every array index on the way must be the last element,
/// the only one a header can address.
fn header_path(root: &Node, path: &[Seg]) -> Result<String, String> {
    let mut keys = Vec::new();
    let mut node = Some(root);
    for seg in path {
        match seg {
            Seg::Key(k) => keys.push(format_key(k)),
            Seg::Index(i) => {
                let len = match node.map(|n| &n.kind) {
                    Some(NodeKind::TableArray(items)) => items.len(),
                    _ => 0,
                };
                if i + 1 != len {
                    return Err(format!("'{}' cannot be addressed by a section header", path_name(path)));
                }
            }
        }
        node = node.and_then(|n| n.child(seg));
    }
    Ok(keys.join("."))
}

/// Table entries with `type` first, then the rest in order.
fn ordered(t: &Table) -> Vec<(String, Value)> {
    let mut v: Vec<(String, Value)> = t.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    v.sort_by_key(|(k, _)| k != "type");
    v
}

// ---------------------------------------------------------------------- parser

struct Parser<'a> {
    src: &'a str,
    b: &'a [u8],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn new(src: &'a str) -> Parser<'a> {
        Parser { src, b: src.as_bytes(), pos: 0 }
    }

    fn peek(&self) -> Option<u8> {
        self.b.get(self.pos).copied()
    }

    fn line_of(&self, pos: usize) -> usize {
        self.src[..pos.min(self.b.len())].matches('\n').count() + 1
    }

    fn err<T>(&self, msg: impl fmt::Display) -> Result<T, String> {
        Err(format!("line {}: {}", self.line_of(self.pos), msg))
    }

    fn err_at<T>(&self, pos: usize, msg: impl fmt::Display) -> Result<T, String> {
        Err(format!("line {}: {}", self.line_of(pos), msg))
    }

    /// The character at the cursor, for messages.
    fn found(&self) -> String {
        match self.src[self.pos..].chars().next() {
            Some(c) => format!("'{c}'"),
            None => "end of file".into(),
        }
    }

    fn expect(&mut self, c: u8) -> Result<(), String> {
        if self.peek() == Some(c) {
            self.pos += 1;
            Ok(())
        } else {
            self.err(format!("expected '{}', found {}", c as char, self.found()))
        }
    }

    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t')) {
            self.pos += 1;
        }
    }

    fn skip_comment(&mut self) {
        if self.peek() == Some(b'#') {
            while !matches!(self.peek(), None | Some(b'\n')) {
                self.pos += 1;
            }
        }
    }

    fn skip_blank(&mut self) {
        loop {
            self.skip_ws();
            self.skip_comment();
            match self.peek() {
                Some(b'\n' | b'\r') => self.pos += 1,
                _ => return,
            }
        }
    }

    fn end_of_line(&mut self) -> Result<(), String> {
        self.skip_ws();
        self.skip_comment();
        if self.peek() == Some(b'\r') {
            self.pos += 1;
        }
        match self.peek() {
            None => Ok(()),
            Some(b'\n') => {
                self.pos += 1;
                Ok(())
            }
            Some(_) => self.err(format!("unexpected {} after value", self.found())),
        }
    }

    fn line_start(&self, pos: usize) -> usize {
        self.src[..pos].rfind('\n').map_or(0, |i| i + 1)
    }

    fn document(&mut self) -> Result<Node, String> {
        let mut root = Node { span: 0..0, kind: NodeKind::Table(TableNode::new(Style::Root { insert_at: 0 })) };
        // The table keys go into, and the section (root or header) whose insertion point follows them.
        let mut current: Vec<Seg> = Vec::new();
        let mut headers: Vec<(Vec<Seg>, usize)> = Vec::new();
        let mut root_has_keys = false;
        loop {
            self.skip_blank();
            let start = self.pos;
            match self.peek() {
                None => break,
                Some(b'[') => {
                    self.pos += 1;
                    let is_array = self.peek() == Some(b'[');
                    if is_array {
                        self.pos += 1;
                    }
                    self.skip_ws();
                    let keys = self.key_path()?;
                    self.skip_ws();
                    self.expect(b']')?;
                    if is_array {
                        self.expect(b']')?;
                    }
                    self.end_of_line()?;
                    let header = start..self.pos;
                    current = open_section(&mut root, &keys, header, is_array).or_else(|e| self.err_at(start, e))?;
                    headers.push((current.clone(), start));
                }
                Some(_) => {
                    let keys = self.key_path()?;
                    self.skip_ws();
                    self.expect(b'=')?;
                    self.skip_ws();
                    let value = self.value()?;
                    let kv = start..value.span.end;
                    self.end_of_line()?;
                    let line = self.line_start(start)..self.pos;
                    let at = line.end;
                    let section = node_mut(&mut root, &current).expect("the current section exists");
                    let NodeKind::Table(t) = &mut section.kind else { unreachable!("sections are tables") };
                    insert(t, &keys, value, Some(line), kv).or_else(|e| self.err_at(start, e))?;
                    match &mut t.style {
                        Style::Root { insert_at } => {
                            *insert_at = at;
                            root_has_keys = true;
                        }
                        Style::Header { insert_at, .. } => *insert_at = at,
                        _ => {}
                    }
                }
            }
        }
        // Each section's block starts at the comment lines directly above its header and ends
        // where the next one starts.
        let starts: Vec<usize> = headers.iter().map(|(_, h)| self.block_start(*h)).collect();
        for (i, (path, _)) in headers.iter().enumerate() {
            let end = starts.get(i + 1).copied().unwrap_or(self.src.len());
            if let Some(Node { kind: NodeKind::Table(TableNode { style: Style::Header { block, .. }, .. }), .. }) =
                node_mut(&mut root, path)
            {
                *block = starts[i]..end;
            }
        }
        if !root_has_keys && let NodeKind::Table(TableNode { style: Style::Root { insert_at }, .. }) = &mut root.kind {
            *insert_at = starts.first().copied().unwrap_or(self.src.len());
        }
        root.span = 0..self.src.len();
        Ok(root)
    }

    /// Start of the comment lines directly above the line starting at `pos`.
    fn block_start(&self, pos: usize) -> usize {
        let mut start = pos;
        while start > 0 {
            let prev = self.line_start(start - 1);
            if !self.src[prev..start].trim_start().starts_with('#') {
                break;
            }
            start = prev;
        }
        start
    }

    fn key_path(&mut self) -> Result<Vec<String>, String> {
        let mut path = Vec::new();
        loop {
            self.skip_ws();
            path.push(self.key()?);
            self.skip_ws();
            if self.peek() == Some(b'.') {
                self.pos += 1;
            } else {
                return Ok(path);
            }
        }
    }

    fn key(&mut self) -> Result<String, String> {
        match self.peek() {
            Some(b'"') => self.basic_string(),
            Some(b'\'') => self.literal_string(),
            _ => {
                let start = self.pos;
                while matches!(self.peek(), Some(c) if c.is_ascii_alphanumeric() || c == b'_' || c == b'-') {
                    self.pos += 1;
                }
                if start == self.pos {
                    return self.err(format!("expected a key, found {}", self.found()));
                }
                Ok(self.src[start..self.pos].to_owned())
            }
        }
    }

    fn value(&mut self) -> Result<Node, String> {
        let start = self.pos;
        let kind = match self.peek() {
            Some(b'"') => {
                if self.b[self.pos..].starts_with(b"\"\"\"") {
                    return self.err("multi-line strings are not supported");
                }
                NodeKind::Scalar(Value::String(self.basic_string()?))
            }
            Some(b'\'') => NodeKind::Scalar(Value::String(self.literal_string()?)),
            Some(b'[') => self.array()?,
            Some(b'{') => self.inline_table()?,
            Some(_) => NodeKind::Scalar(self.scalar()?),
            None => return self.err("expected a value, found end of file"),
        };
        Ok(Node { span: start..self.pos, kind })
    }

    fn basic_string(&mut self) -> Result<String, String> {
        self.expect(b'"')?;
        let mut s = String::new();
        loop {
            let run = self.pos;
            while !matches!(self.peek(), None | Some(b'"' | b'\\' | b'\n')) {
                self.pos += 1;
            }
            s.push_str(&self.src[run..self.pos]);
            match self.peek() {
                None | Some(b'\n') => return self.err("unterminated string"),
                Some(b'"') => {
                    self.pos += 1;
                    return Ok(s);
                }
                _ => {
                    self.pos += 1;
                    let c = match self.peek() {
                        Some(b'n') => '\n',
                        Some(b't') => '\t',
                        Some(b'r') => '\r',
                        Some(b'b') => '\u{8}',
                        Some(b'f') => '\u{c}',
                        Some(b'"') => '"',
                        Some(b'\\') => '\\',
                        Some(u @ (b'u' | b'U')) => {
                            let n = if u == b'u' { 4 } else { 8 };
                            let hex = self.src.get(self.pos + 1..self.pos + 1 + n).unwrap_or("");
                            match u32::from_str_radix(hex, 16).ok().and_then(char::from_u32) {
                                Some(c) => {
                                    self.pos += n;
                                    c
                                }
                                None => return self.err(format!("invalid unicode escape '\\{}{hex}'", u as char)),
                            }
                        }
                        None => return self.err("unterminated string"),
                        Some(_) => return self.err(format!("invalid escape '\\{}'", self.found().trim_matches('\''))),
                    };
                    self.pos += 1;
                    s.push(c);
                }
            }
        }
    }

    fn literal_string(&mut self) -> Result<String, String> {
        self.expect(b'\'')?;
        let start = self.pos;
        while !matches!(self.peek(), None | Some(b'\'' | b'\n')) {
            self.pos += 1;
        }
        if self.peek() != Some(b'\'') {
            return self.err("unterminated string");
        }
        self.pos += 1;
        Ok(self.src[start..self.pos - 1].to_owned())
    }

    fn array(&mut self) -> Result<NodeKind, String> {
        self.expect(b'[')?;
        let mut items = Vec::new();
        loop {
            self.skip_blank();
            if self.peek() == Some(b']') {
                self.pos += 1;
                return Ok(NodeKind::Array(items));
            }
            items.push(self.value()?);
            self.skip_blank();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b']') => {
                    self.pos += 1;
                    return Ok(NodeKind::Array(items));
                }
                None => return self.err("unterminated array"),
                Some(_) => return self.err(format!("expected ',' or ']' in array, found {}", self.found())),
            }
        }
    }

    fn inline_table(&mut self) -> Result<NodeKind, String> {
        self.expect(b'{')?;
        let mut t = TableNode::new(Style::Inline);
        loop {
            self.skip_blank();
            if self.peek() == Some(b'}') {
                self.pos += 1;
                return Ok(NodeKind::Table(t));
            }
            let start = self.pos;
            let path = self.key_path()?;
            self.skip_ws();
            self.expect(b'=')?;
            self.skip_ws();
            let v = self.value()?;
            let kv = start..v.span.end;
            insert(&mut t, &path, v, None, kv).or_else(|e| self.err_at(start, e))?;
            self.skip_blank();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b'}') => {
                    self.pos += 1;
                    return Ok(NodeKind::Table(t));
                }
                None => return self.err("unterminated inline table"),
                Some(_) => return self.err(format!("expected ',' or '}}' in inline table, found {}", self.found())),
            }
        }
    }

    fn scalar(&mut self) -> Result<Value, String> {
        let start = self.pos;
        while matches!(self.peek(), Some(c) if c.is_ascii_alphanumeric() || matches!(c, b'+' | b'-' | b'.' | b'_')) {
            self.pos += 1;
        }
        let tok: String = self.src[start..self.pos].chars().filter(|&c| c != '_').collect();
        match tok.as_str() {
            "true" => return Ok(Value::Bool(true)),
            "false" => return Ok(Value::Bool(false)),
            "inf" | "+inf" => return Ok(Value::Number(f64::INFINITY)),
            "-inf" => return Ok(Value::Number(f64::NEG_INFINITY)),
            "nan" | "+nan" | "-nan" => return Ok(Value::Number(f64::NAN)),
            "" => {
                return match self.peek() {
                    Some(_) => self.err(format!("unexpected {}", self.found())),
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

fn node_mut<'n>(root: &'n mut Node, path: &[Seg]) -> Option<&'n mut Node> {
    path.iter().try_fold(root, |n, seg| n.child_mut(seg))
}

/// Opens section `[keys]` (or a new element of `[[keys]]`); returns its path.
fn open_section(root: &mut Node, keys: &[String], header: Span, is_array: bool) -> Result<Vec<Seg>, String> {
    let (last, parents) = keys.split_last().expect("a key path is never empty");
    let mut path = Vec::new();
    let mut node = root;
    for key in parents {
        let NodeKind::Table(t) = &mut node.kind else { unreachable!("only tables are walked into") };
        if t.get(key).is_none() {
            let span = header.start..header.start;
            let node = Node { span: span.clone(), kind: NodeKind::Table(TableNode::new(Style::Implicit)) };
            t.entries.push(Entry { key: key.clone(), node, line: None, kv: span });
        }
        let child = t.entries.iter_mut().find(|e| e.key == *key).map(|e| &mut e.node).expect("just ensured");
        path.push(Seg::Key(key.clone()));
        match &child.kind {
            NodeKind::Table(t) if t.style != Style::Inline => {}
            NodeKind::TableArray(_) => {}
            _ => return Err(format!("'{key}' is a {}, not a table", child.type_name())),
        }
        node = if let NodeKind::TableArray(items) = &child.kind {
            path.push(Seg::Index(items.len() - 1));
            let NodeKind::TableArray(items) = &mut child.kind else { unreachable!() };
            items.last_mut().expect("arrays of tables are never empty")
        } else {
            child
        };
    }
    let NodeKind::Table(t) = &mut node.kind else { unreachable!("only tables are walked into") };
    path.push(Seg::Key(last.clone()));
    let section = TableNode::new(Style::Header { block: header.clone(), insert_at: header.end });
    let section = Node { span: header.clone(), kind: NodeKind::Table(section) };
    let existing = t.entries.iter_mut().find(|e| e.key == *last);
    match (existing, is_array) {
        (None, false) => t.entries.push(Entry { key: last.clone(), node: section, line: None, kv: header }),
        (None, true) => {
            let node = Node { span: header.clone(), kind: NodeKind::TableArray(vec![section]) };
            t.entries.push(Entry { key: last.clone(), node, line: None, kv: header });
            path.push(Seg::Index(0));
        }
        (Some(e), false) => match &mut e.node.kind {
            // A table created implicitly (by `[a.b]` before `[a]`) may be defined once.
            NodeKind::Table(t) if t.style == Style::Implicit => {
                t.style = section.as_table().expect("a table").style.clone();
                e.node.span = header;
            }
            NodeKind::Table(_) => return Err(format!("table [{}] is defined twice", keys.join("."))),
            _ => return Err(format!("'{last}' is a {}, not a table", e.node.type_name())),
        },
        (Some(e), true) => match &mut e.node.kind {
            NodeKind::TableArray(items) => {
                items.push(section);
                path.push(Seg::Index(items.len() - 1));
            }
            _ => return Err(format!("'{last}' is a {}, not an array of tables", e.node.type_name())),
        },
    }
    Ok(path)
}

/// Inserts `value` at the dotted key `keys` of table `t`.
fn insert(t: &mut TableNode, keys: &[String], value: Node, line: Option<Span>, kv: Span) -> Result<(), String> {
    let (last, parents) = keys.split_last().expect("a key path is never empty");
    let mut t = t;
    for key in parents {
        if t.get(key).is_none() {
            let node = Node { span: kv.start..kv.start, kind: NodeKind::Table(TableNode::new(Style::Implicit)) };
            t.entries.push(Entry { key: key.clone(), node, line: None, kv: kv.clone() });
        }
        let e = t.entries.iter_mut().find(|e| e.key == *key).expect("just ensured");
        let type_name = e.node.type_name();
        t = match &mut e.node.kind {
            NodeKind::Table(inner) if inner.style != Style::Inline => inner,
            NodeKind::TableArray(items) => match items.last_mut().map(|n| &mut n.kind) {
                Some(NodeKind::Table(inner)) => inner,
                _ => return Err(format!("'{key}' is not a table")),
            },
            _ => return Err(format!("'{key}' is a {type_name}, not a table")),
        };
    }
    if t.get(last).is_some() {
        return Err(format!("duplicate key '{last}'"));
    }
    t.entries.push(Entry { key: last.clone(), node: value, line, kv });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(segs: &[&str]) -> Vec<Seg> {
        segs.iter().map(|s| s.parse::<usize>().map_or_else(|_| Seg::from(*s), Seg::Index)).collect()
    }

    const SCENE: &str = "\
# A scene
include = \"lib.toml\"   # shared

[render]
width = 1_280   # pixels
spp = 64

# Red
[materials.red]
albedo = [0.8, 0.1, 0.1]

[[objects]]   # floor
type = \"quad\"
material = { type = \"diffuse\", albedo = 0.5 }

# The ball
[[objects]]
type = \"sphere\"
radius = 1.0
";

    #[test]
    fn spans_and_values() {
        let d = Document::parse(SCENE).unwrap();
        assert_eq!(d.text(), SCENE);
        let w = d.get(&p(&["render", "width"])).unwrap();
        assert_eq!(&SCENE[w.span.clone()], "1_280");
        assert_eq!(w.value(), Value::Number(1280.0));
        let r = d.get(&p(&["objects", "1", "radius"])).unwrap();
        assert_eq!(&SCENE[r.span.clone()], "1.0");
        let Some(NodeKind::Table(t)) = d.get(&p(&["objects", "1"])).map(|n| &n.kind) else { panic!() };
        let Style::Header { block, .. } = &t.style else { panic!() };
        assert_eq!(&SCENE[block.clone()], "# The ball\n[[objects]]\ntype = \"sphere\"\nradius = 1.0\n");
        assert_eq!(d.to_table(), super::super::parse(SCENE).unwrap());
    }

    #[test]
    fn edits_keep_formatting() {
        let mut d = Document::parse(SCENE).unwrap();
        // Equal values keep their text.
        assert!(d.set(&p(&["render", "width"]), &Value::Number(1280.0)).unwrap().is_none());
        let s = d.set(&p(&["render", "spp"]), &Value::Number(128.0)).unwrap().unwrap();
        assert_eq!((s.old.as_str(), s.new.as_str()), ("64", "128"));
        d.set(&p(&["render", "seed"]), &Value::Number(7.0)).unwrap();
        assert!(d.text().contains("spp = 128\nseed = 7\n\n# Red"), "{}", d.text());
        d.set(&p(&["objects", "0", "material", "roughness"]), &Value::Number(0.25)).unwrap();
        assert!(d.text().contains("{ type = \"diffuse\", albedo = 0.5, roughness = 0.25 }"));
        d.set(&p(&["materials", "red", "albedo", "1"]), &Value::Number(0.2)).unwrap();
        assert!(d.text().contains("albedo = [0.8, 0.2, 0.1]"));
        d.set(&p(&["include"]), &Value::String("other \"lib\".toml".into())).unwrap();
        assert!(d.text().contains("include = \"other \\\"lib\\\".toml\"   # shared"));

        // Undo by inverse splice restores the text exactly.
        let before = d.text().to_owned();
        let s = d.set(&p(&["objects", "1", "radius"]), &Value::Number(2.5)).unwrap().unwrap();
        d.apply(&s.inverse()).unwrap();
        assert_eq!(d.text(), before);
    }

    #[test]
    fn adds_sections_and_tables() {
        let mut d = Document::parse(SCENE).unwrap();
        let mut gold = Table::new();
        gold.insert("roughness".into(), Value::Number(0.1));
        gold.insert("type".into(), Value::String("metal".into()));
        d.set(&p(&["materials", "gold"]), &Value::Table(gold)).unwrap();
        assert!(
            d.text().contains(
                "albedo = [0.8, 0.1, 0.1]\n\n[materials.gold]\ntype = \"metal\"\nroughness = 0.1\n\n[[objects]]"
            ),
            "{}",
            d.text()
        );
        d.push_table(&p(&["objects"]), &[("type".into(), Value::String("box".into()))]).unwrap();
        assert!(d.text().ends_with("radius = 1.0\n\n[[objects]]\ntype = \"box\"\n"), "{}", d.text());
        d.set(&p(&["camera", "fov"]), &Value::Number(35.0)).ok();
        let mut cam = Table::new();
        cam.insert("fov".into(), Value::Number(35.0));
        d.set(&p(&["camera"]), &Value::Table(cam)).unwrap();
        assert!(d.text().ends_with("type = \"box\"\n\n[camera]\nfov = 35\n"), "{}", d.text());
        assert_eq!(d.get(&p(&["objects"])).map(|n| n.value().as_array().map(<[Value]>::len)), Some(Some(3)));
    }

    #[test]
    fn removes_lines_items_and_sections() {
        let mut d = Document::parse(SCENE).unwrap();
        d.remove(&p(&["render", "width"])).unwrap();
        assert!(d.text().contains("[render]\nspp = 64\n"));
        d.remove(&p(&["objects", "0", "material", "type"])).unwrap();
        assert!(d.text().contains("material = { albedo = 0.5 }"), "{}", d.text());
        d.remove(&p(&["materials", "red", "albedo", "2"])).unwrap();
        d.remove(&p(&["materials", "red", "albedo", "0"])).unwrap();
        assert!(d.text().contains("albedo = [0.1]"), "{}", d.text());
        d.remove(&p(&["objects", "0"])).unwrap();
        assert!(d.text().contains("albedo = [0.1]\n\n# The ball\n[[objects]]\ntype = \"sphere\""), "{}", d.text());
        d.remove(&p(&["objects", "0"])).unwrap();
        assert!(d.text().ends_with("albedo = [0.1]\n"), "{:?}", d.text());
        d.remove(&p(&["materials"])).unwrap();
        assert_eq!(d.text(), "# A scene\ninclude = \"lib.toml\"   # shared\n\n[render]\nspp = 64\n");
    }

    #[test]
    fn root_keys_go_before_the_first_section() {
        let mut d = Document::parse("# header comment\n\n[render]\nspp = 1\n").unwrap();
        d.set(&p(&["include"]), &Value::String("a.toml".into())).unwrap();
        assert_eq!(d.text(), "# header comment\n\ninclude = \"a.toml\"\n\n[render]\nspp = 1\n");
        let mut d = Document::parse("[[objects]]\ntype = \"sphere\"\n[prototypes.p.x]\na = 1\n").unwrap();
        d.push_table(&p(&["prototypes", "p", "objects"]), &[("type".into(), Value::String("box".into()))]).unwrap();
        assert_eq!(d.value(&p(&["prototypes", "p", "objects", "0", "type"])), Some(Value::String("box".into())));
        assert!(d.text().ends_with("a = 1\n\n[[prototypes.p.objects]]\ntype = \"box\"\n"), "{}", d.text());
    }

    #[test]
    fn crlf_and_errors() {
        let mut d = Document::parse("[a]\r\nx = 1\r\n").unwrap();
        d.set(&p(&["a", "y"]), &Value::Bool(true)).unwrap();
        assert_eq!(d.text(), "[a]\r\nx = 1\r\ny = true\r\n");
        assert!(Document::parse("[a]\n[a]\n").unwrap_err().contains("defined twice"));
        assert!(Document::parse("a = 1\n[a]\n").unwrap_err().starts_with("line 2"));
        assert!(Document::parse("x = \"\\q\"").unwrap_err().contains("invalid escape"));
        let d = Document::parse("s = \"\\u00e9t\\u00E9 ü\"\n").unwrap();
        assert_eq!(d.value(&p(&["s"])), Some(Value::String("été ü".into())));
    }

    #[test]
    fn splice_diff_respects_char_boundaries() {
        let s = Splice::diff("aé", "aè");
        assert_eq!((s.start, s.old.as_str(), s.new.as_str()), (1, "é", "è"));
        let s = Splice::diff("abc", "abc");
        assert!(s.old.is_empty() && s.new.is_empty());
    }
}
