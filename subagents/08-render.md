# SUBAGENT: Compositor Rendering Pipeline
# Crate: `crates/compositor` (module: `src/render/`)

> **Read `ARCHITECTURE.md` in the repo root before starting.** It explains why we use Smithay (not wlroots), how the compositor rendering pipeline works, the full Wayland protocol reference, smithay API patterns (delegate pattern, GlesRenderer, calloop), and the winit dev backend.

> **Also read `WINDOW_SPEC.md`** — it contains the pixel-perfect border, shadow, and inner highlight specifications with exact rgba values, render order, and visual verification checklist.
# Branch: `feat/compositor-render`

You are building the per-window rendering pipeline inside the compositor. This ties together squircle rounding, macOS-style borders, shadows, and SSD/CSD decoration compositing.

## Dependencies (from Phase 1 crates)
- `rounding` — SquirclePath generation
- `theme` — WindowBorderStyle, shadow layers, colour tokens
- `animation` — WindowAnimState (animated geometry, opacity, scale)

## Module Structure
```
src/render/
├── mod.rs              # WindowRenderer, render loop integration
├── border.rs           # macOS multi-layer border rendering
├── shadow.rs           # Shadow texture generation + rendering
├── clip.rs             # Squircle clipping (stencil + SDF shader)
├── decoration.rs       # SSD title bar rendering via iced IcedElement
└── effects.rs          # Background blur, alpha modifier application
```

## Per-window render flow
```
for each window (back to front):
    skip if minimized + animation complete
    
    let geo = window.animation.rect.current()    // animated rect
    let opacity = window.animation.opacity.current()
    let scale = window.animation.scale.current()
    
    if fullscreen: draw raw client texture, skip decorations
    
    // 1. Shadow (multi-layer gaussian behind window)
    let border_style = theme.border_style(is_dark, is_focused);
    render_shadow(renderer, geo, border_style.shadow_layers)
    
    // 2. Outer stroke (1px along squircle path)
    let path = SquirclePath::new(geo, theme.squircle_config)
    stroke_squircle(renderer, path, 1.0, border_style.outer_stroke)
    
    // 3. Window content
    match decoration_mode {
        SSD => {
            render_iced_titlebar(renderer, geo, window)  // via IcedElement
            clip_and_draw(renderer, client_texture, geo.content_rect(), path, opacity)
        }
        CSD => {
            clip_and_draw(renderer, client_texture, geo, path, opacity)
        }
    }
    
    // 4. Inner highlight (inset 1px, gradient: strong top, fading sides)
    stroke_squircle_inset(renderer, path, border_style.inner_highlight_top)
```

## Squircle clipping — two strategies

**Strategy A: Stencil buffer** — simpler, works with GlesRenderer
```
glEnable(GL_STENCIL_TEST);
// Draw squircle fill into stencil
glStencilFunc(GL_ALWAYS, 1, 0xFF);
glStencilOp(GL_REPLACE, GL_REPLACE, GL_REPLACE);
draw_squircle_fill(geo);
// Draw client texture, clipped by stencil
glStencilFunc(GL_EQUAL, 1, 0xFF);
draw_textured_quad(client_texture, geo);
glDisable(GL_STENCIL_TEST);
```

**Strategy B: SDF fragment shader** — smoother, anti-aliased edges
```wgsl
fn squircle_sdf(p: vec2<f32>, rect: vec4<f32>, radius: f32, smoothing: f32) -> f32 {
    // Compute signed distance to squircle boundary
    // Negative inside, positive outside
}
// In fragment:
let dist = squircle_sdf(frag_coord, window_rect, r, smoothing);
let alpha = 1.0 - smoothstep(-1.0, 1.0, dist);
return vec4(color.rgb, color.a * alpha);
```

Implement both, feature-gated. Use SDF by default.

## Shadow rendering
Pre-compute a shadow texture: render squircle fill, apply gaussian blur, cache. Use 9-slice for scaling to any window size. Shadow layers from theme (multiple blur passes at different radii/offsets for depth).

## SSD decorations via iced
Use the COSMIC `IcedElement` pattern: render an iced widget tree (title bar with close/maximize/minimize buttons + title text) into a GPU texture. Composite that texture above the client surface. Handle mouse hit-testing for button clicks.

## Frozen texture capture
For minimize/alt-tab: capture the window's composited frame (decorations + client) into a cached texture. Use GL framebuffer readback or render-to-texture.

## Performance requirements
- No allocations per frame for static geometry
- Cache squircle paths: invalidate only on resize/theme change
- Shadow textures cached per (window_size, shadow_config) pair
- SSD iced decoration textures cached, re-rendered only on title/focus change
- Target: 1000 windows composited in <16ms at 1080p

Work iteratively: shadow first, then squircle clipping, then borders, then SSD decorations.
