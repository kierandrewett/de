//! Window snap zones (drag a window near a screen edge → preview a snap target).
//!
//! Detection runs every Move-drag motion event; if the cursor is within the
//! edge band, we return the rect the window will snap to on release. The
//! renderer pushes that rect into Slint as a preview overlay.

use crate::wm::WorkArea;

/// Width of the snap band at each screen edge (logical pixels).
const SNAP_BAND_PX: f64 = 12.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapZone {
    Maximize,
    LeftHalf,
    RightHalf,
    TopLeftQuarter,
    TopRightQuarter,
    BottomLeftQuarter,
    BottomRightQuarter,
}

#[derive(Debug, Clone, Copy)]
pub struct SnapRect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

/// Compute the snap candidate for a given cursor position within an output.
///
/// Returns `None` if the cursor is in the interior (no snap edge active).
pub fn detect(
    cursor_x: f64,
    cursor_y: f64,
    work_area: WorkArea,
) -> Option<(SnapZone, SnapRect)> {
    let usable_x = work_area.x;
    let usable_y = work_area.y;
    let usable_w = work_area.w.max(100);
    let usable_h = work_area.h.max(100);
    let right_edge = work_area.right();
    let bottom_edge = work_area.bottom();
    let left_w = usable_w / 2;
    let right_w = usable_w - left_w;
    let top_h = usable_h / 2;
    let bottom_h = usable_h - top_h;

    let top = cursor_y <= work_area.y as f64 + SNAP_BAND_PX;
    let left = cursor_x <= work_area.x as f64 + SNAP_BAND_PX;
    let right = cursor_x >= right_edge as f64 - SNAP_BAND_PX;
    let bottom = cursor_y >= bottom_edge as f64 - SNAP_BAND_PX;

    if top && left {
        return Some((
            SnapZone::TopLeftQuarter,
            SnapRect {
                x: usable_x,
                y: usable_y,
                w: left_w,
                h: top_h,
            },
        ));
    }
    if top && right {
        return Some((
            SnapZone::TopRightQuarter,
            SnapRect {
                x: usable_x + left_w,
                y: usable_y,
                w: right_w,
                h: top_h,
            },
        ));
    }
    if bottom && left {
        return Some((
            SnapZone::BottomLeftQuarter,
            SnapRect {
                x: usable_x,
                y: usable_y + top_h,
                w: left_w,
                h: bottom_h,
            },
        ));
    }
    if bottom && right {
        return Some((
            SnapZone::BottomRightQuarter,
            SnapRect {
                x: usable_x + left_w,
                y: usable_y + top_h,
                w: right_w,
                h: bottom_h,
            },
        ));
    }
    if top {
        return Some((
            SnapZone::Maximize,
            SnapRect {
                x: usable_x,
                y: usable_y,
                w: usable_w,
                h: usable_h,
            },
        ));
    }
    if left {
        return Some((
            SnapZone::LeftHalf,
            SnapRect {
                x: usable_x,
                y: usable_y,
                w: left_w,
                h: usable_h,
            },
        ));
    }
    if right {
        return Some((
            SnapZone::RightHalf,
            SnapRect {
                x: usable_x + left_w,
                y: usable_y,
                w: right_w,
                h: usable_h,
            },
        ));
    }
    None
}
