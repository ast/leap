//! leap — a modeless, LEAP-driven editor (Canon Cat edition).
//!
//! The crate is split into a **UI-agnostic core** and one or more **front-ends**:
//!
//! - Core: [`buffer`], [`store`], [`leap`], [`echo`], [`statusline`], [`editor`],
//!   plus the [`input`] (logical keys) and [`view`] (frame model) abstractions and
//!   [`workspace`] (open the database). No terminal or windowing types appear here.
//! - Front-ends ([`frontend`]): translate native input → [`input::KeyChord`], drive
//!   [`editor::Editor`], and render its [`view::Frame`]. The crossterm terminal UI
//!   is always built; the winit/wgpu Wayland GUI is behind the `gui` feature.
//!
//! See `docs/CANON_CAT.md` for the fileless, database-backed design.

pub mod buffer;
pub mod calc;
pub mod echo;
pub mod editor;
pub mod frontend;
pub mod input;
pub mod leap;
pub mod statusline;
pub mod store;
pub mod tutorial;
pub mod view;
pub mod workspace;
