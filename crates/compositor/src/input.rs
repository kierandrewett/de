//! Input event routing — translates backend input events into seat actions.
//!
//! Pointer button presses are funnelled through three checks (in order):
//!   1. SSD title-bar drag — left-click on the bar (but not on a button)
//!      starts an interactive move via [`shell::grab`].
//!   2. Focus-on-click — any press on a window/layer-surface raises and
//!      focuses it.
//!   3. Forward to the client — the press is delivered via the seat.
//!
//! When a drag is active, motion events update the window geometry in
//! [`Shell`] and re-`map_element` the smithay `Space` so the renderer
//! sees the new position. The press/release pair that begins/ends a
//! drag is *not* forwarded to the underlying client — the SSD chrome
//! consumes them.

use smithay::backend::input::{
    AbsolutePositionEvent, Axis, ButtonState, Event, InputBackend, InputEvent, KeyState,
    KeyboardKeyEvent, PointerAxisEvent, PointerButtonEvent, PointerMotionEvent,
};
use smithay::desktop::Window;
use smithay::input::keyboard::{xkb, FilterResult};
use smithay::utils::{Logical, Point, Rectangle, SERIAL_COUNTER};

use crate::shell::{grab, snapping::SnapDetector, MonitorInfo};
use crate::state::State;
use crate::wayland::handlers::xdg_shell::ShellWindowId;

/// Linux input event code for the primary mouse button.
const BTN_LEFT: u32 = 0x110;

/// Handle raw input events from the winit backend.
pub fn handle_winit_input<I: InputBackend>(state: &mut State, event: InputEvent<I>) {
    handle_input(state, event);
}

/// Handle raw input events from the libinput backend (udev mode).
pub fn handle_libinput_event<I: InputBackend>(state: &mut State, event: InputEvent<I>) {
    handle_input(state, event);
}

fn handle_input<I: InputBackend>(state: &mut State, event: InputEvent<I>) {
    use InputEvent::*;
    match event {
        Keyboard { event } => {
            let serial = SERIAL_COUNTER.next_serial();
            let time = event.time_msec();
            let key_code = event.key_code();
            let key_state = event.state();

            if let Some(keyboard) = state.common.seat.get_keyboard() {
                // Window-management shortcuts. The closure receives a
                // `&mut State` so we can dispatch into the shell directly
                // and `Intercept` swallows the key so the focused client
                // doesn't also see it.
                keyboard.input::<(), _>(
                    state,
                    key_code,
                    key_state,
                    serial,
                    time,
                    |state, mods, kh| {
                        let sym = kh.modified_sym();
                        // Alt release while a switcher session is open
                        // commits the selection. We watch the keysym
                        // rather than `mods.alt` because the modifier
                        // state already reflects the release by the
                        // time this filter runs.
                        if key_state == KeyState::Released
                            && matches!(
                                sym,
                                xkb::Keysym::Alt_L
                                    | xkb::Keysym::Alt_R
                                    | xkb::Keysym::Meta_L
                                    | xkb::Keysym::Meta_R
                            )
                            && state.common.shell.alt_tab.is_some()
                        {
                            confirm_alt_tab(state);
                            return FilterResult::Intercept(());
                        }
                        if key_state != KeyState::Pressed {
                            return FilterResult::Forward;
                        }
                        if try_window_shortcut(state, mods, sym) {
                            FilterResult::Intercept(())
                        } else {
                            FilterResult::Forward
                        }
                    },
                );
            }
        }

        PointerMotion { event } => {
            let serial = SERIAL_COUNTER.next_serial();
            let pointer = state.common.seat.get_pointer().unwrap();
            let current = pointer.current_location();
            let delta = event.delta();
            let new_pos = current + delta;
            update_active_grab(state, new_pos);
            let under = find_under(state, new_pos);
            pointer.motion(
                state,
                under,
                &smithay::input::pointer::MotionEvent {
                    location: new_pos,
                    serial,
                    time: event.time_msec(),
                },
            );
            pointer.frame(state);
        }

        PointerMotionAbsolute { event } => {
            let serial = SERIAL_COUNTER.next_serial();
            let output = state.common.space.outputs().next().cloned();
            if let Some(output) = output {
                let output_geo = state.common.space.output_geometry(&output).unwrap_or_default();
                let output_size = output_geo.size.to_f64();
                let new_pos = Point::from((
                    event.x_transformed(output_size.w as i32),
                    event.y_transformed(output_size.h as i32),
                )) + output_geo.loc.to_f64();
                update_active_grab(state, new_pos);
                let under = find_under(state, new_pos);
                let pointer = state.common.seat.get_pointer().unwrap();
                pointer.motion(
                    state,
                    under,
                    &smithay::input::pointer::MotionEvent {
                        location: new_pos,
                        serial,
                        time: event.time_msec(),
                    },
                );
                pointer.frame(state);
            }
        }

        PointerButton { event } => {
            let serial = SERIAL_COUNTER.next_serial();
            let pointer = state.common.seat.get_pointer().unwrap();
            let pos = pointer.current_location();
            let pressed = event.state() == ButtonState::Pressed;
            let is_left = event.button_code() == BTN_LEFT;

            if pressed {
                // Title-bar control button — close / minimize / maximize.
                // Checked before the drag region so a click on a button
                // dispatches the action instead of starting a move grab.
                // Action runs through the same animated paths as the IPC
                // handlers, then we swallow the press without forwarding
                // (the client never knew about it).
                if is_left {
                    if let Some((id, btn)) = find_titlebar_button(state, pos) {
                        focus_id(state, id, serial);
                        dispatch_titlebar_button(state, id, btn);
                        return;
                    }
                }
                // Title-bar drag — left-click on the bar (excluding the
                // close/min/max button rects). Initiates a move grab and
                // suppresses the press from reaching any client.
                if is_left {
                    if let Some((window, id)) = find_titlebar_drag_target(state, pos) {
                        focus_window(state, &window, id, serial);
                        let start = (pos.x.round() as i32, pos.y.round() as i32).into();
                        grab::begin_move(
                            &mut state.common.grab,
                            &state.common.shell,
                            id,
                            start,
                        );
                        return;
                    }
                }
                // Focus-on-click — applies to any button so right-click
                // menus etc. also focus their window. A click that
                // lands on empty desktop (no window, no layer) is
                // treated as "deactivate everything" so the chrome
                // re-renders in the unfocused style.
                match find_under(state, pos) {
                    Some((target, _)) => focus_pointer_target(state, target, serial),
                    None => clear_focus(state, serial),
                }
            } else if state.common.grab.active.is_some() {
                // Release ends the drag. Don't forward — the matching
                // press wasn't forwarded either, so the client never
                // knew about this button.
                finish_active_grab(state, pos);
                return;
            }

            pointer.button(
                state,
                &smithay::input::pointer::ButtonEvent {
                    serial,
                    time: event.time_msec(),
                    button: event.button_code(),
                    state: event.state(),
                },
            );
            pointer.frame(state);
        }

        PointerAxis { event } => {
            let source = event.source();
            let h = event.amount(Axis::Horizontal).unwrap_or(0.0);
            let v = event.amount(Axis::Vertical).unwrap_or(0.0);
            let pointer = state.common.seat.get_pointer().unwrap();
            let mut frame = smithay::input::pointer::AxisFrame::new(event.time_msec())
                .source(source);
            if h != 0.0 {
                frame = frame.value(Axis::Horizontal, h);
            }
            if v != 0.0 {
                frame = frame.value(Axis::Vertical, v);
            }
            pointer.axis(state, frame);
            pointer.frame(state);
        }

        _ => {}
    }
}

/// Find the surface and local offset under the given compositor-space point.
fn find_under(
    state: &State,
    point: Point<f64, Logical>,
) -> Option<(crate::focus::PointerFocusTarget, Point<f64, Logical>)> {
    use smithay::desktop::layer_map_for_output;
    use smithay::wayland::shell::wlr_layer::Layer as WlrLayer;

    // Check overlay/top layer shell surfaces first (panels, docks — on top of windows).
    // The second tuple element returned here is the absolute compositor-space
    // position of the surface origin — smithay's pointer machinery
    // subtracts it from the cursor location to derive the surface-local
    // coordinate it sends to the client. Passing the local offset
    // instead (which we tried before) makes iced think every event is
    // at (0,0) and no button hit-tests pass.
    for output in state.common.space.outputs() {
        let map = layer_map_for_output(output);
        for layer in [WlrLayer::Overlay, WlrLayer::Top] {
            if let Some(l) = map.layer_under(layer, point) {
                let geo = map.layer_geometry(l).unwrap_or_default();
                return Some((
                    crate::focus::PointerFocusTarget::LayerSurface(l.clone()),
                    geo.loc.to_f64(),
                ));
            }
        }
    }

    // Then regular windows.
    if let Some((window, offset)) = state.common.space.element_under(point) {
        return Some((
            crate::focus::PointerFocusTarget::Window(Box::new(window.clone())),
            offset.to_f64(),
        ));
    }

    // Then bottom/background layer surfaces — same coordinate convention.
    for output in state.common.space.outputs() {
        let map = layer_map_for_output(output);
        for layer in [WlrLayer::Bottom, WlrLayer::Background] {
            if let Some(l) = map.layer_under(layer, point) {
                let geo = map.layer_geometry(l).unwrap_or_default();
                return Some((
                    crate::focus::PointerFocusTarget::LayerSurface(l.clone()),
                    geo.loc.to_f64(),
                ));
            }
        }
    }

    None
}

// ---------------------------------------------------------------------------
// Title-bar drag
// ---------------------------------------------------------------------------

/// Hit-test SSD title bars under `pos`. Returns the (window, shell id)
/// pair to drag, or `None` if the pointer isn't on a draggable title-bar
/// region (i.e. it's on a control button, on a CSD window, or on
/// content). Iterates topmost-first so overlapping bars resolve in
/// stacking order.
fn find_titlebar_drag_target(
    state: &State,
    pos: Point<f64, Logical>,
) -> Option<(Window, u64)> {
    // `space.elements()` is back-to-front; reverse so we hit the topmost
    // window first under overlapping bars.
    let elements: Vec<Window> = state.common.space.elements().rev().cloned().collect();
    for window in elements {
        if !crate::shell::is_ssd(&window) {
            continue;
        }
        let geo = match state.common.space.element_geometry(&window) {
            Some(g) => g,
            None => continue,
        };
        let chrome = crate::shell::title_bar_chrome(geo);
        if !chrome.bar.to_f64().contains(pos) {
            continue;
        }
        // Click is in the bar — but exclude the control buttons so they
        // can still register their own clicks via the dedicated handler.
        let on_button = [chrome.close, chrome.minimize, chrome.maximize]
            .iter()
            .any(|b| b.to_f64().contains(pos));
        if on_button {
            return None;
        }
        let id = window.user_data().get::<ShellWindowId>().map(|s| s.0)?;
        return Some((window, id));
    }
    None
}

// ---------------------------------------------------------------------------
// Focus
// ---------------------------------------------------------------------------

/// Focus the given window everywhere — keyboard focus, shell focus
/// stack, smithay Z-order, and a single IPC broadcast for the panel/dock.
fn focus_window(
    state: &mut State,
    window: &Window,
    id: u64,
    serial: smithay::utils::Serial,
) {
    use crate::focus::KeyboardFocusTarget;
    let target = KeyboardFocusTarget::Window(Box::new(window.clone()));
    if let Some(kb) = state.common.seat.get_keyboard() {
        kb.set_focus(state, Some(target), serial);
    }
    apply_shell_focus(state, window, id);
}

/// Focus whatever's under the pointer (a [`PointerFocusTarget`]). For
/// window targets this updates shell focus + Z-order; layer surfaces
/// only get keyboard focus.
fn focus_pointer_target(
    state: &mut State,
    target: crate::focus::PointerFocusTarget,
    serial: smithay::utils::Serial,
) {
    use crate::focus::{KeyboardFocusTarget, PointerFocusTarget};

    match target {
        PointerFocusTarget::Window(window) => {
            let id = window.user_data().get::<ShellWindowId>().map(|s| s.0);
            let kb_target = KeyboardFocusTarget::Window(window.clone());
            if let Some(kb) = state.common.seat.get_keyboard() {
                kb.set_focus(state, Some(kb_target), serial);
            }
            if let Some(id) = id {
                apply_shell_focus(state, &window, id);
            }
        }
        PointerFocusTarget::LayerSurface(layer) => {
            let kb_target = KeyboardFocusTarget::LayerSurface(layer);
            if let Some(kb) = state.common.seat.get_keyboard() {
                kb.set_focus(state, Some(kb_target), serial);
            }
        }
    }
}

/// Drop keyboard focus and mark "no window focused" in the shell,
/// then broadcast the change so the panel/dock and the SSD chrome
/// pick up the deactivated state.
fn clear_focus(state: &mut State, serial: smithay::utils::Serial) {
    if state.common.shell.focused_window_id().is_none() {
        return;
    }
    if let Some(kb) = state.common.seat.get_keyboard() {
        kb.set_focus(state, None, serial);
    }
    // Drive every window's focus spring to 0 — without this the
    // chrome stays at its previous focus_amount and the window keeps
    // rendering in the active style after clicking off it.
    state.common.shell.unfocus_all_animate();
    state
        .common
        .ipc
        .broadcast(&ipc::ShellEvent::FocusedWindowChanged { window: None });
}

fn apply_shell_focus(state: &mut State, window: &Window, id: u64) {
    if state.common.shell.focused_window_id() == Some(id) {
        return;
    }
    state.common.shell.focus_window(id);
    state.common.space.raise_element(window, true);
    let info = state.common.shell.window_info(id);
    state
        .common
        .ipc
        .broadcast(&ipc::ShellEvent::FocusedWindowChanged { window: info });
}

// ---------------------------------------------------------------------------
// Move grab
// ---------------------------------------------------------------------------

/// Build a [`MonitorInfo`] for the output containing `pos`. We skip
/// `shell.monitors` (which is currently only populated by tests) and
/// derive directly from smithay's `Space`. Panel/dock heights are 0
/// here — clamping during drag still works because the underlying snap
/// detector only needs the work area.
fn current_monitor(
    state: &State,
    pos: Point<f64, Logical>,
) -> Option<MonitorInfo> {
    for output in state.common.space.outputs() {
        let geo = state.common.space.output_geometry(output)?;
        if geo.to_f64().contains(pos) {
            return Some(MonitorInfo {
                name: output.name(),
                logical_rect: geo,
                panel_height: 0,
                dock_height: 0,
            });
        }
    }
    None
}

/// Tick the active grab with the latest pointer position, then mirror
/// the resulting `MappedWindow.geometry` back into smithay's `Space`
/// (which is what the render path actually reads).
fn update_active_grab(state: &mut State, pos: Point<f64, Logical>) {
    if state.common.grab.active.is_none() {
        return;
    }
    let monitor = match current_monitor(state, pos) {
        Some(m) => m,
        None => return,
    };
    // FancyZones: Alt held during a move drag enables zone snapping.
    // Without it `SnapDetector` only resolves edge halves / corners.
    let zone_held = state
        .common
        .seat
        .get_keyboard()
        .map(|kb| kb.modifier_state().alt)
        .unwrap_or(false);
    if let Some(active) = state.common.grab.active.as_mut() {
        active.zone_modifier_held = zone_held;
    }
    let detector = SnapDetector::new();
    let pt = (pos.x.round() as i32, pos.y.round() as i32).into();
    grab::update(
        &mut state.common.grab,
        &mut state.common.shell,
        pt,
        &monitor,
        &detector,
    );
    sync_grabbed_window_to_space(state);
}

/// Finish the active grab. Same `Space` sync as [`update_active_grab`]
/// so any final clamp/snap from `grab::finish` is reflected.
fn finish_active_grab(state: &mut State, pos: Point<f64, Logical>) {
    let monitor = match current_monitor(state, pos) {
        Some(m) => m,
        None => return,
    };
    // Snapshot the id before finish() takes the grab.
    let id = state.common.grab.active.as_ref().map(|g| g.window_id);
    let _snap = grab::finish(&mut state.common.grab, &mut state.common.shell, &monitor);
    if let Some(id) = id {
        sync_window_to_space(state, id);
    }
}

fn sync_grabbed_window_to_space(state: &mut State) {
    let id = match state.common.grab.active.as_ref() {
        Some(g) => g.window_id,
        None => return,
    };
    sync_window_to_space(state, id);
}

fn sync_window_to_space(state: &mut State, id: u64) {
    let new_loc = match state.common.shell.window(id) {
        Some(w) => w.geometry.loc,
        None => return,
    };
    let win = state
        .common
        .space
        .elements()
        .find(|w| w.user_data().get::<ShellWindowId>().map(|s| s.0) == Some(id))
        .cloned();
    if let Some(win) = win {
        // `activate=false` so we don't reorder Z-stacking on every motion
        // tick — focus already raised the window when the drag began.
        state.common.space.map_element(win, new_loc, false);
    }
}

// ---------------------------------------------------------------------------
// Title-bar control buttons
// ---------------------------------------------------------------------------

/// Which title-bar control button got clicked.
#[derive(Debug, Clone, Copy)]
enum TitleBarButton {
    Close,
    Minimize,
    Maximize,
}

/// Hit-test the per-window control buttons under `pos`. Iterates
/// topmost-first so overlapping bars resolve in stacking order. Mirrors
/// [`find_titlebar_drag_target`] but returns the button kind instead of
/// the draggable window pair.
fn find_titlebar_button(
    state: &State,
    pos: Point<f64, Logical>,
) -> Option<(u64, TitleBarButton)> {
    let elements: Vec<Window> = state.common.space.elements().rev().cloned().collect();
    for window in elements {
        if !crate::shell::is_ssd(&window) {
            continue;
        }
        let geo = state.common.space.element_geometry(&window)?;
        let chrome = crate::shell::title_bar_chrome(geo);
        if !chrome.bar.to_f64().contains(pos) {
            continue;
        }
        let id = window.user_data().get::<ShellWindowId>().map(|s| s.0)?;
        if chrome.close.to_f64().contains(pos) {
            return Some((id, TitleBarButton::Close));
        }
        if chrome.minimize.to_f64().contains(pos) {
            return Some((id, TitleBarButton::Minimize));
        }
        if chrome.maximize.to_f64().contains(pos) {
            return Some((id, TitleBarButton::Maximize));
        }
        return None;
    }
    None
}

/// Execute the action for a title-bar control button. Goes through the
/// same animated shell paths as IPC so behaviour is identical regardless
/// of how the action was triggered.
fn dispatch_titlebar_button(state: &mut State, id: u64, button: TitleBarButton) {
    match button {
        TitleBarButton::Close => close_window(state, id),
        TitleBarButton::Minimize => minimize_window(state, id),
        TitleBarButton::Maximize => toggle_maximize(state, id),
    }
}

/// Build a `MonitorInfo` for the output containing the window. Falls
/// back to the first output if the window's centre isn't on any output.
/// Panel/dock heights are 0 here — no protocol is currently exposing
/// reserved space, so the work area equals the output rect.
fn monitor_for_window(state: &State, id: u64) -> Option<MonitorInfo> {
    let centre = {
        let g = state.common.shell.window(id)?.geometry;
        Point::<i32, Logical>::from((g.loc.x + g.size.w / 2, g.loc.y + g.size.h / 2))
    };
    let mut fallback: Option<MonitorInfo> = None;
    for output in state.common.space.outputs() {
        let geo = state.common.space.output_geometry(output)?;
        let info = MonitorInfo {
            name: output.name(),
            logical_rect: geo,
            panel_height: 0,
            dock_height: 0,
        };
        if geo.contains(centre) {
            return Some(info);
        }
        fallback.get_or_insert(info);
    }
    fallback
}

/// Close action — politely asks the client to close, then starts the
/// fade-out. Same path as `ipc::CloseWindow`.
fn close_window(state: &mut State, id: u64) {
    let win = state
        .common
        .space
        .elements()
        .find(|w| w.user_data().get::<ShellWindowId>().map(|s| s.0) == Some(id))
        .cloned();
    if let Some(window) = win {
        if let smithay::desktop::WindowSurface::Wayland(toplevel) = window.underlying_surface() {
            toplevel.send_close();
        }
    }
    state.common.shell.begin_close(id);
}

/// Minimize action — drives the spring animation toward the dock icon
/// rect (queried from the dock; falls back to a centred 80×80 rect at
/// the bottom of the monitor if no dock is attached).
fn minimize_window(state: &mut State, id: u64) {
    let dock_rect = monitor_for_window(state, id).map(|m| {
        let work = m.logical_rect;
        ipc::Rect {
            x: work.loc.x + work.size.w / 2 - 40,
            y: work.loc.y + work.size.h - 80,
            w: 80,
            h: 80,
        }
    });
    crate::shell::minimize::minimize_window(&mut state.common.shell, id, dock_rect);
    broadcast_window_state(state, id);
}

/// Toggle maximized state for a window. Saves the pre-maximize rect on
/// the way in and springs back to it on the way out.
fn toggle_maximize(state: &mut State, id: u64) {
    let monitor = match monitor_for_window(state, id) {
        Some(m) => m,
        None => return,
    };
    let was_maximized = state
        .common
        .shell
        .window(id)
        .map(|w| w.is_maximized)
        .unwrap_or(false);
    if was_maximized {
        crate::shell::maximize::unmaximize_window(&mut state.common.shell, id);
    } else {
        crate::shell::maximize::maximize_window(&mut state.common.shell, id, &monitor);
    }
    sync_window_geometry_to_space(state, id);
    propagate_size_to_client(state, id);
    broadcast_window_state(state, id);
}

/// Push the latest `MappedWindow.geometry` (loc + size) into smithay's
/// `Space`. Used after a maximize / unmaximize where both axes change.
fn sync_window_geometry_to_space(state: &mut State, id: u64) {
    let geo = match state.common.shell.window(id) {
        Some(w) => w.geometry,
        None => return,
    };
    let win = state
        .common
        .space
        .elements()
        .find(|w| w.user_data().get::<ShellWindowId>().map(|s| s.0) == Some(id))
        .cloned();
    if let Some(win) = win {
        state.common.space.map_element(win, geo.loc, false);
    }
    let _ = Rectangle::<i32, Logical>::default();
}

/// Tell the wayland client to resize itself to match `MappedWindow.geometry.size`.
fn propagate_size_to_client(state: &mut State, id: u64) {
    let size = match state.common.shell.window(id) {
        Some(w) => w.geometry.size,
        None => return,
    };
    let win = state
        .common
        .space
        .elements()
        .find(|w| w.user_data().get::<ShellWindowId>().map(|s| s.0) == Some(id))
        .cloned();
    if let Some(win) = win {
        if let smithay::desktop::WindowSurface::Wayland(toplevel) = win.underlying_surface() {
            toplevel.with_pending_state(|s| s.size = Some(size));
            toplevel.send_pending_configure();
        }
    }
}

fn broadcast_window_state(state: &mut State, id: u64) {
    if let Some(info) = state.common.shell.window_info(id) {
        state.common.ipc.broadcast(&ipc::ShellEvent::WindowStateChanged {
            window_id: info.id,
            state: info.state,
        });
    }
}

/// Focus a window by id without needing the smithay `Window` handle.
/// Used by the title-bar button handler so a click on a control still
/// raises and focuses its window.
fn focus_id(state: &mut State, id: u64, serial: smithay::utils::Serial) {
    let win = state
        .common
        .space
        .elements()
        .find(|w| w.user_data().get::<ShellWindowId>().map(|s| s.0) == Some(id))
        .cloned();
    if let Some(win) = win {
        focus_window(state, &win, id, serial);
    }
}

// ---------------------------------------------------------------------------
// Keyboard shortcuts
// ---------------------------------------------------------------------------

/// Try to handle a key press as a window-management shortcut. Returns
/// `true` if the event was consumed (i.e. should not reach any client).
fn try_window_shortcut(
    state: &mut State,
    mods: &smithay::input::keyboard::ModifiersState,
    sym: xkb::Keysym,
) -> bool {
    let logo = mods.logo;
    let alt = mods.alt;
    // Super+Up — toggle maximize for the focused window.
    if logo && sym == xkb::Keysym::Up {
        if let Some(id) = state.common.shell.focused_window_id() {
            toggle_maximize(state, id);
        }
        return true;
    }
    // Super+H — minimize the focused window.
    if logo && (sym == xkb::Keysym::h || sym == xkb::Keysym::H) {
        if let Some(id) = state.common.shell.focused_window_id() {
            minimize_window(state, id);
        }
        return true;
    }
    // Super+W — politely close the focused window.
    if logo && (sym == xkb::Keysym::w || sym == xkb::Keysym::W) {
        if let Some(id) = state.common.shell.focused_window_id() {
            close_window(state, id);
        }
        return true;
    }
    // Alt+Tab — held-modifier switcher. First press opens the
    // AltTabState (snapshots the focus stack); subsequent presses
    // while Alt is held cycle the selected entry without committing
    // focus. Confirm fires on Alt release; Escape cancels.
    if alt && sym == xkb::Keysym::Tab {
        alt_tab_step(state, false);
        return true;
    }
    if alt && sym == xkb::Keysym::ISO_Left_Tab {
        alt_tab_step(state, true);
        return true;
    }
    // Escape while a switcher session is open cancels — focus stays
    // on whatever was active before alt-tab opened.
    if state.common.shell.alt_tab.is_some() && sym == xkb::Keysym::Escape {
        crate::shell::alt_tab::cancel(&mut state.common.shell);
        return true;
    }
    false
}

/// Step the Alt+Tab switcher. Opens a new session on first press,
/// then cycles selection on each subsequent press while Alt is held.
fn alt_tab_step(state: &mut State, reverse: bool) {
    if state.common.shell.alt_tab.is_none() {
        crate::shell::alt_tab::begin(&mut state.common.shell);
    }
    if reverse {
        crate::shell::alt_tab::prev(&mut state.common.shell);
    } else {
        crate::shell::alt_tab::next(&mut state.common.shell);
    }
}

/// Confirm the Alt+Tab switcher selection — called on Alt release.
/// Focuses the selected window via the same path as a normal click,
/// so chrome animation, focus stack, and the IPC broadcast all fire.
fn confirm_alt_tab(state: &mut State) {
    let chosen = crate::shell::alt_tab::confirm(&mut state.common.shell);
    let Some(id) = chosen else { return };
    let serial = SERIAL_COUNTER.next_serial();
    focus_id(state, id, serial);
}

