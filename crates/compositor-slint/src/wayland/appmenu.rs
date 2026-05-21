//! `org_kde_kwin_appmenu` handler — the Wayland-native half of KDE-style
//! global menus.
//!
//! A client creates an `org_kde_kwin_appmenu` for one of its surfaces and
//! calls `set_address(service_name, object_path)` pointing at the
//! `com.canonical.dbusmenu` interface it exports over D-Bus. We stash that
//! address on the matching `ToplevelInfo`; the renderer fetches + renders it
//! in the panel for whichever window has keyboard focus.
//!
//! Raw `wayland-server` dispatch (no smithay delegate macro — this is a
//! vendored, non-smithay protocol).

use smithay::reexports::wayland_server::{
    self, protocol::wl_surface::WlSurface, Client, DataInit, Dispatch, DisplayHandle,
    GlobalDispatch, New, Resource,
};
use wayland_server::backend::GlobalId;

use crate::wayland::appmenu_protocol::org_kde_kwin_appmenu::{
    self, OrgKdeKwinAppmenu,
};
use crate::wayland::appmenu_protocol::org_kde_kwin_appmenu_manager::{
    self, OrgKdeKwinAppmenuManager,
};
use crate::wayland_state::SpikeState;

/// Holds the `org_kde_kwin_appmenu_manager` global.
#[derive(Debug)]
pub struct AppmenuManagerState {
    #[allow(dead_code)]
    global: GlobalId,
}

impl AppmenuManagerState {
    pub fn new(dh: &DisplayHandle) -> Self {
        let global = dh.create_global::<SpikeState, OrgKdeKwinAppmenuManager, ()>(2, ());
        Self { global }
    }
}

impl GlobalDispatch<OrgKdeKwinAppmenuManager, ()> for SpikeState {
    fn bind(
        _state: &mut Self,
        _dh: &DisplayHandle,
        _client: &Client,
        resource: New<OrgKdeKwinAppmenuManager>,
        _global_data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }
}

impl Dispatch<OrgKdeKwinAppmenuManager, ()> for SpikeState {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _resource: &OrgKdeKwinAppmenuManager,
        request: org_kde_kwin_appmenu_manager::Request,
        _data: &(),
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            org_kde_kwin_appmenu_manager::Request::Create { id, surface } => {
                // The per-appmenu user data is the surface it decorates, so
                // `set_address` can find the right ToplevelInfo.
                data_init.init(id, surface);
            }
            org_kde_kwin_appmenu_manager::Request::Release => {}
        }
    }
}

impl Dispatch<OrgKdeKwinAppmenu, WlSurface> for SpikeState {
    fn request(
        state: &mut Self,
        _client: &Client,
        _resource: &OrgKdeKwinAppmenu,
        request: org_kde_kwin_appmenu::Request,
        surface: &WlSurface,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            org_kde_kwin_appmenu::Request::SetAddress {
                service_name,
                object_path,
            } => {
                tracing::info!(
                    service = %service_name,
                    path = %object_path,
                    "appmenu: client registered a global menu"
                );
                if let Some(tl) = state.toplevels.iter_mut().find(|t| &t.surface == surface) {
                    tl.appmenu = Some((service_name, object_path));
                } else {
                    // Surface mapped after the appmenu object — store on the
                    // pending map so the toplevel adopts it when it appears.
                    state
                        .pending_appmenu
                        .insert(surface.id(), (service_name, object_path));
                }
            }
            org_kde_kwin_appmenu::Request::Release => {
                if let Some(tl) = state.toplevels.iter_mut().find(|t| &t.surface == surface) {
                    tl.appmenu = None;
                }
                state.pending_appmenu.remove(&surface.id());
            }
        }
    }
}
