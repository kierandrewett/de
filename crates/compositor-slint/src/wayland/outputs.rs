//! Output management — wl_output + xdg-output.
//!
//! OutputManagerState::new_with_xdg_output advertises both protocols.
//! No multi-output handling yet; the single output is "spike-output".

use smithay::{delegate_output, wayland::output::OutputHandler};

use crate::wayland_state::SpikeState;

impl OutputHandler for SpikeState {}

delegate_output!(SpikeState);
