//! Server-side decoration chrome for layer-shell surfaces.
//!
//! Layer surfaces like the dock, the date/time popout, and the control-centre
//! popout receive the same macOS-inspired chrome as toplevel windows:
//! squircle clipping, multi-layer shadow, outer 0.5 px border, and the
//! vertical-gradient inner highlight.
//!
//! The panel (`shell-panel`) is intentionally excluded — it is flush to the
//! screen edge and draws its own bottom border.
//!
//! ## Chrome kinds
//!
//! | Namespace   | Kind             |
//! |-------------|------------------|
//! | `datetime`  | `Full`           |
//! | `cc`        | `Full`           |
//! | `shell-dock`| `Full`           |
//! | `shell-panel`| (none — excluded)|
//!
//! Both `Full` and `Dock` end up using the same **dark_active** spec values
//! from `WINDOW_SPEC.md`: 14 px squircle radius, 3-layer shadow, 0.5 px
//! outer stroke at `rgba(0,0,0,0.72)`, 1 px inner highlight top at
//! `rgba(255,255,255,0.08)`.

use smithay::{
    backend::renderer::{
        element::{
            texture::{TextureBuffer, TextureRenderElement},
            Kind,
        },
        gles::{element::TextureShaderElement, GlesRenderer, GlesTexProgram, Uniform},
        utils::{import_surface_tree, with_renderer_surface_state, RendererSurfaceState},
        Renderer,
    },
    desktop::LayerSurface,
    utils::{Point, Rectangle, Transform},
};

use crate::render::window_chrome::{ChromeKey, WindowChromeCache};

// ─── Chrome kind ─────────────────────────────────────────────────────────────

/// What kind of compositor-side chrome a layer surface should receive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayerChromeKind {
    /// Full chrome: squircle clip + shadow + outer border + inner highlight.
    /// Used by `datetime`, `cc`, and `shell-dock`.
    Full,
}

/// Return the chrome variant for a layer surface namespace, or `None` if the
/// surface should not receive any chrome.
///
/// Rules per the task spec:
/// - `shell-panel` — NO chrome (flush to top edge; client draws its own border).
/// - `shell-dock`  — full chrome.
/// - `datetime`    — full chrome.
/// - `cc`          — full chrome.
/// - anything else — no chrome (unknown surfaces are not decorated).
pub fn layer_wants_chrome(namespace: &str) -> Option<LayerChromeKind> {
    match namespace {
        "datetime" | "cc" | "shell-dock" => Some(LayerChromeKind::Full),
        _ => None,
    }
}

// ─── Squircle-clipped layer surface element ───────────────────────────────────

/// Helper struct grouping the per-surface state we need from
/// `RendererSurfaceState`.
struct LayerSurfaceImport {
    texture: smithay::backend::renderer::gles::GlesTexture,
    view: smithay::backend::renderer::utils::SurfaceView,
    buffer_scale: i32,
    buffer_transform: Transform,
}

/// Build a [`TextureShaderElement`] that draws a layer surface's wl_surface
/// at `physical_location` clipped to the squircle described by `radius_px` /
/// `smoothing`.
///
/// Returns `None` if the surface has no buffer yet (not yet committed) or if
/// buffer import fails.
pub fn build_clipped_layer_element(
    renderer: &mut GlesRenderer,
    clip_program: &GlesTexProgram,
    layer: &LayerSurface,
    physical_location: Point<f64, smithay::utils::Physical>,
    output_scale: f64,
    geo_size_logical: smithay::utils::Size<i32, smithay::utils::Logical>,
    radius_px: f32,
    smoothing: f32,
    alpha: f32,
) -> Option<TextureShaderElement> {
    let surface = layer.wl_surface().clone();

    // Import the latest buffer for the surface tree.
    let _ = import_surface_tree(renderer, &surface);

    let context_id = renderer.context_id();
    let surface_info =
        with_renderer_surface_state(&surface, |state: &mut RendererSurfaceState| {
            let texture = state
                .texture::<smithay::backend::renderer::gles::GlesTexture>(context_id.clone())?
                .clone();
            let view = state.view()?;
            let buffer_scale = state.buffer_scale();
            let buffer_transform = state.buffer_transform();
            Some(LayerSurfaceImport {
                texture,
                view,
                buffer_scale,
                buffer_transform,
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

    // Native logical destination — the surface view tells us the logical size
    // the compositor expects to render at.
    let native_dst = surface_info.view.dst;
    // Use the compositor-known geometry size as the override so we are not
    // subject to what the client last committed.
    let dst_logical = geo_size_logical;

    let tex_elem = TextureRenderElement::from_texture_buffer(
        physical_location,
        &tex_buffer,
        Some(alpha),
        // Src covers the full native texture.
        Some(Rectangle::from_size(
            (native_dst.w as f64, native_dst.h as f64).into(),
        )),
        Some(dst_logical),
        Kind::Unspecified,
    );

    // The shader computes the SDF in on-screen physical pixels.
    let tex_w_phys = (dst_logical.w as f64 * output_scale) as f32;
    let tex_h_phys = (dst_logical.h as f64 * output_scale) as f32;

    // For layer surfaces the surface fills the full squircle rect (no title bar
    // offset), so surface_offset_px is (0, 0) and window_size_px == tex_size_px.
    let uniforms = vec![
        Uniform::new("u_tex_size_px", [tex_w_phys, tex_h_phys]),
        Uniform::new("u_surface_offset_px", [0.0_f32, 0.0_f32]),
        Uniform::new("u_window_size_px", [tex_w_phys, tex_h_phys]),
        Uniform::new("u_radius_px", radius_px),
        Uniform::new("u_smoothing", smoothing),
    ];

    Some(TextureShaderElement::new(
        tex_elem,
        clip_program.clone(),
        uniforms,
    ))
}

// ─── Chrome buffer builder ────────────────────────────────────────────────────

/// Build (or look up in the cache) the shadow + decoration chrome textures for
/// a chrome-eligible layer surface and return the pre-positioned data needed
/// to emit render elements.
pub struct LayerChromeBuffers {
    /// The shadow texture. Position at `(geo_x - shadow_pad, geo_y - shadow_pad)`.
    pub shadow: smithay::backend::renderer::element::memory::MemoryRenderBuffer,
    /// Physical location for the shadow texture (top-left).
    pub shadow_phys_loc: Point<f64, smithay::utils::Physical>,
    /// Logical size of the shadow buffer (window + 2× padding each axis).
    pub shadow_size_logical: smithay::utils::Size<i32, smithay::utils::Logical>,
    /// The decoration overlay (outer stroke + inner highlight). Same size as
    /// the surface rect.
    pub decoration: smithay::backend::renderer::element::memory::MemoryRenderBuffer,
    /// Physical location for the decoration buffer (same as surface top-left).
    pub decoration_phys_loc: Point<f64, smithay::utils::Physical>,
}

/// Get or build chrome buffers for a layer surface.
///
/// `geo` is the output-logical geometry of the layer surface.
/// `scale` is the output fractional scale.
/// Always uses the dark-active border style (all popouts/dock are dark).
pub fn get_or_build_layer_chrome(
    cache: &mut WindowChromeCache,
    theme: &theme::WindowTheme,
    geo: smithay::utils::Rectangle<i32, smithay::utils::Logical>,
    scale: smithay::utils::Scale<f64>,
) -> LayerChromeBuffers {
    // Layer surfaces always use dark-active styling.
    let border = &theme.border.dark_active;

    let key = ChromeKey::new(
        geo.size.w as f64,
        geo.size.h as f64,
        scale.x,
        true,  // always "active" for layer surfaces
        true,  // always dark
    );

    let bundle = cache.get_or_build(key, theme, border);

    let pad = bundle.shadow_padding_logical;
    let shadow_phys_loc: Point<f64, smithay::utils::Physical> = (
        (geo.loc.x as f64 - pad) * scale.x,
        (geo.loc.y as f64 - pad) * scale.y,
    )
        .into();
    let decoration_phys_loc: Point<f64, smithay::utils::Physical> = (
        geo.loc.x as f64 * scale.x,
        geo.loc.y as f64 * scale.y,
    )
        .into();
    let shadow_size_logical: smithay::utils::Size<i32, smithay::utils::Logical> = (
        geo.size.w + (2.0 * pad).round() as i32,
        geo.size.h + (2.0 * pad).round() as i32,
    )
        .into();

    LayerChromeBuffers {
        shadow: bundle.shadow.clone(),
        shadow_phys_loc,
        shadow_size_logical,
        decoration: bundle.decoration.clone(),
        decoration_phys_loc,
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panel_gets_no_chrome() {
        assert_eq!(layer_wants_chrome("shell-panel"), None);
    }

    #[test]
    fn dock_gets_full_chrome() {
        assert_eq!(layer_wants_chrome("shell-dock"), Some(LayerChromeKind::Full));
    }

    #[test]
    fn datetime_gets_full_chrome() {
        assert_eq!(layer_wants_chrome("datetime"), Some(LayerChromeKind::Full));
    }

    #[test]
    fn cc_gets_full_chrome() {
        assert_eq!(layer_wants_chrome("cc"), Some(LayerChromeKind::Full));
    }

    #[test]
    fn unknown_namespace_gets_no_chrome() {
        assert_eq!(layer_wants_chrome("launcher"), None);
        assert_eq!(layer_wants_chrome("notification"), None);
        assert_eq!(layer_wants_chrome(""), None);
    }
}
