//! Core compositor, SHM, seat, and output protocol handlers.

use smithay::{
    backend::renderer::utils::on_commit_buffer_handler,
    delegate_compositor, delegate_output, delegate_seat, delegate_shm,
    desktop::{layer_map_for_output, WindowSurfaceType},
    input::{pointer::CursorImageStatus, Seat, SeatHandler, SeatState},
    reexports::wayland_server::{
        protocol::wl_surface::WlSurface,
        Client, Resource,
    },
    wayland::{
        buffer::BufferHandler,
        compositor::{with_states, CompositorClientState, CompositorHandler, CompositorState},
        selection::{
            data_device::set_data_device_focus,
            primary_selection::set_primary_focus,
        },
        seat::WaylandFocus,
        shell::wlr_layer::LayerSurfaceData,
        shm::{ShmHandler, ShmState},
    },
};

use crate::{
    focus::{KeyboardFocusTarget, PointerFocusTarget},
    state::{ClientState, State},
};

// ─── CompositorHandler ───────────────────────────────────────────────────────

impl CompositorHandler for State {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.common.compositor_state
    }

    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        &client.get_data::<ClientState>().unwrap().compositor_state
    }

    fn commit(&mut self, surface: &WlSurface) {
        on_commit_buffer_handler::<Self>(surface);
        self.common.popup_manager.commit(surface);

        let window_for_surface = self
            .common
            .space
            .elements()
            .find(|w| {
                w.wl_surface()
                    .map(|s| &*s == surface)
                    .unwrap_or(false)
            })
            .cloned();

        if let Some(ref window) = window_for_surface {
            window.on_commit();
        }

        // First commit with a real buffer kicks off the open animation —
        // until this point the spring is held at start so the fade-in
        // doesn't burn through while the surface is still invisible.
        if let Some(window) = window_for_surface {
            let has_buffer = smithay::backend::renderer::utils::with_renderer_surface_state(
                surface,
                |state| state.buffer().is_some(),
            )
            .unwrap_or(false);
            if has_buffer {
                if let Some(id) = window
                    .user_data()
                    .get::<crate::wayland::handlers::xdg_shell::ShellWindowId>()
                    .map(|i| i.0)
                {
                    if let Some(w) = self.common.shell.window_mut(id) {
                        w.start_open_animation();
                    }
                }
            }
        }

        // wlr-layer-shell: send the initial configure on the client's
        // first commit. The protocol spec mandates the configure go out
        // *after* the initial commit so the client gets a chance to
        // declare its desired anchor/exclusive zone first; we then
        // arrange (which fills in the actual size from output bounds)
        // and dispatch the configure. Without this the panel/dock sit
        // forever waiting on a size and never paint.
        configure_layer_surface_on_commit(self, surface);
    }
}

/// If `surface` is a wlr-layer-shell surface and we haven't sent its
/// initial configure yet, do so now (after letting `arrange()` compute
/// the size). Mirrors anvil's pattern.
fn configure_layer_surface_on_commit(state: &mut State, surface: &WlSurface) {
    let output = state
        .common
        .space
        .outputs()
        .find(|o| {
            let map = layer_map_for_output(o);
            map.layer_for_surface(surface, WindowSurfaceType::TOPLEVEL)
                .is_some()
        })
        .cloned();
    let Some(output) = output else { return };

    let initial_configure_sent = with_states(surface, |states| {
        states
            .data_map
            .get::<LayerSurfaceData>()
            .map(|d| d.lock().unwrap().initial_configure_sent)
            .unwrap_or(true)
    });

    let mut map = layer_map_for_output(&output);
    // Arrange first so any size the client declared (anchors,
    // exclusive zone, requested size) is reflected in the configure
    // we're about to send.
    map.arrange();
    if !initial_configure_sent {
        if let Some(layer) =
            map.layer_for_surface(surface, WindowSurfaceType::TOPLEVEL)
        {
            tracing::info!(
                output = %output.name(),
                ns = layer.namespace(),
                "sending initial layer-shell configure",
            );
            layer.layer_surface().send_configure();
        }
    }
}

delegate_compositor!(State);

// ─── BufferHandler ────────────────────────────────────────────────────────────

impl BufferHandler for State {
    fn buffer_destroyed(
        &mut self,
        _buffer: &smithay::reexports::wayland_server::protocol::wl_buffer::WlBuffer,
    ) {
    }
}

// ─── ShmHandler ───────────────────────────────────────────────────────────────

impl ShmHandler for State {
    fn shm_state(&self) -> &ShmState {
        &self.common.shm_state
    }
}

delegate_shm!(State);

// ─── SeatHandler ──────────────────────────────────────────────────────────────

impl SeatHandler for State {
    type KeyboardFocus = KeyboardFocusTarget;
    type PointerFocus = PointerFocusTarget;
    type TouchFocus = PointerFocusTarget;

    fn seat_state(&mut self) -> &mut SeatState<Self> {
        &mut self.common.seat_state
    }

    fn focus_changed(&mut self, seat: &Seat<Self>, focused: Option<&Self::KeyboardFocus>) {
        let dh = self.common.display_handle.clone();
        let client = focused
            .and_then(|t| t.wl_surface())
            .and_then(|s| s.as_ref().client());
        set_data_device_focus(&dh, seat, client.clone());
        set_primary_focus(&dh, seat, client);
    }

    fn cursor_image(&mut self, _seat: &Seat<Self>, image: CursorImageStatus) {
        self.common.cursor_status = image;
    }
}

delegate_seat!(State);

// ─── OutputHandler ────────────────────────────────────────────────────────────

impl smithay::wayland::output::OutputHandler for State {}

delegate_output!(State);
