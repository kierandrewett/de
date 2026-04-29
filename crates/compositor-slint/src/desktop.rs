//! `.desktop` file parsing, icon resolution, and dock config.
//!
//! Adapted from `crates/shell-dock/src/desktop.rs` and
//! `crates/shell-dock/src/config.rs`.  No external crate dependency on
//! `freedesktop-desktop-entry` — we parse the INI format ourselves to
//! avoid adding a crate that may not be in the workspace lock file.
//!
//! ## SVG icon decoding
//!
//! Slint's `FemtoVG` backend does not decode SVG natively (no resvg integration
//! in the `renderer-femtovg-wgpu` feature).  We use the `resvg` crate directly:
//! when `load_icon` receives a `.svg` path it rasterises at `DOCK_ICON_SIZE × DOCK_ICON_SIZE`
//! and converts the tiny-skia `Pixmap` to a premultiplied RGBA8 `slint::Image`.
//!
//! Results are cached per (path, size) in a process-global `OnceLock`-backed HashMap
//! so repeated calls for the same icon are O(1) (the dock reloads the item list on
//! every running-state update).

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Mutex,
};

use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

/// Dock icon size in logical pixels.  Rasterise SVG icons at this size.
// Rasterise dock icons at 2× the visual size (52 px in the slot → 104 px
// raster) so they stay crisp on HiDPI and don't look pixelated even when
// the slot grows on hover. Slint's image-fit:contain downscales linearly.
const DOCK_ICON_SIZE: u32 = 128;

// ─────────────────────────────────────────────────────────────────────────────
// SVG icon cache
// ─────────────────────────────────────────────────────────────────────────────

/// Per-process cache mapping (absolute path string, pixel size) → Slint Image.
/// We store the raw RGBA bytes rather than the `slint::Image` so the cache
/// can live in a `Mutex<HashMap<…>>` without `slint::Image` needing to be `Send`.
struct SvgCache {
    /// (canonical path, size_px) → premultiplied RGBA8 bytes + (width, height)
    entries: HashMap<(String, u32), (Vec<u8>, u32, u32)>,
}

impl SvgCache {
    fn new() -> Self {
        Self { entries: HashMap::new() }
    }
}

static SVG_CACHE: std::sync::OnceLock<Mutex<SvgCache>> = std::sync::OnceLock::new();

fn svg_cache() -> &'static Mutex<SvgCache> {
    SVG_CACHE.get_or_init(|| Mutex::new(SvgCache::new()))
}

/// Rasterise an SVG file at `target_size × target_size` and return premultiplied
/// RGBA8 bytes plus (width, height).  Uses a per-process cache.
fn rasterise_svg(path: &Path, target_size: u32) -> Option<(Vec<u8>, u32, u32)> {
    let canonical = path.canonicalize().ok()?;
    let key = (canonical.to_string_lossy().to_string(), target_size);

    // Fast path: cache hit.
    {
        let cache = svg_cache().lock().ok()?;
        if let Some(entry) = cache.entries.get(&key) {
            return Some(entry.clone());
        }
    }

    // Slow path: rasterise with resvg.
    let svg_data = std::fs::read(path).ok()?;

    let opt = resvg::usvg::Options::default();
    let tree = resvg::usvg::Tree::from_data(&svg_data, &opt).ok()?;

    let view_box = tree.size();
    let scale_x = target_size as f32 / view_box.width();
    let scale_y = target_size as f32 / view_box.height();
    let scale = scale_x.min(scale_y);

    let px_w = (view_box.width() * scale).ceil() as u32;
    let px_h = (view_box.height() * scale).ceil() as u32;
    let px_w = px_w.max(1);
    let px_h = px_h.max(1);

    let mut pixmap = resvg::tiny_skia::Pixmap::new(px_w, px_h)?;

    let transform = resvg::tiny_skia::Transform::from_scale(scale, scale);
    resvg::render(&tree, transform, &mut pixmap.as_mut());

    // tiny-skia Pixmap stores premultiplied RGBA8 natively — perfect for Slint.
    let bytes = pixmap.data().to_vec();

    let result = (bytes, px_w, px_h);

    // Store in cache.
    if let Ok(mut cache) = svg_cache().lock() {
        cache.entries.insert(key, result.clone());
    }

    Some(result)
}

// ─────────────────────────────────────────────────────────────────────────────
// Config
// ─────────────────────────────────────────────────────────────────────────────

/// A single pinned application entry in the dock config.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PinnedApp {
    pub app_id: String,
}

/// User-configurable dock settings read from `~/.config/myDE/dock.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DockConfig {
    #[serde(default)]
    pub pinned: Vec<PinnedApp>,
}

impl Default for DockConfig {
    fn default() -> Self {
        // Default pin set, ordered macOS-style: file manager, browser, mail,
        // calendar, terminal, editor, music, settings — and a few extras
        // for visual testing of the dock with a richer line-up.
        Self {
            pinned: vec![
                PinnedApp { app_id: "org.gnome.Nautilus".into() },
                PinnedApp { app_id: "org.mozilla.firefox".into() },
                PinnedApp { app_id: "org.gnome.Geary".into() },
                PinnedApp { app_id: "org.gnome.Calendar".into() },
                PinnedApp { app_id: "kitty".into() },
                PinnedApp { app_id: "code".into() },
                PinnedApp { app_id: "org.gnome.Music".into() },
                PinnedApp { app_id: "org.gnome.Settings".into() },
                PinnedApp { app_id: "org.gnome.Calculator".into() },
                PinnedApp { app_id: "org.gnome.TextEditor".into() },
            ],
        }
    }
}

impl DockConfig {
    /// Path to the config file: `~/.config/myDE/dock.json`.
    pub fn config_path() -> PathBuf {
        home_dir().join(".config/myDE/dock.json")
    }

    /// Load config from disk, returning `Default::default()` on any error.
    pub fn load() -> Self {
        let path = Self::config_path();
        match std::fs::read_to_string(&path) {
            Ok(s) => serde_json::from_str(&s).unwrap_or_else(|e| {
                warn!("dock config parse error: {e}, using defaults");
                Self::default()
            }),
            Err(e) => {
                debug!("dock config not found ({e}), using defaults");
                Self::default()
            }
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// AppInfo
// ─────────────────────────────────────────────────────────────────────────────

/// Resolved metadata for an application identified by its `app_id`.
#[derive(Debug, Clone)]
pub struct AppInfo {
    /// Human-readable application name.
    pub name: String,
    /// Launch command (with `%`-field codes stripped).
    pub exec: String,
    /// Resolved path to the icon file, or `None` if not found.
    pub icon: Option<PathBuf>,
}

/// Resolves `AppInfo` for `app_id` by searching standard `.desktop` directories.
///
/// Falls back to a best-effort entry (name derived from `app_id`) if no
/// `.desktop` file is found.  Even in the fallback path, an icon matching
/// `app_id` is searched for directly.
pub fn resolve(app_id: &str) -> AppInfo {
    // Build filename candidates.
    let mut candidates: Vec<String> = vec![
        format!("{app_id}.desktop"),
        format!("{}.desktop", app_id.to_lowercase()),
    ];
    if let Some(leaf) = app_id.rsplit('.').next() {
        if leaf != app_id {
            candidates.push(format!("{leaf}.desktop"));
            candidates.push(format!("{}.desktop", leaf.to_lowercase()));
        }
    }
    for prefix in ["org.gnome.", "org.mozilla.", "org.kde."] {
        candidates.push(format!("{prefix}{app_id}.desktop"));
    }

    for dir in &search_dirs() {
        for candidate in &candidates {
            if let Some(info) = parse_file(&dir.join(candidate), app_id) {
                return info;
            }
        }
    }

    // Fallback — no .desktop file found.
    AppInfo {
        name: pretty_name(app_id),
        exec: app_id.to_string(),
        icon: resolve_icon(app_id).or_else(|| resolve_icon(&app_id.to_lowercase())),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Internals
// ─────────────────────────────────────────────────────────────────────────────

fn search_dirs() -> Vec<PathBuf> {
    let home = home_dir();
    let mut dirs = vec![
        home.join(".local/share/applications"),
        PathBuf::from("/usr/share/applications"),
        PathBuf::from("/usr/local/share/applications"),
        PathBuf::from("/var/lib/flatpak/exports/share/applications"),
        home.join(".local/share/flatpak/exports/share/applications"),
    ];
    if let Ok(xdg) = std::env::var("XDG_DATA_DIRS") {
        for part in xdg.split(':') {
            dirs.push(PathBuf::from(part).join("applications"));
        }
    }
    dirs
}

fn parse_file(path: &Path, app_id: &str) -> Option<AppInfo> {
    let content = std::fs::read_to_string(path).ok()?;
    let mut name: Option<String> = None;
    let mut exec: Option<String> = None;
    let mut icon_name: Option<String> = None;
    let mut in_entry = false;

    for line in content.lines() {
        let line = line.trim();
        if line == "[Desktop Entry]" {
            in_entry = true;
        } else if line.starts_with('[') {
            in_entry = false;
        } else if in_entry && !line.starts_with('#') {
            if let Some(v) = line.strip_prefix("Name=") {
                if name.is_none() {
                    name = Some(v.to_string());
                }
            } else if let Some(v) = line.strip_prefix("Exec=") {
                exec = Some(strip_exec_fields(v));
            } else if let Some(v) = line.strip_prefix("Icon=") {
                icon_name = Some(v.to_string());
            }
        }
    }

    let name = name.unwrap_or_else(|| pretty_name(app_id));
    let exec = exec.unwrap_or_else(|| app_id.to_string());
    let icon = icon_name.as_deref().and_then(resolve_icon);

    debug!("resolved .desktop for {app_id}: name={name:?}, exec={exec:?}, icon={icon:?}");
    Some(AppInfo { name, exec, icon })
}

/// Remove `%`-field substitution codes from an `Exec=` value.
pub fn strip_exec_fields(exec: &str) -> String {
    let mut out = String::with_capacity(exec.len());
    let mut chars = exec.chars();
    while let Some(c) = chars.next() {
        if c == '%' {
            chars.next(); // skip single-char field code
        } else {
            out.push(c);
        }
    }
    out.trim().to_string()
}

/// Searches standard icon theme directories for `icon_name`.
pub fn resolve_icon(icon_name: &str) -> Option<PathBuf> {
    if icon_name.starts_with('/') {
        let p = PathBuf::from(icon_name);
        return p.exists().then_some(p);
    }

    let home = home_dir();
    // Prefer large raster (PNG) first so the image crate can decode it directly.
    // SVG is listed last as a fallback; `load_icon` handles SVG→PNG fallback.
    let sizes = [
        "256x256/apps",
        "128x128/apps",
        "96x96/apps",
        "64x64/apps",
        "48x48/apps",
        "scalable/apps",
        "32x32/apps",
        "24x24/apps",
        "16x16/apps",
        "scalable/mimetypes",
        "16x16/mimetypes",
    ];
    let exts = ["png", "svg", "xpm"];

    let theme_roots: &[PathBuf] = &[
        home.join(".local/share/icons/hicolor"),
        PathBuf::from("/usr/share/icons/hicolor"),
        PathBuf::from("/usr/share/icons/Adwaita"),
        PathBuf::from("/usr/share/icons/breeze"),
    ];

    for root in theme_roots {
        for size in &sizes {
            let base = root.join(size);
            for ext in &exts {
                let p = base.join(format!("{icon_name}.{ext}"));
                if p.exists() {
                    return Some(p);
                }
            }
        }
    }

    // Pixmaps fallback.
    for ext in &exts {
        let p = PathBuf::from("/usr/share/pixmaps").join(format!("{icon_name}.{ext}"));
        if p.exists() {
            return Some(p);
        }
    }

    None
}

/// Generic icon to use when no app-specific icon resolved.
pub fn generic_app_icon() -> Option<PathBuf> {
    resolve_icon("application-x-executable")
        .or_else(|| resolve_icon("application-x-generic"))
}

/// Resolve a freedesktop icon name (e.g. "telegram", "discord",
/// "network-wireless-signal-good") to a `slint::Image` via the system
/// theme. Used for StatusNotifierItem entries which carry icon-name strings
/// rather than full paths. Returns `None` if the theme lacks the icon.
pub fn load_icon_by_name(name: &str) -> Option<slint::Image> {
    let path = resolve_icon(name)?;
    Some(load_icon(&path))
}

/// Load an icon from disk and return a Slint `Image`.  Falls back to a
/// default (empty) image if the path is missing or loading fails.
///
/// For SVG icons: rasterise at `DOCK_ICON_SIZE × DOCK_ICON_SIZE` using `resvg`
/// (via the `rasterise_svg` helper which maintains a per-process cache).
/// This path works correctly with Slint's `FemtoVG` backend which does NOT
/// decode SVG natively.
///
/// For raster icons (PNG/JPEG/etc.): the `image` crate is used directly.
pub fn load_icon(icon_path: &Path) -> slint::Image {
    let ext = icon_path.extension().and_then(|e| e.to_str()).unwrap_or("");

    if ext == "svg" {
        // Rasterise via resvg at the dock icon size.
        match rasterise_svg(icon_path, DOCK_ICON_SIZE) {
            Some((bytes, w, h)) => {
                let buf = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(
                    &bytes, w, h,
                );
                debug!("SVG icon {:?} rasterised at {}×{}", icon_path, w, h);
                return slint::Image::from_rgba8_premultiplied(buf);
            }
            None => {
                // resvg failed — look for a PNG sibling at a raster size.
                if let Some(icon_name) = icon_path.file_stem().and_then(|s| s.to_str()) {
                    // Prefer larger sizes first — Slint downscales smoothly
                    // but upscaling a 32×32 PNG to fill a 60 px slot looks
                    // pixelated. 256 → 128 → 64 → 48 → 32 fallback chain.
                    for size in &["256x256/apps", "128x128/apps", "96x96/apps",
                                  "64x64/apps", "48x48/apps", "32x32/apps"] {
                        for root in &[
                            PathBuf::from("/usr/share/icons/hicolor"),
                            PathBuf::from("/usr/share/icons/Adwaita"),
                        ] {
                            let png = root.join(size).join(format!("{icon_name}.png"));
                            if png.exists() {
                                if let Some(img) = load_raster(&png) {
                                    return img;
                                }
                            }
                        }
                    }
                }
                debug!("SVG icon {:?} could not be rasterised and no PNG fallback found", icon_path);
            }
        }
    } else if let Some(img) = load_raster(icon_path) {
        return img;
    }

    slint::Image::default()
}

/// Load a raster image (PNG/JPEG/etc.) using the `image` crate.
fn load_raster(icon_path: &Path) -> Option<slint::Image> {
    match image::open(icon_path) {
        Ok(img) => {
            let rgba = img.to_rgba8();
            let (w, h) = rgba.dimensions();
            let buf = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(
                rgba.as_raw(),
                w,
                h,
            );
            Some(slint::Image::from_rgba8(buf))
        }
        Err(e) => {
            warn!("failed to load icon {:?}: {}", icon_path, e);
            None
        }
    }
}

fn pretty_name(app_id: &str) -> String {
    let base = app_id.rsplit('.').next().unwrap_or(app_id);
    base.split('-')
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn home_dir() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/root"))
}
