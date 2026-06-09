//! GPU rendering of a [`Frame`] with wgpu + glyphon, in the Canon Cat palette.
//!
//! Dark **ink** text on a **paper**-white background. The status line, the LEAP
//! match, and the cursor are drawn *inverse* (paper-on-ink) — the Cat's look —
//! using a small solid-color quad pass behind the glyphs: glyphon itself draws
//! no backgrounds, so we paint ink rectangles first, then overlay the affected
//! text again in paper so it stays legible.

use std::sync::Arc;
use std::time::Instant;

use anyhow::{anyhow, Result};
use glyphon::cosmic_text::Hinting;
use glyphon::{
    Attrs, Buffer, Cache, Color, Family, FontSystem, Metrics, Resolution, Shaping, SwashCache,
    TextArea, TextAtlas, TextBounds, TextRenderer, Viewport, Wrap,
};
use wgpu::util::DeviceExt;
use winit::window::Window;

use super::font::{self, CellMetrics};
use crate::view::Row;

/// Per-frame timing/counters for the `LEAP_PERF` instrumentation.
#[derive(Clone, Copy)]
pub struct RenderStats {
    /// CPU time for layout + glyph prepare + encode (excludes the vsync present wait).
    pub cpu_us: u128,
    /// Whether the body was re-shaped this frame (vs. served from cache).
    pub reshaped: bool,
}

/// A pixel-positioned description of one frame, laid out by the front-end (which
/// owns the scroll animation). The renderer just draws it.
pub struct Scene<'a> {
    /// The visible row window (one extra row top & bottom for partial rows).
    pub rows: &'a [Row],
    /// Pixel y of `rows[0]` (≤ 0 when the first row is partially scrolled off).
    pub body_top_px: f32,
    /// Text-area height in rows (viewport minus the two chrome rows).
    pub text_rows: usize,
    pub status: &'a str,
    pub echo: &'a str,
    /// Canon Cat two-part cursor — the **blinking** insertion cursor (where the
    /// next char appears): cell top-left in pixels, the glyph under it, and the
    /// blink state.
    pub cursor_px: (f32, f32),
    pub cursor_glyph: Option<&'a str>,
    pub cursor_visible: bool,
    /// …and the **solid** erase highlight on the character left of the cursor
    /// (what Backspace removes). `None` at the start of a line, or while a span
    /// selection is active.
    pub highlight_px: Option<(f32, f32)>,
    pub highlight_glyph: Option<&'a str>,
    /// Inverse highlight spans (the selection — possibly multi-row — or a LEAP
    /// match), one per highlighted visible row.
    pub spans: &'a [HighlightSpan<'a>],
    /// In-progress IME composition, shown inline at the cursor.
    pub preedit: &'a str,
}

/// One inverse-highlight span (selection row or LEAP match), in pixels. `text`
/// is the highlighted substring, re-drawn in paper over the ink rectangle.
pub struct HighlightSpan<'a> {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub text: &'a str,
}

// --- Canon Cat palette (compile-time) ------------------------------------

/// Paper-white background (and inverse foreground).
const PAPER: (u8, u8, u8) = (0xF7, 0xF4, 0xEC);
/// Dark ink text (and inverse background).
const INK: (u8, u8, u8) = (0x1C, 0x1A, 0x17);

fn color(rgb: (u8, u8, u8)) -> Color {
    Color::rgb(rgb.0, rgb.1, rgb.2)
}

/// The configured font family, or the generic monospace fallback.
fn family_of(font_family: &Option<String>) -> Family<'_> {
    match font_family {
        Some(name) => Family::Name(name),
        None => Family::Monospace,
    }
}

/// sRGB byte → linear float, for clear color and quad fills on an sRGB surface.
fn srgb_to_linear(u: u8) -> f32 {
    let c = u as f32 / 255.0;
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn linear(rgb: (u8, u8, u8)) -> [f32; 4] {
    [
        srgb_to_linear(rgb.0),
        srgb_to_linear(rgb.1),
        srgb_to_linear(rgb.2),
        1.0,
    ]
}

// --- solid-color quad pass ------------------------------------------------

const QUAD_SHADER: &str = r#"
struct VsOut { @builtin(position) clip: vec4<f32>, @location(0) color: vec4<f32> };

@vertex
fn vs(@location(0) pos: vec2<f32>, @location(1) color: vec4<f32>) -> VsOut {
    var out: VsOut;
    out.clip = vec4<f32>(pos, 0.0, 1.0);
    out.color = color;
    return out;
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> { return in.color; }
"#;

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct QuadVertex {
    pos: [f32; 2],
    color: [f32; 4],
}

const QUAD_ATTRS: [wgpu::VertexAttribute; 2] =
    wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x4];

/// Max quads we ever draw in a frame (status bar + cursor + one per highlighted
/// row for a full-screen selection).
const QUAD_CAPACITY: usize = 256;

/// Append a pixel-space rectangle (two triangles) to `verts`. `rect` is
/// `[x0, y0, x1, y1]` in pixels; `screen` is `[width, height]` in pixels.
fn push_rect(verts: &mut Vec<QuadVertex>, rect: [f32; 4], screen: [f32; 2], color: [f32; 4]) {
    let [x0, y0, x1, y1] = rect;
    let [w, h] = screen;
    let nx = |x: f32| x / w * 2.0 - 1.0;
    let ny = |y: f32| 1.0 - y / h * 2.0;
    let tl = [nx(x0), ny(y0)];
    let tr = [nx(x1), ny(y0)];
    let bl = [nx(x0), ny(y1)];
    let br = [nx(x1), ny(y1)];
    for pos in [tl, tr, bl, tr, br, bl] {
        verts.push(QuadVertex { pos, color });
    }
}

/// Build the graphical ruler tick marks (quad geometry) for a surface: a short
/// tick per column, taller every 5, tallest every 10, bottom-aligned on the
/// ruler row. Generated once per resize/scale and cached in a vertex buffer.
fn ruler_verts(width: u32, height: u32, cell: CellMetrics, scale: f32) -> Vec<QuadVertex> {
    let (w, h) = (width as f32, height as f32);
    let (adv, lh) = (cell.advance, cell.line_height);
    let rows = (h / lh).floor() as usize;
    let ruler_y = rows.saturating_sub(3) as f32 * lh; // ruler is the first chrome row
    let screen = [w, h];
    let ink = linear(INK);
    let tw = scale.max(1.0); // tick width in px
    let cols = (w / adv).floor() as usize;
    let mut verts = Vec::with_capacity((cols + 1) * 6);
    for c in 0..=cols {
        // Tens columns carry a number instead of a tick (see ruler_labels).
        if c > 0 && c.is_multiple_of(10) {
            continue;
        }
        let th = lh * if c.is_multiple_of(5) { 0.45 } else { 0.22 };
        let x = c as f32 * adv + adv / 2.0 - tw / 2.0;
        push_rect(&mut verts, [x, ruler_y + lh - th, x + tw, ruler_y + lh], screen, ink);
    }
    verts
}

/// The ruler number row: the tens digit at every 10th column (`10`→`1`,
/// `80`→`8`), spaces elsewhere — laid out monospace so digits land on column.
fn ruler_labels(width: u32, advance: f32) -> String {
    let cols = (width as f32 / advance).floor() as usize;
    (0..=cols)
        .map(|c| {
            if c > 0 && c.is_multiple_of(10) {
                std::char::from_digit((c / 10 % 10) as u32, 10).unwrap_or(' ')
            } else {
                ' '
            }
        })
        .collect()
}

/// Build a one-line monospace glyph buffer (status/echo/ruler-labels/overlays).
fn build_line(
    fs: &mut FontSystem,
    metrics: Metrics,
    advance: f32,
    width: f32,
    family: Family,
    text: &str,
    color: Color,
) -> Buffer {
    let mut b = Buffer::new(fs, metrics);
    b.set_wrap(fs, Wrap::None);
    b.set_hinting(fs, Hinting::Enabled);
    b.set_monospace_width(fs, Some(advance));
    b.set_size(fs, Some(width), Some(metrics.line_height));
    let attrs = Attrs::new().family(family).color(color);
    b.set_text(fs, text, &attrs, Shaping::Basic, None);
    b.shape_until_scroll(fs, false);
    b
}

// --- GPU state ------------------------------------------------------------

/// The GPU surface, glyph renderer, and the solid-quad pipeline.
pub struct Gpu {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    font_system: FontSystem,
    swash_cache: SwashCache,
    atlas: TextAtlas,
    viewport: Viewport,
    text_renderer: TextRenderer,
    text_buffer: Buffer,
    quad_pipeline: wgpu::RenderPipeline,
    quad_buffer: wgpu::Buffer,
    /// Cached graphical-ruler tick geometry (rebuilt only on resize/scale).
    ruler_quad_buffer: wgpu::Buffer,
    ruler_quad_verts: u32,
    /// Cached ruler number glyphs (tens digits), shaped once per resize/scale.
    ruler_label_buf: Buffer,
    metrics: Metrics,
    cell: CellMetrics,
    scale: f32,
    /// The device's max 2D texture dimension; surfaces are clamped to it.
    max_dim: u32,
    /// Resolved font family name, or `None` for the generic monospace fallback.
    font_family: Option<String>,
    /// Base (unscaled) point size.
    base_size: f32,
    /// The body text + surface dims last shaped, so a cursor-only move (same
    /// text) doesn't re-layout the whole screen.
    last_body: String,
    last_dims: (u32, u32),
}

impl Gpu {
    /// Set up the surface, device, glyphon pipeline, and quad pipeline.
    pub fn new(window: Arc<Window>) -> Result<Self> {
        let size = window.inner_size();
        let scale = window.scale_factor() as f32;

        let instance =
            wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
        let surface = instance.create_surface(window)?;
        // Integrated GPU by default (lower power, avoids the discrete card);
        // WGPU_POWER_PREF=high forces high-performance.
        let power_preference = match std::env::var("WGPU_POWER_PREF").as_deref() {
            Ok("high") => wgpu::PowerPreference::HighPerformance,
            _ => wgpu::PowerPreference::LowPower,
        };
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference,
            compatible_surface: Some(&surface),
            force_fallback_adapter: false,
        }))
        .map_err(|e| anyhow!("no suitable GPU adapter: {e}"))?;
        let info = adapter.get_info();
        eprintln!(
            "leap-gui: adapter {:?} [{:?}] via {:?} (driver: {})",
            info.name, info.device_type, info.backend, info.driver
        );
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("leap-gui device"),
            required_features: wgpu::Features::empty(),
            // The adapter's real limits — downlevel_defaults caps textures at
            // 2048, which a fullscreen surface (e.g. 2560×1440) exceeds. The
            // adapter's own limits are always satisfiable and report the true max
            // texture size; this still works under the GL fallback.
            required_limits: adapter.limits(),
            ..Default::default()
        }))?;
        // Clamp the surface to the device's max texture size — a display larger
        // than the GPU's max would otherwise panic on configure.
        let max_dim = device.limits().max_texture_dimension_2d;

        let mut config = surface
            .get_default_config(
                &adapter,
                size.width.clamp(1, max_dim),
                size.height.clamp(1, max_dim),
            )
            .ok_or_else(|| anyhow!("surface not supported by the adapter"))?;
        // Prefer an sRGB surface: glyphon blends glyph coverage gamma-correctly
        // (ColorMode::Accurate) assuming an sRGB target, and our clear/quad colors
        // are converted to linear for the same. A non-sRGB surface (common on
        // Wayland) makes dark text look washed-out/thin.
        let caps = surface.get_capabilities(&adapter);
        if let Some(srgb) = caps.formats.iter().copied().find(|f| f.is_srgb()) {
            config.format = srgb;
        }
        config.present_mode = wgpu::PresentMode::Fifo;
        surface.configure(&device, &config);
        eprintln!("leap-gui: surface format {:?}", config.format);

        let mut font_system = FontSystem::new();
        let swash_cache = SwashCache::new();
        let cache = Cache::new(&device);
        let viewport = Viewport::new(&device, &cache);
        let mut atlas = TextAtlas::new(&device, &queue, &cache, config.format);
        let text_renderer =
            TextRenderer::new(&mut atlas, &device, wgpu::MultisampleState::default(), None);

        let quad_pipeline = build_quad_pipeline(&device, config.format);
        let quad_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("leap-gui quads"),
            size: (QUAD_CAPACITY * 6 * std::mem::size_of::<QuadVertex>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let font_family = font::resolve_family(&font_system);
        let base_size = font::resolved_size();
        let metrics = font::metrics(base_size * scale);
        let cell = font::measure_cell(&mut font_system, metrics, family_of(&font_family));
        let mut text_buffer = Buffer::new(&mut font_system, metrics);
        text_buffer.set_wrap(&mut font_system, Wrap::None);
        // Metrics hinting (off by default) snaps glyphs to integer X; with a
        // whole-pixel monospace width the grid is pixel-aligned → crisper text.
        text_buffer.set_hinting(&mut font_system, Hinting::Enabled);
        text_buffer.set_monospace_width(&mut font_system, Some(cell.advance));

        let ruler_init = ruler_verts(config.width, config.height, cell, scale);
        let ruler_quad_verts = ruler_init.len() as u32;
        let ruler_quad_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("leap-gui ruler"),
            contents: bytemuck::cast_slice(&ruler_init),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let ruler_label_buf = build_line(
            &mut font_system,
            metrics,
            cell.advance,
            config.width as f32,
            family_of(&font_family),
            &ruler_labels(config.width, cell.advance),
            color(INK),
        );

        Ok(Self {
            surface,
            device,
            queue,
            config,
            font_system,
            swash_cache,
            atlas,
            viewport,
            text_renderer,
            text_buffer,
            quad_pipeline,
            quad_buffer,
            ruler_quad_buffer,
            ruler_quad_verts,
            ruler_label_buf,
            metrics,
            cell,
            scale,
            max_dim,
            font_family,
            base_size,
            last_body: String::new(),
            last_dims: (0, 0),
        })
    }

    /// React to a HiDPI scale-factor change (recompute font + cell metrics).
    pub fn set_scale(&mut self, scale: f32) {
        if (scale - self.scale).abs() <= f32::EPSILON {
            return;
        }
        self.scale = scale;
        self.metrics = font::metrics(self.base_size * scale);
        let fam = family_of(&self.font_family);
        self.cell = font::measure_cell(&mut self.font_system, self.metrics, fam);
        self.text_buffer.set_metrics(&mut self.font_system, self.metrics);
        self.text_buffer
            .set_monospace_width(&mut self.font_system, Some(self.cell.advance));
        self.rebuild_ruler();
    }

    /// Regenerate the cached ruler tick geometry + number glyphs (after a resize
    /// or scale change).
    fn rebuild_ruler(&mut self) {
        let verts = ruler_verts(self.config.width, self.config.height, self.cell, self.scale);
        self.ruler_quad_verts = verts.len() as u32;
        self.ruler_quad_buffer = self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("leap-gui ruler"),
            contents: bytemuck::cast_slice(&verts),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let labels = ruler_labels(self.config.width, self.cell.advance);
        self.ruler_label_buf = self.make_line(&labels, color(INK));
    }

    /// Reconfigure the surface after a window resize (clamped to the GPU's max
    /// texture size).
    pub fn resize(&mut self, width: u32, height: u32) {
        self.config.width = width.clamp(1, self.max_dim);
        self.config.height = height.clamp(1, self.max_dim);
        self.surface.configure(&self.device, &self.config);
        self.rebuild_ruler();
    }

    /// Viewport size in character cells for the current surface + font.
    pub fn grid(&self) -> (usize, usize) {
        let cols = (self.config.width as f32 / self.cell.advance).floor().max(1.0) as usize;
        let rows = (self.config.height as f32 / self.cell.line_height).floor().max(1.0) as usize;
        (cols, rows)
    }

    /// Build a one-line text buffer (status, echo, and inverse overlays).
    fn make_line(&mut self, text: &str, color: Color) -> Buffer {
        build_line(
            &mut self.font_system,
            self.metrics,
            self.cell.advance,
            self.config.width as f32,
            family_of(&self.font_family),
            text,
            color,
        )
    }

    /// Pixel height of one text line (for the front-end's scroll math).
    pub fn line_height(&self) -> f32 {
        self.metrics.line_height
    }

    /// Pixel advance of one monospace cell.
    pub fn advance(&self) -> f32 {
        self.cell.advance
    }

    fn cols(&self) -> usize {
        (self.config.width as f32 / self.cell.advance).floor().max(1.0) as usize
    }

    /// Draw a [`Scene`] already laid out in pixels by the front-end.
    pub fn render(&mut self, scene: &Scene) -> Result<RenderStats> {
        let t0 = Instant::now();
        let mut reshaped = false;
        let composing = !scene.preedit.is_empty();
        let text_rows = scene.text_rows;
        let lh = self.metrics.line_height;
        let adv = self.cell.advance;
        let (w, h) = (self.config.width as f32, self.config.height as f32);
        let screen = [w, h];
        let cols = self.cols();

        // Body window text, one visual line per row.
        let mut body = String::new();
        for (i, row) in scene.rows.iter().enumerate() {
            match row {
                Row::Text(s) => body.push_str(s),
                Row::MarkerRule => body.push_str(&"─".repeat(cols)),
                Row::Tilde => body.push('~'),
            }
            if i + 1 < scene.rows.len() {
                body.push('\n');
            }
        }
        // Re-shape only when the window text or surface size changed; a sub-line
        // scroll keeps the same rows and just repositions the buffer.
        let dims = (self.config.width, self.config.height);
        if body != self.last_body || dims != self.last_dims {
            let ink_attrs = Attrs::new().family(family_of(&self.font_family)).color(color(INK));
            self.text_buffer.set_size(&mut self.font_system, Some(w), Some(h));
            self.text_buffer
                .set_text(&mut self.font_system, &body, &ink_attrs, Shaping::Basic, None);
            self.text_buffer.shape_until_scroll(&mut self.font_system, false);
            self.last_body = body;
            self.last_dims = dims;
            reshaped = true;
        }

        // Chrome rows: ruler, then the status bar, then echo.
        let ruler_y = text_rows as f32 * lh;
        let status_y = (text_rows as f32 + 1.0) * lh;
        let echo_y = (text_rows as f32 + 2.0) * lh;

        // Inverse ink rectangles: status bar, highlight spans, ruler marker, cursor.
        let ink_lin = linear(INK);
        let mut quads: Vec<QuadVertex> = Vec::with_capacity(QUAD_CAPACITY * 6);
        push_rect(&mut quads, [0.0, status_y, w, status_y + lh], screen, ink_lin);
        for s in scene.spans {
            push_rect(&mut quads, [s.x, s.y, s.x + s.width, s.y + lh], screen, ink_lin);
        }
        let (cur_x, cur_y) = scene.cursor_px;
        // The Cat's blinking column indicator: a thin vertical line through the
        // ruler row, centered on the cursor's column, blinking with the cursor.
        if scene.cursor_visible {
            let mw = (self.scale * 1.5).max(1.5);
            let mx = cur_x + adv / 2.0 - mw / 2.0;
            push_rect(&mut quads, [mx, ruler_y, mx + mw, ruler_y + lh], screen, ink_lin);
        }
        if !composing {
            // Solid erase highlight (the character to the left).
            if let Some((hx, hy)) = scene.highlight_px {
                push_rect(&mut quads, [hx, hy, hx + adv, hy + lh], screen, ink_lin);
            }
            // Blinking insertion cursor.
            if scene.cursor_visible {
                push_rect(&mut quads, [cur_x, cur_y, cur_x + adv, cur_y + lh], screen, ink_lin);
            }
        }
        self.queue
            .write_buffer(&self.quad_buffer, 0, bytemuck::cast_slice(&quads));
        let quad_verts = quads.len() as u32;

        // Paper overlays for the inverse regions.
        let status_buf = self.make_line(scene.status, color(PAPER));
        let echo_buf = self.make_line(scene.echo, color(INK));
        let span_bufs: Vec<(Buffer, f32, f32)> = scene
            .spans
            .iter()
            .filter(|s| !s.text.is_empty())
            .map(|s| (self.make_line(s.text, color(PAPER)), s.x, s.y))
            .collect();
        let highlight_buf = match (scene.highlight_px, scene.highlight_glyph) {
            (Some((hx, hy)), Some(g)) if !composing => {
                Some((self.make_line(g, color(PAPER)), hx, hy))
            }
            _ => None,
        };
        let cursor_buf = match scene.cursor_glyph {
            Some(g) if !composing && scene.cursor_visible => {
                Some((self.make_line(g, color(PAPER)), cur_x, cur_y))
            }
            _ => None,
        };
        let preedit_buf =
            composing.then(|| (self.make_line(scene.preedit, color(INK)), cur_x, cur_y));

        self.viewport.update(
            &self.queue,
            Resolution { width: self.config.width, height: self.config.height },
        );

        let surface_tex = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(t) | wgpu::CurrentSurfaceTexture::Suboptimal(t) => t,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                self.surface.configure(&self.device, &self.config);
                return Ok(RenderStats { cpu_us: t0.elapsed().as_micros(), reshaped });
            }
            _ => return Ok(RenderStats { cpu_us: t0.elapsed().as_micros(), reshaped }),
        };
        let view = surface_tex
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("leap-gui encoder") });

        let bounds = TextBounds { left: 0, top: 0, right: self.config.width as i32, bottom: self.config.height as i32 };
        // Clip the body to the text region so partial top/bottom rows during a
        // scroll don't bleed into the chrome.
        let body_bounds = TextBounds { left: 0, top: 0, right: self.config.width as i32, bottom: (text_rows as f32 * lh) as i32 };
        let mut areas = vec![
            TextArea { buffer: &self.text_buffer, left: 0.0, top: scene.body_top_px, scale: 1.0, bounds: body_bounds, default_color: color(INK), custom_glyphs: &[] },
            TextArea { buffer: &self.ruler_label_buf, left: 0.0, top: ruler_y, scale: 1.0, bounds, default_color: color(INK), custom_glyphs: &[] },
            TextArea { buffer: &status_buf, left: 0.0, top: status_y, scale: 1.0, bounds, default_color: color(PAPER), custom_glyphs: &[] },
            TextArea { buffer: &echo_buf, left: 0.0, top: echo_y, scale: 1.0, bounds, default_color: color(INK), custom_glyphs: &[] },
        ];
        for (buf, left, top) in &span_bufs {
            areas.push(TextArea { buffer: buf, left: *left, top: *top, scale: 1.0, bounds, default_color: color(PAPER), custom_glyphs: &[] });
        }
        if let Some((buf, left, top)) = &highlight_buf {
            areas.push(TextArea { buffer: buf, left: *left, top: *top, scale: 1.0, bounds, default_color: color(PAPER), custom_glyphs: &[] });
        }
        if let Some((buf, left, top)) = &cursor_buf {
            areas.push(TextArea { buffer: buf, left: *left, top: *top, scale: 1.0, bounds, default_color: color(PAPER), custom_glyphs: &[] });
        }
        if let Some((buf, left, top)) = &preedit_buf {
            areas.push(TextArea { buffer: buf, left: *left, top: *top, scale: 1.0, bounds, default_color: color(INK), custom_glyphs: &[] });
        }

        self.text_renderer.prepare(
            &self.device,
            &self.queue,
            &mut self.font_system,
            &mut self.atlas,
            &self.viewport,
            areas,
            &mut self.swash_cache,
        )?;

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("leap-gui pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: linear(PAPER)[0] as f64,
                            g: linear(PAPER)[1] as f64,
                            b: linear(PAPER)[2] as f64,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });

            // Inverse backgrounds first, then the cached graphical ruler ticks,
            // then the text (paper overlays on top).
            pass.set_pipeline(&self.quad_pipeline);
            pass.set_vertex_buffer(0, self.quad_buffer.slice(..));
            pass.draw(0..quad_verts, 0..1);
            pass.set_vertex_buffer(0, self.ruler_quad_buffer.slice(..));
            pass.draw(0..self.ruler_quad_verts, 0..1);

            self.text_renderer.render(&self.atlas, &self.viewport, &mut pass)?;
        }

        self.queue.submit(Some(encoder.finish()));
        // Measure CPU work *before* present so the vsync wait isn't counted.
        let cpu_us = t0.elapsed().as_micros();
        surface_tex.present();
        self.atlas.trim();
        Ok(RenderStats { cpu_us, reshaped })
    }
}

/// Build the solid-color triangle pipeline used for inverse rectangles.
fn build_quad_pipeline(device: &wgpu::Device, format: wgpu::TextureFormat) -> wgpu::RenderPipeline {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("leap-gui quad shader"),
        source: wgpu::ShaderSource::Wgsl(QUAD_SHADER.into()),
    });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("leap-gui quad layout"),
        bind_group_layouts: &[],
        immediate_size: 0,
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("leap-gui quad pipeline"),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            buffers: &[wgpu::VertexBufferLayout {
                array_stride: std::mem::size_of::<QuadVertex>() as u64,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &QUAD_ATTRS,
            }],
        },
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        multiview_mask: None,
        cache: None,
    })
}
