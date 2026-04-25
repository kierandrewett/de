//! Path command definitions for squircle paths.

/// A single drawing command in a squircle path.
///
/// Commands are emitted in clockwise order starting from the top-left corner's
/// entry point on the top edge.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PathCommand {
    /// Move to a point without drawing.
    MoveTo(f32, f32),
    /// Draw a straight line to a point.
    LineTo(f32, f32),
    /// Draw a cubic Bézier curve.
    CubicTo {
        /// First control point.
        ctrl1: (f32, f32),
        /// Second control point.
        ctrl2: (f32, f32),
        /// End point of the curve.
        end: (f32, f32),
    },
    /// Draw a circular arc segment.
    ///
    /// The arc is defined by its center, radius, start angle (radians), and
    /// sweep angle (radians, positive = clockwise).
    ArcTo {
        /// Center of the arc's circle.
        center: (f32, f32),
        /// Radius of the arc.
        radius: f32,
        /// Start angle in radians (0 = positive X axis, clockwise positive).
        start_angle: f32,
        /// Sweep angle in radians (positive = clockwise).
        sweep_angle: f32,
    },
    /// Close the current subpath by drawing a line back to the start.
    Close,
}
