use animation::easing::{EasingAnimation, EasingCurve};
use std::time::Duration;

#[test]
fn linear_half_time_half_value() {
    let mut anim = EasingAnimation::new(0.0, 100.0, Duration::from_secs(1), EasingCurve::Linear);
    let v = anim.tick(Duration::from_millis(500));
    assert!((v - 50.0).abs() < 1e-9, "expected 50.0, got {v}");
}

#[test]
fn ease_out_expo_fast_start() {
    let mut anim =
        EasingAnimation::new(0.0, 100.0, Duration::from_secs(1), EasingCurve::EaseOutExpo);
    let v = anim.tick(Duration::from_millis(500));
    assert!(v > 90.0, "EaseOutExpo at 50%% time should be >90%%, got {v}");
}

#[test]
fn ease_out_back_overshoots() {
    let mut anim =
        EasingAnimation::new(0.0, 100.0, Duration::from_millis(500), EasingCurve::EaseOutBack);
    let mut max_val = 0.0_f64;
    for i in 1..=50 {
        let v = anim.tick(Duration::from_millis(10));
        if v > max_val {
            max_val = v;
        }
        let _ = i;
    }
    assert!(max_val > 100.0, "EaseOutBack should overshoot, max={max_val}");
}

#[test]
fn cubic_bezier_variant_works() {
    // CSS ease = cubic-bezier(0.25, 0.1, 0.25, 1.0)
    let mut anim = EasingAnimation::new(
        0.0,
        100.0,
        Duration::from_millis(500),
        EasingCurve::CubicBezier(0.25, 0.1, 0.25, 1.0),
    );
    let v = anim.tick(Duration::from_millis(250));
    // Should be between 0 and 100
    assert!(v > 0.0 && v < 100.0, "cubic-bezier mid value: {v}");
}

#[test]
fn animation_does_not_exceed_end_value() {
    for curve in [
        EasingCurve::Linear,
        EasingCurve::EaseInQuad,
        EasingCurve::EaseOutQuad,
        EasingCurve::EaseInOutQuad,
        EasingCurve::EaseInCubic,
        EasingCurve::EaseOutCubic,
        EasingCurve::EaseInOutCubic,
        EasingCurve::EaseOutExpo,
        EasingCurve::EaseInOutExpo,
    ] {
        let mut anim = EasingAnimation::new(0.0, 100.0, Duration::from_millis(200), curve.clone());
        for _ in 0..20 {
            let v = anim.tick(Duration::from_millis(10));
            assert!(
                v >= -0.01 && v <= 100.01,
                "curve {curve:?} out of range: {v}"
            );
        }
    }
}
