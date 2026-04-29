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
    backdrop::{BackdropSynth, WindowSnapshot},
    cursor::{self, CursorKind, HitZone, WindowRect, TITLEBAR_HEIGHT},
    cursor_render::CursorRenderer,
    desktop,
    ipc_server::{self, IpcCommand, PendingIpc},
    platform::{CalloopPlatform, GpuWindowAdapter},
    render::{ChromeRenderer, WindowChromeParams},
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
            | wgpu::TextureUsages::COPY_DST       // final_tex is a copy destination
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
    /// Slint's render-to-texture output. Re-rendered only when Slint has
    /// pending redraws (idle desktop = 0 GPU renders here).
    render_texture: Option<wgpu::Texture>,
    /// Per-frame composite target. Blitted from render_texture every frame,
    /// then chrome passes draw on top. Decouples chrome compositing from
    /// Slint's dirty-flag — without this, on non-redraw frames the chrome
    /// pass accumulates shadow on top of last frame's already-shadowed
    /// render_texture, which reads as flicker / progressively-darkening.
    final_texture: Option<wgpu::Texture>,
    render_texture_size: (u32, u32),
    /// Host display scale_factor (1.0 on standard, 2.0 on HiDPI). Used to
    /// translate WM/slint LOGICAL coords → swapchain PHYSICAL coords for
    /// the chrome shader passes + per-window re-blits, which operate on
    /// the physical-sized render textures.
    scale_factor: f32,

    gpu_window: Option<Rc<GpuWindowAdapter>>,
    window_ref: Arc<Mutex<Option<Rc<GpuWindowAdapter>>>>,
    ui: Option<Compositor>,

    /// Composable chrome render pipeline — shadow / border / highlight (and
    /// optional squircle-clip) as separate, single-purpose passes.
    chrome: Option<ChromeRenderer>,

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
    /// Dock-menu deferred actions: `(app_id, action_id)` — 2=Show All, 5=Quit.
    pending_dock_action: Arc<Mutex<VecDeque<(String, i32)>>>,
    /// Currently-registered StatusNotifierItems. Updated each frame by
    /// draining tray-host events; rendered as panel tray icons.
    tray_items: Vec<crate::tray::TrayItem>,
    pending_minimize: Arc<Mutex<VecDeque<i32>>>,
    pending_maximize: Arc<Mutex<VecDeque<i32>>>,
    pending_activate: Arc<Mutex<VecDeque<i32>>>,
    /// Pending alt-tab step requests from the winit key handler.
    pending_alt_tab_step:   Arc<Mutex<u32>>,
    /// Pending alt-tab commit (Alt released).
    pending_alt_tab_commit: Arc<Mutex<bool>>,

    /// Persistent windows VecModel — Slint repeater preserves WindowChrome
    /// component identity (and IconButton hover state) across redraws.
    windows_model: Rc<VecModel<crate::WindowItem>>,
    /// Persistent popups VecModel — same pattern for xdg_popup surfaces
    /// (context menus, dropdowns). Re-used per-frame in update_windows.
    popups_model: Rc<VecModel<crate::PopupItem>>,

    /// CPU backdrop synth — pushes a wallpaper+windows blurred Image to Slint
    /// every ~200 ms so the panel/dock backdrops include window content.
    backdrop: BackdropSynth,

    /// 1-second rolling FPS sample, surfaced in the debug overlay.
    fps: f32,
    last_fps_sample: Instant,
    last_fps_count: u64,

    /// Snap candidate from the last Move-drag motion. If Some on left-up,
    /// apply_pending_snap glides the window into the snap rect.
    pending_snap: Option<(i32 /* wm_id */, crate::snap::SnapRect)>,

    /// Queue of IPC commands posted by the unix-socket server thread. Drained
    /// in the main loop on each iteration.
    pending_ipc: PendingIpc,
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
            final_texture: None,
            render_texture_size: (0, 0),
            scale_factor: 1.0,
            gpu_window: None,
            window_ref,
            ui: Some(ui),
            chrome: None,
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
            pending_dock_action: Arc::new(Mutex::new(VecDeque::new())),
            tray_items:        Vec::new(),
            pending_minimize: Arc::new(Mutex::new(VecDeque::new())),
            pending_maximize: Arc::new(Mutex::new(VecDeque::new())),
            pending_activate: Arc::new(Mutex::new(VecDeque::new())),
            pending_alt_tab_step:   Arc::new(Mutex::new(0)),
            pending_alt_tab_commit: Arc::new(Mutex::new(false)),
            windows_model: Rc::new(VecModel::default()),
            popups_model: Rc::new(VecModel::default()),
            backdrop: BackdropSynth::new(WIDTH, HEIGHT),
            fps: 0.0,
            last_fps_sample: Instant::now(),
            last_fps_count: 0,
            pending_snap: None,
            pending_ipc: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn get_render_texture(&mut self, width: u32, height: u32) -> Option<&wgpu::Texture> {
        if self.render_texture.is_none() || self.render_texture_size != (width, height) {
            let gpu_window = self.gpu_window.as_ref()?;
            let device = &gpu_window.wgpu_device;
            let format = self.swapchain_format.unwrap_or(wgpu::TextureFormat::Rgba8Unorm);
            self.render_texture = Some(make_render_texture(device, width, height, format));
            self.final_texture  = Some(make_render_texture(device, width, height, format));
            self.render_texture_size = (width, height);
            debug!("(Re)created render+final textures {}x{} {:?}", width, height, format);
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

        let chrome = ChromeRenderer::new(
            std::sync::Arc::new(gpu_window.wgpu_device.clone()),
            format,
        );
        self.chrome = Some(chrome);
        info!("ChromeRenderer initialised (shadow + border + highlight passes)");

        // Initial resize: physical = logical at startup (winit hasn't
        // delivered a Resized yet) — scale_factor is queried from the
        // winit window itself, which already knows the host's HiDPI
        // factor at this point. The first WindowEvent::Resized that
        // arrives shortly after will fix this up if the host disagrees.
        let initial_scale = window.scale_factor() as f32;
        let initial_phys_w = (WIDTH  as f32 * initial_scale).round() as u32;
        let initial_phys_h = (HEIGHT as f32 * initial_scale).round() as u32;
        self.scale_factor = initial_scale;
        gpu_window.resize(initial_phys_w, initial_phys_h, initial_scale);

        if let Some(ui) = &self.ui {
            // Bind the persistent windows model exactly once. From now on
            // update_windows mutates rows in place via push/remove/set_row_data
            // — the repeater preserves WindowChrome identity (and IconButton
            // hover state) across redraws.
            ui.set_windows(slint::ModelRc::from(self.windows_model.clone()));
            ui.set_popups(slint::ModelRc::from(self.popups_model.clone()));
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
                // winit `size` is PHYSICAL pixels. Slint, our WM, our cursor
                // overlay, and wayland clients all work in LOGICAL pixels —
                // so when the host has scale_factor > 1 (HiDPI), feeding raw
                // physical sizes everywhere makes the cursor visual end up
                // at scale × the real pointer position and pointer events
                // forwarded to clients land at the wrong spot. Divide by
                // the host's scale_factor here so the WM, slint, and the
                // wayland boundary all share one coord system.
                let scale = self.window.as_ref()
                    .map(|w| w.scale_factor())
                    .unwrap_or(1.0)
                    .max(0.0001);
                self.scale_factor = scale as f32;
                let logical_w = ((size.width  as f64 / scale).round() as u32).max(1);
                let logical_h = ((size.height as f64 / scale).round() as u32).max(1);
                if let Some(surface) = self.wgpu_surface.as_ref() {
                    // wgpu surface is in physical pixels (raw size).
                    let fmt = configure_surface(
                        surface,
                        &gpu_window.wgpu_adapter,
                        &gpu_window.wgpu_device,
                        size.width.max(1),
                        size.height.max(1),
                    );
                    self.swapchain_format = Some(fmt);
                }
                // Tell slint + WM we're at logical size (matches the wayland
                // output's advertised logical-pixel size). The render texture
                // is allocated at PHYSICAL size below (matches swapchain).
                gpu_window.resize(size.width.max(1), size.height.max(1), scale as f32);
                self.render_texture = None;
                self.final_texture  = None;
                self.wm.output_w = logical_w as i32;
                self.wm.output_h = logical_h as i32;
                self.backdrop.set_output_size(logical_w, logical_h);
                if let Some(c) = self.chrome.as_mut() {
                    c.invalidate();
                }
            }

            WindowEvent::CursorMoved { position, .. } => {
                // winit `position` is PHYSICAL pixels. Convert to LOGICAL so
                // the cursor visual, slint hit testing, and wayland client
                // events all share the compositor's logical-pixel space.
                let scale = self.window.as_ref()
                    .map(|w| w.scale_factor())
                    .unwrap_or(1.0)
                    .max(0.0001);
                let lx = position.x / scale;
                let ly = position.y / scale;
                self.pointer_pos = (lx, ly);
                self.update_cursor_position(lx, ly);
                gpu_window.inner_window().dispatch_event(
                    slint::platform::WindowEvent::PointerMoved {
                        position: LogicalPosition::new(lx as f32, ly as f32),
                    },
                );
                self.pending_pointers.lock().unwrap().push_back(
                    PendingPointerEvent::Motion { x: lx, y: ly }
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
                    // Drag release (active_drag = None + final configure) is
                    // handled in the main loop's PointerEvent::Button branch
                    // where we have SpikeState access — see release_drag().
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
                        // Super+T → toggle light/dark theme + per-mode wallpaper.
                        self.theme.toggle_mode();
                        self.apply_theme_to_slint();
                        self.swap_wallpaper_for_current_mode();
                        debug!("Super+T: toggled theme to {:?}", self.theme.current_mode);
                    }
                    23 if pressed && self.super_held => {
                        // Super+I → toggle debug overlay.
                        if let Some(ui) = self.ui.as_ref() {
                            let now = ui.get_debug_overlay_visible();
                            ui.set_debug_overlay_visible(!now);
                        }
                    }
                    53 if pressed && self.super_held => {
                        // Super+/ → keyboard shortcuts help.
                        if let Some(ui) = self.ui.as_ref() {
                            let now = ui.get_help_overlay_visible();
                            ui.set_help_overlay_visible(!now);
                        }
                    }
                    // Super+W (scancode 17) → close focused window.
                    17 if pressed && self.super_held => {
                        if let Some(id) = self.wm.focused_id() {
                            self.pending_close.lock().unwrap().push_back(id);
                            debug!("Super+W: queued close for focused id={}", id);
                        }
                    }
                    // Super+M (scancode 50) → minimize focused window.
                    50 if pressed && self.super_held => {
                        if let Some(id) = self.wm.focused_id() {
                            self.pending_minimize.lock().unwrap().push_back(id);
                            debug!("Super+M: queued minimize for focused id={}", id);
                        }
                    }
                    // Super+D (scancode 32) → show desktop / minimize all.
                    32 if pressed && self.super_held => {
                        let ids: Vec<i32> = self.wm.windows.values()
                            .filter(|w| !w.minimized && !w.closing)
                            .map(|w| w.id)
                            .collect();
                        let mut q = self.pending_minimize.lock().unwrap();
                        for id in ids { q.push_back(id); }
                        debug!("Super+D: minimized all visible windows");
                    }
                    // Super+Space (scancode 57) → toggle the app launcher.
                    57 if pressed && self.super_held => {
                        if let Some(ui) = self.ui.as_ref() {
                            let now = ui.get_launcher_open();
                            ui.set_launcher_open(!now);
                            if !now { ui.set_launcher_query(SharedString::default()); }
                        }
                    }
                    // Escape → close any open compositor overlay (menus,
                    // popouts, debug overlay, launcher) without forwarding
                    // to clients.
                    1 if pressed => {
                        if let Some(ui) = self.ui.as_ref() {
                            let any_open =
                                ui.get_desktop_menu_open()
                                || ui.get_datetime_popout_open()
                                || ui.get_control_centre_open()
                                || ui.get_help_overlay_visible()
                                || ui.get_launcher_open()
                                || ui.get_dock_menu_open();
                            if any_open {
                                ui.set_desktop_menu_open(false);
                                ui.set_datetime_popout_open(false);
                                ui.set_control_centre_open(false);
                                ui.set_help_overlay_visible(false);
                                ui.set_launcher_open(false);
                                ui.set_dock_menu_open(false);
                                if let Some(gpu) = self.gpu_window.as_ref() {
                                    gpu.mark_dirty();
                                }
                            }
                        }
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
            let local = chrono::Local::now();
            ui.set_clock_text(SharedString::from(local.format("%H:%M:%S").to_string()));
            // Short panel date — e.g. "Wed 29 Apr" — sits next to the time.
            ui.set_panel_date_text(SharedString::from(local.format("%a %-d %b").to_string()));
            ui.set_popout_date_text(SharedString::from(local.format("%A, %-d %B").to_string()));
            ui.set_popout_day_text(SharedString::from(local.format("%A").to_string()));

            // Calendar grid for the popout — rebuild only when the day
            // changes (cheap to do every second; the model is 42 cells).
            ui.set_popout_calendar_month_text(
                SharedString::from(local.format("%B %Y").to_string()));
            let cal = build_calendar_grid(local.date_naive());
            let model = std::rc::Rc::new(VecModel::from(cal));
            ui.set_popout_calendar_days(slint::ModelRc::from(model));
            if ui.get_debug_overlay_visible() {
                let dump = self.build_debug_dump();
                ui.set_debug_text(SharedString::from(dump));
            }
        }

        // Roll the FPS sample on a 1-second window.
        let fps_elapsed = now.duration_since(self.last_fps_sample);
        if fps_elapsed >= Duration::from_secs(1) {
            let frames = self.frame_count - self.last_fps_count;
            self.fps = frames as f32 / fps_elapsed.as_secs_f32();
            self.last_fps_count = self.frame_count;
            self.last_fps_sample = now;
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

        if let (Some(render_tex), Some(final_tex)) =
            (self.render_texture.as_ref(), self.final_texture.as_ref())
        {
            let mut encoder = device.create_command_encoder(
                &wgpu::CommandEncoderDescriptor { label: Some("chrome+blit") }
            );

            // Step 1 — copy render_tex → final_tex. final_tex is freshly
            // populated every frame, even on frames where Slint didn't
            // re-render. Without this, chrome passes that ran onto render_tex
            // would accumulate shadow on top of last frame's shadow until
            // Slint finally re-rendered and cleared it (visible as flicker).
            encoder.copy_texture_to_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: render_tex,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::TexelCopyTextureInfo {
                    texture: final_tex,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            );

            // Step 2 — chrome passes draw onto final_tex. scene_view is
            // render_tex (only the squircle clip pass samples it; off by
            // default). target_view is final_tex.
            if let Some(chrome) = self.chrome.as_mut() {
                // WM coords are LOGICAL pixels; the chrome shader operates on
                // the PHYSICAL render texture. Multiply by scale_factor here
                // so a window at logical (200, 100) on a 2× HiDPI screen
                // maps to physical (400, 200), matching what slint actually
                // rendered into the texture.
                let s = self.scale_factor;
                let chrome_windows: Vec<WindowChromeParams> = if let Some(ui) = self.ui.as_ref() {
                    let model = ui.get_windows();
                    let len = model.row_count();
                    let mode_t = self.theme.mode_t;
                    (0..len).map(|i| {
                        let item = model.row_data(i).unwrap();
                        let focus_t = self.theme.window_focus_t(item.id);
                        let titlebar = if item.csd { 0.0 } else { 33.0 };
                        let chrome_w = item.geom_w.max(1) as f32;
                        let chrome_h = item.geom_h.max(1) as f32 + titlebar;
                        WindowChromeParams {
                            x: item.x as f32 * s,
                            y: item.y as f32 * s,
                            w: chrome_w * s,
                            h: chrome_h * s,
                            active: item.focused,
                            focus_t,
                            mode_t,
                            csd: item.csd,
                        }
                    }).collect()
                } else { Vec::new() };

                if !chrome_windows.is_empty() {
                    let scene_view  = render_tex.create_view(&wgpu::TextureViewDescriptor::default());
                    let target_view = final_tex.create_view(&wgpu::TextureViewDescriptor::default());
                    // Snapshot the per-window rects in physical pixels;
                    // used by the between-phases closure to re-blit each
                    // window's region from render_tex onto final_tex,
                    // overwriting any shadow that bled into the window's
                    // footprint. Caps each rect to the surface bounds so
                    // copy_texture_to_texture never reads/writes OOB.
                    let win_rects: Vec<(u32, u32, u32, u32)> = chrome_windows.iter().map(|win| {
                        let x = win.x as i32;
                        let y = win.y as i32;
                        // chrome_windows w/h already include the SSD titlebar
                        // (or the cropped geom_h for CSD), so this rect IS the
                        // full visual footprint — no extra titlebar add.
                        let ww = win.w as i32;
                        let wh = win.h as i32;
                        let cx0 = x.max(0);
                        let cy0 = y.max(0);
                        let cx1 = (x + ww).min(w as i32).max(cx0);
                        let cy1 = (y + wh).min(h as i32).max(cy0);
                        (cx0 as u32, cy0 as u32, (cx1 - cx0) as u32, (cy1 - cy0) as u32)
                    }).collect();

                    chrome.render(
                        queue, &mut encoder,
                        &scene_view, &target_view,
                        w, h,
                        &chrome_windows,
                        |enc, wi| {
                            // Z-order fix: after each window's chrome
                            // (shadow + border + highlight), re-blit ONLY
                            // higher-z windows' rects from render_tex onto
                            // final_tex. This overwrites any chrome from
                            // this back window that fell inside a front
                            // window's footprint with the front window's
                            // actual content. render_tex already holds
                            // z-stacked content from Slint's repeater so
                            // copying its rects restores correct z without
                            // per-window content textures.
                            for j in (wi + 1)..win_rects.len() {
                                let (rx, ry, rw, rh) = win_rects[j];
                                if rw == 0 || rh == 0 { continue }
                                enc.copy_texture_to_texture(
                                    wgpu::TexelCopyTextureInfo {
                                        texture: render_tex,
                                        mip_level: 0,
                                        origin: wgpu::Origin3d { x: rx, y: ry, z: 0 },
                                        aspect: wgpu::TextureAspect::All,
                                    },
                                    wgpu::TexelCopyTextureInfo {
                                        texture: final_tex,
                                        mip_level: 0,
                                        origin: wgpu::Origin3d { x: rx, y: ry, z: 0 },
                                        aspect: wgpu::TextureAspect::All,
                                    },
                                    wgpu::Extent3d { width: rw, height: rh, depth_or_array_layers: 1 },
                                );
                            }
                        },
                    );
                }

                // ── Step 2b. Re-blit every visible UI overlay from render_tex
                //              onto final_tex AFTER chrome. Chrome (shadow,
                //              border, highlight) draws over whatever was on
                //              final_tex — without re-blitting the overlays
                //              the chrome bleeds through panels, popouts,
                //              context menus, the launcher, etc. Each rect
                //              is clamped to the swapchain so an overlay
                //              positioned partly off-screen doesn't crash
                //              copy_texture_to_texture.
                // reblit takes LOGICAL coords and converts internally to
                // physical (matches the swapchain texture's pixel grid).
                let s = self.scale_factor;
                let mut reblit = |x: i32, y: i32, rw: i32, rh: i32| {
                    let px  = (x  as f32 * s).round() as i32;
                    let py  = (y  as f32 * s).round() as i32;
                    let prw = (rw as f32 * s).round() as i32;
                    let prh = (rh as f32 * s).round() as i32;
                    let cx0 = px.max(0);
                    let cy0 = py.max(0);
                    let cx1 = (px + prw).min(w as i32).max(cx0);
                    let cy1 = (py + prh).min(h as i32).max(cy0);
                    let cw = (cx1 - cx0) as u32;
                    let ch = (cy1 - cy0) as u32;
                    if cw == 0 || ch == 0 { return; }
                    encoder.copy_texture_to_texture(
                        wgpu::TexelCopyTextureInfo {
                            texture: render_tex, mip_level: 0,
                            origin: wgpu::Origin3d { x: cx0 as u32, y: cy0 as u32, z: 0 },
                            aspect: wgpu::TextureAspect::All,
                        },
                        wgpu::TexelCopyTextureInfo {
                            texture: final_tex, mip_level: 0,
                            origin: wgpu::Origin3d { x: cx0 as u32, y: cy0 as u32, z: 0 },
                            aspect: wgpu::TextureAspect::All,
                        },
                        wgpu::Extent3d { width: cw, height: ch, depth_or_array_layers: 1 },
                    );
                };

                let panel_h = crate::wm::PANEL_HEIGHT.max(0);
                let dock_h  = crate::wm::DOCK_HEIGHT.max(0);
                // reblit takes LOGICAL coords; iw/ih are LOGICAL screen size.
                let iw = self.wm.output_w;
                let ih = self.wm.output_h;
                // Always-visible bands.
                reblit(0, 0, iw, panel_h);
                reblit(0, ih - dock_h, iw, dock_h);

                // Open-overlay rects — read state directly off the Slint
                // window so the re-blit covers exactly what's painted.
                if let Some(ui) = self.ui.as_ref() {
                    if ui.get_datetime_popout_open() {
                        // Centre-top, 320×380, anchored panel_h + 6.
                        let pw = 320;
                        let ph = 380;
                        reblit((iw - pw) / 2, panel_h + 6, pw, ph);
                    }
                    if ui.get_control_centre_open() {
                        let pw = 300;
                        let ph = 220;
                        reblit(iw - pw - 14, panel_h + 6, pw, ph);
                    }
                    if ui.get_launcher_open() {
                        let pw = 540;
                        let ph = 64;
                        reblit((iw - pw) / 2, 120, pw, ph);
                    }
                    if ui.get_help_overlay_visible() {
                        let pw = 480;
                        let ph = 360;
                        reblit((iw - pw) / 2, (ih - ph) / 2, pw, ph);
                    }
                    if ui.get_desktop_menu_open() {
                        let pw = 220;
                        let ph = 260;
                        reblit(ui.get_desktop_menu_x(), ui.get_desktop_menu_y(), pw, ph);
                    }
                    if ui.get_dock_menu_open() {
                        let pw = 220;
                        let ph = 280;
                        reblit(ui.get_dock_menu_x(), ui.get_dock_menu_y(), pw, ph);
                    }
                }
            }

            // Step 3 — final_tex → swapchain.
            encoder.copy_texture_to_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: final_tex,
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

    /// Build the Slint `WindowItem` list from `WM` state + toplevel pixel buffers,
    /// then push it to the UI.  Called every frame when client textures are dirty
    /// or when WM state changes.
    fn update_windows(&mut self, state: &mut SpikeState) {
        use smithay::wayland::compositor::with_states;
        use smithay::wayland::shell::xdg::{SurfaceCachedState, XdgToplevelSurfaceData};

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

        // First-render kick: any window awaiting its first buffer commit
        // starts its open animation NOW, so the user actually sees it play.
        let kick_ids: Vec<i32> = self.wm.windows.values()
            .filter(|w| w.awaiting_first_render)
            .filter(|w| {
                state.toplevels.iter().find(|t| t.surface == w.surface)
                    .map_or(false, |tl| tl.pixels.lock().unwrap().width > 0)
            })
            .map(|w| w.id)
            .collect();
        // Sync each toplevel's committed buffer size into the WM so the
        // geometry springs follow the client's actual size after configures.
        // Skip windows currently being resized — `set_geometry_by_id` already
        // pins the WM to the in-flight drag size and we don't want commits
        // landing mid-drag to overwrite that.
        let resizing_idx = match &self.active_drag {
            Some(ActiveDrag::Resize { toplevel_idx, .. }) => Some(*toplevel_idx),
            _ => None,
        };
        // Pass 1 — for each toplevel, work out the visible-content rect
        // inside the composited buffer. Source of truth in priority order:
        //   1. `xdg_surface.set_window_geometry` if the client provides it.
        //   2. Auto-detected via alpha scan (`detect_visible_bbox`) — finds
        //      the bounding box of opaque pixels along centerlines, which
        //      gives us the visible-window rect of CSD clients that bake
        //      shadow / border padding into their buffer (no app-id table,
        //      no protocol-specific assumptions — purely pixel-driven).
        //   3. Full buffer dims (no crop) when the buffer is fully opaque.
        //
        // Padding (visible rect strictly inside the buffer) is what flips
        // a window into CSD layout: it means the client is drawing its own
        // shadow/border which we crop away, then wrap with our chrome.
        struct ToplevelMeta {
            surface: WlSurface,
            gx: i32, gy: i32, gw: i32, gh: i32,
            bw: i32, bh: i32,
            csd_now: bool,
            csd_verdict: bool,
            is_resizing: bool,
            app_id: String,
        }
        let mut metas: Vec<ToplevelMeta> = Vec::with_capacity(state.toplevels.len());
        for (i, tl) in state.toplevels.iter().enumerate() {
            let (gx_xdg, gy_xdg, gw_xdg, gh_xdg, app_id) = with_states(&tl.surface, |states| {
                let mut guard = states.cached_state.get::<SurfaceCachedState>();
                let geom = guard.current().geometry
                    .map(|r| (r.loc.x, r.loc.y, r.size.w, r.size.h))
                    .unwrap_or((0, 0, 0, 0));
                let app_id = states.data_map
                    .get::<XdgToplevelSurfaceData>()
                    .and_then(|d| d.lock().ok()?.app_id.clone())
                    .unwrap_or_default();
                (geom.0, geom.1, geom.2, geom.3, app_id)
            });
            let (bw, bh, auto_bbox) = {
                let p = tl.pixels.lock().unwrap();
                let bbox = crate::wayland_state::detect_visible_bbox(
                    &p.pixels, p.width, p.height,
                );
                (p.width as i32, p.height as i32, bbox)
            };
            let (gx, gy, gw, gh) = if gw_xdg > 0 && gh_xdg > 0 {
                (gx_xdg, gy_xdg, gw_xdg, gh_xdg)
            } else if let Some(b) = auto_bbox {
                b
            } else {
                (0, 0, bw, bh)
            };
            let has_padding = bw > 0 && bh > 0
                && (gx > 0 || gy > 0 || gw < bw || gh < bh);
            metas.push(ToplevelMeta {
                surface: tl.surface.clone(),
                gx, gy, gw, gh, bw, bh,
                csd_now: tl.csd,
                csd_verdict: tl.csd || has_padding,
                is_resizing: Some(i) == resizing_idx,
                app_id,
            });
        }

        // Pass 2 — apply: write csd + geom_w/h to WindowState, mirror csd
        // back to ToplevelInfo, push buffer dims into WM geometry springs.
        for m in &metas {
            if let Some(win) = self.wm.windows.values_mut()
                .find(|w| w.surface == m.surface)
            {
                win.csd = m.csd_verdict;
                if m.gw > 0 && m.gh > 0 {
                    win.geom_w = m.gw;
                    win.geom_h = m.gh;
                    win.geom_x = m.gx;
                    win.geom_y = m.gy;
                }
                if !m.app_id.is_empty() && win.app_id != m.app_id {
                    win.app_id = m.app_id.clone();
                }
            }
            if m.csd_verdict && !m.csd_now {
                if let Some(t) = state.toplevels.iter_mut()
                    .find(|t| t.surface == m.surface)
                {
                    t.csd = true;
                }
            }
            if !m.is_resizing && m.bw > 0 && m.bh > 0 {
                self.wm.update_geometry(&m.surface, m.bw, m.bh);
            }
        }

        for id in kick_ids {
            if let Some(win) = self.wm.windows.values_mut().find(|w| w.id == id) {
                win.awaiting_first_render = false;
                win.start_open();
                debug!("WM: open animation kicked for id={}", id);
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
            let buf_w = client_data.width as i32;
            let buf_h = client_data.height as i32;
            let texture = slint::Image::from_rgba8_premultiplied(pixel_buf);
            drop(client_data);

            // Use the per-toplevel visible-rect we already worked out in
            // pass 1 (xdg geom rect → auto-detected bbox → full buffer).
            // Identical source-of-truth in Rust and Slint: source-clip on
            // the Image and the chrome size both come from this rect, so
            // image-fit:fill renders 1:1 and text stays crisp.
            let (geom_x, geom_y, geom_w, geom_h) = metas.iter()
                .find(|m| m.surface == win.surface)
                .map(|m| (m.gx, m.gy, m.gw, m.gh))
                .unwrap_or((0, 0, buf_w, buf_h));


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
                title: SharedString::from(title.clone()),
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
                geom_x,
                geom_y,
                geom_w,
                geom_h,
                csd: toplevel.csd,
            });
        }

        // Push the focused window's title into the panel's "focused-app"
        // slot. Falls back to "Desktop" when nothing is focused so the panel
        // is never blank — matches GNOME's "Activities"/macOS finder pattern.
        if let Some(ui) = self.ui.as_ref() {
            let focused_title: String = items.iter()
                .find(|it| it.focused)
                .map(|it| it.title.to_string())
                .unwrap_or_else(|| "Desktop".to_string());
            ui.set_focused_app(SharedString::from(focused_title));

            // Honour `wl_pointer.set_cursor` requests from clients:
            //   * Hidden  → set cursor-visible=false (video / drawing apps).
            //   * Named   → look up the named cursor in the system Xcursor
            //               theme and override our default — this is what
            //               makes the cursor change to text-beam over text
            //               fields, pointer over links, wait spinners, etc.
            //   * Surface → not implemented yet; fall back to compositor
            //               default so users still see something.
            use smithay::input::pointer::CursorImageStatus;
            use smithay::input::pointer::CursorIcon;
            match &state.cursor_status {
                CursorImageStatus::Hidden => {
                    ui.set_cursor_visible(false);
                }
                // Skip the default named cursor — that's what smithay sets on
                // pointer.leave, and overriding it would clobber our own
                // hit-zone-driven cursor (Move over titlebar, Resize on
                // edges, etc.). Only honour explicit non-default names.
                CursorImageStatus::Named(icon) if *icon != CursorIcon::Default => {
                    ui.set_cursor_visible(true);
                    if let Some((img, (hx, hy))) =
                        self.cursor_renderer.get_dynamic(icon.name())
                    {
                        ui.set_cursor_image(img);
                        ui.set_cursor_hotspot_x(hx);
                        ui.set_cursor_hotspot_y(hy);
                    }
                }
                CursorImageStatus::Named(_) => {
                    // Default named → leave our own hit-zone cursor in place.
                    ui.set_cursor_visible(true);
                }
                CursorImageStatus::Surface(surf) => {
                    // Client supplied a wl_surface as its cursor (drawing
                    // apps, custom carets, animated cursors). The commit
                    // handler imported the pixels into
                    // `state.cursor_surface_pixels`; we just need to
                    // push them to Slint's cursor-image plus the hotspot
                    // from CursorImageSurfaceData.
                    ui.set_cursor_visible(true);
                    let p = state.cursor_surface_pixels.lock().unwrap();
                    if p.width > 0 && p.height > 0 {
                        let pixel_buf = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(
                            &p.pixels, p.width, p.height,
                        );
                        let img = slint::Image::from_rgba8_premultiplied(pixel_buf);
                        let (cw, ch) = (p.width as f32, p.height as f32);
                        drop(p);
                        // Read hotspot from the surface's cached state.
                        use smithay::input::pointer::CursorImageSurfaceData;
                        use smithay::wayland::compositor::with_states;
                        let (hx, hy) = with_states(surf, |states| {
                            states.data_map.get::<CursorImageSurfaceData>()
                                .and_then(|d| d.lock().ok().map(|attrs|
                                    (attrs.hotspot.x as f32, attrs.hotspot.y as f32)))
                                .unwrap_or((0.0, 0.0))
                        });
                        ui.set_cursor_image(img);
                        ui.set_cursor_hotspot_x(hx);
                        ui.set_cursor_hotspot_y(hy);
                        ui.set_cursor_size(cw.max(ch));
                    }
                }
            }
        }

        // In-place diff against the persistent VecModel by `id`. Rebuilding
        // from scratch each frame caused Slint's repeater to recreate every
        // WindowChrome instance — that reset the IconButton TouchArea
        // `has-hover` flag, so hovering the close/min/max buttons would
        // briefly highlight then drop after the next frame.
        let model = &self.windows_model;
        let _ = ui;

        // 1. Remove rows whose id is no longer present in `items`.
        //    Walk backwards so removal indices stay valid.
        let mut i = model.row_count();
        while i > 0 {
            i -= 1;
            let row_id = model.row_data(i).map(|r| r.id);
            let still_present = row_id
                .map(|id| items.iter().any(|it| it.id == id))
                .unwrap_or(false);
            if !still_present {
                model.remove(i);
            }
        }

        // 2. Insert / update rows in `items` order so z-order matches.
        for (target_idx, item) in items.iter().enumerate() {
            // Locate existing row with same id (might be at any index).
            let cur_idx = (0..model.row_count())
                .find(|&j| model.row_data(j).map(|r| r.id) == Some(item.id));
            match cur_idx {
                Some(j) if j == target_idx => {
                    // Same slot — just update the row data in place. This
                    // is the cheap path; the WindowChrome instance keeps
                    // its identity so hover/animation state survives.
                    model.set_row_data(target_idx, item.clone());
                }
                Some(j) => {
                    // Row exists but at the wrong index — move via remove
                    // + insert at the right position.
                    let existing = model.row_data(j).unwrap();
                    model.remove(j);
                    let insert_at = target_idx.min(model.row_count());
                    model.insert(insert_at, existing);
                    model.set_row_data(target_idx, item.clone());
                }
                None => {
                    // New window — insert at the target index so order is preserved.
                    let insert_at = target_idx.min(model.row_count());
                    model.insert(insert_at, item.clone());
                }
            }
        }
        debug!("update_windows: pushed {} items, wm has {} windows",
               items.len(), self.wm.windows.len());

        // ── Build PopupItem list from state.popups ──────────────────────────
        // Resolve each popup's compositor-space position by walking up the
        // parent chain to a toplevel: popup_x = toplevel.x + sum(popup.rel_x).
        // Popups can have popups as parents (nested menus); cap the walk so
        // a malformed chain can't loop forever.
        let mut popup_items: Vec<crate::PopupItem> = Vec::new();
        for (pi, popup) in state.popups.iter().enumerate() {
            // Skip popups whose pixel buffer hasn't arrived yet.
            let (bw, bh, has_pixels) = {
                let p = popup.pixels.lock().unwrap();
                (p.width as i32, p.height as i32, p.width > 0)
            };
            if !has_pixels { continue; }

            // Walk parent chain to find the absolute compositor position.
            let mut abs_x = popup.rel_x;
            let mut abs_y = popup.rel_y;
            let mut cur_parent = popup.parent.clone();
            for _ in 0..16 {
                if let Some(p) = state.popups.iter().find(|p| p.surface == cur_parent) {
                    abs_x += p.rel_x;
                    abs_y += p.rel_y;
                    cur_parent = p.parent.clone();
                    continue;
                }
                if let Some(tl_win) = self.wm.windows.values()
                    .find(|w| w.surface == cur_parent)
                {
                    abs_x += tl_win.anim.current_x();
                    // For SSD parents the popup is positioned relative to
                    // the CONTENT area, which sits below our titlebar.
                    let titlebar = if tl_win.csd { 0 } else {
                        crate::wm::TITLEBAR_HEIGHT as i32
                    };
                    abs_y += tl_win.anim.current_y() + titlebar;
                }
                break;
            }

            // Build the texture from the composited popup pixels.
            let p = popup.pixels.lock().unwrap();
            let pixel_buf = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(
                &p.pixels, p.width, p.height,
            );
            let texture = slint::Image::from_rgba8_premultiplied(pixel_buf);
            drop(p);

            let w = if popup.w > 0 { popup.w } else { bw };
            let h = if popup.h > 0 { popup.h } else { bh };
            popup_items.push(crate::PopupItem {
                id: pi as i32,
                x: abs_x,
                y: abs_y,
                w,
                h,
                texture,
            });
        }

        // In-place diff for popups (same pattern as windows): keep order;
        // remove rows whose id disappeared; update existing; insert new.
        let pmodel = &self.popups_model;
        let mut i = pmodel.row_count();
        while i > 0 {
            i -= 1;
            let still = popup_items.iter().any(|it| Some(it.id) == pmodel.row_data(i).map(|r| r.id));
            if !still { pmodel.remove(i); }
        }
        for (target_idx, item) in popup_items.iter().enumerate() {
            let cur_idx = (0..pmodel.row_count())
                .find(|&j| pmodel.row_data(j).map(|r| r.id) == Some(item.id));
            match cur_idx {
                Some(j) if j == target_idx => pmodel.set_row_data(target_idx, item.clone()),
                Some(j) => {
                    let existing = pmodel.row_data(j).unwrap();
                    pmodel.remove(j);
                    let insert_at = target_idx.min(pmodel.row_count());
                    pmodel.insert(insert_at, existing);
                    pmodel.set_row_data(target_idx, item.clone());
                }
                None => {
                    let insert_at = target_idx.min(pmodel.row_count());
                    pmodel.insert(insert_at, item.clone());
                }
            }
        }

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

        // Dock-menu deferred actions: Show All Windows / Quit.
        let dock_actions: Vec<(String, i32)> = {
            let mut q = self.pending_dock_action.lock().unwrap();
            q.drain(..).collect()
        };
        for (app_id, action) in dock_actions {
            let ids = self.wm.ids_for_app(&app_id);
            info!("WM: dock-action app={} action={} → {} window(s)",
                app_id, action, ids.len());
            match action {
                2 => {
                    // Show All Windows — raise + focus the most-recent one
                    // belonging to this app. Future polish: open a window
                    // overview / mission-control style picker.
                    if let Some(&id) = ids.last() {
                        self.wm.focus_by_id(id);
                        self.update_focused_surface(state);
                    }
                }
                5 => {
                    // Quit — close every window for this app via the same
                    // path Slint's close-window callback uses.
                    for id in ids {
                        let surface = self.wm.windows.values()
                            .find(|w| w.id == id)
                            .map(|w| w.surface.clone());
                        if let Some(surf) = surface {
                            self.send_xdg_close(&surf, state);
                            self.wm.begin_close_by_id(id);
                        }
                    }
                    self.update_focused_surface(state);
                }
                _ => {}
            }
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

        // On left press: update WM focus.
        // - Click on a window  → focus it (existing behaviour).
        // - Click on the desktop (wallpaper, not on any window AND not on the
        //   panel or dock) → unfocus all windows so the panel's focused-app
        //   text clears and keyboard focus is dropped.
        // - Click on panel/dock → leave focus alone (those are shell areas).
        if button == 0x110 && pressed {
            let (x, y) = self.pointer_pos;
            let panel_h = crate::wm::PANEL_HEIGHT as f64;
            let dock_h  = crate::wm::DOCK_HEIGHT as f64;
            let oh      = self.wm.output_h as f64;
            let in_panel = y < panel_h;
            let in_dock  = y >= oh - dock_h;
            if let Some(focused_surface) = self.wm.pointer_click_focus(x, y) {
                state.active_surface = Some(focused_surface.clone());
                if let Some(kb) = state.seat.get_keyboard() {
                    kb.set_focus(state, Some(focused_surface), SERIAL_COUNTER.next_serial());
                }
                if let Some(gpu_window) = self.gpu_window.as_ref() {
                    gpu_window.mark_dirty();
                }
            } else if !in_panel && !in_dock {
                // Click on desktop / wallpaper.
                if self.wm.unfocus_all() {
                    state.active_surface = None;
                    if let Some(kb) = state.seat.get_keyboard() {
                        kb.set_focus(state, None, SERIAL_COUNTER.next_serial());
                    }
                    // Push the new (un)focus state into the Slint model so
                    // the WindowChrome's `focused` property flips and its
                    // titlebar bg animates from active → inactive. Without
                    // this the model still carries the old focused=true
                    // and the titlebar stays at the active colour.
                    self.update_windows(state);
                    if let Some(gpu_window) = self.gpu_window.as_ref() {
                        gpu_window.mark_dirty();
                    }
                }
                // Left-click on the desktop also closes any open menu.
                if let Some(ui) = self.ui.as_ref() {
                    ui.set_desktop_menu_open(false);
                }
            }
        }

        // Right-click on the desktop opens our generic context menu.
        if button == 0x111 && pressed {
            let (x, y) = self.pointer_pos;
            let panel_h = crate::wm::PANEL_HEIGHT as f64;
            let oh      = self.wm.output_h as f64;
            let ow      = self.wm.output_w as f64;
            let dock_h  = crate::wm::DOCK_HEIGHT as f64;
            let on_window = self.wm.pointer_click_focus(x, y).is_some();
            let in_panel = y < panel_h;
            let in_dock  = y >= oh - dock_h;
            if !on_window && !in_panel && !in_dock {
                if let Some(ui) = self.ui.as_ref() {
                    // Clamp the menu position so the rect (220 × ~260)
                    // stays fully on screen — flips horizontally near the
                    // right edge and vertically near the bottom edge,
                    // matching macOS / GNOME behaviour.
                    let menu_w = 220.0;
                    let menu_h = 260.0;
                    let mx = if x + menu_w > ow { (x - menu_w).max(0.0) } else { x };
                    let my = if y + menu_h > oh - dock_h
                        { (y - menu_h).max(panel_h) }
                        else { y.max(panel_h) };
                    ui.set_desktop_menu_x(mx as i32);
                    ui.set_desktop_menu_y(my as i32);
                    ui.set_desktop_menu_open(true);
                    if let Some(gpu_window) = self.gpu_window.as_ref() {
                        gpu_window.mark_dirty();
                    }
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
        use smithay::wayland::compositor::with_states;
        use smithay::wayland::shell::xdg::XdgToplevelSurfaceData;

        let app_id_for = |surf: &WlSurface| -> Option<String> {
            with_states(surf, |states| {
                states.data_map.get::<XdgToplevelSurfaceData>()
                    .and_then(|d| d.lock().ok()?.app_id.clone())
            })
        };

        let focused_app_id: Option<String> = state
            .active_surface.as_ref().and_then(|s| app_id_for(s));

        // Collect every running app_id from the toplevel list.
        let mut running_ids: std::collections::HashSet<String> =
            std::collections::HashSet::new();
        for tl in &state.toplevels {
            if let Some(id) = app_id_for(&tl.surface) {
                if !id.is_empty() { running_ids.insert(id); }
            }
        }

        if running_ids == self.last_running_ids {
            return;
        }
        self.last_running_ids = running_ids.clone();

        let Some(ui) = self.ui.as_ref() else { return };

        let matches_pinned = |a: &str, b: &str| -> bool {
            a == b
                || a.rsplit('.').next() == Some(b)
                || b.rsplit('.').next() == Some(a)
        };

        let mut items: Vec<DockItem> = self
            .dock_entries
            .iter()
            .map(|entry| {
                let app_id_str = entry.item.app_id.as_str();
                let running = running_ids.iter().any(|rid| matches_pinned(rid, app_id_str));
                let focused = focused_app_id.as_deref()
                    .map_or(false, |fid| matches_pinned(fid, app_id_str));
                DockItem {
                    icon: entry.item.icon.clone(),
                    app_id: entry.item.app_id.clone(),
                    name:  entry.item.name.clone(),
                    running,
                    focused,
                    pinned: entry.item.pinned,
                }
            })
            .collect();

        // Append non-pinned running apps after the pinned ones.
        let pinned_ids: Vec<String> = self.dock_entries.iter()
            .map(|e| e.item.app_id.to_string()).collect();
        let mut extras: Vec<String> = running_ids.iter()
            .filter(|rid| !pinned_ids.iter().any(|pid| matches_pinned(rid, pid)))
            .cloned().collect();
        extras.sort();
        for app_id in &extras {
            let info = desktop::resolve(app_id);
            let icon = match info.icon.as_deref() {
                Some(p) => desktop::load_icon(p),
                None => desktop::generic_app_icon()
                    .map(|p| desktop::load_icon(&p))
                    .unwrap_or_default(),
            };
            let focused = focused_app_id.as_deref()
                .map_or(false, |fid| matches_pinned(fid, app_id));
            items.push(DockItem {
                icon,
                app_id: SharedString::from(app_id.as_str()),
                name:  SharedString::from(info.name.as_str()),
                running: true,
                focused,
                pinned: false,
            });
        }

        let model = std::rc::Rc::new(VecModel::from(items));
        ui.set_dock_items(slint::ModelRc::from(model));
        debug!("dock running state updated, focused={:?}, running={:?}",
            focused_app_id, running_ids);
    }

    // ──────────────────────────────────────────────────────────────────────────
    // Cursor + drag system
    // ──────────────────────────────────────────────────────────────────────────

    /// Build window rects from current state for hit-testing.
    /// Build hit-test rects from the WM's CONFIGURED window geometry — not
    /// the client buffer size. CSD clients (kitty in some configs, GTK,
    /// libadwaita) commit buffers larger than the visible window so they
    /// can paint their own shadow. Using `pix.width/height` here would
    /// extend resize/move zones into our compositor-drawn shadow band.
    fn window_rects(&self, state: &SpikeState) -> Vec<WindowRect> {
        let mut rects: Vec<WindowRect> = Vec::new();
        // Front-to-back: highest z_order first.
        let mut wins: Vec<&crate::wm::WindowState> = self.wm.windows.values().collect();
        wins.sort_by_key(|w| std::cmp::Reverse(w.z_order));
        for win in wins {
            // Skip windows the user can't actually click on.
            if win.closing { continue; }
            if win.minimized && win.anim.is_settled() { continue; }
            // Match this WM window to a state.toplevels index — needed for
            // find_toplevel_idx callers that index by toplevel position.
            let Some(idx) = state.toplevels.iter().position(|t| t.surface == win.surface) else { continue };
            let titlebar = if win.csd { 0.0 } else { TITLEBAR_HEIGHT };
            rects.push(WindowRect {
                id: idx as i32,
                x:  win.anim.current_x() as f64,
                y:  win.anim.current_y() as f64,
                w:  win.geom_w.max(1) as f64,
                h:  win.geom_h.max(1) as f64 + titlebar,
            });
        }
        rects
    }

    /// Find the topmost toplevel whose hit-rect contains (ptr_x, ptr_y).
    /// Returns the index into `state.toplevels`. Uses WM geometry, not
    /// buffer dimensions, so the resize-grab band stays inside the visible
    /// window even for CSD clients with shadow-padded buffers.
    fn find_toplevel_idx(&self, state: &SpikeState, ptr_x: f64, ptr_y: f64) -> Option<usize> {
        let mut wins: Vec<&crate::wm::WindowState> = self.wm.windows.values().collect();
        wins.sort_by_key(|w| std::cmp::Reverse(w.z_order));
        for win in wins {
            if win.closing { continue; }
            if win.minimized && win.anim.is_settled() { continue; }
            let wx = win.anim.current_x() as f64;
            let wy = win.anim.current_y() as f64;
            let ww = win.geom_w.max(1) as f64;
            let titlebar = if win.csd { 0.0 } else { TITLEBAR_HEIGHT };
            let wh = win.geom_h.max(1) as f64 + titlebar;
            if ptr_x >= wx - cursor::EDGE_ZONE
                && ptr_x < wx + ww + cursor::EDGE_ZONE
                && ptr_y >= wy - cursor::EDGE_ZONE
                && ptr_y < wy + wh + cursor::EDGE_ZONE
            {
                if let Some(idx) = state.toplevels.iter().position(|t| t.surface == win.surface) {
                    return Some(idx);
                }
            }
        }
        None
    }

    /// Called each pointer-motion event (after `pointer_pos` is updated).
    /// Updates cursor shape, starts drags, continues active drags.
    pub fn handle_pointer_update(&mut self, state: &mut SpikeState, x: f64, y: f64) {
        // Build window rects for hit-testing (topmost-first).
        let win_rects = self.window_rects(state);

        // If there's an active drag, handle it.
        if let Some(drag) = self.active_drag.clone() {
            match &drag {
                ActiveDrag::Resize { toplevel_idx, edge, start_geom, .. } => {
                    if let Some((mut nx, mut ny, mut nw, mut nh)) = resize::compute_resize(&drag, x, y) {
                        // ── Top-edge constraint ───────────────────────────
                        // Block the top edge from sliding under the panel.
                        // Adjust height so the bottom edge stays where the
                        // resize math wanted it.
                        let panel = crate::wm::PANEL_HEIGHT;
                        if matches!(*edge, resize::ResizeEdge::North
                                          | resize::ResizeEdge::NorthWest
                                          | resize::ResizeEdge::NorthEast)
                            && ny < panel
                        {
                            let desired_bottom = start_geom.y + start_geom.h;
                            ny = panel;
                            nh = (desired_bottom - ny).max(resize::MIN_WINDOW_SIZE);
                        }

                        // ── Bottom-edge dock-snap ─────────────────────────
                        // When the bottom edge crosses the dock-top line,
                        // engage a sticky snap. To break out (down OR up)
                        // the user must travel further than the threshold
                        // from the snap-engagement pointer position.
                        let dock_top = self.wm.output_h - crate::wm::DOCK_HEIGHT;
                        let edge_is_south = matches!(*edge,
                            resize::ResizeEdge::South
                            | resize::ResizeEdge::SouthEast
                            | resize::ResizeEdge::SouthWest,
                        );
                        if edge_is_south {
                            let raw_bottom = ny + nh;
                            // Snap engages when bottom reaches dock-top line.
                            let mut engaged_at = match &self.active_drag {
                                Some(ActiveDrag::Resize { dock_snap_engaged_at, .. }) => *dock_snap_engaged_at,
                                _ => None,
                            };
                            if engaged_at.is_none() && raw_bottom >= dock_top {
                                engaged_at = Some(y);
                            }
                            if let Some(snap_y) = engaged_at {
                                if (y - snap_y).abs() > resize::DOCK_SNAP_BREAK_THRESHOLD {
                                    // Break out — release snap; resize follows pointer freely.
                                    engaged_at = None;
                                } else {
                                    // Sticky: clamp bottom to dock-top line.
                                    nh = (dock_top - ny).max(resize::MIN_WINDOW_SIZE);
                                }
                            }
                            if let Some(ActiveDrag::Resize { dock_snap_engaged_at, .. })
                                = &mut self.active_drag
                            {
                                *dock_snap_engaged_at = engaged_at;
                            }
                        }

                        if let Some(tl) = state.toplevels.get_mut(*toplevel_idx) {
                            tl.x = nx;
                            tl.y = ny;
                            let surface = tl.surface.clone();
                            let cw = nw.max(resize::MIN_WINDOW_SIZE);
                            let ch = nh.max(resize::MIN_WINDOW_SIZE);

                            // Throttle configures to ~60Hz so we don't flood
                            // the client with size churn.
                            let now = std::time::Instant::now();
                            let should_send = match &mut self.active_drag {
                                Some(ActiveDrag::Resize { last_configure_at, .. }) => {
                                    let send = last_configure_at
                                        .map_or(true, |t| now.duration_since(t)
                                            >= std::time::Duration::from_millis(16));
                                    if send { *last_configure_at = Some(now); }
                                    send
                                }
                                _ => true,
                            };
                            if should_send {
                                let maybe_ts = state.xdg_shell_state
                                    .toplevel_surfaces().iter()
                                    .find(|ts| ts.wl_surface() == &surface).cloned();
                                if let Some(toplevel) = maybe_ts {
                                    use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;
                                    toplevel.with_pending_state(|s: &mut smithay::wayland::shell::xdg::ToplevelState| {
                                        s.size = Some((cw, ch).into());
                                        // Tell the client it's actively being
                                        // resized; many clients (kitty, gtk,
                                        // qt) gate their redraw path on this
                                        // and won't repaint at the new size
                                        // without it.
                                        s.states.set(xdg_toplevel::State::Resizing);
                                    });
                                    toplevel.send_configure();
                                    debug!("resize configure: {}×{} at ({},{})", cw, ch, nx, ny);
                                }
                            }
                            // Sync WM so Slint follows the drag.
                            if let Some(wm_id) = self.wm.id_for_surface(&surface) {
                                self.wm.set_geometry_by_id(wm_id, nx, ny, cw, ch);
                            }
                        }
                    }
                    self.update_windows(state);
                    if let Some(gpu_window) = self.gpu_window.as_ref() {
                        gpu_window.mark_dirty();
                    }
                    return; // Don't update cursor during resize drag.
                }
                ActiveDrag::Move { toplevel_idx, offset_x, offset_y, pending_unmaximize } => {
                    let (ox, oy) = (*offset_x, *offset_y);

                    // If this drag started on a maximized window, gate motion
                    // until the pointer travels past MAXIMIZED_DRAG_THRESHOLD
                    // — then unmaximize in place and let the move proceed.
                    if let Some((sx, sy)) = pending_unmaximize {
                        let dx = x - *sx;
                        let dy = y - *sy;
                        if (dx * dx + dy * dy).sqrt() < resize::MAXIMIZED_DRAG_THRESHOLD {
                            return;
                        }
                        let toplevel_idx = *toplevel_idx;
                        let surface = state.toplevels.get(toplevel_idx)
                            .map(|t| t.surface.clone());
                        if let Some(surface) = surface {
                            let wm_id_opt = self.wm.id_for_surface(&surface);
                            if let Some(wm_id) = wm_id_opt {
                                let restore = self.wm.windows.values()
                                    .find(|w| w.id == wm_id)
                                    .and_then(|w| w.pre_maximize);
                                if let Some((_rx, _ry, rw, rh)) = restore {
                                    let rel_x = ((x - 0.0) / self.wm.output_w as f64)
                                        .clamp(0.0, 1.0);
                                    let new_x = (x - rel_x * rw as f64) as i32;
                                    let new_y = (y - (crate::wm::TITLEBAR_HEIGHT / 2.0)) as i32;
                                    if let Some(win_mut) = self.wm.windows.values_mut()
                                        .find(|w| w.id == wm_id)
                                    {
                                        win_mut.start_unmaximize();
                                    }
                                    self.wm.set_geometry_by_id(wm_id, new_x, new_y, rw, rh);
                                    if let Some(tl_mut) = state.toplevels.get_mut(toplevel_idx) {
                                        tl_mut.x = new_x;
                                        tl_mut.y = new_y;
                                    }
                                    let maybe_ts = state.xdg_shell_state
                                        .toplevel_surfaces().iter()
                                        .find(|ts| ts.wl_surface() == &surface).cloned();
                                    if let Some(toplevel) = maybe_ts {
                                        toplevel.with_pending_state(
                                            |s: &mut smithay::wayland::shell::xdg::ToplevelState| {
                                                use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;
                                                s.size = Some((rw, rh).into());
                                                s.states.unset(xdg_toplevel::State::Maximized);
                                            },
                                        );
                                        toplevel.send_configure();
                                    }
                                    if let Some(ActiveDrag::Move { offset_x, offset_y, pending_unmaximize, .. })
                                        = &mut self.active_drag
                                    {
                                        *offset_x = x - new_x as f64;
                                        *offset_y = y - new_y as f64;
                                        *pending_unmaximize = None;
                                    }
                                }
                            }
                        }
                        // After unmaximize fires, fall through to the regular
                        // move path on the NEXT motion event — return now so
                        // the just-restored window settles into the spring
                        // before we start dragging it.
                        if let Some(gpu_window) = self.gpu_window.as_ref() {
                            gpu_window.mark_dirty();
                        }
                        return;
                    }

                    let mut maybe_wm_id: Option<i32> = None;
                    if let Some(tl) = state.toplevels.get_mut(*toplevel_idx) {
                        let nx = (x - ox) as i32;
                        // Clamp y so the window's titlebar can't slide under
                        // the panel (the top bar is sacred), but the bottom
                        // is unconstrained — the user explicitly wants to be
                        // able to drag windows down behind the dock; the
                        // dock's post-chrome re-blit makes them visually
                        // disappear behind it without us cutting them off.
                        let panel  = crate::wm::PANEL_HEIGHT;
                        let oh     = self.wm.output_h;
                        let raw_ny = (y - oy) as i32;
                        // Allow the window to push down so just its titlebar
                        // remains visible on screen.
                        let max_ny = (oh - 24).max(panel);
                        let ny     = raw_ny.clamp(panel, max_ny);
                        tl.x = nx;
                        tl.y = ny;
                        let surface = tl.surface.clone();
                        if let Some(wm_id) = self.wm.id_for_surface(&surface) {
                            self.wm.set_position_by_id(wm_id, nx, ny);
                            maybe_wm_id = Some(wm_id);
                        }
                        debug!("move: window #{} to ({},{})", toplevel_idx, nx, ny);
                    }

                    // Snap detection — show preview if cursor is in an edge band.
                    if let Some(ui) = self.ui.as_ref() {
                        let ow = self.wm.output_w;
                        let oh = self.wm.output_h;
                        match crate::snap::detect(x, y, ow, oh) {
                            Some((_zone, rect)) => {
                                ui.set_snap_preview_visible(true);
                                ui.set_snap_preview_x(rect.x);
                                ui.set_snap_preview_y(rect.y);
                                ui.set_snap_preview_w(rect.w);
                                ui.set_snap_preview_h(rect.h);
                                if let Some(id) = maybe_wm_id {
                                    self.pending_snap = Some((id, rect));
                                }
                            }
                            None => {
                                ui.set_snap_preview_visible(false);
                                self.pending_snap = None;
                            }
                        }
                    }

                    self.update_windows(state);
                    if let Some(gpu_window) = self.gpu_window.as_ref() {
                        gpu_window.mark_dirty();
                    }
                    return; // Don't update cursor during move drag.
                }
            }
        }

        // No active drag — run hit test to determine cursor and start drag on press.
        let hit = cursor::hit_test(x, y, &win_rects);
        let raw_zone = hit.map(|h| h.zone).unwrap_or(HitZone::None);

        // If the hovered window is maximized, suppress edge/corner zones so
        // the cursor stays a normal arrow / titlebar and a press never fires
        // a resize drag — maximized windows aren't user-resizable.
        let zone = if matches!(raw_zone,
            HitZone::EdgeNorth | HitZone::EdgeSouth | HitZone::EdgeEast | HitZone::EdgeWest
            | HitZone::CornerNW { .. } | HitZone::CornerNE { .. }
            | HitZone::CornerSW { .. } | HitZone::CornerSE { .. }
        ) {
            let idx_opt = self.find_toplevel_idx(state, x, y);
            let maximized = idx_opt
                .and_then(|idx| state.toplevels.get(idx))
                .and_then(|tl| self.wm.id_for_surface(&tl.surface))
                .and_then(|id| self.wm.windows.values().find(|w| w.id == id))
                .map_or(false, |w| w.maximized);
            if maximized { HitZone::None } else { raw_zone }
        } else {
            raw_zone
        };

        let new_cursor = cursor::zone_to_cursor(zone);

        if new_cursor != self.current_cursor {
            self.current_cursor = new_cursor;
            self.update_cursor_overlay();
        }

        // Start a drag if left button is down and we just detected it (drag init).
        if self.left_button_down && self.active_drag.is_none() {
            if let Some(ref hit_result) = hit {
                let idx_opt = self.find_toplevel_idx(state, x, y);
                if let Some(idx) = idx_opt {
                    // Win+drag — Super held + left-press anywhere → Move.
                    if self.super_held {
                        let maximized = self.wm.windows.values()
                            .find(|w| state.toplevels.get(idx)
                                .map_or(false, |t| self.wm.id_for_surface(&t.surface) == Some(w.id)))
                            .map_or(false, |w| w.maximized);
                        let (ox, oy) = self.maybe_unsnap_for_move(state, idx, x, y);
                        self.active_drag = Some(ActiveDrag::Move {
                            toplevel_idx: idx,
                            offset_x: ox,
                            offset_y: oy,
                            pending_unmaximize: if maximized { Some((x, y)) } else { None },
                        });
                        debug!("Win+drag move started on window #{} (maximized={})", idx, maximized);
                        let _ = hit_result;
                        return;
                    }
                    // Don't start a drag when the press lands on a control
                    // button — Slint's IconButton TouchArea will fire
                    // `clicked` and route through close-clicked /
                    // minimize-clicked / maximize-clicked. We just need to
                    // avoid hijacking the press into a move drag.
                    if matches!(zone,
                        HitZone::CloseButton
                        | HitZone::MinimizeButton
                        | HitZone::MaximizeButton)
                    {
                        let _ = hit_result;
                        return;
                    }
                    if let Some(edge) = ResizeEdge::from_zone(zone) {
                        let t = &state.toplevels[idx];
                        // Snapshot the window's logical content size from the
                        // WM, not from `pix.width/height`. Buffer dims are in
                        // physical pixels; xdg_toplevel.configure expects
                        // surface-local (logical) coords, so feeding buffer
                        // dims back to the client at non-1.0 scales would
                        // double the size each drag. The WM's per-window
                        // geometry is the authoritative logical size.
                        let (w, h) = self.wm.id_for_surface(&t.surface)
                            .and_then(|wm_id| self.wm.windows.values()
                                .find(|w| w.id == wm_id)
                                .map(|w| (w.w, w.h)))
                            .unwrap_or_else(|| {
                                let pix = t.pixels.lock().unwrap();
                                (pix.width as i32, pix.height as i32)
                            });
                        self.active_drag = Some(ActiveDrag::Resize {
                            toplevel_idx: idx,
                            edge,
                            start_ptr_x: x,
                            start_ptr_y: y,
                            start_geom: WindowGeomSnapshot { x: t.x, y: t.y, w, h },
                            last_configure_at: None,
                            dock_snap_engaged_at: None,
                        });
                        debug!("resize drag started: {:?} on window #{} from {}x{}", edge, idx, w, h);
                    } else if zone == HitZone::TitleBar {
                        let maximized = state.toplevels.get(idx)
                            .and_then(|t| self.wm.id_for_surface(&t.surface))
                            .and_then(|id| self.wm.windows.values().find(|w| w.id == id))
                            .map_or(false, |w| w.maximized);
                        let (ox, oy) = self.maybe_unsnap_for_move(state, idx, x, y);
                        self.active_drag = Some(ActiveDrag::Move {
                            toplevel_idx: idx,
                            offset_x: ox,
                            offset_y: oy,
                            pending_unmaximize: if maximized { Some((x, y)) } else { None },
                        });
                        debug!("move drag started on window #{} (maximized={})", idx, maximized);
                    }
                }
                let _ = hit_result;
            }
        }
    }

    /// Build the debug-overlay text dump (Super+I).
    fn build_debug_dump(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!("theme: {:?}  mode_t={:.2}\n",
            self.theme.current_mode, self.theme.mode_t));
        out.push_str(&format!("output: {}×{}\n", self.wm.output_w, self.wm.output_h));
        out.push_str(&format!("frames: {}  fps: {:.1}\n",
            self.frame_count, self.fps));
        out.push_str(&format!("cursor: {:?} at ({:.0},{:.0})\n",
            self.current_cursor, self.pointer_pos.0, self.pointer_pos.1));
        out.push_str(&format!("drag: {}\n", match &self.active_drag {
            None    => "none".to_string(),
            Some(d) => format!("{:?}", d),
        }));
        out.push_str(&format!("\nWindows ({}):\n", self.wm.windows.len()));
        let mut wins: Vec<&crate::wm::WindowState> = self.wm.windows.values().collect();
        wins.sort_by_key(|w| w.id);
        for w in &wins {
            out.push_str(&format!(
                "  #{}  z={}  {}×{}+{}+{}  focus={}  min={}  max={}  closing={}\n",
                w.id, w.z_order, w.w, w.h, w.x, w.y,
                w.focused, w.minimized, w.maximized, w.closing,
            ));
            out.push_str(&format!(
                "        opacity={:.2}  scale={:.2}  awaiting_first={}\n",
                w.anim.opacity.value_f32(), w.anim.scale.value_f32(),
                w.awaiting_first_render,
            ));
        }
        out.push_str("\nKeys: Super+T theme  Super+I debug  Super+W close  Super+M min\n");
        out.push_str("      Super+D show-desktop  Super+/ help  Esc dismiss\n");
        out
    }

    /// Swap the wallpaper to whichever per-mode file matches the current
    /// theme. No-op if the user only has one wallpaper file.
    pub fn swap_wallpaper_for_current_mode(&mut self) {
        let tag = match self.theme.current_mode {
            ThemeMode::Light => "light",
            ThemeMode::Dark  => "dark",
        };
        let Some(p) = wallpaper::find_wallpaper_for_mode(tag) else { return };
        if let Some(img) = wallpaper::load_from_path(&p) {
            if let Some(ui) = self.ui.as_ref() { ui.set_wallpaper(img); }
        }
        if let Some(b) = wallpaper::load_blurred_from_path(&p, 24.0) {
            if let Some(ui) = self.ui.as_ref() { ui.set_wallpaper_blurred(b); }
        }
        self.backdrop.load_wallpaper(&p);
        if let Some(gpu_window) = self.gpu_window.as_ref() { gpu_window.mark_dirty(); }
        info!("wallpaper swapped for mode {:?} → {:?}", self.theme.current_mode, p);
    }

    /// Refresh the panel/dock backdrop on a throttled cadence.
    pub fn refresh_backdrop(&mut self, state: &SpikeState) {
        let mut snapshots: Vec<(i32, i32, i32, i32, Vec<u8>, u32, u32)> = Vec::new();
        for win in self.wm.windows_sorted() {
            if win.minimized && win.anim.is_settled() { continue }
            let Some(tl) = state.toplevels.iter().find(|t| t.surface == win.surface) else { continue };
            let pix = tl.pixels.lock().unwrap();
            if pix.width == 0 || pix.height == 0 { continue }
            snapshots.push((
                win.anim.current_x(),
                win.anim.current_y(),
                win.anim.current_w(),
                win.anim.current_h(),
                pix.pixels.clone(),
                pix.width,
                pix.height,
            ));
        }
        let snaps: Vec<WindowSnapshot<'_>> = snapshots.iter().map(|t| WindowSnapshot {
            x: t.0, y: t.1, w: t.2, h: t.3,
            pixels: &t.4, buf_w: t.5, buf_h: t.6,
        }).collect();
        if let Some(img) = self.backdrop.try_synth(&snaps) {
            if let Some(ui) = self.ui.as_ref() { ui.set_wallpaper_blurred(img); }
        }
    }

    /// Clear active_drag, plus — if the released drag was a Resize — emit a
    /// final non-resizing xdg_toplevel.configure so the client knows the
    /// drag is over and can stop any "resize-active" optimisations (clients
    /// which gated their redraw on the `Resizing` state will now repaint).
    fn release_drag(&mut self, state: &mut SpikeState) {
        if let Some(ActiveDrag::Resize { toplevel_idx, .. }) = self.active_drag.clone() {
            if let Some(tl) = state.toplevels.get(toplevel_idx) {
                let surface = tl.surface.clone();
                let size = self.wm.id_for_surface(&surface)
                    .and_then(|id| self.wm.windows.values().find(|w| w.id == id))
                    .map(|w| (w.w, w.h));
                if let Some((cw, ch)) = size {
                    let maybe_ts = state.xdg_shell_state.toplevel_surfaces().iter()
                        .find(|ts| ts.wl_surface() == &surface).cloned();
                    if let Some(toplevel) = maybe_ts {
                        use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;
                        toplevel.with_pending_state(|s: &mut smithay::wayland::shell::xdg::ToplevelState| {
                            s.size = Some((cw, ch).into());
                            s.states.unset(xdg_toplevel::State::Resizing);
                        });
                        toplevel.send_configure();
                        debug!("resize-end configure: {}×{} (Resizing cleared)", cw, ch);
                    }
                }
            }
        }
        self.active_drag = None;
    }

    /// If the toplevel is currently snapped, restore it to its pre-snap rect
    /// at a position that keeps the cursor proportionally on the titlebar.
    /// Returns the (offset_x, offset_y) for the resulting Move drag.
    fn maybe_unsnap_for_move(&mut self, state: &mut SpikeState, idx: usize, x: f64, y: f64) -> (f64, f64) {
        let t = match state.toplevels.get(idx) {
            Some(t) => t,
            None => return (0.0, 0.0),
        };
        let mut start_x = t.x as f64;
        let mut start_y = t.y as f64;
        let surface = t.surface.clone();

        let Some(wm_id) = self.wm.id_for_surface(&surface) else {
            return (x - start_x, y - start_y);
        };

        let restore = self.wm.windows.values()
            .find(|w| w.id == wm_id)
            .and_then(|w| w.pre_snap.map(|p| (p, w.x as f64, w.w as f64)));

        if let Some(((_ox, _oy, ow, oh), cur_x, cur_w)) = restore {
            let rel_x = ((x - cur_x) / cur_w.max(1.0)).clamp(0.0, 1.0);
            let new_x = (x - rel_x * ow as f64) as i32;
            let new_y = (y - (TITLEBAR_HEIGHT / 2.0)) as i32;

            self.wm.set_geometry_by_id(wm_id, new_x, new_y, ow, oh);
            if let Some(win_mut) = self.wm.windows.values_mut().find(|w| w.id == wm_id) {
                win_mut.pre_snap = None;
            }
            if let Some(tl_mut) = state.toplevels.get_mut(idx) {
                tl_mut.x = new_x;
                tl_mut.y = new_y;
            }
            let maybe_ts = state.xdg_shell_state.toplevel_surfaces().iter()
                .find(|ts| ts.wl_surface() == &surface).cloned();
            if let Some(toplevel) = maybe_ts {
                toplevel.with_pending_state(|s: &mut smithay::wayland::shell::xdg::ToplevelState| {
                    s.size = Some((ow, oh).into());
                });
                toplevel.send_configure();
                debug!("snap-out: restored to {}×{} at ({},{})", ow, oh, new_x, new_y);
            }
            start_x = new_x as f64;
            start_y = new_y as f64;
        }
        (x - start_x, y - start_y)
    }

    /// Drain and dispatch IPC commands from the unix-socket server.
    pub fn process_ipc_commands(&mut self, state: &mut SpikeState) {
        let cmds: Vec<IpcCommand> = {
            let mut q = self.pending_ipc.lock().unwrap();
            std::mem::take(&mut *q)
        };
        for cmd in cmds {
            self.dispatch_ipc(state, cmd);
        }
    }

    fn dispatch_ipc(&mut self, state: &mut SpikeState, cmd: IpcCommand) {
        match cmd {
            IpcCommand::PointerMove { x, y } => {
                self.pointer_pos = (x, y);
                self.update_cursor_position(x, y);
                self.handle_pointer_update(state, x, y);
                self.forward_pointer_motion(state, x, y);
                if let Some(gw) = self.gpu_window.as_ref() { gw.mark_dirty(); }
            }
            IpcCommand::PointerButton { button_evdev, pressed } => {
                if button_evdev == 0x110 {
                    self.left_button_down = pressed;
                    if !pressed {
                        self.apply_pending_snap(state);
                        self.release_drag(state);
                    } else {
                        let (x, y) = self.pointer_pos;
                        self.handle_pointer_update(state, x, y);
                    }
                }
                self.forward_pointer_button(state, button_evdev, pressed);
            }
            IpcCommand::KeyEvent { scancode, pressed } => {
                self.pending_keys.lock().unwrap().push_back(PendingKeyEvent { scancode, pressed });
            }
            IpcCommand::TypeText { text } => {
                let mut keys = self.pending_keys.lock().unwrap();
                for ch in text.chars() {
                    if let Some((sc, shift)) = ascii_to_scancode(ch) {
                        if shift {
                            keys.push_back(PendingKeyEvent { scancode: 42, pressed: true });
                        }
                        keys.push_back(PendingKeyEvent { scancode: sc, pressed: true });
                        keys.push_back(PendingKeyEvent { scancode: sc, pressed: false });
                        if shift {
                            keys.push_back(PendingKeyEvent { scancode: 42, pressed: false });
                        }
                    }
                }
            }
            IpcCommand::Screenshot { save_path } => {
                if let Err(e) = self.save_screenshot(&save_path) {
                    warn!("ipc: screenshot failed: {}", e);
                } else {
                    info!("ipc: screenshot saved to {}", save_path);
                }
            }
            IpcCommand::ActivateWindow { wm_id } => {
                self.pending_activate.lock().unwrap().push_back(wm_id);
            }
            IpcCommand::CloseWindow { wm_id } => {
                self.pending_close.lock().unwrap().push_back(wm_id);
            }
            IpcCommand::MinimizeWindow { wm_id } => {
                self.pending_minimize.lock().unwrap().push_back(wm_id);
            }
            IpcCommand::MoveWindow { wm_id, x, y } => {
                self.wm.set_position_by_id(wm_id, x, y);
                if let Some(surf) = self.wm.windows.values().find(|w| w.id == wm_id).map(|w| w.surface.clone()) {
                    if let Some(tl) = state.toplevels.iter_mut().find(|t| t.surface == surf) {
                        tl.x = x; tl.y = y;
                    }
                }
                if let Some(gw) = self.gpu_window.as_ref() { gw.mark_dirty(); }
            }
            IpcCommand::ResizeWindow { wm_id, w, h } => {
                let pos = self.wm.windows.values().find(|win| win.id == wm_id)
                    .map(|win| (win.x, win.y));
                if let Some((wx, wy)) = pos {
                    self.wm.set_geometry_by_id(wm_id, wx, wy, w, h);
                }
                if let Some(surf) = self.wm.windows.values().find(|win| win.id == wm_id).map(|win| win.surface.clone()) {
                    let maybe_ts = state.xdg_shell_state.toplevel_surfaces().iter()
                        .find(|ts| ts.wl_surface() == &surf).cloned();
                    if let Some(toplevel) = maybe_ts {
                        toplevel.with_pending_state(|s: &mut smithay::wayland::shell::xdg::ToplevelState| {
                            s.size = Some((w, h).into());
                        });
                        toplevel.send_configure();
                    }
                }
                if let Some(gw) = self.gpu_window.as_ref() { gw.mark_dirty(); }
            }
            IpcCommand::DumpWindows { save_path } => {
                let mut entries: Vec<serde_json::Value> = Vec::new();
                for win in self.wm.windows_sorted() {
                    entries.push(serde_json::json!({
                        "id": win.id,
                        "title": win.title,
                        "x": win.x, "y": win.y, "w": win.w, "h": win.h,
                        "focused": win.focused,
                        "minimized": win.minimized,
                        "maximized": win.maximized,
                        "closing": win.closing,
                        "anim_opacity": win.anim.opacity.value_f32(),
                        "anim_scale":   win.anim.scale.value_f32(),
                        "awaiting_first_render": win.awaiting_first_render,
                    }));
                }
                let s = serde_json::to_string_pretty(&entries).unwrap_or_default();
                if let Err(e) = std::fs::write(&save_path, s) {
                    warn!("ipc: dump-windows write failed: {}", e);
                }
            }
            IpcCommand::SetTheme { mode } => {
                let want = match mode.to_ascii_lowercase().as_str() {
                    "light" => ThemeMode::Light,
                    _       => ThemeMode::Dark,
                };
                if self.theme.current_mode != want {
                    self.theme.toggle_mode();
                    self.apply_theme_to_slint();
                    self.swap_wallpaper_for_current_mode();
                }
            }
        }
    }

    /// Read the current Slint render texture back to CPU and write it as PNG.
    /// Used by the `ipc::Screenshot` command.
    pub fn save_screenshot(&self, save_path: &str) -> anyhow::Result<()> {
        // Read final_texture (post-chrome) so the screenshot matches what's
        // on display. Falls back to render_texture if final isn't allocated.
        let render_tex = self.final_texture.as_ref()
            .or(self.render_texture.as_ref())
            .ok_or_else(|| anyhow::anyhow!("no render texture yet"))?;
        let gpu = self.gpu_window.as_ref()
            .ok_or_else(|| anyhow::anyhow!("no gpu adapter"))?;
        let device = &gpu.wgpu_device;
        let queue  = &gpu.wgpu_queue;
        let (w, h) = self.render_texture_size;

        // wgpu requires bytes_per_row to be a multiple of 256.
        let unpadded = 4 * w;
        let bytes_per_row = (unpadded + 255) & !255;
        let buf_size = (bytes_per_row * h) as u64;

        let buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("screenshot-readback"),
            size: buf_size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("screenshot-encoder"),
        });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: render_tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buf,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(bytes_per_row),
                    rows_per_image: Some(h),
                },
            },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
        queue.submit(std::iter::once(encoder.finish()));

        // Block until the copy is done + the buffer is mappable.
        let slice = buf.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| { let _ = tx.send(r); });
        device.poll(wgpu::PollType::wait_indefinitely()).ok();
        rx.recv().map_err(|e| anyhow::anyhow!("map_async channel: {}", e))?
            .map_err(|e| anyhow::anyhow!("map_async failed: {:?}", e))?;
        let data = slice.get_mapped_range();

        // Strip per-row padding into a tightly packed RGBA buffer.
        let mut rgba = Vec::with_capacity((unpadded * h) as usize);
        for y in 0..h {
            let row_start = (y * bytes_per_row) as usize;
            rgba.extend_from_slice(&data[row_start..row_start + unpadded as usize]);
        }
        drop(data);
        buf.unmap();

        // Some swapchain formats are BGRA — swap channels if needed so PNG looks right.
        if matches!(self.swapchain_format, Some(wgpu::TextureFormat::Bgra8Unorm))
        {
            for px in rgba.chunks_exact_mut(4) {
                px.swap(0, 2);
            }
        }

        image::save_buffer(
            save_path, &rgba, w, h, image::ColorType::Rgba8,
        ).map_err(|e| anyhow::anyhow!("save_buffer: {}", e))?;
        Ok(())
    }

    /// Apply any pending snap rect committed by the last Move drag. Called
    /// from the main loop on left-button release.
    fn apply_pending_snap(&mut self, state: &mut SpikeState) {
        if let Some(ui) = self.ui.as_ref() {
            ui.set_snap_preview_visible(false);
        }
        let Some((wm_id, rect)) = self.pending_snap.take() else { return };

        let surface = self.wm.windows.values()
            .find(|w| w.id == wm_id)
            .map(|w| w.surface.clone());
        let Some(surface) = surface else { return };

        if let Some(win) = self.wm.windows.values_mut().find(|w| w.id == wm_id) {
            // Save pre-snap rect (only on the FIRST snap — re-snapping
            // preserves the original unsnapped geometry).
            if win.pre_snap.is_none() {
                win.pre_snap = Some((win.x, win.y, win.w, win.h));
            }
            win.x = rect.x;
            win.y = rect.y;
            win.w = rect.w;
            win.h = rect.h;
            win.anim.set_geometry_target(rect.x, rect.y, rect.w, rect.h);
        }

        let maybe_ts = state.xdg_shell_state.toplevel_surfaces().iter()
            .find(|ts| ts.wl_surface() == &surface).cloned();
        if let Some(toplevel) = maybe_ts {
            toplevel.with_pending_state(|s: &mut smithay::wayland::shell::xdg::ToplevelState| {
                s.size = Some((rect.w, rect.h).into());
            });
            toplevel.send_configure();
            debug!("snap: configure {}×{} at ({},{})", rect.w, rect.h, rect.x, rect.y);
        }
        for tl in state.toplevels.iter_mut() {
            if tl.surface == surface {
                tl.x = rect.x;
                tl.y = rect.y;
                break;
            }
        }
        if let Some(gpu_window) = self.gpu_window.as_ref() { gpu_window.mark_dirty(); }
    }

    /// Push the current cursor image to the Slint UI.
    fn update_cursor_overlay(&mut self) {
        let Some(ui) = self.ui.as_ref() else { return };
        let kind = self.current_cursor;
        let img = self.cursor_renderer.get(kind);
        let (hx, hy) = self.cursor_renderer.hotspot(kind);
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

/// Build a 42-cell flat calendar grid (6 rows × 7 cols, Monday-first).
/// Pads the start with prev-month trail days and the end with next-month
/// trail days so every cell has a number — `is_other_month` flags the
/// padding for muted rendering.
fn build_calendar_grid(today: chrono::NaiveDate) -> Vec<crate::CalendarDay> {
    use chrono::{Datelike, NaiveDate};
    let year = today.year();
    let month = today.month();
    let first = NaiveDate::from_ymd_opt(year, month, 1).expect("valid month");
    // chrono Mon=0..Sun=6 — matches our Mon-first layout.
    let lead = first.weekday().num_days_from_monday() as i64;
    let days_in_month: u32 = {
        let next_month = if month == 12 {
            NaiveDate::from_ymd_opt(year + 1, 1, 1)
        } else {
            NaiveDate::from_ymd_opt(year, month + 1, 1)
        }.expect("valid next month");
        (next_month - first).num_days() as u32
    };
    let mut cells = Vec::with_capacity(42);
    // Prev-month trail.
    let prev_last = first - chrono::Duration::days(1);
    let prev_total = prev_last.day();
    for i in 0..lead {
        let day = prev_total - lead as u32 + 1 + i as u32;
        cells.push(crate::CalendarDay {
            day_num: day as i32,
            is_today: false,
            is_other_month: true,
        });
    }
    // This month.
    for d in 1..=days_in_month {
        cells.push(crate::CalendarDay {
            day_num: d as i32,
            is_today: d == today.day(),
            is_other_month: false,
        });
    }
    // Next-month trail to fill 42 cells.
    let mut trail = 1u32;
    while cells.len() < 42 {
        cells.push(crate::CalendarDay {
            day_num: trail as i32,
            is_today: false,
            is_other_month: true,
        });
        trail += 1;
    }
    cells
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
                name:   SharedString::from(info.name.as_str()),
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
                    name:   SharedString::from(info.name.as_str()),
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

    // Wallpaper. If the resolved wallpaper is named *_light.* we also default
    // the theme to Light so the chrome matches the desktop's overall mood.
    let wallpaper_path = wallpaper::find_wallpaper_path();
    let mut start_in_light_mode = false;
    match wallpaper_path.as_ref().and_then(|p| wallpaper::load_from_path(p)) {
        Some(img) => {
            info!("setting wallpaper image");
            ui.set_wallpaper(img);
            if let Some(p) = wallpaper_path.as_ref() {
                if wallpaper::looks_light(p) {
                    start_in_light_mode = true;
                    info!("wallpaper looks light → defaulting theme to Light");
                }
            }
        }
        None => {
            warn!("no wallpaper found on disk, using default #1e1e2e background");
        }
    }
    if let Some(blurred) = wallpaper_path.as_ref()
        .and_then(|p| wallpaper::load_blurred_from_path(p, 24.0))
    {
        ui.set_wallpaper_blurred(blurred);
    }
    if start_in_light_mode {
        ui.set_theme_mode(TokenMode::Light);
    }

    // Dock entries.
    let dock_entries = load_dock_entries();
    info!("loaded {} dock entries", dock_entries.len());
    {
        let items: Vec<DockItem> = dock_entries.iter().map(|e| e.item.clone()).collect();
        let model = std::rc::Rc::new(VecModel::from(items));
        ui.set_dock_items(slint::ModelRc::from(model));
    }

    // Desktop right-click menu — static, themed entries. Click handler logs
    // the chosen id; specific actions are wired in `dispatch_desktop_menu`.
    {
        use crate::MenuItem;
        let items = vec![
            MenuItem { id: 1, label: SharedString::from("New Folder"),
                accelerator: SharedString::from("⇧⌘N"), separator: false, enabled: true },
            MenuItem { id: 2, label: SharedString::from("Get Info"),
                accelerator: SharedString::from("⌘I"), separator: false, enabled: false },
            MenuItem { id: -1, label: SharedString::default(),
                accelerator: SharedString::default(), separator: true, enabled: false },
            MenuItem { id: 3, label: SharedString::from("Change Wallpaper…"),
                accelerator: SharedString::default(), separator: false, enabled: true },
            MenuItem { id: 4, label: SharedString::from("Toggle Theme"),
                accelerator: SharedString::from("⌃T"), separator: false, enabled: true },
            MenuItem { id: -1, label: SharedString::default(),
                accelerator: SharedString::default(), separator: true, enabled: false },
            MenuItem { id: 5, label: SharedString::from("Show Debug Overlay"),
                accelerator: SharedString::from("⌃I"), separator: false, enabled: true },
            MenuItem { id: 6, label: SharedString::from("About this DE"),
                accelerator: SharedString::default(), separator: false, enabled: true },
        ];
        let model = std::rc::Rc::new(VecModel::from(items));
        ui.set_desktop_menu_items(slint::ModelRc::from(model));
    }
    {
        let weak = ui.as_weak();
        ui.on_desktop_menu_clicked(move |id| {
            tracing::info!("desktop-menu: id={}", id);
            if let Some(ui) = weak.upgrade() {
                match id {
                    4 => {
                        // Toggle theme is exposed as the existing UI callback.
                        ui.invoke_toggle_theme();
                    }
                    5 => {
                        ui.set_debug_overlay_visible(!ui.get_debug_overlay_visible());
                    }
                    _ => {}
                }
            }
        });
    }

    // Dock per-app context menu — Open New Window, Show All Windows, Quit.
    {
        use crate::MenuItem;
        let items = vec![
            MenuItem { id: 1, label: SharedString::from("Open New Window"),
                accelerator: SharedString::default(), separator: false, enabled: true },
            MenuItem { id: 2, label: SharedString::from("Show All Windows"),
                accelerator: SharedString::default(), separator: false, enabled: true },
            MenuItem { id: -1, label: SharedString::default(),
                accelerator: SharedString::default(), separator: true, enabled: false },
            MenuItem { id: 3, label: SharedString::from("Keep in Dock"),
                accelerator: SharedString::default(), separator: false, enabled: false },
            MenuItem { id: 4, label: SharedString::from("Show in Files"),
                accelerator: SharedString::default(), separator: false, enabled: false },
            MenuItem { id: -1, label: SharedString::default(),
                accelerator: SharedString::default(), separator: true, enabled: false },
            MenuItem { id: 5, label: SharedString::from("Quit"),
                accelerator: SharedString::from("⌘Q"), separator: false, enabled: true },
        ];
        let model = std::rc::Rc::new(VecModel::from(items));
        ui.set_dock_menu_items(slint::ModelRc::from(model));
    }
    // dock-menu-clicked is wired AFTER `exec_map` is built (just below).

    // Launch-app callback.
    let exec_map: std::collections::HashMap<String, String> = dock_entries
        .iter()
        .map(|e| (e.item.app_id.to_string(), e.exec.clone()))
        .collect();
    let exec_map = Arc::new(exec_map);

    // launch-app is wired AFTER the wayland socket exists (further down in
    // this fn) so spawned processes inherit OUR WAYLAND_DISPLAY rather than
    // the host's.

    // WM action callbacks — queue into the pending queues so the main loop
    // can process them with access to SpikeState.
    let pending_close: Arc<Mutex<VecDeque<i32>>> = Arc::new(Mutex::new(VecDeque::new()));
    let pending_minimize: Arc<Mutex<VecDeque<i32>>> = Arc::new(Mutex::new(VecDeque::new()));
    let pending_maximize: Arc<Mutex<VecDeque<i32>>> = Arc::new(Mutex::new(VecDeque::new()));
    let pending_activate: Arc<Mutex<VecDeque<i32>>> = Arc::new(Mutex::new(VecDeque::new()));

    // Dock context-menu actions — actions 2 & 5 need access to the WM
    // (via SpikeState in the main loop), so we push them onto a queue.
    let pending_dock_action: Arc<Mutex<VecDeque<(String, i32)>>> =
        Arc::new(Mutex::new(VecDeque::new()));

    // App launcher submit — match the typed query against any pinned app's
    // id or display name (case-insensitive prefix); fall back to running
    // the query verbatim through `sh -c` so users can also dispatch any
    // command-line invocation via Super+Space.
    {
        let dock_entries_q = dock_entries.iter()
            .map(|e| (e.item.app_id.to_string(), e.item.name.to_string(), e.exec.clone()))
            .collect::<Vec<_>>();
        ui.on_launcher_submit(move |q| {
            let query = q.trim().to_string();
            if query.is_empty() { return; }
            let lq = query.to_lowercase();
            // Find a pinned-app match by id or name prefix.
            let exec = dock_entries_q.iter().find_map(|(id, name, exec)| {
                if id.to_lowercase().contains(&lq)
                   || name.to_lowercase().contains(&lq)
                {
                    Some(exec.clone())
                } else { None }
            }).unwrap_or(query);
            tracing::info!("launcher: launching {:?}", exec);
            let _ = std::process::Command::new("sh")
                .arg("-c").arg(&exec).spawn();
        });
    }

    // StatusNotifierItem clicks — fire `Activate` / `ContextMenu` on the
    // item's DBus interface. We pull (service, object-path) straight off
    // the Slint model so the click closure doesn't need a separate id →
    // endpoint lookup; tray::activate / context_menu run the DBus call
    // on a one-shot worker thread.
    {
        let weak = ui.as_weak();
        ui.on_tray_clicked(move |id| {
            let Some(ui) = weak.upgrade() else { return };
            let model = ui.get_tray_items();
            for i in 0..model.row_count() {
                if let Some(it) = model.row_data(i) {
                    if it.id == id {
                        crate::tray::activate(
                            it.service.to_string(),
                            it.object_path.to_string(),
                            0, 0,
                        );
                        break;
                    }
                }
            }
        });
    }
    {
        let weak = ui.as_weak();
        ui.on_tray_right_clicked(move |id| {
            let Some(ui) = weak.upgrade() else { return };
            let model = ui.get_tray_items();
            for i in 0..model.row_count() {
                if let Some(it) = model.row_data(i) {
                    if it.id == id {
                        crate::tray::context_menu(
                            it.service.to_string(),
                            it.object_path.to_string(),
                            0, 0,
                        );
                        break;
                    }
                }
            }
        });
    }
    {
        let exec_map_q = exec_map.clone();
        let q = pending_dock_action.clone();
        ui.on_dock_menu_clicked(move |app_id, action| {
            tracing::info!("dock-menu: app={} action={}", app_id, action);
            match action {
                1 => {
                    // Open New Window — fire the .desktop exec directly.
                    if let Some(exec) = exec_map_q.get(app_id.as_str()) {
                        let _ = std::process::Command::new("sh")
                            .arg("-c").arg(exec).spawn();
                    }
                }
                _ => {
                    // Defer to the main loop so we can read/mutate the WM.
                    q.lock().unwrap().push_back((app_id.to_string(), action));
                }
            }
        });
    }

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

    // Theme toggle from the control-centre tile. Slint fires; main loop
    // picks it up via this Arc<Mutex<bool>>.
    let pending_theme_toggle: Arc<Mutex<bool>> = Arc::new(Mutex::new(false));
    {
        let flag = pending_theme_toggle.clone();
        ui.on_toggle_theme(move || {
            *flag.lock().unwrap() = true;
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

    // Wire launch-app now that the socket exists. Spawned apps inherit OUR
    // WAYLAND_DISPLAY so they connect to this compositor, not the host.
    {
        let exec_map = exec_map.clone();
        let socket_name = socket_name.clone();
        ui.on_launch_app(move |app_id| {
            let app = app_id.to_string();
            info!("launch-app({})", app);
            let exec_line = exec_map.get(&app).cloned().unwrap_or_else(|| {
                let info = desktop::resolve(&app);
                info.exec
            });
            info!("  exec: {} (WAYLAND_DISPLAY={})", exec_line, socket_name.to_string_lossy());
            let _ = std::process::Command::new("setsid")
                .args(["-f", "sh", "-c", &exec_line])
                .env("WAYLAND_DISPLAY", &socket_name)
                .env("XDG_SESSION_TYPE", "wayland")
                .env_remove("DISPLAY")
                .spawn();
        });
    }

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

    // Sync the Rust ThemeState with the wallpaper-driven default mode so
    // the first user toggle behaves as expected.
    if start_in_light_mode {
        app.theme.set_mode(ThemeMode::Light);
        app.theme.tick();
    }

    // Prime the CPU backdrop synth with the active wallpaper.
    if let Some(p) = wallpaper_path.as_ref() {
        app.backdrop.load_wallpaper(p);
    }

    // IPC server — out-of-process drivers can move the pointer, type, take
    // screenshots, list windows, etc. via $XDG_RUNTIME_DIR/myDE.sock.
    ipc_server::spawn(None, app.pending_ipc.clone());

    // StatusNotifierWatcher (AppIndicator host) — spawns a worker thread
    // running tokio. Apps that integrate appindicator-style tray icons
    // (Slack, Discord, Steam, network managers, …) register here and we
    // surface them on the panel.
    let mut tray_events = crate::tray::spawn_tray_host();

    // Initialise cursor overlay with the default arrow cursor.
    app.update_cursor_overlay();

    // Wire the pending WM-action queues from Slint callbacks into the app.
    app.pending_close    = pending_close;
    app.pending_minimize = pending_minimize;
    app.pending_maximize = pending_maximize;
    app.pending_activate = pending_activate;
    app.pending_dock_action = pending_dock_action;

    // 7. Main loop.
    info!("Entering GPU compositor main loop");
    loop {
        match winit_event_loop.pump_app_events(Some(Duration::from_millis(1)), &mut app) {
            PumpStatus::Exit(_) => { info!("winit exited"); break; }
            PumpStatus::Continue => {}
        }

        calloop.dispatch(Some(Duration::ZERO), &mut state)
            .context("calloop dispatch error")?;

        // Drain any tray-host events accumulated since last tick; the
        // worker thread sends Added/Removed when apps register through
        // the StatusNotifierWatcher. When the set changes, rebuild the
        // panel's tray-items model with resolved icons.
        let mut tray_changed = false;
        while let Ok(ev) = tray_events.try_recv() {
            match ev {
                crate::tray::TrayEvent::Added(item) => {
                    info!("tray: + {} ({})", item.title, item.service);
                    app.tray_items.push(item);
                    tray_changed = true;
                }
                crate::tray::TrayEvent::Removed(id) => {
                    info!("tray: - id={}", id);
                    app.tray_items.retain(|it| it.id != id);
                    tray_changed = true;
                }
            }
        }
        if tray_changed {
            if let Some(ui) = app.ui.as_ref() {
                let items: Vec<crate::TrayIconItem> = app.tray_items.iter()
                    .map(|t| crate::TrayIconItem {
                        id: t.id as i32,
                        title: SharedString::from(t.title.as_str()),
                        icon_name: SharedString::from(t.icon_name.as_str()),
                        icon: if t.icon_name.is_empty() {
                            slint::Image::default()
                        } else {
                            // Resolve by icon-theme name (e.g. "telegram",
                            // "discord") via the existing desktop helper.
                            desktop::load_icon_by_name(&t.icon_name)
                                .unwrap_or_default()
                        },
                        service: SharedString::from(t.service.as_str()),
                        object_path: SharedString::from(t.object_path.as_str()),
                    })
                    .collect();
                let model = std::rc::Rc::new(VecModel::from(items));
                ui.set_tray_items(slint::ModelRc::from(model));
            }
        }

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
        // Throttled CPU backdrop synth (samples wallpaper + window content).
        app.refresh_backdrop(&state);
        // Drain IPC commands posted by external drivers.
        app.process_ipc_commands(&mut state);

        // Theme toggle requested from the control-centre tile.
        {
            let mut flag = pending_theme_toggle.lock().unwrap();
            if *flag {
                *flag = false;
                app.theme.toggle_mode();
                app.apply_theme_to_slint();
                app.swap_wallpaper_for_current_mode();
                info!("control-centre: toggled theme to {:?}", app.theme.current_mode);
            }
        }

        // Theme tick — advance mode_t + per-window focus_t animations.
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
                        if button == 0x110 && pressed {
                            let (x, y) = app.pointer_pos;
                            app.handle_pointer_update(&mut state, x, y);
                        }
                        // Apply any pending snap on left release before
                        // forwarding (so the configure goes out alongside),
                        // then end any active drag — `release_drag` clears
                        // active_drag and, for resize drags, sends a final
                        // xdg_toplevel.configure with Resizing unset.
                        if button == 0x110 && !pressed {
                            app.apply_pending_snap(&mut state);
                            app.release_drag(&mut state);
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

/// Map an ASCII character to its Linux evdev scancode + whether SHIFT is required.
/// Used by the IPC `TypeText` command. Coverage: a-z, A-Z, 0-9, common punctuation,
/// space, newline, tab. Returns None for chars we don't know how to type.
fn ascii_to_scancode(ch: char) -> Option<(u32, bool)> {
    Some(match ch {
        // letters (lowercase + shifted uppercase)
        'a' => (30, false), 'A' => (30, true),
        'b' => (48, false), 'B' => (48, true),
        'c' => (46, false), 'C' => (46, true),
        'd' => (32, false), 'D' => (32, true),
        'e' => (18, false), 'E' => (18, true),
        'f' => (33, false), 'F' => (33, true),
        'g' => (34, false), 'G' => (34, true),
        'h' => (35, false), 'H' => (35, true),
        'i' => (23, false), 'I' => (23, true),
        'j' => (36, false), 'J' => (36, true),
        'k' => (37, false), 'K' => (37, true),
        'l' => (38, false), 'L' => (38, true),
        'm' => (50, false), 'M' => (50, true),
        'n' => (49, false), 'N' => (49, true),
        'o' => (24, false), 'O' => (24, true),
        'p' => (25, false), 'P' => (25, true),
        'q' => (16, false), 'Q' => (16, true),
        'r' => (19, false), 'R' => (19, true),
        's' => (31, false), 'S' => (31, true),
        't' => (20, false), 'T' => (20, true),
        'u' => (22, false), 'U' => (22, true),
        'v' => (47, false), 'V' => (47, true),
        'w' => (17, false), 'W' => (17, true),
        'x' => (45, false), 'X' => (45, true),
        'y' => (21, false), 'Y' => (21, true),
        'z' => (44, false), 'Z' => (44, true),
        // digits + shifted symbols (US layout)
        '1' => (2, false),  '!' => (2, true),
        '2' => (3, false),  '@' => (3, true),
        '3' => (4, false),  '#' => (4, true),
        '4' => (5, false),  '$' => (5, true),
        '5' => (6, false),  '%' => (6, true),
        '6' => (7, false),  '^' => (7, true),
        '7' => (8, false),  '&' => (8, true),
        '8' => (9, false),  '*' => (9, true),
        '9' => (10, false), '(' => (10, true),
        '0' => (11, false), ')' => (11, true),
        // punctuation
        '-'  => (12, false), '_' => (12, true),
        '='  => (13, false), '+' => (13, true),
        '['  => (26, false), '{' => (26, true),
        ']'  => (27, false), '}' => (27, true),
        '\\' => (43, false), '|' => (43, true),
        ';'  => (39, false), ':' => (39, true),
        '\'' => (40, false), '"' => (40, true),
        '`'  => (41, false), '~' => (41, true),
        ','  => (51, false), '<' => (51, true),
        '.'  => (52, false), '>' => (52, true),
        '/'  => (53, false), '?' => (53, true),
        // whitespace
        ' '  => (57, false),  // space
        '\n' => (28, false),  // enter
        '\t' => (15, false),  // tab
        _ => return None,
    })
}
