//! D-Bus server implementing `org.freedesktop.Notifications`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use tokio::sync::mpsc::{self, Receiver, Sender};
use tracing::{debug, warn};
use zbus::object_server::SignalEmitter;
use zbus::zvariant::OwnedValue;
use zbus::{interface, Connection};

use crate::hints::parse_hints;
use crate::history::NotificationHistory;
use crate::types::{CloseReason, Notification, NotificationEvent};

// ---------------------------------------------------------------------------
// Internal shared state
// ---------------------------------------------------------------------------

struct NotificationState {
    active: HashMap<u32, Notification>,
    history: NotificationHistory,
    next_id: u32,
}

impl NotificationState {
    fn new() -> Self {
        Self { active: HashMap::new(), history: NotificationHistory::new(), next_id: 1 }
    }
}

// ---------------------------------------------------------------------------
// D-Bus interface object
// ---------------------------------------------------------------------------

struct NotificationIface {
    state: Arc<Mutex<NotificationState>>,
    tx: Sender<NotificationEvent>,
}

#[interface(name = "org.freedesktop.Notifications")]
impl NotificationIface {
    /// Creates or replaces a notification; returns its assigned ID.
    // 8 arguments is mandated by the org.freedesktop.Notifications D-Bus spec.
    #[allow(clippy::too_many_arguments)]
    async fn notify(
        &self,
        app_name: &str,
        replaces_id: u32,
        app_icon: &str,
        summary: &str,
        body: &str,
        actions: Vec<String>,
        hints: HashMap<String, OwnedValue>,
        expire_timeout: i32,
    ) -> zbus::fdo::Result<u32> {
        let (parsed_hints, timeout) = parse_hints(&hints, expire_timeout);

        // D-Bus sends actions as [key, label, key, label, …]
        let parsed_actions: Vec<(String, String)> = actions
            .chunks(2)
            .filter_map(|c| c.get(1).map(|label| (c[0].clone(), label.clone())))
            .collect();

        let (id, event) = {
            let mut state = self
                .state
                .lock()
                .map_err(|e| zbus::fdo::Error::Failed(format!("state lock poisoned: {e}")))?;

            let (id, old_id) =
                if replaces_id != 0 && state.active.contains_key(&replaces_id) {
                    (replaces_id, Some(replaces_id))
                } else {
                    let id = state.next_id;
                    state.next_id = state.next_id.checked_add(1).unwrap_or(1);
                    (id, None)
                };

            let notification = Notification {
                id,
                app_name: app_name.to_owned(),
                app_icon: app_icon.to_owned(),
                summary: summary.to_owned(),
                body: body.to_owned(),
                actions: parsed_actions,
                urgency: parsed_hints.urgency,
                image: parsed_hints.image,
                expire_timeout: timeout,
                timestamp: SystemTime::now(),
                category: parsed_hints.category,
                desktop_entry: parsed_hints.desktop_entry,
                transient: parsed_hints.transient,
                resident: parsed_hints.resident,
            };

            state.active.insert(id, notification.clone());

            let event = match old_id {
                Some(old) => {
                    state.history.replace(old, notification.clone());
                    NotificationEvent::Replaced { old_id: old, notification }
                }
                None => {
                    state.history.push(notification.clone());
                    NotificationEvent::New(notification)
                }
            };

            (id, event)
        };

        debug!(id, app_name, summary, "notification received");

        if let Err(e) = self.tx.send(event).await {
            warn!("notification channel closed: {e}");
        }

        Ok(id)
    }

    /// Closes a notification; called by the originating application.
    async fn close_notification(
        &self,
        id: u32,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<()> {
        let found = {
            let mut state = self
                .state
                .lock()
                .map_err(|e| zbus::fdo::Error::Failed(format!("state lock poisoned: {e}")))?;
            state.active.remove(&id).is_some()
        };

        if found {
            debug!(id, "notification closed by application");
            Self::notification_closed(&emitter, id, CloseReason::ClosedByApp.as_u32())
                .await
                .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;

            let event = NotificationEvent::Closed { id, reason: CloseReason::ClosedByApp };
            if let Err(e) = self.tx.send(event).await {
                warn!("notification channel closed: {e}");
            }
        }

        Ok(())
    }

    /// Returns the feature set this server supports.
    fn get_capabilities(&self) -> Vec<&'static str> {
        vec![
            "body",
            "body-markup",
            "body-hyperlinks",
            "actions",
            "icon-static",
            "persistence",
            "action-icons",
        ]
    }

    /// Returns server identification strings: `(name, vendor, version, spec_version)`.
    fn get_server_information(&self) -> (&'static str, &'static str, &'static str, &'static str) {
        ("myDE-notification", "myDE", "0.1.0", "1.2")
    }

    /// Emitted when a notification is closed for any reason.
    #[zbus(signal)]
    async fn notification_closed(
        emitter: &SignalEmitter<'_>,
        id: u32,
        reason: u32,
    ) -> zbus::Result<()>;

    /// Emitted when an action button is activated by the user.
    #[zbus(signal)]
    async fn action_invoked(
        emitter: &SignalEmitter<'_>,
        id: u32,
        action_key: &str,
    ) -> zbus::Result<()>;
}

// ---------------------------------------------------------------------------
// Public server handle
// ---------------------------------------------------------------------------

/// Handle to the running `org.freedesktop.Notifications` D-Bus server.
///
/// Created by [`NotificationServer::start`]. Drive the returned
/// [`Receiver`] on the shell's async task to process incoming notifications.
pub struct NotificationServer {
    connection: Connection,
    state: Arc<Mutex<NotificationState>>,
    tx: Sender<NotificationEvent>,
}

impl NotificationServer {
    /// Registers the D-Bus interface and requests the well-known name.
    ///
    /// Returns `(server, receiver)` on success. The caller must drive
    /// the `zbus::Connection` event loop (e.g. by keeping it alive on a
    /// tokio task) for method calls to be dispatched.
    pub async fn start(
        connection: &Connection,
    ) -> crate::Result<(Self, Receiver<NotificationEvent>)> {
        let (tx, rx) = mpsc::channel(128);
        let state = Arc::new(Mutex::new(NotificationState::new()));

        let iface = NotificationIface { state: Arc::clone(&state), tx: tx.clone() };

        connection
            .object_server()
            .at("/org/freedesktop/Notifications", iface)
            .await?;

        connection.request_name("org.freedesktop.Notifications").await?;

        debug!("notification server registered on session bus");

        Ok((Self { connection: connection.clone(), state, tx }, rx))
    }

    /// Closes a notification from the shell side (e.g. auto-expiry or dismiss button).
    ///
    /// Emits `NotificationClosed` on D-Bus and sends [`NotificationEvent::Closed`]
    /// to the consumer channel. No-op if `id` is not currently active.
    pub async fn close(&self, id: u32, reason: CloseReason) {
        let found = {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            state.active.remove(&id).is_some()
        };

        if !found {
            return;
        }

        match SignalEmitter::new(&self.connection, "/org/freedesktop/Notifications") {
            Ok(emitter) => {
                if let Err(e) =
                    NotificationIface::notification_closed(&emitter, id, reason.as_u32()).await
                {
                    warn!(id, ?reason, "failed to emit NotificationClosed: {e}");
                }
            }
            Err(e) => warn!("failed to create signal emitter for NotificationClosed: {e}"),
        }

        let event = NotificationEvent::Closed { id, reason };
        if let Err(e) = self.tx.send(event).await {
            warn!("notification channel closed: {e}");
        }

        debug!(id, ?reason, "notification closed by shell");
    }

    /// Invokes an action on a notification (called by the shell's rendering layer).
    ///
    /// Emits `ActionInvoked` on D-Bus, then closes the notification unless it
    /// carries the `resident` hint.
    pub async fn invoke_action(&self, id: u32, action: &str) {
        let resident = {
            let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            state.active.get(&id).map(|n| n.resident).unwrap_or(false)
        };

        match SignalEmitter::new(&self.connection, "/org/freedesktop/Notifications") {
            Ok(emitter) => {
                if let Err(e) =
                    NotificationIface::action_invoked(&emitter, id, action).await
                {
                    warn!(id, action, "failed to emit ActionInvoked: {e}");
                }
            }
            Err(e) => warn!("failed to create signal emitter for ActionInvoked: {e}"),
        }

        let event = NotificationEvent::ActionInvoked { id, action: action.to_owned() };
        if let Err(e) = self.tx.send(event).await {
            warn!("notification channel closed: {e}");
        }

        if !resident {
            self.close(id, CloseReason::Dismissed).await;
        }
    }

    /// Removes a single entry from the notification history.
    ///
    /// Does **not** affect active (currently displayed) notifications — use
    /// [`close`][Self::close] for that. This is for the notification-centre
    /// "clear single entry" action.
    pub fn clear_history_entry(&self, id: u32) {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .history
            .remove_by_id(id);
    }

    /// Returns a snapshot of the notification history (last 100 non-transient entries).
    ///
    /// The `Vec` is cloned from the internal state at the time of the call.
    /// Use this to populate a notification-centre UI on open.
    pub fn history(&self) -> Vec<Notification> {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .history
            .as_slice()
            .to_vec()
    }
}

// ---------------------------------------------------------------------------
// Tests (logic only — D-Bus integration requires a running session bus)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::SystemTime;
    use crate::types::Urgency;

    fn make_notification(id: u32, urgency: Urgency, resident: bool, transient: bool) -> Notification {
        Notification {
            id,
            app_name: "test-app".into(),
            app_icon: String::new(),
            summary: format!("Summary {id}"),
            body: String::new(),
            actions: vec![],
            urgency,
            image: None,
            expire_timeout: None,
            timestamp: SystemTime::now(),
            category: None,
            desktop_entry: None,
            transient,
            resident,
        }
    }

    #[test]
    fn initial_next_id_is_one() {
        let state = NotificationState::new();
        assert_eq!(state.next_id, 1);
    }

    #[test]
    fn id_increments_monotonically() {
        let mut state = NotificationState::new();
        let first = state.next_id;
        state.next_id = state.next_id.checked_add(1).unwrap_or(1);
        assert_eq!(first, 1);
        assert_eq!(state.next_id, 2);
    }

    #[test]
    fn id_wraps_from_u32_max_to_one() {
        let mut state = NotificationState::new();
        state.next_id = u32::MAX;
        let id = state.next_id;
        state.next_id = state.next_id.checked_add(1).unwrap_or(1);
        assert_eq!(id, u32::MAX);
        assert_eq!(state.next_id, 1, "should wrap to 1, not 0");
    }

    #[test]
    fn active_and_history_are_independent() {
        let mut state = NotificationState::new();
        let n = make_notification(1, Urgency::Normal, false, false);
        state.active.insert(1, n.clone());
        state.history.push(n);

        assert!(state.active.contains_key(&1));
        assert_eq!(state.history.as_slice().len(), 1);

        state.active.remove(&1);
        assert!(!state.active.contains_key(&1));
        // History is unchanged by active removal
        assert_eq!(state.history.as_slice().len(), 1);
    }

    #[test]
    fn history_snapshot_does_not_alias_state() {
        let mut state = NotificationState::new();
        state.history.push(make_notification(1, Urgency::Normal, false, false));

        let snapshot: Vec<Notification> = state.history.as_slice().to_vec();
        state.history.push(make_notification(2, Urgency::Normal, false, false));

        assert_eq!(snapshot.len(), 1, "snapshot should not see later pushes");
        assert_eq!(state.history.as_slice().len(), 2);
    }

    #[test]
    fn transient_notification_not_in_history() {
        let mut state = NotificationState::new();
        state.history.push(make_notification(1, Urgency::Normal, false, true));
        assert!(state.history.as_slice().is_empty());
    }
}
