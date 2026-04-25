//! Edge case tests: degenerate rects, huge radii, zero radii.

use rounding::{PathCommand, SquircleConfig, SquirclePath};

#[test]
fn zero_radius_produces_straight_rect() {
    let cfg = SquircleConfig::new(0.0, 0.6);
    let path = SquirclePath::new(0.0, 0.0, 100.0, 100.0, cfg);
    // With r=0, p=0, no meaningful curves. Should still produce a valid path.
    assert!(matches!(path.commands[0], PathCommand::MoveTo(_, _)));
    assert!(matches!(path.commands.last().unwrap(), PathCommand::Close));
}

#[test]
fn very_large_radius_clamped() {
    let cfg = SquircleConfig::new(1_000_000.0, 0.6);
    let path = SquirclePath::new(0.0, 0.0, 100.0, 80.0, cfg);
    // Should not panic, NaN, or inf.
    assert_no_nans(&path);
}

#[test]
fn very_small_rect_uses_degenerate_path() {
    let cfg = SquircleConfig::new(2.0, 0.6);
    // 3×3 rect is below the 4px threshold → should degenerate.
    let path = SquirclePath::new(0.0, 0.0, 3.0, 3.0, cfg);
    assert!(!path.commands.is_empty());
    assert_no_nans(&path);
}

#[test]
fn rectangular_not_square() {
    let cfg = SquircleConfig::new(10.0, 0.6);
    let path = SquirclePath::new(0.0, 0.0, 300.0, 50.0, cfg);
    assert!(!path.commands.is_empty());
    assert_no_nans(&path);
}

#[test]
fn different_per_corner_radii_no_overlap() {
    let configs = [
        SquircleConfig::new(30.0, 0.6),
        SquircleConfig::new(10.0, 0.3),
        SquircleConfig::new(20.0, 1.0),
        SquircleConfig::new(5.0, 0.0),
    ];
    let path = SquirclePath::with_corners(
        0.0,
        0.0,
        80.0,
        60.0,
        configs[0],
        configs[1],
        configs[2],
        configs[3],
    );
    assert!(!path.commands.is_empty());
    assert_no_nans(&path);
}

#[test]
fn smoothing_clamped_above_one() {
    let cfg = SquircleConfig::new(10.0, 2.0); // smoothing > 1 → clamped to 1
    let path = SquirclePath::new(0.0, 0.0, 100.0, 100.0, cfg);
    // Should behave like smoothing=1: no arcs.
    let has_arc = path
        .commands
        .iter()
        .any(|c| matches!(c, PathCommand::ArcTo { .. }));
    assert!(!has_arc);
}

#[test]
fn smoothing_clamped_below_zero() {
    let cfg = SquircleConfig::new(10.0, -1.0); // smoothing < 0 → clamped to 0
    let path = SquirclePath::new(0.0, 0.0, 100.0, 100.0, cfg);
    // Should behave like smoothing=0: arcs present.
    let arc_count = path
        .commands
        .iter()
        .filter(|c| matches!(c, PathCommand::ArcTo { .. }))
        .count();
    assert_eq!(arc_count, 4);
}

#[test]
fn radius_exactly_half_dimension() {
    // r = w/2 = h/2 = 25 on a 50×50 rect: creates a circle.
    let cfg = SquircleConfig::new(25.0, 0.0);
    let path = SquirclePath::new(0.0, 0.0, 50.0, 50.0, cfg);
    assert!(!path.commands.is_empty());
    assert_no_nans(&path);
}

#[test]
fn tessellate_small_tolerance() {
    let cfg = SquircleConfig::new(10.0, 0.6);
    let path = SquirclePath::new(0.0, 0.0, 100.0, 100.0, cfg);
    // Small tolerance → many segments.
    let pts_fine = path.tessellate(0.1);
    let pts_coarse = path.tessellate(1.0);
    assert!(
        pts_fine.len() >= pts_coarse.len(),
        "finer tolerance should produce at least as many points"
    );
}

#[test]
fn all_smoothing_values_produce_finite_coords() {
    for xi_tenth in 0..=10 {
        let xi = xi_tenth as f32 / 10.0;
        let cfg = SquircleConfig::new(15.0, xi);
        let path = SquirclePath::new(5.0, 10.0, 120.0, 80.0, cfg);
        assert_no_nans(&path);
    }
}

/// Assert that no command in the path contains NaN or infinite values.
fn assert_no_nans(path: &SquirclePath) {
    for (i, cmd) in path.commands.iter().enumerate() {
        match *cmd {
            PathCommand::MoveTo(x, y) | PathCommand::LineTo(x, y) => {
                assert!(x.is_finite(), "cmd[{i}] MoveTo/LineTo x={x}");
                assert!(y.is_finite(), "cmd[{i}] MoveTo/LineTo y={y}");
            }
            PathCommand::CubicTo { ctrl1, ctrl2, end } => {
                assert!(ctrl1.0.is_finite(), "cmd[{i}] ctrl1.x={}", ctrl1.0);
                assert!(ctrl1.1.is_finite(), "cmd[{i}] ctrl1.y={}", ctrl1.1);
                assert!(ctrl2.0.is_finite(), "cmd[{i}] ctrl2.x={}", ctrl2.0);
                assert!(ctrl2.1.is_finite(), "cmd[{i}] ctrl2.y={}", ctrl2.1);
                assert!(end.0.is_finite(), "cmd[{i}] end.x={}", end.0);
                assert!(end.1.is_finite(), "cmd[{i}] end.y={}", end.1);
            }
            PathCommand::ArcTo { center, radius, start_angle, sweep_angle } => {
                assert!(center.0.is_finite(), "cmd[{i}] arc center.x={}", center.0);
                assert!(center.1.is_finite(), "cmd[{i}] arc center.y={}", center.1);
                assert!(radius.is_finite(), "cmd[{i}] arc radius={radius}");
                assert!(start_angle.is_finite(), "cmd[{i}] arc start_angle={start_angle}");
                assert!(sweep_angle.is_finite(), "cmd[{i}] arc sweep={sweep_angle}");
            }
            PathCommand::Close => {}
        }
    }
}
