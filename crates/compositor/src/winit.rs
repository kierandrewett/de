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
                surface::WaylandSurfaceRenderElement,
                AsRenderElements, Kind,
            },
            gles::GlesRenderer,
        },
        winit::{self, WinitEvent, WinitGraphicsBackend},
    },
    desktop::{layer_map_for_output, space::render_output},
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
    utils::{Point, Scale, Transform},
    wayland::{shell::wlr_layer::Layer as WlrLayer, socket::ListeningSocketSource},
};

use crate::state::{Backend, ClientState, CommonState, State};

/// Data owned by the winit backend (stored in `State`).
#[allow(dead_code)]
pub struct WinitData {
    pub backend: WinitGraphicsBackend<GlesRenderer>,
    pub output: Output,
    pub damage_tracker: OutputDamageTracker,
    /// Compiled GLSL squircle-clip texture program. Lazily initialised
    /// on first frame; `None` means "fall back to rectangular surfaces".
    pub clip_program:
        Option<smithay::backend::renderer::gles::GlesTexProgram>,
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
    let (mut winit_backend, mut winit_events) = winit::init::<GlesRenderer>()
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

    // Advertise zwp_linux_dmabuf_v1 with the renderer's actual format set.
    // Mesa's EGL-on-Wayland implementation requires either v4 (with
    // default DmabufFeedback) or wl_drm via `bind_wl_display`; without
    // one of those, GTK4 / wgpu / Vulkan clients fail to allocate any
    // GPU buffer and crash during their own renderer init. We do both:
    // v4 feedback for modern clients, wl_drm for legacy.
    {
        use smithay::backend::{
            egl::EGLDevice,
            renderer::ImportDma,
        };
        use smithay::wayland::dmabuf::DmabufFeedbackBuilder;

        let renderer = winit_backend.renderer();
        let render_node = EGLDevice::device_for_display(renderer.egl_context().display())
            .ok()
            .and_then(|d| d.try_get_render_node().ok().flatten());

        match render_node {
            Some(node) => {
                let dmabuf_formats = renderer.dmabuf_formats();
                match DmabufFeedbackBuilder::new(node.dev_id(), dmabuf_formats).build() {
                    Ok(feedback) => {
                        common
                            .dmabuf_state
                            .create_global_with_default_feedback::<State>(&dh, &feedback);
                        tracing::info!(
                            node = %node.dev_path().map(|p| p.display().to_string()).unwrap_or_default(),
                            "dmabuf v4 advertised with default feedback",
                        );
                    }
                    Err(e) => {
                        tracing::warn!(
                            "dmabuf feedback build failed ({e:?}); falling back to v3",
                        );
                        let formats = renderer.dmabuf_formats();
                        common.dmabuf_state.create_global::<State>(&dh, formats);
                    }
                }
            }
            None => {
                tracing::warn!(
                    "no DRM render node for renderer EGL display; advertising dmabuf v3 only",
                );
                let formats = renderer.dmabuf_formats();
                common.dmabuf_state.create_global::<State>(&dh, formats);
            }
        }
    }

    // (Skipped: `ImportEgl::bind_wl_display` for the legacy wl_drm
    // protocol. Tried it and it breaks our own EGL surface on radeonsi —
    // `eglSwapBuffers` immediately starts returning BAD_SURFACE on the
    // next frame. Modern clients use the dmabuf v4 path configured
    // above; the only thing we'd lose by skipping wl_drm is support for
    // very old EGL clients that haven't been updated for dmabuf, which
    // is basically nothing in 2026.)

    let winit_data = WinitData {
        backend: winit_backend,
        output,
        damage_tracker,
        clip_program: None,
    };
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

// Wraps every overlay render-element variant. The `Texture` variant handles
// every CPU-rasterised buffer we composite (cursor, SSD title bar, per-window
// shadow halo and per-window decoration overlay). `Solid` is the fallback
// cursor when no SVG cursor is loaded. `Clip` is the SDF-clipped wayland
// surface — produced by [`crate::render::squircle_clip`].
//
// All shadows are emitted with their squircle interior cut out so they can
// be composited *above* the wayland surfaces without darkening the client —
// this lets us keep using `space::render_output`, which always layers
// custom elements on top of the space.
/// Throttled bind() success/failure counter. Logs once per second
/// with the running totals so a BAD_SURFACE storm shows up as e.g.
/// `bind status: ok=0 fail=15` rather than 15 separate WARN lines.
/// Counts reset every emission.
/// Run the per-frame "client liveness" steps that don't depend on the
/// winit / render backend: send wl_surface.frame callbacks, signal any
/// wp_fifo barriers from the previous commit, then poke each client's
/// transaction queue so commits held behind a now-signaled blocker
/// actually apply.
///
/// Doing this BEFORE the winit mutable borrow keeps it firing even
/// when our own bind/render fails — clients (e.g. iced_layershell-based
/// shell-panel) need their callbacks and unblocked commits to advance,
/// regardless of whether we manage to put a fresh frame on the host.
fn pre_render_drive_clients(state: &mut State) {
    use smithay::reexports::wayland_server::Resource;
    use smithay::wayland::compositor::CompositorHandler;
    use smithay::wayland::seat::WaylandFocus;

    // Borrow winit just long enough to grab the output for send_frame.
    // We DROP the borrow before doing the blocker_cleared work, which
    // re-borrows `state` mutably for any commit() callback the queue
    // invokes on now-unblocked transactions.
    let output = match &state.backend {
        Backend::Winit(w) => w.output.clone(),
        _ => return,
    };

    let now = state.common.clock.now();
    state.common.space.elements().for_each(|w| {
        w.send_frame(
            &output,
            now,
            Some(std::time::Duration::from_secs(1)),
            |_, _| Some(output.clone()),
        );
    });
    let surfaces_in_scene: Vec<_> = state
        .common
        .space
        .elements()
        .filter_map(|w| w.wl_surface().map(|s: std::borrow::Cow<'_, _>| s.into_owned()))
        .chain(state.common.space.outputs().flat_map(|o| {
            let map = layer_map_for_output(o);
            map.layers()
                .map(|l| l.wl_surface().clone())
                .collect::<Vec<_>>()
        }))
        .collect();
    for o in state.common.space.outputs() {
        let map = layer_map_for_output(o);
        for layer in map.layers() {
            layer.send_frame(
                o,
                now,
                None,
                |_, _| Some(o.clone()),
            );
        }
    }
    for surface in &surfaces_in_scene {
        signal_fifo_barriers(surface);
    }

    // Now wake the per-client transaction queues so that commits held
    // behind a wait_barrier actually apply. `blocker_cleared` may invoke
    // our `CompositorHandler::commit` on each unblocked surface, so we
    // pass `state` mutably.
    let dh = state.common.display_handle.clone();
    let mut clients: Vec<smithay::reexports::wayland_server::Client> = Vec::new();
    let mut seen: std::collections::HashSet<
        smithay::reexports::wayland_server::backend::ClientId,
    > = std::collections::HashSet::new();
    for s in &surfaces_in_scene {
        if let Some(c) = s.client() {
            if seen.insert(c.id()) {
                clients.push(c);
            }
        }
    }
    for client in clients {
        let ccs_ptr: *const smithay::wayland::compositor::CompositorClientState =
            state.client_compositor_state(&client) as *const _;
        // SAFETY: CompositorClientState lives inside our ClientState in
        // the client's UserData, pinned for the life of the client.
        // We need this raw-pointer dance because `client_compositor_state`
        // borrows `state` immutably while we want to pass `state` mutably
        // into `blocker_cleared` on the same call.
        let ccs = unsafe { &*ccs_ptr };
        ccs.blocker_cleared(state, &dh);
    }
}

/// Walk the surface tree rooted at `surface` and signal any pending
/// `wp_fifo_v1` barrier on each subsurface. Required for clients that
/// use mesa-vk's fifo-mode swapchain (notably wgpu, which iced_layershell
/// uses for our shell apps): without signalling, the client's next commit
/// is held in a smithay `Blocker` and never reaches us.
fn signal_fifo_barriers(
    surface: &smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
) {
    use smithay::wayland::compositor::{with_surface_tree_downward, TraversalAction};
    use smithay::wayland::fifo::FifoBarrierCachedState;

    with_surface_tree_downward(
        surface,
        (),
        |_, _, _| TraversalAction::DoChildren(()),
        |_sub, states, _| {
            let barrier = states
                .cached_state
                .get::<FifoBarrierCachedState>()
                .current()
                .barrier
                .take();
            if let Some(b) = barrier {
                b.signal();
            }
        },
        |_, _, _| true,
    );
}

fn bind_status_log(ok: bool) {
    use std::sync::Mutex;
    use std::time::Instant;
    static STATE: std::sync::OnceLock<Mutex<(u64, u64, Instant)>> =
        std::sync::OnceLock::new();
    let lock = STATE.get_or_init(|| Mutex::new((0, 0, Instant::now())));
    let mut s = lock.lock().unwrap();
    if ok {
        s.0 += 1;
    } else {
        s.1 += 1;
    }
    if s.2.elapsed() >= std::time::Duration::from_secs(1) {
        let (ok_n, fail_n) = (s.0, s.1);
        if fail_n > 0 {
            tracing::warn!(ok = ok_n, fail = fail_n, "winit bind status (1 s window)");
        } else {
            tracing::debug!(ok = ok_n, "winit bind status (1 s window)");
        }
        s.0 = 0;
        s.1 = 0;
        s.2 = Instant::now();
    }
}

render_elements! {
    pub CursorElement<=GlesRenderer>;
    Texture=MemoryRenderBufferRenderElement<GlesRenderer>,
    Solid=SolidColorRenderElement,
    Clip=smithay::backend::renderer::gles::element::TextureShaderElement,
    // Wayland surface trees — used for layer-shell surfaces (panels,
    // docks, notifications). Composited above the space windows by
    // virtue of being in the custom-elements list passed to
    // `render_output`.
    Surface=WaylandSurfaceRenderElement<GlesRenderer>,
}

/// Render one frame into the winit window.
///
/// Composition order, front-to-back:
/// 1. **Cursor** — top of everything
/// 2. **Decoration overlays** (per SSD/CSD window) — corner cutouts that
///    visually clip the rectangular client surface to a squircle, plus the
///    outer 0.5 px stroke and the 1 px inner highlight gradient.
/// 3. **Title bars** (SSD only) — iced-rendered chrome above the client
///    content but *below* the decoration overlay so the squircle clips it.
/// 4. **Space windows** — wayland surfaces rendered by smithay.
/// 5. **Shadows** (per window) — multi-layer Gaussian-blurred squircle
///    drawn behind every window. Shadows are at the very back so they show
///    up on the desktop area outside the window rect; the in-window region
///    is overwritten by the client.
fn render_frame(state: &mut State) {
    // Drive surface frame callbacks + signal wp_fifo barriers + wake
    // per-client transaction queues UP FRONT — before any winit borrow
    // or render path that might bail out. iced clients (panel, dock)
    // gate their event loop on `wl_surface.frame` completion AND on
    // smithay clearing the wp_fifo `wait_barrier` blocker on each
    // commit; if either step is skipped (BAD_SURFACE storm, missed
    // signal), the panel clock visibly freezes after first paint.
    pre_render_drive_clients(state);

    let Backend::Winit(ref mut winit) = state.backend else { return };

    // Always full-damage on the winit backend. Calling
    // `winit.backend.buffer_age()` triggers a noisy `eglQuerySurface`
    // BAD_SURFACE error on every frame because of a known smithay/winit
    // borrow bug (smithay #1672, fix in unmerged #1673). Until that lands
    // upstream, just pay the cost of full-screen damage on every frame —
    // we're a winit-hosted dev backend, not a real display anyway.
    let age = 0usize;
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

    // ── Snapshot per-window data we need to build chrome ────────────────
    // Done up-front so we can drop the borrow on `state.common.space`
    // before mutably borrowing `chrome_iced`, `window_chrome`, and the
    // renderer.
    // Sweep windows whose close animation has finished, then unmap from
    // the smithay Space and broadcast the close. Done before the
    // snapshot so closed entries don't appear in this frame's chrome.
    let closed_ids = state.common.shell.sweep_closed_windows();
    if !closed_ids.is_empty() {
        let to_unmap: Vec<smithay::desktop::Window> = state
            .common
            .space
            .elements()
            .filter(|w| {
                w.user_data()
                    .get::<crate::wayland::handlers::xdg_shell::ShellWindowId>()
                    .map(|id| id.0)
                    .map(|id| closed_ids.contains(&id))
                    .unwrap_or(false)
            })
            .cloned()
            .collect();
        for w in &to_unmap {
            state.common.space.unmap_elem(w);
        }
        for id in &closed_ids {
            state
                .common
                .ipc
                .broadcast(&ipc::ShellEvent::WindowClosed { window_id: *id });
        }
    }

    let focused_id = state.common.shell.focused_window_id();
    struct WindowSnapshot {
        window: smithay::desktop::Window,
        content_geo: smithay::utils::Rectangle<i32, smithay::utils::Logical>,
        full_geo: smithay::utils::Rectangle<i32, smithay::utils::Logical>,
        title: String,
        focused: bool,
        is_ssd: bool,
        /// Animated focus amount [0, 1] — sampled from the shell at frame
        /// time so the chrome crossfades smoothly between active/inactive
        /// states without recomputing on every animation tick.
        focus_amount: f32,
        /// Animated open/close opacity [0, 1]. 0 = invisible, 1 = full.
        opacity: f32,
        /// Animated open/close scale (1.0 = full size, 0.85 = appearing
        /// or disappearing). Applied to chrome + the SDF-clipped surface
        /// so the whole window scales in/out as one from its centre.
        anim_scale: f32,
    }
    let snapshots: Vec<WindowSnapshot> = state
        .common
        .space
        .elements()
        .filter_map(|w| {
            let content_geo = state.common.space.element_geometry(w)?;
            let is_ssd = crate::shell::is_ssd(w);
            // SSD windows extend upward by TITLE_BAR_HEIGHT for the bar.
            let full_geo = if is_ssd {
                let chrome = crate::shell::title_bar_chrome(content_geo);
                smithay::utils::Rectangle::new(
                    chrome.bar.loc,
                    smithay::utils::Size::from((
                        content_geo.size.w,
                        content_geo.size.h + chrome.bar.size.h,
                    )),
                )
            } else {
                content_geo
            };
            let title = if let smithay::desktop::WindowSurface::Wayland(toplevel) =
                w.underlying_surface()
            {
                smithay::wayland::compositor::with_states(toplevel.wl_surface(), |states| {
                    states
                        .data_map
                        .get::<std::sync::Mutex<smithay::wayland::shell::xdg::XdgToplevelSurfaceRoleAttributes>>()
                        .and_then(|m| m.lock().ok().and_then(|g| g.title.clone()))
                })
                .unwrap_or_default()
            } else {
                String::new()
            };
            let win_id = w
                .user_data()
                .get::<crate::wayland::handlers::xdg_shell::ShellWindowId>()
                .map(|id| id.0);
            // Sample the per-window focus animation. Falls back to a
            // hard 0/1 if the shell hasn't tracked this window yet.
            let (focus_amount, opacity, anim_scale) = win_id
                .and_then(|id| state.common.shell.window(id))
                .map(|w| {
                    (
                        w.animation.focus.position()[0] as f32,
                        w.animation.opacity.position()[0] as f32,
                        w.animation.scale.position()[0] as f32,
                    )
                })
                .unwrap_or_else(|| {
                    let f = if win_id == focused_id { 1.0 } else { 0.0 };
                    (f, 1.0, 1.0)
                });
            let focus_amount = focus_amount.clamp(0.0, 1.0);
            let opacity = opacity.clamp(0.0, 1.0);
            // Clamp anim_scale to a sane range so any rogue overshoot
            // can't push it negative or absurdly large.
            let anim_scale = anim_scale.clamp(0.5, 1.05);
            Some(WindowSnapshot {
                window: w.clone(),
                content_geo,
                full_geo,
                title,
                focused: win_id == focused_id,
                is_ssd,
                focus_amount,
                opacity,
                anim_scale,
            })
        })
        .collect();
    let element_count = snapshots.len();

    // ── Build per-window chrome buffers (shadow + decoration overlay) ──
    // We cache both the active and inactive variants and crossfade them
    // at composite time using the per-window focus animation. The
    // crossfade isn't a perfectly linear lerp under src-over alpha
    // blending (the second pass also fades over the first), but the
    // visual result is smooth and the cache stays cheap (two static
    // buffers per window instead of recomputing each frame).
    struct ChromeBuffers {
        active_shadow: MemoryRenderBuffer,
        inactive_shadow: MemoryRenderBuffer,
        active_decoration: MemoryRenderBuffer,
        inactive_decoration: MemoryRenderBuffer,
        // Active and inactive shadow textures have different padding
        // (active uses 3 layers up to 48 px blur, inactive uses 2 up to
        // 12 px), so the textures are different sizes and have to be
        // positioned independently.
        active_shadow_phys_loc: Point<f64, smithay::utils::Physical>,
        inactive_shadow_phys_loc: Point<f64, smithay::utils::Physical>,
        /// Native logical width/height of each shadow buffer — used to
        /// rescale the shadow alongside the rest of the chrome during
        /// the open/close scale animation, so the halo shrinks with the
        /// window instead of getting visually decoupled from it.
        active_shadow_size_logical: smithay::utils::Size<i32, smithay::utils::Logical>,
        inactive_shadow_size_logical: smithay::utils::Size<i32, smithay::utils::Logical>,
        decoration_phys_loc: Point<f64, smithay::utils::Physical>,
    }
    let mut chrome_buffers: Vec<ChromeBuffers> = Vec::with_capacity(snapshots.len());
    {
        let dark = state.common.dark_mode;
        let theme = &state.common.window_theme;
        for snap in &snapshots {
            let active_style = if dark {
                &theme.border.dark_active
            } else {
                &theme.border.light_active
            };
            let inactive_style = if dark {
                &theme.border.dark_inactive
            } else {
                &theme.border.light_inactive
            };
            let key_active = crate::render::window_chrome::ChromeKey::new(
                snap.full_geo.size.w as f64,
                snap.full_geo.size.h as f64,
                scale.x,
                true,
                dark,
            );
            let key_inactive = crate::render::window_chrome::ChromeKey::new(
                snap.full_geo.size.w as f64,
                snap.full_geo.size.h as f64,
                scale.x,
                false,
                dark,
            );
            let active_bundle = state
                .common
                .window_chrome
                .get_or_build(key_active, theme, active_style);
            let active_pad_logical = active_bundle.shadow_padding_logical;
            let active_shadow = active_bundle.shadow.clone();
            let active_decoration = active_bundle.decoration.clone();
            let inactive_bundle = state
                .common
                .window_chrome
                .get_or_build(key_inactive, theme, inactive_style);
            let inactive_pad_logical = inactive_bundle.shadow_padding_logical;
            let inactive_shadow = inactive_bundle.shadow.clone();
            let inactive_decoration = inactive_bundle.decoration.clone();
            let active_shadow_phys_loc = (
                (snap.full_geo.loc.x as f64 - active_pad_logical) * scale.x,
                (snap.full_geo.loc.y as f64 - active_pad_logical) * scale.y,
            )
                .into();
            let inactive_shadow_phys_loc = (
                (snap.full_geo.loc.x as f64 - inactive_pad_logical) * scale.x,
                (snap.full_geo.loc.y as f64 - inactive_pad_logical) * scale.y,
            )
                .into();
            let decoration_phys_loc = (
                snap.full_geo.loc.x as f64 * scale.x,
                snap.full_geo.loc.y as f64 * scale.y,
            )
                .into();
            // Native logical size of each shadow buffer: window size +
            // 2× shadow padding on each axis. Used to scale the shadow
            // alongside the rest of the chrome during the open/close
            // animation.
            let active_shadow_size_logical: smithay::utils::Size<i32, smithay::utils::Logical> = (
                snap.full_geo.size.w + (2.0 * active_pad_logical).round() as i32,
                snap.full_geo.size.h + (2.0 * active_pad_logical).round() as i32,
            )
                .into();
            let inactive_shadow_size_logical: smithay::utils::Size<i32, smithay::utils::Logical> = (
                snap.full_geo.size.w + (2.0 * inactive_pad_logical).round() as i32,
                snap.full_geo.size.h + (2.0 * inactive_pad_logical).round() as i32,
            )
                .into();
            chrome_buffers.push(ChromeBuffers {
                active_shadow,
                inactive_shadow,
                active_decoration,
                inactive_decoration,
                active_shadow_phys_loc,
                inactive_shadow_phys_loc,
                active_shadow_size_logical,
                inactive_shadow_size_logical,
                decoration_phys_loc,
            });
        }
    }

    // Cursor location in physical coords.
    let cursor_phys = state.common.seat.get_pointer().map(|pointer| {
        let cursor_logical = pointer.current_location()
            - smithay::utils::Point::from((
                state.common.cursor_hotspot.0 as f64,
                state.common.cursor_hotspot.1 as f64,
            ));
        cursor_logical.to_physical(scale)
    });

    // ── Build title-bar chrome buffers (SSD only) ──────────────────────
    // The bar is rendered once in its *active* style; the inactive
    // 0.75-opacity variant from WINDOW_SPEC.md is reproduced by
    // lowering the bar element's alpha at composite time, which fades
    // bg + text together.
    struct TitleBarBuf {
        buffer: MemoryRenderBuffer,
        phys_loc: Point<f64, smithay::utils::Physical>,
        /// Composite-time alpha — lerps 0.75 (inactive) → 1.0 (active).
        alpha: f32,
    }
    let mut title_bars: Vec<TitleBarBuf> = Vec::new();
    for snap in &snapshots {
        if !snap.is_ssd {
            continue;
        }
        // Title bar occupies the top TITLE_BAR_HEIGHT of full_geo.
        let bar_height_logical: i32 = crate::shell::TITLE_BAR_HEIGHT;
        let bar_logical = smithay::utils::Rectangle::<i32, smithay::utils::Logical>::new(
            snap.full_geo.loc,
            smithay::utils::Size::from((snap.full_geo.size.w, bar_height_logical)),
        );
        let bar_phys: smithay::utils::Rectangle<i32, smithay::utils::Physical> =
            bar_logical.to_physical_precise_round(scale);
        let phys_loc = bar_logical.loc.to_f64().to_physical(scale);
        // Always render the focused variant — both look identical now
        // that the inactive style is the same chrome with 0.75 opacity.
        let buffer = match state.common.chrome_iced.render_title_bar(
            bar_phys.size.w.max(1) as u32,
            bar_phys.size.h.max(1) as u32,
            scale.x,
            &snap.title,
            true,
        ) {
            Some(b) => b.clone(),
            None => continue,
        };
        let alpha = 0.75 + 0.25 * snap.focus_amount;
        title_bars.push(TitleBarBuf { buffer, phys_loc, alpha });
    }

    // ── Render under a scoped borrow so we can call submit afterwards ──
    let damage_owned = {
        let (renderer, mut fb) = match winit.backend.bind() {
            Ok(pair) => {
                bind_status_log(true);
                pair
            }
            Err(e) => {
                bind_status_log(false);
                // Fast-rate `tracing::warn!` here drowns the log when
                // the EGL surface is in a BAD_SURFACE storm (which
                // happens whenever the host iconifies our window). The
                // status helper above logs once per second with a
                // running success/fail count instead.
                let _ = e;
                return;
            }
        };

        // Front-to-back overlay order (custom elements are layered ABOVE
        // the space by `render_output`):
        //   1. cursor
        //   2. decoration overlays  (clip corners + draw border + inner highlight)
        //   3. title bars           (below decorations so the squircle clips them)
        //   4. shadows              (interior cut out — only the halo is visible)
        //   5. (space windows — appended internally by render_output)
        let mut all_overlays: Vec<CursorElement> = Vec::new();

        if let Some(cursor_physical) = cursor_phys {
            if let Some(buf) = state.common.cursor_buffer.as_ref() {
                match MemoryRenderBufferRenderElement::from_buffer(
                    renderer,
                    cursor_physical,
                    buf,
                    None,
                    None,
                    None,
                    Kind::Cursor,
                ) {
                    Ok(el) => all_overlays.push(CursorElement::Texture(el)),
                    Err(e) => {
                        tracing::trace!("cursor texture element failed: {e}; using solid fallback");
                        all_overlays.extend(fallback_cursor(cursor_physical.to_i32_round()));
                    }
                }
            } else {
                all_overlays.extend(fallback_cursor(cursor_physical.to_i32_round()));
            }
        }

        // ── Compile (or retrieve cached) squircle-clip shader program ──
        // Done once here — before both the layer-shell and window loops —
        // so chrome-eligible layer surfaces and toplevel windows share the
        // same compiled program handle.
        if winit.clip_program.is_none() {
            match crate::render::squircle_clip::compile_clip_program(renderer) {
                Ok(prog) => winit.clip_program = Some(prog),
                Err(e) => tracing::error!("clip program compile failed: {e}"),
            }
        }
        let clip_program = winit.clip_program.clone();

        // ── Layer-shell surfaces (panels / docks / wallpapers) ──────────
        // Pulled from the per-output `LayerMap`. We render every layer
        // here as a `CursorElement::Surface` and let render_output put
        // them above the space windows.
        //
        // Chrome-eligible layer surfaces (dock, datetime, cc) get the
        // same macOS-style decoration as toplevel windows:
        //   1. decoration overlay (inner highlight + outer stroke)  ← front
        //   2. SDF-clipped squircle surface                         ← middle
        //   3. shadow halo                                          ← back
        // All other surfaces render as plain WaylandSurfaceRenderElements.
        //
        // TODO: Bottom and Background should sit *below* the windows
        // (wallpapers must not occlude clients). render_output's
        // custom-elements list always layers above the space; a proper
        // fix is to switch to `OutputDamageTracker::render_output`
        // directly with a single z-ordered element vec. For now we
        // iterate all four layers above so panels/docks/notifications
        // are visible — wallpapers will be wrong-ordered but not
        // missing.
        //
        // Order in `all_overlays` is front-to-back, so push Overlay
        // first (notifications occlude panels), then Top (panels), then
        // Bottom, then Background.
        let outputs: Vec<Output> = state.common.space.outputs().cloned().collect();
        {
            // Borrow chrome cache + theme without holding a reference into
            // `winit` (which we need for the renderer above).
            let dark = state.common.dark_mode;
            let theme_clone = state.common.window_theme.clone();
            let radius_px_layer = theme_clone.corner_radius * scale.x as f32;
            let smoothing_layer = theme_clone.corner_smoothing;

            for output in &outputs {
                let map = layer_map_for_output(output);
                for layer_kind in [
                    WlrLayer::Overlay,
                    WlrLayer::Top,
                    WlrLayer::Bottom,
                    WlrLayer::Background,
                ] {
                    for layer in map.layers_on(layer_kind).rev() {
                        let Some(geo) = map.layer_geometry(layer) else {
                            tracing::trace!(
                                ns = layer.namespace(),
                                ?layer_kind,
                                "layer surface has no geometry yet (no commit)",
                            );
                            continue;
                        };
                        let ns = layer.namespace();
                        let chrome_kind =
                            crate::render::layer_chrome::layer_wants_chrome(ns);

                        if let (Some(_kind), Some(ref prog)) = (chrome_kind, &clip_program) {
                            // ── Chrome path ─────────────────────────────
                            // Build shadow + decoration buffers from the cache.
                            let chrome_bufs =
                                crate::render::layer_chrome::get_or_build_layer_chrome(
                                    &mut state.common.window_chrome,
                                    &theme_clone,
                                    geo,
                                    scale,
                                );

                            // 1. Decoration overlay (front-most of this surface's stack).
                            let deco_size = smithay::utils::Size::<i32, smithay::utils::Logical>::from(
                                (geo.size.w, geo.size.h),
                            );
                            if let Ok(el) = MemoryRenderBufferRenderElement::from_buffer(
                                renderer,
                                chrome_bufs.decoration_phys_loc,
                                &chrome_bufs.decoration,
                                Some(1.0_f32),
                                None,
                                Some(deco_size),
                                Kind::Unspecified,
                            ) {
                                all_overlays.push(CursorElement::Texture(el));
                            }

                            // 2. SDF-clipped squircle surface.
                            let phys_loc: Point<f64, smithay::utils::Physical> = (
                                geo.loc.x as f64 * scale.x,
                                geo.loc.y as f64 * scale.y,
                            )
                                .into();
                            if let Some(el) =
                                crate::render::layer_chrome::build_clipped_layer_element(
                                    renderer,
                                    prog,
                                    layer,
                                    phys_loc,
                                    scale.x,
                                    geo.size,
                                    radius_px_layer,
                                    smoothing_layer,
                                    1.0,
                                )
                            {
                                all_overlays.push(CursorElement::Clip(el));
                            } else {
                                // Surface buffer not yet imported — fall back to
                                // regular render_elements so the surface stays
                                // visible on the first few frames.
                                let phys_loc_i32 = geo.loc.to_physical_precise_round(scale);
                                let fallback: Vec<CursorElement> =
                                    layer.render_elements(renderer, phys_loc_i32, scale, 1.0);
                                all_overlays.extend(fallback);
                            }

                            // 3. Shadow halo (back of this surface's stack).
                            if let Ok(el) = MemoryRenderBufferRenderElement::from_buffer(
                                renderer,
                                chrome_bufs.shadow_phys_loc,
                                &chrome_bufs.shadow,
                                Some(1.0_f32),
                                None,
                                Some(chrome_bufs.shadow_size_logical),
                                Kind::Unspecified,
                            ) {
                                all_overlays.push(CursorElement::Texture(el));
                            }

                            tracing::trace!(
                                ns,
                                ?layer_kind,
                                ?geo,
                                "layer surface rendered with chrome",
                            );
                        } else {
                            // ── Plain path (no chrome) ──────────────────
                            let phys_loc = geo.loc.to_physical_precise_round(scale);
                            let elements: Vec<CursorElement> = layer.render_elements(
                                renderer,
                                phys_loc,
                                scale,
                                1.0,
                            );
                            tracing::trace!(
                                ns,
                                ?layer_kind,
                                n = elements.len(),
                                ?geo,
                                "layer surface rendered",
                            );
                            all_overlays.extend(elements);
                        }
                    }
                }
            }
            let _ = dark; // dark mode is implicit via theme (always dark for layer chrome)
        }

        // ── Per-window element grouping ─────────────────────────────────
        // For overlapping windows the front window's chrome MUST fully
        // occlude the back window — that means all of W1's elements
        // (decoration → title bar → surface → shadow) come before any
        // of W2's. `space.elements()` returns bottom-to-top, so we
        // iterate snapshots in reverse to push front-most first.
        let theme_ref = &state.common.window_theme;
        let radius_px = theme_ref.corner_radius * scale.x as f32;
        let smoothing = theme_ref.corner_smoothing;
        let title_bar_height_logical = crate::shell::TITLE_BAR_HEIGHT as f32;
        let mut clipped_count = 0usize;

        // Index `title_bars` by snap index for quick lookup. Title bars
        // were pushed in snapshot order, but only for SSD windows — so
        // we walk both in lock-step instead of indexing by position.
        let mut title_bar_by_geo: std::collections::HashMap<
            (i32, i32, i32, i32),
            &MemoryRenderBuffer,
        > = std::collections::HashMap::new();
        for (snap_i, snap) in snapshots.iter().enumerate() {
            if snap.is_ssd {
                if let Some(bar) = title_bars.get(0) {
                    let _ = (snap_i, bar);
                }
            }
        }
        // Simpler approach: walk title_bars and snapshots together since
        // both are populated in the same iteration order over SSD snaps.
        // Build an index `snap_idx → Option<&TitleBarBuf>`.
        let mut title_bar_for_snap: Vec<Option<&TitleBarBuf>> = Vec::with_capacity(snapshots.len());
        let mut tb_iter = title_bars.iter();
        for snap in &snapshots {
            if snap.is_ssd {
                title_bar_for_snap.push(tb_iter.next());
            } else {
                title_bar_for_snap.push(None);
            }
        }

        // Push each window's elements (front-most first) so the front
        // window fully covers the back one.
        //
        // Active and inactive chrome variants are layered with crossfade
        // alphas (active: focus_amount, inactive: 1 - focus_amount). The
        // active layer goes ABOVE the inactive layer so that as a window
        // gains focus, the activation paints in *over* the dimming
        // version, which feels right for opacity-style transitions.
        for (snap_i, (snap, cb)) in snapshots.iter().zip(chrome_buffers.iter()).enumerate().rev() {
            let f = snap.focus_amount;
            // Open / close fade: every chrome layer is multiplied by
            // `snap.opacity` so the whole window animates in/out as one.
            let o = snap.opacity;
            let s = snap.anim_scale;
            // Window centre stays put; everything shrinks toward it.
            // `chrome_offset` is the (negative) per-axis pixel delta we
            // add to a chrome element's *origin* to keep it centred when
            // its size is multiplied by `s`.
            let full_w_phys = snap.full_geo.size.w as f64 * scale.x;
            let full_h_phys = snap.full_geo.size.h as f64 * scale.y;
            let chrome_origin_dx = (full_w_phys * (1.0 - s as f64)) * 0.5;
            let chrome_origin_dy = (full_h_phys * (1.0 - s as f64)) * 0.5;
            // Logical (output-scale-1.0) offset for re-sizing memory
            // render elements via the `size` parameter.
            let scaled_full_w_log = (snap.full_geo.size.w as f32 * s).round() as i32;
            let scaled_full_h_log = (snap.full_geo.size.h as f32 * s).round() as i32;
            // Decoration buffer is full-window; title bar is bar-only.
            let bar_height_log = crate::shell::TITLE_BAR_HEIGHT;
            let scaled_bar_w_log = scaled_full_w_log;
            let scaled_bar_h_log = (bar_height_log as f32 * s).round() as i32;

            // 1. Decoration overlay (inner highlight + outer stroke).
            //    Active on top, inactive underneath — crossfade with
            //    the focus animation. Position + size are scaled toward
            //    the window centre during the open/close animation.
            let deco_loc: Point<f64, smithay::utils::Physical> = (
                cb.decoration_phys_loc.x + chrome_origin_dx,
                cb.decoration_phys_loc.y + chrome_origin_dy,
            )
                .into();
            let deco_size = smithay::utils::Size::<i32, smithay::utils::Logical>::from(
                (scaled_full_w_log, scaled_full_h_log),
            );
            if let Ok(el) = MemoryRenderBufferRenderElement::from_buffer(
                renderer,
                deco_loc,
                &cb.active_decoration,
                Some(f * o),
                None,
                Some(deco_size),
                Kind::Unspecified,
            ) {
                all_overlays.push(CursorElement::Texture(el));
            }
            if let Ok(el) = MemoryRenderBufferRenderElement::from_buffer(
                renderer,
                deco_loc,
                &cb.inactive_decoration,
                Some((1.0 - f) * o),
                None,
                Some(deco_size),
                Kind::Unspecified,
            ) {
                all_overlays.push(CursorElement::Texture(el));
            }

            // 2. Title bar (SSD only) — single buffer; alpha lerped to
            //    fade the inactive variant down to 0.75 opacity. Same
            //    centre-anchored scale as the decoration.
            if let Some(bar) = title_bar_for_snap.get(snap_i).and_then(|b| *b) {
                let bar_loc: Point<f64, smithay::utils::Physical> = (
                    bar.phys_loc.x + chrome_origin_dx,
                    bar.phys_loc.y + chrome_origin_dy,
                )
                    .into();
                let bar_size = smithay::utils::Size::<i32, smithay::utils::Logical>::from(
                    (scaled_bar_w_log, scaled_bar_h_log),
                );
                if let Ok(el) = MemoryRenderBufferRenderElement::from_buffer(
                    renderer,
                    bar_loc,
                    &bar.buffer,
                    Some(bar.alpha * o),
                    None,
                    Some(bar_size),
                    Kind::Unspecified,
                ) {
                    all_overlays.push(CursorElement::Texture(el));
                }
            }

            // 3. SDF-clipped wayland surface — scale the destination
            //    rect AND the shader uniforms together so the squircle
            //    appears proportionally sized to the (visually shrunk)
            //    window during the open/close animation.
            if let Some(ref prog) = clip_program {
                let surface_offset_y = if snap.is_ssd {
                    title_bar_height_logical * scale.x as f32
                } else {
                    0.0
                };
                let scaled_full_w_phys =
                    snap.full_geo.size.w as f32 * scale.x as f32 * s;
                let scaled_full_h_phys =
                    snap.full_geo.size.h as f32 * scale.y as f32 * s;
                let scaled_surface_offset_y = surface_offset_y * s;
                let scaled_radius_px = radius_px * s;
                let params = crate::render::squircle_clip::ClipParams {
                    window_size_px: (scaled_full_w_phys, scaled_full_h_phys),
                    surface_offset_px: (0.0, scaled_surface_offset_y),
                    radius_px: scaled_radius_px,
                    smoothing,
                    alpha: o,
                };
                // Scale the surface destination from the WINDOW centre
                // (not the content origin), so the title bar + content
                // remain visually aligned.
                let phys_loc = (
                    snap.content_geo.loc.x as f64 * scale.x + chrome_origin_dx,
                    snap.content_geo.loc.y as f64 * scale.y + chrome_origin_dy,
                )
                    .into();
                let dst_size_log = smithay::utils::Size::<i32, smithay::utils::Logical>::from(
                    (
                        (snap.content_geo.size.w as f32 * s).round() as i32,
                        (snap.content_geo.size.h as f32 * s).round() as i32,
                    ),
                );
                if let Some(el) = crate::render::squircle_clip::build_clipped_element_sized(
                    renderer,
                    prog,
                    &snap.window,
                    phys_loc,
                    scale.x,
                    params,
                    Some(dst_size_log),
                ) {
                    all_overlays.push(CursorElement::Clip(el));
                    clipped_count += 1;
                }
            }

            // 4. Shadow halo — crossfade active/inactive layers. Each
            //    variant uses its own physical location because their
            //    paddings differ (active 70 px, inactive ~18 px). The
            //    shadow texture is scaled toward the window centre (not
            //    the texture's own origin) so the halo shrinks with the
            //    window during the open/close animation.
            //
            //    `active_shadow_phys_loc` is `window_top_left -
            //    active_padding`. To scale around the window centre we
            //    pivot at `window_centre` and shrink by `s`. That's
            //    equivalent to: new_origin = window_centre +
            //    (origin - window_centre) * s.
            let win_cx = snap.full_geo.loc.x as f64 * scale.x + full_w_phys * 0.5;
            let win_cy = snap.full_geo.loc.y as f64 * scale.y + full_h_phys * 0.5;
            let scale_around = |p: Point<f64, smithay::utils::Physical>|
                -> Point<f64, smithay::utils::Physical> {
                (
                    win_cx + (p.x - win_cx) * s as f64,
                    win_cy + (p.y - win_cy) * s as f64,
                )
                    .into()
            };
            // Scale the shadow buffer too — the pre-baked interior
            // cutout scales uniformly with the texture, so the halo
            // shrinks/grows with the rest of the window and the
            // open/close animation feels like a single unit. Origin is
            // moved so the texture pivots at the window centre.
            let active_shadow_scaled_log =
                smithay::utils::Size::<i32, smithay::utils::Logical>::from((
                    (cb.active_shadow_size_logical.w as f32 * s).round() as i32,
                    (cb.active_shadow_size_logical.h as f32 * s).round() as i32,
                ));
            let inactive_shadow_scaled_log =
                smithay::utils::Size::<i32, smithay::utils::Logical>::from((
                    (cb.inactive_shadow_size_logical.w as f32 * s).round() as i32,
                    (cb.inactive_shadow_size_logical.h as f32 * s).round() as i32,
                ));
            if let Ok(el) = MemoryRenderBufferRenderElement::from_buffer(
                renderer,
                scale_around(cb.active_shadow_phys_loc),
                &cb.active_shadow,
                Some(f * o),
                None,
                Some(active_shadow_scaled_log),
                Kind::Unspecified,
            ) {
                all_overlays.push(CursorElement::Texture(el));
            }
            if let Ok(el) = MemoryRenderBufferRenderElement::from_buffer(
                renderer,
                scale_around(cb.inactive_shadow_phys_loc),
                &cb.inactive_shadow,
                Some((1.0 - f) * o),
                None,
                Some(inactive_shadow_scaled_log),
                Kind::Unspecified,
            ) {
                all_overlays.push(CursorElement::Texture(el));
            }
        }
        let _ = title_bar_by_geo;

        // Hide the space windows from `render_output` whenever the SDF
        // clip program compiled. We were previously falling back to the
        // rectangular pipeline whenever *any* window failed to build a
        // clipped element (e.g. a brand-new window before its first
        // buffer commit) — but that re-renders ALL windows rectangularly
        // at full opacity *behind* the clipped+animated ones, so the
        // open/close fade was being painted over by the rectangular
        // version on the first frames. Instead just let the
        // not-yet-clipped windows be invisible until their buffer
        // arrives; they'd be at opacity ≈ 0 from the spring anyway.
        let space_ref = &state.common.space;
        let space_slice: Vec<&smithay::desktop::Space<smithay::desktop::Window>> =
            if clip_program.is_some() {
                Vec::new()
            } else {
                vec![space_ref]
            };

        let result = render_output::<_, CursorElement, _, _>(
            &winit.output,
            renderer,
            &mut fb,
            1.0,
            age,
            space_slice.iter().copied(),
            &all_overlays,
            &mut winit.damage_tracker,
            [0.06, 0.06, 0.07, 1.0],
        );

        // Honour any pending screenshot request before submit (the back
        // buffer is what we just drew). Path comes from the IPC handler.
        if let Some(out_path) = state.common.pending_screenshot.take() {
            if let Err(e) = capture_framebuffer(renderer, &fb, &winit.output, &out_path) {
                tracing::warn!("screenshot capture failed: {e}");
            }
        }

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

    // Frame callbacks were already dispatched at the top of this
    // function — moved there so EGL/render failures don't stall
    // clients. See the comment on the early `send_frame` block above.
}

/// Read the just-rendered framebuffer back to CPU memory and write it as
/// a PNG. Used by the in-compositor screenshot path so callers don't have
/// to take a host-side screenshot of the nested winit window.
fn capture_framebuffer(
    renderer: &mut GlesRenderer,
    fb: &smithay::backend::renderer::gles::GlesTarget<'_>,
    output: &smithay::output::Output,
    path: &std::path::Path,
) -> anyhow::Result<()> {
    use smithay::backend::renderer::ExportMem;
    use smithay::utils::{Point, Rectangle, Size};

    let mode = output.current_mode().ok_or_else(|| anyhow::anyhow!("output has no mode"))?;
    let size: Size<i32, smithay::utils::Buffer> = (mode.size.w, mode.size.h).into();
    let region: Rectangle<i32, smithay::utils::Buffer> = Rectangle::new(Point::from((0, 0)), size);

    let mapping = renderer
        .copy_framebuffer(fb, region, Fourcc::Argb8888)
        .map_err(|e| anyhow::anyhow!("copy_framebuffer: {e:?}"))?;
    let bytes = renderer
        .map_texture(&mapping)
        .map_err(|e| anyhow::anyhow!("map_texture: {e:?}"))?;

    // smithay returns BGRA premultiplied; tiny_skia wants RGBA premultiplied.
    let mut rgba = bytes.to_vec();
    for px in rgba.chunks_exact_mut(4) {
        px.swap(0, 2);
    }

    use smithay::backend::renderer::TextureMapping;
    let w = mode.size.w as usize;
    let h = mode.size.h as usize;
    let stride = w * 4;
    if mapping.flipped() {
        let row: Vec<u8> = vec![0; stride];
        let mut tmp = row;
        for y in 0..h / 2 {
            let other = h - 1 - y;
            tmp.copy_from_slice(&rgba[y * stride..y * stride + stride]);
            rgba.copy_within(other * stride..other * stride + stride, y * stride);
            rgba[other * stride..other * stride + stride].copy_from_slice(&tmp);
        }
    }

    let mut pixmap = tiny_skia::Pixmap::new(w as u32, h as u32)
        .ok_or_else(|| anyhow::anyhow!("Pixmap::new {w}x{h}"))?;
    pixmap.data_mut().copy_from_slice(&rgba);
    pixmap.save_png(path)?;
    tracing::info!("screenshot saved to {}", path.display());
    Ok(())
}

/// Last-resort 12×12 white square at the cursor location, used when no
/// real cursor texture is available.
fn fallback_cursor(loc: smithay::utils::Point<i32, smithay::utils::Physical>) -> Vec<CursorElement> {
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
