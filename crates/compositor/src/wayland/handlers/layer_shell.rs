//! wlr-layer-shell handler — panels, docks, wallpapers, overlays.

use smithay::{
    delegate_layer_shell,
    desktop::{layer_map_for_output, LayerSurface},
    output::Output,
    reexports::wayland_server::protocol::wl_output,
    wayland::shell::wlr_layer::{
        Layer, LayerSurface as WlrLayerSurface, WlrLayerShellHandler, WlrLayerShellState,
    },
};

use crate::state::State;

impl WlrLayerShellHandler for State {
    fn shell_state(&mut self) -> &mut WlrLayerShellState {
        &mut self.common.layer_shell_state
    }

    fn new_layer_surface(
        &mut self,
        surface: WlrLayerSurface,
        wl_output: Option<wl_output::WlOutput>,
        layer: Layer,
        namespace: String,
    ) {
        // Find the requested output or fall back to the first available output.
        let output = wl_output
            .as_ref()
            .and_then(|o| {
                self.common
                    .space
                    .outputs()
                    .find(|out| out.owns(o))
                    .cloned()
            })
            .or_else(|| self.common.space.outputs().next().cloned());

        let output = match output {
            Some(o) => o,
            None => {
                tracing::warn!("new_layer_surface: no output available, closing surface");
                surface.send_close();
                return;
            }
        };

        let mut map = layer_map_for_output(&output);
        let layer_surface = LayerSurface::new(surface, namespace.clone());
        if let Err(e) = map.map_layer(&layer_surface) {
            tracing::warn!("layer_shell: map_layer failed for ns={namespace:?}: {e:?}");
        } else {
            tracing::info!(
                output = %output.name(),
                ns = %namespace,
                layer = ?layer,
                "layer_shell: surface mapped",
            );
        }
    }

    fn layer_destroyed(&mut self, surface: WlrLayerSurface) {
        // Remove from whichever output's layer map owns this surface.
        let outputs: Vec<Output> = self.common.space.outputs().cloned().collect();
        for output in &outputs {
            let mut map = layer_map_for_output(output);
            let layer = map
                .layers()
                .find(|l| l.layer_surface() == &surface)
                .cloned();
            if let Some(layer) = layer {
                map.unmap_layer(&layer);
                break;
            }
        }
    }
}

delegate_layer_shell!(State);
