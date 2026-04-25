//! SVG path data export for squircle paths.

use std::f32::consts::PI;

use crate::commands::PathCommand;
use crate::path::SquirclePath;

impl SquirclePath {
    /// Convert this path to an SVG path data string.
    ///
    /// Arc segments are approximated with cubic Bézier curves since SVG's `A`
    /// command uses a different parameterisation. The approximation uses the
    /// standard Bézier-arc formula accurate to within 0.03%.
    pub fn to_svg_path_data(&self) -> String {
        let mut out = String::with_capacity(self.commands.len() * 32);
        for cmd in &self.commands {
            match *cmd {
                PathCommand::MoveTo(x, y) => {
                    out.push_str(&format!("M {x:.4} {y:.4} "));
                }
                PathCommand::LineTo(x, y) => {
                    out.push_str(&format!("L {x:.4} {y:.4} "));
                }
                PathCommand::CubicTo { ctrl1, ctrl2, end } => {
                    out.push_str(&format!(
                        "C {:.4} {:.4} {:.4} {:.4} {:.4} {:.4} ",
                        ctrl1.0, ctrl1.1, ctrl2.0, ctrl2.1, end.0, end.1,
                    ));
                }
                PathCommand::ArcTo {
                    center,
                    radius,
                    start_angle,
                    sweep_angle,
                } => {
                    // Approximate arc with Bézier segments (max 90° each).
                    arc_to_beziers(center, radius, start_angle, sweep_angle, &mut out);
                }
                PathCommand::Close => {
                    out.push_str("Z ");
                }
            }
        }
        out.trim_end().to_string()
    }
}

/// Emit cubic Bézier approximations for a circular arc into `out`.
///
/// Splits the arc into segments of at most 90° and uses the standard
/// `k = 4/3 * tan(θ/4)` control-point formula.
fn arc_to_beziers(
    center: (f32, f32),
    radius: f32,
    start_angle: f32,
    sweep_angle: f32,
    out: &mut String,
) {
    if sweep_angle.abs() < 1e-6 || radius < f32::EPSILON {
        return;
    }

    // Number of segments: at most 90° each.
    let max_segment = PI / 2.0;
    let n = ((sweep_angle.abs() / max_segment).ceil() as usize).max(1);
    let seg_sweep = sweep_angle / n as f32;

    let mut angle = start_angle;
    for _ in 0..n {
        let (sx, sy) = (
            center.0 + radius * angle.cos(),
            center.1 + radius * angle.sin(),
        );
        let next_angle = angle + seg_sweep;
        let (ex, ey) = (
            center.0 + radius * next_angle.cos(),
            center.1 + radius * next_angle.sin(),
        );

        // Control point factor for this segment angle.
        let k = (4.0 / 3.0) * (seg_sweep / 4.0).tan();

        // Tangent at start: (-sin, cos) * radius * k
        let c1x = sx - radius * angle.sin() * k;
        let c1y = sy + radius * angle.cos() * k;
        // Tangent at end (reversed): (sin, -cos) * radius * k
        let c2x = ex + radius * next_angle.sin() * k;
        let c2y = ey - radius * next_angle.cos() * k;

        out.push_str(&format!(
            "C {c1x:.4} {c1y:.4} {c2x:.4} {c2y:.4} {ex:.4} {ey:.4} "
        ));
        angle = next_angle;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::SquircleConfig;

    #[test]
    fn svg_output_starts_with_m() {
        let p = SquirclePath::new(0.0, 0.0, 100.0, 100.0, SquircleConfig::default());
        let svg = p.to_svg_path_data();
        assert!(svg.starts_with('M'), "SVG path should start with M, got: {svg}");
    }

    #[test]
    fn svg_output_ends_with_z() {
        let p = SquirclePath::new(0.0, 0.0, 100.0, 100.0, SquircleConfig::default());
        let svg = p.to_svg_path_data();
        assert!(svg.ends_with('Z'), "SVG path should end with Z, got: {svg}");
    }

    #[test]
    fn svg_empty_for_zero_rect() {
        let p = SquirclePath::new(0.0, 0.0, 0.0, 0.0, SquircleConfig::default());
        assert_eq!(p.to_svg_path_data(), "");
    }
}
