//! Internal math helpers for squircle path generation.
#![allow(dead_code)]

use std::f32::consts::{FRAC_PI_2, PI};

/// The standard Bézier approximation constant for a quarter-circle arc.
///
/// A cubic Bézier with control points at distance `r * KAPPA` from the arc
/// endpoints approximates a 90° circular arc with < 0.027% maximum error.
pub(crate) const KAPPA: f32 = 0.552_284_8;

/// Linear interpolation between `a` and `b` by factor `t ∈ [0, 1]`.
#[inline]
pub(crate) fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// Rotate a 2D point `(x, y)` by `angle` radians around the origin.
#[inline]
pub(crate) fn rotate(x: f32, y: f32, angle: f32) -> (f32, f32) {
    let (sin, cos) = angle.sin_cos();
    (x * cos - y * sin, x * sin + y * cos)
}

/// Translate a point by `(dx, dy)`.
#[inline]
pub(crate) fn translate(p: (f32, f32), dx: f32, dy: f32) -> (f32, f32) {
    (p.0 + dx, p.1 + dy)
}

/// Angle constants for the four corners (clockwise from top-left).
///
/// Each entry is `(arc_start_angle, corner_offset_from_rect_origin)` and the
/// rotation applied to bring local corner coordinates into world space.
///
/// Corners in order: TL, TR, BR, BL.
/// The local frame has the corner at the origin, the straight edge going right
/// (positive X) and down (positive Y) into the rect interior.
pub(crate) const CORNER_ROTATIONS: [f32; 4] = [
    PI,          // TL: local +X goes left along top edge
    -FRAC_PI_2,  // TR: local +X goes down along right edge
    0.0,         // BR: local +X goes right along bottom edge
    FRAC_PI_2,   // BL: local +X goes up along left edge
];

/// Compute the world-space corner origin for each of the four rect corners.
///
/// Order: TL, TR, BR, BL.
#[inline]
pub(crate) fn corner_origins(x: f32, y: f32, w: f32, h: f32) -> [(f32, f32); 4] {
    [
        (x, y),           // TL
        (x + w, y),       // TR
        (x + w, y + h),   // BR
        (x, y + h),       // BL
    ]
}

/// Standard arc start angles for each corner's circular arc segment (clockwise).
///
/// These are the angles at which the arc begins for the central portion.
/// Order: TL, TR, BR, BL.
pub(crate) const ARC_START_ANGLES: [f32; 4] = [
    PI,          // TL arc starts pointing left (180°)
    -FRAC_PI_2,  // TR arc starts pointing up (270° / -90°)
    0.0,         // BR arc starts pointing right (0°)
    FRAC_PI_2,   // BL arc starts pointing down (90°)
];

/// Clamp a corner radius so it never exceeds half the available dimension.
#[inline]
pub(crate) fn clamp_radius(radius: f32, max_half: f32) -> f32 {
    radius.min(max_half).max(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lerp_endpoints() {
        assert_eq!(lerp(0.0, 10.0, 0.0), 0.0);
        assert_eq!(lerp(0.0, 10.0, 1.0), 10.0);
        assert!((lerp(0.0, 10.0, 0.5) - 5.0).abs() < 1e-6);
    }

    #[test]
    fn rotate_zero_angle() {
        let (rx, ry) = rotate(3.0, 4.0, 0.0);
        assert!((rx - 3.0).abs() < 1e-5);
        assert!((ry - 4.0).abs() < 1e-5);
    }

    #[test]
    fn clamp_radius_limits() {
        assert_eq!(clamp_radius(100.0, 25.0), 25.0);
        assert_eq!(clamp_radius(10.0, 25.0), 10.0);
        assert_eq!(clamp_radius(-1.0, 25.0), 0.0);
    }
}
