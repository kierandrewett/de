//! Floating window layout: cascade placement and interactive positioning.
#![allow(dead_code)]

use smithay::utils::{Logical, Point, Rectangle, Size};

use super::{DecorationMode, MappedWindow, MonitorInfo, Shell, WindowSurface};

/// Horizontal / vertical cascade step between successive new windows.
const CASCADE_STEP: i32 = 30;

/// Default window size when none is requested by the client.
const DEFAULT_WIDTH: i32 = 800;
const DEFAULT_HEIGHT: i32 = 600;

/// Compute the next cascade position for a new window on `monitor`.
///
/// Windows are placed starting from the work-area origin with a
/// [`CASCADE_STEP`]-pixel diagonal offset per existing visible window.
/// If the computed position would push the window fully off-screen, it wraps
/// back to the origin.
pub fn cascade_position(
    shell: &Shell,
    monitor: &MonitorInfo,
    window_size: Size<i32, Logical>,
) -> Point<i32, Logical> {
    let wa = monitor.work_area();
    let visible_count = shell
        .visible_windows()
        .filter(|w| {
            monitor
                .logical_rect
                .contains(w.geometry.loc)
        })
        .count() as i32;

    let raw_x = wa.loc.x + CASCADE_STEP * visible_count;
    let raw_y = wa.loc.y + CASCADE_STEP * visible_count;

    // Wrap if the window would be placed outside the work area.
    let max_x = wa.loc.x + wa.size.w - window_size.w;
    let max_y = wa.loc.y + wa.size.h - window_size.h;

    let x = if raw_x > max_x.max(wa.loc.x) { wa.loc.x } else { raw_x };
    let y = if raw_y > max_y.max(wa.loc.y) { wa.loc.y } else { raw_y };

    Point::from((x, y))
}

/// Add a new window to the shell on the active workspace, placing it via
/// cascade.  Returns the assigned window ID.
pub fn place_new_window(
    shell: &mut Shell,
    surface: WindowSurface,
    requested_size: Option<Size<i32, Logical>>,
    decoration_mode: DecorationMode,
    app_id: String,
    title: String,
) -> u64 {
    let id = shell.alloc_window_id();

    let size = requested_size.unwrap_or_else(|| {
        Size::from((DEFAULT_WIDTH, DEFAULT_HEIGHT))
    });

    // Choose monitor: prefer the primary; fall back to first available.
    let monitor = shell
        .primary_monitor()
        .cloned()
        .or_else(|| shell.monitors.first().cloned());

    let loc = monitor
        .as_ref()
        .map(|m| cascade_position(shell, m, size))
        .unwrap_or_else(|| Point::from((0, 0)));

    let geometry = Rectangle::new(loc, size);
    let win = MappedWindow::new(id, surface, geometry, decoration_mode, app_id, title);

    shell.windows.push(win);

    // Register on the active workspace.
    if let Some(ws) = shell.workspaces.get_mut(shell.workspace_index) {
        ws.window_ids.push(id);
    }

    shell.focus_window(id);
    tracing::debug!(id, ?geometry, "placed new window via cascade");
    id
}

/// Apply a move delta to the window's committed geometry and animation target.
///
/// The window is not clamped — that is intentional so that grabs can drag
/// partially off-screen and snapping can detect edge proximity.
pub fn apply_move_delta(shell: &mut Shell, id: u64, delta: Point<i32, Logical>) {
    if let Some(win) = shell.window_mut(id) {
        let new_loc = Point::from((
            win.geometry.loc.x + delta.x,
            win.geometry.loc.y + delta.y,
        ));
        win.geometry = Rectangle::new(new_loc, win.geometry.size);
        // Keep animation in sync so there's no snap-back on the next frame.
        win.animation.set_geometry_instant(win.geometry);
    }
}

/// Raise a window to the top of the paint order (and focus it).
pub fn raise_window(shell: &mut Shell, id: u64) {
    if let Some(pos) = shell.windows.iter().position(|w| w.id == id) {
        let win = shell.windows.remove(pos);
        shell.windows.push(win);
    }
    shell.focus_window(id);
}

/// Clamp a window's position so that at least `margin` pixels of its title bar
/// remain visible within the given monitor's work area.
pub fn clamp_to_work_area(
    win_rect: Rectangle<i32, Logical>,
    work_area: Rectangle<i32, Logical>,
    margin: i32,
) -> Rectangle<i32, Logical> {
    let min_x = work_area.loc.x - win_rect.size.w + margin;
    let max_x = work_area.loc.x + work_area.size.w - margin;
    let min_y = work_area.loc.y; // top edge: can't go above panel
    let max_y = work_area.loc.y + work_area.size.h - margin;

    let x = win_rect.loc.x.clamp(min_x, max_x);
    let y = win_rect.loc.y.clamp(min_y, max_y);
    Rectangle::new(Point::from((x, y)), win_rect.size)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::Shell;

    fn make_monitor() -> MonitorInfo {
        MonitorInfo {
            name: "test-0".to_string(),
            logical_rect: Rectangle::new(
                Point::from((0, 0)),
                Size::from((1920, 1080)),
            ),
            panel_height: 30,
            dock_height: 70,
        }
    }

    #[test]
    fn cascade_empty_shell_places_at_work_area_origin() {
        let shell = Shell::new();
        let monitor = make_monitor();
        let size = Size::from((800, 600));
        let pos = cascade_position(&shell, &monitor, size);
        let wa = monitor.work_area();
        assert_eq!(pos, wa.loc);
    }

    #[test]
    fn cascade_wraps_when_offset_exceeds_work_area() {
        let mut shell = Shell::new();
        shell.monitors.push(make_monitor());

        // Fill up the cascade with many windows so we'd go off-screen.
        for i in 0..100 {
            let id = shell.alloc_window_id();
            let surface = WindowSurface { token: id };
            let rect = Rectangle::new(
                Point::from((i * CASCADE_STEP, i * CASCADE_STEP)),
                Size::from((800, 600)),
            );
            shell.windows.push(MappedWindow::new(
                id,
                surface,
                rect,
                DecorationMode::ServerSide,
                "app".into(),
                "T".into(),
            ));
        }

        let monitor = make_monitor();
        let size = Size::from((800, 600));
        let pos = cascade_position(&shell, &monitor, size);
        // Must still be within the monitor's logical rect.
        assert!(pos.x < monitor.logical_rect.loc.x + monitor.logical_rect.size.w);
        assert!(pos.y < monitor.logical_rect.loc.y + monitor.logical_rect.size.h);
    }

    #[test]
    fn place_new_window_returns_id_and_adds_to_workspace() {
        let mut shell = Shell::new();
        shell.monitors.push(make_monitor());

        let surface = WindowSurface { token: 0 };
        let id = place_new_window(
            &mut shell,
            surface,
            None,
            DecorationMode::ServerSide,
            "org.test.App".into(),
            "Test Window".into(),
        );

        assert!(shell.window(id).is_some());
        assert!(shell.workspaces[0].window_ids.contains(&id));
        assert_eq!(shell.focused_window_id(), Some(id));
    }

    #[test]
    fn apply_move_delta_updates_geometry() {
        let mut shell = Shell::new();
        shell.monitors.push(make_monitor());

        let surface = WindowSurface { token: 0 };
        let id = place_new_window(
            &mut shell,
            surface,
            Some(Size::from((400, 300))),
            DecorationMode::ServerSide,
            "app".into(),
            "T".into(),
        );

        let before = shell.window(id).unwrap().geometry.loc;
        apply_move_delta(&mut shell, id, Point::from((50, 20)));
        let after = shell.window(id).unwrap().geometry.loc;

        assert_eq!(after.x, before.x + 50);
        assert_eq!(after.y, before.y + 20);
    }

    #[test]
    fn clamp_to_work_area_keeps_title_bar_visible() {
        let work_area = Rectangle::new(
            Point::from((0, 30)),
            Size::from((1920, 980)),
        );
        let win = Rectangle::new(
            Point::from((-900, 50)),
            Size::from((800, 600)),
        );
        let clamped = clamp_to_work_area(win, work_area, 100);
        // At least 100px of title bar must be within work area horizontally.
        assert!(clamped.loc.x + 100 <= work_area.loc.x + work_area.size.w);
        assert!(clamped.loc.x + clamped.size.w >= work_area.loc.x + 100);
    }
}
