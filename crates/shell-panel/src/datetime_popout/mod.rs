use chrono::{Datelike, Local, NaiveDate};
use iced::widget::{column, container, row, text};
use iced::{Color, Element, Length, Theme};

use crate::app::{Message, State};

const BG: Color = Color { r: 0.082, g: 0.090, b: 0.106, a: 0.96 };
const HEADER_COLOR: Color = Color { r: 0.55, g: 0.60, b: 0.70, a: 1.0 };
const TODAY_BG: Color = Color { r: 0.204, g: 0.596, b: 0.859, a: 1.0 };

/// Render the date/time popout: full datetime header, month calendar, world clocks.
pub fn view(state: &State) -> Element<'_, Message> {
    let now = &state.now;
    let content = column![
        datetime_header(now),
        iced::widget::rule::horizontal(1u32),
        calendar_grid(now),
        iced::widget::rule::horizontal(1u32),
        world_clocks(now),
    ]
    .spacing(12)
    .padding(16);

    container(content)
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

fn datetime_header(now: &chrono::DateTime<Local>) -> Element<'_, Message> {
    column![
        text(now.format("%A, %B %-d").to_string())
            .size(18)
            .color(Color::WHITE),
        text(now.format("%H:%M:%S").to_string())
            .size(28)
            .color(Color::WHITE),
    ]
    .spacing(4)
    .into()
}

fn calendar_grid(now: &chrono::DateTime<Local>) -> Element<'_, Message> {
    let today = now.date_naive();
    let year = today.year();
    let month = today.month();

    let month_label = now.format("%B %Y").to_string();

    let first_day = NaiveDate::from_ymd_opt(year, month, 1).unwrap();
    let weekday_offset = first_day.weekday().num_days_from_monday();
    let days_in_month = days_in_month(year, month);

    let day_headers: Vec<Element<'_, Message>> = ["Mo", "Tu", "We", "Th", "Fr", "Sa", "Su"]
        .iter()
        .map(|d| {
            text(*d)
                .size(10)
                .color(HEADER_COLOR)
                .width(32)
                .align_x(iced::alignment::Horizontal::Center)
                .into()
        })
        .collect();

    let header_row: Element<'_, Message> = row(day_headers).spacing(2).into();

    let total_cells = weekday_offset + days_in_month;
    let remainder = total_cells % 7;
    let trailing = if remainder == 0 { 0 } else { 7 - remainder };

    let weeks: Vec<Element<'_, Message>> = (0..(total_cells + trailing))
        .collect::<Vec<_>>()
        .chunks(7)
        .map(|chunk| {
            let cells: Vec<Element<'_, Message>> = chunk
                .iter()
                .map(|&i| {
                    let offset = i as i32 - weekday_offset as i32;
                    if offset < 0 || offset >= days_in_month as i32 {
                        day_cell_empty()
                    } else {
                        let day = (offset + 1) as u32;
                        day_cell(day, today.day() == day)
                    }
                })
                .collect();
            row(cells).spacing(2).into()
        })
        .collect();

    column![
        text(month_label).size(13).color(Color::WHITE),
        header_row,
        column(weeks).spacing(2),
    ]
    .spacing(6)
    .into()
}

fn day_cell(day: u32, is_today: bool) -> Element<'static, Message> {
    let label = day.to_string();
    if is_today {
        container(
            text(label)
                .size(11)
                .color(Color::WHITE)
                .align_x(iced::alignment::Horizontal::Center),
        )
        .width(32)
        .height(24)
        .align_x(iced::alignment::Horizontal::Center)
        .align_y(iced::alignment::Vertical::Center)
        .style(|_: &Theme| iced::widget::container::Style {
            background: Some(TODAY_BG.into()),
            border: iced::Border { radius: 4.0.into(), ..Default::default() },
            ..Default::default()
        })
        .into()
    } else {
        container(
            text(label)
                .size(11)
                .color(Color { a: 0.8, ..Color::WHITE })
                .align_x(iced::alignment::Horizontal::Center),
        )
        .width(32)
        .height(24)
        .align_x(iced::alignment::Horizontal::Center)
        .align_y(iced::alignment::Vertical::Center)
        .into()
    }
}

fn day_cell_empty() -> Element<'static, Message> {
    iced::widget::Space::new().width(32u32).height(24u32).into()
}

fn days_in_month(year: i32, month: u32) -> u32 {
    let next_month = if month == 12 { 1 } else { month + 1 };
    let next_year = if month == 12 { year + 1 } else { year };
    NaiveDate::from_ymd_opt(next_year, next_month, 1)
        .unwrap()
        .signed_duration_since(NaiveDate::from_ymd_opt(year, month, 1).unwrap())
        .num_days() as u32
}

struct WorldClock {
    label: &'static str,
    tz_offset_hours: i32,
}

const WORLD_CLOCKS: &[WorldClock] = &[
    WorldClock { label: "UTC", tz_offset_hours: 0 },
    WorldClock { label: "New York", tz_offset_hours: -5 },
    WorldClock { label: "London", tz_offset_hours: 0 },
    WorldClock { label: "Tokyo", tz_offset_hours: 9 },
];

fn world_clocks(now: &chrono::DateTime<Local>) -> Element<'_, Message> {
    let utc_now = now.with_timezone(&chrono::Utc);

    let rows: Vec<Element<'_, Message>> = WORLD_CLOCKS
        .iter()
        .map(|wc| {
            let offset = chrono::FixedOffset::east_opt(wc.tz_offset_hours * 3600).unwrap();
            let local = utc_now.with_timezone(&offset);
            row![
                text(wc.label)
                    .size(11)
                    .color(HEADER_COLOR)
                    .width(80),
                text(local.format("%H:%M").to_string())
                    .size(12)
                    .color(Color::WHITE),
            ]
            .spacing(8)
            .into()
        })
        .collect();

    column(rows).spacing(4).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn days_in_jan() {
        assert_eq!(days_in_month(2024, 1), 31);
    }

    #[test]
    fn days_in_feb_leap() {
        assert_eq!(days_in_month(2024, 2), 29);
    }

    #[test]
    fn days_in_feb_non_leap() {
        assert_eq!(days_in_month(2023, 2), 28);
    }

    #[test]
    fn days_in_dec() {
        assert_eq!(days_in_month(2024, 12), 31);
    }
}
