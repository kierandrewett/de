//! Compositor `State` skeleton. Subagent 07 (protocols) owns this file.
//!
//! Subagents 08 (render) and 09 (shell) may add their fields under the marked sections.
//! Keep additions minimal and well-scoped to reduce merge conflicts.

/// Top-level compositor state, threaded through the calloop event loop.
///
/// This is a placeholder — subagent 07 will fill in real fields
/// (Display, CompositorState, XdgShellState, SeatState, etc.).
#[allow(dead_code)]
pub struct State {
    // === wayland fields (subagent 07) ===
    // === render fields (subagent 08) ===
    // === shell fields (subagent 09) ===
}
