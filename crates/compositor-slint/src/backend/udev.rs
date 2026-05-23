//! Production udev/DRM backend bootstrap.
//!
//! This path owns the first hardware-backend lifecycle stages: libseat session
//! creation, udev device discovery, libinput seat assignment, calloop event
//! source registration, and DRM/KMS probing. The KMS render loop is still a
//! follow-up, but the backend now keeps the compositor event loop alive instead
//! of exiting after probe.

use anyhow::{bail, Context, Result};
use std::{
    cell::RefCell,
    collections::VecDeque,
    path::PathBuf,
    rc::Rc,
    sync::{Arc, Mutex},
    time::Duration,
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
use smithay::input::pointer::{AxisFrame, ButtonEvent};
use smithay::output::{Mode as WlMode, Output, PhysicalProperties};
use smithay::reexports::{
    calloop::RegistrationToken,
    drm::control::{connector, crtc, Device as ControlDevice, Mode, ModeTypeFlags},
    input::Libinput,
    rustix::fs::OFlags,
    wayland_server::backend::GlobalId,
};
use smithay::utils::{DeviceFd, Point, SERIAL_COUNTER};
use tracing::{error, info, warn};

use crate::{renderer::input_util, wayland_runtime::WaylandRuntime, wayland_state::SpikeState};

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

    info!(
        count = device_count,
        "udev backend: DRM probe succeeded; entering compositor event loop"
    );
    runtime.run_event_loop()
}

struct UdevRuntime {
    wayland: WaylandRuntime,
    session: LibSeatSession,
    seat_name: String,
    libinput_context: Rc<RefCell<Libinput>>,
    session_events: Arc<Mutex<VecDeque<UdevSessionEvent>>>,
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
    registration_token: RegistrationToken,
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
    global: Option<GlobalId>,
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

enum UdevSessionEvent {
    Pause,
    Activate,
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
        let session_events = Arc::new(Mutex::new(VecDeque::new()));
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
        let libinput_backend = LibinputInputBackend::new(libinput_context.clone());
        let libinput_context = Rc::new(RefCell::new(libinput_context));
        info!(seat = %seat_name, "udev backend: libinput seat assigned");

        let session_events_for_libseat = session_events.clone();
        wayland
            .event_loop
            .handle()
            .insert_source(
                notifier,
                move |event, &mut (), _state: &mut SpikeState| match event {
                    SessionEvent::PauseSession => {
                        info!("udev backend: session pause requested");
                        if let Ok(mut events) = session_events_for_libseat.lock() {
                            events.push_back(UdevSessionEvent::Pause);
                        } else {
                            error!("udev backend: session event queue lock poisoned on pause");
                        }
                    }
                    SessionEvent::ActivateSession => {
                        info!("udev backend: session activation requested");
                        if let Ok(mut events) = session_events_for_libseat.lock() {
                            events.push_back(UdevSessionEvent::Activate);
                        } else {
                            error!("udev backend: session event queue lock poisoned on activate");
                        }
                    }
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
            libinput_context,
            session_events,
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
        let (drm, registration_token, render) = self.open_drm_device(node, device.path.clone())?;
        self.log_kms_state(&drm)?;
        let output_offset = self.wayland.state.outputs.len();
        let mut outputs = Self::select_kms_outputs(&drm, output_offset)?;
        self.register_wayland_outputs(&mut outputs);
        self.drm_devices.push(DrmProbeDevice {
            node,
            path: device.path,
            drm,
            registration_token,
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
                    self.refresh_drm_device(device_id)?;
                }
                UdevHotplugEvent::Removed { device_id } => {
                    self.remove_drm_device(device_id);
                }
            }
        }

        Ok(())
    }

    fn drain_session_events(&mut self) -> Result<()> {
        loop {
            let event = self.session_events.lock().unwrap().pop_front();
            let Some(event) = event else { break };

            match event {
                UdevSessionEvent::Pause => self.pause_session(),
                UdevSessionEvent::Activate => self.resume_session()?,
            }
        }

        Ok(())
    }

    fn pause_session(&mut self) {
        info!("udev backend: pausing session devices");
        self.libinput_context.borrow().suspend();

        for device in &mut self.drm_devices {
            device.drm.pause();
            info!(
                node = ?device.node,
                path = %device.path.display(),
                "udev backend: DRM device paused and master released"
            );
        }
    }

    fn resume_session(&mut self) -> Result<()> {
        info!("udev backend: resuming session devices");
        if let Err(err) = self.libinput_context.borrow_mut().resume() {
            warn!(?err, "udev backend: failed to resume libinput context");
        }

        for device in &mut self.drm_devices {
            // TODO(kms): If activation fails, close and reopen the device through
            // libseat, then rebuild render resources. COSMIC's `reopen_device`
            // path is the reference for that full recovery step.
            device.drm.activate(false).map_err(|err| {
                anyhow::anyhow!(
                    "failed to reactivate DRM device {} after session resume: {err:?}",
                    device.path.display()
                )
            })?;
            info!(
                node = ?device.node,
                path = %device.path.display(),
                "udev backend: DRM device reactivated and master reacquired"
            );
        }

        Ok(())
    }

    fn run_event_loop(&mut self) -> Result<()> {
        info!(
            socket = ?self.wayland.socket_name,
            "udev backend: Wayland event loop running; KMS rendering remains TODO"
        );

        // TODO(kms): Replace this dispatch-only loop with persistent
        // `DrmOutputManager`/`DrmOutput` rendering, page-flip completion, frame
        // callbacks, and output-local presentation feedback.
        while !self.wayland.state.should_exit {
            self.drain_session_events()?;
            self.drain_hotplug_events()?;
            self.wayland
                .event_loop
                .dispatch(Some(Duration::from_millis(16)), &mut self.wayland.state)
                .map_err(|err| {
                    anyhow::anyhow!("udev backend event loop dispatch failed: {err:?}")
                })?;
            self.drain_session_events()?;
            self.drain_hotplug_events()?;
            if let Err(err) = self.wayland.display_handle.flush_clients() {
                error!(?err, "udev backend: failed to flush Wayland clients");
            }
        }

        Ok(())
    }

    fn register_wayland_outputs(&mut self, outputs: &mut [KmsProbeOutput]) {
        for output in outputs {
            self.register_wayland_output(output);
        }
    }

    fn register_wayland_output(&mut self, output: &mut KmsProbeOutput) {
        if output.global.is_none() {
            output.global = Some(
                output
                    .output
                    .create_global::<SpikeState>(&self.wayland.display_handle),
            );
        }
        self.wayland.state.register_output(output.output.clone());
        info!(
            output = %output.output.name(),
            connector = ?output.connector,
            crtc = ?output.crtc,
            "udev backend: registered Wayland output global"
        );
    }

    fn unregister_wayland_output(&mut self, output: &mut KmsProbeOutput) {
        output.output.leave_all();
        self.wayland.state.unregister_output(&output.output);
        if let Some(global) = output.global.take() {
            self.wayland
                .display_handle
                .remove_global::<SpikeState>(global);
        }
        info!(
            output = %output.output.name(),
            connector = ?output.connector,
            crtc = ?output.crtc,
            "udev backend: unregistered Wayland output global"
        );
    }

    fn refresh_drm_device(&mut self, device_id: libc::dev_t) -> Result<()> {
        let Some(index) = self
            .drm_devices
            .iter()
            .position(|device| device.node.dev_id() == device_id)
        else {
            warn!(
                ?device_id,
                "udev backend: change event for unknown DRM device"
            );
            return Ok(());
        };

        let output_offset = self.wayland.state.outputs.len();
        // TODO(hotplug): Replace the lightweight connector rescan with
        // `DrmScanner` plus atomic test/apply once the backend owns durable
        // `DrmOutput` state. This path intentionally only reconciles probe-time
        // Wayland output bookkeeping.
        let new_outputs = Self::select_kms_outputs(&self.drm_devices[index].drm, output_offset)?;
        let mut device = self.drm_devices.remove(index);
        self.reconcile_drm_outputs(&mut device, new_outputs);
        self.drm_devices.insert(index, device);

        Ok(())
    }

    fn reconcile_drm_outputs(
        &mut self,
        device: &mut DrmProbeDevice,
        mut new_outputs: Vec<KmsProbeOutput>,
    ) {
        let mut index = 0;
        while index < device.outputs.len() {
            let old_connector = device.outputs[index].connector;
            let Some(new_index) = new_outputs
                .iter()
                .position(|output| output.connector == old_connector)
            else {
                let mut removed = device.outputs.remove(index);
                self.unregister_wayland_output(&mut removed);
                continue;
            };

            let new_output = new_outputs.remove(new_index);
            let existing = &mut device.outputs[index];
            if existing.crtc != new_output.crtc || existing.mode != new_output.mode {
                existing.crtc = new_output.crtc;
                existing.mode = new_output.mode;
                Self::update_wayland_output_state(&existing.output, existing.mode, index);
                info!(
                    node = ?device.node,
                    connector = ?existing.connector,
                    crtc = ?existing.crtc,
                    mode = %existing.mode.name().to_string_lossy(),
                    "udev backend: refreshed changed KMS connector"
                );
            }
            index += 1;
        }

        for mut output in new_outputs {
            self.register_wayland_output(&mut output);
            device.outputs.push(output);
        }
    }

    fn remove_drm_device(&mut self, device_id: libc::dev_t) {
        let Some(index) = self
            .drm_devices
            .iter()
            .position(|device| device.node.dev_id() == device_id)
        else {
            warn!(
                ?device_id,
                "udev backend: remove event for unknown DRM device"
            );
            return;
        };

        let mut device = self.drm_devices.remove(index);
        for output in &mut device.outputs {
            self.unregister_wayland_output(output);
        }
        self.wayland
            .event_loop
            .handle()
            .remove(device.registration_token);
        info!(
            node = ?device.node,
            path = %device.path.display(),
            "udev backend: DRM device removed from probe list"
        );
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
    ) -> Result<(DrmDevice, RegistrationToken, RenderProbe)> {
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

        let registration_token = self
            .wayland
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

        Ok((drm, registration_token, render))
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

    fn select_kms_outputs(drm: &DrmDevice, output_offset: usize) -> Result<Vec<KmsProbeOutput>> {
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

            let Some(crtc) = Self::select_crtc(drm, &resources, &connector)? else {
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

            let output =
                Self::create_wayland_output(&connector, mode, output_offset + outputs.len());

            outputs.push(KmsProbeOutput {
                connector: connector.handle(),
                crtc,
                mode,
                output,
                global: None,
            });
        }

        Ok(outputs)
    }

    fn create_wayland_output(
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

    fn update_wayland_output_state(output: &Output, mode: Mode, output_index: usize) {
        let wl_mode = WlMode::from(mode);
        let (width, _) = mode.size();
        let x = i32::from(width) * output_index as i32;
        output.set_preferred(wl_mode);
        output.change_current_state(Some(wl_mode), None, None, Some((x, 0).into()));
    }

    fn select_crtc(
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
            input_util::forward_keyboard_keycode(
                state,
                event.key_code(),
                event.state() == smithay::backend::input::KeyState::Pressed,
                SERIAL_COUNTER.next_serial(),
                event.time_msec(),
            );
        }
        InputEvent::PointerMotion { event } => {
            let delta_unaccel = event.delta_unaccel();
            input_util::forward_pointer_motion(
                state,
                (state.pointer_pos.0 + event.delta_x()).max(0.0),
                (state.pointer_pos.1 + event.delta_y()).max(0.0),
                Some((delta_unaccel.x, delta_unaccel.y)),
                Some(event.time_msec()),
                input_util::state_surface_under,
            );
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
            let Some(loc) = libinput_touch_location(state, &event) else {
                return;
            };
            input_util::forward_touch_down(
                state,
                event.slot(),
                loc.x,
                loc.y,
                event.time_msec(),
                input_util::state_surface_under,
            );
        }
        InputEvent::TouchMotion { event } => {
            let Some(loc) = libinput_touch_location(state, &event) else {
                return;
            };
            input_util::forward_touch_motion(state, event.slot(), loc.x, loc.y, event.time_msec());
        }
        InputEvent::TouchUp { event } => {
            input_util::forward_touch_up(state, event.slot(), event.time_msec());
        }
        InputEvent::TouchFrame { .. } => {
            input_util::forward_touch_frame(state);
        }
        InputEvent::TouchCancel { .. } => {
            input_util::forward_touch_cancel(state);
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
