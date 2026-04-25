//! Inspector side-panel: expandable widget tree + details.

use crate::{
    message::DevToolsMessage,
    overlay::color_for_widget_type,
    perf::perf_view,
    state::{DevToolsState, WidgetNode},
};
use iced::{
    widget::{button, column, container, row, scrollable, text, Space},
    Background, Border, Color, Element, Length,
};
use std::collections::HashSet;

/// Width of the inspector panel in logical pixels.
pub const PANEL_WIDTH: f32 = 300.0;

/// Background colour for the inspector panel.
const PANEL_BG: Color = Color {
    r: 0.11,
    g: 0.11,
    b: 0.13,
    a: 1.0,
};

/// Header background colour.
const HEADER_BG: Color = Color {
    r: 0.08,
    g: 0.08,
    b: 0.10,
    a: 1.0,
};

/// Text colour for regular labels.
const LABEL_COLOR: Color = Color {
    r: 0.85,
    g: 0.85,
    b: 0.85,
    a: 1.0,
};

/// Text colour for muted/secondary labels.
const MUTED_COLOR: Color = Color {
    r: 0.5,
    g: 0.5,
    b: 0.55,
    a: 1.0,
};

/// Indentation per depth level in logical pixels.
const INDENT: f32 = 14.0;

// ------------------------------------------------------------------
// Tree node rendering
// ------------------------------------------------------------------

/// Render a single [`WidgetNode`] and its children (if expanded).
fn node_view<'a>(
    node: &'a WidgetNode,
    depth: usize,
    expanded: &HashSet<usize>,
    selected: Option<usize>,
) -> Vec<Element<'a, DevToolsMessage>> {
    let mut out = Vec::new();

    let is_expanded = expanded.contains(&node.id);
    let is_selected = selected == Some(node.id);
    let has_children = !node.children.is_empty();

    let type_color = color_for_widget_type(&node.widget_type);

    // Expand/collapse toggle or spacer
    let toggle: Element<'_, DevToolsMessage> = if has_children {
        let icon = if is_expanded { "▼" } else { "▶" };
        let node_id = node.id;
        button(text(icon).size(9))
            .on_press(DevToolsMessage::ToggleExpand(node_id))
            .padding([2, 4])
            .style(|_theme: &iced::Theme, _status| button::Style {
                background: None,
                text_color: MUTED_COLOR,
                border: Border::default(),
                shadow: iced::Shadow::default(),
            })
            .into()
    } else {
        Space::new(14, 1).into()
    };

    // Coloured dot for the widget type
    let dot: Element<'_, DevToolsMessage> = container(Space::new(8, 8))
        .style(move |_theme: &iced::Theme| container::Style {
            background: Some(Background::Color(Color {
                a: 1.0,
                ..type_color
            })),
            border: Border {
                radius: 4.0.into(),
                ..Border::default()
            },
            ..container::Style::default()
        })
        .into();

    // Widget type label
    let type_label: Element<'_, DevToolsMessage> =
        text(&node.widget_type).size(12).color(LABEL_COLOR).into();

    // Bounds info
    let b = node.bounds;
    let bounds_label: Element<'_, DevToolsMessage> = text(format!(
        "({:.0},{:.0} {:.0}×{:.0})",
        b.x, b.y, b.w, b.h
    ))
    .size(10)
    .color(MUTED_COLOR)
    .into();

    // Build the row for this node
    let node_id = node.id;
    let indent_px = (depth as f32) * INDENT;

    let inner_row = row![toggle, dot, Space::new(4, 0), type_label, Space::new(6, 0), bounds_label]
        .spacing(0)
        .align_y(iced::Alignment::Center);

    let bg_color = if is_selected {
        Color::from_rgba(0.2, 0.5, 1.0, 0.25)
    } else {
        Color::from_rgba(0.0, 0.0, 0.0, 0.0)
    };

    let row_elem: Element<'_, DevToolsMessage> = button(
        row![Space::new(indent_px, 0), inner_row]
            .align_y(iced::Alignment::Center),
    )
    .on_press(DevToolsMessage::SelectWidget(node_id))
    .padding([3, 6])
    .width(Length::Fill)
    .style(move |_theme: &iced::Theme, status| button::Style {
        background: Some(Background::Color(match status {
            button::Status::Hovered | button::Status::Pressed => {
                Color::from_rgba(0.3, 0.3, 0.35, 0.4)
            }
            _ => bg_color,
        })),
        text_color: LABEL_COLOR,
        border: Border::default(),
        shadow: iced::Shadow::default(),
    })
    .into();

    out.push(row_elem);

    // Render children if expanded
    if is_expanded {
        for child in &node.children {
            out.extend(node_view(child, depth + 1, expanded, selected));
        }
    }

    out
}

// ------------------------------------------------------------------
// Detail pane for the selected widget
// ------------------------------------------------------------------

/// Find a node by id (depth-first).
fn find_node(nodes: &[WidgetNode], id: usize) -> Option<&WidgetNode> {
    for node in nodes {
        if node.id == id {
            return Some(node);
        }
        if let Some(found) = find_node(&node.children, id) {
            return Some(found);
        }
    }
    None
}

fn detail_view(node: &WidgetNode) -> Element<'static, DevToolsMessage> {
    let b = node.bounds;
    let mut rows: Vec<Element<'static, DevToolsMessage>> = vec![
        detail_row("Type", node.widget_type.clone()),
        detail_row("ID", node.id.to_string()),
        detail_row("X", format!("{:.2}", b.x)),
        detail_row("Y", format!("{:.2}", b.y)),
        detail_row("Width", format!("{:.2}", b.w)),
        detail_row("Height", format!("{:.2}", b.h)),
    ];

    if let Some(content) = &node.content {
        rows.push(detail_row("Content", content.clone()));
    }

    rows.push(detail_row("Children", node.children.len().to_string()));

    column(rows).spacing(2).padding(8).into()
}

fn detail_row(label: &'static str, value: String) -> Element<'static, DevToolsMessage> {
    row![
        text(label).size(11).color(MUTED_COLOR).width(70),
        text(value).size(11).color(LABEL_COLOR),
    ]
    .into()
}

// ------------------------------------------------------------------
// Public panel view builder
// ------------------------------------------------------------------

/// Build the inspector panel [`Element`] from the current [`DevToolsState`].
pub fn panel_view(state: &DevToolsState) -> Element<'_, DevToolsMessage> {
    // ----- Header -----
    let header: Element<'_, DevToolsMessage> = container(
        row![
            text("DevTools").size(13).color(Color::WHITE),
            Space::new(Length::Fill, 0),
            text(format!("{} nodes", state.widget_count()))
                .size(11)
                .color(MUTED_COLOR),
        ]
        .align_y(iced::Alignment::Center)
        .spacing(8),
    )
    .padding([6, 10])
    .width(Length::Fill)
    .style(|_theme: &iced::Theme| container::Style {
        background: Some(Background::Color(HEADER_BG)),
        border: Border {
            color: Color::from_rgba(1.0, 1.0, 1.0, 0.08),
            width: 0.0,
            radius: 0.0.into(),
        },
        ..container::Style::default()
    })
    .into();

    // ----- Toggle buttons -----
    let toggle_buttons: Element<'_, DevToolsMessage> = row![
        toggle_button("Bounds", state.show_bounds, DevToolsMessage::ToggleBounds),
        toggle_button("Padding", state.show_padding, DevToolsMessage::TogglePadding),
    ]
    .spacing(6)
    .padding([4, 8])
    .into();

    // ----- Widget tree -----
    let mut tree_nodes: Vec<Element<'_, DevToolsMessage>> = Vec::new();
    for node in &state.widget_tree {
        tree_nodes.extend(node_view(
            node,
            0,
            &state.tree_expanded,
            state.selected_widget,
        ));
    }
    if tree_nodes.is_empty() {
        tree_nodes.push(
            text("No widget tree. Call DevToolsState::set_widget_tree().")
                .size(11)
                .color(MUTED_COLOR)
                .into(),
        );
    }

    let tree_section: Element<'_, DevToolsMessage> =
        scrollable(column(tree_nodes).spacing(0).width(Length::Fill))
            .height(Length::FillPortion(2))
            .into();

    // ----- Detail pane -----
    let detail_section: Element<'_, DevToolsMessage> = {
        let inner: Element<'_, DevToolsMessage> =
            if let Some(id) = state.selected_widget {
                if let Some(node) = find_node(&state.widget_tree, id) {
                    container(detail_view(node))
                        .width(Length::Fill)
                        .into()
                } else {
                    text("Widget not found").size(11).color(MUTED_COLOR).into()
                }
            } else {
                text("Select a widget to inspect it.")
                    .size(11)
                    .color(MUTED_COLOR)
                    .into()
            };

        scrollable(
            container(inner)
                .width(Length::Fill)
                .padding(4),
        )
        .height(Length::FillPortion(1))
        .into()
    };

    // ----- Perf overlay -----
    let perf_section: Element<'_, DevToolsMessage> = perf_view(state);

    // ----- Assemble panel -----
    container(
        column![
            header,
            toggle_buttons,
            separator(),
            tree_section,
            separator(),
            detail_section,
            separator(),
            Into::<Element<'_, DevToolsMessage>>::into(container(perf_section).padding([4, 8])),
        ]
        .spacing(0)
        .width(Length::Fill)
        .height(Length::Fill),
    )
    .width(PANEL_WIDTH)
    .height(Length::Fill)
    .style(|_theme: &iced::Theme| container::Style {
        background: Some(Background::Color(PANEL_BG)),
        border: Border {
            color: Color::from_rgba(1.0, 1.0, 1.0, 0.1),
            width: 1.0,
            radius: 0.0.into(),
        },
        ..container::Style::default()
    })
    .into()
}

fn separator<'a>() -> Element<'a, DevToolsMessage> {
    container(Space::new(Length::Fill, 1))
        .style(|_theme: &iced::Theme| container::Style {
            background: Some(Background::Color(Color::from_rgba(
                1.0, 1.0, 1.0, 0.08,
            ))),
            ..container::Style::default()
        })
        .width(Length::Fill)
        .into()
}

fn toggle_button<'a>(
    label: &'a str,
    active: bool,
    msg: DevToolsMessage,
) -> Element<'a, DevToolsMessage> {
    let bg = if active {
        Color::from_rgba(0.2, 0.5, 1.0, 0.6)
    } else {
        Color::from_rgba(0.3, 0.3, 0.35, 0.4)
    };

    button(text(label).size(11).color(Color::WHITE))
        .on_press(msg)
        .padding([3, 8])
        .style(move |_theme: &iced::Theme, status| button::Style {
            background: Some(Background::Color(match status {
                button::Status::Hovered | button::Status::Pressed => {
                    Color::from_rgba(bg.r + 0.1, bg.g + 0.1, bg.b + 0.1, bg.a)
                }
                _ => bg,
            })),
            text_color: Color::WHITE,
            border: Border {
                radius: 3.0.into(),
                ..Border::default()
            },
            shadow: iced::Shadow::default(),
        })
        .into()
}
