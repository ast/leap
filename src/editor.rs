//! Top-level editor state and the event loop.
//!
//! Buffer + diff renderer + two-row chrome (status line and a **display-only**
//! [`Echo`] line — no modal minibuffer). On the Canon Cat branch there are **no
//! files**: the buffer is reconstructed from the SQLite [store](crate::store) on
//! launch and every keystroke is persisted back, so the workspace resumes
//! exactly where you left off. The text is one continuous stream divided into
//! documents by boundary markers (`C-x [` / `C-x ]` to jump, `C-x C-n` for a new
//! one). **LEAP** (`C-s`/`C-r`, see [`crate::leap`]) is incremental
//! search-to-move. Because nothing is ever unsaved, `C-q` just quits.

use std::io::{self, Write};

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::style::{Attribute, Print, SetAttribute};
use crossterm::{cursor, queue, terminal};

use crate::buffer::Buffer;
use crate::echo::Echo;
use crate::leap::{Dir, Leap};
use crate::statusline::StatusLine;
use crate::store::Store;

/// Compact the append-only log into a snapshot once it grows past this many rows.
const COMPACT_THRESHOLD: i64 = 2000;

/// A multi-key prefix awaiting its second key (Emacs-style `C-x ...`).
enum Prefix {
    CtrlX,
}

/// The running editor: document plus viewport, chrome, and loop state.
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
    /// Kill ring (single register for now; becomes a ring + OSC 52 in M6).
    kill_ring: String,
    /// Whether the previous command was a kill, so consecutive kills accumulate
    /// into one kill-ring entry (Emacs behaviour).
    last_was_kill: bool,
    /// Height of the text area on the last render, for page up/down (`C-v`/`M-v`).
    last_text_rows: usize,
    /// Diff-render cache: the exact (full-width) string currently shown on each
    /// screen row. A row is only re-emitted when its desired content differs,
    /// so a pure cursor move repaints just the status line, not the whole screen.
    frame: Vec<String>,
    frame_cols: u16,
    frame_rows: u16,
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
            frame: Vec::new(),
            frame_cols: 0,
            frame_rows: 0,
        })
    }

    /// Event loop: handle a key, flush its edits to the store, repaint. Blocking
    /// reads — there is no animated gesture to keep alive (quitting is instant
    /// because the workspace is always saved).
    pub fn run(&mut self) -> Result<()> {
        self.render()?;
        while self.running {
            match event::read()? {
                Event::Key(key) => {
                    self.on_key(key);
                    self.persist_edits()?;
                }
                Event::Resize(_, _) => {}
                _ => {}
            }
            if self.running {
                self.render()?;
            }
        }
        // Clean exit: a final flush + log compaction so next launch is fast.
        self.persist_edits()?;
        if self.store.edit_count()? > COMPACT_THRESHOLD {
            let text = self.buffer_text();
            self.store.snapshot(&text)?;
        }
        Ok(())
    }

    /// Drain the buffer's edit journal into the append-only log.
    fn persist_edits(&mut self) -> Result<()> {
        let ops = self.buffer.take_journal();
        if !ops.is_empty() {
            self.store.append(&ops, self.group)?;
            self.group += 1;
            self.buffer.mark_saved();
        }
        Ok(())
    }

    /// The full stream as a `String` (for snapshot compaction).
    fn buffer_text(&self) -> String {
        (0..self.buffer.len_lines())
            .map(|l| {
                let mut s = self.buffer.display_line(l, 0, usize::MAX);
                s.push('\n');
                s
            })
            .collect()
    }

    fn on_key(&mut self, key: KeyEvent) {
        // Key releases (Kitty protocol) carry no command on this branch.
        if key.kind == KeyEventKind::Release {
            return;
        }

        // A LEAP session is active: keystrokes drive the search, not the buffer.
        if self.leap.is_some() {
            self.last_was_kill = false;
            self.leap_key(key);
            return;
        }
        // A prefix key is pending (e.g. C-x): this key completes it.
        if let Some(prefix) = self.prefix.take() {
            self.last_was_kill = false;
            self.prefix_key(prefix, key);
            return;
        }
        // Ordinary key: any previous echo message lasts only until now.
        self.echo.clear();

        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        // Consecutive kills accumulate; any other command breaks the run.
        let is_kill = ctrl && matches!(key.code, KeyCode::Char('k') | KeyCode::Char('w'));
        if !is_kill {
            self.last_was_kill = false;
        }

        match key.code {
            // ---- quit / prefix / cancel ------------------------------------
            KeyCode::Char('q') if ctrl => self.running = false,
            KeyCode::Char('x') if ctrl => {
                self.prefix = Some(Prefix::CtrlX);
                self.echo.show("C-x-");
            }
            // C-g is Emacs keyboard-quit (abort).
            KeyCode::Char('g') if ctrl => self.echo.show("Quit"),

            // ---- LEAP (incremental search-to-move) -------------------------
            KeyCode::Char('s') if ctrl => self.leap_start(Dir::Forward),
            KeyCode::Char('r') if ctrl => self.leap_start(Dir::Backward),

            // ---- Emacs navigation (no arrows) ------------------------------
            KeyCode::Char('b') if ctrl => self.buffer.move_left(),
            KeyCode::Char('f') if ctrl => self.buffer.move_right(),
            KeyCode::Char('p') if ctrl => self.buffer.move_up(),
            KeyCode::Char('n') if ctrl => self.buffer.move_down(),
            KeyCode::Char('a') if ctrl => self.buffer.move_home(),
            KeyCode::Char('e') if ctrl => self.buffer.move_end(),
            KeyCode::Char('b') if alt => self.buffer.move_word_backward(),
            KeyCode::Char('f') if alt => self.buffer.move_word_forward(),
            KeyCode::Char('v') if ctrl => self.page_down(),
            KeyCode::Char('v') if alt => self.page_up(),
            // C-l: redraw and center the cursor's line (Emacs recenter).
            KeyCode::Char('l') if ctrl => self.recenter(),

            // ---- Emacs editing ---------------------------------------------
            KeyCode::Char('d') if ctrl => self.edit(Buffer::delete_forward),
            KeyCode::Char('h') if ctrl => self.edit(Buffer::backspace),
            KeyCode::Char('k') if ctrl => self.kill(Buffer::kill_line, false),
            KeyCode::Char('w') if ctrl => self.kill(Buffer::backward_kill_word, true),
            KeyCode::Char('y') if ctrl => self.yank(),

            // ---- arrows & named keys ---------------------------------------
            KeyCode::Left => self.buffer.move_left(),
            KeyCode::Right => self.buffer.move_right(),
            KeyCode::Up => self.buffer.move_up(),
            KeyCode::Down => self.buffer.move_down(),
            KeyCode::Home => self.buffer.move_home(),
            KeyCode::End => self.buffer.move_end(),

            // ---- right-thumb cluster: page, or jump documents with Ctrl -----
            KeyCode::PageUp if ctrl => self.prev_document(),
            KeyCode::PageDown if ctrl => self.next_document(),
            KeyCode::PageUp => self.page_up(),
            KeyCode::PageDown => self.page_down(),

            // M-Enter starts a new document (must precede the plain Enter arm).
            KeyCode::Enter if alt => {
                self.edit(Buffer::insert_document_break);
                self.echo.show("New document");
            }
            KeyCode::Enter => self.edit(Buffer::insert_newline),
            KeyCode::Tab => self.edit(|b| b.insert_char('\t')),
            KeyCode::Backspace => self.edit(Buffer::backspace),
            KeyCode::Delete => self.edit(Buffer::delete_forward),
            // Plain text input (Shift for uppercase is fine; Ctrl/Alt are reserved).
            KeyCode::Char(c) if !ctrl && !alt => self.edit(move |b| b.insert_char(c)),
            _ => {}
        }
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
    /// and force a full repaint (clears any terminal glitches). Like Emacs
    /// `recenter`. `scroll_to_cursor` leaves `top` alone since the cursor is now
    /// comfortably on screen.
    fn recenter(&mut self) {
        let (line, _) = self.buffer.cursor_line_col();
        self.top = line.saturating_sub(self.last_text_rows / 2);
        self.frame_cols = 0; // invalidate the diff cache → clear-and-repaint
    }

    /// Run a mutating edit on the buffer.
    fn edit<F: FnOnce(&mut Buffer)>(&mut self, f: F) {
        f(&mut self.buffer);
    }

    /// Complete a pending prefix key (currently only `C-x`).
    fn prefix_key(&mut self, prefix: Prefix, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match prefix {
            Prefix::CtrlX => match key.code {
                // Document jumps mirror C-p / C-n; brackets avoided (deep layer
                // on the Ergodox). Single-chord equivalents are Ctrl+PgUp/PgDn.
                KeyCode::Char('p') => self.prev_document(),
                KeyCode::Char('n') => self.next_document(),
                // C-x C-c: quit (everything is already saved).
                KeyCode::Char('c') if ctrl => self.running = false,
                KeyCode::Char('g') if ctrl => self.echo.show("Cancelled"),
                KeyCode::Esc => self.echo.show("Cancelled"),
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
    fn leap_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        match key.code {
            KeyCode::Char('s') if ctrl => self.leap_repeat(Dir::Forward),
            KeyCode::Char('r') if ctrl => self.leap_repeat(Dir::Backward),
            KeyCode::Enter => self.leap_land(),
            KeyCode::Esc => self.leap_cancel(),
            KeyCode::Char('g') if ctrl => self.leap_cancel(),
            KeyCode::Backspace => self.leap_backspace(),
            KeyCode::Char('h') if ctrl => self.leap_backspace(),
            KeyCode::Char(c) if !ctrl && !alt => self.leap_input(c),
            // Any other key lands the session and is then handled normally, so
            // LEAP is never a trap (C-q quits, arrows move, etc.).
            _ => {
                self.leap_land();
                self.on_key(key);
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
        self.frame_cols = 0; // clear the match highlight on the next repaint
    }

    /// Abandon the session, returning the cursor to where it started.
    fn leap_cancel(&mut self) {
        if let Some(l) = self.leap.take() {
            self.buffer.set_cursor(l.origin);
        }
        self.frame_cols = 0;
        self.echo.show("Quit");
    }

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

    fn render(&mut self) -> Result<()> {
        let (cols, rows) = terminal::size()?;
        // Bottom two rows are chrome: status line, then the echo line.
        let text_rows = rows.saturating_sub(2) as usize;
        let text_cols = cols as usize;
        let status_row = rows.saturating_sub(2);
        let mini_row = rows.saturating_sub(1);
        self.last_text_rows = text_rows; // for page up/down
        self.scroll_to_cursor(text_rows, text_cols);

        // Persist the resume position whenever it moves (cheap single-row write).
        let st = (self.buffer.cursor(), self.top, self.left);
        if st != self.saved_state {
            self.store.set_state(st.0, st.1, st.2)?;
            self.saved_state = st;
        }

        let mut out = io::stdout();

        // On first frame or resize, drop the cache and clear the screen once;
        // every row then differs from its (empty) cache and is repainted.
        if self.frame_cols != cols || self.frame_rows != rows {
            self.frame = vec![String::new(); rows as usize];
            self.frame_cols = cols;
            self.frame_rows = rows;
            queue!(out, terminal::Clear(terminal::ClearType::All))?;
        }

        queue!(out, terminal::BeginSynchronizedUpdate, cursor::Hide)?;

        // The LEAP match to highlight this frame: (line, start_dcol, end_dcol).
        let leap_hl = self.leap_highlight();
        // While leaping we draw the text rows directly (with highlight), so drop
        // their cache to force a clean repaint that also clears a stale match.
        if leap_hl.is_some() || self.leap.is_some() {
            for row in 0..text_rows {
                self.frame[row] = String::new();
            }
        }

        // Text area: only repaint rows whose content changed (or all, leaping).
        for row in 0..text_rows {
            let idx = self.top + row;
            let desired = if idx >= self.buffer.len_lines() {
                "~".to_string()
            } else if self.buffer.line_is_marker(idx) {
                marker_rule(text_cols)
            } else {
                self.buffer.display_line(idx, self.left, text_cols)
            };
            match leap_hl {
                Some((line, lo, hi)) if line == idx => {
                    self.draw_row_highlight(&mut out, row as u16, &desired, lo, hi, text_cols)?;
                }
                _ => self.draw_row(&mut out, row as u16, desired, text_cols, false)?,
            }
        }

        // Status line (reverse video); changes on most cursor moves.
        let status = self.status_string(text_cols);
        self.draw_row(&mut out, status_row, status, text_cols, true)?;

        // Echo line. Priority: LEAP label > transient message.
        let echo_line = match &self.leap {
            Some(l) => l.label().chars().take(text_cols).collect(),
            None => self.echo.render(text_cols),
        };
        self.draw_row(&mut out, mini_row, echo_line, text_cols, false)?;

        // Park the hardware cursor at the buffer cursor. Repositioning never
        // clears, so it can't flicker.
        let (line, _) = self.buffer.cursor_line_col();
        let cursor_col = (self.buffer.cursor_display_col() - self.left) as u16;
        let cursor_row = (line - self.top) as u16;
        queue!(
            out,
            cursor::MoveTo(cursor_col, cursor_row),
            cursor::Show,
            terminal::EndSynchronizedUpdate,
        )?;
        out.flush()?;
        Ok(())
    }

    /// The current LEAP match as `(line, start_dcol, end_dcol)` in absolute
    /// display columns, or `None` when not leaping / no match.
    fn leap_highlight(&self) -> Option<(usize, usize, usize)> {
        let l = self.leap.as_ref()?;
        let m = l.matched?;
        let qlen = l.query.chars().count();
        let (line, start) = self.buffer.line_col_at(m);
        let (end_line, end) = self.buffer.line_col_at(m + qlen);
        (end_line == line).then_some((line, start, end))
    }

    /// Draw a text row with the LEAP match highlighted (reverse video over the
    /// display columns `[start_dcol, end_dcol)`). Always repaints.
    fn draw_row_highlight(
        &mut self,
        out: &mut impl Write,
        row: u16,
        desired: &str,
        start_dcol: usize,
        end_dcol: usize,
        width: usize,
    ) -> Result<()> {
        queue!(out, cursor::MoveTo(0, row))?;
        let mut drawn = 0;
        for (i, c) in desired.chars().take(width).enumerate() {
            let dcol = self.left + i;
            if dcol >= start_dcol && dcol < end_dcol {
                queue!(out, SetAttribute(Attribute::Reverse), Print(c), SetAttribute(Attribute::Reset))?;
            } else {
                queue!(out, Print(c))?;
            }
            drawn += 1;
        }
        for _ in drawn..width {
            queue!(out, Print(' '))?;
        }
        self.frame[row as usize] = String::new(); // force redraw next frame
        Ok(())
    }

    /// Repaint screen row `row` only if `desired` (padded to the full width)
    /// differs from what the cache says is already there. `reverse` selects the
    /// inverted attribute used for the status line.
    fn draw_row(
        &mut self,
        out: &mut impl Write,
        row: u16,
        desired: String,
        width: usize,
        reverse: bool,
    ) -> Result<()> {
        // Pad to full width so a shorter new line fully overwrites the old one
        // in a single write — no Clear, hence no blank flash.
        let mut line = desired;
        let len = line.chars().count();
        if len < width {
            line.extend(std::iter::repeat_n(' ', width - len));
        }

        if self.frame[row as usize] == line {
            return Ok(());
        }
        queue!(out, cursor::MoveTo(0, row))?;
        if reverse {
            queue!(out, SetAttribute(Attribute::Reverse), Print(&line), SetAttribute(Attribute::Reset))?;
        } else {
            queue!(out, Print(&line))?;
        }
        self.frame[row as usize] = line;
        Ok(())
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

/// A full-width horizontal rule drawn in place of a document-boundary marker line.
fn marker_rule(width: usize) -> String {
    "─".repeat(width)
}
