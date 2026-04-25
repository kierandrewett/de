//! JSON dump of the widget tree for external tooling.

use crate::state::WidgetNode;
use std::io;
use std::path::PathBuf;

/// Returns the path to use for the JSON dump file.
///
/// Uses `$XDG_RUNTIME_DIR` when set, otherwise falls back to `/tmp`.
fn dump_path(app_name: &str) -> PathBuf {
    let dir = std::env::var("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            tracing::debug!(
                "XDG_RUNTIME_DIR not set, falling back to /tmp for devtools dump"
            );
            PathBuf::from("/tmp")
        });
    dir.join(format!("myDE-devtools-{app_name}.json"))
}

/// Serialises the widget tree to a JSON file and returns the parsed
/// [`serde_json::Value`] representation.
///
/// The output file is written to:
/// - `$XDG_RUNTIME_DIR/myDE-devtools-{app_name}.json` when the environment
///   variable is set.
/// - `/tmp/myDE-devtools-{app_name}.json` otherwise.
///
/// # Errors
///
/// Returns an [`io::Error`] if the file cannot be written or serialisation
/// fails.
pub fn dump_widget_tree(
    nodes: &[WidgetNode],
    app_name: &str,
) -> Result<serde_json::Value, io::Error> {
    let value = serde_json::to_value(nodes).map_err(|e| {
        io::Error::new(io::ErrorKind::InvalidData, e)
    })?;

    let path = dump_path(app_name);

    let json_string = serde_json::to_string_pretty(&value).map_err(|e| {
        io::Error::new(io::ErrorKind::InvalidData, e)
    })?;

    std::fs::write(&path, json_string)?;
    tracing::debug!(path = %path.display(), "devtools widget tree dumped");

    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{Bounds, WidgetNode};

    fn sample_nodes() -> Vec<WidgetNode> {
        vec![WidgetNode {
            id: 0,
            widget_type: "Container".to_owned(),
            bounds: Bounds::new(0.0, 0.0, 800.0, 600.0),
            content: None,
            children: vec![WidgetNode {
                id: 1,
                widget_type: "Text".to_owned(),
                bounds: Bounds::new(10.0, 10.0, 200.0, 20.0),
                content: Some("Hello".to_owned()),
                children: vec![],
            }],
        }]
    }

    #[test]
    fn dump_writes_json_file() {
        let nodes = sample_nodes();
        // Use /tmp so we don't need XDG_RUNTIME_DIR set.
        std::env::remove_var("XDG_RUNTIME_DIR");
        let result = dump_widget_tree(&nodes, "test-dump");
        assert!(result.is_ok(), "dump failed: {result:?}");

        let path = PathBuf::from("/tmp/myDE-devtools-test-dump.json");
        assert!(path.exists(), "dump file not created");

        let content = std::fs::read_to_string(&path).expect("read dump file");
        let parsed: serde_json::Value =
            serde_json::from_str(&content).expect("parse dump json");
        assert!(parsed.is_array());
        assert_eq!(parsed.as_array().unwrap().len(), 1);

        // Clean up.
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn dump_path_uses_xdg_runtime_dir() {
        std::env::set_var("XDG_RUNTIME_DIR", "/tmp");
        let p = dump_path("myapp");
        assert_eq!(p, PathBuf::from("/tmp/myDE-devtools-myapp.json"));
        std::env::remove_var("XDG_RUNTIME_DIR");
    }

    #[test]
    fn dump_path_falls_back_to_tmp() {
        std::env::remove_var("XDG_RUNTIME_DIR");
        let p = dump_path("myapp");
        assert_eq!(p, PathBuf::from("/tmp/myDE-devtools-myapp.json"));
    }

    #[test]
    fn serialised_value_matches_structure() {
        let nodes = sample_nodes();
        std::env::remove_var("XDG_RUNTIME_DIR");
        let value = dump_widget_tree(&nodes, "test-struct").unwrap();
        let _ = std::fs::remove_file("/tmp/myDE-devtools-test-struct.json");

        let first = &value[0];
        assert_eq!(first["widget_type"], "Container");
        assert_eq!(first["id"], 0);
        assert_eq!(first["children"][0]["widget_type"], "Text");
        assert_eq!(first["children"][0]["content"], "Hello");
    }
}
