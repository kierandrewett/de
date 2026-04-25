//! Clipboard and selection handlers:
//! wl_data_device, primary-selection, ext-data-control, wlr-data-control.

use smithay::{
    delegate_data_device, delegate_ext_data_control, delegate_data_control,
    delegate_primary_selection,
    wayland::selection::{
        data_device::{
            DataDeviceHandler, DataDeviceState, WaylandDndGrabHandler,
        },
        ext_data_control::{DataControlHandler, DataControlState},
        primary_selection::{PrimarySelectionHandler, PrimarySelectionState},
        wlr_data_control::DataControlHandler as WlrDataControlHandler,
        SelectionHandler,
    },
};

use crate::state::State;

// ─── DataDevice (copy/paste + drag-and-drop) ─────────────────────────────────

impl SelectionHandler for State {
    type SelectionUserData = ();
}

impl DataDeviceHandler for State {
    fn data_device_state(&mut self) -> &mut DataDeviceState {
        &mut self.common.data_device_state
    }
}

impl WaylandDndGrabHandler for State {}

delegate_data_device!(State);

// ─── PrimarySelection (middle-click paste) ────────────────────────────────────

impl PrimarySelectionHandler for State {
    fn primary_selection_state(&mut self) -> &mut PrimarySelectionState {
        &mut self.common.primary_selection_state
    }
}

delegate_primary_selection!(State);

// ─── ExtDataControl (clipboard managers — ext variant) ────────────────────────

impl DataControlHandler for State {
    fn data_control_state(&mut self) -> &mut DataControlState {
        &mut self.common.data_control_state
    }
}

delegate_ext_data_control!(State);

// ─── WlrDataControl (clipboard managers — wlr legacy variant) ────────────────

impl WlrDataControlHandler for State {
    fn data_control_state(
        &mut self,
    ) -> &mut smithay::wayland::selection::wlr_data_control::DataControlState {
        &mut self.common.wlr_data_control_state
    }
}

delegate_data_control!(State);
