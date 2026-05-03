//! Drop-shadow pass — rasterises the squircle silhouette, runs a separable
//! Gaussian blur over it, then composites the blurred mask into the output
//! at (shadow_ox, shadow_oy) with `shadow_a` opacity.
//!
//! Per shadow layer the renderer issues 4 sub-passes:
//!     silhouette → blur_h → blur_v → composite
//!
//! No mask cache: re-rendering each frame is cheap (one full-surface alpha
//! pass + two 1D blur passes per layer) and removes the position-blind cache
//! bug that the old monolithic shader had.

use wgpu;

use super::common::{blend_src_over_premul, make_pipeline, ChromeUniforms, Shared};

const SHADER_SRC: &str = include_str!("../../shaders/shadow.wgsl");

pub struct ShadowPass {
    silhouette_pipeline: wgpu::RenderPipeline,
    blur_h_pipeline: wgpu::RenderPipeline,
    blur_v_pipeline: wgpu::RenderPipeline,
    composite_pipeline: wgpu::RenderPipeline,

    /// Intermediate textures at full surface size; reallocated when the
    /// surface resizes (cheap, only on output change).
    intermediates: Option<Intermediates>,
}

struct Intermediates {
    width: u32,
    height: u32,
    mask: wgpu::Texture,
    mask_view: wgpu::TextureView,
    hblur: wgpu::Texture,
    hblur_view: wgpu::TextureView,
    vblur: wgpu::Texture,
    vblur_view: wgpu::TextureView,
}

/// Spec for one drop-shadow layer (offset + blur + alpha).
#[derive(Debug, Clone, Copy)]
pub struct ShadowLayer {
    pub offset_x: f32,
    pub offset_y: f32,
    pub blur_sigma: f32,
    pub alpha: f32,
}

impl ShadowPass {
    pub fn new(shared: &Shared) -> Self {
        let silhouette_pipeline = make_pipeline(
            &shared.device,
            &shared.bgl,
            "shadow-silhouette",
            SHADER_SRC,
            "fs_silhouette",
            shared.format,
            None,
        );
        let blur_h_pipeline = make_pipeline(
            &shared.device,
            &shared.bgl,
            "shadow-blur-h",
            SHADER_SRC,
            "fs_blur_h",
            shared.format,
            None,
        );
        let blur_v_pipeline = make_pipeline(
            &shared.device,
            &shared.bgl,
            "shadow-blur-v",
            SHADER_SRC,
            "fs_blur_v",
            shared.format,
            None,
        );
        let composite_pipeline = make_pipeline(
            &shared.device,
            &shared.bgl,
            "shadow-composite",
            SHADER_SRC,
            "fs_composite",
            shared.format,
            Some(blend_src_over_premul()),
        );
        Self {
            silhouette_pipeline,
            blur_h_pipeline,
            blur_v_pipeline,
            composite_pipeline,
            intermediates: None,
        }
    }

    /// Drop intermediates so they're reallocated at the next render.
    pub fn invalidate(&mut self) {
        self.intermediates = None;
    }

    fn ensure_intermediates(&mut self, shared: &Shared, w: u32, h: u32) {
        if self
            .intermediates
            .as_ref()
            .map_or(true, |i| i.width != w || i.height != h)
        {
            let mk = |label: &str| {
                let tex = shared.device.create_texture(&wgpu::TextureDescriptor {
                    label: Some(label),
                    size: wgpu::Extent3d {
                        width: w,
                        height: h,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: shared.format,
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                        | wgpu::TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                });
                let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
                (tex, view)
            };
            let (mask, mask_view) = mk("shadow-mask");
            let (hblur, hblur_view) = mk("shadow-hblur");
            let (vblur, vblur_view) = mk("shadow-vblur");
            self.intermediates = Some(Intermediates {
                width: w,
                height: h,
                mask,
                mask_view,
                hblur,
                hblur_view,
                vblur,
                vblur_view,
            });
        }
    }

    /// Run one shadow layer: silhouette → blur-h → blur-v → composite.
    /// Caller has already written the per-call ChromeUniforms to slot
    /// `dynamic_offset` in `shared.dynamic_uniform_buf`.
    pub fn render_layer(
        &mut self,
        shared: &Shared,
        encoder: &mut wgpu::CommandEncoder,
        target_view: &wgpu::TextureView,
        surface_w: u32,
        surface_h: u32,
        dynamic_offset: u32,
    ) {
        self.ensure_intermediates(shared, surface_w, surface_h);
        let im = self.intermediates.as_ref().unwrap();

        // The silhouette pass declares t_src in its layout but doesn't sample
        // it; bind a *different* texture so wgpu doesn't see mask_view as both
        // colour-attachment AND sampled resource within the same pass scope.
        let dummy_bg = shared.make_bind_group("shadow-dummy-bg", &im.vblur_view);
        let mask_bg = shared.make_bind_group("shadow-mask-bg", &im.mask_view);
        let hblur_bg = shared.make_bind_group("shadow-hblur-bg", &im.hblur_view);
        let vblur_bg = shared.make_bind_group("shadow-vblur-bg", &im.vblur_view);

        // ── 1. silhouette → mask ────────────────────────────────────────────
        {
            let mut p = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("shadow-silhouette-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &im.mask_view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            p.set_pipeline(&self.silhouette_pipeline);
            p.set_bind_group(0, &dummy_bg, &[dynamic_offset]);
            p.draw(0..6, 0..1);
        }

        // ── 2. mask → hblur ─────────────────────────────────────────────────
        {
            let mut p = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("shadow-blur-h-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &im.hblur_view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            p.set_pipeline(&self.blur_h_pipeline);
            p.set_bind_group(0, &mask_bg, &[dynamic_offset]);
            p.draw(0..6, 0..1);
        }

        // ── 3. hblur → vblur ────────────────────────────────────────────────
        {
            let mut p = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("shadow-blur-v-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &im.vblur_view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            p.set_pipeline(&self.blur_v_pipeline);
            p.set_bind_group(0, &hblur_bg, &[dynamic_offset]);
            p.draw(0..6, 0..1);
        }

        // ── 4. composite vblur → target with shadow offset + alpha ──────────
        {
            let mut p = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("shadow-composite-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target_view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            p.set_pipeline(&self.composite_pipeline);
            p.set_bind_group(0, &vblur_bg, &[dynamic_offset]);
            p.draw(0..6, 0..1);
        }
    }
}

/// macOS-style 2-layer drop shadow for an active window. Tight falloff
/// (max ~18 px wide), low alphas — combined peak ~9% darkening only at the
/// immediate edge. Should read as soft depth, not a frame.
pub fn active_layers() -> [ShadowLayer; 3] {
    [
        // Crisp 1 px contact line right at the edge.
        ShadowLayer {
            offset_x: 0.0,
            offset_y: 0.0,
            blur_sigma: 1.0,
            alpha: 0.035,
        },
        // Main soft drop.
        ShadowLayer {
            offset_x: 0.0,
            offset_y: 3.0,
            blur_sigma: 6.0,
            alpha: 0.05,
        },
        // Slot kept for compatibility — alpha 0 so it's a no-op (focus_t
        // crossfade still uses 3 layer indices in render/mod.rs).
        ShadowLayer {
            offset_x: 0.0,
            offset_y: 0.0,
            blur_sigma: 1.0,
            alpha: 0.0,
        },
    ]
}

/// Lighter 2-layer shadow for inactive windows. Combined peak ~3.5%.
pub fn inactive_layers() -> [ShadowLayer; 2] {
    [
        ShadowLayer {
            offset_x: 0.0,
            offset_y: 0.0,
            blur_sigma: 1.0,
            alpha: 0.015,
        },
        ShadowLayer {
            offset_x: 0.0,
            offset_y: 2.0,
            blur_sigma: 4.0,
            alpha: 0.020,
        },
    ]
}

/// Build the ChromeUniforms for a shadow-layer call.
pub fn uniforms_for_layer(
    win_x: f32,
    win_y: f32,
    win_w: f32,
    win_h: f32,
    surface_w: f32,
    surface_h: f32,
    layer: ShadowLayer,
    alpha_scale: f32,
) -> ChromeUniforms {
    ChromeUniforms {
        win_x,
        win_y,
        win_w,
        win_h,
        surface_w,
        surface_h,
        radius_px: 14.0,
        smoothing: 0.6,
        mode_t: 0.0,
        focus_t: 1.0,
        shadow_a: layer.alpha * alpha_scale,
        shadow_oy: layer.offset_y,
        shadow_ox: layer.offset_x,
        blur_sigma: layer.blur_sigma,
        _pad0: 0.0,
        _pad1: 0.0,
    }
}
