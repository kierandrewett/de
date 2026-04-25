//! The [`DevTools`] wrapper widget.
//!
//! When devtools is disabled (either via the feature flag or
//! [`DevToolsState::enabled`] being `false`), this widget is a transparent
//! zero-cost pass-through to the wrapped content.

use crate::{
    message::DevToolsMessage,
    overlay::BoundsOverlay,
    panel::{panel_view, PANEL_WIDTH},
    state::DevToolsState,
};
use iced::{
    advanced::{
        layout, mouse, overlay, renderer,
        widget::{
            tree::{self, Tree},
            Operation,
        },
        Clipboard, Layout, Shell, Widget,
    },
    event, Element, Length, Rectangle, Size, Vector,
};

// ------------------------------------------------------------------
// Type aliases for the concrete iced renderer stack
// ------------------------------------------------------------------

/// Concrete iced theme type.
pub type IcedTheme = iced::Theme;

/// Concrete iced renderer type.
pub type IcedRenderer = iced::Renderer;

// ------------------------------------------------------------------
// Internal layout child indices
// ------------------------------------------------------------------

/// Index of the content child in the widget tree.
const CONTENT_IDX: usize = 0;

/// Index of the panel child (only present when devtools is enabled).
const PANEL_IDX: usize = 1;

/// Index of the bounds overlay child (only present when bounds are shown).
const OVERLAY_IDX: usize = 2;

// ------------------------------------------------------------------
// DevTools widget
// ------------------------------------------------------------------

/// An iced widget wrapper that optionally renders a devtools inspector panel.
///
/// # Usage
///
/// ```no_run
/// use shell_devtools::{DevTools, DevToolsState};
/// use iced::Element;
///
/// fn view<'a>(content: Element<'a, MyMessage>, devtools: &'a mut DevToolsState)
///     -> Element<'a, MyMessage>
/// where
///     MyMessage: From<shell_devtools::DevToolsMessage> + Clone + 'a,
/// {
///     DevTools::new(content, devtools).into()
/// }
/// # #[derive(Clone)] enum MyMessage {}
/// # impl From<shell_devtools::DevToolsMessage> for MyMessage {
/// #     fn from(_: shell_devtools::DevToolsMessage) -> Self { todo!() }
/// # }
/// ```
#[allow(missing_debug_implementations)]
pub struct DevTools<'a, Message>
where
    Message: From<DevToolsMessage> + Clone + 'a,
{
    /// The wrapped application content.
    content: Element<'a, Message, IcedTheme, IcedRenderer>,

    /// The inspector panel element (built from `DevToolsState`).
    panel: Option<Element<'a, Message, IcedTheme, IcedRenderer>>,

    /// The bounds overlay element (built from `DevToolsState`).
    bounds_overlay: Option<Element<'a, Message, IcedTheme, IcedRenderer>>,

    /// Whether devtools is currently enabled.
    enabled: bool,
}

impl<'a, Message> DevTools<'a, Message>
where
    Message: From<DevToolsMessage> + Clone + 'a,
{
    /// Creates a new [`DevTools`] wrapper from the application content and
    /// the mutable [`DevToolsState`].
    ///
    /// When `state.enabled` is `false` this is a zero-cost pass-through.
    pub fn new(
        content: impl Into<Element<'a, Message, IcedTheme, IcedRenderer>>,
        state: &'a mut DevToolsState,
    ) -> Self {
        let enabled = state.enabled;
        let show_bounds = state.show_bounds;

        let panel: Option<Element<'a, Message, IcedTheme, IcedRenderer>> =
            if enabled {
                Some(panel_view(state).map(Message::from))
            } else {
                None
            };

        let bounds_overlay: Option<
            Element<'a, Message, IcedTheme, IcedRenderer>,
        > = if enabled && show_bounds {
            Some(
                BoundsOverlay::new(
                    &state.widget_tree,
                    state.selected_widget,
                )
                .into(),
            )
        } else {
            None
        };

        Self {
            content: content.into(),
            panel,
            bounds_overlay,
            enabled,
        }
    }
}

// ------------------------------------------------------------------
// Widget implementation
// ------------------------------------------------------------------

impl<'a, Message> Widget<Message, IcedTheme, IcedRenderer> for DevTools<'a, Message>
where
    Message: From<DevToolsMessage> + Clone + 'a,
{
    fn size(&self) -> Size<Length> {
        Size {
            width: Length::Fill,
            height: Length::Fill,
        }
    }

    fn tag(&self) -> tree::Tag {
        tree::Tag::stateless()
    }

    fn state(&self) -> tree::State {
        tree::State::None
    }

    fn children(&self) -> Vec<Tree> {
        let mut children = vec![Tree::new(self.content.as_widget())];
        if let Some(panel) = &self.panel {
            children.push(Tree::new(panel.as_widget()));
        }
        if let Some(ov) = &self.bounds_overlay {
            children.push(Tree::new(ov.as_widget()));
        }
        children
    }

    fn diff(&self, tree: &mut Tree) {
        let mut widgets: Vec<&dyn Widget<Message, IcedTheme, IcedRenderer>> =
            vec![self.content.as_widget()];
        if let Some(panel) = &self.panel {
            widgets.push(panel.as_widget());
        }
        if let Some(ov) = &self.bounds_overlay {
            widgets.push(ov.as_widget());
        }
        tree.diff_children(&widgets);
    }

    fn layout(
        &self,
        tree: &mut Tree,
        renderer: &IcedRenderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let total = limits.max();

        if !self.enabled {
            // Pass-through: content takes all available space.
            let child_limits =
                layout::Limits::new(iced::Size::ZERO, total);
            let content_node = self
                .content
                .as_widget()
                .layout(&mut tree.children[CONTENT_IDX], renderer, &child_limits);
            return layout::Node::with_children(total, vec![content_node]);
        }

        // Panel occupies the right 300 px; content takes the rest.
        let panel_w = PANEL_WIDTH.min(total.width);
        let content_w = (total.width - panel_w).max(0.0);

        let content_limits = layout::Limits::new(
            iced::Size::ZERO,
            iced::Size::new(content_w, total.height),
        );
        let mut content_node = self
            .content
            .as_widget()
            .layout(&mut tree.children[CONTENT_IDX], renderer, &content_limits);
        content_node.move_to_mut(iced::Point::ORIGIN);

        let panel_limits = layout::Limits::new(
            iced::Size::ZERO,
            iced::Size::new(panel_w, total.height),
        );

        let mut layout_children = vec![content_node];

        if let Some(panel) = &self.panel {
            let mut panel_node = panel
                .as_widget()
                .layout(&mut tree.children[PANEL_IDX], renderer, &panel_limits);
            panel_node.move_to_mut(iced::Point::new(content_w, 0.0));
            layout_children.push(panel_node);
        }

        if let Some(ov) = &self.bounds_overlay {
            let ov_limits = layout::Limits::new(
                iced::Size::ZERO,
                iced::Size::new(content_w, total.height),
            );
            let ov_tree_idx = if self.panel.is_some() {
                OVERLAY_IDX
            } else {
                PANEL_IDX
            };
            let mut ov_node = ov
                .as_widget()
                .layout(&mut tree.children[ov_tree_idx], renderer, &ov_limits);
            ov_node.move_to_mut(iced::Point::ORIGIN);
            layout_children.push(ov_node);
        }

        layout::Node::with_children(total, layout_children)
    }

    fn operate(
        &self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &IcedRenderer,
        operation: &mut dyn Operation,
    ) {
        operation.container(None, layout.bounds(), &mut |op| {
            if let Some(content_layout) = layout.children().next() {
                self.content.as_widget().operate(
                    &mut tree.children[CONTENT_IDX],
                    content_layout,
                    renderer,
                    op,
                );
            }
            // Operations are not propagated into the devtools panel or overlay.
        });
    }

    fn on_event(
        &mut self,
        tree: &mut Tree,
        event: iced::Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &IcedRenderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) -> event::Status {
        let mut layouts = layout.children();
        let content_layout = match layouts.next() {
            Some(l) => l,
            None => return event::Status::Ignored,
        };

        let content_status = self.content.as_widget_mut().on_event(
            &mut tree.children[CONTENT_IDX],
            event.clone(),
            content_layout,
            cursor,
            renderer,
            clipboard,
            shell,
            viewport,
        );

        if !self.enabled {
            return content_status;
        }

        let panel_status =
            if let (Some(panel), Some(panel_layout)) =
                (&mut self.panel, layouts.next())
            {
                panel.as_widget_mut().on_event(
                    &mut tree.children[PANEL_IDX],
                    event,
                    panel_layout,
                    cursor,
                    renderer,
                    clipboard,
                    shell,
                    viewport,
                )
            } else {
                event::Status::Ignored
            };

        event::Status::merge(content_status, panel_status)
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &IcedRenderer,
    ) -> mouse::Interaction {
        let mut layouts = layout.children();
        let content_layout = match layouts.next() {
            Some(l) => l,
            None => return mouse::Interaction::None,
        };

        let content_interaction = self.content.as_widget().mouse_interaction(
            &tree.children[CONTENT_IDX],
            content_layout,
            cursor,
            viewport,
            renderer,
        );

        if !self.enabled {
            return content_interaction;
        }

        let panel_interaction =
            if let (Some(panel), Some(panel_layout)) =
                (&self.panel, layouts.next())
            {
                panel.as_widget().mouse_interaction(
                    &tree.children[PANEL_IDX],
                    panel_layout,
                    cursor,
                    viewport,
                    renderer,
                )
            } else {
                mouse::Interaction::None
            };

        content_interaction.max(panel_interaction)
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut IcedRenderer,
        theme: &IcedTheme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        let mut layouts = layout.children();
        let content_layout = match layouts.next() {
            Some(l) => l,
            None => return,
        };

        // Draw main content.
        self.content.as_widget().draw(
            &tree.children[CONTENT_IDX],
            renderer,
            theme,
            style,
            content_layout,
            cursor,
            viewport,
        );

        if !self.enabled {
            return;
        }

        if self.bounds_overlay.is_some() {
            // Get the panel layout (if any) before consuming the iterator.
            let panel_layout_opt = if self.panel.is_some() {
                layouts.next()
            } else {
                None
            };
            let ov_layout_opt = layouts.next();

            // Draw bounds overlay on top of content.
            if let (Some(ov), Some(ov_layout)) =
                (&self.bounds_overlay, ov_layout_opt)
            {
                let ov_tree_idx =
                    if self.panel.is_some() { OVERLAY_IDX } else { PANEL_IDX };
                ov.as_widget().draw(
                    &tree.children[ov_tree_idx],
                    renderer,
                    theme,
                    style,
                    ov_layout,
                    cursor,
                    viewport,
                );
            }

            // Draw panel on top of overlay.
            if let (Some(panel), Some(panel_layout)) =
                (&self.panel, panel_layout_opt)
            {
                panel.as_widget().draw(
                    &tree.children[PANEL_IDX],
                    renderer,
                    theme,
                    style,
                    panel_layout,
                    cursor,
                    viewport,
                );
            }
        } else {
            // No overlay: draw the panel directly.
            if let (Some(panel), Some(panel_layout)) =
                (&self.panel, layouts.next())
            {
                panel.as_widget().draw(
                    &tree.children[PANEL_IDX],
                    renderer,
                    theme,
                    style,
                    panel_layout,
                    cursor,
                    viewport,
                );
            }
        }
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'_>,
        renderer: &IcedRenderer,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, IcedTheme, IcedRenderer>> {
        let content_layout = layout.children().next()?;
        self.content.as_widget_mut().overlay(
            &mut tree.children[CONTENT_IDX],
            content_layout,
            renderer,
            translation,
        )
    }
}

// ------------------------------------------------------------------
// Into<Element> conversion
// ------------------------------------------------------------------

impl<'a, Message> From<DevTools<'a, Message>>
    for Element<'a, Message, IcedTheme, IcedRenderer>
where
    Message: From<DevToolsMessage> + Clone + 'a,
{
    fn from(devtools: DevTools<'a, Message>) -> Self {
        Element::new(devtools)
    }
}
