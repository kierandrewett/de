// Shared chunk prepended to every per-effect WGSL source via format!() at
// runtime. Defines the uniform binding, vertex shader, and the squircle SDF
// used by every chrome effect pass.

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
    radius_px:  f32,     // outer corner radius (e.g. 14 px)
    smoothing:  f32,     // squircle smoothing  (0.6 → iOS-style)
    // Theme crossfade parameters (0→1, animated by Rust).
    mode_t:     f32,     // 0 = dark, 1 = light
    focus_t:    f32,     // 0 = inactive, 1 = active
    // Shadow alpha for this layer (shadow passes only).
    shadow_a:   f32,
    // Shadow Y / X offset in physical pixels (shadow composite pass only).
    shadow_oy:  f32,
    shadow_ox:  f32,
    // Blur sigma for Gaussian shadow passes (pixels). 0 elsewhere.
    blur_sigma: f32,
    _pad0:      f32,
    _pad1:      f32,
}

@group(0) @binding(0) var<uniform> u: ChromeUniforms;
// Optional sampled texture (scene or intermediate). Passes that don't sample
// still bind a dummy texture to satisfy the layout.
@group(0) @binding(1) var t_src: texture_2d<f32>;
@group(0) @binding(2) var s_src: sampler;

struct VertexOut {
    @builtin(position) clip_pos: vec4<f32>,
    @location(0)       uv:       vec2<f32>,
}

@vertex
fn vs_main(@builtin(vertex_index) vid: u32) -> VertexOut {
    // Two triangles covering NDC [-1, 1].
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

// iOS-style continuous-curvature squircle SDF. Returns the signed distance
// from `pos` (in window-centred coords) to the squircle edge. Negative inside.
//
// `smoothing` of 0.0 collapses to a circular-arc rounded rect; 0.6 is the
// iOS / SwiftUI default — gentle G2 continuity at the corners.
fn squircle_sdf(pos: vec2<f32>, half_size: vec2<f32>, radius: f32, smoothing: f32) -> f32 {
    let p_scale: f32 = 1.0 + smoothing * 0.7;
    let blend_k: f32 = 8.0 + smoothing * 16.0;

    // Standard rounded-rect SDF (circular arcs).
    let q = abs(pos) - half_size + vec2<f32>(radius, radius);
    let arc_dist = length(max(q, vec2<f32>(0.0))) + min(max(q.x, q.y), 0.0) - radius;

    if smoothing < 0.001 {
        return arc_dist;
    }

    // Wider rounded-rect with the same parameters scaled out — blended into
    // the corner region only, gives the squircle's continuous curvature.
    let reach = radius * p_scale;
    let q2    = abs(pos) - half_size + vec2<f32>(reach, reach);
    let reach_dist = length(max(q2, vec2<f32>(0.0))) + min(max(q2.x, q2.y), 0.0) - reach;

    let corner_prox = -min(max(q.x, q.y), 0.0) / max(radius, 0.001);
    let blend = clamp(corner_prox * blend_k - (blend_k - 1.0), 0.0, 1.0);

    return mix(arc_dist, reach_dist * (radius / max(reach, 0.001)), blend * smoothing);
}

// Theme-aware four-state palette helper for outer-stroke alpha:
//   dark_inactive=0.55, dark_active=0.72, light_inactive=0.15, light_active=0.22
fn outer_stroke_alpha(mode_t: f32, focus_t: f32) -> f32 {
    let dark_a  = mix(0.55, 0.72, focus_t);
    let light_a = mix(0.15, 0.22, focus_t);
    return mix(dark_a, light_a, mode_t);
}

// Theme-aware four-state palette helper for highlight top-edge alpha:
//   dark_inactive=0.04, dark_active=0.08, light_inactive=0.25, light_active=0.50
fn highlight_top_alpha(mode_t: f32, focus_t: f32) -> f32 {
    let dark_a  = mix(0.04, 0.08, focus_t);
    let light_a = mix(0.25, 0.50, focus_t);
    return mix(dark_a, light_a, mode_t);
}
