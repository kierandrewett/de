//! Real-time backdrop-blur pass — separable Gaussian on RGBA.
//!
//! Caller uses this to blur a rectangular region of `final_tex` (e.g. the
//! dock pill or the top panel) by sampling a separate "scene" texture (the
//! Slint scene rendered without overlay UI). The pipeline:
//!
//!   scene_tex  ── fs_blur_h_rgba ──► hblur scratch
//!   hblur      ── fs_blur_v_rgba ──► vblur scratch
//!   vblur      ── fs_composite_*  ──► target_tex (alpha-blended)
//!
//! Two scratch textures are allocated at full surface size (cheap; same
//! shape as ShadowPass's intermediates). A composite pass with squircle
//! masking writes the blurred result back to the final_tex within the rect.
//!
//! There's also `overlay_blit`: a simple sample-and-write pass used to
//! alpha-composite the overlay slint render (dock + panel UI on transparent
//! bg) onto final_tex after the blur is done.

use wgpu;

use super::common::{blend_src_over_premul, make_pipeline, ChromeUniforms, Shared, UNIFORM_STRIDE};

const SHADER_SRC: &str = include_str!("../../shaders/blur.wgsl");

pub struct BlurPass {
    blur_h_pipeline:        wgpu::RenderPipeline,
    blur_v_pipeline:        wgpu::RenderPipeline,
    composite_pill_pipeline: wgpu::RenderPipeline,
    composite_rect_pipeline: wgpu::RenderPipeline,
    overlay_blit_pipeline:   wgpu::RenderPipeline,

    intermediates: Option<Intermediates>,
}

struct Intermediates {
    width:      u32,
    height:     u32,
    hblur:      wgpu::Texture,
    hblur_view: wgpu::TextureView,
    vblur:      wgpu::Texture,
    vblur_view: wgpu::TextureView,
}

#[derive(Debug, Clone, Copy)]
pub enum BlurShape {
    /// Squircle pill (dock) — radius_px / smoothing used for the SDF.
    Pill,
    /// Rectangular (panel).
    Rect,
}

impl BlurPass {
    pub fn new(shared: &Shared) -> Self {
        let blur_h_pipeline = make_pipeline(
            &shared.device, &shared.bgl,
            "blur-h-rgba", SHADER_SRC, "fs_blur_h_rgba",
            shared.format, None,
        );
        let blur_v_pipeline = make_pipeline(
            &shared.device, &shared.bgl,
            "blur-v-rgba", SHADER_SRC, "fs_blur_v_rgba",
            shared.format, None,
        );
        let composite_pill_pipeline = make_pipeline(
            &shared.device, &shared.bgl,
            "blur-composite-pill", SHADER_SRC, "fs_composite_pill",
            shared.format, Some(blend_src_over_premul()),
        );
        let composite_rect_pipeline = make_pipeline(
            &shared.device, &shared.bgl,
            "blur-composite-rect", SHADER_SRC, "fs_composite_rect",
            shared.format, Some(blend_src_over_premul()),
        );
        let overlay_blit_pipeline = make_pipeline(
            &shared.device, &shared.bgl,
            "overlay-blit", SHADER_SRC, "fs_overlay_blit",
            shared.format, Some(blend_src_over_premul()),
        );
        Self {
            blur_h_pipeline,
            blur_v_pipeline,
            composite_pill_pipeline,
            composite_rect_pipeline,
            overlay_blit_pipeline,
            intermediates: None,
        }
    }

    pub fn invalidate(&mut self) {
        self.intermediates = None;
    }

    fn ensure_intermediates(&mut self, shared: &Shared, w: u32, h: u32) {
        if self.intermediates.as_ref().map_or(true, |i| i.width != w || i.height != h) {
            let mk = |label: &str| {
                let tex = shared.device.create_texture(&wgpu::TextureDescriptor {
                    label: Some(label),
                    size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
                    mip_level_count: 1,
                    sample_count:    1,
                    dimension:       wgpu::TextureDimension::D2,
                    format:          shared.format,
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                         | wgpu::TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                });
                let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
                (tex, view)
            };
            let (hblur, hblur_view) = mk("blur-hblur-rgba");
            let (vblur, vblur_view) = mk("blur-vblur-rgba");
            self.intermediates = Some(Intermediates {
                width: w, height: h, hblur, hblur_view, vblur, vblur_view,
            });
        }
    }

    /// Blur `source_view` within (rect_x, rect_y, rect_w, rect_h) and
    /// composite the result onto `target_view`.
    ///
    /// Uniforms have already been written to `dynamic_offset` in
    /// `shared.dynamic_uniform_buf` (the caller is responsible for packing
    /// `ChromeUniforms` with win_x/y/w/h, blur_sigma, radius_px/smoothing).
    pub fn blur_into(
        &mut self,
        shared:         &Shared,
        encoder:        &mut wgpu::CommandEncoder,
        source_view:    &wgpu::TextureView,
        target_view:    &wgpu::TextureView,
        surface_w:      u32,
        surface_h:      u32,
        dynamic_offset: u32,
        shape:          BlurShape,
    ) {
        self.ensure_intermediates(shared, surface_w, surface_h);
        let im = self.intermediates.as_ref().unwrap();

        let source_bg = shared.make_bind_group("blur-source-bg", source_view);
        let hblur_bg  = shared.make_bind_group("blur-hblur-bg",  &im.hblur_view);
        let vblur_bg  = shared.make_bind_group("blur-vblur-bg",  &im.vblur_view);

        // ── 1. source → hblur ───────────────────────────────────────────────
        {
            let mut p = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("blur-h-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &im.hblur_view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load:  wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            p.set_pipeline(&self.blur_h_pipeline);
            p.set_bind_group(0, &source_bg, &[dynamic_offset]);
            p.draw(0..6, 0..1);
        }

        // ── 2. hblur → vblur ────────────────────────────────────────────────
        {
            let mut p = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("blur-v-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &im.vblur_view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load:  wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
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

        // ── 3. composite vblur → target ─────────────────────────────────────
        {
            let mut p = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("blur-composite-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target_view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load:  wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            let pipeline = match shape {
                BlurShape::Pill => &self.composite_pill_pipeline,
                BlurShape::Rect => &self.composite_rect_pipeline,
            };
            p.set_pipeline(pipeline);
            p.set_bind_group(0, &vblur_bg, &[dynamic_offset]);
            p.draw(0..6, 0..1);
        }
    }

    /// Alpha-blend `overlay_view` onto `target_view`. If win_w/win_h in the
    /// supplied uniforms are 0, the blit covers the whole surface; otherwise
    /// it's clipped to the (win_x, win_y, win_w, win_h) rect.
    pub fn overlay_blit(
        &mut self,
        shared:         &Shared,
        encoder:        &mut wgpu::CommandEncoder,
        overlay_view:   &wgpu::TextureView,
        target_view:    &wgpu::TextureView,
        dynamic_offset: u32,
    ) {
        let bg = shared.make_bind_group("overlay-blit-bg", overlay_view);
        let mut p = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("overlay-blit-pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target_view,
                resolve_target: None,
                depth_slice: None,
                ops: wgpu::Operations {
                    load:  wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        p.set_pipeline(&self.overlay_blit_pipeline);
        p.set_bind_group(0, &bg, &[dynamic_offset]);
        p.draw(0..6, 0..1);
    }
}

/// Build ChromeUniforms for a blur-rect call. `radius_px` and `smoothing`
/// are only consulted by the pill composite pass.
pub fn uniforms_for_blur(
    rect_x: f32, rect_y: f32, rect_w: f32, rect_h: f32,
    surface_w: f32, surface_h: f32,
    sigma: f32, radius_px: f32, smoothing: f32,
) -> ChromeUniforms {
    ChromeUniforms {
        win_x: rect_x, win_y: rect_y, win_w: rect_w, win_h: rect_h,
        surface_w, surface_h,
        radius_px, smoothing,
        mode_t: 0.0, focus_t: 0.0,
        shadow_a: 0.0, shadow_oy: 0.0, shadow_ox: 0.0,
        blur_sigma: sigma,
        _pad0: 0.0, _pad1: 0.0,
    }
}

/// Compute byte offset for a uniform-buffer slot.
pub fn slot_offset(slot: u32) -> u32 {
    (slot as u64 * UNIFORM_STRIDE) as u32
}
