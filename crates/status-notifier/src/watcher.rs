//! D-Bus server object implementing `org.kde.StatusNotifierWatcher`.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::{Mutex, mpsc};
use zbus::object_server::SignalEmitter;

use crate::{
    item_client::{item_key, load_item, parse_service, spawn_item_watcher},
    types::TrayEvent,
};

/// State shared between the D-Bus interface and the public `StatusNotifierWatcher`.
pub(crate) struct WatcherState {
    /// Registered item keys in insertion order.
    pub(crate) keys: Vec<String>,
    /// Loaded item metadata, keyed by canonical item key.
    pub(crate) items: HashMap<String, crate::types::StatusNotifierItem>,
    /// Live watcher task handles (dropped = task cancelled).
    pub(crate) watchers: HashMap<String, tokio::task::JoinHandle<()>>,
    /// Channel for forwarding events to the public API consumer.
    pub(crate) tx: mpsc::Sender<TrayEvent>,
}

/// The D-Bus interface object registered at `/StatusNotifierWatcher`.
pub(crate) struct WatcherInterface {
    pub(crate) state: Arc<Mutex<WatcherState>>,
}

#[zbus::interface(name = "org.kde.StatusNotifierWatcher")]
impl WatcherInterface {
    /// Called by apps to register a new tray icon.
    async fn register_status_notifier_item(
        &self,
        service: &str,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
        #[zbus(connection)] conn: &zbus::Connection,
    ) -> zbus::fdo::Result<()> {
        let sender = hdr
            .sender()
            .map(|s| s.to_string())
            .unwrap_or_default();

        let (bus_name, obj_path) = parse_service(&sender, service);
        let key = item_key(&bus_name, &obj_path);

        tracing::debug!(%key, %bus_name, %obj_path, "RegisterStatusNotifierItem");

        // Load item properties from D-Bus.
        let item_result = load_item(conn, &bus_name, &obj_path).await;
        let item = match item_result {
            Ok(i) => i,
            Err(e) => {
                tracing::warn!(%key, "failed to load SNI item properties: {e}");
                // Return error so the app knows registration failed.
                return Err(zbus::fdo::Error::Failed(format!(
                    "could not read item properties: {e}"
                )));
            }
        };

        // Spawn signal watcher for this item.
        let watcher_handle = spawn_item_watcher(
            conn.clone(),
            bus_name.clone(),
            obj_path.clone(),
            key.clone(),
            {
                let state = self.state.lock().await;
                state.tx.clone()
            },
        );

        {
            let mut state = self.state.lock().await;
            if !state.keys.contains(&key) {
                state.keys.push(key.clone());
            }
            state.items.insert(key.clone(), item);
            state.watchers.insert(key.clone(), watcher_handle);
            let _ = state.tx.send(TrayEvent::ItemRegistered(key.clone())).await;
        }

        // Emit D-Bus signal (best-effort — don't fail registration if signal emission fails).
        if let Err(e) = Self::status_notifier_item_registered(&emitter, &key).await {
            tracing::warn!("failed to emit StatusNotifierItemRegistered: {e}");
        }

        Ok(())
    }

    /// List of registered item keys.
    #[zbus(property)]
    async fn registered_status_notifier_items(&self) -> Vec<String> {
        self.state.lock().await.keys.clone()
    }

    /// Always `true` — we are the host.
    #[zbus(property)]
    fn is_status_notifier_host_registered(&self) -> bool {
        true
    }

    /// Protocol version.
    #[zbus(property)]
    fn protocol_version(&self) -> i32 {
        0
    }

    /// Emitted when a new item registers.
    #[zbus(signal)]
    pub async fn status_notifier_item_registered(
        emitter: &SignalEmitter<'_>,
        service: &str,
    ) -> zbus::Result<()>;

    /// Emitted when an item disappears.
    #[zbus(signal)]
    pub async fn status_notifier_item_unregistered(
        emitter: &SignalEmitter<'_>,
        service: &str,
    ) -> zbus::Result<()>;

    /// Emitted once when the host (us) registers.
    #[zbus(signal)]
    pub async fn status_notifier_host_registered(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;
}

/// Remove an item by key and emit `StatusNotifierItemUnregistered`.
///
/// Called when `NameOwnerChanged` reports the item's bus name vanished.
pub(crate) async fn remove_item(
    state: &Arc<Mutex<WatcherState>>,
    key: &str,
    emitter: &SignalEmitter<'_>,
) {
    let removed = {
        let mut s = state.lock().await;
        if s.keys.contains(&key.to_string()) {
            s.keys.retain(|k| k != key);
            s.items.remove(key);
            s.watchers.remove(key);
            let _ = s.tx.send(TrayEvent::ItemUnregistered(key.to_string())).await;
            true
        } else {
            false
        }
    };

    if removed {
        tracing::debug!(key, "item unregistered");
        if let Err(e) =
            WatcherInterface::status_notifier_item_unregistered(emitter, key).await
        {
            tracing::warn!("failed to emit StatusNotifierItemUnregistered: {e}");
        }
    }
}

/// Resolve the canonical item key given a bus name, for use in `NameOwnerChanged`.
///
/// Returns keys whose prefix matches `bus_name`.
pub(crate) async fn keys_for_bus(
    state: &Arc<Mutex<WatcherState>>,
    bus_name: &str,
) -> Vec<String> {
    state
        .lock()
        .await
        .keys
        .iter()
        .filter(|k| k.starts_with(bus_name))
        .cloned()
        .collect()
}

/// Object path at which the watcher is registered.
pub(crate) const WATCHER_PATH: &str = "/StatusNotifierWatcher";
/// Well-known D-Bus name we request.
pub(crate) const WATCHER_BUS_NAME: &str = "org.kde.StatusNotifierWatcher";
