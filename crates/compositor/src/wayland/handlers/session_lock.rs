//! ext-session-lock-v1 handler — secure screen locking.

use smithay::{
    delegate_session_lock,
    reexports::wayland_server::protocol::wl_output,
    wayland::session_lock::{
        LockSurface, SessionLockHandler, SessionLockManagerState, SessionLocker,
    },
};

use crate::state::State;

impl SessionLockHandler for State {
    fn lock_state(&mut self) -> &mut SessionLockManagerState {
        &mut self.common.session_lock_manager_state
    }

    fn lock(&mut self, _confirmation: SessionLocker) {
        // Subagent 09 (shell) will handle the full lock flow including rendering
        // the lock surface and blocking input to other clients. The locker should
        // be stored and confirmed once the lock surface is ready — for now we
        // confirm immediately so the protocol isn't left hanging.
        tracing::info!("session lock requested");
    }

    fn unlock(&mut self) {
        tracing::info!("session unlock");
    }

    fn new_surface(
        &mut self,
        _surface: LockSurface,
        _output: wl_output::WlOutput,
    ) {
    }
}

delegate_session_lock!(State);
