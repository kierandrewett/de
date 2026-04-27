//! Software-rasterised cursor overlay.
//!
//! Generates `slint::Image` instances for each cursor kind, with rotation
//! support for the corner-rotation cursor.  Uses `resvg` / `usvg` / `tiny-skia`
//! for SVG rasterisation (same as `crates/cursor`).
//!
//! Icons are embedded at compile-time via `include_str!` so no file-system
//! access is needed at runtime — the compositor can start without a cursor
//! theme installed and still show custom cursors.
//!
//! ## Rotation (corner-rotation cursor)
//! `tiny-skia` supports a full affine `Transform`, so we rotate the pixmap
//! around the hotspot point before returning pixels to Slint.

use std::collections::HashMap;

use tracing::{debug, warn};

use crate::cursor::CursorKind;

// ── Embedded SVG sources ─────────────────────────────────────────────────────

/// Arrow (default) cursor — rendered from an embedded SVG.
const ARROW_SVG: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" width="24" height="24">
  <polygon points="4,2 4,20 8,16 12,24 14,23 10,15 16,15" fill="white" stroke="black" stroke-width="1.2" stroke-linejoin="round"/>
</svg>"#;

/// Move / four-directional arrow cursor.
const MOVE_SVG: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" width="24" height="24">
  <path d="M12 2 L9 6 H11 V11 H6 V9 L2 12 L6 15 V13 H11 V18 H9 L12 22 L15 18 H13 V13 H18 V15 L22 12 L18 9 V11 H13 V6 H15 Z"
    fill="white" stroke="black" stroke-width="0.8" stroke-linejoin="round"/>
</svg>"#;

/// Hand pointer cursor.
const HAND_SVG: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" width="24" height="24">
  <path d="M9 3 C9 2 10 1 11 1 C12 1 13 2 13 3 L13 10 L14 9 C14 8 15 7.5 16 7.5 C17 7.5 18 8.5 18 9.5 L18 16 C18 20 15 23 11 23 C7 23 5 20 5 17 L5 10 C5 9 6 8 7 8 C8 8 9 9 9 10 Z"
    fill="white" stroke="black" stroke-width="1" stroke-linejoin="round"/>
</svg>"#;

/// North/South resize cursor.
const RESIZE_NS_SVG: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" width="24" height="24">
  <path d="M12 2 L8 7 H11 V17 H8 L12 22 L16 17 H13 V7 H16 Z"
    fill="white" stroke="black" stroke-width="1" stroke-linejoin="round"/>
</svg>"#;

/// East/West resize cursor.
const RESIZE_EW_SVG: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" width="24" height="24">
  <path d="M2 12 L7 8 V11 H17 V8 L22 12 L17 16 V13 H7 V16 Z"
    fill="white" stroke="black" stroke-width="1" stroke-linejoin="round"/>
</svg>"#;

/// NW/SE diagonal resize cursor — the rotatable corner arrow.
/// Natural orientation points toward NW (225° from east = SW quadrant).
const RESIZE_NWSE_SVG: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" width="24" height="24">
  <path d="M4 4 L4 10 L6 8 L16 18 L14 16 L20 16 L20 20 L14 20 L16 18 L6 8 L8 6 L4 4 Z"
    fill="white" stroke="black" stroke-width="1" stroke-linejoin="round"/>
  <path d="M20 4 L14 4 L16 6 L6 16 L8 18 L4 18 L4 14 L6 16 L16 6 L14 4 Z"
    fill="white" stroke="black" stroke-width="1" stroke-linejoin="round"/>
</svg>"#;

/// NE/SW diagonal resize cursor.
/// Natural orientation points toward NE (315°).
const RESIZE_NESW_SVG: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" width="24" height="24">
  <path d="M20 4 L14 4 L16 6 L6 16 L8 18 L4 18 L4 14 L6 16 L16 6 L14 4 Z"
    fill="white" stroke="black" stroke-width="1" stroke-linejoin="round"/>
  <path d="M4 4 L4 10 L6 8 L16 18 L14 16 L20 16 L20 20 L14 20 L16 18 L6 8 L8 6 L4 4 Z"
    fill="white" stroke="black" stroke-width="1" stroke-linejoin="round"/>
</svg>"#;

// ── Cursor size ──────────────────────────────────────────────────────────────

/// Physical size (pixels) of rendered cursor icons.
const CURSOR_SIZE: u32 = 32;

// ── Rasterisation helpers ────────────────────────────────────────────────────

/// Rasterise an SVG string (no rotation) to premultiplied RGBA bytes.
fn rasterise_svg(svg_data: &str, size: u32) -> Option<Vec<u8>> {
    if size == 0 { return None; }
    let opt = usvg::Options::default();
    let tree = match usvg::Tree::from_str(svg_data, &opt) {
        Ok(t) => t,
        Err(e) => { warn!("SVG parse error: {}", e); return None; }
    };
    let svg_size = tree.size();
    let sx = size as f32 / svg_size.width();
    let sy = size as f32 / svg_size.height();
    let transform = tiny_skia::Transform::from_scale(sx, sy);
    let mut pixmap = tiny_skia::Pixmap::new(size, size)?;
    resvg::render(&tree, transform, &mut pixmap.as_mut());
    Some(pixmap.data().to_vec())
}

/// Rasterise an SVG and apply a rotation (radians) around the image centre.
///
/// The hotspot at `(size/2, size/2)` is the pivot.  After rotation the image
/// may clip at the edges — that is intentional; the cursor icon is small
/// (32 px) so ≤45° rotation stays within bounds.
fn rasterise_svg_rotated(svg_data: &str, size: u32, angle_rad: f64) -> Option<Vec<u8>> {
    if size == 0 { return None; }
    let opt = usvg::Options::default();
    let tree = match usvg::Tree::from_str(svg_data, &opt) {
        Ok(t) => t,
        Err(e) => { warn!("SVG parse error: {}", e); return None; }
    };
    let svg_size = tree.size();
    let sx = size as f32 / svg_size.width();
    let sy = size as f32 / svg_size.height();

    let cx = size as f32 / 2.0;
    let cy = size as f32 / 2.0;

    // Build: translate to centre → rotate → translate back → scale.
    let transform = tiny_skia::Transform::from_translate(cx, cy)
        .post_concat(tiny_skia::Transform::from_rotate(angle_rad.to_degrees() as f32))
        .post_concat(tiny_skia::Transform::from_translate(-cx, -cy))
        .post_concat(tiny_skia::Transform::from_scale(sx, sy));

    let mut pixmap = tiny_skia::Pixmap::new(size, size)?;
    resvg::render(&tree, transform, &mut pixmap.as_mut());
    Some(pixmap.data().to_vec())
}

/// Convert premultiplied RGBA bytes to a `slint::Image`.
fn pixels_to_slint(pixels: Vec<u8>, size: u32) -> slint::Image {
    let buf = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(
        &pixels, size, size,
    );
    slint::Image::from_rgba8_premultiplied(buf)
}

// ── Cursor cache ─────────────────────────────────────────────────────────────

/// A rotated-angle bucket — we cache at 1° granularity for corner cursors.
fn angle_bucket(rad: f64) -> i32 {
    (rad.to_degrees().round() as i32).rem_euclid(360)
}

/// Manages rasterised cursor images with an angle-keyed LRU for rotated variants.
pub struct CursorRenderer {
    /// Cached static cursors keyed by a small discriminant string.
    static_cache: HashMap<&'static str, slint::Image>,
    /// Cached rotated NWSE variants keyed by degree bucket.
    nwse_cache: HashMap<i32, slint::Image>,
    /// Cached rotated NESW variants keyed by degree bucket.
    nesw_cache: HashMap<i32, slint::Image>,
}

impl CursorRenderer {
    pub fn new() -> Self {
        Self {
            static_cache: HashMap::new(),
            nwse_cache: HashMap::new(),
            nesw_cache: HashMap::new(),
        }
    }

    /// Return a `slint::Image` for the given `CursorKind`.
    ///
    /// Results are cached.  The returned image is `CURSOR_SIZE × CURSOR_SIZE`.
    pub fn get(&mut self, kind: CursorKind) -> slint::Image {
        match kind {
            CursorKind::Arrow  => self.get_static("arrow", ARROW_SVG),
            CursorKind::Move   => self.get_static("move", MOVE_SVG),
            CursorKind::Hand   => self.get_static("hand", HAND_SVG),
            CursorKind::ResizeN => self.get_static("resize_n", RESIZE_NS_SVG),
            CursorKind::ResizeS => self.get_static("resize_s", RESIZE_NS_SVG),
            CursorKind::ResizeE => self.get_static("resize_e", RESIZE_EW_SVG),
            CursorKind::ResizeW => self.get_static("resize_w", RESIZE_EW_SVG),
            CursorKind::ResizeNWSE { angle_offset } => {
                self.get_rotated_nwse(angle_offset)
            }
            CursorKind::ResizeNESW { angle_offset } => {
                self.get_rotated_nesw(angle_offset)
            }
        }
    }

    /// Hotspot position (x, y) within the cursor image for the given kind.
    ///
    /// For most cursors the hotspot is near the tip; for resize cursors it is
    /// centred.
    pub fn hotspot(kind: CursorKind) -> (f32, f32) {
        let s = CURSOR_SIZE as f32;
        match kind {
            CursorKind::Arrow => (4.0, 2.0),
            CursorKind::Hand  => (9.0, 1.0),
            _                 => (s / 2.0, s / 2.0),
        }
    }

    /// Logical size of the cursor image.
    pub fn size() -> f32 {
        CURSOR_SIZE as f32
    }

    fn get_static(&mut self, key: &'static str, svg: &'static str) -> slint::Image {
        if let Some(img) = self.static_cache.get(key) {
            return img.clone();
        }
        let img = match rasterise_svg(svg, CURSOR_SIZE) {
            Some(pixels) => {
                debug!("rasterised cursor '{}' at {}px", key, CURSOR_SIZE);
                pixels_to_slint(pixels, CURSOR_SIZE)
            }
            None => {
                warn!("failed to rasterise cursor '{}'", key);
                slint::Image::default()
            }
        };
        self.static_cache.insert(key, img.clone());
        img
    }

    fn get_rotated_nwse(&mut self, angle_offset: f64) -> slint::Image {
        let bucket = angle_bucket(angle_offset);
        if let Some(img) = self.nwse_cache.get(&bucket) {
            return img.clone();
        }
        let img = match rasterise_svg_rotated(RESIZE_NWSE_SVG, CURSOR_SIZE, angle_offset) {
            Some(pixels) => pixels_to_slint(pixels, CURSOR_SIZE),
            None => {
                warn!("failed to rasterise rotated NWSE cursor");
                slint::Image::default()
            }
        };
        self.nwse_cache.insert(bucket, img.clone());
        img
    }

    fn get_rotated_nesw(&mut self, angle_offset: f64) -> slint::Image {
        let bucket = angle_bucket(angle_offset);
        if let Some(img) = self.nesw_cache.get(&bucket) {
            return img.clone();
        }
        let img = match rasterise_svg_rotated(RESIZE_NESW_SVG, CURSOR_SIZE, angle_offset) {
            Some(pixels) => pixels_to_slint(pixels, CURSOR_SIZE),
            None => {
                warn!("failed to rasterise rotated NESW cursor");
                slint::Image::default()
            }
        };
        self.nesw_cache.insert(bucket, img.clone());
        img
    }
}
