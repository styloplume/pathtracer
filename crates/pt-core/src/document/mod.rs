//! The editable scene document: the scene file and every file it includes, each kept as a
//! format-preserving TOML [`Document`], plus a [`Tree`] of what the merged scene contains
//! (settings, background, lights, objects, materials, media, prototypes) that records which
//! file and which entry each item comes from. Edits go to one file, are recorded for undo,
//! and the tree is rebuilt.
//!
//! The render-ready `Scene` is built by the loader from the documents' current texts, so the
//! editor and the tracer can never disagree about what a file means.

mod history;

pub use history::{Change, Command, History};

use crate::loader::{self, Source};
use crate::scene::Scene;
use crate::schema::{self, Diagnostic};
use crate::toml::{Document, NodeKind, Seg, Table, Value, path_name};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// One file of the scene.
#[derive(Debug)]
pub struct SourceFile {
    pub path: PathBuf,
    pub doc: Document,
    /// The text on disk, as last loaded or saved.
    saved: String,
    /// The files this one includes (indices into `SceneDoc::files`), in order.
    pub includes: Vec<usize>,
}

impl SourceFile {
    pub fn is_dirty(&self) -> bool {
        self.doc.text() != self.saved
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Kind {
    Light,
    Object,
    Material,
    Medium,
    Prototype,
}

/// A light, object or named definition of the merged scene, and where it is written.
#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    pub kind: Kind,
    /// Index into `SceneDoc::files`.
    pub file: usize,
    /// The entry in that file: `["objects", 3]`, `["materials", "gold"]`.
    pub path: Vec<Seg>,
    /// The definition's name; for lights and objects their `name` key, else their type.
    pub label: String,
}

/// A key of `[render]`, `[camera]` or `[output]`, and the file whose value wins.
#[derive(Clone, Debug, PartialEq)]
pub struct Setting {
    pub section: &'static str,
    pub key: String,
    pub file: usize,
}

/// What the merged scene contains, following the loader's merge rules: lists are
/// concatenated (included files first), named definitions and settings are overridden by
/// later files, and `[background]` is replaced as a whole.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Tree {
    pub settings: Vec<Setting>,
    /// The file whose `[background]` is used.
    pub background: Option<usize>,
    pub lights: Vec<Item>,
    pub objects: Vec<Item>,
    /// Sorted by name, like the loader reads them.
    pub materials: Vec<Item>,
    pub media: Vec<Item>,
    pub prototypes: Vec<Item>,
}

const SETTINGS: [&str; 3] = ["render", "camera", "output"];

/// A scene being edited.
#[derive(Debug)]
pub struct SceneDoc {
    /// The scene file first, then the included files in the order they are first included.
    pub files: Vec<SourceFile>,
    /// Files in merge order (a file after the files it includes); a file included twice
    /// appears twice, as the loader merges it twice.
    order: Vec<usize>,
    pub tree: Tree,
    history: History,
}

impl SceneDoc {
    /// Opens the scene file `path` and everything it includes.
    pub fn open(path: &Path) -> Result<SceneDoc, String> {
        Self::open_from(path, &loader::read_file)
    }

    /// Opens a scene whose files are read through `read`.
    pub fn open_from(path: &Path, read: Source<'_>) -> Result<SceneDoc, String> {
        let mut doc =
            SceneDoc { files: Vec::new(), order: Vec::new(), tree: Tree::default(), history: History::default() };
        doc.load_file(path, read)?;
        doc.rebuild(read)?;
        Ok(doc)
    }

    /// The scene file.
    pub fn path(&self) -> &Path {
        &self.files[0].path
    }

    pub fn file_index(&self, path: &Path) -> Option<usize> {
        let canon = loader::canonical(path);
        self.files.iter().position(|f| f.path == path || loader::canonical(&f.path) == canon)
    }

    fn load_file(&mut self, path: &Path, read: Source<'_>) -> Result<usize, String> {
        if let Some(i) = self.file_index(path) {
            return Ok(i);
        }
        let text = read(path)?;
        let doc = Document::parse(text.clone()).map_err(|e| format!("{}: {e}", path.display()))?;
        self.files.push(SourceFile { path: path.to_path_buf(), doc, saved: text, includes: Vec::new() });
        Ok(self.files.len() - 1)
    }

    /// Re-reads the include structure (loading newly included files through `read`) and
    /// rebuilds the tree.
    fn rebuild(&mut self, read: Source<'_>) -> Result<(), String> {
        let mut order = Vec::new();
        self.walk(0, &mut Vec::new(), &mut order, read)?;
        self.order = order;
        self.tree = self.build_tree();
        Ok(())
    }

    fn walk(
        &mut self,
        file: usize,
        stack: &mut Vec<PathBuf>,
        order: &mut Vec<usize>,
        read: Source<'_>,
    ) -> Result<(), String> {
        let path = self.files[file].path.clone();
        let canon = loader::canonical(&path);
        if stack.contains(&canon) {
            return Err(format!("include cycle: '{}' includes itself", path.display()));
        }
        let names =
            loader::includes(&self.files[file].doc.to_table()).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut includes = Vec::new();
        for name in names {
            let p = loader::include_path(&path, &name);
            let i = self.load_file(&p, read).map_err(|e| format!("{e}\n  (included from '{}')", path.display()))?;
            includes.push(i);
        }
        self.files[file].includes.clone_from(&includes);
        stack.push(canon);
        for i in includes {
            self.walk(i, stack, order, read)?;
        }
        stack.pop();
        order.push(file);
        Ok(())
    }

    fn build_tree(&self) -> Tree {
        let mut t = Tree::default();
        let mut settings: Vec<Setting> = Vec::new();
        let mut named: [BTreeMap<String, Item>; 3] = Default::default();
        for &f in &self.order {
            let NodeKind::Table(root) = &self.files[f].doc.root().kind else { continue };
            for e in &root.entries {
                let key = e.key.as_str();
                match (&e.node.kind, key) {
                    (NodeKind::Table(s), _) if SETTINGS.contains(&key) => {
                        let section = SETTINGS.into_iter().find(|&n| n == key).expect("matched");
                        for k in &s.entries {
                            settings.retain(|x| !(x.section == section && x.key == k.key));
                            settings.push(Setting { section, key: k.key.clone(), file: f });
                        }
                    }
                    (_, "background") => t.background = Some(f),
                    (NodeKind::Array(items) | NodeKind::TableArray(items), "lights" | "objects") => {
                        let kind = if key == "lights" { Kind::Light } else { Kind::Object };
                        for (i, n) in items.iter().enumerate() {
                            let label = n.as_table().map_or_else(String::new, |t| item_label(&t.to_table()));
                            let path = vec![Seg::from(key), Seg::Index(i)];
                            let list = if kind == Kind::Light { &mut t.lights } else { &mut t.objects };
                            list.push(Item { kind, file: f, path, label });
                        }
                    }
                    (NodeKind::Table(defs), "materials" | "media" | "prototypes") => {
                        let (slot, kind) = match key {
                            "materials" => (0, Kind::Material),
                            "media" => (1, Kind::Medium),
                            _ => (2, Kind::Prototype),
                        };
                        for d in &defs.entries {
                            let path = vec![Seg::from(key), Seg::from(d.key.as_str())];
                            named[slot].insert(d.key.clone(), Item { kind, file: f, path, label: d.key.clone() });
                        }
                    }
                    _ => {}
                }
            }
        }
        let [materials, media, prototypes] = named;
        t.materials = materials.into_values().collect();
        t.media = media.into_values().collect();
        t.prototypes = prototypes.into_values().collect();
        t.settings = settings;
        t
    }

    /// The current text of `path` if it is one of the scene's files, else the file on disk.
    pub fn read(&self, path: &Path) -> Result<String, String> {
        match self.file_index(path) {
            Some(i) => Ok(self.files[i].doc.text().to_owned()),
            None => loader::read_file(path),
        }
    }

    /// The merged scene document, as the loader sees it.
    pub fn merged(&self) -> Result<Table, String> {
        loader::load_document_from(self.path(), &|p| self.read(p))
    }

    /// Builds the render-ready scene from the current (possibly unsaved) texts.
    pub fn to_scene(&self) -> Result<Scene, String> {
        loader::load_scene_from(self.path(), &|p| self.read(p))
    }

    /// Schema problems of the merged scene.
    pub fn validate(&self) -> Result<Vec<Diagnostic>, String> {
        Ok(schema::validate(&self.merged()?))
    }

    /// The value at `path` of file `file`.
    pub fn value(&self, file: usize, path: &[Seg]) -> Option<Value> {
        self.files.get(file)?.doc.value(path)
    }

    /// The table of `item`.
    pub fn item_table(&self, item: &Item) -> Option<Table> {
        match self.value(item.file, &item.path)? {
            Value::Table(t) => Some(t),
            _ => None,
        }
    }

    // ------------------------------------------------------------------ editing

    fn edit(
        &mut self,
        file: usize,
        label: &str,
        merge: Option<&str>,
        f: impl FnOnce(&mut Document) -> Result<Option<crate::toml::Splice>, String>,
    ) -> Result<bool, String> {
        let source = self.files.get_mut(file).ok_or_else(|| format!("no file #{file}"))?;
        let Some(splice) = f(&mut source.doc)? else { return Ok(false) };
        self.history.record(label, merge, Change { file, splice: splice.clone() });
        if let Err(e) = self.rebuild(&loader::read_file) {
            // An edit that breaks the include structure (a cycle, a missing file) is refused.
            self.files[file].doc.apply(&splice.inverse()).expect("reverting the edit just made");
            self.history.pop_undo();
            self.history.drop_redo();
            self.rebuild(&loader::read_file).expect("the previous state was valid");
            return Err(e);
        }
        Ok(true)
    }

    /// Sets the value at `path` of file `file`; returns whether the text changed.
    pub fn set(&mut self, file: usize, path: &[Seg], value: &Value) -> Result<bool, String> {
        let label = format!("set {}", path_name(path));
        self.edit(file, &label, None, |d| d.set(path, value))
    }

    /// Like `set`, but consecutive calls with the same `merge` key are one undo step until
    /// `seal` is called (for continuous changes such as a slider drag).
    pub fn set_merging(&mut self, file: usize, path: &[Seg], value: &Value, merge: &str) -> Result<bool, String> {
        let label = format!("set {}", path_name(path));
        self.edit(file, &label, Some(merge), |d| d.set(path, value))
    }

    /// Removes the entry at `path` of file `file`.
    pub fn remove(&mut self, file: usize, path: &[Seg]) -> Result<bool, String> {
        let label = format!("remove {}", path_name(path));
        self.edit(file, &label, None, |d| d.remove(path))
    }

    /// Appends a table to the array of tables at `path` (`["objects"]`) of file `file`.
    pub fn push_table(&mut self, file: usize, path: &[Seg], entries: &[(String, Value)]) -> Result<bool, String> {
        let label = format!("add to {}", path_name(path));
        self.edit(file, &label, None, |d| d.push_table(path, entries))
    }

    /// Ends the current merged edit.
    pub fn seal(&mut self) {
        self.history.seal();
    }

    pub fn history(&self) -> &History {
        &self.history
    }

    /// Reverts the last undo step; returns whether there was one.
    pub fn undo(&mut self) -> Result<bool, String> {
        let Some(cmd) = self.history.pop_undo() else { return Ok(false) };
        for c in cmd.changes.iter().rev() {
            self.files[c.file].doc.apply(&c.splice.inverse())?;
        }
        self.rebuild(&loader::read_file)?;
        Ok(true)
    }

    /// Makes the last undone step again.
    pub fn redo(&mut self) -> Result<bool, String> {
        let Some(cmd) = self.history.pop_redo() else { return Ok(false) };
        for c in &cmd.changes {
            self.files[c.file].doc.apply(&c.splice)?;
        }
        self.rebuild(&loader::read_file)?;
        Ok(true)
    }

    // ------------------------------------------------------------------ saving

    pub fn is_dirty(&self) -> bool {
        self.files.iter().any(SourceFile::is_dirty)
    }

    /// Writes every changed file; returns their paths. Unchanged files are not touched.
    pub fn save(&mut self) -> Result<Vec<PathBuf>, String> {
        let mut written = Vec::new();
        for f in &mut self.files {
            if f.is_dirty() {
                write_atomic(&f.path, f.doc.text())?;
                f.saved = f.doc.text().to_owned();
                written.push(f.path.clone());
            }
        }
        Ok(written)
    }
}

/// Writes through a temporary file and a rename, so a failed write never leaves half a file.
fn write_atomic(path: &Path, text: &str) -> Result<(), String> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".saving");
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, text).map_err(|e| format!("cannot write '{}': {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| {
        std::fs::remove_file(&tmp).ok();
        format!("cannot replace '{}': {e}", path.display())
    })
}

/// An object's or light's label: its `name`, else its type (a mesh's file name).
fn item_label(t: &Table) -> String {
    if let Some(Value::String(n)) = t.get("name") {
        return n.clone();
    }
    let file = t.get("file").and_then(Value::as_str);
    match (t.get("type").and_then(Value::as_str), file) {
        (Some(ty), None) => ty.to_owned(),
        (ty, Some(f)) => {
            let name = Path::new(f).file_name().map_or(f.into(), |n| n.to_string_lossy());
            format!("{} {name}", ty.unwrap_or("mesh"))
        }
        (None, None) => "?".into(),
    }
}

#[cfg(test)]
mod tests;
