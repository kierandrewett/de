use iced::widget::{button, row, text};
use iced::{Color, Element};
use status_notifier::{ItemStatus, TrayIcon};

use crate::app::{Message, State};

/// System tray icons rendered as small buttons.
pub fn view(state: &State) -> Element<'_, Message> {
    let icons: Vec<Element<'_, Message>> = state
        .tray_items
        .iter()
        .map(|(id, item)| {
            let label = icon_label(&item.icon);
            let id_clone = id.clone();
            let opacity = match item.status {
                ItemStatus::Passive => 0.5,
                _ => 1.0,
            };
            button(text(label).size(12).color(Color { a: opacity, ..Color::WHITE }))
                .on_press(Message::TrayActivate(id_clone, 0, 0))
                .style(|_, _| iced::widget::button::Style {
                    background: None,
                    ..Default::default()
                })
                .into()
        })
        .collect();

    row(icons).spacing(4).align_y(iced::alignment::Vertical::Center).into()
}

/// Fall back to the icon name initial when no pixmap is available.
fn icon_label(icon: &TrayIcon) -> String {
    match icon {
        TrayIcon::Named(name) => {
            // Use the first character as a text placeholder until image rendering lands.
            name.chars().next().map(|c| c.to_uppercase().to_string()).unwrap_or_else(|| "?".into())
        }
        TrayIcon::Pixmap(_) => "■".into(),
    }
}
