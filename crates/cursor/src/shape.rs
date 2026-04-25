//! Cursor shape enum and name-mapping table.

/// Cursor shape from the `wp_cursor_shape_device_v1` Wayland protocol.
///
/// Each variant corresponds to one entry in the protocol enum.
/// Use [`CursorShape::to_name`] or [`CursorThemeManager::shape_to_name`] to
/// obtain the XCursor directory name for theme lookups.
///
/// [`CursorThemeManager::shape_to_name`]: crate::CursorThemeManager::shape_to_name
// TODO: import from smithay::wayland::cursor_shape once available
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum CursorShape {
    /// Default arrow cursor.
    Default,
    /// Context-menu cursor.
    ContextMenu,
    /// Help / question-mark cursor.
    Help,
    /// Pointer / hand cursor.
    Pointer,
    /// Progress cursor (busy + pointer).
    Progress,
    /// Wait / busy cursor.
    Wait,
    /// Cell-selection cursor.
    Cell,
    /// Crosshair cursor.
    Crosshair,
    /// Text / I-beam cursor.
    Text,
    /// Vertical text cursor.
    VerticalText,
    /// Alias / shortcut cursor.
    Alias,
    /// Copy cursor.
    Copy,
    /// Move cursor.
    Move,
    /// No-drop cursor.
    NoDrop,
    /// Not-allowed cursor.
    NotAllowed,
    /// Grab cursor (open hand).
    Grab,
    /// Grabbing cursor (closed hand).
    Grabbing,
    /// East-side resize.
    EResize,
    /// North-side resize.
    NResize,
    /// North-east resize.
    NeResize,
    /// North-west resize.
    NwResize,
    /// South-side resize.
    SResize,
    /// South-east resize.
    SeResize,
    /// South-west resize.
    SwResize,
    /// West-side resize.
    WResize,
    /// East–west (horizontal) resize.
    EwResize,
    /// North–south (vertical) resize.
    NsResize,
    /// North-east / south-west resize.
    NeswResize,
    /// North-west / south-east resize.
    NwseResize,
    /// Column resize.
    ColResize,
    /// Row resize.
    RowResize,
    /// All-scroll / pan cursor.
    AllScroll,
    /// Zoom-in cursor.
    ZoomIn,
    /// Zoom-out cursor.
    ZoomOut,
}

impl CursorShape {
    /// Return the traditional XCursor / X11 cursor name for this shape.
    ///
    /// These names match the directory entries found in `cursors_scalable/`
    /// directories in XDG icon themes.
    pub fn to_name(self) -> &'static str {
        match self {
            CursorShape::Default      => "left_ptr",
            CursorShape::ContextMenu  => "context-menu",
            CursorShape::Help         => "help",
            CursorShape::Pointer      => "hand2",
            CursorShape::Progress     => "left_ptr_watch",
            CursorShape::Wait         => "watch",
            CursorShape::Cell         => "plus",
            CursorShape::Crosshair    => "crosshair",
            CursorShape::Text         => "xterm",
            CursorShape::VerticalText => "vertical-text",
            CursorShape::Alias        => "dnd-link",
            CursorShape::Copy         => "dnd-copy",
            CursorShape::Move         => "fleur",
            CursorShape::NoDrop       => "dnd-none",
            CursorShape::NotAllowed   => "crossed_circle",
            CursorShape::Grab         => "hand1",
            CursorShape::Grabbing     => "grabbing",
            CursorShape::EResize      => "right_side",
            CursorShape::NResize      => "top_side",
            CursorShape::NeResize     => "top_right_corner",
            CursorShape::NwResize     => "top_left_corner",
            CursorShape::SResize      => "bottom_side",
            CursorShape::SeResize     => "bottom_right_corner",
            CursorShape::SwResize     => "bottom_left_corner",
            CursorShape::WResize      => "left_side",
            CursorShape::EwResize     => "h_double_arrow",
            CursorShape::NsResize     => "v_double_arrow",
            CursorShape::NeswResize   => "fd_double_arrow",
            CursorShape::NwseResize   => "bd_double_arrow",
            CursorShape::ColResize    => "col-resize",
            CursorShape::RowResize    => "row-resize",
            CursorShape::AllScroll    => "all-scroll",
            CursorShape::ZoomIn       => "zoom-in",
            CursorShape::ZoomOut      => "zoom-out",
        }
    }
}

/// Return fallback names to try when the primary name is not found in the theme.
///
/// Many themes ship cursors under CSS names (e.g. `"default"`) while others use
/// traditional X11 names (e.g. `"left_ptr"`). This table bridges the gap.
pub fn cursor_fallbacks(name: &str) -> &'static [&'static str] {
    match name {
        "left_ptr"        => &["default", "arrow"],
        "hand2"           => &["pointer", "pointing_hand"],
        "watch"           => &["wait"],
        "xterm"           => &["text", "ibeam"],
        "left_ptr_watch"  => &["progress"],
        "fleur"           => &["move", "all-scroll"],
        "hand1"           => &["grab", "openhand"],
        "grabbing"        => &["closedhand"],
        "crossed_circle"  => &["not-allowed", "forbidden"],
        "dnd-link"        => &["alias"],
        "dnd-copy"        => &["copy"],
        "dnd-none"        => &["no-drop"],
        "right_side"      => &["e-resize"],
        "top_side"        => &["n-resize"],
        "top_right_corner"  => &["ne-resize"],
        "top_left_corner"   => &["nw-resize"],
        "bottom_side"       => &["s-resize"],
        "bottom_right_corner" => &["se-resize"],
        "bottom_left_corner"  => &["sw-resize"],
        "left_side"       => &["w-resize"],
        "h_double_arrow"  => &["ew-resize"],
        "v_double_arrow"  => &["ns-resize"],
        "fd_double_arrow" => &["nesw-resize"],
        "bd_double_arrow" => &["nwse-resize"],
        _                 => &[],
    }
}
