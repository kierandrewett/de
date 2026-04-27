//! chrome_shader.rs — wgpu post-pass for macOS-quality squircle window chrome.
//!
//! # Architecture contract
//!
//! The `FemtoVGWGPURenderer` renders the full Slint scene (including WindowChrome
//! components with circular-arc `border-radius` placeholders) into an offscreen
//! `wgpu::Texture` called `render_texture`.  Before that texture is blitted to the
//! swapchain we run multiple wgpu passes per window:
//!
//! **Per shadow layer (active: 3, inactive: 2):**
//!   Pass 1 — `fs_shadow_mask`: Renders the squircle silhouette into an offscreen
//!             "shadow mask" texture (alpha = 1 inside, 0 outside).
//!   Pass 2 — `fs_blur_h`: True horizontal Gaussian blur (32 taps) on the mask.
//!   Pass 3 — `fs_blur_v`: True vertical Gaussian blur (32 taps) on the H-blurred result.
//!   Pass 4 — `fs_shadow_composite`: Blends the final blurred mask into the swapchain
//!             at (shadow_ox, shadow_oy) with `shadow_a` opacity.
//!
//! **Per window:**
//!   Pass 5 — `fs_chrome`: Reads `render_texture` (Slint scene), applies the squircle
//!             alpha mask (border-overlaid contract), draws outer 0.5 px stroke, inner
//!             1 px highlight gradient — ALL overlaid on top of the client texture.
//!
//! # Border-overlaid contract
//!
//! The client texture extends to the FULL window rect.  Chrome composites the squircle
//! alpha mask + outer border + inner highlight ON TOP of it.  The squircle becomes a
//! soft alpha mask multiplied into the client's alpha channel; the border/highlight
//! strokes overlay on top.  Result: corners look smooth (no clip artifact), borders
//! sit on top of client content (no halo).
//!
//! # True separable Gaussian shadow
//!
//! Each shadow layer uses a real two-pass separable Gaussian (H × V) with 32 taps.
//! For sigma=48 px (the widest layer), 32 taps covers ~0.67 sigma — the truncation
//! is slightly visible at very large values, but the visual improvement over the old
//! single-axis `exp(-t²/2)` approximation is substantial.
//!
//! Shadow mask textures are cached per (win_w, win_h, sigma) to avoid re-blurring
//! on every frame.  Cache is invalidated on window resize.
//!
//! # Uniform buffer strategy
//!
//! All per-draw-call uniforms are packed into a single buffer using 256-byte-aligned
//! dynamic offsets (wgpu minimum dynamic-offset alignment).  Each draw call binds
//! the same bind group with a different `dynamic_offset`.

use std::collections::HashMap;
use wgpu;

// ──────────────────────────────────────────────────────────────────────────────
// Per-window parameters passed to the shader
// ──────────────────────────────────────────────────────────────────────────────

/// Geometry and style for one window's chrome pass.
#[derive(Clone, Debug)]
pub struct WindowChromeParams {
    /// Window position in physical pixels (top-left).
    pub x: f32,
    pub y: f32,
    /// Window size in physical pixels.
    pub w: f32,
    pub h: f32,
    /// True = active/focused window.
    pub active: bool,
}

// ──────────────────────────────────────────────────────────────────────────────
// GPU uniform layout (must match `ChromeUniforms` struct in chrome.wgsl exactly)
// ──────────────────────────────────────────────────────────────────────────────

/// 80-byte uniform block — repr(C), padded to 80 bytes (multiple of 16).
/// wgpu dynamic-offset alignment is 256 bytes, so we allocate 256 bytes per
/// draw call in the dynamic buffer but only the first 80 bytes carry data.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ChromeUniforms {
    win_x: f32,
    win_y: f32,
    win_w: f32,
    win_h: f32,      // 16

    surface_w: f32,
    surface_h: f32,
    radius_px: f32,
    smoothing: f32,  // 32

    stroke_r: f32,
    stroke_g: f32,
    stroke_b: f32,
    stroke_a: f32,   // 48

    highlight_a: f32,
    is_active: f32,
    shadow_a: f32,
    shadow_oy: f32,  // 64

    shadow_ox: f32,
    blur_sigma: f32,
    _pad0: f32,
    _pad1: f32,      // 80
}

const UNIFORM_STRUCT_SIZE: usize = std::mem::size_of::<ChromeUniforms>();
// wgpu requires dynamic uniform offsets to be aligned to 256 bytes.
const UNIFORM_STRIDE: u64 = 256;
// Max draw calls per frame: MAX_WINDOWS * (3 layers * 4 passes + 1 chrome) = 32 * 13 = 416
const MAX_DRAWS: u64 = 512;
const DYNAMIC_UNIFORM_BUF_SIZE: u64 = UNIFORM_STRIDE * MAX_DRAWS;

// ──────────────────────────────────────────────────────────────────────────────
// Shadow layer spec (from WINDOW_SPEC.md)
// ──────────────────────────────────────────────────────────────────────────────

struct ShadowLayer {
    offset_y: f32,
    offset_x: f32,
    blur_sigma: f32,
    alpha: f32,
}

fn active_shadow_layers() -> Vec<ShadowLayer> {
    vec![
        ShadowLayer { offset_x: 0.0, offset_y:  1.0, blur_sigma:  3.0, alpha: 0.12 },
        ShadowLayer { offset_x: 0.0, offset_y:  8.0, blur_sigma: 24.0, alpha: 0.12 },
        ShadowLayer { offset_x: 0.0, offset_y: 20.0, blur_sigma: 48.0, alpha: 0.08 },
    ]
}

fn inactive_shadow_layers() -> Vec<ShadowLayer> {
    vec![
        ShadowLayer { offset_x: 0.0, offset_y: 1.0, blur_sigma:  2.0, alpha: 0.08 },
        ShadowLayer { offset_x: 0.0, offset_y: 4.0, blur_sigma: 12.0, alpha: 0.06 },
    ]
}

// ──────────────────────────────────────────────────────────────────────────────
// Cached shadow textures (blurred mask per window × sigma)
// ──────────────────────────────────────────────────────────────────────────────

/// Cache key for a blurred shadow mask texture.
#[derive(Hash, Eq, PartialEq, Clone, Debug)]
struct ShadowCacheKey {
    /// Window width in physical pixels (as u32 bits — f32 bitcast for hashing).
    win_w: u32,
    win_h: u32,
    /// blur_sigma as u32 bits (bitcast from f32).
    sigma_bits: u32,
}

impl ShadowCacheKey {
    fn new(win_w: f32, win_h: f32, sigma: f32) -> Self {
        Self {
            win_w: win_w.to_bits(),
            win_h: win_h.to_bits(),
            sigma_bits: sigma.to_bits(),
        }
    }
}

struct ShadowCacheEntry {
    /// The fully-blurred shadow mask texture (H+V blur applied).
    blurred_texture: wgpu::Texture,
    /// Pre-built view for sampling in composite pass.
    blurred_view: wgpu::TextureView,
}

// ──────────────────────────────────────────────────────────────────────────────
// Bind group cache for the scene texture (chrome pass)
// ──────────────────────────────────────────────────────────────────────────────

struct SceneBindGroup {
    bind_group: wgpu::BindGroup,
}

// ──────────────────────────────────────────────────────────────────────────────
// ChromeShader — owns all wgpu state
// ──────────────────────────────────────────────────────────────────────────────

pub struct ChromeShader {
    // Pipeline for rendering the hard squircle silhouette into the shadow mask.
    shadow_mask_pipeline: wgpu::RenderPipeline,
    // Pipeline for horizontal Gaussian blur.
    blur_h_pipeline: wgpu::RenderPipeline,
    // Pipeline for vertical Gaussian blur.
    blur_v_pipeline: wgpu::RenderPipeline,
    // Pipeline for compositing the blurred shadow into the output.
    shadow_composite_pipeline: wgpu::RenderPipeline,
    // Pipeline for the chrome overlay (border-overlaid contract).
    chrome_pipeline: wgpu::RenderPipeline,
    // Bind group layout shared by all pipelines:
    //   binding 0: dynamic uniform buffer
    //   binding 1: source texture (scene or shadow mask)
    //   binding 2: sampler
    bgl: wgpu::BindGroupLayout,
    // Sampler for all texture reads.
    sampler: wgpu::Sampler,
    // Dynamic uniform buffer (256-byte slots, one per draw call).
    dynamic_uniform_buf: wgpu::Buffer,
    // Cache: (scene_texture_ptr) → SceneBindGroup (for chrome pass).
    scene_bind_group_cache: HashMap<u64, SceneBindGroup>,
    // Cache: ShadowCacheKey → blurred shadow mask texture.
    shadow_cache: HashMap<ShadowCacheKey, ShadowCacheEntry>,
    // Swapchain texture format (for pipeline creation).
    format: wgpu::TextureFormat,
}

impl ChromeShader {
    /// Build all render pipelines.  `format` must match the render/swapchain texture.
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let shader_src = include_str!("../shaders/chrome.wgsl");
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("chrome.wgsl"),
            source: wgpu::ShaderSource::Wgsl(shader_src.into()),
        });

        // ── Bind group layout ───────────────────────────────────────────────
        let uniform_size = std::num::NonZeroU64::new(UNIFORM_STRUCT_SIZE as u64).unwrap();
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
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
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("chrome-pl"),
            bind_group_layouts: &[&bgl],
            immediate_size: 0,
        });

        // Standard src-over alpha blending.
        let blend_src_over = wgpu::BlendState {
            color: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::SrcAlpha,
                dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                operation: wgpu::BlendOperation::Add,
            },
            alpha: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::One,
                dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                operation: wgpu::BlendOperation::Add,
            },
        };

        // Helper: create a pipeline for a given fragment entry point.
        let make_pipeline = |label: &str, fs_entry: &str, blend: Option<wgpu::BlendState>| {
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
                    entry_point: Some(fs_entry),
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
        };

        // Shadow mask: write alpha (no blending needed — always overwrites).
        let shadow_mask_pipeline = make_pipeline(
            "chrome-shadow-mask",
            "fs_shadow_mask",
            None, // replace (not blend) into the offscreen mask texture
        );

        // Blur passes: same "replace" semantics (intermediate textures).
        let blur_h_pipeline = make_pipeline("chrome-blur-h", "fs_blur_h", None);
        let blur_v_pipeline = make_pipeline("chrome-blur-v", "fs_blur_v", None);

        // Shadow composite: src-over blend into the output.
        let shadow_composite_pipeline = make_pipeline(
            "chrome-shadow-composite",
            "fs_shadow_composite",
            Some(blend_src_over.clone()),
        );

        // Chrome: src-over blend into the output.
        let chrome_pipeline = make_pipeline("chrome-chrome", "fs_chrome", Some(blend_src_over));

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
            size: DYNAMIC_UNIFORM_BUF_SIZE,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Self {
            shadow_mask_pipeline,
            blur_h_pipeline,
            blur_v_pipeline,
            shadow_composite_pipeline,
            chrome_pipeline,
            bgl,
            sampler,
            dynamic_uniform_buf,
            scene_bind_group_cache: HashMap::new(),
            shadow_cache: HashMap::new(),
            format,
        }
    }

    /// Invalidate caches (call when the scene texture or output size changes).
    pub fn invalidate_cache(&mut self) {
        self.scene_bind_group_cache.clear();
        self.shadow_cache.clear();
    }

    /// Create (or retrieve cached) bind group for a given texture view.
    /// The bind group uses the dynamic uniform buffer with dynamic offsets.
    fn make_bind_group(
        &self,
        device: &wgpu::Device,
        texture_view: &wgpu::TextureView,
    ) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("chrome-bg"),
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
                    resource: wgpu::BindingResource::TextureView(texture_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        })
    }

    /// Obtain or create a cached bind group for the scene texture.
    fn ensure_scene_bind_group(
        &mut self,
        device: &wgpu::Device,
        scene_view: &wgpu::TextureView,
        cache_key: u64,
    ) {
        if self.scene_bind_group_cache.contains_key(&cache_key) {
            return;
        }
        let bg = self.make_bind_group(device, scene_view);
        self.scene_bind_group_cache.insert(cache_key, SceneBindGroup { bind_group: bg });
    }

    /// Allocate an offscreen R8Unorm (single channel) texture for shadow blur passes.
    /// Uses `format` from construction so the pipeline formats match.
    fn make_shadow_texture(&self, device: &wgpu::Device, width: u32, height: u32) -> wgpu::Texture {
        device.create_texture(&wgpu::TextureDescriptor {
            label: Some("shadow-mask"),
            size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: self.format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        })
    }

    /// Run the three-pass Gaussian shadow blur for one layer, returning the blurred texture.
    /// Uses the shadow_cache; only renders if not already cached.
    fn get_blurred_shadow(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        win_w: f32,
        win_h: f32,
        sigma: f32,
        surface_w: f32,
        surface_h: f32,
    ) -> Option<&ShadowCacheEntry> {
        let key = ShadowCacheKey::new(win_w, win_h, sigma);

        if self.shadow_cache.contains_key(&key) {
            return self.shadow_cache.get(&key);
        }

        // The shadow mask texture covers the window + blur_sigma*3 padding on all sides.
        // We render at full-surface UV space so the blur shader can use surface dimensions.
        // (Texture dimensions = full surface width/height for simplicity.)
        let tex_w = surface_w as u32;
        let tex_h = surface_h as u32;

        // Allocate three textures: mask, h-blur, v-blur (final).
        let mask_tex  = self.make_shadow_texture(device, tex_w, tex_h);
        let hblur_tex = self.make_shadow_texture(device, tex_w, tex_h);
        let vblur_tex = self.make_shadow_texture(device, tex_w, tex_h);

        let mask_view  = mask_tex.create_view(&wgpu::TextureViewDescriptor::default());
        let hblur_view = hblur_tex.create_view(&wgpu::TextureViewDescriptor::default());
        let vblur_view = vblur_tex.create_view(&wgpu::TextureViewDescriptor::default());

        // Build uniforms for the shadow passes.
        let shadow_uniforms = ChromeUniforms {
            win_x: 0.0, // will be set during composite pass; mask pass uses relative coords
            win_y: 0.0,
            win_w,
            win_h,
            surface_w,
            surface_h,
            radius_px: 14.0,
            smoothing: 0.6,
            stroke_r: 0.0, stroke_g: 0.0, stroke_b: 0.0, stroke_a: 0.0,
            highlight_a: 0.0,
            is_active: 0.0,
            shadow_a: 0.0,
            shadow_oy: 0.0,
            shadow_ox: 0.0,
            blur_sigma: sigma,
            _pad0: 0.0, _pad1: 0.0,
        };

        let mut packed = vec![0u8; UNIFORM_STRIDE as usize];
        let bytes = bytemuck::bytes_of(&shadow_uniforms);
        packed[..UNIFORM_STRUCT_SIZE].copy_from_slice(bytes);
        queue.write_buffer(&self.dynamic_uniform_buf, 0, &packed);

        let dynamic_offset = 0u32;

        let mask_bg   = self.make_bind_group(device, &mask_view);
        let hblur_bg  = self.make_bind_group(device, &hblur_view);

        let mut encoder = device.create_command_encoder(
            &wgpu::CommandEncoderDescriptor { label: Some("shadow-blur") }
        );

        // Pass 1: render squircle silhouette mask.
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("shadow-mask-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &mask_view,
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
            pass.set_pipeline(&self.shadow_mask_pipeline);
            // Use a dummy bind group for the scene texture binding (not sampled in mask pass).
            pass.set_bind_group(0, &mask_bg, &[dynamic_offset]);
            pass.draw(0..6, 0..1);
        }

        // Pass 2: horizontal blur (reads mask_tex, writes hblur_tex).
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("shadow-blur-h-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &hblur_view,
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
            pass.set_pipeline(&self.blur_h_pipeline);
            pass.set_bind_group(0, &mask_bg, &[dynamic_offset]);
            pass.draw(0..6, 0..1);
        }

        // Pass 3: vertical blur (reads hblur_tex, writes vblur_tex).
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("shadow-blur-v-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &vblur_view,
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
            pass.set_pipeline(&self.blur_v_pipeline);
            pass.set_bind_group(0, &hblur_bg, &[dynamic_offset]);
            pass.draw(0..6, 0..1);
        }

        queue.submit(std::iter::once(encoder.finish()));

        self.shadow_cache.insert(key.clone(), ShadowCacheEntry {
            blurred_texture: vblur_tex,
            blurred_view: vblur_view,
        });

        self.shadow_cache.get(&key)
    }

    /// Run the full chrome pass for all windows.
    ///
    /// `render_texture` is the Slint offscreen texture (MUST have TEXTURE_BINDING).
    /// `output_view` is the swapchain frame view to draw into.
    /// `windows` is the list of windows to process.
    pub fn render(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        render_texture: &wgpu::Texture,
        output_view: &wgpu::TextureView,
        surface_w: u32,
        surface_h: u32,
        windows: &[WindowChromeParams],
    ) {
        if windows.is_empty() {
            return;
        }

        let sw = surface_w as f32;
        let sh = surface_h as f32;

        // ── Phase 1: Ensure blurred shadow cache is populated ─────────────────
        // We pre-compute blurred masks for all unique (win_w, win_h, sigma) tuples
        // this frame.  If already cached they're cheap no-ops.
        for win in windows.iter() {
            let layers = if win.active { active_shadow_layers() } else { inactive_shadow_layers() };
            for layer in &layers {
                self.get_blurred_shadow(device, queue, win.w, win.h, layer.blur_sigma, sw, sh);
            }
        }

        // ── Phase 2: Shadow composite passes ─────────────────────────────────
        // For each window × layer (back to front), composite the blurred shadow
        // into `output_view` at (win.x + shadow_ox, win.y + shadow_oy).
        // We write all uniforms into the dynamic buffer before starting any passes.

        // Collect all composite-pass uniforms.
        let mut draw_calls: Vec<(/* is_chrome */ bool, /* uniform_slot */ u32, /* bg_ptr */ Option<u64>)> = Vec::new();
        let mut raw_uniforms: Vec<ChromeUniforms> = Vec::new();

        // Shadow composite draws (one per window per layer, back-to-front).
        for win in windows.iter().rev() {
            let layers = if win.active { active_shadow_layers() } else { inactive_shadow_layers() };
            for layer in layers.iter().rev() {
                let idx = raw_uniforms.len() as u32;
                raw_uniforms.push(ChromeUniforms {
                    win_x: win.x,
                    win_y: win.y,
                    win_w: win.w,
                    win_h: win.h,
                    surface_w: sw,
                    surface_h: sh,
                    radius_px: 14.0,
                    smoothing: 0.6,
                    stroke_r: 0.0, stroke_g: 0.0, stroke_b: 0.0, stroke_a: 0.0,
                    highlight_a: 0.0,
                    is_active: if win.active { 1.0 } else { 0.0 },
                    shadow_a: layer.alpha,
                    shadow_oy: layer.offset_y,
                    shadow_ox: layer.offset_x,
                    blur_sigma: layer.blur_sigma,
                    _pad0: 0.0, _pad1: 0.0,
                });
                // Use the cached blurred texture pointer as a unique key for the draw.
                let cache_key = ShadowCacheKey::new(win.w, win.h, layer.blur_sigma);
                let tex_ptr = self.shadow_cache.get(&cache_key)
                    .map(|e| &e.blurred_texture as *const _ as u64);
                draw_calls.push((false, idx, tex_ptr));
            }
        }

        // Chrome draws (one per window, front-to-back order).
        let scene_view = render_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let scene_cache_key = render_texture as *const _ as u64;

        for win in windows.iter() {
            let (stroke_a, highlight_a) = if win.active {
                (0.72_f32, 0.08_f32)
            } else {
                (0.55_f32, 0.04_f32)
            };
            let idx = raw_uniforms.len() as u32;
            raw_uniforms.push(ChromeUniforms {
                win_x: win.x,
                win_y: win.y,
                win_w: win.w,
                win_h: win.h,
                surface_w: sw,
                surface_h: sh,
                radius_px: 14.0,
                smoothing: 0.6,
                stroke_r: 0.0, stroke_g: 0.0, stroke_b: 0.0,
                stroke_a,
                highlight_a,
                is_active: if win.active { 1.0 } else { 0.0 },
                shadow_a: 0.0,
                shadow_oy: 0.0,
                shadow_ox: 0.0,
                blur_sigma: 0.0,
                _pad0: 0.0, _pad1: 0.0,
            });
            draw_calls.push((true, idx, Some(scene_cache_key)));
        }

        // ── Upload all uniforms ───────────────────────────────────────────────
        let total_slots = raw_uniforms.len() as u64;
        if total_slots == 0 || total_slots > MAX_DRAWS {
            return;
        }

        let mut packed = vec![0u8; (UNIFORM_STRIDE * total_slots) as usize];
        for (i, u) in raw_uniforms.iter().enumerate() {
            let offset = (i as u64 * UNIFORM_STRIDE) as usize;
            let bytes = bytemuck::bytes_of(u);
            packed[offset..offset + UNIFORM_STRUCT_SIZE].copy_from_slice(bytes);
        }
        queue.write_buffer(&self.dynamic_uniform_buf, 0, &packed);

        // ── Ensure scene bind group ───────────────────────────────────────────
        self.ensure_scene_bind_group(device, &scene_view, scene_cache_key);

        // ── Emit draw calls ───────────────────────────────────────────────────
        for (is_chrome, slot_idx, bg_key) in &draw_calls {
            let dynamic_offset = (*slot_idx as u64 * UNIFORM_STRIDE) as u32;

            let (pipeline, bg) = if *is_chrome {
                let bg_ptr = bg_key.unwrap_or(scene_cache_key);
                let bg = &self.scene_bind_group_cache[&bg_ptr].bind_group;
                (&self.chrome_pipeline, bg)
            } else {
                // Shadow composite pass: use the blurred shadow texture.
                // We need a bind group pointing at the blurred texture.
                // Look up via the bg_key (blurred texture pointer).
                let tex_ptr = bg_key.unwrap();
                // Build a temporary bind group for the blurred texture.
                // We can't cache this generically without more bookkeeping, so we
                // rebuild it each frame (cheap — no GPU allocation, just a CPU table lookup).
                // (The wgpu bind group is dropped after the pass via implicit scope.)
                let cache_key_for_lookup = self.shadow_cache.iter()
                    .find(|(_, e)| &e.blurred_texture as *const _ as u64 == tex_ptr)
                    .map(|(k, _)| k.clone());

                if let Some(cache_key) = cache_key_for_lookup {
                    if let Some(entry) = self.shadow_cache.get(&cache_key) {
                        let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
                            label: Some("shadow-composite-bg"),
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
                                    resource: wgpu::BindingResource::TextureView(&entry.blurred_view),
                                },
                                wgpu::BindGroupEntry {
                                    binding: 2,
                                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                                },
                            ],
                        });

                        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                            label: Some("shadow-composite-pass"),
                            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                                view: output_view,
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
                        pass.set_pipeline(&self.shadow_composite_pipeline);
                        pass.set_bind_group(0, &bg, &[dynamic_offset]);
                        pass.draw(0..6, 0..1);
                    }
                }
                continue; // shadow draw already handled inline above
            };

            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("chrome-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: output_view,
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
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, bg, &[dynamic_offset]);
            pass.draw(0..6, 0..1);
        }
    }
}
