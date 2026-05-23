//! Frame timing protocols: presentation-time, wp-fifo-v1, commit-timing-v1.
//!
//! The render side MUST call `SpikeState::pre_render_drive_clients()` once per
//! frame (before submitting the frame) so that:
//!   1. wl_surface.frame callbacks are sent, advancing client animation.
//!   2. wp_fifo_v1 barriers are signalled, unblocking FIFO-mode swapchains.
//!   3. Transaction queues are drained via `blocker_cleared`.

use smithay::{
    backend::renderer::element::{Id, RenderElementStates},
    delegate_commit_timing, delegate_fifo, delegate_presentation,
    output::Output,
    reexports::wayland_server::{backend::ObjectId, protocol::wl_surface::WlSurface, Resource},
    wayland::compositor::{with_surface_tree_downward, TraversalAction},
};
use tracing::debug;

use crate::wayland_state::SpikeState;

// presentation-time: no trait needed, just the delegate.
delegate_presentation!(SpikeState);

// wp-fifo-v1: no trait needed.
delegate_fifo!(SpikeState);

// commit-timing-v1: no trait needed.
delegate_commit_timing!(SpikeState);

// ──────────────────────────────────────────────────────────────────────────────
// Pre-render helper — call once per frame
// ──────────────────────────────────────────────────────────────────────────────

impl SpikeState {
    /// Send wl_surface.frame callbacks to surfaces presented according to the
    /// `OutputDamageTracker` render states for the frame that was just drawn.
    pub fn send_frame_callbacks_for_render_state(
        &self,
        output: &Output,
        surfaces: &[WlSurface],
        states: &RenderElementStates,
    ) {
        use smithay::desktop::utils::send_frames_surface_tree;
        use std::time::Duration;

        let time: Duration = self.clock.now().into();
        let mut seen: std::collections::HashSet<ObjectId> = std::collections::HashSet::new();

        for surface in surfaces {
            if !seen.insert(surface.id()) {
                continue;
            }
            send_frames_surface_tree(
                surface,
                output,
                time,
                Some(Duration::from_secs(1)),
                |surface, _| {
                    states
                        .element_was_presented(Id::from(surface))
                        .then(|| output.clone())
                },
            );
        }

        debug!(
            "send_frame_callbacks_for_render_state: sent to {} surfaces",
            surfaces.len()
        );
    }

    /// Signal wp_fifo barriers and drain blocked transaction queues.
    ///
    /// Call this BEFORE submitting the frame to the display so that clients
    /// can queue their next commit immediately after receiving the frame
    /// callback.
    ///
    /// Ported from `crates/compositor/src/winit.rs::pre_render_drive_clients`.
    #[allow(clippy::mutable_key_type)]
    pub fn pre_render_drive_clients(&mut self) {
        use smithay::reexports::wayland_server::Resource;
        use smithay::wayland::compositor::CompositorHandler;

        // Collect surfaces we know about — every mapped toplevel plus every
        // layer surface. Drive ALL clients each frame; unfocused windows
        // still need their fifo barriers signalled and transactions drained.
        let mut surfaces: Vec<WlSurface> = Vec::new();
        for tl in &self.toplevels {
            surfaces.push(tl.surface.clone());
        }
        // Include any layer surfaces.
        for li in &self.layer_surfaces {
            surfaces.push(li.surface.wl_surface().clone());
        }

        // Signal fifo barriers on every (sub)surface.
        for surface in &surfaces {
            signal_fifo_barriers(surface);
        }

        // Release wp_commit_timing_v1 deferred commits whose target
        // timestamp has passed.
        //
        // Mesa's Vulkan WSI sets a target timestamp on every commit via
        // `wp_commit_timer_v1.set_timestamp` (see CommitTimingManagerState
        // in smithay). Smithay parks the commit on a Blocker until we
        // call `signal_until(now)` past the target. Without this drive,
        // every wing/mpv/Chrome commit (any wgpu+vulkan+fifo client) is
        // pinned in the transaction queue forever — only the very first
        // commit, scheduled for ~0, ever applies. Symptom from the wild:
        // client lights up briefly, renders 1-2 frames, then freezes
        // because subsequent commits are queued at e.g. 9443.075s but
        // we never advance the gate.
        let now = current_timestamp(&self.clock);
        for surface in &surfaces {
            signal_commit_timer(surface, now);
        }

        // Drain per-client transaction queues.
        let dh = self.display_handle.clone();
        let mut seen: std::collections::HashSet<
            smithay::reexports::wayland_server::backend::ClientId,
        > = std::collections::HashSet::new();
        let mut clients: Vec<smithay::reexports::wayland_server::Client> = Vec::new();
        for s in &surfaces {
            if let Some(c) = s.client() {
                if seen.insert(c.id()) {
                    clients.push(c);
                }
            }
        }
        for client in clients {
            let ccs_ptr: *const smithay::wayland::compositor::CompositorClientState =
                self.client_compositor_state(&client) as *const _;
            // SAFETY: CompositorClientState lives inside our ClientState in
            // the client's UserData, pinned for the life of the client.
            let ccs = unsafe { &*ccs_ptr };
            ccs.blocker_cleared(self, &dh);
        }

        debug!(
            "pre_render_drive_clients: processed {} surfaces",
            surfaces.len()
        );
    }
}

/// Get the current monotonic clock as a Timestamp suitable for
/// `CommitTimerBarrierState::signal_until`.
fn current_timestamp(
    clock: &smithay::utils::Clock<smithay::utils::Monotonic>,
) -> smithay::wayland::commit_timing::Timestamp {
    clock.now().into()
}

/// Walk the surface tree rooted at `surface` and signal any
/// `wp_commit_timer_v1` barrier whose target timestamp is at or before
/// `deadline`. Required for mesa-vulkan WSI clients (eframe/wgpu, mpv,
/// Chrome): each commit's `set_timestamp` parks the commit on a blocker
/// until we advance the gate. Without this they freeze after a couple
/// of frames.
fn signal_commit_timer(surface: &WlSurface, deadline: smithay::wayland::commit_timing::Timestamp) {
    use smithay::reexports::wayland_server::Resource;
    use smithay::wayland::commit_timing::CommitTimerBarrierStateUserData;

    let mut signaled_any = false;
    with_surface_tree_downward(
        surface,
        (),
        |_, _, _| TraversalAction::DoChildren(()),
        |_sub, states, _| {
            if let Some(s) = states.data_map.get::<CommitTimerBarrierStateUserData>() {
                if s.lock().unwrap().signal_until(deadline) {
                    signaled_any = true;
                }
            }
        },
        |_, _, _| true,
    );
    if signaled_any {
        debug!(
            "commit-timer: signalled deferred commit(s) on root surface_id={}",
            surface.id().protocol_id(),
        );
    }
}

/// Walk the surface tree rooted at `surface` and signal any pending
/// `wp_fifo_v1` barrier on each node.  Required for clients that use
/// mesa-vk's fifo-mode swapchain.
fn signal_fifo_barriers(surface: &WlSurface) {
    use smithay::reexports::wayland_server::Resource;
    use smithay::wayland::fifo::FifoBarrierCachedState;

    let mut signaled = 0u32;
    let mut walked = 0u32;
    with_surface_tree_downward(
        surface,
        (),
        |_, _, _| TraversalAction::DoChildren(()),
        |_sub, states, _| {
            walked += 1;
            let barrier = states
                .cached_state
                .get::<FifoBarrierCachedState>()
                .current()
                .barrier
                .take();
            if let Some(b) = barrier {
                b.signal();
                signaled += 1;
            }
        },
        |_, _, _| true,
    );
    if signaled > 0 {
        debug!(
            "fifo: signaled {} barrier(s) on root surface_id={} (walked {} subsurfaces)",
            signaled,
            surface.id().protocol_id(),
            walked,
        );
    }
}
