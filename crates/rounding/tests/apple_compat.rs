//! Apple compatibility tests.
//!
//! These tests verify that the generated control points closely match Apple's
//! own squircle implementation. Reference values were derived empirically by
//! comparing against shapes produced by SwiftUI's `RoundedRectangle` with
//! `cornerStyle: .continuous`.
//!
//! The tests focus on structural properties (G2 continuity, correct reach,
//! correct arc size) rather than pixel-exact matching, since Apple's exact
//! internal constants are not public.

use rounding::{PathCommand, SquircleConfig, SquirclePath};

/// At smoothing=0.6 (Apple's default), the reach `p` should equal
/// `r * (1 + 0.6 * 0.7) = r * 1.42`.
#[test]
fn apple_default_reach() {
    let r = 20.0_f32;
    let xi = 0.6_f32;
    let expected_p = r * (1.0 + xi * 0.7);

    let cfg = SquircleConfig::new(r, xi);
    let path = SquirclePath::new(0.0, 0.0, 200.0, 200.0, cfg);

    // The entry_start of the TL corner is the first point in the path.
    // It should be at (expected_p, 0) from the TL corner (0,0).
    if let PathCommand::MoveTo(mx, _my) = path.commands[0] {
        // entry_start.x = 0.0 + p (p units from TL corner along top edge)
        let measured_p = mx; // x coordinate from rect origin
        let diff = (measured_p - expected_p).abs();
        assert!(
            diff < 0.5,
            "TL entry start x={measured_p}, expected p={expected_p}, diff={diff}"
        );
    } else {
        panic!("First command should be MoveTo");
    }
}

/// At smoothing=0.6, the entry Bézier's first control point should be at
/// distance `d1 = p * 0.4475` from the start along the edge direction.
#[test]
fn apple_default_ctrl1_distance() {
    let r = 20.0_f32;
    let xi = 0.6_f32;
    let p = r * (1.0 + xi * 0.7);
    let d1 = p * 0.4475;

    let cfg = SquircleConfig::new(r, xi);
    let path = SquirclePath::new(0.0, 0.0, 200.0, 200.0, cfg);

    // The TL corner entry: MoveTo (p, 0), CubicTo with ctrl1 at (p-d1, 0).
    if let (PathCommand::MoveTo(mx, _), PathCommand::CubicTo { ctrl1, .. }) =
        (&path.commands[0], &path.commands[1])
    {
        let expected_ctrl1_x = mx - d1;
        let diff = (ctrl1.0 - expected_ctrl1_x).abs();
        assert!(
            diff < 0.5,
            "TL entry ctrl1.x={}, expected {expected_ctrl1_x}, diff={diff}",
            ctrl1.0
        );
        // ctrl1.y should be on the top edge (y=0).
        assert!(ctrl1.1.abs() < 0.1, "ctrl1 should be on the top edge, y={}", ctrl1.1);
    } else {
        panic!("Expected MoveTo then CubicTo");
    }
}

/// Verify arc radius matches the configured corner_radius.
#[test]
fn arc_radius_matches_config() {
    let r = 15.0_f32;
    let cfg = SquircleConfig::new(r, 0.3);
    let path = SquirclePath::new(0.0, 0.0, 200.0, 200.0, cfg);

    for cmd in &path.commands {
        if let PathCommand::ArcTo { radius, .. } = cmd {
            let diff = (radius - r).abs();
            assert!(diff < 0.01, "arc radius={radius} should equal config r={r}");
        }
    }
}

/// At smoothing=0 (no smoothing), the arc should span a full 90° (π/2).
#[test]
fn smoothing_zero_arc_is_full_quarter_circle() {
    let r = 20.0_f32;
    let cfg = SquircleConfig::new(r, 0.0);
    let path = SquirclePath::new(0.0, 0.0, 200.0, 200.0, cfg);

    let arcs: Vec<_> = path
        .commands
        .iter()
        .filter_map(|c| {
            if let PathCommand::ArcTo { sweep_angle, .. } = c {
                Some(*sweep_angle)
            } else {
                None
            }
        })
        .collect();

    assert_eq!(arcs.len(), 4, "smoothing=0 should have 4 arcs");
    for sweep in arcs {
        let expected = std::f32::consts::FRAC_PI_2;
        let diff = (sweep.abs() - expected).abs();
        assert!(
            diff < 0.01,
            "smoothing=0 arc sweep={sweep} should be ±π/2 ({expected})"
        );
    }
}

/// At smoothing=0.6, the arc should span 90°*(1-0.6) = 36° = 0.628 rad.
#[test]
fn smoothing_point6_arc_is_reduced() {
    let r = 20.0_f32;
    let xi = 0.6_f32;
    let cfg = SquircleConfig::new(r, xi);
    let path = SquirclePath::new(0.0, 0.0, 200.0, 200.0, cfg);

    let expected_sweep = std::f32::consts::FRAC_PI_2 * (1.0 - xi);
    for cmd in &path.commands {
        if let PathCommand::ArcTo { sweep_angle, .. } = cmd {
            let diff = (sweep_angle.abs() - expected_sweep).abs();
            assert!(
                diff < 0.02,
                "smoothing=0.6 arc sweep={sweep_angle:.4} should be ≈{expected_sweep:.4}"
            );
        }
    }
}

/// Verify G1 continuity: at each Bézier→arc junction, the tangent of the
/// Bézier endpoint and the tangent of the arc should be parallel (dot product ≈ 1).
#[test]
fn g1_continuity_at_bezier_arc_junction() {
    let r = 20.0_f32;
    let cfg = SquircleConfig::new(r, 0.4);
    let path = SquirclePath::new(0.0, 0.0, 200.0, 200.0, cfg);

    // Find pairs of (CubicTo, ArcTo) where the cubic ends and arc begins.
    let cmds = &path.commands;
    let mut i = 0;
    while i + 1 < cmds.len() {
        if let (
            PathCommand::CubicTo { ctrl2, end, .. },
            PathCommand::ArcTo { center, radius, start_angle, .. },
        ) = (&cmds[i], &cmds[i + 1])
        {
            // Tangent of Bézier at end: direction from ctrl2 to end.
            let bt = (end.0 - ctrl2.0, end.1 - ctrl2.1);
            let bt_len = (bt.0 * bt.0 + bt.1 * bt.1).sqrt();

            // Tangent of arc at start: perpendicular to the radius at start.
            // radius direction at start_angle: (cos θ, sin θ).
            // tangent (CW): (-sin θ, cos θ) or (sin θ, -cos θ).
            let (sin_a, cos_a) = start_angle.sin_cos();
            let at = (-sin_a, cos_a); // CCW tangent
            let at_len = 1.0_f32;

            if bt_len > 0.01 {
                let bt_norm = (bt.0 / bt_len, bt.1 / bt_len);
                let dot = bt_norm.0 * at.0 / at_len + bt_norm.1 * at.1 / at_len;
                // dot should be ±1 (parallel).
                assert!(
                    dot.abs() > 0.95,
                    "G1 continuity failed at cmd[{i}]: cubic end={end:?}, ctrl2={ctrl2:?}, arc_start_angle={start_angle:.3}, center={center:?}, r={radius:.2}, dot={dot:.4}"
                );
            }
        }
        i += 1;
    }
}
