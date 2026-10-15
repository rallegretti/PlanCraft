//! Durations, work amounts and their textual representation.
//!
//! Internally every duration and amount of work is stored as a whole number
//! of *working minutes*.  Conversion to days / weeks / months uses the
//! project's [`TimeSettings`] (8h days, 40h weeks and 20 day months by default),
//! exactly like Microsoft Project does.

use serde::{Deserialize, Serialize};
use std::fmt::Write as _;

/// Working minutes.
pub type Minutes = i64;

pub const MINUTES_PER_HOUR: Minutes = 60;

/// Conversion factors between working minutes and larger units.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct TimeSettings {
    /// Working minutes in one "day" of duration.
    pub minutes_per_day: Minutes,
    /// Working minutes in one "week" of duration.
    pub minutes_per_week: Minutes,
    /// Working days in one "month" of duration.
    pub days_per_month: Minutes,
    /// Default start time of day for new tasks (minutes after midnight).
    pub default_start_time: u32,
    /// Default finish time of day for new tasks (minutes after midnight).
    pub default_finish_time: u32,
}

impl Default for TimeSettings {
    fn default() -> Self {
        Self {
            minutes_per_day: 8 * 60,
            minutes_per_week: 40 * 60,
            days_per_month: 20,
            default_start_time: 8 * 60,
            default_finish_time: 17 * 60,
        }
    }
}

/// Unit used when displaying or entering a duration.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum DurationUnit {
    Minutes,
    Hours,
    #[default]
    Days,
    Weeks,
    Months,
}

impl DurationUnit {
    pub fn minutes(self, s: &TimeSettings) -> Minutes {
        match self {
            DurationUnit::Minutes => 1,
            DurationUnit::Hours => 60,
            DurationUnit::Days => s.minutes_per_day.max(1),
            DurationUnit::Weeks => s.minutes_per_week.max(1),
            DurationUnit::Months => (s.minutes_per_day * s.days_per_month).max(1),
        }
    }

    fn label(self, plural: bool) -> &'static str {
        match (self, plural) {
            (DurationUnit::Minutes, false) => "min",
            (DurationUnit::Minutes, true) => "mins",
            (DurationUnit::Hours, false) => "hr",
            (DurationUnit::Hours, true) => "hrs",
            (DurationUnit::Days, false) => "day",
            (DurationUnit::Days, true) => "days",
            (DurationUnit::Weeks, false) => "wk",
            (DurationUnit::Weeks, true) => "wks",
            (DurationUnit::Months, false) => "mon",
            (DurationUnit::Months, true) => "mons",
        }
    }
}

fn trim_number(v: f64) -> String {
    let mut s = String::new();
    if (v - v.round()).abs() < 1e-9 {
        let _ = write!(s, "{}", v.round() as i64);
    } else {
        let _ = write!(s, "{:.2}", v);
        while s.ends_with('0') {
            s.pop();
        }
        if s.ends_with('.') {
            s.pop();
        }
    }
    s
}

/// Formats a duration, e.g. `"5 days"`, `"0.5 days"`, `"3 days?"`.
pub fn format_duration(mins: Minutes, unit: DurationUnit, s: &TimeSettings, estimated: bool) -> String {
    let v = mins as f64 / unit.minutes(s) as f64;
    let plural = (v - 1.0).abs() > 1e-9;
    let mut out = format!("{} {}", trim_number(v), unit.label(plural));
    if estimated {
        out.push('?');
    }
    out
}

/// Formats an amount of work in hours, e.g. `"40 hrs"`.
pub fn format_work(mins: Minutes) -> String {
    let v = mins as f64 / 60.0;
    let plural = (v - 1.0).abs() > 1e-9;
    format!("{} {}", trim_number(v), if plural { "hrs" } else { "hr" })
}

/// Parses a duration such as `"3"`, `"3d"`, `"2.5 days"`, `"4h"`, `"1w"`, `"30m"`, `"1mo"`, `"3d?"`.
///
/// A bare number uses `default_unit`.  Returns the minutes and whether the value
/// was marked as an estimate with a trailing `?`.
pub fn parse_duration(text: &str, default_unit: DurationUnit, s: &TimeSettings) -> Option<(Minutes, bool)> {
    let mut t = text.trim().to_ascii_lowercase();
    let estimated = t.ends_with('?');
    if estimated {
        t.pop();
    }
    let t = t.trim();
    if t.is_empty() {
        return None;
    }
    let split = t
        .char_indices()
        .find(|(_, c)| !(c.is_ascii_digit() || *c == '.' || *c == '-' || *c == '+'))
        .map(|(i, _)| i)
        .unwrap_or(t.len());
    let (num, unit) = t.split_at(split);
    let value: f64 = num.trim().parse().ok()?;
    let unit = match unit.trim() {
        "" => default_unit,
        "m" | "min" | "mins" | "minute" | "minutes" => DurationUnit::Minutes,
        "h" | "hr" | "hrs" | "hour" | "hours" => DurationUnit::Hours,
        "d" | "dy" | "day" | "days" => DurationUnit::Days,
        "w" | "wk" | "wks" | "week" | "weeks" => DurationUnit::Weeks,
        "mo" | "mon" | "mons" | "month" | "months" => DurationUnit::Months,
        _ => return None,
    };
    Some(((value * unit.minutes(s) as f64).round() as Minutes, estimated))
}

/// Parses an amount of work. A bare number means hours.
pub fn parse_work(text: &str, s: &TimeSettings) -> Option<Minutes> {
    parse_duration(text, DurationUnit::Hours, s).map(|(m, _)| m)
}

/// Lag (positive) or lead (negative) applied to a dependency.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(tag = "unit", content = "value", rename_all = "snake_case")]
pub enum Lag {
    /// No lag.
    #[default]
    None,
    /// Working minutes on the project calendar.
    Working(Minutes),
    /// Elapsed (calendar) minutes, ignoring working time.
    Elapsed(Minutes),
    /// Percentage of the predecessor's duration.
    Percent(i32),
}

impl Lag {
    pub fn is_zero(&self) -> bool {
        matches!(self, Lag::None | Lag::Working(0) | Lag::Elapsed(0) | Lag::Percent(0))
    }

    /// Formats the lag with a leading sign, e.g. `"+2 days"`, `"-50%"`, `"+1 eday"`.
    pub fn format(&self, s: &TimeSettings) -> String {
        match *self {
            Lag::None => String::new(),
            Lag::Working(m) => {
                let sign = if m < 0 { "-" } else { "+" };
                format!("{sign}{}", format_duration(m.abs(), best_unit(m.abs(), s), s, false))
            }
            Lag::Elapsed(m) => {
                let sign = if m < 0 { "-" } else { "+" };
                let days = m.abs() as f64 / (24.0 * 60.0);
                format!("{sign}{} edays", trim_number(days))
            }
            Lag::Percent(p) => format!("{}{}%", if p < 0 { "-" } else { "+" }, p.abs()),
        }
    }

    /// Parses lag text such as `"2d"`, `"-1d"`, `"+50%"`, `"3ed"` (elapsed days).
    pub fn parse(text: &str, s: &TimeSettings) -> Option<Lag> {
        let t = text.trim().to_ascii_lowercase().replace(' ', "");
        if t.is_empty() {
            return Some(Lag::None);
        }
        if let Some(p) = t.strip_suffix('%') {
            return p.parse::<i32>().ok().map(Lag::Percent);
        }
        for suffix in ["edays", "eday", "ed", "e"] {
            if let Some(n) = t.strip_suffix(suffix) {
                let v: f64 = n.parse().ok()?;
                return Some(Lag::Elapsed((v * 24.0 * 60.0).round() as Minutes));
            }
        }
        parse_duration(&t, DurationUnit::Days, s).map(|(m, _)| Lag::Working(m))
    }
}

/// Picks the largest unit that represents `mins` without fractions, falling back to days.
pub fn best_unit(mins: Minutes, s: &TimeSettings) -> DurationUnit {
    if mins == 0 {
        return DurationUnit::Days;
    }
    if mins % DurationUnit::Days.minutes(s) == 0 {
        DurationUnit::Days
    } else if mins % 60 == 0 {
        DurationUnit::Hours
    } else {
        DurationUnit::Minutes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_and_parse_round_trip() {
        let s = TimeSettings::default();
        assert_eq!(format_duration(480 * 5, DurationUnit::Days, &s, false), "5 days");
        assert_eq!(format_duration(480, DurationUnit::Days, &s, true), "1 day?");
        assert_eq!(format_duration(240, DurationUnit::Days, &s, false), "0.5 days");
        assert_eq!(parse_duration("5", DurationUnit::Days, &s), Some((2400, false)));
        assert_eq!(parse_duration("2.5 days?", DurationUnit::Days, &s), Some((1200, true)));
        assert_eq!(parse_duration("4h", DurationUnit::Days, &s), Some((240, false)));
        assert_eq!(parse_duration("1w", DurationUnit::Days, &s), Some((2400, false)));
        assert_eq!(parse_duration("1mo", DurationUnit::Days, &s), Some((9600, false)));
        assert_eq!(parse_duration("abc", DurationUnit::Days, &s), None);
        assert_eq!(format_work(2400), "40 hrs");
    }

    #[test]
    fn lag_parsing() {
        let s = TimeSettings::default();
        assert_eq!(Lag::parse("2d", &s), Some(Lag::Working(960)));
        assert_eq!(Lag::parse("-1d", &s), Some(Lag::Working(-480)));
        assert_eq!(Lag::parse("50%", &s), Some(Lag::Percent(50)));
        assert_eq!(Lag::parse("3ed", &s), Some(Lag::Elapsed(3 * 1440)));
        assert_eq!(Lag::Working(960).format(&s), "+2 days");
        assert_eq!(Lag::Percent(-25).format(&s), "-25%");
    }
}
