//! The Wayland GUI front-end (winit + wgpu + glyphon), behind the `gui` feature.
//!
//! Opens a window, sets up the GPU, decodes input into the core's
//! [`KeyChord`](crate::input::KeyChord), and renders the editor's
//! [`Frame`](crate::view::Frame) in the Canon Cat palette. Redraws happen on
//! demand (input / resize / scale / IME) — idle costs no frames. IME composition
//! shows inline at the cursor; the kill ring syncs with the system clipboard.

mod font;
mod input;
mod render;

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use winit::application::ApplicationHandler;
use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::event::{Ime, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::ModifiersState;
use winit::window::{Window, WindowId};

use crate::editor::Editor;
use crate::input::{KeyChord, LogicalKey};
use crate::view::Row;
use render::{Gpu, LeapDraw, RenderStats, Scene};

/// Frame-time instrumentation, enabled by setting the `LEAP_PERF` env var.
/// Prints a summary every 120 rendered frames and on exit.
#[derive(Default)]
struct Perf {
    on: bool,
    frames: u32,
    cpu_sum: u128,
    cpu_max: u128,
    reshapes: u32,
}

impl Perf {
    fn new() -> Self {
        Self {
            on: std::env::var_os("LEAP_PERF").is_some(),
            ..Default::default()
        }
    }

    fn record(&mut self, s: RenderStats) {
        if !self.on {
            return;
        }
        self.frames += 1;
        self.cpu_sum += s.cpu_us;
        self.cpu_max = self.cpu_max.max(s.cpu_us);
        self.reshapes += u32::from(s.reshaped);
        if self.frames >= 120 {
            self.flush();
        }
    }

    fn flush(&mut self) {
        if !self.on || self.frames == 0 {
            return;
        }
        eprintln!(
            "leap-perf: {} frames · cpu avg {:.2}ms max {:.2}ms · {} reshapes",
            self.frames,
            self.cpu_sum as f64 / self.frames as f64 / 1000.0,
            self.cpu_max as f64 / 1000.0,
            self.reshapes,
        );
        self.frames = 0;
        self.cpu_sum = 0;
        self.cpu_max = 0;
        self.reshapes = 0;
    }
}

/// Exponential time constant for scroll easing (seconds): snappy but smooth.
const SCROLL_TAU: f32 = 0.06;

/// Cursor blink half-period (seconds) — the Canon Cat's blinking insertion cursor.
const BLINK_PERIOD: f64 = 0.53;

/// Ease `current` toward `target` over `dt` seconds; snaps when within 0.5 px.
fn approach(current: f32, target: f32, dt: f32) -> f32 {
    if (target - current).abs() < 0.5 {
        target
    } else {
        current + (target - current) * (1.0 - (-dt / SCROLL_TAU).exp())
    }
}

/// Whether the blinking cursor is in its visible half-cycle.
fn blink_visible(epoch: Instant, now: Instant) -> bool {
    let phase = now.saturating_duration_since(epoch).as_secs_f64() / BLINK_PERIOD;
    (phase as u64).is_multiple_of(2)
}

/// Run the editor in a Wayland window until it quits.
pub fn run(editor: Editor) -> Result<()> {
    let event_loop = EventLoop::new()?;
    event_loop.set_control_flow(ControlFlow::Wait); // redraw on demand, idle otherwise
    let clipboard = match arboard::Clipboard::new() {
        Ok(c) => Some(c),
        Err(e) => {
            eprintln!("leap-gui: system clipboard unavailable ({e}); using internal kill ring only");
            None
        }
    };
    let mut app = App {
        editor,
        window: None,
        gpu: None,
        modifiers: ModifiersState::empty(),
        preedit: String::new(),
        clipboard,
        scroll_px: None,
        last_frame: None,
        blink_epoch: Instant::now(),
        blink_drawn: true,
        perf: Perf::new(),
    };
    event_loop.run_app(&mut app)?;
    Ok(())
}

struct App {
    editor: Editor,
    window: Option<Arc<Window>>,
    gpu: Option<Gpu>,
    modifiers: ModifiersState,
    /// In-progress IME composition text, shown inline until committed.
    preedit: String,
    clipboard: Option<arboard::Clipboard>,
    /// Eased pixel scroll offset (`None` until the first frame / after a resize,
    /// when it snaps to the target).
    scroll_px: Option<f32>,
    /// Timestamp of the last rendered frame, for animation timing.
    last_frame: Option<Instant>,
    /// When the current blink cycle started (reset on input so the cursor is
    /// solid right after you act, then blinks while idle).
    blink_epoch: Instant,
    /// The blink visibility last drawn, so we only repaint on a real toggle.
    blink_drawn: bool,
    perf: Perf,
}

impl App {
    /// Reset the blink so the cursor is solid immediately after activity.
    fn poke_blink(&mut self) {
        self.blink_epoch = Instant::now();
        self.blink_drawn = true;
    }
}

impl App {
    fn exit(&mut self, event_loop: &ActiveEventLoop) {
        self.perf.flush();
        let _ = self.editor.shutdown();
        event_loop.exit();
    }

    /// Dispatch one key chord into the editor, syncing the kill ring with the
    /// system clipboard, then persist and request a repaint.
    fn handle_chord(&mut self, chord: KeyChord, event_loop: &ActiveEventLoop) {
        self.poke_blink();
        // Pull the system clipboard into the kill ring just before a yank, so
        // C-y pastes what was copied in another app.
        let is_yank = chord.ctrl && matches!(chord.key, LogicalKey::Char('y'));
        if is_yank
            && let Some(cb) = &mut self.clipboard
            && let Ok(text) = cb.get_text()
        {
            self.editor.set_kill_ring(text);
        }

        let before = self.editor.kill_ring().to_string();
        self.editor.input(chord);
        if let Err(e) = self.editor.persist_edits() {
            eprintln!("leap-gui: persist error: {e}");
        }
        // Mirror a kill (C-k / C-w) out to the system clipboard.
        if self.editor.kill_ring() != before
            && let Some(cb) = &mut self.clipboard
        {
            let _ = cb.set_text(self.editor.kill_ring().to_string());
        }

        if !self.editor.running() {
            self.exit(event_loop);
            return;
        }
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    /// Advance the scroll animation, build the pixel-positioned scene, and draw.
    /// Keeps requesting redraws until the scroll has settled (idle costs none).
    fn redraw(&mut self) {
        let now = Instant::now();
        let dt = self
            .last_frame
            .map(|t| (now - t).as_secs_f32().min(0.05))
            .unwrap_or(0.0);
        self.last_frame = Some(now);

        let Some(gpu) = self.gpu.as_mut() else {
            return;
        };
        let (cols, rows) = gpu.grid();
        let frame = match self.editor.compute_frame(cols, rows) {
            Ok(f) => f,
            Err(e) => {
                eprintln!("leap-gui: frame error: {e}");
                return;
            }
        };
        let lh = gpu.line_height();
        let adv = gpu.advance();
        let text_rows = rows.saturating_sub(2);
        let left = self.editor.view_left();
        let top = self.editor.view_top();

        // Ease the pixel scroll toward the target top line.
        let target = top as f32 * lh;
        let cur = self.scroll_px.unwrap_or(target);
        let scroll_px = approach(cur, target, dt);
        let settled = scroll_px == target;
        self.scroll_px = Some(scroll_px);

        // Row window covering the (possibly fractional) scroll position.
        let first = (scroll_px / lh).floor().max(0.0) as usize;
        let body = self.editor.rows_at(first, text_rows + 2, left, cols);
        let body_top_px = first as f32 * lh - scroll_px;

        // Canon Cat two-part cursor, glued to the scroll so the blocks + inverse
        // glyphs share the fractional offset.
        let cursor_line = top + frame.cursor.1;
        let cursor_col = frame.cursor.0;
        let cursor_y = cursor_line as f32 * lh - scroll_px;
        let cursor_px = (cursor_col as f32 * adv, cursor_y);
        let row_text = body.get(cursor_line.saturating_sub(first));
        let glyph_at = |col: usize| -> Option<String> {
            match row_text {
                Some(Row::Text(t)) => t.chars().nth(col).map(|c| c.to_string()),
                _ => None,
            }
        };
        let cursor_glyph = glyph_at(cursor_col);
        // Solid erase highlight on the character to the left — only in the Cat's
        // "wide" (just-typed) state, and not at the start of a line. After a
        // move/leap the cursor is "narrow": a single blinking block.
        let highlight = (self.editor.cursor_wide() && cursor_col > 0)
            .then(|| (((cursor_col - 1) as f32 * adv, cursor_y), glyph_at(cursor_col - 1)));
        let cursor_visible = blink_visible(self.blink_epoch, now);
        self.blink_drawn = cursor_visible;

        // LEAP highlight placed in the animated window.
        let leap = frame.leap_hl.map(|(rel, s, e)| {
            let line = top + rel;
            let matched: String = match body.get(line.saturating_sub(first)) {
                Some(Row::Text(t)) => t.chars().skip(s).take(e.saturating_sub(s)).collect(),
                _ => String::new(),
            };
            (
                s as f32 * adv,
                line as f32 * lh - scroll_px,
                e.saturating_sub(s) as f32 * adv,
                matched,
            )
        });

        let scene = Scene {
            rows: &body,
            body_top_px,
            text_rows,
            status: &frame.status,
            echo: &frame.echo,
            cursor_px,
            cursor_glyph: cursor_glyph.as_deref(),
            cursor_visible,
            highlight_px: highlight.as_ref().map(|(px, _)| *px),
            highlight_glyph: highlight.as_ref().and_then(|(_, g)| g.as_deref()),
            leap: leap.as_ref().map(|(x, y, w, m)| LeapDraw {
                x: *x,
                y: *y,
                width: *w,
                matched: m,
            }),
            preedit: &self.preedit,
        };
        match gpu.render(&scene) {
            Ok(stats) => self.perf.record(stats),
            Err(e) => eprintln!("leap-gui: render error: {e}"),
        }

        if let Some(w) = &self.window {
            // Place the IME candidate window at the cursor.
            w.set_ime_cursor_area(
                PhysicalPosition::new(cursor_px.0 as f64, cursor_px.1 as f64),
                PhysicalSize::new(adv as f64, lh as f64),
            );
            // Keep animating until the scroll settles.
            if !settled {
                w.request_redraw();
            }
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return; // already initialized (Wayland can resume more than once)
        }
        let attrs = Window::default_attributes().with_title("leap");
        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                eprintln!("leap-gui: failed to create window: {e}");
                event_loop.exit();
                return;
            }
        };
        window.set_ime_allowed(true); // dead keys / compose / CJK
        match Gpu::new(window.clone()) {
            Ok(gpu) => {
                self.gpu = Some(gpu);
                self.window = Some(window);
            }
            Err(e) => {
                eprintln!("leap-gui: GPU init failed: {e}");
                event_loop.exit();
                return;
            }
        }
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => self.exit(event_loop),

            WindowEvent::ModifiersChanged(modifiers) => {
                self.modifiers = modifiers.state();
            }

            WindowEvent::KeyboardInput { event, .. } => {
                if let Some(chord) = input::chord_from_winit(&event, self.modifiers) {
                    self.handle_chord(chord, event_loop);
                }
            }

            WindowEvent::Ime(ime) => {
                match ime {
                    Ime::Commit(text) => {
                        self.editor.insert_text(&text);
                        self.preedit.clear();
                        if let Err(e) = self.editor.persist_edits() {
                            eprintln!("leap-gui: persist error: {e}");
                        }
                    }
                    Ime::Preedit(text, _cursor) => self.preedit = text,
                    Ime::Enabled | Ime::Disabled => self.preedit.clear(),
                }
                self.poke_blink();
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }

            WindowEvent::Resized(size) => {
                if let Some(gpu) = &mut self.gpu {
                    gpu.resize(size.width, size.height);
                }
                self.scroll_px = None; // snap (metrics/viewport changed)
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }

            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                if let Some(gpu) = &mut self.gpu {
                    gpu.set_scale(scale_factor as f32);
                }
                self.scroll_px = None;
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }

            WindowEvent::RedrawRequested => self.redraw(),

            _ => {}
        }
    }

    /// Drive the cursor blink while idle: wake at the next toggle and repaint
    /// only when the visible state actually flips (a scroll animation, if any,
    /// drives its own redraws and isn't affected).
    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let Some(w) = &self.window else {
            return;
        };
        let now = Instant::now();
        let period = Duration::from_secs_f64(BLINK_PERIOD);
        let phase = (now.saturating_duration_since(self.blink_epoch).as_secs_f64() / BLINK_PERIOD) as u64;
        let next = self.blink_epoch + period.checked_mul((phase + 1) as u32).unwrap_or(period);
        event_loop.set_control_flow(ControlFlow::WaitUntil(next));
        if phase.is_multiple_of(2) != self.blink_drawn {
            w.request_redraw();
        }
    }
}
