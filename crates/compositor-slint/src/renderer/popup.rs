//! Popup positioning + hit-testing utilities for the compositor render path.
//!
//! Extracted from renderer.rs. `popup_rect` walks the popup hierarchy and
//! anchors each popup against its parent (which may itself be a popup or a
//! toplevel WindowState). `popup_surface_under` is the hit-test that
//! `forward_pointer_motion` / `forward_pointer_button` consult to route
//! pointer events to popups when the cursor is over one.

use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;

use super::CompositorApp;
use crate::wayland_state::{PopupInfo, SpikeState};

impl CompositorApp {
    pub(super) fn popup_rect(
        &self,
        state: &SpikeState,
        popup: &PopupInfo,
    ) -> Option<(f64, f64, f64, f64)> {
        let (bw, bh) = {
            let p = popup.pixels.lock().unwrap();
            (p.width as i32, p.height as i32)
        };
        let w = if popup.w > 0 { popup.w } else { bw };
        let h = if popup.h > 0 { popup.h } else { bh };
        if w <= 0 || h <= 0 {
            return None;
        }

        let mut abs_x = popup.rel_x;
        let mut abs_y = popup.rel_y;
        let mut cur_parent = popup.parent.clone();
        let mut anchored = false;
        for _ in 0..16 {
            if let Some(parent_popup) = state.popups.iter().find(|p| p.surface == cur_parent) {
                abs_x += parent_popup.rel_x;
                abs_y += parent_popup.rel_y;
                cur_parent = parent_popup.parent.clone();
                continue;
            }
            if let Some(tl_win) = self.wm.windows.values().find(|w| w.surface == cur_parent) {
                abs_x += tl_win.anim.current_x();
                let titlebar = if tl_win.csd {
                    0
                } else {
                    crate::wm::TITLEBAR_HEIGHT as i32
                };
                abs_y += tl_win.anim.current_y() + titlebar;
                anchored = true;
            }
            break;
        }

        anchored.then_some((abs_x as f64, abs_y as f64, w as f64, h as f64))
    }

    pub(super) fn popup_surface_under(
        &self,
        state: &SpikeState,
        x: f64,
        y: f64,
    ) -> Option<(WlSurface, f64, f64)> {
        use smithay::desktop::utils::under_from_surface_tree;
        use smithay::desktop::WindowSurfaceType;
        use smithay::utils::Point;

        for popup in state.popups.iter().rev() {
            let Some((px, py, pw, ph)) = self.popup_rect(state, popup) else {
                continue;
            };
            if x < px || x >= px + pw || y < py || y >= py + ph {
                continue;
            }

            let popup_origin = Point::<i32, smithay::utils::Logical>::from((px as i32, py as i32));
            if let Some((surface, origin)) = under_from_surface_tree(
                &popup.surface,
                Point::from((x, y)),
                popup_origin,
                WindowSurfaceType::ALL,
            ) {
                return Some((surface, origin.x as f64, origin.y as f64));
            }

            return Some((popup.surface.clone(), px, py));
        }

        None
    }
}
