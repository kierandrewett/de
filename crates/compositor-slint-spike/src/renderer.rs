//! Main compositor loop: winit window + calloop event loop + Slint rendering.
//!
//! DELIVERABLES 1, 2, 3, 4, 5 converge here.
//!
//! Loop strategy: winit 0.30 uses ApplicationHandler. We use pump_app_events()
//! (Linux platform extension) to run winit non-blockingly inside our own loop,
//! then pump calloop, then render with Slint.
//!
//! SPIKE: Single output, single client, ~1280×960.

use std::{
    collections::VecDeque,
    num::NonZeroU32,
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
            generic::Generic, timer::{TimeoutAction, Timer}, EventLoop, Interest,
            Mode as CalloopMode, PostAction,
        },
        wayland_server::Display,
    },
    utils::Transform,
    wayland::socket::ListeningSocketSource,
};

use slint::{
    platform::software_renderer::{PremultipliedRgbaColor, RepaintBufferType},
    ComponentHandle, LogicalPosition, PhysicalSize,
};

use winit::{
    application::ApplicationHandler,
    dpi::LogicalSize,
    event::{ElementState, KeyEvent, MouseButton, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop as WinitEventLoop},
    keyboard::PhysicalKey,
    platform::{
        pump_events::{EventLoopExtPumpEvents, PumpStatus},
        scancode::PhysicalKeyExtScancode,
    },
    window::{Window, WindowId},
};

/// Pending keyboard events to forward to the wayland client.
#[derive(Debug, Clone)]
pub struct PendingKeyEvent {
    pub scancode: u32,
    pub pressed: bool,
}

use crate::{
    platform::CalloopPlatform,
    wayland_state::{ClientState, SpikeState},
    CompositorUI,
};

const WIDTH: u32 = 1280;
const HEIGHT: u32 = 960;

// ──────────────────────────────────────────────────────────────────────────────
// Application state for winit ApplicationHandler
// ──────────────────────────────────────────────────────────────────────────────

struct CompositorApp {
    window: Option<Rc<Window>>,
    sb_context: Option<softbuffer::Context<Rc<Window>>>,
    sb_surface: Option<softbuffer::Surface<Rc<Window>, Rc<Window>>>,
    pixel_buf: Vec<PremultipliedRgbaColor>,

    // Slint window adapter
    slint_window: Option<Rc<slint::platform::software_renderer::MinimalSoftwareWindow>>,
    ui: Option<CompositorUI>,
    window_ref: Arc<Mutex<Option<Rc<slint::platform::software_renderer::MinimalSoftwareWindow>>>>,

    pointer_pos: (f64, f64),
    start_time: Instant,
    last_clock_update: Instant,

    /// Pending keyboard events to forward to wayland client (D5)
    pending_keys: Arc<Mutex<VecDeque<PendingKeyEvent>>>,
}

impl CompositorApp {
    fn new(
        window_ref: Arc<Mutex<Option<Rc<slint::platform::software_renderer::MinimalSoftwareWindow>>>>,
        ui: CompositorUI,
        pending_keys: Arc<Mutex<VecDeque<PendingKeyEvent>>>,
    ) -> Self {
        Self {
            window: None,
            sb_context: None,
            sb_surface: None,
            pixel_buf: vec![PremultipliedRgbaColor::default(); (WIDTH * HEIGHT) as usize],
            slint_window: None,
            ui: Some(ui),
            window_ref,
            pointer_pos: (0.0, 0.0),
            start_time: Instant::now(),
            last_clock_update: Instant::now(),
            pending_keys,
        }
    }
}

impl ApplicationHandler for CompositorApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }

        let attrs = Window::default_attributes()
            .with_title("Slint Compositor Spike")
            .with_inner_size(LogicalSize::new(WIDTH, HEIGHT));

        let window = Rc::new(
            event_loop
                .create_window(attrs)
                .expect("failed to create window"),
        );

        let sb_context = softbuffer::Context::new(window.clone())
            .expect("softbuffer context");
        let mut sb_surface =
            softbuffer::Surface::new(&sb_context, window.clone())
                .expect("softbuffer surface");
        sb_surface
            .resize(
                NonZeroU32::new(WIDTH).unwrap(),
                NonZeroU32::new(HEIGHT).unwrap(),
            )
            .expect("softbuffer resize");

        // Get slint window adapter
        let slint_window = self.window_ref.lock().unwrap().clone()
            .expect("Slint window adapter should exist after CompositorUI::new()");
        slint_window.set_size(PhysicalSize::new(WIDTH, HEIGHT));

        if let Some(ui) = &self.ui {
            ui.window().show().ok();
        }

        self.window = Some(window);
        self.sb_context = Some(sb_context);
        self.sb_surface = Some(sb_surface);
        self.slint_window = Some(slint_window);

        info!("Window created, Slint rendering active (Deliverable 2 ✓)");
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _id: WindowId,
        event: WindowEvent,
    ) {
        let slint_window = match self.slint_window.as_ref() {
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
                if let Some(sb) = self.sb_surface.as_mut() {
                    sb.resize(
                        NonZeroU32::new(w).unwrap(),
                        NonZeroU32::new(h).unwrap(),
                    )
                    .ok();
                }
                slint_window.set_size(PhysicalSize::new(w, h));
                self.pixel_buf
                    .resize((w * h) as usize, PremultipliedRgbaColor::default());
            }

            WindowEvent::CursorMoved { position, .. } => {
                self.pointer_pos = (position.x, position.y);
                slint_window.dispatch_event(slint::platform::WindowEvent::PointerMoved {
                    position: LogicalPosition::new(
                        position.x as f32,
                        position.y as f32,
                    ),
                });
            }

            WindowEvent::MouseInput { state, button, .. } => {
                let slint_btn = match button {
                    MouseButton::Left => slint::platform::PointerEventButton::Left,
                    MouseButton::Right => slint::platform::PointerEventButton::Right,
                    MouseButton::Middle => slint::platform::PointerEventButton::Middle,
                    _ => slint::platform::PointerEventButton::Other,
                };
                let pos = LogicalPosition::new(
                    self.pointer_pos.0 as f32,
                    self.pointer_pos.1 as f32,
                );
                let slint_event = match state {
                    ElementState::Pressed => {
                        slint::platform::WindowEvent::PointerPressed {
                            position: pos,
                            button: slint_btn,
                        }
                    }
                    ElementState::Released => {
                        slint::platform::WindowEvent::PointerReleased {
                            position: pos,
                            button: slint_btn,
                        }
                    }
                };
                slint_window.dispatch_event(slint_event);
            }

            WindowEvent::KeyboardInput { event: key_event, .. } => {
                // DELIVERABLE 5: Queue keyboard events for forwarding to wayland client
                // (forwarding happens in the main loop where we have SpikeState)
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
        // Request continuous redraws for ~60fps
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
    }
}

impl CompositorApp {
    fn render_frame(&mut self) {
        let Some(slint_window) = self.slint_window.as_ref() else {
            return;
        };
        let Some(sb_surface) = self.sb_surface.as_mut() else {
            return;
        };
        let Some(ui) = self.ui.as_ref() else {
            return;
        };

        // Update clock
        let now = Instant::now();
        if now.duration_since(self.last_clock_update) >= Duration::from_secs(1) {
            self.last_clock_update = now;
            let elapsed = now.duration_since(self.start_time);
            let secs = elapsed.as_secs();
            let h = secs / 3600;
            let m = (secs % 3600) / 60;
            let s = secs % 60;
            ui.set_clock_text(slint::SharedString::from(format!("{:02}:{:02}:{:02}", h, m, s)));
        }

        // Render Slint scene → pixel buffer
        let size = slint_window.size();
        let w = size.width.max(1) as usize;
        let h = size.height.max(1) as usize;
        self.pixel_buf.resize(w * h, PremultipliedRgbaColor::default());

        slint_window.draw_if_needed(|renderer| {
            renderer.render(&mut self.pixel_buf, w);
        });

        // Blit to softbuffer
        if let Ok(mut buf) = sb_surface.buffer_mut() {
            let len = buf.len().min(self.pixel_buf.len());
            for i in 0..len {
                let src = &self.pixel_buf[i];
                // softbuffer expects 0x00RRGGBB
                buf[i] = ((src.red as u32) << 16)
                    | ((src.green as u32) << 8)
                    | (src.blue as u32);
            }
            buf.present().ok();
        }
    }

    /// Called from the main loop to update client texture if new pixels arrived.
    fn update_client_texture(&mut self, state: &mut SpikeState) {
        let mut client_data = state.client_pixels.lock().unwrap();
        if !client_data.dirty || client_data.width == 0 {
            return;
        }

        let w = client_data.width;
        let h = client_data.height;
        let pixels = client_data.pixels.clone();
        client_data.dirty = false;
        drop(client_data);

        let Some(ui) = self.ui.as_ref() else { return };

        let pixel_buf = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(
            &pixels, w, h,
        );
        let image = slint::Image::from_rgba8_premultiplied(pixel_buf);
        ui.set_client_texture(image);
        ui.set_client_visible(true);
        ui.set_client_w(w as i32);
        ui.set_client_h(h as i32);

        debug!("Updated Slint client texture: {}x{}", w, h);
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Main entry point
// ──────────────────────────────────────────────────────────────────────────────

pub fn run() -> Result<()> {
    // ── 1. Set up calloop event loop ──────────────────────────────────────────
    let mut calloop = EventLoop::<SpikeState>::try_new()
        .context("failed to create calloop event loop")?;
    let loop_signal = calloop.get_signal();

    // ── 2. Set up Slint platform ──────────────────────────────────────────────
    let platform = CalloopPlatform::new(loop_signal.clone());
    let window_ref = platform.window.clone();

    slint::platform::set_platform(Box::new(platform))
        .context("failed to set Slint platform")?;

    // Create the Slint UI (triggers create_window_adapter)
    let ui = CompositorUI::new().context("failed to create Slint UI")?;
    ui.set_clock_text(slint::SharedString::from("00:00:00"));

    // DELIVERABLE 5: Wire up quit button
    {
        let signal = loop_signal.clone();
        ui.on_quit_clicked(move || {
            info!("Quit button clicked (Deliverable 5 ✓)");
            signal.stop();
        });
    }

    // DELIVERABLE 5: Wire up client-clicked callback for pointer forwarding
    // (forwarding happens in the main loop where we have access to SpikeState)
    // We store the click coordinates in an Arc<Mutex<>> so the main loop can read them
    let last_client_click: Arc<std::sync::Mutex<Option<(f32, f32)>>> =
        Arc::new(std::sync::Mutex::new(None));
    {
        let click_ref = last_client_click.clone();
        ui.on_client_clicked(move |x, y| {
            *click_ref.lock().unwrap() = Some((x, y));
        });
    }

    info!("Slint platform initialized (Deliverable 1 ✓)");

    // ── 3. Set up Wayland display + socket ────────────────────────────────────
    let mut display = Display::<SpikeState>::new()
        .context("failed to create wayland display")?;
    let display_handle = display.handle();

    let socket_source = ListeningSocketSource::new_auto()
        .context("failed to bind wayland socket")?;
    let socket_name = socket_source.socket_name().to_os_string();
    info!("Wayland socket: {:?}", socket_name);
    println!("WAYLAND_DISPLAY={}", socket_name.to_string_lossy());

    // Insert wayland display into calloop
    let wayland_source = Generic::new(display, Interest::READ, CalloopMode::Level);
    calloop
        .handle()
        .insert_source(wayland_source, |_event, display, state| {
            // SAFETY: display lives for the duration of the event loop.
            unsafe { display.get_mut().dispatch_clients(state)? };
            Ok(PostAction::Continue)
        })
        .context("failed to insert wayland event source")?;

    // Insert socket listener
    calloop
        .handle()
        .insert_source(socket_source, |stream, _, state| {
            state
                .display_handle
                .insert_client(stream, Arc::new(ClientState::default()))
                .unwrap();
        })
        .context("failed to insert socket source")?;

    info!("Wayland socket ready (Deliverable 3 ✓)");

    // ── 4. Create SpikeState ──────────────────────────────────────────────────
    let mut state = SpikeState::new(
        display_handle.clone(),
        calloop.handle(),
        loop_signal.clone(),
    );

    // Register virtual output
    let output = Output::new(
        "spike-output".to_owned(),
        PhysicalProperties {
            size: (WIDTH as i32, HEIGHT as i32).into(),
            subpixel: Subpixel::Unknown,
            make: "Spike".into(),
            model: "Virtual".into(),
            serial_number: "0000".into(),
        },
    );
    let mode = Mode {
        size: (WIDTH as i32, HEIGHT as i32).into(),
        refresh: 60_000,
    };
    output.change_current_state(
        Some(mode),
        Some(Transform::Normal),
        None,
        Some((0, 0).into()),
    );
    output.set_preferred(mode);
    output.create_global::<SpikeState>(&display_handle);

    // ── 5. Set up winit event loop ────────────────────────────────────────────
    let mut winit_event_loop = WinitEventLoop::new()
        .context("failed to create winit event loop")?;
    winit_event_loop.set_control_flow(ControlFlow::Poll);

    let pending_keys: Arc<Mutex<VecDeque<PendingKeyEvent>>> =
        Arc::new(Mutex::new(VecDeque::new()));
    let mut app = CompositorApp::new(window_ref, ui, pending_keys.clone());

    // ── 6. Main loop: pump winit + calloop alternately ────────────────────────
    info!("Entering main loop");

    loop {
        // Pump winit events (non-blocking, 1ms timeout)
        let pump_status = winit_event_loop
            .pump_app_events(Some(Duration::from_millis(1)), &mut app);

        match pump_status {
            PumpStatus::Exit(_code) => {
                info!("Winit event loop exited");
                break;
            }
            PumpStatus::Continue => {}
        }

        // Pump calloop for wayland events (non-blocking)
        calloop
            .dispatch(Some(Duration::ZERO), &mut state)
            .context("calloop dispatch error")?;

        if state.should_exit {
            break;
        }

        // Flush wayland clients
        state.display_handle.flush_clients().ok();

        // Update Slint timers/animations (Deliverable 1)
        slint::platform::update_timers_and_animations();

        // Update client texture if new pixels arrived (Deliverable 4)
        app.update_client_texture(&mut state);

        // Forward pointer clicks to wayland client (Deliverable 5)
        if let Some((cx, cy)) = last_client_click.lock().unwrap().take() {
            forward_pointer_click(&mut state, cx as f64, cy as f64);
        }

        // Forward keyboard events to wayland client (Deliverable 5)
        {
            let mut keys = pending_keys.lock().unwrap();
            while let Some(key_event) = keys.pop_front() {
                forward_keyboard_event(&mut state, key_event);
            }
        }

        // Small sleep to avoid CPU spin at 100%
        std::thread::sleep(Duration::from_millis(4));
    }

    info!("Compositor exiting cleanly");
    Ok(())
}

/// Forward a pointer press+release to the active wayland client surface.
fn forward_pointer_click(state: &mut SpikeState, rel_x: f64, rel_y: f64) {
    use smithay::utils::SERIAL_COUNTER;

    let Some(surface) = state.active_surface.clone() else {
        return;
    };

    let pointer = state.seat.get_pointer().unwrap();
    let serial = SERIAL_COUNTER.next_serial();
    let time = state.clock.now().as_millis() as u32;

    // Set focus to the client surface
    pointer.motion(
        state,
        Some((surface.clone(), (rel_x, rel_y).into())),
        &smithay::input::pointer::MotionEvent {
            location: (rel_x + 100.0, rel_y + 100.0).into(), // compositor-space
            serial,
            time,
        },
    );

    // Send button press
    pointer.button(
        state,
        &smithay::input::pointer::ButtonEvent {
            button: 0x110, // BTN_LEFT
            state: smithay::backend::input::ButtonState::Pressed,
            serial,
            time,
        },
    );

    pointer.frame(state);

    // Send button release
    let serial2 = SERIAL_COUNTER.next_serial();
    pointer.button(
        state,
        &smithay::input::pointer::ButtonEvent {
            button: 0x110,
            state: smithay::backend::input::ButtonState::Released,
            serial: serial2,
            time: time + 50,
        },
    );
    pointer.frame(state);
}

/// Forward a keyboard event from winit to the active wayland client.
fn forward_keyboard_event(state: &mut SpikeState, key_event: PendingKeyEvent) {
    use smithay::{
        backend::input::KeyState,
        input::keyboard::Keycode,
        utils::SERIAL_COUNTER,
    };

    let Some(surface) = state.active_surface.clone() else {
        return;
    };

    let Some(keyboard) = state.seat.get_keyboard() else {
        return;
    };

    // Make sure this surface has keyboard focus
    keyboard.set_focus(state, Some(surface), SERIAL_COUNTER.next_serial());

    let serial = SERIAL_COUNTER.next_serial();
    let time = state.clock.now().as_millis() as u32;

    // The scancode from winit is a Linux keycode (evdev).
    // XKB expects evdev keycodes + 8 (historical X11 offset).
    let keycode = Keycode::new(key_event.scancode + 8);
    let key_state = if key_event.pressed {
        KeyState::Pressed
    } else {
        KeyState::Released
    };

    keyboard.input_forward(state, keycode, key_state, serial, time, false);
}
