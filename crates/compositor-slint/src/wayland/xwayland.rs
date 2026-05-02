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

use std::os::unix::io::OwnedFd;
use std::process::Stdio;

use smithay::{
    delegate_xwayland_keyboard_grab, delegate_xwayland_shell,
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    utils::{Logical, Rectangle},
    wayland::{
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

use crate::wayland_state::SpikeState;

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
    let result = state.loop_handle.insert_source(xwayland, move |event, _, data| match event {
        XWaylandEvent::Ready { x11_socket, display_number } => {
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
}

delegate_xwayland_shell!(SpikeState);

// ──────────────────────────────────────────────────────────────────────────────
// XWaylandKeyboardGrabHandler — port of anvil's one-line impl.
// ──────────────────────────────────────────────────────────────────────────────

impl XWaylandKeyboardGrabHandler for SpikeState {
    fn keyboard_focus_for_xsurface(&self, surface: &WlSurface) -> Option<WlSurface> {
        // Our SeatHandler::KeyboardFocus is `WlSurface` so the focus target
        // for an X11 window is just the wl_surface backing it.
        Some(surface.clone())
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
        // Tell the X11 client we accept the map. Anvil places the window via
        // its space machinery here; we use the geometry the client requested
        // (or its existing geometry if it didn't specify one) — the
        // `WindowManager` will lay it out properly once the underlying
        // wl_surface gets a buffer and our existing toplevel-tracking kicks in.
        if let Err(e) = window.set_mapped(true) {
            warn!("X11: set_mapped(true) failed: {e}");
            return;
        }
        let geo = window.geometry();
        if let Err(e) = window.configure(Some(geo)) {
            warn!("X11: configure on map failed: {e}");
        }
        debug!("X11: map_window_request id={:?} geo={:?}", window.window_id(), geo);
    }

    fn mapped_override_redirect_window(&mut self, _xwm: XwmId, window: X11Surface) {
        debug!(
            "X11: mapped_override_redirect id={:?} geo={:?}",
            window.window_id(),
            window.geometry()
        );
    }

    fn unmapped_window(&mut self, _xwm: XwmId, window: X11Surface) {
        debug!("X11: unmapped id={:?}", window.window_id());
        if !window.is_override_redirect() {
            let _ = window.set_mapped(false);
        }
    }

    fn destroyed_window(&mut self, _xwm: XwmId, window: X11Surface) {
        debug!("X11: destroyed id={:?}", window.window_id());
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
        let _ = window.configure(geo);
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
    }

    fn resize_request(
        &mut self,
        _xwm: XwmId,
        window: X11Surface,
        _button: u32,
        _edges: X11ResizeEdge,
    ) {
        // TODO: route to WindowManager resize machinery (PointerResizeGrab).
        debug!("X11: resize_request id={:?}", window.window_id());
    }

    fn move_request(&mut self, _xwm: XwmId, window: X11Surface, _button: u32) {
        // TODO: route to WindowManager move machinery (PointerMoveGrab).
        debug!("X11: move_request id={:?}", window.window_id());
    }

    fn maximize_request(&mut self, _xwm: XwmId, window: X11Surface) {
        let _ = window.set_maximized(true);
    }

    fn unmaximize_request(&mut self, _xwm: XwmId, window: X11Surface) {
        let _ = window.set_maximized(false);
    }

    fn fullscreen_request(&mut self, _xwm: XwmId, window: X11Surface) {
        let _ = window.set_fullscreen(true);
    }

    fn unfullscreen_request(&mut self, _xwm: XwmId, window: X11Surface) {
        let _ = window.set_fullscreen(false);
    }

    fn minimize_request(&mut self, _xwm: XwmId, window: X11Surface) {
        let _ = window.set_hidden(true);
    }

    fn unminimize_request(&mut self, _xwm: XwmId, window: X11Surface) {
        let _ = window.set_hidden(false);
    }

    fn property_notify(&mut self, _xwm: XwmId, window: X11Surface, property: WmWindowProperty) {
        match property {
            WmWindowProperty::Title => {
                debug!("X11: title -> {:?} (id={:?})", window.title(), window.window_id());
            }
            WmWindowProperty::Class => {
                debug!("X11: class -> {:?} (id={:?})", window.class(), window.window_id());
            }
            _ => {}
        }
    }

    // ── Selection bridge (X11 <-> wayland clipboard / primary). Mirrors
    //    anvil one-for-one — smithay handles most of the plumbing once these
    //    are wired up.
    fn allow_selection_access(&mut self, _xwm: XwmId, _selection: SelectionTarget) -> bool {
        // We don't track the focused-surface→xwm mapping yet, so allow the
        // bridge unconditionally. TODO: gate on whether the focused surface
        // belongs to this xwm.
        true
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
