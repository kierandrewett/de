// Drop shadow pass — three-stage Gaussian blur of a squircle silhouette,
// composited at (shadow_ox, shadow_oy) with shadow_a opacity.
//
// Pipelines (one fragment entry each):
//   fs_silhouette  — squircle silhouette into a full-surface mask texture
//   fs_blur_h      — horizontal Gaussian blur of the mask
//   fs_blur_v      — vertical   Gaussian blur of the H-blurred result
//   fs_composite   — sample the blurred mask + composite into the swapchain
//
// FIX vs the old monolithic chrome.wgsl: the silhouette is now rendered with
// the actual win_x / win_y, not at origin. The composite reads at
// (frag_px - shadow_offset) and the silhouette ALSO sits at win_x,win_y in
// the mask, so the shadow lands at (win_x + shadow_ox, win_y + shadow_oy) —
// no more position-blind cache.

const BLUR_TAPS: i32 = 32;

fn gaussian_weight(offset: f32, sigma: f32) -> f32 {
    return exp(-0.5 * (offset / sigma) * (offset / sigma));
}

// ── Pass 1: silhouette ───────────────────────────────────────────────────────
@fragment
fn fs_silhouette(in: VertexOut) -> @location(0) vec4<f32> {
    let frag_px = in.uv * vec2<f32>(u.surface_w, u.surface_h);

    let pad = u.blur_sigma * 3.0 + 2.0;
    if frag_px.x < (u.win_x - pad) || frag_px.x > (u.win_x + u.win_w + pad) ||
       frag_px.y < (u.win_y - pad) || frag_px.y > (u.win_y + u.win_h + pad) {
        discard;
    }

    let half_size  = vec2<f32>(u.win_w * 0.5, u.win_h * 0.5);
    let win_center = vec2<f32>(u.win_x + half_size.x, u.win_y + half_size.y);
    let p          = frag_px - win_center;

    let d    = squircle_sdf(p, half_size, u.radius_px, u.smoothing);
    let mask = 1.0 - smoothstep(-0.5, 0.5, d);

    return vec4<f32>(0.0, 0.0, 0.0, mask);
}

// ── Pass 2: horizontal blur ──────────────────────────────────────────────────
@fragment
fn fs_blur_h(in: VertexOut) -> @location(0) vec4<f32> {
    let sigma = u.blur_sigma;
    if sigma < 0.5 { return textureSample(t_src, s_src, in.uv); }

    var acc: f32 = 0.0;
    var weight_sum: f32 = 0.0;
    for (var i = -BLUR_TAPS; i <= BLUR_TAPS; i++) {
        let offset    = f32(i);
        let w         = gaussian_weight(offset, sigma);
        let sample_uv = in.uv + vec2<f32>(offset / u.surface_w, 0.0);
        acc           = acc + textureSample(t_src, s_src, sample_uv).a * w;
        weight_sum    = weight_sum + w;
    }
    return vec4<f32>(0.0, 0.0, 0.0, acc / weight_sum);
}

// ── Pass 3: vertical blur ────────────────────────────────────────────────────
@fragment
fn fs_blur_v(in: VertexOut) -> @location(0) vec4<f32> {
    let sigma = u.blur_sigma;
    if sigma < 0.5 { return textureSample(t_src, s_src, in.uv); }

    var acc: f32 = 0.0;
    var weight_sum: f32 = 0.0;
    for (var i = -BLUR_TAPS; i <= BLUR_TAPS; i++) {
        let offset    = f32(i);
        let w         = gaussian_weight(offset, sigma);
        let sample_uv = in.uv + vec2<f32>(0.0, offset / u.surface_h);
        acc           = acc + textureSample(t_src, s_src, sample_uv).a * w;
        weight_sum    = weight_sum + w;
    }
    return vec4<f32>(0.0, 0.0, 0.0, acc / weight_sum);
}

// ── Pass 4: composite ────────────────────────────────────────────────────────
@fragment
fn fs_composite(in: VertexOut) -> @location(0) vec4<f32> {
    let frag_px = in.uv * vec2<f32>(u.surface_w, u.surface_h);

    // The silhouette in the blurred mask already sits at (win_x, win_y).
    // Shifting the sample point by -shadow_offset moves the apparent shadow
    // to (win_x + shadow_ox, win_y + shadow_oy).
    let mask_uv = (frag_px - vec2<f32>(u.shadow_ox, u.shadow_oy))
                / vec2<f32>(u.surface_w, u.surface_h);

    if mask_uv.x < 0.0 || mask_uv.x > 1.0 || mask_uv.y < 0.0 || mask_uv.y > 1.0 {
        discard;
    }

    // Discard outside the shadow's visible bounding box (window rect grown
    // by the blur kernel + offset).
    let pad = u.blur_sigma * 3.0 + 20.0;
    if frag_px.x < (u.win_x - pad + u.shadow_ox) ||
       frag_px.x > (u.win_x + u.win_w + pad + u.shadow_ox) ||
       frag_px.y < (u.win_y - pad + u.shadow_oy) ||
       frag_px.y > (u.win_y + u.win_h + pad + u.shadow_oy) {
        discard;
    }

    let mask_alpha = textureSample(t_src, s_src, mask_uv).a;

    // Mask out the inside of the window — shadow should only ever appear
    // OUTSIDE the window's silhouette. Without this, the blurred mask's
    // solid interior bleeds into the window content as a darkened tint.
    let half_size  = vec2<f32>(u.win_w * 0.5, u.win_h * 0.5);
    let win_center = vec2<f32>(u.win_x + half_size.x, u.win_y + half_size.y);
    let p          = frag_px - win_center;
    let d_inside   = squircle_sdf(p, half_size, u.radius_px, u.smoothing);
    let outside_w  = smoothstep(-0.5, 0.5, d_inside); // 0 inside, 1 outside

    let alpha      = u.shadow_a * mask_alpha * outside_w;
    return vec4<f32>(0.0, 0.0, 0.0, alpha);
}
