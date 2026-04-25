//! Window animation preset configurations.

use crate::easing::EasingCurve;

/// Describes how to configure one animation.
#[derive(Debug, Clone, PartialEq)]
pub enum AnimPreset {
    /// Fixed-duration easing animation.
    Easing {
        /// Duration in milliseconds.
        duration_ms: u32,
        /// The easing curve.
        curve: EasingCurve,
    },
    /// Spring (damped harmonic oscillator) animation.
    Spring {
        /// Damping ratio — 1.0 = critically damped, <1.0 = underdamped.
        damping_ratio: f64,
        /// Spring stiffness constant.
        stiffness: f64,
        /// Settlement epsilon threshold.
        epsilon: f64,
    },
}

/// Preset animation configurations for all standard window state transitions.
pub struct WindowAnimationPresets {
    /// Window opening animation.
    pub open: AnimPreset,
    /// Window closing animation.
    pub close: AnimPreset,
    /// Window minimize animation.
    pub minimize: AnimPreset,
    /// Window unminimize animation.
    pub unminimize: AnimPreset,
    /// Window maximize animation.
    pub maximize: AnimPreset,
    /// Window unmaximize animation.
    pub unmaximize: AnimPreset,
    /// Window snap-to-edge animation.
    pub snap: AnimPreset,
    /// Focus ring pulse animation.
    pub focus_pulse: AnimPreset,
    /// Window drag/move follow animation.
    pub move_follow: AnimPreset,
}

impl Default for WindowAnimationPresets {
    fn default() -> Self {
        Self {
            open: AnimPreset::Easing {
                duration_ms: 250,
                curve: EasingCurve::EaseOutExpo,
            },
            close: AnimPreset::Easing {
                duration_ms: 200,
                curve: EasingCurve::EaseOutQuad,
            },
            minimize: AnimPreset::Spring {
                damping_ratio: 1.0,
                stiffness: 600.0,
                epsilon: 0.001,
            },
            unminimize: AnimPreset::Spring {
                damping_ratio: 1.0,
                stiffness: 600.0,
                epsilon: 0.001,
            },
            maximize: AnimPreset::Spring {
                damping_ratio: 1.0,
                stiffness: 800.0,
                epsilon: 0.001,
            },
            unmaximize: AnimPreset::Spring {
                damping_ratio: 1.0,
                stiffness: 800.0,
                epsilon: 0.001,
            },
            snap: AnimPreset::Spring {
                damping_ratio: 1.0,
                stiffness: 800.0,
                epsilon: 0.001,
            },
            focus_pulse: AnimPreset::Easing {
                duration_ms: 150,
                curve: EasingCurve::EaseInOutCubic,
            },
            move_follow: AnimPreset::Spring {
                damping_ratio: 0.8,
                stiffness: 1200.0,
                epsilon: 0.01,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_presets_have_expected_values() {
        let p = WindowAnimationPresets::default();

        assert!(
            matches!(p.open, AnimPreset::Easing { duration_ms: 250, curve: EasingCurve::EaseOutExpo }),
            "unexpected open preset"
        );
        assert!(
            matches!(
                p.move_follow,
                AnimPreset::Spring { damping_ratio, .. } if (damping_ratio - 0.8).abs() < 1e-9
            ),
            "unexpected move_follow preset"
        );
    }
}
