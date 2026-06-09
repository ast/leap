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

/// Frame metadata — everything a front-end needs *except* the row contents,
/// which it builds from [`Editor::row_at`](crate::editor::Editor)/`rows_at`. The
/// terminal front-end uses [`Frame`] (rows included); the GUI uses this plus
/// `rows_at` so it can render the rows around an in-progress scroll animation.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct FrameMeta {
    /// First visible line and column (the settled scroll target).
    pub top: usize,
    pub left: usize,
    /// Text-area height in rows (viewport minus the two chrome rows).
    pub text_rows: usize,
    pub status: String,
    pub echo: String,
    /// Cursor `(col, row)` in cells, viewport-relative.
    pub cursor: (usize, usize),
    pub full_repaint: bool,
    pub redraw_text: bool,
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
    /// Per-text-row inverse highlight (selection or LEAP match): one entry per
    /// row, each an optional `(start_col, end_col)` span in visible cells.
    pub highlights: Vec<Option<(usize, usize)>>,
    /// The whole surface should be cleared and repainted (recenter / LEAP
    /// land/cancel / first frame). Front-ends that always repaint can ignore it.
    pub full_repaint: bool,
    /// Only the text rows should be force-repainted (a LEAP session is active and
    /// the highlight may have moved). Cheaper than `full_repaint`.
    pub redraw_text: bool,
}
