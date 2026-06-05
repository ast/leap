//! The fzf-style file finder: a visible, escapable transient overlay that
//! replaces the modal minibuffer for opening and naming files.
//!
//! It's "LEAP applied to filenames" — type to narrow a fuzzy-matched list, move
//! the selection, accept or escape. Modelled on `~/src/levelup/sleipnir`'s
//! picker but written in raw crossterm (no ratatui) to keep leap lean. While
//! active it owns the terminal in its own loop and returns an [`Outcome`]; the
//! editor then forces a full repaint over the dirtied screen.

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::style::{Attribute, Color, Print, ResetColor, SetAttribute, SetForegroundColor};
use crossterm::{cursor, queue, terminal};
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};

use crate::walk::{self, Candidate};

const TICK: Duration = Duration::from_millis(100);

/// Why the finder was invoked.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Pick an existing file to open.
    Open,
    /// Name a file to save to (the typed query becomes the path).
    Save,
}

/// What the user chose.
pub enum Outcome {
    Open(PathBuf),
    Save(PathBuf),
    Cancel,
    /// `C-q`: quit the editor directly from the finder (so it isn't a trap).
    Quit,
}

/// Run the finder over a walk rooted at `root`, seeded with `seed_query`. Owns
/// the terminal until the user accepts or cancels.
pub fn run(mode: Mode, root: &Path, seed_query: &str) -> Result<Outcome> {
    let mut state = State::new(walk::walk(root), seed_query.to_string());
    state.refilter();

    loop {
        let (cols, rows) = terminal::size()?;
        state.render(mode, cols, rows)?;

        if !event::poll(TICK)? {
            continue;
        }
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind == KeyEventKind::Release {
            continue;
        }
        match state.handle_key(key) {
            Action::Continue => {}
            Action::Cancel => return Ok(Outcome::Cancel),
            Action::Quit => return Ok(Outcome::Quit),
            Action::Accept => {
                if let Some(outcome) = state.accept(mode, root) {
                    return Ok(outcome);
                }
            }
        }
    }
}

enum Action {
    Continue,
    Accept,
    Cancel,
    Quit,
}

struct State {
    query: String,
    /// Cursor position in `query`, as a char index (`0..=query.chars().count()`).
    qcursor: usize,
    pool: Vec<Candidate>,
    /// Indices into `pool`, ranked best-first for the current query.
    results: Vec<usize>,
    /// Index into `results` of the highlighted row.
    selected: usize,
    /// First visible row of `results` (scroll offset).
    offset: usize,
    matcher: Matcher,
}

impl State {
    fn new(pool: Vec<Candidate>, query: String) -> Self {
        let qcursor = query.chars().count();
        Self {
            query,
            qcursor,
            pool,
            results: Vec::new(),
            selected: 0,
            offset: 0,
            matcher: Matcher::new(Config::DEFAULT),
        }
    }

    /// Re-rank `pool` against the current query.
    fn refilter(&mut self) {
        self.results.clear();
        let q = self.query.trim();
        if q.is_empty() {
            // Empty query keeps the walk's mtime-recency order.
            self.results.extend(0..self.pool.len());
        } else {
            let pat = pattern(q);
            let mut buf = Vec::new();
            let mut scored: Vec<(usize, u32)> = Vec::new();
            for (i, cand) in self.pool.iter().enumerate() {
                let hay = Utf32Str::new(&cand.display, &mut buf);
                if let Some(score) = pat.score(hay, &mut self.matcher) {
                    scored.push((i, score));
                }
            }
            scored.sort_by(|a, b| {
                b.1.cmp(&a.1)
                    .then_with(|| self.pool[b.0].mtime.cmp(&self.pool[a.0].mtime))
            });
            self.results.extend(scored.into_iter().map(|(i, _)| i));
        }
        self.selected = 0;
        self.offset = 0;
    }

    fn move_selection(&mut self, delta: isize) {
        if self.results.is_empty() {
            return;
        }
        let last = self.results.len() as isize - 1;
        self.selected = (self.selected as isize + delta).clamp(0, last) as usize;
    }

    /// Resolve the user's acceptance into an outcome, or `None` to stay open.
    fn accept(&self, mode: Mode, root: &Path) -> Option<Outcome> {
        match mode {
            Mode::Open => {
                let idx = *self.results.get(self.selected)?;
                Some(Outcome::Open(self.pool[idx].path.clone()))
            }
            Mode::Save => {
                let q = self.query.trim();
                if !q.is_empty() {
                    // Typed query is the path (absolute, or relative to root).
                    let p = Path::new(q);
                    let path = if p.is_absolute() { p.to_path_buf() } else { root.join(p) };
                    Some(Outcome::Save(path))
                } else {
                    // No query: save over the highlighted file, if any.
                    let idx = *self.results.get(self.selected)?;
                    Some(Outcome::Save(self.pool[idx].path.clone()))
                }
            }
        }
    }

    fn handle_key(&mut self, key: KeyEvent) -> Action {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        match key.code {
            // Leaving the finder: cancel back to the buffer, or quit the editor
            // outright (so the finder is never a trap you must exit first).
            KeyCode::Esc => return Action::Cancel,
            KeyCode::Char('g') if ctrl => return Action::Cancel,
            KeyCode::Char('q') if ctrl => return Action::Quit,
            KeyCode::Enter => return Action::Accept,

            // Selection movement: list keys (Emacs C-n/C-p, Up/Down).
            KeyCode::Down => self.move_selection(1),
            KeyCode::Up => self.move_selection(-1),
            KeyCode::Char('n') if ctrl => self.move_selection(1),
            KeyCode::Char('p') if ctrl => self.move_selection(-1),

            // Complete the query to the highlighted result.
            KeyCode::Tab => {
                if let Some(&idx) = self.results.get(self.selected) {
                    self.query = self.pool[idx].display.clone();
                    self.qcursor = self.q_len();
                    self.refilter();
                }
            }

            // Query cursor movement (Emacs readline; no refilter — query unchanged).
            KeyCode::Left => self.qcursor = self.qcursor.saturating_sub(1),
            KeyCode::Right => self.qcursor = (self.qcursor + 1).min(self.q_len()),
            KeyCode::Char('b') if ctrl => self.qcursor = self.qcursor.saturating_sub(1),
            KeyCode::Char('f') if ctrl => self.qcursor = (self.qcursor + 1).min(self.q_len()),
            KeyCode::Home => self.qcursor = 0,
            KeyCode::End => self.qcursor = self.q_len(),
            KeyCode::Char('a') if ctrl => self.qcursor = 0,
            KeyCode::Char('e') if ctrl => self.qcursor = self.q_len(),
            KeyCode::Char('b') if alt => self.qcursor = self.q_word_back(),
            KeyCode::Char('f') if alt => self.qcursor = self.q_word_forward(),

            // Query editing (refilter afterwards).
            KeyCode::Backspace => self.edit(Self::q_backspace),
            KeyCode::Char('h') if ctrl => self.edit(Self::q_backspace),
            KeyCode::Delete => self.edit(Self::q_delete),
            KeyCode::Char('d') if ctrl => self.edit(Self::q_delete),
            KeyCode::Char('k') if ctrl => self.edit(Self::q_kill_to_end),
            KeyCode::Char('w') if ctrl => self.edit(Self::q_kill_word_back),
            KeyCode::Char('u') if ctrl => self.edit(Self::q_kill_to_start),
            KeyCode::Char(c) if !ctrl && !alt => self.edit(|s| s.q_insert(c)),
            _ => {}
        }
        Action::Continue
    }

    /// Apply a query mutation, then re-rank.
    fn edit<F: FnOnce(&mut Self)>(&mut self, f: F) {
        f(self);
        self.refilter();
    }

    // --- query line editing (cursor-aware, UTF-8 safe) -------------------

    fn q_len(&self) -> usize {
        self.query.chars().count()
    }

    /// Byte offset of char index `i` (or the end).
    fn q_byte(&self, i: usize) -> usize {
        self.query.char_indices().nth(i).map_or(self.query.len(), |(b, _)| b)
    }

    fn q_insert(&mut self, c: char) {
        let b = self.q_byte(self.qcursor);
        self.query.insert(b, c);
        self.qcursor += 1;
    }

    fn q_backspace(&mut self) {
        if self.qcursor == 0 {
            return;
        }
        let (b0, b1) = (self.q_byte(self.qcursor - 1), self.q_byte(self.qcursor));
        self.query.replace_range(b0..b1, "");
        self.qcursor -= 1;
    }

    fn q_delete(&mut self) {
        if self.qcursor >= self.q_len() {
            return;
        }
        let (b0, b1) = (self.q_byte(self.qcursor), self.q_byte(self.qcursor + 1));
        self.query.replace_range(b0..b1, "");
    }

    fn q_kill_to_end(&mut self) {
        let b = self.q_byte(self.qcursor);
        self.query.truncate(b);
    }

    fn q_kill_to_start(&mut self) {
        let b = self.q_byte(self.qcursor);
        self.query.replace_range(0..b, "");
        self.qcursor = 0;
    }

    fn q_kill_word_back(&mut self) {
        let start = self.q_word_back();
        let (b0, b1) = (self.q_byte(start), self.q_byte(self.qcursor));
        self.query.replace_range(b0..b1, "");
        self.qcursor = start;
    }

    /// Char index of the start of the word before the cursor (skip non-word,
    /// then word chars) — same word notion as the editor's `M-b`/`C-w`.
    fn q_word_back(&self) -> usize {
        let chars: Vec<char> = self.query.chars().collect();
        let mut i = self.qcursor;
        while i > 0 && !is_word_char(chars[i - 1]) {
            i -= 1;
        }
        while i > 0 && is_word_char(chars[i - 1]) {
            i -= 1;
        }
        i
    }

    fn q_word_forward(&self) -> usize {
        let chars: Vec<char> = self.query.chars().collect();
        let mut i = self.qcursor;
        while i < chars.len() && !is_word_char(chars[i]) {
            i += 1;
        }
        while i < chars.len() && is_word_char(chars[i]) {
            i += 1;
        }
        i
    }

    fn render(&mut self, mode: Mode, cols: u16, rows: u16) -> Result<()> {
        let width = cols as usize;
        let list_rows = rows.saturating_sub(1); // row 0 is the prompt.

        // Keep the selection within the visible window.
        let h = list_rows as usize;
        if h > 0 {
            if self.selected < self.offset {
                self.offset = self.selected;
            } else if self.selected >= self.offset + h {
                self.offset = self.selected + 1 - h;
            }
        }

        let mut out = io::stdout();
        queue!(out, terminal::BeginSynchronizedUpdate, cursor::Hide)?;

        // Prompt row.
        let label = match mode {
            Mode::Open => "find",
            Mode::Save => "save",
        };
        let count = format!("{}/{}", self.results.len(), self.pool.len());
        let prompt = format!("{label} › {}", self.query);
        let pad = width
            .saturating_sub(prompt.chars().count() + count.chars().count() + 1);
        let prompt_line: String = format!("{prompt}{}{count} ", " ".repeat(pad))
            .chars()
            .take(width)
            .collect();
        queue!(
            out,
            cursor::MoveTo(0, 0),
            terminal::Clear(terminal::ClearType::CurrentLine),
            SetAttribute(Attribute::Bold),
            Print(&prompt_line),
            SetAttribute(Attribute::Reset),
        )?;

        // Result rows.
        let pat = (!self.query.trim().is_empty()).then(|| pattern(self.query.trim()));
        let mut idx_buf = Vec::new();
        let mut char_buf = Vec::new();
        for row in 0..list_rows {
            let screen_row = row + 1;
            queue!(
                out,
                cursor::MoveTo(0, screen_row),
                terminal::Clear(terminal::ClearType::CurrentLine),
            )?;
            let Some(&pool_idx) = self.results.get(self.offset + row as usize) else {
                continue;
            };
            let display = &self.pool[pool_idx].display;
            let selected = self.offset + row as usize == self.selected;
            let matched = pat.as_ref().map(|p| {
                idx_buf.clear();
                let hay = Utf32Str::new(display, &mut char_buf);
                p.indices(hay, &mut self.matcher, &mut idx_buf);
                idx_buf.clone()
            });
            self.draw_result(&mut out, display, matched.as_deref(), selected, width)?;
        }

        // Park the cursor at the query edit position ("label › " is label+3 wide).
        let q_col = (label.len() + 3 + self.qcursor).min(width.saturating_sub(1));
        queue!(
            out,
            cursor::MoveTo(q_col as u16, 0),
            cursor::Show,
            terminal::EndSynchronizedUpdate,
        )?;
        out.flush()?;
        Ok(())
    }

    fn draw_result(
        &self,
        out: &mut impl Write,
        display: &str,
        matched: Option<&[u32]>,
        selected: bool,
        width: usize,
    ) -> Result<()> {
        if selected {
            queue!(out, SetAttribute(Attribute::Reverse))?;
        }
        // A leading marker keeps the selected row legible even without colour.
        queue!(out, Print(if selected { "› " } else { "  " }))?;

        let matched = matched.unwrap_or(&[]);
        for (i, c) in display.chars().take(width.saturating_sub(2)).enumerate() {
            let hit = matched.contains(&(i as u32));
            if hit {
                queue!(out, SetForegroundColor(Color::Cyan), Print(c), ResetColor)?;
                if selected {
                    queue!(out, SetAttribute(Attribute::Reverse))?;
                }
            } else {
                queue!(out, Print(c))?;
            }
        }
        if selected {
            queue!(out, SetAttribute(Attribute::Reset))?;
        }
        Ok(())
    }
}

fn pattern(query: &str) -> Pattern {
    Pattern::parse(query, CaseMatching::Smart, Normalization::Smart)
}

/// Word notion for the query line's `M-b`/`M-f`/`C-w` (matches the editor's).
fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(display: &str, mtime: i64) -> Candidate {
        Candidate {
            path: PathBuf::from(format!("/root/{display}")),
            display: display.to_string(),
            mtime,
        }
    }

    fn state(displays: &[&str]) -> State {
        let pool = displays.iter().map(|d| cand(d, 0)).collect();
        State::new(pool, String::new())
    }

    #[test]
    fn empty_query_keeps_pool_order() {
        let mut s = state(&["a.rs", "b.rs", "c.rs"]);
        s.refilter();
        assert_eq!(s.results, vec![0, 1, 2]);
    }

    #[test]
    fn fuzzy_query_filters_and_ranks() {
        let mut s = state(&["src/main.rs", "src/buffer.rs", "README.md"]);
        s.query = "buf".to_string();
        s.refilter();
        let got: Vec<&str> = s.results.iter().map(|&i| s.pool[i].display.as_str()).collect();
        assert_eq!(got, vec!["src/buffer.rs"]);
    }

    #[test]
    fn ranks_better_match_first() {
        let mut s = state(&["xayzb", "ab"]);
        s.query = "ab".to_string();
        s.refilter();
        // "ab" (contiguous) should outrank "xayzb" (scattered).
        assert_eq!(s.pool[s.results[0]].display, "ab");
    }

    #[test]
    fn save_accepts_query_as_relative_path() {
        let mut s = state(&["existing.rs"]);
        s.query = "new/file.txt".to_string();
        s.refilter();
        let root = Path::new("/root");
        match s.accept(Mode::Save, root) {
            Some(Outcome::Save(p)) => assert_eq!(p, Path::new("/root/new/file.txt")),
            _ => panic!("expected Save outcome"),
        }
    }

    #[test]
    fn open_accepts_selected_candidate() {
        let mut s = state(&["one.rs", "two.rs"]);
        s.refilter();
        s.move_selection(1);
        match s.accept(Mode::Open, Path::new("/root")) {
            Some(Outcome::Open(p)) => assert_eq!(p, PathBuf::from("/root/two.rs")),
            _ => panic!("expected Open outcome"),
        }
    }

    #[test]
    fn ctrl_q_requests_quit() {
        let mut s = state(&["a.rs"]);
        let ev = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL);
        assert!(matches!(s.handle_key(ev), Action::Quit));
    }

    #[test]
    fn query_editing_respects_cursor() {
        let mut s = state(&[]);
        for c in "abcd".chars() {
            s.q_insert(c);
        }
        assert_eq!((s.query.as_str(), s.qcursor), ("abcd", 4));
        s.qcursor = 0; // C-a / Home
        s.qcursor += 1; // C-f
        s.q_insert('X'); // insert after 'a'
        assert_eq!(s.query, "aXbcd");
        s.q_delete(); // delete the 'b' at the cursor
        assert_eq!(s.query, "aXcd");
    }

    #[test]
    fn ctrl_k_kills_to_end_of_query() {
        let mut s = state(&[]);
        for c in "hello".chars() {
            s.q_insert(c);
        }
        s.qcursor = 2; // after "he"
        s.q_kill_to_end();
        assert_eq!(s.query, "he");
    }

    #[test]
    fn ctrl_u_kills_to_start_of_query() {
        let mut s = state(&[]);
        for c in "hello".chars() {
            s.q_insert(c);
        }
        s.qcursor = 2; // after "he"
        s.q_kill_to_start();
        assert_eq!((s.query.as_str(), s.qcursor), ("llo", 0));
    }

    #[test]
    fn ctrl_w_kills_word_back_in_query() {
        let mut s = state(&[]);
        for c in "src/buffer".chars() {
            s.q_insert(c);
        }
        s.q_kill_word_back(); // removes "buffer"
        assert_eq!(s.query, "src/");
    }
}
