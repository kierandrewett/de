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
        tracing::debug!("decoration: new_decoration → defaulting ClientSide");
        // Default to ClientSide. Modern Wayland apps almost universally
        // draw their own headerbar (libadwaita, GTK4, Qt, Firefox,
        // Chromium/Electron). Forcing SSD on them double-decorates,
        // because their headerbar is app content we can't hide. Clients
        // that genuinely want SSD ask for it via `set_mode(ServerSide)`
        // and `request_mode` below honours them. Matches anvil + cosmic.
        toplevel.with_pending_state(|s| {
            s.decoration_mode = Some(Mode::ClientSide);
        });
        let wl = toplevel.wl_surface();
        if let Some(tl) = self.toplevels.iter_mut().find(|t| &t.surface == wl) {
            tl.csd = true;
        }
        // Don't fire send_configure if the initial configure hasn't gone
        // out yet — the deferred path in `wayland/compositor.rs` will
        // pick up our pending-state change and emit the first configure
        // with the decoration mode already populated. Firing here would
        // race the toplevel's app_id/title setup.
        if toplevel.is_initial_configure_sent() {
            toplevel.send_configure();
        }
    }

    fn request_mode(&mut self, toplevel: ToplevelSurface, mode: Mode) {
        let csd = matches!(mode, Mode::ClientSide);
        tracing::debug!("decoration: request_mode csd={csd}");
        toplevel.with_pending_state(|s| {
            s.decoration_mode = Some(if csd {
                Mode::ClientSide
            } else {
                Mode::ServerSide
            });
        });
        if toplevel.is_initial_configure_sent() {
            toplevel.send_configure();
        }

        let wl = toplevel.wl_surface();
        if let Some(tl) = self.toplevels.iter_mut().find(|t| &t.surface == wl) {
            tl.csd = csd;
        }
    }

    fn unset_mode(&mut self, toplevel: ToplevelSurface) {
        // Client withdrew its preference — fall back to our default
        // (ClientSide).
        toplevel.with_pending_state(|s| {
            s.decoration_mode = Some(Mode::ClientSide);
        });
        if toplevel.is_initial_configure_sent() {
            toplevel.send_configure();
        }
        let wl = toplevel.wl_surface();
        if let Some(tl) = self.toplevels.iter_mut().find(|t| &t.surface == wl) {
            tl.csd = true;
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

    fn new_decoration(
        &mut self,
        surface: &smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
        decoration: &wayland_protocols_misc::server_decoration::server::org_kde_kwin_server_decoration::OrgKdeKwinServerDecoration,
    ) {
        // Default to Client. Same rationale as xdg-decoration above: most
        // modern Qt/KDE apps draw their own decorations. Apps that want
        // SSD ask explicitly via `request_mode(Server)`.
        use wayland_protocols_misc::server_decoration::server::org_kde_kwin_server_decoration::Mode;
        decoration.mode(Mode::Client);
        if let Some(tl) = self.toplevels.iter_mut().find(|t| &t.surface == surface) {
            tl.csd = true;
        }
    }

    fn request_mode(
        &mut self,
        surface: &smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
        decoration: &wayland_protocols_misc::server_decoration::server::org_kde_kwin_server_decoration::OrgKdeKwinServerDecoration,
        mode: smithay::reexports::wayland_server::WEnum<
            wayland_protocols_misc::server_decoration::server::org_kde_kwin_server_decoration::Mode,
        >,
    ) {
        // Honour an explicit Server request; treat anything else as
        // Client (our default).
        use smithay::reexports::wayland_server::WEnum;
        use wayland_protocols_misc::server_decoration::server::org_kde_kwin_server_decoration::Mode;
        let resolved = match mode {
            WEnum::Value(Mode::Server) => Mode::Server,
            _ => Mode::Client,
        };
        decoration.mode(resolved);
        let csd = matches!(resolved, Mode::Client);
        if let Some(tl) = self.toplevels.iter_mut().find(|t| &t.surface == surface) {
            tl.csd = csd;
        }
    }
}

delegate_kde_decoration!(SpikeState);
