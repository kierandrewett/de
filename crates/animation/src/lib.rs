//! Zero-dependency animation engine for a Wayland compositor.
//!
//! Supports easing (fixed-duration Bézier interpolation) and spring
//! (physically-modelled damped harmonic oscillator) animations.

#![deny(missing_docs)]

pub mod animated;
pub mod curves;
pub mod easing;
pub mod presets;
pub mod spring;
