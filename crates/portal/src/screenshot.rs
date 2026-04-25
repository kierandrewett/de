//! `org.freedesktop.impl.portal.Screenshot` implementation.
#![allow(missing_docs)]
//!
//! Captures the screen via the compositor IPC socket and returns a `file://`
//! URI pointing to the saved image.

use std::collections::HashMap;

use zbus::interface;
use zvariant::{OwnedValue, Str, Value};

use ipc::ScreenRegion;

/// Handler for the Screenshot portal interface.
pub struct ScreenshotPortal;

impl ScreenshotPortal {
    /// Create a new [`ScreenshotPortal`].
    pub fn new() -> Self {
        Self
    }

    /// Request a screenshot from the compositor over the IPC socket.
    async fn request_screenshot(region: Option<ScreenRegion>) -> anyhow::Result<String> {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        use tokio::net::UnixStream;

        let socket_path = ipc::socket_path();
        let mut stream = UnixStream::connect(&socket_path).await?;

        let req = ipc::ShellRequest::TakeScreenshot { region };
        let msg = ipc::serialize(&req);
        stream.write_all(msg.as_bytes()).await?;

        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).await?;

        #[derive(serde::Deserialize)]
        struct ScreenshotResponse {
            path: String,
        }
        let resp: ScreenshotResponse = serde_json::from_str(line.trim_end())?;
        Ok(format!("file://{}", resp.path))
    }
}

impl Default for ScreenshotPortal {
    fn default() -> Self {
        Self::new()
    }
}

#[allow(missing_docs)]
#[interface(name = "org.freedesktop.impl.portal.Screenshot")]
impl ScreenshotPortal {
    /// Take a screenshot of the full desktop (or a region if `interactive`).
    async fn screenshot(
        &self,
        _handle: &str,
        _app_id: &str,
        _parent_window: &str,
        options: HashMap<String, OwnedValue>,
    ) -> zbus::fdo::Result<(u32, HashMap<String, OwnedValue>)> {
        let _interactive = match options.get("interactive") {
            Some(v) => match &**v {
                Value::Bool(b) => *b,
                _ => false,
            },
            None => false,
        };

        match Self::request_screenshot(None).await {
            Ok(uri) => {
                let mut results: HashMap<String, OwnedValue> = HashMap::new();
                results.insert("uri".into(), OwnedValue::from(Str::from(uri)));
                Ok((0, results))
            }
            Err(e) => {
                tracing::error!(error = %e, "screenshot IPC failed");
                Ok((2, HashMap::new()))
            }
        }
    }

    /// Pick a color from the screen.
    async fn pick_color(
        &self,
        _handle: &str,
        _app_id: &str,
        _parent_window: &str,
        _options: HashMap<String, OwnedValue>,
    ) -> zbus::fdo::Result<(u32, HashMap<String, OwnedValue>)> {
        // TODO: spawn portal-ui color picker, return (ddd) color result.
        tracing::info!("PickColor not yet implemented");
        Ok((1, HashMap::new()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_does_not_panic() {
        let _ = ScreenshotPortal::new();
    }
}
