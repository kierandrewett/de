use animation::spring::SpringAnimation;

fn run_spring(spring: &mut SpringAnimation, max_secs: f64, dt: f64) -> bool {
    let steps = (max_secs / dt) as usize;
    for _ in 0..steps {
        spring.tick(dt);
        if spring.is_complete() {
            return true;
        }
    }
    false
}

#[test]
fn critically_damped_no_overshoot() {
    let mut spring = SpringAnimation::new(600.0, 1.0, 0.001);
    spring.set_position(0.0);
    spring.set_target(100.0);

    let dt = 1.0 / 120.0;
    let mut max_val = 0.0_f64;
    for _ in 0..3000 {
        let v = spring.tick(dt);
        if v > max_val {
            max_val = v;
        }
    }
    assert!(spring.is_complete(), "critically damped spring should settle");
    assert!(max_val <= 100.01, "critically damped should not overshoot: max={max_val}");
}

#[test]
fn underdamped_overshoots_and_settles() {
    let mut spring = SpringAnimation::new(600.0, 0.5, 0.001);
    spring.set_position(0.0);
    spring.set_target(100.0);

    let dt = 1.0 / 120.0;
    let mut overshot = false;
    for _ in 0..600 {
        let v = spring.tick(dt);
        if v > 100.5 {
            overshot = true;
            break;
        }
    }
    assert!(overshot, "underdamped spring (ratio=0.5) should overshoot");
    assert!(run_spring(&mut spring, 5.0, dt), "underdamped spring should eventually settle");
}

#[test]
fn set_target_with_velocity_increases_overshoot() {
    let mut s1 = SpringAnimation::new(400.0, 0.8, 0.001);
    s1.set_position(0.0);
    s1.set_target(100.0);

    let mut s2 = SpringAnimation::new(400.0, 0.8, 0.001);
    s2.set_position(0.0);
    s2.set_target_with_velocity(100.0, 500.0);

    let dt = 1.0 / 120.0;
    let mut max1 = 0.0_f64;
    let mut max2 = 0.0_f64;
    for _ in 0..600 {
        max1 = max1.max(s1.tick(dt));
        max2 = max2.max(s2.tick(dt));
    }
    assert!(
        max2 > max1,
        "spring with initial velocity should overshoot more: {max2} vs {max1}"
    );
}

#[test]
fn completion_flag() {
    let mut spring = SpringAnimation::new(600.0, 1.0, 0.001);
    spring.set_position(0.0);
    spring.set_target(100.0);
    assert!(!spring.is_complete());
    assert!(run_spring(&mut spring, 3.0, 1.0 / 120.0));
    assert!(spring.is_complete());
}

#[test]
fn stability_with_large_dt() {
    let mut spring = SpringAnimation::new(600.0, 1.0, 0.001);
    spring.set_position(0.0);
    spring.set_target(100.0);
    // 100ms frame drops
    for _ in 0..200 {
        spring.tick(0.1);
    }
    let pos = spring.position();
    assert!(pos.is_finite(), "position diverged: {pos}");
    assert!(
        pos > -500.0 && pos < 500.0,
        "position out of reasonable range: {pos}"
    );
}

#[test]
fn set_position_resets_velocity() {
    let mut spring = SpringAnimation::new(600.0, 0.5, 0.001);
    spring.set_position(0.0);
    spring.set_target_with_velocity(100.0, 1000.0);
    // Advance briefly to build up state
    spring.tick(0.05);
    // Jump to a position
    spring.set_position(50.0);
    assert!((spring.position() - 50.0).abs() < 1e-9);
    assert!((spring.velocity() - 0.0).abs() < 1e-9);
}

// Performance targets are release-mode only; skip in debug builds.
#[cfg_attr(debug_assertions, ignore)]
#[test]
fn performance_one_million_ticks() {
    let mut spring = SpringAnimation::new(600.0, 1.0, 0.0);
    spring.set_position(0.0);
    spring.set_target(100.0);

    let start = std::time::Instant::now();
    let dt = 1.0 / 60.0;
    for _ in 0..1_000_000 {
        spring.tick(dt);
    }
    let elapsed = start.elapsed();
    assert!(
        elapsed.as_millis() < 50,
        "1M spring ticks took {}ms, expected <50ms",
        elapsed.as_millis()
    );
}
