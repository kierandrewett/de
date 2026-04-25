//! Public API: [`StatusNotifierWatcher`].

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::{Mutex, mpsc};
use zbus::Connection;
use zbus::fdo::DBusProxy;

use crate::{
    error::{Error, Result},
    item_client::load_item,
    menu_client::{fetch_menu, send_menu_event},
    types::{DbusMenu, StatusNotifierItem, TrayEvent},
    watcher::{
        WatcherInterface, WatcherState, WATCHER_BUS_NAME, WATCHER_PATH,
        keys_for_bus, remove_item,
    },
};

/// Handle to the running StatusNotifierWatcher D-Bus service.
///
/// Obtain one with [`StatusNotifierWatcher::start`].  The service runs for as
/// long as this handle is kept alive.
pub struct StatusNotifierWatcher {
    conn: Connection,
    state: Arc<Mutex<WatcherState>>,
}

impl StatusNotifierWatcher {
    /// Start the StatusNotifierWatcher service on the session bus.
    ///
    /// Registers the `org.kde.StatusNotifierWatcher` well-known name, exports
    /// the object at `/StatusNotifierWatcher`, and starts a background task
    /// that watches for bus-name disappearances so stale items are removed.
    ///
    /// Returns `(Self, rx)` — the handle and a channel that receives
    /// [`TrayEvent`]s.
    pub async fn start(connection: &Connection) -> Result<(Self, mpsc::Receiver<TrayEvent>)> {
        let (tx, rx) = mpsc::channel(64);

        let state = Arc::new(Mutex::new(WatcherState {
            keys: Vec::new(),
            items: HashMap::new(),
            watchers: HashMap::new(),
            tx,
        }));

        let iface = WatcherInterface { state: state.clone() };

        // Register the D-Bus object.
        connection
            .object_server()
            .at(WATCHER_PATH, iface)
            .await?;

        // Request the well-known bus name.
        connection
            .request_name(WATCHER_BUS_NAME)
            .await?;

        // Emit StatusNotifierHostRegistered to announce ourselves.
        let iface_ref = connection
            .object_server()
            .interface::<_, WatcherInterface>(WATCHER_PATH)
            .await?;
        if let Err(e) =
            WatcherInterface::status_notifier_host_registered(iface_ref.signal_emitter()).await
        {
            tracing::warn!("failed to emit StatusNotifierHostRegistered: {e}");
        }

        // Spawn NameOwnerChanged watcher to clean up dead items.
        let conn_clone = connection.clone();
        let state_clone = state.clone();
        tokio::spawn(async move {
            if let Err(e) = watch_name_owner_changes(conn_clone, state_clone).await {
                tracing::warn!("NameOwnerChanged watcher ended: {e}");
            }
        });

        Ok((Self { conn: connection.clone(), state }, rx))
    }

    /// Return a snapshot of all currently registered tray items.
    pub async fn items(&self) -> Vec<StatusNotifierItem> {
        self.state.lock().await.items.values().cloned().collect()
    }

    /// Return a single item by its key (bus_name + object_path).
    ///
    /// Returns `Err(Error::ItemNotFound)` if the key is not registered.
    pub async fn get_item(&self, item_id: &str) -> Result<StatusNotifierItem> {
        self.state
            .lock()
            .await
            .items
            .get(item_id)
            .cloned()
            .ok_or_else(|| Error::ItemNotFound(item_id.to_string()))
    }

    /// Re-read all properties for an item from D-Bus and update the cache.
    pub async fn refresh_item(&self, item_id: &str) -> Result<StatusNotifierItem> {
        let (bus_name, obj_path) = {
            let state = self.state.lock().await;
            let item = state
                .items
                .get(item_id)
                .ok_or_else(|| Error::ItemNotFound(item_id.to_string()))?;
            (item.bus_name.clone(), item.object_path.clone())
        };

        let updated = load_item(&self.conn, &bus_name, &obj_path).await?;
        self.state
            .lock()
            .await
            .items
            .insert(item_id.to_string(), updated.clone());

        Ok(updated)
    }

    /// Fetch the context menu for a registered item.
    ///
    /// Returns `Err(Error::NoMenu)` if the item has no menu path.
    pub async fn get_menu(&self, item_id: &str) -> Result<DbusMenu> {
        let (bus_name, menu_path) = {
            let state = self.state.lock().await;
            let item = state
                .items
                .get(item_id)
                .ok_or_else(|| Error::ItemNotFound(item_id.to_string()))?;
            let path =
                item.menu_path.clone().ok_or_else(|| Error::NoMenu(item_id.to_string()))?;
            (item.bus_name.clone(), path)
        };

        fetch_menu(&self.conn, &bus_name, &menu_path).await
    }

    /// Send an `Activate` request to a tray item at the given screen position.
    ///
    /// This is the primary-click action on a tray icon.
    pub async fn activate_item(&self, item_id: &str, x: i32, y: i32) -> Result<()> {
        let (bus_name, obj_path) = self.item_coords(item_id).await?;
        activate_item_on_proxy(&self.conn, &bus_name, &obj_path, x, y).await
    }

    /// Send a `SecondaryActivate` request (typically middle-click).
    pub async fn secondary_activate_item(&self, item_id: &str, x: i32, y: i32) -> Result<()> {
        let (bus_name, obj_path) = self.item_coords(item_id).await?;
        secondary_activate_on_proxy(&self.conn, &bus_name, &obj_path, x, y).await
    }

    /// Trigger a menu item by its dbusmenu id.
    pub async fn send_menu_event(&self, item_id: &str, menu_item_id: i32) -> Result<()> {
        let (bus_name, menu_path) = {
            let state = self.state.lock().await;
            let item = state
                .items
                .get(item_id)
                .ok_or_else(|| Error::ItemNotFound(item_id.to_string()))?;
            let path =
                item.menu_path.clone().ok_or_else(|| Error::NoMenu(item_id.to_string()))?;
            (item.bus_name.clone(), path)
        };

        send_menu_event(&self.conn, &bus_name, &menu_path, menu_item_id).await
    }

    async fn item_coords(
        &self,
        item_id: &str,
    ) -> Result<(String, zbus::zvariant::OwnedObjectPath)> {
        let state = self.state.lock().await;
        let item = state
            .items
            .get(item_id)
            .ok_or_else(|| Error::ItemNotFound(item_id.to_string()))?;
        Ok((item.bus_name.clone(), item.object_path.clone()))
    }
}

// --- Activate helpers -------------------------------------------------------

async fn activate_item_on_proxy(
    conn: &Connection,
    bus_name: &str,
    obj_path: &zbus::zvariant::OwnedObjectPath,
    x: i32,
    y: i32,
) -> Result<()> {
    // Use a raw method call to avoid needing a separate proxy definition.
    conn.call_method(
        Some(bus_name),
        obj_path.as_str(),
        Some("org.kde.StatusNotifierItem"),
        "Activate",
        &(x, y),
    )
    .await
    .map(|_| ())
    .map_err(Error::Dbus)
}

async fn secondary_activate_on_proxy(
    conn: &Connection,
    bus_name: &str,
    obj_path: &zbus::zvariant::OwnedObjectPath,
    x: i32,
    y: i32,
) -> Result<()> {
    conn.call_method(
        Some(bus_name),
        obj_path.as_str(),
        Some("org.kde.StatusNotifierItem"),
        "SecondaryActivate",
        &(x, y),
    )
    .await
    .map(|_| ())
    .map_err(Error::Dbus)
}

// --- NameOwnerChanged watcher -----------------------------------------------

/// Watch for D-Bus name disappearances and remove stale tray items.
async fn watch_name_owner_changes(
    conn: Connection,
    state: Arc<Mutex<WatcherState>>,
) -> Result<()> {
    use futures_util::StreamExt;

    let dbus_proxy = DBusProxy::new(&conn).await?;
    let mut stream = dbus_proxy.receive_name_owner_changed().await?;

    while let Some(signal) = stream.next().await {
        let args = match signal.args() {
            Ok(a) => a,
            Err(_) => continue,
        };

        // Only care when a name disappears (new_owner is empty).
        if !args.new_owner.as_ref().map(|o| o.is_empty()).unwrap_or(true) {
            continue;
        }

        let vanished = args.name.to_string();
        let affected = keys_for_bus(&state, &vanished).await;

        if affected.is_empty() {
            continue;
        }

        // Get a signal emitter from the watcher interface.
        let emitter_result = conn
            .object_server()
            .interface::<_, WatcherInterface>(WATCHER_PATH)
            .await;

        let iface_ref = match emitter_result {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("could not get watcher interface ref: {e}");
                continue;
            }
        };

        for key in &affected {
            remove_item(&state, key, iface_ref.signal_emitter()).await;
        }
    }

    Ok(())
}
