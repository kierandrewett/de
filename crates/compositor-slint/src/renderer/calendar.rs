//! Calendar grid utilities for the panel popout calendar.
//!
//! Pure functions — no compositor state, no Slint dependencies beyond the
//! generated `CalendarDay` row type. Extracted from renderer.rs to keep that
//! file focused on the render loop.

/// Add `delta` months to `base`, clamping the day to whatever the
/// destination month actually has (so e.g. Jan 31 + 1 month = Feb 28).
pub fn add_months(base: chrono::NaiveDate, delta: i32) -> chrono::NaiveDate {
    use chrono::{Datelike, NaiveDate};
    let total = base.year() * 12 + (base.month() as i32 - 1) + delta;
    let year = total.div_euclid(12);
    let month = (total.rem_euclid(12) + 1) as u32;
    let last_day = {
        let next = if month == 12 {
            NaiveDate::from_ymd_opt(year + 1, 1, 1)
        } else {
            NaiveDate::from_ymd_opt(year, month + 1, 1)
        }
        .expect("valid next month");
        let first = NaiveDate::from_ymd_opt(year, month, 1).expect("valid month");
        (next - first).num_days() as u32
    };
    let day = base.day().min(last_day);
    NaiveDate::from_ymd_opt(year, month, day).expect("valid date")
}

/// Build a 42-cell grid for the month containing `displayed`. Today's
/// cell is highlighted iff `today` falls within that month — when
/// browsing prev/next via the popout chevrons, the highlight
/// disappears and reappears as the user scrolls back.
pub fn build_calendar_grid_for(
    displayed: chrono::NaiveDate,
    today: chrono::NaiveDate,
) -> Vec<crate::CalendarDay> {
    use chrono::{Datelike, NaiveDate};
    let year = displayed.year();
    let month = displayed.month();
    let first = NaiveDate::from_ymd_opt(year, month, 1).expect("valid month");
    let lead = first.weekday().num_days_from_monday() as i64;
    let days_in_month: u32 = {
        let next_month = if month == 12 {
            NaiveDate::from_ymd_opt(year + 1, 1, 1)
        } else {
            NaiveDate::from_ymd_opt(year, month + 1, 1)
        }
        .expect("valid next month");
        (next_month - first).num_days() as u32
    };
    let highlight_today = today.year() == year && today.month() == month;
    let mut cells = Vec::with_capacity(42);
    let prev_last = first - chrono::Duration::days(1);
    let prev_total = prev_last.day();
    for i in 0..lead {
        let day = prev_total - lead as u32 + 1 + i as u32;
        cells.push(crate::CalendarDay {
            day_num: day as i32,
            is_today: false,
            is_other_month: true,
        });
    }
    for d in 1..=days_in_month {
        cells.push(crate::CalendarDay {
            day_num: d as i32,
            is_today: highlight_today && d == today.day(),
            is_other_month: false,
        });
    }
    let mut trail = 1u32;
    while cells.len() < 42 {
        cells.push(crate::CalendarDay {
            day_num: trail as i32,
            is_today: false,
            is_other_month: true,
        });
        trail += 1;
    }
    cells
}
