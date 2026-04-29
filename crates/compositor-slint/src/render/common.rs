//! Shared chrome-rendering primitives used by every per-effect pass.
//!
//! Layout contract for all chrome passes:
//!
//!   group(0) binding(0) — dynamic uniform: ChromeUniforms (256-byte stride)
//!   group(0) binding(1) — sampled texture (scene OR an intermediate mask)
//!   group(0) binding(2) — sampler
//!
//! Each pass's WGSL is the `_common.wgsl` chunk + its own fragment entry.
//! Sources are concatenated at runtime; WGSL has no `#include`.

use std::sync::Arc;

use wgpu;

/// Shared chunk prepended to every per-effect WGSL source.
pub const COMMON_WGSL: &str = include_str!("../../shaders/_common.wgsl");

/// Compose a complete WGSL module from `_common.wgsl` + a per-effect fragment.
pub fn compose_wgsl(frag_src: &str) -> String {
    format!("{}\n{}", COMMON_WGSL, frag_src)
}

/// 64-byte uniform block — must match `ChromeUniforms` in `_common.wgsl`.
#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct ChromeUniforms {
    pub win_x:      f32,
    pub win_y:      f32,
    pub win_w:      f32,
    pub win_h:      f32,    // 16

    pub surface_w:  f32,
    pub surface_h:  f32,
    pub radius_px:  f32,
    pub smoothing:  f32,    // 32

    pub mode_t:     f32,
    pub focus_t:    f32,
    pub shadow_a:   f32,
    pub shadow_oy:  f32,    // 48

    pub shadow_ox:  f32,
    pub blur_sigma: f32,
    pub _pad0:      f32,
    pub _pad1:      f32,    // 64
}

impl Default for ChromeUniforms {
    fn default() -> Self {
        Self {
            win_x: 0.0, win_y: 0.0, win_w: 0.0, win_h: 0.0,
            surface_w: 0.0, surface_h: 0.0, radius_px: 14.0, smoothing: 0.6,
            mode_t: 0.0, focus_t: 1.0,
            shadow_a: 0.0, shadow_oy: 0.0, shadow_ox: 0.0, blur_sigma: 0.0,
            _pad0: 0.0, _pad1: 0.0,
        }
    }
}

pub const UNIFORM_STRUCT_SIZE: usize = std::mem::size_of::<ChromeUniforms>();
/// wgpu requires dynamic uniform offsets to be aligned to 256 bytes.
pub const UNIFORM_STRIDE: u64 = 256;

/// Maximum draw calls per frame: 32 windows × ~12 passes/window ≈ 384.
pub const MAX_DRAWS: u64 = 512;
pub const DYNAMIC_UNIFORM_BUF_SIZE: u64 = UNIFORM_STRIDE * MAX_DRAWS;

/// Standard premultiplied src-over blend.
pub fn blend_src_over_premul() -> wgpu::BlendState {
    wgpu::BlendState {
        color: wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::One,
            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
            operation: wgpu::BlendOperation::Add,
        },
        alpha: wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::One,
            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
            operation: wgpu::BlendOperation::Add,
        },
    }
}

/// Build the bind-group layout shared by every chrome pass.
pub fn make_bind_group_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    let uniform_size = std::num::NonZeroU64::new(UNIFORM_STRUCT_SIZE as u64).unwrap();
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("chrome-bgl"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: true,
                    min_binding_size: Some(uniform_size),
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
    })
}

/// Build a render pipeline using `_common.wgsl` + the supplied per-effect
/// fragment source. `entry` is the fragment entry name, `format` the target
/// texture format, `blend` the blend state for the colour attachment.
pub fn make_pipeline(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    label: &str,
    fragment_src: &str,
    fragment_entry: &str,
    format: wgpu::TextureFormat,
    blend: Option<wgpu::BlendState>,
) -> wgpu::RenderPipeline {
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(label),
        source: wgpu::ShaderSource::Wgsl(compose_wgsl(fragment_src).into()),
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some(label),
        bind_group_layouts: &[layout],
        immediate_size: 0,
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(label),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &module,
            entry_point: Some("vs_main"),
            buffers: &[],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &module,
            entry_point: Some(fragment_entry),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend,
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        }),
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    })
}

/// Common state shared by every chrome pass — single dynamic uniform buffer,
/// a linear sampler, the bind-group layout. Instances are owned by
/// `ChromeRenderer` and lent out via `Arc` so each pass can build bind groups.
pub struct Shared {
    pub device:               Arc<wgpu::Device>,
    pub format:               wgpu::TextureFormat,
    pub bgl:                  wgpu::BindGroupLayout,
    pub sampler:              wgpu::Sampler,
    pub dynamic_uniform_buf:  wgpu::Buffer,
    /// 1×1 dummy texture for passes that don't actually sample t_src. The
    /// bind-group layout still requires a texture binding, and binding the
    /// scene texture here would conflict with using it as a colour target
    /// in the same encoder scope (wgpu validation error).
    pub dummy_view:           wgpu::TextureView,
}

impl Shared {
    pub fn new(device: Arc<wgpu::Device>, format: wgpu::TextureFormat) -> Self {
        let bgl     = make_bind_group_layout(&device);
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("chrome-sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        });
        let dynamic_uniform_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("chrome-dynamic-uniforms"),
            size:  DYNAMIC_UNIFORM_BUF_SIZE,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let dummy_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("chrome-dummy-1x1"),
            size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count:    1,
            dimension:       wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let dummy_view = dummy_tex.create_view(&wgpu::TextureViewDescriptor::default());
        Self { device, format, bgl, sampler, dynamic_uniform_buf, dummy_view }
    }

    /// Build a bind group bound to (dynamic-uniform-buffer, the given view, sampler).
    pub fn make_bind_group(&self, label: &str, view: &wgpu::TextureView) -> wgpu::BindGroup {
        self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some(label),
            layout: &self.bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: &self.dynamic_uniform_buf,
                        offset: 0,
                        size: std::num::NonZeroU64::new(UNIFORM_STRUCT_SIZE as u64),
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
        })
    }
}
