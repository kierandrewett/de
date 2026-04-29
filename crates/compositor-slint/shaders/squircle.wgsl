// Squircle clip pass. Reads the Slint scene texture in the window region and
// outputs (scene.rgb * mask, scene.a * mask) so the corners are rounded with
// a proper squircle (G2-continuous curvature) instead of Slint's circular
// border-radius arc.
//
// Premultiplied output, src_over blend so this composites cleanly over the
// already-blitted scene in the swapchain.

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

    let d          = squircle_sdf(p, half_size, u.radius_px, u.smoothing);
    let mask_alpha = 1.0 - smoothstep(-0.5, 0.5, d);

    let scene_col = textureSample(t_src, s_src, in.uv);
    return vec4<f32>(scene_col.rgb * mask_alpha, scene_col.a * mask_alpha);
}
