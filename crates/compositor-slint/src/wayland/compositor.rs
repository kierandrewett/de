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
    wayland::{
        compositor::{
            with_states, CompositorClientState, CompositorHandler, CompositorState,
        },
        shell::xdg::{XdgPopupSurfaceData, XdgToplevelSurfaceData},
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

    fn commit(&mut self, surface: &WlSurface) {
        on_commit_buffer_handler::<Self>(surface);

        // Initial xdg_surface configure must arrive AFTER the client has had
        // a chance to populate app_id/title/decoration mode but BEFORE it
        // commits its first buffer with content. We deferred the
        // send_configure() out of new_toplevel/new_popup; fire it here on
        // the first commit if it has not yet been sent.
        if let Some(toplevel) = self.toplevels.iter().find(|t| &t.surface == surface) {
            let initial_sent = with_states(surface, |states| {
                states
                    .data_map
                    .get::<XdgToplevelSurfaceData>()
                    .map(|d| d.lock().unwrap().initial_configure_sent)
                    .unwrap_or(true)
            });
            if !initial_sent {
                toplevel.toplevel.send_configure();
            }
        }
        if let Some(popup) = self.popups.iter().find(|p| &p.surface == surface) {
            let initial_sent = with_states(surface, |states| {
                states
                    .data_map
                    .get::<XdgPopupSurfaceData>()
                    .map(|d| d.lock().unwrap().initial_configure_sent)
                    .unwrap_or(true)
            });
            if !initial_sent {
                if let Err(e) = popup.popup.send_configure() {
                    tracing::warn!("popup initial send_configure failed: {:?}", e);
                }
            }
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
