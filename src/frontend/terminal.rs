//! The terminal (crossterm) front-end.
//!
//! Owns the terminal: raw mode + alternate screen (restored on drop, even on
//! panic), the Kitty-keyboard negotiation, the blocking event loop, and a
//! flicker-free **diff renderer** that only re-emits screen rows whose content
//! changed. It decodes crossterm key events into [`KeyChord`]s for the core and
//! paints the [`Frame`] the core returns.

use std::io::{self, Write};

use anyhow::Result;
use crossterm::event::{
    self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, KeyboardEnhancementFlags,
    PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::style::{Attribute, Print, SetAttribute};
use crossterm::{
    cursor, execute, queue,
    terminal::{
        self, supports_keyboard_enhancement, EnterAlternateScreen, LeaveAlternateScreen,
    },
};

use crate::editor::Editor;
use crate::input::{KeyChord, LogicalKey};
use crate::view::Row;

/// Run the editor in the terminal until it quits, then restore the terminal.
pub fn run(editor: Editor) -> Result<()> {
    let _guard = setup()?;
    let mut tui = Tui {
        editor,
        frame: Vec::new(),
        frame_cols: 0,
        frame_rows: 0,
    };
    tui.render()?;
    while tui.editor.running() {
        match event::read()? {
            Event::Key(key) => {
                if let Some(chord) = chord_from_crossterm(key) {
                    tui.editor.input(chord);
                    tui.editor.persist_edits()?;
                }
            }
            Event::Resize(_, _) => {}
            _ => {}
        }
        if tui.editor.running() {
            tui.render()?;
        }
    }
    tui.editor.shutdown()?;
    Ok(())
}

/// Decode a crossterm key event into a logical [`KeyChord`]. Returns `None` for
/// key releases (Kitty protocol) and keys the editor doesn't use — the core
/// never sees releases.
fn chord_from_crossterm(key: KeyEvent) -> Option<KeyChord> {
    if key.kind == KeyEventKind::Release {
        return None;
    }
    let logical = match key.code {
        KeyCode::Char(c) => LogicalKey::Char(c),
        KeyCode::Enter => LogicalKey::Enter,
        KeyCode::Tab => LogicalKey::Tab,
        KeyCode::Backspace => LogicalKey::Backspace,
        KeyCode::Delete => LogicalKey::Delete,
        KeyCode::Esc => LogicalKey::Esc,
        KeyCode::Left => LogicalKey::Left,
        KeyCode::Right => LogicalKey::Right,
        KeyCode::Up => LogicalKey::Up,
        KeyCode::Down => LogicalKey::Down,
        KeyCode::Home => LogicalKey::Home,
        KeyCode::End => LogicalKey::End,
        KeyCode::PageUp => LogicalKey::PageUp,
        KeyCode::PageDown => LogicalKey::PageDown,
        _ => return None,
    };
    Some(KeyChord {
        key: logical,
        ctrl: key.modifiers.contains(KeyModifiers::CONTROL),
        alt: key.modifiers.contains(KeyModifiers::ALT),
        shift: key.modifiers.contains(KeyModifiers::SHIFT),
    })
}

/// The terminal renderer: the editor core plus a per-row diff cache.
struct Tui {
    editor: Editor,
    /// The exact (full-width) string currently shown on each screen row. A row
    /// is only re-emitted when its desired content differs, so a pure cursor
    /// move repaints just the status line, not the whole screen.
    frame: Vec<String>,
    frame_cols: u16,
    frame_rows: u16,
}

impl Tui {
    fn render(&mut self) -> Result<()> {
        let (cols, rows) = terminal::size()?;
        let frame = self.editor.compute_frame(cols as usize, rows as usize)?;
        let text_cols = cols as usize;
        let text_rows = rows.saturating_sub(3) as usize;
        let ruler_row = rows.saturating_sub(3);
        let status_row = rows.saturating_sub(2);
        let mini_row = rows.saturating_sub(1);

        let mut out = io::stdout();

        // First frame, resize, or a forced repaint: drop the cache and clear the
        // screen once; every row then differs from its (empty) cache.
        if self.frame_cols != cols || self.frame_rows != rows || frame.full_repaint {
            self.frame = vec![String::new(); rows as usize];
            self.frame_cols = cols;
            self.frame_rows = rows;
            queue!(out, terminal::Clear(terminal::ClearType::All))?;
        } else if frame.redraw_text {
            // Leaping: the highlight may have moved — force the text rows.
            for row in 0..text_rows.min(self.frame.len()) {
                self.frame[row] = String::new();
            }
        }

        queue!(out, terminal::BeginSynchronizedUpdate, cursor::Hide)?;

        for (row, r) in frame.rows.iter().enumerate() {
            let desired = match r {
                Row::Text(s) => s.clone(),
                Row::MarkerRule => marker_rule(text_cols),
                Row::Tilde => "~".to_string(),
            };
            match frame.highlights.get(row).copied().flatten() {
                Some((lo, hi)) => {
                    self.draw_row_highlight(&mut out, row as u16, &desired, lo, hi, text_cols)?;
                }
                None => self.draw_row(&mut out, row as u16, desired, text_cols, false)?,
            }
        }

        // Chrome: ruler (with a reverse-video marker at the cursor column),
        // status line (reverse video), then the echo line.
        let (ccol, _) = frame.cursor;
        self.draw_row_highlight(&mut out, ruler_row, &frame.ruler, ccol, ccol + 1, text_cols)?;
        self.draw_row(&mut out, status_row, frame.status.clone(), text_cols, true)?;
        self.draw_row(&mut out, mini_row, frame.echo.clone(), text_cols, false)?;

        // Park the hardware cursor at the buffer cursor.
        let (ccol, crow) = frame.cursor;
        queue!(
            out,
            cursor::MoveTo(ccol as u16, crow as u16),
            cursor::Show,
            terminal::EndSynchronizedUpdate,
        )?;
        out.flush()?;
        Ok(())
    }

    /// Draw a text row with the LEAP match highlighted (reverse video over the
    /// *visible* cells `[start, end)`). Always repaints.
    fn draw_row_highlight(
        &mut self,
        out: &mut impl Write,
        row: u16,
        desired: &str,
        start: usize,
        end: usize,
        width: usize,
    ) -> Result<()> {
        queue!(out, cursor::MoveTo(0, row))?;
        let mut drawn = 0;
        for (i, c) in desired.chars().take(width).enumerate() {
            if i >= start && i < end {
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
}

/// A full-width horizontal rule drawn in place of a document-boundary marker.
fn marker_rule(width: usize) -> String {
    "─".repeat(width)
}

// --- terminal setup / teardown -------------------------------------------

/// Restores the terminal to its normal state when dropped.
pub struct TerminalGuard;

/// Enter raw mode + the alternate screen and negotiate keyboard enhancement.
/// The returned guard restores the terminal when it goes out of scope.
fn setup() -> Result<TerminalGuard> {
    install_panic_hook();
    terminal::enable_raw_mode()?;
    let mut out = io::stdout();
    execute!(out, EnterAlternateScreen, cursor::Hide)?;

    if matches!(supports_keyboard_enhancement(), Ok(true)) {
        // DISAMBIGUATE_ESCAPE_CODES so modified keys arrive cleanly as CSI-u;
        // REPORT_EVENT_TYPES so we get Press/Repeat/Release rather than just Press.
        let flags = KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
            | KeyboardEnhancementFlags::REPORT_EVENT_TYPES;
        let _ = execute!(out, PushKeyboardEnhancementFlags(flags));
    }
    Ok(TerminalGuard)
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = restore();
    }
}

fn restore() -> io::Result<()> {
    let mut out = io::stdout();
    // Popping enhancement flags is harmless if none were pushed.
    let _ = execute!(out, PopKeyboardEnhancementFlags);
    execute!(out, cursor::Show, LeaveAlternateScreen)?;
    terminal::disable_raw_mode()?;
    out.flush()
}

/// Restore the terminal before the default panic handler prints, so the panic
/// message lands on a sane screen instead of inside the alternate buffer.
fn install_panic_hook() {
    let original = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = restore();
        original(info);
    }));
}
