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

    /// Hit-test layer-shell surfaces against the screen point (x, y).
    /// Iterates `state.layer_surfaces` checking the layers in the order
    /// the caller passes — callers should pass top-of-stack first
    /// (typically `[Overlay, Top]` for the chain ABOVE windows and
    /// `[Bottom, Background]` for the chain BELOW them). Uses
    /// `under_from_surface_tree` so subsurfaces inside a panel/dock
    /// receive events directly. Closes the layer-shell input gap (audit
    /// C11): without this, panels, docks, launchers, notification daemons
    /// and OSDs render but can't receive clicks/scroll, and
    /// `KeyboardInteractivity::OnDemand` is dead.
    pub(super) fn layer_surface_under(
        &self,
        state: &SpikeState,
        x: f64,
        y: f64,
        layers: &[smithay::wayland::shell::wlr_layer::Layer],
    ) -> Option<(WlSurface, f64, f64)> {
        use smithay::desktop::utils::under_from_surface_tree;
        use smithay::desktop::WindowSurfaceType;
        use smithay::utils::Point;

        for &want in layers {
            for li in &state.layer_surfaces {
                if li.layer != want {
                    continue;
                }
                if li.w <= 0 || li.h <= 0 {
                    continue;
                }
                let lx = x - li.x as f64;
                let ly = y - li.y as f64;
                if lx < 0.0 || ly < 0.0 || lx >= li.w as f64 || ly >= li.h as f64 {
                    continue;
                }
                let origin =
                    Point::<i32, smithay::utils::Logical>::from((li.x, li.y));
                if let Some((surface, sub_origin)) = under_from_surface_tree(
                    li.surface.wl_surface(),
                    Point::from((x, y)),
                    origin,
                    WindowSurfaceType::ALL,
                ) {
                    return Some((surface, sub_origin.x as f64, sub_origin.y as f64));
                }
                return Some((
                    li.surface.wl_surface().clone(),
                    li.x as f64,
                    li.y as f64,
                ));
            }
        }
        None
    }

    /// Full pointer hit-test chain in z-order, screen point → surface.
    /// Used by both motion routing and the press-time focus refresh so
    /// the two cannot disagree. While the session is locked, the only
    /// valid focus is the lock surface for this output. Otherwise:
    /// xdg popups (top) → layer-shell Overlay/Top → X11 override-redirect →
    /// xdg toplevels →
    /// layer-shell Bottom/Background. (Audit C11.)
    pub(super) fn surface_under_full(
        &self,
        state: &SpikeState,
        x: f64,
        y: f64,
    ) -> Option<(WlSurface, f64, f64)> {
        use smithay::wayland::shell::wlr_layer::Layer;
        if state.session_locked {
            return state
                .lock_surfaces
                .first()
                .map(|li| (li.surface.wl_surface().clone(), 0.0, 0.0));
        }
        self.popup_surface_under(state, x, y)
            .or_else(|| self.layer_surface_under(state, x, y, &[Layer::Overlay, Layer::Top]))
            .or_else(|| self.x11_override_redirect_surface_under(state, x, y))
            .or_else(|| self.wm.surface_under(x, y))
            .or_else(|| {
                self.layer_surface_under(state, x, y, &[Layer::Bottom, Layer::Background])
            })
    }

    pub(super) fn client_popup_surface_under(
        &self,
        state: &SpikeState,
        x: f64,
        y: f64,
    ) -> Option<(WlSurface, f64, f64)> {
        self.popup_surface_under(state, x, y)
            .or_else(|| self.x11_override_redirect_surface_under(state, x, y))
    }

    pub(super) fn x11_override_redirect_surface_under(
        &self,
        state: &SpikeState,
        x: f64,
        y: f64,
    ) -> Option<(WlSurface, f64, f64)> {
        use smithay::desktop::utils::under_from_surface_tree;
        use smithay::desktop::WindowSurfaceType;
        use smithay::utils::Point;

        for toplevel in state.toplevels.iter().rev() {
            let Some(x11) = toplevel.x11_surface.as_ref() else {
                continue;
            };
            if x11
                .user_data()
                .get::<crate::wayland::xwayland::X11OverrideRedirect>()
                .is_none()
            {
                continue;
            }

            let geo = x11.geometry();
            let width = geo.size.w.max(1) as f64;
            let height = geo.size.h.max(1) as f64;
            let origin_x = geo.loc.x as f64;
            let origin_y = geo.loc.y as f64;
            if x < origin_x || x >= origin_x + width || y < origin_y || y >= origin_y + height {
                continue;
            }

            let surface_origin =
                Point::<i32, smithay::utils::Logical>::from((geo.loc.x, geo.loc.y));
            if let Some((surface, origin)) = under_from_surface_tree(
                &toplevel.surface,
                Point::from((x, y)),
                surface_origin,
                WindowSurfaceType::ALL,
            ) {
                return Some((surface, origin.x as f64, origin.y as f64));
            }

            return Some((toplevel.surface.clone(), origin_x, origin_y));
        }

        None
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
