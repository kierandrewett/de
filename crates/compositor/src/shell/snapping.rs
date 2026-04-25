//! Edge snapping and FancyZones-style zone snapping.
#![allow(dead_code)]
//!
//! During a window drag, [`SnapDetector::detect`] is called on every pointer
//! motion.  When it returns `Some(target)`, the UI shows a ghost preview at
//! [`snap_rect_for_target`].  On pointer release the window is animated into
//! that rect.

use smithay::utils::{Logical, Point, Rectangle, Size};

use ipc::SnapLayout;

use super::MonitorInfo;

// ---------------------------------------------------------------------------
// SnapTarget
// ---------------------------------------------------------------------------

/// Where a dragged window should snap to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SnapTarget {
    /// Left half of the work area.
    LeftHalf,
    /// Right half of the work area.
    RightHalf,
    /// Top half of the work area.
    TopHalf,
    /// Bottom half of the work area.
    BottomHalf,
    /// Top-left quarter.
    TopLeft,
    /// Top-right quarter.
    TopRight,
    /// Bottom-left quarter.
    BottomLeft,
    /// Bottom-right quarter.
    BottomRight,
    /// Full work area (drag to top edge).
    Maximize,
    /// A user-defined FancyZones zone.
    Zone {
        /// Index into the monitor's snap layout list.
        layout: usize,
        /// Index within that layout's zone list.
        zone: usize,
    },
}

// ---------------------------------------------------------------------------
// SnapDetector
// ---------------------------------------------------------------------------

/// Detects snap targets based on cursor proximity to screen edges / corners.
#[derive(Debug, Clone)]
pub struct SnapDetector {
    /// Distance from screen edge that triggers a half-screen snap (pixels).
    pub edge_threshold: i32,
    /// Distance from screen corner that triggers a quarter-screen snap (pixels).
    pub corner_threshold: i32,
}

impl Default for SnapDetector {
    fn default() -> Self {
        Self { edge_threshold: 10, corner_threshold: 20 }
    }
}

impl SnapDetector {
    /// Create with the default thresholds (10 px edge, 20 px corner).
    pub fn new() -> Self {
        Self::default()
    }

    /// Determine the snap target for a cursor at `cursor` on `monitor`.
    ///
    /// `layouts` is the list of user-defined snap layouts for this monitor.
    /// Returns `None` when the cursor is not near any snap zone.
    pub fn detect(
        &self,
        cursor: Point<i32, Logical>,
        monitor: &MonitorInfo,
        _layouts: &[SnapLayout],
    ) -> Option<SnapTarget> {
        let r = monitor.logical_rect;
        let left = r.loc.x;
        let right = r.loc.x + r.size.w;
        let top = r.loc.y;
        let bottom = r.loc.y + r.size.h;

        let near_left = cursor.x <= left + self.edge_threshold;
        let near_right = cursor.x >= right - self.edge_threshold;
        let near_top = cursor.y <= top + self.edge_threshold;
        let near_bottom = cursor.y >= bottom - self.edge_threshold;

        let corner_left = cursor.x <= left + self.corner_threshold;
        let corner_right = cursor.x >= right - self.corner_threshold;
        let corner_top = cursor.y <= top + self.corner_threshold;
        let corner_bottom = cursor.y >= bottom - self.corner_threshold;

        // Corners take precedence over edges (larger threshold wins).
        if corner_left && corner_top {
            return Some(SnapTarget::TopLeft);
        }
        if corner_right && corner_top {
            return Some(SnapTarget::TopRight);
        }
        if corner_left && corner_bottom {
            return Some(SnapTarget::BottomLeft);
        }
        if corner_right && corner_bottom {
            return Some(SnapTarget::BottomRight);
        }

        // Edge snaps.
        if near_top {
            return Some(SnapTarget::Maximize);
        }
        if near_left {
            return Some(SnapTarget::LeftHalf);
        }
        if near_right {
            return Some(SnapTarget::RightHalf);
        }
        if near_bottom {
            return Some(SnapTarget::BottomHalf);
        }

        None
    }

    /// Like `detect` but only triggers on edges, never corners.
    /// Used when the user holds a modifier key to show zone overlay instead.
    pub fn detect_edges_only(
        &self,
        cursor: Point<i32, Logical>,
        monitor: &MonitorInfo,
    ) -> Option<SnapTarget> {
        let r = monitor.logical_rect;
        let near_left = cursor.x <= r.loc.x + self.edge_threshold;
        let near_right = cursor.x >= r.loc.x + r.size.w - self.edge_threshold;
        let near_top = cursor.y <= r.loc.y + self.edge_threshold;
        let near_bottom = cursor.y >= r.loc.y + r.size.h - self.edge_threshold;

        if near_top {
            Some(SnapTarget::Maximize)
        } else if near_left {
            Some(SnapTarget::LeftHalf)
        } else if near_right {
            Some(SnapTarget::RightHalf)
        } else if near_bottom {
            Some(SnapTarget::BottomHalf)
        } else {
            None
        }
    }
}

// ---------------------------------------------------------------------------
// snap_rect_for_target
// ---------------------------------------------------------------------------

/// Compute the work-area rectangle a window should occupy for a given snap target.
///
/// For [`SnapTarget::Zone`] the fractional zone coordinates are resolved
/// against `work_area`.  If the referenced layout or zone index is out of
/// bounds the full work area is returned as a safe default.
pub fn snap_rect_for_target(
    target: &SnapTarget,
    work_area: Rectangle<i32, Logical>,
    layouts: &[SnapLayout],
) -> Rectangle<i32, Logical> {
    let x = work_area.loc.x;
    let y = work_area.loc.y;
    let w = work_area.size.w;
    let h = work_area.size.h;

    match target {
        SnapTarget::LeftHalf => {
            Rectangle::new(Point::from((x, y)), Size::from((w / 2, h)))
        }
        SnapTarget::RightHalf => {
            Rectangle::new(
                Point::from((x + w / 2, y)),
                Size::from((w - w / 2, h)),
            )
        }
        SnapTarget::TopHalf => {
            Rectangle::new(Point::from((x, y)), Size::from((w, h / 2)))
        }
        SnapTarget::BottomHalf => {
            Rectangle::new(
                Point::from((x, y + h / 2)),
                Size::from((w, h - h / 2)),
            )
        }
        SnapTarget::TopLeft => {
            Rectangle::new(Point::from((x, y)), Size::from((w / 2, h / 2)))
        }
        SnapTarget::TopRight => {
            Rectangle::new(
                Point::from((x + w / 2, y)),
                Size::from((w - w / 2, h / 2)),
            )
        }
        SnapTarget::BottomLeft => {
            Rectangle::new(
                Point::from((x, y + h / 2)),
                Size::from((w / 2, h - h / 2)),
            )
        }
        SnapTarget::BottomRight => {
            Rectangle::new(
                Point::from((x + w / 2, y + h / 2)),
                Size::from((w - w / 2, h - h / 2)),
            )
        }
        SnapTarget::Maximize => work_area,
        SnapTarget::Zone { layout, zone } => {
            resolve_zone(*layout, *zone, work_area, layouts)
        }
    }
}

fn resolve_zone(
    layout_idx: usize,
    zone_idx: usize,
    work_area: Rectangle<i32, Logical>,
    layouts: &[SnapLayout],
) -> Rectangle<i32, Logical> {
    let Some(layout) = layouts.get(layout_idx) else {
        return work_area;
    };
    let Some(zone) = layout.zones.get(zone_idx) else {
        return work_area;
    };

    let w = work_area.size.w as f64;
    let h = work_area.size.h as f64;

    let zx = work_area.loc.x + (zone.x * w) as i32;
    let zy = work_area.loc.y + (zone.y * h) as i32;
    let zw = (zone.width * w) as i32;
    let zh = (zone.height * h) as i32;

    Rectangle::new(Point::from((zx, zy)), Size::from((zw.max(1), zh.max(1))))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use ipc::SnapZone;

    fn monitor_1080p() -> MonitorInfo {
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

    fn no_layouts() -> Vec<SnapLayout> {
        Vec::new()
    }

    // --- SnapDetector --------------------------------------------------------

    #[test]
    fn detect_left_edge() {
        let det = SnapDetector::new();
        let m = monitor_1080p();
        let cursor = Point::from((5, 400));
        assert_eq!(det.detect(cursor, &m, &no_layouts()), Some(SnapTarget::LeftHalf));
    }

    #[test]
    fn detect_right_edge() {
        let det = SnapDetector::new();
        let m = monitor_1080p();
        let cursor = Point::from((1915, 400));
        assert_eq!(det.detect(cursor, &m, &no_layouts()), Some(SnapTarget::RightHalf));
    }

    #[test]
    fn detect_top_edge_is_maximize() {
        let det = SnapDetector::new();
        let m = monitor_1080p();
        let cursor = Point::from((960, 5));
        assert_eq!(det.detect(cursor, &m, &no_layouts()), Some(SnapTarget::Maximize));
    }

    #[test]
    fn detect_top_left_corner() {
        let det = SnapDetector::new();
        let m = monitor_1080p();
        let cursor = Point::from((15, 15));
        assert_eq!(det.detect(cursor, &m, &no_layouts()), Some(SnapTarget::TopLeft));
    }

    #[test]
    fn detect_bottom_right_corner() {
        let det = SnapDetector::new();
        let m = monitor_1080p();
        let cursor = Point::from((1905, 1065));
        assert_eq!(det.detect(cursor, &m, &no_layouts()), Some(SnapTarget::BottomRight));
    }

    #[test]
    fn detect_center_returns_none() {
        let det = SnapDetector::new();
        let m = monitor_1080p();
        let cursor = Point::from((960, 540));
        assert_eq!(det.detect(cursor, &m, &no_layouts()), None);
    }

    // --- snap_rect_for_target -----------------------------------------------

    #[test]
    fn left_half_occupies_left_side() {
        let wa = Rectangle::new(
            Point::from((0, 30)),
            Size::from((1920, 980)),
        );
        let r = snap_rect_for_target(&SnapTarget::LeftHalf, wa, &no_layouts());
        assert_eq!(r.loc.x, 0);
        assert_eq!(r.size.w, 960);
        assert_eq!(r.size.h, 980);
    }

    #[test]
    fn right_half_starts_at_midpoint() {
        let wa = Rectangle::new(
            Point::from((0, 30)),
            Size::from((1920, 980)),
        );
        let r = snap_rect_for_target(&SnapTarget::RightHalf, wa, &no_layouts());
        assert_eq!(r.loc.x, 960);
        assert_eq!(r.size.w, 960);
    }

    #[test]
    fn halves_are_complementary() {
        let wa = Rectangle::new(
            Point::from((0, 30)),
            Size::from((1920, 980)),
        );
        let left = snap_rect_for_target(&SnapTarget::LeftHalf, wa, &no_layouts());
        let right = snap_rect_for_target(&SnapTarget::RightHalf, wa, &no_layouts());
        assert_eq!(left.size.w + right.size.w, wa.size.w);
        assert_eq!(left.loc.x + left.size.w, right.loc.x);
    }

    #[test]
    fn zone_snap_resolves_fractional_coordinates() {
        let wa = Rectangle::new(
            Point::from((0, 0)),
            Size::from((1920, 1080)),
        );
        let layouts = vec![SnapLayout {
            name: "thirds".to_string(),
            zones: vec![
                SnapZone { x: 0.0, y: 0.0, width: 0.333, height: 1.0 },
                SnapZone { x: 0.333, y: 0.0, width: 0.334, height: 1.0 },
                SnapZone { x: 0.667, y: 0.0, width: 0.333, height: 1.0 },
            ],
        }];
        let r = snap_rect_for_target(
            &SnapTarget::Zone { layout: 0, zone: 1 },
            wa,
            &layouts,
        );
        assert_eq!(r.loc.x, (0.333 * 1920.0) as i32);
        assert!(r.size.w > 0);
    }

    #[test]
    fn zone_snap_out_of_bounds_returns_work_area() {
        let wa = Rectangle::new(
            Point::from((0, 0)),
            Size::from((1920, 1080)),
        );
        let r = snap_rect_for_target(
            &SnapTarget::Zone { layout: 99, zone: 0 },
            wa,
            &no_layouts(),
        );
        assert_eq!(r, wa);
    }
}
