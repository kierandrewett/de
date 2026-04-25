#![deny(missing_docs)]
//! SVG cursor theme loading and on-demand rasterisation for Wayland compositors.
//!
//! Supports KDE-format SVG cursor themes (`cursors_scalable/` directories) with
//! transparent fallback to legacy Xcursor bitmap themes.
//!
//! # Usage
//! ```no_run
//! use cursor::{CursorThemeManager, CursorShape};
//!
//! let mut mgr = CursorThemeManager::load("Breeze", "default").unwrap();
//! let name = CursorThemeManager::shape_to_name(CursorShape::Pointer);
//! if let Some(cursor) = mgr.get_cursor(name, 48) {
//!     println!("{}×{} hotspot ({},{})", cursor.width, cursor.height,
//!              cursor.hotspot_x, cursor.hotspot_y);
//! }
//! ```

mod metadata;
mod render;
mod shape;
mod theme;
mod xcursor_fb;

pub use shape::CursorShape;

use lru::LruCache;
use std::num::NonZeroUsize;
use std::path::PathBuf;
use thiserror::Error;
use tracing::{debug, warn};

/// Maximum number of rasterised cursors held in the LRU cache.
const CACHE_SIZE: usize = 64;

/// Placeholder accent colour used in KDE SVG cursor themes.
///
/// When [`CursorThemeManager::set_accent_color`] is called, occurrences of
/// this hex string in SVG source are replaced with the new colour before
/// rendering.
pub const ACCENT_PLACEHOLDER: &str = "#1d99f3";

/// Rasterised pixel data for a single cursor frame.
///
/// Pixel data is in premultiplied RGBA format, row-major, 4 bytes per pixel.
pub struct CachedCursor {
    /// Raw pixel bytes: premultiplied RGBA, 4 bytes per pixel, row-major.
    pub pixels: Vec<u8>,
    /// Image width in pixels.
    pub width: u32,
    /// Image height in pixels.
    pub height: u32,
    /// Hotspot X coordinate (pixels from left edge).
    pub hotspot_x: i32,
    /// Hotspot Y coordinate (pixels from top edge).
    pub hotspot_y: i32,
}

/// An animated cursor consisting of multiple frames with per-frame durations.
pub struct AnimatedCursor {
    /// Ordered list of frames; same length as `frame_durations_ms`.
    pub frames: Vec<CachedCursor>,
    /// Display duration of each frame in milliseconds.
    pub frame_durations_ms: Vec<u32>,
}

/// Errors returned by cursor theme operations.
#[derive(Debug, Error)]
pub enum CursorError {
    /// The requested theme directory could not be found on any search path.
    #[error("cursor theme not found: {0}")]
    ThemeNotFound(String),
    /// An I/O error occurred while reading a theme file.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    /// A cursor's `metadata.json` could not be deserialised.
    #[error("metadata parse error: {0}")]
    Metadata(#[from] serde_json::Error),
    /// SVG parsing or rasterisation failed.
    #[error("SVG error: {0}")]
    Svg(String),
    /// A pixmap could not be allocated for the requested size.
    #[error("pixmap allocation failed for size {0}")]
    RenderFailed(u32),
}

/// Specialised [`Result`](std::result::Result) for cursor operations.
pub type Result<T> = std::result::Result<T, CursorError>;

/// Manages cursor theme loading, SVG rasterisation, and LRU caching.
///
/// # Thread safety
///
/// `CursorThemeManager` is `Send` but not `Sync`; the Wayland compositor is
/// single-threaded and owns this manager exclusively.
pub struct CursorThemeManager {
    /// Root directory of the SVG cursor theme (contains `cursors_scalable/`).
    theme_dir: Option<PathBuf>,
    /// Xcursor fallback used when no matching SVG cursor is found.
    xcursor_theme: xcursor::CursorTheme,
    /// LRU cache keyed by `(cursor_name, physical_size)`.
    cache: LruCache<(String, u32), CachedCursor>,
    /// Optional accent colour to substitute for [`ACCENT_PLACEHOLDER`] in SVGs.
    accent_color: Option<[u8; 3]>,
}

impl CursorThemeManager {
    /// Load a cursor theme by name, falling back to `fallback` for missing cursors.
    ///
    /// Searches `$XCURSOR_PATH`, `~/.local/share/icons`, `~/.icons`, and
    /// `/usr/share/icons` for the theme directory.  Missing themes are not fatal;
    /// the manager will use the Xcursor fallback for every cursor request.
    pub fn load(theme_name: &str, fallback: &str) -> Result<Self> {
        let theme_dir = theme::find_theme(theme_name);
        if theme_dir.is_none() {
            warn!(
                "SVG cursor theme '{}' not found, falling back to xcursor only",
                theme_name
            );
        } else {
            debug!(
                "Loaded SVG cursor theme '{}' from {:?}",
                theme_name, theme_dir
            );
        }

        // xcursor internally handles inheritance chains; `fallback` is only used
        // as the xcursor theme name when the primary name yields no result.
        let xcursor_theme = xcursor::CursorTheme::load(theme_name);
        let _ = fallback;

        Ok(Self {
            theme_dir,
            xcursor_theme,
            cache: LruCache::new(NonZeroUsize::new(CACHE_SIZE).expect("CACHE_SIZE > 0")),
            accent_color: None,
        })
    }

    /// Return a cached reference to a rasterised cursor.
    ///
    /// Tries the SVG theme first, then falls back to the Xcursor theme.
    /// Returns `None` if the cursor cannot be found in either source.
    pub fn get_cursor(&mut self, name: &str, physical_size: u32) -> Option<&CachedCursor> {
        let key = (name.to_owned(), physical_size);

        if !self.cache.contains(&key) {
            let cursor = self.render_uncached(name, physical_size)?;
            self.cache.put(key.clone(), cursor);
        }

        self.cache.get(&key)
    }

    /// Build and return an animated cursor for `name` at `physical_size`.
    ///
    /// Returns `None` if the cursor has no animation data, is not in the SVG
    /// theme, or cannot be rendered.  Each call re-renders all frames; callers
    /// should cache the result externally if repeated access is needed.
    pub fn get_animated(&mut self, name: &str, physical_size: u32) -> Option<AnimatedCursor> {
        let theme_dir = self.theme_dir.as_ref()?;
        let cursor_dir = theme_dir.join("cursors_scalable").join(name);

        let meta_path = cursor_dir.join("metadata.json");
        let meta = metadata::CursorMetadata::load(&meta_path)
            .map_err(|e| {
                debug!("No animation metadata for '{}': {}", name, e);
                e
            })
            .ok()?;

        if !meta.is_animated() {
            return None;
        }

        let hotspot_x =
            metadata::CursorMetadata::scale_hotspot(meta.hotspot_x, meta.nominal_size, physical_size);
        let hotspot_y =
            metadata::CursorMetadata::scale_hotspot(meta.hotspot_y, meta.nominal_size, physical_size);

        let accent = self.accent_color;
        let mut frames = Vec::with_capacity(meta.frames.len());
        let mut durations = Vec::with_capacity(meta.frames.len());

        for frame_entry in &meta.frames {
            let svg_path = cursor_dir.join(&frame_entry.filename);
            let svg_data = match std::fs::read_to_string(&svg_path) {
                Ok(d) => d,
                Err(e) => {
                    warn!("Failed to read SVG frame {:?}: {}", svg_path, e);
                    return None;
                }
            };

            let pixels = match render::render_svg(&svg_data, physical_size, accent) {
                Ok(p) => p,
                Err(e) => {
                    warn!("Failed to render SVG frame: {}", e);
                    return None;
                }
            };

            frames.push(CachedCursor {
                pixels,
                width: physical_size,
                height: physical_size,
                hotspot_x,
                hotspot_y,
            });
            durations.push(frame_entry.duration);
        }

        Some(AnimatedCursor {
            frames,
            frame_durations_ms: durations,
        })
    }

    /// Convert a `cursor-shape-v1` [`CursorShape`] to its XCursor directory name.
    ///
    /// The returned string can be passed directly to [`get_cursor`](Self::get_cursor).
    pub fn shape_to_name(shape: CursorShape) -> &'static str {
        shape.to_name()
    }

    /// Clear the rasterisation cache, freeing all cached pixel data.
    pub fn clear_cache(&mut self) {
        self.cache.clear();
        debug!("Cursor cache cleared");
    }

    /// Set an accent colour for live SVG recolouring.
    ///
    /// SVG source strings containing [`ACCENT_PLACEHOLDER`] (`#1d99f3`, the
    /// KDE Breeze accent colour) will have it replaced with `color` on the
    /// next render.  Clears the existing cache so stale pixels are not served.
    pub fn set_accent_color(&mut self, color: [u8; 3]) {
        self.accent_color = Some(color);
        self.clear_cache();
    }

    // ── Internal helpers ──────────────────────────────────────────────────────

    /// Render `name` at `physical_size` without consulting the cache.
    fn render_uncached(&self, name: &str, physical_size: u32) -> Option<CachedCursor> {
        // Try SVG theme with primary name and common aliases.
        if self.theme_dir.is_some() {
            let mut names_to_try = Vec::with_capacity(4);
            names_to_try.push(name);
            names_to_try.extend_from_slice(shape::cursor_fallbacks(name));

            for candidate in names_to_try {
                if let Some(cursor) = self.try_svg(candidate, physical_size) {
                    return Some(cursor);
                }
            }
        }

        // Xcursor fallback (also tries aliases internally).
        let mut names_to_try = Vec::with_capacity(4);
        names_to_try.push(name);
        names_to_try.extend_from_slice(shape::cursor_fallbacks(name));

        for candidate in names_to_try {
            match xcursor_fb::load_xcursor(&self.xcursor_theme, candidate, physical_size) {
                Ok(Some(c)) => return Some(c),
                Ok(None) => {}
                Err(e) => warn!("xcursor error for '{}': {}", candidate, e),
            }
        }

        debug!("Cursor '{}' not found at size {}", name, physical_size);
        None
    }

    /// Attempt to load and rasterise an SVG cursor from the theme directory.
    fn try_svg(&self, name: &str, physical_size: u32) -> Option<CachedCursor> {
        let theme_dir = self.theme_dir.as_ref()?;
        let cursor_dir = theme_dir.join("cursors_scalable").join(name);

        let meta_path = cursor_dir.join("metadata.json");
        let meta = match metadata::CursorMetadata::load(&meta_path) {
            Ok(m) => m,
            Err(_) => return None,
        };

        let hotspot_x =
            metadata::CursorMetadata::scale_hotspot(meta.hotspot_x, meta.nominal_size, physical_size);
        let hotspot_y =
            metadata::CursorMetadata::scale_hotspot(meta.hotspot_y, meta.nominal_size, physical_size);

        // For static cursors the SVG shares the directory name;
        // for animated cursors use the first frame.
        let svg_filename = if meta.is_animated() {
            meta.frames
                .first()
                .map(|f| f.filename.clone())
                .unwrap_or_else(|| format!("{}.svg", name))
        } else {
            format!("{}.svg", name)
        };

        let svg_path = cursor_dir.join(&svg_filename);
        let svg_data = match std::fs::read_to_string(&svg_path) {
            Ok(d) => d,
            Err(e) => {
                debug!("SVG not readable at {:?}: {}", svg_path, e);
                return None;
            }
        };

        match render::render_svg(&svg_data, physical_size, self.accent_color) {
            Ok(pixels) => Some(CachedCursor {
                pixels,
                width: physical_size,
                height: physical_size,
                hotspot_x,
                hotspot_y,
            }),
            Err(e) => {
                warn!("SVG render failed for '{}' @{}: {}", name, physical_size, e);
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal on-disk SVG cursor theme for testing.
    fn make_test_theme(dir: &std::path::Path, name: &str) {
        let cursor_dir = dir.join("cursors_scalable").join(name);
        std::fs::create_dir_all(&cursor_dir).unwrap();

        // metadata.json
        let meta = serde_json::json!({
            "hotspot_x": 4,
            "hotspot_y": 4,
            "nominal_size": 24
        });
        std::fs::write(cursor_dir.join("metadata.json"), meta.to_string()).unwrap();

        // SVG file
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" width="24" height="24">
  <rect x="2" y="2" width="20" height="20" fill="#1d99f3"/>
</svg>"##;
        std::fs::write(cursor_dir.join(format!("{}.svg", name)), svg).unwrap();
    }

    #[test]
    fn test_load_and_get_cursor() {
        let tmp = tempfile::tempdir().unwrap();
        make_test_theme(tmp.path(), "left_ptr");

        // Temporarily set XCURSOR_PATH so find_theme discovers our temp theme.
        std::env::set_var("XCURSOR_PATH", tmp.path());
        let mut mgr = CursorThemeManager::load(".", "default").unwrap();
        std::env::remove_var("XCURSOR_PATH");

        // Inject theme_dir directly since "." lookup is awkward.
        mgr.theme_dir = Some(tmp.path().to_path_buf());

        let cursor = mgr.get_cursor("left_ptr", 24);
        assert!(cursor.is_some(), "should find test cursor");
        let c = cursor.unwrap();
        assert_eq!(c.width, 24);
        assert_eq!(c.height, 24);
        assert_eq!(c.pixels.len(), 24 * 24 * 4);
    }

    #[test]
    fn test_cache_hit() {
        let tmp = tempfile::tempdir().unwrap();
        make_test_theme(tmp.path(), "left_ptr");

        let mut mgr = CursorThemeManager::load("__no_theme__", "default").unwrap();
        mgr.theme_dir = Some(tmp.path().to_path_buf());

        let first = mgr.get_cursor("left_ptr", 24);
        assert!(first.is_some());
        // Second call should return the cached entry.
        let second = mgr.get_cursor("left_ptr", 24);
        assert!(second.is_some());
        assert_eq!(mgr.cache.len(), 1, "only one distinct entry should be cached");
    }

    #[test]
    fn test_clear_cache() {
        let tmp = tempfile::tempdir().unwrap();
        make_test_theme(tmp.path(), "left_ptr");

        let mut mgr = CursorThemeManager::load("__no_theme__", "default").unwrap();
        mgr.theme_dir = Some(tmp.path().to_path_buf());

        mgr.get_cursor("left_ptr", 24);
        assert_eq!(mgr.cache.len(), 1);
        mgr.clear_cache();
        assert_eq!(mgr.cache.len(), 0);
    }

    #[test]
    fn test_accent_color_clears_cache() {
        let tmp = tempfile::tempdir().unwrap();
        make_test_theme(tmp.path(), "left_ptr");

        let mut mgr = CursorThemeManager::load("__no_theme__", "default").unwrap();
        mgr.theme_dir = Some(tmp.path().to_path_buf());

        mgr.get_cursor("left_ptr", 24);
        assert_eq!(mgr.cache.len(), 1);
        mgr.set_accent_color([0xFF, 0x00, 0x00]);
        assert_eq!(mgr.cache.len(), 0, "set_accent_color must clear cache");
    }

    #[test]
    fn test_missing_cursor_returns_none() {
        let mut mgr = CursorThemeManager::load("__no_theme__", "default").unwrap();
        assert!(mgr.get_cursor("__no_such_cursor__", 24).is_none());
    }

    #[test]
    fn test_shape_to_name_all_variants() {
        use CursorShape::*;
        let shapes = [
            Default, ContextMenu, Help, Pointer, Progress, Wait, Cell, Crosshair,
            Text, VerticalText, Alias, Copy, Move, NoDrop, NotAllowed, Grab, Grabbing,
            EResize, NResize, NeResize, NwResize, SResize, SeResize, SwResize, WResize,
            EwResize, NsResize, NeswResize, NwseResize, ColResize, RowResize, AllScroll,
            ZoomIn, ZoomOut,
        ];
        for shape in shapes {
            let name = CursorThemeManager::shape_to_name(shape);
            assert!(!name.is_empty(), "shape {:?} returned empty name", shape);
        }
    }

    #[test]
    fn test_shape_to_name_defaults() {
        assert_eq!(CursorThemeManager::shape_to_name(CursorShape::Default), "left_ptr");
        assert_eq!(CursorThemeManager::shape_to_name(CursorShape::Pointer), "hand2");
        assert_eq!(CursorThemeManager::shape_to_name(CursorShape::Wait), "watch");
        assert_eq!(CursorThemeManager::shape_to_name(CursorShape::Text), "xterm");
        assert_eq!(CursorThemeManager::shape_to_name(CursorShape::Grab), "hand1");
        assert_eq!(CursorThemeManager::shape_to_name(CursorShape::Crosshair), "crosshair");
    }

    #[test]
    fn test_xcursor_fallback_no_panic() {
        // With no theme dir set, should fall through to xcursor gracefully.
        let mut mgr = CursorThemeManager::load("__no_theme__", "default").unwrap();
        // This must not panic regardless of whether xcursor finds anything.
        let _ = mgr.get_cursor("left_ptr", 24);
        let _ = mgr.get_cursor("hand2", 48);
    }

    #[test]
    fn test_animated_cursor_no_crash() {
        let tmp = tempfile::tempdir().unwrap();
        let cursor_dir = tmp.path().join("cursors_scalable").join("wait_anim");
        std::fs::create_dir_all(&cursor_dir).unwrap();

        let meta = serde_json::json!({
            "hotspot_x": 12,
            "hotspot_y": 12,
            "nominal_size": 24,
            "frames": [
                {"filename": "f1.svg", "duration": 50},
                {"filename": "f2.svg", "duration": 50}
            ]
        });
        std::fs::write(cursor_dir.join("metadata.json"), meta.to_string()).unwrap();

        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><circle cx="12" cy="12" r="10" fill="red"/></svg>"##;
        std::fs::write(cursor_dir.join("f1.svg"), svg).unwrap();
        std::fs::write(cursor_dir.join("f2.svg"), svg).unwrap();

        let mut mgr = CursorThemeManager::load("__no_theme__", "default").unwrap();
        mgr.theme_dir = Some(tmp.path().to_path_buf());

        let anim = mgr.get_animated("wait_anim", 24).expect("should build animated cursor");
        assert_eq!(anim.frames.len(), 2);
        assert_eq!(anim.frame_durations_ms, vec![50, 50]);
        assert_eq!(anim.frames[0].hotspot_x, 12);
        assert_eq!(anim.frames[0].hotspot_y, 12);
    }

    /// Test with the Breeze theme if it is installed on this machine.
    #[test]
    fn test_breeze_theme_if_available() {
        let breeze = std::path::Path::new("/usr/share/icons/Breeze");
        let breeze_snow = std::path::Path::new("/usr/share/icons/Breeze_Snow");
        let theme_path = if breeze.join("cursors_scalable").exists() {
            breeze
        } else if breeze_snow.join("cursors_scalable").exists() {
            breeze_snow
        } else {
            return; // Breeze not installed; skip
        };

        let mut mgr = CursorThemeManager::load("__no_theme__", "default").unwrap();
        mgr.theme_dir = Some(theme_path.to_path_buf());

        for &size in &[24u32, 48, 96] {
            let cursor = mgr.get_cursor("left_ptr", size);
            if let Some(c) = cursor {
                assert_eq!(c.width, size);
                assert_eq!(c.height, size);
                assert_eq!(c.pixels.len(), (size * size * 4) as usize);
                assert!(c.pixels.iter().any(|&b| b > 0), "rendered cursor must have non-zero pixels");
            }
        }
    }
}
