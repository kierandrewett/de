//! Window snap zones (drag a window near a screen edge → preview a snap target).
//!
//! Detection runs every Move-drag motion event; if the cursor is within the
//! edge band, we return the rect the window will snap to on release. The
//! renderer pushes that rect into Slint as a preview overlay.

use crate::wm::{DOCK_HEIGHT, PANEL_HEIGHT};

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
    output_w: i32,
    output_h: i32,
) -> Option<(SnapZone, SnapRect)> {
    let usable_y = PANEL_HEIGHT;
    let usable_h = (output_h - PANEL_HEIGHT - DOCK_HEIGHT).max(100);
    let usable_w = output_w.max(100);

    let top = cursor_y <= PANEL_HEIGHT as f64 + SNAP_BAND_PX;
    let left = cursor_x <= SNAP_BAND_PX;
    let right = cursor_x >= output_w as f64 - SNAP_BAND_PX;
    let bottom = cursor_y >= output_h as f64 - DOCK_HEIGHT as f64 - SNAP_BAND_PX;

    if top && left {
        return Some((
            SnapZone::TopLeftQuarter,
            SnapRect {
                x: 0,
                y: usable_y,
                w: usable_w / 2,
                h: usable_h / 2,
            },
        ));
    }
    if top && right {
        return Some((
            SnapZone::TopRightQuarter,
            SnapRect {
                x: usable_w / 2,
                y: usable_y,
                w: usable_w / 2,
                h: usable_h / 2,
            },
        ));
    }
    if bottom && left {
        return Some((
            SnapZone::BottomLeftQuarter,
            SnapRect {
                x: 0,
                y: usable_y + usable_h / 2,
                w: usable_w / 2,
                h: usable_h / 2,
            },
        ));
    }
    if bottom && right {
        return Some((
            SnapZone::BottomRightQuarter,
            SnapRect {
                x: usable_w / 2,
                y: usable_y + usable_h / 2,
                w: usable_w / 2,
                h: usable_h / 2,
            },
        ));
    }
    if top {
        return Some((
            SnapZone::Maximize,
            SnapRect {
                x: 0,
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
                x: 0,
                y: usable_y,
                w: usable_w / 2,
                h: usable_h,
            },
        ));
    }
    if right {
        return Some((
            SnapZone::RightHalf,
            SnapRect {
                x: usable_w / 2,
                y: usable_y,
                w: usable_w / 2,
                h: usable_h,
            },
        ));
    }
    None
}
