//! Crate-level error type.

use thiserror::Error;

/// All errors that can be returned by this crate.
#[derive(Debug, Error)]
pub enum Error {
    /// A D-Bus communication error.
    #[error("D-Bus error: {0}")]
    Dbus(#[from] zbus::Error),

    /// A D-Bus FDO error (e.g. `UnknownObject`, `Failed`).
    #[error("D-Bus FDO error: {0}")]
    Fdo(#[from] zbus::fdo::Error),

    /// A zvariant type error during value decoding.
    #[error("variant error: {0}")]
    Variant(#[from] zbus::zvariant::Error),

    /// The requested tray item was not found in the registry.
    #[error("tray item not found: {0}")]
    ItemNotFound(String),

    /// The tray item has no associated context menu.
    #[error("no menu for item: {0}")]
    NoMenu(String),
}

/// Crate-level `Result` alias.
pub type Result<T> = std::result::Result<T, Error>;
