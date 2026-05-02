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
        foreign_toplevel_list::ForeignToplevelListState,
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
        pointer_warp::PointerWarpHandler,
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
        _token: XdgActivationToken,
        _token_data: XdgActivationTokenData,
        surface: WlSurface,
    ) {
        // Honour the activation request unconditionally for now — anvil
        // does the same. A focus-stealing-prevention pass would gate on
        // `token_data.user_data` (a recent user-interaction serial); we
        // don't yet track that. Without this handler doing anything,
        // "open from terminal" / "click notification action" workflows
        // never raise the new window.
        self.active_surface = Some(surface.clone());
        if let Some(kb) = self.seat.get_keyboard() {
            let serial = smithay::utils::SERIAL_COUNTER.next_serial();
            kb.set_focus(self, Some(surface), serial);
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
    fn context_created(
        &mut self,
        _source: SecurityContextListenerSource,
        context: SecurityContext,
    ) {
        debug!(app_id = ?context.app_id, "security context created");
    }
}

delegate_security_context!(SpikeState);

// ─── XdgSystemBell ────────────────────────────────────────────────────────────

impl XdgSystemBellHandler for SpikeState {
    fn ring(&mut self, _surface: Option<WlSurface>) {
        debug!("system bell");
    }
}

delegate_xdg_system_bell!(SpikeState);

// ─── XdgToplevelIcon ─────────────────────────────────────────────────────────

impl XdgToplevelIconHandler for SpikeState {
    fn set_icon(&mut self, _toplevel: XdgToplevel, _wl_surface: WlSurface) {}
}

delegate_xdg_toplevel_icon!(SpikeState);

// ─── XdgToplevelTag ──────────────────────────────────────────────────────────

impl XdgToplevelTagHandler for SpikeState {
    fn set_tag(&mut self, _toplevel: XdgToplevel, _tag: String) {}
    fn set_description(&mut self, _toplevel: XdgToplevel, _description: String) {}
}

delegate_xdg_toplevel_tag!(SpikeState);

// ─── XdgDialog ────────────────────────────────────────────────────────────────

impl XdgDialogHandler for SpikeState {
    fn dialog_hint_changed(&mut self, _toplevel: ToplevelSurface, _hint: ToplevelDialogHint) {}
}

delegate_xdg_dialog!(SpikeState);

// ─── PointerWarp ─────────────────────────────────────────────────────────────

impl PointerWarpHandler for SpikeState {
    fn warp_pointer(
        &mut self,
        _surface: WlSurface,
        _pointer: smithay::reexports::wayland_server::protocol::wl_pointer::WlPointer,
        _pos: Point<f64, smithay::utils::Logical>,
        _serial: smithay::utils::Serial,
    ) {
    }
}

delegate_pointer_warp!(SpikeState);
