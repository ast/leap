//! Terminal setup/teardown.
//!
//! [`setup`] returns an RAII [`TerminalGuard`] that puts the terminal into raw
//! mode + the alternate screen, and restores it on drop — including on panic,
//! via an installed hook, so a crash never leaves the user's terminal wedged.
//!
//! It also negotiates the **Kitty keyboard protocol** where available, so held
//! keys report real release/repeat events (for future hold-to-confirm gestures).

use std::io::{self, Write};

use anyhow::Result;
use crossterm::event::{
    KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::{
    cursor, execute,
    terminal::{
        self, supports_keyboard_enhancement, EnterAlternateScreen, LeaveAlternateScreen,
    },
};

/// Restores the terminal to its normal state when dropped.
pub struct TerminalGuard;

/// Enter raw mode + the alternate screen and negotiate keyboard enhancement.
/// The returned guard restores the terminal when it goes out of scope.
pub fn setup() -> Result<TerminalGuard> {
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
