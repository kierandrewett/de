use tracing::warn;

/// Snapshot of BlueZ state relevant to the panel.
#[derive(Debug, Clone, Default)]
pub struct BluetoothState {
    /// Whether the Bluetooth adapter is powered on.
    pub enabled: bool,
}

// ── zbus proxy ───────────────────────────────────────────────────────────────

#[zbus::proxy(
    interface = "org.bluez.Adapter1",
    default_service = "org.bluez"
)]
trait BlueZAdapter {
    #[zbus(property)]
    fn powered(&self) -> zbus::Result<bool>;

    #[zbus(property)]
    fn set_powered(&self, value: bool) -> zbus::Result<()>;
}

#[zbus::proxy(
    interface = "org.bluez.Device1",
    default_service = "org.bluez"
)]
trait BlueZDevice {
    #[zbus(property)]
    fn connected(&self) -> zbus::Result<bool>;

    #[zbus(property)]
    fn alias(&self) -> zbus::Result<String>;
}

// ── Actions ──────────────────────────────────────────────────────────────────

/// Enable or disable the Bluetooth adapter.
pub async fn toggle_bluetooth(enabled: bool) {
    match zbus::Connection::system().await {
        Ok(conn) => {
            if let Some(adapter_path) = find_adapter(&conn).await {
                if let Ok(b) = BlueZAdapterProxy::builder(&conn).path(adapter_path.as_str()) {
                    if let Ok(proxy) = b.build().await {
                        let _ = proxy.set_powered(enabled).await;
                    }
                }
            }
        }
        Err(e) => warn!("toggle_bluetooth: {e}"),
    }
}

async fn find_adapter(conn: &zbus::Connection) -> Option<String> {
    use zbus::fdo::ObjectManagerProxy;
    let mgr = ObjectManagerProxy::builder(conn)
        .destination("org.bluez")
        .ok()?
        .path("/")
        .ok()?
        .build()
        .await
        .ok()?;

    let objects = mgr.get_managed_objects().await.ok()?;
    for (path, ifaces) in objects {
        if ifaces.contains_key("org.bluez.Adapter1") {
            return Some(path.to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bluetooth_state_defaults() {
        let s = BluetoothState::default();
        assert!(!s.enabled);
    }
}
