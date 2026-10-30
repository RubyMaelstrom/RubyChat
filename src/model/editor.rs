//! The input line's editing: moving and deleting by grapheme cluster
//! (UAX #29) and by word, a selection, and a history. `ui::input` paints it.

use std::ops::Range;

use unicode_segmentation::UnicodeSegmentation;

/// History entries kept per input (DESIGN.md §7).
const HISTORY: usize = 200;

#[derive(Clone, Debug, Default)]
pub struct LineEditor {
    text: String,
    /// Byte offset of the caret, on a grapheme boundary.
    cursor: usize,
    /// The selection's other end, when there is one.
    anchor: Option<usize>,
    history: Vec<String>,
    /// The history entry shown, and what was typed before recalling.
    recall: Option<usize>,
    draft: String,
    /// Horizontal scroll that keeps the caret visible, in logical pixels.
    pub scroll: f32,
}

impl LineEditor {
    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    pub fn selection(&self) -> Option<Range<usize>> {
        let anchor = self.anchor?;
        (anchor != self.cursor).then(|| anchor.min(self.cursor)..anchor.max(self.cursor))
    }

    /// Removes the selection, if any; true if there was one.
    fn delete_selection(&mut self) -> bool {
        let Some(range) = self.selection() else {
            self.anchor = None;
            return false;
        };
        self.text.replace_range(range.clone(), "");
        self.cursor = range.start;
        self.anchor = None;
        true
    }

    /// Inserts `text` at the caret, replacing the selection. Line breaks and
    /// other control characters (but IRC formatting codes) are left out.
    pub fn insert(&mut self, text: &str) {
        let clean: String = text
            .chars()
            .filter(|&c| {
                !c.is_control()
                    || matches!(
                        c,
                        '\u{2}'
                            | '\u{3}'
                            | '\u{f}'
                            | '\u{11}'
                            | '\u{16}'
                            | '\u{1d}'
                            | '\u{1e}'
                            | '\u{1f}'
                    )
            })
            .collect();
        if clean.is_empty() {
            return;
        }
        self.delete_selection();
        self.text.insert_str(self.cursor, &clean);
        self.cursor += clean.len();
    }

    fn previous_boundary(&self, word: bool) -> usize {
        if word {
            let mut start = 0;
            for (index, piece) in self.text[..self.cursor].split_word_bound_indices() {
                if !piece.trim().is_empty() {
                    start = index;
                }
            }
            start
        } else {
            self.text[..self.cursor]
                .grapheme_indices(true)
                .next_back()
                .map_or(0, |(index, _)| index)
        }
    }

    fn next_boundary(&self, word: bool) -> usize {
        let rest = &self.text[self.cursor..];
        if word {
            let mut seen_word = false;
            for (index, piece) in rest.split_word_bound_indices() {
                let blank = piece.trim().is_empty();
                if seen_word && blank {
                    return self.cursor + index;
                }
                seen_word |= !blank;
            }
            self.text.len()
        } else {
            rest.graphemes(true)
                .next()
                .map_or(self.cursor, |g| self.cursor + g.len())
        }
    }

    pub fn backspace(&mut self, word: bool) {
        if self.delete_selection() {
            return;
        }
        let start = self.previous_boundary(word);
        self.text.replace_range(start..self.cursor, "");
        self.cursor = start;
    }

    pub fn delete(&mut self, word: bool) {
        if self.delete_selection() {
            return;
        }
        let end = self.next_boundary(word);
        self.text.replace_range(self.cursor..end, "");
    }

    fn move_to(&mut self, position: usize, select: bool) {
        if select {
            self.anchor.get_or_insert(self.cursor);
        } else {
            self.anchor = None;
        }
        self.cursor = position;
    }

    pub fn left(&mut self, word: bool, select: bool) {
        let target = match self.selection() {
            Some(range) if !select => range.start,
            _ => self.previous_boundary(word),
        };
        self.move_to(target, select);
    }

    pub fn right(&mut self, word: bool, select: bool) {
        let target = match self.selection() {
            Some(range) if !select => range.end,
            _ => self.next_boundary(word),
        };
        self.move_to(target, select);
    }

    pub fn home(&mut self, select: bool) {
        self.move_to(0, select);
    }

    pub fn end(&mut self, select: bool) {
        self.move_to(self.text.len(), select);
    }

    /// Replaces the text in `range` (on character boundaries) with `text`,
    /// leaving the caret after it.
    pub fn replace(&mut self, range: Range<usize>, text: &str) {
        self.text.replace_range(range.clone(), text);
        self.cursor = range.start + text.len();
        self.anchor = None;
    }

    /// Starts the line over as `text`, with the caret at its end.
    pub fn set(&mut self, text: &str) {
        self.clear();
        self.insert(text);
    }

    pub fn select_all(&mut self) {
        self.anchor = Some(0);
        self.cursor = self.text.len();
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
        self.anchor = None;
        self.recall = None;
    }

    /// Takes the line for sending and remembers it.
    pub fn submit(&mut self) -> Option<String> {
        if self.text.trim().is_empty() {
            return None;
        }
        let line = std::mem::take(&mut self.text);
        if self.history.last() != Some(&line) {
            self.history.push(line.clone());
            if self.history.len() > HISTORY {
                self.history.remove(0);
            }
        }
        self.clear();
        self.draft.clear();
        Some(line)
    }

    fn show(&mut self, text: String) {
        self.text = text;
        self.cursor = self.text.len();
        self.anchor = None;
    }

    /// Up: the previous history entry.
    pub fn recall_older(&mut self) {
        let index = match self.recall {
            None if self.history.is_empty() => return,
            None => {
                self.draft = self.text.clone();
                self.history.len() - 1
            }
            Some(0) => return,
            Some(index) => index - 1,
        };
        self.recall = Some(index);
        self.show(self.history[index].clone());
    }

    /// Down: the next entry, then back to what was being typed.
    pub fn recall_newer(&mut self) {
        let Some(index) = self.recall else {
            return;
        };
        if index + 1 < self.history.len() {
            self.recall = Some(index + 1);
            self.show(self.history[index + 1].clone());
        } else {
            self.recall = None;
            let draft = std::mem::take(&mut self.draft);
            self.show(draft);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn typed(text: &str) -> LineEditor {
        let mut editor = LineEditor::default();
        editor.insert(text);
        editor
    }

    #[test]
    fn edits_by_grapheme_cluster() {
        // A family emoji is one cluster of several code points.
        let mut editor = typed("hi \u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}!");
        editor.left(false, false);
        editor.backspace(false);
        assert_eq!(editor.text(), "hi !");
        editor.insert("e\u{301}");
        assert_eq!(editor.text(), "hi e\u{301}!");
        editor.left(false, false);
        assert_eq!(
            editor.cursor(),
            3,
            "the accent and its letter move together"
        );
        editor.delete(false);
        assert_eq!(editor.text(), "hi !");
    }

    #[test]
    fn word_motions_and_selection() {
        let mut editor = typed("join the  channel");
        editor.backspace(true);
        assert_eq!(editor.text(), "join the  ");
        editor.home(false);
        editor.right(true, true);
        assert_eq!(editor.selection(), Some(0..4));
        editor.insert("part");
        assert_eq!(editor.text(), "part the  ");
        editor.select_all();
        editor.delete(false);
        assert!(editor.is_empty());
        editor.insert("a\r\nb\u{7}c");
        assert_eq!(editor.text(), "abc", "no line breaks or bells");
    }

    #[test]
    fn history_recalls_and_restores_the_draft() {
        let mut editor = LineEditor::default();
        for line in ["/join #a", "/join #b"] {
            editor.insert(line);
            assert_eq!(editor.submit().as_deref(), Some(line));
        }
        assert_eq!(editor.submit(), None, "nothing to send");
        editor.insert("draft");
        editor.recall_older();
        assert_eq!(editor.text(), "/join #b");
        editor.recall_older();
        editor.recall_older();
        assert_eq!(editor.text(), "/join #a");
        editor.recall_newer();
        editor.recall_newer();
        assert_eq!(editor.text(), "draft");
    }
}
