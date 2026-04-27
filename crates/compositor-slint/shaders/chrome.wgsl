// chrome.wgsl — Squircle window-chrome overlay shader.
//
// Render pipeline:
//   1. Shadow pass  — blurred squircle silhouette(s) drawn BEFORE the window
//   2. Chrome pass  — reads the Slint offscreen texture, clips to squircle,
//                     draws outer 0.5 px stroke, draws inner 1 px highlight
//
// Coordinate system: NDC fullscreen quad, window bounds delivered via
// a uniform buffer.  The shader is invoked once per window in a `for`
// loop on the CPU side; each invocation re-binds the uniform buffer
// with the current window's parameters.
//
// ── Four-state palette ────────────────────────────────────────────────────────
//
// `mode_t`  (0=dark,     1=light)
// `focus_t` (0=inactive, 1=active)
//
// Values are lerped between the four WINDOW_SPEC states:
//
//   dark_active:   outer a=0.72 / highlight_top a=0.08 / shadow 3 layers
//   dark_inactive: outer a=0.55 / highlight_top a=0.04 / shadow 2 layers
//   light_active:  outer a=0.22 / highlight_top a=0.50 / shadow 3 layers
//   light_inactive:outer a=0.15 / highlight_top a=0.25 / shadow 2 layers
//
// Each component is lerped: dark_val + mode_t*(light_val - dark_val)
//                           inactive_val + focus_t*(active_val - inactive_val)
// i.e. bilinear blend over the (mode_t, focus_t) unit square.

// ── Structs ──────────────────────────────────────────────────────────────────

struct ChromeUniforms {
    // Window bounds in physical pixels (relative to the output surface).
    win_x:      f32,
    win_y:      f32,
    win_w:      f32,
    win_h:      f32,
    // Output surface size in physical pixels.
    surface_w:  f32,
    surface_h:  f32,
    // Style parameters.
    radius_px:  f32,     // outer corner radius (14 px)
    smoothing:  f32,     // squircle smoothing  (0.6)
    // Theme crossfade parameters (0→1 animated by Rust).
    mode_t:     f32,     // 0 = dark, 1 = light
    focus_t:    f32,     // 0 = inactive, 1 = active
    // Shadow colour alpha for this layer (shadow pass only).
    shadow_a:   f32,
    // Shadow Y offset in physical pixels (shadow pass only).
    shadow_oy:  f32,
    // Shadow X offset in physical pixels (shadow pass only).
    shadow_ox:  f32,
    // blur_sigma is repurposed into this slot for shadow pass.
    blur_sigma: f32,
    _pad0:      f32,
    _pad1:      f32,
}

@group(0) @binding(0) var<uniform> u: ChromeUniforms;
@group(0) @binding(1) var t_scene: texture_2d<f32>;
@group(0) @binding(2) var s_scene: sampler;

// ── Vertex stage — fullscreen triangle trick ─────────────────────────────────

struct VertexOut {
    @builtin(position) clip_pos: vec4<f32>,
    @location(0)       uv:       vec2<f32>,
}

@vertex
fn vs_main(@builtin(vertex_index) vid: u32) -> VertexOut {
    // Two triangles covering NDC [-1,1].  Vertex indices 0..5.
    var positions = array<vec2<f32>, 6>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>( 1.0, -1.0),
        vec2<f32>(-1.0,  1.0),
        vec2<f32>(-1.0,  1.0),
        vec2<f32>( 1.0, -1.0),
        vec2<f32>( 1.0,  1.0),
    );
    let p = positions[vid];
    var out: VertexOut;
    out.clip_pos = vec4<f32>(p, 0.0, 1.0);
    // UV: (0,0) = top-left, (1,1) = bottom-right.
    out.uv = p * vec2<f32>(0.5, -0.5) + vec2<f32>(0.5, 0.5);
    return out;
}

// ── Squircle SDF ──────────────────────────────────────────────────────────────
//
// Ported from the legacy GLSL in crates/compositor/src/render/squircle_clip.rs.
// Returns negative inside, positive outside.

fn squircle_sdf(pos: vec2<f32>, half_size: vec2<f32>, radius: f32, smoothing: f32) -> f32 {
    let p_scale: f32 = 1.0 + smoothing * 0.7;
    let blend_k: f32 = 8.0 + smoothing * 16.0;

    let q = abs(pos) - half_size + vec2<f32>(radius, radius);

    // Circular-arc SDF
    let arc_dist = length(max(q, vec2<f32>(0.0))) + min(max(q.x, q.y), 0.0) - radius;

    if smoothing < 0.001 {
        return arc_dist;
    }

    // Extended-reach squircle blend
    let reach = radius * p_scale;
    let q2    = abs(pos) - half_size + vec2<f32>(reach, reach);
    let reach_dist = length(max(q2, vec2<f32>(0.0))) + min(max(q2.x, q2.y), 0.0) - reach;

    let corner_prox = -min(max(q.x, q.y), 0.0) / max(radius, 0.001);
    let blend = clamp(corner_prox * blend_k - (blend_k - 1.0), 0.0, 1.0);

    return mix(arc_dist, reach_dist * (radius / max(reach, 0.001)), blend * smoothing);
}

// ── Four-state colour helpers ─────────────────────────────────────────────────
//
// Bilinear blend over (mode_t, focus_t):
//   dark_inactive (0,0)  dark_active (0,1)
//   light_inactive(1,0)  light_active(1,1)

// Returns the outer stroke alpha, lerped across all four states.
fn outer_stroke_alpha(mode_t: f32, focus_t: f32) -> f32 {
    // dark:  inactive=0.55, active=0.72
    // light: inactive=0.15, active=0.22
    let dark_a  = mix(0.55, 0.72, focus_t);
    let light_a = mix(0.15, 0.22, focus_t);
    return mix(dark_a, light_a, mode_t);
}

// Returns the inner highlight top-edge alpha, lerped across all four states.
fn highlight_top_alpha(mode_t: f32, focus_t: f32) -> f32 {
    // dark:  inactive=0.04, active=0.08
    // light: inactive=0.25, active=0.50
    let dark_a  = mix(0.04, 0.08, focus_t);
    let light_a = mix(0.25, 0.50, focus_t);
    return mix(dark_a, light_a, mode_t);
}

// ── Chrome fragment stage ─────────────────────────────────────────────────────
//
// Composites over whatever is already in the render target:
//   1. Clips the Slint scene texture to the squircle.
//   2. Draws a 0.5 px outer stroke (lerped alpha, black colour).
//   3. Draws a 1 px inner highlight with alpha modulated by vertical gradient.

@fragment
fn fs_chrome(in: VertexOut) -> @location(0) vec4<f32> {
    // Physical pixel coordinates of this fragment.
    let frag_px = in.uv * vec2<f32>(u.surface_w, u.surface_h);

    // Discard fragments outside the window's bounding box (with some slack for stroke).
    let slack = u.radius_px + 2.0;
    if frag_px.x < (u.win_x - slack) || frag_px.x > (u.win_x + u.win_w + slack) ||
       frag_px.y < (u.win_y - slack) || frag_px.y > (u.win_y + u.win_h + slack) {
        discard;
    }

    let half_size = vec2<f32>(u.win_w * 0.5, u.win_h * 0.5);
    let win_center = vec2<f32>(u.win_x + half_size.x, u.win_y + half_size.y);
    let p = frag_px - win_center;   // centred coordinates

    // SDF at this fragment (negative = inside window)
    let d = squircle_sdf(p, half_size, u.radius_px, u.smoothing);

    // ── 1. Squircle clip mask ──────────────────────────────────────────────────
    // Smooth clip: alpha = 1 inside, 0 outside, anti-aliased over 1 px.
    let clip_alpha = 1.0 - smoothstep(-0.5, 0.5, d);

    // Sample the Slint scene texture at this fragment.
    let scene_col = textureSample(t_scene, s_scene, in.uv);

    // The window content, clipped.
    var colour = scene_col * clip_alpha;

    // ── 2. Outer stroke (0.5 px, black with mode/focus-lerped alpha) ───────────
    // The stroke sits just inside (d < 0) the squircle edge.
    // stroke_alpha peaks at d == -0.25 (centre of 0.5 px stroke).
    let stroke_a_base = outer_stroke_alpha(u.mode_t, u.focus_t);
    let stroke_alpha  = smoothstep(-1.0, 0.0, d) * (1.0 - smoothstep(-0.5, 0.5, d));
    let stroke_col    = vec4<f32>(0.0, 0.0, 0.0, stroke_a_base * stroke_alpha);

    // Porter-Duff src-over: stroke over clipped content.
    colour = stroke_col + colour * (1.0 - stroke_col.a);

    // ── 3. Inner highlight (1 px inset, vertical gradient alpha) ──────────────
    // Highlight lives 1 px inside the edge (d ≈ -1).
    let highlight_stripe = smoothstep(-2.0, -1.0, d) * (1.0 - smoothstep(-1.0, 0.0, d));

    // Vertical gradient: full at top (y_norm=0), zero at y_norm ≥ 0.667.
    let y_norm = (frag_px.y - u.win_y) / u.win_h;
    let grad   = clamp(1.0 - y_norm * 1.5, 0.0, 1.0);

    let hl_top_a = highlight_top_alpha(u.mode_t, u.focus_t);
    let hl_a     = hl_top_a * highlight_stripe * grad;
    // Premultiply before src-over: hl_rgb * hl_a so the formula works correctly
    // even when hl_a = 0 (avoids adding (1,1,1) to the output).
    let hl_col_pm = vec4<f32>(hl_a, hl_a, hl_a, hl_a); // white premultiplied

    colour = hl_col_pm + colour * (1.0 - hl_a);

    return colour;
}

// ── Shadow fragment stage ─────────────────────────────────────────────────────
//
// Draws a soft Gaussian-approximated shadow (box blur) behind the window.
// Called with a shadow-offset + blur parameters baked into the uniforms.
// Returns a pure black RGBA colour; caller blends over the scene.
//
// The shadow shape is the squircle silhouette shifted by shadow_ox/oy,
// blurred with an exponential falloff that approximates Gaussian spread.
// The blur width is passed via u.blur_sigma.
// The actual corner radius is always 14 px (outer window corner).
// Note: same shadow layers for both light and dark modes per WINDOW_SPEC.

@fragment
fn fs_shadow(in: VertexOut) -> @location(0) vec4<f32> {
    let frag_px = in.uv * vec2<f32>(u.surface_w, u.surface_h);

    // Early discard: fragments more than 3×blur_sigma away from the shadow bounding
    // box will have negligible alpha (exp(-9/2) ≈ 0.01).
    let shadow_slack = u.blur_sigma * 3.0 + 20.0;
    if frag_px.x < (u.win_x - shadow_slack + u.shadow_ox) ||
       frag_px.x > (u.win_x + u.win_w + shadow_slack + u.shadow_ox) ||
       frag_px.y < (u.win_y - shadow_slack + u.shadow_oy) ||
       frag_px.y > (u.win_y + u.win_h + shadow_slack + u.shadow_oy) {
        discard;
    }

    let corner_r = 14.0; // always the outer window corner radius
    let half_size = vec2<f32>(u.win_w * 0.5, u.win_h * 0.5);
    // Window center, shifted by shadow offset
    let shadow_center = vec2<f32>(
        u.win_x + half_size.x + u.shadow_ox,
        u.win_y + half_size.y + u.shadow_oy,
    );
    let p = frag_px - shadow_center;

    // SDF at shadow-shifted position.
    let d = squircle_sdf(p, half_size, corner_r, u.smoothing);

    // Gaussian-like falloff: exp(-0.5 * (d/sigma)^2) for d > 0.
    // For d <= 0 (inside silhouette) alpha = 1.
    var shadow_coverage: f32;
    if d <= 0.0 {
        shadow_coverage = 1.0;
    } else {
        // Approximation: faster falloff than true Gaussian but visually close.
        let t = d / max(u.blur_sigma, 0.001);
        shadow_coverage = exp(-t * t * 0.5);
    }

    let alpha = u.shadow_a * shadow_coverage;
    return vec4<f32>(0.0, 0.0, 0.0, alpha);
}
