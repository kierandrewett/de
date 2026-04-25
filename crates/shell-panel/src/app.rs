use std::collections::HashMap;
use std::time::Duration;

use iced::{Element, Subscription, Task};
use iced_layershell::reexport::{
    Anchor, IcedId, KeyboardInteractivity, Layer, NewLayerShellSettings, OutputOption,
};
use iced_layershell::settings::{LayerShellSettings, StartMode};
use iced_layershell::to_layer_message;
use status_notifier::StatusNotifierItem;

use crate::{control_centre, datetime_popout, dbus, panel};

const PANEL_HEIGHT: u32 = 36;
const CC_WIDTH: u32 = 440;
const CC_HEIGHT: u32 = 680;
const DT_WIDTH: u32 = 360;
const DT_HEIGHT: u32 = 520;

/// All application messages.
///
/// The `#[to_layer_message(multi)]` attribute injects Wayland layer-shell
/// window-management variants (`NewLayerShell`, `RemoveWindow`, etc.) and
/// generates the `TryInto<LayerShellCustomActionWithId>` impl that the
/// iced_layershell runtime uses to intercept those variants before forwarding
/// the rest to [`update`].
#[to_layer_message(multi)]
#[derive(Debug, Clone)]
pub enum Message {
    /// 30-second clock tick.
    Tick,
    /// Toggle the Control Centre overlay.
    ToggleCC,
    /// Toggle the date/time popout.
    ToggleDateTime,

    // ── IPC events ──────────────────────────────────────────────────────────
    IpcEvent(ipc::ShellEvent),
    IpcDisconnected,

    // ── Tray ─────────────────────────────────────────────────────────────────
    TrayItemAdded(String, Box<StatusNotifierItem>),
    TrayItemRemoved(String),
    TrayActivate(String, i32, i32),
    TraySecondaryActivate(String, i32, i32),

    // ── D-Bus state updates ──────────────────────────────────────────────────
    NetworkUpdate(dbus::network::NetworkState),
    BluetoothUpdate(dbus::bluetooth::BluetoothState),
    BatteryUpdate(dbus::upower::BatteryState),
    MprisUpdate(Option<dbus::mpris::MprisState>),

    // ── Control Centre interactions ──────────────────────────────────────────
    ToggleWifi,
    ToggleBluetooth,
    ToggleDnd,
    ToggleNightLight,
    ToggleDarkMode,
    VolumeChanged(f32),
    BrightnessChanged(f32),

    // ── MPRIS controls ───────────────────────────────────────────────────────
    MprisPlayPause,
    MprisNext,
    MprisPrev,

    // ── Power / session ──────────────────────────────────────────────────────
    Lock,
    Shutdown,
    Reboot,
    Suspend,

    // ── Screen recording ────────────────────────────────────────────────────
    StartRecording,
    StopRecording,
}

/// Root application state shared across all layer-shell windows.
pub struct State {
    /// Window ID of the Control Centre overlay, if open.
    pub cc_id: Option<IcedId>,
    /// Window ID of the date/time popout, if open.
    pub datetime_id: Option<IcedId>,

    /// Current local time (updated every tick).
    pub now: chrono::DateTime<chrono::Local>,
    /// App name / title of the currently focused window.
    pub focused_app: Option<String>,

    /// Whether a screen recording is in progress.
    pub is_recording: bool,
    /// Elapsed recording time in seconds.
    pub recording_elapsed: Option<u64>,

    /// Live tray items by their StatusNotifier key.
    pub tray_items: HashMap<String, StatusNotifierItem>,

    /// Latest NetworkManager state.
    pub network: dbus::network::NetworkState,
    /// Latest BlueZ state.
    pub bluetooth: dbus::bluetooth::BluetoothState,
    /// Latest UPower battery state.
    pub battery: dbus::upower::BatteryState,
    /// Latest MPRIS now-playing state.
    pub mpris: Option<dbus::mpris::MprisState>,

    // Control Centre toggles
    pub dnd: bool,
    pub night_light: bool,
    pub dark_mode: bool,

    /// Master volume (0.0 – 1.0).
    pub volume: f32,
    /// Screen brightness (0.0 – 1.0).
    pub brightness: f32,
}

impl Default for State {
    fn default() -> Self {
        Self {
            cc_id: None,
            datetime_id: None,
            now: chrono::Local::now(),
            focused_app: None,
            is_recording: false,
            recording_elapsed: None,
            tray_items: HashMap::new(),
            network: Default::default(),
            bluetooth: Default::default(),
            battery: Default::default(),
            mpris: None,
            dnd: false,
            night_light: false,
            dark_mode: true,
            volume: 0.5,
            brightness: 1.0,
        }
    }
}

/// Boot: return the initial state and an empty task.
pub fn boot() -> (State, Task<Message>) {
    (State::default(), Task::none())
}

/// Update the application state in response to a message.
pub fn update(state: &mut State, msg: Message) -> Task<Message> {
    match msg {
        // ── Time ────────────────────────────────────────────────────────────
        Message::Tick => {
            state.now = chrono::Local::now();
        }

        // ── Window management ────────────────────────────────────────────────
        Message::ToggleCC => {
            if let Some(id) = state.cc_id.take() {
                return Task::done(Message::RemoveWindow(id));
            }
            let (id, task) = Message::layershell_open(NewLayerShellSettings {
                size: Some((CC_WIDTH, CC_HEIGHT)),
                layer: Layer::Overlay,
                anchor: Anchor::Top | Anchor::Right,
                exclusive_zone: Some(-1),
                keyboard_interactivity: KeyboardInteractivity::OnDemand,
                output_option: OutputOption::LastOutput,
                events_transparent: false,
                margin: None,
                namespace: Some("cc".into()),
            });
            state.cc_id = Some(id);
            return task;
        }
        Message::ToggleDateTime => {
            if let Some(id) = state.datetime_id.take() {
                return Task::done(Message::RemoveWindow(id));
            }
            let (id, task) = Message::layershell_open(NewLayerShellSettings {
                size: Some((DT_WIDTH, DT_HEIGHT)),
                layer: Layer::Overlay,
                anchor: Anchor::Top,
                exclusive_zone: Some(-1),
                keyboard_interactivity: KeyboardInteractivity::OnDemand,
                output_option: OutputOption::LastOutput,
                events_transparent: false,
                margin: None,
                namespace: Some("datetime".into()),
            });
            state.datetime_id = Some(id);
            return task;
        }

        // ── IPC ─────────────────────────────────────────────────────────────
        Message::IpcEvent(ev) => handle_ipc_event(state, ev),
        Message::IpcDisconnected => {
            state.focused_app = None;
            state.is_recording = false;
        }

        // ── Tray ────────────────────────────────────────────────────────────
        Message::TrayItemAdded(id, item) => {
            state.tray_items.insert(id, *item);
        }
        Message::TrayItemRemoved(id) => {
            state.tray_items.remove(&id);
        }
        Message::TrayActivate(id, x, y) => {
            if let Some(item) = state.tray_items.get(&id) {
                return tray_call_task(
                    item.bus_name.clone(),
                    item.object_path.to_string(),
                    "Activate",
                    x,
                    y,
                );
            }
        }
        Message::TraySecondaryActivate(id, x, y) => {
            if let Some(item) = state.tray_items.get(&id) {
                return tray_call_task(
                    item.bus_name.clone(),
                    item.object_path.to_string(),
                    "SecondaryActivate",
                    x,
                    y,
                );
            }
        }

        // ── D-Bus state ──────────────────────────────────────────────────────
        Message::NetworkUpdate(s) => state.network = s,
        Message::BluetoothUpdate(s) => state.bluetooth = s,
        Message::BatteryUpdate(s) => state.battery = s,
        Message::MprisUpdate(s) => state.mpris = s,

        // ── CC toggles ───────────────────────────────────────────────────────
        Message::ToggleWifi => {
            let desired = !state.network.wifi_enabled;
            return simple_task(async move {
                dbus::network::toggle_wifi(desired).await;
                Message::Tick
            });
        }
        Message::ToggleBluetooth => {
            let desired = !state.bluetooth.enabled;
            return simple_task(async move {
                dbus::bluetooth::toggle_bluetooth(desired).await;
                Message::Tick
            });
        }
        Message::ToggleDnd => state.dnd = !state.dnd,
        Message::ToggleNightLight => state.night_light = !state.night_light,
        Message::ToggleDarkMode => state.dark_mode = !state.dark_mode,
        Message::VolumeChanged(v) => {
            state.volume = v;
            return simple_task(async move {
                dbus::mpris::set_volume(v as f64).await;
                Message::Tick
            });
        }
        Message::BrightnessChanged(b) => {
            state.brightness = b;
            return simple_task(async move {
                dbus::set_brightness(b).await;
                Message::Tick
            });
        }

        // ── MPRIS ────────────────────────────────────────────────────────────
        Message::MprisPlayPause => {
            return simple_task(async {
                dbus::mpris::play_pause().await;
                Message::Tick
            });
        }
        Message::MprisNext => {
            return simple_task(async {
                dbus::mpris::next().await;
                Message::Tick
            });
        }
        Message::MprisPrev => {
            return simple_task(async {
                dbus::mpris::prev().await;
                Message::Tick
            });
        }

        // ── Power / session ──────────────────────────────────────────────────
        Message::Lock => {
            return simple_task(async {
                dbus::logind::lock_session().await;
                Message::Tick
            });
        }
        Message::Shutdown => {
            return simple_task(async {
                dbus::logind::shutdown().await;
                Message::Tick
            });
        }
        Message::Reboot => {
            return simple_task(async {
                dbus::logind::reboot().await;
                Message::Tick
            });
        }
        Message::Suspend => {
            return simple_task(async {
                dbus::logind::suspend().await;
                Message::Tick
            });
        }

        // ── Recording ────────────────────────────────────────────────────────
        Message::StartRecording => {
            state.is_recording = true;
            state.recording_elapsed = Some(0);
            return ipc_request(ipc::ShellRequest::StartScreenRecording);
        }
        Message::StopRecording => {
            state.is_recording = false;
            state.recording_elapsed = None;
            return ipc_request(ipc::ShellRequest::StopScreenRecording);
        }

        // LayerShell variants are intercepted by the runtime before reaching
        // update(), but a catch-all keeps the match exhaustive.
        _ => {}
    }
    Task::none()
}

fn handle_ipc_event(state: &mut State, event: ipc::ShellEvent) {
    match event {
        ipc::ShellEvent::FocusedWindowChanged { window } => {
            state.focused_app = window.map(|w| {
                if w.title.is_empty() {
                    w.app_id
                } else {
                    w.title
                }
            });
        }
        ipc::ShellEvent::ScreencastStateChanged { active, elapsed_secs } => {
            state.is_recording = active;
            state.recording_elapsed = elapsed_secs;
        }
        _ => {}
    }
}

/// Dispatch the view to the right surface based on window ID.
pub fn view(state: &State, id: IcedId) -> Element<'_, Message> {
    if state.cc_id == Some(id) {
        control_centre::view(state)
    } else if state.datetime_id == Some(id) {
        datetime_popout::view(state)
    } else {
        panel::view(state)
    }
}

/// Active subscriptions: clock tick, IPC events, tray, and D-Bus monitors.
pub fn subscription(_state: &State) -> Subscription<Message> {
    Subscription::batch([
        iced::time::every(Duration::from_secs(30)).map(|_| Message::Tick),
        Subscription::run(dbus::ipc_subscription),
        Subscription::run(dbus::tray_subscription),
        Subscription::run(dbus::network::subscription),
        Subscription::run(dbus::upower::subscription),
        Subscription::run(dbus::mpris::subscription),
    ])
}

/// Run the panel application.
pub fn run() -> iced_layershell::Result {
    iced_layershell::daemon(boot, "shell-panel", update, view)
        .subscription(subscription)
        .layer_settings(LayerShellSettings {
            anchor: Anchor::Top | Anchor::Left | Anchor::Right,
            layer: Layer::Top,
            exclusive_zone: PANEL_HEIGHT as i32,
            size: Some((0, PANEL_HEIGHT)),
            keyboard_interactivity: KeyboardInteractivity::OnDemand,
            start_mode: StartMode::AllScreens,
            ..Default::default()
        })
        .run()
}

// ── Helpers ──────────────────────────────────────────────────────────────────

fn simple_task<F>(fut: F) -> Task<Message>
where
    F: std::future::Future<Output = Message> + Send + 'static,
{
    Task::future(fut)
}

fn ipc_request(req: ipc::ShellRequest) -> Task<Message> {
    Task::future(async move {
        use tokio::io::AsyncWriteExt;
        if let Ok(mut stream) = tokio::net::UnixStream::connect(ipc::socket_path()).await {
            let msg = ipc::serialize(&req);
            let _ = stream.write_all(msg.as_bytes()).await;
        }
        Message::Tick
    })
}

fn tray_call_task(
    bus_name: String,
    obj_path: String,
    method: &'static str,
    x: i32,
    y: i32,
) -> Task<Message> {
    Task::future(async move {
        if let Ok(conn) = zbus::Connection::session().await {
            let _ = conn
                .call_method(
                    Some(bus_name.as_str()),
                    obj_path.as_str(),
                    Some("org.kde.StatusNotifierItem"),
                    method,
                    &(x, y),
                )
                .await;
        }
        Message::Tick
    })
}
