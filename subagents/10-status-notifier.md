# SUBAGENT: System Tray — StatusNotifierItem Host
# Crate: `crates/status-notifier`

> **Read `ARCHITECTURE.md` in the repo root before starting.** It explains why we use Smithay (not wlroots), how the compositor rendering pipeline works, the full Wayland protocol reference, smithay API patterns (delegate pattern, GlesRenderer, calloop), and the winit dev backend.
# Branch: `feat/status-notifier`

You are building a D-Bus service that acts as a StatusNotifierWatcher and StatusNotifierHost for a Wayland DE's system tray. Apps like Discord, Steam, Slack, and Electron apps register tray icons via this protocol.

## D-Bus Interfaces

### You register: `org.kde.StatusNotifierWatcher`
- Method: `RegisterStatusNotifierItem(service: s)` — apps call this
- Property: `RegisteredStatusNotifierItems: as` — list of registered bus names
- Property: `IsStatusNotifierHostRegistered: b` — always true
- Signals: `StatusNotifierItemRegistered(s)`, `StatusNotifierItemUnregistered(s)`, `StatusNotifierHostRegistered`

### You read from each app: `org.kde.StatusNotifierItem`
Properties: `Category`, `Id`, `Title`, `Status` (Passive/Active/NeedsAttention), `IconName` or `IconPixmap` (array of width,height,ARGB_data), `AttentionIconName`/`AttentionIconPixmap`, `OverlayIconName`/`OverlayIconPixmap`, `ToolTip`, `Menu` (object path)
Signals: `NewIcon`, `NewAttentionIcon`, `NewTitle`, `NewStatus`, `NewToolTip`

### You read menus: `com.canonical.dbusmenu`
- `GetLayout(parentId: i, recursionDepth: i, propertyNames: as) → (revision: u, layout: recursive_struct)`
- `Event(id: i, eventId: s, data: v, timestamp: u)` — send clicks
- Menu item properties: `type`, `label`, `enabled`, `visible`, `icon-name`, `icon-data`, `toggle-type`, `toggle-state`, `children-display`

## AppIndicator compatibility
Some apps use `org.ayatana.AppIndicator` or `org.canonical.AppIndicator3` paths. Handle both transparently.

## Public API
```rust
pub struct StatusNotifierWatcher { /* zbus connection, items map */ }
pub struct StatusNotifierItem { pub id: String, pub title: String, pub icon: TrayIcon, pub menu_path: Option<ObjectPath>, ... }
pub enum TrayIcon { Named(String), Pixmap(Vec<TrayIconPixmap>) }
pub struct TrayIconPixmap { pub width: i32, pub height: i32, pub argb_data: Vec<u8> }
pub struct DbusMenu { pub items: Vec<MenuItem> }
pub struct MenuItem { pub id: i32, pub label: String, pub enabled: bool, pub icon: Option<TrayIcon>, pub children: Vec<MenuItem>, pub is_separator: bool, pub toggle_type: Option<ToggleType>, pub toggle_state: Option<bool> }
pub enum TrayEvent { ItemRegistered(String), ItemUnregistered(String), ItemUpdated { id: String, property: UpdatedProperty } }

impl StatusNotifierWatcher {
    pub async fn start(connection: &zbus::Connection) -> Result<(Self, tokio::sync::mpsc::Receiver<TrayEvent>)>;
    pub async fn get_menu(&self, item_id: &str) -> Result<DbusMenu>;
    pub async fn activate_item(&self, item_id: &str, x: i32, y: i32) -> Result<()>;
    pub async fn send_menu_event(&self, item_id: &str, menu_item_id: i32) -> Result<()>;
}
```

Use `zbus` for all D-Bus. ARGB pixmap data is network byte order — convert to native RGBA. Use `freedesktop-icons` crate for icon name→path lookup. Reference: COSMIC's `cosmic-applet-status-area`.

Work iteratively: watcher registration first, then item property reading, then icon rendering, then DBusMenu parsing.
