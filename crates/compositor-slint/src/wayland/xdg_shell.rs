//! xdg-shell handler — toplevels, popups, configure.

use std::sync::{Arc, Mutex};

use smithay::{
    delegate_xdg_shell,
    utils::SERIAL_COUNTER,
    wayland::shell::xdg::{
        PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState,
    },
};
use tracing::info;

use crate::wayland_state::{ClientSurfaceData, SpikeState, ToplevelInfo};

/// Cascading window offset: each new window is placed 40px further right/down.
const CASCADE_STEP: i32 = 40;
const CASCADE_BASE: i32 = 100;

impl XdgShellHandler for SpikeState {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg_shell_state
    }

    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        let idx = self.toplevels.len() as i32;
        let x = CASCADE_BASE + idx * CASCADE_STEP;
        let y = CASCADE_BASE + idx * CASCADE_STEP;

        info!("new xdg toplevel #{} at ({},{})", idx, x, y);

        // Tell the client its initial size (content area: width × height).
        surface.with_pending_state(|s| {
            s.size = Some((800, 600).into());
        });
        surface.send_configure();

        let wl_surface = surface.wl_surface().clone();

        // Add to toplevels list.
        self.toplevels.push(ToplevelInfo {
            surface: wl_surface.clone(),
            toplevel: surface.clone(),
            x,
            y,
            pixels: Arc::new(Mutex::new(ClientSurfaceData::default())),
        });

        // Focus the new toplevel (most recently mapped = focused).
        self.active_surface = Some(wl_surface.clone());

        if let Some(kb) = self.seat.get_keyboard() {
            kb.set_focus(self, Some(wl_surface), SERIAL_COUNTER.next_serial());
        }
    }

    fn new_popup(&mut self, _surface: PopupSurface, _positioner: PositionerState) {}

    fn toplevel_destroyed(&mut self, surface: ToplevelSurface) {
        let wl = surface.wl_surface();
        info!("toplevel destroyed");

        // Push to destroyed_surfaces so the WM can start a close animation.
        self.destroyed_surfaces.push(wl.clone());

        // Keep the toplevel in `self.toplevels` until the WM close animation
        // finishes — update_windows will remove it via sweep_closed.
        // We do NOT retain-filter here; the pixel buffer stays alive for the animation.

        // Update active_surface to the most recent remaining toplevel (if any).
        // The WM will refine this when it processes the close.
        let still_mapped: Vec<_> = self.toplevels.iter()
            .filter(|t| &t.surface != wl)
            .collect();
        self.active_surface = still_mapped.last().map(|t| t.surface.clone());

        // Legacy single-surface pixel buffer: clear if it was the active one.
        if self.active_surface.is_none() {
            *self.client_pixels.lock().unwrap() = ClientSurfaceData::default();
        }
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
