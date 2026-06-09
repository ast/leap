//! Front-ends: translate native input into [`KeyChord`](crate::input::KeyChord)s,
//! drive the [`Editor`](crate::editor::Editor) core, and render its
//! [`Frame`](crate::view::Frame).
//!
//! The crossterm [`terminal`] UI is always built. The winit/wgpu Wayland GUI
//! lives behind the `gui` cargo feature.

pub mod terminal;

#[cfg(feature = "gui")]
pub mod gui;
