//! Fractional-scale-v1 + viewporter.

use smithay::{
    delegate_fractional_scale, delegate_viewporter,
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    wayland::fractional_scale::FractionalScaleHandler,
};

use crate::wayland_state::SpikeState;

// fractional-scale-v1: FractionalScaleHandler has a default no-op impl.
impl FractionalScaleHandler for SpikeState {
    fn new_fractional_scale(&mut self, _surface: WlSurface) {
        // Default scale is 1.0; future work can update per-output scale here.
    }
}

delegate_fractional_scale!(SpikeState);

// viewporter: no handler trait required.
delegate_viewporter!(SpikeState);
