mod battery;
mod mpris;
mod power;
mod sliders;
mod toggles;

use iced::widget::{column, container, scrollable};
use iced::{Color, Element, Length, Shadow, Theme, Vector};

use crate::app::{Message, State};

/// Dark chrome background: #111318 (rgb 17,19,24) — WINDOW_SPEC dark active title bar.
const BG: Color = Color { r: 0.067, g: 0.075, b: 0.094, a: 1.0 };
/// Outer border colour: rgba(0,0,0,0.72) — WINDOW_SPEC dark active outer border.
const BORDER_COLOR: Color = Color { r: 0.0, g: 0.0, b: 0.0, a: 0.72 };

/// Render the Control Centre overlay.
pub fn view(state: &State) -> Element<'_, Message> {
    let content = column![
        toggles::view(state),
        sliders::view(state),
        mpris::view(state),
        battery::view(state),
        power::view(),
    ]
    .spacing(12)
    .padding(16);

    container(scrollable(content).height(Length::Fill))
        .width(Length::Fill)
        .height(Length::Fill)
        .style(|_: &Theme| iced::widget::container::Style {
            background: Some(BG.into()),
            border: iced::Border {
                // 16px circular arc approximates 14px squircle (WINDOW_SPEC).
                radius: 16.0.into(),
                width: 0.5,
                color: BORDER_COLOR,
            },
            shadow: Shadow {
                // Strongest macOS layer (layer 2: offset 8, blur 24, alpha 0.5).
                color: Color { r: 0.0, g: 0.0, b: 0.0, a: 0.5 },
                offset: Vector::new(0.0, 8.0),
                blur_radius: 24.0,
            },
            text_color: None,
            snap: false,
        })
        .into()
}
