//! Portal service configuration, loaded from `$XDG_CONFIG_HOME/myDE/settings.json`.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Settings that the portal service advertises to D-Bus clients.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Color scheme preference: 0 = no preference, 1 = dark, 2 = light.
    pub color_scheme: u32,
    /// Accent color as `[r, g, b]` in `[0.0, 1.0]`.
    pub accent_color: [f64; 3],
    /// Contrast level: 0 = standard, 1 = high.
    pub contrast: u32,
    /// GTK theme name.
    pub gtk_theme: String,
    /// Icon theme name.
    pub icon_theme: String,
    /// Cursor theme name.
    pub cursor_theme: String,
    /// Cursor size in pixels.
    pub cursor_size: u32,
    /// Font description string (e.g. `"Inter 11"`).
    pub font_name: String,
    /// Text scaling factor (1.0 = 100 %).
    pub text_scaling_factor: f64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            color_scheme: 1,
            accent_color: [0.0, 0.478, 1.0],
            contrast: 0,
            gtk_theme: "myDE-dark".into(),
            icon_theme: "Papirus-Dark".into(),
            cursor_theme: "Bibata-Modern-Classic".into(),
            cursor_size: 24,
            font_name: "Inter 11".into(),
            text_scaling_factor: 1.0,
        }
    }
}

impl Config {
    /// Load from the standard path, falling back to defaults on any error.
    pub fn load() -> Self {
        let path = Self::path();
        match std::fs::read_to_string(&path) {
            Ok(s) => serde_json::from_str(&s).unwrap_or_else(|e| {
                tracing::warn!(path = %path.display(), error = %e, "invalid settings JSON, using defaults");
                Self::default()
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                tracing::debug!(path = %path.display(), "settings file not found, using defaults");
                Self::default()
            }
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "cannot read settings file, using defaults");
                Self::default()
            }
        }
    }

    /// Returns the expected path to the settings JSON file.
    pub fn path() -> PathBuf {
        let base = std::env::var("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/root".into()))
                    .join(".config")
            });
        base.join("myDE").join("settings.json")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_dark_scheme() {
        let cfg = Config::default();
        assert_eq!(cfg.color_scheme, 1);
    }

    #[test]
    fn round_trip_json() {
        let cfg = Config::default();
        let json = serde_json::to_string(&cfg).expect("serialize");
        let back: Config = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.color_scheme, cfg.color_scheme);
        assert_eq!(back.gtk_theme, cfg.gtk_theme);
    }

    #[test]
    fn path_ends_with_settings_json() {
        let p = Config::path();
        assert!(p.to_string_lossy().ends_with("settings.json"));
    }
}
