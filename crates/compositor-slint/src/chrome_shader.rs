//! chrome_shader.rs — wgpu post-pass for macOS-quality squircle window chrome.
//!
//! # Architecture contract
//!
//! The `FemtoVGWGPURenderer` renders the full Slint scene (including WindowChrome
//! components with *circular-arc* `border-radius` placeholders) into an offscreen
//! `wgpu::Texture` called `render_texture`.  Before that texture is blitted to the
//! swapchain we run two extra wgpu passes per window:
//!
//!   **Pass A — Shadow** (`fs_shadow` entry point in `chrome.wgsl`)
//!     Three Gaussian-blurred squircle silhouettes drawn BEHIND each window,
//!     back-to-front, with the spec's offset/blur/colour (active: 3 layers;
//!     inactive: 2 layers).  Results are composited (src-over) into an
//!     intermediate `shadow_texture`.
//!
//!   **Pass B — Chrome** (`fs_chrome` entry point in `chrome.wgsl`)
//!     Reads `render_texture` (Slint scene), clips each window to the squircle
//!     SDF, draws the 0.5 px outer stroke (alpha lerped via mode_t/focus_t),
//!     and draws the 1 px inner highlight gradient (same lerp).
//!     Output replaces the circular-arc corners that Slint drew.
//!
//! # Theme crossfade uniforms
//!
//! `mode_t`  (0=dark, 1=light)  — set by `set_mode_t`, animated by `src/theme.rs`
//! `focus_t` (0=inactive, 1=active) — per-window, animated by `src/theme.rs`
//!
//! The shader lerps between the four state palettes from WINDOW_SPEC:
//!   dark_active:    outer 0.72 / highlight_top 0.08
//!   dark_inactive:  outer 0.55 / highlight_top 0.04
//!   light_active:   outer 0.22 / highlight_top 0.50
//!   light_inactive: outer 0.15 / highlight_top 0.25
//!
//! # Render-texture usage flag contract
//!
//! `render_texture` must be created with:
//!   `RENDER_ATTACHMENT | COPY_SRC | TEXTURE_BINDING`
//! The extra `TEXTURE_BINDING` flag is added in `renderer.rs` so the chrome
//! shader can sample the Slint scene.
//!
//! # Uniform buffer strategy
//!
//! All per-draw-call uniforms are packed into a single buffer using 256-byte-aligned
//! dynamic offsets (wgpu minimum dynamic-offset alignment).  Each draw call binds
//! the same bind group with a different `dynamic_offset`.  This avoids data hazards
//! from re-writing a single buffer slot before submission.

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
    /// Per-window focus crossfade: 0.0 = inactive, 1.0 = active (animated).
    pub focus_t: f32,
    /// Global mode crossfade: 0.0 = dark, 1.0 = light (animated).
    pub mode_t: f32,
}

// ──────────────────────────────────────────────────────────────────────────────
// GPU uniform layout (must match `ChromeUniforms` struct in chrome.wgsl exactly)
// ──────────────────────────────────────────────────────────────────────────────

/// 64-byte uniform block — repr(C), padded to 64 bytes (multiple of 16).
/// wgpu dynamic-offset alignment is 256 bytes, so we allocate 256 bytes per
/// draw call in the dynamic buffer but only the first 64 bytes carry data.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ChromeUniforms {
    win_x:      f32,
    win_y:      f32,
    win_w:      f32,
    win_h:      f32,      // 16

    surface_w:  f32,
    surface_h:  f32,
    radius_px:  f32,
    smoothing:  f32,      // 32

    mode_t:     f32,
    focus_t:    f32,
    shadow_a:   f32,
    shadow_oy:  f32,      // 48

    shadow_ox:  f32,
    blur_sigma: f32,
    _pad0:      f32,
    _pad1:      f32,      // 64
}

const UNIFORM_STRUCT_SIZE: usize = std::mem::size_of::<ChromeUniforms>();
// wgpu requires dynamic uniform offsets to be aligned to 256 bytes.
const UNIFORM_STRIDE: u64 = 256;
// Max draw calls per frame: MAX_WINDOWS * (3 shadow + 1 chrome) = 32 * 4 = 128
const MAX_DRAWS: u64 = 128;
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
// Cached bind group (keyed by scene texture pointer)
// ──────────────────────────────────────────────────────────────────────────────

struct SceneBindGroup {
    /// The bind group that uses the dynamic uniform buffer and the scene texture.
    bind_group: wgpu::BindGroup,
}

// ──────────────────────────────────────────────────────────────────────────────
// ChromeShader — owns all wgpu state for the two chrome passes
// ──────────────────────────────────────────────────────────────────────────────

pub struct ChromeShader {
    // Shadow pipeline (fs_shadow entry point).
    shadow_pipeline: wgpu::RenderPipeline,
    // Chrome pipeline (fs_chrome entry point).
    chrome_pipeline: wgpu::RenderPipeline,
    // Bind group layout (shared between both pipelines).
    bgl: wgpu::BindGroupLayout,
    // Sampler for the scene texture.
    sampler: wgpu::Sampler,
    // Dynamic uniform buffer (256-byte slots, one per draw call).
    // Uploaded in one write_buffer call before the encoder is built.
    dynamic_uniform_buf: wgpu::Buffer,
    // Cache: (scene_texture_ptr) → SceneBindGroup.
    bind_group_cache: HashMap<u64, SceneBindGroup>,
}

impl ChromeShader {
    /// Build both render pipelines.  `format` must match the render/swapchain texture.
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
                // binding 0: uniforms (dynamic offset)
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
                // binding 1: scene texture
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
                // binding 2: sampler
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

        // Alpha-blending state: standard src-over.
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

        let color_target = wgpu::ColorTargetState {
            format,
            blend: Some(blend_src_over),
            write_mask: wgpu::ColorWrites::ALL,
        };

        // ── Shadow pipeline ─────────────────────────────────────────────────
        let shadow_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("chrome-shadow-pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs_shadow"),
                targets: &[Some(color_target.clone())],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        // ── Chrome pipeline ─────────────────────────────────────────────────
        let chrome_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("chrome-chrome-pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs_chrome"),
                targets: &[Some(color_target)],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("chrome-sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        let dynamic_uniform_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("chrome-dynamic-uniforms"),
            size: DYNAMIC_UNIFORM_BUF_SIZE,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Self {
            shadow_pipeline,
            chrome_pipeline,
            bgl,
            sampler,
            dynamic_uniform_buf,
            bind_group_cache: HashMap::new(),
        }
    }

    /// Invalidate the cached bind group (call when the scene texture changes size).
    pub fn invalidate_cache(&mut self) {
        self.bind_group_cache.clear();
    }

    /// Obtain or create a bind group for the given scene texture view.
    /// The bind group uses the dynamic uniform buffer (with dynamic offsets).
    fn ensure_bind_group(
        &mut self,
        device: &wgpu::Device,
        scene_view: &wgpu::TextureView,
        cache_key: u64,
    ) {
        if self.bind_group_cache.contains_key(&cache_key) {
            return;
        }
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("chrome-bg"),
            layout: &self.bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    // Bind the whole dynamic uniform buffer; actual data is
                    // selected via dynamic offset at draw time.
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: &self.dynamic_uniform_buf,
                        offset: 0,
                        // size = None means "rest of buffer", which wgpu validates
                        // against min_binding_size (UNIFORM_STRUCT_SIZE).
                        size: std::num::NonZeroU64::new(UNIFORM_STRUCT_SIZE as u64),
                    }),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(scene_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });
        self.bind_group_cache.insert(cache_key, SceneBindGroup { bind_group });
    }

    /// Run the full chrome pass for all windows.
    ///
    /// `render_texture` is the Slint offscreen texture (MUST have TEXTURE_BINDING).
    /// `output_view` is the swapchain frame view to draw into.
    /// `windows` is the list of windows to process, each carrying:
    ///   - `focus_t`: animated 0→1 when window is focused, 1→0 when unfocused.
    ///   - `mode_t`:  animated 0→1 on dark→light switch, 1→0 on light→dark switch.
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

        let scene_view = render_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let cache_key: u64 = render_texture as *const _ as u64;
        let sw = surface_w as f32;
        let sh = surface_h as f32;

        // ── Pre-compute all uniforms ────────────────────────────────────────
        // We collect (pipeline_kind, uniform_slot_index) pairs to drive the
        // draw loop after uploading all uniforms.
        //
        // pipeline_kind: false = shadow, true = chrome.
        let mut draw_calls: Vec<(bool, u32)> = Vec::new();
        let mut raw_uniforms: Vec<ChromeUniforms> = Vec::new();

        // Shadows: back-to-front per window, widest blur first.
        // We use focus_t to decide how many shadow layers to draw (lerp between
        // 2-layer inactive and 3-layer active by always drawing 3 but scaling
        // the third layer's alpha by focus_t).
        for win in windows.iter().rev() {
            // Always draw 3 layers; layer 3 (wide ambient) has its alpha scaled
            // by focus_t so it fades in as the window gains focus.
            let active_layers = active_shadow_layers();
            let inactive_layers = inactive_shadow_layers();

            // Layer 1 & 2: lerp alpha between inactive and active values.
            for layer_idx in 0..2 {
                let a_layer = &active_layers[layer_idx];
                let i_layer = &inactive_layers[layer_idx];
                let alpha = i_layer.alpha + win.focus_t * (a_layer.alpha - i_layer.alpha);
                let blur  = i_layer.blur_sigma + win.focus_t * (a_layer.blur_sigma - i_layer.blur_sigma);
                let oy    = i_layer.offset_y + win.focus_t * (a_layer.offset_y - i_layer.offset_y);
                let idx = raw_uniforms.len() as u32;
                raw_uniforms.push(ChromeUniforms {
                    win_x: win.x, win_y: win.y, win_w: win.w, win_h: win.h,
                    surface_w: sw, surface_h: sh,
                    radius_px: 14.0, smoothing: 0.6,
                    mode_t: win.mode_t, focus_t: win.focus_t,
                    shadow_a: alpha, shadow_oy: oy,
                    shadow_ox: a_layer.offset_x,
                    blur_sigma: blur,
                    _pad0: 0.0, _pad1: 0.0,
                });
                draw_calls.push((false, idx));
            }

            // Layer 3 (wide ambient): only in active state — scaled by focus_t.
            {
                let a_layer = &active_layers[2];
                let alpha = a_layer.alpha * win.focus_t;
                if alpha > 0.001 {
                    let idx = raw_uniforms.len() as u32;
                    raw_uniforms.push(ChromeUniforms {
                        win_x: win.x, win_y: win.y, win_w: win.w, win_h: win.h,
                        surface_w: sw, surface_h: sh,
                        radius_px: 14.0, smoothing: 0.6,
                        mode_t: win.mode_t, focus_t: win.focus_t,
                        shadow_a: alpha, shadow_oy: a_layer.offset_y,
                        shadow_ox: a_layer.offset_x,
                        blur_sigma: a_layer.blur_sigma,
                        _pad0: 0.0, _pad1: 0.0,
                    });
                    draw_calls.push((false, idx));
                }
            }
        }

        // Chrome: front-to-back order (on top of shadows).
        // The shader computes stroke_a and highlight_a from mode_t/focus_t itself.
        for win in windows.iter() {
            let idx = raw_uniforms.len() as u32;
            raw_uniforms.push(ChromeUniforms {
                win_x: win.x, win_y: win.y, win_w: win.w, win_h: win.h,
                surface_w: sw, surface_h: sh,
                radius_px: 14.0, smoothing: 0.6,
                mode_t: win.mode_t, focus_t: win.focus_t,
                shadow_a: 0.0, shadow_oy: 0.0,
                shadow_ox: 0.0, blur_sigma: 0.0,
                _pad0: 0.0, _pad1: 0.0,
            });
            draw_calls.push((true, idx));
        }

        // ── Upload all uniforms at once ────────────────────────────────────
        // Each slot is UNIFORM_STRIDE (256) bytes; only the first UNIFORM_STRUCT_SIZE
        // bytes carry data — the rest are zeroed padding.
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

        // ── Ensure bind group ───────────────────────────────────────────────
        self.ensure_bind_group(device, &scene_view, cache_key);
        let bg = &self.bind_group_cache[&cache_key].bind_group;

        // ── Emit draw calls ─────────────────────────────────────────────────
        for (is_chrome, slot_idx) in &draw_calls {
            let dynamic_offset = (*slot_idx as u64 * UNIFORM_STRIDE) as u32;

            let pipeline = if *is_chrome { &self.chrome_pipeline } else { &self.shadow_pipeline };
            let label = if *is_chrome { "chrome-chrome-pass" } else { "chrome-shadow-pass" };

            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some(label),
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
