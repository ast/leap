//! Top-level editor state and the event loop.
//!
//! Milestone 2 scope: a `ropey`-backed [`Buffer`] with a live cursor, basic
//! editing (insert/newline/backspace/delete), arrow/Home/End movement, and a
//! viewport that scrolls to keep the cursor visible. LEAP navigation, selection,
//! undo, files, and highlighting arrive in later milestones (see `docs/DESIGN.md`).

use std::io::{self, Write};
use std::path::PathBuf;

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::style::{Attribute, Print, SetAttribute};
use crossterm::{cursor, queue, terminal};

use crate::buffer::Buffer;

/// The running editor: document plus viewport and loop state.
pub struct Editor {
    buffer: Buffer,
    /// First visible line (vertical scroll offset).
    top: usize,
    /// First visible display column (horizontal scroll offset).
    left: usize,
    running: bool,
    /// Diff-render cache: the exact (full-width) string currently shown on each
    /// screen row. A row is only re-emitted when its desired content differs,
    /// so a pure cursor move repaints just the status line, not the whole screen.
    frame: Vec<String>,
    frame_cols: u16,
    frame_rows: u16,
}

impl Editor {
    /// Open `path` (or an empty buffer) with the cursor at `line` (1-based).
    pub fn open(path: Option<PathBuf>, line: usize, readonly: bool) -> Result<Self> {
        let mut buffer = Buffer::open(path, readonly)?;
        buffer.goto_line(line);
        Ok(Self {
            buffer,
            top: 0,
            left: 0,
            running: true,
            frame: Vec::new(),
            frame_cols: 0,
            frame_rows: 0,
        })
    }

    /// Render once, then block on events and re-render after each — no busy
    /// loop, and `event::read` wakes on resize as well as key input.
    pub fn run(&mut self) -> Result<()> {
        self.render()?;
        while self.running {
            match event::read()? {
                Event::Key(key) => self.on_key(key),
                Event::Resize(_, _) => {}
                _ => {}
            }
            if self.running {
                self.render()?;
            }
        }
        Ok(())
    }

    fn on_key(&mut self, key: KeyEvent) {
        // Under the Kitty protocol we'd also see Release/Repeat; act on Press.
        if key.kind != KeyEventKind::Press {
            return;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        match key.code {
            KeyCode::Char('q') if ctrl => self.running = false,
            KeyCode::Left => self.buffer.move_left(),
            KeyCode::Right => self.buffer.move_right(),
            KeyCode::Up => self.buffer.move_up(),
            KeyCode::Down => self.buffer.move_down(),
            KeyCode::Home => self.buffer.move_home(),
            KeyCode::End => self.buffer.move_end(),
            KeyCode::Enter => self.buffer.insert_newline(),
            KeyCode::Tab => self.buffer.insert_char('\t'),
            KeyCode::Backspace => self.buffer.backspace(),
            KeyCode::Delete => self.buffer.delete_forward(),
            // Plain text input (Shift for uppercase is fine; Ctrl/Alt are reserved).
            KeyCode::Char(c) if !ctrl && !alt => self.buffer.insert_char(c),
            _ => {}
        }
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
        let text_rows = rows.saturating_sub(1) as usize; // bottom row = status line.
        let text_cols = cols as usize;
        let status_row = rows.saturating_sub(1);
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

        // Text area: only repaint rows whose content changed.
        for row in 0..text_rows {
            let idx = self.top + row;
            let desired = if idx < self.buffer.len_lines() {
                self.buffer.display_line(idx, self.left, text_cols)
            } else {
                "~".to_string()
            };
            self.draw_row(&mut out, row as u16, desired, text_cols, false)?;
        }

        // Status line (reverse video); changes on most cursor moves.
        let status = self.status_string(text_cols);
        self.draw_row(&mut out, status_row, status, text_cols, true)?;

        // Reposition the hardware cursor and reveal it. This never clears, so it
        // can't flicker.
        let (line, _) = self.buffer.cursor_line_col();
        let screen_row = (line - self.top) as u16;
        let screen_col = (self.buffer.cursor_display_col() - self.left) as u16;
        queue!(
            out,
            cursor::MoveTo(screen_col, screen_row),
            cursor::Show,
            terminal::EndSynchronizedUpdate,
        )?;
        out.flush()?;
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
        let name = self
            .buffer
            .path()
            .and_then(|p| p.file_name())
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "*scratch*".to_string());
        let modified = if self.buffer.is_dirty() { " ●" } else { "" };
        let ro = if self.buffer.readonly() { " (ro)" } else { "" };
        let (line, col) = self.buffer.cursor_line_col();

        let left = format!(" {name}{modified}{ro} ");
        let right = format!(" Ln {}, Col {}  ^Q quit ", line + 1, col + 1);
        let gap = width.saturating_sub(left.chars().count() + right.chars().count());
        format!("{left}{}{right}", " ".repeat(gap)).chars().take(width).collect()
    }
}
