//! The editing state of a single-line text field: its text, the caret and the selection
//! (in characters), and the operations keys and the mouse perform on them.

#[derive(Clone, Debug, PartialEq)]
pub struct TextEdit {
    chars: Vec<char>,
    /// Caret position, 0..=len.
    pub cursor: usize,
    /// The other end of the selection (== cursor: no selection).
    pub anchor: usize,
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

impl TextEdit {
    /// Everything selected, as a field is when focused.
    pub fn new(text: &str) -> TextEdit {
        let chars: Vec<char> = text.chars().collect();
        let n = chars.len();
        TextEdit { chars, cursor: n, anchor: 0 }
    }

    pub fn text(&self) -> String {
        self.chars.iter().collect()
    }

    pub fn len(&self) -> usize {
        self.chars.len()
    }

    pub fn selection(&self) -> (usize, usize) {
        (self.cursor.min(self.anchor), self.cursor.max(self.anchor))
    }

    pub fn has_selection(&self) -> bool {
        self.cursor != self.anchor
    }

    pub fn selected_text(&self) -> String {
        let (a, b) = self.selection();
        self.chars[a..b].iter().collect()
    }

    pub fn select_all(&mut self) {
        self.anchor = 0;
        self.cursor = self.chars.len();
    }

    fn delete_selection(&mut self) -> bool {
        let (a, b) = self.selection();
        self.chars.drain(a..b);
        self.cursor = a;
        self.anchor = a;
        a != b
    }

    /// Replaces the selection with `s` (line breaks and tabs become spaces).
    pub fn insert(&mut self, s: &str) {
        self.delete_selection();
        let new: Vec<char> = s.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
        let n = new.len();
        self.chars.splice(self.cursor..self.cursor, new);
        self.cursor += n;
        self.anchor = self.cursor;
    }

    /// Where a word jump from `i` lands: over spaces, then over one word or one run of
    /// punctuation.
    fn word_from(&self, i: usize, forward: bool) -> usize {
        let at = |k: usize| self.chars[k];
        let mut k = i;
        if forward {
            while k < self.len() && at(k).is_whitespace() {
                k += 1;
            }
            let word = k < self.len() && is_word(at(k));
            while k < self.len() && !at(k).is_whitespace() && is_word(at(k)) == word {
                k += 1;
            }
        } else {
            while k > 0 && at(k - 1).is_whitespace() {
                k -= 1;
            }
            let word = k > 0 && is_word(at(k - 1));
            while k > 0 && !at(k - 1).is_whitespace() && is_word(at(k - 1)) == word {
                k -= 1;
            }
        }
        k
    }

    pub fn left(&mut self, word: bool, select: bool) {
        self.cursor = if !select && self.has_selection() {
            self.selection().0
        } else if word {
            self.word_from(self.cursor, false)
        } else {
            self.cursor.saturating_sub(1)
        };
        if !select {
            self.anchor = self.cursor;
        }
    }

    pub fn right(&mut self, word: bool, select: bool) {
        self.cursor = if !select && self.has_selection() {
            self.selection().1
        } else if word {
            self.word_from(self.cursor, true)
        } else {
            (self.cursor + 1).min(self.len())
        };
        if !select {
            self.anchor = self.cursor;
        }
    }

    pub fn home(&mut self, select: bool) {
        self.cursor = 0;
        if !select {
            self.anchor = 0;
        }
    }

    pub fn end(&mut self, select: bool) {
        self.cursor = self.len();
        if !select {
            self.anchor = self.cursor;
        }
    }

    pub fn backspace(&mut self, word: bool) {
        if !self.delete_selection() {
            let to = if word { self.word_from(self.cursor, false) } else { self.cursor.saturating_sub(1) };
            self.chars.drain(to..self.cursor);
            self.cursor = to;
            self.anchor = to;
        }
    }

    pub fn delete(&mut self, word: bool) {
        if !self.delete_selection() {
            let to = if word { self.word_from(self.cursor, true) } else { (self.cursor + 1).min(self.len()) };
            self.chars.drain(self.cursor..to);
        }
    }

    /// Puts the caret at `i` (extending the selection with `select`).
    pub fn set_cursor(&mut self, i: usize, select: bool) {
        self.cursor = i.min(self.len());
        if !select {
            self.anchor = self.cursor;
        }
    }

    /// Selects the word (or the run of spaces or punctuation) around position `i`.
    pub fn select_word(&mut self, i: usize) {
        let i = i.min(self.len());
        if self.chars.is_empty() {
            return;
        }
        let k = if i == self.len() { i - 1 } else { i };
        let class = |c: char| {
            if c.is_whitespace() {
                0
            } else if is_word(c) {
                1
            } else {
                2
            }
        };
        let cls = class(self.chars[k]);
        let mut a = k;
        while a > 0 && class(self.chars[a - 1]) == cls {
            a -= 1;
        }
        let mut b = k + 1;
        while b < self.len() && class(self.chars[b]) == cls {
            b += 1;
        }
        self.anchor = a;
        self.cursor = b;
    }

    /// Cuts the selection; returns its text.
    pub fn cut(&mut self) -> String {
        let s = self.selected_text();
        self.delete_selection();
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edits_like_a_text_field() {
        let mut e = TextEdit::new("hello world");
        assert_eq!(e.selected_text(), "hello world", "focusing selects everything");
        e.end(false);
        e.backspace(true);
        assert_eq!(e.text(), "hello ");
        e.insert("there");
        e.left(true, true);
        assert_eq!(e.selected_text(), "there");
        e.insert("you\n");
        assert_eq!(e.text(), "hello you ");
        e.home(false);
        e.right(true, false);
        assert_eq!(e.cursor, 5);
        e.delete(false);
        assert_eq!(e.text(), "helloyou ");
        e.select_word(2);
        assert_eq!(e.selected_text(), "helloyou");
        assert_eq!(e.cut(), "helloyou");
        assert_eq!((e.text().as_str(), e.cursor), (" ", 0));
    }

    #[test]
    fn word_jumps_stop_at_punctuation() {
        let mut e = TextEdit::new("a.b  c_d");
        e.home(false);
        e.right(true, false);
        assert_eq!(e.cursor, 1);
        e.right(true, false);
        assert_eq!(e.cursor, 2);
        e.right(true, false);
        assert_eq!(e.cursor, 3);
        e.right(true, false);
        assert_eq!(e.cursor, 8, "spaces, then the word with its underscore");
        e.left(true, false);
        assert_eq!(e.cursor, 5);
        let mut u = TextEdit::new("ünïcödé");
        u.end(false);
        u.left(false, false);
        u.backspace(false);
        assert_eq!(u.text(), "ünïcöé");
    }
}
