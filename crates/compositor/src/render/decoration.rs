//! Server-side decoration (SSD) title bar rendering.
//!
//! The SSD title bar is rendered as an iced widget tree via the COSMIC
//! `IcedElement` pattern (see ARCHITECTURE.md §COSMIC as Reference). The iced
//! tree is rendered to a GPU texture which is composited above the client surface.
//!
//! # Current status
//!
//! The `DecorationRenderer` renders a solid background quad as a placeholder.
//! Full iced integration (title text, window control buttons, font rendering)
//! is deferred to a follow-up pass.
//!
//! # Window controls (WINDOW_SPEC.md)
//!
//! Buttons are positioned on the **right** side (macOS reversed — spec says
//! right side, not left). Layout from right edge:
//! ```text
//! [close] [maximize] [minimize]  ← right to left, 4 px gap, 6 px from edge
//! ```
//! Button size: 21 × 21 px. First button inset: 6 px from right, 6 px from top.

use super::{FrameRef, Rect};

// ─── Geometry constants (WINDOW_SPEC.md) ────────────────────────────────────

/// Width and height of each window control button.
pub const BUTTON_SIZE: f32 = 21.0;
/// Gap between adjacent window control buttons.
pub const BUTTON_GAP: f32 = 4.0;
/// Inset from the right edge of the title bar to the rightmost button.
pub const BUTTON_INSET_RIGHT: f32 = 6.0;
/// Inset from the top edge to the button top.
pub const BUTTON_INSET_TOP: f32 = 6.0;

// ─── Button geometry ─────────────────────────────────────────────────────────

/// Which button was hit by a pointer event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonHit {
    /// Close button (×).
    Close,
    /// Maximize / restore button (⬜).
    Maximize,
    /// Minimize button (—).
    Minimize,
    /// No button was hit (click falls through to title bar drag or content).
    None,
}

/// Geometry of the three window control buttons for a given title bar rect.
///
/// Buttons are laid out right-to-left: close is rightmost, minimize is leftmost.
#[derive(Debug, Clone, Copy)]
pub struct ButtonLayout {
    /// Close button rect (rightmost).
    pub close: Rect,
    /// Maximize button rect (middle).
    pub maximize: Rect,
    /// Minimize button rect (leftmost).
    pub minimize: Rect,
}

impl ButtonLayout {
    /// Compute button positions for a title bar at the given rect.
    ///
    /// `title_bar_rect` is the full title bar bounding box (top of window,
    /// spanning the window width).
    pub fn from_title_bar(title_bar_rect: Rect) -> Self {
        let right = title_bar_rect.x + title_bar_rect.width;
        let top = title_bar_rect.y;

        let close_x = right - BUTTON_INSET_RIGHT - BUTTON_SIZE;
        let maximize_x = close_x - BUTTON_GAP - BUTTON_SIZE;
        let minimize_x = maximize_x - BUTTON_GAP - BUTTON_SIZE;
        let btn_y = top + BUTTON_INSET_TOP;

        Self {
            close: Rect::new(close_x, btn_y, BUTTON_SIZE, BUTTON_SIZE),
            maximize: Rect::new(maximize_x, btn_y, BUTTON_SIZE, BUTTON_SIZE),
            minimize: Rect::new(minimize_x, btn_y, BUTTON_SIZE, BUTTON_SIZE),
        }
    }

    /// Hit-test a pointer position against all three buttons.
    ///
    /// Returns the first button whose rect contains `(px, py)`, or
    /// [`ButtonHit::None`] if the pointer is not over any button.
    pub fn hit_test(&self, px: f32, py: f32) -> ButtonHit {
        if self.close.contains(px, py) {
            ButtonHit::Close
        } else if self.maximize.contains(px, py) {
            ButtonHit::Maximize
        } else if self.minimize.contains(px, py) {
            ButtonHit::Minimize
        } else {
            ButtonHit::None
        }
    }
}

// ─── Title bar colours ───────────────────────────────────────────────────────

/// Resolve the title bar background colour for the given mode and focus state.
///
/// Values from `WINDOW_SPEC.md` §SSD Title bar.
pub fn title_bar_background(is_dark: bool, is_focused: bool) -> [f32; 4] {
    match (is_dark, is_focused) {
        (false, true) => [1.0, 1.0, 1.0, 1.0],            // #FFFFFF
        (false, false) => [1.0, 1.0, 1.0, 0.5],            // rgba(255,255,255,0.5)
        (true, true) => [0.067, 0.075, 0.094, 1.0],        // #111318
        (true, false) => [0.067, 0.075, 0.094, 0.75],      // #111318 @ 75%
    }
}

/// Resolve the title bar bottom divider colour for the given mode.
///
/// Values from `WINDOW_SPEC.md` §SSD Title bar.
pub fn title_bar_divider(is_dark: bool) -> [f32; 4] {
    if is_dark {
        [0.0, 0.0, 0.0, 0.8]   // rgba(0,0,0,0.8)
    } else {
        [0.733, 0.733, 0.733, 1.0] // #BBBBBB
    }
}

// ─── DecorationRenderer ──────────────────────────────────────────────────────

/// Renders the server-side decoration title bar above the client content.
///
/// # Full iced integration (TODO)
///
/// Replace the solid-colour fallback below with:
/// 1. Build an iced widget tree: `Row::new()` with a `Text` widget for the title
///    and three `Button` widgets for close/maximize/minimize.
/// 2. Render via `IcedElement::draw(renderer, frame)` (COSMIC pattern).
/// 3. Cache the resulting GPU texture keyed by `(title, is_focused, is_dark)`.
/// 4. Invalidate cache on title change or focus change.
pub struct DecorationRenderer;

impl DecorationRenderer {
    /// Create a new decoration renderer.
    pub fn new() -> Self {
        Self
    }

    /// Render the title bar for a server-side decorated window.
    ///
    /// `title_rect` is the bounding box for the title bar (top of window,
    /// `title_bar_height` tall, full window width).
    ///
    /// Draws: background fill + 0.5 px bottom divider line.
    /// TODO: add title text and window control buttons.
    pub fn render<F: FrameRef>(
        &self,
        frame: &mut F,
        title_rect: Rect,
        _title: &str,
        is_focused: bool,
        is_dark: bool,
    ) -> Result<(), F::Error> {
        if title_rect.width <= 0.0 || title_rect.height <= 0.0 {
            return Ok(());
        }

        // Background fill.
        let bg = title_bar_background(is_dark, is_focused);
        frame.draw_colored_quad(title_rect, bg)?;

        // Bottom divider line (0.5 px).
        let divider_color = title_bar_divider(is_dark);
        let divider_rect = Rect::new(
            title_rect.x,
            title_rect.y + title_rect.height - 0.5,
            title_rect.width,
            0.5,
        );
        frame.draw_colored_quad(divider_rect, divider_color)?;

        // TODO: render title text centred in title_rect (Inter SemiBold 13 px).
        // TODO: render window control buttons via ButtonLayout::from_title_bar.

        Ok(())
    }

    /// Hit-test a pointer event against the title bar.
    ///
    /// Returns the button hit (or `ButtonHit::None` for the draggable area).
    pub fn hit_test_title_bar(title_rect: Rect, px: f32, py: f32) -> ButtonHit {
        if !title_rect.contains(px, py) {
            return ButtonHit::None;
        }
        ButtonLayout::from_title_bar(title_rect).hit_test(px, py)
    }
}

impl Default for DecorationRenderer {
    fn default() -> Self {
        Self::new()
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn title_rect() -> Rect {
        // Window at (100, 200) with width=400, title bar height=33 px.
        Rect::new(100.0, 200.0, 400.0, 33.0)
    }

    #[test]
    fn button_layout_close_is_rightmost() {
        let layout = ButtonLayout::from_title_bar(title_rect());
        // Close should be furthest right.
        assert!(layout.close.x > layout.maximize.x);
        assert!(layout.maximize.x > layout.minimize.x);
    }

    #[test]
    fn button_layout_close_right_inset() {
        let tr = title_rect();
        let layout = ButtonLayout::from_title_bar(tr);
        let expected_x = tr.x + tr.width - BUTTON_INSET_RIGHT - BUTTON_SIZE;
        assert!((layout.close.x - expected_x).abs() < 1e-4);
    }

    #[test]
    fn button_layout_gap_between_buttons() {
        let layout = ButtonLayout::from_title_bar(title_rect());
        let close_left = layout.close.x;
        let max_right = layout.maximize.x + layout.maximize.width;
        assert!((close_left - max_right - BUTTON_GAP).abs() < 1e-4);
    }

    #[test]
    fn button_layout_top_inset() {
        let tr = title_rect();
        let layout = ButtonLayout::from_title_bar(tr);
        let expected_y = tr.y + BUTTON_INSET_TOP;
        assert!((layout.close.y - expected_y).abs() < 1e-4);
        assert!((layout.maximize.y - expected_y).abs() < 1e-4);
        assert!((layout.minimize.y - expected_y).abs() < 1e-4);
    }

    #[test]
    fn button_size_matches_spec() {
        let layout = ButtonLayout::from_title_bar(title_rect());
        assert_eq!(layout.close.width, BUTTON_SIZE);
        assert_eq!(layout.close.height, BUTTON_SIZE);
        assert_eq!(layout.maximize.width, BUTTON_SIZE);
        assert_eq!(layout.minimize.width, BUTTON_SIZE);
    }

    #[test]
    fn hit_test_close_button() {
        let tr = title_rect();
        let layout = ButtonLayout::from_title_bar(tr);
        let cx = layout.close.x + layout.close.width * 0.5;
        let cy = layout.close.y + layout.close.height * 0.5;
        assert_eq!(layout.hit_test(cx, cy), ButtonHit::Close);
    }

    #[test]
    fn hit_test_maximize_button() {
        let tr = title_rect();
        let layout = ButtonLayout::from_title_bar(tr);
        let cx = layout.maximize.x + layout.maximize.width * 0.5;
        let cy = layout.maximize.y + layout.maximize.height * 0.5;
        assert_eq!(layout.hit_test(cx, cy), ButtonHit::Maximize);
    }

    #[test]
    fn hit_test_minimize_button() {
        let tr = title_rect();
        let layout = ButtonLayout::from_title_bar(tr);
        let cx = layout.minimize.x + layout.minimize.width * 0.5;
        let cy = layout.minimize.y + layout.minimize.height * 0.5;
        assert_eq!(layout.hit_test(cx, cy), ButtonHit::Minimize);
    }

    #[test]
    fn hit_test_title_area_returns_none() {
        let tr = title_rect();
        let layout = ButtonLayout::from_title_bar(tr);
        // Click in the title text area (left half of bar).
        let tx = tr.x + tr.width * 0.25;
        let ty = tr.y + tr.height * 0.5;
        assert_eq!(layout.hit_test(tx, ty), ButtonHit::None);
    }

    #[test]
    fn hit_test_outside_title_bar_is_none() {
        let tr = title_rect();
        // Below the title bar.
        let hit = DecorationRenderer::hit_test_title_bar(tr, tr.x + 10.0, tr.y + tr.height + 10.0);
        assert_eq!(hit, ButtonHit::None);
    }

    #[test]
    fn hit_test_close_via_decoration_renderer() {
        let tr = title_rect();
        let layout = ButtonLayout::from_title_bar(tr);
        let cx = layout.close.x + layout.close.width * 0.5;
        let cy = layout.close.y + layout.close.height * 0.5;
        let hit = DecorationRenderer::hit_test_title_bar(tr, cx, cy);
        assert_eq!(hit, ButtonHit::Close);
    }

    #[test]
    fn title_bar_background_light_active_is_white() {
        let bg = title_bar_background(false, true);
        assert_eq!(bg, [1.0, 1.0, 1.0, 1.0]);
    }

    #[test]
    fn title_bar_background_light_inactive_is_translucent() {
        let bg = title_bar_background(false, false);
        assert!((bg[3] - 0.5).abs() < 1e-6);
    }

    #[test]
    fn title_bar_background_dark_active_matches_spec() {
        // #111318 → rgb(17/255, 19/255, 24/255) ≈ (0.067, 0.075, 0.094)
        let bg = title_bar_background(true, true);
        assert!((bg[0] - 17.0 / 255.0).abs() < 0.001);
        assert!((bg[1] - 19.0 / 255.0).abs() < 0.001);
        assert!((bg[2] - 24.0 / 255.0).abs() < 0.001);
        assert_eq!(bg[3], 1.0);
    }
}
