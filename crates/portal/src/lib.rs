//! xdg-desktop-portal backend implementation for myDE.
//!
//! Exposes a D-Bus service under the name
//! `org.freedesktop.impl.portal.desktop.myDE` that xdg-desktop-portal
//! dispatches requests to.  Interfaces implemented:
//!
//! - [`settings`] — `org.freedesktop.impl.portal.Settings`
//! - [`file_chooser`] — `org.freedesktop.impl.portal.FileChooser`
//! - [`screenshot`] — `org.freedesktop.impl.portal.Screenshot`
//! - [`screencast`] — `org.freedesktop.impl.portal.ScreenCast`
//! - [`notification`] — `org.freedesktop.impl.portal.Notification`
//! - [`global_shortcuts`] — `org.freedesktop.impl.portal.GlobalShortcuts`
#![deny(missing_docs)]

pub mod config;
pub mod file_chooser;
pub mod global_shortcuts;
pub mod notification;
pub mod screencast;
pub mod screenshot;
pub mod settings;

pub use config::Config;

/// Well-known D-Bus name this service registers.
pub const DBUS_NAME: &str = "org.freedesktop.impl.portal.desktop.myDE";

/// Object path where all portal interfaces are served.
pub const OBJECT_PATH: &str = "/org/freedesktop/portal/desktop";

/// Run the portal D-Bus service until the connection is lost.
///
/// Connects to the session bus, registers all portal interfaces, and blocks
/// until the event loop terminates.
pub async fn run() -> anyhow::Result<()> {
    use std::sync::Arc;
    use tokio::sync::RwLock;

    let config = Arc::new(RwLock::new(Config::load()));
    tracing::info!("Starting portal service as {DBUS_NAME}");

    let conn = zbus::connection::Builder::session()?
        .name(DBUS_NAME)?
        .serve_at(
            OBJECT_PATH,
            settings::SettingsPortal::new(Arc::clone(&config)),
        )?
        .serve_at(
            OBJECT_PATH,
            file_chooser::FileChooserPortal::new(),
        )?
        .serve_at(
            OBJECT_PATH,
            screenshot::ScreenshotPortal::new(),
        )?
        .serve_at(
            OBJECT_PATH,
            screencast::ScreenCastPortal::new(),
        )?
        .serve_at(
            OBJECT_PATH,
            notification::NotificationPortal::new(),
        )?
        .serve_at(
            OBJECT_PATH,
            global_shortcuts::GlobalShortcutsPortal::new(),
        )?
        .build()
        .await?;

    tracing::info!("Portal service running");

    // Hold the connection open until the process is interrupted.
    std::future::pending::<()>().await;
    drop(conn);
    Ok(())
}
