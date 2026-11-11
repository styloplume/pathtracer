//! Hot reload in the editor: the scene's files (and the meshes and textures it loads)
//! are watched; a scene file changed by another program is taken in as an undoable edit
//! (or, if it has unsaved changes here, the editor asks which to keep), and a changed mesh
//! or texture loads the scene again. The editor's own saves are recognised as such: the
//! text on disk is what it saved.

use crate::editor::Editor;
use crate::workspace::Workspace;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};

const EVERY: Duration = Duration::from_millis(500);

pub struct Watch {
    files: Vec<(PathBuf, Option<SystemTime>)>,
    next: Instant,
}

impl Default for Watch {
    fn default() -> Watch {
        Watch { files: Vec::new(), next: Instant::now() + EVERY }
    }
}

fn mtime(p: &std::path::Path) -> Option<SystemTime> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
}

impl Watch {
    /// Watches the files of `ws` (keeping what is known of files watched already).
    pub fn track(&mut self, ws: &Workspace) {
        let mut paths: Vec<PathBuf> = ws.doc.files.iter().map(|f| f.path.clone()).collect();
        for p in &ws.scene.files {
            if !paths.contains(p) {
                paths.push(p.clone());
            }
        }
        let old = std::mem::take(&mut self.files);
        self.files = paths
            .into_iter()
            .map(|p| {
                let known = old.iter().find(|(q, _)| *q == p).map(|(_, t)| *t);
                let t = known.unwrap_or_else(|| mtime(&p));
                (p, t)
            })
            .collect();
    }

    pub fn next_check(&self) -> Instant {
        self.next
    }

    /// The watched files whose modification time changed since the last look (looked at
    /// twice a second at most).
    pub fn poll(&mut self) -> Vec<PathBuf> {
        let now = Instant::now();
        if now < self.next {
            return Vec::new();
        }
        self.next = now + EVERY;
        let mut changed = Vec::new();
        for (p, t) in &mut self.files {
            let m = mtime(p);
            if m != *t {
                *t = m;
                changed.push(p.clone());
            }
        }
        changed
    }

    /// Looks again at once (tests).
    #[cfg(test)]
    pub fn now(&mut self) {
        self.next = Instant::now();
    }
}

impl Editor {
    /// Takes in what other programs changed.
    pub fn watch_files(&mut self) {
        for path in self.watch.poll() {
            let Some(i) = self.ws.doc.file_index(&path) else {
                // A mesh, texture or volume the scene loads.
                self.scene_stale = true;
                let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                self.notify(format!("{name} changed on disk: reloaded"));
                continue;
            };
            let Ok(disk) = std::fs::read_to_string(&path) else { continue };
            let f = &self.ws.doc.files[i];
            if disk == f.saved() || disk == f.doc.text() {
                continue; // our own save, or the same text
            }
            if f.is_dirty() {
                self.ask_changed(i);
            } else {
                self.reload_file(i);
            }
        }
    }
}
