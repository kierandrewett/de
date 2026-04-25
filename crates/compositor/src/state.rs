//! Compositor `State` — threaded through the calloop event loop.
//!
//! Subagent 07 owns the Wayland fields. Subagent 08 adds render fields.
//! Subagent 09 adds shell/window-management fields.

use std::time::Instant;

use smithay::{
    desktop::{PopupManager, Space, Window},
    input::{pointer::CursorImageStatus, Seat, SeatState},
    reexports::{
        calloop::{LoopHandle, LoopSignal},
        wayland_server::{
            backend::{ClientData, ClientId, DisconnectReason},
            DisplayHandle,
        },
    },
    utils::{Clock, Monotonic},
    wayland::{
        alpha_modifier::AlphaModifierState,
        commit_timing::CommitTimingManagerState,
        compositor::{CompositorClientState, CompositorState},
        content_type::ContentTypeState,
        cursor_shape::CursorShapeManagerState,
        dmabuf::DmabufState,
        fifo::FifoManagerState,
        foreign_toplevel_list::ForeignToplevelListState,
        fractional_scale::FractionalScaleManagerState,
        idle_inhibit::IdleInhibitManagerState,
        idle_notify::IdleNotifierState,
        image_capture_source::{ImageCaptureSourceState, OutputCaptureSourceState},
        image_copy_capture::ImageCopyCaptureState,
        input_method::InputMethodManagerState,
        keyboard_shortcuts_inhibit::KeyboardShortcutsInhibitState,
        output::OutputManagerState,
        pointer_constraints::PointerConstraintsState,
        pointer_gestures::PointerGesturesState,
        presentation::PresentationState,
        relative_pointer::RelativePointerManagerState,
        security_context::SecurityContextState,
        selection::{
            data_device::DataDeviceState,
            ext_data_control::DataControlState,
            primary_selection::PrimarySelectionState,
            wlr_data_control::DataControlState as WlrDataControlState,
        },
        session_lock::SessionLockManagerState,
        shell::{
            kde::decoration::KdeDecorationState,
            wlr_layer::WlrLayerShellState,
            xdg::{decoration::XdgDecorationState, dialog::XdgDialogState, XdgShellState},
        },
        shm::ShmState,
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
    },
};


/// Top-level compositor state passed to every calloop callback.
pub struct State {
    pub backend: Backend,
    pub common: CommonState,
}

/// Backend-specific data.
#[allow(dead_code)]
pub enum Backend {
    Winit(Box<crate::winit::WinitData>),
    Udev(crate::udev::UdevData),
}

/// Protocol and desktop state shared across backends.
#[allow(dead_code)]
pub struct CommonState {
    // ── Runtime ──────────────────────────────────────────────────────────
    pub display_handle: DisplayHandle,
    pub loop_handle: LoopHandle<'static, State>,
    pub loop_signal: LoopSignal,
    pub start_time: Instant,
    pub clock: Clock<Monotonic>,
    pub socket_name: String,

    // ── Seat / Input ─────────────────────────────────────────────────────
    pub seat: Seat<State>,
    pub seat_state: SeatState<State>,
    pub cursor_status: CursorImageStatus,

    // ── P0 Core ──────────────────────────────────────────────────────────
    pub compositor_state: CompositorState,
    pub shm_state: ShmState,
    pub dmabuf_state: DmabufState,
    pub output_manager_state: OutputManagerState,
    pub xdg_shell_state: XdgShellState,
    pub layer_shell_state: WlrLayerShellState,

    // ── P1 Daily-driver ───────────────────────────────────────────────────
    pub xdg_decoration_state: XdgDecorationState,
    pub kde_decoration_state: KdeDecorationState,
    pub data_device_state: DataDeviceState,
    pub primary_selection_state: PrimarySelectionState,
    pub data_control_state: DataControlState,
    pub wlr_data_control_state: WlrDataControlState,
    pub fractional_scale_manager_state: FractionalScaleManagerState,
    pub viewporter_state: ViewporterState,
    pub presentation_state: PresentationState,
    pub single_pixel_buffer_state: SinglePixelBufferState,
    pub idle_notifier_state: IdleNotifierState<State>,
    pub idle_inhibit_manager_state: IdleInhibitManagerState,
    pub session_lock_manager_state: SessionLockManagerState,
    pub relative_pointer_state: RelativePointerManagerState,
    pub pointer_constraints_state: PointerConstraintsState,
    pub pointer_gestures_state: PointerGesturesState,
    pub keyboard_shortcuts_inhibit_state: KeyboardShortcutsInhibitState,
    pub cursor_shape_manager_state: CursorShapeManagerState,
    pub tablet_manager_state: TabletManagerState,
    pub text_input_state: TextInputManagerState,
    pub input_method_state: InputMethodManagerState,
    pub virtual_keyboard_state: VirtualKeyboardManagerState,
    pub activation_state: XdgActivationState,
    pub content_type_state: ContentTypeState,
    pub fifo_state: FifoManagerState,
    pub commit_timing_state: CommitTimingManagerState,

    // ── P2 Full-DE ────────────────────────────────────────────────────────
    pub xdg_foreign_state: XdgForeignState,
    pub foreign_toplevel_list_state: ForeignToplevelListState,
    pub alpha_modifier_state: AlphaModifierState,
    pub security_context_state: SecurityContextState,
    pub image_capture_source_state: ImageCaptureSourceState,
    pub output_capture_source_state: OutputCaptureSourceState,
    pub image_copy_capture_state: ImageCopyCaptureState,
    pub xdg_dialog_state: XdgDialogState,
    pub xdg_system_bell_state: XdgSystemBellState,
    pub xdg_toplevel_icon_manager: XdgToplevelIconManager,
    pub xdg_toplevel_tag_manager: XdgToplevelTagManager,
    pub xwayland_keyboard_grab_state: XWaylandKeyboardGrabState,

    // ── Desktop bookkeeping ───────────────────────────────────────────────
    pub space: Space<Window>,
    pub popup_manager: PopupManager,

    // === render fields (subagent 08) ===

    // === shell fields (subagent 09) ===
    /// Window management state: layout, focus, animations.
    pub shell: crate::shell::Shell,
    /// Active pointer grab (move or resize), if any.
    pub grab: crate::shell::grab::GrabState,
}

impl CommonState {
    /// Initialise all protocol states from a live `DisplayHandle`.
    pub fn new(
        dh: &DisplayHandle,
        loop_handle: LoopHandle<'static, State>,
        loop_signal: LoopSignal,
        socket_name: String,
    ) -> Self {
        let clock = Clock::<Monotonic>::new();

        let compositor_state = CompositorState::new::<State>(dh);
        let shm_state = ShmState::new::<State>(dh, vec![]);
        let output_manager_state = OutputManagerState::new_with_xdg_output::<State>(dh);
        let xdg_shell_state = XdgShellState::new::<State>(dh);
        let layer_shell_state = WlrLayerShellState::new::<State>(dh);
        let xdg_decoration_state = XdgDecorationState::new::<State>(dh);
        let kde_decoration_state = KdeDecorationState::new::<State>(
            dh,
            wayland_protocols_misc::server_decoration::server::org_kde_kwin_server_decoration_manager::Mode::Server,
        );
        let data_device_state = DataDeviceState::new::<State>(dh);
        let primary_selection_state = PrimarySelectionState::new::<State>(dh);
        let data_control_state =
            DataControlState::new::<State, _>(dh, Some(&primary_selection_state), |_| true);
        let wlr_data_control_state =
            WlrDataControlState::new::<State, _>(dh, Some(&primary_selection_state), |_| true);
        let dmabuf_state = DmabufState::new();
        let fractional_scale_manager_state = FractionalScaleManagerState::new::<State>(dh);
        let viewporter_state = ViewporterState::new::<State>(dh);
        let presentation_state = PresentationState::new::<State>(dh, clock.id() as u32);
        let single_pixel_buffer_state = SinglePixelBufferState::new::<State>(dh);
        let idle_notifier_state = IdleNotifierState::<State>::new(dh, loop_handle.clone());
        let idle_inhibit_manager_state = IdleInhibitManagerState::new::<State>(dh);
        let session_lock_manager_state =
            SessionLockManagerState::new::<State, _>(dh, |_| true);
        let relative_pointer_state = RelativePointerManagerState::new::<State>(dh);
        let pointer_constraints_state = PointerConstraintsState::new::<State>(dh);
        let pointer_gestures_state = PointerGesturesState::new::<State>(dh);
        let keyboard_shortcuts_inhibit_state = KeyboardShortcutsInhibitState::new::<State>(dh);
        let cursor_shape_manager_state = CursorShapeManagerState::new::<State>(dh);
        let tablet_manager_state = TabletManagerState::new::<State>(dh);
        let text_input_state = TextInputManagerState::new::<State>(dh);
        let input_method_state = InputMethodManagerState::new::<State, _>(dh, |_| true);
        let virtual_keyboard_state =
            VirtualKeyboardManagerState::new::<State, _>(dh, |_| true);
        let activation_state = XdgActivationState::new::<State>(dh);
        let content_type_state = ContentTypeState::new::<State>(dh);
        let fifo_state = FifoManagerState::new::<State>(dh);
        let commit_timing_state = CommitTimingManagerState::new::<State>(dh);
        let xdg_foreign_state = XdgForeignState::new::<State>(dh);
        let foreign_toplevel_list_state = ForeignToplevelListState::new::<State>(dh);
        let alpha_modifier_state = AlphaModifierState::new::<State>(dh);
        let security_context_state =
            SecurityContextState::new::<State, _>(dh, |_| true);
        let image_capture_source_state = ImageCaptureSourceState::new();
        let output_capture_source_state = OutputCaptureSourceState::new::<State>(dh);
        let image_copy_capture_state = ImageCopyCaptureState::new::<State>(dh);
        let xdg_dialog_state = XdgDialogState::new::<State>(dh);
        let xdg_system_bell_state = XdgSystemBellState::new::<State>(dh);
        let xdg_toplevel_icon_manager = XdgToplevelIconManager::new::<State>(dh);
        let xdg_toplevel_tag_manager = XdgToplevelTagManager::new::<State>(dh);
        let xwayland_keyboard_grab_state = XWaylandKeyboardGrabState::new::<State>(dh);

        let mut seat_state = SeatState::new();
        let mut seat: Seat<State> = seat_state.new_wl_seat(dh, "seat0");
        seat.add_keyboard(Default::default(), 200, 25).ok();
        seat.add_pointer();

        Self {
            display_handle: dh.clone(),
            loop_handle,
            loop_signal,
            start_time: Instant::now(),
            clock,
            socket_name,
            seat,
            seat_state,
            cursor_status: CursorImageStatus::default_named(),
            compositor_state,
            shm_state,
            dmabuf_state,
            output_manager_state,
            xdg_shell_state,
            layer_shell_state,
            xdg_decoration_state,
            kde_decoration_state,
            data_device_state,
            primary_selection_state,
            data_control_state,
            wlr_data_control_state,
            fractional_scale_manager_state,
            viewporter_state,
            presentation_state,
            single_pixel_buffer_state,
            idle_notifier_state,
            idle_inhibit_manager_state,
            session_lock_manager_state,
            relative_pointer_state,
            pointer_constraints_state,
            pointer_gestures_state,
            keyboard_shortcuts_inhibit_state,
            cursor_shape_manager_state,
            tablet_manager_state,
            text_input_state,
            input_method_state,
            virtual_keyboard_state,
            activation_state,
            content_type_state,
            fifo_state,
            commit_timing_state,
            xdg_foreign_state,
            foreign_toplevel_list_state,
            alpha_modifier_state,
            security_context_state,
            image_capture_source_state,
            output_capture_source_state,
            image_copy_capture_state,
            xdg_dialog_state,
            xdg_system_bell_state,
            xdg_toplevel_icon_manager,
            xdg_toplevel_tag_manager,
            xwayland_keyboard_grab_state,
            space: Space::default(),
            popup_manager: PopupManager::default(),
            shell: crate::shell::Shell::new(),
            grab: crate::shell::grab::GrabState::default(),
        }
    }
}

/// Per-client data stored by wayland-server.
#[derive(Default)]
pub struct ClientState {
    pub compositor_state: CompositorClientState,
}

impl ClientData for ClientState {
    fn initialized(&self, _client_id: ClientId) {}
    fn disconnected(&self, _client_id: ClientId, _reason: DisconnectReason) {}
}
