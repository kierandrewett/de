//! Dock configuration — pinned apps, icon size, and visual settings.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

fn default_icon_size() -> u32 {
    48
}
fn default_margin() -> u32 {
    8
}
fn default_icon_padding() -> u32 {
    6
}

/// A single pinned application entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PinnedApp {
    /// Wayland `app_id` or `.desktop` file stem.
    pub app_id: String,
}

/// User-configurable dock settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Icon size in logical pixels (default: 48).
    #[serde(default = "default_icon_size")]
    pub icon_size: u32,

    /// Gap between the dock surface and the screen edge, in logical pixels.
    #[serde(default = "default_margin")]
    pub margin: u32,

    /// Inner padding around each icon, in logical pixels.
    #[serde(default = "default_icon_padding")]
    pub icon_padding: u32,

    /// Ordered list of pinned applications.
    #[serde(default)]
    pub pinned: Vec<PinnedApp>,

    /// Automatically hide when the pointer is away from the bottom edge.
    #[serde(default)]
    pub auto_hide: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            icon_size: 48,
            margin: 8,
            icon_padding: 6,
            // Default pins. Each entry's icon is resolved first by
            // looking up `<app_id>.desktop`, then by trying the literal
            // string as an icon-theme name. Fedora ships
            // `org.mozilla.firefox.desktop` (not `firefox.desktop`)
            // and no `org.gnome.Terminal` at all, so those are the
            // canonical names to use across distros.
            pinned: vec![
                PinnedApp { app_id: "org.gnome.Nautilus".into() },
                PinnedApp { app_id: "org.mozilla.firefox".into() },
                PinnedApp { app_id: "kitty".into() },
            ],
            auto_hide: false,
        }
    }
}

impl Config {
    /// Path to the config file: `~/.config/myDE/dock.json`.
    pub fn path() -> PathBuf {
        let home = std::env::var("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("/root"));
        home.join(".config").join("myDE").join("dock.json")
    }

    /// Loads config from disk, returning `Default::default()` on any error.
    pub fn load() -> Self {
        let path = Self::path();
        match std::fs::read_to_string(&path) {
            Ok(s) => serde_json::from_str(&s).unwrap_or_else(|e| {
                tracing::warn!("dock config parse error: {e}, using defaults");
                Self::default()
            }),
            Err(e) => {
                tracing::debug!("dock config not found ({e}), using defaults");
                Self::default()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_has_pinned_apps() {
        let cfg = Config::default();
        assert!(!cfg.pinned.is_empty());
    }

    #[test]
    fn default_icon_size_is_48() {
        assert_eq!(Config::default().icon_size, 48);
    }

    #[test]
    fn config_path_ends_with_dock_json() {
        let p = Config::path();
        assert_eq!(p.file_name().and_then(|n| n.to_str()), Some("dock.json"));
    }

    #[test]
    fn round_trip_serialization() {
        let cfg = Config::default();
        let json = serde_json::to_string(&cfg).expect("serialize");
        let decoded: Config = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(decoded.icon_size, cfg.icon_size);
        assert_eq!(decoded.pinned.len(), cfg.pinned.len());
    }

    #[test]
    fn missing_fields_use_defaults() {
        let json = r#"{"pinned":[]}"#;
        let cfg: Config = serde_json::from_str(json).expect("deserialize");
        assert_eq!(cfg.icon_size, 48);
        assert_eq!(cfg.margin, 8);
    }
}
