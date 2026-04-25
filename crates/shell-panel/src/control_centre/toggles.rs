use iced::widget::{button, column, row, text};
use iced::{Color, Element, Theme};

use crate::app::{Message, State};

const ACTIVE_BG: Color = Color { r: 0.204, g: 0.596, b: 0.859, a: 1.0 };
const INACTIVE_BG: Color = Color { r: 0.15, g: 0.16, b: 0.18, a: 1.0 };

/// Quick-toggle row: Wi-Fi · BT · DND · Night Light · Dark Mode.
pub fn view(state: &State) -> Element<'_, Message> {
    row![
        toggle_button("Wi-Fi", state.network.wifi_enabled, Message::ToggleWifi),
        toggle_button("BT", state.bluetooth.enabled, Message::ToggleBluetooth),
        toggle_button("DND", state.dnd, Message::ToggleDnd),
        toggle_button("🌙", state.night_light, Message::ToggleNightLight),
        toggle_button("◑", state.dark_mode, Message::ToggleDarkMode),
    ]
    .spacing(8)
    .into()
}

fn toggle_button<'a>(label: &'a str, active: bool, msg: Message) -> Element<'a, Message> {
    let bg = if active { ACTIVE_BG } else { INACTIVE_BG };
    button(
        column![text(label).size(11).color(Color::WHITE)]
            .align_x(iced::alignment::Horizontal::Center)
            .padding([6, 0]),
    )
    .on_press(msg)
    .style(move |_: &Theme, _| iced::widget::button::Style {
        background: Some(bg.into()),
        border: iced::Border { radius: 8.0.into(), ..Default::default() },
        text_color: Color::WHITE,
        ..Default::default()
    })
    .width(64)
    .into()
}
