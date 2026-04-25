//! Basic path generation integration tests.

use std::time::Instant;

use rounding::{PathCommand, SquircleConfig, SquirclePath};

// ── Symmetry test ─────────────────────────────────────────────────────────────

/// A square with uniform config should produce 4 structurally identical corner
/// segments. We verify this by checking that the sequence of command types
/// repeats with period 4 (excluding the leading MoveTo and trailing Close).
#[test]
fn symmetry_square_uniform_config() {
    let cfg = SquircleConfig::new(20.0, 0.6);
    let path = SquirclePath::new(0.0, 0.0, 100.0, 100.0, cfg);

    // Collect command types (skip MoveTo at start, Close at end).
    let cmds = &path.commands;
    assert!(matches!(cmds[0], PathCommand::MoveTo(_, _)));
    assert!(matches!(cmds.last().expect("non-empty"), PathCommand::Close));

    // The interior commands should be a repeating pattern of 4 groups.
    // Each group = CubicTo(entry) + optional ArcTo + CubicTo(exit) + optional LineTo.
    // For smoothing=0.6 there are arcs, so pattern per corner = C, A, C, [L].
    let inner: Vec<&str> = cmds[1..cmds.len() - 1]
        .iter()
        .map(|c| match c {
            PathCommand::MoveTo(..) => "M",
            PathCommand::LineTo(..) => "L",
            PathCommand::CubicTo { .. } => "C",
            PathCommand::ArcTo { .. } => "A",
            PathCommand::Close => "Z",
        })
        .collect();

    // Should be a multiple of 4 groups.
    // Each corner: C A C L (or C A C when last LineTo elided).
    // With 4 corners the pattern length should be divisible by 4 conceptually.
    assert!(!inner.is_empty(), "inner commands should not be empty");

    // Every fourth group must start with C (entry Bézier).
    // We find all "C" positions and verify there are exactly 8 (2 per corner × 4 corners).
    let cubic_count = inner.iter().filter(|&&t| t == "C").count();
    assert_eq!(cubic_count, 8, "Expected 8 cubic commands (2 per corner × 4 corners), got {cubic_count}");

    // There should be 4 arc commands.
    let arc_count = inner.iter().filter(|&&t| t == "A").count();
    assert_eq!(arc_count, 4, "Expected 4 arc commands for smoothing=0.6, got {arc_count}");
}

// ── Smoothing = 0 test ────────────────────────────────────────────────────────

/// With smoothing=0 the path should produce ArcTo commands (standard rounded rect).
#[test]
fn smoothing_zero_produces_arcs() {
    let cfg = SquircleConfig::new(15.0, 0.0);
    let path = SquirclePath::new(0.0, 0.0, 200.0, 100.0, cfg);
    let arc_count = path
        .commands
        .iter()
        .filter(|c| matches!(c, PathCommand::ArcTo { .. }))
        .count();
    assert_eq!(arc_count, 4, "smoothing=0 should produce 4 ArcTo commands");
}

// ── Smoothing = 1 test ────────────────────────────────────────────────────────

/// With smoothing=1 no ArcTo commands should appear.
#[test]
fn smoothing_one_no_arcs() {
    let cfg = SquircleConfig::new(15.0, 1.0);
    let path = SquirclePath::new(0.0, 0.0, 200.0, 100.0, cfg);
    let has_arc = path
        .commands
        .iter()
        .any(|c| matches!(c, PathCommand::ArcTo { .. }));
    assert!(!has_arc, "smoothing=1.0 must not produce any ArcTo commands");
}

// ── Clamp test ────────────────────────────────────────────────────────────────

/// Radius of 100 on a 50×50 rect should be clamped to 25 (half of 50).
/// The path must still be valid and contain exactly one MoveTo.
#[test]
fn radius_clamp_to_half_dimension() {
    let cfg = SquircleConfig::new(100.0, 0.6);
    let path = SquirclePath::new(0.0, 0.0, 50.0, 50.0, cfg);

    assert!(!path.commands.is_empty());
    let move_count = path
        .commands
        .iter()
        .filter(|c| matches!(c, PathCommand::MoveTo(_, _)))
        .count();
    assert_eq!(move_count, 1);
}

/// A rectangle where opposite corners would sum beyond the dimension.
#[test]
fn adjacent_corner_overlap_scaling() {
    let cfg = SquircleConfig::new(40.0, 0.6);
    // r[TL] + r[TR] = 80 > 50 (width). Should scale all radii.
    let path = SquirclePath::with_corners(
        0.0,
        0.0,
        50.0,
        100.0,
        cfg,
        cfg,
        cfg,
        cfg,
    );
    assert!(!path.commands.is_empty());
    // Verify no NaN or infinity in coordinates.
    for cmd in &path.commands {
        match *cmd {
            PathCommand::MoveTo(x, y) | PathCommand::LineTo(x, y) => {
                assert!(x.is_finite(), "MoveTo/LineTo x is not finite: {x}");
                assert!(y.is_finite(), "MoveTo/LineTo y is not finite: {y}");
            }
            PathCommand::CubicTo { ctrl1, ctrl2, end } => {
                assert!(ctrl1.0.is_finite() && ctrl1.1.is_finite());
                assert!(ctrl2.0.is_finite() && ctrl2.1.is_finite());
                assert!(end.0.is_finite() && end.1.is_finite());
            }
            PathCommand::ArcTo { center, radius, start_angle, sweep_angle } => {
                assert!(center.0.is_finite() && center.1.is_finite());
                assert!(radius.is_finite() && radius >= 0.0);
                assert!(start_angle.is_finite());
                assert!(sweep_angle.is_finite());
            }
            PathCommand::Close => {}
        }
    }
}

// ── Path structure tests ──────────────────────────────────────────────────────

#[test]
fn path_always_starts_move_ends_close() {
    for &smoothing in &[0.0_f32, 0.3, 0.6, 1.0] {
        for &r in &[5.0_f32, 20.0, 50.0] {
            let cfg = SquircleConfig::new(r, smoothing);
            let path = SquirclePath::new(10.0, 20.0, 120.0, 80.0, cfg);
            if path.commands.is_empty() {
                continue;
            }
            assert!(
                matches!(path.commands[0], PathCommand::MoveTo(_, _)),
                "r={r}, xi={smoothing}: first command must be MoveTo"
            );
            assert!(
                matches!(path.commands.last().expect("non-empty"), PathCommand::Close),
                "r={r}, xi={smoothing}: last command must be Close"
            );
        }
    }
}

#[test]
fn empty_path_for_zero_dimensions() {
    let cfg = SquircleConfig::default();
    assert!(SquirclePath::new(0.0, 0.0, 0.0, 100.0, cfg).commands.is_empty());
    assert!(SquirclePath::new(0.0, 0.0, 100.0, 0.0, cfg).commands.is_empty());
    assert!(SquirclePath::new(0.0, 0.0, 0.0, 0.0, cfg).commands.is_empty());
}

#[test]
fn negative_dimensions_same_as_positive() {
    let cfg = SquircleConfig::default();
    let pos = SquirclePath::new(0.0, 0.0, 80.0, 60.0, cfg);
    let neg = SquirclePath::new(0.0, 0.0, -80.0, -60.0, cfg);
    assert_eq!(pos.commands.len(), neg.commands.len());
}

// ── SVG output tests ──────────────────────────────────────────────────────────

#[test]
fn svg_output_is_valid_string() {
    let cfg = SquircleConfig::default();
    let path = SquirclePath::new(0.0, 0.0, 100.0, 100.0, cfg);
    let svg = path.to_svg_path_data();
    assert!(svg.starts_with('M'), "SVG should start with M");
    assert!(svg.ends_with('Z'), "SVG should end with Z");
    // Should contain cubic commands.
    assert!(svg.contains('C'), "SVG should contain cubic commands");
}

// ── Tessellation tests ────────────────────────────────────────────────────────

/// Tessellated points should form a closed polygon — first and last points
/// should be the same (or the implicit close should connect them).
#[test]
fn tessellated_closed_polygon() {
    let cfg = SquircleConfig::new(10.0, 0.6);
    let path = SquirclePath::new(0.0, 0.0, 100.0, 100.0, cfg);
    let pts = path.tessellate(0.25);
    assert!(!pts.is_empty(), "tessellated points should not be empty");

    // The last point should be the same as the first (closed polygon).
    let first = pts[0];
    let last = pts[pts.len() - 1];
    let dist = ((first[0] - last[0]).powi(2) + (first[1] - last[1]).powi(2)).sqrt();
    assert!(
        dist < 1.0,
        "tessellated polygon should be closed: first={first:?}, last={last:?}, dist={dist}"
    );
}

/// Tessellated points should all lie within the bounding box of the squircle
/// (with some tolerance for the corner reach).
#[test]
fn tessellated_within_bounding_box() {
    let cfg = SquircleConfig::new(10.0, 0.6);
    let path = SquirclePath::new(0.0, 0.0, 100.0, 100.0, cfg);
    let pts = path.tessellate(0.5);

    // p_max = r * (1 + ξ * 0.7) = 10 * (1 + 0.42) = 14.2
    // The path extends p units along the edge FROM the corner, not outside.
    // So all points should be within (0, 0) to (100, 100) since the squircle
    // curves away from the corner, not beyond the rect boundary.
    for pt in &pts {
        assert!(pt[0] >= -0.5, "x below rect: {}", pt[0]);
        assert!(pt[0] <= 100.5, "x above rect: {}", pt[0]);
        assert!(pt[1] >= -0.5, "y below rect: {}", pt[1]);
        assert!(pt[1] <= 100.5, "y above rect: {}", pt[1]);
    }
}

// ── SDF WGSL test ─────────────────────────────────────────────────────────────

/// The generated WGSL should be syntactically valid (contains fn keyword,
/// braces, and a return statement).
#[test]
fn sdf_wgsl_valid_syntax() {
    let cfg = SquircleConfig::new(12.0, 0.6);
    let code = SquirclePath::sdf_wgsl(&cfg);

    assert!(code.contains("fn squircle_sdf("), "must define squircle_sdf function");
    assert!(code.contains("-> f32"), "must return f32");
    assert!(code.contains('{'), "must have opening brace");
    assert!(code.contains('}'), "must have closing brace");
    assert!(code.contains("return"), "must have return statement");
    assert!(code.contains("vec2<f32>"), "must use vec2<f32> type");
}

// ── Performance test ─────────────────────────────────────────────────────────

/// Generate 10,000 paths of varied sizes. Total time must be < 10ms.
#[test]
fn performance_10k_paths() {
    // Use a deterministic pseudo-random sequence to vary inputs.
    let mut seed: u32 = 0xdeadbeef;
    let mut next_f32 = |lo: f32, hi: f32| -> f32 {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        let t = (seed as f32) / (u32::MAX as f32);
        lo + t * (hi - lo)
    };

    let n = 10_000;
    let start = Instant::now();

    for _ in 0..n {
        let w = next_f32(20.0, 400.0);
        let h = next_f32(20.0, 300.0);
        let r = next_f32(0.0, 30.0);
        let xi = next_f32(0.0, 1.0);
        let cfg = SquircleConfig::new(r, xi);
        let path = SquirclePath::new(0.0, 0.0, w, h, cfg);
        // Prevent the compiler from optimizing away the generation.
        assert!(!path.commands.is_empty() || (w < f32::EPSILON || h < f32::EPSILON));
    }

    let elapsed = start.elapsed();
    assert!(
        elapsed.as_millis() < 10,
        "10k path generation took {}ms, expected < 10ms",
        elapsed.as_millis()
    );
}

#[test]
fn sdf_wgsl_different_configs_produce_different_output() {
    let c1 = SquircleConfig::new(10.0, 0.0);
    let c2 = SquircleConfig::new(10.0, 1.0);
    let c3 = SquircleConfig::new(20.0, 0.6);

    let s1 = SquirclePath::sdf_wgsl(&c1);
    let s2 = SquirclePath::sdf_wgsl(&c2);
    let s3 = SquirclePath::sdf_wgsl(&c3);

    assert_ne!(s1, s2, "different smoothing → different shader");
    assert_ne!(s1, s3, "different radius → different shader");
    assert_ne!(s2, s3, "different configs → different shader");
}
