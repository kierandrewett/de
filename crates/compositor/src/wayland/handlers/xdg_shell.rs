//! XDG shell handler — toplevel and popup window management.

use smithay::{
    delegate_xdg_shell,
    desktop::{PopupKind, Window},
    reexports::wayland_server::protocol::wl_seat,
    utils::Serial,
    wayland::{
        seat::WaylandFocus,
        shell::xdg::{
            PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState,
        },
    },
};

use crate::state::State;

impl XdgShellHandler for State {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.common.xdg_shell_state
    }

    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        let window = Window::new_wayland_window(surface);
        // Place new windows at the origin; subagent 09 (shell) handles tiling/placement.
        self.common.space.map_element(window, (0, 0), false);
    }

    fn new_popup(&mut self, surface: PopupSurface, _positioner: PositionerState) {
        self.common.popup_manager.track_popup(PopupKind::Xdg(surface)).ok();
    }

    fn move_request(&mut self, _surface: ToplevelSurface, _seat: wl_seat::WlSeat, _serial: Serial) {
        // Subagent 09 implements interactive move.
    }

    fn resize_request(
        &mut self,
        _surface: ToplevelSurface,
        _seat: wl_seat::WlSeat,
        _serial: Serial,
        _edges: smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::ResizeEdge,
    ) {
        // Subagent 09 implements interactive resize.
    }

    fn reposition_request(
        &mut self,
        surface: PopupSurface,
        positioner: PositionerState,
        token: u32,
    ) {
        surface.with_pending_state(|state| {
            state.geometry = positioner.get_geometry();
            state.positioner = positioner;
        });
        surface.send_repositioned(token);
        surface.send_configure().ok();
    }

    fn grab(&mut self, _surface: PopupSurface, _seat: wl_seat::WlSeat, _serial: Serial) {}

    fn toplevel_destroyed(&mut self, surface: ToplevelSurface) {
        let target_surface = surface.wl_surface();
        let window = self
            .common
            .space
            .elements()
            .find(|w| w.wl_surface().map(|s| s.as_ref() == target_surface).unwrap_or(false))
            .cloned();

        if let Some(w) = window {
            self.common.space.unmap_elem(&w);
        }
    }
}

delegate_xdg_shell!(State);
