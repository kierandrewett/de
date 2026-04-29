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

/// Physical size (pixels) of rendered cursor icons. 24 px is the freedesktop
/// default and matches the host system's cursor size.
const CURSOR_SIZE: u32 = 24;

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

// ── Adwaita / Xcursor loader ─────────────────────────────────────────────────
//
// We try the system's cursor theme first. If a matching cursor file exists
// we use the rasterised image at the size closest to CURSOR_SIZE; otherwise
// we fall back to the embedded SVG.

const ADWAITA_DIR: &str = "/usr/share/icons/Adwaita/cursors";
const FALLBACK_DIRS: &[&str] = &[
    "/usr/share/icons/default/cursors",
    "/usr/share/icons/breeze_cursors/cursors",
    "/usr/share/icons/DMZ-White/cursors",
];

fn load_xcursor(name: &str, target_size: u32)
    -> Option<(Vec<u8>, u32, u32, u32, u32)>
{
    for dir in std::iter::once(ADWAITA_DIR).chain(FALLBACK_DIRS.iter().copied()) {
        let path = std::path::Path::new(dir).join(name);
        let Ok(bytes) = std::fs::read(&path) else { continue };
        let Some(images) = xcursor::parser::parse_xcursor(&bytes) else { continue };
        if images.is_empty() { continue }

        let img = images.iter()
            .min_by_key(|i| (i.size as i64 - target_size as i64).abs())
            .unwrap();

        debug!("xcursor: {} from {:?} ({}×{}, hot={},{})",
            name, dir, img.width, img.height, img.xhot, img.yhot);
        return Some((img.pixels_rgba.clone(), img.width, img.height, img.xhot, img.yhot));
    }
    None
}

fn scale_pixels_to(
    pixels: &[u8], src_w: u32, src_h: u32,
    src_hot_x: u32, src_hot_y: u32,
    target: u32,
) -> Option<(Vec<u8>, f32, f32)> {
    if src_w == 0 || src_h == 0 || target == 0 { return None }
    let mut src = tiny_skia::Pixmap::new(src_w, src_h)?;
    src.data_mut().copy_from_slice(pixels);

    let mut dst = tiny_skia::Pixmap::new(target, target)?;
    let scale = (target as f32 / src_w as f32).min(target as f32 / src_h as f32);
    let off_x = (target as f32 - src_w as f32 * scale) / 2.0;
    let off_y = (target as f32 - src_h as f32 * scale) / 2.0;

    let transform = tiny_skia::Transform::from_translate(off_x, off_y)
        .post_concat(tiny_skia::Transform::from_scale(scale, scale));

    dst.draw_pixmap(0, 0, src.as_ref(), &tiny_skia::PixmapPaint::default(),
                    transform, None);

    let hot_x = off_x + src_hot_x as f32 * scale;
    let hot_y = off_y + src_hot_y as f32 * scale;
    Some((dst.data().to_vec(), hot_x, hot_y))
}

struct AdwaitaImage {
    image: slint::Image,
    hotspot: (f32, f32),
}

fn try_load_adwaita(name: &str, target: u32) -> Option<AdwaitaImage> {
    let (pixels, w, h, hx, hy) = load_xcursor(name, target)?;
    let (scaled, sx, sy) = scale_pixels_to(&pixels, w, h, hx, hy, target)?;
    Some(AdwaitaImage {
        image: pixels_to_slint(scaled, target),
        hotspot: (sx, sy),
    })
}

fn adwaita_name(kind: CursorKind) -> &'static str {
    match kind {
        CursorKind::Arrow              => "default",
        CursorKind::Move               => "move",
        CursorKind::Hand               => "pointer",
        CursorKind::ResizeN            => "n-resize",
        CursorKind::ResizeS            => "s-resize",
        CursorKind::ResizeE            => "e-resize",
        CursorKind::ResizeW            => "w-resize",
        CursorKind::ResizeNWSE { .. }  => "nw-resize",
        CursorKind::ResizeNESW { .. }  => "ne-resize",
    }
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
    /// Cached Adwaita cursors keyed by name. None = tried + failed.
    adwaita_cache: HashMap<&'static str, Option<AdwaitaImage>>,
    /// Cached cursors keyed by arbitrary name — used for `wl_pointer.set_cursor`
    /// requests where clients pass a named cursor (text, pointer, wait, ...).
    /// Key is owned `String` so we can intern any name the client throws at us.
    dynamic_cache: HashMap<String, Option<AdwaitaImage>>,
}

impl CursorRenderer {
    pub fn new() -> Self {
        Self {
            static_cache: HashMap::new(),
            nwse_cache: HashMap::new(),
            nesw_cache: HashMap::new(),
            adwaita_cache: HashMap::new(),
            dynamic_cache: HashMap::new(),
        }
    }

    /// Load + cache an Xcursor by arbitrary name (used for client-requested
    /// cursors via `wl_pointer.set_cursor`). Returns `(image, (hx, hy))` if
    /// the system theme has it, otherwise `None` so the caller can fall back
    /// to the compositor default.
    pub fn get_dynamic(&mut self, name: &str) -> Option<(slint::Image, (f32, f32))> {
        if !self.dynamic_cache.contains_key(name) {
            let loaded = try_load_adwaita(name, CURSOR_SIZE);
            self.dynamic_cache.insert(name.to_string(), loaded);
        }
        self.dynamic_cache.get(name)
            .and_then(|o| o.as_ref())
            .map(|a| (a.image.clone(), a.hotspot))
    }

    /// Lazily load + cache an Adwaita cursor by name.
    fn ensure_adwaita(&mut self, name: &'static str) -> Option<&AdwaitaImage> {
        if !self.adwaita_cache.contains_key(name) {
            let loaded = try_load_adwaita(name, CURSOR_SIZE);
            self.adwaita_cache.insert(name, loaded);
        }
        self.adwaita_cache.get(name).and_then(|o| o.as_ref())
    }

    /// Return a `slint::Image` for the given `CursorKind`. Prefers the
    /// system Xcursor theme; falls back to the embedded SVG if it isn't
    /// installed or the file isn't found.
    pub fn get(&mut self, kind: CursorKind) -> slint::Image {
        // Rotated diagonals never use Adwaita (Slint doesn't expose pixmap
        // bytes for re-rasterisation), so they go straight to SVG.
        match kind {
            CursorKind::ResizeNWSE { angle_offset } => {
                if angle_offset.abs() < 0.5_f64.to_radians() {
                    if let Some(a) = self.ensure_adwaita("nw-resize") {
                        return a.image.clone();
                    }
                }
                return self.get_rotated_nwse(angle_offset);
            }
            CursorKind::ResizeNESW { angle_offset } => {
                if angle_offset.abs() < 0.5_f64.to_radians() {
                    if let Some(a) = self.ensure_adwaita("ne-resize") {
                        return a.image.clone();
                    }
                }
                return self.get_rotated_nesw(angle_offset);
            }
            _ => {}
        }
        let name = adwaita_name(kind);
        if let Some(a) = self.ensure_adwaita(name) {
            return a.image.clone();
        }
        match kind {
            CursorKind::Arrow  => self.get_static("arrow",   ARROW_SVG),
            CursorKind::Move   => self.get_static("move",    MOVE_SVG),
            CursorKind::Hand   => self.get_static("hand",    HAND_SVG),
            CursorKind::ResizeN => self.get_static("resize_n", RESIZE_NS_SVG),
            CursorKind::ResizeS => self.get_static("resize_s", RESIZE_NS_SVG),
            CursorKind::ResizeE => self.get_static("resize_e", RESIZE_EW_SVG),
            CursorKind::ResizeW => self.get_static("resize_w", RESIZE_EW_SVG),
            _                   => slint::Image::default(),
        }
    }

    /// Hotspot position (x, y) within the cursor image. Adwaita hotspots are
    /// read from the Xcursor file; SVG fallbacks use sensible defaults.
    pub fn hotspot(&mut self, kind: CursorKind) -> (f32, f32) {
        let name = adwaita_name(kind);
        if let Some(a) = self.ensure_adwaita(name) {
            return a.hotspot;
        }
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
