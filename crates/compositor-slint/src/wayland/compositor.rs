//! wl_compositor + wl_subcompositor handler.
//!
//! wl_subcompositor is part of smithay's CompositorState and is handled by
//! the same `delegate_compositor!` macro — there is no separate delegate.
//!
//! ## Buffer import order
//!
//! On each `commit` we try buffers in this order:
//!   1. SHM — the standard CPU-copy path (all clients support this).
//!   2. DMA-BUF pending — if `dmabuf_imported` already ran the GLES read-back
//!      for this surface, retrieve the stored `ClientSurfaceData`.
//! We use the buffer type from `with_renderer_surface_state` to decide.

use smithay::{
    delegate_compositor,
    backend::renderer::utils::on_commit_buffer_handler,
    reexports::wayland_server::{protocol::wl_surface::WlSurface, Client},
    wayland::compositor::{CompositorClientState, CompositorHandler, CompositorState},
};
use tracing::debug;

use crate::wayland_state::{ClientState, ClientSurfaceData, SpikeState};
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

        let toplevel_idx = self.toplevels.iter().position(|t| &t.surface == surface);
        if let Some(idx) = toplevel_idx {
            let pixels_arc = self.toplevels[idx].pixels.clone();

            // Try SHM import first (most clients).
            import_shm_buffer(surface, &pixels_arc);

            // If SHM import produced nothing (width == 0), check DMA-BUF pending.
            // `dmabuf_imported` stores pixel data keyed by "WxH" — we look up the
            // first matching key that has a non-zero width.
            if pixels_arc.lock().unwrap().width == 0 {
                // Find any pending DMA-BUF entry (we take the first available).
                // In practice a single client commits one buffer at a time.
                if let Some((_key, data)) = self.dmabuf_pending.iter().find(|(_, d)| d.width > 0).map(|(k, d)| (k.clone(), d.clone())) {
                    debug!("DMA-BUF: consuming pending {}x{} pixels for toplevel", data.width, data.height);
                    let key = format!("{}x{}", data.width, data.height);
                    self.dmabuf_pending.remove(&key);
                    *pixels_arc.lock().unwrap() = data;
                }
            }

            // Also update legacy single-surface buffer if this is the active surface.
            let is_active = self.active_surface.as_ref().map(|s| s == surface).unwrap_or(false);
            if is_active {
                import_shm_buffer(surface, &self.client_pixels.clone());
                // Sync DMA-BUF data to legacy buffer too.
                let current = pixels_arc.lock().unwrap().clone();
                if current.width > 0 {
                    *self.client_pixels.lock().unwrap() = ClientSurfaceData {
                        pixels: current.pixels,
                        width: current.width,
                        height: current.height,
                        dirty: true,
                    };
                }
            }
        }
    }
}

delegate_compositor!(SpikeState);
