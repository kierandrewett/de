// Real-time backdrop-blur passes — separable Gaussian on RGBA.
//
// Pipelines (one fragment entry each):
//   fs_blur_h_rgba   — horizontal Gaussian into a scratch texture
//   fs_blur_v_rgba   — vertical   Gaussian, sampled by composite
//   fs_composite_pill — sample blurred scratch + squircle-mask + alpha-blend
//                       back over final_tex within (win_x..+win_w, win_y..+win_h)
//   fs_composite_rect — same as above but rectangular (no squircle clip)
//
// ChromeUniforms reuse:
//   win_x, win_y, win_w, win_h — blur rect bounds in physical pixels
//   blur_sigma                 — Gaussian sigma (bigger = softer)
//   radius_px, smoothing       — squircle clip parameters (composite_pill only)
//
// Sampling clamp: each blur pass clamps tap UVs to the blur rect. Without this
// the kernel would pick up pixels OUTSIDE the dock pill (sharp content from
// the wallpaper/windows beyond the rect bleeds INTO the rect at the edges,
// which reads as a halo).

const BLUR_TAPS: i32 = 16;

fn gauss_w(offset: f32, sigma: f32) -> f32 {
    return exp(-0.5 * (offset / sigma) * (offset / sigma));
}

// Clamp a UV point to the rect (win_x..win_x+win_w) / (win_y..win_y+win_h),
// returned in 0..1 surface UVs.
fn clamp_uv_to_rect(uv: vec2<f32>) -> vec2<f32> {
    let lo = vec2<f32>(u.win_x, u.win_y) / vec2<f32>(u.surface_w, u.surface_h);
    let hi = vec2<f32>(u.win_x + u.win_w, u.win_y + u.win_h)
           / vec2<f32>(u.surface_w, u.surface_h);
    return clamp(uv, lo, hi);
}

// ── Pass 1: horizontal Gaussian on RGBA ──────────────────────────────────────
@fragment
fn fs_blur_h_rgba(in: VertexOut) -> @location(0) vec4<f32> {
    let frag_px = in.uv * vec2<f32>(u.surface_w, u.surface_h);

    // Outside the blur rect: pass-through (we're writing to a scratch tex
    // that will be sampled by the next pass; unaffected pixels don't matter).
    if frag_px.x < u.win_x - 1.0 || frag_px.x > u.win_x + u.win_w + 1.0 ||
       frag_px.y < u.win_y - 1.0 || frag_px.y > u.win_y + u.win_h + 1.0 {
        return textureSample(t_src, s_src, in.uv);
    }

    let sigma = max(u.blur_sigma, 0.5);
    var acc:        vec4<f32> = vec4<f32>(0.0);
    var weight_sum: f32       = 0.0;
    let step:       f32       = sigma * 2.0 / f32(BLUR_TAPS);

    for (var i = -BLUR_TAPS; i <= BLUR_TAPS; i++) {
        let offset    = f32(i) * step;
        let w         = gauss_w(offset, sigma);
        let sample_uv = clamp_uv_to_rect(in.uv + vec2<f32>(offset / u.surface_w, 0.0));
        acc          += textureSample(t_src, s_src, sample_uv) * w;
        weight_sum   += w;
    }
    return acc / weight_sum;
}

// ── Pass 2: vertical Gaussian on RGBA ────────────────────────────────────────
@fragment
fn fs_blur_v_rgba(in: VertexOut) -> @location(0) vec4<f32> {
    let frag_px = in.uv * vec2<f32>(u.surface_w, u.surface_h);

    if frag_px.x < u.win_x - 1.0 || frag_px.x > u.win_x + u.win_w + 1.0 ||
       frag_px.y < u.win_y - 1.0 || frag_px.y > u.win_y + u.win_h + 1.0 {
        return textureSample(t_src, s_src, in.uv);
    }

    let sigma = max(u.blur_sigma, 0.5);
    var acc:        vec4<f32> = vec4<f32>(0.0);
    var weight_sum: f32       = 0.0;
    let step:       f32       = sigma * 2.0 / f32(BLUR_TAPS);

    for (var i = -BLUR_TAPS; i <= BLUR_TAPS; i++) {
        let offset    = f32(i) * step;
        let w         = gauss_w(offset, sigma);
        let sample_uv = clamp_uv_to_rect(in.uv + vec2<f32>(0.0, offset / u.surface_h));
        acc          += textureSample(t_src, s_src, sample_uv) * w;
        weight_sum   += w;
    }
    return acc / weight_sum;
}

// ── Pass 3a: composite squircle pill (dock) ──────────────────────────────────
// Sample the blurred scratch and write back to final_tex within the squircle
// pill bounds. Uses src-over blend so callers using LoadOp::Load + this
// pipeline replace the rect cleanly.
@fragment
fn fs_composite_pill(in: VertexOut) -> @location(0) vec4<f32> {
    let frag_px = in.uv * vec2<f32>(u.surface_w, u.surface_h);

    if frag_px.x < u.win_x || frag_px.x > u.win_x + u.win_w ||
       frag_px.y < u.win_y || frag_px.y > u.win_y + u.win_h {
        discard;
    }

    // Squircle alpha mask: smoothstep across the SDF edge so the pill has
    // anti-aliased rounded corners rather than a hard cut.
    let half_size  = vec2<f32>(u.win_w * 0.5, u.win_h * 0.5);
    let win_center = vec2<f32>(u.win_x + half_size.x, u.win_y + half_size.y);
    let p          = frag_px - win_center;
    let d          = squircle_sdf(p, half_size, u.radius_px, u.smoothing);
    let mask       = 1.0 - smoothstep(-0.5, 0.5, d);
    if mask < 0.001 { discard; }

    let rgba = textureSample(t_src, s_src, in.uv);
    return vec4<f32>(rgba.rgb * mask, mask);
}

// ── Pass 3b: composite rectangular (panel) ───────────────────────────────────
@fragment
fn fs_composite_rect(in: VertexOut) -> @location(0) vec4<f32> {
    let frag_px = in.uv * vec2<f32>(u.surface_w, u.surface_h);

    if frag_px.x < u.win_x || frag_px.x > u.win_x + u.win_w ||
       frag_px.y < u.win_y || frag_px.y > u.win_y + u.win_h {
        discard;
    }

    let rgba = textureSample(t_src, s_src, in.uv);
    return vec4<f32>(rgba.rgb, 1.0);
}

// ── Pass 4: alpha-blend overlay_tex onto final_tex ───────────────────────────
// Used by the dual-pass slint pipeline to drop the dock/panel UI (icons,
// clock, tint, gloss) on top of the GPU-blurred backdrop. Caller uses
// blend = src-over premul. The shader just samples the overlay texture.
@fragment
fn fs_overlay_blit(in: VertexOut) -> @location(0) vec4<f32> {
    let frag_px = in.uv * vec2<f32>(u.surface_w, u.surface_h);

    // Limit to the rect so we don't draw outside the dock/panel. win_w==0
    // means "whole surface" — used by the full-scene overlay blit.
    if u.win_w > 0.5 && u.win_h > 0.5 {
        if frag_px.x < u.win_x || frag_px.x > u.win_x + u.win_w ||
           frag_px.y < u.win_y || frag_px.y > u.win_y + u.win_h {
            discard;
        }
    }

    return textureSample(t_src, s_src, in.uv);
}
