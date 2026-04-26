mod clock;
mod indicators;
mod tray;

use iced::widget::{button, container, row, text};
use iced::{Color, Element, Length, Theme};

use crate::app::{Message, State};

pub(crate) const PANEL_HEIGHT: f32 = 36.0;

/// Panel background: #111318 at 100% alpha — WINDOW_SPEC dark active title bar.
const PANEL_BG: Color = Color { r: 0.067, g: 0.075, b: 0.094, a: 1.0 };
/// Bottom divider: rgba(0,0,0,0.8) — WINDOW_SPEC dark active bottom border.
const BOTTOM_BORDER: Color = Color { r: 0.0, g: 0.0, b: 0.0, a: 0.8 };
/// Primary text (clock + CC button): rgba(255,255,255,0.8) — WINDOW_SPEC title text dark active.
const TEXT_PRIMARY: Color = Color { r: 1.0, g: 1.0, b: 1.0, a: 0.8 };
/// Secondary text (focused app name): slightly dimmer, rgba(255,255,255,0.55).
const TEXT_SECONDARY: Color = Color { r: 1.0, g: 1.0, b: 1.0, a: 0.55 };

/// Render the top panel bar.
pub fn view(state: &State) -> Element<'_, Message> {
    let left = text(state.focused_app.as_deref().unwrap_or("Desktop"))
        .size(13)
        .color(TEXT_SECONDARY);

    let centre = button(clock::view(state))
        .on_press(Message::ToggleDateTime)
        .style(ghost_button);

    let right = row![
        indicators::view(state),
        tray::view(state),
        button(text("  ▾").size(12).color(TEXT_PRIMARY))
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
        // 0.5px bottom border via the container border (iced renders all 4 sides,
        // but the bottom is the visible one against window content below the panel).
        border: iced::Border {
            width: 0.5,
            color: BOTTOM_BORDER,
            radius: 0.0.into(),
        },
        ..Default::default()
    });

    bar.into()
}

/// Transparent/ghost button style used for panel buttons.
fn ghost_button(_theme: &Theme, _status: button::Status) -> button::Style {
    button::Style {
        background: None,
        text_color: TEXT_PRIMARY,
        ..Default::default()
    }
}
