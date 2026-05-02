//! Window decoration handlers — xdg-decoration (SSD/CSD) + kde-server-decoration.
//!
//! We default to ServerSide for all toplevels.  The `uses_ssd` flag on
//! `WindowInfo` lets the render side know it must draw a title bar.

use smithay::{
    delegate_kde_decoration, delegate_xdg_decoration,
    reexports::wayland_protocols::xdg::decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode,
    wayland::{
        compositor::with_states,
        shell::{
            kde::decoration::{KdeDecorationHandler, KdeDecorationState},
            xdg::{decoration::XdgDecorationHandler, ToplevelSurface, XdgToplevelSurfaceData},
        },
    },
};

use crate::wayland_state::SpikeState;

/// True iff the toplevel has already had its first `send_configure()`
/// fire. We use this to suppress mid-init `send_configure()` calls from
/// the decoration handler — those would prematurely fire the deferred
/// initial configure (see `wayland/xdg_shell.rs` for why we defer).
fn initial_configure_sent(toplevel: &ToplevelSurface) -> bool {
    with_states(toplevel.wl_surface(), |states| {
        states
            .data_map
            .get::<XdgToplevelSurfaceData>()
            .map(|d| d.lock().unwrap().initial_configure_sent)
            .unwrap_or(false)
    })
}

// ──────────────────────────────────────────────────────────────────────────────
// xdg-decoration (standard protocol)
// ──────────────────────────────────────────────────────────────────────────────

impl XdgDecorationHandler for SpikeState {
    fn new_decoration(&mut self, toplevel: ToplevelSurface) {
        // Default to ServerSide — we draw our own titlebar. Clients that
        // genuinely want CSD (and respect the negotiation) will request it
        // via `request_mode(ClientSide)`. Apps like Firefox always paint
        // their own header bar internally regardless, but our SSD chrome
        // still wraps the window so the user gets consistent decorations.
        toplevel.with_pending_state(|s| {
            s.decoration_mode = Some(Mode::ServerSide);
        });
        // Don't fire send_configure if the initial configure hasn't gone
        // out yet — the deferred path in `wayland/compositor.rs` will
        // pick up our pending-state change and emit the first configure
        // with the decoration mode already populated. Firing here would
        // race the toplevel's app_id/title setup.
        if initial_configure_sent(&toplevel) {
            toplevel.send_configure();
        }
    }

    fn request_mode(&mut self, toplevel: ToplevelSurface, mode: Mode) {
        let csd = matches!(mode, Mode::ClientSide);
        toplevel.with_pending_state(|s| {
            s.decoration_mode = Some(if csd { Mode::ClientSide } else { Mode::ServerSide });
        });
        if initial_configure_sent(&toplevel) {
            toplevel.send_configure();
        }

        let wl = toplevel.wl_surface();
        if let Some(tl) = self.toplevels.iter_mut().find(|t| &t.surface == wl) {
            tl.csd = csd;
        }
    }

    fn unset_mode(&mut self, toplevel: ToplevelSurface) {
        toplevel.with_pending_state(|s| {
            s.decoration_mode = Some(Mode::ServerSide);
        });
        if initial_configure_sent(&toplevel) {
            toplevel.send_configure();
        }
        let wl = toplevel.wl_surface();
        if let Some(tl) = self.toplevels.iter_mut().find(|t| &t.surface == wl) {
            tl.csd = false;
        }
    }
}

delegate_xdg_decoration!(SpikeState);

// ──────────────────────────────────────────────────────────────────────────────
// KDE server decoration (Qt / KWin compat)
// ──────────────────────────────────────────────────────────────────────────────

impl KdeDecorationHandler for SpikeState {
    fn kde_decoration_state(&self) -> &KdeDecorationState {
        &self.kde_decoration_state
    }
}

delegate_kde_decoration!(SpikeState);
