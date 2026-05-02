//! Frame timing protocols: presentation-time, wp-fifo-v1, commit-timing-v1.
//!
//! The render side MUST call `SpikeState::pre_render_drive_clients()` once per
//! frame (before submitting the frame) so that:
//!   1. wl_surface.frame callbacks are sent, advancing client animation.
//!   2. wp_fifo_v1 barriers are signalled, unblocking FIFO-mode swapchains.
//!   3. Transaction queues are drained via `blocker_cleared`.

use smithay::{
    delegate_commit_timing, delegate_fifo, delegate_presentation,
    output::Output,
    reexports::wayland_server::protocol::wl_surface::WlSurface,
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
    /// Send wl_surface.frame callbacks to the supplied list of root surfaces.
    ///
    /// The renderer is the only thing that knows which surfaces it actually
    /// composited this frame (i.e. which windows are NOT minimised, NOT
    /// in their close-settle phase, AND have a non-zero buffer). Walking the
    /// full `toplevels` list here would fire frame callbacks on invisible
    /// clients and drive them at full output framerate even when nothing
    /// they render reaches the screen — battery + idle CPU win is material.
    pub fn send_frame_callbacks_for(&self, output: &Output, surfaces: &[WlSurface]) {
        use smithay::desktop::utils::send_frames_surface_tree;
        use std::time::Duration;

        let time: Duration = self.clock.now().into();

        for surface in surfaces {
            // The closure unconditionally returns `Some(output)`: we treat
            // every surface in this list as on-output since the renderer
            // already filtered for visibility before passing it in.
            // Throttle of 1s lets clients with no recently-sent callback
            // still receive one even if they momentarily fail the visibility
            // gate (e.g. mid-resize buffer-size dip to zero).
            send_frames_surface_tree(
                surface,
                output,
                time,
                Some(Duration::from_secs(1)),
                |_, _| Some(output.clone()),
            );
        }

        debug!("send_frame_callbacks_for: sent to {} surfaces", surfaces.len());
    }

    /// Signal wp_fifo barriers and drain blocked transaction queues.
    ///
    /// Call this BEFORE submitting the frame to the display so that clients
    /// can queue their next commit immediately after receiving the frame
    /// callback.
    ///
    /// Ported from `crates/compositor/src/winit.rs::pre_render_drive_clients`.
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

        debug!("pre_render_drive_clients: processed {} surfaces", surfaces.len());
    }
}

/// Walk the surface tree rooted at `surface` and signal any pending
/// `wp_fifo_v1` barrier on each node.  Required for clients that use
/// mesa-vk's fifo-mode swapchain.
fn signal_fifo_barriers(surface: &WlSurface) {
    use smithay::wayland::fifo::FifoBarrierCachedState;

    with_surface_tree_downward(
        surface,
        (),
        |_, _, _| TraversalAction::DoChildren(()),
        |_sub, states, _| {
            let barrier = states
                .cached_state
                .get::<FifoBarrierCachedState>()
                .current()
                .barrier
                .take();
            if let Some(b) = barrier {
                b.signal();
            }
        },
        |_, _, _| true,
    );
}
