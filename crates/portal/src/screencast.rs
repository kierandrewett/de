//! `org.freedesktop.impl.portal.ScreenCast` implementation.
#![allow(missing_docs)]
//!
//! Bridges compositor frame output into a PipeWire stream, enabling screen
//! sharing in browsers, Discord, OBS, etc.
//!
//! # Current status
//!
//! The D-Bus interface skeleton is present and correctly typed.  The PipeWire
//! stream creation (`pipewire-rs`) is stubbed with `// TODO` markers and
//! returns a "not implemented" response until the compositor exposes a
//! frame-push IPC and the PipeWire integration is wired up.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::Mutex;
use zbus::interface;
use zvariant::{OwnedValue, Str, Value};

/// Active ScreenCast session state.
#[derive(Debug, Default)]
struct Session {
    source_types: u32,
    multiple: bool,
}

/// Handler for the ScreenCast portal interface.
pub struct ScreenCastPortal {
    sessions: Arc<Mutex<HashMap<String, Session>>>,
}

impl ScreenCastPortal {
    /// Create a new [`ScreenCastPortal`].
    pub fn new() -> Self {
        Self {
            sessions: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

impl Default for ScreenCastPortal {
    fn default() -> Self {
        Self::new()
    }
}

#[allow(missing_docs)]
#[interface(name = "org.freedesktop.impl.portal.ScreenCast")]
impl ScreenCastPortal {
    /// Create a new ScreenCast session.
    async fn create_session(
        &self,
        options: HashMap<String, OwnedValue>,
    ) -> zbus::fdo::Result<(u32, HashMap<String, OwnedValue>)> {
        let token = options
            .get("session_handle_token")
            .and_then(|v| match &**v {
                Value::Str(s) => Some(s.to_string()),
                _ => None,
            })
            .unwrap_or_else(uuid_token);

        let handle = format!("/org/freedesktop/portal/desktop/session/myDE/{token}");

        let session = Session::default();
        self.sessions.lock().await.insert(handle.clone(), session);
        tracing::info!(%handle, "ScreenCast session created");

        let mut results: HashMap<String, OwnedValue> = HashMap::new();
        results.insert(
            "session_handle".into(),
            OwnedValue::from(Str::from(handle)),
        );
        Ok((0, results))
    }

    /// Select which sources (monitors / windows) the client can capture.
    async fn select_sources(
        &self,
        session_handle: zvariant::ObjectPath<'_>,
        options: HashMap<String, OwnedValue>,
    ) -> zbus::fdo::Result<(u32, HashMap<String, OwnedValue>)> {
        let handle = session_handle.as_str().to_owned();

        let source_types = options
            .get("types")
            .and_then(|v| match &**v {
                Value::U32(n) => Some(*n),
                _ => None,
            })
            .unwrap_or(1);

        let multiple = options
            .get("multiple")
            .and_then(|v| match &**v {
                Value::Bool(b) => Some(*b),
                _ => None,
            })
            .unwrap_or(false);

        let mut sessions = self.sessions.lock().await;
        if let Some(s) = sessions.get_mut(&handle) {
            s.source_types = source_types;
            s.multiple = multiple;
            tracing::debug!(%handle, source_types, multiple, "sources selected");
            Ok((0, HashMap::new()))
        } else {
            tracing::warn!(%handle, "SelectSources: unknown session");
            Ok((2, HashMap::new()))
        }
    }

    /// Start the capture stream.
    ///
    /// Returns `(1, {})` until PipeWire integration is implemented.
    async fn start(
        &self,
        session_handle: zvariant::ObjectPath<'_>,
        _parent_window: &str,
        _options: HashMap<String, OwnedValue>,
    ) -> zbus::fdo::Result<(u32, HashMap<String, OwnedValue>)> {
        let handle = session_handle.as_str();
        // TODO: create PipeWire stream and feed compositor frames into it.
        // TODO: return stream node ID in results["streams"] as a(ua{sv}).
        tracing::warn!(%handle, "ScreenCast Start not yet implemented (PipeWire pending)");
        Ok((1, HashMap::new()))
    }
}

fn uuid_token() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    format!("token{nanos:08x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uuid_token_is_not_empty() {
        assert!(!uuid_token().is_empty());
    }

    #[test]
    fn new_does_not_panic() {
        let _ = ScreenCastPortal::new();
    }
}
