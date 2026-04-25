//! Notification history — a fixed-size ring buffer of non-transient notifications.

use crate::types::Notification;

const MAX_HISTORY: usize = 100;

/// Fixed-capacity store of recent notifications for the notification centre.
///
/// Transient notifications are never stored. Once the buffer reaches
/// [`MAX_HISTORY`] entries, the oldest is evicted on each push.
#[derive(Debug, Default)]
pub(crate) struct NotificationHistory {
    inner: Vec<Notification>,
}

impl NotificationHistory {
    /// Creates an empty history.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Appends `notification` unless it is transient.
    pub(crate) fn push(&mut self, notification: Notification) {
        if notification.transient {
            return;
        }
        if self.inner.len() >= MAX_HISTORY {
            self.inner.remove(0);
        }
        self.inner.push(notification);
    }

    /// Removes the notification with `id` from history, if present.
    pub(crate) fn remove_by_id(&mut self, id: u32) {
        self.inner.retain(|n| n.id != id);
    }

    /// Replaces the notification with `old_id` in-place.
    ///
    /// If `old_id` is not in history, the new notification is pushed normally.
    /// If the replacement is transient, the old entry is removed.
    pub(crate) fn replace(&mut self, old_id: u32, notification: Notification) {
        if let Some(pos) = self.inner.iter().position(|n| n.id == old_id) {
            if notification.transient {
                self.inner.remove(pos);
            } else {
                self.inner[pos] = notification;
            }
        } else {
            self.push(notification);
        }
    }

    /// Returns a slice of all stored notifications, oldest first.
    pub(crate) fn as_slice(&self) -> &[Notification] {
        &self.inner
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::SystemTime;

    fn make_notification(id: u32, transient: bool) -> Notification {
        Notification {
            id,
            app_name: "test".into(),
            app_icon: String::new(),
            summary: format!("Notification {id}"),
            body: String::new(),
            actions: vec![],
            urgency: crate::types::Urgency::Normal,
            image: None,
            expire_timeout: None,
            timestamp: SystemTime::now(),
            category: None,
            desktop_entry: None,
            transient,
            resident: false,
        }
    }

    #[test]
    fn push_stores_non_transient() {
        let mut h = NotificationHistory::new();
        h.push(make_notification(1, false));
        assert_eq!(h.as_slice().len(), 1);
        assert_eq!(h.as_slice()[0].id, 1);
    }

    #[test]
    fn push_ignores_transient() {
        let mut h = NotificationHistory::new();
        h.push(make_notification(1, true));
        assert!(h.as_slice().is_empty());
    }

    #[test]
    fn evicts_oldest_when_full() {
        let mut h = NotificationHistory::new();
        for i in 1..=(MAX_HISTORY as u32 + 1) {
            h.push(make_notification(i, false));
        }
        assert_eq!(h.as_slice().len(), MAX_HISTORY);
        assert_eq!(h.as_slice()[0].id, 2, "oldest entry should be evicted");
    }

    #[test]
    fn remove_by_id_deletes_entry() {
        let mut h = NotificationHistory::new();
        h.push(make_notification(1, false));
        h.push(make_notification(2, false));
        h.remove_by_id(1);
        assert_eq!(h.as_slice().len(), 1);
        assert_eq!(h.as_slice()[0].id, 2);
    }

    #[test]
    fn remove_by_id_noop_for_missing() {
        let mut h = NotificationHistory::new();
        h.push(make_notification(1, false));
        h.remove_by_id(99);
        assert_eq!(h.as_slice().len(), 1);
    }

    #[test]
    fn replace_updates_in_place() {
        let mut h = NotificationHistory::new();
        h.push(make_notification(1, false));
        let mut updated = make_notification(1, false);
        updated.summary = "Updated".into();
        h.replace(1, updated);
        assert_eq!(h.as_slice().len(), 1);
        assert_eq!(h.as_slice()[0].summary, "Updated");
    }

    #[test]
    fn replace_with_transient_removes_old() {
        let mut h = NotificationHistory::new();
        h.push(make_notification(1, false));
        h.replace(1, make_notification(1, true));
        assert!(h.as_slice().is_empty());
    }

    #[test]
    fn replace_missing_id_pushes_new() {
        let mut h = NotificationHistory::new();
        h.replace(99, make_notification(99, false));
        assert_eq!(h.as_slice().len(), 1);
    }
}
