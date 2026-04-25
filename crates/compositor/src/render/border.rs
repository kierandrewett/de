//! macOS-style multi-layer window border rendering.
//!
//! The border system replicates macOS Sequoia's three-layer lighting model:
//!
//! 1. **Outer stroke** (0.5 px) — separates the window from the desktop.
//!    Colour varies by light/dark mode and active/inactive state.
//!
//! 2. **Inner highlight** (1 px inset) — simulates top-down directional lighting.
//!    The highlight is non-uniform: full brightness along the top edge, fading
//!    as it wraps around the sides, completely absent at the bottom.
//!
//! All values are sourced from `WINDOW_SPEC.md`.
//!
//! # GPU integration (TODO — subagent 07)
//!
//! Both render methods currently fall back to approximate CPU-compositable
//! coloured quads. The final implementation should use
//! [`OUTER_STROKE_FRAG_GLSL`][crate::render::clip::OUTER_STROKE_FRAG_GLSL] and
//! [`INNER_HIGHLIGHT_FRAG_GLSL`][crate::render::clip::INNER_HIGHLIGHT_FRAG_GLSL]
//! bound through Smithay's custom pixel shader API.

use rounding::SquircleConfig;
use theme::WindowBorderStyle;

use super::{FrameRef, Rect};

// ─── Inner highlight alpha formula ───────────────────────────────────────────

/// Compute the inner highlight alpha at a normalised vertical position.
///
/// `y_norm` is `0.0` at the top of the window and `1.0` at the bottom.
/// `top_alpha` is the alpha of the highlight colour at the very top edge
/// (from [`WindowBorderStyle::inner_highlight_top`]).
///
/// The formula from `WINDOW_SPEC.md`:
/// ```text
/// alpha = top_alpha * clamp(1.0 - y_normalised * 1.5, 0.0, 1.0)
/// ```
/// This gives:
/// - Top (`y_norm=0.0`): full `top_alpha`
/// - Two-thirds down (`y_norm=0.667`): zero
/// - Bottom: zero (clamped)
pub fn inner_highlight_alpha(y_norm: f32, top_alpha: f32) -> f32 {
    top_alpha * (1.0 - y_norm * 1.5).clamp(0.0, 1.0)
}

/// Compute the fade colour for the inner highlight at a given vertical position.
///
/// Returns an RGBA colour where the alpha is modulated by `inner_highlight_alpha`.
/// RGB channels are taken from `top_color`.
pub fn inner_highlight_color_at(y_norm: f32, top_color: [f32; 4]) -> [f32; 4] {
    let alpha = inner_highlight_alpha(y_norm, top_color[3]);
    [top_color[0], top_color[1], top_color[2], alpha]
}

// ─── BorderRenderer ──────────────────────────────────────────────────────────

/// Renders the outer border stroke and inner highlight for one window.
///
/// Both passes are thin (sub-pixel to 1 px), so they have no meaningful CPU-side
/// geometry to cache — they are shader-driven in the final implementation.
pub struct BorderRenderer;

impl BorderRenderer {
    /// Create a new border renderer.
    pub fn new() -> Self {
        Self
    }

    /// Render the outer 0.5 px squircle stroke.
    ///
    /// The stroke follows the outer edge of `window_rect`. In the GPU
    /// implementation, [`OUTER_STROKE_FRAG_GLSL`][crate::render::clip::OUTER_STROKE_FRAG_GLSL]
    /// is used; here we approximate with a 1 px inset solid quad edge at the
    /// top of the window (simplest approximation visible without a shader).
    ///
    /// # TODO (GPU integration)
    /// Replace with a full-quad draw using the `outer_stroke` GLSL shader and
    /// `ClipParams` uniforms.
    pub fn render_outer_stroke<F: FrameRef>(
        &self,
        frame: &mut F,
        window_rect: Rect,
        _config: SquircleConfig,
        border_style: &WindowBorderStyle,
    ) -> Result<(), F::Error> {
        let stroke_w = 0.5_f32;
        let color = border_style.outer_stroke;

        // Top edge
        frame.draw_colored_quad(
            Rect::new(window_rect.x, window_rect.y, window_rect.width, stroke_w),
            color,
        )?;
        // Bottom edge
        frame.draw_colored_quad(
            Rect::new(
                window_rect.x,
                window_rect.y + window_rect.height - stroke_w,
                window_rect.width,
                stroke_w,
            ),
            color,
        )?;
        // Left edge
        frame.draw_colored_quad(
            Rect::new(window_rect.x, window_rect.y, stroke_w, window_rect.height),
            color,
        )?;
        // Right edge
        frame.draw_colored_quad(
            Rect::new(
                window_rect.x + window_rect.width - stroke_w,
                window_rect.y,
                stroke_w,
                window_rect.height,
            ),
            color,
        )?;

        Ok(())
    }

    /// Render the 1 px inset inner highlight gradient.
    ///
    /// The highlight is rendered as a gradient quad: `top_color` at the top
    /// of the window, transparent at the bottom. In the GPU implementation,
    /// [`INNER_HIGHLIGHT_FRAG_GLSL`][crate::render::clip::INNER_HIGHLIGHT_FRAG_GLSL]
    /// produces a per-pixel SDF-accurate version.
    ///
    /// # TODO (GPU integration)
    /// Replace with the `inner_highlight` GLSL shader with `ClipParams` uniforms.
    pub fn render_inner_highlight<F: FrameRef>(
        &self,
        frame: &mut F,
        window_rect: Rect,
        border_style: &WindowBorderStyle,
    ) -> Result<(), F::Error> {
        // Compute top and bottom colours for the gradient approximation.
        let top_color = border_style.inner_highlight_top;
        let bottom_color = [top_color[0], top_color[1], top_color[2], 0.0_f32];

        let inset = 1.0_f32;
        let highlight_rect = window_rect.inset(inset);

        if highlight_rect.width <= 0.0 || highlight_rect.height <= 0.0 {
            return Ok(());
        }

        frame.draw_gradient_quad(highlight_rect, top_color, bottom_color)
    }
}

impl Default for BorderRenderer {
    fn default() -> Self {
        Self::new()
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inner_highlight_alpha_at_top() {
        // y_norm=0 → full top_alpha.
        let a = inner_highlight_alpha(0.0, 0.50);
        assert!((a - 0.50).abs() < 1e-6, "got {a}");
    }

    #[test]
    fn inner_highlight_alpha_at_bottom() {
        // y_norm=1.0 → alpha = top_alpha * clamp(1 - 1.5, 0, 1) = 0.
        let a = inner_highlight_alpha(1.0, 0.50);
        assert_eq!(a, 0.0);
    }

    #[test]
    fn inner_highlight_alpha_at_two_thirds() {
        // y_norm ≈ 0.667 → alpha = 0.50 * 0 = 0.
        let a = inner_highlight_alpha(2.0 / 3.0, 0.50);
        assert!(a < 1e-4, "expected ~0, got {a}");
    }

    #[test]
    fn inner_highlight_alpha_monotone_decreasing() {
        let top = 0.50;
        let samples = [0.0, 0.1, 0.25, 0.5, 0.667, 0.8, 1.0];
        let mut prev = f32::MAX;
        for &y in &samples {
            let a = inner_highlight_alpha(y, top);
            assert!(a <= prev + 1e-6, "not monotone at y={y}: {a} > {prev}");
            prev = a;
        }
    }

    #[test]
    fn inner_highlight_color_at_preserves_rgb() {
        let top_color = [1.0_f32, 1.0, 1.0, 0.50];
        let color = inner_highlight_color_at(0.0, top_color);
        assert_eq!(&color[..3], &[1.0_f32, 1.0, 1.0]);
        assert!((color[3] - 0.50).abs() < 1e-6);
    }

    #[test]
    fn inner_highlight_color_at_bottom_is_transparent() {
        let top_color = [1.0_f32, 1.0, 1.0, 0.50];
        let color = inner_highlight_color_at(1.0, top_color);
        assert_eq!(color[3], 0.0);
    }

    #[test]
    fn light_active_top_alpha_matches_spec() {
        // WINDOW_SPEC.md: light active top = rgba(255,255,255,0.50)
        let style = theme::WindowBorderTheme::default().light_active;
        assert!((style.inner_highlight_top[3] - 0.50).abs() < 1e-6);
    }

    #[test]
    fn dark_active_top_alpha_matches_spec() {
        // WINDOW_SPEC.md: dark active top = rgba(255,255,255,0.08)
        let style = theme::WindowBorderTheme::default().dark_active;
        assert!((style.inner_highlight_top[3] - 0.08).abs() < 1e-6);
    }

    #[test]
    fn top_alpha_always_greater_than_side_alpha() {
        let b = theme::WindowBorderTheme::default();
        for style in [&b.light_active, &b.light_inactive, &b.dark_active, &b.dark_inactive] {
            assert!(
                style.inner_highlight_top[3] >= style.inner_highlight_side[3],
                "top alpha must be >= side alpha"
            );
        }
    }

    #[test]
    fn inner_highlight_alpha_clamped_not_negative() {
        // Very large y_norm values must not produce negative alpha.
        let a = inner_highlight_alpha(10.0, 1.0);
        assert!(a >= 0.0);
    }
}
