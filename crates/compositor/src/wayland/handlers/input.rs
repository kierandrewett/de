//! Advanced input protocol handlers:
//! relative-pointer, pointer-constraints, pointer-gestures,
//! keyboard-shortcuts-inhibit, cursor-shape, tablet-v2,
//! text-input-v3, input-method-v2, virtual-keyboard-v1.

use smithay::{
    backend::input::TabletToolDescriptor,
    delegate_cursor_shape, delegate_input_method_manager, delegate_keyboard_shortcuts_inhibit,
    delegate_pointer_constraints, delegate_pointer_gestures, delegate_relative_pointer,
    delegate_tablet_manager, delegate_text_input_manager, delegate_virtual_keyboard_manager,
    input::pointer::CursorImageStatus,
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    utils::{Logical, Rectangle},
    wayland::{
        input_method::{InputMethodHandler, PopupSurface},
        keyboard_shortcuts_inhibit::{
            KeyboardShortcutsInhibitHandler, KeyboardShortcutsInhibitState,
            KeyboardShortcutsInhibitor,
        },
        pointer_constraints::{with_pointer_constraint, PointerConstraintsHandler},
        tablet_manager::TabletSeatHandler,
    },
};

use crate::state::State;

// ─── RelativePointerManagerState ─────────────────────────────────────────────
delegate_relative_pointer!(State);

// ─── PointerConstraints ───────────────────────────────────────────────────────

impl PointerConstraintsHandler for State {
    fn new_constraint(&mut self, surface: &WlSurface, pointer: &smithay::input::pointer::PointerHandle<Self>) {
        with_pointer_constraint(surface, pointer, |c| {
            if let Some(c) = c {
                c.activate();
            }
        });
    }

    fn cursor_position_hint(
        &mut self,
        _surface: &WlSurface,
        _pointer: &smithay::input::pointer::PointerHandle<Self>,
        _location: smithay::utils::Point<f64, smithay::utils::Logical>,
    ) {
    }
}

delegate_pointer_constraints!(State);

// ─── PointerGesturesState ─────────────────────────────────────────────────────
delegate_pointer_gestures!(State);

// ─── KeyboardShortcutsInhibit ─────────────────────────────────────────────────

impl KeyboardShortcutsInhibitHandler for State {
    fn keyboard_shortcuts_inhibit_state(&mut self) -> &mut KeyboardShortcutsInhibitState {
        &mut self.common.keyboard_shortcuts_inhibit_state
    }

    fn new_inhibitor(&mut self, inhibitor: KeyboardShortcutsInhibitor) {
        inhibitor.activate();
    }

    fn inhibitor_destroyed(&mut self, _inhibitor: KeyboardShortcutsInhibitor) {}
}

delegate_keyboard_shortcuts_inhibit!(State);

// ─── CursorShape ──────────────────────────────────────────────────────────────
// cursor-shape-v1 requires TabletSeatHandler; the delegate macro handles dispatch.
delegate_cursor_shape!(State);

// ─── TabletManager ───────────────────────────────────────────────────────────

impl TabletSeatHandler for State {
    fn tablet_tool_image(&mut self, _tool: &TabletToolDescriptor, image: CursorImageStatus) {
        self.common.cursor_status = image;
    }
}

delegate_tablet_manager!(State);

// ─── TextInput ────────────────────────────────────────────────────────────────
delegate_text_input_manager!(State);

// ─── InputMethod ─────────────────────────────────────────────────────────────

impl InputMethodHandler for State {
    fn new_popup(&mut self, _surface: PopupSurface) {}

    fn dismiss_popup(&mut self, _surface: PopupSurface) {}

    fn popup_repositioned(&mut self, _surface: PopupSurface) {}

    fn parent_geometry(&self, _parent: &WlSurface) -> Rectangle<i32, Logical> {
        Rectangle::default()
    }
}

delegate_input_method_manager!(State);

// ─── VirtualKeyboard ─────────────────────────────────────────────────────────
delegate_virtual_keyboard_manager!(State);
