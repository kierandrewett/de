//! Fixed-duration easing animations with Bézier interpolation.

use std::time::Duration;

use crate::curves::cubic_bezier;

/// Named easing curves, including standard presets and custom CSS cubic-bezier.
#[derive(Debug, Clone, PartialEq)]
pub enum EasingCurve {
    /// Constant velocity.
    Linear,
    /// Slow start, fast end.
    EaseInQuad,
    /// Fast start, slow end.
    EaseOutQuad,
    /// Slow start and end.
    EaseInOutQuad,
    /// Slow start, fast end (cubic).
    EaseInCubic,
    /// Fast start, slow end (cubic).
    EaseOutCubic,
    /// Slow start and end (cubic).
    EaseInOutCubic,
    /// Very fast start, slow exponential tail.
    EaseOutExpo,
    /// Symmetric exponential ease.
    EaseInOutExpo,
    /// Slight overshoot at end — good for popups/menus.
    EaseOutBack,
    /// CSS `cubic-bezier(x1, y1, x2, y2)`.
    CubicBezier(f64, f64, f64, f64),
}

impl EasingCurve {
    /// Evaluates the curve at normalized time `t ∈ [0, 1]`, returning progress `∈ [0, 1]`.
    #[inline]
    pub fn evaluate(&self, t: f64) -> f64 {
        let t = t.clamp(0.0, 1.0);
        match self {
            Self::Linear => t,
            Self::EaseInQuad => t * t,
            Self::EaseOutQuad => t * (2.0 - t),
            Self::EaseInOutQuad => {
                if t < 0.5 {
                    2.0 * t * t
                } else {
                    -1.0 + (4.0 - 2.0 * t) * t
                }
            }
            Self::EaseInCubic => t * t * t,
            Self::EaseOutCubic => {
                let t1 = t - 1.0;
                t1 * t1 * t1 + 1.0
            }
            Self::EaseInOutCubic => {
                if t < 0.5 {
                    4.0 * t * t * t
                } else {
                    let t1 = 2.0 * t - 2.0;
                    0.5 * t1 * t1 * t1 + 1.0
                }
            }
            Self::EaseOutExpo => {
                if t >= 1.0 {
                    1.0
                } else {
                    1.0 - 2.0_f64.powf(-10.0 * t)
                }
            }
            Self::EaseInOutExpo => {
                if t <= 0.0 {
                    0.0
                } else if t >= 1.0 {
                    1.0
                } else if t < 0.5 {
                    2.0_f64.powf(20.0 * t - 10.0) / 2.0
                } else {
                    (2.0 - 2.0_f64.powf(-20.0 * t + 10.0)) / 2.0
                }
            }
            Self::EaseOutBack => cubic_bezier(0.34, 1.56, 0.64, 1.0, t),
            Self::CubicBezier(x1, y1, x2, y2) => cubic_bezier(*x1, *y1, *x2, *y2, t),
        }
    }
}

/// A fixed-duration animation that interpolates a scalar value using an [`EasingCurve`].
pub struct EasingAnimation {
    duration: Duration,
    curve: EasingCurve,
    start_value: f64,
    end_value: f64,
    elapsed: Duration,
    complete: bool,
}

impl EasingAnimation {
    /// Creates a new easing animation from `from` to `to` over `duration`.
    pub fn new(from: f64, to: f64, duration: Duration, curve: EasingCurve) -> Self {
        Self {
            duration,
            curve,
            start_value: from,
            end_value: to,
            elapsed: Duration::ZERO,
            complete: duration.is_zero(),
        }
    }

    /// Advances the animation by `dt` and returns the current interpolated value.
    #[inline]
    pub fn tick(&mut self, dt: Duration) -> f64 {
        if self.complete {
            return self.end_value;
        }
        self.elapsed += dt;
        if self.elapsed >= self.duration {
            self.elapsed = self.duration;
            self.complete = true;
        }
        self.value()
    }

    /// Returns the current interpolated value without advancing time.
    #[inline]
    pub fn value(&self) -> f64 {
        if self.complete {
            return self.end_value;
        }
        let t = if self.duration.is_zero() {
            1.0
        } else {
            self.elapsed.as_secs_f64() / self.duration.as_secs_f64()
        };
        let progress = self.curve.evaluate(t);
        self.start_value + (self.end_value - self.start_value) * progress
    }

    /// Returns `true` when the animation has finished.
    #[inline]
    pub fn is_complete(&self) -> bool {
        self.complete
    }

    /// Resets the animation to new start/end values, preserving duration and curve.
    pub fn reset(&mut self, from: f64, to: f64) {
        self.start_value = from;
        self.end_value = to;
        self.elapsed = Duration::ZERO;
        self.complete = self.duration.is_zero();
    }

    /// Returns the current value as `f32`.
    #[inline]
    pub fn value_f32(&self) -> f32 {
        self.value() as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linear_midpoint() {
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
    fn completes_exactly_at_end() {
        let mut anim = EasingAnimation::new(0.0, 100.0, Duration::from_millis(100), EasingCurve::Linear);
        anim.tick(Duration::from_millis(100));
        assert!(anim.is_complete());
        assert!((anim.value() - 100.0).abs() < 1e-9);
    }

    #[test]
    fn reset_restarts() {
        let mut anim = EasingAnimation::new(0.0, 100.0, Duration::from_millis(100), EasingCurve::Linear);
        anim.tick(Duration::from_millis(100));
        assert!(anim.is_complete());
        anim.reset(50.0, 200.0);
        assert!(!anim.is_complete());
        let v = anim.tick(Duration::from_millis(50));
        assert!((v - 125.0).abs() < 1e-9, "expected 125.0, got {v}");
    }

    #[test]
    fn zero_duration_is_immediately_complete() {
        let mut anim = EasingAnimation::new(0.0, 100.0, Duration::ZERO, EasingCurve::Linear);
        assert!(anim.is_complete());
        assert!((anim.tick(Duration::from_millis(16)) - 100.0).abs() < 1e-9);
    }
}
