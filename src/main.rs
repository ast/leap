//! leap — a modeless, LEAP-driven terminal text editor.
//!
//! See `docs/DESIGN.md` for the architecture and milestone plan.

mod buffer;
mod echo;
mod editor;
mod finder;
mod hold;
mod leap;
mod statusline;
mod terminal;
mod walk;

use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;

use crate::editor::Editor;

/// A modeless, LEAP-driven terminal text editor in the spirit of the Canon Cat.
#[derive(Parser, Debug)]
#[command(name = "leap", version, about)]
struct Cli {
    /// File to open.
    file: Option<PathBuf>,

    /// Position the view at this 1-based line number.
    #[arg(long, value_name = "N", default_value_t = 1)]
    line: usize,

    /// Open the file read-only.
    #[arg(long)]
    readonly: bool,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    // Hold the terminal guard for the whole session; dropping it (normal exit,
    // `?` error, or panic) restores the terminal.
    let guard = terminal::setup()?;
    let mut editor = Editor::open(cli.file, cli.line, cli.readonly, guard.kbd_enhanced())?;
    editor.run()
}
