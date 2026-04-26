//! Linux DMA-BUF handler — GPU buffer import for hardware-accelerated clients.
//!
//! Without this every GTK4 / wgpu / Vulkan client (gnome-help, the panel,
//! the dock, anything iced-on-wgpu) fails its renderer init: their EGL/
//! Vulkan probe needs to allocate a GPU-shared buffer, and the wayland
//! way to do that is `zwp_linux_dmabuf_v1`. The protocol global itself
//! is created in `winit.rs` (where the renderer lives so we can query
//! formats + feedback); this file owns the per-import dispatch.

use smithay::{
    backend::{allocator::dmabuf::Dmabuf, renderer::ImportDma},
    delegate_dmabuf,
    wayland::dmabuf::{DmabufGlobal, DmabufHandler, DmabufState, ImportNotifier},
};

use crate::state::{Backend, State};

impl DmabufHandler for State {
    fn dmabuf_state(&mut self) -> &mut DmabufState {
        &mut self.common.dmabuf_state
    }

    fn dmabuf_imported(
        &mut self,
        _global: &DmabufGlobal,
        dmabuf: Dmabuf,
        notifier: ImportNotifier,
    ) {
        // Try the import on the active renderer. If the GPU rejects it
        // (unsupported format / modifier combination) we have to tell
        // the client immediately — otherwise they'll commit a buffer
        // that we can't sample, causing a black surface or a protocol
        // disconnect on first frame.
        let result = match &mut self.backend {
            Backend::Winit(w) => w.backend.renderer().import_dmabuf(&dmabuf, None),
            Backend::Udev(_) => {
                // Udev backend ships its own per-output renderer; until
                // that path is wired, optimistically accept and let the
                // first frame fail loudly if the GPU refuses.
                let _ = notifier.successful::<State>();
                return;
            }
        };
        match result {
            Ok(_texture) => {
                if notifier.successful::<State>().is_err() {
                    tracing::warn!("dmabuf_imported: failed to signal success to client");
                }
            }
            Err(e) => {
                tracing::warn!("dmabuf import rejected by renderer: {e:?}");
                notifier.failed();
            }
        }
    }
}

delegate_dmabuf!(State);
