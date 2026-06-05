//! LEAP — incremental search-to-move, the editor's namesake navigation.
//!
//! Modelled on the Canon Cat's LEAP keys (and Emacs isearch): press `C-s`/`C-r`
//! to start leaping forward/backward, then type the target text and the cursor
//! flies to each match as you type; press the key again to jump to the next
//! occurrence; `Enter` lands; `Esc`/`C-g` returns to where you started. On most
//! terminals this is a visible, escapable *session* (the query shows on the echo
//! line); a true hold-a-key quasimode is a future Kitty-protocol enhancement.
//!
//! This struct is just the session *state*; the [`Editor`](crate::editor::Editor)
//! drives the search against the [`Buffer`](crate::buffer::Buffer) (which owns
//! the text) and the cursor.

/// Search direction.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Dir {
    Forward,
    Backward,
}

/// An active LEAP session.
pub struct Leap {
    pub dir: Dir,
    pub query: String,
    /// Cursor position when the session began — restored on cancel.
    pub origin: usize,
    /// Start char index of the current match, or `None` while failing/empty.
    pub matched: Option<usize>,
    /// Whether the current match was found only after wrapping around.
    pub wrapped: bool,
}

impl Leap {
    pub fn new(dir: Dir, origin: usize) -> Self {
        Self {
            dir,
            query: String::new(),
            origin,
            matched: None,
            wrapped: false,
        }
    }

    /// Smart case: case-insensitive unless the query contains an uppercase
    /// letter (matches the fzf finder and Emacs isearch).
    pub fn insensitive(&self) -> bool {
        !self.query.chars().any(char::is_uppercase)
    }

    /// The echo-line label, e.g. `LEAP→ foo  (wrapped)`.
    pub fn label(&self) -> String {
        let arrow = match self.dir {
            Dir::Forward => '→',
            Dir::Backward => '←',
        };
        let mut s = format!("LEAP{arrow} {}", self.query);
        if !self.query.is_empty() && self.matched.is_none() {
            s.push_str("  (no match)");
        } else if self.wrapped {
            s.push_str("  (wrapped)");
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smart_case_is_insensitive_until_uppercase() {
        let mut l = Leap::new(Dir::Forward, 0);
        l.query = "foo".into();
        assert!(l.insensitive());
        l.query = "Foo".into();
        assert!(!l.insensitive());
    }

    #[test]
    fn label_shows_direction_and_state() {
        let mut l = Leap::new(Dir::Forward, 0);
        l.query = "bar".into();
        l.matched = Some(10);
        assert_eq!(l.label(), "LEAP→ bar");
        l.wrapped = true;
        assert_eq!(l.label(), "LEAP→ bar  (wrapped)");
        l.matched = None;
        assert_eq!(l.label(), "LEAP→ bar  (no match)");

        let mut b = Leap::new(Dir::Backward, 0);
        b.query = "x".into();
        b.matched = Some(1);
        assert_eq!(b.label(), "LEAP← x");
    }
}
