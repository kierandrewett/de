use iced::widget::{button, column, container, row, text};
use iced::{Color, Element, Length, Theme};

use crate::app::{Message, State};

const CARD_BG: Color = Color { r: 0.12, g: 0.13, b: 0.15, a: 1.0 };

/// Now-playing card with track info and transport controls.
pub fn view(state: &State) -> Element<'_, Message> {
    let Some(mpris) = &state.mpris else {
        return iced::widget::Space::new().into();
    };

    let info = column![
        text(&mpris.title)
            .size(13)
            .color(Color::WHITE),
        text(&mpris.artist)
            .size(11)
            .color(Color { a: 0.7, ..Color::WHITE }),
    ]
    .spacing(2);

    let play_label = if mpris.playing { "⏸" } else { "▶" };

    let controls = row![
        ctrl_btn("⏮", Message::MprisPrev),
        ctrl_btn(play_label, Message::MprisPlayPause),
        ctrl_btn("⏭", Message::MprisNext),
    ]
    .spacing(8);

    let card = container(
        row![
            info,
            iced::widget::space::horizontal(),
            controls,
        ]
        .align_y(iced::alignment::Vertical::Center)
        .padding([10, 12]),
    )
    .width(Length::Fill)
    .style(|_: &Theme| iced::widget::container::Style {
        background: Some(CARD_BG.into()),
        // Inner card radius = 10 per WINDOW_SPEC consistency rule.
        border: iced::Border { radius: 10.0.into(), ..Default::default() },
        ..Default::default()
    });

    card.into()
}

fn ctrl_btn(label: &str, msg: Message) -> Element<'_, Message> {
    button(text(label).size(16).color(Color::WHITE))
        .on_press(msg)
        .style(|_, _| iced::widget::button::Style {
            background: None,
            ..Default::default()
        })
        .into()
}
