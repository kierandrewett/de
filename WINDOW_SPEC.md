# WINDOW DECORATION SPEC — Pixel-Perfect Reference
# This file is referenced by subagents 05-theme.md and 08-render.md
# Source: Kieran's Figma designs + macOS Sequoia border/shadow analysis

This is the single source of truth for window decoration rendering. Every value here has been extracted from either the Figma designs (layout, radius, controls) or macOS Sequoia (borders, shadows, inner highlights). The compositor's render pipeline MUST reproduce these values exactly.

---

## Geometry (from Figma)

```
Corner radius (outer):     14px
Corner radius (inner):     12px  (outer - border width - content inset)
Corner smoothing:          0.6   (macOS-style squircle, not circular arc)
Title bar height:          33px  (standard), 43px (tall variant)
Title bar padding:         6px
Window controls position:  RIGHT side (not macOS left-side traffic lights)
Window controls size:      21×21px each
Window controls gap:       4px between buttons
Window controls inset:     6px from top, 6px from right edge
Title text:                Inter Semi Bold, 13px, centred in title bar
```

### Window control buttons (right-to-left: minimize, maximize, close)

**Light mode controls:**
- Minimize: line icon, stroke `rgba(0,0,0,0.8)`
- Maximize: 9×9px rounded rect (`border-radius: 3px`), stroke `rgba(0,0,0,0.8)`, bg `rgba(0,0,0,0.1)`, pill container `border-radius: 21px`
- Close: × icon, stroke `rgba(0,0,0,0.8)`

**Dark mode controls:**
- Same layout, colours inverted: stroke `rgba(255,255,255,0.8)`, maximize bg `rgba(255,255,255,0.1)`

---

## Colours (from Figma)

### SSD Title bar
| Property | Light Active | Light Inactive | Dark Active | Dark Inactive |
|---|---|---|---|---|
| Background | `#FFFFFF` | `rgba(255,255,255,0.5)` | `#111318` | `#111318` at 75% opacity |
| Bottom border | `0.5px solid #BBBBBB` | `0.5px solid #BBBBBB` | `0.5px solid rgba(0,0,0,0.8)` | `0.5px solid rgba(0,0,0,0.8)` |
| Title text | `rgba(0,0,0,0.75)` | `rgba(0,0,0,0.75)` | `rgba(255,255,255,0.8)` | `rgba(255,255,255,0.8)` |
| Overall opacity | 1.0 | 0.75 | 1.0 | 0.75 |

### Window content area background
| Light | Dark |
|---|---|
| `#F6F6F6` | `#1D1D1D` |

---

## Borders (from macOS Sequoia — NOT from Figma)

macOS windows have a three-layer border system that creates depth through simulated lighting.

### Outer border (the defining edge)
A very thin stroke that separates the window from the desktop background.

| State | Value |
|---|---|
| **Light active** | `0.5px solid rgba(0, 0, 0, 0.22)` |
| **Light inactive** | `0.5px solid rgba(0, 0, 0, 0.15)` |
| **Dark active** | `0.5px solid rgba(0, 0, 0, 0.72)` |
| **Dark inactive** | `0.5px solid rgba(0, 0, 0, 0.55)` |

### Inner highlight (simulates top-down lighting on glass)
This is the signature macOS touch. It's a **non-uniform** inset stroke — brighter on top, fading on the sides, absent on the bottom. This creates the illusion that the window is a physical surface catching light from above.

**Implementation:** Render as 4 separate inset edge strokes with different alpha values, following the squircle path.

| Edge | Light Active | Light Inactive | Dark Active | Dark Inactive |
|---|---|---|---|---|
| **Top** | `1px inset rgba(255,255,255, 0.50)` | `1px inset rgba(255,255,255, 0.25)` | `1px inset rgba(255,255,255, 0.08)` | `1px inset rgba(255,255,255, 0.04)` |
| **Left** | `1px inset rgba(255,255,255, 0.18)` | `1px inset rgba(255,255,255, 0.09)` | `1px inset rgba(255,255,255, 0.03)` | `1px inset rgba(255,255,255, 0.015)` |
| **Right** | `1px inset rgba(255,255,255, 0.18)` | `1px inset rgba(255,255,255, 0.09)` | `1px inset rgba(255,255,255, 0.03)` | `1px inset rgba(255,255,255, 0.015)` |
| **Bottom** | `1px inset rgba(255,255,255, 0.0)` | `1px inset rgba(255,255,255, 0.0)` | `1px inset rgba(255,255,255, 0.0)` | `1px inset rgba(255,255,255, 0.0)` |

**Rendering approach:** Rather than 4 separate strokes, you can render a single 1px inset squircle stroke where the alpha is modulated by a vertical gradient:
```
alpha = base_alpha * clamp(1.0 - (y_normalised * 1.5), 0.0, 1.0)
```
Where `y_normalised` goes from 0.0 (top of window) to 1.0 (bottom of window). This gives:
- Top: full alpha
- Upper third: fading
- Lower two thirds: zero

This is more elegant than per-edge strokes and handles corners smoothly.

---

## Shadow (from macOS Sequoia — NOT from Figma)

macOS uses a multi-layer shadow system that changes between active and inactive states. The shadow is softer and more layered than most Linux compositors implement.

### Active window shadow (3 layers)
```
Layer 1 (tight contact shadow):   0px  1px   3px  0px  rgba(0, 0, 0, 0.12)
Layer 2 (medium spread):          0px  8px  24px  0px  rgba(0, 0, 0, 0.12)
Layer 3 (wide ambient):           0px 20px  48px  0px  rgba(0, 0, 0, 0.08)
```

### Inactive window shadow (2 layers, much softer)
```
Layer 1 (tight contact shadow):   0px  1px   2px  0px  rgba(0, 0, 0, 0.08)
Layer 2 (medium spread):          0px  4px  12px  0px  rgba(0, 0, 0, 0.06)
```

**Note:** The same shadow values are used in both light and dark modes on macOS. The shadow is always dark. On dark backgrounds, the shadow is less visually prominent naturally because the contrast is lower.

### Shadow rendering
Pre-compute the shadow as a blurred texture of the squircle shape. Use 9-slice scaling so the shadow texture works at any window size. Cache per (corner_radius, shadow_config) pair.

The shadow MUST follow the squircle shape (continuous curvature), not a standard rounded rectangle. This means you blur the squircle path fill, not a circular-arc rounded rect.

---

## CSD Windows (client-side decorated)

For CSD windows, the client draws its own content and title bar. The compositor:
1. Renders the shadow (same as SSD)
2. Renders the outer border stroke (same as SSD, follows squircle at 14px radius)
3. Clips the client surface to the squircle at 14px radius with anti-aliased SDF
4. Renders the inner highlight gradient on top (same as SSD)

The client content is inset by the border width (0.5px from the outer edge + 0.5px for the inner highlight = effectively 1px). In the Figma designs, the CSD Discord screenshot is inset 1px from the outer frame edge with its own 12px inner radius.

**No title bar is rendered by the compositor for CSD windows.** The client (e.g. Discord, Firefox) handles its own controls.

---

## Render Order (compositor pipeline)

For each window, back to front:

```
1. SHADOW
   - Render pre-computed shadow texture behind and below the window
   - Use active or inactive shadow layers based on focus state
   - Shadow extends beyond window bounds (allocate extra space)

2. OUTER BORDER
   - Stroke the squircle path at 0.5px with outer border colour
   - This goes on the exact outer edge of the window bounds

3. WINDOW FILL (SSD only, behind title bar + content)
   - Fill the squircle with the content area background colour

4. TITLE BAR (SSD only)
   - Render the Slint WindowChrome titlebar (rendered to render_tex by FemtoVG)
   - Clip to squircle with top corners at 12px inner radius
   - Draw 0.5px bottom divider line

5. CLIENT CONTENT
   - Clip client surface texture to squircle (14px outer, or 12px inner for SSD content area)
   - Apply SDF smoothstep for anti-aliased edge clipping
   - Apply window opacity (from animation state)

6. INNER HIGHLIGHT
   - Render 1px inset stroke following the squircle path
   - Modulate alpha with vertical gradient (bright top, zero bottom)
   - This goes ON TOP of everything — it overlays the client content slightly

7. WINDOW CONTROLS (SSD only)
   - Render minimize/maximize/close buttons in top-right
   - These are part of the Slint WindowChrome titlebar (IconButton TouchAreas)
```

---

## Rust Data Structures

```rust
/// Complete window border specification for one state
pub struct WindowBorderSpec {
    /// Outer stroke
    pub outer_stroke_width: f32,          // 0.5
    pub outer_stroke_color: [f32; 4],     // rgba

    /// Inner highlight (gradient from top to bottom)
    pub inner_highlight_width: f32,       // 1.0
    pub inner_highlight_top_color: [f32; 4],   // brightest at top
    pub inner_highlight_side_color: [f32; 4],  // fades on sides
    // Bottom is always [0,0,0,0] — no highlight

    /// Shadow layers (rendered back-to-front)
    pub shadow_layers: Vec<ShadowLayer>,
}

pub struct ShadowLayer {
    pub offset_x: f32,
    pub offset_y: f32,
    pub blur_radius: f32,
    pub spread: f32,
    pub color: [f32; 4],
}

impl WindowBorderSpec {
    pub fn light_active() -> Self {
        Self {
            outer_stroke_width: 0.5,
            outer_stroke_color: [0.0, 0.0, 0.0, 0.22],
            inner_highlight_width: 1.0,
            inner_highlight_top_color: [1.0, 1.0, 1.0, 0.50],
            inner_highlight_side_color: [1.0, 1.0, 1.0, 0.18],
            shadow_layers: vec![
                ShadowLayer { offset_x: 0.0, offset_y:  1.0, blur_radius:  3.0, spread: 0.0, color: [0.0, 0.0, 0.0, 0.12] },
                ShadowLayer { offset_x: 0.0, offset_y:  8.0, blur_radius: 24.0, spread: 0.0, color: [0.0, 0.0, 0.0, 0.12] },
                ShadowLayer { offset_x: 0.0, offset_y: 20.0, blur_radius: 48.0, spread: 0.0, color: [0.0, 0.0, 0.0, 0.08] },
            ],
        }
    }

    pub fn light_inactive() -> Self {
        Self {
            outer_stroke_width: 0.5,
            outer_stroke_color: [0.0, 0.0, 0.0, 0.15],
            inner_highlight_width: 1.0,
            inner_highlight_top_color: [1.0, 1.0, 1.0, 0.25],
            inner_highlight_side_color: [1.0, 1.0, 1.0, 0.09],
            shadow_layers: vec![
                ShadowLayer { offset_x: 0.0, offset_y: 1.0, blur_radius:  2.0, spread: 0.0, color: [0.0, 0.0, 0.0, 0.08] },
                ShadowLayer { offset_x: 0.0, offset_y: 4.0, blur_radius: 12.0, spread: 0.0, color: [0.0, 0.0, 0.0, 0.06] },
            ],
        }
    }

    pub fn dark_active() -> Self {
        Self {
            outer_stroke_width: 0.5,
            outer_stroke_color: [0.0, 0.0, 0.0, 0.72],
            inner_highlight_width: 1.0,
            inner_highlight_top_color: [1.0, 1.0, 1.0, 0.08],
            inner_highlight_side_color: [1.0, 1.0, 1.0, 0.03],
            shadow_layers: vec![
                ShadowLayer { offset_x: 0.0, offset_y:  1.0, blur_radius:  3.0, spread: 0.0, color: [0.0, 0.0, 0.0, 0.12] },
                ShadowLayer { offset_x: 0.0, offset_y:  8.0, blur_radius: 24.0, spread: 0.0, color: [0.0, 0.0, 0.0, 0.12] },
                ShadowLayer { offset_x: 0.0, offset_y: 20.0, blur_radius: 48.0, spread: 0.0, color: [0.0, 0.0, 0.0, 0.08] },
            ],
        }
    }

    pub fn dark_inactive() -> Self {
        Self {
            outer_stroke_width: 0.5,
            outer_stroke_color: [0.0, 0.0, 0.0, 0.55],
            inner_highlight_width: 1.0,
            inner_highlight_top_color: [1.0, 1.0, 1.0, 0.04],
            inner_highlight_side_color: [1.0, 1.0, 1.0, 0.015],
            shadow_layers: vec![
                ShadowLayer { offset_x: 0.0, offset_y: 1.0, blur_radius:  2.0, spread: 0.0, color: [0.0, 0.0, 0.0, 0.08] },
                ShadowLayer { offset_x: 0.0, offset_y: 4.0, blur_radius: 12.0, spread: 0.0, color: [0.0, 0.0, 0.0, 0.06] },
            ],
        }
    }
}

/// Title bar spec (from Figma)
pub struct TitleBarSpec {
    pub height: f32,                      // 33.0 standard, 43.0 tall
    pub padding: f32,                     // 6.0
    pub inner_corner_radius: f32,         // 12.0
    pub font_family: &'static str,        // "Inter"
    pub font_weight: u16,                 // 600 (Semi Bold)
    pub font_size: f32,                   // 13.0
}

pub struct TitleBarColors {
    pub background: [f32; 4],
    pub text: [f32; 4],
    pub bottom_border: [f32; 4],
    pub bottom_border_width: f32,         // 0.5
}

impl TitleBarColors {
    pub fn light_active() -> Self {
        Self {
            background: [1.0, 1.0, 1.0, 1.0],            // #FFFFFF
            text: [0.0, 0.0, 0.0, 0.75],                  // rgba(0,0,0,0.75)
            bottom_border: [0.733, 0.733, 0.733, 1.0],    // #BBBBBB
            bottom_border_width: 0.5,
        }
    }

    pub fn light_inactive() -> Self {
        Self {
            background: [1.0, 1.0, 1.0, 0.5],             // rgba(255,255,255,0.5)
            text: [0.0, 0.0, 0.0, 0.75],
            bottom_border: [0.733, 0.733, 0.733, 1.0],
            bottom_border_width: 0.5,
        }
    }

    pub fn dark_active() -> Self {
        Self {
            background: [0.067, 0.075, 0.094, 1.0],       // #111318
            text: [1.0, 1.0, 1.0, 0.8],                    // rgba(255,255,255,0.8)
            bottom_border: [0.0, 0.0, 0.0, 0.8],
            bottom_border_width: 0.5,
        }
    }

    pub fn dark_inactive() -> Self {
        Self {
            background: [0.067, 0.075, 0.094, 0.75],      // #111318 at 75% opacity
            text: [1.0, 1.0, 1.0, 0.8],
            bottom_border: [0.0, 0.0, 0.0, 0.8],
            bottom_border_width: 0.5,
        }
    }
}
```

---

## Visual Verification Checklist

When rendering is complete, verify against these criteria:

1. **Light mode on light background:** Window should have a barely-visible outer edge. The top edge should catch a subtle white highlight. Shadow should be soft and multi-layered, not a single hard blob.

2. **Light mode on dark background:** Outer border should clearly define the window edge. Inner highlight should be more visible as contrast increases.

3. **Dark mode on dark background:** Window should NOT disappear into the background. The outer border (dark, near-black) and the very faint inner highlight at the top should provide just enough definition. Shadow is present but low-contrast against dark backgrounds.

4. **Active vs inactive:** Active windows should have noticeably stronger shadows (3 layers vs 2). Inactive windows should feel "settled" while active windows "float" above.

5. **Corners:** Must be squircle (continuous curvature), not circular arc. At 14px radius, the transition from straight to curved should be imperceptible — no visible "kink" where the rounding starts.

6. **Inner highlight gradient:** Should NOT be a uniform stroke. Looking at the top-left corner, the highlight should be bright along the top edge and smoothly fade as it rounds into the left edge.
