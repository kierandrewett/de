//! DnD icon textured-quad alpha-blend pass.
//!
//! Replaces the earlier `queue.write_texture` overwrite — which forced
//! semi-transparent drag previews to render as opaque. We upload the
//! icon to a per-frame GPU texture and run a single fullscreen-quad
//! pass that:
//!   - discards fragments outside the icon rect
//!   - samples the icon (straight-alpha) and pre-multiplies in shader
//!   - alpha-blends into the composite target with src-over
//!
//! The icon texture is rebuilt only when the icon's dimensions change;
//! pixels are re-uploaded each frame the icon is active. One draw call
//! per frame at most.

use std::sync::Arc;

use wgpu;

const SHADER_SRC: &str = include_str!("../../shaders/dnd_icon.wgsl");

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct DndUniforms {
    rect_x: f32,
    rect_y: f32,
    rect_w: f32,
    rect_h: f32,
    surface_w: f32,
    surface_h: f32,
    _pad0: f32,
    _pad1: f32,
}

pub struct DndIconPass {
    device: Arc<wgpu::Device>,
    pipeline: wgpu::RenderPipeline,
    bgl: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    uniforms: wgpu::Buffer,
    /// Cached icon texture; recreated when (w, h) change.
    icon_tex: Option<(wgpu::Texture, wgpu::TextureView, u32, u32)>,
}

impl DndIconPass {
    pub fn new(device: Arc<wgpu::Device>, format: wgpu::TextureFormat) -> Self {
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("dnd-icon-bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: std::num::NonZeroU64::new(
                            std::mem::size_of::<DndUniforms>() as u64,
                        ),
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("dnd-icon"),
            source: wgpu::ShaderSource::Wgsl(SHADER_SRC.into()),
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("dnd-icon"),
            bind_group_layouts: &[&bgl],
            immediate_size: 0,
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("dnd-icon"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(crate::render::common::blend_src_over_premul()),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("dnd-icon-sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        });

        let uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("dnd-icon-uniforms"),
            size: std::mem::size_of::<DndUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Self {
            device,
            pipeline,
            bgl,
            sampler,
            uniforms,
            icon_tex: None,
        }
    }

    /// Upload `pixels` (RGBA8, straight alpha, tightly packed) into the
    /// internal icon texture, recreating it if dimensions changed. The
    /// texture is bound by `render` for sampling.
    fn upload_icon(&mut self, queue: &wgpu::Queue, pixels: &[u8], w: u32, h: u32) {
        let need_new = match &self.icon_tex {
            Some((_, _, tw, th)) => *tw != w || *th != h,
            None => true,
        };
        if need_new {
            let tex = self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("dnd-icon-tex"),
                size: wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
            self.icon_tex = Some((tex, view, w, h));
        }
        let (tex, _, _, _) = self.icon_tex.as_ref().unwrap();
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            pixels,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(w * 4),
                rows_per_image: Some(h),
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
    }

    /// Run the alpha-blend pass. `rect_xywh` is the icon's destination
    /// rectangle in physical surface pixels; pixels outside it are
    /// discarded by the fragment shader so the rest of the swapchain is
    /// untouched.
    pub fn render(
        &mut self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        target_view: &wgpu::TextureView,
        surface_w: u32,
        surface_h: u32,
        rect_xywh: (f32, f32, f32, f32),
        pixels: &[u8],
        icon_w: u32,
        icon_h: u32,
    ) {
        if icon_w == 0 || icon_h == 0 || rect_xywh.2 <= 0.0 || rect_xywh.3 <= 0.0 {
            return;
        }
        self.upload_icon(queue, pixels, icon_w, icon_h);
        let (_, view, _, _) = self.icon_tex.as_ref().unwrap();

        let u = DndUniforms {
            rect_x: rect_xywh.0,
            rect_y: rect_xywh.1,
            rect_w: rect_xywh.2,
            rect_h: rect_xywh.3,
            surface_w: surface_w as f32,
            surface_h: surface_h as f32,
            _pad0: 0.0,
            _pad1: 0.0,
        };
        queue.write_buffer(&self.uniforms, 0, bytemuck::bytes_of(&u));

        let bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("dnd-icon-bg"),
            layout: &self.bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: &self.uniforms,
                        offset: 0,
                        size: std::num::NonZeroU64::new(std::mem::size_of::<DndUniforms>() as u64),
                    }),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });

        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("dnd-icon-pass"),
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
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &bg, &[]);
        pass.draw(0..6, 0..1);
    }
}
