//! `org.freedesktop.Notifications` D-Bus server for the desktop environment.
//!
//! This crate implements the [freedesktop notification spec v1.2][spec].
//! It registers the `org.freedesktop.Notifications` well-known name on the
//! session bus and forwards events to the caller via an async channel.
//!
//! The rendering layer (shell panel) consumes [`NotificationEvent`]s and
//! renders notification cards as layer-shell surfaces.
//!
//! # Example
//! ```no_run
//! # async fn run() -> notification::Result<()> {
//! let conn = zbus::Connection::session().await?;
//! let (server, mut rx) = notification::NotificationServer::start(&conn).await?;
//!
//! while let Some(event) = rx.recv().await {
//!     println!("event: {event:?}");
//! }
//! # Ok(())
//! # }
//! ```
//!
//! [spec]: https://specifications.freedesktop.org/notification-spec/notification-spec-latest.html
#![deny(missing_docs)]

mod history;
mod hints;
mod server;
pub mod types;

pub use server::NotificationServer;
pub use types::{CloseReason, Notification, NotificationEvent, NotificationImage, Urgency};

/// Crate-level error type.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A D-Bus transport or protocol error.
    #[error("D-Bus error: {0}")]
    Zbus(#[from] zbus::Error),
}

/// Crate-level [`Result`](std::result::Result) alias.
pub type Result<T> = std::result::Result<T, Error>;
