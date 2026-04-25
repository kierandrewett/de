//! Colour palette tokens.

use serde::{Deserialize, Serialize};

/// macOS blue — `#007AFF` = `rgb(0, 122, 255)`.
const ACCENT_BLUE: [f32; 4] = [0.0, 0.478, 1.0, 1.0];

/// Full colour palette for a theme mode.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ColorPalette {
    /// Primary accent colour (buttons, links, selection highlights).
    pub accent: [f32; 4],
    /// Page / desktop background.
    pub background: [f32; 4],
    /// Default surface colour (cards, panels, windows).
    pub surface: [f32; 4],
    /// Elevated surface (popovers, menus, dialogs drawn above `surface`).
    pub surface_elevated: [f32; 4],
    /// Primary text rendered on a surface.
    pub on_surface: [f32; 4],
    /// Secondary / dimmed text rendered on a surface.
    pub on_surface_dim: [f32; 4],
    /// Subtle separator / outline colour.
    pub border: [f32; 4],
    /// Drop-shadow base colour (always opaque black; alpha in shadow layers).
    pub shadow: [f32; 4],
    /// Destructive action colour (close button, delete, errors).
    pub destructive: [f32; 4],
    /// Success / confirmation colour.
    pub success: [f32; 4],
    /// Warning colour.
    pub warning: [f32; 4],
}

impl ColorPalette {
    /// Dark mode palette.
    pub fn dark() -> Self {
        Self {
            accent: ACCENT_BLUE,
            // #1D1D1D
            background: [0.114, 0.114, 0.114, 1.0],
            // #111318 — title bar / surface base
            surface: [0.067, 0.075, 0.094, 1.0],
            // slightly lighter elevated surface
            surface_elevated: [0.15, 0.16, 0.19, 1.0],
            // rgba(255,255,255,0.80)
            on_surface: [1.0, 1.0, 1.0, 0.80],
            // rgba(255,255,255,0.45)
            on_surface_dim: [1.0, 1.0, 1.0, 0.45],
            // rgba(255,255,255,0.08)
            border: [1.0, 1.0, 1.0, 0.08],
            shadow: [0.0, 0.0, 0.0, 1.0],
            // #FF453A — macOS dark-mode destructive red
            destructive: [1.0, 0.271, 0.227, 1.0],
            // #32D74B — macOS dark-mode green
            success: [0.196, 0.843, 0.294, 1.0],
            // #FFD60A — macOS dark-mode yellow
            warning: [1.0, 0.839, 0.039, 1.0],
        }
    }

    /// Light mode palette.
    pub fn light() -> Self {
        Self {
            accent: ACCENT_BLUE,
            // #F6F6F6
            background: [0.965, 0.965, 0.965, 1.0],
            // #FFFFFF
            surface: [1.0, 1.0, 1.0, 1.0],
            surface_elevated: [1.0, 1.0, 1.0, 1.0],
            // rgba(0,0,0,0.75)
            on_surface: [0.0, 0.0, 0.0, 0.75],
            // rgba(0,0,0,0.45)
            on_surface_dim: [0.0, 0.0, 0.0, 0.45],
            // rgba(0,0,0,0.12)
            border: [0.0, 0.0, 0.0, 0.12],
            shadow: [0.0, 0.0, 0.0, 1.0],
            // #FF3B30 — macOS light-mode destructive red
            destructive: [1.0, 0.231, 0.188, 1.0],
            // #34C759 — macOS light-mode green
            success: [0.204, 0.780, 0.349, 1.0],
            // #FF9F0A — macOS light-mode orange/yellow
            warning: [1.0, 0.624, 0.039, 1.0],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dark_accent_is_blue() {
        let [r, g, b, a] = ColorPalette::dark().accent;
        assert_eq!(r, 0.0);
        assert!(g > 0.4 && g < 0.6);
        assert_eq!(b, 1.0);
        assert_eq!(a, 1.0);
    }

    #[test]
    fn light_background_is_near_white() {
        let [r, g, b, _a] = ColorPalette::light().background;
        assert!(r > 0.9 && g > 0.9 && b > 0.9);
    }

    #[test]
    fn all_channels_in_range() {
        for palette in [ColorPalette::dark(), ColorPalette::light()] {
            for channel in palette.accent {
                assert!((0.0..=1.0).contains(&channel));
            }
        }
    }
}
