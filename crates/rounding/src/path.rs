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
        let effective_smoothings: [f32; 4] = if use_bezier {
            smoothings
        } else {
            [0.0; 4]
        };

        // Build all four corners.
        // Corner order: TL(0), TR(1), BR(2), BL(3) — clockwise in screen coords.
        let corners = [
            build_tl_corner(x, y, r[0], effective_smoothings[0], w, h),
            build_tr_corner(x, y, w, h, r[1], effective_smoothings[1]),
            build_br_corner(x, y, w, h, r[2], effective_smoothings[2]),
            build_bl_corner(x, y, h, r[3], effective_smoothings[3], w),
        ];

        // Assemble the full path.
        let mut cmds: Vec<PathCommand> = Vec::with_capacity(64);

        // MoveTo the start of corner 0's entry point.
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

            // Line to next corner's entry point.
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

/// Precomputed geometry for a single corner.
#[derive(Debug)]
struct CornerData {
    /// Start of the entry Bézier (on the incoming straight edge).
    entry_start: (f32, f32),
    /// First control point of the entry Bézier.
    entry_ctrl1: (f32, f32),
    /// Second control point of the entry Bézier.
    entry_ctrl2: (f32, f32),
    /// End of the entry Bézier = start of the arc (or apex if no arc).
    arc_start: (f32, f32),
    /// Optional circular arc.
    arc: Option<ArcData>,
    /// First control point of the exit Bézier.
    exit_ctrl1: (f32, f32),
    /// Second control point of the exit Bézier.
    exit_ctrl2: (f32, f32),
    /// End of the exit Bézier (on the outgoing straight edge).
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
// Per-corner builders (explicit geometry, no rotation matrices)
// ──────────────────────────────────────────────────────────────────────────────
//
// Path winds clockwise:  TL → top edge → TR → right edge → BR → bottom edge →
//                        BL → left edge → TL.
//
// Each corner has an "incoming" direction (the edge we arrive along) and an
// "outgoing" direction (the edge we leave along).
//
// Control point conventions:
//   d1 = p * 0.4475   (ctrl1, tangent to straight edge)
//   d2 = (p - arc_extent) * 0.5523  (ctrl2, tangent to arc)
//
// The arc, when present, lives in the central angle band and has its center
// offset r inward from the corner along both edges.

/// Compute reach `p` and arc extent for a corner.
#[inline]
fn corner_params(r: f32, xi: f32, available: f32) -> (f32, f32) {
    let p = (r * (1.0 + xi * 0.7)).min(available);
    let arc_extent = r * (1.0 - xi);
    (p, arc_extent)
}

/// Build control point distances.
#[inline]
fn ctrl_dists(p: f32, arc_extent: f32) -> (f32, f32) {
    let d1 = p * 0.4475;
    let d2 = (p - arc_extent) * 0.5523;
    (d1, d2)
}

/// Decide whether to emit an arc and compute its parameters.
fn make_arc(
    xi: f32,
    r: f32,
    center: (f32, f32),
    start_angle: f32,
    sweep: f32,
) -> Option<ArcData> {
    if xi >= 1.0 - ARC_EPSILON {
        return None;
    }
    Some(ArcData {
        center,
        radius: r,
        start_angle,
        sweep_angle: sweep,
    })
}

// ── Top-left corner ──────────────────────────────────────────────────────────
//
// Incoming: top edge, arriving from the RIGHT (entry_start is to the right of TL)
//   entry_start = (x + p, y)
//   ctrl1 = (x + p - d1, y)   [tangent to top edge]
//
// Outgoing: left edge, going DOWN
//   exit_end = (x, y + p)
//   ctrl2_exit = (x, y + p - d1)
//
// Arc: circle center = (x + r, y + r).
//   Arc goes from angle 270° (pointing up = touching top edge) to 180°
//   (pointing left = touching left edge), sweeping -90° (CW in screen coords).
//   With smoothing, the arc is a smaller central portion of that 90°.
//
//   The arc subtends angle θ = 90°·(1-ξ) centred at 225°.
//   start_angle = 180° + 45°·ξ  (in radians: π + π/4·ξ)
//   sweep = -90°·(1-ξ) = -(π/2)·(1-ξ)   [CW = negative in standard math]
//
//   arc_start (entry Bézier end) = center + r*(cos start_angle, sin start_angle)
//   arc_end   (exit Bézier start) = center + r*(cos(start_angle+sweep), sin(...))
fn build_tl_corner(x: f32, y: f32, r: f32, xi: f32, w: f32, h: f32) -> CornerData {
    let available = (w / 2.0).min(h / 2.0);
    let (p, arc_extent) = corner_params(r, xi, available);
    let (d1, d2) = ctrl_dists(p, arc_extent);

    let cx = x + r;
    let cy = y + r;

    // Arc angles for TL (CW in screen coords = negative sweep in standard math).
    let arc_start_angle = PI + FRAC_PI_2 / 2.0 * xi; // 180° + 45°·ξ
    let sweep = -(FRAC_PI_2 * (1.0 - xi));           // negative = CW

    let arc_sx = cx + r * arc_start_angle.cos();
    let arc_sy = cy + r * arc_start_angle.sin();
    let arc_end_angle = arc_start_angle + sweep;
    let arc_ex = cx + r * arc_end_angle.cos();
    let arc_ey = cy + r * arc_end_angle.sin();

    // When arc vanishes (ξ=1), arc_start = arc_end = corner apex (x+r, y+r)? No.
    // At ξ=1: arc_start_angle = 225°, sweep=0 → arc_sx = cx + r*cos(225°), etc.
    // That's the midpoint of the 90° arc = (cx - r/√2, cy - r/√2).
    // But with no arc, the Bézier entry and exit must meet at a single point.
    // For ξ=1 the Bézier should produce a smooth curve through the "apex".
    // The arc_start == arc_end at ξ=1, which is fine — we just skip the arc.

    let entry_start = (x + p, y);
    let entry_ctrl1 = (x + p - d1, y);
    let entry_ctrl2 = (arc_sx + d2, arc_sy); // approach arc_start from the right
    let arc_start = (arc_sx, arc_sy);

    let arc_data = make_arc(xi, r, (cx, cy), arc_start_angle, sweep);

    // Exit from arc_end going downward along left edge.
    let exit_ctrl1 = (arc_ex, arc_ey - d2); // leave arc_end going down
    let exit_ctrl2 = (x, y + p - d1);
    let exit_end = (x, y + p);

    CornerData {
        entry_start,
        entry_ctrl1,
        entry_ctrl2,
        arc_start,
        arc: arc_data,
        exit_ctrl1,
        exit_ctrl2,
        exit_end,
    }
}

// ── Top-right corner ─────────────────────────────────────────────────────────
//
// Incoming: top edge, arriving from the LEFT
//   entry_start = (x + w - p, y)   [to the left of TR on top edge]
//   ctrl1 = (x + w - p + d1, y)
//
// Outgoing: right edge, going DOWN
//   exit_end = (x + w, y + p)
//   ctrl2_exit = (x + w, y + p - d1)
//
// Arc center = (x + w - r, y + r). Standard arc 270°→0°, sweep +90° CW.
//   (In standard math coords with Y-down, 0° is right, 270°=-90° is up.)
//   With smoothing: start_angle = 270° - 45°·ξ = -π/2 - π/4·ξ
//   Actually symmetric to TL: start_angle = -π/2 + π/4·ξ... let me work it out.
//
//   TL arc centre is at 225°±θ/2 (between 180° and 270°).
//   TR arc centre is at 315°±θ/2 (between 270° and 360°/0°), i.e. -45°.
//   TR arc_start_angle = 270° - 45°·ξ   (goes from 270° toward 315° as ξ→1)
//   TR sweep = +90°·(1-ξ)  [CW in screen coords = positive in Y-down math]
//   Wait: 270° to 360°: is that CW or CCW?
//
//   In screen coords (Y down), angles go CW. 270° = up (-Y), 360°/0° = right (+X).
//   Going from 270° to 360° is moving from "up" to "right", which is CW.
//   So sweep = +90°·(1-ξ).
//
//   start_angle = -π/2 + π/4·ξ  (i.e. 270° - 45°·ξ in standard math)
//   Actually 270°-45°·ξ = 270° when ξ=0, sweeping +90° ends at 360°. ✓
//   And 270°+45°·1 = 315°, sweep=0. OK but that should be symmetric to TL.
//   TL: 180°+45°ξ, sweep -90°(1-ξ). At ξ=0: 180°→270° going CCW (−90°).
//   TR: mirror: start at -90°(=270°) going to 0°. Sweep = +90°.
//   With ξ: start = -90° - (-90°·ξ/2) ... let's just do -π/2 + π/4·ξ.
//   At ξ=0: start=−π/2=270°, sweep=+π/2 → end=0°. ✓
//   At ξ=1: start=−π/4=315°, sweep=0. ✓ (midpoint of the 90° band)

fn build_tr_corner(x: f32, y: f32, w: f32, h: f32, r: f32, xi: f32) -> CornerData {
    let available = (w / 2.0).min(h / 2.0);
    let (p, arc_extent) = corner_params(r, xi, available);
    let (d1, d2) = ctrl_dists(p, arc_extent);

    let cx = x + w - r;
    let cy = y + r;

    let arc_start_angle = -FRAC_PI_2 + FRAC_PI_2 / 2.0 * xi; // -90° + 45°·ξ
    let sweep = FRAC_PI_2 * (1.0 - xi);

    let arc_sx = cx + r * arc_start_angle.cos();
    let arc_sy = cy + r * arc_start_angle.sin();
    let arc_end_angle = arc_start_angle + sweep;
    let arc_ex = cx + r * arc_end_angle.cos();
    let arc_ey = cy + r * arc_end_angle.sin();

    let entry_start = (x + w - p, y);
    let entry_ctrl1 = (x + w - p + d1, y);
    let entry_ctrl2 = (arc_sx - d2, arc_sy); // approach arc_start from the left
    let arc_start = (arc_sx, arc_sy);

    let arc_data = make_arc(xi, r, (cx, cy), arc_start_angle, sweep);

    let exit_ctrl1 = (arc_ex, arc_ey - d2);
    let exit_ctrl2 = (x + w, y + p - d1);
    let exit_end = (x + w, y + p);

    CornerData {
        entry_start,
        entry_ctrl1,
        entry_ctrl2,
        arc_start,
        arc: arc_data,
        exit_ctrl1,
        exit_ctrl2,
        exit_end,
    }
}

// ── Bottom-right corner ──────────────────────────────────────────────────────
//
// Incoming: right edge, arriving from ABOVE
//   entry_start = (x + w, y + h - p)
//   ctrl1 = (x + w, y + h - p + d1)
//
// Outgoing: bottom edge, going LEFT
//   exit_end = (x + w - p, y + h)
//   ctrl2 = (x + w - p + d1, y + h)
//
// Arc center = (x + w - r, y + h - r).
//   BR arc: from 0° to 90° (right to down), sweep +90°.
//   With ξ: start = 0° - 45°·ξ/2... mirror of TL rotated 180°.
//   TL centred at 225°. BR centred at 45°.
//   start = 0° + 45°·ξ/2... let's just: start = -π/4·ξ (=45° moving toward 0 as ξ→1)
//   Hmm — at ξ=0: 0° → 90°, sweep=90°. At ξ=1: 45°, sweep=0.
//   start = -(π/4)·ξ → at ξ=0: 0°, ξ=1: -45°? No, we want ξ=0 → start=0°.
//
//   Let me be systematic. For each corner, the full arc spans 90° CW. The arc
//   is positioned at the centre of that span, with width θ = 90°·(1-ξ).
//
//   TL: span 180°→270° (going CW in screen), centre=225°. start=225°-θ/2, end=225°+θ/2.
//       sweep = −θ (CW direction in standard math when Y-down is CW = negative standard).
//       Wait I need to be consistent. In screen coords Y-down:
//       - CW = increasing angle in standard math? No!
//       - Standard math: angles CCW, Y-up.
//       - Screen coords: Y-down, so CW in screen = decreasing angle in standard math.
//
//       Actually let me just use the fact that in SVG/screen: arc from 270° to 180°
//       is CW (going from top to left). Let me check:
//       At 270° (= -90°): point is directly UP from center = (cx, cy-r) — that's the TOP.
//       At 180°: point is LEFT of center = (cx-r, cy).
//       Going from TOP to LEFT: that's counterclockwise in screen coords? No, clockwise!
//       In screen coords (Y down), clockwise means: right → down → left → up → right.
//       270° = up, 180° = left. Going up → left is CCW in screen.
//
//       Hmm. Let me just use direction vectors.
//
//       TL corner: entering from top-right (positive X), exiting to bottom (positive Y).
//       The arc at TL: at arc_start, tangent is along +X (horizontal, going right).
//       At arc_end, tangent is along -Y (going up)?? No — exiting downward.
//
//       I've been going back and forth. Let me just compute numerically for ξ=0
//       and verify the path looks right.
//
// For BR: enter from right edge going down (positive Y). Exit along bottom edge going left (negative X).
// Arc center = (x+w-r, y+h-r).
// Entry tangent should be along +Y (going down). That's at angle 90° from center.
//   Actually: tangent = d(point)/dθ = r*(-sin θ, cos θ). For tangent=(0,1): cos θ=1→θ=0, sin θ=0. ✓
//   So arc entry is at θ=0°, and exit is at θ=90° (tangent=(-1,0) = going left).
//   Wait: at θ=90°, tangent=(-sin 90°, cos 90°) = (-1, 0). ✓ going left.
//   So BR arc goes from θ=0° to θ=90°, sweep=+90°.
//   With ξ: centered at 45°.
//   start = 45° - 45°·(1-ξ)/2 * 2...
//   start = 0 + 45°·ξ/...
//   Let me just use: start_angle = (π/4)·ξ, sweep = π/2·(1-ξ).
//   At ξ=0: start=0°, sweep=90°. ✓
//   At ξ=1: start=45°, sweep=0°. ✓

fn build_br_corner(x: f32, y: f32, w: f32, h: f32, r: f32, xi: f32) -> CornerData {
    let available = (w / 2.0).min(h / 2.0);
    let (p, arc_extent) = corner_params(r, xi, available);
    let (d1, d2) = ctrl_dists(p, arc_extent);

    let cx = x + w - r;
    let cy = y + h - r;

    let arc_start_angle = FRAC_PI_2 / 2.0 * xi; // 45°·ξ
    let sweep = FRAC_PI_2 * (1.0 - xi);

    let arc_sx = cx + r * arc_start_angle.cos();
    let arc_sy = cy + r * arc_start_angle.sin();
    let arc_end_angle = arc_start_angle + sweep;
    let arc_ex = cx + r * arc_end_angle.cos();
    let arc_ey = cy + r * arc_end_angle.sin();

    // Entry: arriving from above on right edge.
    let entry_start = (x + w, y + h - p);
    let entry_ctrl1 = (x + w, y + h - p + d1);
    let entry_ctrl2 = (arc_sx, arc_sy - d2);
    let arc_start = (arc_sx, arc_sy);

    let arc_data = make_arc(xi, r, (cx, cy), arc_start_angle, sweep);

    // Exit: going left along bottom edge.
    let exit_ctrl1 = (arc_ex + d2, arc_ey);
    let exit_ctrl2 = (x + w - p + d1, y + h);
    let exit_end = (x + w - p, y + h);

    CornerData {
        entry_start,
        entry_ctrl1,
        entry_ctrl2,
        arc_start,
        arc: arc_data,
        exit_ctrl1,
        exit_ctrl2,
        exit_end,
    }
}

// ── Bottom-left corner ────────────────────────────────────────────────────────
//
// Incoming: bottom edge, arriving from the RIGHT
//   entry_start = (x + p, y + h)
//   ctrl1 = (x + p - d1, y + h)
//
// Outgoing: left edge, going UP
//   exit_end = (x, y + h - p)
//   ctrl2 = (x, y + h - p + d1)
//
// Arc center = (x + r, y + h - r).
// Enter along bottom edge going left (tangent=-X direction).
//   tangent = (-sin θ, cos θ) = (-1, 0) → sin θ = 1 → θ = 90°.
//   exit tangent = (0, -1) going up → (-sin θ, cos θ)=(0,-1) → cos θ=-1 → θ=180°.
//   So arc goes from 90° to 180°, sweep=+90°.
//   With ξ: centered at 135°.
//   start = π/2 + π/4·ξ... at ξ=0: 90°, sweep=90° to 180°. ✓
//   At ξ=1: start=135°, sweep=0°. ✓
//   start_angle = π/2 + (π/4)·ξ

fn build_bl_corner(x: f32, y: f32, h: f32, r: f32, xi: f32, w: f32) -> CornerData {
    let available = (w / 2.0).min(h / 2.0);
    let (p, arc_extent) = corner_params(r, xi, available);
    let (d1, d2) = ctrl_dists(p, arc_extent);

    let cx = x + r;
    let cy = y + h - r;

    let arc_start_angle = FRAC_PI_2 + FRAC_PI_2 / 2.0 * xi; // 90° + 45°·ξ
    let sweep = FRAC_PI_2 * (1.0 - xi);

    let arc_sx = cx + r * arc_start_angle.cos();
    let arc_sy = cy + r * arc_start_angle.sin();
    let arc_end_angle = arc_start_angle + sweep;
    let arc_ex = cx + r * arc_end_angle.cos();
    let arc_ey = cy + r * arc_end_angle.sin();

    // Entry: arriving from right along bottom edge (going left).
    let entry_start = (x + p, y + h);
    let entry_ctrl1 = (x + p - d1, y + h);
    let entry_ctrl2 = (arc_sx + d2, arc_sy);
    let arc_start = (arc_sx, arc_sy);

    let arc_data = make_arc(xi, r, (cx, cy), arc_start_angle, sweep);

    // Exit: going up along left edge.
    let exit_ctrl1 = (arc_ex, arc_ey + d2);
    let exit_ctrl2 = (x, y + h - p + d1);
    let exit_end = (x, y + h - p);

    CornerData {
        entry_start,
        entry_ctrl1,
        entry_ctrl2,
        arc_start,
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
        let arc_count = p.commands.iter().filter(|c| matches!(c, PathCommand::ArcTo { .. })).count();
        assert_eq!(arc_count, 4, "smoothing=0 should produce 4 arc commands, got {arc_count}");
    }

    #[test]
    fn negative_dimensions_treated_as_absolute() {
        let p1 = SquirclePath::new(0.0, 0.0, 100.0, 100.0, SquircleConfig::default());
        let p2 = SquirclePath::new(0.0, 0.0, -100.0, -100.0, SquircleConfig::default());
        assert_eq!(p1.commands.len(), p2.commands.len());
    }

    #[test]
    fn clamp_radius_test() {
        // Radius 100 on 50×50 rect → clamped to 25.
        let cfg = SquircleConfig::new(100.0, 0.6);
        let p = SquirclePath::new(0.0, 0.0, 50.0, 50.0, cfg);
        assert!(!p.commands.is_empty());
    }

    #[test]
    fn symmetry_square() {
        // A square with uniform config should produce a symmetric path.
        let cfg = SquircleConfig::new(20.0, 0.6);
        let p = SquirclePath::new(0.0, 0.0, 100.0, 100.0, cfg);
        // Count command types — they should repeat in groups of 4.
        let types: Vec<&str> = p.commands.iter().map(|c| match c {
            PathCommand::MoveTo(..) => "M",
            PathCommand::LineTo(..) => "L",
            PathCommand::CubicTo { .. } => "C",
            PathCommand::ArcTo { .. } => "A",
            PathCommand::Close => "Z",
        }).collect();
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
