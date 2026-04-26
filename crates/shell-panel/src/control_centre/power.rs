use iced::widget::{button, row, text};
use iced::{Color, Element, Theme};

use crate::app::Message;

const BTN_BG: Color = Color { r: 0.15, g: 0.16, b: 0.18, a: 1.0 };
const DANGER_BG: Color = Color { r: 0.55, g: 0.12, b: 0.12, a: 1.0 };

/// Power action row: lock · suspend · reboot · shutdown.
pub fn view() -> Element<'static, Message> {
    row![
        power_btn("🔒", Message::Lock, BTN_BG),
        power_btn("💤", Message::Suspend, BTN_BG),
        power_btn("↺", Message::Reboot, BTN_BG),
        power_btn("⏻", Message::Shutdown, DANGER_BG),
    ]
    .spacing(8)
    .into()
}

fn power_btn(label: &'static str, msg: Message, bg: Color) -> Element<'static, Message> {
    button(
        text(label)
            .size(16)
            .color(Color::WHITE),
    )
    .on_press(msg)
    .style(move |_: &Theme, _| iced::widget::button::Style {
        background: Some(bg.into()),
        // Inner card radius = 10 per WINDOW_SPEC consistency rule.
        border: iced::Border { radius: 10.0.into(), ..Default::default() },
        text_color: Color::WHITE,
        ..Default::default()
    })
    .width(52)
    .height(44)
    .into()
}
