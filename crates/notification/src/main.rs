//! Notification daemon binary — registers the `org.freedesktop.Notifications`
//! D-Bus interface and logs incoming notifications.
//!
//! The receiver returned by [`NotificationServer::start`] is drained here so
//! every Notify / CloseNotification call is observable in the daemon log.

use notification::{NotificationEvent, NotificationServer};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();

    let conn = zbus::Connection::session().await?;
    let (_server, mut rx) = match NotificationServer::start(&conn).await {
        Ok(pair) => pair,
        Err(e) => {
            // GNOME / KDE / etc. ship their own notification daemon already
            // owning this name. In a nested-compositor playground this is
            // expected — log and exit cleanly instead of crashing.
            tracing::warn!(
                "could not register org.freedesktop.Notifications: {e}. \
                Another daemon already owns the name; running in standby."
            );
            // Park forever so the binary doesn't immediately die and the
            // launcher harness reports a healthy pid.
            std::future::pending::<()>().await;
            return Ok(());
        }
    };
    tracing::info!("org.freedesktop.Notifications registered on session bus");

    while let Some(event) = rx.recv().await {
        match event {
            NotificationEvent::New(n) => {
                tracing::info!(
                    id = n.id,
                    app = %n.app_name,
                    summary = %n.summary,
                    "notification posted"
                );
            }
            NotificationEvent::Replaced { old_id, notification: n } => {
                tracing::info!(
                    old_id,
                    new_id = n.id,
                    summary = %n.summary,
                    "notification replaced"
                );
            }
            NotificationEvent::Closed { id, reason } => {
                tracing::info!(id, ?reason, "notification closed");
            }
            NotificationEvent::ActionInvoked { id, action } => {
                tracing::info!(id, %action, "action invoked");
            }
        }
    }

    Ok(())
}
