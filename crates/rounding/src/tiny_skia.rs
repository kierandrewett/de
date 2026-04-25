//! Conversion from [`SquirclePath`] to a [`tiny_skia::Path`].
//!
//! This module is only available with the `tiny-skia` feature.

use std::f32::consts::PI;

use tiny_skia::{Path, PathBuilder};

use crate::commands::PathCommand;
use crate::path::SquirclePath;

impl SquirclePath {
    /// Convert this squircle path to a [`tiny_skia::Path`] for CPU rasterisation.
    ///
    /// Arc segments are converted to cubic Bézier approximations since
    /// `tiny_skia` does not have a native arc primitive.
    ///
    /// Returns `None` only if the path is empty (zero-size rect).
    pub fn to_tiny_skia_path(&self) -> Option<Path> {
        if self.commands.is_empty() {
            return None;
        }

        let mut pb = PathBuilder::new();

        for cmd in &self.commands {
            match *cmd {
                PathCommand::MoveTo(x, y) => {
                    pb.move_to(x, y);
                }
                PathCommand::LineTo(x, y) => {
                    pb.line_to(x, y);
                }
                PathCommand::CubicTo { ctrl1, ctrl2, end } => {
                    pb.cubic_to(ctrl1.0, ctrl1.1, ctrl2.0, ctrl2.1, end.0, end.1);
                }
                PathCommand::ArcTo {
                    center,
                    radius,
                    start_angle,
                    sweep_angle,
                } => {
                    arc_to_tiny_skia(center, radius, start_angle, sweep_angle, &mut pb);
                }
                PathCommand::Close => {
                    pb.close();
                }
            }
        }

        pb.finish()
    }
}

/// Approximate a circular arc as cubic Bézier curves in the given `PathBuilder`.
fn arc_to_tiny_skia(
    center: (f32, f32),
    radius: f32,
    start_angle: f32,
    sweep_angle: f32,
    pb: &mut PathBuilder,
) {
    if sweep_angle.abs() < 1e-6 || radius < f32::EPSILON {
        return;
    }

    let max_segment = PI / 2.0;
    let n = ((sweep_angle.abs() / max_segment).ceil() as usize).max(1);
    let seg_sweep = sweep_angle / n as f32;

    let mut angle = start_angle;
    for _ in 0..n {
        let next_angle = angle + seg_sweep;

        let sx = center.0 + radius * angle.cos();
        let sy = center.1 + radius * angle.sin();
        let ex = center.0 + radius * next_angle.cos();
        let ey = center.1 + radius * next_angle.sin();

        let k = (4.0 / 3.0) * (seg_sweep / 4.0).tan();

        let c1x = sx - radius * angle.sin() * k;
        let c1y = sy + radius * angle.cos() * k;
        let c2x = ex + radius * next_angle.sin() * k;
        let c2y = ey - radius * next_angle.cos() * k;

        pb.cubic_to(c1x, c1y, c2x, c2y, ex, ey);
        angle = next_angle;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::SquircleConfig;

    #[test]
    fn converts_to_tiny_skia_path() {
        let p = SquirclePath::new(0.0, 0.0, 100.0, 100.0, SquircleConfig::default());
        let ts = p.to_tiny_skia_path();
        assert!(ts.is_some(), "Should produce a valid tiny_skia Path");
    }

    #[test]
    fn zero_rect_returns_none() {
        let p = SquirclePath::new(0.0, 0.0, 0.0, 0.0, SquircleConfig::default());
        let ts = p.to_tiny_skia_path();
        assert!(ts.is_none(), "Zero-size rect should return None");
    }
}
