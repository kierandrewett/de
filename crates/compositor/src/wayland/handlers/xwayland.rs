//! XWayland compatibility handlers — xwayland-keyboard-grab-v1.
//!
//! xwayland-shell-v1 requires full XwmHandler (X11 window manager) and is deferred
//! to a later phase. xwayland-keyboard-grab-v1 is self-contained.

use smithay::{
    delegate_xwayland_keyboard_grab,
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    wayland::{
        seat::WaylandFocus,
        xwayland_keyboard_grab::XWaylandKeyboardGrabHandler,
    },
};

use crate::state::State;

// ─── XWaylandKeyboardGrab ────────────────────────────────────────────────────

impl XWaylandKeyboardGrabHandler for State {
    fn keyboard_focus_for_xsurface(
        &self,
        surface: &WlSurface,
    ) -> Option<crate::focus::KeyboardFocusTarget> {
        self.common
            .space
            .elements()
            .find(|w| w.wl_surface().as_deref() == Some(surface))
            .cloned()
            .map(|w| crate::focus::KeyboardFocusTarget::Window(Box::new(w)))
    }
}

delegate_xwayland_keyboard_grab!(State);
