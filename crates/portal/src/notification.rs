//! `org.freedesktop.impl.portal.Notification` implementation.
#![allow(missing_docs)]
//!
//! Forwards notifications to the `org.freedesktop.Notifications` D-Bus
//! service (our dedicated notification daemon, `crates/notification`).

use std::collections::HashMap;

use zbus::interface;
use zvariant::OwnedValue;

/// Handler for the Notification portal interface.
pub struct NotificationPortal;

impl NotificationPortal {
    /// Create a new [`NotificationPortal`].
    pub fn new() -> Self {
        Self
    }
}

impl Default for NotificationPortal {
    fn default() -> Self {
        Self::new()
    }
}

#[allow(missing_docs)]
#[interface(name = "org.freedesktop.impl.portal.Notification")]
impl NotificationPortal {
    /// Add or update a notification. Forwards to the
    /// `org.freedesktop.Notifications` daemon (`crates/notification`).
    async fn add_notification(
        &self,
        app_id: &str,
        id: &str,
        notification: HashMap<String, OwnedValue>,
    ) -> zbus::fdo::Result<()> {
        let title = extract_str(&notification, "title").unwrap_or_default();
        let body = extract_str(&notification, "body").unwrap_or_default();
        tracing::info!(app_id, id, %title, %body, "AddNotification");

        let conn = zbus::Connection::session().await
            .map_err(|e| zbus::fdo::Error::Failed(format!("session bus: {e}")))?;
        // Forward to org.freedesktop.Notifications.Notify
        // (app_name, replaces_id, app_icon, summary, body, actions, hints, expire_timeout)
        #[allow(clippy::type_complexity)]
        let notify_args: (&str, u32, &str, &str, &str, Vec<&str>, HashMap<&str, zbus::zvariant::Value>, i32) =
            (app_id, 0, "", &title, &body, Vec::new(), HashMap::new(), -1);
        conn.call_method(
            Some("org.freedesktop.Notifications"),
            "/org/freedesktop/Notifications",
            Some("org.freedesktop.Notifications"),
            "Notify",
            &notify_args,
        )
        .await
        .map_err(|e| zbus::fdo::Error::Failed(format!("Notify forward: {e}")))?;
        Ok(())
    }

    /// Remove a notification by ID. Forwards `CloseNotification` to the
    /// notification daemon when the portal id parses as a u32.
    async fn remove_notification(
        &self,
        app_id: &str,
        id: &str,
    ) -> zbus::fdo::Result<()> {
        tracing::info!(app_id, id, "RemoveNotification");
        let Ok(notify_id) = id.parse::<u32>() else {
            // Portal ids may be opaque strings; without a u32 mapping we can
            // only log and acknowledge.
            return Ok(());
        };
        let conn = zbus::Connection::session().await
            .map_err(|e| zbus::fdo::Error::Failed(format!("session bus: {e}")))?;
        conn.call_method(
            Some("org.freedesktop.Notifications"),
            "/org/freedesktop/Notifications",
            Some("org.freedesktop.Notifications"),
            "CloseNotification",
            &(notify_id,),
        )
        .await
        .ok();
        Ok(())
    }
}

fn extract_str(map: &HashMap<String, OwnedValue>, key: &str) -> Option<String> {
    match &**map.get(key)? {
        zvariant::Value::Str(s) => Some(s.to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_does_not_panic() {
        let _ = NotificationPortal::new();
    }
}
