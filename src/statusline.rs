//! The status line: a one-row summary rendered just above the minibuffer.
//!
//! Pure formatting — the [`Editor`](crate::editor::Editor) builds a [`StatusLine`]
//! from buffer state each frame and asks it to render to the exact terminal width.

/// The data shown on the status line. Built fresh per frame.
pub struct StatusLine {
    pub name: String,
    pub dirty: bool,
    pub readonly: bool,
    /// 1-based cursor line.
    pub line: usize,
    /// 1-based cursor column.
    pub col: usize,
    pub total_lines: usize,
}

impl StatusLine {
    /// Render to exactly `width` columns: name and flags on the left, cursor
    /// position on the right, padded (or truncated) to fill the row.
    pub fn render(&self, width: usize) -> String {
        let dot = if self.dirty { " ●" } else { "" };
        let ro = if self.readonly { " (ro)" } else { "" };
        let left = format!(" {}{dot}{ro} ", self.name);
        let right = format!(" Ln {}, Col {}  {} lines ", self.line, self.col, self.total_lines);
        let gap = width.saturating_sub(left.chars().count() + right.chars().count());
        format!("{left}{}{right}", " ".repeat(gap)).chars().take(width).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> StatusLine {
        StatusLine {
            name: "file.rs".into(),
            dirty: true,
            readonly: false,
            line: 2,
            col: 5,
            total_lines: 9,
        }
    }

    #[test]
    fn fills_exact_width() {
        let s = sample().render(80);
        assert_eq!(s.chars().count(), 80);
        assert!(s.contains("file.rs"));
        assert!(s.contains('●'));
        assert!(s.contains("Ln 2, Col 5"));
        assert!(s.contains("9 lines"));
    }

    #[test]
    fn truncates_to_width() {
        let s = sample().render(10);
        assert_eq!(s.chars().count(), 10);
    }

    #[test]
    fn readonly_flag_shown() {
        let mut s = sample();
        s.readonly = true;
        assert!(s.render(80).contains("(ro)"));
    }
}
