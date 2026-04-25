# SUBAGENT: Notification Server
# Crate: `crates/notification`

> **Read `ARCHITECTURE.md` in the repo root before starting.** It explains why we use Smithay (not wlroots), how the compositor rendering pipeline works, the full Wayland protocol reference, smithay API patterns (delegate pattern, GlesRenderer, calloop), and the winit dev backend.
# Branch: `feat/notification`

You are implementing the `org.freedesktop.Notifications` D-Bus specification — the standard Linux notification server. Our DE renders notifications as layer-shell overlay surfaces.

## D-Bus Interface: `org.freedesktop.Notifications`

### Methods
- `Notify(app_name: s, replaces_id: u, app_icon: s, summary: s, body: s, actions: as, hints: a{sv}, expire_timeout: i) → id: u`
- `CloseNotification(id: u)`
- `GetCapabilities() → as` — return `["body", "body-markup", "body-hyperlinks", "actions", "icon-static", "persistence", "action-icons"]`
- `GetServerInformation() → (name: s, vendor: s, version: s, spec_version: s)`

### Signals
- `NotificationClosed(id: u, reason: u)` — 1=expired, 2=dismissed, 3=closed_by_app, 4=undefined
- `ActionInvoked(id: u, action_key: s)`

### Hints to handle
- `urgency` (byte): 0=low, 1=normal, 2=critical (critical = no auto-dismiss)
- `image-data` (iiibiiay): width, height, rowstride, has_alpha, bpp, channels, data
- `image-path` (s): path to image file
- `desktop-entry` (s): app .desktop file id
- `category` (s): notification category
- `sound-name` (s): sound to play
- `transient` (b): don't persist in history
- `resident` (b): keep after action invoked

## Public API
```rust
pub struct NotificationServer { /* zbus connection, active notifications */ }
pub struct Notification {
    pub id: u32, pub app_name: String, pub app_icon: String,
    pub summary: String, pub body: String,
    pub actions: Vec<(String, String)>,  // (key, label) pairs
    pub urgency: Urgency, pub image: Option<NotificationImage>,
    pub expire_timeout: Option<Duration>,
    pub timestamp: SystemTime,
}
pub enum Urgency { Low, Normal, Critical }
pub enum NotificationImage { Path(PathBuf), Data { width: i32, height: i32, pixels: Vec<u8> } }
pub enum NotificationEvent {
    New(Notification), Replaced { old_id: u32, notification: Notification },
    Closed { id: u32, reason: CloseReason }, ActionInvoked { id: u32, action: String },
}

impl NotificationServer {
    pub async fn start(connection: &zbus::Connection) -> Result<(Self, tokio::sync::mpsc::Receiver<NotificationEvent>)>;
    pub async fn close(&self, id: u32, reason: CloseReason);
    pub fn history(&self) -> &[Notification]; // for notification centre
}
```

## Rendering (done by compositor/panel, not this crate)
This crate only handles D-Bus and notification state. It emits events via a channel. The shell-panel process consumes these events and renders notification cards as layer-shell surfaces:
- Top-right corner, stacking downward
- Squircle-rounded card with macOS border treatment
- Slide-in from right (spring animation), slide-out on dismiss
- Auto-dismiss after timeout (except critical)
- Action buttons as clickable text

## MPRIS awareness
Notifications from media players (app_name matching an active MPRIS player) get enhanced rendering by the panel: album art, playback controls. This crate doesn't handle MPRIS directly — it just passes the notification through. The panel cross-references with its MPRIS D-Bus subscriptions.

Use `zbus` for D-Bus. Assign monotonically increasing IDs starting from 1. Handle `replaces_id` for notification updates. Store history (last 100 notifications, excluding transient).

Work iteratively: basic Notify/Close first, then hints parsing, then image data, then history, then full capabilities.
