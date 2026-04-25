//! Linux DMA-BUF handler — GPU buffer import for hardware-accelerated clients.

use smithay::{
    delegate_dmabuf,
    wayland::dmabuf::{DmabufGlobal, DmabufHandler, DmabufState, ImportNotifier},
};

use crate::state::State;

impl DmabufHandler for State {
    fn dmabuf_state(&mut self) -> &mut DmabufState {
        &mut self.common.dmabuf_state
    }

    fn dmabuf_imported(
        &mut self,
        _global: &DmabufGlobal,
        _dmabuf: smithay::backend::allocator::dmabuf::Dmabuf,
        notifier: ImportNotifier,
    ) {
        // The actual import happens via the renderer (subagent 08).
        // For now, optimistically signal success — the renderer will reject it
        // at draw time if it cannot import the buffer.
        if notifier.successful::<State>().is_err() {
            tracing::warn!("dmabuf_imported: failed to signal success");
        }
    }
}

delegate_dmabuf!(State);
