//! Public types for the StatusNotifierItem / StatusNotifierWatcher protocol.

use zbus::zvariant::OwnedObjectPath;

/// Raw pixmap data from a tray icon, in RGBA byte order (converted from
/// network-byte-order ARGB as transmitted on D-Bus).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrayIconPixmap {
    /// Width in pixels.
    pub width: i32,
    /// Height in pixels.
    pub height: i32,
    /// Pixel data in RGBA byte order, `width * height * 4` bytes.
    pub argb_data: Vec<u8>,
}

/// The visual representation of a tray icon.
#[derive(Debug, Clone)]
pub enum TrayIcon {
    /// Freedesktop icon theme name (e.g. `"firefox"`).
    Named(String),
    /// Raw pixmap data in one or more sizes.
    Pixmap(Vec<TrayIconPixmap>),
}

/// Status of a tray item, as reported by the app.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ItemStatus {
    /// The item is not active (may be hidden).
    #[default]
    Passive,
    /// The item is active and visible.
    Active,
    /// The item requires the user's attention.
    NeedsAttention,
}

impl std::str::FromStr for ItemStatus {
    type Err = ();

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "Active" => Ok(Self::Active),
            "NeedsAttention" => Ok(Self::NeedsAttention),
            _ => Ok(Self::Passive),
        }
    }
}

/// A registered system-tray item, with all properties read from D-Bus.
#[derive(Debug, Clone)]
pub struct StatusNotifierItem {
    /// Unique application identifier (e.g. `"discord"`).
    pub id: String,
    /// Human-readable title.
    pub title: String,
    /// Primary icon.
    pub icon: TrayIcon,
    /// Optional overlay icon rendered on top of the primary icon.
    pub overlay_icon: Option<TrayIcon>,
    /// Icon used when `status == NeedsAttention`.
    pub attention_icon: Option<TrayIcon>,
    /// Object path for the `com.canonical.dbusmenu` context menu, if any.
    pub menu_path: Option<OwnedObjectPath>,
    /// Current item status.
    pub status: ItemStatus,
    /// Tooltip plain text, if provided.
    pub tooltip: Option<String>,
    /// D-Bus bus name that owns this item (used to reach its object server).
    pub bus_name: String,
    /// D-Bus object path of the item on its bus.
    pub object_path: OwnedObjectPath,
}

/// Toggle button type for a menu item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToggleType {
    /// A checkmark toggle.
    Checkmark,
    /// A radio button.
    Radio,
}

/// A single item within a `com.canonical.dbusmenu` context menu.
#[derive(Debug, Clone)]
pub struct MenuItem {
    /// Menu item identifier (used when sending events back to the app).
    pub id: i32,
    /// Visible label text, HTML entities stripped.
    pub label: String,
    /// Whether the item can be clicked.
    pub enabled: bool,
    /// Whether the item should be visible.
    pub visible: bool,
    /// Optional icon for this menu item.
    pub icon: Option<TrayIcon>,
    /// Nested child items.
    pub children: Vec<MenuItem>,
    /// `true` if this is a visual separator rather than a labelled item.
    pub is_separator: bool,
    /// Toggle style, if any.
    pub toggle_type: Option<ToggleType>,
    /// Current toggle state (`true` = checked/selected).
    pub toggle_state: Option<bool>,
}

/// A fully parsed context menu returned by `com.canonical.dbusmenu`.
#[derive(Debug, Clone)]
pub struct DbusMenu {
    /// Menu revision number reported by the app.
    pub revision: u32,
    /// Top-level menu items (the root node's children).
    pub items: Vec<MenuItem>,
}

/// Which property changed on a tray item (carried in [`TrayEvent::ItemUpdated`]).
#[derive(Debug, Clone)]
pub enum UpdatedProperty {
    /// The primary icon changed.
    Icon,
    /// The title changed.
    Title,
    /// The status changed, new value included.
    Status(ItemStatus),
    /// The tooltip changed.
    Tooltip,
    /// The attention icon changed.
    AttentionIcon,
    /// The overlay icon changed.
    OverlayIcon,
}

/// Events emitted on the channel returned by [`crate::StatusNotifierWatcher::start`].
#[derive(Debug)]
pub enum TrayEvent {
    /// A new tray item registered. The string is the item's canonical key
    /// (`bus_name + object_path`).
    ItemRegistered(String),
    /// A previously registered tray item disappeared.
    ItemUnregistered(String),
    /// A property on a registered item changed.
    ItemUpdated {
        /// The item key.
        id: String,
        /// Which property changed.
        property: UpdatedProperty,
    },
}
