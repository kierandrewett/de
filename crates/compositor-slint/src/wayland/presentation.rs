//! wp_presentation_feedback dispatch helpers.
//!
//! We are a winit+wgpu compositor with no real DRM page-flip event, so the
//! timestamp comes from the monotonic clock at the moment the swapchain
//! `frame.present()` returned. The refresh interval is read from the
//! Output's current mode (was hard-coded 60 Hz — clients on 144/240 Hz
//! monitors were lied to). Flags advertise `Vsync` only — the wgpu
//! swapchain genuinely vsyncs but we don't have a hardware completion
//! signal, so claiming `HwClock|HwCompletion` (as we used to) was wrong
//! and made tools like mpv's `--video-sync=display-resample` mistime
//! frames.

use std::time::Duration;

use smithay::{
    backend::renderer::element::{Id, RenderElementStates},
    desktop::utils::SurfacePresentationFeedback,
    output::Output,
    reexports::wayland_server::{backend::ObjectId, protocol::wl_surface::WlSurface, Resource},
    utils::{Monotonic, Time},
    wayland::{
        compositor::{with_surface_tree_downward, TraversalAction},
        presentation::Refresh,
    },
};
use wayland_protocols::wp::presentation_time::server::wp_presentation_feedback;

use crate::wayland_state::SpikeState;

/// 60 Hz refresh in nanoseconds — fallback when an output has no current mode.
const REFRESH_60HZ_NS: u64 = 16_666_667;

/// `Output::current_mode().refresh` is in milli-Hz (e.g. 60_000 for 60 Hz).
/// Convert to nanoseconds between refresh ticks.
fn refresh_nanos_for(output: &Output) -> u64 {
    output
        .current_mode()
        .map(|mode| {
            // refresh in milli-Hz; period_ns = 1e12 / refresh.
            if mode.refresh > 0 {
                (1_000_000_000_000u64) / mode.refresh as u64
            } else {
                REFRESH_60HZ_NS
            }
        })
        .unwrap_or(REFRESH_60HZ_NS)
}

impl SpikeState {
    /// Drain pending wp_presentation_feedback callbacks for surfaces that the
    /// damage tracker reported as presented in the frame that just drew.
    pub fn send_presentation_feedback_for_render_state(
        &self,
        output: &Output,
        surfaces: &[WlSurface],
        states: &RenderElementStates,
        present_time: Time<Monotonic>,
        seq: u64,
    ) {
        let mut feedbacks: Vec<SurfacePresentationFeedback> = Vec::new();
        let mut seen: std::collections::HashSet<ObjectId> = std::collections::HashSet::new();
        for surface in surfaces {
            if !seen.insert(surface.id()) {
                continue;
            }
            collect_feedback(surface, states, &mut feedbacks);
        }

        if feedbacks.is_empty() {
            return;
        }

        let clk_id = <Monotonic as smithay::utils::ClockSource>::ID as u32;
        let time: Duration = present_time.into();
        let refresh = Refresh::fixed(Duration::from_nanos(refresh_nanos_for(output)));
        for mut feedback in feedbacks.drain(..) {
            // Vsync only: the wgpu swapchain in FIFO mode is genuinely vsynced,
            // but we have no hardware completion signal — claiming HwClock or
            // HwCompletion (as we used to) made mpv's display-resample mode
            // mistime frames since it trusted the timestamps as wallclock.
            // Anvil only ships those flags when it has real DRM metadata.
            feedback.presented(
                output,
                clk_id,
                time,
                refresh,
                seq,
                wp_presentation_feedback::Kind::Vsync,
            );
        }
    }
}

/// Walk one surface tree and pull feedback only for surfaces that the render
/// states mark as presented on this output frame.
fn collect_feedback(
    surface: &WlSurface,
    render_states: &RenderElementStates,
    out: &mut Vec<SurfacePresentationFeedback>,
) {
    with_surface_tree_downward(
        surface,
        (),
        |_, _, &()| TraversalAction::DoChildren(()),
        |surface, states, &()| {
            if !render_states.element_was_presented(Id::from(surface)) {
                return;
            }
            // Match the flag set in `send_presentation_feedback_for` — Vsync
            // only. See that fn for rationale.
            if let Some(feedback) = SurfacePresentationFeedback::from_states(
                states,
                wp_presentation_feedback::Kind::Vsync,
            ) {
                out.push(feedback);
            }
        },
        |_, _, &()| true,
    );
}
