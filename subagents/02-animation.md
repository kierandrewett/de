# SUBAGENT: Animation Engine
# Crate: `crates/animation`

> **Read `ARCHITECTURE.md` in the repo root before starting.** It explains why we use Smithay (not wlroots), how the compositor rendering pipeline works, the full Wayland protocol reference, smithay API patterns (delegate pattern, GlesRenderer, calloop), and the winit dev backend.
# Branch: `feat/animation`

You are building a zero-dependency Rust animation engine for a Wayland compositor. It supports two animation types: **easing** (fixed duration, Bézier interpolation) and **spring** (physically-modelled, velocity-aware). Every visual state change in the compositor uses this — window open/close, minimize, maximize, snap, focus, workspace switch.

## Crate Structure
```
crates/animation/
├── Cargo.toml
├── src/
│   ├── lib.rs
│   ├── easing.rs        # EasingAnimation + EasingCurve
│   ├── spring.rs        # SpringAnimation (damped harmonic oscillator)
│   ├── animated.rs      # AnimatedValue<N>, AnimatedPoint, AnimatedRect, etc
│   ├── presets.rs       # Window animation preset configs
│   └── curves.rs        # Cubic Bézier evaluation, standard curves
└── tests/
    ├── easing_tests.rs
    ├── spring_tests.rs
    └── preset_tests.rs
```

## Easing Animation

```rust
pub struct EasingAnimation {
    duration: Duration,
    curve: EasingCurve,
    start_value: f64,
    end_value: f64,
    elapsed: Duration,
    complete: bool,
}

pub enum EasingCurve {
    Linear,
    EaseInQuad, EaseOutQuad, EaseInOutQuad,
    EaseInCubic, EaseOutCubic, EaseInOutCubic,
    EaseOutExpo, EaseInOutExpo,
    EaseOutBack,  // slight overshoot — good for popups/menus
    CubicBezier(f64, f64, f64, f64),  // CSS cubic-bezier(x1, y1, x2, y2)
}

impl EasingAnimation {
    pub fn new(from: f64, to: f64, duration: Duration, curve: EasingCurve) -> Self;
    pub fn tick(&mut self, dt: Duration) -> f64;  // advance, return current value
    pub fn value(&self) -> f64;                    // current interpolated value
    pub fn is_complete(&self) -> bool;
    pub fn reset(&mut self, from: f64, to: f64);   // reuse without realloc
}
```

Implement the cubic Bézier evaluation using De Casteljau's algorithm or Newton-Raphson for the CSS `cubic-bezier(x1, y1, x2, y2)` mapping (x=time, y=progress). This is the same algorithm browsers use.

## Spring Animation

Model a damped harmonic oscillator. The equation of motion:
```
x'' = -stiffness * (x - target) - damping * x'
```
Where `damping = damping_ratio * 2 * sqrt(stiffness * mass)`.

```rust
pub struct SpringAnimation {
    stiffness: f64,
    damping_ratio: f64,
    mass: f64,
    epsilon: f64,         // completion threshold
    position: f64,
    velocity: f64,
    target: f64,
    complete: bool,
}

impl SpringAnimation {
    pub fn new(stiffness: f64, damping_ratio: f64, epsilon: f64) -> Self;

    /// Advance the spring by dt seconds. Returns current position.
    pub fn tick(&mut self, dt: f64) -> f64;

    /// Set a new target, preserving current velocity.
    pub fn set_target(&mut self, target: f64);

    /// Set a new target with an initial velocity (gesture fling release).
    pub fn set_target_with_velocity(&mut self, target: f64, velocity: f64);

    /// Jump to a position instantly, resetting velocity.
    pub fn set_position(&mut self, position: f64);

    pub fn position(&self) -> f64;
    pub fn velocity(&self) -> f64;
    pub fn is_complete(&self) -> bool;
}
```

**Integration method:** Use semi-implicit Euler (symplectic Euler) for numerical stability:
```
let spring_force = -stiffness * (position - target);
let damping_force = -damping * velocity;
let acceleration = (spring_force + damping_force) / mass;
velocity += acceleration * dt;
position += velocity * dt;
```

For critically damped case (damping_ratio = 1.0), you MAY use the exact analytical solution for better accuracy, but semi-implicit Euler is fine if dt is small enough (1/60s or better).

**Completion:** The spring is complete when `|position - target| < epsilon && |velocity| < epsilon`.

## Multi-dimensional Animated Values

```rust
/// An animated value with N components, using the same animation type for all.
pub struct AnimatedValue<const N: usize> {
    springs: [SpringAnimation; N],  // or could be easing
}

pub type AnimatedFloat = AnimatedValue<1>;
pub type AnimatedPoint = AnimatedValue<2>;   // x, y
pub type AnimatedRect = AnimatedValue<4>;    // x, y, w, h
pub type AnimatedColor = AnimatedValue<4>;   // r, g, b, a

impl<const N: usize> AnimatedValue<N> {
    pub fn new_spring(stiffness: f64, damping_ratio: f64, epsilon: f64) -> Self;
    pub fn tick(&mut self, dt: f64) -> [f64; N];
    pub fn set_target(&mut self, target: [f64; N]);
    pub fn set_target_with_velocity(&mut self, target: [f64; N], velocity: [f64; N]);
    pub fn position(&self) -> [f64; N];
    pub fn is_complete(&self) -> bool;
}
```

## Window Animation Presets

```rust
pub struct WindowAnimationPresets {
    pub open: AnimPreset,
    pub close: AnimPreset,
    pub minimize: AnimPreset,
    pub unminimize: AnimPreset,
    pub maximize: AnimPreset,
    pub unmaximize: AnimPreset,
    pub snap: AnimPreset,
    pub focus_pulse: AnimPreset,
    pub move_follow: AnimPreset,
}

pub enum AnimPreset {
    Easing { duration_ms: u32, curve: EasingCurve },
    Spring { damping_ratio: f64, stiffness: f64, epsilon: f64 },
}

impl Default for WindowAnimationPresets {
    fn default() -> Self {
        Self {
            open: AnimPreset::Easing { duration_ms: 250, curve: EasingCurve::EaseOutExpo },
            close: AnimPreset::Easing { duration_ms: 200, curve: EasingCurve::EaseOutQuad },
            minimize: AnimPreset::Spring { damping_ratio: 1.0, stiffness: 600.0, epsilon: 0.001 },
            unminimize: AnimPreset::Spring { damping_ratio: 1.0, stiffness: 600.0, epsilon: 0.001 },
            maximize: AnimPreset::Spring { damping_ratio: 1.0, stiffness: 800.0, epsilon: 0.001 },
            unmaximize: AnimPreset::Spring { damping_ratio: 1.0, stiffness: 800.0, epsilon: 0.001 },
            snap: AnimPreset::Spring { damping_ratio: 1.0, stiffness: 800.0, epsilon: 0.001 },
            focus_pulse: AnimPreset::Easing { duration_ms: 150, curve: EasingCurve::EaseInOutCubic },
            move_follow: AnimPreset::Spring { damping_ratio: 0.8, stiffness: 1200.0, epsilon: 0.01 },
        }
    }
}
```

## Cargo.toml
```toml
[package]
name = "animation"
version = "0.1.0"
edition = "2021"

# Zero dependencies — pure math
[dependencies]

[dev-dependencies]
```

## Requirements
- No allocations in `tick()` — everything pre-allocated in struct fields
- `#[inline]` on `tick()`, `value()`, `is_complete()`
- Must handle variable dt (not tied to 60fps — support 144Hz+, and also handle frame drops where dt might be 33ms+)
- All f64 internally for precision; expose f32 conversion helpers
- Spring: handle dt > 16ms by subdividing into 1ms steps (prevents instability with large dt)
- No `unsafe`
- No panics in any code path

## Tests
1. Easing Linear: tick from 0→100 over 1s, at 0.5s value should be 50.0
2. Easing EaseOutExpo: at 50% time, progress should be >90% (fast start)
3. Spring critically damped: from 0→100, should settle within reasonable time, never overshoot
4. Spring underdamped (ratio=0.5): should overshoot target, then oscillate and settle
5. Spring with velocity: set_target_with_velocity(100, 500) should overshoot more than set_target(100)
6. Multi-dimensional: AnimatedRect tick should update all 4 components
7. Completion: spring should report is_complete() when settled
8. Performance: 1M ticks should complete in < 50ms
9. Stability: spring with dt=0.1s (100ms frame drop) should not diverge

## Work iteratively
1. Implement EasingCurve evaluation (start with Linear, then standard curves, then CubicBezier)
2. Implement EasingAnimation
3. Implement SpringAnimation with semi-implicit Euler
4. Implement AnimatedValue<N>
5. Add presets
6. Tests throughout
7. Polish: clippy, docs
