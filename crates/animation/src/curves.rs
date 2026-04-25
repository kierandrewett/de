//! Cubic Bézier evaluation matching the CSS `cubic-bezier(x1, y1, x2, y2)` spec.

/// Evaluates a CSS cubic-bezier curve, returning the Y (progress) value for a given X (time) input.
///
/// The curve passes through (0,0) and (1,1). `x1`, `y1`, `x2`, `y2` are the two control points.
/// Uses Newton-Raphson to solve for the parameter `t` given `x`, then computes `y(t)`.
pub fn cubic_bezier(x1: f64, y1: f64, x2: f64, y2: f64, x: f64) -> f64 {
    if x <= 0.0 {
        return 0.0;
    }
    if x >= 1.0 {
        return 1.0;
    }

    // Binary search fallback tolerance
    const TOLERANCE: f64 = 1e-7;
    const MAX_ITER: usize = 10;

    let t = solve_t_for_x(x1, x2, x, TOLERANCE, MAX_ITER);
    bezier_component(y1, y2, t)
}

/// Computes one component of a cubic Bézier with end-points at 0 and 1.
///
/// The formula is: `3*(1-t)^2*t*p1 + 3*(1-t)*t^2*p2 + t^3` where p0=0, p3=1.
#[inline]
pub(crate) fn bezier_component(p1: f64, p2: f64, t: f64) -> f64 {
    let t2 = t * t;
    let t3 = t2 * t;
    let mt = 1.0 - t;
    let mt2 = mt * mt;
    3.0 * mt2 * t * p1 + 3.0 * mt * t2 * p2 + t3
}

/// Derivative of `bezier_component` with respect to `t`.
#[inline]
fn bezier_component_derivative(p1: f64, p2: f64, t: f64) -> f64 {
    let t2 = t * t;
    let mt = 1.0 - t;
    3.0 * (mt * mt - 2.0 * mt * t) * p1 + 3.0 * (2.0 * mt * t - t2) * p2 + 3.0 * t2
}

/// Solves for the Bézier parameter `t` such that `bezier_component(x1, x2, t) ≈ x`.
///
/// Uses Newton-Raphson with binary-search fallback.
fn solve_t_for_x(x1: f64, x2: f64, x: f64, tolerance: f64, max_iter: usize) -> f64 {
    let mut t = x; // initial guess

    for _ in 0..max_iter {
        let x_est = bezier_component(x1, x2, t);
        let error = x_est - x;
        if error.abs() < tolerance {
            return t;
        }
        let deriv = bezier_component_derivative(x1, x2, t);
        if deriv.abs() < 1e-12 {
            break;
        }
        t -= error / deriv;
        t = t.clamp(0.0, 1.0);
    }

    // Binary search fallback
    let mut lo = 0.0_f64;
    let mut hi = 1.0_f64;
    loop {
        t = (lo + hi) * 0.5;
        let x_est = bezier_component(x1, x2, t);
        let error = x_est - x;
        if error.abs() < tolerance || hi - lo < tolerance {
            return t;
        }
        if error < 0.0 {
            lo = t;
        } else {
            hi = t;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linear_is_identity() {
        // cubic-bezier(0,0,1,1) == linear
        for i in 0..=10 {
            let x = i as f64 / 10.0;
            let y = cubic_bezier(0.0, 0.0, 1.0, 1.0, x);
            assert!((y - x).abs() < 1e-6, "linear at {x}: got {y}");
        }
    }

    #[test]
    fn boundary_values() {
        let y0 = cubic_bezier(0.25, 0.1, 0.25, 1.0, 0.0);
        let y1 = cubic_bezier(0.25, 0.1, 0.25, 1.0, 1.0);
        assert!((y0 - 0.0).abs() < 1e-9);
        assert!((y1 - 1.0).abs() < 1e-9);
    }
}
