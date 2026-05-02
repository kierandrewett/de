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
    backend::renderer::utils::{on_commit_buffer_handler, with_renderer_surface_state},
    reexports::{
        calloop::Interest,
        wayland_server::{protocol::wl_surface::WlSurface, Client, Resource},
    },
    wayland::{
        compositor::{
            add_blocker, add_pre_commit_hook, with_states, BufferAssignment, CompositorClientState,
            CompositorHandler, CompositorState, SurfaceAttributes,
        },
        dmabuf::get_dmabuf,
    },
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

    fn new_surface(&mut self, surface: &WlSurface) {
        // Mesa 24+ Vulkan WSI / explicit-sync clients may commit a DMA-BUF
        // before the producer GPU has finished writing to it. Importing on
        // commit without waiting samples garbage. We mirror anvil's approach:
        // block the commit on the dmabuf's implicit acquire-fence becoming
        // readable, then let smithay re-drive the commit via blocker_cleared.
        add_pre_commit_hook::<Self, _>(surface, |state, _dh, surface| {
            let maybe_dmabuf = with_states(surface, |surface_data| {
                surface_data
                    .cached_state
                    .get::<SurfaceAttributes>()
                    .pending()
                    .buffer
                    .as_ref()
                    .and_then(|assignment| match assignment {
                        BufferAssignment::NewBuffer(buffer) => get_dmabuf(buffer).cloned().ok(),
                        _ => None,
                    })
            });
            let Some(dmabuf) = maybe_dmabuf else { return };
            let Ok((blocker, source)) = dmabuf.generate_blocker(Interest::READ) else { return };
            let Some(client) = surface.client() else { return };
            let res = state.loop_handle.insert_source(source, move |_, _, data| {
                let dh = data.display_handle.clone();
                data.client_compositor_state(&client).blocker_cleared(data, &dh);
                Ok(())
            });
            if res.is_ok() {
                add_blocker(surface, blocker);
            }
        });
    }

    fn commit(&mut self, surface: &WlSurface) {
        on_commit_buffer_handler::<Self>(surface);

        // If the surface's current buffer is a DMA-BUF, run the GPU readback
        // now (the pre-commit blocker has guaranteed the acquire fence
        // signalled). Result lands in `dmabuf_pending` keyed by surface id.
        let dmabuf = with_renderer_surface_state(surface, |s| {
            s.buffer().and_then(|b| get_dmabuf(b).cloned().ok())
        }).flatten();
        if let Some(dmabuf) = dmabuf {
            self.import_dmabuf_for_surface(surface, &dmabuf);
        }

        // Walk up the wl_subsurface parent chain. Bounded depth so a
        // malformed parent chain (would-be cycle) can't hang the wayland
        // thread.
        use smithay::wayland::compositor::get_parent;
        let mut root: WlSurface = surface.clone();
        for _ in 0..32 {
            match get_parent(&root) {
                Some(p) => root = p,
                None => break,
            }
        }

        // If this commit is for the current cursor surface (a client set a
        // wl_surface as its cursor via `wl_pointer.set_cursor`), import the
        // pixels into the dedicated cursor buffer. update_windows pushes
        // them to Slint's cursor-image each frame so animated cursors and
        // dynamic in-app cursors render correctly.
        use smithay::input::pointer::CursorImageStatus;
        if let CursorImageStatus::Surface(cursor_surf) = &self.cursor_status {
            if cursor_surf == surface || *cursor_surf == root {
                let pixels = self.cursor_surface_pixels.clone();
                let _ = import_shm_buffer(cursor_surf, &pixels);
                return;
            }
        }

        // If `root` is a popup surface (or `surface` itself is a popup that
        // has no wl_subsurface parent), import for the popup's pixel buffer
        // and bail before falling through to toplevel handling.
        let popup_idx = self.popups.iter()
            .position(|p| p.surface == root || p.surface == *surface);
        if let Some(pidx) = popup_idx {
            let popup_surf = self.popups[pidx].surface.clone();
            let pixels_arc = self.popups[pidx].pixels.clone();
            let _ = import_shm_buffer(&popup_surf, &pixels_arc);
            return;
        }

        let toplevel_idx = self.toplevels.iter().position(|t| t.surface == root);
        debug!("commit: surface_is_toplevel={} root_in_toplevels={}",
            surface == &root, toplevel_idx.is_some());
        let surface = &root;
        if let Some(idx) = toplevel_idx {
            let pixels_arc = self.toplevels[idx].pixels.clone();

            // Composite the surface tree (toplevel + subsurfaces) into a
            // single RGBA buffer. > 1 surface is our heuristic for "this app
            // draws its own chrome" (Firefox, GTK header-bar apps).
            //
            // We DON'T re-configure CSD clients to a larger size — instead
            // we'll crop the composited buffer to the client's
            // `xdg_surface.set_window_geometry` rect on the Slint side.
            // That rect already excludes the client's own shadow / corner
            // padding, so cropping there gives us the visible window at 1:1
            // and our chrome is sized to match (no stretching, no blur).
            let n_surfaces = import_shm_buffer(surface, &pixels_arc);
            if n_surfaces > 1 {
                self.toplevels[idx].csd = true;
            }

            // If SHM import produced nothing (width == 0), pull the
            // surface-keyed DMA-BUF pixels populated earlier in this commit.
            // Keying by surface id (rather than "WxH") prevents two surfaces
            // at the same resolution from swapping each other's frames.
            if pixels_arc.lock().unwrap().width == 0 {
                if let Some(data) = self.dmabuf_pending.remove(&surface.id()) {
                    debug!("DMA-BUF: consuming pending {}x{} pixels for toplevel", data.width, data.height);
                    *pixels_arc.lock().unwrap() = data;
                }
            }

            // Also update legacy single-surface buffer if this is the active surface.
            let is_active = self.active_surface.as_ref().map(|s| s == surface).unwrap_or(false);
            if is_active {
                let _ = import_shm_buffer(surface, &self.client_pixels.clone());
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
