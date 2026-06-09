//! The editable document — the Canon Cat branch's single continuous text stream.
//!
//! A [`Buffer`] wraps a [`ropey::Rope`] and owns the editing state that goes
//! with it: the cursor, a sticky goal column for vertical movement, the dirty
//! flag, and a monotonic version counter (used later to discard stale results
//! from background workers — see `docs/DESIGN.md`).
//!
//! On this branch there are **no files**. Every mutation is also recorded in a
//! [journal](Buffer::take_journal) of [`EditOp`]s, which the editor drains and
//! appends to the SQLite [store](crate::store) after each keystroke — so the
//! workspace is persisted continuously and resumes exactly where you left off.
//!
//! The cursor is a single `char` index into the rope (`0..=len_chars`). Line and
//! column are derived from it on demand via the rope's index maps. Columns come
//! in two flavours: **char columns** (used for movement and the status line) and
//! **display columns** (tabs expanded), used for rendering and scrolling.

use ropey::Rope;

use crate::store::EditOp;

/// Width a tab expands to on screen. Will become a compile-time theme/option;
/// hard-coded for now.
const TAB_WIDTH: usize = 4;

/// An open document plus its cursor and edit state.
pub struct Buffer {
    rope: Rope,
    /// Cursor as a char index into the rope, in `0..=rope.len_chars()`.
    cursor: usize,
    /// Sticky target display-agnostic char column for up/down; cleared by any
    /// horizontal move or edit.
    goal_col: Option<usize>,
    dirty: bool,
    /// Edits applied since the last drain, in apply order, awaiting persistence.
    journal: Vec<EditOp>,
}

impl Buffer {
    /// Build a buffer from existing text (e.g. a [`crate::store::Resume`]),
    /// placing the cursor at char index `cursor`. The text is treated as already
    /// persisted, so it does not enter the journal and the buffer starts clean.
    pub fn from_text(text: &str, cursor: usize) -> Self {
        Self {
            cursor: cursor.min(text.chars().count()),
            rope: Rope::from_str(text),
            goal_col: None,
            dirty: false,
            journal: Vec::new(),
        }
    }

    // --- accessors -------------------------------------------------------

    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Clear the dirty flag — called by the editor once the journal has been
    /// flushed to the store, so "dirty" tracks *unpersisted* edits (essentially
    /// never set, since we persist every keystroke).
    pub fn mark_saved(&mut self) {
        self.dirty = false;
    }

    /// Drain the pending edits for the store to append. Apply order is preserved.
    pub fn take_journal(&mut self) -> Vec<EditOp> {
        std::mem::take(&mut self.journal)
    }

    /// Number of lines for display. A trailing newline yields a final empty
    /// line, which is a valid cursor position.
    pub fn len_lines(&self) -> usize {
        self.rope.len_lines()
    }

    /// Total char count (one past the last valid cursor position).
    pub fn len_chars(&self) -> usize {
        self.rope.len_chars()
    }

    /// The cursor as a char index.
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Move the cursor to a char index (clamped). Used by LEAP.
    pub fn set_cursor(&mut self, idx: usize) {
        self.cursor = idx.min(self.rope.len_chars());
        self.goal_col = None;
    }

    /// `(line, display_col)` of an arbitrary char index, tabs expanded — used to
    /// place the LEAP match highlight.
    pub fn line_col_at(&self, char_idx: usize) -> (usize, usize) {
        let line = self.rope.char_to_line(char_idx);
        let char_col = char_idx - self.rope.line_to_char(line);
        let mut col = 0;
        for (i, c) in self.rope.line(line).chars().enumerate() {
            if i >= char_col || c == '\n' || c == '\r' {
                break;
            }
            col += tab_advance(c, col);
        }
        (line, col)
    }

    // --- search (for LEAP) ----------------------------------------------

    /// First match of `needle` at or after char index `from`. `insensitive`
    /// does ASCII-case-insensitive matching (UTF-8 safe). Returns the match
    /// start as a char index.
    pub fn search_forward(&self, from: usize, needle: &str, insensitive: bool) -> Option<usize> {
        if needle.is_empty() {
            return None;
        }
        let text: String = self.rope.chunks().collect();
        let from_byte = self.rope.char_to_byte(from);
        find_from(&text, needle, from_byte, insensitive).map(|b| self.rope.byte_to_char(b))
    }

    /// Last match of `needle` that starts strictly before char index `before`.
    pub fn search_backward(&self, before: usize, needle: &str, insensitive: bool) -> Option<usize> {
        if needle.is_empty() {
            return None;
        }
        let text: String = self.rope.chunks().collect();
        let before_byte = self.rope.char_to_byte(before);
        find_last_before(&text, needle, before_byte, insensitive).map(|b| self.rope.byte_to_char(b))
    }

    /// Cursor position as `(line, char_col)`, both 0-based.
    pub fn cursor_line_col(&self) -> (usize, usize) {
        let line = self.rope.char_to_line(self.cursor);
        let col = self.cursor - self.rope.line_to_char(line);
        (line, col)
    }

    /// Cursor's on-screen column (0-based), with tabs expanded.
    pub fn cursor_display_col(&self) -> usize {
        let (line, char_col) = self.cursor_line_col();
        let mut col = 0;
        for (i, c) in self.rope.line(line).chars().enumerate() {
            if i >= char_col || c == '\n' || c == '\r' {
                break;
            }
            col += tab_advance(c, col);
        }
        col
    }

    /// Build the visible slice of `line`, tabs expanded, scrolled horizontally
    /// by `left` display columns and clipped to `width` columns.
    pub fn display_line(&self, line: usize, left: usize, width: usize) -> String {
        let mut expanded = String::new();
        let mut col = 0;
        for c in self.rope.line(line).chars() {
            if c == '\n' || c == '\r' || c == Self::DOC_MARKER {
                continue;
            }
            if c == '\t' {
                let n = tab_advance(c, col);
                expanded.extend(std::iter::repeat_n(' ', n));
                col += n;
            } else {
                expanded.push(c);
                col += 1; // wide-char (CJK) width is deferred — see DESIGN.md.
            }
        }
        expanded.chars().skip(left).take(width).collect()
    }

    // --- cursor movement -------------------------------------------------

    pub fn move_left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
        self.goal_col = None;
    }

    pub fn move_right(&mut self) {
        if self.cursor < self.rope.len_chars() {
            self.cursor += 1;
        }
        self.goal_col = None;
    }

    pub fn move_up(&mut self) {
        self.move_vertical(-1);
    }

    pub fn move_down(&mut self) {
        self.move_vertical(1);
    }

    pub fn move_home(&mut self) {
        let (line, _) = self.cursor_line_col();
        self.cursor = self.rope.line_to_char(line);
        self.goal_col = None;
    }

    pub fn move_end(&mut self) {
        let (line, _) = self.cursor_line_col();
        self.cursor = self.rope.line_to_char(line) + self.line_char_len(line);
        self.goal_col = None;
    }

    /// `M-f`: move to the end of the next word (skip non-word chars, then word
    /// chars), Emacs-style.
    pub fn move_word_forward(&mut self) {
        let len = self.rope.len_chars();
        let mut i = self.cursor;
        while i < len && !is_word_char(self.rope.char(i)) {
            i += 1;
        }
        while i < len && is_word_char(self.rope.char(i)) {
            i += 1;
        }
        self.cursor = i;
        self.goal_col = None;
    }

    /// `M-b`: move to the start of the previous word.
    pub fn move_word_backward(&mut self) {
        self.cursor = self.word_backward_pos();
        self.goal_col = None;
    }

    fn move_vertical(&mut self, dir: isize) {
        let (line, col) = self.cursor_line_col();
        // Remember the column we started a vertical run from, so passing through
        // short lines doesn't permanently shrink the target column.
        let goal = *self.goal_col.get_or_insert(col);
        let target = line as isize + dir;
        if target < 0 || target as usize >= self.rope.len_lines() {
            return;
        }
        let target = target as usize;
        let new_col = goal.min(self.line_char_len(target));
        self.cursor = self.rope.line_to_char(target) + new_col;
    }

    // --- document boundaries (Canon Cat) ---------------------------------

    /// The in-band sentinel char marking a document boundary in the stream:
    /// U+001E RECORD SEPARATOR. One char wide, never typed by a user, rendered
    /// specially by the editor.
    pub const DOC_MARKER: char = '\u{1e}';

    /// Char index of the next document marker strictly after the cursor.
    fn next_marker(&self) -> Option<usize> {
        (self.cursor + 1..self.rope.len_chars())
            .find(|&i| self.rope.char(i) == Self::DOC_MARKER)
    }

    /// The leading text of the document containing the cursor (from just after
    /// the preceding marker to the end of that line), used as a status label.
    pub fn document_title(&self, max: usize) -> String {
        let start = self.current_document_start();
        self.rope
            .chars_at(start)
            .take_while(|&c| c != '\n' && c != Self::DOC_MARKER)
            .filter(|c| !c.is_control())
            .take(max)
            .collect::<String>()
            .trim()
            .to_string()
    }

    fn prev_marker_at_or_before(&self, pos: usize) -> Option<usize> {
        (0..pos).rev().find(|&i| self.rope.char(i) == Self::DOC_MARKER)
    }

    /// Whether `line` is a lone document-boundary marker (so the editor can draw
    /// it as a horizontal rule instead of a stray control char).
    pub fn line_is_marker(&self, line: usize) -> bool {
        line < self.rope.len_lines() && self.rope.line(line).chars().next() == Some(Self::DOC_MARKER)
    }

    /// First char of the document body that the marker at char index `m`
    /// introduces — i.e. the start of the line after the marker.
    fn marker_body_start(&self, m: usize) -> usize {
        let next_line = self.rope.char_to_line(m) + 1;
        self.rope.line_to_char(next_line.min(self.rope.len_lines()))
    }

    /// Start of the document body containing char index `pos`.
    fn document_start_containing(&self, pos: usize) -> usize {
        match self.prev_marker_at_or_before(pos) {
            Some(m) => self.marker_body_start(m),
            None => 0,
        }
    }

    /// Start of the document the cursor is in (`C-x [` first lands here).
    pub fn current_document_start(&self) -> usize {
        self.document_start_containing(self.cursor)
    }

    /// Start of the document before the cursor's, if any (`C-x [` again).
    pub fn prev_document_start(&self) -> Option<usize> {
        let cur = self.current_document_start();
        (cur > 0).then(|| self.document_start_containing(cur.saturating_sub(2)))
    }

    /// Start of the next document after the cursor's, if any (`C-x ]`).
    pub fn next_document_start(&self) -> Option<usize> {
        self.next_marker().map(|m| self.marker_body_start(m))
    }

    // --- editing ---------------------------------------------------------

    pub fn insert_char(&mut self, c: char) {
        let pos = self.cursor;
        self.rope.insert_char(pos, c);
        self.cursor += 1;
        self.journal.push(EditOp::Insert {
            pos,
            text: c.to_string(),
        });
        self.on_edit();
    }

    pub fn insert_newline(&mut self) {
        self.insert_char('\n');
    }

    /// Start a new document at the cursor (`C-x C-n`): drop a boundary marker on
    /// its own line. Breaks the current line first if the cursor isn't at its
    /// start, so the marker always renders as a clean rule.
    pub fn insert_document_break(&mut self) {
        let (_, col) = self.cursor_line_col();
        if col != 0 {
            self.insert_char('\n');
        }
        self.insert_char(Self::DOC_MARKER);
        self.insert_char('\n');
    }

    /// Delete the character before the cursor (Backspace).
    pub fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let pos = self.cursor - 1;
        let removed: String = self.rope.slice(pos..self.cursor).chars().collect();
        self.rope.remove(pos..self.cursor);
        self.cursor = pos;
        self.journal.push(EditOp::Delete { pos, text: removed });
        self.on_edit();
    }

    /// Delete the character at the cursor (Delete / `C-d`).
    pub fn delete_forward(&mut self) {
        if self.cursor >= self.rope.len_chars() {
            return;
        }
        let removed: String = self.rope.slice(self.cursor..self.cursor + 1).chars().collect();
        self.rope.remove(self.cursor..self.cursor + 1);
        self.journal.push(EditOp::Delete {
            pos: self.cursor,
            text: removed,
        });
        self.on_edit();
    }

    /// Insert a string at the cursor (used by yank); advances past it.
    pub fn insert_str(&mut self, s: &str) {
        if s.is_empty() {
            return;
        }
        let pos = self.cursor;
        self.rope.insert(pos, s);
        self.cursor += s.chars().count();
        self.journal.push(EditOp::Insert {
            pos,
            text: s.to_string(),
        });
        self.on_edit();
    }

    /// `C-k`: kill from the cursor to end of line, or — if already there — the
    /// line break itself (joining the next line). Returns the removed text so
    /// the editor can push it onto the kill ring.
    pub fn kill_line(&mut self) -> String {
        let (line, _) = self.cursor_line_col();
        let eol = self.rope.line_to_char(line) + self.line_char_len(line);
        let end = if self.cursor < eol {
            eol
        } else if self.cursor < self.rope.len_chars() {
            self.cursor + 1 // at end of line: kill the newline
        } else {
            return String::new();
        };
        let killed: String = self.rope.slice(self.cursor..end).chars().collect();
        self.rope.remove(self.cursor..end);
        self.journal.push(EditOp::Delete {
            pos: self.cursor,
            text: killed.clone(),
        });
        self.on_edit();
        killed
    }

    /// `C-w`: kill the word before the cursor. Returns the removed text.
    pub fn backward_kill_word(&mut self) -> String {
        let target = self.word_backward_pos();
        if target >= self.cursor {
            return String::new();
        }
        let killed: String = self.rope.slice(target..self.cursor).chars().collect();
        self.rope.remove(target..self.cursor);
        self.cursor = target;
        self.journal.push(EditOp::Delete {
            pos: target,
            text: killed.clone(),
        });
        self.on_edit();
        killed
    }

    fn on_edit(&mut self) {
        self.dirty = true;
        self.goal_col = None;
    }

    // --- helpers ---------------------------------------------------------

    /// The char index at the start of the word before the cursor (skip non-word
    /// chars, then word chars), used by `M-b` and `C-w`.
    fn word_backward_pos(&self) -> usize {
        let mut i = self.cursor;
        while i > 0 && !is_word_char(self.rope.char(i - 1)) {
            i -= 1;
        }
        while i > 0 && is_word_char(self.rope.char(i - 1)) {
            i -= 1;
        }
        i
    }

    /// Length of `line` in chars, excluding the trailing line break.
    fn line_char_len(&self, line: usize) -> usize {
        if line >= self.rope.len_lines() {
            return 0;
        }
        let slice = self.rope.line(line);
        let mut len = slice.len_chars();
        if len > 0 && slice.char(len - 1) == '\n' {
            len -= 1;
            if len > 0 && slice.char(len - 1) == '\r' {
                len -= 1;
            }
        }
        len
    }
}

/// Whether `c` counts as part of a word for `M-f`/`M-b`/`C-w`.
fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Whether `needle` matches `hay` at byte offset `i` (ASCII-case-insensitive
/// when `insensitive`). Byte-wise comparison is UTF-8 safe: non-ASCII bytes are
/// untouched by ASCII case folding and must match exactly.
fn matches_at(hay: &[u8], needle: &[u8], i: usize, insensitive: bool) -> bool {
    i + needle.len() <= hay.len() && {
        let window = &hay[i..i + needle.len()];
        if insensitive {
            window.eq_ignore_ascii_case(needle)
        } else {
            window == needle
        }
    }
}

/// First byte offset >= `from` where `needle` matches at a char boundary.
fn find_from(text: &str, needle: &str, from: usize, insensitive: bool) -> Option<usize> {
    let (hay, nb) = (text.as_bytes(), needle.as_bytes());
    let mut i = from;
    while i + nb.len() <= hay.len() {
        if text.is_char_boundary(i) && matches_at(hay, nb, i, insensitive) {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// Greatest byte offset `< before` where `needle` matches at a char boundary.
fn find_last_before(text: &str, needle: &str, before: usize, insensitive: bool) -> Option<usize> {
    let (hay, nb) = (text.as_bytes(), needle.as_bytes());
    let mut best = None;
    let mut i = 0;
    while i + nb.len() <= hay.len() && i < before {
        if text.is_char_boundary(i) && matches_at(hay, nb, i, insensitive) {
            best = Some(i);
        }
        i += 1;
    }
    best
}

/// How many columns a character advances the cursor, given the current column.
fn tab_advance(c: char, col: usize) -> usize {
    if c == '\t' {
        TAB_WIDTH - (col % TAB_WIDTH)
    } else {
        1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a buffer containing `s`, cursor left at the end.
    fn buf(s: &str) -> Buffer {
        let mut b = Buffer::from_text("", 0);
        for c in s.chars() {
            b.insert_char(c);
        }
        b
    }

    #[test]
    fn empty_buffer_has_one_line() {
        let b = buf("");
        assert_eq!(b.len_lines(), 1);
        assert_eq!(b.cursor_line_col(), (0, 0));
        assert!(!b.is_dirty());
    }

    #[test]
    fn insert_advances_cursor_and_dirties() {
        let b = buf("abc");
        assert_eq!(b.cursor_line_col(), (0, 3));
        assert_eq!(b.display_line(0, 0, 80), "abc");
        assert!(b.is_dirty());
    }

    #[test]
    fn newline_splits_into_lines() {
        let b = buf("ab\ncd");
        assert_eq!(b.len_lines(), 2);
        assert_eq!(b.cursor_line_col(), (1, 2));
        assert_eq!(b.display_line(0, 0, 80), "ab");
        assert_eq!(b.display_line(1, 0, 80), "cd");
    }

    #[test]
    fn horizontal_movement_clamps_at_ends() {
        let mut b = buf("ab");
        b.move_right(); // already at end -> no-op
        assert_eq!(b.cursor_line_col(), (0, 2));
        b.move_left();
        b.move_left();
        b.move_left(); // clamps at start
        assert_eq!(b.cursor_line_col(), (0, 0));
    }

    #[test]
    fn vertical_movement_keeps_goal_column() {
        // Long, short, long: moving down then down should return to column 5.
        let mut b = buf("abcdef\nx\nyyyyyy");
        b.set_cursor(0);
        for _ in 0..5 {
            b.move_right(); // column 5 on the long first line
        }
        assert_eq!(b.cursor_line_col(), (0, 5));
        b.move_down(); // short line "x" -> clamp to col 1
        assert_eq!(b.cursor_line_col(), (1, 1));
        b.move_down(); // long line again -> goal column 5 restored
        assert_eq!(b.cursor_line_col(), (2, 5));
    }

    #[test]
    fn backspace_merges_lines() {
        let mut b = buf("ab\ncd");
        b.move_home(); // start of "cd"
        assert_eq!(b.cursor_line_col(), (1, 0));
        b.backspace(); // remove the newline, merging the lines
        assert_eq!(b.len_lines(), 1);
        assert_eq!(b.cursor_line_col(), (0, 2));
        assert_eq!(b.display_line(0, 0, 80), "abcd");
    }

    #[test]
    fn delete_forward_at_eol_merges_next_line() {
        let mut b = buf("ab\ncd");
        b.set_cursor(0);
        b.move_end(); // end of "ab"
        b.delete_forward();
        assert_eq!(b.display_line(0, 0, 80), "abcd");
        assert_eq!(b.len_lines(), 1);
    }

    #[test]
    fn move_end_stops_before_newline() {
        let mut b = buf("hello\nx");
        b.set_cursor(0);
        b.move_end();
        assert_eq!(b.cursor_line_col(), (0, 5));
    }

    #[test]
    fn tabs_expand_for_display_and_cursor_column() {
        let b = buf("\tx"); // tab then x
        assert_eq!(b.display_line(0, 0, 80), "    x");
        assert_eq!(b.cursor_line_col(), (0, 2));
        assert_eq!(b.cursor_display_col(), TAB_WIDTH + 1);
    }

    #[test]
    fn word_movement_skips_punctuation() {
        let mut b = buf("foo, bar_baz qux");
        b.set_cursor(0);
        b.move_word_forward(); // end of "foo"
        assert_eq!(b.cursor_line_col(), (0, 3));
        b.move_word_forward(); // end of "bar_baz" (underscore is a word char)
        assert_eq!(b.cursor_line_col(), (0, 12));
        b.move_word_backward(); // back to start of "bar_baz"
        assert_eq!(b.cursor_line_col(), (0, 5));
    }

    #[test]
    fn kill_line_to_eol_then_newline() {
        let mut b = buf("hello world\nnext");
        b.set_cursor(0);
        for _ in 0..6 {
            b.move_right(); // before "world"
        }
        assert_eq!(b.kill_line(), "world");
        assert_eq!(b.display_line(0, 0, 80), "hello ");
        // At end of line now: a second kill removes the newline, joining lines.
        assert_eq!(b.kill_line(), "\n");
        assert_eq!(b.len_lines(), 1);
        assert_eq!(b.display_line(0, 0, 80), "hello next");
    }

    #[test]
    fn backward_kill_word_removes_and_returns() {
        let mut b = buf("alpha beta");
        let killed = b.backward_kill_word();
        assert_eq!(killed, "beta");
        assert_eq!(b.display_line(0, 0, 80), "alpha ");
        assert_eq!(b.cursor_line_col(), (0, 6));
    }

    #[test]
    fn insert_str_yanks_at_cursor() {
        let mut b = buf("ac");
        b.move_left(); // between a and c
        b.insert_str("XYZ");
        assert_eq!(b.display_line(0, 0, 80), "aXYZc");
        assert_eq!(b.cursor_line_col(), (0, 4));
    }

    // --- persistence journal --------------------------------------------

    #[test]
    fn from_text_starts_clean_with_no_journal() {
        let mut b = Buffer::from_text("hello\nworld", 3);
        assert_eq!(b.cursor_line_col(), (0, 3));
        assert!(!b.is_dirty());
        assert!(b.take_journal().is_empty());
    }

    #[test]
    fn edits_are_journalled_as_ops() {
        let mut b = Buffer::from_text("abc", 3);
        b.insert_char('d'); // Insert at 3
        b.backspace(); // Delete at 3
        b.set_cursor(0);
        b.delete_forward(); // Delete at 0 ("a")
        let ops = b.take_journal();
        assert_eq!(
            ops,
            vec![
                EditOp::Insert { pos: 3, text: "d".into() },
                EditOp::Delete { pos: 3, text: "d".into() },
                EditOp::Delete { pos: 0, text: "a".into() },
            ]
        );
        // Draining empties the journal.
        assert!(b.take_journal().is_empty());
    }

    #[test]
    fn journal_replays_to_same_text() {
        // The ops a buffer emits, applied to a String, reproduce its content.
        let mut b = Buffer::from_text("", 0);
        for c in "héllo".chars() {
            b.insert_char(c);
        }
        b.set_cursor(2);
        b.insert_str("XY");
        let mut s = String::new();
        for op in b.take_journal() {
            match op {
                EditOp::Insert { pos, text } => {
                    let at = s.char_indices().nth(pos).map(|(b, _)| b).unwrap_or(s.len());
                    s.insert_str(at, &text);
                }
                EditOp::Delete { .. } => unreachable!(),
            }
        }
        assert_eq!(s, "héXYllo");
    }

    // --- document boundaries --------------------------------------------

    #[test]
    fn document_start_navigation_steps_between_docs() {
        // Two markers → three documents: "one", "two", "three".
        let mut b = Buffer::from_text("one\u{1e}\ntwo\u{1e}\nthree", 0);
        let doc2 = b.next_document_start().unwrap();
        b.set_cursor(doc2);
        assert_eq!(b.document_title(40), "two");
        let doc3 = b.next_document_start().unwrap();
        b.set_cursor(doc3);
        assert_eq!(b.document_title(40), "three");
        assert_eq!(b.next_document_start(), None, "last document");
        // Back up: into doc 2, then doc 1.
        b.set_cursor(b.prev_document_start().unwrap());
        assert_eq!(b.document_title(40), "two");
        b.set_cursor(b.prev_document_start().unwrap());
        assert_eq!(b.document_title(40), "one");
        assert_eq!(b.prev_document_start(), None, "first document");
    }

    #[test]
    fn marker_lines_are_recognised_and_hidden() {
        let b = Buffer::from_text("a\n\u{1e}\nb", 0);
        assert!(b.line_is_marker(1));
        assert!(!b.line_is_marker(0));
        // The marker never renders as a raw control char.
        assert_eq!(b.display_line(1, 0, 80), "");
    }

    #[test]
    fn insert_document_break_drops_a_marker_line() {
        // Mid-line: the line is broken first so the marker sits on its own line.
        let mut b = Buffer::from_text("end", 3);
        b.insert_document_break();
        assert!(b.line_is_marker(1));
        assert_eq!(b.cursor_line_col(), (2, 0), "cursor starts the new document");
    }

    // --- search (LEAP) ---------------------------------------------------

    #[test]
    fn search_forward_finds_next_match() {
        let b = buf("foo bar foo baz");
        assert_eq!(b.search_forward(0, "foo", false), Some(0));
        assert_eq!(b.search_forward(1, "foo", false), Some(8));
        assert_eq!(b.search_forward(9, "foo", false), None);
    }

    #[test]
    fn search_is_ascii_case_insensitive_when_asked() {
        let b = buf("Hello WORLD");
        assert_eq!(b.search_forward(0, "world", true), Some(6));
        assert_eq!(b.search_forward(0, "world", false), None);
    }

    #[test]
    fn search_backward_finds_previous_match() {
        let b = buf("foo bar foo baz");
        assert_eq!(b.search_backward(15, "foo", false), Some(8));
        assert_eq!(b.search_backward(8, "foo", false), Some(0));
        assert_eq!(b.search_backward(0, "foo", false), None);
    }

    #[test]
    fn line_col_at_accounts_for_tabs() {
        let b = buf("a\n\tx"); // line 1 is "\tx"; 'x' is at char col 1, display col TAB_WIDTH
        let x_idx = 3; // chars: a(0) \n(1) \t(2) x(3)
        assert_eq!(b.line_col_at(x_idx), (1, TAB_WIDTH));
    }
}
