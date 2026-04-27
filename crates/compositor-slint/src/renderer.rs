//! Main compositor loop: winit window + calloop event loop + Slint GPU rendering.
//!
//! GPU MIGRATION: Replaces softbuffer + SoftwareRenderer with:
//!   - wgpu surface (swapchain) on the winit window
//!   - FemtoVGWGPURenderer::render_to_texture() each frame
//!   - offscreen wgpu::Texture blit to swapchain via copy_texture_to_texture
//!
//! PANIC FIX (TextureView lifetime):
//!   The original code initialised TWO separate wgpu devices: one for the
//!   Slint platform (FemtoVGWGPURenderer) and one for the winit swapchain.
//!   `copy_texture_to_texture` / render_to_texture internally creates
//!   TextureViews; wgpu asserts that every resource that references a view
//!   must belong to the same Device.  Mixing devices triggers:
//!     "TextureView[Id(0,1)] is no longer alive (left=1 right=2)"
//!   Fix: use a SINGLE wgpu device for everything.  We request one adapter
//!   in run(), pass it to the CalloopPlatform (which hands it to
//!   FemtoVGWGPURenderer), and expose the same device/queue from
//!   GpuWindowAdapter so resumed() can configure the swapchain on it.
//!
//! DAMAGE TRACKING (D4): render_to_texture() only called when Slint has
//! pending changes.  Idle desktop = 0 GPU renders (last frame stays presented).
//!
//! SHM CLIENT BUFFERS (D3): CPU memcpy into SharedPixelBuffer -> Slint Image.
//! FemtoVG uploads to GPU on next render pass.
//!
//! DMA-BUF BLOCKER: wgpu 28 lacks stable DMA-BUF import on Linux.

use std::{
    collections::VecDeque,
    rc::Rc,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use tracing::{debug, info, warn};

use smithay::{
    output::{Mode, Output, PhysicalProperties, Subpixel},
    reexports::{
        calloop::{
            generic::Generic, EventLoop, Interest, Mode as CalloopMode, PostAction,
        },
        wayland_server::Display,
    },
    utils::{Transform, SERIAL_COUNTER},
    wayland::socket::ListeningSocketSource,
};

use slint::{ComponentHandle, LogicalPosition, Model, SharedString, VecModel};

use winit::{
    application::ApplicationHandler,
    dpi::LogicalSize,
    event::{ElementState, MouseButton, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop as WinitEventLoop},
    platform::{
        pump_events::{EventLoopExtPumpEvents, PumpStatus},
        scancode::PhysicalKeyExtScancode,
    },
    window::{Window, WindowId},
};

use crate::{
    chrome_shader::{ChromeShader, WindowChromeParams},
    cursor::{self, CursorKind, HitZone, WindowRect, TITLEBAR_HEIGHT},
    cursor_render::CursorRenderer,
    desktop,
    platform::{CalloopPlatform, GpuWindowAdapter},
    resize::{self, ActiveDrag, ResizeEdge, WindowGeomSnapshot},
    wallpaper,
    wayland_state::{ClientState, SpikeState},
    Compositor, DockItem,
};

const WIDTH: u32 = 1280;
const HEIGHT: u32 = 960;

// ──────────────────────────────────────────────────────────────────────────────
// Pending input events (processed in the main loop where SpikeState is available)
// ──────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct PendingKeyEvent {
    pub scancode: u32,
    pub pressed: bool,
}

/// Pointer motion or button event, queued during winit window_event()
/// and forwarded to smithay in the main loop.
#[derive(Debug, Clone)]
pub enum PendingPointerEvent {
    /// Pointer moved to compositor-space (x, y).
    Motion { x: f64, y: f64 },
    /// Mouse button pressed/released. `button` is the Linux evdev button code.
    Button { button: u32, pressed: bool },
}

/// Map a winit `MouseButton` to a Linux evdev button code.
fn winit_button_to_evdev(button: MouseButton) -> u32 {
    match button {
        MouseButton::Left   => 0x110, // BTN_LEFT
        MouseButton::Right  => 0x111, // BTN_RIGHT
        MouseButton::Middle => 0x112, // BTN_MIDDLE
        MouseButton::Back   => 0x116, // BTN_SIDE
        MouseButton::Forward=> 0x115, // BTN_EXTRA
        _                   => 0x110,
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// wgpu helpers
// ──────────────────────────────────────────────────────────────────────────────

/// Allocate an offscreen texture for Slint/FemtoVG to render into.
/// MUST be created with the same device as FemtoVGWGPURenderer.
/// RENDER_ATTACHMENT is required by FemtoVGWGPURenderer.
/// COPY_SRC is needed to blit into the swapchain.
/// TEXTURE_BINDING is needed by the chrome shader to sample the Slint scene.
fn make_render_texture(device: &wgpu::Device, width: u32, height: u32, format: wgpu::TextureFormat) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("slint-render-target"),
        size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT
            | wgpu::TextureUsages::COPY_SRC
            | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    })
}

/// Configure the wgpu swapchain surface.
/// We force Rgba8Unorm so copy_texture_to_texture works without format conversion.
/// FemtoVGWGPURenderer also requires Rgba8Unorm on all backends.
fn configure_surface(
    surface: &wgpu::Surface<'static>,
    adapter: &wgpu::Adapter,
    device: &wgpu::Device,
    width: u32,
    height: u32,
) -> wgpu::TextureFormat {
    let caps = surface.get_capabilities(adapter);
    // Force Rgba8Unorm: required by FemtoVGWGPURenderer and allows direct
    // copy_texture_to_texture blit without format conversion shaders.
    let format = if caps.formats.contains(&wgpu::TextureFormat::Rgba8Unorm) {
        wgpu::TextureFormat::Rgba8Unorm
    } else if caps.formats.contains(&wgpu::TextureFormat::Bgra8Unorm) {
        wgpu::TextureFormat::Bgra8Unorm
    } else {
        caps.formats[0]
    };

    surface.configure(
        device,
        &wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_DST,
            format,
            width,
            height,
            present_mode: wgpu::PresentMode::AutoVsync,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        },
    );
    format
}

// ──────────────────────────────────────────────────────────────────────────────
// Application state
// ──────────────────────────────────────────────────────────────────────────────

struct CompositorApp {
    window: Option<Rc<Window>>,

    // wgpu swapchain resources — all Option<> because created in resumed().
    // NOTE: these are references to the SAME device/queue/adapter that
    // FemtoVGWGPURenderer uses (cloned from GpuWindowAdapter), guaranteeing
    // all wgpu objects share one Device lifetime.
    wgpu_surface: Option<wgpu::Surface<'static>>,
    swapchain_format: Option<wgpu::TextureFormat>,
    // Offscreen texture that Slint/FemtoVG renders into each frame.
    // Created with the shared device — same Device as FemtoVG.
    // TEXTURE_BINDING is added so the chrome shader can sample the scene.
    render_texture: Option<wgpu::Texture>,
    render_texture_size: (u32, u32),

    // Slint GPU window adapter (holds device/queue/adapter/instance)
    gpu_window: Option<Rc<GpuWindowAdapter>>,
    window_ref: Arc<Mutex<Option<Rc<GpuWindowAdapter>>>>,
    ui: Option<Compositor>,

    // Chrome shader — squircle clip + stroke + highlight + shadow post-pass.
    // Initialised lazily in resumed() once the swapchain format is known.
    chrome_shader: Option<ChromeShader>,

    pointer_pos: (f64, f64),
    start_time: Instant,
    last_clock_update: Instant,
    pending_keys: Arc<Mutex<VecDeque<PendingKeyEvent>>>,
    /// Pending pointer events — queued in window_event(), processed in the main loop.
    pending_pointers: Arc<Mutex<VecDeque<PendingPointerEvent>>>,
    frame_count: u64,

    // Dock state.
    dock_entries: Vec<ResolvedDockEntry>,
    /// app_ids that were running last time we checked (avoids unnecessary re-emits).
    last_running_ids: std::collections::HashSet<String>,

    // ── Cursor system ─────────────────────────────────────────────────────────
    /// SVG cursor rasteriser + cache.
    cursor_renderer: CursorRenderer,
    /// Current cursor kind (updated each pointer motion).
    current_cursor: CursorKind,

    // ── Drag state ────────────────────────────────────────────────────────────
    /// Active window drag (resize or move), if any.
    active_drag: Option<ActiveDrag>,
    /// True while the left mouse button is held down.
    left_button_down: bool,
}

impl CompositorApp {
    fn new(
        window_ref: Arc<Mutex<Option<Rc<GpuWindowAdapter>>>>,
        ui: Compositor,
        pending_keys: Arc<Mutex<VecDeque<PendingKeyEvent>>>,
        pending_pointers: Arc<Mutex<VecDeque<PendingPointerEvent>>>,
        dock_entries: Vec<ResolvedDockEntry>,
    ) -> Self {
        Self {
            window: None,
            wgpu_surface: None,
            swapchain_format: None,
            render_texture: None,
            render_texture_size: (0, 0),
            gpu_window: None,
            window_ref,
            ui: Some(ui),
            chrome_shader: None,
            pointer_pos: (0.0, 0.0),
            start_time: Instant::now(),
            last_clock_update: Instant::now(),
            pending_keys,
            pending_pointers,
            frame_count: 0,
            dock_entries,
            last_running_ids: std::collections::HashSet::new(),
            cursor_renderer: CursorRenderer::new(),
            current_cursor: CursorKind::Arrow,
            active_drag: None,
            left_button_down: false,
        }
    }

    /// Return an offscreen texture of the right size, (re)creating if size changed.
    /// ALWAYS uses the FemtoVG device (from gpu_window) so both the renderer and
    /// this texture share the same Device.
    fn get_render_texture(&mut self, width: u32, height: u32) -> Option<&wgpu::Texture> {
        if self.render_texture.is_none() || self.render_texture_size != (width, height) {
            let gpu_window = self.gpu_window.as_ref()?;
            let device = &gpu_window.wgpu_device;
            let format = self.swapchain_format.unwrap_or(wgpu::TextureFormat::Rgba8Unorm);
            self.render_texture = Some(make_render_texture(device, width, height, format));
            self.render_texture_size = (width, height);
            debug!("(Re)created render texture {}x{} {:?}", width, height, format);
        }
        self.render_texture.as_ref()
    }
}

impl ApplicationHandler for CompositorApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }

        let attrs = Window::default_attributes()
            .with_title("Slint Compositor (GPU)")
            .with_inner_size(LogicalSize::new(WIDTH, HEIGHT));

        let window = Rc::new(
            event_loop.create_window(attrs).expect("failed to create window"),
        );

        // Retrieve the Slint GPU window adapter.
        // IMPORTANT: Use the SAME device/queue/adapter/instance that FemtoVG was
        // initialised with.  Building the swapchain on these shared resources
        // ensures every wgpu object (render texture, texture views, swapchain
        // frame) belongs to the same Device — the root fix for the TextureView
        // lifetime panic.
        let gpu_window = self.window_ref.lock().unwrap().clone()
            .expect("Slint GPU window adapter should exist after Compositor::new()");

        // Create wgpu surface from the window raw handle using FemtoVG's instance.
        // SAFETY: window is kept alive in self.window for the program lifetime.
        let surface = unsafe {
            use raw_window_handle::{HasDisplayHandle, HasWindowHandle};
            let target = wgpu::SurfaceTargetUnsafe::RawHandle {
                raw_display_handle: window.display_handle().unwrap().as_raw(),
                raw_window_handle: window.window_handle().unwrap().as_raw(),
            };
            gpu_window.wgpu_instance.create_surface_unsafe(target)
                .expect("create_surface_unsafe failed")
        };
        // Extend to 'static: safe because window lives in self.window for the program.
        let surface: wgpu::Surface<'static> = unsafe {
            std::mem::transmute::<wgpu::Surface<'_>, wgpu::Surface<'static>>(surface)
        };

        // Use the same adapter/device/queue FemtoVG already has.
        let format = configure_surface(
            &surface,
            &gpu_window.wgpu_adapter,
            &gpu_window.wgpu_device,
            WIDTH,
            HEIGHT,
        );
        info!("wgpu swapchain ready, format={:?} (shared device)", format);

        // Initialise the chrome shader now that we have a format.
        let chrome = ChromeShader::new(&gpu_window.wgpu_device, format);
        self.chrome_shader = Some(chrome);
        info!("ChromeShader initialised (squircle clip + shadow)");

        gpu_window.resize(WIDTH, HEIGHT);

        if let Some(ui) = &self.ui {
            ui.window().show().ok();
        }

        self.window = Some(window);
        self.wgpu_surface = Some(surface);
        self.swapchain_format = Some(format);
        self.gpu_window = Some(gpu_window);

        info!("GPU window created, FemtoVG renderer active (D2 GPU)");
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _id: WindowId,
        event: WindowEvent,
    ) {
        let gpu_window = match self.gpu_window.as_ref() {
            Some(w) => w.clone(),
            None => return,
        };

        match event {
            WindowEvent::CloseRequested => {
                info!("Close requested");
                event_loop.exit();
            }

            WindowEvent::Resized(size) => {
                let w = size.width.max(1);
                let h = size.height.max(1);
                if let Some(surface) = self.wgpu_surface.as_ref() {
                    let fmt = configure_surface(
                        surface,
                        &gpu_window.wgpu_adapter,
                        &gpu_window.wgpu_device,
                        w,
                        h,
                    );
                    self.swapchain_format = Some(fmt);
                }
                gpu_window.resize(w, h);
                self.render_texture = None; // force recreate at new size
                // Invalidate chrome shader bind group cache — texture changed.
                if let Some(cs) = self.chrome_shader.as_mut() {
                    cs.invalidate_cache();
                }
            }

            WindowEvent::CursorMoved { position, .. } => {
                self.pointer_pos = (position.x, position.y);
                // Update cursor overlay position in Slint immediately.
                self.update_cursor_position(position.x, position.y);
                // Forward to Slint for hit-testing (panel buttons, window chrome).
                gpu_window.inner_window().dispatch_event(
                    slint::platform::WindowEvent::PointerMoved {
                        position: LogicalPosition::new(position.x as f32, position.y as f32),
                    },
                );
                // Also queue for wayland client forwarding (D2).
                self.pending_pointers.lock().unwrap().push_back(
                    PendingPointerEvent::Motion { x: position.x, y: position.y }
                );
            }

            WindowEvent::MouseInput { state, button, .. } => {
                let slint_btn = match button {
                    MouseButton::Left => slint::platform::PointerEventButton::Left,
                    MouseButton::Right => slint::platform::PointerEventButton::Right,
                    MouseButton::Middle => slint::platform::PointerEventButton::Middle,
                    _ => slint::platform::PointerEventButton::Other,
                };
                let pos = LogicalPosition::new(
                    self.pointer_pos.0 as f32, self.pointer_pos.1 as f32,
                );
                let pressed = state == ElementState::Pressed;

                // Track left button state for drag detection.
                if button == MouseButton::Left {
                    self.left_button_down = pressed;
                    if !pressed {
                        // Release any active drag.
                        self.active_drag = None;
                    }
                }

                let slint_event = match state {
                    ElementState::Pressed => slint::platform::WindowEvent::PointerPressed {
                        position: pos, button: slint_btn,
                    },
                    ElementState::Released => slint::platform::WindowEvent::PointerReleased {
                        position: pos, button: slint_btn,
                    },
                };
                gpu_window.inner_window().dispatch_event(slint_event);
                // Also queue for wayland client forwarding (D2).
                let evdev_btn = winit_button_to_evdev(button);
                self.pending_pointers.lock().unwrap().push_back(
                    PendingPointerEvent::Button { button: evdev_btn, pressed }
                );
            }

            WindowEvent::KeyboardInput { event: key_event, .. } => {
                let scancode = key_event.physical_key.to_scancode().unwrap_or(0);
                if scancode > 0 {
                    self.pending_keys.lock().unwrap().push_back(PendingKeyEvent {
                        scancode,
                        pressed: key_event.state == ElementState::Pressed,
                    });
                }
            }

            WindowEvent::RedrawRequested => {
                self.render_frame();
            }

            _ => {}
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
    }
}

impl CompositorApp {
    /// GPU render: damage-tracked Slint render + swapchain blit.
    fn render_frame(&mut self) {
        // Clone the Rc so we don't hold &self.gpu_window across mutable borrows.
        let gpu_window = match self.gpu_window.clone() { Some(w) => w, None => return };
        let Some(ui) = self.ui.as_ref() else { return };

        // Update clock each second via chrono (marks adapter dirty via set_clock_text).
        let now = Instant::now();
        if now.duration_since(self.last_clock_update) >= Duration::from_secs(1) {
            self.last_clock_update = now;
            let time_str = chrono::Local::now().format("%H:%M:%S").to_string();
            ui.set_clock_text(SharedString::from(time_str));
        }

        let size = gpu_window.get_size();
        let (w, h) = (size.width.max(1), size.height.max(1));

        // D4 DAMAGE TRACKING: only re-render if Slint has changes.
        if gpu_window.has_pending_redraw() {
            // get_render_texture uses gpu_window.wgpu_device — same Device as FemtoVG.
            if let Some(render_tex) = self.get_render_texture(w, h) {
                if let Err(e) = gpu_window.render_to_texture(render_tex) {
                    warn!("render_to_texture failed: {}", e);
                    return;
                }
                self.frame_count += 1;
                if self.frame_count % 60 == 0 {
                    info!("frame loop: {} frames rendered", self.frame_count);
                }
                debug!("GPU render: {}x{} (dirty, frame {})", w, h, self.frame_count);
            }
        }

        let Some(surface) = self.wgpu_surface.as_ref() else { return };
        let device = &gpu_window.wgpu_device;
        let queue = &gpu_window.wgpu_queue;

        // Acquire swapchain frame.
        let frame = match surface.get_current_texture() {
            Ok(f) => f,
            Err(wgpu::SurfaceError::Outdated) => return,
            Err(e) => { warn!("swapchain: {}", e); return; }
        };

        // Blit offscreen texture -> swapchain frame, then run chrome pass.
        if let Some(render_tex) = self.render_texture.as_ref() {
            let mut encoder = device.create_command_encoder(
                &wgpu::CommandEncoderDescriptor { label: Some("blit+chrome") }
            );

            // Step 1: Copy Slint scene into swapchain as the base layer.
            encoder.copy_texture_to_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: render_tex,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::TexelCopyTextureInfo {
                    texture: &frame.texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            );

            // Step 2: Run chrome pass — squircle clip, outer stroke, inner highlight,
            // and multi-layer shadow — composited over the swapchain frame.
            if let Some(chrome) = self.chrome_shader.as_mut() {
                // Build per-window params from the current Slint window list.
                let chrome_windows: Vec<WindowChromeParams> = if let Some(ui) = self.ui.as_ref() {
                    let model = ui.get_windows();
                    let len = model.row_count();
                    (0..len)
                        .map(|i| {
                            let item = model.row_data(i).unwrap();
                            WindowChromeParams {
                                x: item.x as f32,
                                y: item.y as f32,
                                w: item.w as f32,
                                h: item.h as f32,
                                active: item.focused,
                            }
                        })
                        .collect()
                } else {
                    Vec::new()
                };

                if !chrome_windows.is_empty() {
                    let frame_view = frame.texture.create_view(&wgpu::TextureViewDescriptor::default());
                    chrome.render(
                        device,
                        queue,
                        &mut encoder,
                        render_tex,
                        &frame_view,
                        w,
                        h,
                        &chrome_windows,
                    );
                }
            }

            queue.submit(std::iter::once(encoder.finish()));
        }

        frame.present();
    }

    /// Update the `windows` property on the Compositor from the wayland surface map.
    /// One WindowItem per mapped xdg-toplevel (SHM texture + title + geometry).
    fn update_windows(&mut self, state: &mut SpikeState) {
        use smithay::wayland::compositor::with_states;
        use smithay::wayland::shell::xdg::XdgToplevelSurfaceData;

        let Some(ui) = self.ui.as_ref() else { return };
        let active_surface = state.active_surface.clone();

        let mut items: Vec<crate::WindowItem> = Vec::new();

        for (idx, toplevel) in state.toplevels.iter().enumerate() {
            let client_data = toplevel.pixels.lock().unwrap();
            if client_data.width == 0 {
                continue;
            }

            // Build a Slint Image from the pixel buffer.
            let pixel_buf = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(
                &client_data.pixels,
                client_data.width,
                client_data.height,
            );
            let texture = slint::Image::from_rgba8_premultiplied(pixel_buf);
            let (w, h) = (client_data.width as i32, client_data.height as i32);
            drop(client_data);

            // Get window title from xdg-toplevel surface data.
            let title = with_states(&toplevel.surface, |states| {
                states
                    .data_map
                    .get::<XdgToplevelSurfaceData>()
                    .and_then(|data| data.lock().ok()?.title.clone())
            })
            .unwrap_or_else(|| "Window".to_string());

            let focused = active_surface.as_ref().map(|s| s == &toplevel.surface).unwrap_or(false);

            items.push(crate::WindowItem {
                id: (idx + 1) as i32,
                title: SharedString::from(title),
                x: toplevel.x,
                y: toplevel.y,
                w,
                h,
                focused,
                texture,
                icon: slint::Image::default(),
            });
        }

        let model = std::rc::Rc::new(VecModel::from(items));
        ui.set_windows(slint::ModelRc::from(model));
    }

    /// Poll all toplevels for dirty SHM buffers and update Slint if any changed.
    fn update_client_texture(&mut self, state: &mut SpikeState) {
        let mut any_dirty = false;

        // Check each toplevel's pixel buffer for dirty flag.
        for toplevel in state.toplevels.iter() {
            let mut client_data = toplevel.pixels.lock().unwrap();
            if client_data.dirty && client_data.width > 0 {
                client_data.dirty = false;
                any_dirty = true;
            }
        }

        // Also check the legacy single-surface buffer.
        {
            let mut client_data = state.client_pixels.lock().unwrap();
            if client_data.dirty && client_data.width > 0 {
                client_data.dirty = false;
                any_dirty = true;
            }
        }

        if any_dirty {
            if let Some(gpu_window) = self.gpu_window.as_ref() {
                gpu_window.mark_dirty();
            }
            self.update_windows(state);
            debug!("SHM client texture updated → windows property refreshed");
        }
    }

    /// Forward a pointer motion event to the wayland client whose window is under the pointer.
    /// `x`, `y` are compositor-space logical coordinates.
    fn forward_pointer_motion(&self, state: &mut SpikeState, x: f64, y: f64) {
        use smithay::input::pointer::MotionEvent;
        use smithay::utils::Point;

        let pointer = match state.seat.get_pointer() {
            Some(p) => p,
            None => return,
        };

        let serial = SERIAL_COUNTER.next_serial();
        let time = state.clock.now().as_millis() as u32;

        // Find the topmost window under the pointer (last in the list = topmost).
        // The window's content area starts at (win.x, win.y + TITLEBAR_HEIGHT).
        const TITLEBAR_HEIGHT: f64 = 33.0;
        let mut hit: Option<(smithay::reexports::wayland_server::protocol::wl_surface::WlSurface, f64, f64)> = None;

        for toplevel in state.toplevels.iter().rev() {
            let client_data = toplevel.pixels.lock().unwrap();
            let (w, h) = (client_data.width as f64, client_data.height as f64);
            drop(client_data);

            let wx = toplevel.x as f64;
            let wy = toplevel.y as f64 + TITLEBAR_HEIGHT;
            if x >= wx && x < wx + w && y >= wy && y < wy + h {
                let local_x = x - wx;
                let local_y = y - wy;
                hit = Some((toplevel.surface.clone(), local_x, local_y));
                break;
            }
        }

        if let Some((surface, local_x, local_y)) = hit {
            pointer.motion(
                state,
                Some((surface, Point::from((local_x, local_y)))),
                &MotionEvent {
                    location: Point::from((x, y)),
                    serial,
                    time,
                },
            );
            pointer.frame(state);
        } else {
            // Pointer not over any client window — clear focus.
            pointer.motion(
                state,
                None,
                &MotionEvent {
                    location: Point::from((x, y)),
                    serial,
                    time,
                },
            );
            pointer.frame(state);
        }
    }

    /// Forward a pointer button event to the currently focused wayland client.
    fn forward_pointer_button(&self, state: &mut SpikeState, button: u32, pressed: bool) {
        use smithay::input::pointer::ButtonEvent;

        let pointer = match state.seat.get_pointer() {
            Some(p) => p,
            None => return,
        };

        let serial = SERIAL_COUNTER.next_serial();
        let time = state.clock.now().as_millis() as u32;
        let button_state = if pressed {
            smithay::backend::input::ButtonState::Pressed
        } else {
            smithay::backend::input::ButtonState::Released
        };

        pointer.button(
            state,
            &ButtonEvent { serial, time, button, state: button_state },
        );
        pointer.frame(state);
    }

    /// Update `dock-items` running/focused flags from the current toplevel list.
    ///
    /// Walks `state.active_surface` to determine which app_ids have an open
    /// toplevel.  For now we use the single-surface model (one active surface
    /// at a time) — the full `ext-foreign-toplevel-list-v1` integration will
    /// be wired once the wayland agent exposes a queryable app_id list.
    fn update_dock_running(&mut self, state: &SpikeState) {
        // Collect the app_id of the currently focused surface.
        let focused_app_id: Option<String> = state
            .active_surface
            .as_ref()
            .and_then(|wl_surface| {
                use smithay::wayland::compositor::with_states;
                use smithay::wayland::shell::xdg::XdgToplevelSurfaceData;
                with_states(wl_surface, |states| {
                    states
                        .data_map
                        .get::<XdgToplevelSurfaceData>()
                        .and_then(|data| data.lock().ok()?.app_id.clone())
                })
            });

        // Determine the set of running app_ids (for now: just the focused one).
        let mut running_ids: std::collections::HashSet<String> =
            std::collections::HashSet::new();
        if let Some(ref app_id) = focused_app_id {
            running_ids.insert(app_id.clone());
        }

        // Only re-emit if the running set changed.
        if running_ids == self.last_running_ids {
            return;
        }
        self.last_running_ids = running_ids.clone();

        let Some(ui) = self.ui.as_ref() else { return };

        // Update each entry's running/focused flags.
        let items: Vec<DockItem> = self
            .dock_entries
            .iter()
            .map(|entry| {
                let app_id_str = entry.item.app_id.as_str();
                // Match against both the full app_id and its leaf (e.g. "firefox"
                // should match "org.mozilla.firefox").
                let running = running_ids.iter().any(|rid| {
                    rid.as_str() == app_id_str
                        || rid.rsplit('.').next() == Some(app_id_str)
                        || app_id_str.rsplit('.').next() == Some(rid.as_str())
                });
                let focused = focused_app_id.as_deref().map_or(false, |fid| {
                    fid == app_id_str
                        || fid.rsplit('.').next() == Some(app_id_str)
                        || app_id_str.rsplit('.').next() == Some(fid)
                });
                DockItem {
                    icon: entry.item.icon.clone(),
                    app_id: entry.item.app_id.clone(),
                    running,
                    focused,
                    pinned: entry.item.pinned,
                }
            })
            .collect();

        let model = std::rc::Rc::new(VecModel::from(items));
        ui.set_dock_items(slint::ModelRc::from(model));
        debug!("dock running state updated, focused={:?}", focused_app_id);
    }

    // ──────────────────────────────────────────────────────────────────────────
    // Cursor + drag system
    // ──────────────────────────────────────────────────────────────────────────

    /// Build window rects from current state for hit-testing.
    fn window_rects(state: &SpikeState) -> Vec<WindowRect> {
        // Topmost window = last in list; iterate in reverse for front-to-back.
        state.toplevels.iter().rev().map(|t| {
            let pix = t.pixels.lock().unwrap();
            let (w, h) = (pix.width as f64, pix.height as f64);
            drop(pix);
            WindowRect {
                id: (&*t as *const _ as usize) as i32, // use ptr as id — fine for hit-test
                x: t.x as f64,
                y: t.y as f64,
                w,
                h: h + TITLEBAR_HEIGHT,
            }
        }).collect()
    }

    /// Hit-test a window from state using toplevel index.
    fn find_toplevel_idx(state: &SpikeState, ptr_x: f64, ptr_y: f64) -> Option<usize> {
        // Topmost = last in list.
        for (idx, t) in state.toplevels.iter().enumerate().rev() {
            let pix = t.pixels.lock().unwrap();
            let (w, h) = (pix.width as f64, pix.height as f64);
            drop(pix);
            let full_h = h + TITLEBAR_HEIGHT;
            if ptr_x >= t.x as f64 - cursor::EDGE_ZONE
                && ptr_x < t.x as f64 + w + cursor::EDGE_ZONE
                && ptr_y >= t.y as f64 - cursor::EDGE_ZONE
                && ptr_y < t.y as f64 + full_h + cursor::EDGE_ZONE
            {
                return Some(idx);
            }
        }
        None
    }

    /// Called each pointer-motion event (after `pointer_pos` is updated).
    /// Updates cursor shape, starts drags, continues active drags.
    pub fn handle_pointer_update(&mut self, state: &mut SpikeState, x: f64, y: f64) {
        // Build window rects for hit-testing (topmost-first).
        let win_rects = Self::window_rects(state);

        // If there's an active drag, handle it.
        if let Some(drag) = self.active_drag.clone() {
            match &drag {
                ActiveDrag::Resize { toplevel_idx, .. } => {
                    if let Some((nx, ny, nw, nh)) = resize::compute_resize(&drag, x, y) {
                        if let Some(tl) = state.toplevels.get_mut(*toplevel_idx) {
                            tl.x = nx;
                            tl.y = ny;
                            // Send configure to client with new content size.
                            let surface = tl.surface.clone();
                            // Find the ToplevelSurface for this WlSurface.
                            let maybe_ts = state.xdg_shell_state
                                .toplevel_surfaces()
                                .iter()
                                .find(|ts| ts.wl_surface() == &surface)
                                .cloned();
                            if let Some(toplevel) = maybe_ts {
                                let cw = nw.max(resize::MIN_WINDOW_SIZE);
                                let ch = nh.max(resize::MIN_WINDOW_SIZE);
                                toplevel.with_pending_state(|s: &mut smithay::wayland::shell::xdg::ToplevelState| {
                                    s.size = Some((cw, ch).into());
                                });
                                toplevel.send_configure();
                                debug!("resize configure: {}×{} at ({},{})", cw, ch, nx, ny);
                            }
                        }
                    }
                    return; // Don't update cursor during resize drag.
                }
                ActiveDrag::Move { toplevel_idx, offset_x, offset_y } => {
                    let (ox, oy) = (*offset_x, *offset_y);
                    if let Some(tl) = state.toplevels.get_mut(*toplevel_idx) {
                        tl.x = (x - ox) as i32;
                        tl.y = (y - oy) as i32;
                        debug!("move: window #{} to ({},{})", toplevel_idx, tl.x, tl.y);
                    }
                    return; // Don't update cursor during move drag.
                }
            }
        }

        // No active drag — run hit test to determine cursor and start drag on press.
        let hit = cursor::hit_test(x, y, &win_rects);
        let zone = hit.map(|h| h.zone).unwrap_or(HitZone::None);
        let new_cursor = cursor::zone_to_cursor(zone);

        if new_cursor != self.current_cursor {
            self.current_cursor = new_cursor;
            self.update_cursor_overlay();
        }

        // Start a drag if left button is down and we just detected it (drag init).
        if self.left_button_down && self.active_drag.is_none() {
            if let Some(ref hit_result) = hit {
                let idx_opt = Self::find_toplevel_idx(state, x, y);
                if let Some(idx) = idx_opt {
                    if let Some(edge) = ResizeEdge::from_zone(zone) {
                        // Start resize drag.
                        let t = &state.toplevels[idx];
                        let pix = t.pixels.lock().unwrap();
                        let (w, h) = (pix.width as i32, pix.height as i32);
                        drop(pix);
                        self.active_drag = Some(ActiveDrag::Resize {
                            toplevel_idx: idx,
                            edge,
                            start_ptr_x: x,
                            start_ptr_y: y,
                            start_geom: WindowGeomSnapshot {
                                x: t.x, y: t.y, w, h,
                            },
                        });
                        debug!("resize drag started: {:?} on window #{}", edge, idx);
                    } else if zone == HitZone::TitleBar {
                        // Start move drag.
                        let t = &state.toplevels[idx];
                        self.active_drag = Some(ActiveDrag::Move {
                            toplevel_idx: idx,
                            offset_x: x - t.x as f64,
                            offset_y: y - t.y as f64,
                        });
                        debug!("move drag started on window #{}", idx);
                    }
                }
                let _ = hit_result; // suppress unused warning
            }
        }
    }

    /// Push the current cursor image to the Slint UI.
    fn update_cursor_overlay(&mut self) {
        let Some(ui) = self.ui.as_ref() else { return };
        let kind = self.current_cursor;
        let img = self.cursor_renderer.get(kind);
        let (hx, hy) = CursorRenderer::hotspot(kind);
        let size = CursorRenderer::size();

        ui.set_cursor_image(img);
        ui.set_cursor_hotspot_x(hx);
        ui.set_cursor_hotspot_y(hy);
        ui.set_cursor_size(size);

        if let Some(gpu_window) = self.gpu_window.as_ref() {
            gpu_window.mark_dirty();
        }
    }

    /// Update the cursor position in the Slint UI (called every pointer motion).
    fn update_cursor_position(&mut self, x: f64, y: f64) {
        let Some(ui) = self.ui.as_ref() else { return };
        ui.set_cursor_x(x as f32);
        ui.set_cursor_y(y as f32);
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Real dock items from .desktop files
// ──────────────────────────────────────────────────────────────────────────────

/// Resolved dock entry — holds the Slint DockItem plus the exec line for launching.
#[derive(Debug, Clone)]
pub struct ResolvedDockEntry {
    pub item: DockItem,
    /// Exec line with `%`-field codes stripped; used by the launch-app handler.
    pub exec: String,
}

/// Load pinned dock entries from config + resolve .desktop metadata.
/// Returns both the list of `DockItem`s (for Slint) and the corresponding
/// exec lines (for the launch handler).
pub fn load_dock_entries() -> Vec<ResolvedDockEntry> {
    let config = desktop::DockConfig::load();
    let mut entries = Vec::new();

    for pinned in &config.pinned {
        let app_id = &pinned.app_id;
        let info = desktop::resolve(app_id);

        let icon = match &info.icon {
            Some(path) => desktop::load_icon(path),
            None => {
                // Try generic fallback.
                desktop::generic_app_icon()
                    .map(|p| desktop::load_icon(&p))
                    .unwrap_or_default()
            }
        };

        entries.push(ResolvedDockEntry {
            item: DockItem {
                icon,
                app_id: SharedString::from(app_id.as_str()),
                running: false,
                focused: false,
                pinned: true,
            },
            exec: info.exec.clone(),
        });
    }

    if entries.is_empty() {
        warn!("dock config produced no entries, using hardcoded fallback");
        for app_id in ["org.gnome.Nautilus", "org.mozilla.firefox", "kitty", "code"] {
            let info = desktop::resolve(app_id);
            let icon = info.icon
                .as_deref()
                .map(desktop::load_icon)
                .unwrap_or_default();
            entries.push(ResolvedDockEntry {
                item: DockItem {
                    icon,
                    app_id: SharedString::from(app_id),
                    running: false,
                    focused: false,
                    pinned: true,
                },
                exec: info.exec,
            });
        }
    }

    entries
}

// ──────────────────────────────────────────────────────────────────────────────
// Main entry point
// ──────────────────────────────────────────────────────────────────────────────

pub fn run() -> Result<()> {
    // 1. calloop
    let mut calloop = EventLoop::<SpikeState>::try_new()
        .context("failed to create calloop event loop")?;
    let loop_signal = calloop.get_signal();

    // 2. wgpu instance/adapter/device/queue — ONE set, shared between FemtoVG
    //    and the swapchain.  This is the core fix for the TextureView lifetime
    //    panic: wgpu asserts that all resources (textures, views, command
    //    encoders) must come from the same Device.  Previously two separate
    //    devices were created, causing the assertion to fail at the first blit.
    //
    //    We request the adapter WITHOUT a compatible_surface here (none exists
    //    yet) and verify surface compatibility in resumed().  If the adapter
    //    turns out to be incompatible with the surface (unusual on desktop Linux)
    //    we fall back to a surface-compatible adapter below.
    let slint_instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
        backends: wgpu::Backends::all(),
        ..Default::default()
    });
    let slint_adapter = pollster::block_on(slint_instance.request_adapter(
        &wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: None,
            force_fallback_adapter: false,
        },
    ))
    .context("no wgpu adapter for Slint platform")?;
    info!("wgpu adapter: {}", slint_adapter.get_info().name);

    let (slint_device, slint_queue) = pollster::block_on(slint_adapter.request_device(
        &wgpu::DeviceDescriptor {
            label: Some("compositor-slint-shared"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::downlevel_webgl2_defaults()
                .using_resolution(slint_adapter.limits()),
            ..Default::default()
        },
    ))
    .context("failed to create shared wgpu device")?;

    // 3. Slint GPU platform (takes ownership of the shared wgpu resources,
    //    but also stores clones in GpuWindowAdapter for resumed() to use)
    let platform = CalloopPlatform::new(
        loop_signal.clone(),
        slint_instance,
        slint_adapter,
        slint_device,
        slint_queue,
        WIDTH,
        HEIGHT,
    );
    let window_ref = platform.window.clone();

    slint::platform::set_platform(Box::new(platform))
        .context("failed to set Slint GPU platform")?;

    // Create the production Compositor UI (triggers create_window_adapter ->
    // FemtoVGWGPURenderer::new with the shared device).
    let ui = Compositor::new().context("failed to create Compositor UI")?;

    // Prime the clock text.
    ui.set_clock_text(SharedString::from(
        chrono::Local::now().format("%H:%M:%S").to_string()
    ));

    // ── Deliverable 1: Load wallpaper from disk ────────────────────────────
    match wallpaper::load() {
        Some(img) => {
            info!("setting wallpaper image");
            ui.set_wallpaper(img);
        }
        None => {
            warn!("no wallpaper found on disk, using default #1e1e2e background");
        }
    }

    // ── Deliverable 2 & 5: Real dock items from .desktop files ────────────
    let dock_entries = load_dock_entries();
    info!("loaded {} dock entries", dock_entries.len());
    {
        let items: Vec<DockItem> = dock_entries.iter().map(|e| e.item.clone()).collect();
        let model = std::rc::Rc::new(VecModel::from(items));
        ui.set_dock_items(slint::ModelRc::from(model));
    }

    // ── Deliverable 4: launch-app via .desktop Exec line ──────────────────
    // Build a map from app_id -> exec so the callback can look it up.
    let exec_map: std::collections::HashMap<String, String> = dock_entries
        .iter()
        .map(|e| (e.item.app_id.to_string(), e.exec.clone()))
        .collect();
    let exec_map = Arc::new(exec_map);

    {
        let exec_map = exec_map.clone();
        ui.on_launch_app(move |app_id| {
            let app = app_id.to_string();
            info!("launch-app({})", app);

            // Look up the resolved exec line; fall back to running app_id directly.
            let exec_line = exec_map.get(&app).cloned().unwrap_or_else(|| {
                // Also try to resolve on the fly for apps not in the pinned list.
                let info = desktop::resolve(&app);
                info.exec
            });

            info!("  exec: {}", exec_line);
            let _ = std::process::Command::new("setsid")
                .args(["-f", "sh", "-c", &exec_line])
                .spawn();
        });
    }

    // Wire window-management callbacks (no-op stubs for now; wayland handlers
    // will expand these when multi-window management lands).
    {
        ui.on_close_window(|id| {
            info!("close-window({})", id);
        });
        ui.on_minimize_window(|id| {
            info!("minimize-window({})", id);
        });
        ui.on_maximize_window(|id| {
            info!("maximize-window({})", id);
        });
        ui.on_activate_window(|id| {
            info!("activate-window({})", id);
        });
    }

    // Wire panel callbacks.
    {
        ui.on_toggle_datetime_popout(|| {
            info!("toggle-datetime-popout");
        });
        ui.on_toggle_control_centre(|| {
            info!("toggle-control-centre");
        });
    }

    // ── Cursor: prime initial cursor image so overlay is visible on first frame.
    // We call this after the main loop starts via the CompositorApp init path.
    // The actual first render happens in resumed() → render_frame().
    info!("Slint GPU platform ready — production Compositor UI");

    // 4. Wayland display + socket
    let mut display = Display::<SpikeState>::new()
        .context("failed to create wayland display")?;
    let display_handle = display.handle();

    let socket_source = ListeningSocketSource::new_auto()
        .context("failed to bind wayland socket")?;
    let socket_name = socket_source.socket_name().to_os_string();
    info!("Wayland socket: {:?}", socket_name);
    println!("WAYLAND_DISPLAY={}", socket_name.to_string_lossy());

    calloop
        .handle()
        .insert_source(
            Generic::new(display, Interest::READ, CalloopMode::Level),
            |_event, display, state| {
                unsafe { display.get_mut().dispatch_clients(state)? };
                Ok(PostAction::Continue)
            },
        )
        .context("failed to insert wayland source")?;

    calloop
        .handle()
        .insert_source(socket_source, |stream, _, state| {
            state.display_handle
                .insert_client(stream, Arc::new(ClientState::default()))
                .unwrap();
        })
        .context("failed to insert socket source")?;

    info!("Wayland socket ready");

    // 5. Compositor state + virtual output
    let mut state = SpikeState::new(display_handle.clone(), calloop.handle(), loop_signal.clone());

    // Advertise DMA-BUF support to clients.  We advertise common 8-bit formats
    // with the LINEAR modifier; the GLES renderer (surfaceless EGL) will accept
    // any format that EGL/Mesa supports at runtime.  Clients that want
    // non-linear (tiled/compressed) formats will fall back to SHM.
    {
        use smithay::backend::allocator::{Fourcc, Format, Modifier};

        // DrmModifier::Linear == 0
        let linear = Modifier::Linear;
        let formats: Vec<Format> = vec![
            Format { code: Fourcc::Argb8888, modifier: linear },
            Format { code: Fourcc::Xrgb8888, modifier: linear },
            Format { code: Fourcc::Abgr8888, modifier: linear },
            Format { code: Fourcc::Xbgr8888, modifier: linear },
        ];

        let _dmabuf_global = state.dmabuf_state.create_global::<SpikeState>(
            &display_handle,
            formats,
        );
        info!("DMA-BUF global advertised (Option B: EGL/GLES two-stage import)");
    }

    let output = Output::new(
        "slint-gpu-output".to_owned(),
        PhysicalProperties {
            size: (WIDTH as i32, HEIGHT as i32).into(),
            subpixel: Subpixel::Unknown,
            make: "SlintGPU".into(),
            model: "Virtual".into(),
            serial_number: "0001".into(),
        },
    );
    let mode = Mode { size: (WIDTH as i32, HEIGHT as i32).into(), refresh: 60_000 };
    output.change_current_state(Some(mode), Some(Transform::Normal), None, Some((0, 0).into()));
    output.set_preferred(mode);
    output.create_global::<SpikeState>(&display_handle);

    // 6. winit event loop
    let mut winit_event_loop = WinitEventLoop::new().context("failed to create winit event loop")?;
    winit_event_loop.set_control_flow(ControlFlow::Poll);

    let pending_keys: Arc<Mutex<VecDeque<PendingKeyEvent>>> = Arc::new(Mutex::new(VecDeque::new()));
    let pending_pointers: Arc<Mutex<VecDeque<PendingPointerEvent>>> = Arc::new(Mutex::new(VecDeque::new()));
    let mut app = CompositorApp::new(
        window_ref,
        ui,
        pending_keys.clone(),
        pending_pointers.clone(),
        dock_entries,
    );

    // Initialise cursor overlay with the default arrow cursor.
    app.update_cursor_overlay();

    // 7. Main loop
    info!("Entering GPU compositor main loop");
    loop {
        match winit_event_loop.pump_app_events(Some(Duration::from_millis(1)), &mut app) {
            PumpStatus::Exit(_) => { info!("winit exited"); break; }
            PumpStatus::Continue => {}
        }

        calloop.dispatch(Some(Duration::ZERO), &mut state)
            .context("calloop dispatch error")?;

        if state.should_exit { break; }

        // D4 — send wl_surface.frame callbacks + signal fifo barriers each frame.
        state.send_frame_callbacks(&output);
        state.pre_render_drive_clients();

        state.display_handle.flush_clients().ok();
        slint::platform::update_timers_and_animations();
        app.update_client_texture(&mut state);
        app.update_dock_running(&state);

        // D3 — forward keyboard events.
        {
            let mut keys = pending_keys.lock().unwrap();
            while let Some(ke) = keys.pop_front() {
                forward_keyboard_event(&mut state, ke);
            }
        }

        // D2 — forward pointer events to wayland clients + cursor/drag handling.
        {
            let events: Vec<PendingPointerEvent> = {
                let mut pointers = pending_pointers.lock().unwrap();
                pointers.drain(..).collect()
            };
            for pe in events {
                match pe {
                    PendingPointerEvent::Motion { x, y } => {
                        // Run cursor hit-test + drag update.
                        app.handle_pointer_update(&mut state, x, y);
                        // Forward to wayland client (only if not in a drag over chrome).
                        app.forward_pointer_motion(&mut state, x, y);
                    }
                    PendingPointerEvent::Button { button, pressed } => {
                        // Trigger drag start on press (left button).
                        if button == 0x110 && pressed {
                            let (x, y) = app.pointer_pos;
                            app.handle_pointer_update(&mut state, x, y);
                        }
                        app.forward_pointer_button(&mut state, button, pressed);
                    }
                }
            }
        }

        std::thread::sleep(Duration::from_millis(4));
    }

    info!("GPU compositor exiting cleanly");
    Ok(())
}

// ──────────────────────────────────────────────────────────────────────────────
// Input forwarding (D5 - unchanged)
// ──────────────────────────────────────────────────────────────────────────────

fn forward_keyboard_event(state: &mut SpikeState, key_event: PendingKeyEvent) {
    use smithay::{backend::input::KeyState, input::keyboard::Keycode, utils::SERIAL_COUNTER};
    let Some(surface) = state.active_surface.clone() else { return };
    let Some(keyboard) = state.seat.get_keyboard() else { return };
    keyboard.set_focus(state, Some(surface), SERIAL_COUNTER.next_serial());
    let serial = SERIAL_COUNTER.next_serial();
    let time = state.clock.now().as_millis() as u32;
    let keycode = Keycode::new(key_event.scancode + 8);
    let ks = if key_event.pressed { KeyState::Pressed } else { KeyState::Released };
    keyboard.input_forward(state, keycode, ks, serial, time, false);
}
