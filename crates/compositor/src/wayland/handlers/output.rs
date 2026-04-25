//! Output management handlers — xdg-output and fractional-scale.

use smithay::{
    delegate_fractional_scale,
    wayland::fractional_scale::FractionalScaleHandler,
};

use crate::state::State;

// ─── FractionalScale ──────────────────────────────────────────────────────────

impl FractionalScaleHandler for State {
    fn new_fractional_scale(
        &mut self,
        surface: smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
    ) {
        // Send the preferred scale of the output the surface is on.
        // The shell/render subagents will update this more precisely during placement.
        if let Some(output) = self.common.space.outputs().next() {
            let scale = output.current_scale().fractional_scale();
            smithay::wayland::compositor::with_states(&surface, |states| {
                smithay::wayland::fractional_scale::with_fractional_scale(states, |fs| {
                    fs.set_preferred_scale(scale);
                });
            });
        }
    }
}

delegate_fractional_scale!(State);

// xdg-output is registered as part of OutputManagerState::new_with_xdg_output
// and needs no additional handler implementation.
