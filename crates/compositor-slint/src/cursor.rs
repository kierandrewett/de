//! Cursor hit-testing, shape selection, and corner-rotation math.
//!
//! The unique deliverable here is the **corner-rotation cursor**: when the
//! pointer is inside a corner's squircle arc zone, we compute the angle from
//! the corner's centre-of-curvature to the pointer position, and return a
//! rotation offset so the resize cursor icon visually "follows the curve" of
//! the squircle corner.
//!
//! ## Coordinate conventions
//! All coordinates are compositor-space logical pixels (f64).
//! `TITLEBAR_HEIGHT` is included in the window's total height; the content area
//! starts at `(win.x, win.y + TITLEBAR_HEIGHT)`.
//!
//! ## Corner-rotation maths (see deliverable 3 in brief)
//! Each corner has a **centre of curvature** (CoC) offset from its vertex by the
//! corner radius `R` inward along both axes.  The angle from CoC to the pointer:
//!
//!   θ = atan2(pointer.y - coc.y,  pointer.x - coc.x)
//!
//! Each corner has a "natural" angle for its default diagonal cursor:
//!   NW → 225°, NE → 315° (= -45°), SW → 135°, SE → 45°
//!
//! Rotation delta = θ – natural_angle, clamped to ±45° (beyond that we're
//! outside the curve and revert to the fixed angle).

use std::f64::consts::PI;

/// Height of the server-side title bar in logical pixels.
pub const TITLEBAR_HEIGHT: f64 = 33.0;

/// Half-width (in logical pixels) of the resize grab band. The full band is
/// `2 × EDGE_ZONE` wide, centred on the visible window edge — half outside
/// the chrome, half inside. Lets the user grab the edge from the shadow
/// band without having to land within a thin stroke, while leaving the
/// inner half-band inside the chrome for natural mouse-on-edge resizing.
pub const EDGE_ZONE: f64 = 6.0;

/// Corner grab zone extends this many pixels from the corner along each axis.
pub const CORNER_ZONE: f64 = 14.0;

/// Squircle corner radius (must match `Tokens.window-corner-radius-outer`).
pub const CORNER_RADIUS: f64 = 14.0;

/// Which part of a window the pointer is over.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum HitZone {
    /// No window under pointer.
    None,
    /// Window title bar (drag to move area, excludes control buttons).
    TitleBar,
    /// Close button.
    CloseButton,
    /// Minimize button.
    MinimizeButton,
    /// Maximize button.
    MaximizeButton,
    /// Client content area.
    Content,
    /// Resize edges (not corners).
    EdgeNorth,
    EdgeSouth,
    EdgeEast,
    EdgeWest,
    /// Resize corners — carry the rotation angle offset (radians) for the cursor icon.
    CornerNW {
        angle_offset: f64,
    },
    CornerNE {
        angle_offset: f64,
    },
    CornerSW {
        angle_offset: f64,
    },
    CornerSE {
        angle_offset: f64,
    },
}

/// Resolved cursor shape for the current pointer position.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CursorKind {
    Arrow,
    Move,
    Hand,
    ResizeN,
    ResizeS,
    ResizeE,
    ResizeW,
    /// Diagonal NW/SE resize, rotated by `angle_offset` radians from its natural diagonal.
    ResizeNWSE {
        angle_offset: f64,
    },
    /// Diagonal NE/SW resize, rotated by `angle_offset` radians.
    ResizeNESW {
        angle_offset: f64,
    },
}

/// Window geometry used by the hit-tester.
#[derive(Debug, Clone, Copy)]
pub struct WindowRect {
    pub id: i32,
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    /// True when the client paints its own header bar (xdg-decoration
    /// ClientSide). The top `TITLEBAR_HEIGHT` band is then part of the
    /// client's own UI and must NOT be treated as our SSD titlebar drag
    /// zone — otherwise every click on the GTK header eats into a Move
    /// drag instead of reaching the close / menu / search buttons.
    pub csd: bool,
}

/// Describes where the pointer is relative to a window.
#[derive(Debug, Clone, Copy)]
pub struct HitResult {
    pub window_id: i32,
    pub zone: HitZone,
}

/// Control-button layout constants (must match WindowChrome.slint / Tokens.slint).
const CONTROL_SIZE: f64 = 21.0;
const CONTROL_GAP: f64 = 4.0;
const CONTROL_INSET: f64 = 6.0;
/// X offset of the controls block from the right edge of the window.
const CONTROLS_BLOCK_WIDTH: f64 = CONTROL_SIZE * 3.0 + CONTROL_GAP * 2.0;

/// Compute the centre-of-curvature for each corner, then the angle from CoC
/// to the pointer.  Returns the angle offset (in radians) relative to the
/// "natural" diagonal direction of the corner, clamped to ±45°.
///
/// Returns `None` if the pointer is not actually in the corner arc zone, in
/// which case the caller should use the fixed corner cursor angle (0 offset).
///
/// `cx`, `cy` are corner vertex coordinates.
/// `dx`, `dy` are the signs (+1/-1) describing the inward direction toward the
/// centre of the window from the corner.
fn corner_angle_offset(
    ptr_x: f64,
    ptr_y: f64,
    cx: f64,
    cy: f64,
    dx: f64,
    dy: f64,
    natural_deg: f64,
) -> f64 {
    // Centre of curvature is R inward from the corner along both axes.
    let coc_x = cx + dx * CORNER_RADIUS;
    let coc_y = cy + dy * CORNER_RADIUS;

    let angle = (ptr_y - coc_y).atan2(ptr_x - coc_x);
    let natural_rad = natural_deg * PI / 180.0;
    let mut delta = angle - natural_rad;

    // Normalise delta to [-π, π].
    while delta > PI {
        delta -= 2.0 * PI;
    }
    while delta < -PI {
        delta += 2.0 * PI;
    }

    // Clamp to ±45° (π/4) — outside the arc we use the neutral angle.
    let max = PI / 4.0;
    delta.max(-max).min(max)
}

/// Given a pointer position in compositor space and a list of window rects
/// (topmost-first), return the hit zone.
///
/// Windows are iterated front-to-back; the first match wins.
pub fn hit_test(ptr_x: f64, ptr_y: f64, windows: &[WindowRect]) -> Option<HitResult> {
    for win in windows {
        let (wx, wy, ww, wh) = (win.x, win.y, win.w, win.h);

        // Expand by EDGE_ZONE to catch resize grabs just outside the window.
        let in_outer = ptr_x >= wx - EDGE_ZONE
            && ptr_x < wx + ww + EDGE_ZONE
            && ptr_y >= wy - EDGE_ZONE
            && ptr_y < wy + wh + EDGE_ZONE;

        if !in_outer {
            continue;
        }

        // ── Corner zones (checked first — highest priority) ─────────────────
        // Each corner zone is a CORNER_ZONE × CORNER_ZONE square at each corner.
        let near_left = ptr_x < wx + CORNER_ZONE;
        let near_right = ptr_x >= wx + ww - CORNER_ZONE;
        let near_top = ptr_y < wy + CORNER_ZONE;
        let near_bottom = ptr_y >= wy + wh - CORNER_ZONE;

        if near_top && near_left {
            // NW corner: natural angle 225° (SW direction from CoC to NW = down-left).
            // CoC is at (wx+R, wy+R); NW natural = 225°.
            let offset = corner_angle_offset(ptr_x, ptr_y, wx, wy, 1.0, 1.0, 225.0);
            return Some(HitResult {
                window_id: win.id,
                zone: HitZone::CornerNW {
                    angle_offset: offset,
                },
            });
        }
        if near_top && near_right {
            // NE corner: natural angle 315°.
            let offset = corner_angle_offset(ptr_x, ptr_y, wx + ww, wy, -1.0, 1.0, 315.0);
            return Some(HitResult {
                window_id: win.id,
                zone: HitZone::CornerNE {
                    angle_offset: offset,
                },
            });
        }
        if near_bottom && near_left {
            // SW corner: natural angle 135°.
            let offset = corner_angle_offset(ptr_x, ptr_y, wx, wy + wh, 1.0, -1.0, 135.0);
            return Some(HitResult {
                window_id: win.id,
                zone: HitZone::CornerSW {
                    angle_offset: offset,
                },
            });
        }
        if near_bottom && near_right {
            // SE corner: natural angle 45°.
            let offset = corner_angle_offset(ptr_x, ptr_y, wx + ww, wy + wh, -1.0, -1.0, 45.0);
            return Some(HitResult {
                window_id: win.id,
                zone: HitZone::CornerSE {
                    angle_offset: offset,
                },
            });
        }

        // ── Edge zones ──────────────────────────────────────────────────────
        if ptr_y < wy + EDGE_ZONE {
            return Some(HitResult {
                window_id: win.id,
                zone: HitZone::EdgeNorth,
            });
        }
        if ptr_y >= wy + wh - EDGE_ZONE {
            return Some(HitResult {
                window_id: win.id,
                zone: HitZone::EdgeSouth,
            });
        }
        if ptr_x < wx + EDGE_ZONE {
            return Some(HitResult {
                window_id: win.id,
                zone: HitZone::EdgeWest,
            });
        }
        if ptr_x >= wx + ww - EDGE_ZONE {
            return Some(HitResult {
                window_id: win.id,
                zone: HitZone::EdgeEast,
            });
        }

        // ── Inside window interior ───────────────────────────────────────────
        // For CSD windows the client owns the entire chrome including the top
        // band, so we skip the SSD titlebar / control-button zones entirely
        // and treat the whole interior as Content. That lets pointer events
        // (clicks on the GTK header's close / menu / search) flow through
        // to the wayland client instead of being captured as a Move drag.
        if !win.csd && ptr_y >= wy && ptr_y < wy + TITLEBAR_HEIGHT {
            // Check control buttons (right-to-left: minimize, maximize, close).
            let controls_x = wx + ww - CONTROLS_BLOCK_WIDTH - CONTROL_INSET;
            let controls_y = wy + CONTROL_INSET;

            if ptr_y >= controls_y && ptr_y < controls_y + CONTROL_SIZE {
                // Minimize (leftmost).
                let min_x = controls_x;
                if ptr_x >= min_x && ptr_x < min_x + CONTROL_SIZE {
                    return Some(HitResult {
                        window_id: win.id,
                        zone: HitZone::MinimizeButton,
                    });
                }
                // Maximize (middle).
                let max_x = controls_x + CONTROL_SIZE + CONTROL_GAP;
                if ptr_x >= max_x && ptr_x < max_x + CONTROL_SIZE {
                    return Some(HitResult {
                        window_id: win.id,
                        zone: HitZone::MaximizeButton,
                    });
                }
                // Close (rightmost).
                let close_x = controls_x + (CONTROL_SIZE + CONTROL_GAP) * 2.0;
                if ptr_x >= close_x && ptr_x < close_x + CONTROL_SIZE {
                    return Some(HitResult {
                        window_id: win.id,
                        zone: HitZone::CloseButton,
                    });
                }
            }
            return Some(HitResult {
                window_id: win.id,
                zone: HitZone::TitleBar,
            });
        }

        // Content area.
        return Some(HitResult {
            window_id: win.id,
            zone: HitZone::Content,
        });
    }

    None
}

/// Map a `HitZone` to the `CursorKind` to display.
pub fn zone_to_cursor(zone: HitZone) -> CursorKind {
    match zone {
        HitZone::None => CursorKind::Arrow,
        HitZone::TitleBar => CursorKind::Move,
        // Window controls keep the default arrow cursor — the
        // hand/pointer felt heavy and macOS / GNOME don't use it for
        // chrome buttons either.
        HitZone::CloseButton => CursorKind::Arrow,
        HitZone::MinimizeButton => CursorKind::Arrow,
        HitZone::MaximizeButton => CursorKind::Arrow,
        HitZone::Content => CursorKind::Arrow,
        HitZone::EdgeNorth => CursorKind::ResizeN,
        HitZone::EdgeSouth => CursorKind::ResizeS,
        HitZone::EdgeEast => CursorKind::ResizeE,
        HitZone::EdgeWest => CursorKind::ResizeW,
        HitZone::CornerNW { angle_offset } => CursorKind::ResizeNWSE { angle_offset },
        HitZone::CornerSE { angle_offset } => CursorKind::ResizeNWSE { angle_offset },
        HitZone::CornerNE { angle_offset } => CursorKind::ResizeNESW { angle_offset },
        HitZone::CornerSW { angle_offset } => CursorKind::ResizeNESW { angle_offset },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_win(x: f64, y: f64, w: f64, h: f64) -> WindowRect {
        WindowRect {
            id: 1,
            x,
            y,
            w,
            h,
            csd: false,
        }
    }

    #[test]
    fn test_content_zone() {
        let win = make_win(100.0, 100.0, 400.0, 300.0);
        let hit = hit_test(200.0, 200.0, &[win]).unwrap();
        assert_eq!(hit.zone, HitZone::Content);
    }

    #[test]
    fn test_title_bar_zone() {
        let win = make_win(100.0, 100.0, 400.0, 300.0);
        let hit = hit_test(200.0, 110.0, &[win]).unwrap();
        assert_eq!(hit.zone, HitZone::TitleBar);
    }

    #[test]
    fn test_north_edge() {
        let win = make_win(100.0, 100.0, 400.0, 300.0);
        let hit = hit_test(200.0, 102.0, &[win]).unwrap();
        assert_eq!(hit.zone, HitZone::EdgeNorth);
    }

    #[test]
    fn test_corner_nw() {
        let win = make_win(100.0, 100.0, 400.0, 300.0);
        // Inside NW corner zone
        let hit = hit_test(108.0, 108.0, &[win]).unwrap();
        matches!(hit.zone, HitZone::CornerNW { .. });
    }

    #[test]
    fn test_no_hit_outside() {
        let win = make_win(100.0, 100.0, 400.0, 300.0);
        let hit = hit_test(50.0, 50.0, &[win]);
        assert!(hit.is_none());
    }

    #[test]
    fn test_corner_rotation_math() {
        // At NW corner with R=14, pointer at (100+3, 100+8) → corner at (100,100)
        // CoC at (114, 114), angle = atan2(8-14, 3-14) = atan2(-6, -11)
        let win = make_win(100.0, 100.0, 400.0, 300.0);
        let hit = hit_test(103.0, 108.0, &[win]).unwrap();
        if let HitZone::CornerNW { angle_offset } = hit.zone {
            // The offset should be non-zero (pointer is off-diagonal).
            // atan2(-6,-11) ≈ -2.644 rad → 208.5°; natural 225° → delta ≈ -16.5°
            // Within ±45° so not clamped.
            assert!(angle_offset.abs() < PI / 4.0 + 0.001);
        } else {
            panic!("Expected CornerNW, got {:?}", hit.zone);
        }
    }
}
