//! Window decoration handlers — XDG decoration (SSD/CSD) and KDE server decoration.

use smithay::{
    delegate_kde_decoration, delegate_xdg_decoration,
    reexports::wayland_protocols::xdg::decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode,
    wayland::shell::{
        kde::decoration::{KdeDecorationHandler, KdeDecorationState},
        xdg::{decoration::XdgDecorationHandler, ToplevelSurface},
    },
};

use crate::state::State;

// ─── XDG decoration (standard) ───────────────────────────────────────────────

impl XdgDecorationHandler for State {
    fn new_decoration(&mut self, toplevel: ToplevelSurface) {
        toplevel.with_pending_state(|state| {
            state.decoration_mode = Some(Mode::ServerSide);
        });
        toplevel.send_configure();
    }

    fn request_mode(&mut self, toplevel: ToplevelSurface, mode: Mode) {
        toplevel.with_pending_state(|state| {
            state.decoration_mode = Some(match mode {
                Mode::ClientSide => Mode::ClientSide,
                _ => Mode::ServerSide,
            });
        });
        toplevel.send_configure();
    }

    fn unset_mode(&mut self, toplevel: ToplevelSurface) {
        toplevel.with_pending_state(|state| {
            state.decoration_mode = Some(Mode::ServerSide);
        });
        toplevel.send_configure();
    }
}

delegate_xdg_decoration!(State);

// ─── KDE server decoration (Qt/KWin compat) ──────────────────────────────────

impl KdeDecorationHandler for State {
    fn kde_decoration_state(&self) -> &KdeDecorationState {
        &self.common.kde_decoration_state
    }
}

delegate_kde_decoration!(State);
