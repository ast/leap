//! Front-end-agnostic view model.
//!
//! [`Editor::compute_frame`](crate::editor::Editor::compute_frame) turns the
//! current editor state into a [`Frame`]: the visible rows, the chrome strings,
//! the cursor cell, and the LEAP highlight — all in **character cells**, with no
//! drawing. Each front-end maps a `Frame` to its medium (terminal cells, or GPU
//! glyphs at pixel positions). This keeps the rendering *policy* in the core and
//! the rendering *mechanism* in the front-ends.

/// One visible text-area row.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Row {
    /// Already display-expanded (tabs), horizontally scrolled, and clipped to the
    /// viewport width.
    Text(String),
    /// A document-boundary marker line — drawn as a full-width rule by the
    /// front-end.
    MarkerRule,
    /// Past the end of the buffer — drawn as a `~` placeholder.
    Tilde,
}

/// A complete, render-ready snapshot of the editor for one frame, in cells.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Frame {
    /// Exactly `text_rows` entries (viewport height minus the two chrome rows).
    pub rows: Vec<Row>,
    /// The status line, formatted to the full width.
    pub status: String,
    /// The echo line (LEAP label or a transient message), clipped to width.
    pub echo: String,
    /// Cursor position in cells, viewport-relative: `(col, row)`.
    pub cursor: (usize, usize),
    /// LEAP match highlight, viewport-relative: `(row, start_col, end_col)` in
    /// visible cells. `None` when not leaping / off-screen / no match.
    pub leap_hl: Option<(usize, usize, usize)>,
    /// The whole surface should be cleared and repainted (recenter / LEAP
    /// land/cancel / first frame). Front-ends that always repaint can ignore it.
    pub full_repaint: bool,
    /// Only the text rows should be force-repainted (a LEAP session is active and
    /// the highlight may have moved). Cheaper than `full_repaint`.
    pub redraw_text: bool,
}
