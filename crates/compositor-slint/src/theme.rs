//! theme.rs — Runtime theme state manager for light/dark mode + focus crossfades.
//!
//! # Overview
//!
//! This module owns two kinds of animated scalars:
//!
//!   1. **Global `mode_t`** (0.0 = dark, 1.0 = light): one value shared across all
//!      windows.  When `toggle_mode()` or `set_mode()` is called, it starts a
//!      200 ms ease-in-out animation toward the new value.
//!
//!   2. **Per-window `focus_t`** (0.0 = inactive, 1.0 = active): one value per
//!      window ID.  When a window gains focus (set_focused) its focus_t animates
//!      toward 1.0; when it loses focus it animates toward 0.0.  Both transitions
//!      use the same 200 ms ease-in-out.
//!
//! # Hotkey
//!
//! The caller (renderer.rs) is responsible for detecting the key press (Super+T)
//! and calling `toggle_mode()`.
//!
//! # Auto-schedule (optional, deliverable 9)
//!
//! If a `theme.json` config file exists in the standard config dir, and it
//! contains `"mode": "auto"`, `tick()` will switch modes based on the current
//! wall-clock time relative to the configured sunrise/sunset times.  The file
//! format is:
//! ```json
//! { "mode": "auto", "auto_sunrise": "06:00", "auto_sunset": "18:00" }
//! ```
//! or:
//! ```json
//! { "mode": "dark" }
//! ```
//! or:
//! ```json
//! { "mode": "light" }
//! ```
//!
//! The file is read once at startup; changes require a compositor restart.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use tracing::debug;

// ── Easing helper ─────────────────────────────────────────────────────────────

/// A simple 200 ms ease-in-out tween for a scalar value in [0, 1].
struct EaseTween {
    from: f32,
    to: f32,
    elapsed: Duration,
    duration: Duration,
}

impl EaseTween {
    const DURATION: Duration = Duration::from_millis(200);

    fn new(from: f32, to: f32) -> Self {
        Self {
            from,
            to,
            elapsed: Duration::ZERO,
            duration: Self::DURATION,
        }
    }

    /// Advance by `dt`, return the current interpolated value.
    fn tick(&mut self, dt: Duration) -> f32 {
        self.elapsed = (self.elapsed + dt).min(self.duration);
        let t = self.elapsed.as_secs_f32() / self.duration.as_secs_f32();
        let eased = ease_in_out(t);
        self.from + (self.to - self.from) * eased
    }

    fn is_complete(&self) -> bool {
        self.elapsed >= self.duration
    }

    fn value(&self) -> f32 {
        if self.is_complete() {
            return self.to;
        }
        let t = self.elapsed.as_secs_f32() / self.duration.as_secs_f32();
        let eased = ease_in_out(t);
        self.from + (self.to - self.from) * eased
    }
}

/// CSS ease-in-out approximation: cubic Bezier(0.42, 0, 0.58, 1).
#[inline]
fn ease_in_out(t: f32) -> f32 {
    if t < 0.5 {
        4.0 * t * t * t
    } else {
        let t1 = 2.0 * t - 2.0;
        0.5 * t1 * t1 * t1 + 1.0
    }
}

// ── ThemeMode ─────────────────────────────────────────────────────────────────

/// Light or dark visual mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeMode {
    /// Dark mode (default).
    Dark,
    /// Light mode.
    Light,
}

impl ThemeMode {
    /// Target value for `mode_t`.
    pub fn mode_t(self) -> f32 {
        match self {
            ThemeMode::Dark => 0.0,
            ThemeMode::Light => 1.0,
        }
    }
}

// ── Auto-schedule config ──────────────────────────────────────────────────────

/// Parsed `theme.json` configuration.
#[derive(Default)]
struct AutoConfig {
    /// `None` = explicit mode, `Some(sunrise_hour, sunset_hour)` = auto.
    auto_times: Option<(u8, u8)>,
    explicit_mode: Option<ThemeMode>,
}


fn load_theme_config() -> AutoConfig {
    // Look for theme.json in XDG_CONFIG_HOME/compositor-slint/ or ~/.config/compositor-slint/
    let config_dir = std::env::var("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
            std::path::PathBuf::from(home).join(".config")
        });
    let path = config_dir.join("compositor-slint").join("theme.json");

    if !path.exists() {
        return AutoConfig::default();
    }

    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) => {
            debug!("theme.json read error: {}", e);
            return AutoConfig::default();
        }
    };

    // Minimal manual parse — avoids adding serde_json as a new dep for this file.
    // (serde_json is already in Cargo.toml for the dock config, so we can use it.)
    parse_theme_json(&text)
}

fn parse_theme_json(text: &str) -> AutoConfig {
    // Attempt a simple serde_json parse if available.
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(text) {
        let mode_str = v.get("mode").and_then(|m| m.as_str()).unwrap_or("dark");
        match mode_str {
            "light" => {
                return AutoConfig {
                    auto_times: None,
                    explicit_mode: Some(ThemeMode::Light),
                }
            }
            "dark" => {
                return AutoConfig {
                    auto_times: None,
                    explicit_mode: Some(ThemeMode::Dark),
                }
            }
            "auto" => {
                let sunrise_str = v
                    .get("auto_sunrise")
                    .and_then(|s| s.as_str())
                    .unwrap_or("06:00");
                let sunset_str = v
                    .get("auto_sunset")
                    .and_then(|s| s.as_str())
                    .unwrap_or("18:00");
                let sunrise_h = parse_hour(sunrise_str).unwrap_or(6);
                let sunset_h = parse_hour(sunset_str).unwrap_or(18);
                return AutoConfig {
                    auto_times: Some((sunrise_h, sunset_h)),
                    explicit_mode: None,
                };
            }
            _ => {}
        }
    }
    AutoConfig::default()
}

fn parse_hour(hhmm: &str) -> Option<u8> {
    hhmm.split(':').next()?.parse().ok()
}

// ── ThemeState ────────────────────────────────────────────────────────────────

/// Central theme state.  Update by calling `tick()` each frame.
pub struct ThemeState {
    /// Current mode (the logical target, not the animation value).
    pub current_mode: ThemeMode,
    /// Animated global mode value: 0=dark, 1=light.
    mode_tween: EaseTween,
    /// Per-window animated focus value.
    /// Key = window_id (from WindowItem.id), value = tween toward 0.0 or 1.0.
    window_tweens: HashMap<i32, EaseTween>,
    /// Snapshot of current focus_t values, updated each tick.
    pub focus_values: HashMap<i32, f32>,
    /// Current global mode_t value.
    pub mode_t: f32,
    /// Timestamp of last tick (for dt calculation).
    last_tick: Instant,
    /// Auto-schedule config.
    auto_config: AutoConfig,
    /// Whether any animation is currently running (used to drive redraws).
    pub animating: bool,
}

impl ThemeState {
    /// Create with dark mode default, load `theme.json` if present.
    pub fn new() -> Self {
        let config = load_theme_config();
        let initial_mode = config.explicit_mode.unwrap_or(ThemeMode::Dark);
        let initial_t = initial_mode.mode_t();
        Self {
            current_mode: initial_mode,
            mode_tween: EaseTween {
                from: initial_t,
                to: initial_t,
                elapsed: EaseTween::DURATION,
                duration: EaseTween::DURATION,
            },
            window_tweens: HashMap::new(),
            focus_values: HashMap::new(),
            mode_t: initial_t,
            last_tick: Instant::now(),
            auto_config: config,
            animating: false,
        }
    }

    /// Toggle between dark and light mode, triggering a 200 ms crossfade.
    pub fn toggle_mode(&mut self) {
        let new_mode = match self.current_mode {
            ThemeMode::Dark => ThemeMode::Light,
            ThemeMode::Light => ThemeMode::Dark,
        };
        self.set_mode(new_mode);
    }

    /// Switch to a specific mode, triggering a 200 ms crossfade.
    pub fn set_mode(&mut self, mode: ThemeMode) {
        if mode == self.current_mode {
            return;
        }
        debug!("theme: switching to {:?}", mode);
        self.current_mode = mode;
        let current_t = self.mode_tween.value();
        self.mode_tween = EaseTween::new(current_t, mode.mode_t());
    }

    /// Notify that a window with `window_id` is now focused / unfocused.
    /// Starts a 200 ms ease-in-out crossfade on that window's focus_t.
    pub fn set_window_focused(&mut self, window_id: i32, focused: bool) {
        let target = if focused { 1.0_f32 } else { 0.0_f32 };
        let current = self
            .focus_values
            .get(&window_id)
            .copied()
            .unwrap_or(if focused { 0.0 } else { 1.0 });
        if (current - target).abs() < 0.001 {
            return; // already there
        }
        self.window_tweens
            .insert(window_id, EaseTween::new(current, target));
    }

    /// Remove tracking for a window (called when window closes).
    pub fn remove_window(&mut self, window_id: i32) {
        self.window_tweens.remove(&window_id);
        self.focus_values.remove(&window_id);
    }

    /// Advance all animations.  Call once per frame in the main loop.
    /// Returns `true` if any animation was still running (caller should request redraw).
    pub fn tick(&mut self) -> bool {
        let now = Instant::now();
        let dt = now.duration_since(self.last_tick);
        self.last_tick = now;

        // Auto-schedule: check wall clock if configured.
        if let Some((sunrise_h, sunset_h)) = self.auto_config.auto_times {
            let hour = chrono::Local::now().hour();
            let hour = hour as u8;
            let is_day = hour >= sunrise_h && hour < sunset_h;
            let expected = if is_day {
                ThemeMode::Light
            } else {
                ThemeMode::Dark
            };
            if expected != self.current_mode {
                self.set_mode(expected);
            }
        }

        let mut any_running = false;

        // Advance global mode tween.
        if !self.mode_tween.is_complete() {
            self.mode_t = self.mode_tween.tick(dt);
            any_running = true;
        } else {
            self.mode_t = self.mode_tween.value();
        }

        // Advance per-window focus tweens.
        let window_ids: Vec<i32> = self.window_tweens.keys().copied().collect();
        for id in window_ids {
            if let Some(tween) = self.window_tweens.get_mut(&id) {
                if !tween.is_complete() {
                    let v = tween.tick(dt);
                    self.focus_values.insert(id, v);
                    any_running = true;
                } else {
                    let v = tween.value();
                    self.focus_values.insert(id, v);
                    // Leave the tween in the map (it will be a no-op next tick).
                }
            }
        }

        self.animating = any_running;
        any_running
    }

    /// Get the current focus_t for a window (default: 0.0 = inactive).
    pub fn window_focus_t(&self, window_id: i32) -> f32 {
        self.focus_values.get(&window_id).copied().unwrap_or(0.0)
    }
}

// Use chrono to get current hour for auto-schedule.
use chrono::Timelike;

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn mode_starts_dark() {
        let state = ThemeState::new();
        // Without a theme.json the mode defaults to Dark.
        assert!(state.mode_t < 0.01, "mode_t should start at 0 (dark)");
    }

    #[test]
    fn toggle_triggers_animation() {
        let mut state = ThemeState::new();
        state.toggle_mode();
        assert_eq!(state.current_mode, ThemeMode::Light);
        // After one tick the animation should be partway through.
        state.tick();
        // mode_t should have moved toward 1.0 but not be there yet.
        assert!(state.mode_t > 0.0);
        assert!(state.mode_t < 1.0);
    }

    #[test]
    fn toggle_twice_goes_back_to_dark() {
        let mut state = ThemeState::new();
        state.toggle_mode();
        state.toggle_mode();
        assert_eq!(state.current_mode, ThemeMode::Dark);
    }

    #[test]
    fn window_focus_crossfade() {
        let mut state = ThemeState::new();
        state.set_window_focused(1, true);
        state.tick();
        let v = state.window_focus_t(1);
        assert!(v > 0.0 && v < 1.0, "focus_t should be mid-animation: {}", v);
    }

    #[test]
    fn parse_theme_json_light() {
        let cfg = parse_theme_json(r#"{"mode":"light"}"#);
        assert_eq!(cfg.explicit_mode, Some(ThemeMode::Light));
    }

    #[test]
    fn parse_theme_json_auto() {
        let cfg =
            parse_theme_json(r#"{"mode":"auto","auto_sunrise":"07:00","auto_sunset":"19:00"}"#);
        assert_eq!(cfg.auto_times, Some((7, 19)));
    }
}
