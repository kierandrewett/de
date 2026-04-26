//! Advanced input protocol handlers:
//! relative-pointer, pointer-constraints, cursor-shape-v1,
//! keyboard-shortcuts-inhibit, pointer-gestures, tablet-v2,
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

use crate::wayland_state::SpikeState;

// ─── RelativePointer ──────────────────────────────────────────────────────────
// No trait needed; smithay handles everything internally.
delegate_relative_pointer!(SpikeState);

// ─── PointerConstraints ───────────────────────────────────────────────────────

impl PointerConstraintsHandler for SpikeState {
    fn new_constraint(
        &mut self,
        surface: &WlSurface,
        pointer: &smithay::input::pointer::PointerHandle<Self>,
    ) {
        // Auto-activate any constraint when created.
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
        _location: smithay::utils::Point<f64, Logical>,
    ) {
    }
}

delegate_pointer_constraints!(SpikeState);

// ─── PointerGestures ──────────────────────────────────────────────────────────
delegate_pointer_gestures!(SpikeState);

// ─── KeyboardShortcutsInhibit ─────────────────────────────────────────────────

impl KeyboardShortcutsInhibitHandler for SpikeState {
    fn keyboard_shortcuts_inhibit_state(&mut self) -> &mut KeyboardShortcutsInhibitState {
        &mut self.keyboard_shortcuts_inhibit_state
    }

    fn new_inhibitor(&mut self, inhibitor: KeyboardShortcutsInhibitor) {
        // Auto-activate: the app asked to grab all keys, grant it.
        inhibitor.activate();
    }

    fn inhibitor_destroyed(&mut self, _inhibitor: KeyboardShortcutsInhibitor) {}
}

delegate_keyboard_shortcuts_inhibit!(SpikeState);

// ─── CursorShape ──────────────────────────────────────────────────────────────
// cursor-shape-v1 requires no handler trait in smithay; the delegate macro
// wires everything.
delegate_cursor_shape!(SpikeState);

// ─── TabletManager ────────────────────────────────────────────────────────────
// cursor-shape-v1 requires TabletSeatHandler; default impl is fine for now.

impl TabletSeatHandler for SpikeState {
    fn tablet_tool_image(&mut self, _tool: &TabletToolDescriptor, _image: CursorImageStatus) {}
}

delegate_tablet_manager!(SpikeState);

// ─── TextInput ────────────────────────────────────────────────────────────────
// IME basics; full wiring is future work.
delegate_text_input_manager!(SpikeState);

// ─── InputMethod ─────────────────────────────────────────────────────────────

impl InputMethodHandler for SpikeState {
    fn new_popup(&mut self, _surface: PopupSurface) {}
    fn dismiss_popup(&mut self, _surface: PopupSurface) {}
    fn popup_repositioned(&mut self, _surface: PopupSurface) {}
    fn parent_geometry(&self, _parent: &WlSurface) -> Rectangle<i32, Logical> {
        Rectangle::default()
    }
}

delegate_input_method_manager!(SpikeState);

// ─── VirtualKeyboard ─────────────────────────────────────────────────────────
delegate_virtual_keyboard_manager!(SpikeState);
