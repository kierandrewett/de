//! Shared design-token crate for the custom Wayland DE.
//!
//! Every visual component (compositor decorations, panel, dock, launcher,
//! notifications) imports this crate for consistent styling.

#![deny(missing_docs)]

pub mod animation;
pub mod color;
pub mod spacing;
pub mod typography;
pub mod window;

pub use animation::WindowAnimationPresets;
pub use color::ColorPalette;
pub use spacing::Spacing;
pub use typography::Typography;
pub use window::{
    ShadowLayer, WindowBorderStyle, WindowBorderTheme, WindowShadowTheme, WindowTheme,
};

use serde::{Deserialize, Serialize};

/// Light or dark visual mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ThemeMode {
    /// Light mode.
    Light,
    /// Dark mode.
    Dark,
}

/// Root theme structure — the single source of truth for all visual tokens.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Theme {
    /// Light or dark mode selection.
    pub mode: ThemeMode,
    /// Colour palette.
    pub colors: ColorPalette,
    /// Typography scale.
    pub typography: Typography,
    /// Spacing scale.
    pub spacing: Spacing,
    /// Window decoration tokens.
    pub windows: WindowTheme,
    /// Window animation presets — semantic tokens, mapped to `animation`
    /// crate primitives at use time (see `theme::animation` module docs).
    pub animations: WindowAnimationPresets,
}

impl Default for Theme {
    fn default() -> Self {
        Self::dark()
    }
}

impl Theme {
    /// Dark mode theme with blue accent and macOS-inspired defaults.
    pub fn dark() -> Self {
        Self {
            mode: ThemeMode::Dark,
            colors: ColorPalette::dark(),
            typography: Typography::default(),
            spacing: Spacing::default(),
            windows: WindowTheme::default(),
            animations: WindowAnimationPresets::default(),
        }
    }

    /// Light mode theme with blue accent and macOS-inspired defaults.
    pub fn light() -> Self {
        Self {
            mode: ThemeMode::Light,
            colors: ColorPalette::light(),
            typography: Typography::default(),
            spacing: Spacing::default(),
            windows: WindowTheme::default(),
            animations: WindowAnimationPresets::default(),
        }
    }

    /// Override the accent colour, returning the modified theme.
    pub fn with_accent(mut self, accent: [f32; 4]) -> Self {
        self.colors.accent = accent;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_dark() {
        assert_eq!(Theme::default().mode, ThemeMode::Dark);
    }

    #[test]
    fn light_mode() {
        assert_eq!(Theme::light().mode, ThemeMode::Light);
    }

    #[test]
    fn with_accent_overrides_colour() {
        let accent = [1.0, 0.0, 0.0, 1.0];
        let theme = Theme::dark().with_accent(accent);
        assert_eq!(theme.colors.accent, accent);
    }

    #[test]
    fn roundtrip_serde_json() {
        let theme = Theme::dark();
        let json = serde_json::to_string(&theme).expect("serialise");
        let back: Theme = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(back.mode, ThemeMode::Dark);
    }
}
