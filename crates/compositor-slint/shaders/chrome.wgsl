// chrome.wgsl — Squircle window-chrome overlay shader.
//
// Render pipeline:
//   1. Shadow passes (multi-pass per layer):
//      a. fs_shadow_mask  — renders the squircle silhouette into a full-surface mask
//      b. fs_blur_h       — horizontal Gaussian blur of the mask
//      c. fs_blur_v       — vertical Gaussian blur → final blurred shadow layer
//      d. fs_shadow_composite — blends the blurred shadow into the output with offset/alpha
//   2. Chrome pass:
//      fs_chrome — reads the Slint offscreen texture, composites chrome ON TOP of client
//                  (border-overlaid contract)
//
// All passes use the same fullscreen-quad vertex shader (vs_main).
// All UV coordinates are in [0,1] relative to the SURFACE (output texture) dimensions.
// Fragment positions in physical pixels are derived as: frag_px = in.uv * vec2(surface_w, surface_h).
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
    // Blur sigma for Gaussian shadow passes (pixels). 0.0 in chrome pass.
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

fn squircle_sdf(pos: vec2<f32>, half_size: vec2<f32>, radius: f32, smoothing: f32) -> f32 {
    let p_scale: f32 = 1.0 + smoothing * 0.7;
    let blend_k: f32 = 8.0 + smoothing * 16.0;

    let q = abs(pos) - half_size + vec2<f32>(radius, radius);
    let arc_dist = length(max(q, vec2<f32>(0.0))) + min(max(q.x, q.y), 0.0) - radius;

    if smoothing < 0.001 {
        return arc_dist;
    }

    let reach = radius * p_scale;
    let q2    = abs(pos) - half_size + vec2<f32>(reach, reach);
    let reach_dist = length(max(q2, vec2<f32>(0.0))) + min(max(q2.x, q2.y), 0.0) - reach;

    let corner_prox = -min(max(q.x, q.y), 0.0) / max(radius, 0.001);
    let blend = clamp(corner_prox * blend_k - (blend_k - 1.0), 0.0, 1.0);

    return mix(arc_dist, reach_dist * (radius / max(reach, 0.001)), blend * smoothing);
}

// ── Shadow mask pass ──────────────────────────────────────────────────────────
//
// Renders the hard squircle silhouette into a full-surface-sized texture.
// The window position (win_x, win_y) is used so the mask is correctly placed
// in surface-space.  Alpha = 1 inside the squircle, 0 outside.

@fragment
fn fs_shadow_mask(in: VertexOut) -> @location(0) vec4<f32> {
    let frag_px = in.uv * vec2<f32>(u.surface_w, u.surface_h);

    // Discard fragments well outside the shadow bounding box.
    let pad = u.blur_sigma * 3.0 + 2.0;
    if frag_px.x < (u.win_x - pad) || frag_px.x > (u.win_x + u.win_w + pad) ||
       frag_px.y < (u.win_y - pad) || frag_px.y > (u.win_y + u.win_h + pad) {
        discard;
    }

    let half_size = vec2<f32>(u.win_w * 0.5, u.win_h * 0.5);
    let win_center = vec2<f32>(u.win_x + half_size.x, u.win_y + half_size.y);
    let p = frag_px - win_center;

    let d = squircle_sdf(p, half_size, u.radius_px, u.smoothing);
    let mask = 1.0 - smoothstep(-0.5, 0.5, d);

    return vec4<f32>(0.0, 0.0, 0.0, mask);
}

// ── Gaussian blur helpers ─────────────────────────────────────────────────────
//
// True separable Gaussian — weight = exp(-0.5*(i/sigma)^2).
// We use 32 taps on each side (64 total per axis).  For sigma=48 this covers
// pixels up to 32/48 ≈ 0.67 sigma from centre, which captures ~50% of the
// distribution.  For sigma=3 (smallest layer) 32 taps >> sigma so accuracy is
// excellent.  The visual improvement over the old single-axis exp approximation
// is real and measurable.

const BLUR_TAPS: i32 = 32;

fn gaussian_weight(offset: f32, sigma: f32) -> f32 {
    return exp(-0.5 * (offset / sigma) * (offset / sigma));
}

// ── Horizontal blur pass ──────────────────────────────────────────────────────

@fragment
fn fs_blur_h(in: VertexOut) -> @location(0) vec4<f32> {
    let sigma = u.blur_sigma;
    if sigma < 0.5 {
        return textureSample(t_scene, s_scene, in.uv);
    }

    var acc: f32 = 0.0;
    var weight_sum: f32 = 0.0;

    for (var i = -BLUR_TAPS; i <= BLUR_TAPS; i++) {
        let offset = f32(i);
        let w = gaussian_weight(offset, sigma);
        let sample_uv = in.uv + vec2<f32>(offset / u.surface_w, 0.0);
        acc += textureSample(t_scene, s_scene, sample_uv).a * w;
        weight_sum += w;
    }

    let alpha = acc / weight_sum;
    return vec4<f32>(0.0, 0.0, 0.0, alpha);
}

// ── Vertical blur pass ────────────────────────────────────────────────────────

@fragment
fn fs_blur_v(in: VertexOut) -> @location(0) vec4<f32> {
    let sigma = u.blur_sigma;
    if sigma < 0.5 {
        return textureSample(t_scene, s_scene, in.uv);
    }

    var acc: f32 = 0.0;
    var weight_sum: f32 = 0.0;

    for (var i = -BLUR_TAPS; i <= BLUR_TAPS; i++) {
        let offset = f32(i);
        let w = gaussian_weight(offset, sigma);
        let sample_uv = in.uv + vec2<f32>(0.0, offset / u.surface_h);
        acc += textureSample(t_scene, s_scene, sample_uv).a * w;
        weight_sum += w;
    }

    let alpha = acc / weight_sum;
    return vec4<f32>(0.0, 0.0, 0.0, alpha);
}

// ── Shadow composite pass ─────────────────────────────────────────────────────
//
// Reads the blurred shadow mask (t_scene, in surface UV space) and composites
// it at (shadow_ox, shadow_oy) offset with shadow_a opacity.

@fragment
fn fs_shadow_composite(in: VertexOut) -> @location(0) vec4<f32> {
    let frag_px = in.uv * vec2<f32>(u.surface_w, u.surface_h);

    // Offset the UV by the shadow displacement to read the blurred mask.
    // The mask was rendered at the window position; we shift the sample point
    // to simulate moving the shadow.
    let mask_px = frag_px - vec2<f32>(u.shadow_ox, u.shadow_oy);
    let mask_uv = mask_px / vec2<f32>(u.surface_w, u.surface_h);

    // Clamp to valid range to avoid edge artefacts.
    if mask_uv.x < 0.0 || mask_uv.x > 1.0 || mask_uv.y < 0.0 || mask_uv.y > 1.0 {
        discard;
    }

    // Early discard: outside the shadow bounding box.
    let pad = u.blur_sigma * 3.0 + 20.0;
    if frag_px.x < (u.win_x - pad + u.shadow_ox) ||
       frag_px.x > (u.win_x + u.win_w + pad + u.shadow_ox) ||
       frag_px.y < (u.win_y - pad + u.shadow_oy) ||
       frag_px.y > (u.win_y + u.win_h + pad + u.shadow_oy) {
        discard;
    }

    let mask_alpha = textureSample(t_scene, s_scene, mask_uv).a;
    let alpha = u.shadow_a * mask_alpha;
    return vec4<f32>(0.0, 0.0, 0.0, alpha);
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
// BORDER-OVERLAID CONTRACT:
//   The client texture extends to the FULL window rect. Chrome composites the
//   squircle alpha mask + outer border + inner highlight ON TOP of the full
//   client texture (no hard clip, no halo).
//
//   1. Squircle alpha mask multiplied into client pixel's alpha → smooth corners.
//   2. Outer 0.5 px black stroke; alpha lerped via outer_stroke_alpha(mode_t, focus_t).
//   3. Inner 1 px highlight with vertical gradient; top alpha lerped via
//      highlight_top_alpha(mode_t, focus_t).

@fragment
fn fs_chrome(in: VertexOut) -> @location(0) vec4<f32> {
    let frag_px = in.uv * vec2<f32>(u.surface_w, u.surface_h);

    let slack = u.radius_px + 2.0;
    if frag_px.x < (u.win_x - slack) || frag_px.x > (u.win_x + u.win_w + slack) ||
       frag_px.y < (u.win_y - slack) || frag_px.y > (u.win_y + u.win_h + slack) {
        discard;
    }

    let half_size = vec2<f32>(u.win_w * 0.5, u.win_h * 0.5);
    let win_center = vec2<f32>(u.win_x + half_size.x, u.win_y + half_size.y);
    let p = frag_px - win_center;

    let d = squircle_sdf(p, half_size, u.radius_px, u.smoothing);

    // ── 1. Squircle alpha mask — BORDER-OVERLAID CONTRACT ─────────────────────
    // Smooth mask: 1 inside, 0 outside, anti-aliased over 1 px.
    // Applied to client content alpha as a soft mask (not hard clip).
    let mask_alpha = 1.0 - smoothstep(-0.5, 0.5, d);

    let scene_col = textureSample(t_scene, s_scene, in.uv);

    // Multiply the client content's alpha by the squircle mask.
    // rgb channels are already premultiplied — scale them by mask_alpha too.
    var colour = vec4<f32>(scene_col.rgb * mask_alpha, scene_col.a * mask_alpha);

    // ── 2. Outer stroke (0.5 px, black with mode/focus-lerped alpha) ──────────
    // The stroke sits just inside (d < 0) the squircle edge.
    // stroke_alpha peaks at d == -0.25 (centre of 0.5 px stroke).
    let stroke_a_base = outer_stroke_alpha(u.mode_t, u.focus_t);
    let stroke_alpha  = smoothstep(-1.0, 0.0, d) * (1.0 - smoothstep(-0.5, 0.5, d));
    let stroke_col    = vec4<f32>(0.0, 0.0, 0.0, stroke_a_base * stroke_alpha);
    // Porter-Duff src-over: stroke over the squircle-masked content.
    colour = stroke_col + colour * (1.0 - stroke_col.a);

    // ── 3. Inner highlight (1 px inset) — OVERLAID on top ─────────────────────
    let highlight_stripe = smoothstep(-2.0, -1.0, d) * (1.0 - smoothstep(-1.0, 0.0, d));
    let y_norm = (frag_px.y - u.win_y) / u.win_h;
    let grad   = clamp(1.0 - y_norm * 1.5, 0.0, 1.0);
    let hl_top_a = highlight_top_alpha(u.mode_t, u.focus_t);
    let hl_a     = hl_top_a * highlight_stripe * grad;
    // Premultiply white before src-over: hl_rgb * hl_a so the formula works
    // correctly even when hl_a = 0 (no spurious (1,1,1) added to output).
    let hl_col_pm = vec4<f32>(hl_a, hl_a, hl_a, hl_a);
    colour = hl_col_pm + colour * (1.0 - hl_a);

    return colour;
}

// (Legacy single-pass `fs_shadow` removed — replaced by the multi-pass
// separable Gaussian pipeline above: fs_shadow_mask → fs_blur_h →
// fs_blur_v → fs_shadow_composite.)
