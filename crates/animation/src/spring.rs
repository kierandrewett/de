//! Spring animation modelled as a damped harmonic oscillator.
//!
//! Uses semi-implicit (symplectic) Euler integration for numerical stability.
//! Large `dt` values are automatically subdivided into 1 ms steps.

/// Maximum integration step to prevent instability on frame drops.
const MAX_STEP_SECS: f64 = 0.001; // 1 ms

/// A physically-modelled spring animation.
///
/// The equation of motion is:
/// `x'' = -stiffness * (x - target) - damping * x'`
/// where `damping = damping_ratio * 2 * sqrt(stiffness * mass)`.
pub struct SpringAnimation {
    stiffness: f64,
    damping_ratio: f64,
    mass: f64,
    /// Completion threshold — done when `|pos - target| < epsilon && |vel| < epsilon`.
    epsilon: f64,
    position: f64,
    velocity: f64,
    target: f64,
    complete: bool,
}

impl SpringAnimation {
    /// Creates a new spring with the given parameters.
    ///
    /// `mass` is set to `1.0`. Use `new_with_mass` for explicit mass control.
    pub fn new(stiffness: f64, damping_ratio: f64, epsilon: f64) -> Self {
        Self::new_with_mass(stiffness, damping_ratio, 1.0, epsilon)
    }

    /// Creates a new spring with an explicit mass parameter.
    pub fn new_with_mass(stiffness: f64, damping_ratio: f64, mass: f64, epsilon: f64) -> Self {
        Self {
            stiffness,
            damping_ratio,
            mass,
            epsilon,
            position: 0.0,
            velocity: 0.0,
            target: 0.0,
            complete: true, // starts at rest
        }
    }

    /// Advances the spring by `dt` seconds, subdividing into 1 ms steps if needed.
    /// Returns the current position.
    #[inline]
    pub fn tick(&mut self, dt: f64) -> f64 {
        if self.complete || dt <= 0.0 {
            return self.position;
        }

        let damping = self.damping_ratio * 2.0 * (self.stiffness * self.mass).sqrt();

        let mut remaining = dt;
        while remaining > 0.0 {
            let step = remaining.min(MAX_STEP_SECS);
            remaining -= step;
            self.integrate_step(damping, step);
        }

        self.check_completion();
        self.position
    }

    #[inline]
    fn integrate_step(&mut self, damping: f64, dt: f64) {
        let spring_force = -self.stiffness * (self.position - self.target);
        let damping_force = -damping * self.velocity;
        let acceleration = (spring_force + damping_force) / self.mass;
        self.velocity += acceleration * dt;
        self.position += self.velocity * dt;
    }

    #[inline]
    fn check_completion(&mut self) {
        if (self.position - self.target).abs() < self.epsilon
            && self.velocity.abs() < self.epsilon
        {
            self.position = self.target;
            self.velocity = 0.0;
            self.complete = true;
        }
    }

    /// Sets a new target, preserving current position and velocity.
    pub fn set_target(&mut self, target: f64) {
        self.target = target;
        // Wake the spring if it was at rest
        if self.complete && (self.position - target).abs() >= self.epsilon {
            self.complete = false;
        }
    }

    /// Sets a new target with an initial velocity (e.g. gesture fling release).
    pub fn set_target_with_velocity(&mut self, target: f64, velocity: f64) {
        self.target = target;
        self.velocity = velocity;
        self.complete = false;
    }

    /// Jumps to a position instantly, resetting velocity.
    pub fn set_position(&mut self, position: f64) {
        self.position = position;
        self.velocity = 0.0;
        self.complete = (position - self.target).abs() < self.epsilon;
    }

    /// Returns the current position.
    #[inline]
    pub fn position(&self) -> f64 {
        self.position
    }

    /// Returns the current velocity.
    #[inline]
    pub fn velocity(&self) -> f64 {
        self.velocity
    }

    /// Returns `true` when the spring has settled within `epsilon` of its target.
    #[inline]
    pub fn is_complete(&self) -> bool {
        self.complete
    }

    /// Returns the current position as `f32`.
    #[inline]
    pub fn position_f32(&self) -> f32 {
        self.position as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_to_completion(spring: &mut SpringAnimation, max_secs: f64) -> bool {
        let dt = 1.0 / 120.0;
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
    fn critically_damped_settles_without_overshoot() {
        let mut spring = SpringAnimation::new(600.0, 1.0, 0.001);
        spring.set_position(0.0);
        spring.set_target(100.0);

        let mut max_val = 0.0_f64;
        let dt = 1.0 / 120.0;
        for _ in 0..2000 {
            let v = spring.tick(dt);
            if v > max_val {
                max_val = v;
            }
        }
        assert!(spring.is_complete(), "spring should settle");
        // critically damped must not overshoot
        assert!(max_val <= 100.0 + 0.01, "overshoot: max={max_val}");
    }

    #[test]
    fn underdamped_oscillates() {
        let mut spring = SpringAnimation::new(600.0, 0.5, 0.001);
        spring.set_position(0.0);
        spring.set_target(100.0);

        let mut overshot = false;
        let dt = 1.0 / 120.0;
        for _ in 0..600 {
            let v = spring.tick(dt);
            if v > 100.5 {
                overshot = true;
                break;
            }
        }
        assert!(overshot, "underdamped spring should overshoot");
        assert!(run_to_completion(&mut spring, 5.0), "should eventually settle");
    }

    #[test]
    fn velocity_increases_overshoot() {
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
        assert!(max2 > max1, "velocity should increase overshoot: {max2} vs {max1}");
    }

    #[test]
    fn completion_reported() {
        let mut spring = SpringAnimation::new(600.0, 1.0, 0.001);
        spring.set_position(0.0);
        spring.set_target(100.0);
        assert!(run_to_completion(&mut spring, 3.0), "should complete");
        assert!(spring.is_complete());
    }

    #[test]
    fn large_dt_stable() {
        let mut spring = SpringAnimation::new(600.0, 1.0, 0.001);
        spring.set_position(0.0);
        spring.set_target(100.0);
        // Simulate 100ms frame drop
        for _ in 0..100 {
            spring.tick(0.1);
        }
        let pos = spring.position();
        assert!(pos.is_finite(), "position must be finite after large dt");
        assert!(pos >= -1000.0 && pos <= 1100.0, "position diverged: {pos}");
    }
}
