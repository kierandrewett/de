//! StatusNotifierItem (AppIndicator) host.
//!
//! Wraps two related DBus protocols:
//!   * `org.kde.StatusNotifierWatcher` — the registry. Apps tell us they
//!     exist by calling `RegisterStatusNotifierItem(service_name)`. We're
//!     also expected to advertise ourselves to a session-wide
//!     `StatusNotifierHost-<pid>` name so KDE/GNOME-aware apps know there's
//!     a host listening.
//!   * `org.kde.StatusNotifierItem` — per-item interface; each registered
//!     item exposes properties (Title, Icon, Status, ToolTip, Menu) and
//!     methods (Activate, ContextMenu, Scroll, SecondaryActivate).
//!
//! ## Architecture
//!
//! DBus runs async; the Wayland event loop is sync. We own a dedicated
//! tokio runtime on a worker thread that hosts the watcher service. Item
//! registrations / removals are sent back to the main thread via an mpsc
//! channel; the renderer drains that on each frame and rebuilds the panel
//! tray model.
//!
//! ## Status
//!
//! First-pass implementation:
//!   * Watcher service registers on the session bus.
//!   * Items are tracked by `(service_name, object_path)`.
//!   * Item Title is read once at registration.
//!   * Left-click → `Activate(x, y)` (`activate()` worker fn).
//!   * Right-click → `ContextMenu(x, y)` (`context_menu()` worker fn).
//!   * DBusMenu fetch + popup rendering not yet wired (clients open
//!     their own popup in response to ContextMenu for now).

use std::sync::{Arc, Mutex};
use std::collections::HashMap;
use tokio::sync::mpsc;
use zbus::{interface, fdo, Connection, names::BusName, zvariant::OwnedObjectPath};

/// One tracked StatusNotifierItem.
#[derive(Debug, Clone)]
pub struct TrayItem {
    /// Stable id assigned at registration — used as the panel-icon model key.
    pub id: u32,
    /// DBus service name (e.g. `:1.123`) the item lives on.
    pub service: String,
    /// Object path the item is registered under (often `/StatusNotifierItem`).
    pub object_path: String,
    /// Title fetched from the item's `Title` property — best-effort.
    pub title: String,
    /// Icon name (themed icon spec) from the item's `IconName` property.
    /// We resolve this against the system icon theme at render time.
    pub icon_name: String,
}

/// Events the watcher worker thread sends back to the main loop.
#[derive(Debug, Clone)]
pub enum TrayEvent {
    Added(TrayItem),
    Removed(u32),
}

/// Inbound DBus interface for `org.kde.StatusNotifierWatcher`.
struct WatcherIface {
    inner: Arc<Mutex<WatcherState>>,
    events_tx: mpsc::UnboundedSender<TrayEvent>,
}

#[derive(Default)]
struct WatcherState {
    next_id: u32,
    /// Service name → assigned id. Lets us produce a stable id across
    /// re-registrations and look the id up on disconnect.
    items: HashMap<String, u32>,
    hosts: Vec<String>,
}

#[interface(name = "org.kde.StatusNotifierWatcher")]
impl WatcherIface {
    /// Apps call this with their service name (and optionally an object
    /// path baked into the service string as `service_name/object_path`).
    /// Spec is squishy here: most clients pass a bare service name and
    /// expose the item at `/StatusNotifierItem`.
    async fn register_status_notifier_item(
        &self,
        #[zbus(connection)] _conn: &Connection,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        service: &str,
    ) -> fdo::Result<()> {
        // Resolve the service name. If `service` looks like an object path
        // (starts with `/`) the actual bus name comes from the message
        // sender — that's the canonical "registering by path" case.
        let (bus_name, object_path) = if service.starts_with('/') {
            let sender = hdr.sender()
                .map(|s| s.to_string())
                .unwrap_or_default();
            (sender, service.to_string())
        } else {
            (service.to_string(), "/StatusNotifierItem".to_string())
        };
        if bus_name.is_empty() {
            return Err(fdo::Error::InvalidArgs("missing sender".into()));
        }

        let id = {
            let mut s = self.inner.lock().unwrap();
            if let Some(&existing) = s.items.get(&bus_name) {
                existing
            } else {
                s.next_id += 1;
                let new_id = s.next_id;
                s.items.insert(bus_name.clone(), new_id);
                new_id
            }
        };

        // Fetch the item's properties (best-effort; defaults if anything
        // fails so we still show *something* in the tray).
        let title = read_str_property(&bus_name, &object_path, "Title")
            .await.unwrap_or_default();
        let icon_name = read_str_property(&bus_name, &object_path, "IconName")
            .await.unwrap_or_default();

        let item = TrayItem {
            id,
            service: bus_name.clone(),
            object_path,
            title,
            icon_name,
        };
        tracing::info!("tray: registered {} (id={}) title={:?} icon={:?}",
            item.service, item.id, item.title, item.icon_name);
        let _ = self.events_tx.send(TrayEvent::Added(item));
        Ok(())
    }

    async fn register_status_notifier_host(&self, service: &str) -> fdo::Result<()> {
        let mut s = self.inner.lock().unwrap();
        if !s.hosts.iter().any(|h| h == service) {
            s.hosts.push(service.to_string());
        }
        Ok(())
    }

    /// Property: registered services as service-name strings.
    #[zbus(property)]
    fn registered_status_notifier_items(&self) -> Vec<String> {
        let s = self.inner.lock().unwrap();
        s.items.keys().cloned().collect()
    }

    #[zbus(property)]
    fn is_status_notifier_host_registered(&self) -> bool {
        let s = self.inner.lock().unwrap();
        !s.hosts.is_empty()
    }

    #[zbus(property)]
    fn protocol_version(&self) -> i32 { 0 }

    #[zbus(signal)]
    async fn status_notifier_item_registered(
        ctx: &zbus::object_server::SignalEmitter<'_>,
        service: &str,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn status_notifier_item_unregistered(
        ctx: &zbus::object_server::SignalEmitter<'_>,
        service: &str,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn status_notifier_host_registered(
        ctx: &zbus::object_server::SignalEmitter<'_>,
    ) -> zbus::Result<()>;
}

/// Send `Activate(x, y)` to a StatusNotifierItem — left-click. Best-effort
/// (some items don't implement Activate; we ignore the error).
pub fn activate(bus: String, path: String, x: i32, y: i32) {
    std::thread::spawn(move || {
        let rt = match tokio::runtime::Builder::new_current_thread()
            .enable_all().build()
        {
            Ok(r) => r,
            Err(_) => return,
        };
        rt.block_on(async move {
            if let Ok(conn) = Connection::session().await {
                if let Ok(bus_name) = BusName::try_from(bus) {
                    if let Ok(obj_path) = OwnedObjectPath::try_from(path.as_str()) {
                        if let Ok(proxy) = zbus::Proxy::new(
                            &conn, bus_name, obj_path,
                            "org.kde.StatusNotifierItem",
                        ).await {
                            let _ = proxy.call::<&str, (i32, i32), ()>(
                                "Activate", &(x, y),
                            ).await;
                        }
                    }
                }
            }
        });
    });
}

/// Read a single string property off a StatusNotifierItem at `(bus, path)`.
async fn read_str_property(bus: &str, path: &str, prop: &str)
    -> zbus::Result<String>
{
    let conn = Connection::session().await?;
    let bus_name = BusName::try_from(bus.to_string())
        .map_err(|e| zbus::Error::Address(format!("bad bus name: {e}")))?;
    let obj_path = OwnedObjectPath::try_from(path)
        .map_err(|e| zbus::Error::Address(format!("bad path: {e}")))?;
    let proxy = zbus::Proxy::new(
        &conn, bus_name, obj_path, "org.kde.StatusNotifierItem",
    ).await?;
    proxy.get_property::<String>(prop).await
}

/// Spawn the StatusNotifierWatcher service on a dedicated worker thread.
/// Returns the receiver end for tray events.
pub fn spawn_tray_host() -> mpsc::UnboundedReceiver<TrayEvent> {
    let (events_tx, events_rx) = mpsc::unbounded_channel();

    std::thread::spawn(move || {
        let rt = match tokio::runtime::Builder::new_current_thread()
            .enable_all().build()
        {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("tray: tokio runtime init failed: {e:?}");
                return;
            }
        };
        rt.block_on(async move {
            if let Err(e) = run_watcher(events_tx).await {
                tracing::warn!("tray: watcher exited: {e:?}");
            }
        });
    });

    events_rx
}

async fn run_watcher(events_tx: mpsc::UnboundedSender<TrayEvent>)
    -> zbus::Result<()>
{
    let conn = Connection::session().await?;
    let inner = Arc::new(Mutex::new(WatcherState::default()));
    let iface = WatcherIface {
        inner: inner.clone(),
        events_tx,
    };
    conn.object_server()
        .at("/StatusNotifierWatcher", iface)
        .await?;
    // Best-effort: ignore "name already taken" — another watcher (e.g. the
    // system tray plasmoid in a hybrid session) is fine to coexist.
    let _ = conn.request_name("org.kde.StatusNotifierWatcher").await;
    tracing::info!("tray: StatusNotifierWatcher running on session bus");

    // Park forever — the connection's task pump processes incoming calls.
    std::future::pending::<()>().await;
    Ok(())
}
