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
        egl::{EGLContext, EGLDevice, EGLDisplay},
        renderer::{gles::GlesRenderer, ExportMem, ImportDma},
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
            DisplayHandle, Resource,
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
        dmabuf::{DmabufFeedbackBuilder, DmabufGlobal, DmabufHandler, DmabufState, ImportNotifier},
        fifo::FifoManagerState,
        foreign_toplevel_list::{ForeignToplevelHandle, ForeignToplevelListState},
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
            ext_data_control::{
                DataControlHandler as ExtDataControlHandler,
                DataControlState as ExtDataControlState,
            },
            primary_selection::{
                set_primary_focus, PrimarySelectionHandler, PrimarySelectionState,
            },
            wlr_data_control::{DataControlHandler, DataControlState},
            SelectionHandler, SelectionSource, SelectionTarget,
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
        xwayland_keyboard_grab::XWaylandKeyboardGrabState,
        xwayland_shell::XWaylandShellState,
    },
    xwayland::X11Wm,
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
    /// `Some` when this client connected via a `wp_security_context_v1`
    /// listener — Flatpak / Snap / Bubblewrap clients carry one to identify
    /// their sandbox. Filters like `client_has_no_security_context` use
    /// this to deny privileged globals (screencopy, data_control,
    /// session_lock) to sandboxed apps.
    pub security_context: Option<smithay::wayland::security_context::SecurityContext>,
}

impl ClientData for ClientState {
    fn initialized(&self, _client_id: ClientId) {
        debug!("wayland client connected");
    }
    fn disconnected(&self, _client_id: ClientId, _reason: DisconnectReason) {
        info!("wayland client disconnected");
    }
}

/// Filter passed to privileged globals (data_control, image_copy_capture,
/// session_lock, ...). Returns `true` when the client has no security
/// context attached — i.e. it's a normal client, not a sandboxed Flatpak.
/// Mirrors cosmic's `client_has_no_security_context`.
pub fn client_has_no_security_context(client: &smithay::reexports::wayland_server::Client) -> bool {
    client
        .get_data::<ClientState>()
        .is_none_or(|d| d.security_context.is_none())
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
    /// Incremented every time the buffer is rewritten (alongside `dirty`).
    /// Image caches keyed by surface use this to detect "is the cached
    /// `slint::Image` still based on the current pixels?" without keeping
    /// the buffer mutex locked between updates.
    pub version: u64,
    /// `RendererSurfaceState::current_commit()` snapshot taken at the
    /// time this entry was last refreshed. The per-surface SHM importer
    /// short-circuits when the surface's current commit equals this —
    /// nothing has changed, so the BGRA→RGBA conversion (and `version`
    /// bump and downstream slint::Image rebuild) can be skipped. Damage
    /// tracking in its simplest form.
    pub last_commit: Option<smithay::backend::renderer::utils::CommitCounter>,
}

// ──────────────────────────────────────────────────────────────────────────────
// Per-toplevel window info (multi-window bookkeeping)
// ──────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ToplevelInfo {
    /// The wayland surface for this toplevel.
    pub surface: WlSurface,
    /// The xdg `ToplevelSurface` handle for native wayland clients. `None`
    /// for X11/Xwayland toplevels — those use `x11_surface` for configure /
    /// close round-trips instead.
    pub toplevel: Option<ToplevelSurface>,
    /// `Some(_)` when this toplevel is backed by an X11 window (i.e. the
    /// client is an Xwayland app). Mutually exclusive with `toplevel`.
    pub x11_surface: Option<smithay::xwayland::X11Surface>,
    /// Cascaded compositor-space position.
    pub x: i32,
    pub y: i32,
    /// Legacy composite pixel buffer — the toplevel + all subsurfaces
    /// CPU-blitted into one premultiplied RGBA buffer by
    /// `import_shm_buffer`'s tree walk. This is the OLD path; per-surface
    /// rendering uses `surface_pixels` below.
    pub pixels: Arc<Mutex<ClientSurfaceData>>,
    /// Per-`wl_surface` pixel buffers for the new per-surface render-element
    /// model. Keyed by `wl_surface.id().protocol_id()` (the toplevel's own
    /// surface ID is included alongside any subsurfaces). Subsurface
    /// position / src-crop / dst-size are resolved separately in
    /// `renderer::update_windows` from `surface_view.{offset,src,dst}`.
    /// Each entry is a single surface's BUFFER pixels (no compositing).
    /// During the staged rewrite this populates alongside `pixels`; once
    /// all readers move over, the legacy composite is removed.
    pub surface_pixels: Arc<Mutex<std::collections::HashMap<u32, ClientSurfaceData>>>,
    /// Client requested ClientSide decorations (CSD). False = SSD; we draw
    /// our own titlebar above the client surface.
    pub csd: bool,
    /// `ext-foreign-toplevel-list-v1` handle. Created when the toplevel is
    /// added to `SpikeState::toplevels`; used to keep external taskbars/docks
    /// (waybar, fuzzel, lavalauncher) in sync with our window list. Updated
    /// on title/app_id changes in `renderer::update_windows`; removed in the
    /// `toplevel_destroyed` / `unmapped` paths.
    pub foreign_handle: Option<ForeignToplevelHandle>,
    /// Last `(title, app_id)` we sent to the foreign-toplevel handle, so the
    /// per-frame sync in `renderer::update_windows` only fires events on
    /// real change. Avoids spamming `send_title`/`send_done` 60×/sec.
    pub last_advertised_title: String,
    pub last_advertised_app_id: String,
    /// True when the client tagged this toplevel as an `xdg-dialog-v1`
    /// dialog/modal. Used by the WM for centred placement, parent attach,
    /// and to hide the minimise button on the SSD chrome.
    pub is_dialog: bool,
    /// Themed icon name set via `xdg-toplevel-icon-v1`. None when the client
    /// hasn't called `set_icon` or set the icon by buffer only. Used by the
    /// dock / taskbar to render a per-window app icon.
    pub icon_name: Option<String>,
    /// `xdg-toplevel-tag-v1` tag + description. Tag is a stable per-window
    /// identifier the client uses for window-session restore; description
    /// is human-readable. Stored for future session-management consumers.
    pub tag: Option<String>,
    pub description: Option<String>,
    /// `(dbus_service_name, dbus_object_path)` of this window's exported
    /// `com.canonical.dbusmenu` — set via `org_kde_kwin_appmenu` (Wayland)
    /// or the `com.canonical.AppMenu.Registrar` D-Bus service (X11/legacy).
    /// `None` when the app exports no global menu (most GTK4/GNOME apps).
    /// The panel renders this window's menu when it has keyboard focus.
    pub appmenu: Option<(String, String)>,
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
    /// Composited pixel buffer from the popup's surface tree (legacy
    /// path; kept for backdrop/screencopy parity with toplevels).
    pub pixels: Arc<Mutex<ClientSurfaceData>>,
    /// Per-`wl_surface` pixel buffers for the per-surface render-element
    /// model (same shape as `ToplevelInfo.surface_pixels`). Populated on
    /// commit by `import_shm_per_surface`. The renderer emits one
    /// `SurfaceItem` per entry so libadwaita-style popovers with
    /// animated subsurfaces render correctly.
    pub surface_pixels: Arc<Mutex<std::collections::HashMap<u32, ClientSurfaceData>>>,
    /// `xdg_surface.set_window_geometry` rect — the VISIBLE menu within the
    /// buffer. Firefox / Chromium / Electron paint a drop-shadow gutter in
    /// the buffer; without honouring this rect we'd render the gutter too
    /// (manifest: "large border around the context menu"). Zero means
    /// "client hasn't set one, treat the whole buffer as visible".
    pub geom_x: i32,
    pub geom_y: i32,
    pub geom_w: i32,
    pub geom_h: i32,
}

/// Active drag-and-drop icon surface paired with the accumulated buffer
/// offset (the hotspot, in logical pixels). Each commit on the icon
/// surface adds the freshly-set `wl_surface.offset` (a.k.a. buffer_delta)
/// to `offset`; the renderer subtracts it from the cursor position so
/// the client-declared hotspot lands exactly on the pointer.
#[derive(Debug, Clone)]
pub struct DndIcon {
    pub surface: WlSurface,
    pub offset: smithay::utils::Point<i32, smithay::utils::Logical>,
}

/// One ext-session-lock-v1 surface (one per output the locker covers).
/// Stored on `SpikeState::lock_surfaces`; the renderer composites these
/// fullscreen and skips everything else while a lock is active.
#[derive(Clone)]
pub struct LockSurfaceInfo {
    pub surface: smithay::wayland::session_lock::LockSurface,
    /// The wl_output this lock surface is bound to. Stored so the per-output
    /// multi-output story has the handle when it lands; currently read only
    /// by session_lock.rs at construction time.
    #[allow(dead_code)]
    pub output: smithay::reexports::wayland_server::protocol::wl_output::WlOutput,
    pub pixels: Arc<Mutex<ClientSurfaceData>>,
}

// ──────────────────────────────────────────────────────────────────────────────
// Compositor state
// ──────────────────────────────────────────────────────────────────────────────

// Many of the smithay state-holder fields below (XdgDecorationState,
// FractionalScaleManagerState, ContentTypeState, etc.) exist only to keep
// their globals + dispatch tables alive — the actual protocol handling
// runs through `delegate_*!` macro-generated paths that the compiler's
// dead-code pass doesn't traverse. Silence the false-positive at the
// struct level so the warning list stays usable for things that actually
// matter.
#[allow(dead_code)]
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
    pub ext_data_control_state: ExtDataControlState,

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
    /// Surfaces with an active idle-inhibit-v1 inhibitor. We don't try
    /// to gate on visibility (the spec allows ignoring) — any active
    /// inhibitor flips `idle_notifier.set_is_inhibited(true)` so video
    /// players, presentations, etc. keep the screen-saver away.
    pub idle_inhibitors: std::collections::HashSet<WlSurface>,
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
    pub xdg_dialog_state: smithay::wayland::shell::xdg::dialog::XdgDialogState,
    /// smithay's PopupManager handles popup grab + chain dismissal. Used in
    /// addition to our own `popups: Vec<PopupInfo>` (which holds pixel
    /// buffers for compositing). PopupManager doesn't need pixels — just
    /// the popup hierarchy + grab state.
    pub popup_manager: smithay::desktop::PopupManager,
    /// `org_kde_kwin_appmenu_manager` global — KDE-style global menus.
    pub appmenu_manager_state: crate::wayland::appmenu::AppmenuManagerState,
    /// Appmenu addresses set via `org_kde_kwin_appmenu::set_address` before
    /// the owning toplevel was tracked. Drained into `ToplevelInfo.appmenu`
    /// when the toplevel maps. Keyed by the surface's ObjectId.
    pub pending_appmenu: std::collections::HashMap<ObjectId, (String, String)>,
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
    /// and frames. The actual readback runs from the renderer after each
    /// `render_frame`, draining `pending_capture_frames` below.
    pub image_copy_capture_state: ImageCopyCaptureState,
    /// Active `ext-image-copy-capture-v1` sessions. Stored so we can re-emit
    /// buffer constraints on output mode changes and so the frame readback
    /// path can iterate them. Dead sessions are swept by `cleanup_capture_sessions`.
    pub capture_sessions: Vec<Session>,
    /// Frames whose capture has been requested but not yet serviced. The
    /// `frame()` handler enqueues them (instead of doing readback inline,
    /// which would need the renderer's wgpu device + final_tex); the main
    /// loop drains them after `render_frame` and performs a sync GPU
    /// readback into each frame's wl_buffer.
    pub pending_capture_frames: Vec<Frame>,

    // ── XWayland ─────────────────────────────────────────────────────────
    /// The xwayland_shell_v1 global state — needed for the Xwayland process to
    /// associate a wl_surface with the X11 window it represents.
    pub xwayland_shell_state: XWaylandShellState,
    /// The X11 window manager attached to the running Xwayland instance.
    /// `None` until `XWaylandEvent::Ready` fires (or after Xwayland exits).
    pub xwm: Option<X11Wm>,
    /// X11 display number Xwayland is listening on (for `DISPLAY=:N`).
    pub xdisplay: Option<u32>,

    // ── Session lock (ext-session-lock-v1) ────────────────────────────────
    /// `Some(_)` between `lock()` and confirmation. Held until the first
    /// lock surface commits a buffer; we then take it and call `.lock()`
    /// to flip the protocol to "locked" so the client knows to render.
    pub pending_session_lock: Option<smithay::wayland::session_lock::SessionLocker>,
    /// True while a lock is active. Drives the renderer's gate (skip all
    /// non-lock content) and the input gate (drop pointer/keyboard
    /// events for non-lock surfaces).
    pub session_locked: bool,
    /// Per-output lock surface + its committed pixel buffer.
    pub lock_surfaces: Vec<LockSurfaceInfo>,

    // ── Bookkeeping ───────────────────────────────────────────────────────
    /// Transitional primary output for the current single-output winit path.
    /// New backend work should use `outputs` and `primary_output()` instead of
    /// reaching for this field directly.
    pub output: Option<Output>,
    /// All registered compositor outputs. The winit backend currently inserts
    /// one virtual output; the udev backend will populate this from DRM
    /// connectors and keep `output` as the primary/output-0 compatibility shim.
    pub outputs: Vec<Output>,

    /// Currently-focused xdg toplevel surface.
    pub active_surface: Option<WlSurface>,

    /// Drag-and-drop icon surface for the active client-initiated DnD grab.
    /// Set by `WaylandDndGrabHandler::dnd_requested` when the client supplies
    /// an icon, cleared in `DndGrabHandler::dropped` / `cancelled`. The
    /// renderer composites this surface under the cursor while a DnD is
    /// active.
    ///
    /// `offset` is the accumulated `wl_surface.offset` (== buffer_delta on
    /// commit) — clients use it as the icon's hotspot relative to its
    /// top-left, so we subtract it from the icon's draw position so the
    /// hotspot lands exactly on the cursor.
    pub dnd_icon: Option<DndIcon>,

    /// Composited pixel buffer for the active DnD icon surface. Populated by
    /// the commit handler each time the icon surface commits (matches the
    /// `cursor_surface_pixels` pattern). The renderer reads this in
    /// `update_windows` and blits it onto `final_tex` under the cursor.
    pub dnd_icon_pixels: Arc<Mutex<ClientSurfaceData>>,

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
    /// Symmetric to `pending_xdg_minimize` but for restore — pushed by the
    /// XwmHandler `unminimize_request`. xdg-shell has no client-driven
    /// unminimize so this queue is X11-only.
    pub pending_xdg_restore: Vec<WlSurface>,
    /// Set by `xdg-system-bell-v1::ring` — the renderer's per-frame tick
    /// drains it and starts a brief flash animation on the targeted
    /// window's chrome. `None` between bells.
    pub pending_bell: Option<WlSurface>,

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

        // wl_compositor v6 (vs default v5) is required for clients to use
        // the v6 `set_buffer_scale` / `set_buffer_transform` requests and
        // to receive `wl_surface.preferred_buffer_scale` /
        // `preferred_buffer_transform` events. HiDPI clients (Chromium,
        // Firefox, GNOME apps) gate fractional/integer scale rendering on
        // these — without v6 they render at scale 1 even when we'd like
        // them at 2x.
        let compositor_state = CompositorState::new_v6::<Self>(dh);
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
        // Privileged-global filter: deny sandboxed clients access to the
        // clipboard manager + screencopy + session lock + image-capture.
        // Without this a Flatpak'd browser could harvest your clipboard.
        let data_control_state = DataControlState::new::<Self, _>(
            dh,
            Some(&primary_selection_state),
            client_has_no_security_context,
        );
        // ext-data-control-v1 (the standardised successor to wlr-data-control).
        // Newer clipboard managers (wl-clipboard 2.2+, cliphist 0.7+) prefer
        // this; we keep wlr too for compat with older tools.
        let ext_data_control_state = ExtDataControlState::new::<Self, _>(
            dh,
            Some(&primary_selection_state),
            client_has_no_security_context,
        );

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
        // input-method and virtual-keyboard let clients inject keystrokes —
        // privilege gate: a sandboxed Flatpak that grabs virtual-keyboard
        // could synthesize keys into other apps. Filter by security context.
        let input_method_state =
            InputMethodManagerState::new::<Self, _>(dh, client_has_no_security_context);
        let virtual_keyboard_state =
            VirtualKeyboardManagerState::new::<Self, _>(dh, client_has_no_security_context);

        let idle_notifier_state = IdleNotifierState::<Self>::new(dh, loop_handle.clone());
        let idle_inhibit_manager_state = IdleInhibitManagerState::new::<Self>(dh);
        // session-lock is privileged: only the trusted lockscreen process
        // should be able to bind it. Filter mirrors cosmic-comp.
        let session_lock_manager_state =
            SessionLockManagerState::new::<Self, _>(dh, client_has_no_security_context);

        let activation_state = XdgActivationState::new::<Self>(dh);
        let content_type_state = ContentTypeState::new::<Self>(dh);
        let alpha_modifier_state = AlphaModifierState::new::<Self>(dh);
        let xdg_foreign_state = XdgForeignState::new::<Self>(dh);
        // ext-foreign-toplevel-list-v1 lets clients enumerate every open
        // window across the desktop — a meaningful info-disclosure surface
        // for a sandboxed app. Filter sandboxed clients.
        let foreign_toplevel_list_state =
            ForeignToplevelListState::new_with_filter::<Self>(dh, client_has_no_security_context);
        let security_context_state = SecurityContextState::new::<Self, _>(dh, |_| true);
        let xdg_system_bell_state = XdgSystemBellState::new::<Self>(dh);
        let xdg_toplevel_icon_manager = XdgToplevelIconManager::new::<Self>(dh);
        // xdg-dialog-v1 — clients (file pickers, About boxes) tag toplevels
        // as dialog so the compositor centres/parent-attaches them. Without
        // the global created, the protocol was advertised by the delegate
        // macro alone, which dispatches but the global was never broadcast.
        let xdg_dialog_state =
            smithay::wayland::shell::xdg::dialog::XdgDialogState::new::<Self>(dh);
        let popup_manager = smithay::desktop::PopupManager::default();
        let appmenu_manager_state = crate::wayland::appmenu::AppmenuManagerState::new(dh);
        let xdg_toplevel_tag_manager = XdgToplevelTagManager::new::<Self>(dh);

        let image_capture_source_state = ImageCaptureSourceState::new();
        // image-capture / image-copy-capture are screen-recording globals —
        // a sandboxed app shouldn't be able to read the screen without the
        // portal granting it. Filter on security context.
        let output_capture_source_state = OutputCaptureSourceState::new_with_filter::<Self, _>(
            dh,
            client_has_no_security_context,
        );
        let image_copy_capture_state =
            ImageCopyCaptureState::new_with_filter::<Self, _>(dh, client_has_no_security_context);

        let xwayland_shell_state = XWaylandShellState::new::<Self>(dh);
        XWaylandKeyboardGrabState::new::<Self>(dh);

        // wl_fixes — lets clients destroy a wl_registry properly without
        // leaking the resource on our side. Stateless; create the global
        // and let the macro-generated dispatch handle the rest.
        smithay::wayland::fixes::FixesState::new::<Self>(dh);

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
            ext_data_control_state,
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
            idle_inhibitors: std::collections::HashSet::new(),
            session_lock_manager_state,
            activation_state,
            content_type_state,
            alpha_modifier_state,
            xdg_foreign_state,
            foreign_toplevel_list_state,
            security_context_state,
            xdg_system_bell_state,
            xdg_toplevel_icon_manager,
            xdg_dialog_state,
            popup_manager,
            appmenu_manager_state,
            pending_appmenu: std::collections::HashMap::new(),
            xdg_toplevel_tag_manager,
            image_capture_source_state,
            output_capture_source_state,
            image_copy_capture_state,
            capture_sessions: Vec::new(),
            pending_capture_frames: Vec::new(),
            xwayland_shell_state,
            xwm: None,
            xdisplay: None,
            pending_session_lock: None,
            session_locked: false,
            lock_surfaces: Vec::new(),
            output: None,
            outputs: Vec::new(),
            active_surface: None,
            dnd_icon: None,
            dnd_icon_pixels: Arc::new(Mutex::new(ClientSurfaceData::default())),
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
            pending_xdg_restore: Vec::new(),
            pending_bell: None,
            should_exit: false,
            pointer_pos: (0.0, 0.0),
            cursor_status: CursorImageStatus::default_named(),
            cursor_surface_pixels: Arc::new(Mutex::new(ClientSurfaceData::default())),
            egl_display: None,
            gles_renderer: None,
            egl_init_tried: false,
            dmabuf_pending: std::collections::HashMap::new(),
        };

        // Pick up XKB config from the environment so users get the right
        // layout/variant/options without us shipping a config file yet
        // (cosmic-comp does this via its own settings layer; we use the
        // same XKB_DEFAULT_LAYOUT etc. environment variables xkbcommon
        // already honours by default). Repeat rate/delay from env if set,
        // otherwise sensible defaults that match GNOME (500ms delay, 33Hz).
        let xkb_config = smithay::input::keyboard::XkbConfig {
            rules: "",
            model: "",
            layout: "",
            variant: "",
            options: None,
        };
        let repeat_delay = std::env::var("DE_KB_REPEAT_DELAY_MS")
            .ok()
            .and_then(|v| v.parse::<i32>().ok())
            .unwrap_or(500);
        let repeat_rate = std::env::var("DE_KB_REPEAT_RATE_HZ")
            .ok()
            .and_then(|v| v.parse::<i32>().ok())
            .unwrap_or(33);
        state
            .seat
            .add_keyboard(xkb_config, repeat_delay, repeat_rate)
            .ok();
        state.seat.add_pointer();
        // Advertise touch capability on the seat (matches cosmic-comp seats.rs:234).
        // Touch event dispatch is plumbed separately in the backends.
        state.seat.add_touch();

        // Create the linux-dmabuf-v1 global. Without this, clients (Firefox,
        // GTK4, Qt6, Chromium, mpv, OBS) fall back to wl_shm which is
        // ~30-50% more CPU under load. Pattern mirrors anvil/winit.rs:146-182.
        state.create_dmabuf_global();

        state
    }

    /// Re-run the xdg-popup positioner against the parent toplevel + output
    /// rect so the popup doesn't render off-screen. Mirrors
    /// anvil/shell/xdg.rs:556-589 simplified for our single-output model.
    pub fn unconstrain_popup(&self, popup: &smithay::wayland::shell::xdg::PopupSurface) {
        use smithay::desktop::{find_popup_root_surface, get_popup_toplevel_coords, PopupKind};
        let Ok(root) = find_popup_root_surface(&PopupKind::Xdg(popup.clone())) else {
            return;
        };
        let Some(tl) = self.toplevels.iter().find(|t| t.surface == root) else {
            return;
        };
        let Some(output) = self.primary_output() else {
            return;
        };
        let Some(mode) = output.current_mode() else {
            return;
        };
        let scale = output.current_scale().fractional_scale();
        let logical_w = (mode.size.w as f64 / scale).max(1.0) as i32;
        let logical_h = (mode.size.h as f64 / scale).max(1.0) as i32;
        // Positioner target rect = output, but expressed relative to the
        // parent toplevel's surface origin (the positioner anchors relative
        // to its parent).
        let mut target = smithay::utils::Rectangle::new(
            smithay::utils::Point::from((0, 0)),
            smithay::utils::Size::from((logical_w, logical_h)),
        );
        target.loc -= get_popup_toplevel_coords(&PopupKind::Xdg(popup.clone()));
        target.loc -= smithay::utils::Point::from((tl.x, tl.y));
        popup.with_pending_state(|state| {
            state.geometry = state.positioner.get_unconstrained_geometry(target);
        });
    }

    /// Validate that a client-supplied `serial` corresponds to a real
    /// pointer/touch grab on `surface`. Used to gate interactive move/resize
    /// requests so a malicious or buggy client can't unilaterally seize the
    /// pointer (xdg-shell spec recommends this; anvil/shell/xdg.rs:80-117
    /// is the reference impl).
    pub fn validate_grab_serial(
        &self,
        seat_resource: &smithay::reexports::wayland_server::protocol::wl_seat::WlSeat,
        surface: &WlSurface,
        serial: Serial,
    ) -> bool {
        let Some(seat) = Seat::<Self>::from_resource(seat_resource) else {
            return false;
        };
        // Pointer path: the grab must be active for this serial and the
        // pointer must currently be focused on a surface owned by the same
        // client as `surface`. The xdg-shell spec also accepts touch.
        // Our SeatHandler::PointerFocus / TouchFocus are both `WlSurface`,
        // so `start.focus` is `Option<(WlSurface, _)>` and we compare client
        // identity directly via ObjectId::same_client_as.
        if let Some(pointer) = seat.get_pointer() {
            if pointer.has_grab(serial) {
                if let Some(start) = pointer.grab_start_data() {
                    if let Some((focus_surface, _)) = start.focus.as_ref() {
                        if focus_surface.id().same_client_as(&surface.id()) {
                            return true;
                        }
                    }
                }
            }
        }
        if let Some(touch) = seat.get_touch() {
            if touch.has_grab(serial) {
                if let Some(start) = touch.grab_start_data() {
                    if let Some((focus_surface, _)) = start.focus.as_ref() {
                        if focus_surface.id().same_client_as(&surface.id()) {
                            return true;
                        }
                    }
                }
            }
        }
        debug!("xdg grab rejected: serial does not match an active pointer/touch grab");
        false
    }

    /// Push the latest `title`/`app_id` of every tracked toplevel to its
    /// `ext-foreign-toplevel-list-v1` handle, diff-gated so the per-frame
    /// call only fires protocol events on real change. Cheap when nothing
    /// changed: a `clone` + string compare per toplevel.
    pub fn sync_foreign_toplevels(&mut self) {
        use smithay::wayland::compositor::with_states;
        use smithay::wayland::shell::xdg::XdgToplevelSurfaceData;
        for tl in &mut self.toplevels {
            let Some(handle) = tl.foreign_handle.as_ref() else {
                continue;
            };
            let (title, app_id) = if let Some(x11) = &tl.x11_surface {
                // X11 windows expose these via X11Surface, not wl_surface data.
                (x11.title(), x11.class())
            } else {
                with_states(&tl.surface, |states| {
                    let Some(data) = states.data_map.get::<XdgToplevelSurfaceData>() else {
                        return (String::new(), String::new());
                    };
                    let Ok(guard) = data.lock() else {
                        return (String::new(), String::new());
                    };
                    (
                        guard.title.clone().unwrap_or_default(),
                        guard.app_id.clone().unwrap_or_default(),
                    )
                })
            };
            let mut changed = false;
            if title != tl.last_advertised_title {
                handle.send_title(&title);
                tl.last_advertised_title = title;
                changed = true;
            }
            if app_id != tl.last_advertised_app_id {
                handle.send_app_id(&app_id);
                tl.last_advertised_app_id = app_id;
                changed = true;
            }
            if changed {
                handle.send_done();
            }
        }
    }

    /// Create the `linux-dmabuf-v1` global. Prefers v4 (with per-render-node
    /// default feedback) when EGL can identify the render node; falls back to
    /// v3 (formats only) otherwise. No-op if EGL/GLES init failed — clients
    /// will use wl_shm.
    fn create_dmabuf_global(&mut self) {
        self.ensure_gles_renderer();

        let (dmabuf_formats, render_node) = match self.gles_renderer.as_ref() {
            Some(r) => {
                let formats: Vec<_> = r.dmabuf_formats().into_iter().collect();
                let node = EGLDevice::device_for_display(r.egl_context().display())
                    .ok()
                    .and_then(|d| d.try_get_render_node().ok().flatten());
                (formats, node)
            }
            None => {
                warn!(
                    "DMA-BUF: GLES renderer unavailable — global NOT advertised, clients will use SHM"
                );
                return;
            }
        };

        if dmabuf_formats.is_empty() {
            warn!("DMA-BUF: GLES exposed zero formats — global NOT advertised");
            return;
        }
        let n_formats = dmabuf_formats.len();
        let dh = self.display_handle.clone();

        if let Some(node) = render_node {
            match DmabufFeedbackBuilder::new(node.dev_id(), dmabuf_formats.clone()).build() {
                Ok(feedback) => {
                    let _ = self
                        .dmabuf_state
                        .create_global_with_default_feedback::<Self>(&dh, &feedback);
                    info!(
                        formats = n_formats,
                        node = ?node.dev_id(),
                        "DMA-BUF v4 global advertised (per-render-node feedback)"
                    );
                    return;
                }
                Err(e) => warn!("DMA-BUF: feedback build failed: {e:?} — falling back to v3"),
            }
        } else {
            warn!("DMA-BUF: no EGL render node — falling back to v3");
        }

        let _ = self.dmabuf_state.create_global::<Self>(&dh, dmabuf_formats);
        info!(formats = n_formats, "DMA-BUF v3 global advertised");
    }

    pub fn register_output(&mut self, output: Output) {
        if self.output.is_none() {
            self.output = Some(output.clone());
        }
        if !self.outputs.iter().any(|existing| existing == &output) {
            self.outputs.push(output);
        }
    }

    pub fn unregister_output(&mut self, output: &Output) {
        self.outputs.retain(|existing| existing != output);
        if self.output.as_ref() == Some(output) {
            self.output = self.outputs.first().cloned();
        }
    }

    pub fn primary_output(&self) -> Option<&Output> {
        self.output.as_ref().or_else(|| self.outputs.first())
    }

    /// Find the Output most likely "hosting" a surface, by checking which
    /// outputs the surface has entered (sent via `wl_surface.enter`).
    /// Returns `None` if the surface hasn't entered any of our outputs yet.
    /// Used by multi-output paths (fractional-scale, future per-output
    /// frame callbacks) so we don't blindly use the primary output's scale
    /// for surfaces living on a secondary monitor.
    pub fn output_for_surface(&self, surface: &WlSurface) -> Option<&Output> {
        use smithay::reexports::wayland_server::Resource;
        let client = surface.client()?;
        for out in &self.outputs {
            let entered: Vec<_> = out.client_outputs(&client).collect();
            if !entered.is_empty() {
                // Heuristic: any client_output for this client on this
                // Output means the client has wl_outputs bound — good
                // enough until we have real per-surface enter tracking.
                return Some(out);
            }
        }
        None
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
    /// Called from the commit handler. At that point the DMA-BUF read-fence
    /// pre-commit blocker installed in `CompositorHandler::new_surface` has
    /// released, so reading from the dmabuf is safe.
    pub fn import_dmabuf_for_surface(&mut self, surface: &WlSurface, dmabuf: &Dmabuf) {
        self.ensure_gles_renderer();

        let renderer = match self.gles_renderer.as_mut() {
            Some(r) => r,
            None => {
                warn!(
                    planes = dmabuf.num_planes(),
                    "DMA-BUF: no GLES renderer at commit"
                );
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

        let region = smithay::utils::Rectangle::new(
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

        // copy_texture with Abgr8888 returns bytes in memory order
        // [R, G, B, A] — what `slint::Image::from_rgba8_premultiplied`
        // expects.  The buffer's pixel data is already pre-multiplied
        // (wayland convention), so we don't multiply again.
        //
        // Alpha-channel correction for opaque-format buffers:
        // many wgpu/Vulkan WSI clients (eframe, Chromium with skia-vk,
        // firefox with webrender-vk) allocate XRGB8888 / XBGR8888 swap-
        // chain images — *no* alpha channel. The X bits are
        // *undefined* per spec; on radv they're whatever leftover bits
        // the driver leaves in memory, often a chequerboard of 0/0xFF
        // or random per-pixel noise. When we copy_texture into an ABGR
        // sink we read those X bits *as alpha*, and slint
        // alpha-blends with garbage — anti-aliased glyph edges,
        // strokes, and circle outlines silently drop out (see wing's
        // input-tracker: grid + solid rect_filled survive, text +
        // circle_filled vanish). The fix is to detect the opaque-
        // format case and force alpha = 0xFF before handing the
        // pixels to slint.
        use smithay::backend::allocator::Fourcc;
        let src_fourcc = dmabuf.format().code;
        let opaque_format = matches!(
            src_fourcc,
            Fourcc::Xrgb8888
                | Fourcc::Xbgr8888
                | Fourcc::Rgbx8888
                | Fourcc::Bgrx8888
                | Fourcc::Rgb888
                | Fourcc::Bgr888
        );
        let mut rgba_pm = raw;
        if opaque_format {
            for px in rgba_pm.chunks_exact_mut(4) {
                px[3] = 0xFF;
            }
            debug!(
                "DMA-BUF: forced alpha=0xFF on opaque source format {:?}",
                src_fourcc
            );
        }

        debug!(
            "DMA-BUF: imported {}x{} ({} planes) for surface {:?} → {} RGBA bytes",
            w,
            h,
            dmabuf.num_planes(),
            surface.id(),
            rgba_pm.len()
        );

        self.dmabuf_pending.insert(
            surface.id(),
            ClientSurfaceData {
                pixels: rgba_pm,
                width: w,
                height: h,
                dirty: true,
                version: 1,
                last_commit: None,
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
///
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
        if let Some(b) = (0..height)
            .rev()
            .find(|&y| alpha_at(cx, y) > ALPHA_THRESHOLD)
        {
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
        if let Some(r) = (0..width)
            .rev()
            .find(|&x| alpha_at(x, cy) > ALPHA_THRESHOLD)
        {
            right = right.max(r);
        }
    }
    if !found_v || !found_h || bottom < top || right < left {
        return None;
    }
    Some((
        left as i32,
        top as i32,
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
    use smithay::wayland::compositor::{with_surface_tree_downward, TraversalAction};
    use smithay::wayland::shm::with_buffer_contents;

    // ── Pass 1 — walk the tree to collect (surface, offset).
    //
    // Offsets accumulate via `surface_view.offset` instead of the raw
    // `SubsurfaceCachedState.location`. surface_view.offset is what
    // smithay's reference rendering / `under_from_surface_tree` use; it
    // composes:
    //   * subsurface position (`wl_subsurface.set_position`)
    //   * `wl_surface.offset(dx, dy)` buffer_delta (used by animated
    //     cursors, sprite-pages, parallax)
    //   * wp_viewporter dst-rect placement
    //   * buffer_scale / buffer_transform conversions
    // Reading only SubsurfaceCachedState.location was the same bug we
    // already fixed in the input path — animated subsurfaces would render
    // at the wrong place even though pointer events landed correctly.
    //
    // Don't call `with_renderer_surface_state` from inside the walk:
    // smithay's tree traversal already holds surface-state locks and a
    // nested borrow deadlocks the wayland thread. Instead we capture
    // `surface_view.offset` from the SurfaceData passed into the closure
    // (the lock there is the same that with_renderer_surface_state would
    // try to take, but smithay's traversal already has it open) and read
    // buffer dims in a separate pass.
    #[derive(Clone, Copy)]
    struct Node {
        offset_x: i32,
        offset_y: i32,
        width: i32,
        height: i32,
    }
    let mut surfaces_and_offsets: Vec<(WlSurface, (i32, i32))> = Vec::new();

    with_surface_tree_downward(
        surface,
        (0i32, 0i32),
        |sub, states, parent_offset| {
            use smithay::backend::renderer::utils::RendererSurfaceStateUserData;
            let mut my_offset = *parent_offset;
            if sub != surface {
                // Pull surface_view.offset (the composed offset including
                // subsurface position, wl_surface.offset, viewporter, etc).
                let view_off = states
                    .data_map
                    .get::<RendererSurfaceStateUserData>()
                    .and_then(|d| d.lock().ok().and_then(|s| s.view()).map(|v| v.offset));
                if let Some(o) = view_off {
                    my_offset.0 += o.x;
                    my_offset.1 += o.y;
                } else {
                    // Fall back to the older accumulator for surfaces that
                    // never reached the renderer state path yet (typically
                    // pre-first-commit). Same result for plain subsurfaces
                    // with buffer_scale=1 and no viewporter.
                    use smithay::wayland::compositor::SubsurfaceCachedState;
                    let mut sub_state = states.cached_state.get::<SubsurfaceCachedState>();
                    let loc = sub_state.current().location;
                    my_offset.0 += loc.x;
                    my_offset.1 += loc.y;
                }
            }
            surfaces_and_offsets.push((sub.clone(), my_offset));
            TraversalAction::DoChildren(my_offset)
        },
        |_, _, _| {},
        |_, _, _| true,
    );

    // Resolve buffer dims now that we're out of the surface-tree closure.
    let nodes: Vec<(WlSurface, Node)> = surfaces_and_offsets
        .into_iter()
        .map(|(s, off)| {
            let (w, h) = with_renderer_surface_state(&s, |st| {
                st.buffer_size().map(|sz| (sz.w, sz.h)).unwrap_or((0, 0))
            })
            .unwrap_or((0, 0));
            (
                s,
                Node {
                    offset_x: off.0,
                    offset_y: off.1,
                    width: w,
                    height: h,
                },
            )
        })
        .collect();

    // ── Compute union bounds. If the root has its own buffer, its dims
    //     anchor (0,0,W,H); otherwise we use the union of children.
    let root_dims = nodes.first().map(|(_, n)| *n).unwrap_or(Node {
        offset_x: 0,
        offset_y: 0,
        width: 0,
        height: 0,
    });
    let mut min_x = 0i32;
    let mut min_y = 0i32;
    let mut max_x = root_dims.width.max(0);
    let mut max_y = root_dims.height.max(0);
    for (_, n) in &nodes {
        if n.width <= 0 || n.height <= 0 {
            continue;
        }
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
        for b in composite.iter_mut() {
            *b = 0;
        }
    }

    // ── Pass 2 — blit each surface's buffer into the composite.
    let mut any_pixels = false;
    let single_surface = n_subs == 1;
    for (sub, node) in &nodes {
        if node.width <= 0 || node.height <= 0 {
            continue;
        }

        let buf = match with_renderer_surface_state(sub, |s| s.buffer().cloned()) {
            Some(Some(b)) => b,
            _ => continue,
        };

        let _ = with_buffer_contents(&buf, |ptr: *const u8, len: usize, spec| {
            let src_w = spec.width;
            let src_h = spec.height;
            let stride = spec.stride as usize;
            let has_alpha = matches!(spec.format, wl_shm::Format::Argb8888);
            let data = unsafe { std::slice::from_raw_parts(ptr, len) };

            let ox = node.offset_x - min_x;
            let oy = node.offset_y - min_y;

            // ── Compute the per-row clipped X range once, outside the loop.
            let dst_x_start = (ox.max(0)) as usize;
            let dst_x_end = ((ox + src_w).min(comp_w as i32)) as usize;
            if dst_x_end <= dst_x_start {
                return;
            }
            let src_x_start = (dst_x_start as i32 - ox) as usize;
            let row_pixels = dst_x_end - dst_x_start;
            let comp_row = comp_w as usize * 4;

            // ── Fast path: single-surface tree with no horizontal/vertical
            //     padding and an opaque XRGB8888 buffer → bulk row memcpy
            //     with B↔R swap. This is the common case for SSD apps like
            //     kitty/foot and skips the per-pixel float math entirely.
            if single_surface && !has_alpha && ox == 0 && oy == 0 && row_pixels == comp_w as usize {
                for y in 0..src_h {
                    if y >= comp_h as i32 {
                        break;
                    }
                    let s_row = (y as usize) * stride;
                    let d_row = (y as usize) * comp_row;
                    if s_row + row_pixels * 4 > data.len() {
                        break;
                    }
                    let src = &data[s_row..s_row + row_pixels * 4];
                    let dst = &mut composite[d_row..d_row + row_pixels * 4];
                    // BGRA → premultiplied RGBA (alpha=255 → no premul math).
                    for px in 0..row_pixels {
                        let i = px * 4;
                        dst[i] = src[i + 2];
                        dst[i + 1] = src[i + 1];
                        dst[i + 2] = src[i];
                        dst[i + 3] = 255;
                    }
                }
                any_pixels = true;
                return;
            }

            // ── General path: per-row source-over compositing with
            // PRE-multiplied source pixels.
            //
            // wl_shm/dmabuf format codes (ARGB8888 / XRGB8888 / etc.) all
            // imply pre-multiplied alpha by wayland convention — clients
            // are required to pre-multiply before committing. Source-over
            // compositing of pre-multiplied source onto pre-multiplied
            // destination is therefore:
            //     out.rgb = src.rgb + dst.rgb * (1 - src.a)
            //     out.a   = src.a   + dst.a   * (1 - src.a)
            // Multiplying src.rgb by af here (as the previous version did)
            // pre-multiplied a second time, which crushed every glyph edge,
            // anti-aliased line, and translucent overlay to near-black /
            // invisible. Symptom: clients render solid fills correctly but
            // text/icons/animations vanish.
            for y in 0..src_h {
                let dy = oy + y;
                if dy < 0 || dy >= comp_h as i32 {
                    continue;
                }
                let s_row = (y as usize) * stride + src_x_start * 4;
                let d_row = (dy as usize) * comp_row + dst_x_start * 4;
                if s_row + row_pixels * 4 > data.len() {
                    continue;
                }
                let src = &data[s_row..s_row + row_pixels * 4];
                let dst = &mut composite[d_row..d_row + row_pixels * 4];
                for px in 0..row_pixels {
                    let si = px * 4;
                    // Source is BGRA (wayland byte order on little-endian).
                    let b = src[si];
                    let g = src[si + 1];
                    let r = src[si + 2];
                    let a = if has_alpha { src[si + 3] } else { 255u8 };
                    if a == 0 {
                        continue;
                    }
                    let di = px * 4;
                    if a == 255 {
                        // Fully opaque — direct overwrite.
                        dst[di] = r;
                        dst[di + 1] = g;
                        dst[di + 2] = b;
                        dst[di + 3] = 255;
                    } else {
                        // src.rgb is already pre-multiplied. Just blend.
                        let inv = (255 - a) as u32;
                        dst[di] = (r as u32 + (dst[di] as u32 * inv) / 255).min(255) as u8;
                        dst[di + 1] = (g as u32 + (dst[di + 1] as u32 * inv) / 255).min(255) as u8;
                        dst[di + 2] = (b as u32 + (dst[di + 2] as u32 * inv) / 255).min(255) as u8;
                        dst[di + 3] = (a as u32 + (dst[di + 3] as u32 * inv) / 255).min(255) as u8;
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
        debug!(
            "composited surface tree: {} surfaces, {}×{}",
            n_subs, comp_w, comp_h
        );
    }

    out_guard.width = comp_w;
    out_guard.height = comp_h;
    out_guard.dirty = true;
    out_guard.version = out_guard.version.wrapping_add(1);
    n_subs
}

/// Per-surface SHM importer for the new render-element model.
///
/// Walks the surface tree rooted at `root` and stores each surface's
/// pixels in `surface_pixels_out`, keyed by `wl_surface.id().protocol_id()`.
/// Unlike `import_shm_buffer`, this does NOT composite — each surface's
/// buffer stays separate so the renderer can position and source-clip
/// them independently.
///
/// Returns the number of surfaces that had pixels imported.
pub fn import_shm_per_surface(
    root: &WlSurface,
    surface_pixels_out: &Arc<Mutex<std::collections::HashMap<u32, ClientSurfaceData>>>,
) -> usize {
    use smithay::backend::renderer::utils::with_renderer_surface_state;
    use smithay::reexports::wayland_server::protocol::wl_shm;
    use smithay::reexports::wayland_server::Resource;
    use smithay::wayland::compositor::{with_surface_tree_downward, TraversalAction};
    use smithay::wayland::shm::with_buffer_contents;

    // Collect every surface in the tree first, then read buffers in a
    // separate pass — same trick as the legacy importer to avoid nested
    // surface-state locks.
    let mut surfaces: Vec<WlSurface> = Vec::new();
    with_surface_tree_downward(
        root,
        (),
        |_, _, _| TraversalAction::DoChildren(()),
        |sub, _, _| surfaces.push(sub.clone()),
        |_, _, _| true,
    );

    let mut imported = 0usize;
    let mut out_map = surface_pixels_out.lock().unwrap();

    for surface in &surfaces {
        let key = surface.id().protocol_id();

        // Resolve buffer + dims + commit counter for this surface.
        // current_commit() advances on every wl_surface.commit; if it
        // matches what we cached last time, the surface hasn't changed
        // and we skip the BGRA→RGBA conversion + slint::Image rebuild
        // entirely — the renderer will reuse the cached image keyed by
        // version.
        let buf_dims_commit = with_renderer_surface_state(surface, |s| {
            let buf = s.buffer().cloned();
            let size = s.buffer_size();
            let commit = s.current_commit();
            (buf, size, commit)
        });
        let (buf, size, commit) = match buf_dims_commit {
            Some((Some(b), Some(sz), c)) => (b, sz, c),
            _ => continue,
        };
        let (sw, sh) = (size.w as u32, size.h as u32);
        if sw == 0 || sh == 0 {
            continue;
        }
        // Damage skip: surface's commit counter unchanged → reuse the
        // existing entry. We DON'T bump `version` here, so the renderer's
        // per-surface image cache hits and skips the GPU re-upload too.
        if let Some(existing) = out_map.get(&key) {
            if existing.last_commit == Some(commit) && existing.width == sw && existing.height == sh
            {
                tracing::trace!("shm: damage-skip surface_id={} (commit unchanged)", key);
                continue;
            }
        }

        // Read SHM contents → premul RGBA.
        let mut converted: Option<Vec<u8>> = None;
        let _ = with_buffer_contents(&buf, |ptr: *const u8, len: usize, spec| {
            let stride = spec.stride as usize;
            let has_alpha = matches!(spec.format, wl_shm::Format::Argb8888);
            let data = unsafe { std::slice::from_raw_parts(ptr, len) };
            let pixel_count = (sw * sh) as usize;
            let mut out = Vec::with_capacity(pixel_count * 4);
            // Walk row-by-row, BGRA → RGBA premul. Source is already
            // pre-multiplied per wayland convention; for opaque XRGB the
            // alpha bits are undefined and we force 0xFF.
            for y in 0..sh as usize {
                let s_row = y * stride;
                if s_row + (sw as usize) * 4 > data.len() {
                    break;
                }
                for x in 0..sw as usize {
                    let i = s_row + x * 4;
                    let b = data[i];
                    let g = data[i + 1];
                    let r = data[i + 2];
                    let a = if has_alpha { data[i + 3] } else { 255u8 };
                    out.push(r);
                    out.push(g);
                    out.push(b);
                    out.push(a);
                }
            }
            converted = Some(out);
        });
        let Some(rgba) = converted else { continue };

        let prev_version = out_map.get(&key).map(|d| d.version).unwrap_or(0);
        out_map.insert(
            key,
            ClientSurfaceData {
                pixels: rgba,
                width: sw,
                height: sh,
                dirty: true,
                version: prev_version.wrapping_add(1),
                last_commit: Some(commit),
            },
        );
        imported += 1;
    }

    // Drop entries for surfaces that have left the tree (e.g. a popup
    // subsurface was destroyed). Without this the map grows unbounded.
    let live: std::collections::HashSet<u32> =
        surfaces.iter().map(|s| s.id().protocol_id()).collect();
    out_map.retain(|k, _| live.contains(k));

    imported
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
    type KeyboardFocus = crate::wayland::xwayland::KeyboardFocusTarget;
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
        let client = focused
            .and_then(|focus| focus.wl_surface())
            .and_then(|surface| dh.get_client(surface.id()).ok());
        set_data_device_focus(dh, seat, client.clone());
        set_primary_focus(dh, seat, client);

        // Push `xdg_toplevel.Activated` to the newly-focused toplevel and
        // clear it on every other one. Without this, GTK/Qt header bars and
        // window-chrome tints don't dim/light on focus transitions
        // (shell-audit P1). Mirrors cosmic shell/focus/mod.rs:277.
        // X11 toplevels get the equivalent treatment via `X11Surface::set_activated`,
        // which we wire here too so XWayland apps respond to focus.
        use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;
        for tl in &self.toplevels {
            let is_focused = focused
                .map(|focus| focus.matches_wl_surface(&tl.surface))
                .unwrap_or(false);
            if let Some(top) = &tl.toplevel {
                let changed = top.with_pending_state(|s| {
                    let was = s.states.contains(xdg_toplevel::State::Activated);
                    if is_focused {
                        s.states.set(xdg_toplevel::State::Activated);
                    } else {
                        s.states.unset(xdg_toplevel::State::Activated);
                    }
                    was != is_focused
                });
                if changed && top.is_initial_configure_sent() {
                    top.send_pending_configure();
                }
            } else if let Some(x11) = &tl.x11_surface {
                let _ = x11.set_activated(is_focused);
            }
        }
    }

    fn cursor_image(&mut self, _seat: &Seat<Self>, image: CursorImageStatus) {
        // Honour `wl_pointer.set_cursor`. The renderer maps each variant:
        // Hidden → hide the cursor; Named → load from the system Xcursor
        // theme; Surface → upload the client's surface pixels and use
        // them as the cursor image (commit hook in wayland/compositor.rs
        // imports the pixels; renderer reads `cursor_surface_pixels`).
        self.cursor_status = image;
    }
}

delegate_seat!(SpikeState);

// ──────────────────────────────────────────────────────────────────────────────
// SelectionHandler (required by DataDeviceHandler + PrimarySelectionHandler)
// ──────────────────────────────────────────────────────────────────────────────

impl SelectionHandler for SpikeState {
    type SelectionUserData = ();

    /// A wayland client just set the selection — forward to XWayland so X11
    /// apps can paste it. Without this, copying in a Wayland app and pasting
    /// into an X11/XWayland app silently fails. Mirrors anvil/src/state.rs:251.
    fn new_selection(
        &mut self,
        ty: SelectionTarget,
        source: Option<SelectionSource>,
        _seat: Seat<Self>,
    ) {
        if let Some(xwm) = self.xwm.as_mut() {
            if let Err(err) = xwm.new_selection(ty, source.map(|s| s.mime_types())) {
                warn!(?err, ?ty, "XWayland: failed to advertise wayland selection");
            }
        }
    }

    /// An X11 client requested to read the wayland selection. Pipe the fd
    /// to XWayland's xwm which already has the transfer machinery.
    fn send_selection(
        &mut self,
        ty: SelectionTarget,
        mime_type: String,
        fd: std::os::fd::OwnedFd,
        _seat: Seat<Self>,
        _user_data: &(),
    ) {
        if let Some(xwm) = self.xwm.as_mut() {
            if let Err(err) = xwm.send_selection(ty, mime_type, fd) {
                warn!(
                    ?err,
                    ?ty,
                    "XWayland: send_selection (wayland -> X11) failed"
                );
            }
        }
    }
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
        // `cancelled`. Seed the icon offset from the current cursor surface's
        // hotspot (cosmic-comp pattern) — clients that don't subsequently call
        // wl_surface.offset on the icon still get hotspot-aligned positioning.
        let initial_offset = match &self.cursor_status {
            CursorImageStatus::Surface(cursor_surface) => {
                smithay::wayland::compositor::with_states(cursor_surface, |states| {
                    states
                        .data_map
                        .get::<smithay::input::pointer::CursorImageSurfaceData>()
                        .and_then(|d| d.lock().ok().map(|a| a.hotspot))
                        .unwrap_or_else(|| smithay::utils::Point::from((0, 0)))
                })
            }
            _ => smithay::utils::Point::from((0, 0)),
        };
        self.dnd_icon = icon.map(|surface| DndIcon {
            surface,
            offset: initial_offset,
        });

        match type_ {
            GrabType::Pointer => {
                let Some(pointer) = seat.get_pointer() else {
                    return;
                };
                let Some(start_data) = pointer.grab_start_data() else {
                    return;
                };
                pointer.set_grab(
                    self,
                    DnDGrab::new_pointer(&self.display_handle, start_data, source, seat),
                    serial,
                    Focus::Keep,
                );
            }
            GrabType::Touch => {
                let Some(touch) = seat.get_touch() else {
                    return;
                };
                let Some(start_data) = touch.grab_start_data() else {
                    return;
                };
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
        // Clear stale icon pixels so a subsequent drag without an icon doesn't
        // briefly draw the previous icon under the cursor.
        *self.dnd_icon_pixels.lock().unwrap() = ClientSurfaceData::default();
    }

    fn cancelled(&mut self, _seat: Seat<Self>, _location: Point<f64, Logical>) {
        self.dnd_icon = None;
        *self.dnd_icon_pixels.lock().unwrap() = ClientSurfaceData::default();
    }
}

// `impl DndGrabHandler for SpikeState` already lives above (custom
// `dropped`/`cancelled` for the dnd_icon lifecycle); X11Wm::start_wm's
// trait bound is satisfied by that impl too — no second blanket impl
// needed here.

impl DataDeviceHandler for SpikeState {
    fn data_device_state(&mut self) -> &mut DataDeviceState {
        &mut self.data_device_state
    }
}

smithay::delegate_data_device!(SpikeState);

// wl_fixes: registry-destroy stub — no app-level handler trait, the
// FixesState dispatch is auto-generated from the global created above.
smithay::delegate_fixes!(SpikeState);

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

// ext-data-control-v1 (standardised successor to wlr-data-control). Two
// separate handlers because the protocols are wire-incompatible; clients pick
// the one they support.
impl ExtDataControlHandler for SpikeState {
    fn data_control_state(&mut self) -> &mut ExtDataControlState {
        &mut self.ext_data_control_state
    }
}

smithay::delegate_ext_data_control!(SpikeState);

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
//   - frame() pushes the Frame onto `pending_capture_frames`; the main
//     loop drains it right after each `render_frame` so the readback
//     reads the just-presented final_tex (see `screencopy.rs`).
//     dmabuf-backed capture is not yet supported; SHM only.
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

    fn new_session(&mut self, session: Session) {
        // Per smithay docs: "The compositor should store this session". Keeps
        // the session alive (Session drops fail all pending frames) and lets
        // `refresh_capture_constraints` re-emit constraints to it when an
        // output mode changes.
        // Immediately push current constraints so the client can size its
        // buffer pool — without this, portal screencast stalls waiting on a
        // constraints event that never arrives.
        if let Some(constraints) = self.capture_constraints(&session.source()) {
            session.update_constraints(constraints);
        }
        self.capture_sessions.push(session);
        // Sweep dead sessions while we're here so the vec doesn't grow.
        self.cleanup_capture_sessions();
    }

    fn frame(&mut self, _session: &SessionRef, frame: Frame) {
        // Defer: the wgpu device + final_tex live in the renderer, not on
        // SpikeState. The main loop drains this vec right after each
        // `render_frame` so the readback samples the just-presented frame.
        self.pending_capture_frames.push(frame);
    }
}

impl SpikeState {
    /// Remove sessions whose underlying client object is no longer alive.
    pub fn cleanup_capture_sessions(&mut self) {
        use smithay::utils::IsAlive;
        self.capture_sessions.retain(|s| s.alive());
    }

    /// Re-issue buffer-constraints to every live capture session — called
    /// after the wl_output mode changes (host window resize, scale change).
    /// Without this, portal screencast keeps allocating buffers at the
    /// stale size and frame submission keeps failing the size match in
    /// `screencopy.rs`.
    /// Idle-inhibit while a mapped toplevel claims `content-type-v1` Video
    /// or Game. Used by movies and games so the screensaver doesn't fire
    /// mid-cutscene; matches gnome-shell / kwin behaviour. Cheap: iterates
    /// the toplevel list and reads cached state.
    pub fn refresh_content_type_idle_inhibit(&mut self) {
        use smithay::wayland::compositor::with_states;
        use smithay::wayland::content_type::ContentTypeSurfaceCachedState;
        use wayland_protocols::wp::content_type::v1::server::wp_content_type_v1::Type;
        let any_active = self.toplevels.iter().any(|tl| {
            with_states(&tl.surface, |states| {
                let mut g = states.cached_state.get::<ContentTypeSurfaceCachedState>();
                matches!(g.current().content_type(), Type::Video | Type::Game)
            })
        });
        // OR with explicit idle-inhibit-v1 inhibitors.
        let inhibited = any_active || !self.idle_inhibitors.is_empty();
        self.idle_notifier_state.set_is_inhibited(inhibited);
    }

    pub fn refresh_capture_constraints(&mut self) {
        use smithay::utils::IsAlive;
        // Snapshot sources off the sessions vec first so the subsequent
        // `&mut self` for capture_constraints doesn't double-borrow.
        let sources: Vec<_> = self
            .capture_sessions
            .iter()
            .enumerate()
            .filter(|(_, s)| s.alive())
            .map(|(i, s)| (i, s.source()))
            .collect();
        let updates: Vec<_> = sources
            .into_iter()
            .filter_map(|(i, src)| self.capture_constraints(&src).map(|c| (i, c)))
            .collect();
        for (i, c) in updates {
            if let Some(session) = self.capture_sessions.get(i) {
                session.update_constraints(c);
            }
        }
        self.cleanup_capture_sessions();
    }
}

smithay::delegate_image_copy_capture!(SpikeState);
