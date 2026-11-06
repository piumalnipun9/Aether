// SPDX-License-Identifier: GPL-3.0-or-later

//! Working out what the clock panel shows.

use chrono::{DateTime, Datelike, Days, Local, Months, NaiveDate};

/// A day in the calendar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Day {
    pub number: u32,
    pub today: bool,
    /// Otherwise, it's in the month before or after the one being shown.
    pub in_month: bool,
}

/// The days to show for the month `offset` months away from `today`, which are whole
/// weeks from Sunday on, and the name of that month.
pub fn month(today: NaiveDate, offset: i32) -> (String, Vec<Day>) {
    let first = today.with_day(1).expect("every month has a first day");
    let months = Months::new(offset.unsigned_abs());
    let first = if offset < 0 { first - months } else { first + months };

    let start = first - Days::new(first.weekday().num_days_from_sunday().into());

    let days = (0..42)
        .map(|index| {
            let date = start + Days::new(index);
            Day {
                number: date.day(),
                today: date == today,
                in_month: date.month() == first.month(),
            }
        })
        .collect();

    (first.format("%B %Y").to_string(), days)
}

/// How long ago `then` was.
pub fn time_ago(then: DateTime<Local>, now: DateTime<Local>) -> String {
    let minutes = (now - then).num_minutes();
    match minutes {
        ..1 => "now".to_owned(),
        1..60 => format!("{minutes}m"),
        60..1440 => format!("{}h", minutes / 60),
        _ => then.format("%b %-d").to_string(),
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    fn date(year: i32, month: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(year, month, day).unwrap()
    }

    #[test]
    fn months_start_on_their_first_week() {
        // November 2026 starts on a Sunday.
        let (name, days) = month(date(2026, 11, 6), 0);
        assert_eq!(name, "November 2026");
        assert_eq!(days.len(), 42);
        assert_eq!(days[0].number, 1);
        assert!(days[0].in_month);
        assert!(days[5].today);
        assert_eq!(days.iter().filter(|day| day.today).count(), 1);
    }

    #[test]
    fn weeks_reach_back_into_the_month_before() {
        // December 2026 starts on a Tuesday.
        let (name, days) = month(date(2026, 11, 6), 1);
        assert_eq!(name, "December 2026");
        assert_eq!((days[0].number, days[0].in_month), (29, false));
        assert_eq!((days[2].number, days[2].in_month), (1, true));
        assert!(days.iter().all(|day| !day.today));
    }

    #[test]
    fn months_cross_years() {
        assert_eq!(month(date(2026, 1, 31), -1).0, "December 2025");
        assert_eq!(month(date(2026, 12, 31), 1).0, "January 2027");
    }

    #[test]
    fn times() {
        let then = Local.with_ymd_and_hms(2026, 11, 6, 12, 0, 0).unwrap();
        let later = |minutes| then + chrono::Duration::minutes(minutes);
        assert_eq!(time_ago(then, later(0)), "now");
        assert_eq!(time_ago(then, later(5)), "5m");
        assert_eq!(time_ago(then, later(120)), "2h");
        assert_eq!(time_ago(then, later(3000)), "Nov 6");
    }
}
