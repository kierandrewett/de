// 0.5 px outer stroke at the squircle edge. Premultiplied black with alpha
// modulated by mode_t × focus_t (palette in _common.wgsl::outer_stroke_alpha).

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

    // 0.5 px stroke peaks at d ≈ -0.25 — where (-1, 0) ramp meets (-0.5, 0.5).
    let stroke_alpha = smoothstep(-1.0, 0.0, d) * (1.0 - smoothstep(-0.5, 0.5, d));
    let a            = outer_stroke_alpha(u.mode_t, u.focus_t) * stroke_alpha;

    return vec4<f32>(0.0, 0.0, 0.0, a);
}
