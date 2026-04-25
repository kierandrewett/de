//! # rounding
//!
//! A Rust crate for generating continuous-curvature rounded rectangle paths
//! matching macOS/iOS corner rounding ("squircle").
//!
//! Unlike standard `border-radius` (which uses circular arcs), this crate
//! generates cubic Bézier curves that ramp curvature smoothly from zero on the
//! straight edge to `1/r` at the arc — a G2-continuous transition that looks
//! organic and premium.
//!
//! ## Quick start
//!
//! ```rust
//! use rounding::{SquircleConfig, SquirclePath};
//!
//! // macOS-style corner rounding (smoothing = 0.6)
//! let config = SquircleConfig::default(); // radius=10, smoothing=0.6
//! let path = SquirclePath::new(0.0, 0.0, 200.0, 100.0, config);
//!
//! // Export as SVG path data for visual inspection
//! println!("{}", path.to_svg_path_data());
//!
//! // Tessellate for GPU rendering
//! let vertices = path.tessellate(0.25);
//!
//! // Generate WGSL SDF shader
//! let wgsl = SquirclePath::sdf_wgsl(&config);
//! ```
//!
//! ## Smoothing parameter
//!
//! The `smoothing` field in [`SquircleConfig`] controls the Bézier proportion:
//!
//! | Value | Effect |
//! |-------|--------|
//! | `0.0` | Standard circular arc (CSS `border-radius`) |
//! | `0.6` | Apple macOS/iOS default |
//! | `1.0` | Maximum smoothness — no arc segment |
//!
//! ## Features
//!
//! - `tiny-skia` — enables [`SquirclePath::to_tiny_skia_path`] for CPU rasterisation.

#![deny(missing_docs)]

pub mod commands;
pub mod config;
pub(crate) mod math;
pub mod path;
pub(crate) mod sdf;
pub(crate) mod svg;
pub(crate) mod tessellate;

#[cfg(feature = "tiny-skia")]
pub(crate) mod tiny_skia;

pub use commands::PathCommand;
pub use config::SquircleConfig;
pub use path::SquirclePath;
