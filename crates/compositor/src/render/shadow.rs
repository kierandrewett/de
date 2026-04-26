//! Shadow texture generation and rendering.
//!
//! Shadows are pre-computed on the CPU: a squircle fill is rasterised with
//! `tiny-skia`, then a separable Gaussian blur is applied. Each shadow layer
//! from the theme is computed independently and composited additively.
//!
//! The resulting [`ShadowPixels`] are cached per (window_size, active state)
//! and must be uploaded to a GPU texture by the [`FrameRef`] implementor.
//!
//! # 9-slice note
//! The current implementation caches per exact window size. A 9-slice
//! optimisation (one texture for all sizes of the same config) can be added
//! when profiling shows the cache miss rate is too high.

use std::collections::HashMap;

use rounding::{SquircleConfig, SquirclePath};
use theme::ShadowLayer;
use tracing::{debug, trace};

use super::{FrameRef, Rect};

// ─── Shadow pixel data ───────────────────────────────────────────────────────

/// Pre-computed RGBA shadow image for one (window_size, focus_state) pair.
///
/// The image is larger than the window to accommodate blur spread and offsets.
/// When rendering, position the image at `(window_x - padding, window_y - padding)`.
#[derive(Debug, Clone)]
pub struct ShadowPixels {
    /// RGBA pixel data, row-major top-to-bottom, straight (non-premultiplied) alpha.
    pub data: Vec<u8>,
    /// Total image width in pixels.
    pub width: u32,
    /// Total image height in pixels.
    pub height: u32,
    /// Pixels of extra space on each side beyond the window bounds.
    ///
    /// The shadow image starts at `(window_x - padding, window_y - padding)`.
    pub padding: u32,
}

// ─── Shadow cache ────────────────────────────────────────────────────────────

/// Cache key for shadow lookup.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ShadowCacheKey {
    /// Window width rounded to nearest pixel.
    pub window_width: u32,
    /// Window height rounded to nearest pixel.
    pub window_height: u32,
    /// `true` for focused (active) windows, `false` for unfocused.
    pub is_active: bool,
}

impl ShadowCacheKey {
    /// Build a key from a logical-pixel rect and focus state.
    pub fn from_rect(width: f32, height: f32, is_active: bool) -> Self {
        Self {
            window_width: width.round() as u32,
            window_height: height.round() as u32,
            is_active,
        }
    }
}

/// In-memory cache of pre-computed shadow images.
///
/// Call [`ShadowCache::clear`] on theme change; call [`ShadowCache::invalidate`]
/// on window resize.
#[derive(Default)]
pub struct ShadowCache {
    entries: HashMap<ShadowCacheKey, ShadowPixels>,
}

impl ShadowCache {
    /// Create an empty cache.
    pub fn new() -> Self {
        Self::default()
    }

    /// Return (and if necessary compute) shadow pixels for the given key.
    pub fn get_or_compute(
        &mut self,
        key: ShadowCacheKey,
        shadow_layers: &[ShadowLayer],
        squircle_config: SquircleConfig,
    ) -> &ShadowPixels {
        self.entries.entry(key.clone()).or_insert_with(|| {
            debug!(
                w = key.window_width,
                h = key.window_height,
                active = key.is_active,
                "computing shadow pixels"
            );
            compute_shadow_pixels(
                key.window_width,
                key.window_height,
                shadow_layers,
                squircle_config,
            )
        })
    }

    /// Remove a single cache entry.
    pub fn invalidate(&mut self, key: &ShadowCacheKey) {
        self.entries.remove(key);
    }

    /// Drop all cached images (e.g., on theme change).
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// Number of cached entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the cache is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

// ─── Shadow renderer ─────────────────────────────────────────────────────────

/// Renders the multi-layer Gaussian shadow behind a window.
pub struct ShadowRenderer {
    cache: ShadowCache,
}

impl ShadowRenderer {
    /// Create a new renderer with an empty cache.
    pub fn new() -> Self {
        Self { cache: ShadowCache::new() }
    }

    /// Mutable access to the underlying cache (needed for invalidation on theme change).
    pub fn cache_mut(&mut self) -> &mut ShadowCache {
        &mut self.cache
    }

    /// Render the shadow for a window.
    ///
    /// `window_rect` is the outer window bounds (before decorations are added).
    /// `shadow_layers` comes from `WindowBorderStyle::shadow_layers`.
    pub fn render<F: FrameRef>(
        &mut self,
        frame: &mut F,
        window_rect: Rect,
        shadow_layers: &[ShadowLayer],
        squircle_config: SquircleConfig,
        is_active: bool,
    ) -> Result<(), F::Error> {
        if shadow_layers.is_empty() {
            return Ok(());
        }

        let key = ShadowCacheKey::from_rect(window_rect.width, window_rect.height, is_active);
        let pixels = self.cache.get_or_compute(key, shadow_layers, squircle_config);

        let dst = Rect::new(
            window_rect.x - pixels.padding as f32,
            window_rect.y - pixels.padding as f32,
            pixels.width as f32,
            pixels.height as f32,
        );

        trace!(
            dst_x = dst.x,
            dst_y = dst.y,
            dst_w = dst.width,
            dst_h = dst.height,
            "drawing shadow"
        );

        frame.draw_pixels(&pixels.data, pixels.width, pixels.height, dst, 1.0)
    }
}

impl Default for ShadowRenderer {
    fn default() -> Self {
        Self::new()
    }
}

// ─── Shadow computation helpers ──────────────────────────────────────────────

/// Canvas layout for a padded shadow image.
struct CanvasLayout {
    window_w: u32,
    window_h: u32,
    total_w: u32,
    total_h: u32,
    padding: u32,
}

impl CanvasLayout {
    fn new(window_w: u32, window_h: u32, padding: u32) -> Self {
        Self {
            window_w,
            window_h,
            total_w: window_w + 2 * padding,
            total_h: window_h + 2 * padding,
            padding,
        }
    }

    fn pixel_count(&self) -> usize {
        (self.total_w * self.total_h) as usize
    }
}

/// Mutable RGBA accumulator buffers for combining shadow layers.
struct LayerAccumulators {
    r: Vec<u16>,
    g: Vec<u16>,
    b: Vec<u16>,
    a: Vec<u16>,
}

impl LayerAccumulators {
    fn new(pixel_count: usize) -> Self {
        Self {
            r: vec![0; pixel_count],
            g: vec![0; pixel_count],
            b: vec![0; pixel_count],
            a: vec![0; pixel_count],
        }
    }
}

// ─── Shadow computation ──────────────────────────────────────────────────────

/// Compute combined shadow pixel data for all layers.
pub(crate) fn compute_shadow_pixels(
    window_w: u32,
    window_h: u32,
    layers: &[ShadowLayer],
    squircle_config: SquircleConfig,
) -> ShadowPixels {
    let padding = shadow_padding(layers);
    let canvas = CanvasLayout::new(window_w, window_h, padding);
    let mut acc = LayerAccumulators::new(canvas.pixel_count());

    for layer in layers {
        accumulate_layer(&canvas, layer, squircle_config, &mut acc);
    }

    // Pack into RGBA bytes.
    let pixel_count = canvas.pixel_count();
    let mut data = vec![0u8; pixel_count * 4];
    for i in 0..pixel_count {
        let a = acc.a[i].min(255) as u8;
        let r = if acc.a[i] > 0 { (acc.r[i] / acc.a[i].max(1)).min(255) as u8 } else { 0 };
        let g = if acc.a[i] > 0 { (acc.g[i] / acc.a[i].max(1)).min(255) as u8 } else { 0 };
        let b = if acc.a[i] > 0 { (acc.b[i] / acc.a[i].max(1)).min(255) as u8 } else { 0 };
        data[i * 4] = r;
        data[i * 4 + 1] = g;
        data[i * 4 + 2] = b;
        data[i * 4 + 3] = a;
    }

    ShadowPixels { data, width: canvas.total_w, height: canvas.total_h, padding }
}

/// Compute one shadow layer and add it into the accumulator buffers.
fn accumulate_layer(
    canvas: &CanvasLayout,
    layer: &ShadowLayer,
    squircle_config: SquircleConfig,
    acc: &mut LayerAccumulators,
) {
    let pixel_count = canvas.pixel_count();
    let mut alpha_mask = vec![0u8; pixel_count];

    // Position the squircle inside the padded canvas, shifted by layer offset + spread.
    let sq_x = canvas.padding as f32 + layer.offset_x - layer.spread;
    let sq_y = canvas.padding as f32 + layer.offset_y - layer.spread;
    let sq_w = canvas.window_w as f32 + 2.0 * layer.spread;
    let sq_h = canvas.window_h as f32 + 2.0 * layer.spread;

    rasterise_squircle_alpha(sq_x, sq_y, sq_w, sq_h, squircle_config, canvas, &mut alpha_mask);

    // Gaussian blur the alpha mask.
    let blurred = if layer.blur_radius >= 0.5 {
        gaussian_blur_u8(&alpha_mask, canvas.total_w as usize, canvas.total_h as usize, layer.blur_radius)
    } else {
        alpha_mask
    };

    // Extract shadow colour.
    let [sr, sg, sb, sa] = layer.color;
    let color_r = (sr * 255.0).round().clamp(0.0, 255.0) as u16;
    let color_g = (sg * 255.0).round().clamp(0.0, 255.0) as u16;
    let color_b = (sb * 255.0).round().clamp(0.0, 255.0) as u16;

    for (i, &mask_byte) in blurred.iter().enumerate() {
        let mask_a = mask_byte as f32 / 255.0;
        let final_a = (mask_a * sa * 255.0).round().clamp(0.0, 255.0) as u16;
        acc.a[i] = acc.a[i].saturating_add(final_a);
        acc.r[i] = acc.r[i].saturating_add(color_r * final_a / 255);
        acc.g[i] = acc.g[i].saturating_add(color_g * final_a / 255);
        acc.b[i] = acc.b[i].saturating_add(color_b * final_a / 255);
    }
}

/// Rasterise a squircle fill into a single-channel alpha buffer using tiny-skia.
fn rasterise_squircle_alpha(
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    squircle_config: SquircleConfig,
    canvas: &CanvasLayout,
    out: &mut [u8],
) {
    debug_assert_eq!(out.len(), canvas.pixel_count());

    // Skip degenerate rects.
    if width <= 0.0 || height <= 0.0 || canvas.total_w == 0 || canvas.total_h == 0 {
        return;
    }

    let path = SquirclePath::new(x, y, width, height, squircle_config);

    if let Some(sk_path) = path.to_tiny_skia_path() {
        if let Some(mut pixmap) = tiny_skia::Pixmap::new(canvas.total_w, canvas.total_h) {
            let mut paint = tiny_skia::Paint::default();
            paint.set_color_rgba8(255, 255, 255, 255);
            paint.anti_alias = true;

            pixmap.fill_path(
                &sk_path,
                &paint,
                tiny_skia::FillRule::Winding,
                tiny_skia::Transform::identity(),
                None,
            );

            // Extract alpha channel.
            // tiny-skia uses premultiplied RGBA. For white fill at full opacity,
            // the alpha channel directly encodes coverage (including anti-alias blend).
            let data = pixmap.data();
            for (i, chunk) in data.chunks_exact(4).enumerate() {
                out[i] = chunk[3];
            }
        }
    }
}

// ─── Gaussian blur (separable, single channel) ───────────────────────────────

/// Apply a separable Gaussian blur to a single-channel u8 image in-place.
///
/// `radius` is the approximate visual blur radius in pixels. The kernel is
/// sized to 3σ on each side (standard 99.7% coverage).
pub(crate) fn gaussian_blur_u8(src: &[u8], width: usize, height: usize, radius: f32) -> Vec<u8> {
    debug_assert_eq!(src.len(), width * height);

    if radius < 0.5 || width == 0 || height == 0 {
        return src.to_vec();
    }

    let kernel = gaussian_kernel(radius / 3.0_f32.max(f32::EPSILON));
    let half = kernel.len() / 2;

    // ── Horizontal pass ─────────────────────────────────────────────────────
    let mut temp = vec![0u8; src.len()];
    for y in 0..height {
        for x in 0..width {
            let mut sum = 0.0f32;
            let mut weight = 0.0f32;
            for (ki, &kv) in kernel.iter().enumerate() {
                let sx = x as isize + ki as isize - half as isize;
                if sx >= 0 && (sx as usize) < width {
                    sum += src[y * width + sx as usize] as f32 * kv;
                    weight += kv;
                }
            }
            temp[y * width + x] = (sum / weight).round().clamp(0.0, 255.0) as u8;
        }
    }

    // ── Vertical pass ───────────────────────────────────────────────────────
    let mut dst = vec![0u8; src.len()];
    for y in 0..height {
        for x in 0..width {
            let mut sum = 0.0f32;
            let mut weight = 0.0f32;
            for (ki, &kv) in kernel.iter().enumerate() {
                let sy = y as isize + ki as isize - half as isize;
                if sy >= 0 && (sy as usize) < height {
                    sum += temp[sy as usize * width + x] as f32 * kv;
                    weight += kv;
                }
            }
            dst[y * width + x] = (sum / weight).round().clamp(0.0, 255.0) as u8;
        }
    }

    dst
}

/// Build a 1-D Gaussian kernel for the given standard deviation.
///
/// The kernel is sized to ⌈3σ⌉ on each side and normalised (weights sum to 1).
pub(crate) fn gaussian_kernel(sigma: f32) -> Vec<f32> {
    let radius = (3.0 * sigma).ceil() as usize;
    let size = 2 * radius + 1;
    let s2 = 2.0 * sigma * sigma;
    let mut kernel: Vec<f32> = (0..size)
        .map(|i| {
            let x = i as f32 - radius as f32;
            (-x * x / s2).exp()
        })
        .collect();
    // Normalise so weights sum to 1.
    let total: f32 = kernel.iter().sum();
    if total > 0.0 {
        for v in &mut kernel {
            *v /= total;
        }
    }
    kernel
}

/// Compute the pixel padding needed to contain all shadow layers.
///
/// Uses the 3σ ≈ blur_radius rule — kernels are sized at 3× the sigma, so the
/// visual extent is approximately `blur_radius` pixels from the shape edge.
fn shadow_padding(layers: &[ShadowLayer]) -> u32 {
    let max_blur = layers.iter().map(|l| l.blur_radius).fold(0.0f32, f32::max);
    let max_oy = layers.iter().map(|l| l.offset_y.abs()).fold(0.0f32, f32::max);
    let max_ox = layers.iter().map(|l| l.offset_x.abs()).fold(0.0f32, f32::max);
    let max_spread = layers.iter().map(|l| l.spread.abs()).fold(0.0f32, f32::max);
    // Add 2 px margin so blur doesn't clip hard at the image edge.
    (max_blur + max_oy + max_ox + max_spread + 2.0).ceil() as u32
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use theme::WindowShadowTheme;

    fn active_layers() -> Vec<ShadowLayer> {
        WindowShadowTheme::default().active
    }

    fn inactive_layers() -> Vec<ShadowLayer> {
        WindowShadowTheme::default().inactive
    }

    #[test]
    fn gaussian_kernel_sums_to_one() {
        let k = gaussian_kernel(2.0);
        let sum: f32 = k.iter().sum();
        assert!((sum - 1.0).abs() < 1e-5, "kernel sum = {sum}");
    }

    #[test]
    fn gaussian_kernel_is_symmetric() {
        let k = gaussian_kernel(3.0);
        for i in 0..k.len() / 2 {
            let j = k.len() - 1 - i;
            assert!((k[i] - k[j]).abs() < 1e-6, "not symmetric at {i}");
        }
    }

    #[test]
    fn gaussian_kernel_peak_at_centre() {
        let k = gaussian_kernel(2.0);
        let centre = k[k.len() / 2];
        for &v in &k {
            assert!(v <= centre + 1e-6);
        }
    }

    #[test]
    fn blur_preserves_length() {
        let src = vec![128u8; 100];
        let blurred = gaussian_blur_u8(&src, 10, 10, 3.0);
        assert_eq!(blurred.len(), 100);
    }

    #[test]
    fn blur_spreads_point_source() {
        // A single bright pixel in a dark image should spread after blurring.
        let mut src = vec![0u8; 10 * 10];
        src[5 * 10 + 5] = 255; // centre pixel bright
        let blurred = gaussian_blur_u8(&src, 10, 10, 1.5);
        // Adjacent pixels should be non-zero.
        assert!(blurred[5 * 10 + 4] > 0, "left neighbour should be non-zero");
        assert!(blurred[5 * 10 + 6] > 0, "right neighbour should be non-zero");
        assert!(blurred[4 * 10 + 5] > 0, "top neighbour should be non-zero");
        assert!(blurred[6 * 10 + 5] > 0, "bottom neighbour should be non-zero");
    }

    #[test]
    fn blur_zero_radius_is_identity() {
        let src: Vec<u8> = (0..=255u8).cycle().take(256).collect();
        let result = gaussian_blur_u8(&src, 16, 16, 0.0);
        assert_eq!(result, src);
    }

    #[test]
    fn shadow_padding_active() {
        let layers = active_layers();
        let p = shadow_padding(&layers);
        // Wide layer has blur_radius=48 → expect padding ≥ 48.
        assert!(p >= 48, "expected padding ≥ 48, got {p}");
    }

    #[test]
    fn shadow_padding_inactive() {
        let layers = inactive_layers();
        let p = shadow_padding(&layers);
        // Widest layer has blur_radius=12 → expect padding ≥ 12.
        assert!(p >= 12, "expected padding ≥ 12, got {p}");
    }

    #[test]
    fn shadow_pixels_correct_dimensions() {
        let layers = active_layers();
        let config = SquircleConfig::new(14.0, 0.6);
        let pixels = compute_shadow_pixels(400, 300, &layers, config);
        let padding = shadow_padding(&layers);

        assert_eq!(pixels.width, 400 + 2 * padding);
        assert_eq!(pixels.height, 300 + 2 * padding);
        assert_eq!(pixels.padding, padding);
        assert_eq!(pixels.data.len(), (pixels.width * pixels.height * 4) as usize);
    }

    #[test]
    fn shadow_pixels_have_nonzero_alpha() {
        let layers = active_layers();
        let config = SquircleConfig::new(14.0, 0.6);
        let pixels = compute_shadow_pixels(200, 150, &layers, config);

        // Centre of the shadow (inside window) should have non-zero alpha.
        let cx = pixels.width / 2;
        let cy = pixels.height / 2;
        let idx = (cy * pixels.width + cx) as usize * 4;
        assert!(pixels.data[idx + 3] > 0, "centre pixel should have shadow alpha");
    }

    #[test]
    fn shadow_cache_hit() {
        let mut cache = ShadowCache::new();
        let layers = active_layers();
        let config = SquircleConfig::new(14.0, 0.6);
        let key = ShadowCacheKey::from_rect(200.0, 150.0, true);

        let _ = cache.get_or_compute(key.clone(), &layers, config);
        assert_eq!(cache.len(), 1);
        // Same key → cache hit, no recompute.
        let _ = cache.get_or_compute(key.clone(), &layers, config);
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn shadow_cache_different_keys() {
        let mut cache = ShadowCache::new();
        let layers = active_layers();
        let config = SquircleConfig::new(14.0, 0.6);

        let _ = cache.get_or_compute(ShadowCacheKey::from_rect(200.0, 150.0, true), &layers, config);
        let _ = cache.get_or_compute(ShadowCacheKey::from_rect(300.0, 200.0, true), &layers, config);
        let _ = cache.get_or_compute(ShadowCacheKey::from_rect(200.0, 150.0, false), &layers, config);
        assert_eq!(cache.len(), 3);
    }

    #[test]
    fn shadow_cache_clear() {
        let mut cache = ShadowCache::new();
        let layers = active_layers();
        let config = SquircleConfig::new(14.0, 0.6);
        let _ = cache.get_or_compute(ShadowCacheKey::from_rect(200.0, 150.0, true), &layers, config);
        cache.clear();
        assert!(cache.is_empty());
    }

    #[test]
    fn shadow_cache_invalidate() {
        let mut cache = ShadowCache::new();
        let layers = active_layers();
        let config = SquircleConfig::new(14.0, 0.6);
        let key = ShadowCacheKey::from_rect(200.0, 150.0, true);
        let _ = cache.get_or_compute(key.clone(), &layers, config);
        assert_eq!(cache.len(), 1);
        cache.invalidate(&key);
        assert!(cache.is_empty());
    }

    #[test]
    fn active_shadow_darker_than_inactive() {
        // Active shadows have more/larger layers and higher alpha than inactive.
        let active_layers = active_layers();
        let inactive_layers = inactive_layers();
        let config = SquircleConfig::new(14.0, 0.6);

        let active = compute_shadow_pixels(300, 200, &active_layers, config);
        let inactive = compute_shadow_pixels(300, 200, &inactive_layers, config);

        // Compare alpha at centre.
        let cx = active.width / 2;
        let cy = active.height / 2;
        let idx_a = (cy * active.width + cx) as usize * 4 + 3;
        let idx_i = (cy * inactive.width + cx) as usize * 4 + 3;
        assert!(
            active.data[idx_a] >= inactive.data[idx_i],
            "active shadow ({}) should be >= inactive ({})",
            active.data[idx_a],
            inactive.data[idx_i],
        );
    }
}
