//! Core compositor, SHM, seat, and output protocol handlers.

use smithay::{
    backend::renderer::utils::on_commit_buffer_handler,
    delegate_compositor, delegate_output, delegate_seat, delegate_shm,
    input::{pointer::CursorImageStatus, Seat, SeatHandler, SeatState},
    reexports::wayland_server::{
        protocol::wl_surface::WlSurface,
        Client, Resource,
    },
    wayland::{
        buffer::BufferHandler,
        compositor::{CompositorClientState, CompositorHandler, CompositorState},
        selection::{
            data_device::set_data_device_focus,
            primary_selection::set_primary_focus,
        },
        seat::WaylandFocus,
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

        if let Some(window) = self
            .common
            .space
            .elements()
            .find(|w| {
                w.wl_surface()
                    .map(|s| &*s == surface)
                    .unwrap_or(false)
            })
            .cloned()
        {
            window.on_commit();
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
