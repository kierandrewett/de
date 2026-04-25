//! Parses the JSON options object written to stdin by the `portal` service.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// File chooser operation mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// Open one or more files/directories.
    OpenFile,
    /// Save a single file.
    SaveFile,
    /// Save multiple files.
    SaveFiles,
    /// Pick a color from the screen.
    PickColor,
}

/// A file type filter (name + list of `(type, pattern)` pairs).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileFilter {
    /// Human-readable filter name (e.g. `"Images"`).
    pub name: String,
    /// Patterns: `(0, "*.png")` = glob, `(1, "image/png")` = MIME.
    pub patterns: Vec<(u32, String)>,
}

/// An extra combo-box or checkbox choice shown in the dialog.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileChoice {
    /// Machine-readable ID.
    pub id: String,
    /// Display label.
    pub label: String,
    /// Combo-box options `[(id, label)]`.  Empty means checkbox.
    pub options: Vec<(String, String)>,
    /// Initially selected value.
    pub initial_selection: String,
}

/// Options object written to `portal-ui` stdin as a single JSON line.
#[derive(Debug, Serialize, Deserialize)]
pub struct Options {
    /// Dialog mode.
    pub mode: Mode,
    /// Window title.
    pub title: String,
    /// Label for the accept button.
    pub accept_label: String,
    /// Allow selecting multiple files (OpenFile only).
    #[serde(default)]
    pub multiple: bool,
    /// Allow selecting directories instead of files.
    #[serde(default)]
    pub directory: bool,
    /// File type filters.
    #[serde(default)]
    pub filters: Vec<FileFilter>,
    /// Extra choices.
    #[serde(default)]
    pub choices: Vec<FileChoice>,
    /// Initial directory.
    pub current_folder: Option<PathBuf>,
    /// Suggested filename (save mode).
    pub current_name: Option<String>,
}

/// Read [`Options`] from a single JSON line on stdin.
pub fn read_from_stdin() -> anyhow::Result<Options> {
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    let opts: Options = serde_json::from_str(line.trim_end())?;
    Ok(opts)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserialize_open_file_options() {
        let json = r#"{"mode":"open_file","title":"Open","accept_label":"Open"}"#;
        let opts: Options = serde_json::from_str(json).expect("parse");
        assert_eq!(opts.mode, Mode::OpenFile);
        assert!(!opts.multiple);
        assert!(opts.filters.is_empty());
    }

    #[test]
    fn deserialize_save_file_with_folder() {
        let json = r#"{"mode":"save_file","title":"Save","accept_label":"Save","current_folder":"/home/user","current_name":"doc.txt"}"#;
        let opts: Options = serde_json::from_str(json).expect("parse");
        assert_eq!(opts.mode, Mode::SaveFile);
        assert_eq!(opts.current_folder.as_deref(), Some(std::path::Path::new("/home/user")));
        assert_eq!(opts.current_name.as_deref(), Some("doc.txt"));
    }

    #[test]
    fn round_trip() {
        let opts = Options {
            mode: Mode::OpenFile,
            title: "Test".into(),
            accept_label: "OK".into(),
            multiple: true,
            directory: false,
            filters: vec![FileFilter {
                name: "Images".into(),
                patterns: vec![(0, "*.png".into())],
            }],
            choices: Vec::new(),
            current_folder: Some(PathBuf::from("/tmp")),
            current_name: None,
        };
        let json = serde_json::to_string(&opts).expect("serialize");
        let back: Options = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.mode, opts.mode);
        assert_eq!(back.multiple, opts.multiple);
        assert_eq!(back.filters.len(), 1);
    }
}
