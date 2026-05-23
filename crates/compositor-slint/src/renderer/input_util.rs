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

use smithay::{
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    utils::{Logical, Point, SERIAL_COUNTER},
    wayland::keyboard_shortcuts_inhibit::KeyboardShortcutsInhibitorSeat,
};
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

pub type SurfaceHit = Option<(WlSurface, f64, f64)>;

pub fn keyboard_shortcuts_inhibited(state: &SpikeState) -> bool {
    let focused_surface = state
        .seat
        .get_keyboard()
        .and_then(|keyboard| keyboard.current_focus())
        .and_then(|focus| focus.wl_surface().map(|surface| surface.into_owned()))
        .or_else(|| state.active_surface.clone());

    focused_surface.is_some_and(|surface| {
        state
            .seat
            .keyboard_shortcuts_inhibitor_for_surface(&surface)
            .map(|inhibitor| inhibitor.is_active())
            .unwrap_or(false)
    })
}

pub fn forward_keyboard_event(state: &mut SpikeState, key_event: PendingKeyEvent) {
    use smithay::input::keyboard::Keycode;

    let keycode = Keycode::new(key_event.scancode + 8);
    forward_keyboard_keycode(
        state,
        keycode,
        key_event.pressed,
        SERIAL_COUNTER.next_serial(),
        state.clock.now().as_millis(),
    );
}

pub fn forward_keyboard_keycode(
    state: &mut SpikeState,
    keycode: smithay::input::keyboard::Keycode,
    pressed: bool,
    serial: smithay::utils::Serial,
    time: u32,
) {
    use smithay::backend::input::KeyState;

    // Reset idle timer on keyboard input — fixes the screen-locks-while-typing
    // bug called out in the input audit. See backend/udev.rs handle_libinput_input_event.
    state.idle_notifier_state.notify_activity(&state.seat);

    // Session-lock gate: keys go ONLY to the lock surface. Without this,
    // the user could keep typing into a focused toplevel that's hidden
    // behind the lock — a textbook lock-screen bypass.
    let surface: Option<WlSurface> = if state.session_locked {
        state
            .lock_surfaces
            .first()
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
    let ks = if pressed {
        KeyState::Pressed
    } else {
        KeyState::Released
    };
    keyboard.input_forward(state, keycode, ks, serial, time, false);
}

pub fn forward_pointer_motion<F>(
    state: &mut SpikeState,
    x: f64,
    y: f64,
    delta_unaccel: Option<(f64, f64)>,
    time: Option<u32>,
    mut surface_under: F,
) where
    F: FnMut(&SpikeState, f64, f64) -> SurfaceHit,
{
    use smithay::input::pointer::{MotionEvent, RelativeMotionEvent};
    use smithay::wayland::pointer_constraints::{with_pointer_constraint, PointerConstraint};

    let Some(pointer) = state.seat.get_pointer() else {
        return;
    };

    state.idle_notifier_state.notify_activity(&state.seat);

    let serial = SERIAL_COUNTER.next_serial();
    let time = time.unwrap_or_else(|| state.clock.now().as_millis());
    let (px, py) = state.pointer_pos;
    let delta_x = x - px;
    let delta_y = y - py;
    let current_hit = surface_under(state, px, py);
    let hit = surface_under(state, x, y);

    let mut pointer_locked = false;
    let mut pointer_confined = false;
    let mut confine_region: Option<smithay::wayland::compositor::RegionAttributes> = None;
    if let Some((surface, origin_x, origin_y)) = current_hit.as_ref() {
        with_pointer_constraint(surface, &pointer, |constraint| match constraint {
            Some(c) if c.is_active() => {
                let local = ((px - origin_x) as i32, (py - origin_y) as i32);
                if !c.region().is_none_or(|r| r.contains(Point::from(local))) {
                    return;
                }
                match &*c {
                    PointerConstraint::Locked(_) => pointer_locked = true,
                    PointerConstraint::Confined(confined) => {
                        pointer_confined = true;
                        confine_region = confined.region().cloned();
                    }
                }
            }
            _ => {}
        });
    }

    let (delta_unaccel_x, delta_unaccel_y) = delta_unaccel.unwrap_or((delta_x, delta_y));
    pointer.relative_motion(
        state,
        current_hit
            .clone()
            .map(|(surface, origin_x, origin_y)| (surface, Point::from((origin_x, origin_y)))),
        &RelativeMotionEvent {
            delta: Point::from((delta_x, delta_y)),
            delta_unaccel: Point::from((delta_unaccel_x, delta_unaccel_y)),
            utime: (u64::from(time)) * 1000,
        },
    );

    if pointer_locked {
        pointer.frame(state);
        return;
    }

    if pointer_confined {
        if let Some((focus_surface, origin_x, origin_y)) = current_hit.as_ref() {
            let crossed_surface = hit
                .as_ref()
                .map(|(surface, _, _)| surface != focus_surface)
                .unwrap_or(true);
            let out_of_region = confine_region.as_ref().is_some_and(|region| {
                let local = ((x - origin_x) as i32, (y - origin_y) as i32);
                !region.contains(Point::from(local))
            });
            if crossed_surface || out_of_region {
                pointer.frame(state);
                return;
            }
        }
    }

    state.pointer_pos = (x, y);
    if let Some((surface, origin_x, origin_y)) = hit {
        with_pointer_constraint(&surface, &pointer, |constraint| match constraint {
            Some(c) if !c.is_active() => {
                let local = ((x - origin_x) as i32, (y - origin_y) as i32);
                if c.region().is_none_or(|r| r.contains(Point::from(local))) {
                    c.activate();
                }
            }
            _ => {}
        });
        pointer.motion(
            state,
            Some((surface, Point::from((origin_x, origin_y)))),
            &MotionEvent {
                location: Point::from((x, y)),
                serial,
                time,
            },
        );
    } else {
        pointer.motion(
            state,
            None,
            &MotionEvent {
                location: Point::from((x, y)),
                serial,
                time,
            },
        );
    }
    pointer.frame(state);
}

pub fn forward_touch_down<F>(
    state: &mut SpikeState,
    slot: smithay::backend::input::TouchSlot,
    x: f64,
    y: f64,
    time: u32,
    mut surface_under: F,
) where
    F: FnMut(&SpikeState, f64, f64) -> SurfaceHit,
{
    let Some(touch) = state.seat.get_touch() else {
        return;
    };

    state.idle_notifier_state.notify_activity(&state.seat);

    let serial = SERIAL_COUNTER.next_serial();
    let under = surface_under(state, x, y);
    if let Some((surface, origin_x, origin_y)) = under.as_ref() {
        state
            .touch_focus
            .insert(slot, (surface.clone(), *origin_x, *origin_y));
        if let Some(keyboard) = state.seat.get_keyboard() {
            let focus =
                crate::wayland::xwayland::KeyboardFocusTarget::for_wl_surface(state, surface);
            keyboard.set_focus(state, Some(focus), serial);
        }
    } else {
        state.touch_focus.remove(&slot);
    }

    touch.down(
        state,
        under.map(|(surface, origin_x, origin_y)| (surface, Point::from((origin_x, origin_y)))),
        &smithay::input::touch::DownEvent {
            slot,
            location: Point::from((x, y)),
            serial,
            time,
        },
    );
}

pub fn forward_touch_motion(
    state: &mut SpikeState,
    slot: smithay::backend::input::TouchSlot,
    x: f64,
    y: f64,
    time: u32,
) {
    let Some(touch) = state.seat.get_touch() else {
        return;
    };

    state.idle_notifier_state.notify_activity(&state.seat);
    let under = state
        .touch_focus
        .get(&slot)
        .cloned()
        .map(|(surface, origin_x, origin_y)| (surface, Point::from((origin_x, origin_y))));
    touch.motion(
        state,
        under,
        &smithay::input::touch::MotionEvent {
            slot,
            location: Point::from((x, y)),
            time,
        },
    );
}

pub fn forward_touch_up(
    state: &mut SpikeState,
    slot: smithay::backend::input::TouchSlot,
    time: u32,
) {
    let Some(touch) = state.seat.get_touch() else {
        return;
    };

    state.idle_notifier_state.notify_activity(&state.seat);
    state.touch_focus.remove(&slot);
    touch.up(
        state,
        &smithay::input::touch::UpEvent {
            slot,
            serial: SERIAL_COUNTER.next_serial(),
            time,
        },
    );
}

pub fn forward_touch_cancel(state: &mut SpikeState) {
    let Some(touch) = state.seat.get_touch() else {
        return;
    };

    state.idle_notifier_state.notify_activity(&state.seat);
    state.touch_focus.clear();
    touch.cancel(state);
}

pub fn forward_touch_frame(state: &mut SpikeState) {
    if let Some(touch) = state.seat.get_touch() {
        touch.frame(state);
    }
}

pub fn state_surface_under(state: &SpikeState, x: f64, y: f64) -> SurfaceHit {
    use smithay::wayland::shell::wlr_layer::Layer;

    if state.session_locked {
        return state
            .lock_surfaces
            .first()
            .map(|lock| (lock.surface.wl_surface().clone(), 0.0, 0.0));
    }

    state_popup_surface_under(state, x, y)
        .or_else(|| state_layer_surface_under(state, x, y, &[Layer::Overlay, Layer::Top]))
        .or_else(|| state_toplevel_surface_under(state, x, y))
        .or_else(|| state_layer_surface_under(state, x, y, &[Layer::Bottom, Layer::Background]))
}

fn state_layer_surface_under(
    state: &SpikeState,
    x: f64,
    y: f64,
    layers: &[smithay::wayland::shell::wlr_layer::Layer],
) -> SurfaceHit {
    use smithay::desktop::utils::under_from_surface_tree;
    use smithay::desktop::WindowSurfaceType;

    for &want in layers {
        for layer in &state.layer_surfaces {
            if layer.layer != want || layer.w <= 0 || layer.h <= 0 {
                continue;
            }
            let lx = x - f64::from(layer.x);
            let ly = y - f64::from(layer.y);
            if lx < 0.0 || ly < 0.0 || lx >= f64::from(layer.w) || ly >= f64::from(layer.h) {
                continue;
            }
            let origin = Point::<i32, Logical>::from((layer.x, layer.y));
            if let Some((surface, surface_origin)) = under_from_surface_tree(
                layer.surface.wl_surface(),
                Point::from((x, y)),
                origin,
                WindowSurfaceType::ALL,
            ) {
                return Some((
                    surface,
                    f64::from(surface_origin.x),
                    f64::from(surface_origin.y),
                ));
            }
            return Some((
                layer.surface.wl_surface().clone(),
                f64::from(layer.x),
                f64::from(layer.y),
            ));
        }
    }

    None
}

fn state_popup_surface_under(state: &SpikeState, x: f64, y: f64) -> SurfaceHit {
    use smithay::desktop::utils::under_from_surface_tree;
    use smithay::desktop::WindowSurfaceType;

    for popup in state.popups.iter().rev() {
        let Some((px, py, pw, ph)) = state_popup_rect(state, popup) else {
            continue;
        };
        if x < px || x >= px + pw || y < py || y >= py + ph {
            continue;
        }
        let origin = Point::<i32, Logical>::from((px as i32, py as i32));
        if let Some((surface, surface_origin)) = under_from_surface_tree(
            &popup.surface,
            Point::from((x, y)),
            origin,
            WindowSurfaceType::ALL,
        ) {
            return Some((
                surface,
                f64::from(surface_origin.x),
                f64::from(surface_origin.y),
            ));
        }
        return Some((popup.surface.clone(), px, py));
    }

    None
}

fn state_popup_rect(
    state: &SpikeState,
    popup: &crate::wayland_state::PopupInfo,
) -> Option<(f64, f64, f64, f64)> {
    let (buffer_w, buffer_h) = {
        let pixels = popup.pixels.lock().ok()?;
        (pixels.width as i32, pixels.height as i32)
    };
    let geometry = popup.configured_geometry()?;
    let width = if geometry.size.w > 0 {
        geometry.size.w
    } else {
        buffer_w
    };
    let height = if geometry.size.h > 0 {
        geometry.size.h
    } else {
        buffer_h
    };
    if width <= 0 || height <= 0 {
        return None;
    }

    let mut abs_x = geometry.loc.x;
    let mut abs_y = geometry.loc.y;
    let mut parent = popup.parent.clone();
    for _ in 0..16 {
        if let Some(parent_popup) = state
            .popups
            .iter()
            .find(|candidate| candidate.surface == parent)
        {
            let parent_geometry = parent_popup.configured_geometry()?;
            abs_x += parent_geometry.loc.x;
            abs_y += parent_geometry.loc.y;
            parent = parent_popup.parent.clone();
            continue;
        }
        if let Some(toplevel) = state
            .toplevels
            .iter()
            .find(|candidate| candidate.surface == parent)
        {
            abs_x += toplevel.x;
            abs_y += toplevel.y;
            return Some((
                f64::from(abs_x),
                f64::from(abs_y),
                f64::from(width),
                f64::from(height),
            ));
        }
        if let Some(layer) = state
            .layer_surfaces
            .iter()
            .find(|layer| layer.surface.wl_surface() == &parent)
        {
            abs_x += layer.x;
            abs_y += layer.y;
            return Some((
                f64::from(abs_x),
                f64::from(abs_y),
                f64::from(width),
                f64::from(height),
            ));
        }
        return None;
    }

    None
}

fn state_toplevel_surface_under(state: &SpikeState, x: f64, y: f64) -> SurfaceHit {
    use smithay::desktop::utils::under_from_surface_tree;
    use smithay::desktop::WindowSurfaceType;

    for toplevel in state.toplevels.iter().rev() {
        let (width, height) = {
            let pixels = toplevel.pixels.lock().ok()?;
            (pixels.width.max(1) as f64, pixels.height.max(1) as f64)
        };
        let origin_x = f64::from(toplevel.x);
        let origin_y = f64::from(toplevel.y);
        if x < origin_x || x >= origin_x + width || y < origin_y || y >= origin_y + height {
            continue;
        }
        let origin = Point::<i32, Logical>::from((toplevel.x, toplevel.y));
        if let Some((surface, surface_origin)) = under_from_surface_tree(
            &toplevel.surface,
            Point::from((x, y)),
            origin,
            WindowSurfaceType::ALL,
        ) {
            return Some((
                surface,
                f64::from(surface_origin.x),
                f64::from(surface_origin.y),
            ));
        }
        return Some((toplevel.surface.clone(), origin_x, origin_y));
    }

    None
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
