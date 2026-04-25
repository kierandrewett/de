# SUBAGENT: Shared IPC Message Types
# Crate: `crates/ipc`

> **Read `ARCHITECTURE.md` in the repo root before starting.** It explains why we use Smithay (not wlroots), how the compositor rendering pipeline works, the full Wayland protocol reference, smithay API patterns (delegate pattern, GlesRenderer, calloop), and the winit dev backend.
# Branch: `feat/ipc`

You are building the shared IPC protocol types for communication between the compositor and shell processes (panel, dock, launcher). Messages are newline-delimited JSON over a unix domain socket at `$XDG_RUNTIME_DIR/myDE.sock`.

## Protocol
The compositor is the server. Shell processes connect as clients. Each message is a single JSON line terminated by `\n`.

```rust
use serde::{Deserialize, Serialize};

/// Requests from shell processes to the compositor
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ShellRequest {
    GetWorkspaces,
    SwitchWorkspace { index: usize },
    GetFocusedWindow,
    GetWindowsForApp { app_id: String },
    GetDockIconPosition { app_id: String },
    GetAllWindows,
    MinimizeWindow { window_id: u64 },
    UnminimizeWindow { window_id: u64 },
    CloseWindow { window_id: u64 },
    ActivateWindow { window_id: u64 },
    SetSnapLayout { monitor_index: usize, layout: SnapLayout },
    GetOutputs,
    SetOutputConfig { config: OutputConfig },
    StartScreenRecording,
    StopScreenRecording,
    GetScreencastState,
    TakeScreenshot { region: Option<ScreenRegion> },
    DumpWidgetTree { target: String },
    Lock,
    Shutdown,
    Reboot,
    Suspend,
}

/// Events from the compositor to shell processes
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ShellEvent {
    WorkspaceChanged { index: usize, total: usize },
    FocusedWindowChanged { window: Option<WindowInfo> },
    WindowOpened { window: WindowInfo },
    WindowClosed { window_id: u64 },
    WindowTitleChanged { window_id: u64, title: String },
    WindowAppIdChanged { window_id: u64, app_id: String },
    WindowStateChanged { window_id: u64, state: WindowState },
    ScreencastStateChanged { active: bool, elapsed_secs: Option<u64> },
    OutputsChanged { outputs: Vec<OutputInfo> },
    ThemeChanged { mode: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowInfo {
    pub id: u64,
    pub app_id: String,
    pub title: String,
    pub state: WindowState,
    pub geometry: Rect,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowState {
    pub is_focused: bool,
    pub is_maximized: bool,
    pub is_minimized: bool,
    pub is_fullscreen: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Rect { pub x: i32, pub y: i32, pub w: i32, pub h: i32 }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapLayout { pub name: String, pub zones: Vec<SnapZone> }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapZone { pub x: f64, pub y: f64, pub width: f64, pub height: f64 }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutputInfo { pub name: String, pub make: String, pub model: String, pub width: i32, pub height: i32, pub x: i32, pub y: i32, pub scale: f64, pub enabled: bool }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutputConfig { pub outputs: Vec<OutputInfo> }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScreenRegion { pub x: i32, pub y: i32, pub w: i32, pub h: i32 }
```

Also provide helper functions:
```rust
pub fn socket_path() -> PathBuf; // $XDG_RUNTIME_DIR/myDE.sock
pub fn serialize(msg: &impl Serialize) -> String; // JSON + \n
pub fn deserialize_request(line: &str) -> Result<ShellRequest>;
pub fn deserialize_event(line: &str) -> Result<ShellEvent>;
```

## Cargo.toml
```toml
[package]
name = "ipc"
version = "0.1.0"
edition = "2021"
[dependencies]
serde = { workspace = true }
serde_json = { workspace = true }
```

Keep this crate minimal — it's just types + serde. No I/O, no async. The compositor and shell processes each handle their own socket I/O.
