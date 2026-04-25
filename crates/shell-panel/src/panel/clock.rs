use iced::widget::text;
use iced::{Color, Element};

use crate::app::{Message, State};

/// Clock face showing HH:MM; clicking opens the date/time popout.
pub fn view(state: &State) -> Element<'_, Message> {
    text(state.now.format("%H:%M").to_string())
        .size(14)
        .color(Color::WHITE)
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn make_state_at(hour: u32, min: u32) -> State {
        let mut s = State::default();
        s.now = chrono::Local
            .with_ymd_and_hms(2026, 4, 25, hour, min, 0)
            .unwrap();
        s
    }

    #[test]
    fn clock_formats_leading_zero() {
        let state = make_state_at(9, 5);
        let formatted = state.now.format("%H:%M").to_string();
        assert_eq!(formatted, "09:05");
    }

    #[test]
    fn clock_formats_noon() {
        let state = make_state_at(12, 0);
        let formatted = state.now.format("%H:%M").to_string();
        assert_eq!(formatted, "12:00");
    }

    #[test]
    fn clock_formats_midnight() {
        let state = make_state_at(0, 0);
        let formatted = state.now.format("%H:%M").to_string();
        assert_eq!(formatted, "00:00");
    }
}
