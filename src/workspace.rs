//! Opening the single fixed workspace — shared by every front-end binary.
//!
//! There are no files on this branch; the workspace is one SQLite database under
//! the XDG data dir (overridable via `LEAP_WORKSPACE`, for tests). A brand-new
//! workspace is seeded with the tutorial as its first content.

use anyhow::Result;

use crate::store::{EditOp, Store};
use crate::tutorial;

/// Open (creating + seeding if needed) the workspace. `LEAP_WORKSPACE` overrides
/// the path — undocumented, for tests so they never touch the real database.
pub fn open() -> Result<Store> {
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
    Ok(store)
}
