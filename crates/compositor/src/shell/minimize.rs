//! Minimize-to-dock and unminimize.
#![allow(dead_code)]
//!
//! ## State machine
//!
//! ```text
//! Visible
//!   │  minimize_window()
//!   ▼
//! Minimizing   ← animation shrinks window toward dock icon rect
//!   │  finish_minimize() called when animation is_complete()
//!   ▼
//! Hidden (is_minimized = true, window excluded from rendering)
//!
//! Hidden
//!   │  unminimize_window()
//!   ▼
//! Unminimizing ← animation grows window from dock icon rect to saved pos
//!   │  finish_unminimize() called when animation is_complete()
//!   ▼
//! Visible (is_minimized = false, focused)
//! ```
//!
//! The compositor's render loop must call [`tick_minimize_animations`] every
//! frame and check the returned event list to drive state transitions.

use smithay::utils::{Point, Rectangle, Size};

use ipc::Rect;

use super::Shell;

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Begin minimizing a window.
///
/// `dock_icon_rect` is the screen-space rect of the dock icon to animate
/// toward (obtained via IPC `GetDockIconPosition`).  If `None`, the window
/// simply disappears after a short fade.
///
/// The window is NOT hidden immediately — call [`finish_minimize`] once
/// the animation settles.
pub fn minimize_window(shell: &mut Shell, id: u64, dock_icon_rect: Option<Rect>) {
    let win = match shell.window_mut(id) {
        Some(w) => w,
        None => {
            tracing::warn!(id, "minimize_window: window not found");
            return;
        }
    };

    if win.is_minimized {
        return;
    }

    // Save geometry for restore.
    win.pre_minimize_rect = Some(win.geometry);

    // Animate toward the dock icon if we have a position, else fade out.
    let target = dock_icon_rect.map(|r| {
        Rectangle::new(Point::from((r.x, r.y)), Size::from((r.w.max(1), r.h.max(1))))
    });

    if let Some(t) = target {
        win.animation.set_geometry_target(t);
    }
    // Always fade out and scale down.
    win.animation.opacity.set_target([0.0]);
    win.animation.scale.set_target([0.0]);

    tracing::debug!(id, "minimize animation started");
}

/// Called by the render loop when the minimize animation has settled.
///
/// Marks the window as hidden and resets animation state.
pub fn finish_minimize(shell: &mut Shell, id: u64) {
    let win = match shell.window_mut(id) {
        Some(w) => w,
        None => return,
    };

    win.is_minimized = true;

    // Remove from focus stack so the next window below gets focus.
    let win_id = win.id;
    shell.focus_stack.retain(|&fid| fid != win_id);

    tracing::debug!(id, "window minimized (hidden)");
}

/// Begin un-minimizing a window.
///
/// `dock_icon_rect` is where the animation should start from (dock icon pos).
/// The window animates back to `pre_minimize_rect`.
pub fn unminimize_window(shell: &mut Shell, id: u64, dock_icon_rect: Option<Rect>) {
    let win = match shell.window_mut(id) {
        Some(w) => w,
        None => {
            tracing::warn!(id, "unminimize_window: window not found");
            return;
        }
    };

    if !win.is_minimized {
        return;
    }

    win.is_minimized = false;

    let restore_rect = win.pre_minimize_rect.unwrap_or(win.geometry);

    // If we know the dock icon position, start the animation from there.
    if let Some(r) = dock_icon_rect {
        let dock_rect = Rectangle::new(
            Point::from((r.x, r.y)),
            Size::from((r.w.max(1), r.h.max(1))),
        );
        win.animation.set_geometry_instant(dock_rect);
        win.animation.opacity.set_position([0.0]);
        win.animation.scale.set_position([0.0]);
    }

    // Spring back to full size.
    win.animation.set_geometry_target(restore_rect);
    win.animation.opacity.set_target([1.0]);
    win.animation.scale.set_target([1.0]);
    win.geometry = restore_rect;

    tracing::debug!(id, ?restore_rect, "unminimize animation started");
}

/// Called by the render loop when the unminimize animation has settled.
///
/// Focuses the window so it receives input.
pub fn finish_unminimize(shell: &mut Shell, id: u64) {
    shell.focus_window(id);
    tracing::debug!(id, "window fully unminimized");
}

/// Walk all windows and emit IDs that need minimize/unminimize finalization.
///
/// The caller should call [`finish_minimize`] or [`finish_unminimize`] for
/// each ID returned.  We separate detection from mutation so that the borrow
/// checker stays happy.
///
/// Returns `(minimizing_done, unminimizing_done)` where each vec contains
/// window IDs whose transition animation just completed.
pub fn tick_minimize_animations(shell: &Shell) -> (Vec<u64>, Vec<u64>) {
    let mut to_finish_minimize = Vec::new();
    let mut to_finish_unminimize = Vec::new();

    for win in &shell.windows {
        // Closing windows also drive opacity → 0 via `begin_close`. Skip
        // them so the close path (`sweep_closed_windows`) handles their
        // teardown — without this we'd race and flip `is_minimized` on a
        // window that's about to be destroyed.
        if win.is_closing {
            continue;
        }
        if win.animation.is_complete() {
            // Check opacity: near-zero means minimize finished, near-one means
            // unminimize finished.
            let opacity = win.animation.current_opacity();
            if !win.is_minimized && opacity < 0.01 {
                to_finish_minimize.push(win.id);
            } else if win.is_minimized && opacity > 0.99 {
                // Shouldn't happen (is_minimized is cleared in unminimize_window)
                // but handle defensively.
                to_finish_unminimize.push(win.id);
            }
        }
    }

    (to_finish_minimize, to_finish_unminimize)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use smithay::utils::{Point, Size};

    use super::*;
    use crate::shell::{DecorationMode, MappedWindow, Shell, WindowSurface};

    fn add_window(shell: &mut Shell) -> u64 {
        let id = shell.alloc_window_id();
        let surface = WindowSurface { token: id };
        let rect = Rectangle::new(
            Point::from((100, 100)),
            Size::from((800, 600)),
        );
        shell.windows.push(MappedWindow::new(
            id,
            surface,
            rect,
            DecorationMode::ServerSide,
            "app".into(),
            "Win".into(),
        ));
        shell.focus_window(id);
        id
    }

    #[test]
    fn minimize_saves_rect_and_sets_animation_target() {
        let mut shell = Shell::new();
        let id = add_window(&mut shell);
        let original = shell.window(id).unwrap().geometry;

        let dock = Rect { x: 800, y: 1020, w: 64, h: 48 };
        minimize_window(&mut shell, id, Some(dock));

        let win = shell.window(id).unwrap();
        assert!(!win.is_minimized); // not hidden yet — animation still running
        assert_eq!(win.pre_minimize_rect, Some(original));
    }

    #[test]
    fn finish_minimize_hides_window_and_removes_focus() {
        let mut shell = Shell::new();
        let id = add_window(&mut shell);

        minimize_window(&mut shell, id, None);
        finish_minimize(&mut shell, id);

        assert!(shell.window(id).unwrap().is_minimized);
        assert!(!shell.focus_stack.contains(&id));
    }

    #[test]
    fn unminimize_restores_geometry_and_clears_flag() {
        let mut shell = Shell::new();
        let id = add_window(&mut shell);
        let original = shell.window(id).unwrap().geometry;

        minimize_window(&mut shell, id, None);
        finish_minimize(&mut shell, id);

        unminimize_window(&mut shell, id, None);

        let win = shell.window(id).unwrap();
        assert!(!win.is_minimized);
        assert_eq!(win.geometry, original);
    }

    #[test]
    fn minimize_already_minimized_is_noop() {
        let mut shell = Shell::new();
        let id = add_window(&mut shell);

        minimize_window(&mut shell, id, None);
        finish_minimize(&mut shell, id);

        let rect_before = shell.window(id).unwrap().pre_minimize_rect;
        minimize_window(&mut shell, id, None); // second call — should be no-op
        let rect_after = shell.window(id).unwrap().pre_minimize_rect;

        assert_eq!(rect_before, rect_after);
    }

    #[test]
    fn unminimize_unknown_id_does_not_panic() {
        let mut shell = Shell::new();
        unminimize_window(&mut shell, 9999, None); // must not panic
    }
}
