//! Window animation preset tokens.
//!
//! These types are defined locally until the `animation` crate is available.
// TODO: import from animation crate once available

use serde::{Deserialize, Serialize};

/// Easing curve variant for window animations.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum Easing {
    /// Constant velocity — no acceleration.
    Linear,
    /// Slow in, fast out.
    EaseIn,
    /// Fast in, slow out.
    EaseOut,
    /// Slow in and slow out (typical UI default).
    EaseInOut,
    /// Overshoot and settle (spring-like feel).
    Spring,
}

/// Duration and easing for a single animation phase.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnimationPreset {
    /// Duration in milliseconds.
    pub duration_ms: u32,
    /// Easing curve.
    pub easing: Easing,
}

/// Preset animation configurations for window lifecycle events.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowAnimationPresets {
    /// Window opening / map animation.
    pub open: AnimationPreset,
    /// Window closing / unmap animation.
    pub close: AnimationPreset,
    /// Window minimize animation.
    pub minimize: AnimationPreset,
    /// Window restore-from-minimize animation.
    pub unminimize: AnimationPreset,
    /// Window maximize / tile animation.
    pub maximize: AnimationPreset,
    /// Window focus gain (opacity or scale nudge).
    pub focus: AnimationPreset,
}

impl Default for WindowAnimationPresets {
    fn default() -> Self {
        Self {
            open: AnimationPreset { duration_ms: 220, easing: Easing::EaseOut },
            close: AnimationPreset { duration_ms: 160, easing: Easing::EaseIn },
            minimize: AnimationPreset { duration_ms: 300, easing: Easing::EaseInOut },
            unminimize: AnimationPreset { duration_ms: 300, easing: Easing::EaseOut },
            maximize: AnimationPreset { duration_ms: 260, easing: Easing::EaseInOut },
            focus: AnimationPreset { duration_ms: 80, easing: Easing::EaseOut },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_is_faster_than_minimize() {
        let p = WindowAnimationPresets::default();
        assert!(p.open.duration_ms < p.minimize.duration_ms);
    }

    #[test]
    fn close_is_faster_than_open() {
        let p = WindowAnimationPresets::default();
        assert!(p.close.duration_ms < p.open.duration_ms);
    }

    #[test]
    fn focus_is_snappiest() {
        let p = WindowAnimationPresets::default();
        let all = [p.open.duration_ms, p.close.duration_ms, p.minimize.duration_ms, p.maximize.duration_ms];
        assert!(all.iter().all(|&d| d > p.focus.duration_ms));
    }

    #[test]
    fn easing_variants_derive_eq() {
        assert_eq!(Easing::EaseOut, Easing::EaseOut);
        assert_ne!(Easing::EaseIn, Easing::EaseOut);
    }
}
