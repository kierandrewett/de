//! Idle and power-management handlers: ext-idle-notify-v1 + idle-inhibit-v1.

use smithay::{
    delegate_idle_inhibit, delegate_idle_notify,
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    wayland::{
        idle_inhibit::IdleInhibitHandler,
        idle_notify::{IdleNotifierHandler, IdleNotifierState},
    },
};
use tracing::debug;

use crate::wayland_state::SpikeState;

// ─── IdleNotify ───────────────────────────────────────────────────────────────

impl IdleNotifierHandler for SpikeState {
    fn idle_notifier_state(&mut self) -> &mut IdleNotifierState<Self> {
        &mut self.idle_notifier_state
    }
}

delegate_idle_notify!(SpikeState);

// ─── IdleInhibit ──────────────────────────────────────────────────────────────

impl IdleInhibitHandler for SpikeState {
    fn inhibit(&mut self, surface: WlSurface) {
        let was_empty = self.idle_inhibitors.is_empty();
        self.idle_inhibitors.insert(surface);
        if was_empty {
            self.idle_notifier_state.set_is_inhibited(true);
            debug!("idle inhibit activated");
        }
    }

    fn uninhibit(&mut self, surface: WlSurface) {
        self.idle_inhibitors.remove(&surface);
        if self.idle_inhibitors.is_empty() {
            self.idle_notifier_state.set_is_inhibited(false);
            debug!("idle inhibit deactivated");
        }
    }
}

delegate_idle_inhibit!(SpikeState);
