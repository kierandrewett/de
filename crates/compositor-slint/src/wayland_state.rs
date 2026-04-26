//! Compositor state — all protocol state fields live here.
//!
//! Handler trait implementations are split into sub-modules under
//! `src/wayland/`.  This file only contains:
//!   - `SpikeState` struct definition + constructor
//!   - `ClientState` (per-connection user data)
//!   - `ClientSurfaceData` (shared pixel buffer)
//!   - `import_shm_buffer` helper (used by wayland/compositor.rs)
//!   - Handler impls for the three protocols that still live inline:
//!     BufferHandler, ShmHandler, DmabufHandler, SeatHandler, SelectionHandler,
//!     DataDeviceHandler, PrimarySelectionHandler.

use std::sync::{Arc, Mutex};

use smithay::{
    delegate_dmabuf, delegate_seat, delegate_shm,
    input::{pointer::CursorImageStatus, Seat, SeatHandler, SeatState},
    reexports::{
        calloop::{LoopHandle, LoopSignal},
        wayland_server::{
            backend::{ClientData, ClientId, DisconnectReason},
            protocol::{wl_buffer::WlBuffer, wl_surface::WlSurface},
            Client, DisplayHandle,
        },
    },
    utils::{Clock, Monotonic},
    wayland::{
        alpha_modifier::AlphaModifierState,
        buffer::BufferHandler,
        commit_timing::CommitTimingManagerState,
        compositor::{CompositorClientState, CompositorState},
        content_type::ContentTypeState,
        cursor_shape::CursorShapeManagerState,
        dmabuf::{DmabufGlobal, DmabufHandler, DmabufState, ImportNotifier},
        fifo::FifoManagerState,
        foreign_toplevel_list::ForeignToplevelListState,
        fractional_scale::FractionalScaleManagerState,
        idle_inhibit::IdleInhibitManagerState,
        idle_notify::IdleNotifierState,
        input_method::InputMethodManagerState,
        keyboard_shortcuts_inhibit::KeyboardShortcutsInhibitState,
        output::OutputManagerState,
        pointer_constraints::PointerConstraintsState,
        pointer_gestures::PointerGesturesState,
        presentation::PresentationState,
        relative_pointer::RelativePointerManagerState,
        security_context::SecurityContextState,
        selection::{
            data_device::{DataDeviceHandler, DataDeviceState, WaylandDndGrabHandler},
            primary_selection::{PrimarySelectionHandler, PrimarySelectionState},
            SelectionHandler,
        },
        session_lock::SessionLockManagerState,
        shell::{
            kde::decoration::KdeDecorationState,
            wlr_layer::WlrLayerShellState,
            xdg::{decoration::XdgDecorationState, XdgShellState},
        },
        shm::{ShmHandler, ShmState},
        single_pixel_buffer::SinglePixelBufferState,
        tablet_manager::TabletManagerState,
        text_input::TextInputManagerState,
        viewporter::ViewporterState,
        virtual_keyboard::VirtualKeyboardManagerState,
        xdg_activation::XdgActivationState,
        xdg_foreign::XdgForeignState,
        xdg_system_bell::XdgSystemBellState,
        xdg_toplevel_icon::XdgToplevelIconManager,
        xdg_toplevel_tag::XdgToplevelTagManager,
    },
};
use tracing::{debug, info, warn};

use crate::wayland::layer_shell::LayerInfo;

// ──────────────────────────────────────────────────────────────────────────────
// Client state (per-connection user data)
// ──────────────────────────────────────────────────────────────────────────────

#[derive(Default)]
pub struct ClientState {
    pub compositor_state: CompositorClientState,
}

impl ClientData for ClientState {
    fn initialized(&self, _client_id: ClientId) {
        debug!("wayland client connected");
    }
    fn disconnected(&self, _client_id: ClientId, _reason: DisconnectReason) {
        info!("wayland client disconnected");
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Shared pixel buffer (SHM → Slint texture pipeline)
// ──────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
pub struct ClientSurfaceData {
    pub pixels: Vec<u8>, // RGBA8 premultiplied (for slint::Image::from_rgba8_premultiplied)
    pub width: u32,
    pub height: u32,
    pub dirty: bool,
}

// ──────────────────────────────────────────────────────────────────────────────
// Per-toplevel window info (multi-window bookkeeping)
// ──────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ToplevelInfo {
    /// The wayland surface for this toplevel.
    pub surface: WlSurface,
    /// Cascaded compositor-space position.
    pub x: i32,
    pub y: i32,
    /// Pixel buffer — updated by import_shm_buffer on each commit.
    pub pixels: Arc<Mutex<ClientSurfaceData>>,
}

// ──────────────────────────────────────────────────────────────────────────────
// Compositor state
// ──────────────────────────────────────────────────────────────────────────────

pub struct SpikeState {
    pub display_handle: DisplayHandle,
    pub loop_handle: LoopHandle<'static, SpikeState>,
    pub loop_signal: LoopSignal,
    pub clock: Clock<Monotonic>,

    // ── P0 Core ──────────────────────────────────────────────────────────
    pub compositor_state: CompositorState,
    pub shm_state: ShmState,
    pub seat_state: SeatState<Self>,
    pub seat: Seat<Self>,
    pub xdg_shell_state: XdgShellState,
    pub output_manager_state: OutputManagerState,
    pub dmabuf_state: DmabufState,
    pub presentation_state: PresentationState,
    pub single_pixel_buffer_state: SinglePixelBufferState,

    // ── P1 Shell chrome ───────────────────────────────────────────────────
    pub layer_shell_state: WlrLayerShellState,

    // ── P1 Decorations ────────────────────────────────────────────────────
    pub xdg_decoration_state: XdgDecorationState,
    pub kde_decoration_state: KdeDecorationState,

    // ── P1 Clipboard / selections ─────────────────────────────────────────
    pub data_device_state: DataDeviceState,
    pub primary_selection_state: PrimarySelectionState,

    // ── P1 Scaling ────────────────────────────────────────────────────────
    pub fractional_scale_manager_state: FractionalScaleManagerState,
    pub viewporter_state: ViewporterState,

    // ── P1 Frame timing ───────────────────────────────────────────────────
    pub fifo_state: FifoManagerState,
    pub commit_timing_state: CommitTimingManagerState,

    // ── P1 Input extensions ───────────────────────────────────────────────
    pub relative_pointer_state: RelativePointerManagerState,
    pub pointer_constraints_state: PointerConstraintsState,
    pub pointer_gestures_state: PointerGesturesState,
    pub keyboard_shortcuts_inhibit_state: KeyboardShortcutsInhibitState,
    pub cursor_shape_manager_state: CursorShapeManagerState,
    pub tablet_manager_state: TabletManagerState,

    // ── P1 IME / virtual keyboard ─────────────────────────────────────────
    pub text_input_state: TextInputManagerState,
    pub input_method_state: InputMethodManagerState,
    pub virtual_keyboard_state: VirtualKeyboardManagerState,

    // ── P1 Idle / lock ────────────────────────────────────────────────────
    pub idle_notifier_state: IdleNotifierState<Self>,
    pub idle_inhibit_manager_state: IdleInhibitManagerState,
    pub session_lock_manager_state: SessionLockManagerState,

    // ── P2 Misc ───────────────────────────────────────────────────────────
    pub activation_state: XdgActivationState,
    pub content_type_state: ContentTypeState,
    pub alpha_modifier_state: AlphaModifierState,
    pub xdg_foreign_state: XdgForeignState,
    pub foreign_toplevel_list_state: ForeignToplevelListState,
    pub security_context_state: SecurityContextState,
    pub xdg_system_bell_state: XdgSystemBellState,
    pub xdg_toplevel_icon_manager: XdgToplevelIconManager,
    pub xdg_toplevel_tag_manager: XdgToplevelTagManager,

    // ── Bookkeeping ───────────────────────────────────────────────────────
    /// Currently-focused xdg toplevel surface.
    pub active_surface: Option<WlSurface>,

    /// All mapped toplevels (multi-window support, insertion-ordered).
    pub toplevels: Vec<ToplevelInfo>,

    /// All mapped layer-shell surfaces (populated by WlrLayerShellHandler).
    pub layer_surfaces: Vec<LayerInfo>,

    /// Shared pixel buffer (SHM surface → Slint texture) — legacy single-window path.
    pub client_pixels: Arc<Mutex<ClientSurfaceData>>,

    pub should_exit: bool,
    pub pointer_pos: (f64, f64),
}

impl SpikeState {
    pub fn new(
        display_handle: DisplayHandle,
        loop_handle: LoopHandle<'static, SpikeState>,
        loop_signal: LoopSignal,
    ) -> Self {
        let clock = Clock::<Monotonic>::new();
        let dh = &display_handle;

        let compositor_state = CompositorState::new::<Self>(dh);
        let shm_state = ShmState::new::<Self>(dh, vec![]);
        let mut seat_state = SeatState::new();
        let seat = seat_state.new_wl_seat(dh, "seat-spike");
        let xdg_shell_state = XdgShellState::new::<Self>(dh);
        let output_manager_state = OutputManagerState::new_with_xdg_output::<Self>(dh);
        let dmabuf_state = DmabufState::new();
        let presentation_state = PresentationState::new::<Self>(dh, clock.id() as u32);
        let single_pixel_buffer_state = SinglePixelBufferState::new::<Self>(dh);

        let layer_shell_state = WlrLayerShellState::new::<Self>(dh);

        let xdg_decoration_state = XdgDecorationState::new::<Self>(dh);
        let kde_decoration_state = KdeDecorationState::new::<Self>(
            dh,
            wayland_protocols_misc::server_decoration::server::org_kde_kwin_server_decoration_manager::Mode::Server,
        );

        let data_device_state = DataDeviceState::new::<Self>(dh);
        let primary_selection_state = PrimarySelectionState::new::<Self>(dh);

        let fractional_scale_manager_state = FractionalScaleManagerState::new::<Self>(dh);
        let viewporter_state = ViewporterState::new::<Self>(dh);

        let fifo_state = FifoManagerState::new::<Self>(dh);
        let commit_timing_state = CommitTimingManagerState::new::<Self>(dh);

        let relative_pointer_state = RelativePointerManagerState::new::<Self>(dh);
        let pointer_constraints_state = PointerConstraintsState::new::<Self>(dh);
        let pointer_gestures_state = PointerGesturesState::new::<Self>(dh);
        let keyboard_shortcuts_inhibit_state = KeyboardShortcutsInhibitState::new::<Self>(dh);
        let cursor_shape_manager_state = CursorShapeManagerState::new::<Self>(dh);
        let tablet_manager_state = TabletManagerState::new::<Self>(dh);

        let text_input_state = TextInputManagerState::new::<Self>(dh);
        let input_method_state = InputMethodManagerState::new::<Self, _>(dh, |_| true);
        let virtual_keyboard_state = VirtualKeyboardManagerState::new::<Self, _>(dh, |_| true);

        let idle_notifier_state = IdleNotifierState::<Self>::new(dh, loop_handle.clone());
        let idle_inhibit_manager_state = IdleInhibitManagerState::new::<Self>(dh);
        let session_lock_manager_state = SessionLockManagerState::new::<Self, _>(dh, |_| true);

        let activation_state = XdgActivationState::new::<Self>(dh);
        let content_type_state = ContentTypeState::new::<Self>(dh);
        let alpha_modifier_state = AlphaModifierState::new::<Self>(dh);
        let xdg_foreign_state = XdgForeignState::new::<Self>(dh);
        let foreign_toplevel_list_state = ForeignToplevelListState::new::<Self>(dh);
        let security_context_state = SecurityContextState::new::<Self, _>(dh, |_| true);
        let xdg_system_bell_state = XdgSystemBellState::new::<Self>(dh);
        let xdg_toplevel_icon_manager = XdgToplevelIconManager::new::<Self>(dh);
        let xdg_toplevel_tag_manager = XdgToplevelTagManager::new::<Self>(dh);

        let mut state = Self {
            display_handle,
            loop_handle,
            loop_signal,
            clock,
            compositor_state,
            shm_state,
            seat_state,
            seat,
            xdg_shell_state,
            output_manager_state,
            dmabuf_state,
            presentation_state,
            single_pixel_buffer_state,
            layer_shell_state,
            xdg_decoration_state,
            kde_decoration_state,
            data_device_state,
            primary_selection_state,
            fractional_scale_manager_state,
            viewporter_state,
            fifo_state,
            commit_timing_state,
            relative_pointer_state,
            pointer_constraints_state,
            pointer_gestures_state,
            keyboard_shortcuts_inhibit_state,
            cursor_shape_manager_state,
            tablet_manager_state,
            text_input_state,
            input_method_state,
            virtual_keyboard_state,
            idle_notifier_state,
            idle_inhibit_manager_state,
            session_lock_manager_state,
            activation_state,
            content_type_state,
            alpha_modifier_state,
            xdg_foreign_state,
            foreign_toplevel_list_state,
            security_context_state,
            xdg_system_bell_state,
            xdg_toplevel_icon_manager,
            xdg_toplevel_tag_manager,
            active_surface: None,
            toplevels: Vec::new(),
            layer_surfaces: Vec::new(),
            client_pixels: Arc::new(Mutex::new(ClientSurfaceData::default())),
            should_exit: false,
            pointer_pos: (0.0, 0.0),
        };

        state.seat.add_keyboard(Default::default(), 200, 25).ok();
        state.seat.add_pointer();

        state
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// SHM buffer import helper (called from wayland/compositor.rs)
// ──────────────────────────────────────────────────────────────────────────────

pub fn import_shm_buffer(surface: &WlSurface, pixels_out: &Arc<Mutex<ClientSurfaceData>>) {
    use smithay::backend::renderer::utils::with_renderer_surface_state;
    use smithay::wayland::shm::with_buffer_contents;

    let buffer = match with_renderer_surface_state(surface, |s| s.buffer().cloned()) {
        Some(Some(b)) => b,
        _ => return,
    };

    let result = with_buffer_contents(&*buffer, |ptr: *const u8, len: usize, spec| {
        use smithay::reexports::wayland_server::protocol::wl_shm;

        let width = spec.width as u32;
        let height = spec.height as u32;
        let stride = spec.stride as usize;

        // Both ARGB8888 and XRGB8888 store bytes in LE as [B, G, R, A/X].
        // For XRGB8888 the X byte is always 0 — treat as fully opaque (alpha=255)
        // so pixels are not zeroed out by the premultiply step below.
        let has_alpha = matches!(spec.format, wl_shm::Format::Argb8888);

        let data = unsafe { std::slice::from_raw_parts(ptr, len) };

        let mut rgba = vec![0u8; (width * height * 4) as usize];

        for y in 0..height as usize {
            for x in 0..width as usize {
                let src = y * stride + x * 4;
                let dst = (y * width as usize + x) * 4;
                if src + 4 > data.len() {
                    break;
                }
                // wl_shm ARGB8888/XRGB8888 in LE: [B, G, R, A/X]
                let b = data[src];
                let g = data[src + 1];
                let r = data[src + 2];
                let a = if has_alpha { data[src + 3] } else { 255u8 };
                // Premultiply for slint::Image::from_rgba8_premultiplied().
                let af = a as f32 / 255.0;
                rgba[dst] = (r as f32 * af) as u8;
                rgba[dst + 1] = (g as f32 * af) as u8;
                rgba[dst + 2] = (b as f32 * af) as u8;
                rgba[dst + 3] = a;
            }
        }

        info!("client surface imported, {}x{} SHM buffer (fmt={:?})", width, height, spec.format);

        ClientSurfaceData {
            pixels: rgba,
            width,
            height,
            dirty: true,
        }
    });

    match result {
        Ok(data) => *pixels_out.lock().unwrap() = data,
        Err(e) => warn!("SHM read failed: {:?}", e),
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// BufferHandler + ShmHandler
// ──────────────────────────────────────────────────────────────────────────────

impl BufferHandler for SpikeState {
    fn buffer_destroyed(&mut self, _buffer: &WlBuffer) {}
}

impl ShmHandler for SpikeState {
    fn shm_state(&self) -> &ShmState {
        &self.shm_state
    }
}

delegate_shm!(SpikeState);

// ──────────────────────────────────────────────────────────────────────────────
// DmabufHandler
// ──────────────────────────────────────────────────────────────────────────────

impl DmabufHandler for SpikeState {
    fn dmabuf_state(&mut self) -> &mut DmabufState {
        &mut self.dmabuf_state
    }

    fn dmabuf_imported(
        &mut self,
        _global: &DmabufGlobal,
        dmabuf: smithay::backend::allocator::dmabuf::Dmabuf,
        notifier: ImportNotifier,
    ) {
        warn!(
            planes = dmabuf.num_planes(),
            "DMA-BUF import requested — not supported in spike (client should use SHM)"
        );
        drop(notifier);
    }
}

delegate_dmabuf!(SpikeState);

// ──────────────────────────────────────────────────────────────────────────────
// SeatHandler
// ──────────────────────────────────────────────────────────────────────────────

impl SeatHandler for SpikeState {
    type KeyboardFocus = WlSurface;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;

    fn seat_state(&mut self) -> &mut SeatState<Self> {
        &mut self.seat_state
    }

    fn focus_changed(&mut self, _seat: &Seat<Self>, _focused: Option<&Self::KeyboardFocus>) {}

    fn cursor_image(&mut self, _seat: &Seat<Self>, _image: CursorImageStatus) {}
}

delegate_seat!(SpikeState);

// ──────────────────────────────────────────────────────────────────────────────
// SelectionHandler (required by DataDeviceHandler + PrimarySelectionHandler)
// ──────────────────────────────────────────────────────────────────────────────

impl SelectionHandler for SpikeState {
    type SelectionUserData = ();
}

// ──────────────────────────────────────────────────────────────────────────────
// DataDeviceHandler
// ──────────────────────────────────────────────────────────────────────────────

impl WaylandDndGrabHandler for SpikeState {}

impl DataDeviceHandler for SpikeState {
    fn data_device_state(&mut self) -> &mut DataDeviceState {
        &mut self.data_device_state
    }
}

smithay::delegate_data_device!(SpikeState);

// ──────────────────────────────────────────────────────────────────────────────
// PrimarySelectionHandler
// ──────────────────────────────────────────────────────────────────────────────

impl PrimarySelectionHandler for SpikeState {
    fn primary_selection_state(&mut self) -> &mut PrimarySelectionState {
        &mut self.primary_selection_state
    }
}

smithay::delegate_primary_selection!(SpikeState);
