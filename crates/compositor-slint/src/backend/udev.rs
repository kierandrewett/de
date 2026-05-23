//! Production udev/DRM backend skeleton.
//!
//! This path verifies the first production prerequisites: libseat session
//! creation, udev device discovery, libinput seat assignment, calloop event
//! source registration, and DRM/KMS probing. It intentionally stops before
//! modesetting so the current winit backend remains the only runnable compositor
//! path until the DRM render loop lands.

use anyhow::{bail, Context, Result};
use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use smithay::backend::{
    allocator::{
        gbm::{GbmAllocator, GbmBufferFlags, GbmDevice},
        Fourcc,
    },
    drm::{
        compositor::FrameFlags,
        exporter::gbm::{GbmFramebufferExporter, NodeFilter},
        output::{DrmOutputManager, DrmOutputRenderElements},
        DrmDevice, DrmDeviceFd, DrmEvent, DrmNode, NodeType,
    },
    egl::{context::ContextPriority, EGLContext, EGLDisplay},
    input::{
        AbsolutePositionEvent, Axis, Device as InputDevice, DeviceCapability,
        Event as InputBackendEvent, GestureBeginEvent, GestureEndEvent,
        GesturePinchUpdateEvent as _, GestureSwipeUpdateEvent as _, InputEvent, KeyboardKeyEvent,
        PointerAxisEvent, PointerButtonEvent, PointerMotionEvent, TouchEvent as _,
    },
    libinput::{LibinputInputBackend, LibinputSessionInterface},
    renderer::{element::solid::SolidColorRenderElement, gles::GlesRenderer},
    session::{libseat::LibSeatSession, Event as SessionEvent, Session},
    udev::{primary_gpu, UdevBackend, UdevEvent},
};
use smithay::input::pointer::{AxisFrame, ButtonEvent, MotionEvent};
use smithay::output::{Mode as WlMode, Output, PhysicalProperties};
use smithay::reexports::{
    drm::control::{connector, crtc, Device as ControlDevice, Mode, ModeTypeFlags},
    input::Libinput,
    rustix::fs::OFlags,
};
use smithay::utils::{DeviceFd, Point, SERIAL_COUNTER};
use tracing::info;

use crate::{wayland_runtime::WaylandRuntime, wayland_state::SpikeState};

type ProbeAllocator = GbmAllocator<DrmDeviceFd>;
type ProbeFramebufferExporter = GbmFramebufferExporter<DrmDeviceFd>;
type ProbeOutputManager =
    DrmOutputManager<ProbeAllocator, ProbeFramebufferExporter, (), DrmDeviceFd>;

pub fn run() -> Result<()> {
    let mut runtime = UdevRuntime::new()?;
    runtime.open_drm_devices()?;
    runtime.drain_hotplug_events()?;
    let device_count = runtime.drm_devices.len();

    if std::env::var("DE_COMPOSITOR_UDEV_CLEAR").as_deref() == Ok("1") {
        runtime.queue_clear_frame_once()?;
    } else {
        info!("udev backend: clear-screen pageflip disabled; set DE_COMPOSITOR_UDEV_CLEAR=1 to try it");
    }

    bail!(
        "udev/DRM backend probe succeeded ({count} DRM device(s)); DRM/KMS modesetting is not implemented yet, run with --backend=winit",
        count = device_count,
    )
}

struct UdevRuntime {
    wayland: WaylandRuntime,
    session: LibSeatSession,
    seat_name: String,
    device_snapshot: Vec<DrmDeviceSnapshot>,
    hotplug_events: Arc<Mutex<VecDeque<UdevHotplugEvent>>>,
    drm_devices: Vec<DrmProbeDevice>,
}

#[derive(Clone)]
struct DrmDeviceSnapshot {
    device_id: libc::dev_t,
    path: PathBuf,
}

struct DrmProbeDevice {
    node: DrmNode,
    path: PathBuf,
    drm: DrmDevice,
    render: RenderProbe,
    outputs: Vec<KmsProbeOutput>,
}

struct RenderProbe {
    gbm: GbmDevice<DrmDeviceFd>,
    renderer: GlesRenderer,
}

#[derive(Clone)]
struct KmsProbeOutput {
    connector: connector::Handle,
    crtc: crtc::Handle,
    mode: Mode,
    output: Output,
}

enum UdevHotplugEvent {
    Added {
        device_id: libc::dev_t,
        path: PathBuf,
    },
    Changed {
        device_id: libc::dev_t,
    },
    Removed {
        device_id: libc::dev_t,
    },
}

impl UdevRuntime {
    fn new() -> Result<Self> {
        let (session, notifier) =
            LibSeatSession::new().context("failed to create libseat session for udev backend")?;
        let seat_name = session.seat();
        info!(seat = %seat_name, "udev backend: libseat session ready");

        let wayland = WaylandRuntime::new().context("failed to create udev Wayland runtime")?;

        let udev_backend =
            UdevBackend::new(&seat_name).context("failed to create Smithay udev backend")?;
        let hotplug_events = Arc::new(Mutex::new(VecDeque::new()));
        let device_snapshot: Vec<_> = udev_backend
            .device_list()
            .map(|(device_id, path)| DrmDeviceSnapshot {
                device_id,
                path: path.to_path_buf(),
            })
            .collect();
        for device in &device_snapshot {
            info!(device_id = ?device.device_id, path = %device.path.display(), "udev backend: discovered DRM device");
        }

        let mut libinput_context = Libinput::new_with_udev::<
            LibinputSessionInterface<LibSeatSession>,
        >(session.clone().into());
        if libinput_context.udev_assign_seat(&seat_name).is_err() {
            bail!("failed to assign libinput context to session seat {seat_name:?}");
        }
        let libinput_backend = LibinputInputBackend::new(libinput_context);
        info!(seat = %seat_name, "udev backend: libinput seat assigned");

        wayland
            .event_loop
            .handle()
            .insert_source(
                notifier,
                |event, &mut (), _state: &mut SpikeState| match event {
                    SessionEvent::PauseSession => info!("udev backend: session paused"),
                    SessionEvent::ActivateSession => info!("udev backend: session activated"),
                },
            )
            .map_err(|err| {
                anyhow::anyhow!("failed to register libseat session notifier: {err:?}")
            })?;

        let hotplug_events_for_udev = hotplug_events.clone();
        wayland
            .event_loop
            .handle()
            .insert_source(
                udev_backend,
                move |event, _devices, _state: &mut SpikeState| match event {
                    UdevEvent::Added { device_id, path } => {
                        info!(?device_id, path = %path.display(), "udev backend: DRM device added");
                        hotplug_events_for_udev
                            .lock()
                            .unwrap()
                            .push_back(UdevHotplugEvent::Added { device_id, path });
                    }
                    UdevEvent::Changed { device_id } => {
                        info!(?device_id, "udev backend: DRM device changed");
                        hotplug_events_for_udev
                            .lock()
                            .unwrap()
                            .push_back(UdevHotplugEvent::Changed { device_id });
                    }
                    UdevEvent::Removed { device_id } => {
                        info!(?device_id, "udev backend: DRM device removed");
                        hotplug_events_for_udev
                            .lock()
                            .unwrap()
                            .push_back(UdevHotplugEvent::Removed { device_id });
                    }
                },
            )
            .map_err(|err| anyhow::anyhow!("failed to register udev monitor: {err:?}"))?;

        wayland
            .event_loop
            .handle()
            .insert_source(
                libinput_backend,
                |event, &mut (), state: &mut SpikeState| {
                    handle_libinput_input_event(event, state);
                },
            )
            .map_err(|err| anyhow::anyhow!("failed to register libinput backend: {err:?}"))?;
        info!("udev backend: calloop event sources registered");

        Ok(Self {
            wayland,
            session,
            seat_name,
            device_snapshot,
            hotplug_events,
            drm_devices: Vec::new(),
        })
    }

    fn open_drm_devices(&mut self) -> Result<()> {
        if self.device_snapshot.is_empty() {
            bail!(
                "udev backend did not discover any DRM devices on seat {:?}",
                self.seat_name
            );
        }

        let preferred_device_id = self.preferred_device_id();
        let mut devices = self.device_snapshot.clone();
        if let Some(preferred_device_id) = preferred_device_id {
            devices.sort_by_key(|device| device.device_id != preferred_device_id);
            info!(
                ?preferred_device_id,
                "udev backend: preferred DRM device selected"
            );
        }

        for device in devices {
            self.probe_drm_device(device)?;
        }

        self.log_open_devices();
        self.advertise_dmabuf_global();

        Ok(())
    }

    fn probe_drm_device(&mut self, device: DrmDeviceSnapshot) -> Result<()> {
        if self
            .drm_devices
            .iter()
            .any(|existing| existing.node.dev_id() == device.device_id)
        {
            info!(device_id = ?device.device_id, "udev backend: DRM device already probed");
            return Ok(());
        }

        let node = DrmNode::from_dev_id(device.device_id).map_err(|err| {
            anyhow::anyhow!("failed to resolve DRM node {:?}: {err:?}", device.device_id)
        })?;
        let (drm, render) = self.open_drm_device(node, device.path.clone())?;
        self.log_kms_state(&drm)?;
        let outputs = self.select_kms_outputs(&drm)?;
        self.register_wayland_outputs(&outputs);
        self.drm_devices.push(DrmProbeDevice {
            node,
            path: device.path,
            drm,
            render,
            outputs,
        });

        Ok(())
    }

    fn drain_hotplug_events(&mut self) -> Result<()> {
        loop {
            let event = self.hotplug_events.lock().unwrap().pop_front();
            let Some(event) = event else { break };

            match event {
                UdevHotplugEvent::Added { device_id, path } => {
                    self.probe_drm_device(DrmDeviceSnapshot { device_id, path })?;
                    self.advertise_dmabuf_global();
                }
                UdevHotplugEvent::Changed { device_id } => {
                    info!(
                        ?device_id,
                        "udev backend: DRM change event queued for future modeset refresh"
                    );
                }
                UdevHotplugEvent::Removed { device_id } => {
                    self.drm_devices
                        .retain(|device| device.node.dev_id() != device_id);
                    info!(
                        ?device_id,
                        "udev backend: DRM device removed from probe list"
                    );
                }
            }
        }

        Ok(())
    }

    fn register_wayland_outputs(&mut self, outputs: &[KmsProbeOutput]) {
        for output in outputs {
            output
                .output
                .create_global::<SpikeState>(&self.wayland.display_handle);
            self.wayland.state.register_output(output.output.clone());
            info!(
                output = %output.output.name(),
                connector = ?output.connector,
                crtc = ?output.crtc,
                "udev backend: registered Wayland output global"
            );
        }
    }

    fn advertise_dmabuf_global(&mut self) {
        let Some(device) = self.drm_devices.first() else {
            return;
        };
        let formats: Vec<_> = device
            .render
            .renderer
            .egl_context()
            .dmabuf_render_formats()
            .iter()
            .copied()
            .collect();
        let format_count = formats.len();
        let _dmabuf_global = self
            .wayland
            .state
            .dmabuf_state
            .create_global::<SpikeState>(&self.wayland.display_handle, formats);
        info!(
            format_count,
            "udev backend: DMA-BUF global advertised from EGL renderer formats"
        );
    }

    fn preferred_device_id(&self) -> Option<libc::dev_t> {
        let node = std::env::var("DE_DRM_DEVICE")
            .ok()
            .and_then(|path| DrmNode::from_path(path).ok())
            .or_else(|| {
                primary_gpu(&self.seat_name)
                    .ok()
                    .flatten()
                    .and_then(|path| DrmNode::from_path(path).ok())
            })?;

        node.node_with_type(NodeType::Primary)
            .and_then(|node| node.ok())
            .unwrap_or(node)
            .dev_id()
            .into()
    }

    fn open_drm_device(
        &mut self,
        node: DrmNode,
        path: PathBuf,
    ) -> Result<(DrmDevice, RenderProbe)> {
        info!(?node, path = %path.display(), "udev backend: opening DRM device");
        let fd = self
            .session
            .open(
                &path,
                OFlags::RDWR | OFlags::CLOEXEC | OFlags::NOCTTY | OFlags::NONBLOCK,
            )
            .map_err(|err| {
                anyhow::anyhow!("failed to open DRM device {}: {err:?}", path.display())
            })?;
        let fd = DrmDeviceFd::new(DeviceFd::from(fd));
        let render = Self::create_render_probe(fd.clone(), &path)?;
        let (drm, notifier) = DrmDevice::new(fd, true).map_err(|err| {
            anyhow::anyhow!(
                "failed to initialize DRM device {}: {err:?}",
                path.display()
            )
        })?;

        self.wayland
            .event_loop
            .handle()
            .insert_source(
                notifier,
                move |event, metadata, _state: &mut SpikeState| match event {
                    DrmEvent::VBlank(crtc) => {
                        info!(?node, ?crtc, ?metadata, "udev backend: DRM vblank");
                    }
                    DrmEvent::Error(error) => {
                        info!(?node, ?error, "udev backend: DRM event error");
                    }
                },
            )
            .map_err(|err| {
                anyhow::anyhow!(
                    "failed to register DRM notifier for {}: {err:?}",
                    path.display()
                )
            })?;

        Ok((drm, render))
    }

    fn create_render_probe(fd: DrmDeviceFd, path: &std::path::Path) -> Result<RenderProbe> {
        let gbm = GbmDevice::new(fd).map_err(|err| {
            anyhow::anyhow!(
                "failed to create GBM device for {}: {err:?}",
                path.display()
            )
        })?;
        let display = unsafe { EGLDisplay::new(gbm.clone()) }.map_err(|err| {
            anyhow::anyhow!(
                "failed to create EGL display for {}: {err:?}",
                path.display()
            )
        })?;
        let context =
            EGLContext::new_with_priority(&display, ContextPriority::High).map_err(|err| {
                anyhow::anyhow!(
                    "failed to create EGL context for {}: {err:?}",
                    path.display()
                )
            })?;
        let renderer = unsafe { GlesRenderer::new(context) }.map_err(|err| {
            anyhow::anyhow!(
                "failed to create GLES renderer for {}: {err:?}",
                path.display()
            )
        })?;

        info!(path = %path.display(), "udev backend: GBM/EGL/GLES render probe ready");
        Ok(RenderProbe { gbm, renderer })
    }

    fn log_kms_state(&self, drm: &DrmDevice) -> Result<()> {
        let resources = drm
            .resource_handles()
            .context("failed to query DRM resource handles")?;

        info!(
            device_id = ?drm.device_id(),
            atomic = drm.is_atomic(),
            crtcs = resources.crtcs().len(),
            connectors = resources.connectors().len(),
            "udev backend: DRM resources"
        );

        for connector_handle in resources.connectors() {
            let connector = drm
                .get_connector(*connector_handle, true)
                .with_context(|| format!("failed to query connector {connector_handle:?}"))?;
            self.log_connector(&connector);
        }

        Ok(())
    }

    fn log_connector(&self, connector: &connector::Info) {
        let connected = connector.state() == connector::State::Connected;
        info!(
            connector = %connector,
            handle = ?connector.handle(),
            state = ?connector.state(),
            connected,
            modes = connector.modes().len(),
            "udev backend: DRM connector"
        );

        for mode in connector.modes() {
            let (width, height) = mode.size();
            let preferred = mode.mode_type().contains(ModeTypeFlags::PREFERRED);
            info!(
                connector = %connector,
                mode = %mode.name().to_string_lossy(),
                width,
                height,
                refresh_millihz = mode.vrefresh(),
                preferred,
                "udev backend: DRM mode"
            );
        }
    }

    fn select_kms_outputs(&self, drm: &DrmDevice) -> Result<Vec<KmsProbeOutput>> {
        let resources = drm
            .resource_handles()
            .context("failed to query DRM resource handles")?;
        let mut outputs = Vec::new();

        for connector_handle in resources.connectors() {
            let connector = drm
                .get_connector(*connector_handle, true)
                .with_context(|| format!("failed to query connector {connector_handle:?}"))?;
            if connector.state() != connector::State::Connected || connector.modes().is_empty() {
                continue;
            }

            let Some(crtc) = self.select_crtc(drm, &resources, &connector)? else {
                info!(connector = %connector, "udev backend: connected connector has no compatible CRTC");
                continue;
            };

            let mode = connector
                .modes()
                .iter()
                .copied()
                .find(|mode| mode.mode_type().contains(ModeTypeFlags::PREFERRED))
                .unwrap_or(connector.modes()[0]);

            let (width, height) = mode.size();
            info!(
                connector = %connector,
                connector_handle = ?connector.handle(),
                ?crtc,
                mode = %mode.name().to_string_lossy(),
                width,
                height,
                refresh_millihz = mode.vrefresh(),
                "udev backend: selected KMS output candidate"
            );

            let output = self.create_wayland_output(&connector, mode, outputs.len());

            outputs.push(KmsProbeOutput {
                connector: connector.handle(),
                crtc,
                mode,
                output,
            });
        }

        Ok(outputs)
    }

    fn create_wayland_output(
        &self,
        connector: &connector::Info,
        mode: Mode,
        output_index: usize,
    ) -> Output {
        let (phys_w, phys_h) = connector.size().unwrap_or((0, 0));
        let output = Output::new(
            connector.to_string(),
            PhysicalProperties {
                size: (phys_w as i32, phys_h as i32).into(),
                subpixel: connector.subpixel().into(),
                make: "Unknown".into(),
                model: connector.interface().as_str().into(),
                serial_number: "Unknown".into(),
            },
        );
        let wl_mode = WlMode::from(mode);
        let (width, _) = mode.size();
        let x = i32::from(width) * output_index as i32;
        output.set_preferred(wl_mode);
        output.change_current_state(Some(wl_mode), None, None, Some((x, 0).into()));
        output
    }

    fn select_crtc(
        &self,
        drm: &DrmDevice,
        resources: &smithay::reexports::drm::control::ResourceHandles,
        connector: &connector::Info,
    ) -> Result<Option<crtc::Handle>> {
        if let Some(encoder_handle) = connector.current_encoder() {
            let encoder = drm
                .get_encoder(encoder_handle)
                .with_context(|| format!("failed to query current encoder {encoder_handle:?}"))?;
            if let Some(crtc) = encoder.crtc() {
                return Ok(Some(crtc));
            }
        }

        for encoder_handle in connector.encoders() {
            let encoder = drm
                .get_encoder(*encoder_handle)
                .with_context(|| format!("failed to query encoder {encoder_handle:?}"))?;
            if let Some(crtc) = resources.filter_crtcs(encoder.possible_crtcs()).first() {
                return Ok(Some(*crtc));
            }
        }

        Ok(None)
    }

    fn log_open_devices(&self) {
        for device in &self.drm_devices {
            let _render_probe = (&device.render.gbm, &device.render.renderer);
            info!(
                node = ?device.node,
                path = %device.path.display(),
                device_id = ?device.drm.device_id(),
                outputs = device.outputs.len(),
                "udev backend: DRM probe device ready"
            );

            for output in &device.outputs {
                let (width, height) = output.mode.size();
                info!(
                    node = ?device.node,
                    connector = ?output.connector,
                    crtc = ?output.crtc,
                    output = %output.output.name(),
                    mode = %output.mode.name().to_string_lossy(),
                    width,
                    height,
                    refresh_millihz = output.mode.vrefresh(),
                    "udev backend: KMS output candidate ready"
                );
            }
        }
    }

    fn queue_clear_frame_once(&mut self) -> Result<()> {
        let Some(mut device) = self.drm_devices.pop() else {
            bail!("cannot queue clear frame without an opened DRM device");
        };
        let Some(output) = device.outputs.first().cloned() else {
            bail!("cannot queue clear frame without a connected KMS output");
        };

        let allocator = GbmAllocator::new(
            device.render.gbm.clone(),
            GbmBufferFlags::RENDERING | GbmBufferFlags::SCANOUT,
        );
        let exporter = GbmFramebufferExporter::new(device.render.gbm.clone(), NodeFilter::None);
        let renderer_formats = device
            .render
            .renderer
            .egl_context()
            .dmabuf_render_formats()
            .iter()
            .copied();
        let mut output_manager = ProbeOutputManager::new(
            device.drm,
            allocator,
            exporter,
            Some(device.render.gbm.clone()),
            [Fourcc::Argb8888, Fourcc::Xrgb8888],
            renderer_formats,
        );
        let render_elements =
            DrmOutputRenderElements::<GlesRenderer, SolidColorRenderElement>::new();

        let mut drm_output = output_manager
            .lock()
            .initialize_output(
                output.crtc,
                output.mode,
                &[output.connector],
                &output.output,
                None,
                &mut device.render.renderer,
                &render_elements,
            )
            .map_err(|err| anyhow::anyhow!("failed to initialize DRM output: {err:?}"))?;

        let empty: [SolidColorRenderElement; 0] = [];
        drm_output
            .render_frame(
                &mut device.render.renderer,
                &empty,
                [0.02, 0.025, 0.035, 1.0],
                FrameFlags::DEFAULT,
            )
            .map_err(|err| anyhow::anyhow!("failed to render clear frame: {err:?}"))?;
        drm_output
            .queue_frame(())
            .map_err(|err| anyhow::anyhow!("failed to queue clear frame: {err:?}"))?;

        info!(
            node = ?device.node,
            connector = ?output.connector,
            crtc = ?output.crtc,
            "udev backend: queued one clear-screen DRM frame"
        );
        Ok(())
    }
}

fn handle_libinput_input_event(event: InputEvent<LibinputInputBackend>, state: &mut SpikeState) {
    // Reset the ext-idle-notify-v1 timers on any user input — without this
    // the screen-locker fires while the user is actively typing/clicking
    // (cosmic-comp/src/input/mod.rs:212 mirrors this pattern). Cheap: a
    // `Vec<IdleNotification>` walk per event.
    state.idle_notifier_state.notify_activity(&state.seat);
    match event {
        InputEvent::Keyboard { event } => {
            let surface = if state.session_locked {
                state
                    .lock_surfaces
                    .first()
                    .map(|li| li.surface.wl_surface().clone())
            } else {
                state
                    .exclusive_keyboard_layer()
                    .cloned()
                    .or_else(|| state.active_surface.clone())
            };
            let Some(surface) = surface else { return };
            let Some(keyboard) = state.seat.get_keyboard() else {
                return;
            };

            // Only re-issue set_focus when the target actually changed.
            // Per-keystroke set_focus calls re-send wl_keyboard.enter/leave
            // and bump modifier serials (input audit P0.7) — clients see a
            // serial storm that interacts badly with grabs.
            let focus = crate::wayland::xwayland::KeyboardFocusTarget::for_wl_surface(
                state, &surface,
            );
            if !keyboard
                .current_focus()
                .as_ref()
                .is_some_and(|current| current.matches_wl_surface(&surface))
            {
                keyboard.set_focus(state, Some(focus), SERIAL_COUNTER.next_serial());
            }
            keyboard.input_forward(
                state,
                event.key_code(),
                event.state(),
                SERIAL_COUNTER.next_serial(),
                event.time_msec(),
                false,
            );
        }
        InputEvent::PointerMotion { event } => {
            let Some(pointer) = state.seat.get_pointer() else {
                return;
            };
            state.pointer_pos.0 += event.delta_x();
            state.pointer_pos.1 += event.delta_y();
            state.pointer_pos.0 = state.pointer_pos.0.max(0.0);
            state.pointer_pos.1 = state.pointer_pos.1.max(0.0);

            pointer.motion(
                state,
                None,
                &MotionEvent {
                    location: Point::from(state.pointer_pos),
                    serial: SERIAL_COUNTER.next_serial(),
                    time: event.time_msec(),
                },
            );
            pointer.frame(state);
        }
        InputEvent::PointerButton { event } => {
            let Some(pointer) = state.seat.get_pointer() else {
                return;
            };
            pointer.button(
                state,
                &ButtonEvent {
                    serial: SERIAL_COUNTER.next_serial(),
                    time: event.time_msec(),
                    button: event.button_code(),
                    state: event.state(),
                },
            );
            pointer.frame(state);
        }
        InputEvent::PointerAxis { event } => {
            let Some(pointer) = state.seat.get_pointer() else {
                return;
            };
            let mut frame = AxisFrame::new(event.time_msec()).source(event.source());
            // Finger-source events deliver a zero-delta "stop" frame when the
            // finger lifts off the touchpad. Without emitting `axis_stop` to
            // clients, kinetic scrolling never settles — every smooth-scroll
            // surface in GTK/Qt keeps the bottom-out indicator drawn.
            let is_finger = event.source() == smithay::backend::input::AxisSource::Finger;
            for axis in [Axis::Horizontal, Axis::Vertical] {
                if let Some(value) = event.amount(axis) {
                    if value != 0.0 {
                        frame = frame.value(axis, value);
                    } else if is_finger {
                        // Zero amount on a finger event = lift-off on this axis.
                        frame = frame.stop(axis);
                    }
                }
                if let Some(v120) = event.amount_v120(axis) {
                    if v120 != 0.0 {
                        frame = frame.v120(axis, v120 as i32);
                    }
                }
                frame = frame.relative_direction(axis, event.relative_direction(axis));
            }
            pointer.axis(state, frame);
            pointer.frame(state);
        }
        // ─── Touchpad gestures (pointer-gestures-unstable-v1) ────────────
        // Three- and four-finger swipes, pinches, holds. Browsers, Wayfire
        // panels, KDE's overview gesture, GNOME's "swipe up to overview" all
        // consume these. Anvil/input_handler.rs:1054-1154 is the reference.
        InputEvent::GestureSwipeBegin { event } => {
            let Some(pointer) = state.seat.get_pointer() else {
                return;
            };
            pointer.gesture_swipe_begin(
                state,
                &smithay::input::pointer::GestureSwipeBeginEvent {
                    serial: SERIAL_COUNTER.next_serial(),
                    time: event.time_msec(),
                    fingers: event.fingers(),
                },
            );
        }
        InputEvent::GestureSwipeUpdate { event } => {
            let Some(pointer) = state.seat.get_pointer() else {
                return;
            };
            pointer.gesture_swipe_update(
                state,
                &smithay::input::pointer::GestureSwipeUpdateEvent {
                    time: event.time_msec(),
                    delta: event.delta(),
                },
            );
        }
        InputEvent::GestureSwipeEnd { event } => {
            let Some(pointer) = state.seat.get_pointer() else {
                return;
            };
            pointer.gesture_swipe_end(
                state,
                &smithay::input::pointer::GestureSwipeEndEvent {
                    serial: SERIAL_COUNTER.next_serial(),
                    time: event.time_msec(),
                    cancelled: event.cancelled(),
                },
            );
        }
        InputEvent::GesturePinchBegin { event } => {
            let Some(pointer) = state.seat.get_pointer() else {
                return;
            };
            pointer.gesture_pinch_begin(
                state,
                &smithay::input::pointer::GesturePinchBeginEvent {
                    serial: SERIAL_COUNTER.next_serial(),
                    time: event.time_msec(),
                    fingers: event.fingers(),
                },
            );
        }
        InputEvent::GesturePinchUpdate { event } => {
            let Some(pointer) = state.seat.get_pointer() else {
                return;
            };
            pointer.gesture_pinch_update(
                state,
                &smithay::input::pointer::GesturePinchUpdateEvent {
                    time: event.time_msec(),
                    delta: event.delta(),
                    scale: event.scale(),
                    rotation: event.rotation(),
                },
            );
        }
        InputEvent::GesturePinchEnd { event } => {
            let Some(pointer) = state.seat.get_pointer() else {
                return;
            };
            pointer.gesture_pinch_end(
                state,
                &smithay::input::pointer::GesturePinchEndEvent {
                    serial: SERIAL_COUNTER.next_serial(),
                    time: event.time_msec(),
                    cancelled: event.cancelled(),
                },
            );
        }
        InputEvent::GestureHoldBegin { event } => {
            let Some(pointer) = state.seat.get_pointer() else {
                return;
            };
            pointer.gesture_hold_begin(
                state,
                &smithay::input::pointer::GestureHoldBeginEvent {
                    serial: SERIAL_COUNTER.next_serial(),
                    time: event.time_msec(),
                    fingers: event.fingers(),
                },
            );
        }
        InputEvent::GestureHoldEnd { event } => {
            let Some(pointer) = state.seat.get_pointer() else {
                return;
            };
            pointer.gesture_hold_end(
                state,
                &smithay::input::pointer::GestureHoldEndEvent {
                    serial: SERIAL_COUNTER.next_serial(),
                    time: event.time_msec(),
                    cancelled: event.cancelled(),
                },
            );
        }
        InputEvent::TouchDown { event } => {
            let Some(touch) = state.seat.get_touch() else {
                return;
            };
            let Some(loc) = libinput_touch_location(state, &event) else {
                return;
            };
            // Touch acts like a click for keyboard focus: bring the surface
            // under the contact point to the top.
            let surface_at = surface_under_for_touch(state, loc.x, loc.y);
            if let Some((surface, _, _)) = surface_at.as_ref() {
                if let Some(kb) = state.seat.get_keyboard() {
                    let focus = crate::wayland::xwayland::KeyboardFocusTarget::for_wl_surface(
                        state, surface,
                    );
                    kb.set_focus(state, Some(focus), SERIAL_COUNTER.next_serial());
                }
            }
            let under = surface_at.map(|(s, ox, oy)| (s, Point::from((ox, oy))));
            touch.down(
                state,
                under,
                &smithay::input::touch::DownEvent {
                    slot: event.slot(),
                    location: loc,
                    serial: SERIAL_COUNTER.next_serial(),
                    time: event.time_msec(),
                },
            );
        }
        InputEvent::TouchMotion { event } => {
            let Some(touch) = state.seat.get_touch() else {
                return;
            };
            let Some(loc) = libinput_touch_location(state, &event) else {
                return;
            };
            let under = surface_under_for_touch(state, loc.x, loc.y)
                .map(|(s, ox, oy)| (s, Point::from((ox, oy))));
            touch.motion(
                state,
                under,
                &smithay::input::touch::MotionEvent {
                    slot: event.slot(),
                    location: loc,
                    time: event.time_msec(),
                },
            );
        }
        InputEvent::TouchUp { event } => {
            let Some(touch) = state.seat.get_touch() else {
                return;
            };
            touch.up(
                state,
                &smithay::input::touch::UpEvent {
                    slot: event.slot(),
                    serial: SERIAL_COUNTER.next_serial(),
                    time: event.time_msec(),
                },
            );
        }
        InputEvent::TouchFrame { .. } => {
            if let Some(touch) = state.seat.get_touch() {
                touch.frame(state);
            }
        }
        InputEvent::TouchCancel { .. } => {
            if let Some(touch) = state.seat.get_touch() {
                touch.cancel(state);
            }
        }
        InputEvent::DeviceAdded { device } => {
            if InputDevice::has_capability(&device, DeviceCapability::Touch)
                && state.seat.get_touch().is_none()
            {
                state.seat.add_touch();
            }
            info!(?device, "udev backend: input device added");
        }
        InputEvent::DeviceRemoved { device } => {
            info!(?device, "udev backend: input device removed");
        }
        other => info!(?other, "udev backend: input event observed"),
    }
}

/// Map a libinput absolute-position event (normalised 0..1) into the
/// compositor's logical-pixel coordinate space using the primary output's
/// current mode + scale. Returns None until we have a registered output.
fn libinput_touch_location<E>(
    state: &SpikeState,
    event: &E,
) -> Option<Point<f64, smithay::utils::Logical>>
where
    E: AbsolutePositionEvent<LibinputInputBackend>,
{
    let output = state.primary_output()?;
    let mode = output.current_mode()?;
    let scale = output.current_scale().fractional_scale();
    let logical_w = mode.size.w as f64 / scale;
    let logical_h = mode.size.h as f64 / scale;
    Some(Point::from((
        event.x_transformed(logical_w as i32),
        event.y_transformed(logical_h as i32),
    )))
}

/// Surface lookup for touch events. Honours the session lock the same way the
/// renderer's `forward_pointer_motion` does — touches go ONLY to the lock
/// surface while locked.
fn surface_under_for_touch(
    state: &SpikeState,
    x: f64,
    y: f64,
) -> Option<(
    smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
    f64,
    f64,
)> {
    if state.session_locked {
        return state
            .lock_surfaces
            .first()
            .map(|li| (li.surface.wl_surface().clone(), 0.0, 0.0));
    }
    // Best-effort: the udev backend currently has no access to the WM's
    // surface_under since the renderer owns the WindowManager. Fall back to
    // "topmost toplevel" — good enough for touch-to-focus on a foreground
    // window. The full surface tree walk lives in WindowManager::surface_under,
    // which the production touch path will need once the udev backend
    // actually drives a frame loop.
    let _ = (x, y);
    state
        .toplevels
        .last()
        .map(|tl| (tl.surface.clone(), 0.0, 0.0))
}
