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
        // Anvil/state.rs:367 pattern: only auto-activate when the constraint
        // surface is currently the pointer focus. Otherwise a client could
        // request a lock on a surface that isn't even under the pointer and
        // hijack input the next time the cursor wanders over it.
        let Some(current_focus) = pointer.current_focus() else {
            return;
        };
        if &current_focus == surface {
            with_pointer_constraint(surface, pointer, |c| {
                if let Some(c) = c {
                    c.activate();
                }
            });
        }
    }

    fn cursor_position_hint(
        &mut self,
        surface: &WlSurface,
        pointer: &smithay::input::pointer::PointerHandle<Self>,
        location: smithay::utils::Point<f64, Logical>,
    ) {
        // Honoured only when the constraint is active — games (CS2, Counter-
        // Strike, etc.) use this to recentre the cursor inside the locked
        // surface so their HUD crosshair matches the OS pointer position.
        let active =
            with_pointer_constraint(surface, pointer, |c| c.is_some_and(|c| c.is_active()));
        if !active {
            return;
        }
        // Translate the surface-local hint to compositor coordinates by
        // looking up the toplevel that owns this surface. For X11 OR or
        // popups (rare for constraints) we fall back to the current pointer
        // location.
        let origin = self
            .toplevels
            .iter()
            .find(|t| t.surface == *surface)
            .map(|t| smithay::utils::Point::from((t.x as f64, t.y as f64)))
            .unwrap_or_else(|| pointer.current_location());
        pointer.set_location(origin + location);
        self.pointer_pos = ((origin + location).x, (origin + location).y);
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
    fn parent_geometry(&self, parent: &WlSurface) -> Rectangle<i32, Logical> {
        // Locate the parent in our toplevels list so the IME popup
        // (candidate window / emoji picker) anchors against the actual
        // window position. Was returning Rectangle::default() (0,0,0,0)
        // → fcitx5 / ibus drew their candidate window in the top-left
        // corner regardless of where the text field actually was.
        if let Some(tl) = self.toplevels.iter().find(|t| &t.surface == parent) {
            // Use the buffer-detected geometry if available, fall back to
            // a 1×1 anchor rect at the toplevel origin.
            return Rectangle::new(
                smithay::utils::Point::from((tl.x, tl.y)),
                smithay::utils::Size::from((1, 1)),
            );
        }
        Rectangle::default()
    }
}

delegate_input_method_manager!(SpikeState);

// ─── VirtualKeyboard ─────────────────────────────────────────────────────────
delegate_virtual_keyboard_manager!(SpikeState);
