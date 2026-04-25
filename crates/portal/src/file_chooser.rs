//! `org.freedesktop.impl.portal.FileChooser` implementation.
#![allow(missing_docs)]
//!
//! Spawns the `portal-ui` binary as a child process, passes options via
//! stdin JSON, and reads the JSON result from stdout.

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use zbus::interface;
use zvariant::{Array, OwnedValue, Str, StructureBuilder, Value};


/// Options passed to the file chooser UI process.
#[derive(Debug, Serialize, Deserialize)]
pub struct FileChooserOptions {
    /// Dialog mode.
    pub mode: FileChooserMode,
    /// Window title.
    pub title: String,
    /// Label for the accept button.
    pub accept_label: String,
    /// Allow selecting multiple files.
    pub multiple: bool,
    /// Allow selecting directories.
    pub directory: bool,
    /// File type filters.
    pub filters: Vec<FileFilter>,
    /// Extra choices (combo-boxes / checkboxes).
    pub choices: Vec<FileChoice>,
    /// Initial directory.
    pub current_folder: Option<PathBuf>,
    /// Suggested filename (save mode).
    pub current_name: Option<String>,
}

/// File chooser operation type.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FileChooserMode {
    /// Open one or more files.
    OpenFile,
    /// Save a single file.
    SaveFile,
    /// Save multiple files.
    SaveFiles,
}

/// A file type filter (name + list of (type, pattern) pairs).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileFilter {
    /// Human-readable filter name (e.g. `"Images"`).
    pub name: String,
    /// Patterns: `(0, "*.png")` = glob, `(1, "image/png")` = MIME type.
    pub patterns: Vec<(u32, String)>,
}

/// An extra user-selectable choice in the dialog.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileChoice {
    /// Machine-readable ID.
    pub id: String,
    /// Display label.
    pub label: String,
    /// Options for a combo-box (`[(id, label)]`), or empty for a checkbox.
    pub options: Vec<(String, String)>,
    /// Initially selected value.
    pub initial_selection: String,
}

/// Result written to stdout by the `portal-ui` process.
#[derive(Debug, Serialize, Deserialize)]
pub struct FileChooserResult {
    /// 0 = success, 1 = cancelled, 2 = error.
    pub response: u32,
    /// Selected URIs (may be empty on cancel/error).
    #[serde(default)]
    pub uris: Vec<String>,
    /// Choices made by the user (`id → selected_value`).
    #[serde(default)]
    pub choices: HashMap<String, String>,
    /// The filter the user selected, if any.
    #[serde(default)]
    pub current_filter: Option<FileFilter>,
    /// Error message (only present when `response == 2`).
    #[serde(default)]
    pub error: Option<String>,
}

/// Handler for the FileChooser portal interface.
pub struct FileChooserPortal;

impl FileChooserPortal {
    /// Create a new [`FileChooserPortal`].
    pub fn new() -> Self {
        Self
    }

    /// Locate the `portal-ui` executable.
    fn portal_ui_bin() -> PathBuf {
        if let Ok(exe) = std::env::current_exe() {
            let candidate = exe.with_file_name("portal-ui");
            if candidate.exists() {
                return candidate;
            }
        }
        PathBuf::from("portal-ui")
    }

    /// Spawn `portal-ui`, write `opts` to its stdin, and read the JSON result.
    async fn run_ui(opts: FileChooserOptions) -> anyhow::Result<FileChooserResult> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::process::Command;

        let input = serde_json::to_string(&opts)?;
        let mut child = Command::new(Self::portal_ui_bin())
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::inherit())
            .spawn()?;

        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(input.as_bytes()).await?;
            stdin.write_all(b"\n").await?;
        }

        let mut stdout_bytes = Vec::new();
        if let Some(mut out) = child.stdout.take() {
            out.read_to_end(&mut stdout_bytes).await?;
        }

        let status = child.wait().await?;
        if !status.success() {
            tracing::warn!(code = ?status.code(), "portal-ui exited with failure");
        }

        let result: FileChooserResult = serde_json::from_slice(&stdout_bytes)?;
        Ok(result)
    }

    /// Convert a `portal-ui` result into a D-Bus response tuple.
    fn to_dbus_result(r: FileChooserResult) -> (u32, HashMap<String, OwnedValue>) {
        let mut results: HashMap<String, OwnedValue> = HashMap::new();

        if r.response == 0 {
            // uris: as — array of strings
            let arr: Array<'static> = Array::from(r.uris);
            if let Ok(v) = OwnedValue::try_from(arr) {
                results.insert("uris".into(), v);
            }

            // choices: a(ss)
            let choices_arr = build_choices_array(&r.choices);
            if let Some(v) = choices_arr {
                results.insert("choices".into(), v);
            }

            // current_filter: (sa(us))
            if let Some(f) = r.current_filter {
                if let Some(fv) = encode_filter(&f) {
                    results.insert("current_filter".into(), fv);
                }
            }
        }

        (r.response, results)
    }

    /// Parse `options: a{sv}` for OpenFile/SaveFile methods.
    fn parse_opts(
        mode: FileChooserMode,
        title: &str,
        options: &HashMap<String, OwnedValue>,
    ) -> FileChooserOptions {
        let accept_label = extract_str(options, "accept_label").unwrap_or_else(|| match mode {
            FileChooserMode::OpenFile => "Open".into(),
            FileChooserMode::SaveFile | FileChooserMode::SaveFiles => "Save".into(),
        });
        let multiple = extract_bool(options, "multiple").unwrap_or(false);
        let directory = extract_bool(options, "directory").unwrap_or(false);
        let current_folder = extract_bytes(options, "current_folder").and_then(|b| {
            let s = b.strip_suffix(b"\0").unwrap_or(&b);
            std::str::from_utf8(s).ok().map(PathBuf::from)
        });
        let current_name = extract_str(options, "current_name");
        let filters = extract_filters(options);

        FileChooserOptions {
            mode,
            title: title.to_owned(),
            accept_label,
            multiple,
            directory,
            filters,
            choices: Vec::new(),
            current_folder,
            current_name,
        }
    }
}

impl Default for FileChooserPortal {
    fn default() -> Self {
        Self::new()
    }
}

#[allow(missing_docs)]
#[interface(name = "org.freedesktop.impl.portal.FileChooser")]
impl FileChooserPortal {
    /// Open a file (or multiple files).
    async fn open_file(
        &self,
        _handle: &str,
        _app_id: &str,
        _parent_window: &str,
        title: &str,
        options: HashMap<String, OwnedValue>,
    ) -> zbus::fdo::Result<(u32, HashMap<String, OwnedValue>)> {
        let opts = Self::parse_opts(FileChooserMode::OpenFile, title, &options);
        match Self::run_ui(opts).await {
            Ok(r) => Ok(Self::to_dbus_result(r)),
            Err(e) => {
                tracing::error!(error = %e, "portal-ui failed for OpenFile");
                Ok((2, HashMap::new()))
            }
        }
    }

    /// Save a single file.
    async fn save_file(
        &self,
        _handle: &str,
        _app_id: &str,
        _parent_window: &str,
        title: &str,
        options: HashMap<String, OwnedValue>,
    ) -> zbus::fdo::Result<(u32, HashMap<String, OwnedValue>)> {
        let opts = Self::parse_opts(FileChooserMode::SaveFile, title, &options);
        match Self::run_ui(opts).await {
            Ok(r) => Ok(Self::to_dbus_result(r)),
            Err(e) => {
                tracing::error!(error = %e, "portal-ui failed for SaveFile");
                Ok((2, HashMap::new()))
            }
        }
    }

    /// Save multiple files.
    async fn save_files(
        &self,
        _handle: &str,
        _app_id: &str,
        _parent_window: &str,
        title: &str,
        options: HashMap<String, OwnedValue>,
    ) -> zbus::fdo::Result<(u32, HashMap<String, OwnedValue>)> {
        let opts = Self::parse_opts(FileChooserMode::SaveFiles, title, &options);
        match Self::run_ui(opts).await {
            Ok(r) => Ok(Self::to_dbus_result(r)),
            Err(e) => {
                tracing::error!(error = %e, "portal-ui failed for SaveFiles");
                Ok((2, HashMap::new()))
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Option extraction helpers
// ---------------------------------------------------------------------------

fn extract_str(opts: &HashMap<String, OwnedValue>, key: &str) -> Option<String> {
    match &**opts.get(key)? {
        Value::Str(s) => Some(s.to_string()),
        _ => None,
    }
}

fn extract_bool(opts: &HashMap<String, OwnedValue>, key: &str) -> Option<bool> {
    match &**opts.get(key)? {
        Value::Bool(b) => Some(*b),
        _ => None,
    }
}

fn extract_bytes(opts: &HashMap<String, OwnedValue>, key: &str) -> Option<Vec<u8>> {
    match &**opts.get(key)? {
        Value::Array(arr) => arr
            .inner()
            .iter()
            .map(|v| match v {
                Value::U8(b) => Some(*b),
                _ => None,
            })
            .collect(),
        _ => None,
    }
}

fn extract_filters(opts: &HashMap<String, OwnedValue>) -> Vec<FileFilter> {
    let arr = match opts.get("filters") {
        Some(v) => match &**v {
            Value::Array(a) => a,
            _ => return Vec::new(),
        },
        None => return Vec::new(),
    };

    arr.inner()
        .iter()
        .filter_map(|entry| {
            let s = match entry {
                Value::Structure(s) => s,
                _ => return None,
            };
            let fields = s.fields();
            let name = match fields.first() {
                Some(Value::Str(n)) => n.to_string(),
                _ => return None,
            };
            let patterns_arr = match fields.get(1) {
                Some(Value::Array(a)) => a,
                _ => return None,
            };
            let patterns: Vec<(u32, String)> = patterns_arr
                .inner()
                .iter()
                .filter_map(|p| {
                    let ps = match p {
                        Value::Structure(s) => s,
                        _ => return None,
                    };
                    let pf = ps.fields();
                    let t = match pf.first() {
                        Some(Value::U32(u)) => *u,
                        _ => return None,
                    };
                    let pat = match pf.get(1) {
                        Some(Value::Str(s)) => s.to_string(),
                        _ => return None,
                    };
                    Some((t, pat))
                })
                .collect();
            Some(FileFilter { name, patterns })
        })
        .collect()
}

fn build_choices_array(choices: &HashMap<String, String>) -> Option<OwnedValue> {
    use zvariant::{signature::Signature, Array, StructureBuilder};

    let element_sig = Signature::static_structure(&[&Signature::Str, &Signature::Str]);
    let mut arr = Array::new(&element_sig);
    for (k, v) in choices {
        let structure = StructureBuilder::new()
            .add_field(k.clone())
            .add_field(v.clone())
            .build()
            .ok()?;
        arr.append(Value::Structure(structure)).ok()?;
    }
    OwnedValue::try_from(arr).ok()
}

fn encode_filter(f: &FileFilter) -> Option<OwnedValue> {
    use zvariant::{signature::Signature, Array};

    // Element signature for each pattern entry: (us)
    let us_sig = Signature::static_structure(&[&Signature::U32, &Signature::Str]);
    let mut patterns_arr = Array::new(&us_sig);
    for (t, p) in &f.patterns {
        let s = StructureBuilder::new()
            .add_field(*t)
            .add_field(p.clone())
            .build()
            .ok()?;
        patterns_arr.append(Value::Structure(s)).ok()?;
    }

    // Outer structure: (s a(us))
    let patterns_ov = OwnedValue::try_from(patterns_arr).ok()?;
    let outer = StructureBuilder::new()
        .add_field(Str::from(f.name.clone()))
        .append_field(Value::from(patterns_ov))
        .build()
        .ok()?;
    OwnedValue::try_from(outer).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_empty_options() {
        let opts = FileChooserPortal::parse_opts(
            FileChooserMode::OpenFile,
            "Open",
            &HashMap::new(),
        );
        assert_eq!(opts.accept_label, "Open");
        assert!(!opts.multiple);
        assert!(!opts.directory);
        assert!(opts.filters.is_empty());
    }

    #[test]
    fn to_dbus_result_cancelled() {
        let r = FileChooserResult {
            response: 1,
            uris: Vec::new(),
            choices: HashMap::new(),
            current_filter: None,
            error: None,
        };
        let (code, map) = FileChooserPortal::to_dbus_result(r);
        assert_eq!(code, 1);
        assert!(map.is_empty());
    }

    #[test]
    fn to_dbus_result_success_has_uris() {
        let mut choices = HashMap::new();
        choices.insert("encoding".into(), "utf8".into());
        let r = FileChooserResult {
            response: 0,
            uris: vec!["file:///home/user/doc.txt".into()],
            choices,
            current_filter: None,
            error: None,
        };
        let (code, map) = FileChooserPortal::to_dbus_result(r);
        assert_eq!(code, 0);
        assert!(map.contains_key("uris"));
        assert!(map.contains_key("choices"));
    }
}
