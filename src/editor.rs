//! The UI-agnostic editor core: state, command dispatch, and the view model.
//!
//! On the Canon Cat branch there are **no files**: the buffer is reconstructed
//! from the SQLite [store](crate::store) on launch and every keystroke is
//! persisted back, so the workspace resumes exactly where you left off. The text
//! is one continuous stream divided into documents by boundary markers. **LEAP**
//! (`C-s`/`C-r`, see [`crate::leap`]) is incremental search-to-move.
//!
//! This module holds **no terminal or windowing types**. Front-ends feed it
//! [`KeyChord`]s via [`Editor::input`] and render the [`Frame`] returned by
//! [`Editor::compute_frame`]. See [`crate::frontend`].

use anyhow::Result;

use crate::buffer::Buffer;
use crate::calc;
use crate::echo::Echo;
use crate::input::{KeyChord, LogicalKey};
use crate::leap::{Dir, Leap};
use crate::statusline::StatusLine;
use crate::store::Store;
use crate::view::{Frame, FrameMeta, Row};

/// Compact the append-only log into a snapshot once it grows past this many rows.
const COMPACT_THRESHOLD: i64 = 2000;

/// A multi-key prefix awaiting its second key (Emacs-style `C-x ...`).
enum Prefix {
    CtrlX,
}

/// The running editor: document plus viewport, chrome, and command state.
pub struct Editor {
    buffer: Buffer,
    /// The persistent workspace; edits are appended here every keystroke.
    store: Store,
    /// Coalescing group id for the next batch of edits (bumped per keystroke).
    group: i64,
    /// Last `(cursor, top, left)` persisted, to avoid redundant state writes.
    saved_state: (usize, usize, usize),
    /// First visible line (vertical scroll offset).
    top: usize,
    /// First visible display column (horizontal scroll offset).
    left: usize,
    running: bool,
    /// Bottom-row transient message display (never an input prompt).
    echo: Echo,
    /// A pressed prefix key awaiting completion (e.g. `C-x`).
    prefix: Option<Prefix>,
    /// Active LEAP search session, if any.
    leap: Option<Leap>,
    /// Kill ring (single register for now; becomes a ring + OSC 52 later).
    kill_ring: String,
    /// Whether the previous command was a kill, so consecutive kills accumulate
    /// into one kill-ring entry (Emacs behaviour).
    last_was_kill: bool,
    /// Height of the text area on the last computed frame, for page up/down.
    last_text_rows: usize,
    /// Whether the last action was typing a character — the Canon Cat "wide"
    /// cursor state (solid highlight on the just-typed char + blinking cursor on
    /// the next position). Cleared by any move/leap → "narrow" (single cursor).
    #[cfg_attr(not(feature = "gui"), allow(dead_code))]
    wide: bool,
    /// Set by commands that need the front-end to fully repaint (recenter, LEAP
    /// land/cancel). Surfaced on the next [`Frame`] and then cleared.
    force_repaint: bool,
}

impl Editor {
    /// Build the editor from the persistent workspace, resuming the cursor and
    /// scroll exactly where the last session left them.
    pub fn new(store: Store) -> Result<Self> {
        let resume = store.resume()?;
        let buffer = Buffer::from_text(&resume.text, resume.cursor);
        let mut echo = Echo::default();
        echo.show("Type · C-s LEAP · Ctrl+PgUp/PgDn or C-x p/n: documents · M-Enter: new · C-q quit");
        Ok(Self {
            buffer,
            store,
            group: 0,
            saved_state: (resume.cursor, resume.top, resume.left),
            top: resume.top,
            left: resume.left,
            running: true,
            echo,
            prefix: None,
            leap: None,
            kill_ring: String::new(),
            last_was_kill: false,
            last_text_rows: 1,
            wide: false,
            force_repaint: false,
        })
    }

    /// Whether the editor wants to keep running (cleared by `C-q` / `C-x C-c`).
    pub fn running(&self) -> bool {
        self.running
    }

    /// Drain the buffer's edit journal into the append-only log. Front-ends call
    /// this after each [`input`](Self::input).
    pub fn persist_edits(&mut self) -> Result<()> {
        let ops = self.buffer.take_journal();
        if !ops.is_empty() {
            self.store.append(&ops, self.group)?;
            self.group += 1;
            self.buffer.mark_saved();
        }
        Ok(())
    }

    /// Flush + compact on a clean exit so the next launch is fast.
    pub fn shutdown(&mut self) -> Result<()> {
        self.persist_edits()?;
        if self.store.edit_count()? > COMPACT_THRESHOLD {
            // Snapshot the *raw* text (markers/tabs/newlines intact) — display_line
            // would strip markers and expand tabs, corrupting the workspace.
            let text = self.buffer.text();
            self.store.snapshot(&text)?;
        }
        Ok(())
    }

    // --- input dispatch ---------------------------------------------------

    /// Handle one logical key. Front-ends decode native events into a
    /// [`KeyChord`] (filtering key releases) and call this.
    pub fn input(&mut self, chord: KeyChord) {
        // Default to the "narrow" cursor; the text-insertion arms below set it
        // back to "wide". (Set before the early returns so leaping is narrow.)
        self.wide = false;
        // A LEAP session is active: keystrokes drive the search, not the buffer.
        if self.leap.is_some() {
            self.last_was_kill = false;
            self.leap_chord(chord);
            return;
        }
        // A prefix key is pending (e.g. C-x): this key completes it.
        if let Some(prefix) = self.prefix.take() {
            self.last_was_kill = false;
            self.prefix_chord(prefix, chord);
            return;
        }
        // Ordinary key: any previous echo message lasts only until now.
        self.echo.clear();

        let (ctrl, alt) = (chord.ctrl, chord.alt);
        // Consecutive kills accumulate; any other command breaks the run.
        let is_kill = ctrl && matches!(chord.key, LogicalKey::Char('k') | LogicalKey::Char('w'));
        if !is_kill {
            self.last_was_kill = false;
        }

        match chord.key {
            // ---- quit / prefix / cancel ------------------------------------
            LogicalKey::Char('q') if ctrl => self.running = false,
            LogicalKey::Char('x') if ctrl => {
                self.prefix = Some(Prefix::CtrlX);
                self.echo.show("C-x-");
            }
            LogicalKey::Char('g') if ctrl => {
                self.buffer.clear_mark();
                self.echo.show("Quit");
            }
            // C-Space sets the mark; motion/LEAP then extend the selection.
            LogicalKey::Char(' ') if ctrl => {
                self.buffer.set_mark();
                self.echo.show("Mark set");
            }

            // ---- LEAP (incremental search-to-move) -------------------------
            LogicalKey::Char('s') if ctrl => self.leap_start(Dir::Forward),
            LogicalKey::Char('r') if ctrl => self.leap_start(Dir::Backward),

            // ---- Emacs navigation (no arrows) ------------------------------
            LogicalKey::Char('b') if ctrl => self.buffer.move_left(),
            LogicalKey::Char('f') if ctrl => self.buffer.move_right(),
            LogicalKey::Char('p') if ctrl => self.buffer.move_up(),
            LogicalKey::Char('n') if ctrl => self.buffer.move_down(),
            LogicalKey::Char('a') if ctrl => self.buffer.move_home(),
            LogicalKey::Char('e') if ctrl => self.buffer.move_end(),
            LogicalKey::Char('b') if alt => self.buffer.move_word_backward(),
            LogicalKey::Char('f') if alt => self.buffer.move_word_forward(),
            LogicalKey::Char('v') if ctrl => self.page_down(),
            LogicalKey::Char('v') if alt => self.page_up(),
            // C-l: redraw and center the cursor's line (Emacs recenter).
            LogicalKey::Char('l') if ctrl => self.recenter(),
            // M-c (or M-=): evaluate the arithmetic on the current line in place.
            LogicalKey::Char('c') if alt => self.calc(),
            LogicalKey::Char('=') if alt => self.calc(),

            // ---- Emacs editing ---------------------------------------------
            LogicalKey::Char('d') if ctrl => self.erase(Buffer::delete_forward),
            LogicalKey::Char('h') if ctrl => self.erase(Buffer::backspace),
            LogicalKey::Char('k') if ctrl => self.kill(Buffer::kill_line, false),
            LogicalKey::Char('w') if ctrl => self.cut_or_kill_word(),
            LogicalKey::Char('w') if alt => self.copy(),
            LogicalKey::Char('y') if ctrl => {
                self.wide = true;
                self.yank();
            }

            // ---- arrows & named keys ---------------------------------------
            LogicalKey::Left => self.buffer.move_left(),
            LogicalKey::Right => self.buffer.move_right(),
            LogicalKey::Up => self.buffer.move_up(),
            LogicalKey::Down => self.buffer.move_down(),
            LogicalKey::Home => self.buffer.move_home(),
            LogicalKey::End => self.buffer.move_end(),

            // ---- right-thumb cluster: page, or jump documents with Ctrl -----
            LogicalKey::PageUp if ctrl => self.prev_document(),
            LogicalKey::PageDown if ctrl => self.next_document(),
            LogicalKey::PageUp => self.page_up(),
            LogicalKey::PageDown => self.page_down(),

            // M-Enter starts a new document (must precede the plain Enter arm).
            LogicalKey::Enter if alt => {
                self.wide = true;
                self.edit(Buffer::insert_document_break);
                self.echo.show("New document");
            }
            LogicalKey::Enter => {
                self.wide = true;
                self.edit(Buffer::insert_newline);
            }
            LogicalKey::Tab => {
                self.wide = true;
                self.edit(|b| b.insert_char('\t'));
            }
            LogicalKey::Backspace => self.erase(Buffer::backspace),
            LogicalKey::Delete => self.erase(Buffer::delete_forward),
            // Plain text input (Shift for uppercase is fine; Ctrl/Alt reserved).
            LogicalKey::Char(c) if !ctrl && !alt => {
                self.wide = true;
                self.edit(move |b| b.insert_char(c));
            }
            _ => {}
        }
    }

    /// Insert a committed string at the cursor (used by GUI IME commits).
    pub fn insert_text(&mut self, s: &str) {
        self.wide = true;
        self.buffer.insert_str(s);
    }

    /// Run a kill command: capture the removed text and add it to the kill ring,
    /// accumulating with the previous command if it was also a kill. A backward
    /// kill (`C-w`) prepends so the recovered text keeps document order; a
    /// forward kill (`C-k`) appends.
    fn kill<F: FnOnce(&mut Buffer) -> String>(&mut self, f: F, backward: bool) {
        let killed = f(&mut self.buffer);
        match (self.last_was_kill, backward) {
            (true, true) => self.kill_ring.insert_str(0, &killed),
            (true, false) => self.kill_ring.push_str(&killed),
            (false, _) => self.kill_ring = killed,
        }
        self.last_was_kill = true;
    }

    /// `C-y`: insert the kill ring at the cursor.
    fn yank(&mut self) {
        if self.kill_ring.is_empty() {
            self.echo.show("Kill ring empty");
        } else {
            let text = self.kill_ring.clone();
            self.buffer.insert_str(&text);
        }
    }

    /// The current kill-ring contents (for clipboard sync in the GUI).
    pub fn kill_ring(&self) -> &str {
        &self.kill_ring
    }

    /// Replace the kill ring (e.g. from the system clipboard before a yank).
    pub fn set_kill_ring(&mut self, text: String) {
        self.kill_ring = text;
        self.last_was_kill = false;
    }

    /// `C-v` / `M-v`: move the cursor a near-screenful down/up.
    fn page_down(&mut self) {
        for _ in 0..self.page_step() {
            self.buffer.move_down();
        }
    }

    fn page_up(&mut self) {
        for _ in 0..self.page_step() {
            self.buffer.move_up();
        }
    }

    fn page_step(&self) -> usize {
        self.last_text_rows.saturating_sub(1).max(1)
    }

    /// `C-l`: scroll so the cursor's line sits in the middle of the text area,
    /// and force a full repaint (clears any glitches). Like Emacs `recenter`.
    fn recenter(&mut self) {
        let (line, _) = self.buffer.cursor_line_col();
        self.top = line.saturating_sub(self.last_text_rows / 2);
        self.force_repaint = true;
    }

    /// Run a mutating edit on the buffer.
    fn edit<F: FnOnce(&mut Buffer)>(&mut self, f: F) {
        f(&mut self.buffer);
    }

    /// Backspace / Delete: erase the selection if there is one, else the single
    /// character via `op`.
    fn erase(&mut self, op: fn(&mut Buffer)) {
        if self.buffer.selection().is_some() {
            self.buffer.delete_selection();
        } else {
            op(&mut self.buffer);
        }
    }

    /// `M-w`: copy the selection to the kill ring (and clear the mark).
    fn copy(&mut self) {
        if let Some(text) = self.buffer.selection_text() {
            self.kill_ring = text;
            self.last_was_kill = false;
            self.buffer.clear_mark();
            self.echo.show("Copied");
        } else {
            self.echo.show("No selection");
        }
    }

    /// `C-w`: cut the selection if any (to the kill ring), else kill the word
    /// before the cursor.
    fn cut_or_kill_word(&mut self) {
        if self.buffer.selection().is_some() {
            if let Some(text) = self.buffer.delete_selection() {
                self.kill_ring = text;
                self.last_was_kill = false;
            }
        } else {
            self.kill(Buffer::backward_kill_word, true);
        }
    }

    /// `M-c` / `M-=`: evaluate the arithmetic on the current line and write the
    /// result in place — `2 + 2` becomes `2 + 2 = 4`. Re-running recomputes (the
    /// expression is taken from before the last `=`), so it's idempotent. The
    /// Canon Cat's inline calculator. (Selection-aware Calc lands with selection.)
    fn calc(&mut self) {
        // If there's a selection, evaluate it and replace it with the result
        // (the Cat's "select an expression, compute it" behaviour).
        if let Some(sel) = self.buffer.selection_text() {
            let expr = sel.trim();
            if !expr.is_empty() {
                match calc::eval(expr) {
                    Ok(value) => {
                        self.buffer.delete_selection();
                        self.buffer.insert_str(&fmt_num(value));
                        self.wide = true;
                    }
                    Err(e) => self.echo.show(format!("Calc: {e}")),
                }
                return;
            }
        }
        let line = self.buffer.current_line();
        // The expression is everything before the last `=` (or the whole line).
        let expr = match line.rsplit_once('=') {
            Some((lhs, _)) => lhs,
            None => &line,
        }
        .trim();
        if expr.is_empty() {
            self.echo.show("Calc: nothing to evaluate");
            return;
        }
        match calc::eval(expr) {
            Ok(value) => {
                self.buffer
                    .replace_current_line(&format!("{expr} = {}", fmt_num(value)));
                self.wide = true; // it inserted text
            }
            Err(e) => self.echo.show(format!("Calc: {e}")),
        }
    }

    /// Complete a pending prefix key (currently only `C-x`).
    fn prefix_chord(&mut self, prefix: Prefix, chord: KeyChord) {
        let ctrl = chord.ctrl;
        match prefix {
            Prefix::CtrlX => match chord.key {
                // Document jumps mirror C-p / C-n; brackets avoided (deep layer
                // on the Ergodox). Single-chord equivalents are Ctrl+PgUp/PgDn.
                LogicalKey::Char('p') => self.prev_document(),
                LogicalKey::Char('n') => self.next_document(),
                // C-x C-c: quit (everything is already saved).
                LogicalKey::Char('c') if ctrl => self.running = false,
                LogicalKey::Char('g') if ctrl => self.echo.show("Cancelled"),
                LogicalKey::Esc => self.echo.show("Cancelled"),
                _ => self.echo.show("C-x: undefined key"),
            },
        }
    }

    /// `C-x [`: move to the start of the current document, then to earlier ones.
    fn prev_document(&mut self) {
        let start = self.buffer.current_document_start();
        if self.buffer.cursor() > start {
            self.buffer.set_cursor(start);
            self.echo.show(self.doc_label());
        } else if let Some(p) = self.buffer.prev_document_start() {
            self.buffer.set_cursor(p);
            self.echo.show(self.doc_label());
        } else {
            self.echo.show("First document");
        }
    }

    /// `C-x ]`: move to the start of the next document.
    fn next_document(&mut self) {
        match self.buffer.next_document_start() {
            Some(p) => {
                self.buffer.set_cursor(p);
                self.echo.show(self.doc_label());
            }
            None => self.echo.show("Last document"),
        }
    }

    /// A short label for the document the cursor is in (its leading text).
    fn doc_label(&self) -> String {
        let title = self.buffer.document_title(40);
        if title.is_empty() {
            "Untitled document".to_string()
        } else {
            format!("Document: {title}")
        }
    }

    // --- LEAP --------------------------------------------------------------

    /// Begin a LEAP session in `dir` from the current cursor.
    fn leap_start(&mut self, dir: Dir) {
        self.leap = Some(Leap::new(dir, self.buffer.cursor()));
    }

    /// A keystroke while a LEAP session is active.
    fn leap_chord(&mut self, chord: KeyChord) {
        let (ctrl, alt) = (chord.ctrl, chord.alt);
        match chord.key {
            LogicalKey::Char('s') if ctrl => self.leap_repeat(Dir::Forward),
            LogicalKey::Char('r') if ctrl => self.leap_repeat(Dir::Backward),
            LogicalKey::Enter => self.leap_land(),
            LogicalKey::Esc => self.leap_cancel(),
            LogicalKey::Char('g') if ctrl => self.leap_cancel(),
            LogicalKey::Backspace => self.leap_backspace(),
            LogicalKey::Char('h') if ctrl => self.leap_backspace(),
            LogicalKey::Char(c) if !ctrl && !alt => self.leap_input(c),
            // Any other key lands the session and is then handled normally, so
            // LEAP is never a trap (C-q quits, arrows move, etc.).
            _ => {
                self.leap_land();
                self.input(chord);
            }
        }
    }

    fn leap_input(&mut self, c: char) {
        if let Some(l) = self.leap.as_mut() {
            l.query.push(c);
            l.wrapped = false;
        }
        self.leap_research(false);
    }

    fn leap_backspace(&mut self) {
        if let Some(l) = self.leap.as_mut() {
            l.query.pop();
            l.wrapped = false;
        }
        self.leap_research(false);
    }

    /// Jump to the next/previous occurrence (and switch direction if needed).
    fn leap_repeat(&mut self, dir: Dir) {
        match self.leap.as_mut() {
            Some(l) if !l.query.is_empty() => l.dir = dir,
            _ => return,
        }
        self.leap_research(true);
    }

    /// Search for the current query, wrapping around the buffer ends, and move
    /// the cursor to the match start. `repeat` distinguishes "jump to the next
    /// match" (`C-s`/`C-r`) from "extend the query in place" (typing) — they
    /// search from adjacent positions so typing doesn't skip the current match.
    fn leap_research(&mut self, repeat: bool) {
        let (query, dir, origin, base, ci) = match &self.leap {
            Some(l) => (
                l.query.clone(),
                l.dir,
                l.origin,
                l.matched.unwrap_or(l.origin),
                l.insensitive(),
            ),
            None => return,
        };
        if query.is_empty() {
            if let Some(l) = self.leap.as_mut() {
                l.matched = None;
                l.wrapped = false;
            }
            self.buffer.set_cursor(origin);
            return;
        }
        let len = self.buffer.len_chars();
        let (found, wrapped) = match dir {
            Dir::Forward => {
                // Extend: at/after the current match; repeat: strictly after.
                let from = if repeat { base + 1 } else { base };
                match self.buffer.search_forward(from.min(len), &query, ci) {
                    Some(m) => (Some(m), false),
                    None => (self.buffer.search_forward(0, &query, ci), true),
                }
            }
            Dir::Backward => {
                // Extend: at/before the current match; repeat: strictly before.
                let before = if repeat { base } else { (base + 1).min(len) };
                match self.buffer.search_backward(before, &query, ci) {
                    Some(m) => (Some(m), false),
                    None => (self.buffer.search_backward(len, &query, ci), true),
                }
            }
        };
        if let Some(l) = self.leap.as_mut() {
            l.matched = found;
            l.wrapped = found.is_some() && wrapped;
        }
        // Move to the match start; on a failing search, stay put.
        if let Some(m) = found {
            self.buffer.set_cursor(m);
        }
    }

    /// Land the session, keeping the cursor at the match.
    fn leap_land(&mut self) {
        self.leap = None;
        self.force_repaint = true; // clear the match highlight on the next repaint
    }

    /// Abandon the session, returning the cursor to where it started.
    fn leap_cancel(&mut self) {
        if let Some(l) = self.leap.take() {
            self.buffer.set_cursor(l.origin);
        }
        self.force_repaint = true;
        self.echo.show("Quit");
    }

    // --- view ---------------------------------------------------------------

    /// Adjust the viewport so the cursor stays on screen.
    fn scroll_to_cursor(&mut self, text_rows: usize, text_cols: usize) {
        let (line, _) = self.buffer.cursor_line_col();
        let col = self.buffer.cursor_display_col();

        if line < self.top {
            self.top = line;
        } else if text_rows > 0 && line >= self.top + text_rows {
            self.top = line + 1 - text_rows;
        }
        if col < self.left {
            self.left = col;
        } else if text_cols > 0 && col >= self.left + text_cols {
            self.left = col + 1 - text_cols;
        }
    }

    /// Produce a render-ready [`Frame`] for a viewport of `cols`×`rows` cells
    /// (the bottom two rows are chrome). Also persists the resume position when
    /// it has moved. No drawing happens here — that's the front-end's job.
    pub fn compute_frame(&mut self, cols: usize, rows: usize) -> Result<Frame> {
        let m = self.tick(cols, rows)?;
        let rows = (0..m.text_rows)
            .map(|r| self.row_at(m.top + r, m.left, cols))
            .collect();
        let highlights = (0..m.text_rows)
            .map(|r| self.row_highlight(m.top + r, m.left, cols))
            .collect();
        Ok(Frame {
            rows,
            status: m.status,
            echo: m.echo,
            ruler: self.ruler_string(m.left, cols),
            cursor: m.cursor,
            highlights,
            full_repaint: m.full_repaint,
            redraw_text: m.redraw_text,
        })
    }

    /// Advance the view (scroll-to-cursor + persist resume state) and return the
    /// frame *metadata* — everything except the row contents. Front-ends build
    /// rows from this via [`row_at`](Self::row_at)/[`rows_at`](Self::rows_at), so
    /// the row-construction logic lives in one place and the GUI doesn't build
    /// rows twice. `cols`/`rows` are the viewport size in cells.
    pub fn tick(&mut self, cols: usize, rows: usize) -> Result<FrameMeta> {
        // Three chrome rows at the bottom: ruler, status, echo.
        let text_rows = rows.saturating_sub(3);
        self.last_text_rows = text_rows; // for page up/down
        self.scroll_to_cursor(text_rows, cols);

        // Persist the resume position whenever it moves (cheap single-row write).
        let st = (self.buffer.cursor(), self.top, self.left);
        if st != self.saved_state {
            self.store.set_state(st.0, st.1, st.2)?;
            self.saved_state = st;
        }

        let status = self.status_string(cols);
        let echo: String = match &self.leap {
            Some(l) => l.label().chars().take(cols).collect(),
            None => self.echo.render(cols),
        };

        let (line, _) = self.buffer.cursor_line_col();
        let cursor = (
            self.buffer.cursor_display_col().saturating_sub(self.left),
            line.saturating_sub(self.top),
        );

        Ok(FrameMeta {
            top: self.top,
            left: self.left,
            text_rows,
            status,
            echo,
            cursor,
            full_repaint: std::mem::take(&mut self.force_repaint),
            redraw_text: self.leap.is_some(),
        })
    }

    /// The Canon Cat ruler: a character scale for the visible columns — `-`
    /// ticks, `+` every 5, and the tens digit every 10 (so `10`→`1`, `80`→`8`).
    /// The front-end overlays the blinking column indicator at the cursor.
    fn ruler_string(&self, left: usize, width: usize) -> String {
        (0..width)
            .map(|c| {
                let col = left + c;
                if col > 0 && col.is_multiple_of(10) {
                    std::char::from_digit((col / 10 % 10) as u32, 10).unwrap_or('-')
                } else if col.is_multiple_of(5) {
                    '+'
                } else {
                    '-'
                }
            })
            .collect()
    }

    /// One display row at absolute line `idx`, scrolled by `left`, clipped to
    /// `width`. The single source of row construction.
    fn row_at(&self, idx: usize, left: usize, width: usize) -> Row {
        if idx >= self.buffer.len_lines() {
            Row::Tilde
        } else if self.buffer.line_is_marker(idx) {
            Row::MarkerRule
        } else {
            Row::Text(self.buffer.display_line(idx, left, width))
        }
    }

    /// Whether the cursor is in the Canon Cat "wide" state (last action was
    /// typing → show the solid erase highlight on the previous char). `false`
    /// after a move/leap → "narrow" single blinking cursor.
    #[cfg(feature = "gui")]
    pub fn cursor_wide(&self) -> bool {
        self.wide
    }

    /// Render an arbitrary row window `[top, top + count)` — lets the GUI draw
    /// the rows around an in-progress scroll animation, not just the settled
    /// viewport.
    #[cfg(feature = "gui")]
    pub fn rows_at(&self, top: usize, count: usize, left: usize, width: usize) -> Vec<Row> {
        (0..count).map(|i| self.row_at(top + i, left, width)).collect()
    }

    /// The inverse-highlight span on absolute line `idx`, in display columns
    /// relative to `left` and clipped to `width`: the **selection** if one is
    /// active (so it spans multiple rows), else a **LEAP match** on that line.
    /// Both front-ends call this per visible row.
    pub fn row_highlight(&self, idx: usize, left: usize, width: usize) -> Option<(usize, usize)> {
        if idx >= self.buffer.len_lines() {
            return None; // a blank row past the end of the buffer
        }
        if let Some((s, e)) = self.buffer.selection() {
            let (ls, le) = self.buffer.line_char_bounds(idx);
            let from = s.max(ls);
            let to = e.min(le);
            if from >= to {
                return None;
            }
            let a = self.buffer.line_col_at(from).1.saturating_sub(left);
            let b = self.buffer.line_col_at(to).1.saturating_sub(left).min(width);
            return (b > a).then_some((a, b));
        }
        let (line, lo, hi) = self.leap_highlight()?;
        if line != idx {
            return None;
        }
        let a = lo.saturating_sub(left);
        let b = hi.saturating_sub(left).min(width);
        (b > a).then_some((a, b))
    }

    /// Whether a selection is active (GUI suppresses the wide single-cell cursor
    /// highlight while a span is selected).
    #[cfg(feature = "gui")]
    pub fn selection_active(&self) -> bool {
        self.buffer.selection().is_some()
    }

    /// The current LEAP match as `(line, start_dcol, end_dcol)` in absolute
    /// display columns, or `None` when not leaping / no match / multi-line.
    fn leap_highlight(&self) -> Option<(usize, usize, usize)> {
        let l = self.leap.as_ref()?;
        let m = l.matched?;
        let qlen = l.query.chars().count();
        let (line, start) = self.buffer.line_col_at(m);
        let (end_line, end) = self.buffer.line_col_at(m + qlen);
        (end_line == line).then_some((line, start, end))
    }

    fn status_string(&self, width: usize) -> String {
        let (line, col) = self.buffer.cursor_line_col();
        let title = self.buffer.document_title(40);
        let name = if title.is_empty() {
            "·leap·".to_string()
        } else {
            title
        };

        StatusLine {
            name,
            // Every keystroke is persisted before we render, so this is in
            // practice always false — the Cat never shows "unsaved".
            dirty: self.buffer.is_dirty(),
            readonly: false,
            line: line + 1,
            col: col + 1,
            total_lines: self.buffer.len_lines(),
        }
        .render(width)
    }
}

/// Format a Calc result: drop the fractional part for whole numbers, else trim
/// trailing zeros (so `4.0`→`4`, `2.50`→`2.5`, `0.1+0.2`→`0.3`).
fn fmt_num(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e15 {
        (v as i64).to_string()
    } else {
        let s = format!("{v:.10}");
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::view::Row;

    /// An editor over a fresh in-memory workspace seeded with `text`.
    fn editor_with(text: &str) -> Editor {
        let mut store = Store::open_in_memory().unwrap();
        store
            .append(&[crate::store::EditOp::Insert { pos: 0, text: text.into() }], 0)
            .unwrap();
        Editor::new(store).unwrap()
    }

    fn ctrl(c: char) -> KeyChord {
        KeyChord::ctrl(c)
    }
    fn ch(c: char) -> KeyChord {
        KeyChord::plain(LogicalKey::Char(c))
    }

    #[test]
    fn typing_inserts_and_persists() {
        let mut e = editor_with("");
        for c in "hi".chars() {
            e.input(ch(c));
        }
        e.persist_edits().unwrap();
        // Reconstruct from the store: the edits were appended.
        assert_eq!(e.store.resume().unwrap().text, "hi");
    }

    #[test]
    fn ctrl_q_stops_running() {
        let mut e = editor_with("x");
        assert!(e.running());
        e.input(ctrl('q'));
        assert!(!e.running());
    }

    #[test]
    fn leap_forward_moves_cursor_to_match() {
        let mut e = editor_with("alpha beta gamma");
        e.input(ctrl('s')); // start LEAP forward
        for c in "beta".chars() {
            e.input(ch(c));
        }
        e.input(KeyChord::plain(LogicalKey::Enter)); // land
        assert_eq!(e.buffer.cursor(), 6); // "beta" starts at char 6
    }

    #[test]
    fn calc_evaluates_current_line_idempotently() {
        let mut e = editor_with("2 + 3 * 4");
        e.input(KeyChord::alt('c'));
        assert_eq!(e.buffer.current_line(), "2 + 3 * 4 = 14");
        // Re-running recomputes from the expression before `=` — no drift.
        e.input(KeyChord::alt('c'));
        assert_eq!(e.buffer.current_line(), "2 + 3 * 4 = 14");
    }

    fn right(e: &mut Editor, n: usize) {
        for _ in 0..n {
            e.input(KeyChord::plain(LogicalKey::Right));
        }
    }

    #[test]
    fn mark_then_motion_cut_selection() {
        let mut e = editor_with("hello world");
        e.input(KeyChord::ctrl(' ')); // set mark at 0
        right(&mut e, 5); // select "hello"
        assert_eq!(e.buffer.selection(), Some((0, 5)));
        e.input(ctrl('w')); // cut
        assert_eq!(e.buffer.current_line(), " world");
        assert_eq!(e.kill_ring(), "hello");
        assert_eq!(e.buffer.selection(), None);
    }

    #[test]
    fn copy_keeps_buffer_and_clears_mark() {
        let mut e = editor_with("abcdef");
        e.input(KeyChord::ctrl(' '));
        right(&mut e, 3);
        e.input(KeyChord::alt('w')); // M-w copy "abc"
        assert_eq!(e.kill_ring(), "abc");
        assert_eq!(e.buffer.current_line(), "abcdef"); // unchanged
        assert_eq!(e.buffer.selection(), None); // mark cleared
    }

    #[test]
    fn erase_deletes_selection() {
        let mut e = editor_with("abcdef");
        e.input(KeyChord::ctrl(' '));
        right(&mut e, 3);
        e.input(KeyChord::plain(LogicalKey::Backspace)); // erase "abc"
        assert_eq!(e.buffer.current_line(), "def");
    }

    #[test]
    fn selection_with_viewport_past_end_does_not_panic() {
        // Regression: with a selection active, the renderer queries row_highlight
        // for blank rows past the buffer end — must not index the rope OOB.
        let mut e = editor_with("a\nb");
        e.input(KeyChord::ctrl(' ')); // mark
        e.input(KeyChord::plain(LogicalKey::Down)); // selection [0,2)
        assert!(e.buffer.selection().is_some());
        // 10-row viewport over a 2-line buffer → rows past the end are queried.
        let f = e.compute_frame(20, 10).unwrap();
        assert_eq!(f.highlights.len(), 7); // 10 rows − 3 chrome rows

        // Directly hit a past-end row too.
        assert_eq!(e.row_highlight(50, 0, 20), None);
    }

    #[test]
    fn calc_evaluates_selected_expression() {
        let mut e = editor_with("x = 2+3 done");
        right(&mut e, 4); // before '2'
        e.input(KeyChord::ctrl(' '));
        right(&mut e, 3); // select "2+3"
        e.input(KeyChord::alt('c'));
        assert_eq!(e.buffer.current_line(), "x = 5 done");
    }

    #[test]
    fn compute_frame_lays_out_rows_status_and_cursor() {
        let mut e = editor_with("one\ntwo\nthree");
        // 6 rows total → 3 text rows + ruler + status + echo.
        let f = e.compute_frame(20, 6).unwrap();
        assert_eq!(f.rows.len(), 3);
        assert_eq!(f.rows[0], Row::Text("one".into()));
        assert_eq!(f.rows[1], Row::Text("two".into()));
        assert!(f.status.contains("Ln 1"));
        assert!(!f.ruler.is_empty());
        assert_eq!(f.cursor, (0, 0));
    }

    #[test]
    fn compute_frame_marks_marker_and_tilde_rows() {
        let mut e = editor_with("a\n\u{1e}\nb"); // line 1 is a document marker
        let f = e.compute_frame(20, 10).unwrap();
        assert_eq!(f.rows[0], Row::Text("a".into()));
        assert_eq!(f.rows[1], Row::MarkerRule);
        assert_eq!(f.rows[2], Row::Text("b".into()));
        assert_eq!(f.rows[3], Row::Tilde); // past end of buffer
    }
}
