//! Window decoration handlers — xdg-decoration (SSD/CSD) + kde-server-decoration.
//!
//! We default to ServerSide for all toplevels.  The `uses_ssd` flag on
//! `WindowInfo` lets the render side know it must draw a title bar.

use smithay::{
    delegate_kde_decoration, delegate_xdg_decoration,
    reexports::wayland_protocols::xdg::decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode,
    wayland::shell::{
        kde::decoration::{KdeDecorationHandler, KdeDecorationState},
        xdg::{decoration::XdgDecorationHandler, ToplevelSurface},
    },
};

use crate::wayland_state::SpikeState;

// ──────────────────────────────────────────────────────────────────────────────
// xdg-decoration (standard protocol)
// ──────────────────────────────────────────────────────────────────────────────

impl XdgDecorationHandler for SpikeState {
    fn new_decoration(&mut self, toplevel: ToplevelSurface) {
        // Prefer SSD: we draw our own chrome.
        toplevel.with_pending_state(|s| {
            s.decoration_mode = Some(Mode::ServerSide);
        });
        toplevel.send_configure();
    }

    fn request_mode(&mut self, toplevel: ToplevelSurface, mode: Mode) {
        // Honour CSD requests (GTK/libadwaita insist); otherwise keep SSD.
        toplevel.with_pending_state(|s| {
            s.decoration_mode = Some(match mode {
                Mode::ClientSide => Mode::ClientSide,
                _ => Mode::ServerSide,
            });
        });
        toplevel.send_configure();
    }

    fn unset_mode(&mut self, toplevel: ToplevelSurface) {
        toplevel.with_pending_state(|s| {
            s.decoration_mode = Some(Mode::ServerSide);
        });
        toplevel.send_configure();
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
