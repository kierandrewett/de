//! Wayland protocol handler modules.
//!
//! Each sub-module implements the smithay handler trait(s) for one protocol
//! group and calls the matching `delegate_*!` macro.  The `SpikeState` struct
//! stays in `wayland_state.rs`; only the trait impls live here.

pub mod compositor;
pub mod decoration;
pub mod idle;
pub mod input;
pub mod layer_shell;
pub mod misc;
pub mod outputs;
pub mod presentation;
pub mod scaling;
pub mod session_lock;
pub mod timing;
pub mod toplevel;
pub mod xdg_shell;
pub mod xwayland;
