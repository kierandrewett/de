//! Border pass — 0.5 px outer stroke at the squircle edge.

use wgpu;

use super::common::{blend_src_over_premul, make_pipeline, Shared};

const SHADER_SRC: &str = include_str!("../../shaders/border.wgsl");

pub struct BorderPass {
    pipeline: wgpu::RenderPipeline,
}

impl BorderPass {
    pub fn new(shared: &Shared) -> Self {
        let pipeline = make_pipeline(
            &shared.device,
            &shared.bgl,
            "border",
            SHADER_SRC,
            "fs_main",
            shared.format,
            Some(blend_src_over_premul()),
        );
        Self { pipeline }
    }

    /// Doesn't sample the scene, but the bind-group layout still expects a
    /// texture binding — bind any view (we use the scene view).
    pub fn render(
        &self,
        shared: &Shared,
        encoder: &mut wgpu::CommandEncoder,
        dummy_view: &wgpu::TextureView,
        target_view: &wgpu::TextureView,
        dynamic_offset: u32,
    ) {
        let bg = shared.make_bind_group("border-bg", dummy_view);
        let mut p = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("border-pass"),
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
