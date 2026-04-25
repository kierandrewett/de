use iced::widget::{column, row, slider, text};
use iced::{Color, Element};

use crate::app::{Message, State};

/// Volume and brightness sliders.
pub fn view(state: &State) -> Element<'_, Message> {
    column![
        row![
            text("🔊").size(14).color(Color::WHITE),
            slider(0.0..=1.0, state.volume, Message::VolumeChanged).step(0.01),
            text(format!("{}%", (state.volume * 100.0).round() as u8))
                .size(11)
                .color(Color { a: 0.7, ..Color::WHITE }),
        ]
        .spacing(8)
        .align_y(iced::alignment::Vertical::Center),
        row![
            text("☀").size(14).color(Color::WHITE),
            slider(0.0..=1.0, state.brightness, Message::BrightnessChanged).step(0.01),
            text(format!("{}%", (state.brightness * 100.0).round() as u8))
                .size(11)
                .color(Color { a: 0.7, ..Color::WHITE }),
        ]
        .spacing(8)
        .align_y(iced::alignment::Vertical::Center),
    ]
    .spacing(8)
    .into()
}
