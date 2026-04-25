mod clock;
mod indicators;
mod tray;

use iced::widget::{button, container, row, text};
use iced::{Color, Element, Length, Theme};

use crate::app::{Message, State};

pub(crate) const PANEL_HEIGHT: f32 = 36.0;
const PANEL_BG: Color = Color { r: 0.067, g: 0.075, b: 0.094, a: 0.92 };

/// Render the top panel bar.
pub fn view(state: &State) -> Element<'_, Message> {
    let left = text(state.focused_app.as_deref().unwrap_or("Desktop"))
        .size(13)
        .color(Color::WHITE);

    let centre = button(clock::view(state))
        .on_press(Message::ToggleDateTime)
        .style(ghost_button);

    let right = row![
        indicators::view(state),
        tray::view(state),
        button(text("  ▾").size(12).color(Color::WHITE))
            .on_press(Message::ToggleCC)
            .style(ghost_button),
    ]
    .spacing(4)
    .align_y(iced::alignment::Vertical::Center);

    let bar = container(
        row![
            left,
            iced::widget::space::horizontal(),
            centre,
            iced::widget::space::horizontal(),
            right,
        ]
        .align_y(iced::alignment::Vertical::Center)
        .padding([0, 12]),
    )
    .width(Length::Fill)
    .height(PANEL_HEIGHT)
    .style(|_: &Theme| iced::widget::container::Style {
        background: Some(PANEL_BG.into()),
        ..Default::default()
    });

    bar.into()
}

/// Transparent/ghost button style used for panel buttons.
fn ghost_button(_theme: &Theme, _status: button::Status) -> button::Style {
    button::Style {
        background: None,
        text_color: Color::WHITE,
        ..Default::default()
    }
}
