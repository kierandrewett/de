//! True alpha-clipping of wayland surfaces to a squircle path.
//!
//! Replaces the v1 "fill the corners with the desktop background" hack
//! with a real per-pixel SDF mask. Each toplevel's main wayland surface
//! is wrapped in a [`TextureShaderElement`] that runs our custom GLSL
//! fragment shader; pixels outside the squircle become alpha-zero, so
//! whatever is behind the window (desktop wallpaper or another window)
//! shows through correctly.
//!
//! Sub-surfaces / popups are not yet clipped — they fall back to the
//! existing `WaylandSurfaceRenderElement` path. That is fine for the
//! common case (terminal, browser, editor) but means a popup that
//! protrudes past the squircle corner will momentarily look square.

#![allow(dead_code)]

use smithay::{
    backend::renderer::{
        element::{
            texture::{TextureBuffer, TextureRenderElement},
            Kind,
        },
        gles::{
            element::TextureShaderElement, GlesError, GlesRenderer, GlesTexProgram, Uniform,
            UniformName, UniformType,
        },
        utils::{import_surface_tree, with_renderer_surface_state, RendererSurfaceState},
        Renderer,
    },
    desktop::Window,
    utils::{Point, Rectangle, Transform},
};

// ─── GLSL fragment shader ───────────────────────────────────────────────────

/// Custom texture-shader source for SDF squircle clipping.
///
/// Receives standard smithay uniforms (`tex`, `alpha`, `tint` if `DEBUG_FLAGS`)
/// plus our own:
/// - `u_tex_size_px`        — texture size in physical pixels
/// - `u_surface_offset_px`  — surface top-left in window-local physical pixels
/// - `u_window_size_px`     — full window size in physical pixels
/// - `u_radius_px`          — squircle corner radius in physical pixels
/// - `u_smoothing`          — squircle smoothing factor 0..1
const FRAGMENT_SHADER: &str = "
//_DEFINES

precision mediump float;

#if defined(EXTERNAL)
uniform samplerExternalOES tex;
#else
uniform sampler2D tex;
#endif
uniform float alpha;
#if defined(DEBUG_FLAGS)
uniform float tint;
#endif

uniform vec2  u_tex_size_px;
uniform vec2  u_surface_offset_px;
uniform vec2  u_window_size_px;
uniform float u_radius_px;
uniform float u_smoothing;

varying vec2 v_coords;

float squircle_sdf(vec2 pos, vec2 hs, float r, float sm) {
    float p_scale  = 1.0 + sm * 0.7;
    float blend_k  = 8.0 + sm * 16.0;

    vec2 q = abs(pos) - hs + vec2(r);
    float arc_dist = length(max(q, vec2(0.0))) + min(max(q.x, q.y), 0.0) - r;

    if (sm < 0.001) return arc_dist;

    float reach = r * p_scale;
    vec2  q2    = abs(pos) - hs + vec2(reach);
    float reach_dist =
        length(max(q2, vec2(0.0))) + min(max(q2.x, q2.y), 0.0) - reach;

    float corner_prox = -min(max(q.x, q.y), 0.0) / max(r, 0.001);
    float blend = clamp(corner_prox * blend_k - (blend_k - 1.0), 0.0, 1.0);

    return mix(arc_dist, reach_dist * (r / max(reach, 0.001)), blend * sm);
}

void main() {
    vec2 pos_in_tex    = v_coords * u_tex_size_px;
    vec2 pos_in_window = pos_in_tex + u_surface_offset_px;
    vec2 from_center   = pos_in_window - u_window_size_px * 0.5;

    float d    = squircle_sdf(from_center, u_window_size_px * 0.5, u_radius_px, u_smoothing);
    float clip = 1.0 - smoothstep(-0.5, 0.5, d);

#if defined(NO_ALPHA)
    vec4 color = vec4(texture2D(tex, v_coords).rgb, 1.0) * (alpha * clip);
#else
    vec4 color = texture2D(tex, v_coords) * (alpha * clip);
#endif

#if defined(DEBUG_FLAGS)
    if (tint == 1.0) {
        color = mix(color, vec4(0.0, 0.3, 0.0, 0.2), 0.5);
    }
#endif

    gl_FragColor = color;
}
";

/// Compile the squircle-clip [`GlesTexProgram`].
///
/// Cache the returned program — it is expensive to compile and the
/// shader source never changes at runtime.
pub fn compile_clip_program(renderer: &mut GlesRenderer) -> Result<GlesTexProgram, GlesError> {
    renderer.compile_custom_texture_shader(
        FRAGMENT_SHADER,
        &[
            UniformName::new("u_tex_size_px", UniformType::_2f),
            UniformName::new("u_surface_offset_px", UniformType::_2f),
            UniformName::new("u_window_size_px", UniformType::_2f),
            UniformName::new("u_radius_px", UniformType::_1f),
            UniformName::new("u_smoothing", UniformType::_1f),
        ],
    )
}

// ─── Element builder ────────────────────────────────────────────────────────

/// Per-window parameters needed to clip the surface.
#[derive(Debug, Clone, Copy)]
pub struct ClipParams {
    /// Full window size (= title bar + content) in *physical* pixels.
    pub window_size_px: (f32, f32),
    /// Top-left of this surface relative to the window origin, in physical pixels.
    /// For SSD windows this is `(0, title_bar_height_px)`.
    /// For CSD windows this is `(0, 0)`.
    pub surface_offset_px: (f32, f32),
    /// Squircle corner radius in physical pixels.
    pub radius_px: f32,
    /// Squircle smoothing factor.
    pub smoothing: f32,
    /// Alpha multiplier (window opacity from animation state).
    pub alpha: f32,
}

/// Build a [`TextureShaderElement`] that draws `window`'s main wayland surface
/// at `physical_location` clipped to the squircle described by `params`.
///
/// Returns `None` if the surface has no buffer / texture (e.g. not yet
/// committed) or if buffer import fails. The caller should fall back to
/// rendering the window through the regular space pipeline in that case.
pub fn build_clipped_element(
    renderer: &mut GlesRenderer,
    clip_program: &GlesTexProgram,
    window: &Window,
    physical_location: Point<f64, smithay::utils::Physical>,
    output_scale: f64,
    params: ClipParams,
) -> Option<TextureShaderElement> {
    let surface = match window.underlying_surface() {
        smithay::desktop::WindowSurface::Wayland(t) => t.wl_surface().clone(),
        // X11 windows go through a different surface element; not yet supported.
        _ => return None,
    };

    // Make sure the renderer has imported the latest buffers for this tree.
    let _ = import_surface_tree(renderer, &surface);

    // Pull the imported texture, buffer-scale and buffer-transform out of
    // the cached renderer state. We do *not* iterate sub-surfaces here —
    // only the root toplevel is squircle-clipped.
    let context_id = renderer.context_id();
    let surface_info = with_renderer_surface_state(&surface, |state: &mut RendererSurfaceState| {
        let texture = state.texture::<smithay::backend::renderer::gles::GlesTexture>(context_id.clone())?.clone();
        let view = state.view()?;
        let buffer_scale = state.buffer_scale();
        let buffer_transform = state.buffer_transform();
        let buffer_size = state.buffer_size()?;
        Some(SurfaceImport {
            texture,
            view,
            buffer_scale,
            buffer_transform,
            buffer_size,
        })
    })
    .flatten()?;

    let tex_buffer = TextureBuffer::from_texture(
        renderer,
        surface_info.texture,
        surface_info.buffer_scale,
        surface_info.buffer_transform,
        None,
    );

    // Pin the destination size in *logical* pixels to match what the
    // surface view would have rendered at — that way the shader's
    // `u_tex_size_px` matches the actual on-screen footprint.
    let dst_logical = surface_info.view.dst;

    let tex_elem = TextureRenderElement::from_texture_buffer(
        physical_location,
        &tex_buffer,
        Some(params.alpha),
        // src in buffer coords
        Some(Rectangle::from_size(
            (dst_logical.w as f64, dst_logical.h as f64).into(),
        )),
        Some(dst_logical),
        Kind::Unspecified,
    );

    // Compute the actual on-screen pixel size that the texture will cover.
    // This is what the shader treats as `u_tex_size_px`.
    let tex_w_phys = (dst_logical.w as f64 * output_scale) as f32;
    let tex_h_phys = (dst_logical.h as f64 * output_scale) as f32;

    let uniforms = vec![
        Uniform::new("u_tex_size_px", [tex_w_phys, tex_h_phys]),
        Uniform::new(
            "u_surface_offset_px",
            [params.surface_offset_px.0, params.surface_offset_px.1],
        ),
        Uniform::new(
            "u_window_size_px",
            [params.window_size_px.0, params.window_size_px.1],
        ),
        Uniform::new("u_radius_px", params.radius_px),
        Uniform::new("u_smoothing", params.smoothing),
    ];

    Some(TextureShaderElement::new(
        tex_elem,
        clip_program.clone(),
        uniforms,
    ))
}

/// Same as [`build_clipped_element`] but lets the caller override the
/// destination size in logical pixels — used by the open/close scale
/// animation, where the surface texture stays the same but the
/// rendered footprint shrinks toward the window centre.
pub fn build_clipped_element_sized(
    renderer: &mut GlesRenderer,
    clip_program: &GlesTexProgram,
    window: &Window,
    physical_location: Point<f64, smithay::utils::Physical>,
    output_scale: f64,
    params: ClipParams,
    dst_size_logical: Option<smithay::utils::Size<i32, smithay::utils::Logical>>,
) -> Option<TextureShaderElement> {
    let surface = match window.underlying_surface() {
        smithay::desktop::WindowSurface::Wayland(t) => t.wl_surface().clone(),
        _ => return None,
    };

    let _ = import_surface_tree(renderer, &surface);

    let context_id = renderer.context_id();
    let surface_info = with_renderer_surface_state(&surface, |state: &mut RendererSurfaceState| {
        let texture = state
            .texture::<smithay::backend::renderer::gles::GlesTexture>(context_id.clone())?
            .clone();
        let view = state.view()?;
        let buffer_scale = state.buffer_scale();
        let buffer_transform = state.buffer_transform();
        let buffer_size = state.buffer_size()?;
        Some(SurfaceImport {
            texture,
            view,
            buffer_scale,
            buffer_transform,
            buffer_size,
        })
    })
    .flatten()?;

    let tex_buffer = TextureBuffer::from_texture(
        renderer,
        surface_info.texture,
        surface_info.buffer_scale,
        surface_info.buffer_transform,
        None,
    );

    // Native logical destination — used as a fallback when the caller
    // doesn't override the size (i.e. animation has settled).
    let native_dst = surface_info.view.dst;
    let dst_logical = dst_size_logical.unwrap_or(native_dst);

    let tex_elem = TextureRenderElement::from_texture_buffer(
        physical_location,
        &tex_buffer,
        Some(params.alpha),
        // src is in buffer coords and tracks the *full* source texture,
        // not the (possibly) shrunken destination. Sampling the whole
        // source while drawing into a smaller dst produces a clean
        // bilinear shrink rather than a partial sample.
        Some(Rectangle::from_size(
            (native_dst.w as f64, native_dst.h as f64).into(),
        )),
        Some(dst_logical),
        Kind::Unspecified,
    );

    // The shader's `u_tex_size_px` follows the *destination* footprint
    // because the SDF is computed in the same coord space as the
    // pixels actually being drawn.
    let tex_w_phys = (dst_logical.w as f64 * output_scale) as f32;
    let tex_h_phys = (dst_logical.h as f64 * output_scale) as f32;

    let uniforms = vec![
        Uniform::new("u_tex_size_px", [tex_w_phys, tex_h_phys]),
        Uniform::new(
            "u_surface_offset_px",
            [params.surface_offset_px.0, params.surface_offset_px.1],
        ),
        Uniform::new(
            "u_window_size_px",
            [params.window_size_px.0, params.window_size_px.1],
        ),
        Uniform::new("u_radius_px", params.radius_px),
        Uniform::new("u_smoothing", params.smoothing),
    ];

    Some(TextureShaderElement::new(
        tex_elem,
        clip_program.clone(),
        uniforms,
    ))
}

/// Helper struct grouping the per-surface state we need from
/// `RendererSurfaceState`. Exists only because `with_renderer_surface_state`
/// can't return a borrow.
struct SurfaceImport {
    texture: smithay::backend::renderer::gles::GlesTexture,
    view: smithay::backend::renderer::utils::SurfaceView,
    buffer_scale: i32,
    buffer_transform: Transform,
    buffer_size: smithay::utils::Size<i32, smithay::utils::Logical>,
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fragment_shader_has_required_markers() {
        // Smithay rewrites `//_DEFINES` and pulls out `void main`.
        assert!(FRAGMENT_SHADER.contains("//_DEFINES"));
        assert!(FRAGMENT_SHADER.contains("void main"));
        assert!(FRAGMENT_SHADER.contains("squircle_sdf"));
        // Required uniforms appear at least once.
        for n in [
            "u_tex_size_px",
            "u_surface_offset_px",
            "u_window_size_px",
            "u_radius_px",
            "u_smoothing",
        ] {
            assert!(FRAGMENT_SHADER.contains(n), "missing uniform {n}");
        }
    }

    #[test]
    fn clip_params_construction() {
        let p = ClipParams {
            window_size_px: (400.0, 300.0),
            surface_offset_px: (0.0, 33.0),
            radius_px: 14.0,
            smoothing: 0.6,
            alpha: 1.0,
        };
        assert_eq!(p.window_size_px, (400.0, 300.0));
    }
}
