//! D-Bus StatusNotifierWatcher and host for system tray icons.
//!
//! Implements the [StatusNotifierItem] and [StatusNotifierWatcher] D-Bus
//! protocols so that apps like Discord, Steam, and Slack can register
//! system-tray icons with a Wayland DE.
//!
//! [StatusNotifierItem]: https://www.freedesktop.org/wiki/Specifications/StatusNotifierItem/
//! [StatusNotifierWatcher]: https://www.freedesktop.org/wiki/Specifications/StatusNotifierItem/StatusNotifierWatcher/

#![deny(missing_docs)]

mod error;
mod icon;
mod item_client;
mod menu_client;
mod service;
mod types;
mod watcher;

pub use error::{Error, Result};
pub use icon::{best_pixmap, lookup_icon};
pub use service::StatusNotifierWatcher;
pub use types::{
    DbusMenu, ItemStatus, MenuItem, StatusNotifierItem, ToggleType, TrayEvent, TrayIcon,
    TrayIconPixmap, UpdatedProperty,
};
