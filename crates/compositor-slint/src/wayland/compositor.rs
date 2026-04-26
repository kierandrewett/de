//! wl_compositor + wl_subcompositor handler.
//!
//! wl_subcompositor is part of smithay's CompositorState and is handled by
//! the same `delegate_compositor!` macro — there is no separate delegate.

use smithay::{
    delegate_compositor,
    backend::renderer::utils::on_commit_buffer_handler,
    reexports::wayland_server::{protocol::wl_surface::WlSurface, Client},
    wayland::compositor::{CompositorClientState, CompositorHandler, CompositorState},
};

use crate::wayland_state::{ClientState, SpikeState};
use crate::wayland_state::import_shm_buffer;

impl CompositorHandler for SpikeState {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor_state
    }

    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        &client.get_data::<ClientState>().unwrap().compositor_state
    }

    fn commit(&mut self, surface: &WlSurface) {
        on_commit_buffer_handler::<Self>(surface);

        let is_active = self
            .active_surface
            .as_ref()
            .map(|s| s == surface)
            .unwrap_or(false);

        if is_active {
            import_shm_buffer(surface, &self.client_pixels);
        }
    }
}

delegate_compositor!(SpikeState);
