mod battery;
mod mpris;
mod power;
mod sliders;
mod toggles;

use iced::widget::{column, container, scrollable};
use iced::{Color, Element, Length, Theme};

use crate::app::{Message, State};

const BG: Color = Color { r: 0.082, g: 0.090, b: 0.106, a: 0.96 };

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
                radius: 12.0.into(),
                ..Default::default()
            },
            ..Default::default()
        })
        .into()
}
