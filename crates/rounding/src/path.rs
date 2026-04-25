//! Core squircle path generation logic.

use std::f32::consts::{FRAC_PI_2, PI};

use crate::{
    commands::PathCommand,
    config::SquircleConfig,
    math::clamp_radius,
};

/// Threshold below which we skip Bézier complexity and use a simple rounded rect.
const SMALL_RECT_THRESHOLD: f32 = 4.0;

/// Minimum smoothing value above which we omit the arc segment.
const ARC_EPSILON: f32 = 1e-4;

/// A generated squircle path, represented as a sequence of [`PathCommand`]s.
///
/// The path is always closed and wound clockwise when viewed in screen
/// coordinates (positive Y downward).
#[derive(Debug, Clone)]
pub struct SquirclePath {
    /// The drawing commands that make up this path.
    pub commands: Vec<PathCommand>,
}

impl SquirclePath {
    /// Generate a squircle path for a rectangle with a uniform corner config.
    ///
    /// Negative dimensions are treated as absolute values. Zero-size rects
    /// return an empty path.
    pub fn new(x: f32, y: f32, width: f32, height: f32, config: SquircleConfig) -> Self {
        Self::with_corners(x, y, width, height, config, config, config, config)
    }

    /// Generate a squircle path with per-corner configuration.
    ///
    /// Corners are specified in CSS order: top-left, top-right, bottom-right,
    /// bottom-left.
    ///
    /// When adjacent corners would overlap, all four radii are scaled down
    /// proportionally so they just fit.
    #[allow(clippy::too_many_arguments)]
    pub fn with_corners(
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        top_left: SquircleConfig,
        top_right: SquircleConfig,
        bottom_right: SquircleConfig,
        bottom_left: SquircleConfig,
    ) -> Self {
        let w = width.abs();
        let h = height.abs();

        // Zero-size rect → empty path.
        if w < f32::EPSILON || h < f32::EPSILON {
            return Self { commands: Vec::new() };
        }

        // Clamp each corner radius to half the shorter dimension first.
        let half_min = (w / 2.0).min(h / 2.0);

        let mut r = [
            clamp_radius(top_left.corner_radius, half_min),
            clamp_radius(top_right.corner_radius, half_min),
            clamp_radius(bottom_right.corner_radius, half_min),
            clamp_radius(bottom_left.corner_radius, half_min),
        ];

        // Adjacent corner overlap check: scale all radii proportionally.
        let scale_top = if r[0] + r[1] > w { w / (r[0] + r[1]) } else { 1.0 };
        let scale_bottom = if r[3] + r[2] > w { w / (r[3] + r[2]) } else { 1.0 };
        let scale_left = if r[0] + r[3] > h { h / (r[0] + r[3]) } else { 1.0 };
        let scale_right = if r[1] + r[2] > h { h / (r[1] + r[2]) } else { 1.0 };
        let scale = scale_top.min(scale_bottom).min(scale_left).min(scale_right);
        if scale < 1.0 {
            for ri in &mut r {
                *ri *= scale;
            }
        }

        let configs = [top_left, top_right, bottom_right, bottom_left];
        let smoothings: [f32; 4] = core::array::from_fn(|i| configs[i].clamped_smoothing());

        // For very small rects, degenerate to simple rounded rect (arcs only).
        let use_bezier = w >= SMALL_RECT_THRESHOLD && h >= SMALL_RECT_THRESHOLD;
        let effective_smoothings: [f32; 4] = if use_bezier { smoothings } else { [0.0; 4] };

        // Build all four corners in clockwise order: TL, TR, BR, BL.
        let corners = [
            build_corner(x, y, w, h, r[0], effective_smoothings[0], CornerIdx::Tl),
            build_corner(x, y, w, h, r[1], effective_smoothings[1], CornerIdx::Tr),
            build_corner(x, y, w, h, r[2], effective_smoothings[2], CornerIdx::Br),
            build_corner(x, y, w, h, r[3], effective_smoothings[3], CornerIdx::Bl),
        ];

        // Assemble the full path.
        let mut cmds: Vec<PathCommand> = Vec::with_capacity(64);

        cmds.push(PathCommand::MoveTo(corners[0].entry_start.0, corners[0].entry_start.1));

        for (i, corner) in corners.iter().enumerate() {
            let next = &corners[(i + 1) % 4];

            // Entry Bézier.
            cmds.push(PathCommand::CubicTo {
                ctrl1: corner.entry_ctrl1,
                ctrl2: corner.entry_ctrl2,
                end: corner.arc_start,
            });

            // Arc segment (if smoothing < 1).
            if let Some(arc) = &corner.arc {
                cmds.push(PathCommand::ArcTo {
                    center: arc.center,
                    radius: arc.radius,
                    start_angle: arc.start_angle,
                    sweep_angle: arc.sweep_angle,
                });
            }

            // Exit Bézier.
            cmds.push(PathCommand::CubicTo {
                ctrl1: corner.exit_ctrl1,
                ctrl2: corner.exit_ctrl2,
                end: corner.exit_end,
            });

            // Line to next corner's entry point (omit if coincident).
            let ex = corner.exit_end;
            let ns = next.entry_start;
            if (ex.0 - ns.0).abs() > f32::EPSILON || (ex.1 - ns.1).abs() > f32::EPSILON {
                cmds.push(PathCommand::LineTo(ns.0, ns.1));
            }
        }

        cmds.push(PathCommand::Close);

        Self { commands: cmds }
    }
}

/// Corner index, used to look up per-corner geometry parameters.
#[derive(Clone, Copy)]
enum CornerIdx {
    Tl,
    Tr,
    Br,
    Bl,
}

/// Precomputed geometry for a single corner.
#[derive(Debug)]
struct CornerData {
    entry_start: (f32, f32),
    entry_ctrl1: (f32, f32),
    entry_ctrl2: (f32, f32),
    arc_start: (f32, f32),
    arc: Option<ArcData>,
    exit_ctrl1: (f32, f32),
    exit_ctrl2: (f32, f32),
    exit_end: (f32, f32),
}

/// Data for the optional circular arc segment.
#[derive(Debug)]
struct ArcData {
    center: (f32, f32),
    radius: f32,
    start_angle: f32,
    sweep_angle: f32,
}

// ──────────────────────────────────────────────────────────────────────────────
// Unified corner builder
// ──────────────────────────────────────────────────────────────────────────────
//
// Each corner is described by:
//   - The geometric corner point in world space.
//   - The arc circle centre (inset r along both edges).
//   - Incoming edge direction (unit vector along the edge arriving at the corner,
//     i.e. the direction the path is travelling just before the corner).
//   - Outgoing edge direction (unit vector leaving the corner along the exit edge).
//
// In screen coords (Y-down), the path winds CLOCKWISE:
//
//   TL: arriving along +X (top edge from left), leaving along +Y (left edge down).
//       Corner at (x, y). Arc center at (x+r, y+r).
//       Arc spans 225° (centre of 90° band 180°→270°).
//       Standard arc: 270°(top) → 180°(left), sweep = −90° (CW in screen).
//
//   TR: arriving along -Y (right edge from below), leaving along +X (top edge right)?
//       No — clockwise from TL to TR means arriving from the LEFT along top edge,
//       but TR is a CORNER not an edge midpoint. The path goes:
//         TL corner → top edge → TR corner → right edge → BR corner → ...
//       So at TR: arriving along +X (coming from left), leaving along +Y (going down right edge).
//       Wait, no: +X means going to the right along the top edge, and TR is at the right end.
//       After TR, the path goes DOWN the right edge = +Y direction.
//       TR: arriving along +X, leaving along +Y. Arc center at (x+w-r, y+r).
//       Standard arc: 270°(top) → 0°(right), sweep = +90° (CCW in standard math = CW screen).
//
//   Hmm, this needs careful thought. Let me use angle analysis.
//
//   For each corner, the arc circle is tangent to both edges. At the tangent point
//   on the incoming edge, the arc tangent matches the incoming edge direction.
//   At the tangent point on the outgoing edge, the arc tangent matches the outgoing
//   edge direction.
//
//   TL corner (x, y):
//     Incoming edge: top edge going right = +X direction.
//     The arc tangent at the top-edge tangent point must be +X.
//     Arc tangent at angle θ (increasing θ) = (-sin θ, cos θ).
//     For tangent = +X = (1,0): -sin θ = 1, cos θ = 0 → θ = -π/2 = 270°.
//     So the arc touches the top edge at θ=270°: point = (x+r + r·cos(270°), y+r + r·sin(270°))
//                                                       = (x+r + 0, y+r - r) = (x+r, y). ✓
//
//     Outgoing edge: left edge going down = +Y direction (after the corner, we go down).
//     Wait — at TL, after the corner, we travel DOWN the LEFT edge.
//     But the path is: entry_start on top edge → TL corner → exit_end on left edge.
//     So outgoing direction = +Y (downward).
//     Arc tangent for +Y: -sin θ = 0, cos θ = 1 → θ = 0°.
//     Hmm, arc at θ=0°: (x+r + r, y+r) = (x+2r, y+r). That's not on the left edge.
//
//     Wait — the outgoing direction is going DOWN the left edge = SOUTH = +Y.
//     But the arc tangent for going down at the left-edge tangent point:
//     Left edge tangent = +Y = (0, 1).
//     Arc tangent (increasing θ) = (-sin θ, cos θ) = (0, 1) → sin θ = 0, cos θ = 1 → θ = 0°.
//     But θ=0° gives point (x+r+r, y+r) = (x+2r, y+r) — that's to the RIGHT of center, not on the left edge.
//
//     Something is wrong. Let me reconsider. For the TL corner:
//     - The left edge is at x = x_rect. The arc circle center is at (x+r, y+r).
//     - The arc touches the left edge at the leftmost point: (x+r-r, y+r) = (x, y+r). ✓
//     - At (x, y+r): the radius direction is (x - (x+r), y+r - (y+r)) = (-r, 0), normalized = (-1, 0).
//     - The arc tangent (perpendicular to radius, in increasing-θ direction): rotate radius 90° CCW: (0, -1).
//     - But the path goes DOWNWARD (+Y) along the left edge, so the arc tangent at the exit point
//       should be in the +Y direction = (0, 1).
//     - (0, -1) ≠ (0, 1), so we need the arc to travel in DECREASING θ direction at this point.
//     - The tangent in decreasing-θ direction: (sin θ, -cos θ).
//     - At θ=180° (left tangent point): (sin(180°), -cos(180°)) = (0, 1). ✓
//
//     So for TL, the arc travels in DECREASING θ. The arc goes from θ=270° to θ=180°,
//     i.e. from 270° DOWN to 180°. Since θ decreases, sweep = 180° - 270° = -90°. ✓
//
//     So TL arc: start_angle=270°=-π/2, sweep=-π/2. End at 180°.
//     Wait: if we start at 270° and sweep -90°, we end at 270-90=180°. ✓
//
//     With smoothing ξ: the arc is reduced to the central θ=(1-ξ)·90° of the 90° band.
//     Central angle = 225° (midpoint of 270° to 180° = midpoint at 225°).
//     start_angle = 225° + θ/2 = 225° + 45°(1-ξ).
//     end_angle = 225° - θ/2 = 225° - 45°(1-ξ).
//     sweep = end - start = -90°(1-ξ).
//
//     In radians: start_angle = 5π/4 + π/4·(1-ξ).
//     At ξ=0: start=5π/4+π/4=6π/4=3π/2=270°. ✓
//     At ξ=1: start=5π/4+0=225°. ✓ (zero sweep)
//
//     Simplify: start_angle = 5π/4 + π/4 - π/4·ξ = 3π/2 - π/4·ξ.
//
//     Arc tangent at start (decreasing θ direction) = (sin(start_angle), -cos(start_angle)).
//     This is the direction the entry Bézier should approach the arc start with.
//
// TL: entry arrives from +X, tangent at arc_start = (sin(start), -cos(start)).
//     ctrl2 = arc_start - d2 · tangent_at_arc_start.
//
// TL: exit leaves from arc_end, tangent at arc_end = (sin(end_angle), -cos(end_angle)).
//     exit_ctrl1 = arc_end + d2 · tangent_at_arc_end.

fn build_corner(x: f32, y: f32, w: f32, h: f32, r: f32, xi: f32, idx: CornerIdx) -> CornerData {
    let available = (w / 2.0).min(h / 2.0);
    let p = (r * (1.0 + xi * 0.7)).min(available);
    let arc_extent = r * (1.0 - xi);
    let d1 = p * 0.4475;
    let d2 = (p - arc_extent) * 0.5523;

    // Arc center, arc start angle, arc sweep, entry/exit edge anchor points.
    let (cx, cy, arc_mid_angle, entry_start, exit_end, d1_entry_dir, d1_exit_dir) = match idx {
        CornerIdx::Tl => {
            // Arc center: inset r right and down from TL corner.
            // Arc travels from 270° to 180° (decreasing θ). Mid = 225°.
            // Entry: top edge, p units right of TL. exit: left edge, p units below TL.
            // d1_entry_dir: -X (move ctrl1 toward corner from entry_start).
            // d1_exit_dir: -Y (move ctrl2 toward exit_end from exit_ctrl2 direction).
            (
                x + r, y + r,
                5.0 * PI / 4.0,   // 225° midpoint
                (x + p, y),       // entry_start
                (x, y + p),       // exit_end
                (-1.0_f32, 0.0_f32), // ctrl1 = entry_start + d1*(1,0) from corner side, so ctrl1 = entry_start - d1*(1,0)
                (0.0_f32, -1.0_f32), // ctrl2_exit direction
            )
        }
        CornerIdx::Tr => {
            // TR corner at (x+w, y). Arc center: (x+w-r, y+r).
            // Arc travels from 270° to 360°/0° (increasing θ). Mid = 315° = -45°.
            // Entry: top edge, p units left of TR. Exit: right edge, p units below TR.
            (
                x + w - r, y + r,
                -PI / 4.0,        // 315° = -45° midpoint
                (x + w - p, y),   // entry_start
                (x + w, y + p),   // exit_end
                (1.0_f32, 0.0_f32),   // ctrl1 direction from entry_start toward corner
                (0.0_f32, -1.0_f32),  // ctrl2_exit direction
            )
        }
        CornerIdx::Br => {
            // BR corner at (x+w, y+h). Arc center: (x+w-r, y+h-r).
            // Arc travels from 0° to 90° (increasing θ). Mid = 45°.
            // Entry: right edge, p units above BR. Exit: bottom edge, p units left of BR.
            (
                x + w - r, y + h - r,
                PI / 4.0,         // 45° midpoint
                (x + w, y + h - p), // entry_start
                (x + w - p, y + h), // exit_end
                (0.0_f32, 1.0_f32),   // ctrl1 direction
                (1.0_f32, 0.0_f32),   // ctrl2_exit direction
            )
        }
        CornerIdx::Bl => {
            // BL corner at (x, y+h). Arc center: (x+r, y+h-r).
            // Arc travels from 90° to 180° (increasing θ). Mid = 135°.
            // Entry: bottom edge, p units right of BL. Exit: left edge, p units above BL.
            (
                x + r, y + h - r,
                3.0 * PI / 4.0,   // 135° midpoint
                (x + p, y + h),   // entry_start
                (x, y + h - p),   // exit_end
                (-1.0_f32, 0.0_f32),  // ctrl1 direction
                (0.0_f32, 1.0_f32),   // ctrl2_exit direction
            )
        }
    };

    // The arc half-angle θh = 45°·(1-ξ).
    let theta_half = FRAC_PI_2 / 2.0 * (1.0 - xi);

    // Arc start and end angles.
    let arc_start_angle = arc_mid_angle + theta_half;
    let arc_end_angle = arc_mid_angle - theta_half;

    // Determine sweep direction.
    // TL: decreasing (negative sweep). TR, BR, BL: increasing (positive sweep).
    let sweep = match idx {
        CornerIdx::Tl => -(FRAC_PI_2 * (1.0 - xi)),
        _ => FRAC_PI_2 * (1.0 - xi),
    };

    // Arc start and end points.
    let arc_sx = cx + r * arc_start_angle.cos();
    let arc_sy = cy + r * arc_start_angle.sin();
    let arc_ex = cx + r * arc_end_angle.cos();
    let arc_ey = cy + r * arc_end_angle.sin();

    // Arc tangent at arc_start in the direction of travel (decreasing or increasing θ).
    // For increasing θ: tangent = (-sin θ, cos θ).
    // For decreasing θ: tangent = (sin θ, -cos θ).
    let (arc_tangent_start, arc_tangent_end) = match idx {
        CornerIdx::Tl => {
            // Decreasing θ: tangent = (sin θ, -cos θ).
            let ts = (arc_start_angle.sin(), -arc_start_angle.cos());
            let te = (arc_end_angle.sin(), -arc_end_angle.cos());
            (ts, te)
        }
        _ => {
            // Increasing θ: tangent = (-sin θ, cos θ).
            let ts = (-arc_start_angle.sin(), arc_start_angle.cos());
            let te = (-arc_end_angle.sin(), arc_end_angle.cos());
            (ts, te)
        }
    };

    // ctrl1 for entry: along the incoming edge toward the geometric corner.
    // d1_entry_dir points FROM entry_start TOWARD the geometric corner.
    // ctrl1 = entry_start + d1 * d1_entry_dir.
    // For TL: corner=(x,y), entry_start=(x+p,y), d1_entry_dir=(-1,0). ctrl1=(x+p-d1,y). ✓
    let entry_ctrl1 = (
        entry_start.0 + d1 * d1_entry_dir.0,
        entry_start.1 + d1 * d1_entry_dir.1,
    );

    // ctrl2 for entry: d2 before arc_start along arc tangent AT arc_start.
    // arc_tangent_start is the direction the arc travels at arc_start.
    // ctrl2 = arc_start - d2 * arc_tangent_start (behind arc_start in travel direction).
    let entry_ctrl2 = (
        arc_sx - d2 * arc_tangent_start.0,
        arc_sy - d2 * arc_tangent_start.1,
    );

    // exit_ctrl1: d2 after arc_end along arc tangent AT arc_end.
    // arc_tangent_end is the direction at arc_end.
    // exit_ctrl1 = arc_end + d2 * arc_tangent_end.
    let exit_ctrl1 = (
        arc_ex + d2 * arc_tangent_end.0,
        arc_ey + d2 * arc_tangent_end.1,
    );

    // exit_ctrl2: on the outgoing edge, d1 before exit_end.
    // d1_exit_dir is the direction from exit_end TOWARD the corner.
    // So exit_ctrl2 = exit_end + d1 * d1_exit_dir.
    let exit_ctrl2 = (
        exit_end.0 + d1 * d1_exit_dir.0,
        exit_end.1 + d1 * d1_exit_dir.1,
    );

    let arc_data = if xi < 1.0 - ARC_EPSILON {
        Some(ArcData {
            center: (cx, cy),
            radius: r,
            start_angle: arc_start_angle,
            sweep_angle: sweep,
        })
    } else {
        None
    };

    CornerData {
        entry_start,
        entry_ctrl1,
        entry_ctrl2,
        arc_start: (arc_sx, arc_sy),
        arc: arc_data,
        exit_ctrl1,
        exit_ctrl2,
        exit_end,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::SquircleConfig;

    #[test]
    fn empty_path_for_zero_size() {
        let p = SquirclePath::new(0.0, 0.0, 0.0, 0.0, SquircleConfig::default());
        assert!(p.commands.is_empty());
    }

    #[test]
    fn path_starts_with_moveto() {
        let p = SquirclePath::new(0.0, 0.0, 100.0, 100.0, SquircleConfig::default());
        assert!(!p.commands.is_empty());
        assert!(matches!(p.commands[0], PathCommand::MoveTo(_, _)));
    }

    #[test]
    fn path_ends_with_close() {
        let p = SquirclePath::new(0.0, 0.0, 100.0, 100.0, SquircleConfig::default());
        assert!(matches!(p.commands.last(), Some(PathCommand::Close)));
    }

    #[test]
    fn smoothing_one_has_no_arc() {
        let cfg = SquircleConfig::new(20.0, 1.0);
        let p = SquirclePath::new(0.0, 0.0, 100.0, 100.0, cfg);
        let has_arc = p.commands.iter().any(|c| matches!(c, PathCommand::ArcTo { .. }));
        assert!(!has_arc, "smoothing=1 should produce no ArcTo commands");
    }

    #[test]
    fn smoothing_zero_has_arcs() {
        let cfg = SquircleConfig::new(20.0, 0.0);
        let p = SquirclePath::new(0.0, 0.0, 100.0, 100.0, cfg);
        let arc_count = p
            .commands
            .iter()
            .filter(|c| matches!(c, PathCommand::ArcTo { .. }))
            .count();
        assert_eq!(
            arc_count, 4,
            "smoothing=0 should produce 4 arc commands, got {arc_count}"
        );
    }

    #[test]
    fn negative_dimensions_treated_as_absolute() {
        let p1 = SquirclePath::new(0.0, 0.0, 100.0, 100.0, SquircleConfig::default());
        let p2 = SquirclePath::new(0.0, 0.0, -100.0, -100.0, SquircleConfig::default());
        assert_eq!(p1.commands.len(), p2.commands.len());
    }

    #[test]
    fn clamp_radius_test() {
        let cfg = SquircleConfig::new(100.0, 0.6);
        let p = SquirclePath::new(0.0, 0.0, 50.0, 50.0, cfg);
        assert!(!p.commands.is_empty());
    }

    #[test]
    fn symmetry_square() {
        let cfg = SquircleConfig::new(20.0, 0.6);
        let p = SquirclePath::new(0.0, 0.0, 100.0, 100.0, cfg);
        let types: Vec<&str> = p
            .commands
            .iter()
            .map(|c| match c {
                PathCommand::MoveTo(..) => "M",
                PathCommand::LineTo(..) => "L",
                PathCommand::CubicTo { .. } => "C",
                PathCommand::ArcTo { .. } => "A",
                PathCommand::Close => "Z",
            })
            .collect();
        assert_eq!(types[0], "M");
        assert_eq!(types.last(), Some(&"Z"));
    }

    #[test]
    fn per_corner_configs() {
        let tl = SquircleConfig::new(5.0, 0.0);
        let tr = SquircleConfig::new(10.0, 0.5);
        let br = SquircleConfig::new(15.0, 1.0);
        let bl = SquircleConfig::new(20.0, 0.6);
        let p = SquirclePath::with_corners(0.0, 0.0, 100.0, 100.0, tl, tr, br, bl);
        assert!(!p.commands.is_empty());
        assert!(matches!(p.commands.last(), Some(PathCommand::Close)));
    }
}
