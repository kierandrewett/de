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
    /// Add or update a notification.
    async fn add_notification(
        &self,
        app_id: &str,
        id: &str,
        notification: HashMap<String, OwnedValue>,
    ) -> zbus::fdo::Result<()> {
        let title = extract_str(&notification, "title").unwrap_or_default();
        let body = extract_str(&notification, "body").unwrap_or_default();
        tracing::info!(app_id, id, %title, %body, "AddNotification");
        // TODO: forward to org.freedesktop.Notifications once that crate is available.
        Ok(())
    }

    /// Remove a notification by ID.
    async fn remove_notification(
        &self,
        app_id: &str,
        id: &str,
    ) -> zbus::fdo::Result<()> {
        tracing::info!(app_id, id, "RemoveNotification");
        // TODO: forward removal to org.freedesktop.Notifications.
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
