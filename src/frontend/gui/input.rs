//! Decode winit keyboard events into the core's [`KeyChord`].
//!
//! Key releases are dropped (the core never sees them). Committed IME text is
//! handled separately (a later milestone); here a single typed character comes
//! through `Key::Character`.

use winit::event::KeyEvent;
use winit::keyboard::{Key, ModifiersState, NamedKey};

use crate::input::{KeyChord, LogicalKey};

/// Map a winit key press to a [`KeyChord`], or `None` to ignore it.
pub fn chord_from_winit(event: &KeyEvent, mods: ModifiersState) -> Option<KeyChord> {
    if !event.state.is_pressed() {
        return None; // releases never reach the core
    }
    let key = match &event.logical_key {
        Key::Named(named) => match named {
            NamedKey::Enter => LogicalKey::Enter,
            NamedKey::Tab => LogicalKey::Tab,
            NamedKey::Backspace => LogicalKey::Backspace,
            NamedKey::Delete => LogicalKey::Delete,
            NamedKey::Escape => LogicalKey::Esc,
            NamedKey::ArrowLeft => LogicalKey::Left,
            NamedKey::ArrowRight => LogicalKey::Right,
            NamedKey::ArrowUp => LogicalKey::Up,
            NamedKey::ArrowDown => LogicalKey::Down,
            NamedKey::Home => LogicalKey::Home,
            NamedKey::End => LogicalKey::End,
            NamedKey::PageUp => LogicalKey::PageUp,
            NamedKey::PageDown => LogicalKey::PageDown,
            NamedKey::Space => LogicalKey::Char(' '),
            _ => return None,
        },
        Key::Character(s) => {
            let mut chars = s.chars();
            let c = chars.next()?;
            if chars.next().is_some() {
                return None; // multi-char strings arrive via IME commit (later)
            }
            // Control/Alt chords match lowercase letters in the core.
            let c = if mods.control_key() || mods.alt_key() {
                c.to_ascii_lowercase()
            } else {
                c
            };
            LogicalKey::Char(c)
        }
        _ => return None,
    };
    Some(KeyChord {
        key,
        ctrl: mods.control_key(),
        alt: mods.alt_key(),
        shift: mods.shift_key(),
    })
}
