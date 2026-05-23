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
    cell::RefCell,
    collections::{HashSet, VecDeque},
    rc::Rc,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use tracing::{debug, info, warn};

use smithay::{
    output::{Mode, Output, PhysicalProperties, Subpixel},
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    utils::{Transform, SERIAL_COUNTER},
};

use slint::{ComponentHandle, LogicalPosition, Model, SharedString, VecModel};

use winit::{
    application::ApplicationHandler,
    dpi::LogicalSize,
    event::{ElementState, KeyEvent, MouseButton, TouchPhase, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop as WinitEventLoop},
    keyboard::{KeyCode, PhysicalKey},
    platform::{
        pump_events::{EventLoopExtPumpEvents, PumpStatus},
        scancode::PhysicalKeyExtScancode,
    },
    window::{Window, WindowId},
};

mod calendar;
pub(crate) mod input_util;
mod popup;
mod wgpu_setup;
use wgpu_setup::{configure_surface, make_render_texture};

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
    wayland_runtime::WaylandRuntime,
    wayland_state::SpikeState,
    wm::WindowManager,
    Compositor, DockItem, TokenMode,
};

const WIDTH: u32 = 1280;
const HEIGHT: u32 = 960;

/// Number of frame-duration samples retained for the debug overlay's chart.
/// 120 ≈ 2s at 60fps, enough to see a spike without scrolling forever.
const FRAME_HISTORY_LEN: usize = 120;

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
    Motion {
        x: f64,
        y: f64,
    },
    /// Mouse button pressed/released. `button` is the Linux evdev button code.
    Button {
        button: u32,
        pressed: bool,
    },
    /// Scroll wheel / touchpad axis. Pixel-delta semantics; line/discrete scrolls
    /// from a wheel are pre-multiplied by 15 (a typical line height) on the
    /// winit→PendingPointerEvent edge so all events are normalised to pixels
    /// before reaching the wayland client. `discrete_v120` carries the v120
    /// representation (1 wheel notch = 120) when the source is a real wheel.
    Axis {
        dx: f64,
        dy: f64,
        discrete_v120: Option<(i32, i32)>,
        is_wheel: bool,
    },
    TouchDown {
        slot: smithay::backend::input::TouchSlot,
        x: f64,
        y: f64,
    },
    TouchMotion {
        slot: smithay::backend::input::TouchSlot,
        x: f64,
        y: f64,
    },
    TouchUp {
        slot: smithay::backend::input::TouchSlot,
    },
    TouchCancel,
    TouchFrame,
}

// `winit_button_to_evdev`, `forward_keyboard_event`, `ascii_to_scancode`, and
// `compute_menu_height` moved to `renderer/input_util.rs`.
use input_util::{
    ascii_to_scancode, compute_menu_height, forward_keyboard_event, winit_button_to_evdev,
};

// `make_render_texture` and `configure_surface` moved to `renderer/wgpu_setup.rs`.

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
    /// Single-pass alpha-blend renderer for the DnD icon overlay. Built
    /// once per swapchain creation; lives until the renderer is torn
    /// down. Replaces the earlier `queue.write_texture` overwrite that
    /// dropped icon transparency.
    dnd_icon_pass: Option<crate::render::dnd_icon::DndIconPass>,

    // Theme state — mode_t / per-window focus_t animations.
    theme: ThemeState,

    pointer_pos: (f64, f64),
    last_clock_update: Instant,
    /// Months ahead (+) or behind (-) the current month for the
    /// datetime popout's calendar. Driven by the prev/next chevrons
    /// in the popout header. `0` means "show current month".
    calendar_month_offset: i32,
    pending_keys: Arc<Mutex<VecDeque<PendingKeyEvent>>>,
    pending_pointers: Arc<Mutex<VecDeque<PendingPointerEvent>>>,
    frame_count: u64,

    // Hotkey state — Super key held tracking for Super+T theme toggle.
    super_held: bool,
    shortcuts_inhibited: bool,

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
    pending_close: Arc<Mutex<VecDeque<i32>>>,
    /// Dock-menu deferred actions: `(app_id, action_id)` — 2=Show All, 5=Quit.
    pending_dock_action: Arc<Mutex<VecDeque<(String, i32)>>>,
    /// Currently-registered StatusNotifierItems. Updated each frame by
    /// draining tray-host events; rendered as panel tray icons.
    tray_items: Vec<crate::tray::TrayItem>,
    pending_minimize: Arc<Mutex<VecDeque<i32>>>,
    pending_maximize: Arc<Mutex<VecDeque<i32>>>,
    pending_activate: Arc<Mutex<VecDeque<i32>>>,
    /// Pending alt-tab step requests from the winit key handler.
    pending_alt_tab_step: Arc<Mutex<u32>>,
    /// Pending alt-tab commit (Alt released).
    pending_alt_tab_commit: Arc<Mutex<bool>>,
    /// Key releases to swallow because the corresponding press was handled
    /// by compositor UI/WM policy and never forwarded to the focused client.
    swallowed_key_releases: HashSet<u32>,

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

    /// Sliding window of per-frame render durations (ms), bound to the debug
    /// overlay's chart. Capped at FRAME_HISTORY_LEN; oldest sample evicted
    /// each push so the chart shows the most recent ~2s at 60fps.
    frame_times_model: Rc<VecModel<f32>>,

    /// Snap candidate from the last Move-drag motion. If Some on left-up,
    /// apply_pending_snap glides the window into the snap rect.
    pending_snap: Option<(
        i32, /* wm_id */
        crate::snap::SnapZone,
        crate::snap::SnapRect,
    )>,

    /// Queue of IPC commands posted by the unix-socket server thread. Drained
    /// in the main loop on each iteration.
    pending_ipc: PendingIpc,

    /// Monotonic timestamp captured immediately after `frame.present()`. Read
    /// (and cleared) by the main loop body to fire wp_presentation_feedback
    /// `presented` events. None until the first frame has been presented.
    /// We can't fire `presented` from inside `render_frame` directly because
    /// SpikeState isn't reachable there.
    last_present_time: Option<smithay::utils::Time<smithay::utils::Monotonic>>,

    /// Snapshot of the active DnD icon's pixels + cursor pos. Refreshed each
    /// `update_windows` (where we have `state`); consumed by `render_frame`
    /// (which doesn't). `None` when no DnD is active or the icon hasn't yet
    /// committed pixels.
    dnd_icon_snapshot: Option<DndIconSnapshot>,

    /// When the host's window scale_factor changes (HiDPI hot-plug, monitor
    /// drag, Hyprland fractional toggle), we update our wgpu/wm/etc. on
    /// the spot but the wl_output's advertised scale lives behind
    /// `state.output` which isn't reachable from the winit handler.
    /// Stash the value here; drain it inside `update_windows` (where we
    /// have `state`) and call `output.change_current_state(scale)` so
    /// clients see fresh `preferred_buffer_scale` events.
    pending_output_scale: Option<f64>,
    /// Pending wl_output mode update — (logical_w, logical_h, refresh_mhz).
    /// Drained in update_windows the same way pending_output_scale is, so
    /// clients see fresh `wl_output.mode` events when the host window
    /// resizes (matching what we already do for `xdg_output.logical_size`).
    pending_output_mode: Option<(i32, i32, i32)>,

    /// Snapshot of the first ext-session-lock-v1 lock surface's pixels.
    /// Refreshed each `update_windows`; consumed by `render_frame` to
    /// overwrite the entire `final_tex` whenever `session_locked` is set.
    /// Multi-output not yet supported; we only render the first lock
    /// surface.
    lock_surface_snapshot: Option<LockSurfaceSnapshot>,

    /// Cached Slint `Image` per layer surface (keyed by wl_surface
    /// protocol_id). Rebuilt only when the surface's `pixels.dirty` flag
    /// flips on commit; otherwise reused so the panel/dock textures don't
    /// re-upload every frame.
    layer_image_cache: std::collections::HashMap<u32, slint::Image>,
    /// Fingerprint of the last layers model published to Slint. When it
    /// matches the current state and no surface dirtied, the per-iteration
    /// rebuild + `set_layers` is skipped entirely.
    last_layers_fingerprint: Vec<(u32, i32, i32, i32, i32, i32)>,
    /// Cached Slint `Image` per WM window id (keyed by `WindowState::id`),
    /// paired with the `ClientSurfaceData::version` it was built from.
    /// Rebuilt only when the toplevel's buffer version has advanced;
    /// otherwise reused so window content doesn't re-upload on animation-
    /// only ticks (focus crossfade, drag, alt-tab) or geometry updates.
    client_image_cache: std::collections::HashMap<i32, (u64, slint::Image)>,
    /// Per-surface image cache for the new render-element model.
    /// Keyed by `(WindowState::id, wl_surface protocol_id)`, value is
    /// `(ClientSurfaceData::version, slint::Image)`. Reuses uploads across
    /// frames when neither the toplevel's window id nor the individual
    /// surface's buffer version has advanced.
    client_image_cache_per_surface: std::collections::HashMap<(i32, u32), (u64, slint::Image)>,
    /// `(dbus_service, dbus_path)` of the focused window's global menu, or
    /// None. Shared with the panel's menu-click callbacks so they can fetch
    /// submenus / dispatch activations. `update_windows` rewrites it when
    /// keyboard focus moves to a window with a different appmenu address.
    appmenu_addr: Rc<RefCell<Option<(String, String)>>>,
    /// Async appmenu fetch results, pushed by worker threads and drained
    /// each `update_windows` tick onto the Slint model.
    appmenu_results: Arc<Mutex<VecDeque<MenuFetchResult>>>,
}

/// Result of an async `com.canonical.dbusmenu` D-Bus fetch. The fetch runs
/// on a worker thread; results are pushed onto `CompositorApp::appmenu_results`
/// and drained on the next `update_windows` tick. (slint's
/// `invoke_from_event_loop` is unavailable here — the compositor drives
/// its own winit loop, not slint's.)
enum MenuFetchResult {
    /// Top-level global-menu bar for the focused window.
    Bar(Vec<crate::dbusmenu::AppMenuNode>),
    /// A global-menu submenu opened from a bar entry — with popup placement.
    Submenu {
        items: Vec<crate::dbusmenu::AppMenuNode>,
        x: i32,
        open_index: i32,
    },
    /// A StatusNotifierItem tray icon's right-click menu.
    TrayMenu {
        items: Vec<crate::dbusmenu::AppMenuNode>,
        x: i32,
        y: i32,
        sni_id: i32,
    },
}

/// Per-frame snapshot of the active lock surface for the renderer.
#[derive(Clone)]
struct LockSurfaceSnapshot {
    pixels: Vec<u8>,
    width: u32,
    height: u32,
}

/// Per-frame snapshot of the active DnD icon's drawing parameters.
#[derive(Clone)]
struct DndIconSnapshot {
    /// Premultiplied-alpha RGBA8 pixels, tightly packed (width * 4 bytes/row).
    pixels: Vec<u8>,
    /// Buffer dimensions in pixels.
    width: u32,
    height: u32,
    /// Cursor position in LOGICAL pixels (matches what slint/WM use). Converted
    /// to physical when blitting onto `final_tex`.
    cursor_x: f64,
    cursor_y: f64,
    /// Accumulated wl_surface.offset (== buffer_delta on commit), in
    /// LOGICAL pixels. Subtracted from the cursor position so the icon's
    /// declared hotspot lands exactly on the pointer.
    hotspot_x: i32,
    hotspot_y: i32,
}

impl CompositorApp {
    fn new(
        window_ref: Arc<Mutex<Option<Rc<GpuWindowAdapter>>>>,
        ui: Compositor,
        pending_keys: Arc<Mutex<VecDeque<PendingKeyEvent>>>,
        pending_pointers: Arc<Mutex<VecDeque<PendingPointerEvent>>>,
        dock_entries: Vec<ResolvedDockEntry>,
        appmenu_addr: Rc<RefCell<Option<(String, String)>>>,
        appmenu_results: Arc<Mutex<VecDeque<MenuFetchResult>>>,
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
            dnd_icon_pass: None,
            theme: ThemeState::new(),
            super_held: false,
            shortcuts_inhibited: false,
            pointer_pos: (0.0, 0.0),
            last_clock_update: Instant::now(),
            calendar_month_offset: 0,
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
            pending_close: Arc::new(Mutex::new(VecDeque::new())),
            pending_dock_action: Arc::new(Mutex::new(VecDeque::new())),
            tray_items: Vec::new(),
            pending_minimize: Arc::new(Mutex::new(VecDeque::new())),
            pending_maximize: Arc::new(Mutex::new(VecDeque::new())),
            pending_activate: Arc::new(Mutex::new(VecDeque::new())),
            pending_alt_tab_step: Arc::new(Mutex::new(0)),
            pending_alt_tab_commit: Arc::new(Mutex::new(false)),
            swallowed_key_releases: HashSet::new(),
            windows_model: Rc::new(VecModel::default()),
            popups_model: Rc::new(VecModel::default()),
            backdrop: BackdropSynth::new(WIDTH, HEIGHT),
            fps: 0.0,
            last_fps_sample: Instant::now(),
            last_fps_count: 0,
            frame_times_model: Rc::new(VecModel::default()),
            pending_snap: None,
            pending_ipc: Arc::new(Mutex::new(Vec::new())),
            last_present_time: None,
            dnd_icon_snapshot: None,
            pending_output_scale: None,
            pending_output_mode: None,
            lock_surface_snapshot: None,
            layer_image_cache: std::collections::HashMap::new(),
            last_layers_fingerprint: Vec::new(),
            client_image_cache: std::collections::HashMap::new(),
            client_image_cache_per_surface: std::collections::HashMap::new(),
            appmenu_addr,
            appmenu_results,
        }
    }

    fn get_render_texture(&mut self, width: u32, height: u32) -> Option<&wgpu::Texture> {
        if self.render_texture.is_none() || self.render_texture_size != (width, height) {
            let gpu_window = self.gpu_window.as_ref()?;
            let device = &gpu_window.wgpu_device;
            let format = self
                .swapchain_format
                .unwrap_or(wgpu::TextureFormat::Rgba8Unorm);
            self.render_texture = Some(make_render_texture(device, width, height, format));
            self.final_texture = Some(make_render_texture(device, width, height, format));
            self.render_texture_size = (width, height);
            debug!(
                "(Re)created render+final textures {}x{} {:?}",
                width, height, format
            );
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
            event_loop
                .create_window(attrs)
                .expect("failed to create window"),
        );

        let gpu_window = self
            .window_ref
            .lock()
            .unwrap()
            .clone()
            .expect("Slint GPU window adapter should exist after Compositor::new()");

        let surface = unsafe {
            use raw_window_handle::{HasDisplayHandle, HasWindowHandle};
            let target = wgpu::SurfaceTargetUnsafe::RawHandle {
                raw_display_handle: window.display_handle().unwrap().as_raw(),
                raw_window_handle: window.window_handle().unwrap().as_raw(),
            };
            gpu_window
                .wgpu_instance
                .create_surface_unsafe(target)
                .expect("create_surface_unsafe failed")
        };
        let surface: wgpu::Surface<'static> =
            unsafe { std::mem::transmute::<wgpu::Surface<'_>, wgpu::Surface<'static>>(surface) };

        let format = configure_surface(
            &surface,
            &gpu_window.wgpu_adapter,
            &gpu_window.wgpu_device,
            WIDTH,
            HEIGHT,
        );
        info!("wgpu swapchain ready, format={:?} (shared device)", format);

        let chrome =
            ChromeRenderer::new(std::sync::Arc::new(gpu_window.wgpu_device.clone()), format);
        self.chrome = Some(chrome);
        info!("ChromeRenderer initialised (shadow + border + highlight passes)");

        self.dnd_icon_pass = Some(crate::render::dnd_icon::DndIconPass::new(
            std::sync::Arc::new(gpu_window.wgpu_device.clone()),
            format,
        ));

        // Initial resize: physical = logical at startup (winit hasn't
        // delivered a Resized yet) — scale_factor is queried from the
        // winit window itself, which already knows the host's HiDPI
        // factor at this point. The first WindowEvent::Resized that
        // arrives shortly after will fix this up if the host disagrees.
        let initial_scale = window.scale_factor() as f32;
        let initial_phys_w = (WIDTH as f32 * initial_scale).round() as u32;
        let initial_phys_h = (HEIGHT as f32 * initial_scale).round() as u32;
        self.scale_factor = initial_scale;
        // Output is created in main() before winit knows the host's
        // scale; push the value through to the wl_output on the next
        // update_windows tick so fractional-scale-v1 + per-surface
        // preferred_buffer_scale fire with the right value at startup
        // rather than waiting for the first scale change.
        if (initial_scale - 1.0).abs() > 0.001 {
            self.pending_output_scale = Some(initial_scale as f64);
        }
        info!(
            "compositor window opened: logical={}x{} physical={}x{} scale_factor={}",
            WIDTH, HEIGHT, initial_phys_w, initial_phys_h, initial_scale,
        );
        gpu_window.resize(initial_phys_w, initial_phys_h, initial_scale);

        if let Some(ui) = &self.ui {
            // Bind the persistent windows model exactly once. From now on
            // update_windows mutates rows in place via push/remove/set_row_data
            // — the repeater preserves WindowChrome identity (and IconButton
            // hover state) across redraws.
            ui.set_windows(slint::ModelRc::from(self.windows_model.clone()));
            ui.set_popups(slint::ModelRc::from(self.popups_model.clone()));
            ui.set_debug_frame_times(slint::ModelRc::from(self.frame_times_model.clone()));
            ui.window().show().ok();
        }

        self.window = Some(window);
        self.wgpu_surface = Some(surface);
        self.swapchain_format = Some(format);
        self.gpu_window = Some(gpu_window);

        info!("GPU window created, FemtoVG renderer active");
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
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
                let scale = self
                    .window
                    .as_ref()
                    .map(|w| w.scale_factor())
                    .unwrap_or(1.0)
                    .max(0.0001);
                let prev_scale = self.scale_factor;
                self.scale_factor = scale as f32;
                if (prev_scale as f64 - scale).abs() > 0.001 {
                    self.pending_output_scale = Some(scale);
                }
                let logical_w = ((size.width as f64 / scale).round() as u32).max(1);
                let logical_h = ((size.height as f64 / scale).round() as u32).max(1);
                info!(
                    "compositor resize: logical={}x{} physical={}x{} scale_factor={} (was {})",
                    logical_w, logical_h, size.width, size.height, self.scale_factor, prev_scale,
                );
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
                self.final_texture = None;
                self.wm.output_w = logical_w as i32;
                self.wm.output_h = logical_h as i32;
                self.backdrop.set_output_size(logical_w, logical_h);
                if let Some(c) = self.chrome.as_mut() {
                    c.invalidate();
                }
                // Read the host's actual refresh from winit if available
                // (Wayland host on a 165Hz monitor → we should propagate
                // that instead of advertising 60Hz to our clients). Fall
                // back to 60Hz if winit doesn't know.
                let refresh_mhz = self
                    .window
                    .as_ref()
                    .and_then(|w| w.current_monitor())
                    .and_then(|m| m.refresh_rate_millihertz())
                    .map(|v| v as i32)
                    .unwrap_or(60_000);
                self.pending_output_mode = Some((logical_w as i32, logical_h as i32, refresh_mhz));
            }

            WindowEvent::CursorMoved { position, .. } => {
                // winit `position` is PHYSICAL pixels. Convert to LOGICAL so
                // the cursor visual, slint hit testing, and wayland client
                // events all share the compositor's logical-pixel space.
                let scale = self
                    .window
                    .as_ref()
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
                if self.slint_pointer_overlay_open() {
                    self.show_builtin_cursor(CursorKind::Arrow);
                    return;
                }
                self.pending_pointers
                    .lock()
                    .unwrap()
                    .push_back(PendingPointerEvent::Motion { x: lx, y: ly });
            }

            WindowEvent::MouseWheel {
                delta, phase: _, ..
            } => {
                use winit::event::MouseScrollDelta;
                // Wayland axis events are pixel-deltas. winit hands us either
                // discrete LineDelta (mouse wheel — one notch per integer) or
                // PixelDelta (touchpad). Multiply LineDelta by ~15 px/line so
                // wheel scrolls feel like real scroll distance, then forward
                // unchanged for PixelDelta. The v120 (=120 per detent) signal
                // is what GTK/Qt use for high-resolution scroll feel; only
                // emit it for genuine wheels.
                let (dx_px, dy_px, discrete_v120, is_wheel) = match delta {
                    MouseScrollDelta::LineDelta(lx, ly) => {
                        let v120 = (
                            (lx as f64 * 120.0).round() as i32,
                            (ly as f64 * 120.0).round() as i32,
                        );
                        (lx as f64 * 15.0, ly as f64 * 15.0, Some(v120), true)
                    }
                    MouseScrollDelta::PixelDelta(p) => (p.x, p.y, None, false),
                };
                // Dispatch to Slint so any in-process scrollable widgets
                // (debug overlay text, dock overflow, future control-centre
                // sliders) react to wheel input. Slint's PointerScrolled
                // matches our "positive = down/right" convention 1:1, so
                // no axis flip is needed.
                let pos =
                    LogicalPosition::new(self.pointer_pos.0 as f32, self.pointer_pos.1 as f32);
                gpu_window.inner_window().dispatch_event(
                    slint::platform::WindowEvent::PointerScrolled {
                        position: pos,
                        delta_x: dx_px as f32,
                        delta_y: dy_px as f32,
                    },
                );
                if self.slint_pointer_overlay_open() {
                    return;
                }
                // Sign convention: winit reports a positive LineDelta/PixelDelta
                // y when the wheel is scrolled UP (away from the user); the
                // wl_pointer.axis protocol wants a POSITIVE value for scroll
                // DOWN (libinput's convention — see backend/udev.rs which
                // forwards libinput amounts unflipped). They're opposite, so
                // negate both axes for the wayland path. The Slint path above
                // keeps winit's signs because Slint's PointerScrolled uses the
                // same convention winit does.
                let discrete_v120 = discrete_v120.map(|(vx, vy)| (-vx, -vy));
                self.pending_pointers
                    .lock()
                    .unwrap()
                    .push_back(PendingPointerEvent::Axis {
                        dx: -dx_px,
                        dy: -dy_px,
                        discrete_v120,
                        is_wheel,
                    });
            }

            WindowEvent::MouseInput { state, button, .. } => {
                let slint_btn = match button {
                    MouseButton::Left => slint::platform::PointerEventButton::Left,
                    MouseButton::Right => slint::platform::PointerEventButton::Right,
                    MouseButton::Middle => slint::platform::PointerEventButton::Middle,
                    _ => slint::platform::PointerEventButton::Other,
                };
                let pos =
                    LogicalPosition::new(self.pointer_pos.0 as f32, self.pointer_pos.1 as f32);
                let pressed = state == ElementState::Pressed;

                // Right-click while a context menu is open → dismiss
                // BEFORE forwarding to slint or queuing for Rust. If
                // we forwarded first, slint's click-absorber would
                // close the menu, then `forward_pointer_button` would
                // see `desktop_menu_open == false` and re-open at the
                // new cursor — net effect: menu teleports instead of
                // dismissing. By returning here we suppress both.
                if button == MouseButton::Right && pressed {
                    if let Some(ui) = self.ui.as_ref() {
                        let any_open = ui.get_desktop_menu_open()
                            || ui.get_dock_menu_open()
                            || ui.get_tray_menu_open()
                            || ui.get_window_menu_open();
                        if any_open {
                            ui.set_desktop_menu_open(false);
                            ui.set_desktop_menu_selected(-1);
                            ui.set_dock_menu_open(false);
                            ui.set_dock_menu_selected(-1);
                            ui.set_tray_menu_open(false);
                            ui.set_tray_menu_selected(-1);
                            ui.set_window_menu_open(false);
                            ui.set_window_menu_selected(-1);
                            if let Some(gpu) = self.gpu_window.as_ref() {
                                gpu.mark_dirty();
                            }
                            return;
                        }
                    }
                }

                // Track left button state for drag detection.
                if button == MouseButton::Left {
                    self.left_button_down = pressed;
                    // Drag release (active_drag = None + final configure) is
                    // handled in the main loop's PointerEvent::Button branch
                    // where we have SpikeState access — see release_drag().
                }

                let slint_captures_pointer = self.slint_pointer_overlay_open();

                let slint_event = match state {
                    ElementState::Pressed => slint::platform::WindowEvent::PointerPressed {
                        position: pos,
                        button: slint_btn,
                    },
                    ElementState::Released => slint::platform::WindowEvent::PointerReleased {
                        position: pos,
                        button: slint_btn,
                    },
                };
                gpu_window.inner_window().dispatch_event(slint_event);
                if slint_captures_pointer {
                    self.show_builtin_cursor(CursorKind::Arrow);
                    return;
                }

                // On press, update WM focus based on pointer position.
                // Actual focus update happens in the main loop via pending_pointers.
                let evdev_btn = winit_button_to_evdev(button);
                self.pending_pointers
                    .lock()
                    .unwrap()
                    .push_back(PendingPointerEvent::Button {
                        button: evdev_btn,
                        pressed,
                    });
            }

            WindowEvent::Touch(touch) => {
                let scale = self
                    .window
                    .as_ref()
                    .map(|w| w.scale_factor())
                    .unwrap_or(1.0)
                    .max(0.0001);
                let x = touch.location.x / scale;
                let y = touch.location.y / scale;
                let Some(slot_id) = u32::try_from(touch.id).ok() else {
                    return;
                };
                let slot = smithay::backend::input::TouchSlot::from(Some(slot_id));
                let mut pending = self.pending_pointers.lock().unwrap();
                match touch.phase {
                    TouchPhase::Started => {
                        pending.push_back(PendingPointerEvent::TouchDown { slot, x, y })
                    }
                    TouchPhase::Moved => {
                        pending.push_back(PendingPointerEvent::TouchMotion { slot, x, y })
                    }
                    TouchPhase::Ended => pending.push_back(PendingPointerEvent::TouchUp { slot }),
                    TouchPhase::Cancelled => pending.push_back(PendingPointerEvent::TouchCancel),
                }
                pending.push_back(PendingPointerEvent::TouchFrame);
            }

            WindowEvent::KeyboardInput {
                event: key_event, ..
            } => {
                let scancode = key_event.physical_key.to_scancode().unwrap_or(0);
                let pressed = key_event.state == ElementState::Pressed;
                let release_consumed = !pressed && self.swallowed_key_releases.remove(&scancode);

                // Context-menu keyboard navigation. When a desktop / dock /
                // tray context menu is open, Up / Down / Enter drive the
                // selection and never reach wayland clients. Escape is
                // already handled below in the catch-all overlay-close
                // branch.
                let shortcuts_inhibited = self.shortcuts_inhibited;
                let menu_consumed = if !shortcuts_inhibited {
                    if pressed {
                        self.handle_menu_nav_key(scancode)
                    } else {
                        // Swallow the matching release so clients don't see a
                        // stray key-up for a press they never received.
                        matches!(scancode, 103 | 108 | 28 | 96) && self.menu_nav_active()
                    }
                } else {
                    false
                };
                let mut compositor_consumed = release_consumed || menu_consumed;

                if !shortcuts_inhibited {
                    // Scancode 125 = KEY_LEFTMETA (Super/Win key), 126 = KEY_RIGHTMETA.
                    match scancode {
                        125 | 126 => {
                            self.super_held = pressed;
                        }
                        20 if pressed && self.super_held => {
                            self.theme.toggle_mode();
                            self.apply_theme_to_slint();
                            self.swap_wallpaper_for_current_mode();
                            debug!("Super+T: toggled theme to {:?}", self.theme.current_mode);
                            compositor_consumed = true;
                        }
                        23 if pressed && self.super_held => {
                            if let Some(ui) = self.ui.as_ref() {
                                let now = ui.get_debug_overlay_visible();
                                ui.set_debug_overlay_visible(!now);
                            }
                            compositor_consumed = true;
                        }
                        53 if pressed && self.super_held => {
                            if let Some(ui) = self.ui.as_ref() {
                                let now = ui.get_help_overlay_visible();
                                ui.set_help_overlay_visible(!now);
                            }
                            compositor_consumed = true;
                        }
                        17 if pressed && self.super_held => {
                            if let Some(id) = self.wm.focused_id() {
                                self.pending_close.lock().unwrap().push_back(id);
                                debug!("Super+W: queued close for focused id={}", id);
                            }
                            compositor_consumed = true;
                        }
                        50 if pressed && self.super_held => {
                            if let Some(id) = self.wm.focused_id() {
                                self.pending_minimize.lock().unwrap().push_back(id);
                                debug!("Super+M: queued minimize for focused id={}", id);
                            }
                            compositor_consumed = true;
                        }
                        32 if pressed && self.super_held => {
                            let ids: Vec<i32> = self
                                .wm
                                .windows
                                .values()
                                .filter(|w| !w.minimized && !w.closing)
                                .map(|w| w.id)
                                .collect();
                            let mut q = self.pending_minimize.lock().unwrap();
                            for id in ids {
                                q.push_back(id);
                            }
                            debug!("Super+D: minimized all visible windows");
                            compositor_consumed = true;
                        }
                        57 if pressed && self.super_held => {
                            if let Some(ui) = self.ui.as_ref() {
                                let now = ui.get_launcher_open();
                                ui.set_launcher_open(!now);
                                if !now {
                                    ui.set_launcher_query(SharedString::default());
                                }
                            }
                            compositor_consumed = true;
                        }
                        1 if pressed => {
                            if let Some(ui) = self.ui.as_ref() {
                                let any_open = ui.get_desktop_menu_open()
                                    || ui.get_datetime_popout_open()
                                    || ui.get_control_centre_open()
                                    || ui.get_help_overlay_visible()
                                    || ui.get_launcher_open()
                                    || ui.get_dock_menu_open()
                                    || ui.get_window_menu_open();
                                if any_open {
                                    ui.set_desktop_menu_open(false);
                                    ui.set_datetime_popout_open(false);
                                    ui.set_control_centre_open(false);
                                    ui.set_help_overlay_visible(false);
                                    ui.set_launcher_open(false);
                                    ui.set_dock_menu_open(false);
                                    ui.set_window_menu_open(false);
                                    if let Some(gpu) = self.gpu_window.as_ref() {
                                        gpu.mark_dirty();
                                    }
                                    compositor_consumed = true;
                                }
                            }
                        }
                        127 if pressed => {
                            if let Some(focused_id) = self.wm.focused_id() {
                                if let Some(win) =
                                    self.wm.windows.values().find(|w| w.id == focused_id)
                                {
                                    let (wx, wy, ww) = (
                                        win.anim.current_x() as f64,
                                        win.anim.current_y() as f64,
                                        win.anim.current_w() as f64,
                                    );
                                    let titlebar_h = crate::wm::TITLEBAR_HEIGHT;
                                    let cx = wx + ww / 2.0;
                                    let cy = wy + titlebar_h;
                                    self.open_window_menu(focused_id, cx, cy);
                                    compositor_consumed = true;
                                }
                            }
                        }
                        _ => {}
                    }

                    // Handle Alt-Tab cycling in the winit handler so we get
                    // immediate key state without waiting for the calloop round-trip.
                    if self.handle_alt_tab_key(&key_event) {
                        compositor_consumed = true;
                    }
                } else if matches!(scancode, 125 | 126) {
                    self.super_held = false;
                }

                if pressed && compositor_consumed && scancode > 0 {
                    self.swallowed_key_releases.insert(scancode);
                }

                if scancode > 0 && !compositor_consumed {
                    self.pending_keys
                        .lock()
                        .unwrap()
                        .push_back(PendingKeyEvent { scancode, pressed });
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
    /// True iff a context menu (desktop / dock / tray / window) is open.
    /// Used to decide whether key-release events for navigation keys
    /// should be swallowed instead of forwarded to wayland clients.
    fn menu_nav_active(&self) -> bool {
        let Some(ui) = self.ui.as_ref() else {
            return false;
        };
        ui.get_desktop_menu_open()
            || ui.get_dock_menu_open()
            || ui.get_tray_menu_open()
            || ui.get_window_menu_open()
    }

    /// True while a compositor-owned Slint surface should capture pointer
    /// input instead of letting the event fall through to Wayland clients.
    fn slint_pointer_overlay_open(&self) -> bool {
        let Some(ui) = self.ui.as_ref() else {
            return false;
        };
        ui.get_desktop_menu_open()
            || ui.get_dock_menu_open()
            || ui.get_tray_menu_open()
            || ui.get_window_menu_open()
            || ui.get_datetime_popout_open()
            || ui.get_control_centre_open()
            || ui.get_launcher_open()
            || ui.get_help_overlay_visible()
    }

    fn show_builtin_cursor(&mut self, kind: CursorKind) {
        self.current_cursor = kind;
        self.update_cursor_overlay();
        if let Some(ui) = self.ui.as_ref() {
            ui.set_cursor_visible(true);
        }
    }

    /// Open the per-window context menu (Minimize / Maximize / Close)
    /// for the given WM window id at screen-space (x, y). Same clamp +
    /// origin maths as the desktop menu so the cursor lands inside the
    /// menu and the open-scale grows out of the click point.
    fn open_window_menu(&mut self, target_id: i32, x: f64, y: f64) {
        let Some(ui) = self.ui.as_ref() else { return };
        let menu_w = 220.0;
        let panel_h = crate::wm::PANEL_HEIGHT as f64;
        let dock_h = crate::wm::DOCK_HEIGHT as f64;
        let ow = self.wm.output_w as f64;
        let oh = self.wm.output_h as f64;
        let items = ui.get_window_menu_items();
        let menu_h = compute_menu_height(&items);
        let side_pad = 8.0;
        let bot_pad = 4.0;
        let x_flipped = x + menu_w + side_pad > ow;
        let mut mx = if x_flipped { x - menu_w } else { x };
        mx = mx.clamp(side_pad, (ow - menu_w - side_pad).max(side_pad));
        let y_flipped = y + menu_h + bot_pad > oh - dock_h;
        let mut my = if y_flipped { y - menu_h } else { y };
        my = my.clamp(
            panel_h + 4.0,
            (oh - dock_h - menu_h - bot_pad).max(panel_h + 4.0),
        );
        let origin_x = (x - mx).clamp(0.0, menu_w);
        let origin_y = (y - my).clamp(0.0, menu_h);
        ui.set_window_menu_target_id(target_id);
        ui.set_window_menu_x(mx as i32);
        ui.set_window_menu_y(my as i32);
        ui.set_window_menu_w(menu_w as i32);
        ui.set_window_menu_h(menu_h as i32);
        ui.set_window_menu_origin_x(origin_x as i32);
        ui.set_window_menu_origin_y(origin_y as i32);
        ui.set_window_menu_selected(-1);
        ui.set_window_menu_open(true);
        if let Some(gpu) = self.gpu_window.as_ref() {
            gpu.mark_dirty();
        }
    }

    /// Drive keyboard navigation for whichever context menu is open.
    /// Returns `true` if the keypress was consumed (don't forward to
    /// clients). Recognises Up / Down / Home / End / Enter / KP-Enter.
    /// Skips separator and disabled rows when stepping. Enter invokes
    /// the same callback the click handler does, then closes the menu.
    fn handle_menu_nav_key(&mut self, scancode: u32) -> bool {
        // 103 = KEY_UP, 108 = KEY_DOWN, 28 = KEY_ENTER, 96 = KEY_KPENTER,
        // 102 = KEY_HOME, 107 = KEY_END.
        if !matches!(scancode, 103 | 108 | 28 | 96 | 102 | 107) {
            return false;
        }
        let Some(ui) = self.ui.as_ref() else {
            return false;
        };

        // Pick the open menu — most-recently-overlaid wins (dock + tray
        // are explicitly stacked above desktop in Compositor.slint).
        enum Kind {
            Desktop,
            Dock,
            Tray,
        }
        let kind = if ui.get_dock_menu_open() {
            Kind::Dock
        } else if ui.get_tray_menu_open() {
            Kind::Tray
        } else if ui.get_desktop_menu_open() {
            Kind::Desktop
        } else {
            return false;
        };

        // Snapshot which rows are pickable (not separator, enabled).
        let model = match kind {
            Kind::Desktop => ui.get_desktop_menu_items(),
            Kind::Dock => ui.get_dock_menu_items(),
            Kind::Tray => ui.get_tray_menu_items(),
        };
        let n = model.row_count() as i32;
        if n == 0 {
            return true;
        }
        let pickable: Vec<bool> = (0..n)
            .map(|i| {
                model
                    .row_data(i as usize)
                    .map(|it| !it.separator && it.enabled)
                    .unwrap_or(false)
            })
            .collect();
        if !pickable.iter().any(|p| *p) {
            return true;
        }

        let current = match kind {
            Kind::Desktop => ui.get_desktop_menu_selected(),
            Kind::Dock => ui.get_dock_menu_selected(),
            Kind::Tray => ui.get_tray_menu_selected(),
        };

        // Step from `from` (exclusive) in `dir` (+1 / -1), wrapping, until
        // we land on a pickable row. Caller guarantees ≥1 pickable row.
        let step = |from: i32, dir: i32| -> i32 {
            let mut i = from;
            for _ in 0..n {
                i += dir;
                if i < 0 {
                    i = n - 1;
                }
                if i >= n {
                    i = 0;
                }
                if pickable[i as usize] {
                    return i;
                }
            }
            from
        };

        let new_idx = match scancode {
            103 => {
                // Up
                let from = if current < 0 { 0 } else { current };
                step(from, -1)
            }
            108 => {
                // Down
                let from = if current < 0 { -1 } else { current };
                step(from, 1)
            }
            102 => {
                // Home — first pickable
                step(-1, 1)
            }
            107 => {
                // End — last pickable
                step(n, -1)
            }
            28 | 96 => {
                // Enter — fire the click callback
                if current < 0 || !pickable[current as usize] {
                    return true;
                }
                let id = model
                    .row_data(current as usize)
                    .map(|it| it.id)
                    .unwrap_or(0);
                match kind {
                    Kind::Desktop => {
                        ui.set_desktop_menu_open(false);
                        ui.set_desktop_menu_selected(-1);
                        ui.invoke_desktop_menu_clicked(id);
                    }
                    Kind::Dock => {
                        let app_id = ui.get_dock_menu_app_id();
                        ui.set_dock_menu_open(false);
                        ui.set_dock_menu_selected(-1);
                        ui.invoke_dock_menu_clicked(app_id, id);
                    }
                    Kind::Tray => {
                        let sni = ui.get_tray_menu_id();
                        ui.set_tray_menu_open(false);
                        ui.set_tray_menu_selected(-1);
                        ui.invoke_tray_menu_clicked(sni, id);
                    }
                }
                if let Some(gpu) = self.gpu_window.as_ref() {
                    gpu.mark_dirty();
                }
                return true;
            }
            _ => return false,
        };

        match kind {
            Kind::Desktop => ui.set_desktop_menu_selected(new_idx),
            Kind::Dock => ui.set_dock_menu_selected(new_idx),
            Kind::Tray => ui.set_tray_menu_selected(new_idx),
        }
        if let Some(gpu) = self.gpu_window.as_ref() {
            gpu.mark_dirty();
        }
        true
    }

    /// Handle alt/tab key events for the alt-tab switcher.
    fn handle_alt_tab_key(&mut self, key_event: &KeyEvent) -> bool {
        let pressed = key_event.state == ElementState::Pressed;
        match key_event.physical_key {
            PhysicalKey::Code(KeyCode::AltLeft) | PhysicalKey::Code(KeyCode::AltRight) => {
                self.alt_tab.alt_held = pressed;
                if !pressed && self.alt_tab.cycling {
                    // Alt released: commit alt-tab selection.
                    self.alt_tab.cycling = false;
                    *self.pending_alt_tab_commit.lock().unwrap() = true;
                }
                false
            }
            PhysicalKey::Code(KeyCode::Tab) => {
                if pressed && self.alt_tab.alt_held {
                    // Tab pressed while alt held: step alt-tab.
                    self.alt_tab.cycling = true;
                    let mut steps = self.pending_alt_tab_step.lock().unwrap();
                    *steps += 1;
                    return true;
                }
                self.alt_tab.cycling
            }
            _ => false,
        }
    }

    /// GPU render: damage-tracked Slint render + swapchain blit.
    fn render_frame(&mut self) {
        let gpu_window = match self.gpu_window.clone() {
            Some(w) => w,
            None => return,
        };
        let Some(ui) = self.ui.as_ref() else { return };

        let frame_start = Instant::now();
        let now = frame_start;
        if now.duration_since(self.last_clock_update) >= Duration::from_secs(1) {
            self.last_clock_update = now;
            let local = chrono::Local::now();
            ui.set_clock_text(SharedString::from(local.format("%H:%M:%S").to_string()));
            // Short panel date — e.g. "Wed 29 Apr" — sits next to the time.
            ui.set_panel_date_text(SharedString::from(local.format("%a %-d %b").to_string()));
            ui.set_popout_date_text(SharedString::from(local.format("%A, %-d %B").to_string()));
            ui.set_popout_day_text(SharedString::from(local.format("%A").to_string()));

            // Calendar grid for the popout — rebuilds whichever month
            // the user is currently browsing (today + offset).
            self.refresh_calendar();
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
                if self.frame_count.is_multiple_of(60) {
                    info!("frame loop: {} frames rendered", self.frame_count);
                }
                debug!(
                    "GPU render: {}x{} (dirty, frame {})",
                    w, h, self.frame_count
                );
            }
        }

        let Some(surface) = self.wgpu_surface.as_ref() else {
            return;
        };
        let device = &gpu_window.wgpu_device;
        let queue = &gpu_window.wgpu_queue;

        let frame = match surface.get_current_texture() {
            Ok(f) => f,
            Err(wgpu::SurfaceError::Outdated) => return,
            Err(e) => {
                warn!("swapchain: {}", e);
                return;
            }
        };

        if let (Some(render_tex), Some(final_tex)) =
            (self.render_texture.as_ref(), self.final_texture.as_ref())
        {
            let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("chrome+blit"),
            });

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
                wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
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
                    (0..len)
                        .map(|i| {
                            let item = model.row_data(i).unwrap();
                            let focus_t = self.theme.window_focus_t(item.id);
                            let titlebar = if item.csd { 0.0 } else { 33.0 };
                            // Honour the lifecycle scale spring (open/close
                            // animation goes 0.85 → 1.0 / 1.0 → 0.85). The
                            // slint content scales inside outer-frame; if we
                            // emitted the GPU chrome at fixed full size the
                            // shadow / border / highlight would stay rooted
                            // at the un-animated rect, hovering visually
                            // around the shrinking content. Apply the scale
                            // to w/h, recentre x/y so the scaled rect stays
                            // anchored on the chrome's centre — matches the
                            // slint outer-frame's `(parent.size - self.size) / 2`.
                            let scale = item.anim_scale.clamp(0.0, 2.0);
                            // Use the ANIMATED w/h here, not `geom_w/h`, so
                            // the GPU chrome (shadow, border) tracks the
                            // resize target instantly. The slint chrome
                            // dimensions were switched to `win.w/win.h` for
                            // the same reason — keep these in sync or the
                            // shadow will lag visibly behind the chrome
                            // edge during resize drags.
                            let chrome_w_full = item.w.max(1) as f32;
                            let chrome_h_full = item.h.max(1) as f32 + titlebar;
                            let chrome_w = chrome_w_full * scale;
                            let chrome_h = chrome_h_full * scale;
                            let cx_offset = (chrome_w_full - chrome_w) / 2.0;
                            let cy_offset = (chrome_h_full - chrome_h) / 2.0;
                            WindowChromeParams {
                                x: (item.x as f32 + cx_offset) * s,
                                y: (item.y as f32 + cy_offset) * s,
                                w: chrome_w * s,
                                h: chrome_h * s,
                                active: item.focused,
                                focus_t,
                                mode_t,
                                csd: item.csd,
                                // Spring-animated outer radius (logical px,
                                // 18 → 0 across a maximize); scale to
                                // physical px for the GPU chrome so the
                                // shaders round/square in lockstep with the
                                // Slint chrome.
                                radius: item.corner_radius * s,
                            }
                        })
                        .collect()
                } else {
                    Vec::new()
                };

                if !chrome_windows.is_empty() {
                    let scene_view =
                        render_tex.create_view(&wgpu::TextureViewDescriptor::default());
                    let target_view =
                        final_tex.create_view(&wgpu::TextureViewDescriptor::default());
                    // Snapshot the per-window rects in physical pixels;
                    // used by the between-phases closure to re-blit each
                    // window's region from render_tex onto final_tex,
                    // overwriting any shadow that bled into the window's
                    // footprint. Caps each rect to the surface bounds so
                    // copy_texture_to_texture never reads/writes OOB.
                    let win_rects: Vec<(u32, u32, u32, u32)> = chrome_windows
                        .iter()
                        .map(|win| {
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
                            (
                                cx0 as u32,
                                cy0 as u32,
                                (cx1 - cx0) as u32,
                                (cy1 - cy0) as u32,
                            )
                        })
                        .collect();

                    chrome.render(
                        queue,
                        &mut encoder,
                        &scene_view,
                        &target_view,
                        w,
                        h,
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
                            for &(rx, ry, rw, rh) in &win_rects[wi + 1..] {
                                if rw == 0 || rh == 0 {
                                    continue;
                                }
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
                                    wgpu::Extent3d {
                                        width: rw,
                                        height: rh,
                                        depth_or_array_layers: 1,
                                    },
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
                    let px = (x as f32 * s).round() as i32;
                    let py = (y as f32 * s).round() as i32;
                    let prw = (rw as f32 * s).round() as i32;
                    let prh = (rh as f32 * s).round() as i32;
                    let cx0 = px.max(0);
                    let cy0 = py.max(0);
                    let cx1 = (px + prw).min(w as i32).max(cx0);
                    let cy1 = (py + prh).min(h as i32).max(cy0);
                    let cw = (cx1 - cx0) as u32;
                    let ch = (cy1 - cy0) as u32;
                    if cw == 0 || ch == 0 {
                        return;
                    }
                    encoder.copy_texture_to_texture(
                        wgpu::TexelCopyTextureInfo {
                            texture: render_tex,
                            mip_level: 0,
                            origin: wgpu::Origin3d {
                                x: cx0 as u32,
                                y: cy0 as u32,
                                z: 0,
                            },
                            aspect: wgpu::TextureAspect::All,
                        },
                        wgpu::TexelCopyTextureInfo {
                            texture: final_tex,
                            mip_level: 0,
                            origin: wgpu::Origin3d {
                                x: cx0 as u32,
                                y: cy0 as u32,
                                z: 0,
                            },
                            aspect: wgpu::TextureAspect::All,
                        },
                        wgpu::Extent3d {
                            width: cw,
                            height: ch,
                            depth_or_array_layers: 1,
                        },
                    );
                };

                let panel_h = crate::wm::PANEL_HEIGHT.max(0);
                let dock_h = crate::wm::DOCK_HEIGHT.max(0);
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

            // Step 2c — DnD icon: alpha-blend the icon surface into
            // final_tex on top of the composited scene at the cursor
            // position. Done as a textured-quad render pass with
            // src-over blending so translucent / soft-edged icons
            // render correctly (the previous queue.write_texture
            // overwrite forced everything to opaque).
            if let Some(snap) = self.dnd_icon_snapshot.as_ref() {
                if let Some(pass) = self.dnd_icon_pass.as_mut() {
                    let s = self.scale_factor;
                    // Subtract the icon's accumulated wl_surface.offset
                    // (the hotspot in logical pixels) from the cursor
                    // position so the hotspot pixel sits under the
                    // pointer rather than the icon's top-left.
                    let logical_x = snap.cursor_x - snap.hotspot_x as f64;
                    let logical_y = snap.cursor_y - snap.hotspot_y as f64;
                    let cx_phys = (logical_x as f32) * s;
                    let cy_phys = (logical_y as f32) * s;
                    let final_view = final_tex.create_view(&wgpu::TextureViewDescriptor::default());
                    pass.render(
                        queue,
                        &mut encoder,
                        &final_view,
                        w,
                        h,
                        (cx_phys, cy_phys, snap.width as f32, snap.height as f32),
                        &snap.pixels,
                        snap.width,
                        snap.height,
                    );
                }
            }

            // Step 2d — Session lock: overwrite final_tex entirely with
            // the lock surface pixels. This runs AFTER all other passes
            // so chrome/dock/panel/etc are clobbered (the locker owns
            // the screen). Wasteful from a GPU-time PoV — the chrome
            // passes ran for nothing — but correctness > optimisation
            // for a security feature; a future refactor can short-
            // circuit the chrome pipeline when locked.
            if let Some(snap) = self.lock_surface_snapshot.as_ref() {
                // SECURITY: clear the WHOLE final_tex to opaque black
                // first, so any area not covered by the lock surface
                // (e.g. an undersized buffer, the lock client hasn't
                // resized yet, only a partial commit landed) does NOT
                // show the unlocked desktop the chrome passes above
                // just rendered. Without this the locker only painted
                // copy_w × copy_h of the final_tex and the rest leaked
                // — a lock-screen confidentiality bug.
                {
                    let lock_clear_view =
                        final_tex.create_view(&wgpu::TextureViewDescriptor::default());
                    let _clear = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("session-lock-clear"),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: &lock_clear_view,
                            resolve_target: None,
                            depth_slice: None,
                            ops: wgpu::Operations {
                                load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                                store: wgpu::StoreOp::Store,
                            },
                        })],
                        depth_stencil_attachment: None,
                        timestamp_writes: None,
                        occlusion_query_set: None,
                        multiview_mask: None,
                    });
                }
                let copy_w = snap.width.min(w);
                let copy_h = snap.height.min(h);
                if copy_w > 0 && copy_h > 0 {
                    let bytes_per_row = snap.width * 4;
                    queue.write_texture(
                        wgpu::TexelCopyTextureInfo {
                            texture: final_tex,
                            mip_level: 0,
                            origin: wgpu::Origin3d::ZERO,
                            aspect: wgpu::TextureAspect::All,
                        },
                        &snap.pixels,
                        wgpu::TexelCopyBufferLayout {
                            offset: 0,
                            bytes_per_row: Some(bytes_per_row),
                            rows_per_image: Some(snap.height),
                        },
                        wgpu::Extent3d {
                            width: copy_w,
                            height: copy_h,
                            depth_or_array_layers: 1,
                        },
                    );
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
                wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
            );

            queue.submit(std::iter::once(encoder.finish()));
        }

        frame.present();

        // Capture the post-present monotonic timestamp so the main loop can
        // fire wp_presentation_feedback. Without a real DRM page-flip event
        // this is "fake vsync" — the actual scanout happens at some point
        // after the swapchain submit returns, but the delta is sub-frame for
        // mailbox/fifo presentation modes and good enough for clients that
        // just need monotonic increments (mpv, Chrome's vsync sync).
        let clock: smithay::utils::Clock<smithay::utils::Monotonic> = smithay::utils::Clock::new();
        self.last_present_time = Some(clock.now());

        // Record this frame's render+present cost (ms) into the chart's
        // sliding window. Eviction first to keep length stable, so Slint
        // sees only an in-place scroll rather than a length change.
        let frame_ms = frame_start.elapsed().as_secs_f32() * 1000.0;
        if self.frame_times_model.row_count() >= FRAME_HISTORY_LEN {
            self.frame_times_model.remove(0);
        }
        self.frame_times_model.push(frame_ms);
    }

    /// Build the Slint `WindowItem` list from `WM` state + toplevel pixel buffers,
    /// then push it to the UI.  Called every frame when client textures are dirty
    /// or when WM state changes.
    fn update_windows(&mut self, state: &mut SpikeState) {
        use smithay::wayland::compositor::with_states;
        use smithay::wayland::shell::xdg::{SurfaceCachedState, XdgToplevelSurfaceData};

        let Some(ui) = self.ui.as_ref() else { return };

        // If the host's scale_factor changed since the last tick, flush
        // the new value into the wl_output. Smithay walks every surface
        // bound to this output and (re)fires preferred_buffer_scale, so
        // clients adapt without a per-surface push from us.
        if let Some(scale) = self.pending_output_scale.take() {
            if let Some(out) = state.primary_output() {
                out.change_current_state(
                    None,
                    None,
                    Some(smithay::output::Scale::Fractional(scale)),
                    None,
                );
            }
            state.sync_xwayland_settings();
        }

        // Propagate window size changes to the wl_output mode. Without this,
        // clients see the original Mode { size, refresh } advertised at
        // startup forever, so xdg-output's logical_size and `wl_output.mode`
        // never reflect the host window's actual dimensions — apps that
        // size themselves to the output (gamescope, mpv fullscreen) end up
        // at the wrong size after the user resizes the host window. We
        // also re-emit screencopy `buffer_size` to all active sessions so
        // portal screencast stays in sync.
        if let Some((w, h, refresh)) = self.pending_output_mode.take() {
            if let Some(out) = state.primary_output() {
                let new_mode = smithay::output::Mode {
                    size: (w, h).into(),
                    refresh,
                };
                out.change_current_state(Some(new_mode), None, None, None);
                out.set_preferred(new_mode);
            }
            // Refresh ext-image-copy-capture constraints so the new buffer
            // dimensions reach already-bound capture sessions before they
            // request the next frame at the stale size.
            state.refresh_capture_constraints();
        }

        // Tick the WM animations.
        self.wm.tick_auto();

        // Push title/app_id changes to ext-foreign-toplevel-list-v1 handles.
        // Diff-gated inside the helper so this is cheap when nothing changed.
        state.sync_foreign_toplevels();
        // Refresh idle inhibit based on content-type-v1 hints (Video/Game
        // surfaces auto-inhibit). Diffed inside set_is_inhibited so this is
        // a no-op when nothing changed.
        state.refresh_content_type_idle_inhibit();

        // Sweep windows whose close animation has finished.
        let closed = self.wm.sweep_closed();
        for (surf, wm_id) in &closed {
            // Remove from SpikeState toplevel list.
            state.toplevels.retain(|t| &t.surface != surf);
            // Drop per-window theme tweens so the HashMaps in ThemeState don't
            // leak entries every time a window closes.
            self.theme.remove_window(*wm_id);
            debug!("WM: swept closed window for surface");
        }
        let closed_surfaces: Vec<WlSurface> = closed.into_iter().map(|(s, _)| s).collect();
        // If we swept anything, the focus might need updating.
        if !closed_surfaces.is_empty() {
            state.active_surface = self.wm.focused_surface();
            if let Some(surface) = &state.active_surface {
                if let Some(kb) = state.seat.get_keyboard() {
                    let focus = crate::wayland::xwayland::KeyboardFocusTarget::for_wl_surface(
                        state, surface,
                    );
                    kb.set_focus(state, Some(focus), SERIAL_COUNTER.next_serial());
                }
            }
        }

        // First-render kick: any window awaiting its first buffer commit
        // starts its open animation NOW, so the user actually sees it play.
        let kick_ids: Vec<i32> = self
            .wm
            .windows
            .values()
            .filter(|w| w.awaiting_first_render)
            .filter(|w| {
                state
                    .toplevels
                    .iter()
                    .find(|t| t.surface == w.surface)
                    .is_some_and(|tl| tl.pixels.lock().unwrap().width > 0)
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
            gx: i32,
            gy: i32,
            gw: i32,
            gh: i32,
            bw: i32,
            bh: i32,
            csd_now: bool,
            csd_verdict: bool,
            is_resizing: bool,
            app_id: String,
        }
        let mut metas: Vec<ToplevelMeta> = Vec::with_capacity(state.toplevels.len());
        for (i, tl) in state.toplevels.iter().enumerate() {
            // Override-redirect X11 windows live in `state.toplevels` so the
            // SHM/dmabuf import path fills their pixel buffer, but they're
            // routed through the popup pipeline (built later in this fn) —
            // skip the WM/CSD bookkeeping for them.
            if tl
                .x11_surface
                .as_ref()
                .map(|x| {
                    x.user_data()
                        .get::<crate::wayland::xwayland::X11OverrideRedirect>()
                        .is_some()
                })
                .unwrap_or(false)
            {
                continue;
            }
            let (gx_xdg, gy_xdg, gw_xdg, gh_xdg, app_id) = with_states(&tl.surface, |states| {
                let mut guard = states.cached_state.get::<SurfaceCachedState>();
                let geom = guard
                    .current()
                    .geometry
                    .map(|r| (r.loc.x, r.loc.y, r.size.w, r.size.h))
                    .unwrap_or((0, 0, 0, 0));
                let app_id = states
                    .data_map
                    .get::<XdgToplevelSurfaceData>()
                    .and_then(|d| d.lock().ok()?.app_id.clone())
                    .unwrap_or_default();
                (geom.0, geom.1, geom.2, geom.3, app_id)
            });
            let (bw, bh, auto_bbox) = {
                let p = tl.pixels.lock().unwrap();
                let bbox = crate::wayland_state::detect_visible_bbox(&p.pixels, p.width, p.height);
                (p.width as i32, p.height as i32, bbox)
            };
            let (gx, gy, gw, gh) = if gw_xdg > 0 && gh_xdg > 0 {
                (gx_xdg, gy_xdg, gw_xdg, gh_xdg)
            } else if let Some(b) = auto_bbox {
                b
            } else {
                (0, 0, bw, bh)
            };
            // SSD vs CSD is protocol-authoritative: `tl.csd` is set by
            // the xdg-decoration / KDE-server-decoration handlers and by
            // `ack_configure`'s mirror — no pixel/buffer heuristics
            // layered on top. (This block previously OR'd in a
            // `has_padding` heuristic and pass 2 stickily forced csd to
            // true; both papered over the real defect — decoration
            // negotiation wasn't authoritative — and could mis-classify
            // windows in either direction.)
            let has_resize_transaction = state.has_pending_xdg_resize_transaction(&tl.surface);
            metas.push(ToplevelMeta {
                surface: tl.surface.clone(),
                gx,
                gy,
                gw,
                gh,
                bw,
                bh,
                csd_now: tl.csd,
                csd_verdict: tl.csd,
                is_resizing: Some(i) == resizing_idx || has_resize_transaction,
                app_id,
            });
        }

        // Pass 2 — apply: write csd + geom_w/h to WindowState, mirror csd
        // back to ToplevelInfo, push buffer dims into WM geometry springs.
        for m in &metas {
            if let Some(win) = self
                .wm
                .windows
                .values_mut()
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
            // No sticky csd-promotion here — the authority is the
            // decoration protocol, mirrored into `tl.csd` by the
            // decoration handlers + `ack_configure`.
            let _ = m.csd_now;
            // Push the VISIBLE window-geometry size to the WM, not the
            // raw buffer dims. GTK CSD apps render a buffer that's
            // larger than the visible window — typically ~30 px of
            // shadow padding on every side — and report the actual
            // visible rect via xdg_surface.set_window_geometry. If we
            // store the buffer size in `win.w/h`, the chrome ends up
            // sized to "visible + padding" and the WM hit-test rect
            // (and outer-frame border) extend out into the shadow
            // padding region, leaving the actual GTK content offset
            // inside a too-large frame. Using `gw/gh` (when set)
            // makes the chrome match the visible rect; the buffer
            // still renders at `bw×bh` cropped to `gx,gy,gw,gh` via
            // content-clip.
            if !m.is_resizing && m.bw > 0 && m.bh > 0 {
                let (uw, uh) = if m.gw > 0 && m.gh > 0 {
                    (m.gw, m.gh)
                } else {
                    (m.bw, m.bh)
                };
                self.wm.update_geometry(&m.surface, uw, uh);
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

            let buf_w = client_data.width as i32;
            let buf_h = client_data.height as i32;
            let current_version = client_data.version;
            // Reuse the cached `slint::Image` whenever the toplevel's
            // buffer hasn't advanced since we last built one. Allocating a
            // fresh `SharedPixelBuffer` per frame for an unchanged client
            // (e.g. during focus crossfade, drag, alt-tab) was a multi-MB
            // memcpy + GPU re-upload for nothing.
            let texture = match self.client_image_cache.get(&win.id) {
                Some((cached_version, img)) if *cached_version == current_version => {
                    drop(client_data);
                    img.clone()
                }
                _ => {
                    let pixel_buf = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(
                        &client_data.pixels,
                        client_data.width,
                        client_data.height,
                    );
                    drop(client_data);
                    let img = slint::Image::from_rgba8_premultiplied(pixel_buf);
                    self.client_image_cache
                        .insert(win.id, (current_version, img.clone()));
                    img
                }
            };

            // Use the per-toplevel visible-rect we already worked out in
            // pass 1 (xdg geom rect → auto-detected bbox → full buffer).
            // Identical source-of-truth in Rust and Slint: source-clip on
            // the Image and the chrome size both come from this rect, so
            // image-fit:fill renders 1:1 and text stays crisp.
            let (geom_x, geom_y, geom_w, geom_h) = metas
                .iter()
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
                if win.title.is_empty() {
                    "Window".to_string()
                } else {
                    win.title.clone()
                }
            });

            // Drive the per-window focus_t spring (200 ms) so the chrome
            // shader crossfades active → inactive (and vice versa) smoothly.
            self.theme.set_window_focused(win.id, win.focused);

            // ── Build the per-surface render-element list (Phase 3). ───
            // Walks the toplevel's surface tree (toplevel itself + every
            // subsurface), pulls each surface's already-imported pixels
            // from `toplevel.surface_pixels` (populated by
            // `import_shm_per_surface` on commit), and emits one
            // SurfaceItem per surface. Coordinates are in window-local
            // space — i.e. (0, 0) is the geom rect's top-left, the
            // toplevel surface's offset is (-geom_x, -geom_y) so its
            // buffer origin aligns with the chrome rect.
            //
            // Subsurfaces use `surface_view.offset` (folds in subsurface
            // location, wl_surface.offset() buffer_delta, viewporter
            // position) so animated offsets work. dst size comes from
            // `surface_view.dst` (post-buffer-scale logical pixels).
            let surfaces_model: Vec<crate::SurfaceItem> = {
                use smithay::backend::renderer::utils::RendererSurfaceStateUserData;
                use smithay::reexports::wayland_server::Resource;
                use smithay::wayland::compositor::{with_surface_tree_downward, TraversalAction};
                let surface_pixels = toplevel.surface_pixels.lock().unwrap();
                let mut out: Vec<crate::SurfaceItem> = Vec::new();
                with_surface_tree_downward(
                    &win.surface,
                    (0i32, 0i32),
                    |sub, states, parent_offset| {
                        let mut my_offset = *parent_offset;
                        // Per-surface view_offset (skip for the root;
                        // the root anchors to (0,0) in this local frame
                        // and we shift the whole thing by -geom below).
                        if sub != &win.surface {
                            let view_off = states
                                .data_map
                                .get::<RendererSurfaceStateUserData>()
                                .and_then(|d| {
                                    d.lock().ok().and_then(|s| s.view()).map(|v| v.offset)
                                });
                            if let Some(o) = view_off {
                                my_offset.0 += o.x;
                                my_offset.1 += o.y;
                            }
                        }
                        TraversalAction::DoChildren(my_offset)
                    },
                    |sub, states, parent_offset| {
                        let mut my_offset = *parent_offset;
                        let view = states
                            .data_map
                            .get::<RendererSurfaceStateUserData>()
                            .and_then(|d| d.lock().ok().and_then(|s| s.view()));
                        if sub != &win.surface {
                            if let Some(v) = view {
                                my_offset.0 += v.offset.x;
                                my_offset.1 += v.offset.y;
                            }
                        }
                        let key = sub.id().protocol_id();
                        let Some(data) = surface_pixels.get(&key) else {
                            return;
                        };
                        if data.width == 0 || data.height == 0 {
                            return;
                        }
                        // dst size in compositor-logical px; falls back to
                        // raw buffer dims if surface_view isn't populated
                        // (e.g. before first renderer-side commit).
                        let (dst_w, dst_h) = view
                            .map(|v| (v.dst.w, v.dst.h))
                            .filter(|(w, h)| *w > 0 && *h > 0)
                            .unwrap_or((data.width as i32, data.height as i32));
                        // Source-clip rect in BUFFER pixels — wp_viewporter
                        // src crop. SurfaceView.src is in logical pixels;
                        // we round to buffer integers. None / zero-sized
                        // src means no crop.
                        let (src_x, src_y, src_w, src_h) = match view.map(|v| v.src) {
                            Some(r) if r.size.w > 0.0 && r.size.h > 0.0 => (
                                r.loc.x.round() as i32,
                                r.loc.y.round() as i32,
                                r.size.w.round() as i32,
                                r.size.h.round() as i32,
                            ),
                            _ => (0, 0, data.width as i32, data.height as i32),
                        };
                        // Convert from "compositor coords with toplevel at
                        // (0,0)" to "window-local with geom-rect at (0,0)".
                        // The toplevel surface's buffer origin sits
                        // -geom_x, -geom_y from where we render.
                        let x_in_window = my_offset.0 - geom_x;
                        let y_in_window = my_offset.1 - geom_y;
                        // Build / cache slint::Image keyed by (window_id,
                        // surface_id, version). Reusing across frames
                        // skips the per-frame SharedPixelBuffer copy.
                        let cache_key = (win.id, key);
                        let img = match self.client_image_cache_per_surface.get(&cache_key) {
                            Some((v, img)) if *v == data.version => img.clone(),
                            _ => {
                                let pixel_buf =
                                    slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(
                                        &data.pixels,
                                        data.width,
                                        data.height,
                                    );
                                let img = slint::Image::from_rgba8_premultiplied(pixel_buf);
                                self.client_image_cache_per_surface
                                    .insert(cache_key, (data.version, img.clone()));
                                img
                            }
                        };
                        out.push(crate::SurfaceItem {
                            id: key as i32,
                            x_in_window,
                            y_in_window,
                            w: dst_w,
                            h: dst_h,
                            src_x,
                            src_y,
                            src_w,
                            src_h,
                            texture: img,
                        });
                    },
                    |_, _, _| true,
                );
                out
            };
            let surfaces = slint::ModelRc::new(slint::VecModel::from(surfaces_model));

            items.push(crate::WindowItem {
                id: win.id,
                title: SharedString::from(title.clone()),
                x: win.anim.current_x(),
                y: win.anim.current_y(),
                w: win.anim.current_w(),
                h: win.anim.current_h(),
                focused: win.focused,
                texture,
                surfaces,
                icon: slint::Image::default(),
                anim_opacity: win.anim.opacity.value_f32().clamp(0.0, 1.0),
                anim_scale: win.anim.scale.value_f32().clamp(0.0, 2.0),
                alt_tab_selected: win.alt_tab_selected,
                geom_x,
                geom_y,
                geom_w,
                geom_h,
                csd: toplevel.csd,
                maximized: win.maximized,
                // Spring-animated outer corner radius (logical px) — eased
                // between the themed radius and 0 across a maximize.
                corner_radius: win.anim.corner_radius.value() as f32,
                // Resizing = geometry springs mid-flight (maximize /
                // unmaximize) OR this window is being interactively
                // drag-resized / waiting for the final resize commit. Drives
                // the stretched-texture render path.
                resizing: !win.anim.geo_w.is_done()
                    || !win.anim.geo_h.is_done()
                    || resizing_idx
                        .and_then(|i| state.toplevels.get(i))
                        .map(|t| t.surface == win.surface)
                        .unwrap_or(false)
                    || state.has_pending_xdg_resize_transaction(&win.surface),
            });
        }

        // Push the focused window's title into the panel's "focused-app"
        // slot. Falls back to "Desktop" when nothing is focused so the panel
        // is never blank — matches GNOME's "Activities"/macOS finder pattern.
        let slint_overlay_open = self.slint_pointer_overlay_open();
        if let Some(ui) = self.ui.as_ref() {
            let focused_title: String = items
                .iter()
                .find(|it| it.focused)
                .map(|it| it.title.to_string())
                .unwrap_or_else(|| "Desktop".to_string());
            ui.set_focused_app(SharedString::from(focused_title));

            // Drain any appmenu fetch results delivered by worker threads
            // since the last frame and apply them to the Slint models.
            loop {
                let next = self.appmenu_results.lock().unwrap().pop_front();
                let Some(result) = next else { break };
                match result {
                    MenuFetchResult::Bar(nodes) => {
                        let bar: Vec<crate::GlobalMenuTopItem> = nodes
                            .into_iter()
                            .filter(|n| !n.separator && !n.label.is_empty())
                            .map(|n| crate::GlobalMenuTopItem {
                                id: n.id,
                                label: SharedString::from(n.label),
                            })
                            .collect();
                        tracing::debug!("appmenu: menu bar applied, {} entries", bar.len());
                        ui.set_global_menu_bar(slint::ModelRc::new(VecModel::from(bar)));
                    }
                    MenuFetchResult::Submenu {
                        items,
                        x,
                        open_index,
                    } => {
                        use crate::MenuItem;
                        let menu: Vec<MenuItem> = items
                            .into_iter()
                            .map(|n| {
                                if n.separator {
                                    MenuItem {
                                        id: -1,
                                        label: SharedString::default(),
                                        accelerator: SharedString::default(),
                                        separator: true,
                                        enabled: false,
                                    }
                                } else {
                                    MenuItem {
                                        id: n.id,
                                        label: SharedString::from(if n.has_submenu {
                                            format!("{}  \u{25B8}", n.label)
                                        } else {
                                            n.label
                                        }),
                                        accelerator: SharedString::default(),
                                        separator: false,
                                        enabled: n.enabled,
                                    }
                                }
                            })
                            .collect();
                        if menu.is_empty() {
                            ui.set_global_menu_open_index(-1);
                        } else {
                            ui.set_global_menu_items(slint::ModelRc::new(VecModel::from(menu)));
                            ui.set_global_menu_x(x);
                            ui.set_global_menu_y(34);
                            ui.set_global_menu_open_index(open_index);
                            ui.set_global_menu_selected(-1);
                            ui.set_global_menu_open(true);
                        }
                    }
                    MenuFetchResult::TrayMenu {
                        items,
                        x,
                        y,
                        sni_id,
                    } => {
                        use crate::MenuItem;
                        // dbusmenu ids can be 0 or negative; MenuItem reserves
                        // -1 for separators, so remap non-positive clickable
                        // ids onto a growing fallback range.
                        let mut row_id: i32 = 1_000_000;
                        let menu: Vec<MenuItem> = items
                            .into_iter()
                            .map(|n| {
                                if n.separator {
                                    MenuItem {
                                        id: -1,
                                        label: SharedString::default(),
                                        accelerator: SharedString::default(),
                                        separator: true,
                                        enabled: false,
                                    }
                                } else {
                                    let id = if n.id <= 0 {
                                        row_id += 1;
                                        row_id - 1
                                    } else {
                                        n.id
                                    };
                                    MenuItem {
                                        id,
                                        label: SharedString::from(n.label),
                                        accelerator: SharedString::default(),
                                        separator: false,
                                        enabled: n.enabled,
                                    }
                                }
                            })
                            .collect();
                        if !menu.is_empty() {
                            ui.set_tray_menu_items(slint::ModelRc::new(VecModel::from(menu)));
                            ui.set_tray_menu_x(x);
                            ui.set_tray_menu_y(y);
                            ui.set_tray_menu_id(sni_id);
                            ui.set_tray_menu_open(true);
                        }
                    }
                }
            }

            // Global menu (KDE-style appmenu): when keyboard focus moves to a
            // window whose exported menu address differs from the cached one,
            // refetch the top-level bar (or clear it if the new window has no
            // appmenu). The address is also stashed in `appmenu_addr` for the
            // panel's menu-click callbacks.
            let focused_appmenu: Option<(String, String)> = self
                .wm
                .windows
                .values()
                .find(|w| w.focused)
                .and_then(|w| state.toplevels.iter().find(|t| t.surface == w.surface))
                .and_then(|t| t.appmenu.clone());
            if *self.appmenu_addr.borrow() != focused_appmenu {
                tracing::debug!(
                    "appmenu: focused window menu changed → {:?}",
                    focused_appmenu
                );
                *self.appmenu_addr.borrow_mut() = focused_appmenu.clone();
                // Focus moved — any open submenu belongs to the old window.
                ui.set_global_menu_open(false);
                ui.set_global_menu_open_index(-1);
                match focused_appmenu {
                    Some((service, path)) => {
                        let results = self.appmenu_results.clone();
                        crate::dbusmenu::fetch_appmenu_children(service, path, 0, move |nodes| {
                            results
                                .lock()
                                .unwrap()
                                .push_back(MenuFetchResult::Bar(nodes));
                        });
                    }
                    None => {
                        ui.set_global_menu_bar(slint::ModelRc::new(VecModel::<
                            crate::GlobalMenuTopItem,
                        >::default(
                        )));
                    }
                }
            }

            // Honour `wl_pointer.set_cursor` requests from clients:
            //   * Hidden  → set cursor-visible=false (video / drawing apps).
            //   * Named   → look up the named cursor in the system Xcursor
            //               theme and override our default — this is what
            //               makes the cursor change to text-beam over text
            //               fields, pointer over links, wait spinners, etc.
            //   * Surface → not implemented yet; fall back to compositor
            //               default so users still see something.
            if slint_overlay_open {
                self.current_cursor = CursorKind::Arrow;
                let img = self.cursor_renderer.get(CursorKind::Arrow);
                let (hx, hy) = self.cursor_renderer.hotspot(CursorKind::Arrow);
                ui.set_cursor_visible(true);
                ui.set_cursor_image(img);
                ui.set_cursor_hotspot_x(hx);
                ui.set_cursor_hotspot_y(hy);
                ui.set_cursor_size(CursorRenderer::size());
            } else {
                use smithay::input::pointer::CursorIcon;
                use smithay::input::pointer::CursorImageStatus;
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
                        if let Some((img, (hx, hy))) = self.cursor_renderer.get_dynamic(icon.name())
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
                            let pixel_buf =
                                slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(
                                    &p.pixels, p.width, p.height,
                                );
                            let img = slint::Image::from_rgba8_premultiplied(pixel_buf);
                            let (cw, ch) = (p.width as f32, p.height as f32);
                            drop(p);
                            // Read hotspot from the surface's cached state.
                            use smithay::input::pointer::CursorImageSurfaceData;
                            use smithay::wayland::compositor::with_states;
                            let (hx, hy) = with_states(surf, |states| {
                                states
                                    .data_map
                                    .get::<CursorImageSurfaceData>()
                                    .and_then(|d| {
                                        d.lock().ok().map(|attrs| {
                                            (attrs.hotspot.x as f32, attrs.hotspot.y as f32)
                                        })
                                    })
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
            let cur_idx =
                (0..model.row_count()).find(|&j| model.row_data(j).map(|r| r.id) == Some(item.id));
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
        debug!(
            "update_windows: pushed {} items, wm has {} windows",
            items.len(),
            self.wm.windows.len()
        );

        // Drop cached `slint::Image`s for windows that no longer exist
        // (closed, swept) so the cache doesn't grow across a long session.
        if self.client_image_cache.len() > self.wm.windows.len() {
            let live: std::collections::HashSet<i32> =
                self.wm.windows.values().map(|w| w.id).collect();
            self.client_image_cache.retain(|id, _| live.contains(id));
        }

        // ── Build PopupItem list from state.popups ──────────────────────────
        // Resolve each popup's compositor-space position by walking up the
        // parent chain to a toplevel. Popup geometry comes from Smithay's
        // committed xdg_popup state, not the old PopupInfo snapshot, so
        // unconstrain/reposition configures are reflected after the client's
        // ack+commit lifecycle.
        // Popups can have popups as parents (nested menus); cap the walk so
        // a malformed chain can't loop forever.
        let mut popup_items: Vec<crate::PopupItem> = Vec::new();
        for (pi, popup) in state.popups.iter().enumerate() {
            // Skip popups whose pixel buffer hasn't arrived yet.
            let (bw, bh, has_pixels) = {
                let p = popup.pixels.lock().unwrap();
                (p.width as i32, p.height as i32, p.width > 0)
            };
            if !has_pixels {
                continue;
            }

            let Some(geometry) = popup.configured_geometry() else {
                continue;
            };

            // Walk parent chain to find the absolute compositor position.
            let mut abs_x = geometry.loc.x;
            let mut abs_y = geometry.loc.y;
            let mut cur_parent = popup.parent.clone();
            for _ in 0..16 {
                if let Some(p) = state.popups.iter().find(|p| p.surface == cur_parent) {
                    let Some(parent_geometry) = p.configured_geometry() else {
                        break;
                    };
                    abs_x += parent_geometry.loc.x;
                    abs_y += parent_geometry.loc.y;
                    cur_parent = p.parent.clone();
                    continue;
                }
                if let Some(tl_win) = self.wm.windows.values().find(|w| w.surface == cur_parent) {
                    abs_x += tl_win.anim.current_x();
                    // For SSD parents the popup is positioned relative to
                    // the CONTENT area, which sits below our titlebar.
                    let titlebar = if tl_win.csd {
                        0
                    } else {
                        crate::wm::TITLEBAR_HEIGHT as i32
                    };
                    abs_y += tl_win.anim.current_y() + titlebar;
                }
                if let Some(layer) = state
                    .layer_surfaces
                    .iter()
                    .find(|layer| layer.surface.wl_surface() == &cur_parent)
                {
                    abs_x += layer.x;
                    abs_y += layer.y;
                }
                break;
            }

            // Build the legacy single-texture (still consumed by the
            // PopupItem.texture fallback path).
            let p = popup.pixels.lock().unwrap();
            let pixel_buf = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(
                &p.pixels, p.width, p.height,
            );
            let texture = slint::Image::from_rgba8_premultiplied(pixel_buf);
            drop(p);

            // Per-surface render-element list for popups — same pattern
            // as toplevels in update_windows. Walks the popup's surface
            // tree, pulls each surface from `surface_pixels`, emits one
            // SurfaceItem per visible wl_surface. Required for libadwaita
            // popovers / GTK menus that animate via subsurfaces.
            let popup_surfaces_model: Vec<crate::SurfaceItem> = {
                use smithay::backend::renderer::utils::RendererSurfaceStateUserData;
                use smithay::reexports::wayland_server::Resource;
                use smithay::wayland::compositor::{with_surface_tree_downward, TraversalAction};
                let surface_pixels = popup.surface_pixels.lock().unwrap();
                let mut out: Vec<crate::SurfaceItem> = Vec::new();
                with_surface_tree_downward(
                    &popup.surface,
                    (0i32, 0i32),
                    |sub, states, parent_offset| {
                        let mut my_offset = *parent_offset;
                        if sub != &popup.surface {
                            let view_off = states
                                .data_map
                                .get::<RendererSurfaceStateUserData>()
                                .and_then(|d| {
                                    d.lock().ok().and_then(|s| s.view()).map(|v| v.offset)
                                });
                            if let Some(o) = view_off {
                                my_offset.0 += o.x;
                                my_offset.1 += o.y;
                            }
                        }
                        TraversalAction::DoChildren(my_offset)
                    },
                    |sub, states, parent_offset| {
                        let mut my_offset = *parent_offset;
                        let view = states
                            .data_map
                            .get::<RendererSurfaceStateUserData>()
                            .and_then(|d| d.lock().ok().and_then(|s| s.view()));
                        if sub != &popup.surface {
                            if let Some(v) = view {
                                my_offset.0 += v.offset.x;
                                my_offset.1 += v.offset.y;
                            }
                        }
                        let key = sub.id().protocol_id();
                        let Some(data) = surface_pixels.get(&key) else {
                            return;
                        };
                        if data.width == 0 || data.height == 0 {
                            return;
                        }
                        let (dst_w, dst_h) = view
                            .map(|v| (v.dst.w, v.dst.h))
                            .filter(|(w, h)| *w > 0 && *h > 0)
                            .unwrap_or((data.width as i32, data.height as i32));
                        let (src_x, src_y, src_w, src_h) = match view.map(|v| v.src) {
                            Some(r) if r.size.w > 0.0 && r.size.h > 0.0 => (
                                r.loc.x.round() as i32,
                                r.loc.y.round() as i32,
                                r.size.w.round() as i32,
                                r.size.h.round() as i32,
                            ),
                            _ => (0, 0, data.width as i32, data.height as i32),
                        };
                        // Popups anchor at (0, 0) in their own local
                        // coord system — Slint positions the whole popup
                        // by its (x, y), and surface offsets stack from
                        // that origin. No geom-rect inset like toplevels.
                        let pixel_buf =
                            slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(
                                &data.pixels,
                                data.width,
                                data.height,
                            );
                        let img = slint::Image::from_rgba8_premultiplied(pixel_buf);
                        out.push(crate::SurfaceItem {
                            id: key as i32,
                            x_in_window: my_offset.0,
                            y_in_window: my_offset.1,
                            w: dst_w,
                            h: dst_h,
                            src_x,
                            src_y,
                            src_w,
                            src_h,
                            texture: img,
                        });
                    },
                    |_, _, _| true,
                );
                out
            };
            let popup_surfaces = slint::ModelRc::new(slint::VecModel::from(popup_surfaces_model));

            // If the client set an xdg_surface.set_window_geometry, the
            // VISIBLE rect within the buffer is (geom_x, geom_y, geom_w,
            // geom_h). The buffer itself is bw × bh (with shadow gutter
            // around the visible rect). Render the visible rect at
            // (abs_x, abs_y) and crop the buffer accordingly. If the
            // client didn't set a window-geometry (geom_w == 0), fall
            // back to "whole buffer is visible".
            let (w, h, vis_x, vis_y) = if popup.geom_w > 0 && popup.geom_h > 0 {
                (popup.geom_w, popup.geom_h, popup.geom_x, popup.geom_y)
            } else if geometry.size.w > 0 && geometry.size.h > 0 {
                (geometry.size.w, geometry.size.h, 0, 0)
            } else {
                (bw, bh, 0, 0)
            };
            popup_items.push(crate::PopupItem {
                id: pi as i32,
                x: abs_x,
                y: abs_y,
                w,
                h,
                buf_w: bw,
                buf_h: bh,
                vis_x,
                vis_y,
                texture,
                surfaces: popup_surfaces,
            });
        }

        // ── X11 override-redirect windows (menus / tooltips / drag indicators)
        //     piggyback on the popup pipeline. They're tracked in
        //     `state.toplevels` so SHM/dmabuf import lands in `tl.pixels`,
        //     positioned at absolute screen coords by the X11 client itself
        //     (we re-read X11Surface::geometry every frame because the client
        //     can move them at any time without a configure round-trip).
        //
        //     IDs use a high offset to avoid collision with xdg_popup ids
        //     (which are indices into `state.popups`, well below 1<<20).
        const X11_OR_ID_BASE: i32 = 1 << 20;
        for (oi, tl) in state.toplevels.iter().enumerate() {
            let Some(x11) = tl.x11_surface.as_ref() else {
                continue;
            };
            if x11
                .user_data()
                .get::<crate::wayland::xwayland::X11OverrideRedirect>()
                .is_none()
            {
                continue;
            }
            let (bw, bh, has_pixels) = {
                let p = tl.pixels.lock().unwrap();
                (p.width as i32, p.height as i32, p.width > 0)
            };
            if !has_pixels {
                continue;
            }
            let geo = x11.geometry();
            let p = tl.pixels.lock().unwrap();
            let pixel_buf = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(
                &p.pixels, p.width, p.height,
            );
            let texture = slint::Image::from_rgba8_premultiplied(pixel_buf);
            drop(p);
            let w = if geo.size.w > 0 { geo.size.w } else { bw };
            let h = if geo.size.h > 0 { geo.size.h } else { bh };
            popup_items.push(crate::PopupItem {
                id: X11_OR_ID_BASE + oi as i32,
                x: geo.loc.x,
                y: geo.loc.y,
                w,
                h,
                // X11 OR windows have no window-geometry concept; the
                // whole buffer is the visible rect.
                buf_w: bw,
                buf_h: bh,
                vis_x: 0,
                vis_y: 0,
                texture,
                // X11 OR windows currently keep using the legacy single
                // texture. Migrating them needs the same per-surface
                // walk as xdg-popups but xwayland surfaces don't tend
                // to use subsurfaces, so leaving them on the fallback
                // path is fine for now.
                surfaces: slint::ModelRc::new(slint::VecModel::<crate::SurfaceItem>::default()),
            });
        }

        // In-place diff for popups (same pattern as windows): keep order;
        // remove rows whose id disappeared; update existing; insert new.
        let pmodel = &self.popups_model;
        let mut i = pmodel.row_count();
        while i > 0 {
            i -= 1;
            let still = popup_items
                .iter()
                .any(|it| Some(it.id) == pmodel.row_data(i).map(|r| r.id));
            if !still {
                pmodel.remove(i);
            }
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

    /// Drain `state.pending_capture_frames` and service each via a sync GPU
    /// readback of `final_tex`. Called from the main loop right after
    /// `render_frame` so the captured pixels reflect the frame the client
    /// just saw on screen. Skipped silently when the renderer hasn't yet
    /// populated `final_tex` — clients re-request next vsync.
    fn process_capture_frames(&mut self, state: &mut SpikeState) {
        if state.pending_capture_frames.is_empty() {
            return;
        }
        let Some(gpu_window) = self.gpu_window.as_ref() else {
            // Renderer not initialised — fail every queued frame so clients
            // don't hang waiting for a never-arriving response.
            for frame in state.pending_capture_frames.drain(..) {
                frame.fail(smithay::wayland::image_copy_capture::CaptureFailureReason::Unknown);
            }
            return;
        };
        let Some(final_tex) = self.final_texture.as_ref() else {
            for frame in state.pending_capture_frames.drain(..) {
                frame.fail(smithay::wayland::image_copy_capture::CaptureFailureReason::Unknown);
            }
            return;
        };
        let (width, height) = self.render_texture_size;
        if width == 0 || height == 0 {
            for frame in state.pending_capture_frames.drain(..) {
                frame.fail(smithay::wayland::image_copy_capture::CaptureFailureReason::Unknown);
            }
            return;
        }
        let device = &gpu_window.wgpu_device;
        let queue = &gpu_window.wgpu_queue;
        let presented = self
            .last_present_time
            .map(std::time::Duration::from)
            .unwrap_or(std::time::Duration::ZERO);
        let ctx = crate::screencopy::CaptureContext {
            device,
            queue,
            final_tex,
            width,
            height,
        };
        let frames: Vec<_> = state.pending_capture_frames.drain(..).collect();
        for frame in frames {
            crate::screencopy::process_frame(&ctx, frame, presented);
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

        // Refresh the DnD icon snapshot every loop. The icon surface is
        // committed by the source client at the same cadence as any other
        // surface; we mirror its pixels into a renderer-side buffer here
        // because `render_frame` runs without `state` access. Force a
        // redraw whenever the icon is actively drawn so cursor motion
        // updates the on-screen position without waiting for slint dirty.
        let prev_active = self.dnd_icon_snapshot.is_some();
        self.dnd_icon_snapshot = if let Some(icon) = state.dnd_icon.as_ref() {
            let p = state.dnd_icon_pixels.lock().unwrap();
            if p.width > 0 && p.height > 0 {
                Some(DndIconSnapshot {
                    pixels: p.pixels.clone(),
                    width: p.width,
                    height: p.height,
                    cursor_x: state.pointer_pos.0,
                    cursor_y: state.pointer_pos.1,
                    hotspot_x: icon.offset.x,
                    hotspot_y: icon.offset.y,
                })
            } else {
                None
            }
        } else {
            None
        };
        if self.dnd_icon_snapshot.is_some() || prev_active {
            if let Some(gpu_window) = self.gpu_window.as_ref() {
                gpu_window.mark_dirty();
            }
        }

        // Lock-surface snapshot: same pattern as DnD. While locked the
        // renderer overwrites final_tex with the lock surface pixels, so
        // it needs a state-free copy each frame.
        let prev_locked = self.lock_surface_snapshot.is_some();
        self.lock_surface_snapshot = if state.session_locked {
            state.lock_surfaces.first().and_then(|li| {
                let p = li.pixels.lock().unwrap();
                (p.width > 0 && p.height > 0).then(|| LockSurfaceSnapshot {
                    pixels: p.pixels.clone(),
                    width: p.width,
                    height: p.height,
                })
            })
        } else {
            None
        };
        if self.lock_surface_snapshot.is_some() || prev_locked {
            if let Some(gpu_window) = self.gpu_window.as_ref() {
                gpu_window.mark_dirty();
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
            let surface = self
                .wm
                .windows
                .values()
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
            let surface = self
                .wm
                .windows
                .values()
                .find(|w| w.id == id)
                .map(|w| w.surface.clone());
            self.wm.minimize_by_id(id);
            // Mirror to X11 so xwayland clients learn they're now hidden.
            if let Some(surf) = surface {
                self.sync_x11_window_state(&surf, state, None, None, Some(true));
            }
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
            let (new_w, new_h) = self
                .wm
                .windows
                .values()
                .find(|w| w.id == id)
                .map(|w| (w.w, w.h))
                .unwrap_or((800, 600));
            let (surface, now_max) = self
                .wm
                .windows
                .values()
                .find(|w| w.id == id)
                .map(|w| (w.surface.clone(), w.maximized))
                .map(|(s, m)| (Some(s), m))
                .unwrap_or((None, false));
            if let Some(surf) = surface {
                self.send_configure(&surf, new_w, new_h, state);
                state.update_reactive_popups_for_toplevel(&surf);
                // Mirror to X11. We don't try to gate on "is this an X11
                // window" — the helper does that and is a no-op otherwise.
                self.sync_x11_window_state(&surf, state, Some(now_max), None, None);
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
            info!(
                "WM: dock-action app={} action={} → {} window(s)",
                app_id,
                action,
                ids.len()
            );
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
                        let surface = self
                            .wm
                            .windows
                            .values()
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

        // ── xdg-shell client requests ─────────────────────────────────────
        self.process_xdg_requests(state);
    }

    /// Drain `pending_xdg_*` queues populated by the wayland-thread
    /// xdg-shell handlers and apply them. Move/resize start an
    /// `ActiveDrag` using the current pointer position; max/min/fullscreen
    /// route through the existing WM helpers and reply with a configure.
    fn process_xdg_requests(&mut self, state: &mut SpikeState) {
        // Move
        let moves: Vec<WlSurface> = state.pending_xdg_move.drain(..).collect();
        for surface in moves {
            // Skip if a drag is already active — clients sometimes re-fire
            // move during an in-flight grab.
            if self.active_drag.is_some() {
                continue;
            }
            let Some(idx) = state.toplevels.iter().position(|t| t.surface == surface) else {
                continue;
            };
            let (px, py) = self.pointer_pos;
            // If the window is maximized, arm the drag-to-unmaximize path
            // (same as the SSD titlebar drag). Without this, dragging a
            // maximized CSD window's headerbar slid the full-screen window
            // around instead of restoring it.
            let maximized = self
                .wm
                .id_for_surface(&surface)
                .and_then(|id| self.wm.windows.values().find(|w| w.id == id))
                .is_some_and(|w| w.maximized);
            let t = &state.toplevels[idx];
            self.active_drag = Some(ActiveDrag::Move {
                toplevel_idx: idx,
                offset_x: px - t.x as f64,
                offset_y: py - t.y as f64,
                pending_unmaximize: if maximized { Some((px, py)) } else { None },
            });
            debug!(
                "xdg client move_request → ActiveDrag::Move on tl#{} (maximized={})",
                idx, maximized
            );
        }

        // Resize
        let resizes: Vec<(WlSurface, ResizeEdge)> = state.pending_xdg_resize.drain(..).collect();
        for (surface, edge) in resizes {
            if self.active_drag.is_some() {
                continue;
            }
            let Some(idx) = state.toplevels.iter().position(|t| t.surface == surface) else {
                continue;
            };
            let (px, py) = self.pointer_pos;
            let t = &state.toplevels[idx];
            // WM owns the authoritative logical size; fall back to buffer
            // dims if the window isn't tracked yet (shouldn't happen post-
            // sync_new_toplevels, but stays robust).
            let (w, h) = self
                .wm
                .id_for_surface(&t.surface)
                .and_then(|id| self.wm.windows.values().find(|w| w.id == id))
                .map(|w| (w.w, w.h))
                .unwrap_or_else(|| {
                    let pix = t.pixels.lock().unwrap();
                    (pix.width as i32, pix.height as i32)
                });
            self.active_drag = Some(ActiveDrag::Resize {
                toplevel_idx: idx,
                edge,
                start_ptr_x: px,
                start_ptr_y: py,
                start_geom: WindowGeomSnapshot {
                    x: t.x,
                    y: t.y,
                    w,
                    h,
                },
                last_configure_at: None,
                dock_snap_engaged_at: None,
            });
            debug!(
                "xdg client resize_request {:?} → ActiveDrag::Resize on tl#{}",
                edge, idx
            );
        }

        // Maximize / unmaximize. Treat fullscreen identically — we have no
        // separate fullscreen geometry yet; honour the protocol state but
        // share the maximize geometry.
        let mut max_actions: Vec<(WlSurface, bool)> =
            state.pending_xdg_maximize.drain(..).collect();
        max_actions.append(&mut state.pending_xdg_fullscreen);
        for (surface, want_max) in max_actions {
            let Some(id) = self.wm.id_for_surface(&surface) else {
                continue;
            };
            {
                let win = match self.wm.windows.values_mut().find(|w| w.id == id) {
                    Some(w) => w,
                    None => continue,
                };
                if want_max && !win.maximized {
                    win.start_maximize(self.wm.output_w, self.wm.output_h);
                } else if !want_max && win.maximized {
                    win.start_unmaximize();
                }
            }
            // Reply with a configure carrying the new size.
            let (new_w, new_h) = self
                .wm
                .windows
                .values()
                .find(|w| w.id == id)
                .map(|w| (w.w, w.h))
                .unwrap_or((800, 600));
            self.send_configure(&surface, new_w, new_h, state);
            state.update_reactive_popups_for_toplevel(&surface);
        }

        // Minimize
        let minimizes: Vec<WlSurface> = state.pending_xdg_minimize.drain(..).collect();
        for surface in minimizes {
            if let Some(id) = self.wm.id_for_surface(&surface) {
                self.wm.minimize_by_id(id);
                self.update_focused_surface(state);
            }
        }

        // Restore (X11 unminimize_request — wayland clients can't un-minimize themselves)
        let restores: Vec<WlSurface> = state.pending_xdg_restore.drain(..).collect();
        for surface in restores {
            if let Some(id) = self.wm.id_for_surface(&surface) {
                self.wm.restore_by_id(id);
                self.update_focused_surface(state);
            }
        }
    }

    /// Update `state.active_surface` and keyboard focus from the WM focused window.
    fn update_focused_surface(&self, state: &mut SpikeState) {
        state.active_surface = self.wm.focused_surface();
        if let Some(surface) = &state.active_surface {
            if let Some(kb) = state.seat.get_keyboard() {
                let focus =
                    crate::wayland::xwayland::KeyboardFocusTarget::for_wl_surface(state, surface);
                kb.set_focus(state, Some(focus), SERIAL_COUNTER.next_serial());
            }
        }
    }

    /// Mirror a WM-driven maximize/fullscreen/minimize toggle back to the
    /// X11Surface so xwayland-backed clients see consistent state.
    ///
    /// `pending_xdg_*` queues already cover the *client*-initiated path (the
    /// X11 client asked to maximise → we propagated to the WM). This helper
    /// covers the other direction — when the user double-clicks our title
    /// bar, alt-drags into a snap, or hits the dock "minimise" — so the X11
    /// client doesn't end up with stale `_NET_WM_STATE_MAXIMIZED` flags.
    ///
    /// All three `set_*` calls are no-ops on native wayland toplevels (we
    /// just skip them) — we never modify protocol state from here, only the
    /// X11-side bookkeeping.
    fn sync_x11_window_state(
        &self,
        surface: &WlSurface,
        state: &SpikeState,
        maximized: Option<bool>,
        fullscreen: Option<bool>,
        hidden: Option<bool>,
    ) {
        let Some(x11) = state
            .toplevels
            .iter()
            .find(|t| &t.surface == surface)
            .and_then(|t| t.x11_surface.clone())
        else {
            return;
        };
        if let Some(v) = maximized {
            let _ = x11.set_maximized(v);
        }
        if let Some(v) = fullscreen {
            let _ = x11.set_fullscreen(v);
        }
        if let Some(v) = hidden {
            let _ = x11.set_hidden(v);
        }
    }

    /// Send a close request to the client backing `surface`. Routes to
    /// `xdg_toplevel.send_close` for native wayland clients, or to
    /// `WM_DELETE_WINDOW` via the X11 window for Xwayland clients.
    fn send_xdg_close(&self, surface: &WlSurface, state: &mut SpikeState) {
        for toplevel in &state.toplevels {
            if &toplevel.surface == surface {
                if let Some(t) = toplevel.toplevel.as_ref() {
                    debug!("WM: sending xdg_toplevel.close to client");
                    t.send_close();
                } else if let Some(x) = toplevel.x11_surface.as_ref() {
                    debug!("WM: sending X11 close to xwayland client");
                    let _ = x.close();
                }
                break;
            }
        }
    }

    /// Send a configure with a new size to the client. xdg toplevels get
    /// a real `xdg_toplevel.configure`; Xwayland windows get an X11
    /// `ConfigureNotify` via `X11Surface::configure`.
    fn send_configure(&self, surface: &WlSurface, w: i32, h: i32, state: &mut SpikeState) {
        for toplevel in &state.toplevels {
            if &toplevel.surface == surface {
                if let Some(t) = toplevel.toplevel.as_ref() {
                    debug!("WM: sending xdg_toplevel configure {}x{}", w, h);
                    t.with_pending_state(|s| {
                        s.size = Some((w, h).into());
                    });
                    t.send_configure();
                } else if let Some(x) = toplevel.x11_surface.as_ref() {
                    debug!("WM: sending X11 configure {}x{}", w, h);
                    let mut geo = x.geometry();
                    geo.size.w = w;
                    geo.size.h = h;
                    let _ = x.configure(Some(geo));
                }
                break;
            }
        }
    }

    // `popup_rect` and `popup_surface_under` moved to `renderer/popup.rs`.

    /// Forward a pointer motion event to the wayland client whose surface is under the pointer.
    fn forward_pointer_motion(&self, state: &mut SpikeState, x: f64, y: f64) {
        input_util::forward_pointer_motion(state, x, y, None, None, |state, hit_x, hit_y| {
            self.surface_under_full(state, hit_x, hit_y)
        });
    }

    /// Forward a wl_pointer.axis frame to whatever surface currently has
    /// pointer focus. Wires winit MouseWheel / touchpad scroll events into
    /// the wayland client — without this, scrolling does literally nothing
    /// inside any wayland window.
    fn forward_pointer_axis(
        &self,
        state: &mut SpikeState,
        dx: f64,
        dy: f64,
        discrete_v120: Option<(i32, i32)>,
        is_wheel: bool,
    ) {
        use smithay::backend::input::{Axis, AxisSource};
        use smithay::input::pointer::AxisFrame;

        // Reset idle timer on scroll input.
        state.idle_notifier_state.notify_activity(&state.seat);

        let pointer = match state.seat.get_pointer() {
            Some(p) => p,
            None => return,
        };
        if dx == 0.0 && dy == 0.0 {
            return;
        }

        let time = state.clock.now().as_millis();
        let mut frame = AxisFrame::new(time).source(if is_wheel {
            AxisSource::Wheel
        } else {
            AxisSource::Finger
        });
        if dx != 0.0 {
            frame = frame.value(Axis::Horizontal, dx);
        }
        if dy != 0.0 {
            frame = frame.value(Axis::Vertical, dy);
        }
        if let Some((vx, vy)) = discrete_v120 {
            if vx != 0 {
                frame = frame.v120(Axis::Horizontal, vx);
            }
            if vy != 0 {
                frame = frame.v120(Axis::Vertical, vy);
            }
        }
        // Touchpad scroll-stop signalling (wayland requires it for Finger
        // sources) is left to a future pass; winit's TouchPhase::Ended
        // would be the natural trigger.

        pointer.axis(state, frame);
        pointer.frame(state);
    }

    /// Forward a pointer button event, and on left-press update WM focus.
    fn forward_pointer_button(&mut self, state: &mut SpikeState, button: u32, pressed: bool) {
        use smithay::input::pointer::ButtonEvent;

        let pointer = match state.seat.get_pointer() {
            Some(p) => p,
            None => return,
        };

        // Reset idle timer on user input. See forward_pointer_motion for rationale.
        state.idle_notifier_state.notify_activity(&state.seat);

        // Session-lock gate: forward the button event so the lock surface
        // can react (swaylock toggles password-field focus on click etc),
        // but skip the WM-focus / desktop-menu / drag-init bookkeeping
        // that would otherwise leak interaction to hidden windows.
        if state.session_locked {
            let serial = SERIAL_COUNTER.next_serial();
            let time = state.clock.now().as_millis();
            let button_state = if pressed {
                smithay::backend::input::ButtonState::Pressed
            } else {
                smithay::backend::input::ButtonState::Released
            };
            pointer.button(
                state,
                &ButtonEvent {
                    serial,
                    time,
                    button,
                    state: button_state,
                },
            );
            pointer.frame(state);
            return;
        }

        let (x, y) = self.pointer_pos;
        let over_client_popup = self.client_popup_surface_under(state, x, y).is_some();

        // Popup dismissal on click outside (the only thing approximating
        // popup-grab semantics without a full `PopupManager` refactor).
        // When the user presses anywhere that's NOT a popup, send
        // `xdg_popup.popup_done` to every tracked popup so context menus,
        // GTK comboboxes, Qt menus close cleanly. Without this, popups
        // stay glued on screen until their parent dies.
        if pressed && !over_client_popup && !state.popups.is_empty() {
            let to_dismiss: Vec<_> = state.popups.iter().map(|p| p.popup.clone()).collect();
            for popup in to_dismiss {
                popup.send_popup_done();
            }
        }

        // On left press: update WM focus.
        // - Click on a window  → focus it (existing behaviour).
        // - Click on the desktop (wallpaper, not on any window AND not on the
        //   panel or dock) → unfocus all windows so the panel's focused-app
        //   text clears and keyboard focus is dropped.
        // - Click on panel/dock → leave focus alone (those are shell areas).
        if button == 0x110 && pressed && !over_client_popup {
            let panel_h = crate::wm::PANEL_HEIGHT as f64;
            let in_panel = y < panel_h;
            let in_dock = self.point_in_dock_pill(x, y);
            if let Some(focused_surface) = self.wm.pointer_click_focus(x, y) {
                state.active_surface = Some(focused_surface.clone());
                if let Some(kb) = state.seat.get_keyboard() {
                    let focus = crate::wayland::xwayland::KeyboardFocusTarget::for_wl_surface(
                        state,
                        &focused_surface,
                    );
                    kb.set_focus(state, Some(focus), SERIAL_COUNTER.next_serial());
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

        // (Right-click dismiss is handled in the winit MouseInput
        //  branch — it short-circuits before this point if a menu was
        //  open, so by the time we get here there's nothing open and
        //  it's safe to OPEN a new menu at the cursor.)

        // Right-click on a window TITLEBAR opens the per-window
        // context menu (Minimize / Maximize / Close). We test the hit
        // zone before the desktop check below; only fall through if
        // the cursor wasn't on a titlebar.
        if button == 0x111 && pressed && !over_client_popup {
            let win_rects = self.window_rects(state);
            if let Some(hit) = cursor::hit_test(x, y, &win_rects) {
                if hit.zone == cursor::HitZone::TitleBar {
                    self.open_window_menu(hit.window_id, x, y);
                    return;
                }
            }
        }

        // Right-click on the desktop opens our generic context menu.
        if button == 0x111 && pressed {
            let panel_h = crate::wm::PANEL_HEIGHT as f64;
            let oh = self.wm.output_h as f64;
            let ow = self.wm.output_w as f64;
            let dock_h = crate::wm::DOCK_HEIGHT as f64;
            let on_window = !over_client_popup && self.wm.pointer_click_focus(x, y).is_some();
            let in_panel = y < panel_h;
            let in_dock = self.point_in_dock_pill(x, y);
            if !over_client_popup && !on_window && !in_panel && !in_dock {
                if let Some(ui) = self.ui.as_ref() {
                    // Position the menu at the cursor's top-left, then
                    // clamp it inside the screen with a margin (GNOME-
                    // style). The clamp can shift the menu away from
                    // the cursor — the cursor then sits *inside* the
                    // menu rather than on its corner. Scale-origin =
                    // cursor offset within the menu rect, so the
                    // animation still grows out of the click point.
                    let menu_w = 220.0;
                    let items_model = ui.get_desktop_menu_items();
                    let menu_h = compute_menu_height(&items_model);
                    // Flip when the menu would overflow on either axis,
                    // so the cursor lands exactly at the corresponding
                    // CORNER of the menu (top-left default, top-right
                    // when x-flipped, bottom-left when y-flipped,
                    // bottom-right when both). Matches the GNOME look:
                    // the menu grows AWAY from the cursor, not "fills
                    // toward it from the screen edge". Side fallback
                    // clamps prevent off-screen rectangles when the
                    // cursor is near the screen edge AND the menu is
                    // larger than the available room on either side.
                    let side_pad = 8.0;
                    let bot_pad = 4.0; // gap to dock pill (drop-shadow has its own blur)
                    let x_flipped = x + menu_w + side_pad > ow;
                    let mut mx = if x_flipped { x - menu_w } else { x };
                    mx = mx.clamp(side_pad, (ow - menu_w - side_pad).max(side_pad));
                    let y_flipped = y + menu_h + bot_pad > oh - dock_h;
                    let mut my = if y_flipped { y - menu_h } else { y };
                    my = my.clamp(
                        panel_h + 4.0,
                        (oh - dock_h - menu_h - bot_pad).max(panel_h + 4.0),
                    );
                    let origin_x = (x - mx).clamp(0.0, menu_w);
                    let origin_y = (y - my).clamp(0.0, menu_h);
                    ui.set_desktop_menu_x(mx as i32);
                    ui.set_desktop_menu_y(my as i32);
                    ui.set_desktop_menu_h(menu_h as i32);
                    ui.set_desktop_menu_origin_x(origin_x as i32);
                    ui.set_desktop_menu_origin_y(origin_y as i32);
                    ui.set_desktop_menu_open(true);
                    if let Some(gpu_window) = self.gpu_window.as_ref() {
                        gpu_window.mark_dirty();
                    }
                }
            }
        }

        // Defensive focus refresh: re-run surface_under at the click point
        // and force a pointer.motion if it disagrees with the cached focus.
        // Without this, a button event is silently dropped whenever the
        // immediately-preceding motion happened to land outside the window
        // (e.g. user moves cursor 0.1 px above the geom top, presses), or
        // when the WM's animated geometry shifted between the last motion
        // and this press. The motion call also synthesises the wl_pointer.
        // enter that the client needs before any button event will route.
        if pressed {
            use smithay::input::pointer::MotionEvent;
            use smithay::reexports::wayland_server::Resource;
            use smithay::utils::Point;
            let hit = self.surface_under_full(state, x, y);
            let want_id = hit.as_ref().map(|(s, _, _)| s.id().protocol_id() as i64);
            let have_id = pointer.current_focus().map(|s| s.id().protocol_id() as i64);
            if want_id != have_id {
                let serial_m = SERIAL_COUNTER.next_serial();
                let time_m = state.clock.now().as_millis();
                if let Some((surface, ox, oy)) = hit {
                    tracing::debug!(
                        "pointer focus refresh on press: forcing motion to surface_id={} origin=({:.1},{:.1})",
                        surface.id().protocol_id(), ox, oy,
                    );
                    pointer.motion(
                        state,
                        Some((surface, Point::from((ox, oy)))),
                        &MotionEvent {
                            location: Point::from((x, y)),
                            serial: serial_m,
                            time: time_m,
                        },
                    );
                    pointer.frame(state);
                } else {
                    tracing::debug!(
                        "pointer focus refresh on press: clearing (no hit at click point)"
                    );
                    pointer.motion(
                        state,
                        None,
                        &MotionEvent {
                            location: Point::from((x, y)),
                            serial: serial_m,
                            time: time_m,
                        },
                    );
                    pointer.frame(state);
                }
            }
        }

        let serial = SERIAL_COUNTER.next_serial();
        let time = state.clock.now().as_millis();
        let button_state = if pressed {
            smithay::backend::input::ButtonState::Pressed
        } else {
            smithay::backend::input::ButtonState::Released
        };

        tracing::debug!(
            "pointer.button → button=0x{:x} pressed={} ptr_focus={}",
            button,
            pressed,
            pointer
                .current_focus()
                .map(|s| {
                    use smithay::reexports::wayland_server::Resource;
                    s.id().protocol_id() as i64
                })
                .unwrap_or(-1),
        );
        pointer.button(
            state,
            &ButtonEvent {
                serial,
                time,
                button,
                state: button_state,
            },
        );
        pointer.frame(state);
    }

    /// Update `dock-items` running/focused flags from the current toplevel list.
    fn update_dock_running(&mut self, state: &SpikeState) {
        use smithay::wayland::compositor::with_states;
        use smithay::wayland::shell::xdg::XdgToplevelSurfaceData;

        let app_id_for = |surf: &WlSurface| -> Option<String> {
            with_states(surf, |states| {
                states
                    .data_map
                    .get::<XdgToplevelSurfaceData>()
                    .and_then(|d| d.lock().ok()?.app_id.clone())
            })
        };

        let focused_app_id: Option<String> = state.active_surface.as_ref().and_then(&app_id_for);

        // Collect every running app_id from the toplevel list.
        let mut running_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
        for tl in &state.toplevels {
            if let Some(id) = app_id_for(&tl.surface) {
                if !id.is_empty() {
                    running_ids.insert(id);
                }
            }
        }

        if running_ids == self.last_running_ids {
            return;
        }
        self.last_running_ids = running_ids.clone();

        let Some(ui) = self.ui.as_ref() else { return };

        let matches_pinned = |a: &str, b: &str| -> bool {
            a == b || a.rsplit('.').next() == Some(b) || b.rsplit('.').next() == Some(a)
        };

        let mut items: Vec<DockItem> = self
            .dock_entries
            .iter()
            .map(|entry| {
                let app_id_str = entry.item.app_id.as_str();
                let running = running_ids
                    .iter()
                    .any(|rid| matches_pinned(rid, app_id_str));
                let focused = focused_app_id
                    .as_deref()
                    .is_some_and(|fid| matches_pinned(fid, app_id_str));
                DockItem {
                    icon: entry.item.icon.clone(),
                    app_id: entry.item.app_id.clone(),
                    name: entry.item.name.clone(),
                    running,
                    focused,
                    pinned: entry.item.pinned,
                }
            })
            .collect();

        // Append non-pinned running apps after the pinned ones.
        let pinned_ids: Vec<String> = self
            .dock_entries
            .iter()
            .map(|e| e.item.app_id.to_string())
            .collect();
        let mut extras: Vec<String> = running_ids
            .iter()
            .filter(|rid| !pinned_ids.iter().any(|pid| matches_pinned(rid, pid)))
            .cloned()
            .collect();
        extras.sort();
        for app_id in &extras {
            let info = desktop::resolve(app_id);
            let icon = match info.icon.as_deref() {
                Some(p) => desktop::load_icon(p),
                None => desktop::generic_app_icon()
                    .map(|p| desktop::load_icon(&p))
                    .unwrap_or_default(),
            };
            let focused = focused_app_id
                .as_deref()
                .is_some_and(|fid| matches_pinned(fid, app_id));
            items.push(DockItem {
                icon,
                app_id: SharedString::from(app_id.as_str()),
                name: SharedString::from(info.name.as_str()),
                running: true,
                focused,
                pinned: false,
            });
        }

        let model = std::rc::Rc::new(VecModel::from(items));
        ui.set_dock_items(slint::ModelRc::from(model));
        debug!(
            "dock running state updated, focused={:?}, running={:?}",
            focused_app_id, running_ids
        );
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
    /// True when `(x, y)` lies inside the visible dock pill rectangle.
    ///
    /// The dock is a centred pill at the bottom of the screen — it does
    /// NOT span the full output width. Left- and right-click handlers
    /// gate "click is on the dock" on this rect so clicks on the empty
    /// wallpaper strips beside the pill (and on the `dock-outer-gap`
    /// band below it) fall through to the desktop handler — without
    /// this, the desktop right-click context menu refused to open
    /// anywhere in the bottom 96 px of the screen.
    ///
    /// Width / padding constants mirror `Dock.slint`:
    ///   * each slot is 64 px wide
    ///   * `dock-padding` is 8 px on each side of the items row
    ///   * `dock-outer-gap` is 8 px above and below the pill
    fn point_in_dock_pill(&self, x: f64, y: f64) -> bool {
        const DOCK_SLOT_W: f64 = 64.0;
        const DOCK_PADDING: f64 = 8.0;
        const DOCK_OUTER_GAP: f64 = 8.0;
        let oh = self.wm.output_h as f64;
        let ow = self.wm.output_w as f64;
        let dock_h = crate::wm::DOCK_HEIGHT as f64;
        let n_slots = self.dock_entries.len() as f64;
        let pill_w = n_slots * DOCK_SLOT_W + 2.0 * DOCK_PADDING;
        let pill_x = (ow - pill_w) / 2.0;
        let pill_y_top = oh - dock_h + DOCK_OUTER_GAP;
        let pill_y_bot = oh - DOCK_OUTER_GAP;
        y >= pill_y_top && y < pill_y_bot && x >= pill_x && x < pill_x + pill_w
    }

    fn window_rects(&self, state: &SpikeState) -> Vec<WindowRect> {
        let mut rects: Vec<WindowRect> = Vec::new();
        // Front-to-back: highest z_order first.
        let mut wins: Vec<&crate::wm::WindowState> = self.wm.windows.values().collect();
        wins.sort_by_key(|w| std::cmp::Reverse(w.z_order));
        for win in wins {
            // Skip windows the user can't actually click on.
            if win.closing {
                continue;
            }
            if win.minimized && win.anim.is_settled() {
                continue;
            }
            // Match this WM window to a state.toplevels index — needed for
            // find_toplevel_idx callers that index by toplevel position.
            let Some(idx) = state
                .toplevels
                .iter()
                .position(|t| t.surface == win.surface)
            else {
                continue;
            };
            let titlebar = if win.csd { 0.0 } else { TITLEBAR_HEIGHT };
            rects.push(WindowRect {
                id: idx as i32,
                x: win.anim.current_x() as f64,
                y: win.anim.current_y() as f64,
                w: win.geom_w.max(1) as f64,
                h: win.geom_h.max(1) as f64 + titlebar,
                csd: win.csd,
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
            if win.closing {
                continue;
            }
            if win.minimized && win.anim.is_settled() {
                continue;
            }
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
                if let Some(idx) = state
                    .toplevels
                    .iter()
                    .position(|t| t.surface == win.surface)
                {
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
                ActiveDrag::Resize {
                    toplevel_idx,
                    edge,
                    start_geom,
                    ..
                } => {
                    if let Some((nx, mut ny, nw, mut nh)) = resize::compute_resize(&drag, x, y) {
                        // ── Top-edge constraint ───────────────────────────
                        // Block the top edge from sliding under the panel.
                        // Adjust height so the bottom edge stays where the
                        // resize math wanted it.
                        let panel = crate::wm::PANEL_HEIGHT;
                        if matches!(
                            *edge,
                            resize::ResizeEdge::North
                                | resize::ResizeEdge::NorthWest
                                | resize::ResizeEdge::NorthEast
                        ) && ny < panel
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
                        let edge_is_south = matches!(
                            *edge,
                            resize::ResizeEdge::South
                                | resize::ResizeEdge::SouthEast
                                | resize::ResizeEdge::SouthWest,
                        );
                        if edge_is_south {
                            let raw_bottom = ny + nh;
                            // Snap engages when bottom reaches dock-top line.
                            let mut engaged_at = match &self.active_drag {
                                Some(ActiveDrag::Resize {
                                    dock_snap_engaged_at,
                                    ..
                                }) => *dock_snap_engaged_at,
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
                            if let Some(ActiveDrag::Resize {
                                dock_snap_engaged_at,
                                ..
                            }) = &mut self.active_drag
                            {
                                *dock_snap_engaged_at = engaged_at;
                            }
                        }

                        let mut reactive_popup_parent = None;
                        if let Some(tl) = state.toplevels.get_mut(*toplevel_idx) {
                            tl.x = nx;
                            tl.y = ny;
                            let surface = tl.surface.clone();
                            reactive_popup_parent = Some(surface.clone());
                            // Honour the client's declared min/max size
                            // (xdg_toplevel.set_min_size / set_max_size).
                            // Without this, the user can force GTK clients
                            // below the size their layout supports, ending up
                            // with broken text-overflow + clipped widgets.
                            let (client_min, client_max) =
                                smithay::wayland::compositor::with_states(&surface, |states| {
                                    let mut cs = states
                                        .cached_state
                                        .get::<smithay::wayland::shell::xdg::SurfaceCachedState>(
                                    );
                                    let cur = cs.current();
                                    (cur.min_size, cur.max_size)
                                });
                            let mut cw = nw;
                            let mut ch = nh;
                            if client_min.w > 0 {
                                cw = cw.max(client_min.w);
                            }
                            if client_min.h > 0 {
                                ch = ch.max(client_min.h);
                            }
                            if client_max.w > 0 {
                                cw = cw.min(client_max.w);
                            }
                            if client_max.h > 0 {
                                ch = ch.min(client_max.h);
                            }
                            let cw = cw.max(resize::MIN_WINDOW_SIZE);
                            let ch = ch.max(resize::MIN_WINDOW_SIZE);

                            // Throttle configures to ~60Hz so we don't flood
                            // the client with size churn.
                            let now = std::time::Instant::now();
                            let should_send = match &mut self.active_drag {
                                Some(ActiveDrag::Resize {
                                    last_configure_at, ..
                                }) => {
                                    let send = last_configure_at.is_none_or(|t| {
                                        now.duration_since(t)
                                            >= std::time::Duration::from_millis(16)
                                    });
                                    if send {
                                        *last_configure_at = Some(now);
                                    }
                                    send
                                }
                                _ => true,
                            };
                            if should_send {
                                let maybe_ts = state
                                    .xdg_shell_state
                                    .toplevel_surfaces()
                                    .iter()
                                    .find(|ts| ts.wl_surface() == &surface)
                                    .cloned();
                                if let Some(toplevel) = maybe_ts {
                                    use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;
                                    toplevel.with_pending_state(
                                        |s: &mut smithay::wayland::shell::xdg::ToplevelState| {
                                            s.size = Some((cw, ch).into());
                                            // Tell the client it's actively being
                                            // resized; many clients (kitty, gtk,
                                            // qt) gate their redraw path on this
                                            // and won't repaint at the new size
                                            // without it.
                                            s.states.set(xdg_toplevel::State::Resizing);
                                        },
                                    );
                                    toplevel.send_configure();
                                    debug!("resize configure: {}×{} at ({},{})", cw, ch, nx, ny);
                                }
                            }
                            // Sync WM so Slint follows the drag.
                            if let Some(wm_id) = self.wm.id_for_surface(&surface) {
                                self.wm.set_geometry_by_id(wm_id, nx, ny, cw, ch);
                            }
                        }
                        if let Some(surface) = reactive_popup_parent {
                            state.update_reactive_popups_for_toplevel(&surface);
                        }
                    }
                    self.update_windows(state);
                    if let Some(gpu_window) = self.gpu_window.as_ref() {
                        gpu_window.mark_dirty();
                    }
                    return; // Don't update cursor during resize drag.
                }
                ActiveDrag::Move {
                    toplevel_idx,
                    offset_x,
                    offset_y,
                    pending_unmaximize,
                } => {
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
                        let surface = state.toplevels.get(toplevel_idx).map(|t| t.surface.clone());
                        if let Some(surface) = surface {
                            let wm_id_opt = self.wm.id_for_surface(&surface);
                            if let Some(wm_id) = wm_id_opt {
                                let restore = self
                                    .wm
                                    .windows
                                    .values()
                                    .find(|w| w.id == wm_id)
                                    .and_then(|w| w.pre_maximize);
                                if let Some((_rx, _ry, rw, rh)) = restore {
                                    let rel_x =
                                        ((x - 0.0) / self.wm.output_w as f64).clamp(0.0, 1.0);
                                    let new_x = (x - rel_x * rw as f64) as i32;
                                    let new_y = (y - (crate::wm::TITLEBAR_HEIGHT / 2.0)) as i32;
                                    if let Some(win_mut) =
                                        self.wm.windows.values_mut().find(|w| w.id == wm_id)
                                    {
                                        win_mut.start_unmaximize();
                                    }
                                    self.wm.set_geometry_by_id(wm_id, new_x, new_y, rw, rh);
                                    if let Some(tl_mut) = state.toplevels.get_mut(toplevel_idx) {
                                        tl_mut.x = new_x;
                                        tl_mut.y = new_y;
                                    }
                                    let maybe_ts = state
                                        .xdg_shell_state
                                        .toplevel_surfaces()
                                        .iter()
                                        .find(|ts| ts.wl_surface() == &surface)
                                        .cloned();
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
                                    // X11 clients won't see the unmaximize via
                                    // xdg_toplevel.configure — push the state
                                    // change to them directly.
                                    self.sync_x11_window_state(
                                        &surface,
                                        state,
                                        Some(false),
                                        None,
                                        None,
                                    );
                                    state.update_reactive_popups_for_toplevel(&surface);
                                    if let Some(ActiveDrag::Move {
                                        offset_x,
                                        offset_y,
                                        pending_unmaximize,
                                        ..
                                    }) = &mut self.active_drag
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
                    let mut reactive_popup_parent = None;
                    if let Some(tl) = state.toplevels.get_mut(*toplevel_idx) {
                        let nx = (x - ox) as i32;
                        // Clamp y so the window's titlebar can't slide under
                        // the panel (the top bar is sacred), but the bottom
                        // is unconstrained — the user explicitly wants to be
                        // able to drag windows down behind the dock; the
                        // dock's post-chrome re-blit makes them visually
                        // disappear behind it without us cutting them off.
                        let panel = crate::wm::PANEL_HEIGHT;
                        let oh = self.wm.output_h;
                        let raw_ny = (y - oy) as i32;
                        // Allow the window to push down so just its titlebar
                        // remains visible on screen.
                        let max_ny = (oh - 24).max(panel);
                        let ny = raw_ny.clamp(panel, max_ny);
                        tl.x = nx;
                        tl.y = ny;
                        let surface = tl.surface.clone();
                        reactive_popup_parent = Some(surface.clone());
                        if let Some(wm_id) = self.wm.id_for_surface(&surface) {
                            self.wm.set_position_by_id(wm_id, nx, ny);
                            maybe_wm_id = Some(wm_id);
                        }
                        debug!("move: window #{} to ({},{})", toplevel_idx, nx, ny);
                    }
                    if let Some(surface) = reactive_popup_parent {
                        state.update_reactive_popups_for_toplevel(&surface);
                    }

                    // Snap detection — show preview if cursor is in an edge band.
                    if let Some(ui) = self.ui.as_ref() {
                        let ow = self.wm.output_w;
                        let oh = self.wm.output_h;
                        match crate::snap::detect(x, y, ow, oh) {
                            Some((zone, rect)) => {
                                ui.set_snap_preview_visible(true);
                                ui.set_snap_preview_x(rect.x);
                                ui.set_snap_preview_y(rect.y);
                                ui.set_snap_preview_w(rect.w);
                                ui.set_snap_preview_h(rect.h);
                                if let Some(id) = maybe_wm_id {
                                    self.pending_snap = Some((id, zone, rect));
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
        let zone = if matches!(
            raw_zone,
            HitZone::EdgeNorth
                | HitZone::EdgeSouth
                | HitZone::EdgeEast
                | HitZone::EdgeWest
                | HitZone::CornerNW { .. }
                | HitZone::CornerNE { .. }
                | HitZone::CornerSW { .. }
                | HitZone::CornerSE { .. }
        ) {
            let idx_opt = self.find_toplevel_idx(state, x, y);
            let maximized = idx_opt
                .and_then(|idx| state.toplevels.get(idx))
                .and_then(|tl| self.wm.id_for_surface(&tl.surface))
                .and_then(|id| self.wm.windows.values().find(|w| w.id == id))
                .is_some_and(|w| w.maximized);
            if maximized {
                HitZone::None
            } else {
                raw_zone
            }
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
                        let maximized = self
                            .wm
                            .windows
                            .values()
                            .find(|w| {
                                state.toplevels.get(idx).is_some_and(|t| {
                                    self.wm.id_for_surface(&t.surface) == Some(w.id)
                                })
                            })
                            .is_some_and(|w| w.maximized);
                        let (ox, oy) = self.maybe_unsnap_for_move(state, idx, x, y);
                        self.active_drag = Some(ActiveDrag::Move {
                            toplevel_idx: idx,
                            offset_x: ox,
                            offset_y: oy,
                            pending_unmaximize: if maximized { Some((x, y)) } else { None },
                        });
                        debug!(
                            "Win+drag move started on window #{} (maximized={})",
                            idx, maximized
                        );
                        let _ = hit_result;
                        return;
                    }
                    // Don't start a drag when the press lands on a control
                    // button — Slint's IconButton TouchArea will fire
                    // `clicked` and route through close-clicked /
                    // minimize-clicked / maximize-clicked. We just need to
                    // avoid hijacking the press into a move drag.
                    if matches!(
                        zone,
                        HitZone::CloseButton | HitZone::MinimizeButton | HitZone::MaximizeButton
                    ) {
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
                        let (w, h) = self
                            .wm
                            .id_for_surface(&t.surface)
                            .and_then(|wm_id| {
                                self.wm
                                    .windows
                                    .values()
                                    .find(|w| w.id == wm_id)
                                    .map(|w| (w.w, w.h))
                            })
                            .unwrap_or_else(|| {
                                let pix = t.pixels.lock().unwrap();
                                (pix.width as i32, pix.height as i32)
                            });
                        self.active_drag = Some(ActiveDrag::Resize {
                            toplevel_idx: idx,
                            edge,
                            start_ptr_x: x,
                            start_ptr_y: y,
                            start_geom: WindowGeomSnapshot {
                                x: t.x,
                                y: t.y,
                                w,
                                h,
                            },
                            last_configure_at: None,
                            dock_snap_engaged_at: None,
                        });
                        debug!(
                            "resize drag started: {:?} on window #{} from {}x{}",
                            edge, idx, w, h
                        );
                    } else if zone == HitZone::TitleBar {
                        let maximized = state
                            .toplevels
                            .get(idx)
                            .and_then(|t| self.wm.id_for_surface(&t.surface))
                            .and_then(|id| self.wm.windows.values().find(|w| w.id == id))
                            .is_some_and(|w| w.maximized);
                        let (ox, oy) = self.maybe_unsnap_for_move(state, idx, x, y);
                        self.active_drag = Some(ActiveDrag::Move {
                            toplevel_idx: idx,
                            offset_x: ox,
                            offset_y: oy,
                            pending_unmaximize: if maximized { Some((x, y)) } else { None },
                        });
                        debug!(
                            "move drag started on window #{} (maximized={})",
                            idx, maximized
                        );
                    }
                }
                let _ = hit_result;
            }
        }
    }

    /// Build the debug-overlay text dump (Super+I).
    fn build_debug_dump(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "theme: {:?}  mode_t={:.2}\n",
            self.theme.current_mode, self.theme.mode_t
        ));
        out.push_str(&format!(
            "output: {}×{}\n",
            self.wm.output_w, self.wm.output_h
        ));
        out.push_str(&format!(
            "frames: {}  fps: {:.1}\n",
            self.frame_count, self.fps
        ));
        out.push_str(&format!(
            "cursor: {:?} at ({:.0},{:.0})\n",
            self.current_cursor, self.pointer_pos.0, self.pointer_pos.1
        ));
        out.push_str(&format!(
            "drag: {}\n",
            match &self.active_drag {
                None => "none".to_string(),
                Some(d) => format!("{:?}", d),
            }
        ));
        out.push_str(&format!("\nWindows ({}):\n", self.wm.windows.len()));
        let mut wins: Vec<&crate::wm::WindowState> = self.wm.windows.values().collect();
        wins.sort_by_key(|w| w.id);
        for w in &wins {
            out.push_str(&format!(
                "  #{}  z={}  {}×{}+{}+{}  focus={}  min={}  max={}  closing={}\n",
                w.id, w.z_order, w.w, w.h, w.x, w.y, w.focused, w.minimized, w.maximized, w.closing,
            ));
            out.push_str(&format!(
                "        opacity={:.2}  scale={:.2}  awaiting_first={}\n",
                w.anim.opacity.value_f32(),
                w.anim.scale.value_f32(),
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
            ThemeMode::Dark => "dark",
        };
        let Some(p) = wallpaper::find_wallpaper_for_mode(tag) else {
            return;
        };
        if let Some(img) = wallpaper::load_from_path(&p) {
            if let Some(ui) = self.ui.as_ref() {
                ui.set_wallpaper(img);
            }
        }
        if let Some(b) = wallpaper::load_blurred_from_path(&p, 24.0) {
            if let Some(ui) = self.ui.as_ref() {
                ui.set_wallpaper_blurred(b);
            }
        }
        self.backdrop.load_wallpaper(&p);
        if let Some(gpu_window) = self.gpu_window.as_ref() {
            gpu_window.mark_dirty();
        }
        info!(
            "wallpaper swapped for mode {:?} → {:?}",
            self.theme.current_mode, p
        );
    }

    /// Publish layer surfaces (panel / dock / wallpaper / overlays) to Slint
    /// only when the model would actually change — a layer was added or
    /// removed, its rect moved, or a client committed new pixels. Without
    /// this gate, the layers model was rebuilt and re-uploaded every main-
    /// loop iteration, which marked Slint dirty every frame and forced a
    /// full GPU re-render at vsync rate. Cached `Image`s for unchanged
    /// surfaces are reused so Slint doesn't re-upload their textures.
    pub fn update_layers(&mut self, state: &mut SpikeState) {
        use smithay::reexports::wayland_server::Resource;
        use smithay::wayland::shell::wlr_layer::Layer;

        let Some(ui) = self.ui.as_ref() else { return };

        // Pass 1 — for each layer surface, decide whether its cached Image
        // is still good. We pull `dirty` out of `pixels` (and reset it) so
        // the next idle iteration sees no pending update. Build the model's
        // fingerprint (id + rect + ordinal) in protocol layer order so we
        // can skip the Slint set_layers call when nothing changed.
        let mut layer_entries: Vec<(usize, u32, i32, i32, i32, i32, i32)> =
            Vec::with_capacity(state.layer_surfaces.len());
        let mut any_dirty = false;
        let mut visible_ids: std::collections::HashSet<u32> =
            std::collections::HashSet::with_capacity(state.layer_surfaces.len());

        for (index, li) in state.layer_surfaces.iter().enumerate() {
            let id = li.surface.wl_surface().id().protocol_id();
            let (pw, ph, dirty) = {
                let mut pix = li.pixels.lock().unwrap();
                let was_dirty = pix.dirty;
                if was_dirty {
                    pix.dirty = false;
                }
                (pix.width, pix.height, was_dirty)
            };
            if pw == 0 || ph == 0 || li.w <= 0 || li.h <= 0 {
                // Drop any stale cached Image so a remap with a fresh buffer
                // re-uploads from scratch.
                self.layer_image_cache.remove(&id);
                continue;
            }
            visible_ids.insert(id);
            if dirty || !self.layer_image_cache.contains_key(&id) {
                let pix = li.pixels.lock().unwrap();
                let buf = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(
                    &pix.pixels,
                    pix.width,
                    pix.height,
                );
                drop(pix);
                let img = slint::Image::from_rgba8_premultiplied(buf);
                self.layer_image_cache.insert(id, img);
                any_dirty = true;
            }
            let ordinal: i32 = match li.layer {
                Layer::Background => 0,
                Layer::Bottom => 1,
                Layer::Top => 2,
                Layer::Overlay => 3,
            };
            layer_entries.push((index, id, li.x, li.y, li.w, li.h, ordinal));
        }

        layer_entries.sort_by_key(|&(index, _, _, _, _, _, ordinal)| (ordinal, index));
        let fingerprint: Vec<(u32, i32, i32, i32, i32, i32)> = layer_entries
            .iter()
            .map(|&(_, id, x, y, w, h, ordinal)| (id, x, y, w, h, ordinal))
            .collect();

        // Evict cached Images for surfaces that disappeared (unmap, destroy)
        // so the cache doesn't grow unbounded across a long session.
        if self.layer_image_cache.len() > visible_ids.len() {
            self.layer_image_cache
                .retain(|id, _| visible_ids.contains(id));
        }

        // No add/remove/move and no surface re-painted → nothing to publish.
        if !any_dirty && fingerprint == self.last_layers_fingerprint {
            return;
        }

        // Build + publish the model from the cache.
        let items: Vec<crate::LayerItem> = fingerprint
            .iter()
            .filter_map(|&(id, x, y, w, h, ordinal)| {
                let img = self.layer_image_cache.get(&id)?.clone();
                Some(crate::LayerItem {
                    id: id as i32,
                    surface: img,
                    x,
                    y,
                    w,
                    h,
                    layer_ordinal: ordinal,
                })
            })
            .collect();
        let model = std::rc::Rc::new(VecModel::from(items));
        ui.set_layers(slint::ModelRc::from(model));
        self.last_layers_fingerprint = fingerprint;
        if let Some(gpu_window) = self.gpu_window.as_ref() {
            gpu_window.mark_dirty();
        }
    }

    /// Refresh the panel/dock backdrop on a throttled cadence.
    #[allow(clippy::type_complexity)]
    pub fn refresh_backdrop(&mut self, state: &SpikeState) {
        // Bail out before the per-window pixel clone if the throttle window
        // hasn't elapsed — the snapshot vector below is megabytes per
        // visible window, and pegging the render thread's memcpy bandwidth
        // for work we'd throw away is what was capping framerate.
        if !self.backdrop.should_refresh() {
            return;
        }
        let mut snapshots: Vec<(i32, i32, i32, i32, Vec<u8>, u32, u32)> = Vec::new();
        for win in self.wm.windows_sorted() {
            if win.minimized && win.anim.is_settled() {
                continue;
            }
            let Some(tl) = state.toplevels.iter().find(|t| t.surface == win.surface) else {
                continue;
            };
            let pix = tl.pixels.lock().unwrap();
            if pix.width == 0 || pix.height == 0 {
                continue;
            }
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
        let snaps: Vec<WindowSnapshot<'_>> = snapshots
            .iter()
            .map(|t| WindowSnapshot {
                x: t.0,
                y: t.1,
                w: t.2,
                h: t.3,
                pixels: &t.4,
                buf_w: t.5,
                buf_h: t.6,
            })
            .collect();
        if let Some(img) = self.backdrop.try_synth(&snaps) {
            if let Some(ui) = self.ui.as_ref() {
                ui.set_wallpaper_blurred(img);
            }
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
                let size = self
                    .wm
                    .id_for_surface(&surface)
                    .and_then(|id| self.wm.windows.values().find(|w| w.id == id))
                    .map(|w| (w.w, w.h));
                if let Some((cw, ch)) = size {
                    let maybe_ts = state
                        .xdg_shell_state
                        .toplevel_surfaces()
                        .iter()
                        .find(|ts| ts.wl_surface() == &surface)
                        .cloned();
                    if let Some(toplevel) = maybe_ts {
                        use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;
                        toplevel.with_pending_state(
                            |s: &mut smithay::wayland::shell::xdg::ToplevelState| {
                                s.size = Some((cw, ch).into());
                                s.states.unset(xdg_toplevel::State::Resizing);
                            },
                        );
                        let serial = toplevel.send_configure();
                        state.begin_xdg_resize_transaction(surface.clone(), serial);
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
    fn maybe_unsnap_for_move(
        &mut self,
        state: &mut SpikeState,
        idx: usize,
        x: f64,
        y: f64,
    ) -> (f64, f64) {
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

        let restore = self
            .wm
            .windows
            .values()
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
            state.update_reactive_popups_for_toplevel(&surface);
            let maybe_ts = state
                .xdg_shell_state
                .toplevel_surfaces()
                .iter()
                .find(|ts| ts.wl_surface() == &surface)
                .cloned();
            if let Some(toplevel) = maybe_ts {
                toplevel.with_pending_state(
                    |s: &mut smithay::wayland::shell::xdg::ToplevelState| {
                        s.size = Some((ow, oh).into());
                        // Clear the tiled/maximized bits we set on snap so
                        // GTK4/libadwaita brings its rounded corners back.
                        use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel as xt;
                        s.states.unset(xt::State::TiledLeft);
                        s.states.unset(xt::State::TiledRight);
                        s.states.unset(xt::State::TiledTop);
                        s.states.unset(xt::State::TiledBottom);
                        s.states.unset(xt::State::Maximized);
                    },
                );
                toplevel.send_configure();
                debug!(
                    "snap-out: restored to {}×{} at ({},{})",
                    ow, oh, new_x, new_y
                );
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
                if let Some(gw) = self.gpu_window.as_ref() {
                    gw.mark_dirty();
                }
            }
            IpcCommand::PointerButton {
                button_evdev,
                pressed,
            } => {
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
                self.pending_keys
                    .lock()
                    .unwrap()
                    .push_back(PendingKeyEvent { scancode, pressed });
            }
            IpcCommand::TypeText { text } => {
                let mut keys = self.pending_keys.lock().unwrap();
                for ch in text.chars() {
                    if let Some((sc, shift)) = ascii_to_scancode(ch) {
                        if shift {
                            keys.push_back(PendingKeyEvent {
                                scancode: 42,
                                pressed: true,
                            });
                        }
                        keys.push_back(PendingKeyEvent {
                            scancode: sc,
                            pressed: true,
                        });
                        keys.push_back(PendingKeyEvent {
                            scancode: sc,
                            pressed: false,
                        });
                        if shift {
                            keys.push_back(PendingKeyEvent {
                                scancode: 42,
                                pressed: false,
                            });
                        }
                    }
                }
            }
            IpcCommand::Screenshot {
                save_path,
                response,
            } => {
                if let Err(e) = self.save_screenshot(&save_path) {
                    warn!("ipc: screenshot failed: {}", e);
                    if let Some(response) = response {
                        let _ = response.send(Err(e.to_string()));
                    }
                } else {
                    info!("ipc: screenshot saved to {}", save_path);
                    if let Some(response) = response {
                        let _ = response.send(Ok(save_path));
                    }
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
                if let Some(surf) = self
                    .wm
                    .windows
                    .values()
                    .find(|w| w.id == wm_id)
                    .map(|w| w.surface.clone())
                {
                    if let Some(tl) = state.toplevels.iter_mut().find(|t| t.surface == surf) {
                        tl.x = x;
                        tl.y = y;
                    }
                    state.update_reactive_popups_for_toplevel(&surf);
                }
                if let Some(gw) = self.gpu_window.as_ref() {
                    gw.mark_dirty();
                }
            }
            IpcCommand::ResizeWindow { wm_id, w, h } => {
                let pos = self
                    .wm
                    .windows
                    .values()
                    .find(|win| win.id == wm_id)
                    .map(|win| (win.x, win.y));
                if let Some((wx, wy)) = pos {
                    self.wm.set_geometry_by_id(wm_id, wx, wy, w, h);
                }
                if let Some(surf) = self
                    .wm
                    .windows
                    .values()
                    .find(|win| win.id == wm_id)
                    .map(|win| win.surface.clone())
                {
                    let maybe_ts = state
                        .xdg_shell_state
                        .toplevel_surfaces()
                        .iter()
                        .find(|ts| ts.wl_surface() == &surf)
                        .cloned();
                    if let Some(toplevel) = maybe_ts {
                        toplevel.with_pending_state(
                            |s: &mut smithay::wayland::shell::xdg::ToplevelState| {
                                s.size = Some((w, h).into());
                            },
                        );
                        toplevel.send_configure();
                    }
                    state.update_reactive_popups_for_toplevel(&surf);
                }
                if let Some(gw) = self.gpu_window.as_ref() {
                    gw.mark_dirty();
                }
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
                    _ => ThemeMode::Dark,
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
        let render_tex = self
            .final_texture
            .as_ref()
            .or(self.render_texture.as_ref())
            .ok_or_else(|| anyhow::anyhow!("no render texture yet"))?;
        let gpu = self
            .gpu_window
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("no gpu adapter"))?;
        let device = &gpu.wgpu_device;
        let queue = &gpu.wgpu_queue;
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
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
        queue.submit(std::iter::once(encoder.finish()));

        // Block until the copy is done + the buffer is mappable.
        let slice = buf.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        device.poll(wgpu::PollType::wait_indefinitely()).ok();
        rx.recv()
            .map_err(|e| anyhow::anyhow!("map_async channel: {}", e))?
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
        if matches!(self.swapchain_format, Some(wgpu::TextureFormat::Bgra8Unorm)) {
            for px in rgba.chunks_exact_mut(4) {
                px.swap(0, 2);
            }
        }

        image::save_buffer(save_path, &rgba, w, h, image::ColorType::Rgba8)
            .map_err(|e| anyhow::anyhow!("save_buffer: {}", e))?;
        Ok(())
    }

    /// Apply any pending snap rect committed by the last Move drag. Called
    /// from the main loop on left-button release.
    fn apply_pending_snap(&mut self, state: &mut SpikeState) {
        if let Some(ui) = self.ui.as_ref() {
            ui.set_snap_preview_visible(false);
        }
        let Some((wm_id, zone, rect)) = self.pending_snap.take() else {
            return;
        };

        let surface = self
            .wm
            .windows
            .values()
            .find(|w| w.id == wm_id)
            .map(|w| w.surface.clone());
        let Some(surface) = surface else { return };

        if let Some(win) = self.wm.windows.values_mut().find(|w| w.id == wm_id) {
            if zone == crate::snap::SnapZone::Maximize {
                // Snap-to-top IS maximize: a window covering the whole work
                // area is maximized. Stash the pre-maximize geometry for
                // restore and flag the window maximized so the chrome
                // squares off (corners + GPU shadow) and a titlebar-drag
                // unmaximizes it via the `pending_unmaximize` path.
                if win.pre_maximize.is_none() {
                    win.pre_maximize = Some((win.x, win.y, win.w, win.h));
                }
                win.maximized = true;
            } else {
                // Half / quarter tiling — restored by dragging off the snap.
                // Save pre-snap rect (only on the FIRST snap — re-snapping
                // preserves the original unsnapped geometry).
                if win.pre_snap.is_none() {
                    win.pre_snap = Some((win.x, win.y, win.w, win.h));
                }
                // Tiling to a half/quarter is not maximized — clear the flag
                // so the chrome rounds again (e.g. maximized → snap-left).
                win.maximized = false;
            }
            win.x = rect.x;
            win.y = rect.y;
            win.w = rect.w;
            win.h = rect.h;
            win.anim.set_geometry_target(rect.x, rect.y, rect.w, rect.h);
        }

        let maybe_ts = state
            .xdg_shell_state
            .toplevel_surfaces()
            .iter()
            .find(|ts| ts.wl_surface() == &surface)
            .cloned();
        if let Some(toplevel) = maybe_ts {
            // xdg-toplevel v2+ `tiled_*` states let GTK4/libadwaita drop their
            // rounded corners on the snapped edges. Maximize snap also sets
            // the Maximized state so client header bars compact.
            use crate::snap::SnapZone;
            use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel as xt;
            toplevel.with_pending_state(|s: &mut smithay::wayland::shell::xdg::ToplevelState| {
                s.size = Some((rect.w, rect.h).into());
                // Clear all tiled/maximized bits before re-applying.
                s.states.unset(xt::State::TiledLeft);
                s.states.unset(xt::State::TiledRight);
                s.states.unset(xt::State::TiledTop);
                s.states.unset(xt::State::TiledBottom);
                s.states.unset(xt::State::Maximized);
                match zone {
                    SnapZone::Maximize => {
                        s.states.set(xt::State::Maximized);
                    }
                    SnapZone::LeftHalf => {
                        s.states.set(xt::State::TiledLeft);
                        s.states.set(xt::State::TiledTop);
                        s.states.set(xt::State::TiledBottom);
                    }
                    SnapZone::RightHalf => {
                        s.states.set(xt::State::TiledRight);
                        s.states.set(xt::State::TiledTop);
                        s.states.set(xt::State::TiledBottom);
                    }
                    SnapZone::TopLeftQuarter => {
                        s.states.set(xt::State::TiledLeft);
                        s.states.set(xt::State::TiledTop);
                    }
                    SnapZone::TopRightQuarter => {
                        s.states.set(xt::State::TiledRight);
                        s.states.set(xt::State::TiledTop);
                    }
                    SnapZone::BottomLeftQuarter => {
                        s.states.set(xt::State::TiledLeft);
                        s.states.set(xt::State::TiledBottom);
                    }
                    SnapZone::BottomRightQuarter => {
                        s.states.set(xt::State::TiledRight);
                        s.states.set(xt::State::TiledBottom);
                    }
                }
            });
            toplevel.send_configure();
            debug!(
                "snap: configure {}×{} at ({},{}) zone={:?}",
                rect.w, rect.h, rect.x, rect.y, zone
            );
        }
        for tl in state.toplevels.iter_mut() {
            if tl.surface == surface {
                tl.x = rect.x;
                tl.y = rect.y;
                break;
            }
        }
        state.update_reactive_popups_for_toplevel(&surface);
        if let Some(gpu_window) = self.gpu_window.as_ref() {
            gpu_window.mark_dirty();
        }
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
                ThemeMode::Dark => TokenMode::Dark,
                ThemeMode::Light => TokenMode::Light,
            };
            ui.set_theme_mode(token_mode);
        }
    }

    /// Push the popout calendar grid + month label for the currently
    /// browsed month (today + `calendar_month_offset` months). Called
    /// on the daily clock tick AND on every prev/next chevron click.
    pub fn refresh_calendar(&self) {
        let Some(ui) = self.ui.as_ref() else { return };
        let today = chrono::Local::now().date_naive();
        let displayed = calendar::add_months(today, self.calendar_month_offset);
        ui.set_popout_calendar_month_text(SharedString::from(
            displayed.format("%B %Y").to_string(),
        ));
        let cal = calendar::build_calendar_grid_for(displayed, today);
        let model = std::rc::Rc::new(VecModel::from(cal));
        ui.set_popout_calendar_days(slint::ModelRc::from(model));
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
            None => desktop::generic_app_icon()
                .map(|p| desktop::load_icon(&p))
                .unwrap_or_default(),
        };

        entries.push(ResolvedDockEntry {
            item: DockItem {
                icon,
                app_id: SharedString::from(app_id.as_str()),
                name: SharedString::from(info.name.as_str()),
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
            let icon = info
                .icon
                .as_deref()
                .map(desktop::load_icon)
                .unwrap_or_default();
            entries.push(ResolvedDockEntry {
                item: DockItem {
                    icon,
                    app_id: SharedString::from(app_id),
                    name: SharedString::from(info.name.as_str()),
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
    // 1. Wayland display/socket + calloop.
    let wayland = WaylandRuntime::new()?;
    let loop_signal = wayland.loop_signal.clone();
    let display_handle = wayland.display_handle.clone();
    let socket_name = wayland.socket_name.clone();

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

    let (slint_device, slint_queue) =
        pollster::block_on(slint_adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("compositor-slint-shared"),
            required_features: wgpu::Features::empty(),
            required_limits:
                wgpu::Limits::downlevel_webgl2_defaults().using_resolution(slint_adapter.limits()),
            ..Default::default()
        }))
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

    // Focused window's global-menu D-Bus address. Created here so both the
    // panel menu-click callbacks (wired below) and `CompositorApp` (which
    // rewrites it on focus change) share the same cell.
    let appmenu_addr: Rc<RefCell<Option<(String, String)>>> = Rc::new(RefCell::new(None));
    // Async appmenu fetch results — worker threads push, the render loop
    // drains. (Shared with the menu-click callbacks wired below.)
    let appmenu_results: Arc<Mutex<VecDeque<MenuFetchResult>>> =
        Arc::new(Mutex::new(VecDeque::new()));

    ui.set_clock_text(SharedString::from(
        chrono::Local::now().format("%H:%M:%S").to_string(),
    ));

    // Wallpaper. If the resolved wallpaper is named *_light.* we also default
    // the theme to Light so the chrome matches the desktop's overall mood.
    let wallpaper_path = wallpaper::find_wallpaper_path();
    let mut start_in_light_mode = false;
    match wallpaper_path
        .as_ref()
        .and_then(|p| wallpaper::load_from_path(p))
    {
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
    if let Some(blurred) = wallpaper_path
        .as_ref()
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
            MenuItem {
                id: 1,
                label: SharedString::from("New Folder"),
                accelerator: SharedString::from("⇧⌘N"),
                separator: false,
                enabled: true,
            },
            MenuItem {
                id: 2,
                label: SharedString::from("Get Info"),
                accelerator: SharedString::from("⌘I"),
                separator: false,
                enabled: false,
            },
            MenuItem {
                id: -1,
                label: SharedString::default(),
                accelerator: SharedString::default(),
                separator: true,
                enabled: false,
            },
            MenuItem {
                id: 3,
                label: SharedString::from("Change Wallpaper…"),
                accelerator: SharedString::default(),
                separator: false,
                enabled: true,
            },
            MenuItem {
                id: 4,
                label: SharedString::from("Toggle Theme"),
                accelerator: SharedString::from("⌃T"),
                separator: false,
                enabled: true,
            },
            MenuItem {
                id: -1,
                label: SharedString::default(),
                accelerator: SharedString::default(),
                separator: true,
                enabled: false,
            },
            MenuItem {
                id: 5,
                label: SharedString::from("Show Debug Overlay"),
                accelerator: SharedString::from("⌃I"),
                separator: false,
                enabled: true,
            },
            MenuItem {
                id: 6,
                label: SharedString::from("About this DE"),
                accelerator: SharedString::default(),
                separator: false,
                enabled: true,
            },
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
            MenuItem {
                id: 1,
                label: SharedString::from("Open New Window"),
                accelerator: SharedString::default(),
                separator: false,
                enabled: true,
            },
            MenuItem {
                id: 2,
                label: SharedString::from("Show All Windows"),
                accelerator: SharedString::default(),
                separator: false,
                enabled: true,
            },
            MenuItem {
                id: -1,
                label: SharedString::default(),
                accelerator: SharedString::default(),
                separator: true,
                enabled: false,
            },
            MenuItem {
                id: 3,
                label: SharedString::from("Keep in Dock"),
                accelerator: SharedString::default(),
                separator: false,
                enabled: false,
            },
            MenuItem {
                id: 4,
                label: SharedString::from("Show in Files"),
                accelerator: SharedString::default(),
                separator: false,
                enabled: false,
            },
            MenuItem {
                id: -1,
                label: SharedString::default(),
                accelerator: SharedString::default(),
                separator: true,
                enabled: false,
            },
            MenuItem {
                id: 5,
                label: SharedString::from("Quit"),
                accelerator: SharedString::from("⌘Q"),
                separator: false,
                enabled: true,
            },
        ];
        let model = std::rc::Rc::new(VecModel::from(items));
        let model_rc: slint::ModelRc<crate::MenuItem> = slint::ModelRc::from(model);
        // Push the rendered height to slint so the icon-right-click
        // flip-up math anchors the menu bottom at the cursor (slint
        // can't easily count separators vs normal rows).
        ui.set_dock_menu_h(compute_menu_height(&model_rc) as i32);
        ui.set_dock_menu_items(model_rc);
    }
    // dock-menu-clicked is wired AFTER `exec_map` is built (just below).

    // Window context menu (right-click on titlebar / Menu key). Static
    // items; the WM id of the target window is held separately on the
    // popup (`window-menu-target-id`) and routed back through the
    // `window-menu-clicked(target_id, action_id)` callback.
    {
        use crate::MenuItem;
        let items = vec![
            MenuItem {
                id: 1,
                label: SharedString::from("Minimize"),
                accelerator: SharedString::from("⌘M"),
                separator: false,
                enabled: true,
            },
            MenuItem {
                id: 2,
                label: SharedString::from("Maximize"),
                accelerator: SharedString::default(),
                separator: false,
                enabled: true,
            },
            MenuItem {
                id: -1,
                label: SharedString::default(),
                accelerator: SharedString::default(),
                separator: true,
                enabled: false,
            },
            MenuItem {
                id: 3,
                label: SharedString::from("Close"),
                accelerator: SharedString::from("⌘W"),
                separator: false,
                enabled: true,
            },
        ];
        let model = std::rc::Rc::new(VecModel::from(items));
        ui.set_window_menu_items(slint::ModelRc::from(model));
    }
    {
        let weak = ui.as_weak();
        ui.on_window_menu_clicked(move |target_id, action_id| {
            let Some(ui) = weak.upgrade() else { return };
            // Wire each action to the existing per-window callbacks the
            // chrome buttons use, so we don't have to duplicate the
            // pending-queue plumbing.
            match action_id {
                1 => ui.invoke_minimize_window(target_id),
                2 => ui.invoke_maximize_window(target_id),
                3 => ui.invoke_close_window(target_id),
                _ => {}
            }
        });
    }

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
        let dock_entries_q = dock_entries
            .iter()
            .map(|e| {
                (
                    e.item.app_id.to_string(),
                    e.item.name.to_string(),
                    e.exec.clone(),
                )
            })
            .collect::<Vec<_>>();
        ui.on_launcher_submit(move |q| {
            let query = q.trim().to_string();
            if query.is_empty() {
                return;
            }
            let lq = query.to_lowercase();
            // Find a pinned-app match by id or name prefix.
            let exec = dock_entries_q
                .iter()
                .find_map(|(id, name, exec)| {
                    if id.to_lowercase().contains(&lq) || name.to_lowercase().contains(&lq) {
                        Some(exec.clone())
                    } else {
                        None
                    }
                })
                .unwrap_or(query);
            tracing::info!("launcher: launching {:?}", exec);
            let _ = std::process::Command::new("sh")
                .arg("-c")
                .arg(&exec)
                .spawn();
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
                            0,
                            0,
                        );
                        break;
                    }
                }
            }
        });
    }
    // Right-click on a tray icon: kick off an async DBusMenu GetLayout
    // fetch on a worker thread. Once the layout returns, hop back to the
    // Slint event loop, populate `tray-menu-items` and flip
    // `tray-menu-open = true` so the ContextMenu pops near the cursor.
    {
        let weak = ui.as_weak();
        let results = appmenu_results.clone();
        ui.on_tray_right_clicked(move |id| {
            let Some(ui) = weak.upgrade() else { return };
            let model = ui.get_tray_items();
            let mut endpoint: Option<(String, String, f32, f32)> = None;
            for i in 0..model.row_count() {
                if let Some(it) = model.row_data(i) {
                    if it.id == id {
                        endpoint = Some((
                            it.service.to_string(),
                            it.object_path.to_string(),
                            ui.get_cursor_x(),
                            ui.get_cursor_y(),
                        ));
                        break;
                    }
                }
            }
            let Some((service, object_path, cx, cy)) = endpoint else {
                return;
            };
            // Position the popup just below + slightly right of the cursor.
            // ContextMenu is 220 px wide and sizes its height to fit; clamp
            // so it doesn't fall off the right edge of the screen.
            let menu_w = 220.0_f64;
            let pad = 8.0_f64;
            let screen_w = ui.window().size().width as f64;
            let max_x = (screen_w - menu_w - pad).max(pad);
            let x = (cx as f64 - 10.0).clamp(pad, max_x) as i32;
            let y = ((cy as f64) + 6.0).max(pad) as i32;
            let results = results.clone();
            crate::dbusmenu::fetch_layout(service, object_path, move |items| {
                results
                    .lock()
                    .unwrap()
                    .push_back(MenuFetchResult::TrayMenu {
                        items,
                        x,
                        y,
                        sni_id: id,
                    });
            });
        });
    }
    // Click on a tray-menu row → send `Event(item_id, "clicked", ...)` to
    // the DBusMenu service. We look up the SNI endpoint by the tray id we
    // stashed in `tray-menu-id` when the menu opened.
    {
        let weak = ui.as_weak();
        ui.on_tray_menu_clicked(move |sni_id, item_id| {
            let Some(ui) = weak.upgrade() else { return };
            let model = ui.get_tray_items();
            for i in 0..model.row_count() {
                if let Some(it) = model.row_data(i) {
                    if it.id == sni_id {
                        crate::dbusmenu::send_clicked(
                            it.service.to_string(),
                            it.object_path.to_string(),
                            item_id,
                        );
                        break;
                    }
                }
            }
        });
    }
    // Global menu: a top-level bar entry was clicked → fetch that submenu's
    // children from the focused window's com.canonical.dbusmenu and drop a
    // ContextMenu popup below the entry.
    {
        let weak = ui.as_weak();
        let addr = appmenu_addr.clone();
        let results = appmenu_results.clone();
        ui.on_global_menu_entry_clicked(move |node_id, entry_x| {
            let Some(ui) = weak.upgrade() else { return };
            let Some((service, path)) = addr.borrow().clone() else {
                return;
            };
            // Index of the clicked bar entry — drives the open-highlight.
            let bar = ui.get_global_menu_bar();
            let mut idx = -1i32;
            for i in 0..bar.row_count() {
                if let Some(e) = bar.row_data(i) {
                    if e.id == node_id {
                        idx = i as i32;
                        break;
                    }
                }
            }
            let x = entry_x as i32;
            let results = results.clone();
            crate::dbusmenu::fetch_appmenu_children(service, path, node_id, move |items| {
                results.lock().unwrap().push_back(MenuFetchResult::Submenu {
                    items,
                    x,
                    open_index: idx,
                });
            });
        });
    }
    // Global menu: a submenu row was activated → fire the dbusmenu
    // `Event(id, "clicked", …)` on the focused window's menu.
    {
        let addr = appmenu_addr.clone();
        ui.on_global_menu_item_activated(move |item_id| {
            if let Some((service, path)) = addr.borrow().clone() {
                crate::dbusmenu::send_appmenu_event(service, path, item_id, "clicked");
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
                        let _ = std::process::Command::new("sh").arg("-c").arg(exec).spawn();
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

    // Calendar prev/next chevrons in the datetime popout — accumulate
    // the month delta in an atomic int the main loop reads + applies
    // to `app.calendar_month_offset`. Day-clicks are no-op stubs for
    // now (placeholder for "open today's events"-style behaviour).
    let pending_calendar_delta: Arc<std::sync::atomic::AtomicI32> =
        Arc::new(std::sync::atomic::AtomicI32::new(0));
    {
        let d = pending_calendar_delta.clone();
        ui.on_popout_calendar_prev(move || {
            d.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
        });
    }
    {
        let d = pending_calendar_delta.clone();
        ui.on_popout_calendar_next(move || {
            d.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        });
    }
    ui.on_popout_calendar_day_clicked(|day| {
        tracing::info!("calendar day clicked: {}", day);
    });

    // ── Cursor: prime initial cursor image so overlay is visible on first frame.
    // We call this after the main loop starts via the CompositorApp init path.
    // The actual first render happens in resumed() → render_frame().
    info!("Slint GPU platform ready — production Compositor UI");

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
            info!(
                "  exec: {} (WAYLAND_DISPLAY={})",
                exec_line,
                socket_name.to_string_lossy()
            );
            let _ = std::process::Command::new("setsid")
                .args(["-f", "sh", "-c", &exec_line])
                .env("WAYLAND_DISPLAY", &socket_name)
                .env("XDG_SESSION_TYPE", "wayland")
                .env_remove("DISPLAY")
                .spawn();
        });
    }

    // 5. Compositor state + virtual output.
    let mut calloop = wayland.event_loop;
    let mut state = wayland.state;

    // Advertise DMA-BUF support to clients.  We advertise common 8-bit formats
    // with the LINEAR modifier; the GLES renderer (surfaceless EGL) will accept
    // any format that EGL/Mesa supports at runtime.  Clients that want
    // non-linear (tiled/compressed) formats will fall back to SHM.
    {
        use smithay::backend::allocator::{Format, Fourcc, Modifier};

        // DrmModifier::Linear == 0
        let linear = Modifier::Linear;
        let formats: Vec<Format> = vec![
            Format {
                code: Fourcc::Argb8888,
                modifier: linear,
            },
            Format {
                code: Fourcc::Xrgb8888,
                modifier: linear,
            },
            Format {
                code: Fourcc::Abgr8888,
                modifier: linear,
            },
            Format {
                code: Fourcc::Xbgr8888,
                modifier: linear,
            },
        ];

        let _dmabuf_global = state
            .dmabuf_state
            .create_global::<SpikeState>(&display_handle, formats);
        info!("DMA-BUF global advertised (Option B: EGL/GLES two-stage import)");
    }

    // wl_output.physical_size is MILLIMETRES, not pixels. Anvil computes
    // this from the real monitor's EDID; on our virtual swapchain we
    // approximate from the logical size assuming ~96 DPI (1 inch ≈ 25.4 mm,
    // 96 px/in → 1 mm ≈ 3.78 px). Without this DPI-aware clients (Firefox,
    // GNOME, libadwaita) infer a ridiculous DPI from `WIDTH mm` and either
    // render at 50× their normal scale or refuse to scale at all.
    let physical_w_mm = (WIDTH as f32 / 3.78).round() as i32;
    let physical_h_mm = (HEIGHT as f32 / 3.78).round() as i32;
    let output = Output::new(
        "slint-gpu-output".to_owned(),
        PhysicalProperties {
            size: (physical_w_mm, physical_h_mm).into(),
            subpixel: Subpixel::Unknown,
            make: "SlintGPU".into(),
            model: "Virtual".into(),
            serial_number: "0001".into(),
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
    // Register the output on the compositor state so handlers can drive
    // surface↔output binding (which is what triggers smithay to emit
    // wl_surface.preferred_buffer_scale / preferred_buffer_transform).
    state.register_output(output.clone());

    // 5b. XWayland — spawn the Xwayland binary so X11 clients can connect
    // through us. The X11Wm + X11 display number land on `state` via the
    // `XWaylandEvent::Ready` callback. Failure is logged but non-fatal —
    // wayland-native clients keep working without it.
    crate::wayland::xwayland::start_xwayland(&mut state);

    // 6. winit event loop.
    let mut winit_event_loop =
        WinitEventLoop::new().context("failed to create winit event loop")?;
    winit_event_loop.set_control_flow(ControlFlow::Poll);

    let pending_keys: Arc<Mutex<VecDeque<PendingKeyEvent>>> = Arc::new(Mutex::new(VecDeque::new()));
    let pending_pointers: Arc<Mutex<VecDeque<PendingPointerEvent>>> =
        Arc::new(Mutex::new(VecDeque::new()));
    let mut app = CompositorApp::new(
        window_ref,
        ui,
        pending_keys.clone(),
        pending_pointers.clone(),
        dock_entries,
        appmenu_addr.clone(),
        appmenu_results.clone(),
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
    app.pending_close = pending_close;
    app.pending_minimize = pending_minimize;
    app.pending_maximize = pending_maximize;
    app.pending_activate = pending_activate;
    app.pending_dock_action = pending_dock_action;

    // 7. Main loop.
    info!("Entering GPU compositor main loop");
    loop {
        app.shortcuts_inhibited = input_util::keyboard_shortcuts_inhibited(&state);

        match winit_event_loop.pump_app_events(Some(Duration::from_millis(1)), &mut app) {
            PumpStatus::Exit(_) => {
                info!("winit exited");
                break;
            }
            PumpStatus::Continue => {}
        }

        calloop
            .dispatch(Some(Duration::ZERO), &mut state)
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
                let items: Vec<crate::TrayIconItem> = app
                    .tray_items
                    .iter()
                    .map(|t| crate::TrayIconItem {
                        id: t.id as i32,
                        title: SharedString::from(t.title.as_str()),
                        icon_name: SharedString::from(t.icon_name.as_str()),
                        icon: if t.icon_name.is_empty() {
                            slint::Image::default()
                        } else {
                            // Resolve by icon-theme name (e.g. "telegram",
                            // "discord") via the existing desktop helper.
                            desktop::load_icon_by_name(&t.icon_name).unwrap_or_default()
                        },
                        service: SharedString::from(t.service.as_str()),
                        object_path: SharedString::from(t.object_path.as_str()),
                    })
                    .collect();
                let model = std::rc::Rc::new(VecModel::from(items));
                ui.set_tray_items(slint::ModelRc::from(model));
            }
        }

        if state.should_exit {
            break;
        }

        // Refresh surface↔output bindings once per frame so newly-mapped
        // surfaces get a wl_surface.enter (and the matching
        // preferred_buffer_scale on v6 clients) without us having to hook
        // every protocol entry point individually.
        state.bind_surfaces_to_output();

        // ── BEGIN layer-shell layout block ─────────────────────────────────
        // Re-read each layer surface's cached anchor / margin / exclusive_zone
        // and recompute its compositor-space rect. Forward the per-edge
        // exclusive-zone reservation to the WM so toplevels avoid panel/dock.
        // Then publish the layer surfaces to Slint so Compositor.slint's
        // background+bottom and top+overlay LayerSurface repeaters render
        // them at the right rect.
        state.refresh_layer_layout(app.wm.output_w, app.wm.output_h);
        let reserved = state.reserved_zones();
        app.wm
            .set_reserved_zones(reserved.top, reserved.bottom, reserved.left, reserved.right);
        app.update_layers(&mut state);
        // ── END layer-shell layout block ───────────────────────────────────

        // Visibility gate: only frame-callback surfaces the renderer
        // actually consumed this frame. Anvil derives this from the
        // damage tracker's RenderOutputResult.states; without one we use
        // "windows the WM considers mapped + non-minimised + non-closing
        // AND with a non-zero client buffer", plus all layer surfaces
        // (always visible if mapped). Without this gate every mapped
        // client gets driven at full output framerate even when invisible.
        let mut visible_surfaces: Vec<WlSurface> = Vec::with_capacity(
            app.wm.windows.len() + state.layer_surfaces.len() + state.popups.len(),
        );
        for win in app.wm.windows_sorted() {
            if win.minimized || win.closing {
                continue;
            }
            if let Some(tl) = state.toplevels.iter().find(|t| t.surface == win.surface) {
                let (bw, bh) = {
                    let p = tl.pixels.lock().unwrap();
                    (p.width, p.height)
                };
                if bw == 0 || bh == 0 {
                    continue;
                }
                visible_surfaces.push(tl.surface.clone());
            }
        }
        for li in &state.layer_surfaces {
            visible_surfaces.push(li.surface.wl_surface().clone());
        }
        // Popups need wl_surface.frame callbacks too — without them the
        // client never commits a buffer for the popup, which is why GTK
        // context menus appeared to "not show". Include popups
        // unconditionally; smithay's send_frame_callbacks skips surfaces
        // with no pending callbacks so the cost is a free walk.
        for popup in &state.popups {
            visible_surfaces.push(popup.surface.clone());
        }
        // X11 override-redirect surfaces also need frame callbacks. They live
        // in state.toplevels but aren't tracked by WindowManager (we treat
        // them as popups in the render path), so the wm.windows loop above
        // misses them.
        for tl in &state.toplevels {
            if tl
                .x11_surface
                .as_ref()
                .and_then(|x| {
                    x.user_data()
                        .get::<crate::wayland::xwayland::X11OverrideRedirect>()
                })
                .is_some()
            {
                visible_surfaces.push(tl.surface.clone());
            }
        }

        state.send_frame_callbacks_for(&output, &visible_surfaces);

        // wp_presentation_feedback: fire `presented` with the timestamp
        // captured immediately after `frame.present()`. Skip if no frame
        // has presented yet this run (first iteration).
        if let Some(present_time) = app.last_present_time.take() {
            state.send_presentation_feedback_for(
                &output,
                &visible_surfaces,
                present_time,
                app.frame_count,
            );
        }

        state.pre_render_drive_clients();

        state.display_handle.flush_clients().ok();
        slint::platform::update_timers_and_animations();

        // Sync WM with SpikeState toplevels: register new toplevels + handle destroyed ones.
        sync_new_toplevels(&mut app.wm, &mut state);

        // Process WM actions from Slint callbacks.
        app.process_wm_actions(&mut state);

        app.update_client_texture(&mut state);
        // Service ext-image-copy-capture-v1 frames the calloop dispatch above
        // queued. Has to run AFTER render_frame (which already happened in
        // pump_app_events) so the readback samples the just-presented
        // final_tex, not the previous frame's contents.
        app.process_capture_frames(&mut state);
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
                info!(
                    "control-centre: toggled theme to {:?}",
                    app.theme.current_mode
                );
            }
        }

        // Calendar navigation — drain the prev/next chevron deltas
        // accumulated since last tick, apply, and push the rebuilt
        // grid back to slint.
        {
            let delta = pending_calendar_delta.swap(0, std::sync::atomic::Ordering::Relaxed);
            if delta != 0 {
                app.calendar_month_offset += delta;
                app.refresh_calendar();
                if let Some(gpu) = app.gpu_window.as_ref() {
                    gpu.mark_dirty();
                }
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
                    PendingPointerEvent::Axis {
                        dx,
                        dy,
                        discrete_v120,
                        is_wheel,
                    } => {
                        app.forward_pointer_axis(&mut state, dx, dy, discrete_v120, is_wheel);
                    }
                    PendingPointerEvent::TouchDown { slot, x, y } => {
                        let time = state.clock.now().as_millis();
                        input_util::forward_touch_down(
                            &mut state,
                            slot,
                            x,
                            y,
                            time,
                            |state, hit_x, hit_y| app.surface_under_full(state, hit_x, hit_y),
                        );
                    }
                    PendingPointerEvent::TouchMotion { slot, x, y } => {
                        let time = state.clock.now().as_millis();
                        input_util::forward_touch_motion(&mut state, slot, x, y, time);
                    }
                    PendingPointerEvent::TouchUp { slot } => {
                        let time = state.clock.now().as_millis();
                        input_util::forward_touch_up(&mut state, slot, time);
                    }
                    PendingPointerEvent::TouchCancel => {
                        input_util::forward_touch_cancel(&mut state);
                    }
                    PendingPointerEvent::TouchFrame => {
                        input_util::forward_touch_frame(&mut state);
                    }
                }
            }
        }

        // Frame pacing. While a window animation is in flight the render's
        // vsync-blocked `present()` already paces the loop at the host
        // refresh — an extra sleep here just pushes the next frame past the
        // vsync deadline and drops the animation below 60fps (choppy). Only
        // sleep when idle, where it stops the event poll busy-spinning.
        let animating = app.wm.windows.values().any(|w| !w.anim.is_settled());
        if !animating {
            std::thread::sleep(Duration::from_millis(4));
        }
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
        // OR windows are popup-like and never managed by the WM.
        if toplevel
            .x11_surface
            .as_ref()
            .map(|x| {
                x.user_data()
                    .get::<crate::wayland::xwayland::X11OverrideRedirect>()
                    .is_some()
            })
            .unwrap_or(false)
        {
            continue;
        }
        let key = toplevel.surface.id().protocol_id() as usize;
        if !wm.windows.contains_key(&key) {
            // Register the toplevel with the WM (starts open animation).
            let id = wm.add_window(toplevel.surface.clone());
            debug!("WM: synced new toplevel id={} key={}", id, key);
        }
    }

    // The WM places windows via `smart_cascade_position` (centres the first
    // window, cascades the rest), but xdg_shell's `new_toplevel` seeded
    // `ToplevelInfo.x/y` with its own naive cascade before the WM ran. Pull
    // the WM's authoritative position back into ToplevelInfo so callers that
    // read `tl.x/tl.y` (drag-offset computation, popup positioning) match the
    // rendered/hit-tested position. Without this, the *first* drag on a freshly
    // mapped window has its grab anchor offset by (wm_pos - cascade_seed).
    for toplevel in &mut state.toplevels {
        let key = toplevel.surface.id().protocol_id() as usize;
        if let Some(win) = wm.windows.get(&key) {
            toplevel.x = win.x;
            toplevel.y = win.y;
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
