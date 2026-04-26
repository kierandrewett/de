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
    // Outer stroke colour (rgba, premultiplied NOT required — we blend manually)
    stroke_r:   f32,
    stroke_g:   f32,
    stroke_b:   f32,
    stroke_a:   f32,
    // Inner highlight top colour alpha (bottom is always 0)
    highlight_a: f32,
    // 1 = active, 0 = inactive (controls highlight intensity)
    active:     f32,
    // Shadow colour alpha for this layer
    shadow_a:   f32,
    // Shadow Y offset in physical pixels
    shadow_oy:  f32,
    // Shadow X offset in physical pixels
    shadow_ox:  f32,
    _pad0:      f32,
    _pad1:      f32,
    _pad2:      f32,
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

// ── Chrome fragment stage ─────────────────────────────────────────────────────
//
// Composites over whatever is already in the render target:
//   1. Clips the Slint scene texture to the squircle.
//   2. Draws a 0.5 px outer stroke (stroke_rgba) just inside the edge.
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

    // ── 2. Outer stroke (0.5 px, stroke_rgba) ─────────────────────────────────
    // The stroke sits just inside (d < 0) the squircle edge.
    // stroke_alpha peaks at d == -0.25 (centre of 0.5 px stroke).
    let stroke_alpha = smoothstep(-1.0, 0.0, d) * (1.0 - smoothstep(-0.5, 0.5, d));
    let stroke_col   = vec4<f32>(u.stroke_r, u.stroke_g, u.stroke_b, u.stroke_a * stroke_alpha);

    // Porter-Duff src-over: stroke over clipped content.
    colour = stroke_col + colour * (1.0 - stroke_col.a);

    // ── 3. Inner highlight (1 px inset, vertical gradient alpha) ──────────────
    // Highlight lives 1 px inside the edge (d ≈ -1).
    let highlight_stripe = smoothstep(-2.0, -1.0, d) * (1.0 - smoothstep(-1.0, 0.0, d));

    // Vertical gradient: full at top (y_norm=0), zero at y_norm ≥ 0.667.
    let y_norm = (frag_px.y - u.win_y) / u.win_h;
    let grad   = clamp(1.0 - y_norm * 1.5, 0.0, 1.0);

    let hl_a = u.highlight_a * highlight_stripe * grad;
    let hl_col = vec4<f32>(1.0, 1.0, 1.0, hl_a);

    colour = hl_col + colour * (1.0 - hl_col.a);

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
// The blur width is passed via u.radius_px (repurposed as blur_sigma for
// this pass).  The actual corner radius remains u.smoothing for reuse.

@fragment
fn fs_shadow(in: VertexOut) -> @location(0) vec4<f32> {
    let frag_px = in.uv * vec2<f32>(u.surface_w, u.surface_h);

    // Shadow parameters from uniforms:
    //   win bounds (win_x, win_y, win_w, win_h) describe the WINDOW, not shadow.
    //   shadow_ox/oy is the shadow offset.
    //   radius_px is used as the CORNER radius of the window shape (14 px).
    //   shadow_a   is the shadow layer alpha.
    //   smoothing  is the squircle smoothing (0.6).
    //
    // We evaluate the squircle SDF at the shadow-offset fragment position,
    // then apply an exponential falloff proportional to the blur radius stored
    // in u.highlight_a (we repurpose it as blur_sigma for the shadow pass;
    // see chrome_shader.rs where the uniform is constructed).

    let blur_sigma = u.highlight_a; // repurposed field for shadow pass

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
        let t = d / max(blur_sigma, 0.001);
        shadow_coverage = exp(-t * t * 0.5);
    }

    let alpha = u.shadow_a * shadow_coverage;
    return vec4<f32>(0.0, 0.0, 0.0, alpha);
}
