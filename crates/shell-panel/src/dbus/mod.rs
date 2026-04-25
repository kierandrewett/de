pub mod bluetooth;
pub mod logind;
pub mod mpris;
pub mod network;
pub mod upower;

use std::time::Duration;

use futures::channel::mpsc;
use status_notifier::{StatusNotifierWatcher, TrayEvent};
use tokio::io::AsyncBufReadExt;
use tracing::{debug, warn};

use crate::app::Message;

// ── IPC ──────────────────────────────────────────────────────────────────────

/// Subscription that streams IPC events from the compositor.
///
/// Reconnects automatically with a 1-second back-off.
pub fn ipc_subscription() -> impl futures::Stream<Item = Message> + Send + 'static {
    iced::stream::channel(16, |mut tx: mpsc::Sender<Message>| async move {
        loop {
            match ipc_loop(&mut tx).await {
                Ok(()) => break,
                Err(e) => {
                    debug!("IPC disconnected: {e}, reconnecting in 1 s");
                    let _ = tx.try_send(Message::IpcDisconnected);
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
            }
        }
    })
}

async fn ipc_loop(tx: &mut mpsc::Sender<Message>) -> anyhow::Result<()> {
    use tokio::io::AsyncWriteExt;

    let path = ipc::socket_path();
    let mut stream = tokio::net::UnixStream::connect(&path).await?;

    // Subscribe to events by sending a GetFocusedWindow request (compositor
    // will keep the connection open and send events as they occur).
    let sub_req = ipc::serialize(&ipc::ShellRequest::GetFocusedWindow);
    stream.write_all(sub_req.as_bytes()).await?;

    let reader = tokio::io::BufReader::new(stream);
    let mut lines = reader.lines();

    while let Some(line) = lines.next_line().await? {
        if let Ok(event) = ipc::deserialize_event(&line) {
            if tx.try_send(Message::IpcEvent(event)).is_err() {
                return Ok(());
            }
        }
    }
    Ok(())
}

// ── Tray ─────────────────────────────────────────────────────────────────────

/// Subscription that runs the StatusNotifierWatcher and forwards tray events.
///
/// The watcher is kept alive inside the stream future for the application's
/// lifetime so tray apps can continue registering.
pub fn tray_subscription() -> impl futures::Stream<Item = Message> + Send + 'static {
    iced::stream::channel(64, |mut tx: mpsc::Sender<Message>| async move {
        let conn = match zbus::Connection::session().await {
            Ok(c) => c,
            Err(e) => {
                warn!("Cannot connect to D-Bus session bus for tray: {e}");
                return;
            }
        };

        let (watcher, mut rx) = match StatusNotifierWatcher::start(&conn).await {
            Ok(r) => r,
            Err(e) => {
                warn!("StatusNotifierWatcher failed to start: {e}");
                return;
            }
        };

        while let Some(event) = rx.recv().await {
            let msg = match event {
                TrayEvent::ItemRegistered(id) => {
                    match watcher.get_item(&id).await {
                        Ok(item) => Message::TrayItemAdded(id, Box::new(item)),
                        Err(e) => {
                            debug!("Could not load tray item {id}: {e}");
                            continue;
                        }
                    }
                }
                TrayEvent::ItemUnregistered(id) => Message::TrayItemRemoved(id),
                TrayEvent::ItemUpdated { id, .. } => {
                    match watcher.refresh_item(&id).await {
                        Ok(item) => Message::TrayItemAdded(id, Box::new(item)),
                        Err(e) => {
                            debug!("Could not refresh tray item {id}: {e}");
                            continue;
                        }
                    }
                }
            };

            if tx.try_send(msg).is_err() {
                break;
            }
        }
    })
}

// ── Brightness ───────────────────────────────────────────────────────────────

/// Write a brightness value (0.0 – 1.0) to the first available backlight.
pub async fn set_brightness(level: f32) {
    let level = level.clamp(0.0, 1.0);
    let backlight_dir = std::path::Path::new("/sys/class/backlight");

    let mut read_dir = match tokio::fs::read_dir(backlight_dir).await {
        Ok(d) => d,
        Err(_) => return,
    };

    if let Ok(Some(entry)) = read_dir.next_entry().await {
        let max_path = entry.path().join("max_brightness");
        let brightness_path = entry.path().join("brightness");

        if let Ok(max_str) = tokio::fs::read_to_string(&max_path).await {
            if let Ok(max) = max_str.trim().parse::<u64>() {
                let value = (level as f64 * max as f64).round() as u64;
                let _ = tokio::fs::write(&brightness_path, value.to_string()).await;
            }
        }
    }
}
