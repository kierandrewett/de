//! Squircle clip pass — applies a squircle alpha mask over the scene texture
//! at the window region, replacing whatever Slint drew there.
//!
//! Disabled by default in `ChromeRenderer::render` until WindowChrome's
//! Slint-side `border-radius` is dropped (otherwise we'd be applying a
//! squircle on top of an already-circular-arc rounded rect).

use wgpu;

use super::common::{blend_src_over_premul, make_pipeline, Shared};

const SHADER_SRC: &str = include_str!("../../shaders/squircle.wgsl");

pub struct SquircleClipPass {
    pipeline: wgpu::RenderPipeline,
}

impl SquircleClipPass {
    pub fn new(shared: &Shared) -> Self {
        let pipeline = make_pipeline(
            &shared.device,
            &shared.bgl,
            "squircle-clip",
            SHADER_SRC,
            "fs_main",
            shared.format,
            Some(blend_src_over_premul()),
        );
        Self { pipeline }
    }

    /// Run the clip pass — blends the squircle-masked scene into the target.
    pub fn render(
        &self,
        shared: &Shared,
        encoder: &mut wgpu::CommandEncoder,
        scene_view: &wgpu::TextureView,
        target_view: &wgpu::TextureView,
        dynamic_offset: u32,
    ) {
        let bg = shared.make_bind_group("squircle-clip-bg", scene_view);
        let mut p = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("squircle-clip-pass"),
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
        p.set_pipeline(&self.pipeline);
        p.set_bind_group(0, &bg, &[dynamic_offset]);
        p.draw(0..6, 0..1);
    }
}
