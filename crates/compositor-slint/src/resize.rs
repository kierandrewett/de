//! Window resize and title-bar-drag-to-move state machine.
//!
//! Tracks active pointer grabs for window resizing (edge / corner) and
//! window moving (title-bar drag).  The `CompositorApp::handle_drag_motion`
//! and `handle_drag_start` / `handle_drag_end` methods in `renderer.rs` call
//! into this module each pointer event.
//!
//! ## Resize strategy
//! On button-press in an edge/corner zone we record:
//!   - which window is being resized (by index into `state.toplevels`)
//!   - which edge/corner (as `ResizeEdge`)
//!   - the pointer position at drag-start
//!   - the window geometry at drag-start
//!
//! On each pointer-motion event:
//!   - compute delta from drag-start pointer position
//!   - derive new (x, y, w, h) from the start geometry + delta
//!   - clamp w/h to `[MIN_WINDOW_SIZE, MAX_WINDOW_SIZE]`
//!   - push an xdg_toplevel.configure with the new content size
//!   - update `ToplevelInfo.{x,y}` for the compositor-space position
//!
//! ## Move strategy
//! On button-press in the title-bar zone we record the drag-start offset from
//! the window's top-left corner.  On each motion event we update `ToplevelInfo.{x,y}`.

use crate::cursor::HitZone;

/// Minimum window content dimension in logical pixels.
pub const MIN_WINDOW_SIZE: i32 = 120;
/// Maximum window content dimension in logical pixels (sanity cap).
pub const MAX_WINDOW_SIZE: i32 = 8192;

/// Which edge(s) are being resized.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ResizeEdge {
    North,
    South,
    East,
    West,
    NorthWest,
    NorthEast,
    SouthWest,
    SouthEast,
}

impl ResizeEdge {
    /// Map a `HitZone` to the `ResizeEdge` (if applicable).
    pub fn from_zone(zone: HitZone) -> Option<Self> {
        match zone {
            HitZone::EdgeNorth             => Some(ResizeEdge::North),
            HitZone::EdgeSouth             => Some(ResizeEdge::South),
            HitZone::EdgeEast              => Some(ResizeEdge::East),
            HitZone::EdgeWest              => Some(ResizeEdge::West),
            HitZone::CornerNW { .. }       => Some(ResizeEdge::NorthWest),
            HitZone::CornerNE { .. }       => Some(ResizeEdge::NorthEast),
            HitZone::CornerSW { .. }       => Some(ResizeEdge::SouthWest),
            HitZone::CornerSE { .. }       => Some(ResizeEdge::SouthEast),
            _ => None,
        }
    }
}

/// Geometry snapshot taken at drag-start.
#[derive(Debug, Clone, Copy)]
pub struct WindowGeomSnapshot {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

/// The active drag operation (if any).
#[derive(Debug, Clone)]
pub enum ActiveDrag {
    /// Resizing a window edge/corner.
    Resize {
        /// Index into `SpikeState::toplevels`.
        toplevel_idx: usize,
        edge: ResizeEdge,
        start_ptr_x: f64,
        start_ptr_y: f64,
        start_geom: WindowGeomSnapshot,
    },
    /// Moving a window via title-bar drag.
    Move {
        toplevel_idx: usize,
        /// Pointer offset from window's top-left corner at drag-start.
        offset_x: f64,
        offset_y: f64,
    },
}

/// Compute the new window geometry after a resize drag.
///
/// Returns `(new_x, new_y, new_content_w, new_content_h)`.
pub fn compute_resize(
    drag: &ActiveDrag,
    ptr_x: f64,
    ptr_y: f64,
) -> Option<(i32, i32, i32, i32)> {
    if let ActiveDrag::Resize { edge, start_ptr_x, start_ptr_y, start_geom, .. } = drag {
        let dx = (ptr_x - start_ptr_x) as i32;
        let dy = (ptr_y - start_ptr_y) as i32;

        let (mut new_x, mut new_y) = (start_geom.x, start_geom.y);
        let mut new_w = start_geom.w;
        let mut new_h = start_geom.h;

        match edge {
            ResizeEdge::East  => { new_w = start_geom.w + dx; }
            ResizeEdge::West  => { new_x = start_geom.x + dx; new_w = start_geom.w - dx; }
            ResizeEdge::South => { new_h = start_geom.h + dy; }
            ResizeEdge::North => { new_y = start_geom.y + dy; new_h = start_geom.h - dy; }
            ResizeEdge::SouthEast => { new_w = start_geom.w + dx; new_h = start_geom.h + dy; }
            ResizeEdge::SouthWest => {
                new_x = start_geom.x + dx; new_w = start_geom.w - dx; new_h = start_geom.h + dy;
            }
            ResizeEdge::NorthEast => {
                new_y = start_geom.y + dy; new_w = start_geom.w + dx; new_h = start_geom.h - dy;
            }
            ResizeEdge::NorthWest => {
                new_x = start_geom.x + dx; new_y = start_geom.y + dy;
                new_w = start_geom.w - dx; new_h = start_geom.h - dy;
            }
        }

        // Clamp dimensions.
        if new_w < MIN_WINDOW_SIZE {
            // Prevent x from flying off on west-side resize.
            if matches!(edge, ResizeEdge::West | ResizeEdge::NorthWest | ResizeEdge::SouthWest) {
                new_x = start_geom.x + start_geom.w - MIN_WINDOW_SIZE;
            }
            new_w = MIN_WINDOW_SIZE;
        }
        if new_h < MIN_WINDOW_SIZE {
            if matches!(edge, ResizeEdge::North | ResizeEdge::NorthWest | ResizeEdge::NorthEast) {
                new_y = start_geom.y + start_geom.h - MIN_WINDOW_SIZE;
            }
            new_h = MIN_WINDOW_SIZE;
        }
        new_w = new_w.min(MAX_WINDOW_SIZE);
        new_h = new_h.min(MAX_WINDOW_SIZE);

        Some((new_x, new_y, new_w, new_h))
    } else {
        None
    }
}

/// Compute the new window position after a move drag.
///
/// Returns `(new_x, new_y)`.
pub fn compute_move(drag: &ActiveDrag, ptr_x: f64, ptr_y: f64) -> Option<(i32, i32)> {
    if let ActiveDrag::Move { offset_x, offset_y, .. } = drag {
        Some(((ptr_x - offset_x) as i32, (ptr_y - offset_y) as i32))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_resize_drag(edge: ResizeEdge) -> ActiveDrag {
        ActiveDrag::Resize {
            toplevel_idx: 0,
            edge,
            start_ptr_x: 100.0,
            start_ptr_y: 100.0,
            start_geom: WindowGeomSnapshot { x: 50, y: 50, w: 400, h: 300 },
        }
    }

    #[test]
    fn test_resize_east() {
        let drag = make_resize_drag(ResizeEdge::East);
        let (nx, ny, nw, nh) = compute_resize(&drag, 150.0, 100.0).unwrap();
        assert_eq!(nx, 50); assert_eq!(ny, 50);
        assert_eq!(nw, 450); assert_eq!(nh, 300);
    }

    #[test]
    fn test_resize_west_clamp() {
        let drag = make_resize_drag(ResizeEdge::West);
        // Dragging so far right that w would be negative → clamped to MIN.
        let (nx, _ny, nw, _nh) = compute_resize(&drag, 500.0, 100.0).unwrap();
        assert_eq!(nw, MIN_WINDOW_SIZE);
        assert_eq!(nx, 50 + 400 - MIN_WINDOW_SIZE);
    }

    #[test]
    fn test_resize_se_corner() {
        let drag = make_resize_drag(ResizeEdge::SouthEast);
        let (nx, ny, nw, nh) = compute_resize(&drag, 120.0, 130.0).unwrap();
        assert_eq!(nx, 50); assert_eq!(ny, 50);
        assert_eq!(nw, 420); assert_eq!(nh, 330);
    }

    #[test]
    fn test_move() {
        let drag = ActiveDrag::Move { toplevel_idx: 0, offset_x: 10.0, offset_y: 15.0 };
        let (nx, ny) = compute_move(&drag, 210.0, 215.0).unwrap();
        assert_eq!(nx, 200); assert_eq!(ny, 200);
    }
}
