use animation::animated::{AnimatedRect, AnimatedValue};
use animation::presets::{AnimPreset, WindowAnimationPresets};
use animation::easing::{EasingAnimation, EasingCurve};
use animation::spring::SpringAnimation;
use std::time::Duration;

#[test]
fn default_presets_are_sane() {
    let p = WindowAnimationPresets::default();

    // open should be easing 250ms EaseOutExpo
    assert!(
        matches!(
            &p.open,
            AnimPreset::Easing { duration_ms: 250, curve: EasingCurve::EaseOutExpo }
        ),
        "unexpected open: {:?}", p.open
    );

    // minimize should be critically-damped spring
    assert!(
        matches!(
            &p.minimize,
            AnimPreset::Spring { damping_ratio, stiffness, .. }
            if (*damping_ratio - 1.0).abs() < 1e-9 && (*stiffness - 600.0).abs() < 1e-9
        ),
        "unexpected minimize: {:?}", p.minimize
    );

    // move_follow should be underdamped spring
    assert!(
        matches!(
            &p.move_follow,
            AnimPreset::Spring { damping_ratio, .. }
            if (*damping_ratio - 0.8).abs() < 1e-9
        ),
        "unexpected move_follow: {:?}", p.move_follow
    );
}

#[test]
fn animated_rect_tick_updates_all_components() {
    let mut rect: AnimatedRect = AnimatedValue::new_spring(600.0, 1.0, 0.001);
    rect.set_position([0.0, 0.0, 100.0, 50.0]);
    rect.set_target([200.0, 150.0, 300.0, 200.0]);

    let before = rect.position();
    let after = rect.tick(1.0 / 60.0);

    for i in 0..4 {
        assert!(
            (after[i] - before[i]).abs() > 1e-9,
            "component {i} did not change: before={} after={}",
            before[i],
            after[i]
        );
    }
}

#[test]
fn preset_easing_creates_valid_animation() {
    let preset = AnimPreset::Easing {
        duration_ms: 250,
        curve: EasingCurve::EaseOutExpo,
    };

    if let AnimPreset::Easing { duration_ms, curve } = preset {
        let mut anim = EasingAnimation::new(0.0, 1.0, Duration::from_millis(duration_ms.into()), curve);
        let v = anim.tick(Duration::from_millis(125));
        assert!(v > 0.9, "EaseOutExpo at 50%% should be >90%%, got {v}");
    }
}

#[test]
fn preset_spring_creates_valid_animation() {
    let preset = AnimPreset::Spring {
        damping_ratio: 1.0,
        stiffness: 800.0,
        epsilon: 0.001,
    };

    if let AnimPreset::Spring { damping_ratio, stiffness, epsilon } = preset {
        let mut spring = SpringAnimation::new(stiffness, damping_ratio, epsilon);
        spring.set_position(0.0);
        spring.set_target(100.0);

        let dt = 1.0 / 120.0;
        for _ in 0..2000 {
            spring.tick(dt);
            if spring.is_complete() {
                break;
            }
        }
        assert!(spring.is_complete(), "spring from preset should settle");
    }
}
