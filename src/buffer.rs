//! The editable document.
//!
//! A [`Buffer`] wraps a [`ropey::Rope`] and owns the editing state that goes
//! with it: the cursor, a sticky goal column for vertical movement, the dirty
//! flag, and a monotonic version counter (used later to discard stale results
//! from background workers — see `docs/DESIGN.md`).
//!
//! The cursor is a single `char` index into the rope (`0..=len_chars`). Line and
//! column are derived from it on demand via the rope's index maps. Columns come
//! in two flavours: **char columns** (used for movement and the status line) and
//! **display columns** (tabs expanded), used for rendering and scrolling.

use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use ropey::Rope;

/// Width a tab expands to on screen. Will become a compile-time theme/option;
/// hard-coded for now.
const TAB_WIDTH: usize = 4;

/// An open document plus its cursor and edit state.
pub struct Buffer {
    rope: Rope,
    path: Option<PathBuf>,
    readonly: bool,
    /// Cursor as a char index into the rope, in `0..=rope.len_chars()`.
    cursor: usize,
    /// Sticky target display-agnostic char column for up/down; cleared by any
    /// horizontal move or edit.
    goal_col: Option<usize>,
    dirty: bool,
    version: u64,
}

impl Buffer {
    /// Open `path` (reading it if it exists, otherwise an empty buffer that will
    /// be created on save), or an empty unnamed buffer when `path` is `None`.
    pub fn open(path: Option<PathBuf>, readonly: bool) -> Result<Self> {
        let rope = match &path {
            Some(p) if p.exists() => {
                let file =
                    File::open(p).with_context(|| format!("opening {}", p.display()))?;
                Rope::from_reader(BufReader::new(file))
                    .with_context(|| format!("reading {}", p.display()))?
            }
            _ => Rope::new(),
        };
        Ok(Self {
            rope,
            path,
            readonly,
            cursor: 0,
            goal_col: None,
            dirty: false,
            version: 0,
        })
    }

    // --- accessors -------------------------------------------------------

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub fn readonly(&self) -> bool {
        self.readonly
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Number of lines for display. A trailing newline yields a final empty
    /// line, which is a valid cursor position.
    pub fn len_lines(&self) -> usize {
        self.rope.len_lines()
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
            if c == '\n' || c == '\r' {
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

    /// Place the cursor at the start of `line` (1-based), clamped.
    pub fn goto_line(&mut self, line: usize) {
        let target = line.saturating_sub(1).min(self.rope.len_lines().saturating_sub(1));
        self.cursor = self.rope.line_to_char(target);
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

    // --- editing ---------------------------------------------------------

    pub fn insert_char(&mut self, c: char) {
        if self.readonly {
            return;
        }
        self.rope.insert_char(self.cursor, c);
        self.cursor += 1;
        self.on_edit();
    }

    pub fn insert_newline(&mut self) {
        self.insert_char('\n');
    }

    /// Delete the character before the cursor (Backspace).
    pub fn backspace(&mut self) {
        if self.readonly || self.cursor == 0 {
            return;
        }
        self.rope.remove(self.cursor - 1..self.cursor);
        self.cursor -= 1;
        self.on_edit();
    }

    /// Delete the character at the cursor (Delete).
    pub fn delete_forward(&mut self) {
        if self.readonly || self.cursor >= self.rope.len_chars() {
            return;
        }
        self.rope.remove(self.cursor..self.cursor + 1);
        self.on_edit();
    }

    fn on_edit(&mut self) {
        self.dirty = true;
        self.version += 1;
        self.goal_col = None;
    }

    // --- helpers ---------------------------------------------------------

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
        let mut b = Buffer::open(None, false).unwrap();
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
        b.goto_line(1); // line 0
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
        b.goto_line(1);
        b.move_end(); // end of "ab"
        b.delete_forward();
        assert_eq!(b.display_line(0, 0, 80), "abcd");
        assert_eq!(b.len_lines(), 1);
    }

    #[test]
    fn move_end_stops_before_newline() {
        let mut b = buf("hello\nx");
        b.goto_line(1);
        b.move_end();
        assert_eq!(b.cursor_line_col(), (0, 5));
    }

    #[test]
    fn tabs_expand_for_display_and_cursor_column() {
        let b = buf("\tx"); // tab then x
        // Tab expands to a full TAB_WIDTH at column 0.
        assert_eq!(b.display_line(0, 0, 80), "    x");
        // Char column is 2 (tab, x); display column accounts for the expansion.
        assert_eq!(b.cursor_line_col(), (0, 2));
        assert_eq!(b.cursor_display_col(), TAB_WIDTH + 1);
    }

    #[test]
    fn readonly_buffer_rejects_edits() {
        let mut b = Buffer::open(None, true).unwrap();
        b.insert_char('x');
        b.backspace();
        assert_eq!(b.len_lines(), 1);
        assert_eq!(b.cursor_line_col(), (0, 0));
        assert!(!b.is_dirty());
    }
}
