//! udev/DRM backend — production mode on real hardware.
//!
//! Enumerates GPUs via udev, sets up DRM/KMS outputs, and handles
//! libinput events. Subagent 08 wires the renderer into this skeleton.

use std::collections::HashMap;
use std::path::PathBuf;

use input::Libinput;
use smithay::{
    backend::{
        drm::DrmNode,
        libinput::{LibinputInputBackend, LibinputSessionInterface},
        session::{libseat::LibSeatSession, Session},
        udev::UdevBackend,
    },
    reexports::{
        calloop::{
            generic::Generic,
            EventLoop, Interest, Mode,
        },
        wayland_server::Display,
    },
};

use crate::state::{Backend, CommonState, State};

/// Data owned by the udev backend.
#[allow(dead_code)]
pub struct UdevData {
    pub session: LibSeatSession,
    pub primary_gpu: DrmNode,
    pub devices: HashMap<DrmNode, DeviceData>,
}

/// Per-DRM-device data.
#[allow(dead_code)]
pub struct DeviceData {
    pub drm_node: DrmNode,
    pub path: PathBuf,
}

/// Initialise the udev/DRM backend and run the event loop.
pub fn run() -> anyhow::Result<()> {
    let mut event_loop = EventLoop::<State>::try_new()?;
    let display = Display::<State>::new()?;
    let dh = display.handle();
    let loop_handle = event_loop.handle();
    let loop_signal = event_loop.get_signal();

    // ── Session ─────────────────────────────────────────────────────────���────
    let (session, _notifier) = LibSeatSession::new()?;
    let seat_name = session.seat();
    tracing::info!("session on seat {}", seat_name);

    // ── udev enumeration ─────────────────────────────────────────────────────
    let udev_backend = UdevBackend::new(&seat_name)?;

    let mut devices: HashMap<DrmNode, DeviceData> = HashMap::new();
    let mut primary_gpu: Option<DrmNode> = None;

    for (_device_id, path) in udev_backend.device_list() {
        if let Ok(node) = DrmNode::from_path(path) {
            if primary_gpu.is_none() {
                primary_gpu = Some(node);
            }
            tracing::info!("DRM device: {} at {}", node, path.display());
            devices.insert(node, DeviceData { drm_node: node, path: path.to_owned() });
        }
    }

    let primary_gpu = primary_gpu.unwrap_or_else(|| {
        DrmNode::from_path("/dev/dri/card0").expect("no GPU found")
    });

    // ── libinput ─────────────────────────────────────────────────────────────
    let libinput_context = {
        let mut ctx = Libinput::new_with_udev(
            LibinputSessionInterface::from(session.clone()),
        );
        ctx.udev_assign_seat(&seat_name).ok();
        ctx
    };
    let libinput_backend = LibinputInputBackend::new(libinput_context);

    loop_handle.insert_source(libinput_backend, |event, _, state| {
        crate::input::handle_libinput_event(state, event);
    }).map_err(|e| anyhow::anyhow!("{e:?}"))?;

    // ── Wayland display ───────────────────────────────────────────────────────
    let socket_name = "wayland-0".to_owned();
    loop_handle.insert_source(
        Generic::new(display, Interest::READ, Mode::Level),
        |_, display, state| {
            unsafe {
                display.get_mut().dispatch_clients(state)?;
            }
            Ok(smithay::reexports::calloop::PostAction::Continue)
        },
    )?;

    let common =
        CommonState::new(&dh, loop_handle.clone(), loop_signal.clone(), socket_name.clone());

    let udev_data = UdevData { session, primary_gpu, devices };
    let mut state = State { backend: Backend::Udev(udev_data), common };

    tracing::info!("Wayland socket: {}", socket_name);
    std::env::set_var("WAYLAND_DISPLAY", &socket_name);

    event_loop.run(None, &mut state, |state| {
        if let Err(e) = state.common.display_handle.flush_clients() {
            tracing::warn!("flush_clients: {e}");
        }
    })?;

    Ok(())
}
