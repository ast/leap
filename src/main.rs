//! leap — the terminal binary (Canon Cat edition).
//!
//! Thin wrapper: parse the CLI, open the workspace, and hand off to the terminal
//! front-end. All logic lives in the `leap` library crate (see `lib.rs`). The
//! Wayland GUI is a separate binary (`leap-gui`, built with `--features gui`).

use anyhow::Result;
use clap::Parser;

use leap::editor::Editor;
use leap::{frontend, workspace};

/// A modeless, LEAP-driven terminal text editor in the spirit of the Canon Cat.
///
/// There are no files: the entire workspace is one continuous text stream kept
/// in a database under your XDG data dir, resumed exactly where you left off.
#[derive(Parser, Debug)]
#[command(name = "leap", version, about)]
struct Cli {}

fn main() -> Result<()> {
    let _cli = Cli::parse();
    let store = workspace::open()?;
    let editor = Editor::new(store)?;
    frontend::terminal::run(editor)
}
