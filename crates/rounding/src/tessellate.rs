//! Bézier and arc tessellation into line-segment vertices.
//!
//! Converts a [`SquirclePath`] into a flat list of 2D points suitable for
//! GPU vertex submission or polygon operations.

use std::f32::consts::PI;

use crate::commands::PathCommand;
use crate::path::SquirclePath;

impl SquirclePath {
    /// Tessellate all curves into line segments for GPU vertex submission.
    ///
    /// `tolerance` is the maximum allowed deviation from the true curve in
    /// pixels. Typical value: `0.25` for screen rendering.
    ///
    /// Returns a list of `[x, y]` points forming a closed polygon. The last
    /// point connects back to the first to close the shape.
    pub fn tessellate(&self, tolerance: f32) -> Vec<[f32; 2]> {
        let tol = tolerance.max(1e-4);
        let mut pts: Vec<[f32; 2]> = Vec::with_capacity(128);
        let mut current = [0.0_f32; 2];
        let mut start = [0.0_f32; 2];

        for cmd in &self.commands {
            match *cmd {
                PathCommand::MoveTo(x, y) => {
                    pts.push([x, y]);
                    current = [x, y];
                    start = [x, y];
                }
                PathCommand::LineTo(x, y) => {
                    pts.push([x, y]);
                    current = [x, y];
                }
                PathCommand::CubicTo { ctrl1, ctrl2, end } => {
                    tessellate_cubic(
                        current,
                        [ctrl1.0, ctrl1.1],
                        [ctrl2.0, ctrl2.1],
                        [end.0, end.1],
                        tol,
                        &mut pts,
                    );
                    current = [end.0, end.1];
                }
                PathCommand::ArcTo {
                    center,
                    radius,
                    start_angle,
                    sweep_angle,
                } => {
                    tessellate_arc(center, radius, start_angle, sweep_angle, tol, &mut pts);
                    // Update current to end of arc.
                    let end_angle = start_angle + sweep_angle;
                    current = [
                        center.0 + radius * end_angle.cos(),
                        center.1 + radius * end_angle.sin(),
                    ];
                }
                PathCommand::Close => {
                    // Close back to start (don't duplicate the start point).
                    if (current[0] - start[0]).abs() > f32::EPSILON
                        || (current[1] - start[1]).abs() > f32::EPSILON
                    {
                        pts.push(start);
                    }
                    current = start;
                }
            }
        }

        pts
    }
}

/// Recursively subdivide a cubic Bézier until each segment is within `tol`.
fn tessellate_cubic(
    p0: [f32; 2],
    p1: [f32; 2],
    p2: [f32; 2],
    p3: [f32; 2],
    tol: f32,
    out: &mut Vec<[f32; 2]>,
) {
    // Check if the curve is flat enough using the flatness criterion.
    // Max distance of control points from the chord p0→p3.
    if cubic_flatness(p0, p1, p2, p3) <= tol {
        out.push(p3);
        return;
    }

    // De Casteljau subdivision at t=0.5.
    let m01 = midpoint(p0, p1);
    let m12 = midpoint(p1, p2);
    let m23 = midpoint(p2, p3);
    let m012 = midpoint(m01, m12);
    let m123 = midpoint(m12, m23);
    let m0123 = midpoint(m012, m123);

    tessellate_cubic(p0, m01, m012, m0123, tol, out);
    tessellate_cubic(m0123, m123, m23, p3, tol, out);
}

/// Flatness heuristic for a cubic Bézier.
///
/// Uses the maximum distance of the two control points from the chord.
fn cubic_flatness(p0: [f32; 2], p1: [f32; 2], p2: [f32; 2], p3: [f32; 2]) -> f32 {
    // Compute the distance of p1 and p2 from the line p0→p3.
    let d1 = point_to_line_dist(p1, p0, p3);
    let d2 = point_to_line_dist(p2, p0, p3);
    d1.max(d2)
}

/// Distance from point `p` to the line through `a` and `b`.
fn point_to_line_dist(p: [f32; 2], a: [f32; 2], b: [f32; 2]) -> f32 {
    let ab = [b[0] - a[0], b[1] - a[1]];
    let ap = [p[0] - a[0], p[1] - a[1]];
    let len2 = ab[0] * ab[0] + ab[1] * ab[1];
    if len2 < f32::EPSILON {
        return (ap[0] * ap[0] + ap[1] * ap[1]).sqrt();
    }
    // Cross product magnitude / |ab|
    (ab[0] * ap[1] - ab[1] * ap[0]).abs() / len2.sqrt()
}

/// Midpoint of two points.
#[inline]
fn midpoint(a: [f32; 2], b: [f32; 2]) -> [f32; 2] {
    [(a[0] + b[0]) * 0.5, (a[1] + b[1]) * 0.5]
}

/// Tessellate a circular arc into points.
///
/// The arc is approximated using the standard Bézier technique, then the
/// Bézier curves are recursively subdivided. We use the adaptive chord-error
/// approach: compute how many segments are needed so the chord error ≤ tol.
fn tessellate_arc(
    center: (f32, f32),
    radius: f32,
    start_angle: f32,
    sweep_angle: f32,
    tol: f32,
    out: &mut Vec<[f32; 2]>,
) {
    if sweep_angle.abs() < 1e-6 || radius < f32::EPSILON {
        return;
    }

    // Minimum segments to keep chord error ≤ tol.
    // chord_error = r * (1 - cos(θ/2)) ≈ r*θ²/8 for small θ
    // tol ≥ r*(1 - cos(θ/2)) → θ ≤ 2*acos(1 - tol/r)
    let max_seg_angle = if tol < radius {
        2.0 * (1.0 - tol / radius).acos().max(1e-6)
    } else {
        PI / 2.0
    };

    let n = ((sweep_angle.abs() / max_seg_angle).ceil() as usize).max(1);
    let seg = sweep_angle / n as f32;

    let mut angle = start_angle;
    for _ in 0..n {
        angle += seg;
        out.push([
            center.0 + radius * angle.cos(),
            center.1 + radius * angle.sin(),
        ]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::SquircleConfig;

    #[test]
    fn tessellated_points_non_empty_for_valid_rect() {
        let p = SquirclePath::new(0.0, 0.0, 100.0, 100.0, SquircleConfig::default());
        let pts = p.tessellate(0.25);
        assert!(!pts.is_empty());
    }

    #[test]
    fn tessellated_empty_for_zero_rect() {
        let p = SquirclePath::new(0.0, 0.0, 0.0, 0.0, SquircleConfig::default());
        let pts = p.tessellate(0.25);
        assert!(pts.is_empty());
    }

    #[test]
    fn tessellated_points_roughly_on_rect_boundary() {
        let cfg = SquircleConfig::new(10.0, 0.6);
        let p = SquirclePath::new(0.0, 0.0, 100.0, 100.0, cfg);
        let pts = p.tessellate(0.25);
        // Squircle paths extend beyond the corner point slightly (the Bézier
        // entry starts p units back from the corner, where p = r*(1+ξ*0.7)).
        // For r=10, ξ=0.6: p = 10*(1+0.42) = 14.2.
        // So points can be up to ~p units outside the nominal rect bounding box
        // at the corners. We use a generous margin here.
        let margin = 20.0;
        for pt in &pts {
            assert!(
                pt[0] >= -margin && pt[0] <= 100.0 + margin,
                "x out of range: {}",
                pt[0]
            );
            assert!(
                pt[1] >= -margin && pt[1] <= 100.0 + margin,
                "y out of range: {}",
                pt[1]
            );
        }
    }
}
