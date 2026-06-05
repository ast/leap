//! Top-level editor state and the event loop.
//!
//! Buffer + diff renderer + two-row chrome (status line and a **display-only**
//! [`Echo`] line — no modal minibuffer). File open/save go through the fzf
//! [`finder`](crate::finder) overlay. **LEAP** (`C-s`/`C-r`, see [`crate::leap`])
//! is incremental search-to-move. Quitting a modified buffer uses a Raskin-style
//! **hold gesture** (hold `C-q` to discard) instead of a modal yes/no — see
//! [`crate::hold`]. The event loop is a polled tick so the hold can fire and
//! animate while a key is held.

use std::io::{self, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::style::{Attribute, Print, SetAttribute};
use crossterm::{cursor, queue, terminal};

use crate::buffer::Buffer;
use crate::echo::Echo;
use crate::finder::{self, Mode, Outcome};
use crate::hold::Hold;
use crate::leap::{Dir, Leap};
use crate::statusline::StatusLine;
use crate::walk;

/// Loop tick: how often the hold gesture is advanced and the meter animates.
const TICK: Duration = Duration::from_millis(50);
/// How long `C-q` must be held to discard unsaved changes and quit.
const QUIT_HOLD: Duration = Duration::from_millis(650);
/// Fallback (no Kitty protocol): repeat gap taken to mean the key was released.
const RELEASE_GAP: Duration = Duration::from_millis(200);

/// A multi-key prefix awaiting its second key (Emacs-style `C-x ...`).
enum Prefix {
    CtrlX,
}

/// The running editor: document plus viewport, chrome, and loop state.
pub struct Editor {
    buffer: Buffer,
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
    /// Hold-to-discard-and-quit gesture (replaces the yes/no quit prompt).
    quit_hold: Hold,
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
    /// Open `path` (or an empty buffer) with the cursor at `line` (1-based).
    /// `kbd_enhanced` is whether the Kitty keyboard protocol is active.
    pub fn open(
        path: Option<PathBuf>,
        line: usize,
        readonly: bool,
        kbd_enhanced: bool,
    ) -> Result<Self> {
        let mut buffer = Buffer::open(path, readonly)?;
        buffer.goto_line(line);
        let mut echo = Echo::default();
        echo.show("C-x C-f open · C-x C-s save · C-q quit");
        Ok(Self {
            buffer,
            top: 0,
            left: 0,
            running: true,
            echo,
            prefix: None,
            leap: None,
            quit_hold: Hold::new(QUIT_HOLD, RELEASE_GAP, kbd_enhanced, Instant::now()),
            kill_ring: String::new(),
            last_was_kill: false,
            last_text_rows: 1,
            frame: Vec::new(),
            frame_cols: 0,
            frame_rows: 0,
        })
    }

    /// Polled event loop: handle input when present, advance the hold gesture
    /// every tick, and repaint only when something changed (or a hold meter is
    /// animating). Idle costs no redraws.
    pub fn run(&mut self) -> Result<()> {
        self.render()?;
        let mut prev_armed = false;
        while self.running {
            let got = if event::poll(TICK)? {
                match event::read()? {
                    Event::Key(key) => {
                        self.on_key(key);
                        true
                    }
                    Event::Resize(_, _) => true,
                    _ => false,
                }
            } else {
                false
            };

            // Advance the hold; firing means "discard changes and quit".
            if self.quit_hold.poll(Instant::now()) {
                self.running = false;
            }

            let armed = self.quit_hold.is_armed();
            if self.running && (got || armed || prev_armed) {
                self.render()?;
            }
            prev_armed = armed;
        }
        Ok(())
    }

    fn on_key(&mut self, key: KeyEvent) {
        // Key releases (Kitty protocol only) end hold gestures.
        if key.kind == KeyEventKind::Release {
            if matches!(key.code, KeyCode::Char('q')) {
                self.quit_hold.release();
            }
            return;
        }
        // Press or auto-repeat below (repeats drive held-key gestures).

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
            KeyCode::Char('q') if ctrl => self.quit_key(),
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
        if self.buffer.readonly() {
            self.echo.show("Buffer is read-only");
            return;
        }
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
        if self.buffer.readonly() {
            self.echo.show("Buffer is read-only");
        } else if self.kill_ring.is_empty() {
            self.echo.show("Kill ring empty");
        } else {
            self.buffer.insert_str(&self.kill_ring);
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

    /// Run a mutating edit, or report why it can't happen in the echo area.
    fn edit<F: FnOnce(&mut Buffer)>(&mut self, f: F) {
        if self.buffer.readonly() {
            self.echo.show("Buffer is read-only");
        } else {
            f(&mut self.buffer);
        }
    }

    /// Complete a pending prefix key (currently only `C-x`).
    fn prefix_key(&mut self, prefix: Prefix, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match prefix {
            Prefix::CtrlX => match key.code {
                KeyCode::Char('s') if ctrl => self.save(),
                KeyCode::Char('w') if ctrl => self.save_as(),
                KeyCode::Char('f') if ctrl => self.find_file(),
                // C-x C-c can't be "held"; on a dirty buffer it points at the
                // C-q hold gesture rather than quitting.
                KeyCode::Char('c') if ctrl => {
                    if self.buffer.is_dirty() {
                        self.echo
                            .show("Unsaved changes — hold C-q to discard, or C-x C-s to save");
                    } else {
                        self.running = false;
                    }
                }
                KeyCode::Char('g') if ctrl => self.echo.show("Cancelled"),
                KeyCode::Esc => self.echo.show("Cancelled"),
                _ => self.echo.show("C-x: undefined key"),
            },
        }
    }

    /// `C-q`: quit immediately when clean; on a dirty buffer, arm the
    /// hold-to-discard gesture (a tap just shows the hint, a sustained hold
    /// fills the meter and quits). No modal yes/no.
    fn quit_key(&mut self) {
        if self.buffer.is_dirty() {
            self.echo
                .show("Unsaved changes — hold C-q to discard, or C-x C-s to save");
            self.quit_hold.press(Instant::now());
        } else {
            self.running = false;
        }
    }

    /// Run the fzf finder over the project, then force a full repaint (it
    /// clobbered the screen). Errors surface on the echo line.
    fn run_finder(&mut self, mode: Mode, seed: &str) -> Outcome {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let root = walk::finder_root(&cwd);
        let outcome = match finder::run(mode, &root, seed) {
            Ok(o) => o,
            Err(e) => {
                self.echo.show(format!("Finder error: {e}"));
                Outcome::Cancel
            }
        };
        self.frame_cols = 0; // invalidate the diff cache → clear-and-repaint
        outcome
    }

    /// `C-x C-f`: open another file via the finder. Refuses to discard unsaved
    /// changes — save first.
    fn find_file(&mut self) {
        if self.buffer.is_dirty() {
            self.echo.show("Unsaved changes — save first (C-x C-s)");
            return;
        }
        match self.run_finder(Mode::Open, "") {
            Outcome::Open(path) => match self.buffer.load(&path) {
                Ok(_) => {
                    self.top = 0;
                    self.left = 0;
                    self.echo.show(format!("Opened {}", self.buffer_name()));
                }
                Err(e) => self.echo.show(format!("Open failed: {e}")),
            },
            Outcome::Quit => self.quit_from_finder(),
            Outcome::Cancel | Outcome::Save(_) => {}
        }
    }

    /// `C-q` pressed inside the finder: quit the editor if the buffer is clean
    /// (the common case), else bounce back with the hold-to-discard hint — we
    /// never silently drop unsaved changes.
    fn quit_from_finder(&mut self) {
        if self.buffer.is_dirty() {
            self.echo
                .show("Unsaved changes — hold C-q to discard, or C-x C-s to save");
        } else {
            self.running = false;
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

    /// `C-x C-s`: save to the current path, or pick a name via the finder.
    fn save(&mut self) {
        if self.buffer.path().is_none() {
            self.save_as();
            return;
        }
        match self.buffer.save() {
            Ok(()) => self.echo.show(format!("Saved {}", self.buffer_name())),
            Err(e) => self.echo.show(format!("Save failed: {e}")),
        }
    }

    /// `C-x C-w`: choose a path via the finder (typed query = path) and save.
    fn save_as(&mut self) {
        let seed = self
            .buffer
            .path()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        match self.run_finder(Mode::Save, &seed) {
            Outcome::Save(path) => match self.buffer.save_as(path) {
                Ok(()) => self.echo.show(format!("Saved {}", self.buffer_name())),
                Err(e) => self.echo.show(format!("Save failed: {e}")),
            },
            Outcome::Cancel => self.echo.show("Cancelled"),
            Outcome::Quit => self.quit_from_finder(),
            Outcome::Open(_) => {}
        }
    }

    /// The current file's display name (file name, or "*scratch*").
    fn buffer_name(&self) -> String {
        self.buffer
            .path()
            .and_then(|p| p.file_name())
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "*scratch*".to_string())
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
        // Bottom two rows are chrome: status line, then the minibuffer.
        let text_rows = rows.saturating_sub(2) as usize;
        let text_cols = cols as usize;
        let status_row = rows.saturating_sub(2);
        let mini_row = rows.saturating_sub(1);
        self.last_text_rows = text_rows; // for page up/down
        self.scroll_to_cursor(text_rows, text_cols);

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
            let desired = if idx < self.buffer.len_lines() {
                self.buffer.display_line(idx, self.left, text_cols)
            } else {
                "~".to_string()
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

        // Echo line. Priority: hold meter > LEAP label > transient message.
        let echo_line = match self.quit_hold.progress(Instant::now()) {
            Some(frac) => format!("Hold C-q to discard changes  {}", meter(frac)),
            None => match &self.leap {
                Some(l) => l.label().chars().take(text_cols).collect(),
                None => self.echo.render(text_cols),
            },
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

        StatusLine {
            name: self.buffer_name(),
            dirty: self.buffer.is_dirty(),
            readonly: self.buffer.readonly(),
            line: line + 1,
            col: col + 1,
            total_lines: self.buffer.len_lines(),
        }
        .render(width)
    }
}

/// A 5-cell progress meter (`▰`/`▱`) for a hold gesture.
fn meter(frac: f32) -> String {
    const CELLS: usize = 5;
    let filled = (frac.clamp(0.0, 1.0) * CELLS as f32).round() as usize;
    (0..CELLS).map(|i| if i < filled { '▰' } else { '▱' }).collect()
}
