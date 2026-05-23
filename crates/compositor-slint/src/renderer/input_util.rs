//! Standalone input helpers extracted from renderer.rs.
//!
//! - `winit_button_to_evdev` — winit::MouseButton → Linux evdev code.
//! - `forward_keyboard_event` — pump one PendingKeyEvent through the seat,
//!   honouring session-lock + layer-shell exclusive-keyboard routing.
//! - `ascii_to_scancode` — ASCII → (evdev_scancode, shift?) for the IPC
//!   `TypeText` command.
//! - `compute_menu_height` — Slint context-menu height in CSS pixels;
//!   used by the right-click placement code so menus that flip up anchor
//!   their bottom edge AT the cursor, not 60–70 px above it.

use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use winit::event::MouseButton;

use super::PendingKeyEvent;
use crate::wayland_state::SpikeState;

/// Map a winit `MouseButton` to a Linux evdev button code.
pub fn winit_button_to_evdev(button: MouseButton) -> u32 {
    match button {
        MouseButton::Left => 0x110,    // BTN_LEFT
        MouseButton::Right => 0x111,   // BTN_RIGHT
        MouseButton::Middle => 0x112,  // BTN_MIDDLE
        MouseButton::Back => 0x116,    // BTN_SIDE
        MouseButton::Forward => 0x115, // BTN_EXTRA
        _ => 0x110,
    }
}

pub fn forward_keyboard_event(state: &mut SpikeState, key_event: PendingKeyEvent) {
    use smithay::{backend::input::KeyState, input::keyboard::Keycode, utils::SERIAL_COUNTER};

    // Reset idle timer on keyboard input — fixes the screen-locks-while-typing
    // bug called out in the input audit. See backend/udev.rs handle_libinput_input_event.
    state.idle_notifier_state.notify_activity(&state.seat);

    // Session-lock gate: keys go ONLY to the lock surface. Without this,
    // the user could keep typing into a focused toplevel that's hidden
    // behind the lock — a textbook lock-screen bypass.
    let surface: Option<WlSurface> = if state.session_locked {
        state
            .lock_surface_for_point(state.pointer_pos.0, state.pointer_pos.1)
            .map(|li| li.surface.wl_surface().clone())
    } else {
        // A mapped Top/Overlay layer surface with
        // KeyboardInteractivity::Exclusive (lock screens, password
        // prompts, app launchers like fuzzel) wins over the WM's
        // focused toplevel. OnDemand layer surfaces still rely on
        // active_surface being set by click-to-focus. None layer
        // surfaces never receive keys.
        state
            .exclusive_keyboard_layer()
            .cloned()
            .or_else(|| state.active_surface.clone())
    };
    let Some(surface) = surface else { return };
    let Some(keyboard) = state.seat.get_keyboard() else {
        return;
    };
    // Only re-issue set_focus when the target actually changed (input audit P0.7).
    let focus = crate::wayland::xwayland::KeyboardFocusTarget::for_wl_surface(state, &surface);
    if !keyboard
        .current_focus()
        .as_ref()
        .is_some_and(|current| current.matches_wl_surface(&surface))
    {
        keyboard.set_focus(state, Some(focus), SERIAL_COUNTER.next_serial());
    }
    let serial = SERIAL_COUNTER.next_serial();
    let time = state.clock.now().as_millis();
    let keycode = Keycode::new(key_event.scancode + 8);
    let ks = if key_event.pressed {
        KeyState::Pressed
    } else {
        KeyState::Released
    };
    keyboard.input_forward(state, keycode, ks, serial, time, false);
}

/// Estimate the height of a Slint context-menu in logical CSS pixels.
/// Item rows are 28 px, separators 7 px, outer chrome 12 px (6 + 6 padding).
pub fn compute_menu_height(items_model: &slint::ModelRc<crate::MenuItem>) -> f64 {
    use slint::Model;
    let n = items_model.row_count();
    let mut h = 12.0_f64; // top + bottom chrome padding
    for i in 0..n {
        if let Some(it) = items_model.row_data(i) {
            h += if it.separator { 7.0 } else { 28.0 };
        }
    }
    h.max(28.0 + 12.0)
}

/// Map an ASCII character to its Linux evdev scancode + whether SHIFT is required.
/// Used by the IPC `TypeText` command. Coverage: a-z, A-Z, 0-9, common punctuation,
/// space, newline, tab. Returns None for chars we don't know how to type.
pub fn ascii_to_scancode(ch: char) -> Option<(u32, bool)> {
    Some(match ch {
        // letters (lowercase + shifted uppercase)
        'a' => (30, false),
        'A' => (30, true),
        'b' => (48, false),
        'B' => (48, true),
        'c' => (46, false),
        'C' => (46, true),
        'd' => (32, false),
        'D' => (32, true),
        'e' => (18, false),
        'E' => (18, true),
        'f' => (33, false),
        'F' => (33, true),
        'g' => (34, false),
        'G' => (34, true),
        'h' => (35, false),
        'H' => (35, true),
        'i' => (23, false),
        'I' => (23, true),
        'j' => (36, false),
        'J' => (36, true),
        'k' => (37, false),
        'K' => (37, true),
        'l' => (38, false),
        'L' => (38, true),
        'm' => (50, false),
        'M' => (50, true),
        'n' => (49, false),
        'N' => (49, true),
        'o' => (24, false),
        'O' => (24, true),
        'p' => (25, false),
        'P' => (25, true),
        'q' => (16, false),
        'Q' => (16, true),
        'r' => (19, false),
        'R' => (19, true),
        's' => (31, false),
        'S' => (31, true),
        't' => (20, false),
        'T' => (20, true),
        'u' => (22, false),
        'U' => (22, true),
        'v' => (47, false),
        'V' => (47, true),
        'w' => (17, false),
        'W' => (17, true),
        'x' => (45, false),
        'X' => (45, true),
        'y' => (21, false),
        'Y' => (21, true),
        'z' => (44, false),
        'Z' => (44, true),
        // digits + shifted symbols (US layout)
        '1' => (2, false),
        '!' => (2, true),
        '2' => (3, false),
        '@' => (3, true),
        '3' => (4, false),
        '#' => (4, true),
        '4' => (5, false),
        '$' => (5, true),
        '5' => (6, false),
        '%' => (6, true),
        '6' => (7, false),
        '^' => (7, true),
        '7' => (8, false),
        '&' => (8, true),
        '8' => (9, false),
        '*' => (9, true),
        '9' => (10, false),
        '(' => (10, true),
        '0' => (11, false),
        ')' => (11, true),
        // punctuation
        '-' => (12, false),
        '_' => (12, true),
        '=' => (13, false),
        '+' => (13, true),
        '[' => (26, false),
        '{' => (26, true),
        ']' => (27, false),
        '}' => (27, true),
        '\\' => (43, false),
        '|' => (43, true),
        ';' => (39, false),
        ':' => (39, true),
        '\'' => (40, false),
        '"' => (40, true),
        '`' => (41, false),
        '~' => (41, true),
        ',' => (51, false),
        '<' => (51, true),
        '.' => (52, false),
        '>' => (52, true),
        '/' => (53, false),
        '?' => (53, true),
        // whitespace
        ' ' => (57, false),  // space
        '\n' => (28, false), // enter
        '\t' => (15, false), // tab
        _ => return None,
    })
}
