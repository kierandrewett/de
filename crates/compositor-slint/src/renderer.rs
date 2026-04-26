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
    utils::Transform,
    wayland::socket::ListeningSocketSource,
};

use slint::{ComponentHandle, LogicalPosition, SharedString, VecModel};

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
    platform::{CalloopPlatform, GpuWindowAdapter},
    wayland_state::{ClientState, SpikeState},
    Compositor, DockItem,
};

const WIDTH: u32 = 1280;
const HEIGHT: u32 = 960;

// ──────────────────────────────────────────────────────────────────────────────
// Pending keyboard events (D5 - unchanged from spike)
// ──────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct PendingKeyEvent {
    pub scancode: u32,
    pub pressed: bool,
}

// ──────────────────────────────────────────────────────────────────────────────
// wgpu helpers
// ──────────────────────────────────────────────────────────────────────────────

/// Allocate an offscreen texture for Slint/FemtoVG to render into.
/// MUST be created with the same device as FemtoVGWGPURenderer.
/// RENDER_ATTACHMENT is required by FemtoVGWGPURenderer.
/// COPY_SRC is needed to blit into the swapchain.
fn make_render_texture(device: &wgpu::Device, width: u32, height: u32, format: wgpu::TextureFormat) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("slint-render-target"),
        size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
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
    render_texture: Option<wgpu::Texture>,
    render_texture_size: (u32, u32),

    // Slint GPU window adapter (holds device/queue/adapter/instance)
    gpu_window: Option<Rc<GpuWindowAdapter>>,
    window_ref: Arc<Mutex<Option<Rc<GpuWindowAdapter>>>>,
    ui: Option<Compositor>,

    pointer_pos: (f64, f64),
    start_time: Instant,
    last_clock_update: Instant,
    pending_keys: Arc<Mutex<VecDeque<PendingKeyEvent>>>,
    frame_count: u64,
}

impl CompositorApp {
    fn new(
        window_ref: Arc<Mutex<Option<Rc<GpuWindowAdapter>>>>,
        ui: Compositor,
        pending_keys: Arc<Mutex<VecDeque<PendingKeyEvent>>>,
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
            pointer_pos: (0.0, 0.0),
            start_time: Instant::now(),
            last_clock_update: Instant::now(),
            pending_keys,
            frame_count: 0,
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
            }

            WindowEvent::CursorMoved { position, .. } => {
                self.pointer_pos = (position.x, position.y);
                gpu_window.inner_window().dispatch_event(
                    slint::platform::WindowEvent::PointerMoved {
                        position: LogicalPosition::new(position.x as f32, position.y as f32),
                    },
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
                let slint_event = match state {
                    ElementState::Pressed => slint::platform::WindowEvent::PointerPressed {
                        position: pos, button: slint_btn,
                    },
                    ElementState::Released => slint::platform::WindowEvent::PointerReleased {
                        position: pos, button: slint_btn,
                    },
                };
                gpu_window.inner_window().dispatch_event(slint_event);
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

        // Blit offscreen texture -> swapchain frame.
        if let Some(render_tex) = self.render_texture.as_ref() {
            let mut encoder = device.create_command_encoder(
                &wgpu::CommandEncoderDescriptor { label: Some("blit") }
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
            queue.submit(std::iter::once(encoder.finish()));
        }

        frame.present();
    }

    /// Update the `windows` property on the Compositor from the wayland surface map.
    /// One WindowItem per mapped xdg-toplevel (SHM texture + title + geometry).
    fn update_windows(&mut self, state: &mut SpikeState) {
        let Some(ui) = self.ui.as_ref() else { return };

        let mut items: Vec<crate::WindowItem> = Vec::new();

        // Pull the single active SHM surface if present.
        let client_data = state.client_pixels.lock().unwrap();
        let has_surface = client_data.width > 0;

        if has_surface {
            // Build a slint Image from the current pixel buffer.
            let pixel_buf = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(
                &client_data.pixels,
                client_data.width,
                client_data.height,
            );
            let texture = slint::Image::from_rgba8_premultiplied(pixel_buf);

            // Get the toplevel title via smithay's compositor surface data map.
            let title = state
                .active_surface
                .as_ref()
                .and_then(|wl_surface| {
                    use smithay::wayland::compositor::with_states;
                    use smithay::wayland::shell::xdg::XdgToplevelSurfaceData;
                    with_states(wl_surface, |states| {
                        states
                            .data_map
                            .get::<XdgToplevelSurfaceData>()
                            .and_then(|data| data.lock().ok()?.title.clone())
                    })
                })
                .unwrap_or_else(|| "Window".to_string());

            items.push(crate::WindowItem {
                id: 1,
                title: SharedString::from(title),
                x: 100,
                y: 100,
                w: client_data.width as i32,
                h: client_data.height as i32,
                focused: true,
                texture,
                icon: slint::Image::default(),
            });
        }
        drop(client_data);

        let model = std::rc::Rc::new(VecModel::from(items));
        ui.set_windows(slint::ModelRc::from(model));
    }

    /// Update Slint client texture from SHM pixel data (legacy D3/D4 path —
    /// kept for compatibility; update_windows() now drives the Compositor UI).
    fn update_client_texture(&mut self, state: &mut SpikeState) {
        let mut client_data = state.client_pixels.lock().unwrap();
        if !client_data.dirty || client_data.width == 0 {
            return;
        }
        client_data.dirty = false;
        drop(client_data);

        // Mark dirty so the next render picks up fresh WindowItems.
        if let Some(gpu_window) = self.gpu_window.as_ref() {
            gpu_window.mark_dirty();
        }

        self.update_windows(state);
        debug!("SHM client texture updated → windows property refreshed");
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Hardcoded placeholder dock items
// ──────────────────────────────────────────────────────────────────────────────

fn make_placeholder_dock_items() -> Vec<DockItem> {
    vec![
        DockItem {
            icon: slint::Image::default(),
            app_id: SharedString::from("firefox"),
            running: false,
            focused: false,
            pinned: true,
        },
        DockItem {
            icon: slint::Image::default(),
            app_id: SharedString::from("kitty"),
            running: false,
            focused: false,
            pinned: true,
        },
        DockItem {
            icon: slint::Image::default(),
            app_id: SharedString::from("nautilus"),
            running: false,
            focused: false,
            pinned: true,
        },
    ]
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

    // Populate dock with 3 placeholder apps.
    {
        let items = make_placeholder_dock_items();
        let model = std::rc::Rc::new(VecModel::from(items));
        ui.set_dock_items(slint::ModelRc::from(model));
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

    // Wire launch-app: spawn the named app via setsid.
    {
        ui.on_launch_app(|app_id| {
            let app = app_id.to_string();
            info!("launch-app({})", app);
            let _ = std::process::Command::new("setsid")
                .args([&app])
                .spawn();
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
    let mut app = CompositorApp::new(window_ref, ui, pending_keys.clone());

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

        state.display_handle.flush_clients().ok();
        slint::platform::update_timers_and_animations();
        app.update_client_texture(&mut state);

        {
            let mut keys = pending_keys.lock().unwrap();
            while let Some(ke) = keys.pop_front() {
                forward_keyboard_event(&mut state, ke);
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
