# SUBAGENT: Squircle Corner Rounding Engine
# Crate: `crates/rounding`

> **Read `ARCHITECTURE.md` in the repo root before starting.** It explains why we use Smithay (not wlroots), how the compositor rendering pipeline works, the full Wayland protocol reference, smithay API patterns (delegate pattern, GlesRenderer, calloop), and the winit dev backend.
# Branch: `feat/rounding`

You are building a Rust crate that generates continuous-curvature rounded rectangle paths matching macOS/iOS corner rounding. This is NOT standard circular-arc rounding — it uses cubic Bézier curves with smooth curvature transitions (G2 continuity).

## Why this matters
A standard `border-radius` rounds corners with a circular arc. The curvature jumps from 0 (straight edge) to 1/r (arc) instantly — a discontinuity that looks "surgical." macOS uses a superellipse approximation where curvature ramps up smoothly, creating corners that feel organic and premium. This is sometimes called a "squircle."

## Algorithm

Each corner is composed of three segments: **entry Bézier** → **circular arc** (optional) → **exit Bézier**.

A `smoothing` parameter ξ ∈ [0.0, 1.0] controls how much of the corner is Bézier vs arc:
- ξ = 0.0 → pure circular arc (standard CSS border-radius)
- ξ = 0.6 → Apple's default (recommended)
- ξ = 1.0 → no arc segment, all Bézier (maximum smoothness)

### Per-corner construction (90° corner):

Given corner_radius `r`, rect dimensions `(w, h)`, and smoothing `ξ`:

1. **Compute reach `p`** — how far the curve extends along the straight edge from the geometric corner point:
   ```
   p = min(r * (1.0 + ξ * 0.7), min(w, h) / 2.0)
   ```
   Clamped so adjacent corners never overlap.

2. **Compute arc reduction** — the standard circular arc spans from the tangent point on one edge to the tangent point on the other. With smoothing, the arc shrinks:
   ```
   arc_extent = r * (1.0 - ξ)  // at ξ=1, arc vanishes
   ```

3. **Entry Bézier curve (cubic):**
   - Start: on the straight edge, at distance `p` from the corner
   - End: at the start of the remaining arc segment
   - Control point 1: positioned so the curve is tangent to the straight edge at the start (curvature = 0)
   - Control point 2: positioned so the curve is tangent to the arc at the end (curvature = 1/r)
   - The specific control point distances use constants derived from matching Apple's output:
     - `d1 = p * 0.4475` (distance from start to ctrl1 along edge direction)
     - `d2 = (p - arc_extent) * 0.5523` (distance from end to ctrl2, approximating circular arc tangent)

4. **Arc segment** (if ξ < 1.0): the remaining central portion of the circle.

5. **Exit Bézier curve:** mirror of entry, transitioning back to the other straight edge.

### Path construction order (clockwise from top-left):
```
MoveTo(top-left corner entry point on top edge)
for each corner (TL, TR, BR, BL):
    CubicTo(entry Bézier)
    ArcTo(remaining arc, if any)
    CubicTo(exit Bézier)
    LineTo(next corner's entry point)
Close
```

## Crate Structure

```
crates/rounding/
├── Cargo.toml
├── src/
│   ├── lib.rs          # public API, re-exports
│   ├── path.rs         # SquirclePath generation
│   ├── config.rs       # SquircleConfig
│   ├── commands.rs     # PathCommand enum
│   ├── math.rs         # helper math (angle, lerp, etc)
│   ├── tiny_skia.rs    # tiny-skia Path conversion (feature-gated)
│   ├── tessellate.rs   # Bézier tessellation to vertices
│   ├── sdf.rs          # SDF WGSL shader generation
│   └── svg.rs          # SVG path data export
└── tests/
    ├── basic.rs        # path generation tests
    ├── edge_cases.rs   # degenerate rects, huge radii
    ├── visual.rs       # generate SVG files for visual inspection
    └── apple_compat.rs # compare against known Apple control points
```

## Public API

```rust
#![deny(missing_docs)]

/// Configuration for continuous-curvature rounding.
#[derive(Debug, Clone, Copy)]
pub struct SquircleConfig {
    /// Corner radius in pixels.
    pub corner_radius: f32,
    /// Smoothing factor. 0.0 = standard rounded rect, 0.6 = macOS default, 1.0 = full squircle.
    pub smoothing: f32,
}

impl Default for SquircleConfig {
    fn default() -> Self {
        Self { corner_radius: 10.0, smoothing: 0.6 }
    }
}

/// A path command in the squircle path.
#[derive(Debug, Clone, Copy)]
pub enum PathCommand {
    MoveTo(f32, f32),
    LineTo(f32, f32),
    CubicTo { ctrl1: (f32, f32), ctrl2: (f32, f32), end: (f32, f32) },
    ArcTo { center: (f32, f32), radius: f32, start_angle: f32, sweep_angle: f32 },
    Close,
}

/// A generated squircle path.
#[derive(Debug, Clone)]
pub struct SquirclePath {
    pub commands: Vec<PathCommand>,
}

impl SquirclePath {
    /// Generate a squircle path for a rectangle with uniform corner config.
    pub fn new(x: f32, y: f32, width: f32, height: f32, config: SquircleConfig) -> Self;

    /// Generate with per-corner configuration (like CSS border-radius shorthand).
    pub fn with_corners(
        x: f32, y: f32, width: f32, height: f32,
        top_left: SquircleConfig, top_right: SquircleConfig,
        bottom_right: SquircleConfig, bottom_left: SquircleConfig,
    ) -> Self;

    /// Convert to an SVG path data string (for debugging/export).
    pub fn to_svg_path_data(&self) -> String;

    /// Tessellate all curves into line segments for GPU vertex submission.
    /// `tolerance` controls the maximum deviation from the true curve (in pixels).
    /// Typical value: 0.25 for screen rendering.
    pub fn tessellate(&self, tolerance: f32) -> Vec<[f32; 2]>;

    /// Generate WGSL fragment shader code for an SDF of this squircle shape.
    /// The SDF returns negative inside, positive outside.
    /// Use with smoothstep for anti-aliased clipping.
    pub fn sdf_wgsl(config: &SquircleConfig) -> String;
}

// Feature-gated conversions
#[cfg(feature = "tiny-skia")]
impl SquirclePath {
    /// Convert to a tiny-skia Path for CPU rasterisation.
    pub fn to_tiny_skia_path(&self) -> tiny_skia::Path;
}
```

## Cargo.toml

```toml
[package]
name = "rounding"
version = "0.1.0"
edition = "2021"

[dependencies]
tiny-skia = { workspace = true, optional = true }

[features]
default = []
tiny-skia = ["dep:tiny-skia"]

[dev-dependencies]
tiny-skia = { workspace = true }
```

## Edge Cases to Handle
- `corner_radius` > `min(width, height) / 2` → clamp radius
- Rect where adjacent corners overlap → scale all radii proportionally
- Zero-size rect → return empty path
- Negative dimensions → treat as absolute values
- Per-corner configs with different radii → each corner computed independently
- Very small rects (< 4px) → degenerate to a simple rounded rect (skip Bézier complexity)

## Tests

1. **Symmetry test:** A square with uniform config should produce 4 identical corners (rotated).
2. **Smoothing=0 test:** Output should match a standard rounded rect (all CubicTo should approximate circular arcs, or produce ArcTo commands only).
3. **Smoothing=1 test:** No ArcTo commands should appear.
4. **Clamp test:** Radius of 100 on a 50×50 rect should clamp to 25.
5. **Visual export test:** Generate an SVG with squircles at smoothing 0.0, 0.2, 0.4, 0.6, 0.8, 1.0 side by side. Write to `tests/output/squircle_comparison.svg`. Visually inspect that curvature increases with smoothing.
6. **Performance test:** Generate 10,000 paths of random sizes. Assert total time < 10ms.
7. **Tessellation test:** Tessellated points should form a closed polygon. All points should lie within 0.5px of the true curve.
8. **SDF WGSL test:** The generated shader code should be valid WGSL (parse it with `naga` if available, or just validate syntax).

## Work iteratively
1. Start with the basic path generation for a single uniform-radius rect
2. Add per-corner support
3. Add SVG export and write visual tests
4. Add tessellation
5. Add SDF shader generation
6. Add tiny-skia conversion
7. Polish: clippy, docs, edge cases
