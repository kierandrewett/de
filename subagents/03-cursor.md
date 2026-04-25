# SUBAGENT: SVG Cursor Subsystem
# Crate: `crates/cursor`

> **Read `ARCHITECTURE.md` in the repo root before starting.** It explains why we use Smithay (not wlroots), how the compositor rendering pipeline works, the full Wayland protocol reference, smithay API patterns (delegate pattern, GlesRenderer, calloop), and the winit dev backend.
# Branch: `feat/cursor`

You are building a Rust crate that loads SVG cursor themes and renders them on demand at any physical pixel size, for use in a Wayland compositor.

## Background
Traditional Xcursor themes ship pre-rasterised bitmaps at a few fixed sizes (24, 32, 48). This breaks with fractional scaling/HiDPI. KDE Plasma 6.3+ pioneered SVG cursor themes: the theme ships SVG source files in a `cursors_scalable/` directory, and the compositor rasterises at the exact size needed.

Our compositor uses the `cursor-shape-v1` Wayland protocol. When an app sends a cursor shape name (e.g. "pointer"), we render it from SVG. For apps that submit their own cursor surface via `wl_pointer.set_cursor`, we display their bitmap as-is.

## SVG Cursor Theme Format (KDE's format)
```
mytheme/
├── index.theme              # [Icon Theme] metadata
├── cursors/                 # Traditional Xcursor files (fallback)
└── cursors_scalable/        # SVG cursors
    ├── left_ptr/
    │   ├── left_ptr.svg
    │   └── metadata.json    # { "hotspot_x": 10, "hotspot_y": 3, "nominal_size": 24 }
    ├── wait/
    │   ├── frame1.svg
    │   ├── frame2.svg
    │   └── metadata.json    # { "frames": [{"filename":"frame1.svg","duration":50}, ...], "hotspot_x": 12, "hotspot_y": 12, "nominal_size": 24 }
    └── ...
```

## Public API
```rust
pub struct CursorThemeManager { /* themes, LRU cache, fallback xcursor */ }
pub struct CachedCursor { pub pixels: Vec<u8>, pub width: u32, pub height: u32, pub hotspot_x: i32, pub hotspot_y: i32 }
pub struct AnimatedCursor { pub frames: Vec<CachedCursor>, pub frame_durations_ms: Vec<u32> }

impl CursorThemeManager {
    pub fn load(theme_name: &str, fallback: &str) -> Result<Self>;
    pub fn get_cursor(&mut self, name: &str, physical_size: u32) -> Option<&CachedCursor>;
    pub fn get_animated(&mut self, name: &str, physical_size: u32) -> Option<AnimatedCursor>;
    pub fn shape_to_name(shape: CursorShape) -> &'static str; // cursor-shape-v1 enum → name
    pub fn clear_cache(&mut self);
    pub fn set_accent_color(&mut self, color: [u8; 3]); // recolour SVGs
}
```

## Implementation Details
- Use `resvg` + `usvg` for SVG→pixel rendering into `tiny_skia::Pixmap`
- LRU cache keyed by `(cursor_name, physical_size)`, max 64 entries
- Hotspot scaling: `hotspot_physical = hotspot_nominal * (physical_size / nominal_size)`
- Search paths: `$XCURSOR_PATH`, `~/.local/share/icons`, `~/.icons`, `/usr/share/icons`
- Xcursor fallback: use the `xcursor` crate for loading legacy `.cursor` files
- Full CSS cursor name mapping: default→left_ptr, pointer→hand2, crosshair→crosshair, text→xterm, wait→watch, grab→hand1, etc. (provide complete table)
- Handle `$XCURSOR_THEME` and `$XCURSOR_SIZE` env vars
- Accent colour recolouring: replace placeholder colour in SVG source string before rendering
- Thread safety: `Send` but not `Sync` (compositor is single-threaded)

## Cargo.toml
```toml
[package]
name = "cursor"
version = "0.1.0"
edition = "2021"

[dependencies]
resvg = { workspace = true }
usvg = { workspace = true }
tiny-skia = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
xcursor = "0.3"
lru = "0.12"
tracing = { workspace = true }
```

## Tests
- Load Breeze SVG theme (if available), render left_ptr at 24/48/96, verify non-empty and correct dimensions
- Hotspot scaling: nominal_size=24 hotspot=(10,3) at size 48 → hotspot should be (20,6)
- Animated cursor: parse metadata.json with frames, verify frame count and durations
- Fallback: missing SVG dir should fall back to xcursor
- Cache: render same cursor twice, second should be cache hit
- Any size 1..512 should never panic

Work iteratively: metadata parsing first, then SVG rendering, then cache, then xcursor fallback, then accent colour.
