//! macOS-style maximize and unmaximize.
#![allow(dead_code)]
//!
//! Maximize grows the window to the monitor work area via a critically-damped
//! spring (stiffness 800, damping 1.0).  The title bar and rounded corners are
//! **kept** — this is intentionally different from GNOME's fullscreen-like
//! maximise.  `pre_maximize_rect` is saved so the window can spring back on
//! unmaximize.

use smithay::utils::{Logical, Rectangle};

use super::{MonitorInfo, Shell};

/// Maximize a window to fill the work area of `monitor`.
///
/// Does nothing if the window is already maximized, fullscreen, or not found.
pub fn maximize_window(shell: &mut Shell, id: u64, monitor: &MonitorInfo) {
    let work_area = monitor.work_area();

    let win = match shell.window_mut(id) {
        Some(w) => w,
        None => {
            tracing::warn!(id, "maximize_window: window not found");
            return;
        }
    };

    if win.is_maximized || win.is_fullscreen {
        return;
    }

    win.pre_maximize_rect = Some(win.geometry);
    win.is_maximized = true;
    win.snap_state = None;
    win.animation.set_geometry_target(work_area);
    // Commit geometry immediately so the client gets sized correctly.
    win.geometry = work_area;

    tracing::debug!(id, ?work_area, "window maximized");
}

/// Restore a maximized window to its saved pre-maximize geometry.
///
/// Does nothing if the window is not maximized or has no saved rect.
pub fn unmaximize_window(shell: &mut Shell, id: u64) {
    let win = match shell.window_mut(id) {
        Some(w) => w,
        None => {
            tracing::warn!(id, "unmaximize_window: window not found");
            return;
        }
    };

    if !win.is_maximized {
        return;
    }

    let restore = match win.pre_maximize_rect.take() {
        Some(r) => r,
        None => {
            // No saved rect — just un-flag without moving.
            win.is_maximized = false;
            return;
        }
    };

    win.is_maximized = false;
    win.animation.set_geometry_target(restore);
    win.geometry = restore;

    tracing::debug!(id, ?restore, "window unmaximized");
}

/// Toggle between maximized and restored.
pub fn toggle_maximize(shell: &mut Shell, id: u64, monitor: &MonitorInfo) {
    let is_max = shell.window(id).map(|w| w.is_maximized).unwrap_or(false);
    if is_max {
        unmaximize_window(shell, id);
    } else {
        maximize_window(shell, id, monitor);
    }
}

/// Apply a snap target that is equivalent to full-maximize (i.e. `Maximize`
/// from the snapping module) without saving `pre_maximize_rect`.
///
/// Used when a window is snapped to the top edge mid-drag.
pub fn snap_maximize(shell: &mut Shell, id: u64, work_area: Rectangle<i32, Logical>) {
    let win = match shell.window_mut(id) {
        Some(w) => w,
        None => return,
    };

    if !win.is_maximized {
        win.pre_maximize_rect = Some(win.geometry);
        win.is_maximized = true;
    }

    win.animation.set_geometry_target(work_area);
    win.geometry = work_area;
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use smithay::utils::{Point, Size};

    use super::*;
    use crate::shell::{DecorationMode, MappedWindow, MonitorInfo, Shell, WindowSurface};

    fn make_monitor() -> MonitorInfo {
        MonitorInfo {
            name: "test".to_string(),
            logical_rect: Rectangle::new(
                Point::from((0, 0)),
                Size::from((1920, 1080)),
            ),
            panel_height: 30,
            dock_height: 70,
        }
    }

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
            "Test".into(),
        ));
        id
    }

    #[test]
    fn maximize_sets_flag_and_saves_pre_rect() {
        let mut shell = Shell::new();
        let id = add_window(&mut shell);
        let original = shell.window(id).unwrap().geometry;
        let monitor = make_monitor();

        maximize_window(&mut shell, id, &monitor);

        let win = shell.window(id).unwrap();
        assert!(win.is_maximized);
        assert_eq!(win.pre_maximize_rect, Some(original));
        assert_eq!(win.geometry, monitor.work_area());
    }

    #[test]
    fn unmaximize_restores_saved_rect() {
        let mut shell = Shell::new();
        let id = add_window(&mut shell);
        let original = shell.window(id).unwrap().geometry;
        let monitor = make_monitor();

        maximize_window(&mut shell, id, &monitor);
        unmaximize_window(&mut shell, id);

        let win = shell.window(id).unwrap();
        assert!(!win.is_maximized);
        assert_eq!(win.geometry, original);
        assert!(win.pre_maximize_rect.is_none());
    }

    #[test]
    fn double_maximize_is_idempotent() {
        let mut shell = Shell::new();
        let id = add_window(&mut shell);
        let original = shell.window(id).unwrap().geometry;
        let monitor = make_monitor();

        maximize_window(&mut shell, id, &monitor);
        maximize_window(&mut shell, id, &monitor); // second call should be a no-op

        let win = shell.window(id).unwrap();
        // pre_maximize_rect should still point to original, not to the work area.
        assert_eq!(win.pre_maximize_rect, Some(original));
    }

    #[test]
    fn toggle_maximize_cycles_correctly() {
        let mut shell = Shell::new();
        let id = add_window(&mut shell);
        let monitor = make_monitor();

        toggle_maximize(&mut shell, id, &monitor);
        assert!(shell.window(id).unwrap().is_maximized);

        toggle_maximize(&mut shell, id, &monitor);
        assert!(!shell.window(id).unwrap().is_maximized);
    }

    #[test]
    fn maximize_unknown_id_does_not_panic() {
        let mut shell = Shell::new();
        let monitor = make_monitor();
        maximize_window(&mut shell, 9999, &monitor); // must not panic
    }
}
