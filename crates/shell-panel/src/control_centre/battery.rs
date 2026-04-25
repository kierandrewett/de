use iced::widget::{column, row, text};
use iced::{Color, Element};

use crate::app::{Message, State};

/// Battery status row: icon + percentage + charging/time info.
pub fn view(state: &State) -> Element<'_, Message> {
    let bat = &state.battery;
    let pct = bat.percentage.round() as u8;
    let icon = battery_icon(pct, bat.charging);
    let status = if bat.charging {
        "Charging".to_string()
    } else if bat.time_to_empty > 0 {
        format!("{} remaining", format_duration(bat.time_to_empty))
    } else {
        String::new()
    };

    row![
        text(icon).size(14).color(Color::WHITE),
        column![
            text(format!("{}%", pct)).size(13).color(Color::WHITE),
            text(status)
                .size(10)
                .color(Color { a: 0.6, ..Color::WHITE }),
        ]
        .spacing(1),
    ]
    .spacing(8)
    .align_y(iced::alignment::Vertical::Center)
    .into()
}

fn battery_icon(pct: u8, charging: bool) -> &'static str {
    if charging {
        return "⚡";
    }
    match pct {
        81..=100 => "🔋",
        61..=80 => "🔋",
        41..=60 => "🔋",
        21..=40 => "🪫",
        _ => "🪫",
    }
}

fn format_duration(secs: i64) -> String {
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    if h > 0 {
        format!("{}h {}m", h, m)
    } else {
        format!("{}m", m)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duration_hours() {
        assert_eq!(format_duration(3661), "1h 1m");
    }

    #[test]
    fn duration_minutes_only() {
        assert_eq!(format_duration(1800), "30m");
    }

    #[test]
    fn duration_zero() {
        assert_eq!(format_duration(0), "0m");
    }
}
