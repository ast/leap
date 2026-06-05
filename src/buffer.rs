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

use std::ffi::{OsStr, OsString};
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use ropey::Rope;

/// Width a tab expands to on screen. Will become a compile-time theme/option;
/// hard-coded for now.
const TAB_WIDTH: usize = 4;

/// Line-ending style. The in-memory rope is always normalised to `\n`; the
/// original style is remembered and reapplied on save so files keep their EOLs.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Eol {
    Lf,
    Crlf,
}

/// An open document plus its cursor and edit state.
pub struct Buffer {
    rope: Rope,
    path: Option<PathBuf>,
    readonly: bool,
    eol: Eol,
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
        let (rope, eol) = match &path {
            Some(p) if p.exists() => read_file(p)?,
            _ => (Rope::new(), Eol::Lf),
        };
        Ok(Self {
            rope,
            path,
            readonly,
            eol,
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

    // --- file I/O --------------------------------------------------------

    /// Save to the buffer's current path. The path must already be set (callers
    /// prompt for a name via "save as" otherwise).
    pub fn save(&mut self) -> Result<()> {
        let path = self.path.clone().context("no file name")?;
        self.write_to(&path)?;
        self.dirty = false;
        Ok(())
    }

    /// Save to `path`, adopting it as the buffer's path going forward.
    pub fn save_as(&mut self, path: PathBuf) -> Result<()> {
        self.write_to(&path)?;
        self.path = Some(path);
        self.dirty = false;
        Ok(())
    }

    /// Replace the buffer contents with `path` (or an empty buffer if it does
    /// not yet exist), adopting it as the buffer's path. Returns whether the
    /// file already existed.
    pub fn load(&mut self, path: &Path) -> Result<bool> {
        let (rope, eol, existed) = if path.exists() {
            let (rope, eol) = read_file(path)?;
            (rope, eol, true)
        } else {
            (Rope::new(), Eol::Lf, false)
        };
        self.rope = rope;
        self.eol = eol;
        self.path = Some(path.to_path_buf());
        self.cursor = 0;
        self.goal_col = None;
        self.dirty = false;
        self.version += 1;
        Ok(existed)
    }

    /// Atomically write the buffer to `path`: serialise (reapplying the EOL
    /// style) to a temp file in the same directory, then rename over the target
    /// so a crash mid-write can't truncate the original.
    fn write_to(&self, path: &Path) -> Result<()> {
        let text: String = self.rope.chunks().collect();
        let data = match self.eol {
            Eol::Lf => text,
            Eol::Crlf => text.replace('\n', "\r\n"),
        };

        let tmp = temp_path(path);
        let write = || -> Result<()> {
            let mut file = File::create(&tmp)
                .with_context(|| format!("creating {}", tmp.display()))?;
            file.write_all(data.as_bytes())?;
            file.sync_all()?;
            Ok(())
        };
        if let Err(e) = write() {
            let _ = fs::remove_file(&tmp);
            return Err(e);
        }

        // Keep the original file's permissions across the replace.
        if let Ok(meta) = fs::metadata(path) {
            let _ = fs::set_permissions(&tmp, meta.permissions());
        }
        fs::rename(&tmp, path).inspect_err(|_| {
            let _ = fs::remove_file(&tmp);
        })?;
        Ok(())
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

    /// Delete the character at the cursor (Delete / `C-d`).
    pub fn delete_forward(&mut self) {
        if self.readonly || self.cursor >= self.rope.len_chars() {
            return;
        }
        self.rope.remove(self.cursor..self.cursor + 1);
        self.on_edit();
    }

    /// Insert a string at the cursor (used by yank); advances past it.
    pub fn insert_str(&mut self, s: &str) {
        if self.readonly || s.is_empty() {
            return;
        }
        self.rope.insert(self.cursor, s);
        self.cursor += s.chars().count();
        self.on_edit();
    }

    /// `C-k`: kill from the cursor to end of line, or — if already there — the
    /// line break itself (joining the next line). Returns the removed text so
    /// the editor can push it onto the kill ring.
    pub fn kill_line(&mut self) -> String {
        if self.readonly {
            return String::new();
        }
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
        self.on_edit();
        killed
    }

    /// `C-w`: kill the word before the cursor. Returns the removed text.
    pub fn backward_kill_word(&mut self) -> String {
        if self.readonly {
            return String::new();
        }
        let target = self.word_backward_pos();
        if target >= self.cursor {
            return String::new();
        }
        let killed: String = self.rope.slice(target..self.cursor).chars().collect();
        self.rope.remove(target..self.cursor);
        self.cursor = target;
        self.on_edit();
        killed
    }

    fn on_edit(&mut self) {
        self.dirty = true;
        self.version += 1;
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

/// Read `path` as UTF-8, detect its EOL style, and normalise CRLF to LF for the
/// in-memory rope.
fn read_file(path: &Path) -> Result<(Rope, Eol)> {
    let text = fs::read_to_string(path)
        .with_context(|| format!("reading {}", path.display()))?;
    let eol = if text.contains("\r\n") { Eol::Crlf } else { Eol::Lf };
    let normalized = match eol {
        Eol::Crlf => text.replace("\r\n", "\n"),
        Eol::Lf => text,
    };
    Ok((Rope::from_str(&normalized), eol))
}

/// A hidden sibling temp path in the target's directory, so the final rename is
/// atomic (same filesystem).
fn temp_path(path: &Path) -> PathBuf {
    let mut name = OsString::from(".");
    name.push(path.file_name().unwrap_or_else(|| OsStr::new("leap")));
    name.push(".leap-tmp");
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.join(name),
        _ => PathBuf::from(name),
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
    fn word_movement_skips_punctuation() {
        let mut b = buf("foo, bar_baz qux");
        b.goto_line(1); // cursor at start
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
        b.goto_line(1);
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

    #[test]
    fn readonly_buffer_rejects_edits() {
        let mut b = Buffer::open(None, true).unwrap();
        b.insert_char('x');
        b.backspace();
        assert_eq!(b.len_lines(), 1);
        assert_eq!(b.cursor_line_col(), (0, 0));
        assert!(!b.is_dirty());
    }

    // --- file I/O --------------------------------------------------------

    use std::sync::atomic::{AtomicU32, Ordering};

    /// A unique temp path per call, scoped to the test process.
    fn temp_file(tag: &str) -> PathBuf {
        static N: AtomicU32 = AtomicU32::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("leap_test_{}_{tag}_{n}", std::process::id()))
    }

    #[test]
    fn save_as_then_reopen_roundtrips() {
        let path = temp_file("roundtrip");
        let mut b = buf("hello\nworld");
        assert!(b.is_dirty());
        b.save_as(path.clone()).unwrap();
        assert!(!b.is_dirty(), "save clears the dirty flag");
        assert_eq!(b.path(), Some(path.as_path()));

        let reopened = Buffer::open(Some(path.clone()), false).unwrap();
        assert_eq!(reopened.display_line(0, 0, 80), "hello");
        assert_eq!(reopened.display_line(1, 0, 80), "world");
        fs::remove_file(&path).ok();
    }

    #[test]
    fn save_writes_exact_bytes() {
        let path = temp_file("bytes");
        let mut b = buf("ab\ncd");
        b.save_as(path.clone()).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "ab\ncd");
        fs::remove_file(&path).ok();
    }

    #[test]
    fn crlf_is_normalized_in_memory_and_restored_on_save() {
        let path = temp_file("crlf");
        fs::write(&path, b"a\r\nb\r\nc").unwrap();
        let mut b = Buffer::open(Some(path.clone()), false).unwrap();
        // In memory the lines are clean (no stray '\r').
        assert_eq!(b.display_line(0, 0, 80), "a");
        assert_eq!(b.eol, Eol::Crlf);
        // Edit and save: CRLF endings come back.
        b.goto_line(b.len_lines());
        b.move_end();
        b.insert_char('!');
        b.save().unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"a\r\nb\r\nc!");
        fs::remove_file(&path).ok();
    }

    #[test]
    fn load_missing_file_starts_empty_new() {
        let path = temp_file("missing");
        let mut b = buf("scratch contents");
        let existed = b.load(&path).unwrap();
        assert!(!existed);
        assert_eq!(b.len_lines(), 1);
        assert_eq!(b.cursor_line_col(), (0, 0));
        assert!(!b.is_dirty());
        assert_eq!(b.path(), Some(path.as_path()));
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

    #[test]
    fn save_preserves_lf_eol() {
        let path = temp_file("lf");
        fs::write(&path, b"x\ny").unwrap();
        let mut b = Buffer::open(Some(path.clone()), false).unwrap();
        b.goto_line(b.len_lines());
        b.move_end();
        b.insert_char('z');
        b.save().unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"x\nyz");
        fs::remove_file(&path).ok();
    }
}
