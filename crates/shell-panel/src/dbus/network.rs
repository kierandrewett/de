use std::time::Duration;

use futures::channel::mpsc;
use tracing::{debug, warn};

use crate::app::Message;

/// Snapshot of NetworkManager state relevant to the panel.
#[derive(Debug, Clone, Default)]
pub struct NetworkState {
    /// Whether the Wi-Fi radio is enabled.
    pub wifi_enabled: bool,
}

// ── zbus proxy ───────────────────────────────────────────────────────────────

#[zbus::proxy(
    interface = "org.freedesktop.NetworkManager",
    default_service = "org.freedesktop.NetworkManager",
    default_path = "/org/freedesktop/NetworkManager"
)]
trait NetworkManager {
    #[zbus(property)]
    fn wireless_enabled(&self) -> zbus::Result<bool>;

    async fn set_wireless_enabled(&self, enabled: bool) -> zbus::Result<()>;
}

// ── Subscription ─────────────────────────────────────────────────────────────

/// Stream that emits `NetworkUpdate` messages whenever the NM state changes.
pub fn subscription() -> impl futures::Stream<Item = Message> + Send + 'static {
    iced::stream::channel(8, |mut tx: mpsc::Sender<Message>| async move {
        loop {
            match network_loop(&mut tx).await {
                Ok(()) => break,
                Err(e) => {
                    debug!("NetworkManager subscription error: {e}");
                    tokio::time::sleep(Duration::from_secs(5)).await;
                }
            }
        }
    })
}

async fn network_loop(tx: &mut mpsc::Sender<Message>) -> anyhow::Result<()> {
    use futures::StreamExt;

    let conn = zbus::Connection::system().await?;
    let proxy = NetworkManagerProxy::new(&conn).await?;

    // Emit initial state.
    let state = fetch_state(&conn).await;
    let _ = tx.try_send(Message::NetworkUpdate(state));

    // Watch for property changes.
    let mut changes = proxy.receive_wireless_enabled_changed().await;
    while changes.next().await.is_some() {
        let state = fetch_state(&conn).await;
        if tx.try_send(Message::NetworkUpdate(state)).is_err() {
            break;
        }
    }

    Ok(())
}

async fn fetch_state(conn: &zbus::Connection) -> NetworkState {
    let proxy = match NetworkManagerProxy::new(conn).await {
        Ok(p) => p,
        Err(e) => {
            warn!("NetworkManager proxy: {e}");
            return NetworkState::default();
        }
    };

    let wifi_enabled = proxy.wireless_enabled().await.unwrap_or(false);

    NetworkState { wifi_enabled }
}

// ── Actions ──────────────────────────────────────────────────────────────────

/// Enable or disable the Wi-Fi radio via NetworkManager.
pub async fn toggle_wifi(enabled: bool) {
    match zbus::Connection::system().await {
        Ok(conn) => {
            if let Ok(proxy) = NetworkManagerProxy::new(&conn).await {
                let _ = proxy.set_wireless_enabled(enabled).await;
            }
        }
        Err(e) => warn!("toggle_wifi: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn network_state_defaults() {
        let s = NetworkState::default();
        assert!(!s.wifi_enabled);
    }
}
