//! Performance overlay: frame-time graph, FPS counter, and widget count.

use crate::state::DevToolsState;
use iced::{
    widget::{column, container, row, text},
    Background, Border, Color, Element, Length,
};

/// Background colour for the performance HUD.
const HUD_BG: Color = Color {
    r: 0.05,
    g: 0.05,
    b: 0.05,
    a: 0.85,
};

/// Foreground text colour.
const HUD_FG: Color = Color::WHITE;

/// Width of each frame-time bar in the graph, in logical pixels.
const BAR_WIDTH: f32 = 4.0;

/// Target frame time for 60 fps (used to scale the bar graph).
const TARGET_FRAME_MS: f32 = 1000.0 / 60.0;

/// Maximum bar height in logical pixels.
const MAX_BAR_HEIGHT: f32 = 40.0;

/// A single bar in the frame-time graph.
struct FrameBar {
    /// Height in logical pixels (clamped to [`MAX_BAR_HEIGHT`]).
    height: f32,
    /// Colour: green when ≤ target, yellow when moderately over, red when very late.
    color: Color,
}

impl FrameBar {
    fn from_ms(ms: f32) -> Self {
        let ratio = ms / TARGET_FRAME_MS;
        let height = (ratio * (MAX_BAR_HEIGHT / 2.0)).min(MAX_BAR_HEIGHT);
        let color = if ratio <= 1.0 {
            Color::from_rgb(0.2, 0.85, 0.2)
        } else if ratio <= 2.0 {
            Color::from_rgb(0.95, 0.75, 0.1)
        } else {
            Color::from_rgb(0.95, 0.2, 0.2)
        };
        Self { height, color }
    }
}

/// Builds an iced [`Element`] for the performance HUD.
///
/// This is rendered as a fixed-size overlay in the top-right corner of the
/// devtools panel.
pub fn perf_view<'a, Message>(state: &'a DevToolsState) -> Element<'a, Message>
where
    Message: 'a,
{
    let fps = state.fps();
    let widget_count = state.widget_count();
    let sample_count = state.frame_times.len();

    // --- FPS and widget count labels ---
    let fps_label = text(format!("FPS: {fps:.1}")).color(HUD_FG).size(11);
    let wc_label =
        text(format!("Widgets: {widget_count}")).color(HUD_FG).size(11);
    let samples_label =
        text(format!("Samples: {sample_count}")).color(HUD_FG).size(11);

    let labels = column![fps_label, wc_label, samples_label].spacing(2);

    // --- Frame-time bars ---
    let bars: Element<'_, Message> = {
        // We use a canvas-less approach: row of tiny coloured quads rendered via
        // iced's container. Each bar is a container with a fixed width and a
        // height proportional to the frame time.
        let bar_elements: Vec<Element<'_, Message>> = state
            .frame_times
            .iter()
            .map(|d| {
                let ms = d.as_secs_f64() as f32 * 1000.0;
                let bar = FrameBar::from_ms(ms);
                container(iced::widget::Space::new(BAR_WIDTH, bar.height))
                    .style(move |_theme: &iced::Theme| {
                        container::Style {
                            background: Some(Background::Color(bar.color)),
                            border: Border::default(),
                            ..container::Style::default()
                        }
                    })
                    .into()
            })
            .collect();

        // Wrap in a fixed-height area so bars grow upward from the baseline
        let bar_row = row(bar_elements)
            .spacing(1)
            .height(MAX_BAR_HEIGHT)
            .width(Length::Fill);

        container(bar_row)
            .style(|_theme: &iced::Theme| container::Style {
                background: Some(Background::Color(Color {
                    r: 0.1,
                    g: 0.1,
                    b: 0.1,
                    a: 0.6,
                })),
                border: Border::default(),
                ..container::Style::default()
            })
            .into()
    };

    let content = column![labels, bars].spacing(6).padding(8).width(Length::Fill);

    container(content)
        .style(|_theme: &iced::Theme| container::Style {
            background: Some(Background::Color(HUD_BG)),
            border: Border {
                color: Color::from_rgba(1.0, 1.0, 1.0, 0.15),
                width: 1.0,
                radius: 4.0.into(),
            },
            ..container::Style::default()
        })
        .into()
}
