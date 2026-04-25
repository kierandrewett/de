use iced::widget::{row, text};
use iced::{Color, Element};

use crate::app::{Message, State};

/// Red recording dot + elapsed time, screen-sharing dot, microphone-in-use dot.
/// Returns an empty row when none are active.
pub fn view(state: &State) -> Element<'_, Message> {
    let mut items: Vec<Element<'_, Message>> = Vec::new();

    if state.is_recording {
        let elapsed = format_elapsed(state.recording_elapsed.unwrap_or(0));
        items.push(
            text(format!("⏺ {elapsed}"))
                .size(12)
                .color(Color::from_rgb8(255, 59, 48))
                .into(),
        );
    }

    row(items).spacing(6).align_y(iced::alignment::Vertical::Center).into()
}

fn format_elapsed(secs: u64) -> String {
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m:02}:{s:02}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn elapsed_seconds_only() {
        assert_eq!(format_elapsed(45), "00:45");
    }

    #[test]
    fn elapsed_minutes_and_seconds() {
        assert_eq!(format_elapsed(125), "02:05");
    }

    #[test]
    fn elapsed_with_hours() {
        assert_eq!(format_elapsed(3661), "1:01:01");
    }
}
