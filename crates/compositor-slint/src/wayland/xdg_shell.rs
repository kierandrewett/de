//! xdg-shell handler — toplevels, popups, configure.
//!
//! Initial-configure deferral: per the xdg-shell protocol the first
//! `xdg_surface.configure` must arrive AFTER the client has had a chance to
//! set its `app_id` / `title` / decoration mode but BEFORE it commits its
//! first buffer. We therefore do NOT call `send_configure()` from
//! `new_toplevel` / `new_popup`; it is deferred to the commit handler in
//! `wayland/compositor.rs`, which checks `XdgToplevelSurfaceData::initial_configure_sent`
//! and fires the configure on the very first commit.

use std::sync::{Arc, Mutex};

use smithay::{
    delegate_xdg_shell,
    reexports::{
        wayland_protocols::xdg::shell::server::xdg_toplevel::{self, ResizeEdge as XdgResizeEdge},
        wayland_server::protocol::{wl_output::WlOutput, wl_seat::WlSeat, wl_surface::WlSurface},
    },
    utils::{Serial, SERIAL_COUNTER},
    wayland::shell::xdg::{
        Configure, PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState,
    },
};
use tracing::info;

use crate::resize::ResizeEdge;
use crate::wayland_state::{ClientSurfaceData, SpikeState, ToplevelInfo};

/// Cascading window offset: each new window is placed 40px further right/down.
const CASCADE_STEP: i32 = 40;
const CASCADE_BASE: i32 = 100;

impl XdgShellHandler for SpikeState {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg_shell_state
    }

    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        let idx = self.toplevels.len() as i32;
        let x = CASCADE_BASE + idx * CASCADE_STEP;
        let y = CASCADE_BASE + idx * CASCADE_STEP;

        info!("new xdg toplevel #{} at ({},{})", idx, x, y);

        // Pre-set the initial content size; the actual `send_configure()`
        // is deferred to the commit handler so the client has had a chance
        // to set app_id / title / decoration mode first (avoids a stale
        // first round-trip and bad initial decoration negotiation).
        surface.with_pending_state(|s| {
            s.size = Some((800, 600).into());
        });

        let wl_surface = surface.wl_surface().clone();

        // Register with ext-foreign-toplevel-list-v1 so docks / taskbars /
        // alt-tab clients see the window appear. Title and app_id are sent
        // empty here — the client hasn't set them yet at new_toplevel time;
        // the per-frame sync in renderer.rs::update_windows pushes them as
        // soon as the client populates `XdgToplevelSurfaceData`.
        let foreign_handle = Some(
            self.foreign_toplevel_list_state
                .new_toplevel::<Self>("", ""),
        );

        // Add to toplevels list.
        self.toplevels.push(ToplevelInfo {
            surface: wl_surface.clone(),
            toplevel: Some(surface.clone()),
            x11_surface: None,
            x,
            y,
            pixels: Arc::new(Mutex::new(ClientSurfaceData::default())),
            surface_pixels: Arc::new(Mutex::new(std::collections::HashMap::new())),
            csd: false,
            foreign_handle,
            last_advertised_title: String::new(),
            last_advertised_app_id: String::new(),
            is_dialog: false,
            icon_name: None,
            tag: None,
            description: None,
        });

        // Focus the new toplevel (most recently mapped = focused).
        self.active_surface = Some(wl_surface.clone());

        if let Some(kb) = self.seat.get_keyboard() {
            kb.set_focus(self, Some(wl_surface), SERIAL_COUNTER.next_serial());
        }
    }

    fn new_popup(&mut self, surface: PopupSurface, positioner: PositionerState) {
        // Track + pre-configure the popup. As with toplevels, the initial
        // `send_configure()` is deferred to the commit handler so it goes
        // out after the client has fully populated its xdg_surface state.
        let geom = positioner.get_geometry();
        surface.with_pending_state(|s| {
            s.geometry = geom;
            s.positioner = positioner;
        });
        // Re-run the constraint solver against the output so the popup never
        // renders off-screen — anvil/shell/xdg.rs:63.
        self.unconstrain_popup(&surface);
        let parent = match surface.get_parent_surface() {
            Some(p) => p,
            None => {
                tracing::warn!("popup has no parent surface — discarding");
                return;
            }
        };
        let wl = surface.wl_surface().clone();
        info!(
            "new xdg popup at ({},{}) size {}x{}",
            geom.loc.x, geom.loc.y, geom.size.w, geom.size.h
        );
        self.popups.push(crate::wayland_state::PopupInfo {
            surface: wl,
            popup: surface,
            parent,
            rel_x: geom.loc.x,
            rel_y: geom.loc.y,
            w: geom.size.w,
            h: geom.size.h,
            pixels: std::sync::Arc::new(std::sync::Mutex::new(
                crate::wayland_state::ClientSurfaceData::default(),
            )),
            surface_pixels: std::sync::Arc::new(std::sync::Mutex::new(
                std::collections::HashMap::new(),
            )),
        });
    }

    fn toplevel_destroyed(&mut self, surface: ToplevelSurface) {
        let wl = surface.wl_surface();
        info!("toplevel destroyed");

        // Send `closed` on the foreign-toplevel handle so taskbars/docks drop
        // the entry right away (before our close animation finishes). The
        // entry stays in `self.toplevels` until the WM sweep so the animation
        // can play.
        if let Some(t) = self.toplevels.iter().find(|t| &t.surface == wl) {
            if let Some(h) = &t.foreign_handle {
                self.foreign_toplevel_list_state.remove_toplevel(h);
            }
        }

        // Push to destroyed_surfaces so the WM can start a close animation.
        self.destroyed_surfaces.push(wl.clone());

        // Keep the toplevel in `self.toplevels` until the WM close animation
        // finishes — update_windows will remove it via sweep_closed.
        // We do NOT retain-filter here; the pixel buffer stays alive for the animation.

        // Update active_surface to the most recent remaining toplevel (if any).
        // The WM will refine this when it processes the close.
        let still_mapped: Vec<_> = self.toplevels.iter().filter(|t| &t.surface != wl).collect();
        self.active_surface = still_mapped.last().map(|t| t.surface.clone());

        // Legacy single-surface pixel buffer: clear if it was the active one.
        if self.active_surface.is_none() {
            *self.client_pixels.lock().unwrap() = ClientSurfaceData::default();
        }
    }

    fn popup_destroyed(&mut self, surface: PopupSurface) {
        let wl = surface.wl_surface().clone();
        self.popups.retain(|p| p.surface != wl);
        // PopupManager has its own destroy hook so we don't need to remove
        // explicitly, but the auto-cleanup helper releases the slot now.
        self.popup_manager.cleanup();
    }

    fn grab(&mut self, surface: PopupSurface, seat: WlSeat, serial: Serial) {
        // Real popup grab via smithay's PopupManager. Anvil shell/xdg.rs:380.
        // Without this, GTK/Qt context menus, comboboxes, and submenus don't
        // get a keyboard grab — keystrokes leak to the parent window — and
        // dismissal on click-outside the popup hierarchy is best-effort.
        use smithay::desktop::{
            find_popup_root_surface, PopupKeyboardGrab, PopupKind, PopupPointerGrab,
            PopupUngrabStrategy,
        };
        use smithay::input::{pointer::Focus, Seat};
        let seat: Seat<SpikeState> = match Seat::from_resource(&seat) {
            Some(s) => s,
            None => return,
        };
        let kind = PopupKind::Xdg(surface.clone());
        let Ok(root) = find_popup_root_surface(&kind) else {
            return;
        };
        let ret = self.popup_manager.grab_popup(root, kind, &seat, serial);
        let mut grab = match ret {
            Ok(g) => g,
            Err(e) => {
                tracing::warn!("popup grab failed: {e:?}");
                return;
            }
        };
        if let Some(keyboard) = seat.get_keyboard() {
            // Spec: if another grab is in progress with a different serial,
            // ungrab and bail (anvil pattern).
            if keyboard.is_grabbed()
                && !(keyboard.has_grab(serial)
                    || keyboard.has_grab(grab.previous_serial().unwrap_or(serial)))
            {
                grab.ungrab(PopupUngrabStrategy::All);
                return;
            }
            keyboard.set_focus(self, grab.current_grab(), serial);
            keyboard.set_grab(self, PopupKeyboardGrab::new(&grab), serial);
        }
        if let Some(pointer) = seat.get_pointer() {
            if pointer.is_grabbed()
                && !(pointer.has_grab(serial)
                    || pointer.has_grab(grab.previous_serial().unwrap_or_else(|| grab.serial())))
            {
                grab.ungrab(PopupUngrabStrategy::All);
                return;
            }
            pointer.set_grab(self, PopupPointerGrab::new(&grab), serial, Focus::Keep);
        }
    }

    fn reposition_request(
        &mut self,
        surface: PopupSurface,
        positioner: PositionerState,
        token: u32,
    ) {
        surface.with_pending_state(|s| {
            s.geometry = positioner.get_geometry();
            s.positioner = positioner;
        });
        // Unconstrain against the output so the popup doesn't render off-screen
        // when the positioner asks for an anchor close to the edge. Mirrors
        // anvil/shell/xdg.rs:70-77.
        self.unconstrain_popup(&surface);
        surface.send_repositioned(token);
    }

    fn ack_configure(&mut self, _surface: WlSurface, _configure: Configure) {
        // Smithay caches acknowledged configures internally; we have no
        // resize-grab state machine on the wayland side to advance (the
        // renderer drives configures during drags and treats the next
        // committed buffer as confirmation). Implementing this handler at
        // all is what stops GTK4 etc. spinning in a configure storm — the
        // protocol just needs the trait method to be reachable.
    }

    fn move_request(&mut self, surface: ToplevelSurface, seat: WlSeat, serial: Serial) {
        // Serial / focus validation, mirrors anvil/shell/xdg.rs:80-97.
        // Without this, any client could request a move grab at any time —
        // including when it doesn't have focus or wasn't the recipient of
        // the most recent pointer/touch button event. Reject the request if
        // the serial doesn't match a real grab on this surface.
        if !self.validate_grab_serial(&seat, surface.wl_surface(), serial) {
            return;
        }
        // Defer: the renderer has the pointer position + ActiveDrag
        // machinery. It will translate this into an `ActiveDrag::Move`
        // grab on its next iteration.
        self.pending_xdg_move.push(surface.wl_surface().clone());
    }

    fn resize_request(
        &mut self,
        surface: ToplevelSurface,
        seat: WlSeat,
        serial: Serial,
        edges: XdgResizeEdge,
    ) {
        if !self.validate_grab_serial(&seat, surface.wl_surface(), serial) {
            return;
        }
        let edge = match edges {
            XdgResizeEdge::Top => ResizeEdge::North,
            XdgResizeEdge::Bottom => ResizeEdge::South,
            XdgResizeEdge::Left => ResizeEdge::West,
            XdgResizeEdge::Right => ResizeEdge::East,
            XdgResizeEdge::TopLeft => ResizeEdge::NorthWest,
            XdgResizeEdge::TopRight => ResizeEdge::NorthEast,
            XdgResizeEdge::BottomLeft => ResizeEdge::SouthWest,
            XdgResizeEdge::BottomRight => ResizeEdge::SouthEast,
            // `None` (no edge) — protocol-spec: treat as a no-op.
            _ => return,
        };
        self.pending_xdg_resize
            .push((surface.wl_surface().clone(), edge));
    }

    fn maximize_request(&mut self, surface: ToplevelSurface) {
        surface.with_pending_state(|s| {
            s.states.set(xdg_toplevel::State::Maximized);
        });
        self.pending_xdg_maximize
            .push((surface.wl_surface().clone(), true));
        // Protocol requires us to always reply with a configure. If the
        // initial configure has not been sent yet the deferred path will
        // include the Maximized state we just set above.
        if surface.is_initial_configure_sent() {
            surface.send_configure();
        }
    }

    fn unmaximize_request(&mut self, surface: ToplevelSurface) {
        surface.with_pending_state(|s| {
            s.states.unset(xdg_toplevel::State::Maximized);
            s.size = None;
        });
        self.pending_xdg_maximize
            .push((surface.wl_surface().clone(), false));
        if surface.is_initial_configure_sent() {
            surface.send_configure();
        }
    }

    fn fullscreen_request(&mut self, surface: ToplevelSurface, output: Option<WlOutput>) {
        surface.with_pending_state(|s| {
            s.states.set(xdg_toplevel::State::Fullscreen);
            s.fullscreen_output = output;
        });
        self.pending_xdg_fullscreen
            .push((surface.wl_surface().clone(), true));
        if surface.is_initial_configure_sent() {
            surface.send_configure();
        }
    }

    fn unfullscreen_request(&mut self, surface: ToplevelSurface) {
        surface.with_pending_state(|s| {
            s.states.unset(xdg_toplevel::State::Fullscreen);
            s.size = None;
            s.fullscreen_output = None;
        });
        self.pending_xdg_fullscreen
            .push((surface.wl_surface().clone(), false));
        if surface.is_initial_configure_sent() {
            surface.send_configure();
        }
    }

    fn minimize_request(&mut self, surface: ToplevelSurface) {
        self.pending_xdg_minimize.push(surface.wl_surface().clone());
    }
}

delegate_xdg_shell!(SpikeState);
