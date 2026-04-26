//! IPC subscription — connects to the compositor socket and emits events.

use ipc::{deserialize_event, socket_path, ShellEvent, ShellRequest};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

use crate::Message;

/// Stable type-identity used to keep the subscription alive across redraws.
struct IpcWorker;

/// Returns a subscription that connects to the compositor IPC socket, reads
/// newline-delimited JSON events, and emits the relevant [`Message`] variants.
///
/// On disconnect, the subscription automatically retries every two seconds.
pub fn subscription() -> iced::Subscription<Message> {
    iced::Subscription::run_with(std::any::TypeId::of::<IpcWorker>(), |_| event_stream())
}

fn event_stream() -> impl iced::futures::Stream<Item = Message> {
    iced::stream::channel(
        64,
        |mut tx: iced::futures::channel::mpsc::Sender<Message>| async move {
        use iced::futures::SinkExt;

        loop {
            match UnixStream::connect(socket_path()).await {
                Ok(stream) => {
                    tracing::info!("IPC: connected to compositor socket");
                    let _ = tx.send(Message::IpcConnected).await;

                    let reader = BufReader::new(stream);
                    let mut lines = reader.lines();

                    loop {
                        match lines.next_line().await {
                            Ok(Some(line)) => {
                                let msg = match deserialize_event(&line) {
                                    Ok(ShellEvent::WindowOpened { window }) => {
                                        Message::WindowOpened(window)
                                    }
                                    Ok(ShellEvent::WindowClosed { window_id }) => {
                                        Message::WindowClosed(window_id)
                                    }
                                    Ok(ShellEvent::WindowStateChanged { window_id, state }) => {
                                        Message::WindowStateChanged { window_id, state }
                                    }
                                    Ok(ShellEvent::WindowAppIdChanged { window_id, app_id }) => {
                                        Message::WindowAppIdChanged { window_id, app_id }
                                    }
                                    Ok(_) => continue, // ignore other events
                                    Err(e) => {
                                        tracing::debug!("IPC: unrecognised line: {e}");
                                        continue;
                                    }
                                };
                                if tx.send(msg).await.is_err() {
                                    return;
                                }
                            }
                            Ok(None) => {
                                tracing::info!("IPC: compositor closed the socket");
                                break;
                            }
                            Err(e) => {
                                tracing::warn!("IPC: read error: {e}");
                                break;
                            }
                        }
                    }

                    let _ = tx.send(Message::IpcDisconnected).await;
                }
                Err(e) => {
                    tracing::debug!("IPC: connect failed ({e}), retrying in 2 s");
                }
            }

            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
    })
}

/// Sends a [`ShellRequest`] to the compositor.  Fire-and-forget — spawns a
/// detached tokio task that connects, writes the request, and closes.
pub fn send_request(request: ShellRequest) {
    tokio::spawn(async move {
        match UnixStream::connect(socket_path()).await {
            Ok(mut stream) => {
                let payload = ipc::serialize(&request);
                if let Err(e) = stream.write_all(payload.as_bytes()).await {
                    tracing::warn!("IPC: failed to send request: {e}");
                }
            }
            Err(e) => {
                tracing::warn!("IPC: could not connect to send request: {e}");
            }
        }
    });
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use ipc::socket_path;

    #[test]
    fn socket_path_ends_with_sock() {
        let p = socket_path();
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
        assert!(name.ends_with(".sock"), "expected .sock suffix, got {name}");
    }
}
