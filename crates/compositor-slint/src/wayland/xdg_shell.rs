//! xdg-shell handler — toplevels, popups, configure.

use smithay::{
    delegate_xdg_shell,
    utils::SERIAL_COUNTER,
    wayland::shell::xdg::{
        PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState,
    },
};
use tracing::info;

use crate::wayland_state::SpikeState;

impl XdgShellHandler for SpikeState {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg_shell_state
    }

    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        info!("new xdg toplevel");
        surface.with_pending_state(|s| {
            s.size = Some((800, 600).into());
        });
        surface.send_configure();

        self.active_surface = Some(surface.wl_surface().clone());

        if let Some(kb) = self.seat.get_keyboard() {
            kb.set_focus(
                self,
                Some(surface.wl_surface().clone()),
                SERIAL_COUNTER.next_serial(),
            );
        }
    }

    fn new_popup(&mut self, _surface: PopupSurface, _positioner: PositionerState) {}

    fn toplevel_destroyed(&mut self, _surface: ToplevelSurface) {
        info!("toplevel destroyed");
        self.active_surface = None;
        *self.client_pixels.lock().unwrap() = crate::wayland_state::ClientSurfaceData::default();
    }

    fn popup_destroyed(&mut self, _surface: PopupSurface) {}

    fn grab(
        &mut self,
        _surface: PopupSurface,
        _seat: smithay::reexports::wayland_server::protocol::wl_seat::WlSeat,
        _serial: smithay::utils::Serial,
    ) {
    }

    fn reposition_request(
        &mut self,
        surface: PopupSurface,
        positioner: PositionerState,
        token: u32,
    ) {
        surface.with_pending_state(|s| {
            s.geometry = positioner.get_geometry();
        });
        surface.send_repositioned(token);
    }
}

delegate_xdg_shell!(SpikeState);
