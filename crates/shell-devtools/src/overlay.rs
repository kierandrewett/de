//! Bounds overlay — draws coloured semi-transparent rectangles over widget
//! bounds.

use crate::state::WidgetNode;
use iced::{
    advanced::{
        layout, mouse, renderer,
        widget::tree::{self, Tree},
        Layout, Widget,
    },
    Background, Border, Color, Element, Length, Rectangle, Shadow, Size,
};

/// Returns the overlay colour for a given widget type name.
///
/// The colour is chosen based on a substring match of the widget type string:
///
/// | Type contains | Colour |
/// |---|---|
/// | `"Container"` | blue |
/// | `"Row"` | green |
/// | `"Column"` | red |
/// | `"Text"` | yellow |
/// | `"Button"` | orange |
/// | `"Space"` | gray |
/// | *(default)* | purple |
pub fn color_for_widget_type(widget_type: &str) -> Color {
    if widget_type.contains("Container") {
        Color::from_rgba(0.2, 0.5, 1.0, 0.25)
    } else if widget_type.contains("Row") {
        Color::from_rgba(0.2, 0.85, 0.3, 0.25)
    } else if widget_type.contains("Column") {
        Color::from_rgba(0.95, 0.2, 0.2, 0.25)
    } else if widget_type.contains("Text") {
        Color::from_rgba(1.0, 0.9, 0.1, 0.30)
    } else if widget_type.contains("Button") {
        Color::from_rgba(1.0, 0.5, 0.1, 0.30)
    } else if widget_type.contains("Space") {
        Color::from_rgba(0.5, 0.5, 0.5, 0.20)
    } else {
        Color::from_rgba(0.7, 0.2, 0.9, 0.25)
    }
}

/// An iced [`Widget`] that renders coloured semi-transparent rectangles over
/// widget bounds.  It has no intrinsic size and is meant to be layered on top
/// of real content.
pub struct BoundsOverlay<'a> {
    nodes: &'a [WidgetNode],
    selected: Option<usize>,
}

impl<'a> BoundsOverlay<'a> {
    /// Create a new [`BoundsOverlay`] from a slice of widget nodes.
    pub fn new(nodes: &'a [WidgetNode], selected: Option<usize>) -> Self {
        Self { nodes, selected }
    }
}

/// Recursively collect all (node_id, bounds, widget_type) tuples.
fn collect_bounds(nodes: &[WidgetNode], out: &mut Vec<(usize, crate::state::Bounds, String)>) {
    for node in nodes {
        out.push((node.id, node.bounds, node.widget_type.clone()));
        collect_bounds(&node.children, out);
    }
}

impl<'a, Message, Theme, Renderer> Widget<Message, Theme, Renderer>
    for BoundsOverlay<'a>
where
    Renderer: renderer::Renderer,
{
    fn size(&self) -> Size<Length> {
        Size {
            width: Length::Fill,
            height: Length::Fill,
        }
    }

    fn layout(
        &self,
        _tree: &mut Tree,
        _renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let max = limits.max();
        layout::Node::new(max)
    }

    fn draw(
        &self,
        _tree: &Tree,
        renderer: &mut Renderer,
        _theme: &Theme,
        _style: &renderer::Style,
        layout: Layout<'_>,
        _cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        let origin = layout.bounds();
        let mut all_bounds = Vec::new();
        collect_bounds(self.nodes, &mut all_bounds);

        for (id, node_bounds, widget_type) in &all_bounds {
            let rect = Rectangle {
                x: origin.x + node_bounds.x,
                y: origin.y + node_bounds.y,
                width: node_bounds.w,
                height: node_bounds.h,
            };

            // Skip rectangles completely outside the viewport.
            if rect.intersection(viewport).is_none() {
                continue;
            }

            let color = color_for_widget_type(widget_type);

            // Highlight selected widget with a brighter, fully opaque border.
            let (fill, border_color, border_width) =
                if self.selected == Some(*id) {
                    (
                        Color {
                            a: color.a * 2.0,
                            ..color
                        },
                        Color::from_rgba(1.0, 1.0, 1.0, 0.9),
                        2.0_f32,
                    )
                } else {
                    (color, Color::from_rgba(1.0, 1.0, 1.0, 0.0), 0.0_f32)
                };

            renderer.fill_quad(
                renderer::Quad {
                    bounds: rect,
                    border: Border {
                        color: border_color,
                        width: border_width,
                        radius: 2.0.into(),
                    },
                    shadow: Shadow::default(),
                },
                Background::Color(fill),
            );
        }
    }

    fn tag(&self) -> tree::Tag {
        tree::Tag::stateless()
    }

    fn state(&self) -> tree::State {
        tree::State::None
    }

    fn children(&self) -> Vec<Tree> {
        Vec::new()
    }
}

impl<'a, Message, Theme, Renderer> From<BoundsOverlay<'a>>
    for Element<'a, Message, Theme, Renderer>
where
    Message: 'a,
    Theme: 'a,
    Renderer: renderer::Renderer + 'a,
{
    fn from(overlay: BoundsOverlay<'a>) -> Self {
        Element::new(overlay)
    }
}
