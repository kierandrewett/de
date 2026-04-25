//! Multi-dimensional animated values backed by spring or easing animations.

use crate::spring::SpringAnimation;

/// An animated N-dimensional value, using one [`SpringAnimation`] per component.
pub struct AnimatedValue<const N: usize> {
    springs: [SpringAnimation; N],
}

/// Animated scalar (1-component).
pub type AnimatedFloat = AnimatedValue<1>;

/// Animated 2D point (x, y).
pub type AnimatedPoint = AnimatedValue<2>;

/// Animated rectangle (x, y, w, h).
pub type AnimatedRect = AnimatedValue<4>;

/// Animated colour (r, g, b, a).
pub type AnimatedColor = AnimatedValue<4>;

impl<const N: usize> AnimatedValue<N> {
    /// Creates a new spring-backed animated value with the given parameters.
    pub fn new_spring(stiffness: f64, damping_ratio: f64, epsilon: f64) -> Self {
        Self {
            springs: std::array::from_fn(|_| SpringAnimation::new(stiffness, damping_ratio, epsilon)),
        }
    }

    /// Advances all components by `dt` seconds. Returns the current positions.
    #[inline]
    pub fn tick(&mut self, dt: f64) -> [f64; N] {
        std::array::from_fn(|i| self.springs[i].tick(dt))
    }

    /// Sets new target positions for all components, preserving velocities.
    pub fn set_target(&mut self, target: [f64; N]) {
        for (i, spring) in self.springs.iter_mut().enumerate() {
            spring.set_target(target[i]);
        }
    }

    /// Sets new target positions with per-component initial velocities.
    pub fn set_target_with_velocity(&mut self, target: [f64; N], velocity: [f64; N]) {
        for (i, spring) in self.springs.iter_mut().enumerate() {
            spring.set_target_with_velocity(target[i], velocity[i]);
        }
    }

    /// Jumps to a position instantly, resetting all velocities.
    pub fn set_position(&mut self, position: [f64; N]) {
        for (i, spring) in self.springs.iter_mut().enumerate() {
            spring.set_position(position[i]);
        }
    }

    /// Returns the current positions of all components.
    #[inline]
    pub fn position(&self) -> [f64; N] {
        std::array::from_fn(|i| self.springs[i].position())
    }

    /// Returns the current positions as `f32`.
    #[inline]
    pub fn position_f32(&self) -> [f32; N] {
        std::array::from_fn(|i| self.springs[i].position_f32())
    }

    /// Returns `true` when all components have settled.
    #[inline]
    pub fn is_complete(&self) -> bool {
        self.springs.iter().all(|s| s.is_complete())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn animated_rect_all_components_move() {
        let mut rect: AnimatedRect = AnimatedValue::new_spring(600.0, 1.0, 0.001);
        rect.set_position([0.0, 0.0, 100.0, 50.0]);
        rect.set_target([200.0, 150.0, 300.0, 200.0]);

        let initial = rect.position();
        let after = rect.tick(1.0 / 60.0);

        for i in 0..4 {
            assert!(
                (after[i] - initial[i]).abs() > 0.0,
                "component {i} did not move"
            );
        }
    }

    #[test]
    fn animated_float_settles() {
        let mut f: AnimatedFloat = AnimatedValue::new_spring(600.0, 1.0, 0.001);
        f.set_position([0.0]);
        f.set_target([100.0]);

        let dt = 1.0 / 120.0;
        for _ in 0..2000 {
            f.tick(dt);
            if f.is_complete() {
                break;
            }
        }
        assert!(f.is_complete());
        assert!((f.position()[0] - 100.0).abs() < 0.01);
    }

    #[test]
    fn position_f32_conversion() {
        let mut p: AnimatedPoint = AnimatedValue::new_spring(800.0, 1.0, 0.001);
        p.set_position([10.0, 20.0]);
        let f32_pos = p.position_f32();
        assert!((f32_pos[0] - 10.0_f32).abs() < 1e-5);
        assert!((f32_pos[1] - 20.0_f32).abs() < 1e-5);
    }
}
