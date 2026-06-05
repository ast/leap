//! The echo line: the bottom screen row, display-only.
//!
//! It replaces the Emacs-style minibuffer's *message* role without its modal
//! input role — Raskin's objection was to the cramped one-line prompt mode you
//! get trapped in. Here the bottom row only ever *shows* a transient message
//! (Saved, read-only, the hold-`C-q` meter); all real input happens in the
//! buffer, the LEAP search, or the fzf [`finder`](crate::finder) overlay.

/// A transient one-line message, cleared on the next keystroke.
#[derive(Default)]
pub struct Echo {
    message: String,
}

impl Echo {
    /// Show a transient message.
    pub fn show(&mut self, msg: impl Into<String>) {
        self.message = msg.into();
    }

    /// Clear the message (called on each ordinary keystroke).
    pub fn clear(&mut self) {
        self.message.clear();
    }

    /// The text to display, clipped to `width`.
    pub fn render(&self, width: usize) -> String {
        self.message.chars().take(width).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shows_and_clears() {
        let mut e = Echo::default();
        assert_eq!(e.render(80), "");
        e.show("Saved foo.txt");
        assert_eq!(e.render(80), "Saved foo.txt");
        e.clear();
        assert_eq!(e.render(80), "");
    }

    #[test]
    fn clips_to_width() {
        let mut e = Echo::default();
        e.show("abcdefgh");
        assert_eq!(e.render(3), "abc");
    }
}
