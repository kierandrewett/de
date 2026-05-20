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
        if toplevel.is_initial_configure_sent() {
            toplevel.send_configure();
        }
    }

    fn request_mode(&mut self, toplevel: ToplevelSurface, mode: Mode) {
        let csd = matches!(mode, Mode::ClientSide);
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
        toplevel.with_pending_state(|s| {
            s.decoration_mode = Some(Mode::ServerSide);
        });
        if toplevel.is_initial_configure_sent() {
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

    fn new_decoration(
        &mut self,
        surface: &smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
        decoration: &wayland_protocols_misc::server_decoration::server::org_kde_kwin_server_decoration::OrgKdeKwinServerDecoration,
    ) {
        // Tell the Qt/KWin-style client up-front that we draw decorations
        // server-side (default mode). Without this, Qt 5/6 apps that opt
        // into kde-server-decoration but not xdg-decoration end up either
        // double-decorated or fully undecorated depending on the Qt
        // version. Pattern matches the protocol's `default_mode` semantics.
        use wayland_protocols_misc::server_decoration::server::org_kde_kwin_server_decoration::Mode;
        decoration.mode(Mode::Server);
        let _ = surface;
    }

    fn request_mode(
        &mut self,
        surface: &smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
        decoration: &wayland_protocols_misc::server_decoration::server::org_kde_kwin_server_decoration::OrgKdeKwinServerDecoration,
        mode: smithay::reexports::wayland_server::WEnum<
            wayland_protocols_misc::server_decoration::server::org_kde_kwin_server_decoration::Mode,
        >,
    ) {
        // Mirror what we do for xdg-decoration: clients may request CSD via
        // Client mode; anything else gets ServerSide. Apps that don't
        // honour our reply (Qt5 on some versions) will end up double-
        // decorated, which is preferable to undecorated.
        use smithay::reexports::wayland_server::WEnum;
        use wayland_protocols_misc::server_decoration::server::org_kde_kwin_server_decoration::Mode;
        let resolved = match mode {
            WEnum::Value(Mode::Client) => Mode::Client,
            _ => Mode::Server,
        };
        decoration.mode(resolved);
        // Mirror into our ToplevelInfo so the chrome renderer knows whether
        // to draw an SSD titlebar over this surface.
        let csd = matches!(resolved, Mode::Client);
        if let Some(tl) = self.toplevels.iter_mut().find(|t| &t.surface == surface) {
            tl.csd = csd;
        }
    }
}

delegate_kde_decoration!(SpikeState);
