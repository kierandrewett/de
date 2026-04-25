//! Internal message type for devtools UI events.

/// Internal messages produced by the devtools inspector panel.
///
/// Applications that use [`crate::DevTools`] need to map these into their own
/// message type via [`iced::Element::map`] (or by using a wrapping enum
/// variant).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DevToolsMessage {
    /// The user clicked on a widget node in the tree panel.
    SelectWidget(usize),

    /// The user toggled the expand/collapse state of a tree node.
    ToggleExpand(usize),

    /// The user toggled the bounds overlay visibility.
    ToggleBounds,

    /// The user toggled the padding overlay visibility.
    TogglePadding,
}
