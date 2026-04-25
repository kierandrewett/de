//! Public types for the notification system.

use std::path::PathBuf;
use std::time::{Duration, SystemTime};

/// Urgency level of a notification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Urgency {
    /// Low urgency — may be shown quietly or suppressed.
    Low,
    /// Normal urgency — the default.
    #[default]
    Normal,
    /// Critical urgency — never auto-dismissed.
    Critical,
}

/// Image to display alongside the notification body.
#[derive(Debug, Clone)]
pub enum NotificationImage {
    /// Filesystem path to an image or icon file.
    Path(PathBuf),
    /// Raw pixel data supplied inline via the `image-data` hint.
    Data {
        /// Image width in pixels.
        width: i32,
        /// Image height in pixels.
        height: i32,
        /// Raw pixel bytes (RGBA or RGB depending on the sender's `has_alpha`).
        pixels: Vec<u8>,
    },
}

/// Reason a notification was closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseReason {
    /// Auto-dismissed after the expiry timeout.
    Expired = 1,
    /// Dismissed by the user (e.g. swiped away, close button).
    Dismissed = 2,
    /// Closed by the application via `CloseNotification`.
    ClosedByApp = 3,
    /// Any other reason.
    Undefined = 4,
}

impl CloseReason {
    /// Returns the numeric reason code defined in the freedesktop spec.
    pub fn as_u32(self) -> u32 {
        self as u32
    }
}

/// A single desktop notification.
#[derive(Debug, Clone)]
pub struct Notification {
    /// Server-assigned unique identifier.
    pub id: u32,
    /// Human-readable name of the sending application.
    pub app_name: String,
    /// Icon name (XDG icon theme) or file path for the application icon.
    pub app_icon: String,
    /// Short one-line summary (notification title).
    pub summary: String,
    /// Optional multi-line body text; may contain a subset of HTML markup.
    pub body: String,
    /// Action key/label pairs. The key `"default"` is the primary action.
    pub actions: Vec<(String, String)>,
    /// Urgency level parsed from the `urgency` hint.
    pub urgency: Urgency,
    /// Optional image sourced from `image-data` or `image-path` hints.
    pub image: Option<NotificationImage>,
    /// Auto-dismiss timeout. `None` means server default (never for Critical).
    pub expire_timeout: Option<Duration>,
    /// When the notification was received by the server.
    pub timestamp: SystemTime,
    /// Category string from the `category` hint (e.g. `"email.arrived"`).
    pub category: Option<String>,
    /// `.desktop` file stem of the sending app (e.g. `"org.gnome.Geary"`).
    pub desktop_entry: Option<String>,
    /// If true, the notification should not be stored in history.
    pub transient: bool,
    /// If true, the notification persists after an action is invoked.
    pub resident: bool,
}

/// Events emitted by [`NotificationServer`][crate::NotificationServer] to the consumer channel.
#[derive(Debug, Clone)]
pub enum NotificationEvent {
    /// A brand-new notification arrived.
    New(Notification),
    /// An existing notification was replaced in-place (same ID).
    Replaced {
        /// The ID that was replaced.
        old_id: u32,
        /// The updated notification (retains the same `id` as `old_id`).
        notification: Notification,
    },
    /// A notification was closed (expired, dismissed, or closed by app).
    Closed {
        /// The closed notification's ID.
        id: u32,
        /// Why it was closed.
        reason: CloseReason,
    },
    /// The user (or shell) invoked one of the notification's action buttons.
    ActionInvoked {
        /// The notification ID.
        id: u32,
        /// The action key string (e.g. `"default"`, `"reply"`).
        action: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urgency_default_is_normal() {
        assert_eq!(Urgency::default(), Urgency::Normal);
    }

    #[test]
    fn close_reason_numeric_codes() {
        assert_eq!(CloseReason::Expired.as_u32(), 1);
        assert_eq!(CloseReason::Dismissed.as_u32(), 2);
        assert_eq!(CloseReason::ClosedByApp.as_u32(), 3);
        assert_eq!(CloseReason::Undefined.as_u32(), 4);
    }

    #[test]
    fn notification_image_path_round_trip() {
        let img = NotificationImage::Path(PathBuf::from("/tmp/icon.png"));
        match img {
            NotificationImage::Path(p) => assert_eq!(p, PathBuf::from("/tmp/icon.png")),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn notification_image_data_stores_pixels() {
        let pixels = vec![255u8; 4];
        let img = NotificationImage::Data { width: 1, height: 1, pixels: pixels.clone() };
        match img {
            NotificationImage::Data { width: 1, height: 1, pixels: p } => assert_eq!(p, pixels),
            _ => panic!("wrong variant"),
        }
    }
}
