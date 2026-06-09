//! leap-gui — the Wayland GUI binary (Canon Cat edition).
//!
//! Built only with `--features gui`. Thin wrapper: open the workspace and hand
//! off to the GUI front-end. All logic lives in the `leap` library crate.

use anyhow::Result;

use leap::editor::Editor;
use leap::{frontend, workspace};

fn main() -> Result<()> {
    let store = workspace::open()?;
    let editor = Editor::new(store)?;
    frontend::gui::run(editor)
}
