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
//!
//! WM (Wave 1A): WindowManager drives focus stack, z-order, animations.

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
        wayland_server::{
            protocol::wl_surface::WlSurface,
            Display,
        },
    },
    utils::{Transform, SERIAL_COUNTER},
    wayland::socket::ListeningSocketSource,
};

use slint::{ComponentHandle, LogicalPosition, Model, SharedString, VecModel};

use winit::{
    application::ApplicationHandler,
    dpi::LogicalSize,
    event::{ElementState, KeyEvent, MouseButton, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop as WinitEventLoop},
    keyboard::{KeyCode, PhysicalKey},
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
    theme::{ThemeMode, ThemeState},
    wallpaper,
    wayland_state::{ClientState, SpikeState},
    wm::WindowManager,
    Compositor, DockItem, TokenMode,
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
fn configure_surface(
    surface: &wgpu::Surface<'static>,
    adapter: &wgpu::Adapter,
    device: &wgpu::Device,
    width: u32,
    height: u32,
) -> wgpu::TextureFormat {
    let caps = surface.get_capabilities(adapter);
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
// Alt-tab key state
// ──────────────────────────────────────────────────────────────────────────────

/// Shared state for alt-tab cycling between the winit handler and the main loop.
#[derive(Debug, Default)]
struct AltTabState {
    /// Alt key is currently held down.
    alt_held: bool,
    /// Tab was pressed while alt was held (cycling has started).
    cycling: bool,
}

// ──────────────────────────────────────────────────────────────────────────────
// Application state
// ──────────────────────────────────────────────────────────────────────────────

struct CompositorApp {
    window: Option<Rc<Window>>,

    wgpu_surface: Option<wgpu::Surface<'static>>,
    swapchain_format: Option<wgpu::TextureFormat>,
    render_texture: Option<wgpu::Texture>,
    render_texture_size: (u32, u32),

    gpu_window: Option<Rc<GpuWindowAdapter>>,
    window_ref: Arc<Mutex<Option<Rc<GpuWindowAdapter>>>>,
    ui: Option<Compositor>,

    chrome_shader: Option<ChromeShader>,

    // Theme state — mode_t / per-window focus_t animations.
    theme: ThemeState,

    pointer_pos: (f64, f64),
    start_time: Instant,
    last_clock_update: Instant,
    pending_keys: Arc<Mutex<VecDeque<PendingKeyEvent>>>,
    pending_pointers: Arc<Mutex<VecDeque<PendingPointerEvent>>>,
    frame_count: u64,

    // Hotkey state — Super key held tracking for Super+T theme toggle.
    super_held: bool,

    // Dock state.
    dock_entries: Vec<ResolvedDockEntry>,
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

    // ── Window manager (Wave 1A) ─────────────────────────────────────────────
    wm: WindowManager,
    /// Alt-tab key state.
    alt_tab: AltTabState,

    // Pending WM actions from Slint callbacks (processed in the main loop).
    pending_close:    Arc<Mutex<VecDeque<i32>>>,
    pending_minimize: Arc<Mutex<VecDeque<i32>>>,
    pending_maximize: Arc<Mutex<VecDeque<i32>>>,
    pending_activate: Arc<Mutex<VecDeque<i32>>>,
    /// Pending alt-tab step requests from the winit key handler.
    pending_alt_tab_step:   Arc<Mutex<u32>>,
    /// Pending alt-tab commit (Alt released).
    pending_alt_tab_commit: Arc<Mutex<bool>>,
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
            theme: ThemeState::new(),
            super_held: false,
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
            wm: WindowManager::new(WIDTH as i32, HEIGHT as i32),
            alt_tab: AltTabState::default(),
            pending_close:    Arc::new(Mutex::new(VecDeque::new())),
            pending_minimize: Arc::new(Mutex::new(VecDeque::new())),
            pending_maximize: Arc::new(Mutex::new(VecDeque::new())),
            pending_activate: Arc::new(Mutex::new(VecDeque::new())),
            pending_alt_tab_step:   Arc::new(Mutex::new(0)),
            pending_alt_tab_commit: Arc::new(Mutex::new(false)),
        }
    }

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

        let gpu_window = self.window_ref.lock().unwrap().clone()
            .expect("Slint GPU window adapter should exist after Compositor::new()");

        let surface = unsafe {
            use raw_window_handle::{HasDisplayHandle, HasWindowHandle};
            let target = wgpu::SurfaceTargetUnsafe::RawHandle {
                raw_display_handle: window.display_handle().unwrap().as_raw(),
                raw_window_handle: window.window_handle().unwrap().as_raw(),
            };
            gpu_window.wgpu_instance.create_surface_unsafe(target)
                .expect("create_surface_unsafe failed")
        };
        let surface: wgpu::Surface<'static> = unsafe {
            std::mem::transmute::<wgpu::Surface<'_>, wgpu::Surface<'static>>(surface)
        };

        let format = configure_surface(
            &surface,
            &gpu_window.wgpu_adapter,
            &gpu_window.wgpu_device,
            WIDTH,
            HEIGHT,
        );
        info!("wgpu swapchain ready, format={:?} (shared device)", format);

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

        info!("GPU window created, FemtoVG renderer active");
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
                self.render_texture = None;
                self.wm.output_w = w as i32;
                self.wm.output_h = h as i32;
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

                // On press, update WM focus based on pointer position.
                // Actual focus update happens in the main loop via pending_pointers.
                let evdev_btn = winit_button_to_evdev(button);
                self.pending_pointers.lock().unwrap().push_back(
                    PendingPointerEvent::Button { button: evdev_btn, pressed }
                );
            }

            WindowEvent::KeyboardInput { event: key_event, .. } => {
                let scancode = key_event.physical_key.to_scancode().unwrap_or(0);
                let pressed = key_event.state == ElementState::Pressed;

                // ── Super+T → toggle light/dark theme ─────────────────────
                // Scancode 125 = KEY_LEFTMETA (Super/Win key)
                // Scancode 126 = KEY_RIGHTMETA
                // Scancode 20  = KEY_T
                match scancode {
                    125 | 126 => { self.super_held = pressed; }
                    20 if pressed && self.super_held => {
                        // Toggle theme mode and update Slint global.
                        self.theme.toggle_mode();
                        self.apply_theme_to_slint();
                        debug!("Super+T: toggled theme to {:?}", self.theme.current_mode);
                    }
                    _ => {}
                }

                if scancode > 0 {
                    self.pending_keys.lock().unwrap().push_back(PendingKeyEvent {
                        scancode,
                        pressed,
                    });
                }

                // Handle Alt-Tab cycling in the winit handler so we get
                // immediate key state without waiting for the calloop round-trip.
                self.handle_alt_tab_key(&key_event);
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
    /// Handle alt/tab key events for the alt-tab switcher.
    fn handle_alt_tab_key(&mut self, key_event: &KeyEvent) {
        let pressed = key_event.state == ElementState::Pressed;
        match key_event.physical_key {
            PhysicalKey::Code(KeyCode::AltLeft) | PhysicalKey::Code(KeyCode::AltRight) => {
                self.alt_tab.alt_held = pressed;
                if !pressed && self.alt_tab.cycling {
                    // Alt released: commit alt-tab selection.
                    self.alt_tab.cycling = false;
                    *self.pending_alt_tab_commit.lock().unwrap() = true;
                }
            }
            PhysicalKey::Code(KeyCode::Tab) => {
                if pressed && self.alt_tab.alt_held {
                    // Tab pressed while alt held: step alt-tab.
                    self.alt_tab.cycling = true;
                    let mut steps = self.pending_alt_tab_step.lock().unwrap();
                    *steps += 1;
                }
            }
            _ => {}
        }
    }

    /// GPU render: damage-tracked Slint render + swapchain blit.
    fn render_frame(&mut self) {
        let gpu_window = match self.gpu_window.clone() { Some(w) => w, None => return };
        let Some(ui) = self.ui.as_ref() else { return };

        let now = Instant::now();
        if now.duration_since(self.last_clock_update) >= Duration::from_secs(1) {
            self.last_clock_update = now;
            let time_str = chrono::Local::now().format("%H:%M:%S").to_string();
            ui.set_clock_text(SharedString::from(time_str));
        }

        let size = gpu_window.get_size();
        let (w, h) = (size.width.max(1), size.height.max(1));

        if gpu_window.has_pending_redraw() {
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

        let frame = match surface.get_current_texture() {
            Ok(f) => f,
            Err(wgpu::SurfaceError::Outdated) => return,
            Err(e) => { warn!("swapchain: {}", e); return; }
        };

        if let Some(render_tex) = self.render_texture.as_ref() {
            let mut encoder = device.create_command_encoder(
                &wgpu::CommandEncoderDescriptor { label: Some("blit+chrome") }
            );

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

            if let Some(chrome) = self.chrome_shader.as_mut() {
                let chrome_windows: Vec<WindowChromeParams> = if let Some(ui) = self.ui.as_ref() {
                    let model = ui.get_windows();
                    let len = model.row_count();
                    let mode_t = self.theme.mode_t;
                    (0..len)
                        .map(|i| {
                            let item = model.row_data(i).unwrap();
                            let focus_t = self.theme.window_focus_t(item.id);
                            WindowChromeParams {
                                x: item.x as f32,
                                y: item.y as f32,
                                w: item.w as f32,
                                h: item.h as f32,
                                active: item.focused,
                                focus_t,
                                mode_t,
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

    /// Build the Slint `WindowItem` list from `WM` state + toplevel pixel buffers,
    /// then push it to the UI.  Called every frame when client textures are dirty
    /// or when WM state changes.
    fn update_windows(&mut self, state: &mut SpikeState) {
        use smithay::wayland::compositor::with_states;
        use smithay::wayland::shell::xdg::XdgToplevelSurfaceData;

        let Some(ui) = self.ui.as_ref() else { return };

        // Tick the WM animations.
        self.wm.tick_auto();

        // Sweep windows whose close animation has finished.
        let closed_surfaces = self.wm.sweep_closed();
        for surf in &closed_surfaces {
            // Remove from SpikeState toplevel list.
            state.toplevels.retain(|t| &t.surface != surf);
            debug!("WM: swept closed window for surface");
        }
        // If we swept anything, the focus might need updating.
        if !closed_surfaces.is_empty() {
            state.active_surface = self.wm.focused_surface();
            if let Some(surface) = &state.active_surface {
                if let Some(kb) = state.seat.get_keyboard() {
                    kb.set_focus(state, Some(surface.clone()), SERIAL_COUNTER.next_serial());
                }
            }
        }

        // Build Slint items sorted by z_order (back to front).
        let sorted_wins = self.wm.windows_sorted();

        let mut items: Vec<crate::WindowItem> = Vec::new();

        for win in &sorted_wins {
            // Skip minimized windows once their animation has settled.
            if win.minimized && win.anim.is_settled() {
                continue;
            }
            // Skip closing windows (they are still animated by is_visible logic).
            // They remain until `sweep_closed` removes them, but we still render them.

            // Find the corresponding ToplevelInfo for the pixel buffer.
            let toplevel = state.toplevels.iter().find(|t| t.surface == win.surface);
            let Some(toplevel) = toplevel else { continue };

            let client_data = toplevel.pixels.lock().unwrap();
            if client_data.width == 0 {
                drop(client_data);
                continue;
            }

            let pixel_buf = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(
                &client_data.pixels,
                client_data.width,
                client_data.height,
            );
            let texture = slint::Image::from_rgba8_premultiplied(pixel_buf);
            drop(client_data);

            // Get window title from xdg-toplevel surface data.
            // with_states<F, T>(...) returns T; closure returns Option<String>.
            let title: String = with_states(&win.surface, |states| -> Option<String> {
                states
                    .data_map
                    .get::<XdgToplevelSurfaceData>()
                    .and_then(|data| data.lock().ok()?.title.clone())
            })
            .unwrap_or_else(|| {
                if win.title.is_empty() { "Window".to_string() } else { win.title.clone() }
            });

            // Drive the per-window focus_t spring (200 ms) so the chrome
            // shader crossfades active → inactive (and vice versa) smoothly.
            self.theme.set_window_focused(win.id, win.focused);

            items.push(crate::WindowItem {
                id: win.id,
                title: SharedString::from(title),
                x: win.anim.current_x(),
                y: win.anim.current_y(),
                w: win.anim.current_w(),
                h: win.anim.current_h(),
                focused: win.focused,
                texture,
                icon: slint::Image::default(),
                anim_opacity: win.anim.opacity.value_f32().clamp(0.0, 1.0),
                anim_scale: win.anim.scale.value_f32().clamp(0.0, 2.0),
                alt_tab_selected: win.alt_tab_selected,
            });
        }

        let model = std::rc::Rc::new(VecModel::from(items));
        ui.set_windows(slint::ModelRc::from(model));

        // Mark Slint dirty so the next render picks up the new list.
        if let Some(gpu_window) = self.gpu_window.as_ref() {
            gpu_window.mark_dirty();
        }
    }

    /// Poll all toplevels for dirty SHM buffers and update Slint if any changed.
    fn update_client_texture(&mut self, state: &mut SpikeState) {
        let mut any_dirty = false;

        for toplevel in state.toplevels.iter() {
            let mut client_data = toplevel.pixels.lock().unwrap();
            if client_data.dirty && client_data.width > 0 {
                client_data.dirty = false;
                any_dirty = true;
            }
        }

        {
            let mut client_data = state.client_pixels.lock().unwrap();
            if client_data.dirty && client_data.width > 0 {
                client_data.dirty = false;
                any_dirty = true;
            }
        }

        // Always update windows when animations are in flight (they need to tick).
        let animations_active = self.wm.windows.values().any(|w| !w.anim.is_settled());

        if any_dirty || animations_active {
            if let Some(gpu_window) = self.gpu_window.as_ref() {
                gpu_window.mark_dirty();
            }
            self.update_windows(state);
            if any_dirty {
                debug!("SHM client texture updated → windows property refreshed");
            }
        }
    }

    /// Process pending WM actions (close, minimize, maximize, activate) queued
    /// by Slint callbacks.  Must be called from the main loop where SpikeState
    /// is available.
    fn process_wm_actions(&mut self, state: &mut SpikeState) {
        // Close
        let close_ids: Vec<i32> = {
            let mut q = self.pending_close.lock().unwrap();
            q.drain(..).collect()
        };
        for id in close_ids {
            info!("WM: close-window({})", id);
            // Find the surface for this id.
            let surface = self.wm.windows.values()
                .find(|w| w.id == id)
                .map(|w| w.surface.clone());
            if let Some(surf) = surface {
                // Send xdg_toplevel.close to the client.
                self.send_xdg_close(&surf, state);
                // Start close animation.
                self.wm.begin_close_by_id(id);
            }
            self.update_focused_surface(state);
        }

        // Minimize
        let minimize_ids: Vec<i32> = {
            let mut q = self.pending_minimize.lock().unwrap();
            q.drain(..).collect()
        };
        for id in minimize_ids {
            info!("WM: minimize-window({})", id);
            self.wm.minimize_by_id(id);
            self.update_focused_surface(state);
        }

        // Maximize
        let maximize_ids: Vec<i32> = {
            let mut q = self.pending_maximize.lock().unwrap();
            q.drain(..).collect()
        };
        for id in maximize_ids {
            info!("WM: maximize-window({})", id);
            self.wm.toggle_maximize_by_id(id);
            // Send configure to client with new size.
            let (new_w, new_h) = self.wm.windows.values()
                .find(|w| w.id == id)
                .map(|w| (w.w, w.h))
                .unwrap_or((800, 600));
            let surface = self.wm.windows.values()
                .find(|w| w.id == id)
                .map(|w| w.surface.clone());
            if let Some(surf) = surface {
                self.send_configure(&surf, new_w, new_h, state);
            }
        }

        // Activate (raise and focus)
        let activate_ids: Vec<i32> = {
            let mut q = self.pending_activate.lock().unwrap();
            q.drain(..).collect()
        };
        for id in activate_ids {
            info!("WM: activate-window({})", id);
            self.wm.focus_by_id(id);
            self.update_focused_surface(state);
        }

        // Alt-tab steps
        let steps = {
            let mut s = self.pending_alt_tab_step.lock().unwrap();
            let v = *s;
            *s = 0;
            v
        };
        if steps > 0 {
            if self.wm.alt_tab_idx.is_none() {
                self.wm.alt_tab_start();
            } else {
                for _ in 0..steps {
                    self.wm.alt_tab_next();
                }
            }
            // Mark Slint dirty to show the selection ring.
            if let Some(gpu_window) = self.gpu_window.as_ref() {
                gpu_window.mark_dirty();
            }
        }

        // Alt-tab commit (alt released)
        let commit = {
            let mut c = self.pending_alt_tab_commit.lock().unwrap();
            let v = *c;
            *c = false;
            v
        };
        if commit {
            self.wm.alt_tab_commit();
            self.update_focused_surface(state);
        }
    }

    /// Update `state.active_surface` and keyboard focus from the WM focused window.
    fn update_focused_surface(&self, state: &mut SpikeState) {
        state.active_surface = self.wm.focused_surface();
        if let Some(surface) = &state.active_surface {
            if let Some(kb) = state.seat.get_keyboard() {
                kb.set_focus(state, Some(surface.clone()), SERIAL_COUNTER.next_serial());
            }
        }
    }

    /// Send an xdg_toplevel.close request to the client.
    fn send_xdg_close(&self, surface: &WlSurface, state: &mut SpikeState) {
        for toplevel in &state.toplevels {
            if &toplevel.surface == surface {
                debug!("WM: sending xdg_toplevel.close to client");
                toplevel.toplevel.send_close();
                break;
            }
        }
    }

    /// Send xdg_toplevel configure with a new size to the client.
    fn send_configure(&self, surface: &WlSurface, w: i32, h: i32, state: &mut SpikeState) {
        for toplevel in &state.toplevels {
            if &toplevel.surface == surface {
                debug!("WM: sending xdg_toplevel configure {}x{}", w, h);
                toplevel.toplevel.with_pending_state(|s| {
                    s.size = Some((w, h).into());
                });
                toplevel.toplevel.send_configure();
                break;
            }
        }
    }

    /// Forward a pointer motion event to the wayland client whose window is under the pointer.
    fn forward_pointer_motion(&self, state: &mut SpikeState, x: f64, y: f64) {
        use smithay::input::pointer::MotionEvent;
        use smithay::utils::Point;

        let pointer = match state.seat.get_pointer() {
            Some(p) => p,
            None => return,
        };

        let serial = SERIAL_COUNTER.next_serial();
        let time = state.clock.now().as_millis() as u32;

        // Use WM surface_under for correct z-order hit testing.
        let hit = self.wm.surface_under(x, y);

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

    /// Forward a pointer button event, and on left-press update WM focus.
    fn forward_pointer_button(&mut self, state: &mut SpikeState, button: u32, pressed: bool) {
        use smithay::input::pointer::ButtonEvent;

        let pointer = match state.seat.get_pointer() {
            Some(p) => p,
            None => return,
        };

        // On left press: update WM focus for the window under the cursor.
        if button == 0x110 && pressed {
            let (x, y) = self.pointer_pos;
            if let Some(focused_surface) = self.wm.pointer_click_focus(x, y) {
                state.active_surface = Some(focused_surface.clone());
                if let Some(kb) = state.seat.get_keyboard() {
                    kb.set_focus(state, Some(focused_surface), SERIAL_COUNTER.next_serial());
                }
                // Mark dirty so the focused state updates in Slint.
                if let Some(gpu_window) = self.gpu_window.as_ref() {
                    gpu_window.mark_dirty();
                }
            }
        }

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
    fn update_dock_running(&mut self, state: &SpikeState) {
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

        let mut running_ids: std::collections::HashSet<String> =
            std::collections::HashSet::new();
        if let Some(ref app_id) = focused_app_id {
            running_ids.insert(app_id.clone());
        }

        if running_ids == self.last_running_ids {
            return;
        }
        self.last_running_ids = running_ids.clone();

        let Some(ui) = self.ui.as_ref() else { return };

        let items: Vec<DockItem> = self
            .dock_entries
            .iter()
            .map(|entry| {
                let app_id_str = entry.item.app_id.as_str();
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

    /// Push the current theme mode to the Slint `Theme` global, so all components
    /// that reference `Theme.mode` pick up the new value and begin their Slint
    /// `animate` crossfades simultaneously.
    ///
    /// This is called:
    ///   - On Super+T press (immediate toggle).
    ///   - Each frame during a theme animation so Slint's animate blocks are kept
    ///     in sync with the Rust-side mode_t tween.
    pub fn apply_theme_to_slint(&self) {
        if let Some(ui) = self.ui.as_ref() {
            let token_mode = match self.theme.current_mode {
                ThemeMode::Dark  => TokenMode::Dark,
                ThemeMode::Light => TokenMode::Light,
            };
            ui.set_theme_mode(token_mode);
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Real dock items from .desktop files
// ──────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ResolvedDockEntry {
    pub item: DockItem,
    pub exec: String,
}

pub fn load_dock_entries() -> Vec<ResolvedDockEntry> {
    let config = desktop::DockConfig::load();
    let mut entries = Vec::new();

    for pinned in &config.pinned {
        let app_id = &pinned.app_id;
        let info = desktop::resolve(app_id);

        let icon = match &info.icon {
            Some(path) => desktop::load_icon(path),
            None => {
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

    // 2. wgpu instance/adapter/device/queue — ONE set, shared.
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

    // 3. Slint GPU platform.
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

    let ui = Compositor::new().context("failed to create Compositor UI")?;

    ui.set_clock_text(SharedString::from(
        chrono::Local::now().format("%H:%M:%S").to_string()
    ));

    // Wallpaper.
    match wallpaper::load() {
        Some(img) => {
            info!("setting wallpaper image");
            ui.set_wallpaper(img);
        }
        None => {
            warn!("no wallpaper found on disk, using default #1e1e2e background");
        }
    }

    // Dock entries.
    let dock_entries = load_dock_entries();
    info!("loaded {} dock entries", dock_entries.len());
    {
        let items: Vec<DockItem> = dock_entries.iter().map(|e| e.item.clone()).collect();
        let model = std::rc::Rc::new(VecModel::from(items));
        ui.set_dock_items(slint::ModelRc::from(model));
    }

    // Launch-app callback.
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
            let exec_line = exec_map.get(&app).cloned().unwrap_or_else(|| {
                let info = desktop::resolve(&app);
                info.exec
            });
            info!("  exec: {}", exec_line);
            let _ = std::process::Command::new("setsid")
                .args(["-f", "sh", "-c", &exec_line])
                .spawn();
        });
    }

    // WM action callbacks — queue into the pending queues so the main loop
    // can process them with access to SpikeState.
    let pending_close: Arc<Mutex<VecDeque<i32>>> = Arc::new(Mutex::new(VecDeque::new()));
    let pending_minimize: Arc<Mutex<VecDeque<i32>>> = Arc::new(Mutex::new(VecDeque::new()));
    let pending_maximize: Arc<Mutex<VecDeque<i32>>> = Arc::new(Mutex::new(VecDeque::new()));
    let pending_activate: Arc<Mutex<VecDeque<i32>>> = Arc::new(Mutex::new(VecDeque::new()));

    {
        let q = pending_close.clone();
        ui.on_close_window(move |id| {
            q.lock().unwrap().push_back(id);
        });
    }
    {
        let q = pending_minimize.clone();
        ui.on_minimize_window(move |id| {
            q.lock().unwrap().push_back(id);
        });
    }
    {
        let q = pending_maximize.clone();
        ui.on_maximize_window(move |id| {
            q.lock().unwrap().push_back(id);
        });
    }
    {
        let q = pending_activate.clone();
        ui.on_activate_window(move |id| {
            q.lock().unwrap().push_back(id);
        });
    }

    // Panel callbacks.
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

    // 4. Wayland display + socket.
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

    // 5. Compositor state + virtual output.
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

    // 6. winit event loop.
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

    // Wire the pending WM-action queues from Slint callbacks into the app.
    app.pending_close    = pending_close;
    app.pending_minimize = pending_minimize;
    app.pending_maximize = pending_maximize;
    app.pending_activate = pending_activate;

    // 7. Main loop.
    info!("Entering GPU compositor main loop");
    loop {
        match winit_event_loop.pump_app_events(Some(Duration::from_millis(1)), &mut app) {
            PumpStatus::Exit(_) => { info!("winit exited"); break; }
            PumpStatus::Continue => {}
        }

        calloop.dispatch(Some(Duration::ZERO), &mut state)
            .context("calloop dispatch error")?;

        if state.should_exit { break; }

        state.send_frame_callbacks(&output);
        state.pre_render_drive_clients();

        state.display_handle.flush_clients().ok();
        slint::platform::update_timers_and_animations();

        // Sync WM with SpikeState toplevels: register new toplevels + handle destroyed ones.
        sync_new_toplevels(&mut app.wm, &mut state);

        // Process WM actions from Slint callbacks.
        app.process_wm_actions(&mut state);

        app.update_client_texture(&mut state);
        app.update_dock_running(&state);

        // Theme tick — advance mode_t + per-window focus_t animations.
        // If any animation is in flight, push the current mode to Slint so its
        // own animate blocks stay in sync, and mark the GPU adapter dirty.
        let theme_animating = app.theme.tick();
        if theme_animating {
            app.apply_theme_to_slint();
            if let Some(gpu_window) = app.gpu_window.as_ref() {
                gpu_window.mark_dirty();
            }
        }

        // Forward keyboard events.
        {
            let mut keys = pending_keys.lock().unwrap();
            while let Some(ke) = keys.pop_front() {
                forward_keyboard_event(&mut state, ke);
            }
        }

        // Forward pointer events to wayland clients + run cursor/drag handling.
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
// WM ↔ SpikeState synchronization
// ──────────────────────────────────────────────────────────────────────────────

/// Sync new and destroyed toplevels from `SpikeState` into the `WindowManager`.
/// Called each main-loop iteration.
fn sync_new_toplevels(wm: &mut WindowManager, state: &mut SpikeState) {
    use smithay::reexports::wayland_server::Resource;

    // Handle destroyed surfaces first: begin close animation.
    let destroyed: Vec<WlSurface> = state.destroyed_surfaces.drain(..).collect();
    for surf in &destroyed {
        let key = surf.id().protocol_id() as usize;
        if wm.windows.contains_key(&key) {
            debug!("WM: toplevel destroyed → begin close animation key={}", key);
            wm.begin_close(surf);
        }
    }

    // Register new toplevels.
    for toplevel in &state.toplevels {
        let key = toplevel.surface.id().protocol_id() as usize;
        if !wm.windows.contains_key(&key) {
            // Register the toplevel with the WM (starts open animation).
            let id = wm.add_window(toplevel.surface.clone());
            debug!("WM: synced new toplevel id={} key={}", id, key);
        }
    }

    // Sync focus: if SpikeState has an active_surface that isn't the WM focus, align them.
    if let Some(active) = &state.active_surface {
        let key = active.id().protocol_id() as usize;
        if wm.windows.contains_key(&key) {
            let focused = wm.windows.get(&key).map(|w| w.focused).unwrap_or(false);
            if !focused {
                wm.focus_surface(active);
            }
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Input forwarding
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
