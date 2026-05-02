//! Output management — wl_output + xdg-output.
//!
//! `OutputManagerState::new_with_xdg_output` advertises both protocols and
//! `delegate_output!` registers dispatch for both `wl_output` and the
//! `zxdg_output_manager_v1` / `zxdg_output_v1` interfaces — there is no
//! separate `delegate_xdg_output!` macro.
//!
//! ## Surface ↔ output binding
//!
//! HiDPI clients gate their buffer scale on `wl_surface.preferred_buffer_scale`
//! (v6+) and the legacy `wl_surface.enter` event (v5 and earlier). Smithay
//! exposes both via `Output::enter` (sends `wl_surface.enter` once per
//! surface) and `compositor::send_surface_state` (sends
//! `preferred_buffer_scale` / `preferred_buffer_transform` when they
//! differ from the cached value). `bind_surfaces_to_output` walks every
//! mapped surface tree once per frame and forwards both — idempotent, so
//! repeated calls don't spam clients with duplicate events.
//!
//! No multi-output handling yet; the single output is always the same one.

use smithay::{
    delegate_output,
    desktop::utils::with_surfaces_surface_tree,
    wayland::{
        compositor::{send_surface_state, SurfaceData},
        output::OutputHandler,
    },
};

use crate::wayland_state::SpikeState;

impl OutputHandler for SpikeState {}

delegate_output!(SpikeState);

impl SpikeState {
    /// Refresh the surface↔output binding for every mapped surface tree.
    /// Idempotent: `Output::enter` and `send_surface_state` both gate on
    /// cached state, so calling this each frame only sends events when
    /// something actually changed (a new surface mapped, or the output
    /// scale / transform changed).
    pub fn bind_surfaces_to_output(&self) {
        let Some(output) = self.output.as_ref() else {
            return;
        };

        let scale = output.current_scale().integer_scale();
        let transform = output.current_transform();

        let emit = |surface: &smithay::reexports::wayland_server::protocol::wl_surface::WlSurface| {
            // wl_surface.enter is per-root-surface, not per node — Smithay's
            // Output keeps a HashSet keyed on the surface so this is a noop
            // after the first call.
            output.enter(surface);

            // preferred_buffer_scale + preferred_buffer_transform are per
            // wl_surface (subsurfaces included) on protocol version 6+.
            // send_surface_state diffs against the cached value before
            // emitting.
            with_surfaces_surface_tree(surface, |sub, states: &SurfaceData| {
                send_surface_state(sub, states, scale, transform);
            });
        };

        for tl in &self.toplevels {
            emit(&tl.surface);
        }
        for popup in &self.popups {
            emit(&popup.surface);
        }
        for layer in &self.layer_surfaces {
            emit(layer.surface.wl_surface());
        }
    }
}
