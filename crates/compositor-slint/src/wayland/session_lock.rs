//! ext-session-lock-v1 handler — secure screen locking.
//!
//! Flow:
//!   1. Client (swaylock / gtklock / hyprlock) requests lock → `lock()` fires.
//!      We stash the `SessionLocker` in `pending_session_lock` and wait for
//!      the lock surface(s) to appear and commit a buffer. We DON'T confirm
//!      the lock yet — confirming before the surface is renderable would
//!      flash the desktop briefly.
//!   2. Client creates one lock surface per output (`new_surface`). We
//!      configure each with the output size and store a per-output pixel
//!      buffer for the renderer to consume.
//!   3. Client commits a buffer → commit handler in `wayland/compositor.rs`
//!      copies pixels into the LockSurfaceInfo and, if this is the first
//!      committed lock surface, takes the pending locker and calls
//!      `.lock()` to flip the protocol to "locked".
//!   4. While `session_locked == true`, the renderer composites only the
//!      lock surface(s) (skipping toplevels / popups / layer-shells / panel
//!      / dock) and the input gates drop pointer/keyboard for everyone
//!      except the lock surface.
//!   5. Client unlocks (or dies — smithay handles death + sends `finished`
//!      back to a backup connection) → `unlock()` clears state.

use smithay::{
    delegate_session_lock,
    output::Output,
    reexports::wayland_server::protocol::wl_output,
    utils::Size,
    wayland::session_lock::{
        LockSurface, SessionLockHandler, SessionLockManagerState, SessionLocker,
    },
};
use std::sync::{Arc, Mutex};
use tracing::{info, warn};

use crate::wayland_state::{ClientSurfaceData, LockSurfaceInfo, SpikeState};

impl SessionLockHandler for SpikeState {
    fn lock_state(&mut self) -> &mut SessionLockManagerState {
        &mut self.session_lock_manager_state
    }

    fn lock(&mut self, confirmation: SessionLocker) {
        if self.session_locked || self.pending_session_lock.is_some() {
            // Protocol allows only one active lock at a time. Reject by
            // dropping the locker — that triggers `finished` on the new
            // ExtSessionLockV1, telling the second client to give up.
            warn!("session lock requested while another lock is active; rejecting");
            drop(confirmation);
            return;
        }
        info!("session lock requested; waiting for first lock-surface buffer to confirm");
        self.pending_session_lock = Some(confirmation);
    }

    fn unlock(&mut self) {
        info!("session unlocked");
        self.session_locked = false;
        self.pending_session_lock = None;
        self.lock_surfaces.clear();
    }

    fn new_surface(&mut self, surface: LockSurface, output: wl_output::WlOutput) {
        // Match the wl_output resource to one of our smithay Outputs and
        // use THAT output's mode — not primary's. Fixes the multi-output
        // session-lock bug from the audit where every output's lock
        // surface was configured at the primary's resolution.
        let (w, h) = output_size(&self.outputs, &output);
        surface.with_pending_state(|state| {
            state.size = Some(Size::from((w as u32, h as u32)));
        });
        surface.send_configure();

        info!("new lock surface for output ({}x{})", w, h);
        self.lock_surfaces.push(LockSurfaceInfo {
            surface,
            output,
            pixels: Arc::new(Mutex::new(ClientSurfaceData::default())),
        });
    }
}

/// Resolve a `wl_output` resource to an actual logical-pixel size by walking
/// our `Output` list and matching client_outputs. Falls back to primary, then
/// to a sensible default if we have no outputs at all.
fn output_size(outputs: &[Output], wl: &wl_output::WlOutput) -> (i32, i32) {
    use smithay::reexports::wayland_server::Resource;
    let client = wl.client();
    if let Some(client) = client {
        for out in outputs {
            if out.client_outputs(&client).into_iter().any(|c| &c == wl) {
                if let Some(mode) = out.current_mode() {
                    return (mode.size.w.max(1), mode.size.h.max(1));
                }
            }
        }
    }
    if let Some(mode) = outputs.first().and_then(Output::current_mode) {
        return (mode.size.w.max(1), mode.size.h.max(1));
    }
    (1280, 960)
}

delegate_session_lock!(SpikeState);
