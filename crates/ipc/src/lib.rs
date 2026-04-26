//! Shared IPC protocol types for compositor ↔ shell communication.
//!
//! Messages are newline-delimited JSON over a Unix domain socket located at
//! `$XDG_RUNTIME_DIR/myDE.sock`. The compositor acts as the server; shell
//! processes (panel, dock, launcher) connect as clients.
//!
//! Each message is a single JSON line terminated by `\n`. Use [`serialize`] to
//! encode and [`deserialize_request`] / [`deserialize_event`] to decode.
#![deny(missing_docs)]

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Crate-level error type — a JSON serialization/deserialization failure.
pub type Error = serde_json::Error;

/// Crate-level `Result` alias using [`Error`].
pub type Result<T> = std::result::Result<T, Error>;

// ---------------------------------------------------------------------------
// Shared geometry / layout types
// ---------------------------------------------------------------------------

/// An axis-aligned rectangle in screen coordinates (pixels).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rect {
    /// Horizontal offset from the left edge of the output, in pixels.
    pub x: i32,
    /// Vertical offset from the top edge of the output, in pixels.
    pub y: i32,
    /// Width in pixels.
    pub w: i32,
    /// Height in pixels.
    pub h: i32,
}

/// A rectangular region of the screen used to scope a screenshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScreenRegion {
    /// Horizontal offset from the left edge of the output, in pixels.
    pub x: i32,
    /// Vertical offset from the top edge of the output, in pixels.
    pub y: i32,
    /// Width in pixels.
    pub w: i32,
    /// Height in pixels.
    pub h: i32,
}

/// A single zone within a snap layout, expressed as fractions of the output
/// dimensions (values in `[0.0, 1.0]`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SnapZone {
    /// Horizontal position as a fraction of output width.
    pub x: f64,
    /// Vertical position as a fraction of output height.
    pub y: f64,
    /// Width as a fraction of output width.
    pub width: f64,
    /// Height as a fraction of output height.
    pub height: f64,
}

/// A named snap layout consisting of one or more zones.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SnapLayout {
    /// Human-readable layout name (e.g. `"halves-horizontal"`).
    pub name: String,
    /// The zones that make up this layout.
    pub zones: Vec<SnapZone>,
}

// ---------------------------------------------------------------------------
// Window types
// ---------------------------------------------------------------------------

/// The state flags for a managed window.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowState {
    /// Whether this window currently holds keyboard focus.
    pub is_focused: bool,
    /// Whether this window is maximized.
    pub is_maximized: bool,
    /// Whether this window is minimized (hidden from the workspace).
    pub is_minimized: bool,
    /// Whether this window occupies the full screen.
    pub is_fullscreen: bool,
}

/// A snapshot of metadata for a compositor-managed window.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowInfo {
    /// Compositor-assigned stable identifier for this window.
    pub id: u64,
    /// Wayland `app_id` (e.g. `"org.gnome.Nautilus"`), or an empty string if
    /// the client has not set one.
    pub app_id: String,
    /// Window title, or an empty string if unset.
    pub title: String,
    /// Current window state flags.
    pub state: WindowState,
    /// Window geometry in output-local pixel coordinates.
    pub geometry: Rect,
}

// ---------------------------------------------------------------------------
// Output types
// ---------------------------------------------------------------------------

/// Metadata and configuration for a single monitor output.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OutputInfo {
    /// Connector name as reported by the kernel (e.g. `"HDMI-A-1"`).
    pub name: String,
    /// Monitor manufacturer name.
    pub make: String,
    /// Monitor model name.
    pub model: String,
    /// Horizontal resolution in pixels.
    pub width: i32,
    /// Vertical resolution in pixels.
    pub height: i32,
    /// Horizontal position in the global compositor coordinate space.
    pub x: i32,
    /// Vertical position in the global compositor coordinate space.
    pub y: i32,
    /// Output scale factor (e.g. `2.0` for HiDPI, `1.25` for 125 % scaling).
    pub scale: f64,
    /// Whether this output is currently enabled.
    pub enabled: bool,
}

/// A desired configuration for one or more outputs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OutputConfig {
    /// The target state for each output.  Outputs omitted from this list are
    /// left unchanged.
    pub outputs: Vec<OutputInfo>,
}

// ---------------------------------------------------------------------------
// ShellRequest — shell → compositor
// ---------------------------------------------------------------------------

/// Requests that shell processes send to the compositor over the IPC socket.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ShellRequest {
    /// Retrieve the current list of workspaces and which one is active.
    GetWorkspaces,

    /// Switch the active workspace to the given zero-based index.
    SwitchWorkspace {
        /// Zero-based index of the workspace to activate.
        index: usize,
    },

    /// Retrieve metadata for the window that currently has keyboard focus.
    GetFocusedWindow,

    /// Retrieve all windows that belong to a specific application.
    GetWindowsForApp {
        /// The Wayland `app_id` to filter by.
        app_id: String,
    },

    /// Query the dock icon position for a given application.
    GetDockIconPosition {
        /// The Wayland `app_id` of the application.
        app_id: String,
    },

    /// Retrieve metadata for every managed window.
    GetAllWindows,

    /// Minimize the specified window.
    MinimizeWindow {
        /// Compositor-assigned window identifier.
        window_id: u64,
    },

    /// Restore (un-minimize) the specified window.
    UnminimizeWindow {
        /// Compositor-assigned window identifier.
        window_id: u64,
    },

    /// Close the specified window.
    CloseWindow {
        /// Compositor-assigned window identifier.
        window_id: u64,
    },

    /// Bring the specified window to the front and give it keyboard focus.
    ActivateWindow {
        /// Compositor-assigned window identifier.
        window_id: u64,
    },

    /// Move the specified window's top-left corner to the given logical
    /// coordinates inside its current output. Used by playground scripts
    /// to deterministically place windows for screenshots / focus tests.
    MoveWindow {
        /// Compositor-assigned window identifier.
        window_id: u64,
        /// Target X in logical pixels.
        x: i32,
        /// Target Y in logical pixels.
        y: i32,
    },

    /// Resize the specified window to the given logical size.
    ResizeWindow {
        /// Compositor-assigned window identifier.
        window_id: u64,
        /// Target width in logical pixels.
        width: i32,
        /// Target height in logical pixels.
        height: i32,
    },

    /// Apply a snap layout to a monitor.
    SetSnapLayout {
        /// Zero-based index of the target monitor.
        monitor_index: usize,
        /// The layout to apply.
        layout: SnapLayout,
    },

    /// Retrieve metadata for all connected outputs.
    GetOutputs,

    /// Apply a new output configuration.
    SetOutputConfig {
        /// The desired configuration.
        config: OutputConfig,
    },

    /// Begin a screen recording session.
    StartScreenRecording,

    /// End the current screen recording session.
    StopScreenRecording,

    /// Query whether a screen recording is currently in progress.
    GetScreencastState,

    /// Capture a screenshot, optionally restricted to a region.
    TakeScreenshot {
        /// If `Some`, only capture the given region.  If `None`, capture the
        /// entire output.
        region: Option<ScreenRegion>,
    },

    /// Dump the widget tree for a named shell surface (for debugging).
    DumpWidgetTree {
        /// The target surface name (e.g. `"panel"`, `"dock"`).
        target: String,
    },

    /// Lock the session immediately.
    Lock,

    /// Initiate a clean system shutdown.
    Shutdown,

    /// Initiate a system reboot.
    Reboot,

    /// Suspend the system.
    Suspend,

    /// Switch the compositor's UI theme (currently affects SSD chrome).
    SetTheme {
        /// Either `"light"` or `"dark"` (case-insensitive).
        mode: String,
    },
}

// ---------------------------------------------------------------------------
// ShellEvent — compositor → shell
// ---------------------------------------------------------------------------

/// Events that the compositor broadcasts to connected shell processes.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ShellEvent {
    /// The active workspace has changed.
    WorkspaceChanged {
        /// Zero-based index of the now-active workspace.
        index: usize,
        /// Total number of workspaces.
        total: usize,
    },

    /// The window with keyboard focus has changed (or focus was lost).
    FocusedWindowChanged {
        /// Metadata for the newly focused window, or `None` if no window has
        /// focus.
        window: Option<WindowInfo>,
    },

    /// A new window has been mapped and is visible on a workspace.
    WindowOpened {
        /// Metadata for the newly opened window.
        window: WindowInfo,
    },

    /// A window has been closed and unmapped.
    WindowClosed {
        /// Compositor-assigned identifier of the closed window.
        window_id: u64,
    },

    /// A window's title string has changed.
    WindowTitleChanged {
        /// Compositor-assigned window identifier.
        window_id: u64,
        /// The new title.
        title: String,
    },

    /// A window's `app_id` has changed.
    WindowAppIdChanged {
        /// Compositor-assigned window identifier.
        window_id: u64,
        /// The new `app_id`.
        app_id: String,
    },

    /// One or more window state flags have changed.
    WindowStateChanged {
        /// Compositor-assigned window identifier.
        window_id: u64,
        /// The updated state flags.
        state: WindowState,
    },

    /// The screen-recording state has changed.
    ScreencastStateChanged {
        /// Whether a recording is currently active.
        active: bool,
        /// Elapsed recording time in seconds, if a recording is active.
        elapsed_secs: Option<u64>,
    },

    /// The set of connected outputs or their configuration has changed.
    OutputsChanged {
        /// Current metadata for all outputs.
        outputs: Vec<OutputInfo>,
    },

    /// The global colour theme mode has changed (e.g. light → dark).
    ThemeChanged {
        /// Theme mode identifier (e.g. `"dark"`, `"light"`).
        mode: String,
    },
}

// ---------------------------------------------------------------------------
// Helper functions
// ---------------------------------------------------------------------------

/// Returns the path to the IPC Unix domain socket.
///
/// Uses `$XDG_RUNTIME_DIR` when set (the normal case inside a systemd/PAM
/// user session).  Falls back to the OS temporary directory so the path is
/// always valid, even in test environments where the variable is absent.
pub fn socket_path() -> PathBuf {
    let dir = std::env::var("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir());
    dir.join("myDE.sock")
}

/// Serializes a message to a newline-terminated JSON string.
///
/// Panics only if the value contains non-serializable data (e.g. a map with
/// non-string keys), which cannot happen for the types in this crate.
pub fn serialize(msg: &impl Serialize) -> String {
    let mut s = serde_json::to_string(msg).expect("ipc types are always serializable");
    s.push('\n');
    s
}

/// Deserializes a [`ShellRequest`] from a single JSON line.
///
/// The trailing `\n` is optional — [`str::trim_end`] is applied before
/// parsing.
pub fn deserialize_request(line: &str) -> Result<ShellRequest> {
    serde_json::from_str(line.trim_end())
}

/// Deserializes a [`ShellEvent`] from a single JSON line.
///
/// The trailing `\n` is optional — [`str::trim_end`] is applied before
/// parsing.
pub fn deserialize_event(line: &str) -> Result<ShellEvent> {
    serde_json::from_str(line.trim_end())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // --- serialize / deserialize_request round-trips ---

    #[test]
    fn serialize_appends_newline() {
        let req = ShellRequest::GetWorkspaces;
        let s = serialize(&req);
        assert!(s.ends_with('\n'), "serialized message must end with '\\n'");
    }

    #[test]
    fn round_trip_unit_request() {
        let req = ShellRequest::GetWorkspaces;
        let line = serialize(&req);
        let decoded = deserialize_request(&line).expect("deserialize failed");
        assert!(matches!(decoded, ShellRequest::GetWorkspaces));
    }

    #[test]
    fn round_trip_switch_workspace() {
        let req = ShellRequest::SwitchWorkspace { index: 3 };
        let line = serialize(&req);
        let decoded = deserialize_request(&line).expect("deserialize failed");
        assert!(matches!(decoded, ShellRequest::SwitchWorkspace { index: 3 }));
    }

    #[test]
    fn round_trip_take_screenshot_none() {
        let req = ShellRequest::TakeScreenshot { region: None };
        let line = serialize(&req);
        let decoded = deserialize_request(&line).expect("deserialize failed");
        assert!(matches!(decoded, ShellRequest::TakeScreenshot { region: None }));
    }

    #[test]
    fn round_trip_take_screenshot_some() {
        let region = ScreenRegion { x: 10, y: 20, w: 300, h: 200 };
        let req = ShellRequest::TakeScreenshot { region: Some(region.clone()) };
        let line = serialize(&req);
        let decoded = deserialize_request(&line).expect("deserialize failed");
        match decoded {
            ShellRequest::TakeScreenshot { region: Some(r) } => assert_eq!(r, region),
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn round_trip_set_snap_layout() {
        let layout = SnapLayout {
            name: "halves".into(),
            zones: vec![
                SnapZone { x: 0.0, y: 0.0, width: 0.5, height: 1.0 },
                SnapZone { x: 0.5, y: 0.0, width: 0.5, height: 1.0 },
            ],
        };
        let req = ShellRequest::SetSnapLayout { monitor_index: 0, layout: layout.clone() };
        let line = serialize(&req);
        let decoded = deserialize_request(&line).expect("deserialize failed");
        match decoded {
            ShellRequest::SetSnapLayout { monitor_index: 0, layout: l } => {
                assert_eq!(l, layout);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    // --- serialize / deserialize_event round-trips ---

    #[test]
    fn round_trip_workspace_changed() {
        let ev = ShellEvent::WorkspaceChanged { index: 1, total: 4 };
        let line = serialize(&ev);
        let decoded = deserialize_event(&line).expect("deserialize failed");
        assert!(matches!(decoded, ShellEvent::WorkspaceChanged { index: 1, total: 4 }));
    }

    #[test]
    fn round_trip_focused_window_none() {
        let ev = ShellEvent::FocusedWindowChanged { window: None };
        let line = serialize(&ev);
        let decoded = deserialize_event(&line).expect("deserialize failed");
        assert!(matches!(decoded, ShellEvent::FocusedWindowChanged { window: None }));
    }

    #[test]
    fn round_trip_window_opened() {
        let info = WindowInfo {
            id: 42,
            app_id: "org.example.App".into(),
            title: "My Window".into(),
            state: WindowState {
                is_focused: true,
                is_maximized: false,
                is_minimized: false,
                is_fullscreen: false,
            },
            geometry: Rect { x: 0, y: 30, w: 1280, h: 720 },
        };
        let ev = ShellEvent::WindowOpened { window: info.clone() };
        let line = serialize(&ev);
        let decoded = deserialize_event(&line).expect("deserialize failed");
        match decoded {
            ShellEvent::WindowOpened { window: w } => assert_eq!(w, info),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn round_trip_outputs_changed() {
        let output = OutputInfo {
            name: "DP-1".into(),
            make: "Dell".into(),
            model: "U2720Q".into(),
            width: 3840,
            height: 2160,
            x: 0,
            y: 0,
            scale: 2.0,
            enabled: true,
        };
        let ev = ShellEvent::OutputsChanged { outputs: vec![output.clone()] };
        let line = serialize(&ev);
        let decoded = deserialize_event(&line).expect("deserialize failed");
        match decoded {
            ShellEvent::OutputsChanged { outputs } => {
                assert_eq!(outputs.len(), 1);
                assert_eq!(outputs[0], output);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    // --- edge cases ---

    #[test]
    fn deserialize_request_strips_trailing_newline() {
        let json = r#"{"type":"Lock"}"#;
        let with_newline = format!("{json}\n");
        let r1 = deserialize_request(json).expect("no newline failed");
        let r2 = deserialize_request(&with_newline).expect("with newline failed");
        assert!(matches!(r1, ShellRequest::Lock));
        assert!(matches!(r2, ShellRequest::Lock));
    }

    #[test]
    fn deserialize_event_strips_trailing_newline() {
        let json = r#"{"type":"ThemeChanged","mode":"dark"}"#;
        let with_newline = format!("{json}\n");
        let e1 = deserialize_event(json).expect("no newline failed");
        let e2 = deserialize_event(&with_newline).expect("with newline failed");
        assert!(matches!(e1, ShellEvent::ThemeChanged { mode } if mode == "dark"));
        assert!(matches!(e2, ShellEvent::ThemeChanged { mode } if mode == "dark"));
    }

    #[test]
    fn deserialize_request_rejects_garbage() {
        assert!(deserialize_request("not json").is_err());
    }

    #[test]
    fn deserialize_event_rejects_garbage() {
        assert!(deserialize_event("{\"type\":\"UnknownEvent\"}").is_err());
    }

    #[test]
    fn socket_path_ends_with_filename() {
        let p = socket_path();
        assert_eq!(p.file_name().and_then(|n| n.to_str()), Some("myDE.sock"));
    }
}
