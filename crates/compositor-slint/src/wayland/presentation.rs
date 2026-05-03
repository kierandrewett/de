//! wp_presentation_feedback dispatch helpers.
//!
//! We are a winit+wgpu compositor with no real DRM page-flip event, so every
//! `presented` we emit is a synthesised "fake vsync" — the timestamp comes
//! from the monotonic clock at the moment the swapchain frame.present()
//! returned, and the refresh interval is hard-coded to 60 Hz to match
//! the synthesised Output mode declared in the renderer. This is
//! wrong-but-useful: mpv's vsync sync, Chrome's frame pacing, and tools
//! that just want monotonically-increasing presented timestamps all work,
//! while pixel-accurate vsync chasers will not. The fix for that is wiring
//! a real OutputDamageTracker + presentation-aware swap path, deferred.

use std::time::Duration;

use smithay::{
    desktop::utils::SurfacePresentationFeedback,
    output::Output,
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    utils::{Monotonic, Time},
    wayland::{
        compositor::{with_surface_tree_downward, TraversalAction},
        presentation::Refresh,
    },
};
use wayland_protocols::wp::presentation_time::server::wp_presentation_feedback;

use crate::wayland_state::SpikeState;

/// 60 Hz refresh in nanoseconds. Hard-coded because winit+wgpu can't tell
/// us the real display refresh; matches the `Mode { refresh: 60_000 }`
/// declared on the synthesised Output in the renderer.
const REFRESH_60HZ_NS: u64 = 16_666_667;

impl SpikeState {
    /// Drain pending wp_presentation_feedback callbacks for every surface in
    /// `surfaces` (and their subsurface trees) and fire `presented` with the
    /// supplied timestamp.
    ///
    /// Should be called once per frame for the surfaces the renderer just
    /// composited. Surfaces we did NOT composite are skipped — the next
    /// frame that includes them will fire their feedback.
    pub fn send_presentation_feedback_for(
        &self,
        output: &Output,
        surfaces: &[WlSurface],
        present_time: Time<Monotonic>,
        seq: u64,
    ) {
        let mut feedbacks: Vec<SurfacePresentationFeedback> = Vec::new();
        for surface in surfaces {
            collect_feedback(surface, &mut feedbacks);
        }

        if feedbacks.is_empty() {
            return;
        }

        let clk_id = <Monotonic as smithay::utils::ClockSource>::ID as u32;
        let time: Duration = present_time.into();
        let refresh = Refresh::fixed(Duration::from_nanos(REFRESH_60HZ_NS));
        for mut feedback in feedbacks.drain(..) {
            feedback.presented(
                output,
                clk_id,
                time,
                refresh,
                seq,
                // Vsync | HwClock | HwCompletion. We render through a winit/wgpu
                // swapchain that drives presents at the host's vsync, our
                // timestamp comes from the host monotonic clock right
                // after frame.present() returns, and the actual scanout
                // happens within sub-frame latency in mailbox/fifo mode.
                // Anvil ships these three combined when DRM metadata is
                // available; without it clients (mpv's vsync sync, video
                // players, frame-pacing benchmarks) think we don't
                // support hardware-timed presentation and disable
                // optimisations.
                wp_presentation_feedback::Kind::Vsync
                    | wp_presentation_feedback::Kind::HwClock
                    | wp_presentation_feedback::Kind::HwCompletion,
            );
        }
    }
}

/// Walk one surface tree and pull every queued [`SurfacePresentationFeedback`]
/// into `out`.
///
/// Anvil's `take_presentation_feedback_surface_tree` gates on a per-surface
/// "primary scanout output" stored in surface user-data. We don't run a
/// damage tracker yet, so no surface has that user-data set; instead we
/// trust the caller's surface list (caller already filtered for
/// visibility on this output).
fn collect_feedback(surface: &WlSurface, out: &mut Vec<SurfacePresentationFeedback>) {
    with_surface_tree_downward(
        surface,
        (),
        |_, _, &()| TraversalAction::DoChildren(()),
        |_surface, states, &()| {
            if let Some(feedback) = SurfacePresentationFeedback::from_states(
                states,
                // Vsync | HwClock | HwCompletion. We render through a winit/wgpu
                // swapchain that drives presents at the host's vsync, our
                // timestamp comes from the host monotonic clock right
                // after frame.present() returns, and the actual scanout
                // happens within sub-frame latency in mailbox/fifo mode.
                // Anvil ships these three combined when DRM metadata is
                // available; without it clients (mpv's vsync sync, video
                // players, frame-pacing benchmarks) think we don't
                // support hardware-timed presentation and disable
                // optimisations.
                wp_presentation_feedback::Kind::Vsync
                    | wp_presentation_feedback::Kind::HwClock
                    | wp_presentation_feedback::Kind::HwCompletion,
            ) {
                out.push(feedback);
            }
        },
        |_, _, &()| true,
    );
}
