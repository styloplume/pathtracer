//! Undo / redo: every edit is recorded as text splices of the files it changed. Consecutive
//! edits with the same merge key (the steps of one slider drag) form one undo step until
//! the history is sealed (the drag ends) or a different edit comes in.

use crate::toml::Splice;

/// One file's text change.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    pub file: usize,
    pub splice: Splice,
}

/// One undo step.
#[derive(Clone, Debug)]
pub struct Command {
    pub label: String,
    merge: Option<String>,
    /// In the order they were made; undo reverts them back to front.
    pub changes: Vec<Change>,
}

#[derive(Debug)]
pub struct History {
    undo: Vec<Command>,
    redo: Vec<Command>,
    /// Whether the next edit starts a new step even if its merge key matches.
    sealed: bool,
    limit: usize,
}

impl Default for History {
    fn default() -> Self {
        History { undo: Vec::new(), redo: Vec::new(), sealed: true, limit: 1000 }
    }
}

impl History {
    /// Records a change; it joins the previous step if both carry the same `merge` key and
    /// the history was not sealed in between.
    pub fn record(&mut self, label: &str, merge: Option<&str>, change: Change) {
        self.redo.clear();
        let joins = !self.sealed && merge.is_some() && self.undo.last().is_some_and(|c| c.merge.as_deref() == merge);
        if joins {
            let cmd = self.undo.last_mut().expect("checked above");
            // A value dragged on is rewritten in the same place: keep one splice.
            match cmd.changes.last_mut() {
                Some(last)
                    if last.file == change.file
                        && last.splice.start == change.splice.start
                        && last.splice.new == change.splice.old =>
                {
                    last.splice.new = change.splice.new;
                }
                _ => cmd.changes.push(change),
            }
        } else {
            self.undo.push(Command { label: label.to_owned(), merge: merge.map(str::to_owned), changes: vec![change] });
            if self.undo.len() > self.limit {
                self.undo.remove(0);
            }
        }
        self.sealed = merge.is_none();
    }

    /// Ends the current merge: the next edit is a new undo step.
    pub fn seal(&mut self) {
        self.sealed = true;
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// Label of the step `undo` would revert.
    pub fn undo_label(&self) -> Option<&str> {
        self.undo.last().map(|c| c.label.as_str())
    }

    pub fn redo_label(&self) -> Option<&str> {
        self.redo.last().map(|c| c.label.as_str())
    }

    /// The step to revert; the caller reverts its changes back to front.
    pub(super) fn pop_undo(&mut self) -> Option<Command> {
        self.sealed = true;
        let c = self.undo.pop()?;
        self.redo.push(c.clone());
        Some(c)
    }

    /// Forgets the step `pop_undo` just moved to the redo stack (an edit that was refused).
    pub(super) fn drop_redo(&mut self) {
        self.redo.pop();
    }

    /// The step to make again; the caller applies its changes front to back.
    pub(super) fn pop_redo(&mut self) -> Option<Command> {
        self.sealed = true;
        let c = self.redo.pop()?;
        self.undo.push(c.clone());
        Some(c)
    }
}
