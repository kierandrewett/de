//! wlr-layer-shell-v1 handler.
//!
//! Panels, docks, wallpapers, overlays, notification popups, lock screens —
//! all use wlr-layer-shell to anchor surfaces to screen edges.
//!
//! Public API for the render side:
//!   `state.layer_surfaces()` — iterate all mapped layer surfaces (LayerInfo).

use smithay::{
    delegate_layer_shell,
    reexports::wayland_server::protocol::wl_output,
    wayland::shell::wlr_layer::{
        Layer, LayerSurface as WlrLayerSurface, WlrLayerShellHandler, WlrLayerShellState,
    },
};
use tracing::{info, warn};

use crate::wayland_state::SpikeState;

// ──────────────────────────────────────────────────────────────────────────────
// Public data exposed to the render side
// ──────────────────────────────────────────────────────────────────────────────

/// Metadata the render side needs to position and z-order layer surfaces.
#[derive(Debug, Clone)]
pub struct LayerInfo {
    pub surface: WlrLayerSurface,
    pub layer: Layer,
    pub namespace: String,
}

// ──────────────────────────────────────────────────────────────────────────────
// Handler impl
// ──────────────────────────────────────────────────────────────────────────────

impl WlrLayerShellHandler for SpikeState {
    fn shell_state(&mut self) -> &mut WlrLayerShellState {
        &mut self.layer_shell_state
    }

    fn new_layer_surface(
        &mut self,
        surface: WlrLayerSurface,
        _wl_output: Option<wl_output::WlOutput>,
        layer: Layer,
        namespace: String,
    ) {
        info!(ns = %namespace, layer = ?layer, "layer_shell: new surface");

        // Send the initial configure so the client knows output geometry.
        // (0, 0) size hint → client chooses its own size.
        // The `layer` field is on LayerSurfaceCachedState, set by the client;
        // we just need to send a configure to unblock the client.
        surface.send_configure();

        self.layer_surfaces.push(LayerInfo {
            surface,
            layer,
            namespace,
        });
    }

    fn layer_destroyed(&mut self, surface: WlrLayerSurface) {
        info!("layer_shell: surface destroyed");
        self.layer_surfaces.retain(|li| li.surface != surface);
    }
}

delegate_layer_shell!(SpikeState);

// ──────────────────────────────────────────────────────────────────────────────
// Public accessor used by render/positioning code
// ──────────────────────────────────────────────────────────────────────────────

impl SpikeState {
    /// Return all currently-mapped layer surfaces, ordered as received.
    /// Render code should iterate and z-sort by `LayerInfo::layer`.
    pub fn layer_surfaces(&self) -> &[LayerInfo] {
        &self.layer_surfaces
    }
}
