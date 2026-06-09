//! Front-end-agnostic input model.
//!
//! Each front-end (the crossterm terminal, the winit GUI) decodes its native key
//! events into a [`KeyChord`] and hands it to [`Editor::input`](crate::editor::Editor::input).
//! The core never sees terminal- or windowing-specific key types, so the same
//! command dispatch drives every front-end. Key *releases* are filtered out by
//! the front-end adapters and never reach the core.

/// A logical key, independent of how it was physically produced.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LogicalKey {
    /// A character (already case-folded by the platform; `Shift` is in the chord).
    Char(char),
    Enter,
    Tab,
    Backspace,
    Delete,
    Esc,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
}

/// A key plus its active modifiers — the unit of input the core dispatches on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct KeyChord {
    pub key: LogicalKey,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

impl KeyChord {
    /// A plain (unmodified) key.
    pub fn plain(key: LogicalKey) -> Self {
        Self { key, ctrl: false, alt: false, shift: false }
    }

    /// A `Ctrl`-modified character, e.g. `KeyChord::ctrl('s')`.
    pub fn ctrl(c: char) -> Self {
        Self { key: LogicalKey::Char(c), ctrl: true, alt: false, shift: false }
    }

    /// An `Alt`/`Meta`-modified character.
    pub fn alt(c: char) -> Self {
        Self { key: LogicalKey::Char(c), ctrl: false, alt: true, shift: false }
    }
}
