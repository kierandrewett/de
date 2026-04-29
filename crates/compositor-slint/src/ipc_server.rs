//! Unix-socket IPC server. Lets out-of-process drivers (playground scripts,
//! agents, debug tooling) introspect and drive the compositor without going
//! through the host OS.
//!
//! Wire protocol: newline-delimited JSON, types from the `ipc` crate.
//!
//! Threading model: a single dedicated background thread accepts connections
//! and pushes parsed `ShellRequest` values into shared queues that the main
//! loop drains each iteration. We never call into the WM / Slint state
//! directly from the IPC thread — all mutations happen on the main thread.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread;

use ipc::{deserialize_request, ShellRequest};
use tracing::{info, warn};

/// Per-request command types the main loop will drain. Keeps the IPC enum
/// (which is shared with potential future shell processes) decoupled from the
/// internal compositor command shape.
#[derive(Debug, Clone)]
pub enum IpcCommand {
    /// Move the synthetic pointer to (x, y).
    PointerMove { x: f64, y: f64 },
    /// Press or release a mouse button at the current pointer position.
    PointerButton { button_evdev: u32, pressed: bool },
    /// Press or release a key by evdev scancode.
    KeyEvent { scancode: u32, pressed: bool },
    /// Type each character in the string as a press+release pair.
    TypeText { text: String },
    /// Capture a screenshot to the given absolute path (PNG).
    Screenshot { save_path: String },
    /// Activate a WM window by id.
    ActivateWindow { wm_id: i32 },
    /// Close a WM window.
    CloseWindow { wm_id: i32 },
    /// Minimize a WM window.
    MinimizeWindow { wm_id: i32 },
    /// Move a WM window to (x, y).
    MoveWindow { wm_id: i32, x: i32, y: i32 },
    /// Resize a WM window to (w, h).
    ResizeWindow { wm_id: i32, w: i32, h: i32 },
    /// Dump a JSON list of windows to a file (used by `GetAllWindows`).
    DumpWindows { save_path: String },
    /// Switch theme to "light" or "dark".
    SetTheme { mode: String },
}

/// Shared queue of pending IPC commands.
pub type PendingIpc = Arc<Mutex<Vec<IpcCommand>>>;

/// Spawn a background thread that owns the unix listener.
///
/// `socket_path` defaults to `$XDG_RUNTIME_DIR/myDE.sock` when None.
pub fn spawn(socket_path: Option<&Path>, queue: PendingIpc) {
    let path = socket_path
        .map(|p| p.to_path_buf())
        .unwrap_or_else(ipc::socket_path);

    // Best-effort cleanup of a stale socket (compositor previously crashed).
    let _ = std::fs::remove_file(&path);

    let listener = match UnixListener::bind(&path) {
        Ok(l) => l,
        Err(e) => {
            warn!("ipc: failed to bind {:?}: {}", path, e);
            return;
        }
    };

    info!("ipc: listening on {:?}", path);

    thread::Builder::new()
        .name("ipc-server".into())
        .spawn(move || accept_loop(listener, queue))
        .expect("failed to spawn ipc server thread");
}

fn accept_loop(listener: UnixListener, queue: PendingIpc) {
    for conn in listener.incoming() {
        match conn {
            Ok(stream) => {
                let q = queue.clone();
                thread::Builder::new()
                    .name("ipc-conn".into())
                    .spawn(move || handle_conn(stream, q))
                    .ok();
            }
            Err(e) => warn!("ipc: accept failed: {}", e),
        }
    }
}

fn handle_conn(stream: UnixStream, queue: PendingIpc) {
    let peer_stream = match stream.try_clone() {
        Ok(s) => s,
        Err(e) => { warn!("ipc: clone failed: {}", e); return }
    };
    let reader = BufReader::new(stream);
    let mut writer = peer_stream;

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(e) => { warn!("ipc: read err: {}", e); break }
        };
        let req = match deserialize_request(&line) {
            Ok(r) => r,
            Err(e) => {
                let _ = writer.write_all(
                    format!("{{\"error\":\"{}\"}}\n", e).as_bytes(),
                );
                continue;
            }
        };

        let cmd = translate(req);
        if let Some(cmd) = cmd {
            queue.lock().unwrap().push(cmd);
            let _ = writer.write_all(b"{\"ok\":true}\n");
        } else {
            let _ = writer.write_all(b"{\"ok\":false,\"error\":\"unsupported\"}\n");
        }
    }
}

fn translate(req: ShellRequest) -> Option<IpcCommand> {
    Some(match req {
        ShellRequest::MovePointer { x, y } => IpcCommand::PointerMove { x, y },
        ShellRequest::ClickPointer { button, pressed } => {
            let b = match button.to_ascii_lowercase().as_str() {
                "left"   => 0x110, // BTN_LEFT
                "right"  => 0x111, // BTN_RIGHT
                "middle" => 0x112, // BTN_MIDDLE
                _ => 0x110,
            };
            IpcCommand::PointerButton { button_evdev: b, pressed }
        }
        ShellRequest::KeyPress { scancode, pressed } => IpcCommand::KeyEvent { scancode, pressed },
        ShellRequest::TypeText { text } => IpcCommand::TypeText { text },
        ShellRequest::Screenshot { save_path } => IpcCommand::Screenshot { save_path },
        ShellRequest::ActivateWindow { window_id } => IpcCommand::ActivateWindow { wm_id: window_id as i32 },
        ShellRequest::CloseWindow { window_id }   => IpcCommand::CloseWindow   { wm_id: window_id as i32 },
        ShellRequest::MinimizeWindow { window_id } => IpcCommand::MinimizeWindow { wm_id: window_id as i32 },
        ShellRequest::MoveWindow { window_id, x, y } => IpcCommand::MoveWindow { wm_id: window_id as i32, x, y },
        ShellRequest::ResizeWindow { window_id, width, height } => IpcCommand::ResizeWindow {
            wm_id: window_id as i32, w: width, h: height,
        },
        ShellRequest::SetTheme { mode } => IpcCommand::SetTheme { mode },
        ShellRequest::GetAllWindows => IpcCommand::DumpWindows {
            // No response channel yet — just dump to a stable path the caller polls.
            save_path: "/tmp/myDE-windows.json".to_string(),
        },
        // Variants we don't act on yet.
        _ => return None,
    })
}
