use std::time::Duration;

use futures::channel::mpsc;
use tracing::debug;

use crate::app::Message;

/// Snapshot of UPower battery state.
#[derive(Debug, Clone)]
pub struct BatteryState {
    /// Battery percentage 0–100.
    pub percentage: f64,
    /// Whether the battery is currently charging.
    pub charging: bool,
    /// Time-to-empty in seconds (`0` = unavailable/charging).
    pub time_to_empty: i64,
}

impl Default for BatteryState {
    fn default() -> Self {
        Self { percentage: 100.0, charging: true, time_to_empty: 0 }
    }
}

// ── zbus proxy ───────────────────────────────────────────────────────────────

/// UPower display device — aggregates all power sources.
#[zbus::proxy(
    interface = "org.freedesktop.UPower.Device",
    default_service = "org.freedesktop.UPower",
    default_path = "/org/freedesktop/UPower/devices/DisplayDevice"
)]
trait UPowerDevice {
    /// Battery percentage (0–100).
    #[zbus(property)]
    fn percentage(&self) -> zbus::Result<f64>;

    /// State: 1=charging, 2=discharging, 4=fully-charged.
    #[zbus(property)]
    fn state(&self) -> zbus::Result<u32>;

    /// Seconds to empty (0 if charging / unavailable).
    #[zbus(property)]
    fn time_to_empty(&self) -> zbus::Result<i64>;
}

// ── Subscription ─────────────────────────────────────────────────────────────

/// Stream that emits `BatteryUpdate` messages when UPower state changes.
pub fn subscription() -> impl futures::Stream<Item = Message> + Send + 'static {
    iced::stream::channel(8, |mut tx: mpsc::Sender<Message>| async move {
        loop {
            match upower_loop(&mut tx).await {
                Ok(()) => break,
                Err(e) => {
                    debug!("UPower subscription error: {e}");
                    tokio::time::sleep(Duration::from_secs(10)).await;
                }
            }
        }
    })
}

async fn upower_loop(tx: &mut mpsc::Sender<Message>) -> anyhow::Result<()> {
    use futures::StreamExt;

    let conn = zbus::Connection::system().await?;
    let proxy = UPowerDeviceProxy::new(&conn).await?;

    // Emit initial state.
    let state = read_battery(&proxy).await;
    let _ = tx.try_send(Message::BatteryUpdate(state));

    // Watch for percentage changes (also catches state transitions).
    let mut changes = proxy.receive_percentage_changed().await;
    while changes.next().await.is_some() {
        let state = read_battery(&proxy).await;
        if tx.try_send(Message::BatteryUpdate(state)).is_err() {
            break;
        }
    }

    Ok(())
}

async fn read_battery(proxy: &UPowerDeviceProxy<'_>) -> BatteryState {
    let percentage = proxy.percentage().await.unwrap_or(0.0);
    let state_u32 = proxy.state().await.unwrap_or(0);
    let time_to_empty = proxy.time_to_empty().await.unwrap_or(0);
    // State 1 = Charging, 4 = FullyCharged
    let charging = state_u32 == 1 || state_u32 == 4;
    BatteryState { percentage, charging, time_to_empty }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_battery_is_charging_full() {
        let s = BatteryState::default();
        assert_eq!(s.percentage, 100.0);
        assert!(s.charging);
    }
}
