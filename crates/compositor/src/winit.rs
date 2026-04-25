//! Winit dev backend — runs the compositor as a nested window inside an existing display.

use std::sync::Arc;
use std::time::{Duration, Instant};

use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            damage::OutputDamageTracker,
            element::{
                memory::{MemoryRenderBuffer, MemoryRenderBufferRenderElement},
                solid::SolidColorRenderElement,
                Kind,
            },
            gles::GlesRenderer,
            ImportAll, ImportMem,
        },
        winit::{self, WinitEvent, WinitGraphicsBackend},
    },
    desktop::space::render_output,
    output::{Mode, Output, PhysicalProperties, Subpixel},
    reexports::{
        calloop::{
            generic::Generic,
            EventLoop, Interest, Mode as CalloopMode,
        },
        wayland_server::Display,
        winit::platform::pump_events::PumpStatus,
    },
    render_elements,
    utils::{Scale, Transform},
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

    // Hide winit's native pointer — we render our own cursor at the
    // compositor-tracked pointer location.
    winit_backend.window().set_cursor_visible(false);

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
        // winit's GL framebuffer is Y-down vs smithay's render-output
        // expectation — Flipped180 corrects the upside-down surfaces.
        // Matches anvil's winit example.
        out.change_current_state(
            Some(mode),
            Some(Transform::Flipped180),
            None,
            Some((0, 0).into()),
        );
        out.set_preferred(mode);
        out
    };

    // Make the Output visible as a wl_output global — without this clients
    // see "no monitors available" and never create surfaces.
    let _output_global = output.create_global::<State>(&dh);

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

        // Advance window animations.
        let now = Instant::now();
        let dt = now.saturating_duration_since(state.common.last_frame).as_secs_f64();
        state.common.last_frame = now;
        state.common.shell.tick_animations(dt);

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

// Wraps the two cursor render-element kinds (real texture vs solid-color
// fallback) into one type that satisfies render_output's `C: RenderElement<R>`
// generic.
render_elements! {
    pub CursorElement<R> where R: ImportAll + ImportMem;
    Texture=MemoryRenderBufferRenderElement<R>,
    Solid=SolidColorRenderElement,
}

/// Render one frame into the winit window. Draws every mapped wayland
/// surface in the compositor `Space` via smithay's damage-tracked
/// `render_output` helper, plus a cursor overlay at the pointer
/// location (real SVG cursor texture if loadable, else a small solid
/// square as a fallback).
fn render_frame(state: &mut State) {
    let Backend::Winit(ref mut winit) = state.backend else { return };

    let age = winit.backend.buffer_age().unwrap_or(0);
    let element_count = state.common.space.elements().count();
    let scale = Scale::from(winit.output.current_scale().fractional_scale());

    // Lazy-load the default cursor texture on first render so the cost
    // doesn't appear in startup time.
    if state.common.cursor_buffer.is_none() {
        let physical_size = 24u32;
        if let Some(c) = state
            .common
            .cursor_manager
            .get_cursor("default", physical_size)
        {
            // Cursor crate emits row-major premultiplied RGBA; smithay reads
            // memory buffers as Argb8888 (which on little-endian = BGRA in
            // memory). Swap R↔B so the cursor doesn't render with an
            // inverted hue.
            let mut bgra = c.pixels.clone();
            for px in bgra.chunks_exact_mut(4) {
                px.swap(0, 2);
            }
            state.common.cursor_buffer = Some(MemoryRenderBuffer::from_slice(
                &bgra,
                Fourcc::Argb8888,
                (c.width as i32, c.height as i32),
                1,
                Transform::Normal,
                None,
            ));
            state.common.cursor_hotspot = (c.hotspot_x, c.hotspot_y);
        }
    }

    // Build cursor overlay element.
    let cursor_elements: Vec<CursorElement<GlesRenderer>> = if let Some(pointer) =
        state.common.seat.get_pointer()
    {
        let cursor_logical = pointer.current_location()
            - smithay::utils::Point::from((
                state.common.cursor_hotspot.0 as f64,
                state.common.cursor_hotspot.1 as f64,
            ));
        let cursor_physical = cursor_logical.to_physical(scale);

        if let Some(buf) = state.common.cursor_buffer.as_ref() {
            match MemoryRenderBufferRenderElement::from_buffer(
                {
                    let (renderer, _) = match winit.backend.bind() {
                        Ok(p) => p,
                        Err(_) => return,
                    };
                    renderer
                },
                cursor_physical,
                buf,
                None,
                None,
                None,
                Kind::Cursor,
            ) {
                Ok(el) => vec![CursorElement::Texture(el)],
                Err(e) => {
                    tracing::trace!("cursor texture element failed: {e}; falling back to solid");
                    fallback_cursor(cursor_physical.to_i32_round())
                }
            }
        } else {
            fallback_cursor(cursor_physical.to_i32_round())
        }
    } else {
        Vec::new()
    };

    // Render under a scoped borrow so we can call `submit` afterwards.
    let damage_owned = {
        let (renderer, mut fb) = match winit.backend.bind() {
            Ok(pair) => pair,
            Err(e) => {
                tracing::warn!("winit bind: {e}");
                return;
            }
        };

        let result = render_output::<_, CursorElement<GlesRenderer>, _, _>(
            &winit.output,
            renderer,
            &mut fb,
            1.0,
            age,
            [&state.common.space],
            &cursor_elements,
            &mut winit.damage_tracker,
            [0.06, 0.06, 0.07, 1.0],
        );

        match result {
            Ok(res) => {
                let dmg_n = res.damage.as_ref().map(|d| d.len()).unwrap_or(0);
                if element_count > 0 {
                    tracing::trace!(elements = element_count, damage = dmg_n, "render_output ok");
                }
                res.damage.cloned()
            }
            Err(e) => {
                tracing::warn!("render_output failed: {e:?}");
                None
            }
        }
    };

    if let Err(e) = winit.backend.submit(damage_owned.as_deref()) {
        tracing::warn!("winit submit: {e}");
    }

    // Drive surface frame callbacks (without these, clients won't paint
    // their next frame). Anvil does this in its main loop.
    let now = state.common.clock.now();
    state.common.space.elements().for_each(|w| {
        w.send_frame(&winit.output, now, Some(std::time::Duration::from_secs(1)), |_, _| {
            Some(winit.output.clone())
        });
    });
}

/// Last-resort 12×12 white square at the cursor location, used when no
/// real cursor texture is available.
fn fallback_cursor(loc: smithay::utils::Point<i32, smithay::utils::Physical>) -> Vec<CursorElement<GlesRenderer>> {
    use smithay::backend::renderer::{element::Id, utils::CommitCounter};
    use smithay::utils::{Rectangle, Size};
    let size: Size<i32, smithay::utils::Physical> = (12, 12).into();
    vec![CursorElement::Solid(SolidColorRenderElement::new(
        Id::new(),
        Rectangle::new(loc, size),
        CommitCounter::default(),
        [0.95, 0.95, 0.95, 1.0],
        Kind::Cursor,
    ))]
}
