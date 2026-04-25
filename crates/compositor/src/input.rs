//! Input event routing — translates backend input events into seat actions.

use smithay::backend::input::{
    AbsolutePositionEvent, Axis, ButtonState, Event, InputBackend, InputEvent,
    KeyboardKeyEvent, PointerAxisEvent, PointerButtonEvent, PointerMotionEvent,
};

use crate::state::State;

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
            let serial = smithay::utils::SERIAL_COUNTER.next_serial();
            let time = event.time_msec();
            let key_code = event.key_code();
            let key_state = event.state();

            if let Some(keyboard) = state.common.seat.get_keyboard() {
                keyboard.input::<(), _>(
                    state,
                    key_code,
                    key_state,
                    serial,
                    time,
                    |_, _, _| smithay::input::keyboard::FilterResult::Forward,
                );
            }
        }

        PointerMotion { event } => {
            let serial = smithay::utils::SERIAL_COUNTER.next_serial();
            let pointer = state.common.seat.get_pointer().unwrap();
            let current = pointer.current_location();
            let delta = event.delta();
            let new_pos = current + delta;
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
            let serial = smithay::utils::SERIAL_COUNTER.next_serial();
            let output = state.common.space.outputs().next().cloned();
            if let Some(output) = output {
                let output_geo = state.common.space.output_geometry(&output).unwrap_or_default();
                let output_size = output_geo.size.to_f64();
                let new_pos = smithay::utils::Point::from((
                    event.x_transformed(output_size.w as i32),
                    event.y_transformed(output_size.h as i32),
                )) + output_geo.loc.to_f64();
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
            let serial = smithay::utils::SERIAL_COUNTER.next_serial();
            let pointer = state.common.seat.get_pointer().unwrap();
            let pos = pointer.current_location();
            let under = find_under(state, pos);

            // Focus window on click.
            if event.state() == ButtonState::Pressed {
                if let Some((target, _)) = &under {
                    let kb_target: Option<crate::focus::KeyboardFocusTarget> = match target {
                        crate::focus::PointerFocusTarget::Window(w) => {
                            Some(crate::focus::KeyboardFocusTarget::Window(w.clone()))
                        }
                        crate::focus::PointerFocusTarget::LayerSurface(l) => {
                            Some(crate::focus::KeyboardFocusTarget::LayerSurface(l.clone()))
                        }
                    };
                    if let (Some(kb), Some(kb_target)) =
                        (state.common.seat.get_keyboard(), kb_target)
                    {
                        kb.set_focus(state, Some(kb_target), serial);
                    }
                }
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
    point: smithay::utils::Point<f64, smithay::utils::Logical>,
) -> Option<(crate::focus::PointerFocusTarget, smithay::utils::Point<f64, smithay::utils::Logical>)>
{
    use smithay::desktop::layer_map_for_output;
    use smithay::wayland::shell::wlr_layer::Layer as WlrLayer;

    // Check overlay/top layer shell surfaces first (panels, docks — on top of windows).
    for output in state.common.space.outputs() {
        let map = layer_map_for_output(output);
        for layer in [WlrLayer::Overlay, WlrLayer::Top] {
            if let Some(l) = map.layer_under(layer, point) {
                return Some((
                    crate::focus::PointerFocusTarget::LayerSurface(l.clone()),
                    point,
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

    // Then bottom/background layer surfaces.
    for output in state.common.space.outputs() {
        let map = layer_map_for_output(output);
        for layer in [WlrLayer::Bottom, WlrLayer::Background] {
            if let Some(l) = map.layer_under(layer, point) {
                return Some((
                    crate::focus::PointerFocusTarget::LayerSurface(l.clone()),
                    point,
                ));
            }
        }
    }

    None
}
