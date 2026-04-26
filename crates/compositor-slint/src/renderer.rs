//! Main compositor loop: winit window + calloop event loop + Slint GPU rendering.
//!
//! GPU MIGRATION: Replaces softbuffer + SoftwareRenderer with:
//!   - wgpu surface (swapchain) on the winit window
//!   - FemtoVGWGPURenderer::render_to_texture() each frame
//!   - offscreen wgpu::Texture blit to swapchain via copy_texture_to_texture
//!
//! DAMAGE TRACKING (D4): render_to_texture() only called when Slint has pending
//! changes. Idle desktop = 0 GPU renders (last frame stays presented).
//!
//! SHM CLIENT BUFFERS (D3): CPU memcpy into SharedPixelBuffer -> Slint Image.
//! FemtoVG uploads to GPU on next render pass. Full wgpu::Texture per surface
//! path (queue.write_texture) is possible but not implemented — ~1 week extra.
//!
//! DMA-BUF BLOCKER: wgpu 28.0.0 lacks stable DMA-BUF import on Linux
//! (wgpu_hal::Api::texture_from_raw_image is not stabilised). SHM-only for now.

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

use slint::{ComponentHandle, LogicalPosition};

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
    CompositorUI,
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
/// format must be Rgba8Unorm (FemtoVG requirement, matches swapchain).
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
        // Fallback for Vulkan on some drivers — we'll use Bgra8Unorm for render tex too.
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

    // wgpu resources — all Option<> because they are created in resumed()
    wgpu_surface: Option<wgpu::Surface<'static>>,
    wgpu_device: Option<wgpu::Device>,
    wgpu_queue: Option<wgpu::Queue>,
    wgpu_adapter: Option<wgpu::Adapter>,
    swapchain_format: Option<wgpu::TextureFormat>,
    // Offscreen texture that Slint/FemtoVG renders into each frame
    render_texture: Option<wgpu::Texture>,
    render_texture_size: (u32, u32),

    // Slint GPU window adapter
    gpu_window: Option<Rc<GpuWindowAdapter>>,
    window_ref: Arc<Mutex<Option<Rc<GpuWindowAdapter>>>>,
    ui: Option<CompositorUI>,

    pointer_pos: (f64, f64),
    start_time: Instant,
    last_clock_update: Instant,
    pending_keys: Arc<Mutex<VecDeque<PendingKeyEvent>>>,
}

impl CompositorApp {
    fn new(
        window_ref: Arc<Mutex<Option<Rc<GpuWindowAdapter>>>>,
        ui: CompositorUI,
        pending_keys: Arc<Mutex<VecDeque<PendingKeyEvent>>>,
    ) -> Self {
        Self {
            window: None,
            wgpu_surface: None,
            wgpu_device: None,
            wgpu_queue: None,
            wgpu_adapter: None,
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
        }
    }

    /// Return an offscreen texture of the right size, (re)creating if size changed.
    /// Format is taken from the swapchain format (Rgba8Unorm or Bgra8Unorm).
    fn get_render_texture(&mut self, width: u32, height: u32) -> &wgpu::Texture {
        if self.render_texture.is_none() || self.render_texture_size != (width, height) {
            let device = self.wgpu_device.as_ref().unwrap();
            let format = self.swapchain_format.unwrap_or(wgpu::TextureFormat::Rgba8Unorm);
            self.render_texture = Some(make_render_texture(device, width, height, format));
            self.render_texture_size = (width, height);
            debug!("(Re)created render texture {}x{} {:?}", width, height, format);
        }
        self.render_texture.as_ref().unwrap()
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

        // Create wgpu surface from the window raw handle.
        // SAFETY: window is kept alive in self.window for the program lifetime.
        let instance_for_surface = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends: wgpu::Backends::all(),
            ..Default::default()
        });
        let surface = unsafe {
            use raw_window_handle::{HasDisplayHandle, HasWindowHandle};
            let target = wgpu::SurfaceTargetUnsafe::RawHandle {
                raw_display_handle: window.display_handle().unwrap().as_raw(),
                raw_window_handle: window.window_handle().unwrap().as_raw(),
            };
            instance_for_surface.create_surface_unsafe(target)
                .expect("create_surface_unsafe failed")
        };
        // Extend to 'static: safe because window lives in self.window for the program.
        let surface: wgpu::Surface<'static> = unsafe {
            std::mem::transmute::<wgpu::Surface<'_>, wgpu::Surface<'static>>(surface)
        };

        // Request a compatible adapter for this surface.
        let adapter = pollster::block_on(instance_for_surface.request_adapter(
            &wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
            },
        ))
        .expect("no wgpu adapter compatible with window surface");

        // Create device + queue for the surface adapter.
        let (surface_device, surface_queue) = pollster::block_on(adapter.request_device(
            &wgpu::DeviceDescriptor {
                label: Some("compositor-slint-surface"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::downlevel_webgl2_defaults()
                    .using_resolution(adapter.limits()),
                ..Default::default()
            },
        ))
        .expect("failed to create surface device");

        let format = configure_surface(&surface, &adapter, &surface_device, WIDTH, HEIGHT);
        info!("wgpu swapchain ready, format={:?}", format);

        // Retrieve the Slint GPU window adapter created during CalloopPlatform setup.
        let gpu_window = self.window_ref.lock().unwrap().clone()
            .expect("Slint GPU window adapter should exist after CompositorUI::new()");
        gpu_window.resize(WIDTH, HEIGHT);

        if let Some(ui) = &self.ui {
            ui.window().show().ok();
        }

        self.window = Some(window);
        self.wgpu_surface = Some(surface);
        self.wgpu_adapter = Some(adapter);
        self.wgpu_device = Some(surface_device);
        self.wgpu_queue = Some(surface_queue);
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
                if let (Some(surface), Some(adapter), Some(device)) = (
                    self.wgpu_surface.as_ref(),
                    self.wgpu_adapter.as_ref(),
                    self.wgpu_device.as_ref(),
                ) {
                    let fmt = configure_surface(surface, adapter, device, w, h);
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

        // Update clock (marks adapter dirty via set_clock_text -> request_redraw)
        let now = Instant::now();
        if now.duration_since(self.last_clock_update) >= Duration::from_secs(1) {
            self.last_clock_update = now;
            let elapsed = now.duration_since(self.start_time);
            let secs = elapsed.as_secs();
            ui.set_clock_text(slint::SharedString::from(format!(
                "{:02}:{:02}:{:02}", secs / 3600, (secs % 3600) / 60, secs % 60
            )));
        }

        let size = gpu_window.get_size();
        let (w, h) = (size.width.max(1), size.height.max(1));

        // D4 DAMAGE TRACKING: only re-render if Slint has changes.
        if gpu_window.has_pending_redraw() {
            let render_tex = self.get_render_texture(w, h);
            if let Err(e) = gpu_window.render_to_texture(render_tex) {
                warn!("render_to_texture failed: {}", e);
                return;
            }
            debug!("GPU render: {}x{} (dirty)", w, h);
        }

        let Some(surface) = self.wgpu_surface.as_ref() else { return };
        let Some(device) = self.wgpu_device.as_ref() else { return };
        let Some(queue) = self.wgpu_queue.as_ref() else { return };

        // Acquire swapchain frame.
        let frame = match surface.get_current_texture() {
            Ok(f) => f,
            Err(wgpu::SurfaceError::Outdated) => return,
            Err(e) => { warn!("swapchain: {}", e); return; }
        };

        // Blit offscreen Rgba8Unorm -> swapchain frame.
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

    /// Update Slint client texture from SHM pixel data (D3/D4).
    /// SHM PATH: CPU memcpy into SharedPixelBuffer -> Slint Image.
    fn update_client_texture(&mut self, state: &mut SpikeState) {
        let mut client_data = state.client_pixels.lock().unwrap();
        if !client_data.dirty || client_data.width == 0 {
            return;
        }
        let (w, h) = (client_data.width, client_data.height);
        let pixels = client_data.pixels.clone();
        client_data.dirty = false;
        drop(client_data);

        let Some(ui) = self.ui.as_ref() else { return };
        let Some(gpu_window) = self.gpu_window.as_ref() else { return };

        let pixel_buf =
            slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(&pixels, w, h);
        let image = slint::Image::from_rgba8_premultiplied(pixel_buf);
        ui.set_client_texture(image);
        ui.set_client_visible(true);
        ui.set_client_w(w as i32);
        ui.set_client_h(h as i32);
        gpu_window.mark_dirty();
        debug!("SHM client texture updated: {}x{}", w, h);
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Main entry point
// ──────────────────────────────────────────────────────────────────────────────

pub fn run() -> Result<()> {
    // 1. calloop
    let mut calloop = EventLoop::<SpikeState>::try_new()
        .context("failed to create calloop event loop")?;
    let loop_signal = calloop.get_signal();

    // 2. wgpu instance/device/queue for Slint platform (headless, no surface yet)
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
    info!("wgpu adapter (Slint): {}", slint_adapter.get_info().name);

    let (slint_device, slint_queue) = pollster::block_on(slint_adapter.request_device(
        &wgpu::DeviceDescriptor {
            label: Some("compositor-slint-platform"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::downlevel_webgl2_defaults()
                .using_resolution(slint_adapter.limits()),
            ..Default::default()
        },
    ))
    .context("failed to create wgpu device for Slint platform")?;

    // 3. Slint GPU platform
    let platform = CalloopPlatform::new(
        loop_signal.clone(),
        slint_instance,
        slint_device,
        slint_queue,
        WIDTH,
        HEIGHT,
    );
    let window_ref = platform.window.clone();

    slint::platform::set_platform(Box::new(platform))
        .context("failed to set Slint GPU platform")?;

    // Create Slint UI (triggers create_window_adapter -> FemtoVGWGPURenderer::new)
    let ui = CompositorUI::new().context("failed to create Slint UI")?;
    ui.set_clock_text(slint::SharedString::from("00:00:00"));

    {
        let signal = loop_signal.clone();
        ui.on_quit_clicked(move || {
            info!("Quit clicked");
            signal.stop();
        });
    }

    let last_client_click: Arc<Mutex<Option<(f32, f32)>>> = Arc::new(Mutex::new(None));
    {
        let click_ref = last_client_click.clone();
        ui.on_client_clicked(move |x, y| {
            *click_ref.lock().unwrap() = Some((x, y));
        });
    }

    info!("Slint GPU platform ready (D1/D2)");

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

    info!("Wayland socket ready (D3)");

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

        if let Some((cx, cy)) = last_client_click.lock().unwrap().take() {
            forward_pointer_click(&mut state, cx as f64, cy as f64);
        }
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

fn forward_pointer_click(state: &mut SpikeState, rel_x: f64, rel_y: f64) {
    use smithay::utils::SERIAL_COUNTER;
    let Some(surface) = state.active_surface.clone() else { return };
    let pointer = state.seat.get_pointer().unwrap();
    let serial = SERIAL_COUNTER.next_serial();
    let time = state.clock.now().as_millis() as u32;
    pointer.motion(state, Some((surface.clone(), (rel_x, rel_y).into())),
        &smithay::input::pointer::MotionEvent {
            location: (rel_x + 100.0, rel_y + 100.0).into(), serial, time,
        });
    pointer.button(state, &smithay::input::pointer::ButtonEvent {
        button: 0x110, state: smithay::backend::input::ButtonState::Pressed, serial, time,
    });
    pointer.frame(state);
    let s2 = SERIAL_COUNTER.next_serial();
    pointer.button(state, &smithay::input::pointer::ButtonEvent {
        button: 0x110, state: smithay::backend::input::ButtonState::Released, serial: s2, time: time + 50,
    });
    pointer.frame(state);
}

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
