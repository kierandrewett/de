//! Winit dev backend — runs the compositor as a nested window inside an existing display.

use std::sync::Arc;
use std::time::Duration;

use smithay::{
    backend::{
        renderer::{
            damage::OutputDamageTracker,
            gles::GlesRenderer,
        },
        winit::{self, WinitEvent, WinitGraphicsBackend},
    },
    output::{Mode, Output, PhysicalProperties, Subpixel},
    reexports::{
        calloop::{
            generic::Generic,
            EventLoop, Interest, Mode as CalloopMode,
        },
        wayland_server::Display,
        winit::platform::pump_events::PumpStatus,
    },
    utils::{Rectangle, Transform},
    wayland::socket::ListeningSocketSource,
};

use crate::state::{Backend, ClientState, CommonState, State};

/// Data owned by the winit backend (stored in `State`).
#[allow(dead_code)]
pub struct WinitData {
    pub backend: WinitGraphicsBackend<GlesRenderer>,
    pub output: Output,
    pub damage_tracker: OutputDamageTracker,
}

/// Initialise the winit backend and run the event loop.
pub fn run() -> anyhow::Result<()> {
    let mut event_loop = EventLoop::<State>::try_new()?;
    let display = Display::<State>::new()?;
    let dh = display.handle();
    let loop_handle = event_loop.handle();
    let loop_signal = event_loop.get_signal();

    // `winit_backend` goes into state; `winit_events` stays local so we can
    // call `dispatch_new_events` in the main loop without a state borrow conflict.
    let (winit_backend, mut winit_events) = winit::init::<GlesRenderer>()
        .map_err(|e| anyhow::anyhow!("{e:?}"))?;

    let output = {
        let win_size = winit_backend.window_size();
        let out = Output::new(
            "winit-0".to_owned(),
            PhysicalProperties {
                size: (win_size.w, win_size.h).into(),
                subpixel: Subpixel::Unknown,
                make: "Smithay".into(),
                model: "Winit".into(),
                serial_number: "Unknown".into(),
            },
        );
        let mode = Mode { size: win_size, refresh: 60_000 };
        out.change_current_state(
            Some(mode),
            Some(Transform::Normal),
            None,
            Some((0, 0).into()),
        );
        out.set_preferred(mode);
        out
    };

    let damage_tracker = OutputDamageTracker::from_output(&output);

    // Socket — auto-select an available name, then accept clients via calloop.
    let listening_socket = ListeningSocketSource::new_auto()?;
    let socket_name = listening_socket
        .socket_name()
        .to_string_lossy()
        .into_owned();

    loop_handle.insert_source(listening_socket, |client_stream, _, state| {
        state
            .common
            .display_handle
            .insert_client(client_stream, Arc::new(ClientState::default()))
            .ok();
    })?;

    let mut common =
        CommonState::new(&dh, loop_handle.clone(), loop_signal.clone(), socket_name.clone());
    common.space.map_output(&output, (0, 0));

    let winit_data = WinitData { backend: winit_backend, output, damage_tracker };
    let mut state = State { backend: Backend::Winit(Box::new(winit_data)), common };

    // Register the Wayland display as a calloop source.
    loop_handle.insert_source(
        Generic::new(display, Interest::READ, CalloopMode::Level),
        |_, display, state| {
            // SAFETY: display lives for the duration of the event loop.
            unsafe {
                display.get_mut().dispatch_clients(state)?;
            }
            Ok(smithay::reexports::calloop::PostAction::Continue)
        },
    )?;

    tracing::info!("Wayland socket: {}", socket_name);
    std::env::set_var("WAYLAND_DISPLAY", &socket_name);

    // Main loop: pump winit events, render, then dispatch calloop.
    let mut running = true;
    while running {
        let status = winit_events.dispatch_new_events(|event| match event {
            WinitEvent::Resized { size, .. } => {
                let Backend::Winit(ref mut winit) = state.backend else { return };
                let mode = Mode { size, refresh: 60_000 };
                winit.output.change_current_state(Some(mode), None, None, None);
                winit.output.set_preferred(mode);
            }
            WinitEvent::Input(event) => {
                crate::input::handle_winit_input(&mut state, event);
            }
            WinitEvent::Focus(_) | WinitEvent::Redraw | WinitEvent::CloseRequested => {}
        });

        if let PumpStatus::Exit(_) = status {
            break;
        }

        render_frame(&mut state);

        if event_loop
            .dispatch(Some(Duration::from_millis(1)), &mut state)
            .is_err()
        {
            running = false;
        } else {
            state.common.space.refresh();
            state.common.popup_manager.cleanup();
            if let Err(e) = state.common.display_handle.flush_clients() {
                tracing::warn!("flush_clients: {e}");
            }
        }
    }

    Ok(())
}

/// Render one frame into the winit window (clear to a dark grey).
fn render_frame(state: &mut State) {
    use smithay::backend::renderer::{Frame, Renderer};

    let Backend::Winit(ref mut winit) = state.backend else { return };

    let size = winit
        .output
        .current_mode()
        .map(|m| m.size)
        .unwrap_or_else(|| (1920i32, 1080i32).into());

    let rendered = match winit.backend.bind() {
        Ok((renderer, mut fb)) => {
            match renderer.render(&mut fb, size, Transform::Normal) {
                Ok(mut frame) => {
                    let _ = frame.clear(
                        [0.1, 0.1, 0.1, 1.0].into(),
                        &[Rectangle::from_size(size)],
                    );
                    let _ = frame.finish();
                    true
                }
                Err(e) => { tracing::warn!("winit render: {e:?}"); false }
            }
        }
        Err(e) => { tracing::warn!("winit bind: {e}"); false }
    };

    if rendered {
        if let Err(e) = winit.backend.submit(None) {
            tracing::warn!("winit submit: {e}");
        }
    }
}
