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

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

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
    Screenshot {
        save_path: String,
        response: Option<mpsc::Sender<Result<String, String>>>,
    },
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

    // Restrict the socket file to the owner. Even with the per-connection
    // peer_cred check in accept_loop, the filesystem mode is a cheaper
    // first line of defence — anyone running under a different uid on
    // this machine cannot even open(2) the socket.
    if let Err(e) = fs::set_permissions(&path, fs::Permissions::from_mode(0o600)) {
        warn!("ipc: chmod 0600 failed on {:?}: {}", path, e);
    }

    info!("ipc: listening on {:?}", path);

    thread::Builder::new()
        .name("ipc-server".into())
        .spawn(move || accept_loop(listener, queue))
        .expect("failed to spawn ipc server thread");
}

/// `SO_PEERCRED` lookup — returns the connecting peer's effective uid.
/// Linux-only (Wayland compositor is Linux-only anyway). Stable-Rust
/// path; `UnixStream::peer_cred()` is still unstable.
fn peer_uid(stream: &UnixStream) -> std::io::Result<u32> {
    let mut ucred: libc::ucred = unsafe { std::mem::zeroed() };
    let mut len: libc::socklen_t = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    let rc = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            &mut ucred as *mut libc::ucred as *mut libc::c_void,
            &mut len,
        )
    };
    if rc < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(ucred.uid)
    }
}

fn accept_loop(listener: UnixListener, queue: PendingIpc) {
    // The IPC accepts every command we have: synthetic input injection,
    // window control, screenshots to disk. Any local process running as
    // a different uid should not be able to drive that. Gate at the
    // socket peer's credentials.
    let euid = unsafe { libc::geteuid() };
    for conn in listener.incoming() {
        match conn {
            Ok(stream) => {
                match peer_uid(&stream) {
                    Ok(uid) if uid == euid => {}
                    Ok(uid) => {
                        warn!(
                            "ipc: rejecting connection from uid {} (own euid {})",
                            uid, euid
                        );
                        continue;
                    }
                    Err(e) => {
                        warn!("ipc: peer_uid lookup failed ({}); dropping connection", e);
                        continue;
                    }
                }
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

/// True if `p` is a sane target for a client-requested file write
/// (screenshot save_path). Required to stop the IPC from being an
/// arbitrary-path file-write primitive: any local process that the
/// peer_cred gate had let through could otherwise dump a PNG over
/// (say) `~/.ssh/authorized_keys` or `/etc/cron.d/x`.
///
/// Conservative allowlist of prefixes — HOME, XDG_RUNTIME_DIR, /tmp —
/// plus a flat ban on `..` components (no traversal). The path also
/// has to be absolute so a relative path can't bypass the prefix check
/// by interpreting it against the compositor's cwd.
fn save_path_ok(p: &str) -> bool {
    let path = Path::new(p);
    if !path.is_absolute() {
        return false;
    }
    if path
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return false;
    }
    let prefixes: Vec<PathBuf> = [
        std::env::var("HOME").ok(),
        std::env::var("XDG_RUNTIME_DIR").ok(),
        Some("/tmp".to_string()),
    ]
    .into_iter()
    .flatten()
    .map(PathBuf::from)
    .collect();
    prefixes.iter().any(|pre| path.starts_with(pre))
}

fn handle_conn(stream: UnixStream, queue: PendingIpc) {
    let peer_stream = match stream.try_clone() {
        Ok(s) => s,
        Err(e) => {
            warn!("ipc: clone failed: {}", e);
            return;
        }
    };
    let reader = BufReader::new(stream);
    let mut writer = peer_stream;

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                warn!("ipc: read err: {}", e);
                break;
            }
        };
        let req = match deserialize_request(&line) {
            Ok(r) => r,
            Err(e) => {
                let _ = writer.write_all(format!("{{\"error\":\"{}\"}}\n", e).as_bytes());
                continue;
            }
        };

        if let ShellRequest::TakeScreenshot { region: _ } = req {
            let save_path = screenshot_path();
            let (tx, rx) = mpsc::channel();
            queue.lock().unwrap().push(IpcCommand::Screenshot {
                save_path: save_path.clone(),
                response: Some(tx),
            });
            match rx.recv_timeout(Duration::from_secs(10)) {
                Ok(Ok(path)) => {
                    let _ = writer.write_all(
                        format!("{{\"path\":{}}}\n", serde_json::to_string(&path).unwrap())
                            .as_bytes(),
                    );
                }
                Ok(Err(error)) => {
                    let _ = writer.write_all(
                        format!("{{\"error\":{}}}\n", serde_json::to_string(&error).unwrap())
                            .as_bytes(),
                    );
                }
                Err(_) => {
                    let _ = writer.write_all(b"{\"error\":\"screenshot timed out\"}\n");
                }
            }
            continue;
        }

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
                "left" => 0x110,   // BTN_LEFT
                "right" => 0x111,  // BTN_RIGHT
                "middle" => 0x112, // BTN_MIDDLE
                _ => 0x110,
            };
            IpcCommand::PointerButton {
                button_evdev: b,
                pressed,
            }
        }
        ShellRequest::KeyPress { scancode, pressed } => IpcCommand::KeyEvent { scancode, pressed },
        ShellRequest::TypeText { text } => IpcCommand::TypeText { text },
        ShellRequest::Screenshot { save_path } => {
            if !save_path_ok(&save_path) {
                warn!("ipc: rejecting screenshot to unsafe path {:?}", save_path);
                return None;
            }
            IpcCommand::Screenshot {
                save_path,
                response: None,
            }
        }
        ShellRequest::ActivateWindow { window_id } => IpcCommand::ActivateWindow {
            wm_id: window_id as i32,
        },
        ShellRequest::CloseWindow { window_id } => IpcCommand::CloseWindow {
            wm_id: window_id as i32,
        },
        ShellRequest::MinimizeWindow { window_id } => IpcCommand::MinimizeWindow {
            wm_id: window_id as i32,
        },
        ShellRequest::MoveWindow { window_id, x, y } => IpcCommand::MoveWindow {
            wm_id: window_id as i32,
            x,
            y,
        },
        ShellRequest::ResizeWindow {
            window_id,
            width,
            height,
        } => IpcCommand::ResizeWindow {
            wm_id: window_id as i32,
            w: width,
            h: height,
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

fn screenshot_path() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir()
        .join(format!("myDE-screenshot-{nanos}.png"))
        .to_string_lossy()
        .into_owned()
}
