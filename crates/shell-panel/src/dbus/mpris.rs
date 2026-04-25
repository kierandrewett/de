use std::collections::HashMap;
use std::time::Duration;

use futures::channel::mpsc;
use tracing::{debug, warn};
use zbus::zvariant::OwnedValue;

use crate::app::Message;

/// Now-playing state from an active MPRIS media player.
#[derive(Debug, Clone)]
pub struct MprisState {
    /// Track title.
    pub title: String,
    /// Artist(s).
    pub artist: String,
    /// Whether the player is currently playing (vs. paused).
    pub playing: bool,
}

// ── zbus proxies ─────────────────────────────────────────────────────────────

#[zbus::proxy(
    interface = "org.mpris.MediaPlayer2.Player",
    default_service = "org.mpris.MediaPlayer2",
    default_path = "/org/mpris/MediaPlayer2"
)]
trait MprisPlayer {
    async fn play_pause(&self) -> zbus::Result<()>;
    async fn next(&self) -> zbus::Result<()>;
    async fn previous(&self) -> zbus::Result<()>;

    #[zbus(property)]
    fn playback_status(&self) -> zbus::Result<String>;

    #[zbus(property)]
    fn metadata(&self) -> zbus::Result<HashMap<String, OwnedValue>>;

    #[zbus(property)]
    fn volume(&self) -> zbus::Result<f64>;

    #[zbus(property)]
    fn set_volume(&self, value: f64) -> zbus::Result<()>;
}

// ── Subscription ─────────────────────────────────────────────────────────────

/// Stream that emits `MprisUpdate` messages when the active player changes.
pub fn subscription() -> impl futures::Stream<Item = Message> + Send + 'static {
    iced::stream::channel(8, |mut tx: mpsc::Sender<Message>| async move {
        loop {
            match mpris_loop(&mut tx).await {
                Ok(()) => break,
                Err(e) => {
                    debug!("MPRIS subscription error: {e}");
                    tokio::time::sleep(Duration::from_secs(5)).await;
                }
            }
        }
    })
}

async fn mpris_loop(tx: &mut mpsc::Sender<Message>) -> anyhow::Result<()> {
    use futures::StreamExt;
    use zbus::fdo::DBusProxy;

    let conn = zbus::Connection::session().await?;

    // Discover active MPRIS players.
    let dbus = DBusProxy::new(&conn).await?;
    let names = dbus.list_names().await?;
    let player = names
        .iter()
        .find(|n| n.starts_with("org.mpris.MediaPlayer2."))
        .cloned();

    let Some(bus_name) = player else {
        let _ = tx.try_send(Message::MprisUpdate(None));
        // Wait for a new name to appear.
        let mut owner_changed = dbus.receive_name_owner_changed().await?;
        while let Some(sig) = owner_changed.next().await {
            if let Ok(args) = sig.args() {
                if args.name.starts_with("org.mpris.MediaPlayer2.") {
                    // New player appeared; restart the loop.
                    break;
                }
            }
        }
        return Ok(());
    };

    let proxy = MprisPlayerProxy::builder(&conn)
        .destination(bus_name.as_str())?
        .build()
        .await?;

    // Emit initial state.
    let state = read_mpris(&proxy).await;
    let _ = tx.try_send(Message::MprisUpdate(Some(state)));

    // Watch PropertiesChanged signals.
    let mut changes = proxy.receive_playback_status_changed().await;
    while changes.next().await.is_some() {
        let state = read_mpris(&proxy).await;
        if tx.try_send(Message::MprisUpdate(Some(state))).is_err() {
            break;
        }
    }

    Ok(())
}

async fn read_mpris(proxy: &MprisPlayerProxy<'_>) -> MprisState {
    let status = proxy.playback_status().await.unwrap_or_default();
    let playing = status == "Playing";
    let meta = proxy.metadata().await.unwrap_or_default();

    let title = extract_string(&meta, "xesam:title").unwrap_or_else(|| "Unknown".into());
    let artist = extract_artists(&meta);

    MprisState { title, artist, playing }
}

fn extract_string(meta: &HashMap<String, OwnedValue>, key: &str) -> Option<String> {
    meta.get(key).and_then(|v| String::try_from(&**v).ok())
}

fn extract_artists(meta: &HashMap<String, OwnedValue>) -> String {
    use zbus::zvariant::Value;
    let Some(owned) = meta.get("xesam:artist") else {
        return "Unknown Artist".into();
    };
    if let Value::Array(arr) = &**owned {
        let parts: Vec<String> = arr.iter()
            .filter_map(|item| String::try_from(item).ok())
            .collect();
        if !parts.is_empty() {
            return parts.join(", ");
        }
    }
    String::try_from(&**owned).unwrap_or_else(|_| "Unknown Artist".into())
}

// ── Actions ──────────────────────────────────────────────────────────────────

async fn with_first_player<F, Fut>(f: F)
where
    F: FnOnce(MprisPlayerProxy<'static>) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    let conn = match zbus::Connection::session().await {
        Ok(c) => c,
        Err(e) => {
            warn!("MPRIS action: no session bus: {e}");
            return;
        }
    };
    let dbus = match zbus::fdo::DBusProxy::new(&conn).await {
        Ok(p) => p,
        Err(e) => {
            warn!("MPRIS action: DBusProxy: {e}");
            return;
        }
    };
    let names = match dbus.list_names().await {
        Ok(n) => n,
        Err(_) => return,
    };
    let Some(bus_name) = names.iter().find(|n| n.starts_with("org.mpris.MediaPlayer2.")).map(|n| n.to_string())
    else {
        return;
    };

    match MprisPlayerProxy::builder(&conn).destination(bus_name) {
        Ok(b) => {
            if let Ok(proxy) = b.build().await {
                f(proxy).await;
            }
        }
        Err(e) => warn!("MPRIS proxy builder: {e}"),
    }
}

/// Send play/pause to the active MPRIS player.
pub async fn play_pause() {
    with_first_player(|p| async move {
        let _ = p.play_pause().await;
    })
    .await;
}

/// Skip to the next track.
pub async fn next() {
    with_first_player(|p| async move {
        let _ = p.next().await;
    })
    .await;
}

/// Go to the previous track.
pub async fn prev() {
    with_first_player(|p| async move {
        let _ = p.previous().await;
    })
    .await;
}

/// Set the player volume.
pub async fn set_volume(level: f64) {
    with_first_player(move |p| async move {
        let _ = p.set_volume(level).await;
    })
    .await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_string_missing_key_returns_none() {
        let map = HashMap::new();
        assert!(extract_string(&map, "xesam:title").is_none());
    }

    #[test]
    fn extract_artists_missing_returns_unknown() {
        let map = HashMap::new();
        assert_eq!(extract_artists(&map), "Unknown Artist");
    }
}
