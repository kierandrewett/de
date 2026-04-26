//! Per-window chrome rendering pipeline (squircle clip, macOS-style border,
//! shadow, title bar). All CPU-side computation lives here; the GPU
//! integration via Smithay's `GlesFrame` is **not yet wired**.
//!
//! ## Status
//!
//! Today the compositor draws plain wayland surfaces via
//! [`smithay::desktop::space::render_output`] (see `crates/compositor/src/winit.rs`).
//! That gives us functional windows but bypasses everything in this module.
//!
//! The renderers below (Shadow, Border, Clip, Decoration, Effect) are
//! intentional dead code until the FrameRef ↔ GlesFrame bridge is implemented.
//! They produce CPU pixel data + path geometry that the bridge will upload as
//! textures and stitch into the per-window draw call.
//!
//! ## Render order (back to front, per WINDOW_SPEC.md)
//! 1. Shadow (pre-computed blurred squircle behind the window)
//! 2. Outer border stroke (0.5 px along squircle path)
//! 3. Window fill (SSD only — background behind title bar + content)
//! 4. Title bar (SSD only — iced-rendered texture)
//! 5. Client content (clipped to squircle via SDF shader)
//! 6. Inner highlight (1 px inset, vertical alpha gradient)
//!
//! ## Wiring TODO
//! Implement `FrameRef for smithay::backend::renderer::gles::GlesFrame`,
//! then in `winit.rs`/`udev.rs::render_frame` call
//! `WindowRenderer::render_window` for each window after `render_output`.
#![allow(dead_code)]

pub mod border;
pub mod clip;
pub mod decoration;
pub mod effects;
pub mod layer_chrome;
pub mod shadow;
pub mod squircle_clip;
pub mod window_chrome;

pub use border::BorderRenderer;
pub use clip::{ClipRenderer, ClipStrategy};
pub use decoration::DecorationRenderer;
pub use effects::EffectRenderer;
pub use shadow::ShadowRenderer;

use rounding::{SquircleConfig, SquirclePath};
use std::collections::HashMap;
use theme::{WindowBorderStyle, WindowTheme};
use tracing::trace;

// ─── Geometric types ────────────────────────────────────────────────────────

/// Axis-aligned rectangle in logical (pre-scale) pixel coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    /// Left edge.
    pub x: f32,
    /// Top edge.
    pub y: f32,
    /// Width (must be non-negative).
    pub width: f32,
    /// Height (must be non-negative).
    pub height: f32,
}

impl Rect {
    /// Create a new rectangle.
    pub fn new(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self { x, y, width, height }
    }

    /// Centre point.
    pub fn center(&self) -> (f32, f32) {
        (self.x + self.width * 0.5, self.y + self.height * 0.5)
    }

    /// Shrink the rect inward by `amount` on all sides.
    pub fn inset(&self, amount: f32) -> Self {
        Self {
            x: self.x + amount,
            y: self.y + amount,
            width: (self.width - amount * 2.0).max(0.0),
            height: (self.height - amount * 2.0).max(0.0),
        }
    }

    /// Whether a point lies inside the rect.
    pub fn contains(&self, px: f32, py: f32) -> bool {
        px >= self.x && px < self.x + self.width && py >= self.y && py < self.y + self.height
    }
}

// ─── Window state ────────────────────────────────────────────────────────────

/// How window decorations are managed for this window.
#[derive(Debug, Clone)]
pub enum DecorationMode {
    /// The client draws its own title bar and controls (GTK4/libadwaita style).
    ClientSide,
    /// The compositor draws the title bar using iced. The client renders content only.
    ServerSide {
        /// Window title shown in the title bar.
        title: String,
    },
}

/// Per-frame geometry snapshot for one window, derived from animated state.
#[derive(Debug, Clone)]
pub struct WindowGeometry {
    /// Outer window rectangle (border + content).
    pub rect: Rect,
    /// Current animated opacity (0.0 = transparent, 1.0 = opaque).
    pub opacity: f32,
    /// Current animated scale factor (1.0 = normal).
    pub scale: f32,
}

impl WindowGeometry {
    /// Content area rect inside any server-side title bar.
    pub fn content_rect(&self, title_bar_height: f32, decoration_mode: &DecorationMode) -> Rect {
        match decoration_mode {
            DecorationMode::ServerSide { .. } => Rect {
                x: self.rect.x,
                y: self.rect.y + title_bar_height,
                width: self.rect.width,
                height: (self.rect.height - title_bar_height).max(0.0),
            },
            DecorationMode::ClientSide => self.rect,
        }
    }
}

/// All state the render pipeline needs to paint one window for one frame.
#[derive(Debug, Clone)]
pub struct WindowRenderState {
    /// Animated geometry.
    pub geometry: WindowGeometry,
    /// Whether this window currently has keyboard focus.
    pub is_focused: bool,
    /// Whether this window is in fullscreen mode.
    pub is_fullscreen: bool,
    /// Whether the window is minimized and the close animation has finished.
    ///
    /// When true, the window is completely invisible and should be skipped.
    pub is_hidden: bool,
    /// Decoration mode negotiated via `xdg-decoration-unstable-v1`.
    pub decoration_mode: DecorationMode,
}

// ─── SquirclePathCache ───────────────────────────────────────────────────────

/// Cache key for squircle paths — float bits used for Hash/Eq without NaN risk.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct PathCacheKey {
    x_bits: u32,
    y_bits: u32,
    width_bits: u32,
    height_bits: u32,
    radius_bits: u32,
    smoothing_bits: u32,
}

impl PathCacheKey {
    fn new(x: f32, y: f32, width: f32, height: f32, config: SquircleConfig) -> Self {
        Self {
            x_bits: x.to_bits(),
            y_bits: y.to_bits(),
            width_bits: width.to_bits(),
            height_bits: height.to_bits(),
            radius_bits: config.corner_radius.to_bits(),
            smoothing_bits: config.smoothing.to_bits(),
        }
    }
}

/// Cache of [`SquirclePath`] instances keyed by geometry + config.
///
/// Paths are recomputed only on resize or theme change — not every frame.
/// Clear on theme change with [`SquirclePathCache::clear`].
#[derive(Default)]
pub struct SquirclePathCache {
    entries: HashMap<PathCacheKey, SquirclePath>,
}

impl SquirclePathCache {
    /// Create an empty cache.
    pub fn new() -> Self {
        Self::default()
    }

    /// Get or compute the path for the given geometry and config.
    pub fn get_or_insert(
        &mut self,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        config: SquircleConfig,
    ) -> &SquirclePath {
        let key = PathCacheKey::new(x, y, width, height, config);
        self.entries
            .entry(key)
            .or_insert_with(|| SquirclePath::new(x, y, width, height, config))
    }

    /// Remove the entry for the given geometry (e.g., after window resize).
    pub fn invalidate(&mut self, x: f32, y: f32, width: f32, height: f32, config: SquircleConfig) {
        self.entries.remove(&PathCacheKey::new(x, y, width, height, config));
    }

    /// Discard all cached paths (e.g., on theme change).
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// Number of cached paths.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the cache is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

// ─── FrameRef — GPU rendering abstraction ───────────────────────────────────

/// Abstraction over the GPU rendering context used by the render pipeline.
///
/// Implemented by the Smithay `GlesFrame` integration in the backend wiring.
/// The implementation for `GlesFrame` lives in the compositor's winit/udev
/// backend setup (subagent 07).
///
/// Methods accept logical pixel coordinates; scaling to physical pixels is the
/// implementor's responsibility.
pub trait FrameRef {
    /// Error type returned by render operations.
    type Error: std::error::Error + Send + Sync + 'static;

    /// Fill a rectangle with a solid colour (RGBA, linear).
    fn draw_colored_quad(&mut self, rect: Rect, color: [f32; 4]) -> Result<(), Self::Error>;

    /// Draw raw RGBA pixel data scaled to fill `dst`.
    ///
    /// `pixels` must be `pixel_width * pixel_height * 4` bytes (R, G, B, A, 8-bpc,
    /// straight alpha). `alpha` multiplies all pixel alphas uniformly.
    fn draw_pixels(
        &mut self,
        pixels: &[u8],
        pixel_width: u32,
        pixel_height: u32,
        dst: Rect,
        alpha: f32,
    ) -> Result<(), Self::Error>;

    /// Draw a horizontal gradient quad.
    ///
    /// `color_top` is the colour at the top edge; `color_bottom` at the bottom.
    /// Useful for the inner highlight gradient (brighter at top, transparent at bottom).
    fn draw_gradient_quad(
        &mut self,
        rect: Rect,
        color_top: [f32; 4],
        color_bottom: [f32; 4],
    ) -> Result<(), Self::Error>;
}

// ─── WindowRenderer ──────────────────────────────────────────────────────────

/// Per-compositor window renderer. Holds all caches and sub-renderers.
///
/// Create one instance per compositor and reuse across frames. Caches are
/// invalidated on theme change via [`WindowRenderer::on_theme_changed`].
pub struct WindowRenderer {
    shadow: ShadowRenderer,
    border: BorderRenderer,
    clip: ClipRenderer,
    decoration: DecorationRenderer,
    effects: EffectRenderer,
    path_cache: SquirclePathCache,
    theme: WindowTheme,
    is_dark_mode: bool,
}

impl WindowRenderer {
    /// Create a new renderer for the given theme and colour scheme.
    pub fn new(theme: WindowTheme, is_dark_mode: bool) -> Self {
        Self {
            shadow: ShadowRenderer::new(),
            border: BorderRenderer::new(),
            clip: ClipRenderer::new(ClipStrategy::Sdf),
            decoration: DecorationRenderer::new(),
            effects: EffectRenderer::new(),
            path_cache: SquirclePathCache::new(),
            theme,
            is_dark_mode,
        }
    }

    /// Notify the renderer that the theme has changed.
    ///
    /// Invalidates all caches so the next frame reflects the new theme.
    pub fn on_theme_changed(&mut self, new_theme: WindowTheme, is_dark_mode: bool) {
        self.theme = new_theme;
        self.is_dark_mode = is_dark_mode;
        self.shadow.cache_mut().clear();
        self.path_cache.clear();
    }

    /// Select the border style for the current window state.
    pub fn border_style(&self, is_focused: bool) -> &WindowBorderStyle {
        let b = &self.theme.border;
        match (self.is_dark_mode, is_focused) {
            (false, true) => &b.light_active,
            (false, false) => &b.light_inactive,
            (true, true) => &b.dark_active,
            (true, false) => &b.dark_inactive,
        }
    }

    /// Build the squircle config from the current theme.
    pub fn squircle_config(&self) -> SquircleConfig {
        SquircleConfig::new(self.theme.corner_radius, self.theme.corner_smoothing)
    }

    /// Render one window to the given frame.
    ///
    /// Call this back-to-front for all visible windows (furthest from viewer first).
    /// The client surface texture is provided as raw RGBA pixels via `client_pixels`.
    ///
    /// Returns `Ok(())` if the window is hidden (minimized + animation done).
    pub fn render_window<F: FrameRef>(
        &mut self,
        frame: &mut F,
        state: &WindowRenderState,
        client_pixels: Option<(&[u8], u32, u32)>,
    ) -> Result<(), F::Error> {
        if state.is_hidden {
            return Ok(());
        }

        let geo = &state.geometry;
        let rect = geo.rect;
        let opacity = geo.opacity;
        let config = self.squircle_config();
        let border_style = self.border_style(state.is_focused).clone();
        let title_bar_height = self.theme.title_bar_height;

        if state.is_fullscreen {
            // Fullscreen: draw client texture raw, no decorations.
            if let Some((pixels, pw, ph)) = client_pixels {
                frame.draw_pixels(pixels, pw, ph, rect, opacity)?;
            }
            return Ok(());
        }

        trace!(
            x = rect.x,
            y = rect.y,
            w = rect.width,
            h = rect.height,
            focused = state.is_focused,
            "render_window"
        );

        // 1. Shadow (behind and below the window).
        self.shadow.render(
            frame,
            rect,
            &border_style.shadow_layers,
            config,
            state.is_focused,
        )?;

        // 2. Outer border stroke.
        self.border.render_outer_stroke(frame, rect, config, &border_style)?;

        // 3. Window fill + client content.
        match &state.decoration_mode.clone() {
            DecorationMode::ServerSide { title } => {
                // SSD: render fill, then title bar texture, then content.
                let title_rect = Rect::new(rect.x, rect.y, rect.width, title_bar_height);
                let content_rect = geo.content_rect(title_bar_height, &state.decoration_mode);

                self.decoration.render(frame, title_rect, title, state.is_focused, self.is_dark_mode)?;

                if let Some((pixels, pw, ph)) = client_pixels {
                    self.clip.render_clipped_texture(
                        frame, pixels, pw, ph, content_rect, config, opacity,
                    )?;
                }
            }
            DecorationMode::ClientSide => {
                // CSD: clip the full client surface to the squircle.
                if let Some((pixels, pw, ph)) = client_pixels {
                    self.clip.render_clipped_texture(
                        frame, pixels, pw, ph, rect, config, opacity,
                    )?;
                }
            }
        }

        // 4. Inner highlight (topmost layer — sits over everything).
        self.border.render_inner_highlight(frame, rect, &border_style)?;

        // 5. Apply any per-surface effects (alpha modifier, background blur).
        self.effects.apply(frame, rect, opacity)?;

        Ok(())
    }

    /// Immutable access to the theme.
    pub fn theme(&self) -> &WindowTheme {
        &self.theme
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rect_center() {
        let r = Rect::new(10.0, 20.0, 100.0, 50.0);
        assert_eq!(r.center(), (60.0, 45.0));
    }

    #[test]
    fn rect_inset() {
        let r = Rect::new(0.0, 0.0, 100.0, 60.0).inset(5.0);
        assert_eq!(r.x, 5.0);
        assert_eq!(r.y, 5.0);
        assert_eq!(r.width, 90.0);
        assert_eq!(r.height, 50.0);
    }

    #[test]
    fn rect_inset_clamped() {
        // Inset larger than half-size must not produce negative dimensions.
        let r = Rect::new(0.0, 0.0, 10.0, 10.0).inset(20.0);
        assert_eq!(r.width, 0.0);
        assert_eq!(r.height, 0.0);
    }

    #[test]
    fn rect_contains() {
        let r = Rect::new(10.0, 20.0, 80.0, 40.0);
        assert!(r.contains(10.0, 20.0)); // top-left corner (inclusive)
        assert!(r.contains(50.0, 40.0)); // centre
        assert!(!r.contains(90.0, 60.0)); // right/bottom (exclusive)
        assert!(!r.contains(9.9, 25.0)); // left of rect
    }

    #[test]
    fn path_cache_insert_and_reuse() {
        let mut cache = SquirclePathCache::new();
        let config = SquircleConfig::new(14.0, 0.6);
        let _ = cache.get_or_insert(0.0, 0.0, 400.0, 300.0, config);
        assert_eq!(cache.len(), 1);
        // Same key → no new entry.
        let _ = cache.get_or_insert(0.0, 0.0, 400.0, 300.0, config);
        assert_eq!(cache.len(), 1);
        // Different size → new entry.
        let _ = cache.get_or_insert(0.0, 0.0, 500.0, 300.0, config);
        assert_eq!(cache.len(), 2);
    }

    #[test]
    fn path_cache_clear() {
        let mut cache = SquirclePathCache::new();
        let config = SquircleConfig::new(14.0, 0.6);
        let _ = cache.get_or_insert(0.0, 0.0, 400.0, 300.0, config);
        cache.clear();
        assert!(cache.is_empty());
    }

    #[test]
    fn border_style_selection() {
        let renderer = WindowRenderer::new(WindowTheme::default(), false);
        let light_active = renderer.border_style(true);
        // Light active outer stroke: rgba(0,0,0,0.22)
        assert!((light_active.outer_stroke[3] - 0.22).abs() < 1e-6);

        let renderer_dark = WindowRenderer::new(WindowTheme::default(), true);
        let dark_active = renderer_dark.border_style(true);
        // Dark active outer stroke: rgba(0,0,0,0.72)
        assert!((dark_active.outer_stroke[3] - 0.72).abs() < 1e-6);
    }

    #[test]
    fn squircle_config_matches_theme() {
        let renderer = WindowRenderer::new(WindowTheme::default(), false);
        let config = renderer.squircle_config();
        assert_eq!(config.corner_radius, 14.0);
        assert_eq!(config.smoothing, 0.6);
    }

    #[test]
    fn hidden_window_skips_render() {
        // Minimal FrameRef stub to verify hidden window returns Ok immediately.
        struct NullFrame;
        #[derive(Debug)]
        struct Never;
        impl std::fmt::Display for Never {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "never")
            }
        }
        impl std::error::Error for Never {}
        impl FrameRef for NullFrame {
            type Error = Never;
            fn draw_colored_quad(&mut self, _: Rect, _: [f32; 4]) -> Result<(), Never> { Ok(()) }
            fn draw_pixels(&mut self, _: &[u8], _: u32, _: u32, _: Rect, _: f32) -> Result<(), Never> { Ok(()) }
            fn draw_gradient_quad(&mut self, _: Rect, _: [f32; 4], _: [f32; 4]) -> Result<(), Never> { Ok(()) }
        }

        let mut renderer = WindowRenderer::new(WindowTheme::default(), false);
        let state = WindowRenderState {
            geometry: WindowGeometry {
                rect: Rect::new(0.0, 0.0, 400.0, 300.0),
                opacity: 1.0,
                scale: 1.0,
            },
            is_focused: true,
            is_fullscreen: false,
            is_hidden: true,
            decoration_mode: DecorationMode::ClientSide,
        };
        let mut frame = NullFrame;
        renderer.render_window(&mut frame, &state, None).unwrap();
    }
}
