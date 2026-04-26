//! `.desktop` file parsing, icon resolution, and dock config.
//!
//! Adapted from `crates/shell-dock/src/desktop.rs` and
//! `crates/shell-dock/src/config.rs`.  No external crate dependency on
//! `freedesktop-desktop-entry` — we parse the INI format ourselves to
//! avoid adding a crate that may not be in the workspace lock file.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

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
        Self {
            pinned: vec![
                PinnedApp { app_id: "org.gnome.Nautilus".into() },
                PinnedApp { app_id: "org.mozilla.firefox".into() },
                PinnedApp { app_id: "kitty".into() },
                PinnedApp { app_id: "code".into() },
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

/// Load an icon from disk and return a Slint `Image`.  Falls back to a
/// default (empty) image if the path is missing or loading fails.
///
/// For SVG icons: first tries `slint::Image::load_from_path` (works if the
/// Slint build includes the resvg backend); if that fails, looks for a sibling
/// PNG at the same icon-theme path.  For raster icons the `image` crate is
/// used directly.
pub fn load_icon(icon_path: &Path) -> slint::Image {
    let ext = icon_path.extension().and_then(|e| e.to_str()).unwrap_or("");

    if ext == "svg" {
        // Attempt Slint's own SVG loader (requires resvg support in the build).
        match slint::Image::load_from_path(icon_path) {
            Ok(img) => return img,
            Err(_) => {
                // Slint SVG failed — look for a PNG sibling at a raster size.
                if let Some(icon_name) = icon_path.file_stem().and_then(|s| s.to_str()) {
                    // Try common raster sizes as fallback.
                    for size in &["48x48/apps", "64x64/apps", "256x256/apps", "32x32/apps"] {
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
                debug!("SVG icon {:?} could not be loaded by Slint and no PNG fallback found", icon_path);
            }
        }
    } else {
        if let Some(img) = load_raster(icon_path) {
            return img;
        }
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
