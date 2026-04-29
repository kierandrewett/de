// 1 px inner top-edge highlight gradient. Simulates top-down lighting on
// glass per WINDOW_SPEC. Premultiplied white, alpha modulated by mode_t ×
// focus_t (palette in _common.wgsl::highlight_top_alpha).

@fragment
fn fs_main(in: VertexOut) -> @location(0) vec4<f32> {
    let frag_px = in.uv * vec2<f32>(u.surface_w, u.surface_h);

    let slack = u.radius_px + 2.0;
    if frag_px.x < (u.win_x - slack) || frag_px.x > (u.win_x + u.win_w + slack) ||
       frag_px.y < (u.win_y - slack) || frag_px.y > (u.win_y + u.win_h + slack) {
        discard;
    }

    let half_size  = vec2<f32>(u.win_w * 0.5, u.win_h * 0.5);
    let win_center = vec2<f32>(u.win_x + half_size.x, u.win_y + half_size.y);
    let p          = frag_px - win_center;

    let d = squircle_sdf(p, half_size, u.radius_px, u.smoothing);

    // Highlight stripe is 1 px inset from the squircle edge.
    let stripe = smoothstep(-2.0, -1.0, d) * (1.0 - smoothstep(-1.0, 0.0, d));

    // Top-down brightness gradient: full at the top edge, fades to 0 by ~67% down.
    let y_norm = (frag_px.y - u.win_y) / u.win_h;
    let grad   = clamp(1.0 - y_norm * 1.5, 0.0, 1.0);

    let hl_top_a = highlight_top_alpha(u.mode_t, u.focus_t);
    let hl_a     = hl_top_a * stripe * grad;

    // Premultiplied white — same alpha for RGB and A.
    return vec4<f32>(hl_a, hl_a, hl_a, hl_a);
}
