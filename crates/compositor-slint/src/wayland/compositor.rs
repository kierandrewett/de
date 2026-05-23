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
//!
//! We use the buffer type from `with_renderer_surface_state` to decide.

use smithay::{
    backend::renderer::utils::{on_commit_buffer_handler, with_renderer_surface_state},
    delegate_compositor,
    reexports::wayland_server::{protocol::wl_surface::WlSurface, Client, Resource},
    wayland::{
        compositor::{
            add_blocker, add_pre_commit_hook, get_parent, is_sync_subsurface, with_states,
            BufferAssignment, CompositorClientState, CompositorHandler, CompositorState,
            SurfaceAttributes,
        },
        dmabuf::get_dmabuf,
        shell::xdg::{XdgPopupSurfaceData, XdgToplevelSurfaceData},
    },
    xwayland::XWaylandClientData,
};
use smithay::reexports::calloop::Interest;
use tracing::debug;

use crate::wayland_state::{import_shm_buffer, import_shm_per_surface};
use crate::wayland_state::{ClientState, ClientSurfaceData, SpikeState};

impl CompositorHandler for SpikeState {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor_state
    }

    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        // A client may have one of several user-data types: our regular
        // `ClientState` for normal wayland clients, `XWaylandClientData`
        // for the XWayland WM-side connection, or some other data we
        // don't know about. Try each in turn; if none match, fall back
        // to a process-wide empty `CompositorClientState` (the `'static`
        // lifetime of the `OnceLock` is compatible with any `'a`).
        if let Some(state) = client.get_data::<ClientState>() {
            return &state.compositor_state;
        }
        if let Some(state) = client.get_data::<XWaylandClientData>() {
            return &state.compositor_state;
        }
        static FALLBACK: std::sync::OnceLock<CompositorClientState> = std::sync::OnceLock::new();
        FALLBACK.get_or_init(CompositorClientState::default)
    }

    fn new_surface(&mut self, surface: &WlSurface) {
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

            let Some(dmabuf) = maybe_dmabuf else {
                return;
            };

            let Ok((blocker, source)) = dmabuf.generate_blocker(Interest::READ) else {
                return;
            };

            let Some(client) = surface.client() else {
                return;
            };

            let res = state.loop_handle.insert_source(source, move |_, _, state| {
                let dh = state.display_handle.clone();
                state.client_compositor_state(&client).blocker_cleared(state, &dh);
                Ok(())
            });

            if res.is_ok() {
                add_blocker(surface, blocker);
            }
        });
    }

    fn commit(&mut self, surface: &WlSurface) {
        on_commit_buffer_handler::<Self>(surface);

        // Drive smithay's PopupManager through this commit. It walks the
        // popup tree rooted at `surface` and sends `xdg_popup.configure`
        // events when geometry changed (required by xdg-shell spec).
        // Idempotent when surface isn't a popup or no popup is mid-config.
        self.popup_manager.commit(surface);

        // wp_fifo_v1 barrier: signal the just-committed barrier IMMEDIATELY,
        // before any subsequent commit can overwrite `current.barrier`.
        //
        // The naive approach (signal all barriers from a per-frame helper)
        // deadlocks when the client back-to-back-commits without giving us
        // a main-loop tick between them: commit N+1's `set_barrier` lands
        // a fresh Arc in `current.barrier`, dropping the reference we held
        // to commit N's barrier. The pending blocker on commit N+1's
        // `wait_barrier` then references the orphaned barrier N Arc and
        // never resolves. Signalling here, in the commit handler, ties
        // the signal to the exact commit that produced the barrier and
        // sidesteps the race entirely. The semantic cost is that we
        // signal "presented" before actually presenting the buffer, but
        // that just removes throttling — we already render at vsync
        // through winit's own swap, so the client still gets paced.
        //
        // After signalling, drive the client's blocker_cleared so smithay
        // re-checks any commit transactions that were waiting on this
        // barrier (anvil does the same: signal + insert client → call
        // blocker_cleared on each).
        let mut signaled_barrier = false;
        {
            use smithay::wayland::compositor::with_states;
            use smithay::wayland::fifo::FifoBarrierCachedState;
            with_states(surface, |states| {
                let mut guard = states.cached_state.get::<FifoBarrierCachedState>();
                if let Some(b) = guard.current().barrier.take() {
                    b.signal();
                    signaled_barrier = true;
                }
                if let Some(b) = guard.pending().barrier.take() {
                    b.signal();
                    signaled_barrier = true;
                }
            });
        }
        if signaled_barrier {
            use smithay::reexports::wayland_server::Resource;
            tracing::debug!(
                "fifo: per-commit signal on surface_id={}",
                surface.id().protocol_id(),
            );
            if let Some(client) = surface.client() {
                let dh = self.display_handle.clone();
                let ccs_ptr: *const smithay::wayland::compositor::CompositorClientState =
                    self.client_compositor_state(&client) as *const _;
                // SAFETY: the CompositorClientState is owned by the
                // ClientState user-data on this client, pinned for the
                // life of the connection.
                let ccs = unsafe { &*ccs_ptr };
                ccs.blocker_cleared(self, &dh);
            }
        }

        // Sync subsurfaces stage their state into the parent's pending tree
        // and become visible only when the parent commits — running the
        // root-buffer recomposite now would re-read stale parent state and
        // waste a frame's worth of allocation. The parent's later commit
        // will walk this subsurface in `with_surface_tree_downward`.
        if is_sync_subsurface(surface) {
            return;
        }

        // If the surface's current buffer is a DMA-BUF, run the GPU readback
        // now. The pre-commit blocker installed in `new_surface` waits for
        // the DMA-BUF read fence first, so the import path never samples a
        // producer-owned buffer mid-write.
        let dmabuf = with_renderer_surface_state(surface, |s| {
            s.buffer().and_then(|b| get_dmabuf(b).cloned().ok())
        })
        .flatten();
        if let Some(dmabuf) = dmabuf {
            self.import_dmabuf_for_surface(surface, &dmabuf);
        }

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
                if let Some(t) = toplevel.toplevel.as_ref() {
                    t.send_configure();
                }
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

        // Walk up the wl_subsurface parent chain to the root. The
        // wl_subsurface protocol guarantees this graph is acyclic, so an
        // unbounded walk cannot hang.
        let mut root: WlSurface = surface.clone();
        while let Some(p) = get_parent(&root) {
            root = p;
        }

        // If this commit is for the current cursor surface (a client set a
        // wl_surface as its cursor via `wl_pointer.set_cursor`), import the
        // pixels into the dedicated cursor buffer. update_windows pushes
        // them to Slint's cursor-image each frame so animated cursors and
        // dynamic in-app cursors render correctly.
        use smithay::input::pointer::CursorImageStatus;
        if let CursorImageStatus::Surface(cursor_surf) = &self.cursor_status {
            if cursor_surf == surface || *cursor_surf == root {
                // Animated client cursors (Mesa libwayland-cursor, GTK
                // throbbers, etc.) page through frames by setting a
                // wl_surface.offset on each commit. The protocol says
                // the hotspot is implicitly adjusted by the inverse
                // delta — without this, the cursor's pivot point drifts
                // every frame.
                use smithay::input::pointer::CursorImageSurfaceData;
                with_states(cursor_surf, |states| {
                    let buffer_delta = states
                        .cached_state
                        .get::<SurfaceAttributes>()
                        .current()
                        .buffer_delta
                        .take();
                    if let (Some(delta), Some(attrs_data)) = (
                        buffer_delta,
                        states.data_map.get::<CursorImageSurfaceData>(),
                    ) {
                        if let Ok(mut attrs) = attrs_data.lock() {
                            attrs.hotspot -= delta;
                        }
                    }
                });
                let pixels = self.cursor_surface_pixels.clone();
                let _ = import_shm_buffer(cursor_surf, &pixels);
                return;
            }
        }

        // Same idea for the active DnD icon surface — clients commit pixels
        // to it like any other surface but we route into a dedicated buffer
        // because it's a transient overlay drawn by the renderer under the
        // cursor, not a window the WM tracks.
        //
        // We also accumulate the wl_surface.offset (= buffer_delta) into
        // the DndIcon's offset field. Clients use this as the icon's
        // hotspot relative to its top-left; the renderer subtracts it
        // from the cursor position when blitting so the hotspot lands
        // exactly on the pointer.
        if let Some(icon_surf) = self.dnd_icon.as_ref().map(|i| i.surface.clone()) {
            if icon_surf == *surface || icon_surf == root {
                let buffer_delta = with_states(&icon_surf, |states| {
                    states
                        .cached_state
                        .get::<SurfaceAttributes>()
                        .current()
                        .buffer_delta
                        .take()
                });
                if let (Some(delta), Some(icon)) = (buffer_delta, self.dnd_icon.as_mut()) {
                    icon.offset += delta;
                }
                let pixels = self.dnd_icon_pixels.clone();
                let _ = import_shm_buffer(&icon_surf, &pixels);
                return;
            }
        }

        // Lock surface (ext-session-lock-v1): import the buffer for the
        // matching LockSurfaceInfo, then — on the FIRST committed lock
        // surface — take the pending SessionLocker and call `.lock()` to
        // flip the protocol to "locked". The renderer's gate consumes
        // `session_locked` next frame and stops rendering everything
        // else.
        let lock_idx = self
            .lock_surfaces
            .iter()
            .position(|li| li.surface.wl_surface() == &root);
        if let Some(lidx) = lock_idx {
            let pixels_arc = self.lock_surfaces[lidx].pixels.clone();
            let _ = import_shm_buffer(&root, &pixels_arc);
            if let Some(data) = self.dmabuf_pending.remove(&root.id()) {
                *pixels_arc.lock().unwrap() = data;
            }
            if let Some(locker) = self.pending_session_lock.take() {
                if pixels_arc.lock().unwrap().width > 0 {
                    locker.lock();
                    self.session_locked = true;
                    debug!("session lock confirmed: first lock surface committed pixels");
                } else {
                    self.pending_session_lock = Some(locker);
                }
            }
            return;
        }

        // If `root` is a popup surface (or `surface` itself is a popup that
        // has no wl_subsurface parent), import for the popup's pixel buffer
        // and bail before falling through to toplevel handling.
        let popup_idx = self
            .popups
            .iter()
            .position(|p| p.surface == root || p.surface == *surface);
        if let Some(pidx) = popup_idx {
            let popup_surf = self.popups[pidx].surface.clone();
            let pixels_arc = self.popups[pidx].pixels.clone();
            let surface_pixels_arc = self.popups[pidx].surface_pixels.clone();
            let _ = import_shm_buffer(&popup_surf, &pixels_arc);
            let _ = import_shm_per_surface(&popup_surf, &surface_pixels_arc);
            // Consume DMA-BUF pending pixels populated by import_dmabuf_for_surface
            // earlier in this commit. Layer surfaces + toplevels already do this;
            // popups were left out, which is why GTK context menus (which use
            // DMA-BUF via libwayland-cursor / GL) appeared to "not show" —
            // popup.pixels stayed at width=0 and the renderer skipped them.
            if pixels_arc.lock().unwrap().width == 0 {
                if let Some(data) = self.dmabuf_pending.remove(&popup_surf.id()) {
                    debug!(
                        "DMA-BUF: consuming pending {}x{} pixels for popup",
                        data.width, data.height
                    );
                    *pixels_arc.lock().unwrap() = data;
                }
            }
            // Read xdg_surface.set_window_geometry — the VISIBLE rect within
            // the popup's buffer. Firefox/Chromium/Electron paint a drop-
            // shadow gutter into the buffer; without this the gutter renders
            // as a "large border around the context menu".
            let (gx, gy, gw, gh) = with_states(&popup_surf, |states| {
                let mut cs = states
                    .cached_state
                    .get::<smithay::wayland::shell::xdg::SurfaceCachedState>();
                cs.current()
                    .geometry
                    .map(|r| (r.loc.x, r.loc.y, r.size.w, r.size.h))
                    .unwrap_or((0, 0, 0, 0))
            });
            if gw > 0 && gh > 0 {
                self.popups[pidx].geom_x = gx;
                self.popups[pidx].geom_y = gy;
                self.popups[pidx].geom_w = gw;
                self.popups[pidx].geom_h = gh;
            }
            return;
        }

        // Layer-shell surface: import the buffer (SHM or DMA-BUF) into the
        // LayerInfo pixel buffer right here on commit. Doing the SHM import
        // per-commit (rather than every iteration of `refresh_layer_layout`)
        // means `pixels.dirty` only flips when the client actually paints,
        // which the renderer uses to skip rebuilding the Slint layers model
        // on idle frames.
        let layer_idx = self
            .layer_surfaces
            .iter()
            .position(|li| li.surface.wl_surface() == &root);
        if let Some(idx) = layer_idx {
            let pixels_arc = self.layer_surfaces[idx].pixels.clone();
            let _ = import_shm_buffer(&root, &pixels_arc);
            if let Some(data) = self.dmabuf_pending.remove(&root.id()) {
                debug!(
                    "DMA-BUF: consuming pending {}x{} pixels for layer surface",
                    data.width, data.height
                );
                *pixels_arc.lock().unwrap() = data;
            }
            return;
        }

        let toplevel_idx = self.toplevels.iter().position(|t| t.surface == root);
        debug!(
            "commit: surface_is_toplevel={} root_in_toplevels={}",
            surface == &root,
            toplevel_idx.is_some()
        );
        let surface = &root;
        if let Some(idx) = toplevel_idx {
            let pixels_arc = self.toplevels[idx].pixels.clone();

            // Composite the surface tree (toplevel + subsurfaces) into
            // a single RGBA buffer. Cropping to the client's
            // `xdg_surface.set_window_geometry` rect happens on the
            // Slint side, so CSD clients' shadow/corner padding is
            // excluded there and our chrome stays 1:1 with the visible
            // window (no stretching, no blur).
            //
            // The return value's surface-count used to flip `csd = true`
            // when > 1 — a pixel/topology heuristic that's been removed.
            // SSD vs CSD is now protocol-authoritative (xdg-decoration /
            // KDE-server-decoration handlers + `ack_configure`).
            let _ = import_shm_buffer(surface, &pixels_arc);

            // Per-surface render-element model (runs alongside the legacy
            // composite during the staged rewrite). Imports each surface
            // in the tree separately so the renderer can position and
            // source-clip them independently — the prerequisite for
            // damage tracking, animated subsurfaces, and per-surface
            // buffer_transform.
            let surface_pixels_arc = self.toplevels[idx].surface_pixels.clone();
            let _ = import_shm_per_surface(surface, &surface_pixels_arc);

            // If SHM import produced nothing (width == 0), pull the
            // surface-keyed DMA-BUF pixels populated earlier in this commit.
            // Keying by surface id (rather than "WxH") prevents two surfaces
            // at the same resolution from swapping each other's frames.
            if pixels_arc.lock().unwrap().width == 0 {
                if let Some(data) = self.dmabuf_pending.remove(&surface.id()) {
                    debug!(
                        "DMA-BUF: consuming pending {}x{} pixels for toplevel",
                        data.width, data.height
                    );
                    // Mirror into the per-surface map; legacy pixels_arc
                    // gets the owned buffer. Cloning is unavoidable while
                    // both code paths coexist (~25 readers depend on
                    // pixels_arc.pixels). Future Phase 5 work will
                    // eliminate one of them.
                    use smithay::reexports::wayland_server::Resource;
                    let key = surface.id().protocol_id();
                    {
                        let mut sp = surface_pixels_arc.lock().unwrap();
                        let prev_version = sp.get(&key).map(|d| d.version).unwrap_or(0);
                        sp.insert(
                            key,
                            crate::wayland_state::ClientSurfaceData {
                                pixels: data.pixels.clone(),
                                width: data.width,
                                height: data.height,
                                dirty: true,
                                version: prev_version.wrapping_add(1),
                                last_commit: None,
                            },
                        );
                    }
                    *pixels_arc.lock().unwrap() = data;
                }
            }

            // Also update legacy single-surface buffer if this is the active surface.
            let is_active = self
                .active_surface
                .as_ref()
                .map(|s| s == surface)
                .unwrap_or(false);
            if is_active {
                let _ = import_shm_buffer(surface, &self.client_pixels.clone());
                // Sync DMA-BUF data to legacy buffer too.
                let current = pixels_arc.lock().unwrap().clone();
                if current.width > 0 {
                    let mut legacy = self.client_pixels.lock().unwrap();
                    let next_version = legacy.version.wrapping_add(1);
                    *legacy = ClientSurfaceData {
                        pixels: current.pixels,
                        width: current.width,
                        height: current.height,
                        dirty: true,
                        version: next_version,
                        last_commit: None,
                    };
                }
            }

            if self.finish_xdg_resize_transaction_commit(surface) {
                debug!(
                    "resize transaction complete: final configure acked and committed"
                );
            }
        }
    }
}

delegate_compositor!(SpikeState);
