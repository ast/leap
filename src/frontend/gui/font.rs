//! GUI font selection and monospace cell metrics.
//!
//! The default family + size are compile-time consts (matching the project's
//! compile-time-config philosophy), overridable at launch via `LEAP_FONT` /
//! `LEAP_FONT_SIZE` and resolved against installed fonts via fontdb. A missing
//! family falls back to the generic system monospace so the GUI always starts.

use glyphon::fontdb;
use glyphon::{Attrs, Buffer, Family, FontSystem, Metrics, Shaping};

/// Default font family (override with `LEAP_FONT`).
pub const DEFAULT_FONT_FAMILY: &str = "JetBrains Mono";
/// Default point size before HiDPI scaling (override with `LEAP_FONT_SIZE`).
pub const DEFAULT_FONT_SIZE: f32 = 13.0;

/// The pixel size of one monospace character cell.
#[derive(Clone, Copy, Debug)]
pub struct CellMetrics {
    pub advance: f32,
    pub line_height: f32,
}

/// Resolve the configured family against installed fonts. Returns the family
/// name to use, or `None` to fall back to the generic monospace family.
pub fn resolve_family(font_system: &FontSystem) -> Option<String> {
    let name = std::env::var("LEAP_FONT").unwrap_or_else(|_| DEFAULT_FONT_FAMILY.to_string());
    let found = font_system
        .db()
        .query(&fontdb::Query {
            families: &[fontdb::Family::Name(&name)],
            ..Default::default()
        })
        .is_some();
    if found {
        Some(name)
    } else {
        eprintln!("leap-gui: font {name:?} not found; using system monospace");
        None
    }
}

/// The configured size (`LEAP_FONT_SIZE` or the default).
pub fn resolved_size() -> f32 {
    std::env::var("LEAP_FONT_SIZE")
        .ok()
        .and_then(|s| s.parse::<f32>().ok())
        .filter(|s| *s > 0.0)
        .unwrap_or(DEFAULT_FONT_SIZE)
}

/// cosmic-text metrics for an already-scaled pixel font size.
pub fn metrics(font_size_px: f32) -> Metrics {
    Metrics::new(font_size_px, (font_size_px * 1.3).ceil())
}

/// Measure the monospace cell box by shaping a run of `M`s and dividing.
pub fn measure_cell(font_system: &mut FontSystem, m: Metrics, family: Family) -> CellMetrics {
    const N: usize = 20;
    let sample = "M".repeat(N);
    let mut buffer = Buffer::new(font_system, m);
    buffer.set_size(font_system, Some(100_000.0), Some(m.line_height));
    let attrs = Attrs::new().family(family);
    buffer.set_text(font_system, &sample, &attrs, Shaping::Basic, None);
    buffer.shape_until_scroll(font_system, false);
    let advance = buffer
        .layout_runs()
        .next()
        .map(|run| run.line_w / N as f32)
        .filter(|w| *w > 0.0)
        .unwrap_or(m.font_size * 0.6);
    CellMetrics {
        advance,
        line_height: m.line_height,
    }
}
