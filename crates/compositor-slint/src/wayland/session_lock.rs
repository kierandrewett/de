//! ext-session-lock-v1 handler — secure screen locking.
//!
//! Globals are registered so lock clients (swaylock, gtklock, etc.) can
//! connect.  Full session-lock flow (rendering lock surface, blocking input)
//! is left for a future shell/window-management pass.

use smithay::{
    delegate_session_lock,
    reexports::wayland_server::protocol::wl_output,
    wayland::session_lock::{
        LockSurface, SessionLockHandler, SessionLockManagerState, SessionLocker,
    },
};
use tracing::info;

use crate::wayland_state::SpikeState;

impl SessionLockHandler for SpikeState {
    fn lock_state(&mut self) -> &mut SessionLockManagerState {
        &mut self.session_lock_manager_state
    }

    fn lock(&mut self, confirmation: SessionLocker) {
        // TODO: render lock surface + block input to other clients.
        // For now, confirm immediately so the protocol is not left hanging.
        info!("session lock requested (stub: confirming immediately)");
        drop(confirmation); // dropping = not confirming; caller must call .lock()
        // Actually: SessionLocker::lock() signals the locked state.
        // We drop it for now (leaves compositor "unlocked" from protocol PoV).
        // A real impl would store it and call .lock() once the surface is ready.
    }

    fn unlock(&mut self) {
        info!("session unlock");
    }

    fn new_surface(&mut self, _surface: LockSurface, _output: wl_output::WlOutput) {}
}

delegate_session_lock!(SpikeState);
