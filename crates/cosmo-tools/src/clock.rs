//! The time and the date, said the way a person would. Answered locally
//! (reflex): a model has no clock, and a round trip to guess one is slow
//! and wrong.

use chrono::{DateTime, Datelike, Local, Timelike};

/// "It's 4:32 PM." (on the hour: "It's 4 PM.")
pub fn time_line() -> String {
    time_line_at(Local::now())
}

/// "It's Tuesday, October 6th."
pub fn date_line() -> String {
    date_line_at(Local::now())
}

fn time_line_at(now: DateTime<Local>) -> String {
    let (pm, hour) = now.hour12();
    let part = if pm { "PM" } else { "AM" };
    match now.minute() {
        0 => format!("It's {hour} {part}."),
        m => format!("It's {hour}:{m:02} {part}."),
    }
}

fn date_line_at(now: DateTime<Local>) -> String {
    format!(
        "It's {}, {} {}.",
        now.format("%A"),
        now.format("%B"),
        ordinal(now.day())
    )
}

/// 1 → "1st", 12 → "12th", 23 → "23rd".
fn ordinal(n: u32) -> String {
    let suffix = match (n % 100, n % 10) {
        (11..=13, _) => "th",
        (_, 1) => "st",
        (_, 2) => "nd",
        (_, 3) => "rd",
        _ => "th",
    };
    format!("{n}{suffix}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at(h: u32, m: u32) -> DateTime<Local> {
        Local.with_ymd_and_hms(2026, 10, 6, h, m, 0).unwrap()
    }

    #[test]
    fn the_time_is_said_plainly() {
        assert_eq!(time_line_at(at(16, 32)), "It's 4:32 PM.");
        assert_eq!(time_line_at(at(9, 5)), "It's 9:05 AM.");
        assert_eq!(time_line_at(at(12, 0)), "It's 12 PM.");
        assert_eq!(time_line_at(at(0, 10)), "It's 12:10 AM.");
    }

    #[test]
    fn the_date_has_an_ordinal() {
        assert_eq!(date_line_at(at(8, 0)), "It's Tuesday, October 6th.");
        for (n, s) in [
            (1, "1st"),
            (2, "2nd"),
            (3, "3rd"),
            (11, "11th"),
            (12, "12th"),
            (13, "13th"),
            (21, "21st"),
            (22, "22nd"),
            (30, "30th"),
        ] {
            assert_eq!(ordinal(n), s);
        }
    }
}
