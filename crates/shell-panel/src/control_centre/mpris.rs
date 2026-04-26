use iced::widget::{button, column, container, row, text};
use iced::{Color, Element, Length, Theme};

use crate::app::{Message, State};

const CARD_BG: Color = Color { r: 0.12, g: 0.13, b: 0.15, a: 1.0 };
/// Album art placeholder background (dark square).
const ART_BG: Color = Color { r: 0.18, g: 0.19, b: 0.22, a: 1.0 };
const CTRL_HOVER: Color = Color { r: 1.0, g: 1.0, b: 1.0, a: 0.12 };

/// Now-playing card with album art placeholder, track info, and transport controls.
///
/// Layout:
/// ┌─────────────────────────────────────────┐
/// │ [art]  Title                            │
/// │        Artist                           │
/// │        ⏮  ▶/⏸  ⏭                       │
/// └─────────────────────────────────────────┘
pub fn view(state: &State) -> Element<'_, Message> {
    let Some(mpris) = &state.mpris else {
        return iced::widget::Space::new().into();
    };

    // Album art: placeholder square with a music note glyph.
    let art = container(
        text("♪")
            .size(20)
            .color(Color { a: 0.45, ..Color::WHITE }),
    )
    .width(52)
    .height(52)
    .align_x(iced::alignment::Horizontal::Center)
    .align_y(iced::alignment::Vertical::Center)
    .style(|_: &Theme| iced::widget::container::Style {
        background: Some(ART_BG.into()),
        border: iced::Border { radius: 6.0.into(), ..Default::default() },
        ..Default::default()
    });

    let play_label = if mpris.playing { "⏸" } else { "▶" };

    // Controls row: prev / play-pause / next.
    let controls = row![
        ctrl_btn("⏮", Message::MprisPrev),
        ctrl_btn(play_label, Message::MprisPlayPause),
        ctrl_btn("⏭", Message::MprisNext),
    ]
    .spacing(4)
    .align_y(iced::alignment::Vertical::Center);

    // Right column: title + artist + controls below.
    let info_col = column![
        text(&mpris.title)
            .size(13)
            .color(Color::WHITE),
        text(&mpris.artist)
            .size(11)
            .color(Color { a: 0.65, ..Color::WHITE }),
        controls,
    ]
    .spacing(4)
    .width(Length::Fill);

    let card = container(
        row![art, info_col]
            .spacing(10)
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
    button(
        text(label)
            .size(16)
            .color(Color::WHITE)
            .align_x(iced::alignment::Horizontal::Center),
    )
    .on_press(msg)
    .width(32)
    .height(32)
    .style(|_: &Theme, status| {
        let bg = match status {
            button::Status::Hovered | button::Status::Pressed => Some(CTRL_HOVER.into()),
            _ => None,
        };
        iced::widget::button::Style {
            background: bg,
            border: iced::Border { radius: 6.0.into(), ..Default::default() },
            ..Default::default()
        }
    })
    .into()
}
