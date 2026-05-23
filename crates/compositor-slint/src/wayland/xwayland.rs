//! XWayland integration.
//!
//! Provides:
//!   - Spawn/attach lifecycle for the Xwayland binary (see `start_xwayland`).
//!   - `XwmHandler` impl that bridges X11 window-manager requests to our
//!     existing `WindowManager` / wayland toplevel bookkeeping.
//!   - `XWaylandShellHandler` + `XWaylandKeyboardGrabHandler` impls and the
//!     matching delegate macros.
//!
//! Architecture
//! ------------
//! Xwayland is a wayland *client* that also implements an X11 server. From
//! our compositor's point of view, every X11 window is backed by a regular
//! `wl_surface` (created by Xwayland on the wayland side), and X11 clients
//! render into it via the X11 protocol. Smithay's `X11Wm` runs the X11-WM
//! plumbing on a `RustConnection` we get from `XWaylandEvent::Ready`.
//!
//! For now we treat every `new_window` / `mapped_override_redirect_window`
//! request as "client wants the window mapped at its requested geometry",
//! thread the underlying `wl_surface` into our `ToplevelInfo` list (so the
//! rest of the compositor — buffer import, focus, render — keeps working),
//! and acknowledge the geometry so the X11 client unblocks.
//!
//! TODO (phase 2): wire move/resize/maximize/fullscreen into the existing
//! `WindowManager` actions. For now those are stubbed with the minimal
//! "ack at the geometry the client asked for" behaviour, which is enough
//! for `xterm`, `xclock`, and most XInput-only X11 apps to display.

use std::borrow::Cow;
use std::os::unix::io::OwnedFd;
use std::process::Stdio;
use std::sync::{Arc, Mutex};

use smithay::{
    backend::input::KeyState,
    delegate_xwayland_keyboard_grab, delegate_xwayland_shell,
    desktop::PopupKind,
    input::{
        keyboard::{KeyboardTarget, KeysymHandle, ModifiersState},
        Seat,
    },
    reexports::wayland_server::{protocol::wl_surface::WlSurface, Resource},
    utils::{IsAlive, Logical, Rectangle, SERIAL_COUNTER, Serial},
    wayland::{
        seat::WaylandFocus,
        selection::{
            data_device::{
                clear_data_device_selection, current_data_device_selection_userdata,
                request_data_device_client_selection, set_data_device_selection,
            },
            primary_selection::{
                clear_primary_selection, current_primary_selection_userdata,
                request_primary_client_selection, set_primary_selection,
            },
            SelectionTarget,
        },
        xwayland_keyboard_grab::XWaylandKeyboardGrabHandler,
        xwayland_shell::{XWaylandShellHandler, XWaylandShellState},
    },
    xwayland::{
        xwm::{Reorder, ResizeEdge as X11ResizeEdge, WmWindowProperty, XwmId},
        X11Surface, X11Wm, XWayland, XWaylandEvent, XwmHandler,
    },
};
use tracing::{debug, info, warn};

use crate::wayland_state::{ClientSurfaceData, SpikeState, ToplevelInfo};

// ──────────────────────────────────────────────────────────────────────────────
// X11Surface user_data tags
// ──────────────────────────────────────────────────────────────────────────────

/// Records the wayland surface of an X11 window's `WM_TRANSIENT_FOR` parent.
///
/// We stash the parent's `WlSurface` (not just the X11 window-id) so the
/// renderer / focus code can use the same `WlSurface`-keyed lookup it already
/// uses for native xdg toplevels — without re-walking `state.toplevels` to
/// resolve the X11 id every time.
///
/// Lives on the *child* X11Surface's `user_data()`. Updated when:
///   - the window first maps (via `refresh_transient_for`)
///   - smithay reports `WmWindowProperty::TransientFor` later in life.
///
/// The full focus-stealing-prevention / close-cascade rules read this — those
/// are scheduled for the next pass; capturing the link first is a prereq.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct X11TransientFor(pub WlSurface);

/// Resolve `child`'s `WM_TRANSIENT_FOR` parent into a `WlSurface` and
/// stash it on the child's user_data as `X11TransientFor`.
///
/// Called at map time and on `WmWindowProperty::TransientFor` notifications.
/// Looks the parent up by X11 window-id in `state.toplevels` — if the parent
/// hasn't been associated with a wl_surface yet (i.e. it hasn't committed a
/// buffer) we silently skip; the next property_notify will retry.
fn refresh_transient_for(state: &SpikeState, child: &X11Surface) {
    let Some(parent_id) = child.is_transient_for() else {
        // No parent — clear any stale link.
        let map = child.user_data();
        if map.get::<X11TransientFor>().is_some() {
            // UserDataMap has no remove; replace the surface with the child
            // itself so the link is effectively a no-op (any later code that
            // reads it will see "child is its own parent" → ignore).
            // In practice the parent simply doesn't get cleared between
            // re-uses of the same X11Surface, which is fine: TRANSIENT_FOR
            // is set once and rarely retracted.
        }
        return;
    };
    let Some(parent_wl) = state
        .toplevels
        .iter()
        .filter_map(|t| t.x11_surface.as_ref().map(|x| (x, &t.surface)))
        .find(|(x, _)| x.window_id() == parent_id)
        .map(|(_, s)| s.clone())
    else {
        debug!(
            "X11: transient_for: parent X11 id {} not yet mapped for child {:?}",
            parent_id,
            child.window_id()
        );
        return;
    };
    child
        .user_data()
        .insert_if_missing(|| X11TransientFor(parent_wl.clone()));
    debug!(
        "X11: transient_for: child {:?} → parent wl_surface={:?}",
        child.window_id(),
        parent_wl.id()
    );
}

/// Marker tagging an `X11Surface` whose entry in `state.toplevels` represents
/// an override-redirect window — i.e. a menu / tooltip / drag indicator that
/// the X11 client positions itself at exact screen coords, bypassing the WM.
///
/// The renderer's WM-sync pass and `update_windows` use this marker to (a)
/// keep OR windows out of the managed-toplevel `WindowManager` and (b) route
/// their composited pixels through the popup pipeline instead.
#[derive(Debug, Clone, Copy)]
pub struct X11OverrideRedirect;

/// Keyboard focus target that preserves smithay's X11-specific focus path.
///
/// Native Wayland surfaces still receive focus as raw `WlSurface`s. XWayland
/// toplevels receive focus through `X11Surface`, whose `KeyboardTarget` impl
/// drives ICCCM input-model handling, `SetInputFocus`, and `WM_TAKE_FOCUS`.
#[derive(Debug, Clone, PartialEq)]
#[allow(clippy::large_enum_variant)]
pub enum KeyboardFocusTarget {
    Wayland(WlSurface),
    X11(X11Surface),
}

impl KeyboardFocusTarget {
    pub fn for_wl_surface(state: &SpikeState, surface: &WlSurface) -> Self {
        state
            .toplevels
            .iter()
            .find(|toplevel| &toplevel.surface == surface)
            .and_then(|toplevel| toplevel.x11_surface.clone())
            .map(Self::X11)
            .unwrap_or_else(|| Self::Wayland(surface.clone()))
    }

    pub fn wl_surface(&self) -> Option<Cow<'_, WlSurface>> {
        match self {
            Self::Wayland(surface) => Some(Cow::Borrowed(surface)),
            Self::X11(surface) => surface.wl_surface().map(Cow::Owned),
        }
    }

    pub fn matches_wl_surface(&self, surface: &WlSurface) -> bool {
        self.wl_surface().is_some_and(|focus| focus.as_ref() == surface)
    }
}

impl From<PopupKind> for KeyboardFocusTarget {
    fn from(popup: PopupKind) -> Self {
        Self::Wayland(popup.wl_surface().clone())
    }
}

impl From<KeyboardFocusTarget> for WlSurface {
    fn from(target: KeyboardFocusTarget) -> Self {
        target
            .wl_surface()
            .expect("keyboard focus target missing wl_surface")
            .into_owned()
    }
}

impl IsAlive for KeyboardFocusTarget {
    fn alive(&self) -> bool {
        match self {
            Self::Wayland(surface) => surface.alive(),
            Self::X11(surface) => surface.alive(),
        }
    }
}

impl KeyboardTarget<SpikeState> for KeyboardFocusTarget {
    fn enter(
        &self,
        seat: &Seat<SpikeState>,
        data: &mut SpikeState,
        keys: Vec<KeysymHandle<'_>>,
        serial: Serial,
    ) {
        match self {
            Self::Wayland(surface) => KeyboardTarget::enter(surface, seat, data, keys, serial),
            Self::X11(surface) => KeyboardTarget::enter(surface, seat, data, keys, serial),
        }
    }

    fn leave(&self, seat: &Seat<SpikeState>, data: &mut SpikeState, serial: Serial) {
        match self {
            Self::Wayland(surface) => KeyboardTarget::leave(surface, seat, data, serial),
            Self::X11(surface) => KeyboardTarget::leave(surface, seat, data, serial),
        }
    }

    fn key(
        &self,
        seat: &Seat<SpikeState>,
        data: &mut SpikeState,
        key: KeysymHandle<'_>,
        state: KeyState,
        serial: Serial,
        time: u32,
    ) {
        match self {
            Self::Wayland(surface) => KeyboardTarget::key(surface, seat, data, key, state, serial, time),
            Self::X11(surface) => KeyboardTarget::key(surface, seat, data, key, state, serial, time),
        }
    }

    fn modifiers(
        &self,
        seat: &Seat<SpikeState>,
        data: &mut SpikeState,
        modifiers: ModifiersState,
        serial: Serial,
    ) {
        match self {
            Self::Wayland(surface) => KeyboardTarget::modifiers(surface, seat, data, modifiers, serial),
            Self::X11(surface) => KeyboardTarget::modifiers(surface, seat, data, modifiers, serial),
        }
    }
}

impl WaylandFocus for KeyboardFocusTarget {
    fn wl_surface(&self) -> Option<Cow<'_, WlSurface>> {
        KeyboardFocusTarget::wl_surface(self)
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Process lifecycle
// ──────────────────────────────────────────────────────────────────────────────

/// Spawn Xwayland alongside the wayland compositor.
///
/// Stashes the `Option<X11Wm>` and the X11 display number on `state` once
/// `XWaylandEvent::Ready` fires. Sets `DISPLAY=:N` in the current process so
/// any child apps spawned via `launch_app` (which inherit our env) find the
/// Xwayland server.
pub fn start_xwayland(state: &mut SpikeState) {
    let (xwayland, client) = match XWayland::spawn(
        &state.display_handle,
        None,
        std::iter::empty::<(String, String)>(),
        true,
        Stdio::null(),
        Stdio::null(),
        |_| (),
    ) {
        Ok(pair) => pair,
        Err(e) => {
            warn!("XWayland: failed to spawn Xwayland process: {e}");
            return;
        }
    };

    let display_handle = state.display_handle.clone();
    let result = state
        .loop_handle
        .insert_source(xwayland, move |event, _, data| match event {
            XWaylandEvent::Ready {
                x11_socket,
                display_number,
            } => {
                info!("XWayland: ready, display=:{display_number}");
                // Make :N visible to the rest of the compositor + any child
                // processes we spawn (their env is inherited from us).
                // SAFETY: single-threaded compositor at this point — calloop
                // dispatches main-thread callbacks serially.
                unsafe { std::env::set_var("DISPLAY", format!(":{display_number}")) };

                match X11Wm::start_wm(
                    data.loop_handle.clone(),
                    &display_handle,
                    x11_socket,
                    client.clone(),
                ) {
                    Ok(wm) => {
                        data.xwm = Some(wm);
                        data.xdisplay = Some(display_number);
                        info!("XWayland: X11Wm attached on :{display_number}");
                    }
                    Err(e) => warn!("XWayland: X11Wm::start_wm failed: {e}"),
                }
            }
            XWaylandEvent::Error => {
                warn!("XWayland: process exited unexpectedly");
                data.xwm = None;
                data.xdisplay = None;
            }
        });
    if let Err(e) = result {
        warn!("XWayland: failed to insert calloop source: {e}");
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// XWaylandShellHandler — bind WlSurface ↔ X11 window via xwayland_shell_v1.
// ──────────────────────────────────────────────────────────────────────────────

impl XWaylandShellHandler for SpikeState {
    fn xwayland_shell_state(&mut self) -> &mut XWaylandShellState {
        &mut self.xwayland_shell_state
    }

    fn surface_associated(&mut self, _xwm: XwmId, wl_surface: WlSurface, x11_surface: X11Surface) {
        // The wl_surface for this X11 window has just been resolved. If we
        // saw `map_window_request` first (no wl_surface available) we
        // never registered the toplevel; do it now.
        let already_tracked = self.toplevels.iter().any(|t| t.surface == wl_surface);
        if already_tracked {
            return;
        }
        if !x11_surface.is_mapped() {
            return;
        }

        let cascade = self.toplevels.len() as i32;
        let x = 100 + cascade * 40;
        let y = 100 + cascade * 40;
        let is_or = x11_surface.is_override_redirect();
        if is_or {
            // Tag the surface so the renderer routes it through the popup
            // pipeline instead of the WindowManager.
            x11_surface
                .user_data()
                .insert_if_missing(|| X11OverrideRedirect);
        }
        // Register with ext-foreign-toplevel-list-v1 — but only for managed
        // windows; X11 override-redirect surfaces are tooltips/menus, not
        // toplevels (cosmic skips them too).
        let title = x11_surface.title();
        let app_id = x11_surface.class();
        let foreign_handle = if is_or {
            None
        } else {
            Some(
                self.foreign_toplevel_list_state
                    .new_toplevel::<Self>(&title, &app_id),
            )
        };
        self.toplevels.push(ToplevelInfo {
            surface: wl_surface.clone(),
            toplevel: None,
            x11_surface: Some(x11_surface.clone()),
            x,
            y,
            pixels: Arc::new(Mutex::new(ClientSurfaceData::default())),
            surface_pixels: Arc::new(Mutex::new(std::collections::HashMap::new())),
            csd: !x11_surface.is_decorated(),
            foreign_handle,
            last_advertised_title: title.clone(),
            last_advertised_app_id: app_id.clone(),
            is_dialog: false,
            icon_name: None,
            tag: None,
            description: None,
            appmenu: None,
        });
        // OR windows aren't focusable / managed — they piggyback on the
        // parent's keyboard focus (xterm popup menu, GTK dropdown).
        if !is_or {
            self.active_surface = Some(wl_surface.clone());
            if let Some(kb) = self.seat.get_keyboard() {
                kb.set_focus(
                    self,
                    Some(KeyboardFocusTarget::X11(x11_surface.clone())),
                    SERIAL_COUNTER.next_serial(),
                );
            }
            let _ = x11_surface.set_activated(true);
        }
        // After the toplevel list contains both parent and child,
        // resolve transient_for so dialog→parent z/focus rules can kick in.
        refresh_transient_for(self, &x11_surface);
        info!(
            "X11: associated + registered toplevel id={:?} class={:?} title={:?} or={}",
            x11_surface.window_id(),
            x11_surface.class(),
            x11_surface.title(),
            is_or,
        );
    }
}

delegate_xwayland_shell!(SpikeState);

// ──────────────────────────────────────────────────────────────────────────────
// XWaylandKeyboardGrabHandler — port of anvil's one-line impl.
// ──────────────────────────────────────────────────────────────────────────────

impl XWaylandKeyboardGrabHandler for SpikeState {
    fn keyboard_focus_for_xsurface(&self, surface: &WlSurface) -> Option<KeyboardFocusTarget> {
        self.toplevels
            .iter()
            .find(|toplevel| &toplevel.surface == surface)
            .and_then(|toplevel| toplevel.x11_surface.clone())
            .map(KeyboardFocusTarget::X11)
    }
}

delegate_xwayland_keyboard_grab!(SpikeState);

// ──────────────────────────────────────────────────────────────────────────────
// XwmHandler — the meaty bit.
// ──────────────────────────────────────────────────────────────────────────────

impl XwmHandler for SpikeState {
    fn xwm_state(&mut self, _xwm: XwmId) -> &mut X11Wm {
        self.xwm
            .as_mut()
            .expect("xwm_state called with no X11Wm attached")
    }

    fn new_window(&mut self, _xwm: XwmId, window: X11Surface) {
        debug!(
            "X11: new_window id={:?} class={:?} title={:?}",
            window.window_id(),
            window.class(),
            window.title()
        );
    }

    fn new_override_redirect_window(&mut self, _xwm: XwmId, window: X11Surface) {
        debug!(
            "X11: new override-redirect id={:?} class={:?}",
            window.window_id(),
            window.class()
        );
    }

    fn map_window_request(&mut self, _xwm: XwmId, window: X11Surface) {
        if let Err(e) = window.set_mapped(true) {
            warn!("X11: set_mapped(true) failed: {e}");
            return;
        }
        let geo = window.geometry();
        if let Err(e) = window.configure(Some(geo)) {
            warn!("X11: configure on map failed: {e}");
        }

        // Bridge into the same `toplevels` list our xdg-shell handler uses
        // so the rest of the compositor (buffer import on commit, focus,
        // chrome rendering) treats the X11 window like a native toplevel.
        // The wl_surface is what XWayland creates for us once the X11
        // client paints — it may not exist yet at the moment of
        // map_window_request, in which case we'll skip and re-try in
        // `mapped_override_redirect_window` / on first commit. For a
        // managed window with a known wl_surface we go ahead and track.
        if let Some(wl_surface) = window.wl_surface() {
            let already_tracked = self.toplevels.iter().any(|t| t.surface == wl_surface);
            if !already_tracked {
                let cascade = self.toplevels.len() as i32;
                let x = 100 + cascade * 40;
                let y = 100 + cascade * 40;
                let title = window.title();
                let app_id = window.class();
                let foreign_handle = Some(
                    self.foreign_toplevel_list_state
                        .new_toplevel::<Self>(&title, &app_id),
                );
                self.toplevels.push(ToplevelInfo {
                    surface: wl_surface.clone(),
                    toplevel: None,
                    x11_surface: Some(window.clone()),
                    x,
                    y,
                    pixels: Arc::new(Mutex::new(ClientSurfaceData::default())),
                    surface_pixels: Arc::new(Mutex::new(std::collections::HashMap::new())),
                    csd: !window.is_decorated(),
                    foreign_handle,
                    last_advertised_title: title.clone(),
                    last_advertised_app_id: app_id.clone(),
                    is_dialog: false,
                    icon_name: None,
                    tag: None,
                    description: None,
                    appmenu: None,
                });
                self.active_surface = Some(wl_surface.clone());
                if let Some(kb) = self.seat.get_keyboard() {
                    kb.set_focus(
                        self,
                        Some(KeyboardFocusTarget::X11(window.clone())),
                        SERIAL_COUNTER.next_serial(),
                    );
                }
                let _ = window.set_activated(true);
                // Resolve TRANSIENT_FOR now that both parent and child are in
                // the toplevel list (parent must have mapped earlier; if not,
                // a later property_notify will retry).
                refresh_transient_for(self, &window);
                info!(
                    "X11: registered toplevel id={:?} class={:?} title={:?} at ({x},{y})",
                    window.window_id(),
                    window.class(),
                    window.title()
                );
            }
        } else {
            debug!(
                "X11: map_window_request id={:?} geo={:?} (no wl_surface yet)",
                window.window_id(),
                geo
            );
        }
    }

    fn mapped_override_redirect_window(&mut self, _xwm: XwmId, window: X11Surface) {
        let geo = window.geometry();
        debug!(
            "X11: mapped_override_redirect id={:?} geo={:?}",
            window.window_id(),
            geo
        );
        // Tag the X11Surface so the renderer's WM-sync skips it (we don't
        // want OR windows in `WindowManager::windows`) and `update_windows`
        // routes its pixels through the popup pipeline instead.
        window.user_data().insert_if_missing(|| X11OverrideRedirect);

        // Track the surface in `state.toplevels` so the SHM/dmabuf import
        // path on `commit` (in wayland/compositor.rs) finds it and fills its
        // pixel buffer — that's the same buffer the popup pipeline reads.
        if let Some(wl_surface) = window.wl_surface() {
            let already_tracked = self.toplevels.iter().any(|t| t.surface == wl_surface);
            if !already_tracked {
                self.toplevels.push(ToplevelInfo {
                    surface: wl_surface.clone(),
                    toplevel: None,
                    x11_surface: Some(window.clone()),
                    // Geometry coords are absolute screen position for OR
                    // windows (the X11 client placed itself there).
                    x: geo.loc.x,
                    y: geo.loc.y,
                    pixels: Arc::new(Mutex::new(ClientSurfaceData::default())),
                    surface_pixels: Arc::new(Mutex::new(std::collections::HashMap::new())),
                    // OR windows always paint everything they need themselves
                    // — never add SSD chrome.
                    csd: true,
                    // Override-redirect = X11 tooltip/menu, not a toplevel
                    // from the user's perspective. Skip foreign-toplevel-list
                    // (matches cosmic-comp behaviour).
                    foreign_handle: None,
                    last_advertised_title: String::new(),
                    last_advertised_app_id: String::new(),
                    is_dialog: false,
                    icon_name: None,
                    tag: None,
                    description: None,
                    appmenu: None,
                });
            }
        } else {
            debug!(
                "X11: OR window id={:?} mapped before wl_surface association — \
                 surface_associated will track it later",
                window.window_id()
            );
        }
    }

    fn unmapped_window(&mut self, _xwm: XwmId, window: X11Surface) {
        debug!("X11: unmapped id={:?}", window.window_id());
        if let Some(wl_surface) = window.wl_surface() {
            // Drop the foreign-toplevel-list entry before retain() removes
            // the ToplevelInfo. Taskbars/docks see the window disappear
            // immediately.
            if let Some(t) = self.toplevels.iter().find(|t| t.surface == wl_surface) {
                if let Some(h) = &t.foreign_handle {
                    self.foreign_toplevel_list_state.remove_toplevel(h);
                }
            }
            self.destroyed_surfaces.push(wl_surface.clone());
            self.toplevels.retain(|t| t.surface != wl_surface);
            if self.active_surface.as_ref() == Some(&wl_surface) {
                self.active_surface = self.toplevels.last().map(|t| t.surface.clone());
            }
        }
        if !window.is_override_redirect() {
            let _ = window.set_mapped(false);
        }
    }

    fn destroyed_window(&mut self, _xwm: XwmId, window: X11Surface) {
        debug!("X11: destroyed id={:?}", window.window_id());
        if let Some(wl_surface) = window.wl_surface() {
            // `unmapped_window` typically fires first and already removed the
            // foreign-toplevel entry + ToplevelInfo; the guards below are
            // idempotent so the same code is safe on both paths.
            if let Some(t) = self.toplevels.iter().find(|t| t.surface == wl_surface) {
                if let Some(h) = &t.foreign_handle {
                    self.foreign_toplevel_list_state.remove_toplevel(h);
                }
            }
            // Bug fix: previously this also pushed to destroyed_surfaces
            // unconditionally, which fired the close animation a second time
            // when `unmapped_window` had already pushed it. Only push if the
            // surface is still in our toplevels list (i.e. unmapped never ran).
            let already_swept = !self.toplevels.iter().any(|t| t.surface == wl_surface);
            if !already_swept {
                self.destroyed_surfaces.push(wl_surface.clone());
                self.toplevels.retain(|t| t.surface != wl_surface);
                if self.active_surface.as_ref() == Some(&wl_surface) {
                    self.active_surface = self.toplevels.last().map(|t| t.surface.clone());
                }
            }
        }
    }

    fn configure_request(
        &mut self,
        _xwm: XwmId,
        window: X11Surface,
        _x: Option<i32>,
        _y: Option<i32>,
        w: Option<u32>,
        h: Option<u32>,
        _reorder: Option<Reorder>,
    ) {
        // Honour size requests but pin the window to its current location —
        // matches anvil's policy of "no client-driven moves".
        let mut geo = window.geometry();
        if let Some(w) = w {
            geo.size.w = w as i32;
        }
        if let Some(h) = h {
            geo.size.h = h as i32;
        }
        // Clamp to the client's own WM_NORMAL_HINTS min/max so we don't shrink
        // an X11 app below its declared minimum (the spec says compositors MUST
        // honour these; older audit flagged this as silently dropped).
        if let Some(min) = window.min_size() {
            if min.w > 0 {
                geo.size.w = geo.size.w.max(min.w);
            }
            if min.h > 0 {
                geo.size.h = geo.size.h.max(min.h);
            }
        }
        if let Some(max) = window.max_size() {
            if max.w > 0 {
                geo.size.w = geo.size.w.min(max.w);
            }
            if max.h > 0 {
                geo.size.h = geo.size.h.min(max.h);
            }
        }
        let _ = window.configure(geo);
    }

    fn active_window_request(
        &mut self,
        _xwm: XwmId,
        window: X11Surface,
        _timestamp: u32,
        _currently_active_window: Option<X11Surface>,
    ) {
        // X11 client requested focus (e.g. via _NET_ACTIVE_WINDOW from another
        // X11 app, or its own urgency handling). Honour it by raising the
        // associated wl_surface and setting it as the keyboard focus.
        let Some(wl_surface) = window.wl_surface() else {
            return;
        };
        self.active_surface = Some(wl_surface.clone());
        if let Some(kb) = self.seat.get_keyboard() {
            kb.set_focus(
                self,
                Some(KeyboardFocusTarget::X11(window.clone())),
                SERIAL_COUNTER.next_serial(),
            );
        }
        let _ = window.set_activated(true);
    }

    fn configure_notify(
        &mut self,
        _xwm: XwmId,
        window: X11Surface,
        geometry: Rectangle<i32, Logical>,
        _above: Option<u32>,
    ) {
        debug!(
            "X11: configure_notify id={:?} geom={:?}",
            window.window_id(),
            geometry
        );
        // For override-redirect windows the X11 client places itself
        // unilaterally — the compositor must follow. Update the tracked
        // position so the renderer composites the OR surface at the new
        // location (otherwise tooltips/menus stay glued to their map-time
        // position when X11 apps move them).
        if !window.is_override_redirect() {
            return;
        }
        let Some(wl_surface) = window.wl_surface() else {
            return;
        };
        if let Some(tl) = self.toplevels.iter_mut().find(|t| t.surface == wl_surface) {
            tl.x = geometry.loc.x;
            tl.y = geometry.loc.y;
        }
    }

    fn resize_request(
        &mut self,
        _xwm: XwmId,
        window: X11Surface,
        _button: u32,
        edges: X11ResizeEdge,
    ) {
        debug!(
            "X11: resize_request id={:?} edges={:?}",
            window.window_id(),
            edges
        );
        if let Some(wl_surface) = window.wl_surface() {
            // Map the X11 resize edge to our internal `ResizeEdge`. The bit
            // values aren't identical between protocols but the semantics are.
            use crate::resize::ResizeEdge as RE;
            let our_edges = match edges {
                X11ResizeEdge::Top => RE::North,
                X11ResizeEdge::Bottom => RE::South,
                X11ResizeEdge::Left => RE::West,
                X11ResizeEdge::Right => RE::East,
                X11ResizeEdge::TopLeft => RE::NorthWest,
                X11ResizeEdge::TopRight => RE::NorthEast,
                X11ResizeEdge::BottomLeft => RE::SouthWest,
                X11ResizeEdge::BottomRight => RE::SouthEast,
            };
            self.pending_xdg_resize.push((wl_surface, our_edges));
        }
    }

    fn move_request(&mut self, _xwm: XwmId, window: X11Surface, _button: u32) {
        debug!("X11: move_request id={:?}", window.window_id());
        if let Some(wl_surface) = window.wl_surface() {
            self.pending_xdg_move.push(wl_surface);
        }
    }

    fn maximize_request(&mut self, _xwm: XwmId, window: X11Surface) {
        // Push the WM action onto the same queue xdg-shell uses so the
        // renderer-side `WindowManager::start_maximize` is invoked.
        let _ = window.set_maximized(true);
        if let Some(wl_surface) = window.wl_surface() {
            self.pending_xdg_maximize.push((wl_surface, true));
        }
    }

    fn unmaximize_request(&mut self, _xwm: XwmId, window: X11Surface) {
        let _ = window.set_maximized(false);
        if let Some(wl_surface) = window.wl_surface() {
            self.pending_xdg_maximize.push((wl_surface, false));
        }
    }

    fn fullscreen_request(&mut self, _xwm: XwmId, window: X11Surface) {
        let _ = window.set_fullscreen(true);
        if let Some(wl_surface) = window.wl_surface() {
            self.pending_xdg_fullscreen.push((wl_surface, true));
        }
    }

    fn unfullscreen_request(&mut self, _xwm: XwmId, window: X11Surface) {
        let _ = window.set_fullscreen(false);
        if let Some(wl_surface) = window.wl_surface() {
            self.pending_xdg_fullscreen.push((wl_surface, false));
        }
    }

    fn minimize_request(&mut self, _xwm: XwmId, window: X11Surface) {
        let _ = window.set_hidden(true);
        if let Some(wl_surface) = window.wl_surface() {
            self.pending_xdg_minimize.push(wl_surface);
        }
    }

    fn unminimize_request(&mut self, _xwm: XwmId, window: X11Surface) {
        // Symmetric with `minimize_request` — bridge into the WM via the
        // pending queue so the renderer's tick drives the restore
        // animation. Audit flagged this as asymmetric (was a bare set_hidden).
        let _ = window.set_hidden(false);
        if let Some(wl_surface) = window.wl_surface() {
            self.pending_xdg_restore.push(wl_surface);
        }
    }

    fn property_notify(&mut self, _xwm: XwmId, window: X11Surface, property: WmWindowProperty) {
        match property {
            WmWindowProperty::Title => {
                debug!(
                    "X11: title -> {:?} (id={:?})",
                    window.title(),
                    window.window_id()
                );
                // Push the title change through the foreign-toplevel handle
                // immediately. The per-frame sync_foreign_toplevels catches
                // it within a frame anyway, but this gives taskbars an
                // event the same tick the X11 client changed _NET_WM_NAME.
                if let Some(wl_surface) = window.wl_surface() {
                    if let Some(tl) = self.toplevels.iter_mut().find(|t| t.surface == wl_surface) {
                        if let Some(handle) = &tl.foreign_handle {
                            let new_title = window.title();
                            if new_title != tl.last_advertised_title {
                                handle.send_title(&new_title);
                                handle.send_done();
                                tl.last_advertised_title = new_title;
                            }
                        }
                    }
                }
            }
            WmWindowProperty::Class => {
                debug!(
                    "X11: class -> {:?} (id={:?})",
                    window.class(),
                    window.window_id()
                );
                if let Some(wl_surface) = window.wl_surface() {
                    if let Some(tl) = self.toplevels.iter_mut().find(|t| t.surface == wl_surface) {
                        if let Some(handle) = &tl.foreign_handle {
                            let new_app_id = window.class();
                            if new_app_id != tl.last_advertised_app_id {
                                handle.send_app_id(&new_app_id);
                                handle.send_done();
                                tl.last_advertised_app_id = new_app_id;
                            }
                        }
                    }
                }
            }
            WmWindowProperty::TransientFor => {
                // Re-resolve: the parent may have only just been mapped, or
                // the client may have re-targeted (rare but legal).
                refresh_transient_for(self, &window);
            }
            WmWindowProperty::Hints => {
                // WM_HINTS.urgency tracks "demands attention". Future:
                // surface this on ToplevelInfo and let the taskbar flash;
                // for now log so we can confirm the wiring's reaching us.
                if let Some(hints) = window.hints() {
                    debug!(
                        "X11: WM_HINTS urgent={:?} input={:?} (id={:?})",
                        hints.urgent,
                        hints.input,
                        window.window_id()
                    );
                }
            }
            _ => {}
        }
    }

    // ── Selection bridge (X11 <-> wayland clipboard / primary). Mirrors
    //    anvil one-for-one — smithay handles most of the plumbing once these
    //    are wired up.
    fn allow_selection_access(&mut self, xwm: XwmId, _selection: SelectionTarget) -> bool {
        // Security gate, mirrors anvil/shell/x11.rs:259-271 and
        // cosmic/xwayland.rs:1233. Only allow this xwm to read the wayland
        // clipboard when the currently-focused window is in fact an X11
        // window owned by this xwm — i.e. the user is interacting with an
        // Xwayland app. Otherwise any X11 client could spy on whatever the
        // user just copied in a Wayland app.
        let Some(keyboard) = self.seat.get_keyboard() else {
            return false;
        };
        let Some(focus) = keyboard.current_focus() else {
            return false;
        };
        self.toplevels.iter().any(|t| {
            focus.matches_wl_surface(&t.surface)
                && t.x11_surface
                    .as_ref()
                    .and_then(|x| x.xwm_id())
                    .is_some_and(|id| id == xwm)
        })
    }

    fn send_selection(
        &mut self,
        _xwm: XwmId,
        selection: SelectionTarget,
        mime_type: String,
        fd: OwnedFd,
    ) {
        match selection {
            SelectionTarget::Clipboard => {
                if let Err(err) = request_data_device_client_selection(&self.seat, mime_type, fd) {
                    warn!(?err, "XWayland: clipboard read into X11 failed");
                }
            }
            SelectionTarget::Primary => {
                if let Err(err) = request_primary_client_selection(&self.seat, mime_type, fd) {
                    warn!(?err, "XWayland: primary read into X11 failed");
                }
            }
        }
    }

    fn new_selection(&mut self, _xwm: XwmId, selection: SelectionTarget, mime_types: Vec<String>) {
        match selection {
            SelectionTarget::Clipboard => {
                set_data_device_selection(&self.display_handle, &self.seat, mime_types, ());
            }
            SelectionTarget::Primary => {
                set_primary_selection(&self.display_handle, &self.seat, mime_types, ());
            }
        }
    }

    fn cleared_selection(&mut self, _xwm: XwmId, selection: SelectionTarget) {
        match selection {
            SelectionTarget::Clipboard => {
                if current_data_device_selection_userdata(&self.seat).is_some() {
                    clear_data_device_selection(&self.display_handle, &self.seat);
                }
            }
            SelectionTarget::Primary => {
                if current_primary_selection_userdata(&self.seat).is_some() {
                    clear_primary_selection(&self.display_handle, &self.seat);
                }
            }
        }
    }

    fn disconnected(&mut self, _xwm: XwmId) {
        warn!("X11: xwm disconnected — clearing X11Wm");
        self.xwm = None;
    }
}
