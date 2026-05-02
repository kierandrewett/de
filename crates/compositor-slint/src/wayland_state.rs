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
//!
//! ## DMA-BUF import strategy (Option B)
//!
//! wgpu-28 (required by Slint 1.16) does not expose a stable DMA-BUF import
//! API.  Instead we use a two-stage CPU-roundtrip import path:
//!
//!   Stage 1 — smithay's GlesRenderer imports the DMA-BUF as an EGL image /
//!             GL texture (via `ImportDma::import_dmabuf`).  This requires an
//!             EGL display and GL context, which we initialise lazily on the
//!             first DMA-BUF commit using `EGLSurfacelessDisplay` (no window
//!             required on Mesa/surfaceless).
//!
//!   Stage 2 — `ExportMem::copy_texture` reads the GL texture pixels back to
//!             CPU RAM (glReadPixels into a PBO, then mapped).  The result is
//!             converted to premultiplied RGBA8 and stored in the same
//!             `ClientSurfaceData` structure that the SHM path uses.
//!             `renderer.rs` then uploads it to wgpu as a normal texture.
//!
//! Performance note: the CPU round-trip is ~5-20 ms per frame at 1080p
//! (PCIe bandwidth + glReadPixels stall).  It unblocks Firefox, kitty GPU,
//! and all GTK4/wgpu-based clients that prefer DMA-BUF.  A zero-copy path
//! (wgpu-29 `create_texture_from_hal`) can replace this once Slint ships a
//! wgpu-29 feature gate.

use std::sync::{Arc, Mutex};

use smithay::{
    backend::{
        allocator::dmabuf::Dmabuf,
        egl::{EGLContext, EGLDisplay},
        renderer::{
            gles::GlesRenderer,
            ExportMem, ImportDma,
        },
    },
    delegate_dmabuf, delegate_seat, delegate_shm,
    input::{
        dnd::{DnDGrab, DndGrabHandler, DndTarget, GrabType, Source},
        pointer::{CursorImageStatus, Focus},
        Seat, SeatHandler, SeatState,
    },
    output::Output,
    reexports::{
        calloop::{LoopHandle, LoopSignal},
        wayland_server::{
            backend::{ClientData, ClientId, DisconnectReason, ObjectId},
            protocol::{wl_buffer::WlBuffer, wl_shm, wl_surface::WlSurface},
            Client, DisplayHandle, Resource,
        },
    },
    utils::{Clock, Logical, Monotonic, Point, Serial},
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
        image_capture_source::{
            ImageCaptureSource, ImageCaptureSourceHandler, ImageCaptureSourceState,
            OutputCaptureSourceHandler, OutputCaptureSourceState,
        },
        image_copy_capture::{
            BufferConstraints, Frame, ImageCopyCaptureHandler, ImageCopyCaptureState, Session,
            SessionRef,
        },
        input_method::InputMethodManagerState,
        keyboard_shortcuts_inhibit::KeyboardShortcutsInhibitState,
        output::OutputManagerState,
        pointer_constraints::PointerConstraintsState,
        pointer_gestures::PointerGesturesState,
        presentation::PresentationState,
        relative_pointer::RelativePointerManagerState,
        security_context::SecurityContextState,
        selection::{
            data_device::{
                set_data_device_focus, DataDeviceHandler, DataDeviceState, WaylandDndGrabHandler,
            },
            primary_selection::{
                set_primary_focus, PrimarySelectionHandler, PrimarySelectionState,
            },
            wlr_data_control::{DataControlHandler, DataControlState},
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
use smithay::wayland::shell::xdg::ToplevelSurface;

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
    /// The ToplevelSurface handle (for sending configure / close to the client).
    pub toplevel: ToplevelSurface,
    /// Cascaded compositor-space position.
    pub x: i32,
    pub y: i32,
    /// Pixel buffer — updated by import_shm_buffer on each commit.
    pub pixels: Arc<Mutex<ClientSurfaceData>>,
    /// Client requested ClientSide decorations (CSD). False = SSD; we draw
    /// our own titlebar above the client surface.
    pub csd: bool,
}

/// A mapped xdg_popup — context menus, dropdowns, autocomplete, etc.
/// Position is computed at `new_popup` time from the positioner, but the
/// final compositor-space placement is resolved per-frame in `update_windows`
/// (parent toplevel may have moved). Pixels are imported on each commit
/// via the surface-tree composite (popups can have their own subsurfaces).
#[derive(Debug, Clone)]
pub struct PopupInfo {
    pub surface: WlSurface,
    pub popup: smithay::wayland::shell::xdg::PopupSurface,
    /// Parent surface (toplevel OR another popup) — pop-up tree origin.
    pub parent: WlSurface,
    /// Popup geometry rect relative to the parent surface (positioner output).
    pub rel_x: i32,
    pub rel_y: i32,
    pub w: i32,
    pub h: i32,
    /// Composited pixel buffer from the popup's surface tree.
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
    // wlr-data-control powers wl-clipboard / cliphist / wl-paste — without it
    // those tools can't observe or write the selection at all.
    pub data_control_state: DataControlState,

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

    // ── P2 Screen capture (ext-image-copy-capture-v1) ─────────────────────
    /// Stateless registry of `ext-image-capture-source` objects (the protocol
    /// side that lets a client say "I want to capture this output / toplevel").
    pub image_capture_source_state: ImageCaptureSourceState,
    /// Backs `ext-output-image-capture-source-manager-v1` — vends per-output
    /// capture sources and stores a `WeakOutput` in the source's user data so
    /// `capture_constraints` can recover the size/format on demand.
    pub output_capture_source_state: OutputCaptureSourceState,
    /// Backs `ext-image-copy-capture-manager-v1` — manages capture sessions
    /// and frames. We currently fail every frame request via
    /// `frame.fail(Unknown)` since wgpu-side framebuffer readback is not
    /// wired yet, but the global must still bind so `grim` /
    /// `xdg-desktop-portal-wlr` / OBS don't error out at startup.
    pub image_copy_capture_state: ImageCopyCaptureState,

    // ── Bookkeeping ───────────────────────────────────────────────────────
    /// The single virtual output for this compositor. Stored here so the
    /// surface enter/leave bookkeeping (which drives wl_output binding and
    /// preferred_buffer_scale emission) can find it from any handler.
    pub output: Option<Output>,

    /// Currently-focused xdg toplevel surface.
    pub active_surface: Option<WlSurface>,

    /// Drag-and-drop icon surface for the active client-initiated DnD grab.
    /// Set by `WaylandDndGrabHandler::dnd_requested` when the client supplies
    /// an icon, cleared in `DndGrabHandler::dropped` / `cancelled`. The
    /// renderer composites this surface under the cursor while a DnD is
    /// active. TODO(renderer): pick this up in the per-frame compose pass
    /// (see `update_windows`) and draw the icon at `pointer_pos`.
    pub dnd_icon: Option<WlSurface>,

    /// All mapped toplevels (multi-window support, insertion-ordered).
    pub toplevels: Vec<ToplevelInfo>,

    /// Mapped xdg_popups — context menus, dropdowns, autocomplete lists.
    /// Cleared per-popup in `popup_destroyed`.
    pub popups: Vec<PopupInfo>,

    /// All mapped layer-shell surfaces (populated by WlrLayerShellHandler).
    pub layer_surfaces: Vec<LayerInfo>,

    /// Shared pixel buffer (SHM surface → Slint texture) — legacy single-window path.
    pub client_pixels: Arc<Mutex<ClientSurfaceData>>,

    /// Surfaces that were destroyed since the last main-loop iteration.
    /// The WM processes these to begin close animations.
    pub destroyed_surfaces: Vec<WlSurface>,

    // ── xdg-shell client requests routed to the renderer ────────────────────
    /// Toplevel surfaces whose client called `xdg_toplevel.move`. The
    /// renderer drains this in `process_wm_actions` and starts an
    /// `ActiveDrag::Move` if the pointer is grabbed.
    pub pending_xdg_move: Vec<WlSurface>,
    /// `(surface, edge)` pairs from `xdg_toplevel.resize`.
    pub pending_xdg_resize: Vec<(WlSurface, crate::resize::ResizeEdge)>,
    /// `(surface, want_maximized)` from `xdg_toplevel.set_maximized` /
    /// `unset_maximized`. The renderer applies via `WindowManager::start_maximize`
    /// / `start_unmaximize` and replies with a configure carrying the new state.
    pub pending_xdg_maximize: Vec<(WlSurface, bool)>,
    /// `(surface, want_fullscreen)` from `set_fullscreen` / `unset_fullscreen`.
    /// We treat fullscreen identically to maximize for now (no per-output
    /// targeting yet) but report the protocol state honestly.
    pub pending_xdg_fullscreen: Vec<(WlSurface, bool)>,
    /// Toplevel surfaces whose client called `xdg_toplevel.set_minimized`.
    pub pending_xdg_minimize: Vec<WlSurface>,

    pub should_exit: bool,
    pub pointer_pos: (f64, f64),

    // ── DMA-BUF two-stage import (Option B) ──────────────────────────────────
    /// Surfaceless EGL display — initialised lazily on first DMA-BUF import.
    /// `None` means not yet attempted or init failed (see `egl_init_tried`).
    /// Latest cursor-image request from the focused client. `Default` keeps
    /// our compositor-supplied xcursor; `Hidden` hides the cursor entirely
    /// while the pointer is over that client; `Surface(_)` is a client-
    /// supplied cursor surface (drawing apps, custom carets) — pixels are
    /// imported in the commit handler into `cursor_surface_pixels`.
    pub cursor_status: CursorImageStatus,
    /// Composited pixel buffer for the current `Surface(_)` cursor. Updated
    /// in the commit handler each time the cursor surface commits a new
    /// frame (animated cursors). `update_windows` forwards this to the
    /// Slint `cursor-image` along with the surface's hotspot.
    pub cursor_surface_pixels: Arc<Mutex<ClientSurfaceData>>,
    pub egl_display: Option<EGLDisplay>,
    /// Surfaceless GLES renderer — initialised from `egl_display`.
    /// Used for: `ImportDma::import_dmabuf` → GL texture,
    ///           `ExportMem::copy_texture` → CPU pixel bytes.
    pub gles_renderer: Option<GlesRenderer>,
    /// Set to `true` once EGL init was attempted so we don't retry on every frame.
    pub egl_init_tried: bool,
    /// Pending DMA-BUF pixel data keyed by the surface's `ObjectId`.
    /// Populated by `import_dmabuf_for_surface` from the commit handler;
    /// drained in `commit()`. Keying by surface (not "WxH") keeps two
    /// surfaces with identical dimensions from swapping each other's frames.
    pub dmabuf_pending: std::collections::HashMap<ObjectId, ClientSurfaceData>,
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
        // Bridge wlr-data-control to primary selection so wl-paste --primary
        // works the same way wl-paste does for the regular clipboard.
        let data_control_state =
            DataControlState::new::<Self, _>(dh, Some(&primary_selection_state), |_| true);

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

        let image_capture_source_state = ImageCaptureSourceState::new();
        let output_capture_source_state = OutputCaptureSourceState::new::<Self>(dh);
        let image_copy_capture_state = ImageCopyCaptureState::new::<Self>(dh);

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
            data_control_state,
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
            image_capture_source_state,
            output_capture_source_state,
            image_copy_capture_state,
            output: None,
            active_surface: None,
            dnd_icon: None,
            toplevels: Vec::new(),
            popups: Vec::new(),
            layer_surfaces: Vec::new(),
            client_pixels: Arc::new(Mutex::new(ClientSurfaceData::default())),
            destroyed_surfaces: Vec::new(),
            pending_xdg_move: Vec::new(),
            pending_xdg_resize: Vec::new(),
            pending_xdg_maximize: Vec::new(),
            pending_xdg_fullscreen: Vec::new(),
            pending_xdg_minimize: Vec::new(),
            should_exit: false,
            pointer_pos: (0.0, 0.0),
            cursor_status: CursorImageStatus::default_named(),
            cursor_surface_pixels: Arc::new(Mutex::new(ClientSurfaceData::default())),
            egl_display: None,
            gles_renderer: None,
            egl_init_tried: false,
            dmabuf_pending: std::collections::HashMap::new(),
        };

        state.seat.add_keyboard(Default::default(), 200, 25).ok();
        state.seat.add_pointer();

        state
    }

    /// Lazily initialise the surfaceless EGL display and GLES renderer used
    /// for DMA-BUF two-stage import (Option B).  Called on the first DMA-BUF
    /// commit; subsequent calls are no-ops.
    ///
    /// Uses `EGLSurfacelessDisplay` (Mesa `EGL_MESA_platform_surfaceless`)
    /// so no window handle or KMS device is required.
    pub fn ensure_gles_renderer(&mut self) {
        if self.egl_init_tried {
            return;
        }
        self.egl_init_tried = true;

        use smithay::backend::egl::native::EGLSurfacelessDisplay;

        let egl_display = match unsafe { EGLDisplay::new(EGLSurfacelessDisplay) } {
            Ok(d) => d,
            Err(e) => {
                warn!("DMA-BUF: EGL surfaceless display init failed: {e:?} — DMA-BUF will be rejected");
                return;
            }
        };

        let egl_context = match EGLContext::new(&egl_display) {
            Ok(c) => c,
            Err(e) => {
                warn!("DMA-BUF: EGL context creation failed: {e:?}");
                return;
            }
        };

        let renderer = match unsafe { GlesRenderer::new(egl_context) } {
            Ok(r) => r,
            Err(e) => {
                warn!("DMA-BUF: GlesRenderer init failed: {e:?}");
                return;
            }
        };

        let n_formats = renderer.dmabuf_formats().into_iter().count();
        info!(
            formats = n_formats,
            "DMA-BUF: GLES renderer ready (surfaceless EGL); DMA-BUF import enabled"
        );
        self.egl_display = Some(egl_display);
        self.gles_renderer = Some(renderer);
    }

    /// Import a DMA-BUF that has just been committed to `surface`, run the
    /// CPU-roundtrip readback and store the resulting pixels in
    /// `dmabuf_pending` keyed by the surface's `ObjectId`.
    ///
    /// Called from the commit handler — at that point the acquire-fence
    /// pre-commit blocker (see `install_dmabuf_blocker`) guarantees the
    /// producer GPU is done, so reading from the dmabuf is safe.
    pub fn import_dmabuf_for_surface(&mut self, surface: &WlSurface, dmabuf: &Dmabuf) {
        self.ensure_gles_renderer();

        let renderer = match self.gles_renderer.as_mut() {
            Some(r) => r,
            None => {
                warn!(planes = dmabuf.num_planes(), "DMA-BUF: no GLES renderer at commit");
                return;
            }
        };

        let gles_texture = match renderer.import_dmabuf(dmabuf, None) {
            Ok(t) => t,
            Err(e) => {
                warn!("DMA-BUF: GLES import failed at commit ({e:?})");
                return;
            }
        };

        use smithay::backend::allocator::Buffer as AllocBuffer;
        let size = dmabuf.size();
        let (w, h) = (size.w as u32, size.h as u32);

        let region = smithay::utils::Rectangle::from_loc_and_size(
            smithay::utils::Point::from((0, 0)),
            smithay::utils::Size::from((size.w, size.h)),
        );

        let mapping = match renderer.copy_texture(
            &gles_texture,
            region,
            smithay::backend::allocator::Fourcc::Abgr8888,
        ) {
            Ok(m) => m,
            Err(e) => {
                warn!("DMA-BUF: copy_texture failed ({e:?})");
                return;
            }
        };

        let raw = match renderer.map_texture(&mapping) {
            Ok(bytes) => bytes.to_vec(),
            Err(e) => {
                warn!("DMA-BUF: map_texture failed ({e:?})");
                return;
            }
        };

        // copy_texture with Abgr8888 gives [R, G, B, A] non-premultiplied;
        // slint::Image::from_rgba8_premultiplied expects premultiplied.
        let pixel_count = (w * h) as usize;
        let mut rgba_pm = Vec::with_capacity(pixel_count * 4);
        for chunk in raw.chunks(4) {
            let (r, g, b, a) = (chunk[0], chunk[1], chunk[2], chunk[3]);
            let af = a as f32 / 255.0;
            rgba_pm.push((r as f32 * af) as u8);
            rgba_pm.push((g as f32 * af) as u8);
            rgba_pm.push((b as f32 * af) as u8);
            rgba_pm.push(a);
        }

        debug!(
            "DMA-BUF: imported {}x{} ({} planes) for surface {:?} → {} RGBA bytes",
            w, h, dmabuf.num_planes(), surface.id(), rgba_pm.len()
        );

        self.dmabuf_pending.insert(
            surface.id(),
            ClientSurfaceData {
                pixels: rgba_pm,
                width: w,
                height: h,
                dirty: true,
            },
        );
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// SHM buffer import helper (called from wayland/compositor.rs)
// ──────────────────────────────────────────────────────────────────────────────

/// Composite the surface tree rooted at `surface` into a single premultiplied
/// RGBA8 buffer. Walks parent + every subsurface, blitting each's pixels at
/// the right offset. Critical for clients like Firefox whose xdg_toplevel
/// commits an empty buffer and ships actual content via subsurfaces.
///
/// Strategy:
///   1. First walk: read each surface's buffer dims + cumulative offset
///      relative to the root, plus the maximum extent.
///   2. Allocate an RGBA buffer sized to fit the union of all surfaces.
///   3. Second walk: in z-order (parent → children, declared order), blit
///      each surface's pixels into the composite.
/// Scan a premultiplied-RGBA buffer for the bounding box of opaque content.
/// Used to detect the visible-window rect inside a buffer that has CSD
/// shadow/border padding (Firefox, GTK header-bar apps).
///
/// Samples 7 evenly-spaced columns and 7 evenly-spaced rows (rather than a
/// single centerline) so a single transparent column / row in the buffer
/// can't bias the result. We take min(top) / max(bottom) across columns and
/// min(left) / max(right) across rows so the bbox encloses the *union* of
/// opaque pixels seen on those scanlines — that's why the bottom edge no
/// longer drops off when the centerline happens to land on a blank gutter.
pub fn detect_visible_bbox(pixels: &[u8], width: u32, height: u32) -> Option<(i32, i32, i32, i32)> {
    if width == 0 || height == 0 {
        return None;
    }
    let needed = (width as usize) * (height as usize) * 4;
    if pixels.len() < needed {
        return None;
    }
    // Threshold sized to stay above shadow-gradient peaks (~60–100) and
    // catch anti-aliased border edges (~150+).
    const ALPHA_THRESHOLD: u8 = 100;
    let alpha_at = |x: u32, y: u32| -> u8 {
        let i = (y as usize * width as usize + x as usize) * 4 + 3;
        pixels[i]
    };
    let mut top = u32::MAX;
    let mut bottom = 0u32;
    let mut found_v = false;
    for i in 1u32..=7 {
        let cx = (width.saturating_mul(i)) / 8;
        if let Some(t) = (0..height).find(|&y| alpha_at(cx, y) > ALPHA_THRESHOLD) {
            top = top.min(t);
            found_v = true;
        }
        if let Some(b) = (0..height).rev().find(|&y| alpha_at(cx, y) > ALPHA_THRESHOLD) {
            bottom = bottom.max(b);
        }
    }
    let mut left = u32::MAX;
    let mut right = 0u32;
    let mut found_h = false;
    for i in 1u32..=7 {
        let cy = (height.saturating_mul(i)) / 8;
        if let Some(l) = (0..width).find(|&x| alpha_at(x, cy) > ALPHA_THRESHOLD) {
            left = left.min(l);
            found_h = true;
        }
        if let Some(r) = (0..width).rev().find(|&x| alpha_at(x, cy) > ALPHA_THRESHOLD) {
            right = right.max(r);
        }
    }
    if !found_v || !found_h || bottom < top || right < left {
        return None;
    }
    Some((
        left as i32, top as i32,
        (right - left + 1) as i32,
        (bottom - top + 1) as i32,
    ))
}

/// Imports/composites the surface tree. Returns the count of surfaces that
/// were composited — callers use `> 1` as a heuristic CSD signal (apps with
/// subsurfaces nearly always paint their own chrome and shouldn't get our
/// SSD titlebar on top).
pub fn import_shm_buffer(surface: &WlSurface, pixels_out: &Arc<Mutex<ClientSurfaceData>>) -> usize {
    use smithay::backend::renderer::utils::with_renderer_surface_state;
    use smithay::reexports::wayland_server::protocol::wl_shm;
    use smithay::wayland::compositor::{
        with_surface_tree_downward, SubsurfaceCachedState, TraversalAction,
    };
    use smithay::wayland::shm::with_buffer_contents;

    // ── Pass 1 — walk the tree to collect (surface, offset). DON'T call
    //              with_renderer_surface_state from inside the walk: smithay's
    //              tree traversal already holds surface-state locks and a
    //              nested borrow deadlocks the wayland thread. We read each
    //              surface's buffer dims later in a separate pass.
    #[derive(Clone, Copy)]
    struct Node {
        offset_x: i32,
        offset_y: i32,
        width:    i32,
        height:   i32,
    }
    let mut surfaces_and_offsets: Vec<(WlSurface, (i32, i32))> = Vec::new();

    with_surface_tree_downward(
        surface,
        (0i32, 0i32),
        |sub, states, parent_offset| {
            let mut my_offset = *parent_offset;
            if sub != surface {
                let mut sub_state = states.cached_state.get::<SubsurfaceCachedState>();
                let loc = sub_state.current().location;
                my_offset.0 += loc.x;
                my_offset.1 += loc.y;
            }
            surfaces_and_offsets.push((sub.clone(), my_offset));
            TraversalAction::DoChildren(my_offset)
        },
        |_, _, _| {},
        |_, _, _| true,
    );

    // Resolve buffer dims now that we're out of the surface-tree closure.
    let nodes: Vec<(WlSurface, Node)> = surfaces_and_offsets.into_iter()
        .map(|(s, off)| {
            let (w, h) = with_renderer_surface_state(&s, |st| {
                st.buffer_size().map(|sz| (sz.w, sz.h)).unwrap_or((0, 0))
            }).unwrap_or((0, 0));
            (s, Node { offset_x: off.0, offset_y: off.1, width: w, height: h })
        })
        .collect();

    // ── Compute union bounds. If the root has its own buffer, its dims
    //     anchor (0,0,W,H); otherwise we use the union of children.
    let root_dims = nodes.first().map(|(_, n)| *n).unwrap_or(Node {
        offset_x: 0, offset_y: 0, width: 0, height: 0,
    });
    let mut min_x = 0i32;
    let mut min_y = 0i32;
    let mut max_x = root_dims.width.max(0);
    let mut max_y = root_dims.height.max(0);
    for (_, n) in &nodes {
        if n.width <= 0 || n.height <= 0 { continue }
        min_x = min_x.min(n.offset_x);
        min_y = min_y.min(n.offset_y);
        max_x = max_x.max(n.offset_x + n.width);
        max_y = max_y.max(n.offset_y + n.height);
    }
    let comp_w = (max_x - min_x).max(1) as u32;
    let comp_h = (max_y - min_y).max(1) as u32;
    let comp_len = (comp_w as usize) * (comp_h as usize) * 4;
    let n_subs = nodes.len();

    // ── Reuse the destination Vec instead of allocating fresh each commit.
    //     Firefox at 1224×824 reallocates ~4 MB per commit at 60 Hz which
    //     was the main allocator/cache-miss source of the slow refresh.
    let mut out_guard = pixels_out.lock().unwrap();
    let composite: &mut Vec<u8> = &mut out_guard.pixels;
    if composite.len() != comp_len {
        composite.clear();
        composite.resize(comp_len, 0);
    } else {
        // Same size — just clear in-place (one memset, no realloc).
        for b in composite.iter_mut() { *b = 0; }
    }

    // ── Pass 2 — blit each surface's buffer into the composite.
    let mut any_pixels = false;
    let single_surface = n_subs == 1;
    for (sub, node) in &nodes {
        if node.width <= 0 || node.height <= 0 { continue }

        let buf = match with_renderer_surface_state(sub, |s| s.buffer().cloned()) {
            Some(Some(b)) => b,
            _ => continue,
        };

        let _ = with_buffer_contents(&*buf, |ptr: *const u8, len: usize, spec| {
            let src_w = spec.width  as i32;
            let src_h = spec.height as i32;
            let stride = spec.stride as usize;
            let has_alpha = matches!(spec.format, wl_shm::Format::Argb8888);
            let data = unsafe { std::slice::from_raw_parts(ptr, len) };

            let ox = node.offset_x - min_x;
            let oy = node.offset_y - min_y;

            // ── Compute the per-row clipped X range once, outside the loop.
            let dst_x_start = (ox.max(0)) as usize;
            let dst_x_end   = ((ox + src_w).min(comp_w as i32)) as usize;
            if dst_x_end <= dst_x_start { return; }
            let src_x_start = (dst_x_start as i32 - ox) as usize;
            let row_pixels  = dst_x_end - dst_x_start;
            let comp_row    = comp_w as usize * 4;

            // ── Fast path: single-surface tree with no horizontal/vertical
            //     padding and an opaque XRGB8888 buffer → bulk row memcpy
            //     with B↔R swap. This is the common case for SSD apps like
            //     kitty/foot and skips the per-pixel float math entirely.
            if single_surface && !has_alpha && ox == 0 && oy == 0
                && row_pixels == comp_w as usize
            {
                for y in 0..src_h {
                    if y >= comp_h as i32 { break; }
                    let s_row = (y as usize) * stride;
                    let d_row = (y as usize) * comp_row;
                    if s_row + row_pixels * 4 > data.len() { break; }
                    let src = &data[s_row .. s_row + row_pixels * 4];
                    let dst = &mut composite[d_row .. d_row + row_pixels * 4];
                    // BGRA → premultiplied RGBA (alpha=255 → no premul math).
                    for px in 0..row_pixels {
                        let i = px * 4;
                        dst[i]     = src[i + 2];
                        dst[i + 1] = src[i + 1];
                        dst[i + 2] = src[i];
                        dst[i + 3] = 255;
                    }
                }
                any_pixels = true;
                return;
            }

            // ── General path: per-row source-over compositing.
            for y in 0..src_h {
                let dy = oy + y;
                if dy < 0 || dy >= comp_h as i32 { continue; }
                let s_row = (y as usize) * stride + src_x_start * 4;
                let d_row = (dy as usize) * comp_row + dst_x_start * 4;
                if s_row + row_pixels * 4 > data.len() { continue; }
                let src = &data[s_row .. s_row + row_pixels * 4];
                let dst = &mut composite[d_row .. d_row + row_pixels * 4];
                for px in 0..row_pixels {
                    let si = px * 4;
                    let b = src[si];
                    let g = src[si + 1];
                    let r = src[si + 2];
                    let a = if has_alpha { src[si + 3] } else { 255u8 };
                    if a == 0 { continue; }
                    let di = px * 4;
                    if a == 255 {
                        // Fully opaque — direct overwrite, no float math.
                        dst[di]     = r;
                        dst[di + 1] = g;
                        dst[di + 2] = b;
                        dst[di + 3] = 255;
                    } else {
                        let af = a as f32 * (1.0 / 255.0);
                        let inv = 1.0 - af;
                        let pr = (r as f32 * af) as u8;
                        let pg = (g as f32 * af) as u8;
                        let pb = (b as f32 * af) as u8;
                        dst[di]     = pr.saturating_add((dst[di]     as f32 * inv) as u8);
                        dst[di + 1] = pg.saturating_add((dst[di + 1] as f32 * inv) as u8);
                        dst[di + 2] = pb.saturating_add((dst[di + 2] as f32 * inv) as u8);
                        dst[di + 3] = a.saturating_add((dst[di + 3] as f32 * inv) as u8);
                    }
                    any_pixels = true;
                }
            }
        });
    }

    if !any_pixels && n_subs <= 1 {
        // Single-surface tree with no pixels — nothing to do; leave the old
        // buffer alone instead of overwriting with a blank one.
        // (The Vec clear above touched it, so refill from old metadata? No —
        // the caller relies on width staying 0 for "nothing committed yet"
        // detection. Since we only get here with no opaque pixels at all,
        // mark the buffer as zero-sized.)
        out_guard.width = 0;
        out_guard.height = 0;
        return n_subs;
    }

    if n_subs > 1 {
        // Quieter than info! at the per-frame rate Firefox hits.
        debug!("composited surface tree: {} surfaces, {}×{}", n_subs, comp_w, comp_h);
    }

    out_guard.width  = comp_w;
    out_guard.height = comp_h;
    out_guard.dirty  = true;
    n_subs
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
        dmabuf: Dmabuf,
        notifier: ImportNotifier,
    ) {
        // We can only validate the DMA-BUF here — the client hasn't attached
        // it to any wl_surface yet, so we cannot store pixels keyed by
        // surface. The actual GLES readback happens at commit time once we
        // know the destination surface (see `import_dmabuf_for_surface`).
        self.ensure_gles_renderer();

        let renderer = match self.gles_renderer.as_mut() {
            Some(r) => r,
            None => {
                warn!(
                    planes = dmabuf.num_planes(),
                    "DMA-BUF: no GLES renderer available — rejecting buffer"
                );
                drop(notifier);
                return;
            }
        };

        match renderer.import_dmabuf(&dmabuf, None) {
            Ok(_) => {
                let _ = notifier.successful::<SpikeState>();
            }
            Err(e) => {
                warn!("DMA-BUF: GLES import failed ({e:?}) — client should fall back to SHM");
                drop(notifier);
            }
        }
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

    fn focus_changed(&mut self, seat: &Seat<Self>, focused: Option<&Self::KeyboardFocus>) {
        // Anvil pattern: route the new keyboard focus's owning client into the
        // selection sub-states so wl_data_device.selection /
        // primary_selection.selection events fire on focus transitions.
        // Without this, copy-paste between apps "works" only by accident
        // (the previous focus's offer leaks until something else clears it).
        let dh = &self.display_handle;
        let client = focused.and_then(|s| dh.get_client(s.id()).ok());
        set_data_device_focus(dh, seat, client.clone());
        set_primary_focus(dh, seat, client);
    }

    fn cursor_image(&mut self, _seat: &Seat<Self>, image: CursorImageStatus) {
        // Honour `wl_pointer.set_cursor` — primarily so clients that hide
        // their cursor (e.g. video players in fullscreen, drawing apps) get
        // the cursor to disappear. Surface cursors aren't rendered yet (TODO);
        // we treat them as "default" so the user still sees something.
        self.cursor_status = image;
    }
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

impl WaylandDndGrabHandler for SpikeState {
    fn dnd_requested<S: Source>(
        &mut self,
        source: S,
        icon: Option<WlSurface>,
        seat: Seat<Self>,
        serial: Serial,
        type_: GrabType,
    ) {
        // Capture the icon so the renderer can composite it under the cursor
        // while the drag is active. Cleared in `DndGrabHandler::dropped` and
        // `cancelled`. (Anvil tracks an offset alongside the surface; we
        // don't have a hotspot story yet so origin-anchor is fine.)
        self.dnd_icon = icon;

        match type_ {
            GrabType::Pointer => {
                let Some(pointer) = seat.get_pointer() else { return };
                let Some(start_data) = pointer.grab_start_data() else { return };
                pointer.set_grab(
                    self,
                    DnDGrab::new_pointer(&self.display_handle, start_data, source, seat),
                    serial,
                    Focus::Keep,
                );
            }
            GrabType::Touch => {
                let Some(touch) = seat.get_touch() else { return };
                let Some(start_data) = touch.grab_start_data() else { return };
                touch.set_grab(
                    self,
                    DnDGrab::new_touch(&self.display_handle, start_data, source, seat),
                    serial,
                );
            }
        }
    }
}

impl DndGrabHandler for SpikeState {
    fn dropped(
        &mut self,
        _target: Option<DndTarget<'_, Self>>,
        _validated: bool,
        _seat: Seat<Self>,
        _location: Point<f64, Logical>,
    ) {
        self.dnd_icon = None;
    }

    fn cancelled(&mut self, _seat: Seat<Self>, _location: Point<f64, Logical>) {
        self.dnd_icon = None;
    }
}

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

// ──────────────────────────────────────────────────────────────────────────────
// Agent block: wlr-data-control + xdg-output + preferred_buffer_scale wiring.
// xdg-output globals are advertised by `OutputManagerState::new_with_xdg_output`
// + `delegate_output!` (in `wayland/outputs.rs`); no separate delegate exists
// because the same `delegate_output!` macro registers the xdg-output dispatch.
// ──────────────────────────────────────────────────────────────────────────────

impl DataControlHandler for SpikeState {
    fn data_control_state(&mut self) -> &mut DataControlState {
        &mut self.data_control_state
    }
}

smithay::delegate_data_control!(SpikeState);

// ──────────────────────────────────────────────────────────────────────────────
// ext-image-capture-source-v1 + ext-image-copy-capture-v1
//
// These three globals are advertised in the constructor; the handlers below
// are minimal stubs:
//   - source_destroyed / new_session do nothing (we don't track sessions yet).
//   - output_source_created stores a WeakOutput in the source's user data so
//     `capture_constraints` can recover the output's mode.
//   - capture_constraints reports the current output's pixel size + ARGB/XRGB
//     SHM formats so well-behaved clients see a "compatible" path.
//   - frame() fails immediately with `Unknown` — we don't have a wgpu
//     readback path yet. This satisfies the protocol contract: the client
//     gets a clean failure instead of a hang or a missing-global error.
//     TODO(renderer): implement actual readback by hooking into the
//     post-present submit on the wgpu queue and copying the output texture
//     into the client-supplied wl_buffer (SHM today, dmabuf later).
// ──────────────────────────────────────────────────────────────────────────────

impl ImageCaptureSourceHandler for SpikeState {
    fn source_destroyed(&mut self, _source: ImageCaptureSource) {}
}

smithay::delegate_image_capture_source!(SpikeState);

impl OutputCaptureSourceHandler for SpikeState {
    fn output_capture_source_state(&mut self) -> &mut OutputCaptureSourceState {
        &mut self.output_capture_source_state
    }

    fn output_source_created(&mut self, source: ImageCaptureSource, output: &Output) {
        source.user_data().insert_if_missing(|| output.downgrade());
    }
}

smithay::delegate_output_capture_source!(SpikeState);

impl ImageCopyCaptureHandler for SpikeState {
    fn image_copy_capture_state(&mut self) -> &mut ImageCopyCaptureState {
        &mut self.image_copy_capture_state
    }

    fn capture_constraints(&mut self, source: &ImageCaptureSource) -> Option<BufferConstraints> {
        use smithay::output::WeakOutput;
        let weak_output = source.user_data().get::<WeakOutput>()?;
        let output = weak_output.upgrade()?;
        let mode = output.current_mode()?;
        Some(BufferConstraints {
            size: mode
                .size
                .to_logical(1)
                .to_buffer(1, smithay::utils::Transform::Normal),
            shm: vec![wl_shm::Format::Argb8888, wl_shm::Format::Xrgb8888],
            dma: None,
        })
    }

    fn new_session(&mut self, _session: Session) {}

    fn frame(&mut self, _session: &SessionRef, frame: Frame) {
        // Stub: framebuffer readback isn't wired yet, fail gracefully so
        // clients see a defined error rather than a hung capture.
        frame.fail(smithay::wayland::image_copy_capture::CaptureFailureReason::Unknown);
    }
}

smithay::delegate_image_copy_capture!(SpikeState);
