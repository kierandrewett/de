//! Idle and power-management handlers — ext-idle-notify and idle-inhibit.

use smithay::{
    delegate_idle_inhibit, delegate_idle_notify,
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    wayland::{
        idle_inhibit::IdleInhibitHandler,
        idle_notify::{IdleNotifierHandler, IdleNotifierState},
    },
};

use crate::state::State;

// ─── IdleNotify ───────────────────────────────────────────────────────────────

impl IdleNotifierHandler for State {
    fn idle_notifier_state(&mut self) -> &mut IdleNotifierState<Self> {
        &mut self.common.idle_notifier_state
    }
}

delegate_idle_notify!(State);

// ─── IdleInhibit ──────────────────────────────────────────────────────────────

impl IdleInhibitHandler for State {
    fn inhibit(&mut self, _surface: WlSurface) {
        tracing::debug!("idle inhibit activated");
    }

    fn uninhibit(&mut self, _surface: WlSurface) {
        tracing::debug!("idle inhibit deactivated");
    }
}

delegate_idle_inhibit!(State);
