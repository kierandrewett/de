//! Output management - wl_output + xdg-output.
//!
//! `OutputManagerState::new_with_xdg_output` advertises both protocols and
//! `delegate_output!` registers dispatch for both `wl_output` and the
//! `zxdg_output_manager_v1` / `zxdg_output_v1` interfaces - there is no
//! separate `delegate_xdg_output!` macro.
//!
//! ## Surface/output binding
//!
//! HiDPI clients gate their buffer scale on `wl_surface.preferred_buffer_scale`
//! (v6+), the legacy `wl_surface.enter` event, and `wp_fractional_scale_v1`.
//! `refresh_surface_outputs` walks every mapped surface tree and updates those
//! protocol signals together so output moves and scale changes cannot drift.

use smithay::{
    delegate_output,
    desktop::utils::with_surfaces_surface_tree,
    output::Output,
    wayland::{
        compositor::{send_surface_state, SurfaceData},
        fractional_scale::with_fractional_scale,
        output::OutputHandler,
    },
};

use crate::wayland_state::SpikeState;

impl OutputHandler for SpikeState {}

delegate_output!(SpikeState);

impl SpikeState {
    /// Back-compat name for the render loop. The implementation is now a full
    /// output-state refresh: `wl_surface.enter`, preferred buffer state,
    /// fractional scale, and the local per-surface output map are updated in
    /// one place so scale changes cannot drift from output membership.
    pub fn bind_surfaces_to_output(&mut self) {
        self.refresh_surface_outputs();
    }

    /// Refresh the surface/output binding for every mapped surface tree.
    pub fn refresh_surface_outputs(&mut self) {
        let Some(output) = self.primary_output() else {
            return;
        };
        let output = output.clone();

        let mut roots: Vec<(
            smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
            Output,
        )> = Vec::new();
        roots.extend(
            self.toplevels
                .iter()
                .map(|tl| (tl.surface.clone(), output.clone())),
        );
        roots.extend(
            self.popups
                .iter()
                .map(|popup| (popup.surface.clone(), output.clone())),
        );
        roots.extend(
            self.layer_surfaces
                .iter()
                .map(|layer| (layer.surface.wl_surface().clone(), layer.output.clone())),
        );
        roots.extend(
            self.lock_surfaces
                .iter()
                .map(|lock| (lock.surface.wl_surface().clone(), lock.output.clone())),
        );
        if let smithay::input::pointer::CursorImageStatus::Surface(surface) = &self.cursor_status {
            roots.push((surface.clone(), output.clone()));
        }
        if let Some(icon) = &self.dnd_icon {
            roots.push((icon.surface.clone(), output));
        }

        for (surface, output) in roots {
            self.refresh_surface_tree_output(&surface, &output);
        }
    }

    fn refresh_surface_tree_output(
        &mut self,
        surface: &smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
        output: &Output,
    ) {
        let scale = output.current_scale().integer_scale();
        let fractional_scale = output.current_scale().fractional_scale();
        let transform = output.current_transform();
        let mut updates = Vec::new();

        with_surfaces_surface_tree(surface, |sub, states: &SurfaceData| {
            updates.push(sub.clone());

            output.enter(sub);
            send_surface_state(sub, states, scale, transform);
            with_fractional_scale(states, |fractional| {
                fractional.set_preferred_scale(fractional_scale);
            });
        });

        for sub in updates {
            use smithay::reexports::wayland_server::Resource;

            if let Some(previous) = self.surface_outputs.insert(sub.id(), output.clone()) {
                if previous != *output {
                    previous.leave(&sub);
                }
            }
        }
    }
}
