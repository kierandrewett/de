//! XDG shell handler — toplevel and popup window management.

use smithay::{
    delegate_xdg_shell,
    desktop::{PopupKind, Window},
    reexports::wayland_server::protocol::wl_seat,
    utils::{Rectangle, Serial, SERIAL_COUNTER},
    wayland::{
        seat::WaylandFocus,
        shell::xdg::{
            PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState,
        },
    },
};

use crate::focus::KeyboardFocusTarget;
use crate::shell::{DecorationMode, MappedWindow, WindowSurface};
use crate::state::State;

/// Stable shell-window identifier attached to a smithay [`Window`] via its
/// `user_data` map. Allows us to round-trip from a wayland surface to the
/// shell's `MappedWindow.id`.
#[derive(Debug, Clone, Copy)]
pub struct ShellWindowId(pub u64);

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

        // Cascade placement: each new window steps `CASCADE_STEP` (30 px)
        // down-right from the work-area origin so a stack of related
        // windows from the same launcher run remains visually
        // distinguishable instead of all stacking on the same coords.
        let location: smithay::utils::Point<i32, smithay::utils::Logical> = if let Some(out) =
            self.common.space.outputs().next().cloned()
        {
            let out_geo = self
                .common
                .space
                .output_geometry(&out)
                .unwrap_or_else(|| Rectangle::from_size((1280, 800).into()));
            let monitor = crate::shell::MonitorInfo {
                name: out.name(),
                logical_rect: out_geo,
                panel_height: 0,
                dock_height: 0,
            };
            crate::shell::floating::cascade_position(&self.common.shell, &monitor, geometry.size)
        } else {
            smithay::utils::Point::from((0, 0))
        };
        self.common.space.map_element(window.clone(), location, true);

        // Mirror the centered position into the shell's record so
        // `MappedWindow.geometry` matches what's actually on screen.
        // Otherwise interactive move / chrome hit-tests start from the
        // (0,0) default and snap the window to a wrong place on first
        // drag.
        if let Some(w) = self.common.shell.window_mut(id) {
            w.geometry = Rectangle::new(location.into(), geometry.size);
            w.animation.set_geometry_instant(w.geometry);
        }

        // Auto-focus the new window so wtype / typed input has somewhere to
        // land. Real placement / activation policy belongs in the shell
        // module; this keeps the playground usable.
        if let Some(kb) = self.common.seat.get_keyboard() {
            let target = KeyboardFocusTarget::Window(Box::new(window));
            let serial = SERIAL_COUNTER.next_serial();
            kb.set_focus(self, Some(target), serial);
        }
        self.common.shell.focus_window(id);

        // Notify shell processes (panel, dock, launcher).
        if let Some(info) = self.common.shell.window_info(id) {
            self.common.ipc.broadcast(&ipc::ShellEvent::WindowOpened { window: info });
        }
        let focus_info = self.common.shell.window_info(id);
        self.common.ipc.broadcast(&ipc::ShellEvent::FocusedWindowChanged { window: focus_info });
    }

    fn new_popup(&mut self, surface: PopupSurface, _positioner: PositionerState) {
        self.common.popup_manager.track_popup(PopupKind::Xdg(surface)).ok();
    }

    fn move_request(&mut self, surface: ToplevelSurface, _seat: wl_seat::WlSeat, _serial: Serial) {
        // Client-initiated move (e.g. CSD title-bar drag). Routed to the
        // same grab path used by SSD title-bar drag in `input.rs` so the
        // animation, snap detection, and Z-order behaviour all match.
        let Some(id) = id_for_toplevel(self, &surface) else { return };
        let pos = self
            .common
            .seat
            .get_pointer()
            .map(|p| p.current_location())
            .unwrap_or_default();
        let start = (pos.x.round() as i32, pos.y.round() as i32).into();
        crate::shell::grab::begin_move(&mut self.common.grab, &self.common.shell, id, start);
    }

    fn resize_request(
        &mut self,
        surface: ToplevelSurface,
        _seat: wl_seat::WlSeat,
        _serial: Serial,
        edges: smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::ResizeEdge,
    ) {
        let Some(id) = id_for_toplevel(self, &surface) else { return };
        let edge = match edges {
            smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::ResizeEdge::Top => {
                crate::shell::grab::ResizeEdge::North
            }
            smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::ResizeEdge::Bottom => {
                crate::shell::grab::ResizeEdge::South
            }
            smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::ResizeEdge::Left => {
                crate::shell::grab::ResizeEdge::West
            }
            smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::ResizeEdge::Right => {
                crate::shell::grab::ResizeEdge::East
            }
            smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::ResizeEdge::TopLeft => {
                crate::shell::grab::ResizeEdge::NorthWest
            }
            smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::ResizeEdge::TopRight => {
                crate::shell::grab::ResizeEdge::NorthEast
            }
            smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::ResizeEdge::BottomLeft => {
                crate::shell::grab::ResizeEdge::SouthWest
            }
            smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::ResizeEdge::BottomRight => {
                crate::shell::grab::ResizeEdge::SouthEast
            }
            // `None` and any future variants — no edge specified, no-op.
            _ => return,
        };
        let pos = self
            .common
            .seat
            .get_pointer()
            .map(|p| p.current_location())
            .unwrap_or_default();
        let start = (pos.x.round() as i32, pos.y.round() as i32).into();
        crate::shell::grab::begin_resize(
            &mut self.common.grab,
            &self.common.shell,
            id,
            start,
            edge,
        );
    }

    fn maximize_request(&mut self, surface: ToplevelSurface) {
        let Some(id) = id_for_toplevel(self, &surface) else { return };
        let Some(monitor) = monitor_for_window(self, id) else { return };
        crate::shell::maximize::maximize_window(&mut self.common.shell, id, &monitor);
        sync_geometry_to_space(self, id);
        ack_state_to_client(&surface, self, id);
        broadcast_state(self, id);
    }

    fn unmaximize_request(&mut self, surface: ToplevelSurface) {
        let Some(id) = id_for_toplevel(self, &surface) else { return };
        crate::shell::maximize::unmaximize_window(&mut self.common.shell, id);
        sync_geometry_to_space(self, id);
        ack_state_to_client(&surface, self, id);
        broadcast_state(self, id);
    }

    fn minimize_request(&mut self, surface: ToplevelSurface) {
        let Some(id) = id_for_toplevel(self, &surface) else { return };
        let dock_rect = monitor_for_window(self, id).map(|m| ipc::Rect {
            x: m.logical_rect.loc.x + m.logical_rect.size.w / 2 - 40,
            y: m.logical_rect.loc.y + m.logical_rect.size.h - 80,
            w: 80,
            h: 80,
        });
        crate::shell::minimize::minimize_window(&mut self.common.shell, id, dock_rect);
        broadcast_state(self, id);
    }

    fn fullscreen_request(
        &mut self,
        surface: ToplevelSurface,
        _output: Option<smithay::reexports::wayland_server::protocol::wl_output::WlOutput>,
    ) {
        let Some(id) = id_for_toplevel(self, &surface) else { return };
        let Some(monitor) = monitor_for_window(self, id) else { return };
        if let Some(w) = self.common.shell.window_mut(id) {
            if !w.is_fullscreen {
                w.pre_maximize_rect = Some(w.geometry);
                w.is_fullscreen = true;
                w.geometry = monitor.logical_rect;
                w.animation.set_geometry_target(monitor.logical_rect);
            }
        }
        sync_geometry_to_space(self, id);
        ack_state_to_client(&surface, self, id);
        broadcast_state(self, id);
    }

    fn unfullscreen_request(&mut self, surface: ToplevelSurface) {
        let Some(id) = id_for_toplevel(self, &surface) else { return };
        if let Some(w) = self.common.shell.window_mut(id) {
            if w.is_fullscreen {
                w.is_fullscreen = false;
                if let Some(restore) = w.pre_maximize_rect.take() {
                    w.geometry = restore;
                    w.animation.set_geometry_target(restore);
                }
            }
        }
        sync_geometry_to_space(self, id);
        ack_state_to_client(&surface, self, id);
        broadcast_state(self, id);
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
            if let Some(ShellWindowId(id)) = w.user_data().get::<ShellWindowId>().copied() {
                // Don't remove immediately — start the close fade so
                // chrome (title bar + border + shadow) fades out instead
                // of popping. The wayland surface is gone, so the
                // SDF-clipped surface element will fail to build and the
                // content area is left empty during the fade. Once the
                // opacity spring settles below 0.01, render_frame's
                // sweep removes the entry and unmaps from the space.
                if self.common.shell.window(id).is_some() {
                    self.common.shell.begin_close(id);
                } else {
                    // Defensive: if the entry was already gone (e.g. the
                    // IPC path beat us to it), still unmap + broadcast.
                    self.common
                        .ipc
                        .broadcast(&ipc::ShellEvent::WindowClosed { window_id: id });
                    self.common.space.unmap_elem(&w);
                }
            }
        }
    }
}

delegate_xdg_shell!(State);

/// Map a `ToplevelSurface` to its shell-side window id (the same id
/// stamped on the smithay `Window` via `user_data` in `new_toplevel`).
fn id_for_toplevel(state: &State, surface: &ToplevelSurface) -> Option<u64> {
    let target = surface.wl_surface();
    state
        .common
        .space
        .elements()
        .find(|w| w.wl_surface().map(|s| s.as_ref() == target).unwrap_or(false))
        .and_then(|w| w.user_data().get::<ShellWindowId>().map(|id| id.0))
}

/// Build a `MonitorInfo` for the output containing the window. Falls
/// back to the first output if the window's centre isn't on any output.
fn monitor_for_window(state: &State, id: u64) -> Option<crate::shell::MonitorInfo> {
    let centre = {
        let g = state.common.shell.window(id)?.geometry;
        smithay::utils::Point::<i32, smithay::utils::Logical>::from((
            g.loc.x + g.size.w / 2,
            g.loc.y + g.size.h / 2,
        ))
    };
    let mut fallback: Option<crate::shell::MonitorInfo> = None;
    for output in state.common.space.outputs() {
        let geo = state.common.space.output_geometry(output)?;
        let info = crate::shell::MonitorInfo {
            name: output.name(),
            logical_rect: geo,
            panel_height: 0,
            dock_height: 0,
        };
        if geo.contains(centre) {
            return Some(info);
        }
        fallback.get_or_insert(info);
    }
    fallback
}

/// Push the window's `MappedWindow.geometry` into smithay's `Space`
/// (location) and tell the client to resize via xdg_toplevel.
fn sync_geometry_to_space(state: &mut State, id: u64) {
    let geo = match state.common.shell.window(id) {
        Some(w) => w.geometry,
        None => return,
    };
    let win = state
        .common
        .space
        .elements()
        .find(|w| w.user_data().get::<ShellWindowId>().map(|s| s.0) == Some(id))
        .cloned();
    if let Some(win) = win {
        state.common.space.map_element(win.clone(), geo.loc, false);
        if let smithay::desktop::WindowSurface::Wayland(toplevel) = win.underlying_surface() {
            toplevel.with_pending_state(|s| s.size = Some(geo.size));
            toplevel.send_pending_configure();
        }
    }
}

/// Mirror our shell-side state back into xdg_toplevel pending state so
/// the client gets the right `set_maximized` / `set_fullscreen` ack.
fn ack_state_to_client(surface: &ToplevelSurface, state: &State, id: u64) {
    use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::State as XdgState;
    let Some(w) = state.common.shell.window(id) else { return };
    surface.with_pending_state(|st| {
        let set = &mut st.states;
        if w.is_maximized {
            set.set(XdgState::Maximized);
        } else {
            set.unset(XdgState::Maximized);
        }
        if w.is_fullscreen {
            set.set(XdgState::Fullscreen);
        } else {
            set.unset(XdgState::Fullscreen);
        }
    });
    surface.send_pending_configure();
}

fn broadcast_state(state: &mut State, id: u64) {
    if let Some(info) = state.common.shell.window_info(id) {
        state.common.ipc.broadcast(&ipc::ShellEvent::WindowStateChanged {
            window_id: info.id,
            state: info.state,
        });
    }
}
