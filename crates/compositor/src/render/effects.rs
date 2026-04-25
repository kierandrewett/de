//! Post-compositing window effects.
//!
//! Currently manages two effects:
//!
//! - **Alpha modifier** (`alpha-modifier-v1`): applies a per-surface opacity
//!   multiplier negotiated with the client. Smithay's `AlphaModifierState` tracks
//!   the requested value; the renderer applies it as a uniform on the final quad.
//!
//! - **Background blur** (`ext-background-effect-v1`): Gaussian blur behind the
//!   window (e.g. for translucent GTK4 windows). Requires reading back the
//!   framebuffer beneath the window, blurring it, and compositing it under the
//!   client surface. This is a future feature — the infrastructure is scaffolded
//!   here so it can be wired up without touching other modules.
//!
//! # GPU integration (TODO — subagent 07)
//!
//! - Alpha modifier: pass `opacity` as a uniform when drawing the client texture.
//!   Currently folded into the `alpha` parameter in [`FrameRef::draw_pixels`].
//!
//! - Background blur: requires `GlesRenderer` render-to-texture support.
//!   Capture a rect of the current framebuffer → apply separable Gaussian blur
//!   on GPU → composite back before drawing the (translucent) client surface.

use super::{FrameRef, Rect};

// ─── AlphaModifier ───────────────────────────────────────────────────────────

/// Per-surface alpha modifier as negotiated via `alpha-modifier-v1`.
///
/// `1.0` means fully opaque; `0.0` means fully transparent.
/// Values outside `[0.0, 1.0]` are clamped.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AlphaModifier(f32);

impl AlphaModifier {
    /// Full opacity (default).
    pub const OPAQUE: Self = Self(1.0);

    /// Create a modifier from a raw `[0.0, 1.0]` value.
    pub fn new(value: f32) -> Self {
        Self(value.clamp(0.0, 1.0))
    }

    /// The raw alpha value.
    pub fn value(self) -> f32 {
        self.0
    }

    /// Combine two modifiers multiplicatively.
    ///
    /// E.g., a window at 80% opacity with a surface alpha of 50% produces
    /// an effective opacity of 40%.
    pub fn combine(self, other: Self) -> Self {
        Self(self.0 * other.0)
    }
}

impl Default for AlphaModifier {
    fn default() -> Self {
        Self::OPAQUE
    }
}

// ─── Background blur state ───────────────────────────────────────────────────

/// Desired background blur radius for `ext-background-effect-v1`.
///
/// `None` means no blur is requested (the default for most windows).
/// `Some(radius)` means the client has requested a Gaussian blur of `radius`
/// pixels behind its surface.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BackgroundBlurRequest(Option<f32>);

impl BackgroundBlurRequest {
    /// No blur requested (default).
    pub const NONE: Self = Self(None);

    /// Request a blur with the given radius in pixels.
    pub fn with_radius(radius: f32) -> Self {
        Self(Some(radius.max(0.0)))
    }

    /// Whether blur has been requested.
    pub fn is_active(self) -> bool {
        self.0.is_some()
    }

    /// The requested blur radius, if any.
    pub fn radius(self) -> Option<f32> {
        self.0
    }
}

impl Default for BackgroundBlurRequest {
    fn default() -> Self {
        Self::NONE
    }
}

// ─── EffectRenderer ──────────────────────────────────────────────────────────

/// Applies per-window post-compositing effects.
///
/// Currently a thin wrapper; will be expanded when `ext-background-effect-v1`
/// and GPU alpha modifier support are wired up.
pub struct EffectRenderer {
    blur_request: BackgroundBlurRequest,
    alpha_modifier: AlphaModifier,
}

impl EffectRenderer {
    /// Create a new effect renderer with defaults (no blur, fully opaque).
    pub fn new() -> Self {
        Self {
            blur_request: BackgroundBlurRequest::NONE,
            alpha_modifier: AlphaModifier::OPAQUE,
        }
    }

    /// Update the per-surface alpha modifier (from `alpha-modifier-v1` protocol).
    pub fn set_alpha_modifier(&mut self, modifier: AlphaModifier) {
        self.alpha_modifier = modifier;
    }

    /// Update the background blur request (from `ext-background-effect-v1`).
    pub fn set_blur_request(&mut self, request: BackgroundBlurRequest) {
        self.blur_request = request;
    }

    /// Current effective alpha modifier.
    pub fn alpha_modifier(&self) -> AlphaModifier {
        self.alpha_modifier
    }

    /// Apply effects to the window region.
    ///
    /// Currently a no-op: alpha is already applied in the main render path via
    /// `FrameRef::draw_pixels`, and background blur is not yet implemented.
    ///
    /// When background blur is implemented:
    /// 1. Readback the framebuffer rect beneath the window.
    /// 2. Apply Gaussian blur (GPU, using a compute shader or two-pass filter).
    /// 3. Draw the blurred rect back as a quad before the client surface.
    pub fn apply<F: FrameRef>(
        &self,
        _frame: &mut F,
        _window_rect: Rect,
        _opacity: f32,
    ) -> Result<(), F::Error> {
        // Background blur: TODO — requires GPU framebuffer readback.
        // if self.blur_request.is_active() { ... }
        Ok(())
    }
}

impl Default for EffectRenderer {
    fn default() -> Self {
        Self::new()
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alpha_modifier_opaque() {
        assert_eq!(AlphaModifier::OPAQUE.value(), 1.0);
    }

    #[test]
    fn alpha_modifier_clamped() {
        assert_eq!(AlphaModifier::new(2.0).value(), 1.0);
        assert_eq!(AlphaModifier::new(-0.5).value(), 0.0);
    }

    #[test]
    fn alpha_modifier_combine() {
        let a = AlphaModifier::new(0.8);
        let b = AlphaModifier::new(0.5);
        let combined = a.combine(b);
        assert!((combined.value() - 0.4).abs() < 1e-6);
    }

    #[test]
    fn alpha_modifier_default_is_opaque() {
        assert_eq!(AlphaModifier::default().value(), 1.0);
    }

    #[test]
    fn background_blur_none_by_default() {
        assert!(!BackgroundBlurRequest::NONE.is_active());
        assert!(BackgroundBlurRequest::NONE.radius().is_none());
    }

    #[test]
    fn background_blur_with_radius() {
        let req = BackgroundBlurRequest::with_radius(20.0);
        assert!(req.is_active());
        assert_eq!(req.radius(), Some(20.0));
    }

    #[test]
    fn background_blur_radius_clamped_at_zero() {
        let req = BackgroundBlurRequest::with_radius(-5.0);
        assert_eq!(req.radius(), Some(0.0));
    }

    #[test]
    fn effect_renderer_defaults() {
        let r = EffectRenderer::new();
        assert_eq!(r.alpha_modifier().value(), 1.0);
    }

    #[test]
    fn effect_renderer_set_alpha() {
        let mut r = EffectRenderer::new();
        r.set_alpha_modifier(AlphaModifier::new(0.75));
        assert!((r.alpha_modifier().value() - 0.75).abs() < 1e-6);
    }
}
