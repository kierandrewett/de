//! CPU-rasterised window-chrome textures: shadow + decoration overlay.
//!
//! Produced once per `(size, focus, dark, scale)` tuple and cached. Each
//! window contributes two `MemoryRenderBuffer`s every frame:
//!
//! 1. **Shadow** — multi-layer Gaussian-blurred squircle, positioned with a
//!    padding margin around the window so it extends beyond the bounds. Goes
//!    *behind* the window (and behind any title bar) in z-order.
//! 2. **Decoration overlay** — exactly the window rect; transparent inside
//!    the squircle except for the 0.5 px outer stroke and the 1 px inner
//!    highlight gradient, opaque outside the squircle so the rectangular
//!    client corners are visually clipped to the squircle shape.
//!
//! The corner-cutout pixels are filled with the compositor's clear colour
//! so they read as "background" once composited — this is a v1 limitation
//! that produces correct output against an empty desktop but does not
//! reveal windows behind. A future iteration can replace it with a custom
//! `PixelShaderElement` that alpha-clips the client surface itself.
//!
//! All textures are emitted in `Fourcc::Argb8888` (little-endian BGRA,
//! pre-multiplied alpha) — the same format the title-bar chrome uses.
#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::Arc;

use rounding::{SquircleConfig, SquirclePath};
use tiny_skia::FillRule;
use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::memory::MemoryRenderBuffer;
use smithay::utils::Transform;
use theme::{ShadowLayer, WindowBorderStyle, WindowTheme};
use tiny_skia::{
    GradientStop, LinearGradient, Paint, Pixmap, SpreadMode, Stroke, Transform as SkTransform,
};

use super::shadow::compute_shadow_pixels;

/// Cut out the squircle interior from a pre-computed shadow texture.
///
/// macOS-style window shadows behave like CSS `box-shadow`: the halo
/// extends *outside* the squircle, but the interior of the window is left
/// untouched. Without this step the blurred shadow would tint the window
/// itself dark grey, breaking the layered look.
fn cut_out_shadow_interior(
    rgba: &mut [u8],
    width: u32,
    height: u32,
    padding: u32,
    window_w: u32,
    window_h: u32,
    config: SquircleConfig,
) {
    if window_w == 0 || window_h == 0 {
        return;
    }
    let path = SquirclePath::new(
        padding as f32,
        padding as f32,
        window_w as f32,
        window_h as f32,
        config,
    );
    let Some(sk_path) = path.to_tiny_skia_path() else { return };
    let Some(mut mask) = Pixmap::new(width, height) else { return };
    let mut paint = Paint::default();
    paint.set_color_rgba8(255, 255, 255, 255);
    paint.anti_alias = true;
    mask.fill_path(
        &sk_path,
        &paint,
        FillRule::Winding,
        SkTransform::identity(),
        None,
    );
    // Multiply the shadow alpha by (1 - mask_alpha). tiny_skia stores
    // pre-multiplied RGBA — both channels and alpha must scale.
    for (shadow_px, mask_px) in rgba.chunks_exact_mut(4).zip(mask.data().chunks_exact(4)) {
        let inside = mask_px[3] as u32; // 0..=255
        let factor = 255 - inside;
        shadow_px[0] = ((shadow_px[0] as u32 * factor + 127) / 255) as u8;
        shadow_px[1] = ((shadow_px[1] as u32 * factor + 127) / 255) as u8;
        shadow_px[2] = ((shadow_px[2] as u32 * factor + 127) / 255) as u8;
        shadow_px[3] = ((shadow_px[3] as u32 * factor + 127) / 255) as u8;
    }
}

// ─── Cache key ───────────────────────────────────────────────────────────────

/// Identifies one cached chrome variant.
///
/// The size is in *physical* pixels so HiDPI displays don't have to rebuild
/// every frame — the cache stays warm as long as the window keeps the same
/// physical-pixel size.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ChromeKey {
    /// Window width in physical pixels.
    pub width_px: u32,
    /// Window height in physical pixels.
    pub height_px: u32,
    /// Output scale × 256, fixed-point so we can hash it.
    pub scale_q8: u32,
    /// Whether the window currently has keyboard focus.
    pub focused: bool,
    /// Whether the compositor is in dark mode.
    pub dark: bool,
}

impl ChromeKey {
    /// Build a key from logical pixels + a render scale.
    pub fn new(width_logical: f64, height_logical: f64, scale: f64, focused: bool, dark: bool) -> Self {
        Self {
            width_px: (width_logical * scale).round().max(1.0) as u32,
            height_px: (height_logical * scale).round().max(1.0) as u32,
            scale_q8: (scale * 256.0).round().clamp(1.0, u32::MAX as f64) as u32,
            focused,
            dark,
        }
    }

    /// Render scale recovered from the fixed-point field.
    pub fn scale(&self) -> f64 {
        self.scale_q8 as f64 / 256.0
    }
}

// ─── Chrome bundle ───────────────────────────────────────────────────────────

/// One window's pre-rendered chrome textures + their padding offsets.
#[derive(Clone)]
pub struct ChromeTextures {
    /// Multi-layer shadow texture. Position at
    /// `(window_logical_x - shadow_padding_logical, window_logical_y - shadow_padding_logical)`.
    pub shadow: MemoryRenderBuffer,
    /// Padding (logical pixels) added around the window for the shadow texture.
    pub shadow_padding_logical: f64,
    /// Decoration overlay (border + corner cutouts + inner highlight). Same
    /// size as the window rect; place at the window's top-left.
    pub decoration: MemoryRenderBuffer,
}

// ─── Cache ───────────────────────────────────────────────────────────────────

/// Process-wide cache of pre-rasterised chrome textures.
#[derive(Default)]
pub struct WindowChromeCache {
    entries: HashMap<ChromeKey, Arc<ChromeTextures>>,
    /// The clear colour used for corner cutouts. Updated in lockstep with
    /// the compositor's clear colour so cutouts blend with the background.
    cutout_color: [u8; 4],
}

impl WindowChromeCache {
    /// Create an empty cache with the given desktop background colour.
    pub fn new(cutout_color: [u8; 4]) -> Self {
        Self { entries: HashMap::new(), cutout_color }
    }

    /// Update the cutout colour. Clears the cache because all decoration
    /// textures are baked against the previous colour.
    pub fn set_cutout_color(&mut self, color: [u8; 4]) {
        if color != self.cutout_color {
            self.cutout_color = color;
            self.entries.clear();
        }
    }

    /// Drop all cached textures (e.g. on theme change).
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// Return `true` if the cache has no entries.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Number of cached chrome bundles.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Retrieve the chrome bundle for the given key, building it on cache miss.
    ///
    /// `theme` and `border` come from the live window theme — the cache key
    /// already encodes mode + focus, so the (theme, border) pair is consistent
    /// for any given key.
    pub fn get_or_build(
        &mut self,
        key: ChromeKey,
        theme: &WindowTheme,
        border: &WindowBorderStyle,
    ) -> Arc<ChromeTextures> {
        if let Some(existing) = self.entries.get(&key) {
            return existing.clone();
        }
        let built = Arc::new(build_chrome(&key, theme, border, self.cutout_color));
        self.entries.insert(key, built.clone());
        built
    }
}

// ─── Chrome builder ──────────────────────────────────────────────────────────

/// Build both chrome textures for one window state.
fn build_chrome(
    key: &ChromeKey,
    theme: &WindowTheme,
    border: &WindowBorderStyle,
    cutout_color: [u8; 4],
) -> ChromeTextures {
    let scale = key.scale();
    let radius_px = (theme.corner_radius * scale as f32).max(0.0);
    let smoothing = theme.corner_smoothing.clamp(0.0, 1.0);
    let config = SquircleConfig::new(radius_px, smoothing);

    let shadow = build_shadow_buffer(key, &border.shadow_layers, config);
    let shadow_padding_logical = shadow_padding_logical(&border.shadow_layers, scale);
    let decoration = build_decoration_buffer(key, theme, border, config, cutout_color);

    ChromeTextures { shadow, shadow_padding_logical, decoration }
}

/// Logical-pixel padding around a window for its shadow texture.
fn shadow_padding_logical(layers: &[ShadowLayer], scale: f64) -> f64 {
    let max_blur = layers.iter().map(|l| l.blur_radius).fold(0.0f32, f32::max);
    let max_oy = layers.iter().map(|l| l.offset_y.abs()).fold(0.0f32, f32::max);
    let max_ox = layers.iter().map(|l| l.offset_x.abs()).fold(0.0f32, f32::max);
    let max_spread = layers.iter().map(|l| l.spread.abs()).fold(0.0f32, f32::max);
    // Match the formula in shadow.rs::shadow_padding (3σ + offset + spread + 2 px margin),
    // computed in logical pixels — physical pixels follow via `* scale` in the rasteriser.
    let logical = max_blur + max_oy + max_ox + max_spread + 2.0;
    let _ = scale; // reserved for future per-scale padding adjustments
    (logical as f64).max(0.0)
}

// ─── Shadow buffer ───────────────────────────────────────────────────────────

fn build_shadow_buffer(
    key: &ChromeKey,
    layers: &[ShadowLayer],
    squircle_config: SquircleConfig,
) -> MemoryRenderBuffer {
    // The macOS spec alphas (0.08–0.12) are tuned for a light-grey
    // wallpaper. On our placeholder near-black desktop those fall under
    // the JND of monitor banding (~1 unit at this brightness), so the
    // shadow effectively disappears. Until we have a real wallpaper /
    // user-chosen desktop colour to weigh contrast against, multiply the
    // alpha by a constant so the halo reads. Capped at 1.0.
    //
    // 3× chosen empirically: keeps the falloff smooth (no banding) while
    // putting ~6 units of darkening at the window edge — enough to
    // perceive against the dark bg without producing the heavy "ring"
    // a higher boost gave us earlier.
    const SHADOW_BG_CONTRAST_BOOST: f32 = 3.0;
    let scaled_layers: Vec<ShadowLayer> = layers
        .iter()
        .map(|l| ShadowLayer {
            offset_x: l.offset_x * key.scale() as f32,
            offset_y: l.offset_y * key.scale() as f32,
            blur_radius: l.blur_radius * key.scale() as f32,
            spread: l.spread * key.scale() as f32,
            color: [
                l.color[0],
                l.color[1],
                l.color[2],
                (l.color[3] * SHADOW_BG_CONTRAST_BOOST).min(1.0),
            ],
        })
        .collect();

    let pixels = compute_shadow_pixels(key.width_px, key.height_px, &scaled_layers, squircle_config);

    // shadow.rs returns straight-alpha RGBA. Smithay's Argb8888 is BGRA pre-mul
    // little-endian — premultiply + channel swap.
    let mut bgra = vec![0u8; pixels.data.len()];
    for (src, dst) in pixels.data.chunks_exact(4).zip(bgra.chunks_exact_mut(4)) {
        let r = src[0] as u32;
        let g = src[1] as u32;
        let b = src[2] as u32;
        let a = src[3] as u32;
        dst[0] = ((b * a + 127) / 255) as u8;
        dst[1] = ((g * a + 127) / 255) as u8;
        dst[2] = ((r * a + 127) / 255) as u8;
        dst[3] = a as u8;
    }

    // Cut out the window's interior — the shadow is an outside-only halo so
    // it can be composited *above* the client surface without darkening it.
    cut_out_shadow_interior(
        &mut bgra,
        pixels.width,
        pixels.height,
        pixels.padding,
        key.width_px,
        key.height_px,
        squircle_config,
    );

    MemoryRenderBuffer::from_slice(
        &bgra,
        Fourcc::Argb8888,
        (pixels.width as i32, pixels.height as i32),
        1,
        Transform::Normal,
        None,
    )
}

// ─── Decoration buffer ──────────────────────────────────────────────────────

fn build_decoration_buffer(
    key: &ChromeKey,
    theme: &WindowTheme,
    border: &WindowBorderStyle,
    config: SquircleConfig,
    cutout_color: [u8; 4],
) -> MemoryRenderBuffer {
    let w = key.width_px.max(1);
    let h = key.height_px.max(1);
    let scale = key.scale() as f32;

    let mut pixmap = Pixmap::new(w, h).unwrap_or_else(|| {
        // Fall back to 1×1 rather than panic; tiny_skia rejects huge sizes.
        Pixmap::new(1, 1).expect("1x1 pixmap")
    });

    let _ = cutout_color; // kept in API for back-compat / cache invalidation

    // ── 2. Outer 0.5 px squircle stroke ────────────────────────────────
    // Per WINDOW_SPEC.md: 0.5 px solid rgba(0, 0, 0, α) where α is
    //   light-active 0.22 / light-inactive 0.15 / dark-active 0.72 /
    //   dark-inactive 0.55.
    // We INSET the path by 0.25 px so the entire stroke band sits *inside*
    // the squircle's edge — without this, half the stroke would land on
    // pixels that the SDF clip already painted with the surface's
    // anti-aliased edge alpha, and the result reads as the window being
    // clipped a pixel short.
    let stroke_w_logical = 0.5_f32;
    let stroke_w_phys = stroke_w_logical * scale;
    let stroke_inset = stroke_w_phys * 0.5;
    let outer_radius = (config.corner_radius - stroke_inset).max(0.0);
    let outer_w = (w as f32 - 2.0 * stroke_inset).max(0.0);
    let outer_h = (h as f32 - 2.0 * stroke_inset).max(0.0);
    if outer_w > 0.0 && outer_h > 0.0 {
        if let Some(outer_path) =
            rounded_rect_path(stroke_inset, stroke_inset, outer_w, outer_h, outer_radius)
        {
            let mut stroke_paint = Paint { anti_alias: true, ..Paint::default() };
            stroke_paint.set_color_rgba8(
                (border.outer_stroke[0] * 255.0).round().clamp(0.0, 255.0) as u8,
                (border.outer_stroke[1] * 255.0).round().clamp(0.0, 255.0) as u8,
                (border.outer_stroke[2] * 255.0).round().clamp(0.0, 255.0) as u8,
                (border.outer_stroke[3] * 255.0).round().clamp(0.0, 255.0) as u8,
            );
            pixmap.stroke_path(
                &outer_path,
                &stroke_paint,
                &Stroke { width: stroke_w_phys, ..Stroke::default() },
                SkTransform::identity(),
                None,
            );
        }
    }

    // ── 3. Inner 1 px highlight stroke with vertical alpha gradient ────
    // Inset by 1 logical px and stroke. NOTE: we build the path manually
    // with `tiny_skia::PathBuilder` instead of going through
    // `rounding::SquirclePath::to_tiny_skia_path()` because the latter
    // emits a self-intersecting clockwise traversal at the TL corner
    // (TL exit lands on the LEFT edge, then a diagonal `LineTo` cuts
    // straight across to TR's TOP entry). Stroking that path produces
    // a "double" highlight band — once on the squircle outline, once
    // along the diagonal cross-cut. See task #10.
    let inset_logical = 1.0_f32;
    let inset_phys = inset_logical * scale;
    // The path is inset 1 px from the squircle, but the *radius* is
    // shrunk by 2 px so the curve tightens visually rather than tracking
    // the outline 1-for-1 — matches the macOS look where the inner
    // highlight reads as a slightly tighter arc inside the border.
    let inner_radius = (config.corner_radius - 2.0 * inset_phys).max(0.0);
    let _ = SquircleConfig::new(inner_radius, config.smoothing);
    let inner_w = (w as f32 - 2.0 * inset_phys).max(0.0);
    let inner_h = (h as f32 - 2.0 * inset_phys).max(0.0);
    if inner_w > 0.0 && inner_h > 0.0 {
        if let Some(inner_path) = rounded_rect_path(
            inset_phys, inset_phys, inner_w, inner_h, inner_radius,
        ) {
            // Build a vertical linear gradient from top-color (full) to
            // top-color (alpha=0) at y_norm=2/3, then transparent through
            // bottom — matches the WINDOW_SPEC.md formula
            // alpha = top_alpha * clamp(1 - y_norm * 1.5, 0, 1).
            let base = border.inner_highlight_top;
            let to_color = |a: f32| {
                let alpha = (a.clamp(0.0, 1.0) * 255.0).round() as u8;
                tiny_skia::Color::from_rgba8(
                    (base[0] * 255.0) as u8,
                    (base[1] * 255.0) as u8,
                    (base[2] * 255.0) as u8,
                    alpha,
                )
            };
            let stops = vec![
                GradientStop::new(0.0, to_color(base[3])),
                GradientStop::new(2.0 / 3.0, to_color(0.0)),
                GradientStop::new(1.0, to_color(0.0)),
            ];
            if let Some(shader) = LinearGradient::new(
                tiny_skia::Point::from_xy(0.0, 0.0),
                tiny_skia::Point::from_xy(0.0, h as f32),
                stops,
                SpreadMode::Pad,
                SkTransform::identity(),
            ) {
                let highlight_paint = Paint {
                    shader,
                    anti_alias: true,
                    ..Paint::default()
                };
                let highlight_stroke = Stroke {
                    width: (inset_phys).max(1.0),
                    ..Stroke::default()
                };
                pixmap.stroke_path(
                    &inner_path,
                    &highlight_paint,
                    &highlight_stroke,
                    SkTransform::identity(),
                    None,
                );
            }
        }
    }

    // tiny_skia produces premultiplied RGBA; smithay reads Argb8888 = BGRA-on-LE.
    let mut bytes = pixmap.take();
    for px in bytes.chunks_exact_mut(4) {
        px.swap(0, 2);
    }

    MemoryRenderBuffer::from_slice(
        &bytes,
        Fourcc::Argb8888,
        (w as i32, h as i32),
        1,
        Transform::Normal,
        None,
    )
}

/// Build a clockwise rounded-rect `tiny_skia::Path` directly via
/// `PathBuilder` — without round-tripping through `rounding::SquirclePath`,
/// which currently emits a malformed traversal (see task #10).
///
/// The path is a standard rounded rectangle with cubic-Bézier quarter-arc
/// corners (control coefficient `K = 4·(√2−1)/3 ≈ 0.5523` — gives a circle
/// approximation accurate to ~0.02 %). Returns `None` if the rectangle is
/// degenerate.
fn rounded_rect_path(x: f32, y: f32, w: f32, h: f32, radius: f32) -> Option<tiny_skia::Path> {
    if w <= 0.0 || h <= 0.0 {
        return None;
    }
    let r = radius.min(w / 2.0).min(h / 2.0).max(0.0);
    const K: f32 = 0.5522847;
    let mut pb = tiny_skia::PathBuilder::new();
    pb.move_to(x + r, y);
    pb.line_to(x + w - r, y);
    pb.cubic_to(x + w - r + r * K, y, x + w, y + r - r * K, x + w, y + r);
    pb.line_to(x + w, y + h - r);
    pb.cubic_to(x + w, y + h - r + r * K, x + w - r + r * K, y + h, x + w - r, y + h);
    pb.line_to(x + r, y + h);
    pb.cubic_to(x + r - r * K, y + h, x, y + h - r + r * K, x, y + h - r);
    pb.line_to(x, y + r);
    pb.cubic_to(x, y + r - r * K, x + r - r * K, y, x + r, y);
    pb.close();
    pb.finish()
}

/// Fully-transparent fallback buffer used if pixmap allocation fails.
fn empty_buffer(w: u32, h: u32) -> MemoryRenderBuffer {
    let bytes = vec![0u8; (w * h * 4) as usize];
    MemoryRenderBuffer::from_slice(
        &bytes,
        Fourcc::Argb8888,
        (w as i32, h as i32),
        1,
        Transform::Normal,
        None,
    )
}

/// Build a translucent squircle "ghost" used as the snap-zone preview
/// while a window is being dragged near a snap target. Fills with a
/// dim translucent rect (white in light mode, slightly brighter in
/// dark) with a 1 px outline so the target is visible against any
/// desktop background.
///
/// Inputs are physical pixels; the caller is responsible for scaling
/// the snap rect to the output's `current_scale`.
pub fn build_snap_preview_buffer(
    width_px: u32,
    height_px: u32,
    scale: f64,
    radius_logical: f32,
    dark: bool,
) -> MemoryRenderBuffer {
    let w = width_px.max(1);
    let h = height_px.max(1);
    let mut pixmap = match Pixmap::new(w, h) {
        Some(p) => p,
        None => return empty_buffer(w, h),
    };
    let radius_px = (radius_logical * scale as f32).max(0.0);
    let path = match rounded_rect_path(0.0, 0.0, w as f32, h as f32, radius_px) {
        Some(p) => p,
        None => return empty_buffer(w, h),
    };

    // Fill: translucent white (premultiplied) — same hue in light/dark
    // so the ghost contrasts against any background; dark mode just
    // gets a touch more alpha to remain visible against dark wallpapers.
    let fill_alpha: f32 = if dark { 0.18 } else { 0.14 };
    let mut fill_paint = Paint::default();
    fill_paint.set_color_rgba8(
        (255.0 * fill_alpha).round() as u8,
        (255.0 * fill_alpha).round() as u8,
        (255.0 * fill_alpha).round() as u8,
        (255.0 * fill_alpha).round() as u8,
    );
    fill_paint.anti_alias = true;
    pixmap.fill_path(
        &path,
        &fill_paint,
        FillRule::Winding,
        SkTransform::identity(),
        None,
    );

    // 1 px outline (in logical px) to give the ghost a clear edge.
    let stroke_alpha: f32 = if dark { 0.45 } else { 0.40 };
    let mut stroke_paint = Paint::default();
    stroke_paint.set_color_rgba8(
        (255.0 * stroke_alpha).round() as u8,
        (255.0 * stroke_alpha).round() as u8,
        (255.0 * stroke_alpha).round() as u8,
        (255.0 * stroke_alpha).round() as u8,
    );
    stroke_paint.anti_alias = true;
    let mut stroke = Stroke::default();
    stroke.width = scale as f32;
    pixmap.stroke_path(&path, &stroke_paint, &stroke, SkTransform::identity(), None);

    // Pre-multiplied BGRA in memory = Argb8888 little-endian. tiny_skia
    // emits RGBA premultiplied, so swap R↔B once.
    let mut bytes = pixmap.take();
    for px in bytes.chunks_exact_mut(4) {
        px.swap(0, 2);
    }
    MemoryRenderBuffer::from_slice(
        &bytes,
        Fourcc::Argb8888,
        (w as i32, h as i32),
        1,
        Transform::Normal,
        None,
    )
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use theme::WindowBorderTheme;

    #[test]
    fn key_round_trip_scale() {
        let k = ChromeKey::new(400.0, 300.0, 1.5, true, false);
        assert!((k.scale() - 1.5).abs() < 1e-3);
        assert_eq!(k.width_px, 600);
        assert_eq!(k.height_px, 450);
    }

    #[test]
    fn key_min_size_one() {
        let k = ChromeKey::new(0.0, 0.0, 1.0, true, false);
        assert_eq!(k.width_px, 1);
        assert_eq!(k.height_px, 1);
    }

    #[test]
    fn cache_hit_then_clear() {
        let mut cache = WindowChromeCache::new([16, 16, 18, 255]);
        let theme = WindowTheme::default();
        let border = WindowBorderTheme::default().light_active;
        let key = ChromeKey::new(400.0, 300.0, 1.0, true, false);
        let _ = cache.get_or_build(key.clone(), &theme, &border);
        assert_eq!(cache.len(), 1);
        let _ = cache.get_or_build(key, &theme, &border);
        assert_eq!(cache.len(), 1, "second lookup must not allocate a new entry");
        cache.clear();
        assert!(cache.is_empty());
    }

    #[test]
    fn cutout_color_change_invalidates() {
        let mut cache = WindowChromeCache::new([16, 16, 18, 255]);
        let theme = WindowTheme::default();
        let border = WindowBorderTheme::default().light_active;
        let key = ChromeKey::new(400.0, 300.0, 1.0, true, false);
        let _ = cache.get_or_build(key, &theme, &border);
        assert_eq!(cache.len(), 1);
        cache.set_cutout_color([255, 255, 255, 255]);
        assert!(cache.is_empty(), "changing cutout colour clears decoration cache");
    }

    #[test]
    fn build_decoration_does_not_panic_for_small_rect() {
        let theme = WindowTheme::default();
        let border = WindowBorderTheme::default().light_active;
        let key = ChromeKey::new(2.0, 2.0, 1.0, true, false);
        let _ = build_chrome(&key, &theme, &border, [16, 16, 18, 255]);
    }

    #[test]
    fn build_decoration_at_hidpi() {
        let theme = WindowTheme::default();
        let border = WindowBorderTheme::default().light_active;
        let key = ChromeKey::new(400.0, 300.0, 2.0, false, true);
        let _ = build_chrome(&key, &theme, &border, [0, 0, 0, 255]);
    }

}
