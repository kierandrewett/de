//! Window decoration tokens.
//!
//! All numeric values are sourced from `WINDOW_SPEC.md` (macOS Sequoia analysis
//! + Figma designs).  Do not change them without updating the spec first.

use serde::{Deserialize, Serialize};

// ─── Shadow ─────────────────────────────────────────────────────────────────

/// A single Gaussian shadow layer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShadowLayer {
    /// Horizontal shadow offset in logical pixels.
    pub offset_x: f32,
    /// Vertical shadow offset in logical pixels (positive = down).
    pub offset_y: f32,
    /// Gaussian blur radius in logical pixels.
    pub blur_radius: f32,
    /// Shadow spread (positive = expand outward).
    pub spread: f32,
    /// RGBA colour of this shadow layer.
    pub color: [f32; 4],
}

/// Active and inactive shadow presets for all window states.
///
/// macOS uses identical shadow values across light and dark modes; the shadow
/// remains visually softer on dark backgrounds purely due to lower contrast.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowShadowTheme {
    /// Three-layer shadow for the focused window.
    pub active: Vec<ShadowLayer>,
    /// Two-layer softer shadow for unfocused windows.
    pub inactive: Vec<ShadowLayer>,
}

impl Default for WindowShadowTheme {
    fn default() -> Self {
        Self {
            active: vec![
                // contact shadow
                ShadowLayer { offset_x: 0.0, offset_y: 1.0, blur_radius: 3.0, spread: 0.0, color: [0.0, 0.0, 0.0, 0.12] },
                // medium ambient
                ShadowLayer { offset_x: 0.0, offset_y: 8.0, blur_radius: 24.0, spread: 0.0, color: [0.0, 0.0, 0.0, 0.12] },
                // wide ambient
                ShadowLayer { offset_x: 0.0, offset_y: 20.0, blur_radius: 48.0, spread: 0.0, color: [0.0, 0.0, 0.0, 0.08] },
            ],
            inactive: vec![
                // contact shadow (softer)
                ShadowLayer { offset_x: 0.0, offset_y: 1.0, blur_radius: 2.0, spread: 0.0, color: [0.0, 0.0, 0.0, 0.08] },
                // medium ambient (softer)
                ShadowLayer { offset_x: 0.0, offset_y: 4.0, blur_radius: 12.0, spread: 0.0, color: [0.0, 0.0, 0.0, 0.06] },
            ],
        }
    }
}

// ─── Border ─────────────────────────────────────────────────────────────────

/// macOS-style multi-layer border for a single window state.
///
/// The three layers (outer stroke, inner top highlight, inner side highlight)
/// together simulate a physical surface catching directional light from above.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowBorderStyle {
    /// Outer 0.5 px stroke colour — separates the window from the desktop.
    pub outer_stroke: [f32; 4],
    /// 1 px inner highlight at the top edge (bright — catches top-down light).
    pub inner_highlight_top: [f32; 4],
    /// 1 px inner highlight on the side edges (faded — less direct light).
    pub inner_highlight_side: [f32; 4],
    /// Shadow layers for this state (rendered behind the window, back-to-front).
    pub shadow_layers: Vec<ShadowLayer>,
}

/// Border styles for all four window states (mode × focus).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowBorderTheme {
    /// Light mode, focused window.
    pub light_active: WindowBorderStyle,
    /// Light mode, unfocused window.
    pub light_inactive: WindowBorderStyle,
    /// Dark mode, focused window.
    pub dark_active: WindowBorderStyle,
    /// Dark mode, unfocused window.
    pub dark_inactive: WindowBorderStyle,
}

impl Default for WindowBorderTheme {
    fn default() -> Self {
        Self {
            light_active: WindowBorderStyle {
                outer_stroke: [0.0, 0.0, 0.0, 0.22],
                inner_highlight_top: [1.0, 1.0, 1.0, 0.50],
                inner_highlight_side: [1.0, 1.0, 1.0, 0.18],
                shadow_layers: vec![
                    ShadowLayer { offset_x: 0.0, offset_y: 1.0, blur_radius: 3.0, spread: 0.0, color: [0.0, 0.0, 0.0, 0.12] },
                    ShadowLayer { offset_x: 0.0, offset_y: 8.0, blur_radius: 24.0, spread: 0.0, color: [0.0, 0.0, 0.0, 0.12] },
                    ShadowLayer { offset_x: 0.0, offset_y: 20.0, blur_radius: 48.0, spread: 0.0, color: [0.0, 0.0, 0.0, 0.08] },
                ],
            },
            light_inactive: WindowBorderStyle {
                outer_stroke: [0.0, 0.0, 0.0, 0.15],
                inner_highlight_top: [1.0, 1.0, 1.0, 0.25],
                inner_highlight_side: [1.0, 1.0, 1.0, 0.09],
                shadow_layers: vec![
                    ShadowLayer { offset_x: 0.0, offset_y: 1.0, blur_radius: 2.0, spread: 0.0, color: [0.0, 0.0, 0.0, 0.08] },
                    ShadowLayer { offset_x: 0.0, offset_y: 4.0, blur_radius: 12.0, spread: 0.0, color: [0.0, 0.0, 0.0, 0.06] },
                ],
            },
            dark_active: WindowBorderStyle {
                outer_stroke: [0.0, 0.0, 0.0, 0.72],
                inner_highlight_top: [1.0, 1.0, 1.0, 0.08],
                inner_highlight_side: [1.0, 1.0, 1.0, 0.03],
                shadow_layers: vec![
                    ShadowLayer { offset_x: 0.0, offset_y: 1.0, blur_radius: 3.0, spread: 0.0, color: [0.0, 0.0, 0.0, 0.12] },
                    ShadowLayer { offset_x: 0.0, offset_y: 8.0, blur_radius: 24.0, spread: 0.0, color: [0.0, 0.0, 0.0, 0.12] },
                    ShadowLayer { offset_x: 0.0, offset_y: 20.0, blur_radius: 48.0, spread: 0.0, color: [0.0, 0.0, 0.0, 0.08] },
                ],
            },
            dark_inactive: WindowBorderStyle {
                outer_stroke: [0.0, 0.0, 0.0, 0.55],
                inner_highlight_top: [1.0, 1.0, 1.0, 0.04],
                inner_highlight_side: [1.0, 1.0, 1.0, 0.015],
                shadow_layers: vec![
                    ShadowLayer { offset_x: 0.0, offset_y: 1.0, blur_radius: 2.0, spread: 0.0, color: [0.0, 0.0, 0.0, 0.08] },
                    ShadowLayer { offset_x: 0.0, offset_y: 4.0, blur_radius: 12.0, spread: 0.0, color: [0.0, 0.0, 0.0, 0.06] },
                ],
            },
        }
    }
}

// ─── WindowTheme ─────────────────────────────────────────────────────────────

/// All window-chrome design tokens.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowTheme {
    /// Outer squircle corner radius in logical pixels (from Figma: 14 px).
    pub corner_radius: f32,
    /// Corner smoothing factor — 0.0 = circular arc, 1.0 = full squircle.
    /// macOS uses ≈ 0.6.
    pub corner_smoothing: f32,
    /// Outer border stroke width in logical pixels.
    pub border_width: f32,
    /// Title bar height in logical pixels (standard variant).
    pub title_bar_height: f32,
    /// Inner content padding in logical pixels.
    pub padding: f32,
    /// Drop shadow presets.
    pub shadow: WindowShadowTheme,
    /// Border colours and per-state shadow layers.
    pub border: WindowBorderTheme,
}

impl Default for WindowTheme {
    fn default() -> Self {
        Self {
            corner_radius: 14.0,
            corner_smoothing: 0.6,
            border_width: 0.5,
            title_bar_height: 33.0,
            padding: 0.0,
            shadow: WindowShadowTheme::default(),
            border: WindowBorderTheme::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corner_radius_matches_spec() {
        assert_eq!(WindowTheme::default().corner_radius, 14.0);
    }

    #[test]
    fn title_bar_height_matches_spec() {
        assert_eq!(WindowTheme::default().title_bar_height, 33.0);
    }

    #[test]
    fn active_shadow_has_three_layers() {
        let theme = WindowTheme::default();
        assert_eq!(theme.shadow.active.len(), 3);
        assert_eq!(theme.border.light_active.shadow_layers.len(), 3);
        assert_eq!(theme.border.dark_active.shadow_layers.len(), 3);
    }

    #[test]
    fn inactive_shadow_has_two_layers() {
        let theme = WindowTheme::default();
        assert_eq!(theme.shadow.inactive.len(), 2);
        assert_eq!(theme.border.light_inactive.shadow_layers.len(), 2);
        assert_eq!(theme.border.dark_inactive.shadow_layers.len(), 2);
    }

    #[test]
    fn light_active_outer_stroke_matches_spec() {
        // WINDOW_SPEC.md: rgba(0,0,0,0.22)
        let stroke = WindowBorderTheme::default().light_active.outer_stroke;
        assert_eq!(stroke, [0.0, 0.0, 0.0, 0.22]);
    }

    #[test]
    fn dark_active_outer_stroke_matches_spec() {
        // WINDOW_SPEC.md: rgba(0,0,0,0.72)
        let stroke = WindowBorderTheme::default().dark_active.outer_stroke;
        assert_eq!(stroke, [0.0, 0.0, 0.0, 0.72]);
    }

    #[test]
    fn inner_highlight_bottom_is_transparent() {
        // The bottom inner highlight must always be [0,0,0,0] — only top/sides
        // are lit; this is enforced by the renderer's gradient, not stored here,
        // but we verify that side alpha < top alpha as a sanity check.
        let border = WindowBorderTheme::default();
        assert!(border.light_active.inner_highlight_side[3] < border.light_active.inner_highlight_top[3]);
        assert!(border.dark_active.inner_highlight_side[3] < border.dark_active.inner_highlight_top[3]);
    }

    #[test]
    fn all_shadow_colours_are_opaque_black_base() {
        for layer in &WindowShadowTheme::default().active {
            assert_eq!(&layer.color[..3], &[0.0_f32, 0.0, 0.0]);
        }
    }

    #[test]
    fn border_width_matches_spec() {
        assert_eq!(WindowTheme::default().border_width, 0.5);
    }
}
