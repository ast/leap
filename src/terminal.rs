//! Terminal setup/teardown.
//!
//! [`setup`] returns an RAII [`TerminalGuard`] that puts the terminal into raw
//! mode + the alternate screen, and restores it on drop — including on panic,
//! via an installed hook, so a crash never leaves the user's terminal wedged.

use std::io::{self, Write};

use anyhow::Result;
use crossterm::{
    cursor, execute,
    terminal::{self, EnterAlternateScreen, LeaveAlternateScreen},
};

/// Restores the terminal to its normal state when dropped.
pub struct TerminalGuard;

/// Enter raw mode + the alternate screen. The returned guard restores the
/// terminal when it goes out of scope.
pub fn setup() -> Result<TerminalGuard> {
    install_panic_hook();
    terminal::enable_raw_mode()?;
    execute!(io::stdout(), EnterAlternateScreen, cursor::Hide)?;
    Ok(TerminalGuard)
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = restore();
    }
}

fn restore() -> io::Result<()> {
    let mut out = io::stdout();
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
