//! Configuration types for squircle corner rounding.

/// Configuration for continuous-curvature rounding on a single corner.
///
/// The `smoothing` parameter controls the proportion of Bézier curve vs
/// circular arc in each corner. Apple uses 0.6 as the default for macOS/iOS.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SquircleConfig {
    /// Corner radius in pixels.
    ///
    /// Will be clamped to `min(width, height) / 2` if it exceeds the rect
    /// dimensions.
    pub corner_radius: f32,

    /// Smoothing factor in the range `[0.0, 1.0]`.
    ///
    /// - `0.0` — standard circular arc (equivalent to CSS `border-radius`)
    /// - `0.6` — Apple's default used on macOS/iOS (recommended)
    /// - `1.0` — maximum smoothness, no arc segment, pure Bézier
    ///
    /// Values outside `[0.0, 1.0]` are clamped.
    pub smoothing: f32,
}

impl Default for SquircleConfig {
    fn default() -> Self {
        Self {
            corner_radius: 10.0,
            smoothing: 0.6,
        }
    }
}

impl SquircleConfig {
    /// Create a new config with the given radius and smoothing.
    #[inline]
    pub fn new(corner_radius: f32, smoothing: f32) -> Self {
        Self {
            corner_radius,
            smoothing: smoothing.clamp(0.0, 1.0),
        }
    }

    /// Returns the smoothing clamped to `[0.0, 1.0]`.
    #[inline]
    pub(crate) fn clamped_smoothing(&self) -> f32 {
        self.smoothing.clamp(0.0, 1.0)
    }
}
