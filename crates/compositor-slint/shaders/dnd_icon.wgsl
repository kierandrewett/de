// Textured-quad alpha-blend pass for the DnD icon overlay. Self-contained
// (does not include _common.wgsl) because we don't share the chrome
// dynamic-uniform layout — only one draw per frame, fixed uniform.

struct DndUniforms {
    rect_x:    f32,
    rect_y:    f32,
    rect_w:    f32,
    rect_h:    f32,
    surface_w: f32,
    surface_h: f32,
    _pad0:     f32,
    _pad1:     f32,
}

@group(0) @binding(0) var<uniform> u: DndUniforms;
@group(0) @binding(1) var t_icon: texture_2d<f32>;
@group(0) @binding(2) var s_icon: sampler;

struct VertexOut {
    @builtin(position) clip_pos: vec4<f32>,
    @location(0)       uv:       vec2<f32>,
}

@vertex
fn vs_main(@builtin(vertex_index) vid: u32) -> VertexOut {
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
    // (0,0) top-left → (1,1) bottom-right
    out.uv = p * vec2<f32>(0.5, -0.5) + vec2<f32>(0.5, 0.5);
    return out;
}

@fragment
fn fs_main(in: VertexOut) -> @location(0) vec4<f32> {
    // Surface-space pixel coordinate of the current fragment.
    let px = in.uv * vec2<f32>(u.surface_w, u.surface_h);
    let local = (px - vec2<f32>(u.rect_x, u.rect_y))
              / vec2<f32>(u.rect_w, u.rect_h);
    if (local.x < 0.0 || local.y < 0.0 || local.x >= 1.0 || local.y >= 1.0) {
        discard;
    }
    let c = textureSample(t_icon, s_icon, local);
    // Client buffers are straight-alpha; the blend pipeline expects
    // premultiplied src-over. Pre-multiply here so blending is correct.
    return vec4<f32>(c.rgb * c.a, c.a);
}
