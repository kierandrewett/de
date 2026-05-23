//! Miscellaneous protocol delegates that need no or trivial handler impls:
//! single-pixel-buffer, xdg-activation, content-type, alpha-modifier,
//! pointer-warp, xdg-foreign, xdg-dialog, xdg-system-bell,
//! toplevel-icon, toplevel-tag, security-context.

use smithay::{
    delegate_alpha_modifier, delegate_content_type, delegate_pointer_warp,
    delegate_security_context, delegate_single_pixel_buffer, delegate_xdg_activation,
    delegate_xdg_dialog, delegate_xdg_foreign, delegate_xdg_system_bell,
    delegate_xdg_toplevel_icon, delegate_xdg_toplevel_tag,
    reexports::{
        wayland_protocols::xdg::shell::server::xdg_toplevel::XdgToplevel,
        wayland_server::protocol::wl_surface::WlSurface,
    },
    utils::Point,
    wayland::{
        pointer_warp::PointerWarpHandler,
        security_context::{
            SecurityContext, SecurityContextHandler, SecurityContextListenerSource,
        },
        shell::xdg::{
            dialog::{ToplevelDialogHint, XdgDialogHandler},
            ToplevelSurface,
        },
        xdg_activation::{
            XdgActivationHandler, XdgActivationState, XdgActivationToken, XdgActivationTokenData,
        },
        xdg_foreign::{XdgForeignHandler, XdgForeignState},
        xdg_system_bell::XdgSystemBellHandler,
        xdg_toplevel_icon::XdgToplevelIconHandler,
        xdg_toplevel_tag::XdgToplevelTagHandler,
    },
};
use tracing::debug;

use crate::wayland_state::SpikeState;

// ─── SinglePixelBuffer ───────────────────────────────────────────────────────
delegate_single_pixel_buffer!(SpikeState);

// ─── AlphaModifier ────────────────────────────────────────────────────────────
delegate_alpha_modifier!(SpikeState);

// ─── ContentType ─────────────────────────────────────────────────────────────
delegate_content_type!(SpikeState);

// ─── XdgActivation ────────────────────────────────────────────────────────────

impl XdgActivationHandler for SpikeState {
    fn activation_state(&mut self) -> &mut XdgActivationState {
        &mut self.activation_state
    }

    fn request_activation(
        &mut self,
        token: XdgActivationToken,
        token_data: XdgActivationTokenData,
        surface: WlSurface,
    ) {
        // Focus-stealing prevention: an activation token minted without
        // a `(serial, seat)` from a recent user input event is "blind"
        // — programmatic, no user intent behind it. Drop it on the
        // floor. Tokens with a serial pass through and raise/focus the
        // requested surface (terminal launches that pass
        // XDG_ACTIVATION_TOKEN, notification action clicks, etc).
        if token_data.serial.is_none() {
            tracing::debug!(
                "xdg-activation: dropping blind token (no input serial) for app_id={:?} token={:?}",
                token_data.app_id,
                token,
            );
            return;
        }
        self.active_surface = Some(surface.clone());
        if let Some(kb) = self.seat.get_keyboard() {
            let serial = smithay::utils::SERIAL_COUNTER.next_serial();
            kb.set_focus(
                self,
                Some(crate::wayland::xwayland::KeyboardFocusTarget::for_wl_surface(
                    self, &surface,
                )),
                serial,
            );
        }
    }
}

delegate_xdg_activation!(SpikeState);

// ─── XdgForeign ───────────────────────────────────────────────────────────────

impl XdgForeignHandler for SpikeState {
    fn xdg_foreign_state(&mut self) -> &mut XdgForeignState {
        &mut self.xdg_foreign_state
    }
}

delegate_xdg_foreign!(SpikeState);

// ─── SecurityContext ──────────────────────────────────────────────────────────

impl SecurityContextHandler for SpikeState {
    fn context_created(&mut self, source: SecurityContextListenerSource, context: SecurityContext) {
        // Mount the new listener socket into the calloop event loop so any
        // sandboxed client connecting through it is bound with a
        // SecurityContext-bearing ClientState — that's what lets
        // `client_has_no_security_context` deny them privileged globals
        // later. Mirrors cosmic/src/wayland/handlers/security_context.rs.
        let context_clone = context.clone();
        let dh = self.display_handle.clone();
        let res = self
            .loop_handle
            .insert_source(source, move |stream, _, _state| {
                let mut dh = dh.clone();
                let _ = dh.insert_client(
                    stream,
                    std::sync::Arc::new(crate::wayland_state::ClientState {
                        compositor_state: Default::default(),
                        security_context: Some(context_clone.clone()),
                    }),
                );
            });
        if let Err(e) = res {
            tracing::warn!("security_context: failed to insert listener source: {e}");
        }
        debug!(app_id = ?context.app_id, "security context created");
    }
}

delegate_security_context!(SpikeState);

// ─── XdgSystemBell ────────────────────────────────────────────────────────────

impl XdgSystemBellHandler for SpikeState {
    fn ring(&mut self, surface: Option<WlSurface>) {
        // Set a tiny flag the renderer reads each frame to flash the
        // window's chrome / panel (visual bell, accessibility-friendly
        // alternative to an audible beep). When `surface` is given we
        // target that specific window; None means "system-wide bell" —
        // we flash the focused window in that case.
        let target = surface.or_else(|| self.active_surface.clone());
        self.pending_bell = target;
        debug!(
            "system bell (target_surface_present={})",
            self.pending_bell.is_some()
        );
    }
}

delegate_xdg_system_bell!(SpikeState);

// ─── XdgToplevelIcon ─────────────────────────────────────────────────────────

impl XdgToplevelIconHandler for SpikeState {
    fn set_icon(&mut self, toplevel: XdgToplevel, _wl_surface: WlSurface) {
        // Read the icon-name (themed-icon identifier per the xdg-icon-spec)
        // off the toplevel's cached state and store it on ToplevelInfo so
        // the dock / taskbar / alt-tab UI can resolve and display an icon
        // for this window. Buffer-based icons (raw pixel data) are
        // available via ToplevelIconCachedState.buffers(); deferred until
        // we have a real consumer.
        use smithay::reexports::wayland_server::Resource;
        let surface_id = toplevel.id().protocol_id();
        // Find the matching ToplevelInfo by the wl_surface that owns this
        // xdg_toplevel — smithay attaches the toplevel resource to the
        // surface's XdgToplevelSurfaceData.
        let target = self.toplevels.iter().position(|t| {
            // The xdg_toplevel resource lives on the surface's user-data.
            smithay::wayland::compositor::with_states(&t.surface, |states| {
                states
                    .data_map
                    .get::<smithay::wayland::shell::xdg::XdgToplevelSurfaceData>()
                    .and_then(|d| d.lock().ok().map(|g| g.title.is_some()))
                    .unwrap_or(false)
            }) && t
                .toplevel
                .as_ref()
                .map(|ts| ts.xdg_toplevel().id().protocol_id() == surface_id)
                .unwrap_or(false)
        });
        if let Some(idx) = target {
            let name =
                smithay::wayland::compositor::with_states(&self.toplevels[idx].surface, |states| {
                    let mut data = states
                        .cached_state
                        .get::<smithay::wayland::xdg_toplevel_icon::ToplevelIconCachedState>(
                    );
                    data.current().icon_name().map(|s| s.to_string())
                });
            self.toplevels[idx].icon_name = name;
        }
    }
}

delegate_xdg_toplevel_icon!(SpikeState);

// ─── XdgToplevelTag ──────────────────────────────────────────────────────────

impl XdgToplevelTagHandler for SpikeState {
    fn set_tag(&mut self, toplevel: XdgToplevel, tag: String) {
        use smithay::reexports::wayland_server::Resource;
        let xt_id = toplevel.id().protocol_id();
        if let Some(tl) = self.toplevels.iter_mut().find(|t| {
            t.toplevel
                .as_ref()
                .map(|ts| ts.xdg_toplevel().id().protocol_id() == xt_id)
                .unwrap_or(false)
        }) {
            tl.tag = Some(tag);
        }
    }
    fn set_description(&mut self, toplevel: XdgToplevel, description: String) {
        use smithay::reexports::wayland_server::Resource;
        let xt_id = toplevel.id().protocol_id();
        if let Some(tl) = self.toplevels.iter_mut().find(|t| {
            t.toplevel
                .as_ref()
                .map(|ts| ts.xdg_toplevel().id().protocol_id() == xt_id)
                .unwrap_or(false)
        }) {
            tl.description = Some(description);
        }
    }
}

delegate_xdg_toplevel_tag!(SpikeState);

// ─── XdgDialog ────────────────────────────────────────────────────────────────

impl XdgDialogHandler for SpikeState {
    fn dialog_hint_changed(&mut self, toplevel: ToplevelSurface, hint: ToplevelDialogHint) {
        // Mirror the dialog hint into our ToplevelInfo so the WM can apply
        // dialog-style placement (centred on parent, no minimise, raise on
        // map). Modal vs non-modal currently treated the same.
        let wl = toplevel.wl_surface();
        if let Some(tl) = self.toplevels.iter_mut().find(|t| &t.surface == wl) {
            tl.is_dialog = matches!(hint, ToplevelDialogHint::Dialog | ToplevelDialogHint::Modal);
        }
        debug!(?hint, "xdg-dialog hint changed");
    }
}

delegate_xdg_dialog!(SpikeState);

// ─── PointerWarp ─────────────────────────────────────────────────────────────

impl PointerWarpHandler for SpikeState {
    fn warp_pointer(
        &mut self,
        surface: WlSurface,
        _pointer: smithay::reexports::wayland_server::protocol::wl_pointer::WlPointer,
        pos: Point<f64, smithay::utils::Logical>,
        _serial: smithay::utils::Serial,
    ) {
        // `pointer-warp-v1`: the focused surface tells us where the pointer
        // should be. Translate surface-local → compositor-space using the
        // toplevel position and apply.  Spec says we MUST only honour this
        // when the surface currently has pointer focus; smithay already
        // gates the dispatch on focus so we just convert and apply here.
        let Some(seat_pointer) = self.seat.get_pointer() else {
            return;
        };
        let origin = self
            .toplevels
            .iter()
            .find(|t| t.surface == surface)
            .map(|t| smithay::utils::Point::from((t.x as f64, t.y as f64)))
            .unwrap_or_else(|| seat_pointer.current_location());
        let target = origin + pos;
        seat_pointer.set_location(target);
        self.pointer_pos = (target.x, target.y);
    }
}

delegate_pointer_warp!(SpikeState);
