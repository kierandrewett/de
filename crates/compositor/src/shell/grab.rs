//! Interactive window move and resize grabs.
#![allow(dead_code)]
//!
//! When the user begins dragging a window title bar or resize handle, the
//! compositor creates an [`ActiveGrab`] and stores it in [`GrabState`].  Every
//! pointer motion event calls [`update`]; pointer button release calls
//! [`finish`].
//!
//! During a move grab, [`super::snapping::SnapDetector`] is consulted on each
//! update.  If a snap target is detected the window geometry is *not* moved to
//! the snap rect immediately — instead [`ActiveGrab::pending_snap`] records it
//! so the render loop can draw the ghost preview.  On drop the window is
//! animated into the snap rect.

use smithay::utils::{Logical, Point, Rectangle, Size};

use super::{
    floating::clamp_to_work_area,
    snapping::{snap_rect_for_target, SnapDetector, SnapTarget},
    MonitorInfo, Shell,
};

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Which edge(s) a resize grab is anchored to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResizeEdge {
    /// Top edge.
    North,
    /// Bottom edge.
    South,
    /// Left edge.
    West,
    /// Right edge.
    East,
    /// Top-left corner.
    NorthWest,
    /// Top-right corner.
    NorthEast,
    /// Bottom-left corner.
    SouthWest,
    /// Bottom-right corner.
    SouthEast,
}

/// What the grab is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrabKind {
    /// Moving the whole window.
    Move,
    /// Resizing from the given edge/corner.
    Resize(ResizeEdge),
}

/// Minimum allowed window dimensions.
const MIN_WIDTH: i32 = 120;
const MIN_HEIGHT: i32 = 80;

/// An in-progress pointer grab on a window.
#[derive(Debug)]
pub struct ActiveGrab {
    /// The window being manipulated.
    pub window_id: u64,
    /// Move or resize.
    pub kind: GrabKind,
    /// Pointer position when the grab started (compositor space).
    pub start_pos: Point<i32, Logical>,
    /// Window geometry when the grab started.
    pub start_geometry: Rectangle<i32, Logical>,
    /// Snap target detected at the current pointer position (ghost preview).
    pub pending_snap: Option<SnapTarget>,
    /// Whether the modifier key for zone-snap is held.
    pub zone_modifier_held: bool,
}

/// Shell-level grab state: at most one grab active at a time.
#[derive(Debug, Default)]
pub struct GrabState {
    /// The active grab, if any.
    pub active: Option<ActiveGrab>,
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Begin a move grab for `window_id` at pointer position `start_pos`.
pub fn begin_move(state: &mut GrabState, shell: &Shell, window_id: u64, start_pos: Point<i32, Logical>) {
    let geometry = match shell.window(window_id) {
        Some(w) => w.geometry,
        None => {
            tracing::warn!(window_id, "begin_move: window not found");
            return;
        }
    };
    state.active = Some(ActiveGrab {
        window_id,
        kind: GrabKind::Move,
        start_pos,
        start_geometry: geometry,
        pending_snap: None,
        zone_modifier_held: false,
    });
    tracing::debug!(window_id, "move grab started");
}

/// Begin a resize grab for `window_id` at `start_pos`, anchored to `edge`.
pub fn begin_resize(
    state: &mut GrabState,
    shell: &Shell,
    window_id: u64,
    start_pos: Point<i32, Logical>,
    edge: ResizeEdge,
) {
    let geometry = match shell.window(window_id) {
        Some(w) => w.geometry,
        None => {
            tracing::warn!(window_id, "begin_resize: window not found");
            return;
        }
    };
    state.active = Some(ActiveGrab {
        window_id,
        kind: GrabKind::Resize(edge),
        start_pos,
        start_geometry: geometry,
        pending_snap: None,
        zone_modifier_held: false,
    });
    tracing::debug!(window_id, ?edge, "resize grab started");
}

/// Process a pointer motion during an active grab.
///
/// `new_pos` is the current pointer position.  `monitor` is the monitor the
/// pointer is on (used for work-area clamping and snap detection).
/// `snap_detector` and `snap_layouts` are used for move grabs.
pub fn update(
    state: &mut GrabState,
    shell: &mut Shell,
    new_pos: Point<i32, Logical>,
    monitor: &MonitorInfo,
    snap_detector: &SnapDetector,
) {
    let grab = match &mut state.active {
        Some(g) => g,
        None => return,
    };

    let id = grab.window_id;
    let delta = Point::from((
        new_pos.x - grab.start_pos.x,
        new_pos.y - grab.start_pos.y,
    ));

    match grab.kind {
        GrabKind::Move => {
            // Unmaximize if moving a maximized window.
            if shell.window(id).map(|w| w.is_maximized).unwrap_or(false) {
                super::maximize::unmaximize_window(shell, id);
                // Rebase the grab: new start_geometry from restored rect.
                if let Some(w) = shell.window(id) {
                    grab.start_geometry = w.geometry;
                    grab.start_pos = new_pos;
                    return; // consume this frame; next frame will move normally
                }
            }

            let new_loc = Point::from((
                grab.start_geometry.loc.x + delta.x,
                grab.start_geometry.loc.y + delta.y,
            ));
            let new_rect =
                Rectangle::new(new_loc, grab.start_geometry.size);

            if let Some(win) = shell.window_mut(id) {
                win.geometry = new_rect;
                win.animation.set_geometry_instant(new_rect);
            }

            // Snap detection — only if zone modifier is NOT held.
            let layouts = shell
                .snap_layouts
                .get(&monitor.name)
                .map(|v| v.as_slice())
                .unwrap_or(&[]);
            let snap = if grab.zone_modifier_held {
                snap_detector.detect_edges_only(new_pos, monitor)
            } else {
                snap_detector.detect(new_pos, monitor, layouts)
            };
            grab.pending_snap = snap;
        }

        GrabKind::Resize(edge) => {
            let new_geo = apply_resize(grab.start_geometry, delta, edge);
            if let Some(win) = shell.window_mut(id) {
                win.geometry = new_geo;
                win.animation.set_geometry_instant(new_geo);
            }
            grab.pending_snap = None;
        }
    }
}

/// Finish the grab (pointer button released).
///
/// If a snap target is pending, the window is animated into it.
/// Otherwise the window is clamped to keep its title bar visible.
///
/// Returns the snap target that was applied, if any.
pub fn finish(
    state: &mut GrabState,
    shell: &mut Shell,
    monitor: &MonitorInfo,
) -> Option<SnapTarget> {
    let grab = state.active.take()?;
    let id = grab.window_id;

    if let GrabKind::Resize(_) = grab.kind {
        // Resize grabs don't snap.
        return None;
    }

    let work_area = monitor.work_area();
    let layouts = shell
        .snap_layouts
        .get(&monitor.name)
        .map(|v| v.as_slice())
        .unwrap_or(&[]);

    if let Some(ref target) = grab.pending_snap {
        let snap_rect = snap_rect_for_target(target, work_area, layouts);

        if *target == SnapTarget::Maximize {
            super::maximize::snap_maximize(shell, id, snap_rect);
        } else {
            if let Some(win) = shell.window_mut(id) {
                win.snap_state = Some(target.clone());
                win.geometry = snap_rect;
                win.animation.set_geometry_target(snap_rect);
            }
        }
        tracing::debug!(id, ?target, "window snapped on drop");
        Some(target.clone())
    } else {
        // No snap: clamp so title bar stays on-screen.
        if let Some(win) = shell.window_mut(id) {
            let clamped = clamp_to_work_area(win.geometry, work_area, 100);
            win.geometry = clamped;
            win.animation.set_geometry_instant(clamped);
        }
        None
    }
}

// ---------------------------------------------------------------------------
// Resize geometry helpers
// ---------------------------------------------------------------------------

fn apply_resize(
    start: Rectangle<i32, Logical>,
    delta: Point<i32, Logical>,
    edge: ResizeEdge,
) -> Rectangle<i32, Logical> {
    let (mut x, mut y) = (start.loc.x, start.loc.y);
    let (mut w, mut h) = (start.size.w, start.size.h);

    match edge {
        ResizeEdge::East | ResizeEdge::NorthEast | ResizeEdge::SouthEast => {
            w = (w + delta.x).max(MIN_WIDTH);
        }
        ResizeEdge::West | ResizeEdge::NorthWest | ResizeEdge::SouthWest => {
            let new_w = (w - delta.x).max(MIN_WIDTH);
            x = x + w - new_w;
            w = new_w;
        }
        _ => {}
    }

    match edge {
        ResizeEdge::South | ResizeEdge::SouthEast | ResizeEdge::SouthWest => {
            h = (h + delta.y).max(MIN_HEIGHT);
        }
        ResizeEdge::North | ResizeEdge::NorthEast | ResizeEdge::NorthWest => {
            let new_h = (h - delta.y).max(MIN_HEIGHT);
            y = y + h - new_h;
            h = new_h;
        }
        _ => {}
    }

    Rectangle::new(Point::from((x, y)), Size::from((w, h)))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use smithay::utils::{Point, Rectangle, Size};

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
            Point::from((200, 200)),
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
        id
    }

    #[test]
    fn move_grab_updates_window_geometry() {
        let mut shell = Shell::new();
        shell.monitors.push(make_monitor());
        let id = add_window(&mut shell);

        let mut grab_state = GrabState::default();
        let detector = SnapDetector::new();
        let monitor = make_monitor();

        begin_move(&mut grab_state, &shell, id, Point::from((300, 300)));
        update(&mut grab_state, &mut shell, Point::from((350, 320)), &monitor, &detector);

        let win = shell.window(id).unwrap();
        assert_eq!(win.geometry.loc.x, 200 + 50);
        assert_eq!(win.geometry.loc.y, 200 + 20);
    }

    #[test]
    fn resize_east_increases_width() {
        let mut shell = Shell::new();
        shell.monitors.push(make_monitor());
        let id = add_window(&mut shell);

        let mut grab_state = GrabState::default();
        let detector = SnapDetector::new();
        let monitor = make_monitor();

        begin_resize(&mut grab_state, &shell, id, Point::from((1000, 500)), ResizeEdge::East);
        update(&mut grab_state, &mut shell, Point::from((1100, 500)), &monitor, &detector);

        let win = shell.window(id).unwrap();
        assert_eq!(win.geometry.size.w, 800 + 100);
        assert_eq!(win.geometry.size.h, 600); // height unchanged
    }

    #[test]
    fn resize_west_moves_origin_and_changes_width() {
        let start = Rectangle::new(
            Point::from((200, 200)),
            Size::from((800, 600)),
        );
        let delta = Point::from((-50, 0));
        let result = apply_resize(start, delta, ResizeEdge::West);
        assert_eq!(result.size.w, 850);
        assert_eq!(result.loc.x, 150);
    }

    #[test]
    fn resize_clamps_to_minimum_size() {
        let start = Rectangle::new(
            Point::from((200, 200)),
            Size::from((130, 90)),
        );
        // Try to shrink past minimum.
        let delta = Point::from((100, 50));
        let result = apply_resize(start, delta, ResizeEdge::West);
        assert!(result.size.w >= MIN_WIDTH);

        let result2 = apply_resize(start, Point::from((0, 40)), ResizeEdge::North);
        assert!(result2.size.h >= MIN_HEIGHT);
    }

    #[test]
    fn finish_snaps_when_pending_snap_is_set() {
        let mut shell = Shell::new();
        shell.monitors.push(make_monitor());
        let id = add_window(&mut shell);

        let mut grab_state = GrabState::default();
        let detector = SnapDetector::new();
        let monitor = make_monitor();

        begin_move(&mut grab_state, &shell, id, Point::from((300, 300)));
        // Drag to the left edge to trigger snap.
        update(&mut grab_state, &mut shell, Point::from((5, 540)), &monitor, &detector);

        let snap = finish(&mut grab_state, &mut shell, &monitor);
        assert_eq!(snap, Some(SnapTarget::LeftHalf));
        let win = shell.window(id).unwrap();
        assert_eq!(win.geometry.loc.x, monitor.work_area().loc.x);
    }

    #[test]
    fn finish_with_no_snap_clamps_geometry() {
        let mut shell = Shell::new();
        shell.monitors.push(make_monitor());
        let id = add_window(&mut shell);

        let mut grab_state = GrabState::default();
        let detector = SnapDetector::new();
        let monitor = make_monitor();

        begin_move(&mut grab_state, &shell, id, Point::from((300, 300)));
        // Drag to a safe interior area — no snap.
        update(&mut grab_state, &mut shell, Point::from((500, 400)), &monitor, &detector);

        let snap = finish(&mut grab_state, &mut shell, &monitor);
        assert!(snap.is_none());
        // Window should still be within bounds.
        let win = shell.window(id).unwrap();
        let wa = monitor.work_area();
        assert!(win.geometry.loc.y >= wa.loc.y);
    }
}
