//! Compositor IPC server — accepts shell client connections, dispatches
//! [`ShellRequest`]s into compositor state, and broadcasts [`ShellEvent`]s.
//!
//! Wire-format: newline-delimited JSON over a Unix domain socket at
//! `ipc::socket_path()`. Each client sends `ShellRequest` lines; the compositor
//! sends `ShellEvent` lines.
//!
//! Architecture:
//!   * Listener socket is registered as a calloop `Generic` source. Accepting
//!     a connection spawns a small reader thread that does blocking
//!     `read_line` on the client's read-half and forwards parsed requests to
//!     the calloop loop via an mpsc channel + `Channel` source.
//!   * The write-half of every live client is kept in `IpcServer.clients`
//!     (an `Arc<Mutex<Vec<UnixStream>>>`). [`IpcServer::broadcast`] iterates
//!     and writes synchronously, dropping any client whose write fails.
//!
//! All file descriptors are managed manually with `std::os::unix::net` —
//! there is no async runtime in the compositor's main loop.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::{Arc, Mutex};
use std::thread;

use ipc::{deserialize_request, serialize, socket_path, ShellEvent, ShellRequest};
use smithay::reexports::calloop::{
    channel::{channel, Channel, Event as ChannelEvent},
    generic::Generic,
    Interest, LoopHandle, Mode as CalloopMode, PostAction,
};

use crate::state::State;

/// IPC server state held inside [`crate::state::CommonState`].
pub struct IpcServer {
    /// Write-halves of every connected client.
    clients: Arc<Mutex<Vec<UnixStream>>>,
    /// Path to the bound socket, removed on drop.
    socket_path: std::path::PathBuf,
}

impl Drop for IpcServer {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.socket_path);
    }
}

impl IpcServer {
    /// Bind the IPC socket and register accept + per-client sources on
    /// `loop_handle`. Returns a handle whose `broadcast` method can be used
    /// to push events to all connected clients.
    pub fn start(loop_handle: LoopHandle<'static, State>) -> std::io::Result<Self> {
        let path = socket_path();
        // Best-effort cleanup of a stale socket from a previous run.
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path)?;
        listener.set_nonblocking(true)?;

        let clients: Arc<Mutex<Vec<UnixStream>>> = Arc::new(Mutex::new(Vec::new()));

        // Channel that reader threads use to deliver parsed requests to the
        // main loop.
        let (req_tx, req_rx): (_, Channel<ShellRequest>) = channel();

        // Accept source — wraps the listening socket as an fd source.
        let clients_for_accept = clients.clone();
        let req_tx_for_accept = req_tx.clone();
        let inner_loop = loop_handle.clone();
        loop_handle.insert_source(
            Generic::new(listener, Interest::READ, CalloopMode::Level),
            move |_readiness, listener, _state| {
                loop {
                    match listener.accept() {
                        Ok((stream, _addr)) => {
                            tracing::info!("ipc: client connected");
                            // Keep the write-half so we can broadcast back.
                            let write_half = match stream.try_clone() {
                                Ok(s) => s,
                                Err(e) => {
                                    tracing::warn!("ipc: try_clone failed: {e}");
                                    continue;
                                }
                            };
                            clients_for_accept.lock().unwrap().push(write_half);

                            // Spawn a small thread that reads lines and forwards
                            // ShellRequests via the calloop channel.
                            let req_tx = req_tx_for_accept.clone();
                            thread::Builder::new()
                                .name("ipc-reader".into())
                                .spawn(move || reader_loop(stream, req_tx))
                                .ok();
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                        Err(e) => {
                            tracing::warn!("ipc: accept error: {e}");
                            break;
                        }
                    }
                }
                Ok(PostAction::Continue)
            },
        ).map_err(|e| std::io::Error::other(format!("ipc accept source: {e}")))?;

        // Channel source — drains parsed ShellRequests into handlers.
        inner_loop.insert_source(req_rx, |event, _, state| {
            if let ChannelEvent::Msg(req) = event {
                handle_request(state, req);
            }
        }).map_err(|e| std::io::Error::other(format!("ipc channel source: {e}")))?;

        Ok(Self {
            clients,
            socket_path: path,
        })
    }

    /// Send `event` to every connected client. Clients whose socket has
    /// errored out are silently dropped.
    pub fn broadcast(&self, event: &ShellEvent) {
        let line = serialize(event);
        let bytes = line.as_bytes();
        let mut clients = self.clients.lock().unwrap();
        clients.retain_mut(|stream| {
            stream.write_all(bytes).is_ok() && stream.flush().is_ok()
        });
    }

    /// Number of currently connected clients (for tests / diagnostics).
    #[allow(dead_code)]
    pub fn client_count(&self) -> usize {
        self.clients.lock().unwrap().len()
    }
}

/// Read newline-delimited JSON from one client until the connection closes
/// or a parse error occurs. Each successfully parsed [`ShellRequest`] is
/// forwarded to the calloop main loop via `tx`.
fn reader_loop(stream: UnixStream, tx: smithay::reexports::calloop::channel::Sender<ShellRequest>) {
    // Switch back to blocking mode for line-based reads.
    if let Err(e) = stream.set_nonblocking(false) {
        tracing::warn!("ipc reader: set_nonblocking(false): {e}");
        return;
    }
    let reader = BufReader::new(stream);
    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                tracing::debug!("ipc reader: line error: {e}");
                return;
            }
        };
        if line.is_empty() {
            continue;
        }
        match deserialize_request(&line) {
            Ok(req) => {
                if tx.send(req).is_err() {
                    return; // main loop gone
                }
            }
            Err(e) => tracing::debug!("ipc reader: bad request line {line:?}: {e}"),
        }
    }
}

/// Dispatch a single [`ShellRequest`] into compositor state. Most variants
/// are best-effort and do not produce a response — the compositor broadcasts
/// state changes via [`ShellEvent`] independently.
fn handle_request(state: &mut State, req: ShellRequest) {
    use ipc::WindowInfo;

    match req {
        ShellRequest::ActivateWindow { window_id } => {
            state.common.shell.focus_window(window_id);
            broadcast_focus(state);
        }
        ShellRequest::MinimizeWindow { window_id } => {
            if let Some(w) = state.common.shell.window_mut(window_id) {
                w.is_minimized = true;
                broadcast_window_state(state, window_id);
            }
        }
        ShellRequest::UnminimizeWindow { window_id } => {
            if let Some(w) = state.common.shell.window_mut(window_id) {
                w.is_minimized = false;
                broadcast_window_state(state, window_id);
            }
        }
        ShellRequest::CloseWindow { window_id } => {
            // Window destruction is owned by the wayland xdg-shell handler;
            // we just drop our shell-side bookkeeping. Future: send the
            // xdg_toplevel.close request to the client too.
            state.common.shell.remove_window(window_id);
            state.common.ipc.broadcast(&ShellEvent::WindowClosed { window_id });
        }
        ShellRequest::GetAllWindows => {
            // Replay current windows as WindowOpened events for the requester.
            // Without per-client targeting we broadcast — clients filter.
            for info in state.common.shell.all_window_infos() {
                state.common.ipc.broadcast(&ShellEvent::WindowOpened { window: info });
            }
        }
        ShellRequest::GetFocusedWindow => {
            let info: Option<WindowInfo> = state
                .common
                .shell
                .focused_window_id()
                .and_then(|id| state.common.shell.window_info(id));
            state.common.ipc.broadcast(&ShellEvent::FocusedWindowChanged { window: info });
        }
        ShellRequest::GetWorkspaces => {
            // Single-workspace stub for now — multi-workspace support lives
            // in the shell module's roadmap.
            state.common.ipc.broadcast(&ShellEvent::WorkspaceChanged { index: 0, total: 1 });
        }
        ShellRequest::SwitchWorkspace { index } => {
            state.common.ipc.broadcast(&ShellEvent::WorkspaceChanged { index, total: 1 });
        }
        ShellRequest::Lock => {
            tracing::info!("ipc: Lock requested (session-lock not yet wired into shell)");
        }
        ShellRequest::TakeScreenshot { region: _ } => {
            // Region cropping is a follow-up; for now we always grab the
            // full output. The compositor reads back the framebuffer on
            // the next render and writes a PNG to a deterministic path.
            let path = std::env::var("XDG_RUNTIME_DIR")
                .map(|d| std::path::PathBuf::from(d).join("myDE-screenshot.png"))
                .unwrap_or_else(|_| std::path::PathBuf::from("/tmp/myDE-screenshot.png"));
            tracing::info!("ipc: TakeScreenshot -> {}", path.display());
            state.common.pending_screenshot = Some(path);
        }
        // The remaining variants either need backend support that doesn't
        // exist yet (output config, screencast) or system integration
        // outside the compositor (Shutdown/Reboot/Suspend via logind).
        // Log and ignore for now — clients tolerate missing replies.
        other => tracing::debug!("ipc: unhandled request {other:?}"),
    }
}

fn broadcast_focus(state: &mut State) {
    let info = state
        .common
        .shell
        .focused_window_id()
        .and_then(|id| state.common.shell.window_info(id));
    state.common.ipc.broadcast(&ShellEvent::FocusedWindowChanged { window: info });
}

fn broadcast_window_state(state: &mut State, window_id: u64) {
    if let Some(info) = state.common.shell.window_info(window_id) {
        state.common.ipc.broadcast(&ShellEvent::WindowStateChanged {
            window_id,
            state: info.state,
        });
    }
}
