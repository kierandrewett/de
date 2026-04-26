use iced::widget::text;
use iced::{Color, Element};

use crate::app::{Message, State};

/// Primary text colour matching WINDOW_SPEC dark active: rgba(255,255,255,0.8).
const TEXT_PRIMARY: Color = Color { r: 1.0, g: 1.0, b: 1.0, a: 0.8 };

/// Clock face showing HH:MM:SS · t=N (debug variant). The seconds
/// + monotonic tick counter make it obvious from a single screenshot
/// whether the panel's `time::every` subscription is firing and iced
/// is repainting. Once the freeze investigation is over, drop the
/// `:%S` and the `· t=` suffix and revert the tick interval to 30 s.
pub fn view(state: &State) -> Element<'_, Message> {
    let label = format!(
        "{}  · t={}",
        state.now.format("%H:%M:%S"),
        state.tick_count,
    );
    tracing::info!(t = state.tick_count, "clock::view called");
    // Size 13, iced::Font::DEFAULT, TEXT_PRIMARY colour per WINDOW_SPEC.
    text(label).size(13).color(TEXT_PRIMARY).into()
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
