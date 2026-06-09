//! leap — a modeless, LEAP-driven terminal text editor (Canon Cat edition).
//!
//! See `docs/CANON_CAT.md` for this branch's fileless, database-backed design,
//! and `docs/DESIGN.md` for the original architecture.

mod buffer;
mod echo;
mod editor;
mod leap;
mod statusline;
mod store;
mod terminal;
mod tutorial;

use anyhow::Result;
use clap::Parser;

use crate::editor::Editor;
use crate::store::{EditOp, Store};

/// A modeless, LEAP-driven terminal text editor in the spirit of the Canon Cat.
///
/// There are no files: the entire workspace is one continuous text stream kept
/// in a database under your XDG data dir, resumed exactly where you left off.
#[derive(Parser, Debug)]
#[command(name = "leap", version, about)]
struct Cli {}

fn main() -> Result<()> {
    let _cli = Cli::parse();

    // Open (or create) the single fixed workspace. `LEAP_WORKSPACE` overrides
    // the path — undocumented, for tests so they never touch the real database.
    let path = match std::env::var_os("LEAP_WORKSPACE") {
        Some(p) => p.into(),
        None => Store::default_path()?,
    };
    let mut store = Store::open(&path)?;

    // First run: seed the empty workspace with the tutorial, as the Cat shipped
    // its manual *inside* the workspace — ordinary text you can edit or delete.
    if store.head() == 0 {
        store.append(
            &[EditOp::Insert {
                pos: 0,
                text: tutorial::TUTORIAL.to_string(),
            }],
            0,
        )?;
    }

    // The terminal guard restores raw mode / alt screen on any exit (incl. panic).
    let _guard = terminal::setup()?;
    let mut editor = Editor::new(store)?;
    editor.run()
}
