//! Compositor `State` skeleton. Subagent 07 (protocols) owns this file.
// Not yet wired from main() — suppress dead_code until subagent 07 fills the backend.
#![allow(dead_code)]
//!
//! Subagents 08 (render) and 09 (shell) may add their fields under the marked sections.
//! Keep additions minimal and well-scoped to reduce merge conflicts.

use crate::shell::{Shell, grab::GrabState};

/// Top-level compositor state, threaded through the calloop event loop.
///
/// This is a placeholder — subagent 07 will fill in real fields
/// (Display, CompositorState, XdgShellState, SeatState, etc.).
pub struct State {
    // === wayland fields (subagent 07) ===

    // === render fields (subagent 08) ===

    // === shell fields (subagent 09) ===
    /// Window management state: layout, focus, animations.
    pub shell: Shell,
    /// Active pointer grab (move or resize), if any.
    pub grab: GrabState,
}

impl State {
    /// Create an empty compositor state.
    pub fn new() -> Self {
        Self {
            shell: Shell::new(),
            grab: GrabState::default(),
        }
    }
}

impl Default for State {
    fn default() -> Self {
        Self::new()
    }
}
