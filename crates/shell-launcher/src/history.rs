//! Persistent launch-frequency tracking.
//!
//! Stores per-app launch counts in
//! `~/.local/share/myDE/launch_history.json`.  Used to boost recently-used
//! applications in search results.

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Serialisable launch-count map.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct LaunchHistory {
    /// Map of app name → launch count.
    #[serde(default)]
    counts: HashMap<String, u32>,
}

impl LaunchHistory {
    /// Load from disk, returning an empty history on any error.
    pub fn load() -> Self {
        let path = history_path();
        let Ok(content) = std::fs::read_to_string(&path) else {
            return Self::default();
        };
        serde_json::from_str(&content).unwrap_or_default()
    }

    /// Persist the history to disk.
    pub fn save(&self) {
        let path = history_path();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match serde_json::to_string_pretty(self) {
            Ok(json) => {
                if let Err(e) = std::fs::write(&path, json) {
                    tracing::warn!("failed to save launch history: {e}");
                }
            }
            Err(e) => tracing::warn!("failed to serialise launch history: {e}"),
        }
    }

    /// Increment the launch count for `app_name` and persist.
    pub fn record(&mut self, app_name: &str) {
        *self.counts.entry(app_name.to_owned()).or_insert(0) += 1;
        self.save();
    }

    /// Current launch-count map, for passing to the search scorer.
    pub fn counts(&self) -> &HashMap<String, u32> {
        &self.counts
    }

    /// Launch count for a specific app (used in tests and future UI).
    #[allow(dead_code)]
    pub fn count_for(&self, app_name: &str) -> u32 {
        self.counts.get(app_name).copied().unwrap_or(0)
    }
}

fn history_path() -> PathBuf {
    if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home).join(".local/share/myDE/launch_history.json")
    } else {
        PathBuf::from("/tmp/myDE_launch_history.json")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    fn with_temp_home(f: impl FnOnce()) {
        let dir = std::env::temp_dir().join(format!("launcher_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        env::set_var("HOME", &dir);
        f();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn record_increments_count() {
        with_temp_home(|| {
            let mut h = LaunchHistory::default();
            h.record("Firefox");
            h.record("Firefox");
            assert_eq!(h.count_for("Firefox"), 2);
        });
    }

    #[test]
    fn save_and_reload() {
        with_temp_home(|| {
            let mut h = LaunchHistory::default();
            h.record("Thunderbird");
            let loaded = LaunchHistory::load();
            assert_eq!(loaded.count_for("Thunderbird"), 1);
        });
    }

    #[test]
    fn missing_file_gives_empty_history() {
        with_temp_home(|| {
            let h = LaunchHistory::load();
            assert!(h.counts.is_empty());
        });
    }
}
