//! A hold-to-confirm gesture.
//!
//! Raskin rejected modal yes/no questions. Instead of an "Are you sure?" dialog,
//! a destructive action requires *sustained key pressure*: arm on press, fire
//! only after the key has been held long enough, abort the instant it's released.
//! This is the same family as the Canon Cat's held keys (LEAP itself is one), so
//! "hold for power actions" is a single consistent idea across the editor.
//!
//! Release detection is exact under the Kitty keyboard protocol (real key-release
//! events). Without it we fall back to a heuristic: key auto-repeat keeps the hold
//! alive, and a gap longer than `release_gap` is taken to mean the key is up.
//!
//! The type is clock-injected — every method takes `now: Instant` — so the timing
//! logic is unit-testable without sleeping.

use std::time::{Duration, Instant};

/// One hold-to-confirm gesture (e.g. hold-to-quit). Reusable across actions.
pub struct Hold {
    /// How long the key must be held before the gesture fires.
    duration: Duration,
    /// Fallback only: a gap between repeats longer than this means "released".
    release_gap: Duration,
    /// Whether real key-release events are available (Kitty protocol).
    kbd_enhanced: bool,
    /// When the current hold started, or `None` if not armed.
    armed: Option<Instant>,
    /// Last press/repeat seen — drives the fallback release detection.
    last: Instant,
    /// Latched after firing until the key is released, so holding past the
    /// fire point can't trigger the action again.
    consumed: bool,
}

impl Hold {
    pub fn new(duration: Duration, release_gap: Duration, kbd_enhanced: bool, now: Instant) -> Self {
        Self {
            duration,
            release_gap,
            kbd_enhanced,
            armed: None,
            last: now,
            consumed: false,
        }
    }

    /// Register a press or auto-repeat of the gesture key: arm the hold (or keep
    /// it armed). Re-arms after a fallback release gap so a fresh press starts a
    /// fresh hold.
    pub fn press(&mut self, now: Instant) {
        if !self.consumed {
            let rearm = self.armed.is_none()
                || (!self.kbd_enhanced && now.duration_since(self.last) > self.release_gap);
            if rearm {
                self.armed = Some(now);
            }
        }
        self.last = now;
    }

    /// Register a key release (Kitty protocol): end the gesture immediately and
    /// clear the post-fire latch.
    pub fn release(&mut self) {
        self.armed = None;
        self.consumed = false;
    }

    /// Advance the gesture. Returns `true` exactly once, on the tick where the
    /// hold has lasted long enough.
    pub fn poll(&mut self, now: Instant) -> bool {
        // Fallback release: a gap in auto-repeats means the key was let go.
        if !self.kbd_enhanced
            && self.armed.is_some()
            && now.duration_since(self.last) > self.release_gap
        {
            self.armed = None;
            self.consumed = false;
            return false;
        }
        if let Some(start) = self.armed
            && now.duration_since(start) >= self.duration
        {
            self.armed = None;
            self.consumed = true;
            return true;
        }
        false
    }

    /// Whether the gesture is currently being held.
    pub fn is_armed(&self) -> bool {
        self.armed.is_some()
    }

    /// Progress toward firing in `0.0..=1.0` while held, else `None`. Drives the
    /// on-screen meter.
    pub fn progress(&self, now: Instant) -> Option<f32> {
        self.armed.map(|start| {
            (now.duration_since(start).as_secs_f32() / self.duration.as_secs_f32()).clamp(0.0, 1.0)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    fn hold(kbd_enhanced: bool, t0: Instant) -> Hold {
        Hold::new(ms(650), ms(200), kbd_enhanced, t0)
    }

    #[test]
    fn fires_after_duration_then_latches() {
        let t0 = Instant::now();
        let mut h = hold(true, t0);
        h.press(t0);
        assert!(!h.poll(t0 + ms(100)), "too soon");
        assert_eq!(h.progress(t0 + ms(325)).map(|f| (f * 100.0).round() as u32), Some(50));
        assert!(h.poll(t0 + ms(650)), "fires at the threshold");
        assert!(!h.poll(t0 + ms(800)), "does not fire twice");
        assert_eq!(h.progress(t0 + ms(800)), None, "disarmed after firing");
    }

    #[test]
    fn stays_consumed_until_release() {
        let t0 = Instant::now();
        let mut h = hold(true, t0);
        h.press(t0);
        assert!(h.poll(t0 + ms(650)));
        // Still holding: a repeat must not re-arm.
        h.press(t0 + ms(700));
        assert_eq!(h.progress(t0 + ms(700)), None);
        // Release, then a new press arms a fresh hold.
        h.release();
        h.press(t0 + ms(900));
        assert!(h.progress(t0 + ms(900)).is_some());
    }

    #[test]
    fn enhanced_ignores_repeat_gaps() {
        // With real release events, only `release()` ends a hold — a long gap
        // since the last press does not.
        let t0 = Instant::now();
        let mut h = hold(true, t0);
        h.press(t0);
        assert!(h.poll(t0 + ms(5000)), "still armed despite the gap");
    }

    #[test]
    fn fallback_gap_disarms() {
        let t0 = Instant::now();
        let mut h = hold(false, t0);
        h.press(t0);
        assert!(!h.poll(t0 + ms(300)), "gap > release_gap → treated as released");
        assert_eq!(h.progress(t0 + ms(300)), None);
    }

    #[test]
    fn fallback_repeats_keep_it_alive_to_fire() {
        let t0 = Instant::now();
        let mut h = hold(false, t0);
        h.press(t0);
        // Realistic auto-repeats (each gap < release_gap) keep `last` fresh, so
        // the hold start stays at t0 and accumulates toward the threshold.
        for n in (100..=600).step_by(100) {
            h.press(t0 + ms(n));
            assert!(h.is_armed(), "stays armed at {n}ms");
        }
        assert!(h.poll(t0 + ms(650)), "fires: armed since t0, last repeat recent");
    }
}
