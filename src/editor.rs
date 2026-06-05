//! Top-level editor state and the event loop.
//!
//! Milestone 1 scope: load a file into memory, draw a scrolling text area plus
//! a status line, and quit on `Ctrl-Q`. The display is read-only for now — the
//! rope-backed editable [`Buffer`] and real cursor arrive in milestone 2, at
//! which point `lines: Vec<String>` here is replaced.

use std::io::{self, Write};
use std::path::PathBuf;

use anyhow::{Context, Result};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::style::{Attribute, Print, SetAttribute};
use crossterm::{cursor, queue, terminal};

/// The running editor: document, viewport, and loop state.
pub struct Editor {
    path: Option<PathBuf>,
    readonly: bool,
    /// File contents as lines — placeholder until the rope buffer lands (M2).
    lines: Vec<String>,
    /// First visible line (0-based): the vertical scroll offset.
    top: usize,
    running: bool,
}

impl Editor {
    /// Load `path` (or start empty) and position the viewport at `line` (1-based).
    pub fn open(path: Option<PathBuf>, line: usize, readonly: bool) -> Result<Self> {
        let lines = match &path {
            Some(p) => std::fs::read_to_string(p)
                .with_context(|| format!("reading {}", p.display()))?
                .lines()
                .map(str::to_owned)
                .collect(),
            None => Vec::new(),
        };
        let top = line.saturating_sub(1).min(lines.len().saturating_sub(1));
        Ok(Self { path, readonly, lines, top, running: true })
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
        match (key.modifiers, key.code) {
            (KeyModifiers::CONTROL, KeyCode::Char('q')) => self.running = false,
            (_, KeyCode::Up) => self.scroll(-1),
            (_, KeyCode::Down) => self.scroll(1),
            _ => {}
        }
    }

    fn scroll(&mut self, delta: isize) {
        let max_top = self.lines.len().saturating_sub(1) as isize;
        self.top = (self.top as isize + delta).clamp(0, max_top) as usize;
    }

    fn render(&self) -> Result<()> {
        let mut out = io::stdout();
        let (cols, rows) = terminal::size()?;
        let text_rows = rows.saturating_sub(1); // bottom row is the status line.

        queue!(out, cursor::Hide)?;
        for row in 0..text_rows {
            let line = self.lines.get(self.top + row as usize).map(String::as_str);
            queue!(
                out,
                cursor::MoveTo(0, row),
                terminal::Clear(terminal::ClearType::CurrentLine),
            )?;
            match line {
                Some(text) => {
                    let shown: String = text.chars().take(cols as usize).collect();
                    queue!(out, Print(shown))?;
                }
                // Past end-of-file: a tilde marker, vi-style.
                None => queue!(out, Print("~"))?,
            }
        }

        self.render_status(&mut out, cols, rows)?;
        out.flush()?;
        Ok(())
    }

    fn render_status(&self, out: &mut impl Write, cols: u16, rows: u16) -> Result<()> {
        let name = self
            .path
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "*scratch*".to_string());
        let ro = if self.readonly { " (ro)" } else { "" };

        let left = format!(" {name}{ro} ");
        let right = format!(" {} lines  ^Q quit ", self.lines.len());
        let width = cols as usize;
        let gap = width.saturating_sub(left.chars().count() + right.chars().count());
        let status: String = format!("{left}{}{right}", " ".repeat(gap))
            .chars()
            .take(width)
            .collect();

        queue!(
            out,
            cursor::MoveTo(0, rows.saturating_sub(1)),
            terminal::Clear(terminal::ClearType::CurrentLine),
            SetAttribute(Attribute::Reverse),
            Print(status),
            SetAttribute(Attribute::Reset),
        )?;
        Ok(())
    }
}
