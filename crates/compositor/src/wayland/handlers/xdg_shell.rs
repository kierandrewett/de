//! XDG shell handler — toplevel and popup window management.

use smithay::{
    delegate_xdg_shell,
    desktop::{PopupKind, Window},
    reexports::wayland_server::protocol::wl_seat,
    utils::{Rectangle, Serial},
    wayland::{
        seat::WaylandFocus,
        shell::xdg::{
            PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState,
        },
    },
};

use crate::shell::{DecorationMode, MappedWindow, WindowSurface};
use crate::state::State;

/// Stable shell-window identifier attached to a smithay [`Window`] via its
/// `user_data` map. Allows us to round-trip from a wayland surface to the
/// shell's `MappedWindow.id`.
#[derive(Debug, Clone, Copy)]
struct ShellWindowId(u64);

impl XdgShellHandler for State {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.common.xdg_shell_state
    }

    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        // Allocate a shell-side id and stash it on the smithay Window so we
        // can find it again on destroy. Title and app_id arrive later via
        // xdg_toplevel.set_title / .set_app_id (not wired here yet).
        let id = self.common.shell.alloc_window_id();
        tracing::info!(id, "xdg_shell: new_toplevel");
        let window = Window::new_wayland_window(surface);
        window.user_data().insert_if_missing(|| ShellWindowId(id));

        // Default geometry — real placement happens once the client commits.
        let geometry: Rectangle<i32, smithay::utils::Logical> =
            Rectangle::from_size((640, 480).into());
        let mapped = MappedWindow::new(
            id,
            WindowSurface { token: id },
            geometry,
            DecorationMode::ServerSide,
            String::new(),
            String::new(),
        );
        self.common.shell.add_window(mapped);

        self.common.space.map_element(window, (0, 0), false);

        // Notify shell processes (panel, dock, launcher).
        if let Some(info) = self.common.shell.window_info(id) {
            self.common.ipc.broadcast(&ipc::ShellEvent::WindowOpened { window: info });
        }
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
            // Pull the shell id back out, drop the corresponding MappedWindow,
            // and tell the shell processes about the close.
            if let Some(ShellWindowId(id)) = w.user_data().get::<ShellWindowId>().copied() {
                self.common.shell.remove_window(id);
                self.common.ipc.broadcast(&ipc::ShellEvent::WindowClosed { window_id: id });
            }
            self.common.space.unmap_elem(&w);
        }
    }
}

delegate_xdg_shell!(State);
