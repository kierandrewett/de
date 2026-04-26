//! ext-foreign-toplevel-list-v1.
//!
//! Powers taskbars, docks, and app-switchers that list open windows.
//! The handler is trivial: smithay manages the list; we just expose state.

use smithay::{
    delegate_foreign_toplevel_list,
    wayland::foreign_toplevel_list::{ForeignToplevelListHandler, ForeignToplevelListState},
};

use crate::wayland_state::SpikeState;

impl ForeignToplevelListHandler for SpikeState {
    fn foreign_toplevel_list_state(&mut self) -> &mut ForeignToplevelListState {
        &mut self.foreign_toplevel_list_state
    }
}

delegate_foreign_toplevel_list!(SpikeState);
