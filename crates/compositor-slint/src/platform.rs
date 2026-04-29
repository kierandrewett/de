//! Custom Slint platform: GPU-backed window adapter using FemtoVGWGPURenderer.
//!
//! GPU MIGRATION D1/D2:
//!   - GpuWindowAdapter wraps FemtoVGWGPURenderer (wgpu-28, unstable-wgpu-28 Slint feature)
//!   - CalloopPlatform stores wgpu resources to create the adapter on demand
//!   - render_to_texture() is called by the render loop each frame
//!
//! The calloop event loop still drives everything — Slint's run_event_loop() returns an error
//! intentionally so calloop stays in control (same as the software spike).
//!
//! PANIC FIX: GpuWindowAdapter now exposes the wgpu device/queue/adapter/instance
//! used by FemtoVG.  renderer.rs uses THESE resources (not a separate second device)
//! for both the offscreen render texture and the swapchain, so every wgpu object
//! lives on the same device and the TextureView lifetime assertion never fires.

use std::{
    cell::Cell,
    rc::{Rc, Weak},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use slint::platform::{
    femtovg_renderer::FemtoVGWGPURenderer, Platform, Renderer, WindowAdapter, WindowEvent,
};
use slint::{LogicalSize, PhysicalSize};
use smithay::reexports::calloop::LoopSignal;
use tracing::debug;

// ──────────────────────────────────────────────────────────────────────────────
// GPU-backed window adapter
// ──────────────────────────────────────────────────────────────────────────────

/// A Slint WindowAdapter backed by FemtoVGWGPURenderer.
/// Replaces MinimalSoftwareWindow — GPU rasterises the Slint scene.
pub struct GpuWindowAdapter {
    /// The Slint internal Window (required by WindowAdapter trait).
    slint_window: slint::Window,
    /// The FemtoVG renderer — GPU rasteriser.
    pub renderer: FemtoVGWGPURenderer,
    /// Cloned wgpu resources — the SAME instance/device/queue passed to FemtoVG.
    /// Exposed so renderer.rs can build the swapchain on the identical device,
    /// preventing the TextureView lifetime panic (wgpu requires all resources that
    /// reference each other to belong to the same Device).
    pub wgpu_instance: wgpu::Instance,
    pub wgpu_adapter: wgpu::Adapter,
    pub wgpu_device: wgpu::Device,
    pub wgpu_queue: wgpu::Queue,
    /// Current size in physical pixels.
    size: Cell<PhysicalSize>,
    /// Whether a redraw has been requested.
    needs_redraw: Cell<bool>,
}

impl GpuWindowAdapter {
    pub fn new(
        instance: wgpu::Instance,
        adapter: wgpu::Adapter,
        device: wgpu::Device,
        queue: wgpu::Queue,
        width: u32,
        height: u32,
    ) -> Rc<Self> {
        // Clone device + queue before moving them into FemtoVG so we can
        // store copies here for the swapchain (wgpu::Device/Queue are Clone).
        let renderer = FemtoVGWGPURenderer::new(instance.clone(), device.clone(), queue.clone())
            .expect("Failed to create FemtoVGWGPURenderer");

        Rc::new_cyclic(|weak: &Weak<Self>| Self {
            slint_window: slint::Window::new(weak.clone()),
            renderer,
            wgpu_instance: instance,
            wgpu_adapter: adapter,
            wgpu_device: device,
            wgpu_queue: queue,
            size: Cell::new(PhysicalSize::new(width, height)),
            needs_redraw: Cell::new(true),
        })
    }

    /// Update stored size + dispatch slint resize. `(physical_w, physical_h)`
    /// is the host's actual pixel size (matches the swapchain texture); the
    /// slint window is set to logical = physical / scale, so HiDPI scale
    /// flows through cleanly: cursor positions, hit-test coords, pointer
    /// events, and wayland forwarding all share the same logical-pixel
    /// space, while the GPU still rasterises at native physical resolution.
    pub fn resize(&self, physical_w: u32, physical_h: u32, scale: f32) {
        let new_size = PhysicalSize::new(physical_w, physical_h);
        self.size.set(new_size);
        let s = scale.max(0.0001);
        let logical = LogicalSize::new(
            physical_w as f32 / s,
            physical_h as f32 / s,
        );
        self.slint_window
            .dispatch_event(WindowEvent::ScaleFactorChanged { scale_factor: s });
        self.slint_window
            .dispatch_event(WindowEvent::Resized { size: logical });
        self.needs_redraw.set(true);
        debug!(
            "GpuWindowAdapter resized: physical={}x{} logical={}x{} scale={}",
            physical_w, physical_h, logical.width, logical.height, s,
        );
    }

    /// Render the Slint scene to the given wgpu texture (GPU path).
    /// The texture MUST have been created with this adapter's wgpu_device.
    pub fn render_to_texture(&self, texture: &wgpu::Texture) -> Result<(), slint::PlatformError> {
        self.renderer.render_to_texture(texture)?;
        self.needs_redraw.set(false);
        Ok(())
    }

    /// Access the inner Slint Window (for dispatching events + show/hide).
    pub fn inner_window(&self) -> &slint::Window {
        &self.slint_window
    }

    /// Current physical size (avoids needing to import WindowAdapter trait in callers).
    pub fn get_size(&self) -> PhysicalSize {
        self.size.get()
    }

    /// True if Slint has pending changes that require a repaint.
    pub fn has_pending_redraw(&self) -> bool {
        self.needs_redraw.get()
    }

    /// Mark dirty so the render loop knows to call render_to_texture() next frame.
    pub fn mark_dirty(&self) {
        self.needs_redraw.set(true);
    }
}

impl WindowAdapter for GpuWindowAdapter {
    fn window(&self) -> &slint::Window {
        &self.slint_window
    }

    fn size(&self) -> PhysicalSize {
        self.size.get()
    }

    fn renderer(&self) -> &dyn Renderer {
        &self.renderer
    }

    fn set_visible(&self, _visible: bool) -> Result<(), slint::PlatformError> {
        Ok(())
    }

    fn request_redraw(&self) {
        self.needs_redraw.set(true);
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Calloop platform (GPU version)
// ──────────────────────────────────────────────────────────────────────────────

/// Custom Platform that creates a GpuWindowAdapter for each Slint component.
pub struct CalloopPlatform {
    _loop_signal: LoopSignal,
    start_time: Instant,
    instance: wgpu::Instance,
    adapter: wgpu::Adapter,
    device: wgpu::Device,
    queue: wgpu::Queue,
    initial_width: u32,
    initial_height: u32,
    /// The created window adapter. SPIKE: single-window assumption.
    pub window: Arc<Mutex<Option<Rc<GpuWindowAdapter>>>>,
}

impl CalloopPlatform {
    pub fn new(
        loop_signal: LoopSignal,
        instance: wgpu::Instance,
        adapter: wgpu::Adapter,
        device: wgpu::Device,
        queue: wgpu::Queue,
        initial_width: u32,
        initial_height: u32,
    ) -> Self {
        Self {
            _loop_signal: loop_signal,
            start_time: Instant::now(),
            instance,
            adapter,
            device,
            queue,
            initial_width,
            initial_height,
            window: Arc::new(Mutex::new(None)),
        }
    }
}

impl Platform for CalloopPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        debug!("CalloopPlatform::create_window_adapter (GPU)");

        let adapter = GpuWindowAdapter::new(
            self.instance.clone(),
            self.adapter.clone(),
            self.device.clone(),
            self.queue.clone(),
            self.initial_width,
            self.initial_height,
        );

        *self.window.lock().unwrap() = Some(adapter.clone());
        Ok(adapter)
    }

    fn duration_since_start(&self) -> Duration {
        self.start_time.elapsed()
    }

    fn run_event_loop(&self) -> Result<(), slint::PlatformError> {
        // Calloop drives everything. Returning an error prevents Slint
        // from trying to own the loop (same pattern as the software spike).
        Err(slint::PlatformError::Other(
            "CalloopPlatform: use calloop directly; run_event_loop not supported".into(),
        ))
    }
}
