//! `org.freedesktop.impl.portal.GlobalShortcuts` implementation.
#![allow(missing_docs)]
//!
//! Lets sandboxed apps register compositor-level keybindings.  Keybinding
//! registration is forwarded to the compositor over the IPC socket once the
//! compositor exposes a `RegisterShortcut` request (currently a TODO).

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::Mutex;
use zbus::interface;
use zvariant::{OwnedValue, Str};

/// Handler for the GlobalShortcuts portal interface.
pub struct GlobalShortcutsPortal {
    sessions: Arc<Mutex<HashMap<String, Vec<ShortcutEntry>>>>,
}

/// A registered global shortcut.
#[derive(Debug, Clone)]
struct ShortcutEntry {
    id: String,
    description: String,
}

impl GlobalShortcutsPortal {
    /// Create a new [`GlobalShortcutsPortal`].
    pub fn new() -> Self {
        Self {
            sessions: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

impl Default for GlobalShortcutsPortal {
    fn default() -> Self {
        Self::new()
    }
}

#[allow(missing_docs)]
#[interface(name = "org.freedesktop.impl.portal.GlobalShortcuts")]
impl GlobalShortcutsPortal {
    /// Create a GlobalShortcuts session.
    async fn create_session(
        &self,
        options: HashMap<String, OwnedValue>,
    ) -> zbus::fdo::Result<(u32, HashMap<String, OwnedValue>)> {
        let token = options
            .get("session_handle_token")
            .and_then(|v| match &**v {
                zvariant::Value::Str(s) => Some(s.to_string()),
                _ => None,
            })
            .unwrap_or_else(|| {
                use std::time::{SystemTime, UNIX_EPOCH};
                let n = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.subsec_nanos())
                    .unwrap_or(0);
                format!("gs{n:08x}")
            });

        let handle = format!("/org/freedesktop/portal/desktop/session/myDE/{token}");
        self.sessions.lock().await.insert(handle.clone(), Vec::new());

        let mut results: HashMap<String, OwnedValue> = HashMap::new();
        results.insert(
            "session_handle".into(),
            OwnedValue::from(Str::from(handle)),
        );
        Ok((0, results))
    }

    /// Bind shortcuts to the session.
    async fn bind_shortcuts(
        &self,
        session_handle: zvariant::ObjectPath<'_>,
        shortcuts: Vec<HashMap<String, OwnedValue>>,
        _parent_window: &str,
        _options: HashMap<String, OwnedValue>,
    ) -> zbus::fdo::Result<(u32, HashMap<String, OwnedValue>)> {
        let handle = session_handle.as_str().to_owned();
        let entries: Vec<ShortcutEntry> = shortcuts
            .iter()
            .filter_map(|s| {
                let id = match s.get("id") {
                    Some(v) => match &**v {
                        zvariant::Value::Str(s) => s.to_string(),
                        _ => return None,
                    },
                    None => return None,
                };
                let description = s
                    .get("description")
                    .and_then(|v| match &**v {
                        zvariant::Value::Str(s) => Some(s.to_string()),
                        _ => None,
                    })
                    .unwrap_or_default();
                Some(ShortcutEntry { id, description })
            })
            .collect();

        tracing::info!(%handle, count = entries.len(), "BindShortcuts");
        // TODO: forward to compositor IPC RegisterShortcut once available.

        let mut sessions = self.sessions.lock().await;
        if let Some(slot) = sessions.get_mut(&handle) {
            *slot = entries;
            Ok((0, HashMap::new()))
        } else {
            Ok((2, HashMap::new()))
        }
    }

    /// List currently registered shortcuts for a session.
    async fn list_shortcuts(
        &self,
        session_handle: zvariant::ObjectPath<'_>,
    ) -> zbus::fdo::Result<(u32, HashMap<String, OwnedValue>)> {
        use zvariant::{signature::Signature, Array, Value};

        let handle = session_handle.as_str();
        let sessions = self.sessions.lock().await;
        match sessions.get(handle) {
            Some(entries) => {
                let ss_sig =
                    Signature::static_structure(&[&Signature::Str, &Signature::Str]);
                let mut arr = Array::new(&ss_sig);
                for e in entries {
                    if let Ok(s) = zvariant::StructureBuilder::new()
                        .add_field(e.id.clone())
                        .add_field(e.description.clone())
                        .build()
                    {
                        let _ = arr.append(Value::Structure(s));
                    }
                }
                let mut results: HashMap<String, OwnedValue> = HashMap::new();
                if let Ok(v) = OwnedValue::try_from(arr) {
                    results.insert("shortcuts".into(), v);
                }
                Ok((0, results))
            }
            None => {
                tracing::warn!(%handle, "ListShortcuts: unknown session");
                Ok((2, HashMap::new()))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_does_not_panic() {
        let _ = GlobalShortcutsPortal::new();
    }
}
