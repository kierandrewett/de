# SUBAGENT: Shared Theme Tokens
# Crate: `crates/theme`

> **Read `ARCHITECTURE.md` in the repo root before starting.** It explains why we use Smithay (not wlroots), how the compositor rendering pipeline works, the full Wayland protocol reference, smithay API patterns (delegate pattern, GlesRenderer, calloop), and the winit dev backend.
# Branch: `feat/theme`

You are building the shared theme/design-token crate for a custom Wayland DE. Every visual component (compositor decorations, panel, dock, launcher, notifications) imports this crate for consistent styling.

## What to build

```rust
pub struct Theme {
    pub mode: ThemeMode,
    pub colors: ColorPalette,
    pub typography: Typography,
    pub spacing: Spacing,
    pub windows: WindowTheme,
    pub animations: animation::WindowAnimationPresets, // re-export from animation crate
}

pub enum ThemeMode { Light, Dark }

pub struct ColorPalette {
    pub accent: [f32; 4],
    pub background: [f32; 4],
    pub surface: [f32; 4],
    pub surface_elevated: [f32; 4],
    pub on_surface: [f32; 4],       // text on surface
    pub on_surface_dim: [f32; 4],   // secondary text
    pub border: [f32; 4],
    pub shadow: [f32; 4],
    pub destructive: [f32; 4],      // red for close buttons etc
    pub success: [f32; 4],
    pub warning: [f32; 4],
}

pub struct Typography {
    pub font_family: String,
    pub font_size_small: f32,    // 11
    pub font_size_normal: f32,   // 13
    pub font_size_large: f32,    // 16
    pub font_size_title: f32,    // 20
    pub font_weight_normal: u16, // 400
    pub font_weight_bold: u16,   // 600
}

pub struct Spacing {
    pub xs: f32,   // 4
    pub sm: f32,   // 8
    pub md: f32,   // 12
    pub lg: f32,   // 16
    pub xl: f32,   // 24
    pub xxl: f32,  // 32
}

pub struct WindowTheme {
    pub corner_radius: f32,        // default 10.0
    pub corner_smoothing: f32,     // default 0.6 (macOS)
    pub border_width: f32,         // default 1.0
    pub title_bar_height: f32,     // default 38.0
    pub padding: f32,              // inner content padding, default 0.0
    pub shadow: WindowShadowTheme,
    pub border: WindowBorderTheme,
}

pub struct WindowBorderTheme {
    pub light_active: WindowBorderStyle,
    pub light_inactive: WindowBorderStyle,
    pub dark_active: WindowBorderStyle,
    pub dark_inactive: WindowBorderStyle,
}

/// macOS-style multi-layer border
pub struct WindowBorderStyle {
    pub outer_stroke: [f32; 4],
    pub inner_highlight_top: [f32; 4],
    pub inner_highlight_side: [f32; 4],
    pub shadow_layers: Vec<ShadowLayer>,
}

pub struct ShadowLayer {
    pub offset_x: f32, pub offset_y: f32,
    pub blur_radius: f32, pub spread: f32,
    pub color: [f32; 4],
}

impl Default for Theme {
    fn default() -> Self { /* dark mode, blue accent, macOS-inspired values */ }
}

impl Theme {
    pub fn light() -> Self;
    pub fn dark() -> Self;
    pub fn with_accent(self, accent: [f32; 4]) -> Self;
}
```

Provide sensible defaults. For window border/shadow values, use the EXACT values from `WINDOW_SPEC.md` in the repo root — it contains pixel-perfect specs extracted from the Figma designs + macOS Sequoia analysis. The key values:

**Light active:** outer `rgba(0,0,0,0.12)`, inner-top `rgba(255,255,255,0.5)`, inner-side `rgba(255,255,255,0.15)`, shadows: `0 2px 8px rgba(0,0,0,0.1)` + `0 8px 24px rgba(0,0,0,0.08)`
**Dark active:** outer `rgba(255,255,255,0.08)`, inner-top `rgba(255,255,255,0.06)`, inner-side `rgba(0,0,0,0)`, shadows: `0 2px 12px rgba(0,0,0,0.3)` + `0 12px 40px rgba(0,0,0,0.25)`
Inactive variants: reduce shadow by ~50%, border opacity by ~40%.

## Cargo.toml
```toml
[package]
name = "theme"
version = "0.1.0"
edition = "2021"
[dependencies]
serde = { workspace = true }
```

No dependency on `animation` crate — the presets are duplicated here as data, the animation crate owns the runtime. Keep this crate minimal and dependency-free aside from serde.
