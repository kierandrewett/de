//! Fractional-scale-v1 + viewporter.

use smithay::{
    delegate_fractional_scale, delegate_viewporter,
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    wayland::fractional_scale::FractionalScaleHandler,
};

use crate::wayland_state::SpikeState;

// fractional-scale-v1: when a client requests a wp_fractional_scale_v1
// for a surface, send the current output's fractional scale as the
// preferred scale. The smithay backend converts this into 1/120 ths and
// emits the protocol event. Clients (Firefox, GTK4) honour this to render
// at the correct buffer scale and avoid blurry HiDPI text.
//
// Future: per-output scale (multi-output) + a hook on output scale
// change to push updates to all bound fractional-scale objects.
impl FractionalScaleHandler for SpikeState {
    fn new_fractional_scale(&mut self, surface: WlSurface) {
        let scale = self
            .output
            .as_ref()
            .map(|o| o.current_scale().fractional_scale())
            .unwrap_or(1.0);
        smithay::wayland::compositor::with_states(&surface, |states| {
            smithay::wayland::fractional_scale::with_fractional_scale(states, |fs| {
                fs.set_preferred_scale(scale);
            });
        });
    }
}

delegate_fractional_scale!(SpikeState);

// viewporter: no handler trait required.
delegate_viewporter!(SpikeState);
