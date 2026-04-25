//! Miscellaneous protocol handlers — activation, foreign-toplevel-list, alpha-modifier,
//! security-context, fifo, commit-timing, content-type, single-pixel-buffer,
//! presentation-time, viewporter, background-effect, pointer-warp, system-bell,
//! toplevel-icon/tag, xdg-dialog.

use smithay::{
    delegate_alpha_modifier, delegate_background_effect, delegate_commit_timing,
    delegate_content_type, delegate_fifo, delegate_foreign_toplevel_list, delegate_pointer_warp,
    delegate_presentation, delegate_security_context, delegate_single_pixel_buffer,
    delegate_viewporter, delegate_xdg_activation, delegate_xdg_dialog, delegate_xdg_foreign,
    delegate_xdg_system_bell, delegate_xdg_toplevel_icon, delegate_xdg_toplevel_tag,
    reexports::{
        wayland_protocols::xdg::shell::server::xdg_toplevel::XdgToplevel,
        wayland_server::protocol::wl_surface::WlSurface,
    },
    utils::Point,
    wayland::{
        compositor::RegionAttributes,
        foreign_toplevel_list::{ForeignToplevelListHandler, ForeignToplevelListState},
        security_context::{
            SecurityContext, SecurityContextHandler, SecurityContextListenerSource,
        },
        shell::xdg::{
            dialog::{ToplevelDialogHint, XdgDialogHandler},
            ToplevelSurface,
        },
        xdg_activation::{
            XdgActivationHandler, XdgActivationState, XdgActivationToken,
            XdgActivationTokenData,
        },
        xdg_foreign::{XdgForeignHandler, XdgForeignState},
        xdg_system_bell::XdgSystemBellHandler,
        xdg_toplevel_icon::XdgToplevelIconHandler,
        xdg_toplevel_tag::XdgToplevelTagHandler,
        background_effect::ExtBackgroundEffectHandler,
        pointer_warp::PointerWarpHandler,
    },
};

use crate::state::State;

// ─── XdgActivation (app focus requests) ──────────────────────────────────────

impl XdgActivationHandler for State {
    fn activation_state(&mut self) -> &mut XdgActivationState {
        &mut self.common.activation_state
    }

    fn request_activation(
        &mut self,
        _token: XdgActivationToken,
        _token_data: XdgActivationTokenData,
        surface: WlSurface,
    ) {
        use smithay::wayland::seat::WaylandFocus;
        let window = self
            .common
            .space
            .elements()
            .find(|w| w.wl_surface().as_deref() == Some(&surface))
            .cloned();
        if let Some(window) = window {
            self.common.space.raise_element(&window, true);
        }
    }
}

delegate_xdg_activation!(State);

// ─── XdgForeign (cross-app parent/child) ─────────────────────────────────────

impl XdgForeignHandler for State {
    fn xdg_foreign_state(&mut self) -> &mut XdgForeignState {
        &mut self.common.xdg_foreign_state
    }
}

delegate_xdg_foreign!(State);

// ─── ForeignToplevelList ──────────────────────────────────────────────────────

impl ForeignToplevelListHandler for State {
    fn foreign_toplevel_list_state(&mut self) -> &mut ForeignToplevelListState {
        &mut self.common.foreign_toplevel_list_state
    }
}

delegate_foreign_toplevel_list!(State);

// ─── SecurityContext (Flatpak sandbox identity) ───────────────────────────────

impl SecurityContextHandler for State {
    fn context_created(&mut self, _source: SecurityContextListenerSource, context: SecurityContext) {
        tracing::debug!(
            app_id = ?context.app_id,
            instance_id = ?context.instance_id,
            "security context created",
        );
    }
}

delegate_security_context!(State);

// ─── AlphaModifier ────────────────────────────────────────────────────────────
delegate_alpha_modifier!(State);

// ─── Fifo (vsync) ────────────────────────────────────────────────────────────
delegate_fifo!(State);

// ─── CommitTiming (frame pacing) ─────────────────────────────────────────────
delegate_commit_timing!(State);

// ─── ContentType (VRR hints) ─────────────────────────────────────────────────
delegate_content_type!(State);

// ─── SinglePixelBuffer ───────────────────────────────────────────────────────
delegate_single_pixel_buffer!(State);

// ─── PresentationTime ────────────────────────────────────────────────────────
delegate_presentation!(State);

// ─── Viewporter ──────────────────────────────────────────────────────────────
delegate_viewporter!(State);

// ─── XdgSystemBell ────────────────────────────────────────────────────────────

impl XdgSystemBellHandler for State {
    fn ring(&mut self, _surface: Option<WlSurface>) {
        tracing::debug!("system bell");
    }
}

delegate_xdg_system_bell!(State);

// ─── XdgToplevelIcon ─────────────────────────────────────────────────────────

impl XdgToplevelIconHandler for State {
    fn set_icon(&mut self, _toplevel: XdgToplevel, _wl_surface: WlSurface) {}
}

delegate_xdg_toplevel_icon!(State);

// ─── XdgToplevelTag ──────────────────────────────────────────────────────────

impl XdgToplevelTagHandler for State {
    fn set_tag(&mut self, _toplevel: XdgToplevel, _tag: String) {}
    fn set_description(&mut self, _toplevel: XdgToplevel, _description: String) {}
}

delegate_xdg_toplevel_tag!(State);

// ─── XdgDialog ────────────────────────────────────────────────────────────────

impl XdgDialogHandler for State {
    fn dialog_hint_changed(&mut self, _toplevel: ToplevelSurface, _hint: ToplevelDialogHint) {}
}

delegate_xdg_dialog!(State);

// ─── BackgroundEffect ─────────────────────────────────────────────────────────

impl ExtBackgroundEffectHandler for State {
    fn set_blur_region(&mut self, _wl_surface: WlSurface, _region: RegionAttributes) {}
}

delegate_background_effect!(State);

// ─── PointerWarp ─────────────────────────────────────────────────────────────

impl PointerWarpHandler for State {
    fn warp_pointer(
        &mut self,
        _surface: WlSurface,
        _pointer: smithay::reexports::wayland_server::protocol::wl_pointer::WlPointer,
        _pos: Point<f64, smithay::utils::Logical>,
        _serial: smithay::utils::Serial,
    ) {
    }
}

delegate_pointer_warp!(State);
