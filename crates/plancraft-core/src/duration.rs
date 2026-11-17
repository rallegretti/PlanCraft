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
            DurationUnit::Months => s.minutes_per_day.saturating_mul(s.days_per_month).max(1),
        }
    }

    /// Elapsed (calendar) minutes in one unit: a day is 24 hours, a week 7 days and a month 30
    /// days, whatever the working time.
    pub fn elapsed_minutes(self) -> Minutes {
        match self {
            DurationUnit::Minutes => 1,
            DurationUnit::Hours => 60,
            DurationUnit::Days => 24 * 60,
            DurationUnit::Weeks => 7 * 24 * 60,
            DurationUnit::Months => 30 * 24 * 60,
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

/// Formats an elapsed duration (calendar time), e.g. `"33 edays"`, `"1 eday?"`.
pub fn format_elapsed(mins: Minutes, unit: DurationUnit, estimated: bool) -> String {
    let v = mins as f64 / unit.elapsed_minutes() as f64;
    let plural = (v - 1.0).abs() > 1e-9;
    let mut out = format!("{} e{}", trim_number(v), unit.label(plural));
    if estimated {
        out.push('?');
    }
    out
}

/// A task duration as typed: its minutes, whether it is an estimate (a trailing `?`), and whether
/// it is elapsed time (`ed`, `eh`, `ew`...: calendar time, nights and non-working days included)
/// rather than working time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TypedDuration {
    pub minutes: Minutes,
    pub estimated: bool,
    pub elapsed: bool,
}

/// Parses a task duration: anything [`parse_duration`] reads, or elapsed time such as `"33ed"`,
/// `"2 ewks"`, `"12eh"` or `"1 emo"` (see [`DurationUnit::elapsed_minutes`]).
pub fn parse_task_duration(text: &str, default_unit: DurationUnit, s: &TimeSettings) -> Option<TypedDuration> {
    if let Some((minutes, estimated)) = parse_duration(text, default_unit, s) {
        return Some(TypedDuration { minutes, estimated, elapsed: false });
    }
    let mut t = text.trim().to_ascii_lowercase();
    let estimated = t.ends_with('?');
    if estimated {
        t.pop();
    }
    let t = t.trim();
    let split =
        t.char_indices().find(|(_, c)| !(c.is_ascii_digit() || *c == '.' || *c == '-' || *c == '+')).map(|(i, _)| i).unwrap_or(t.len());
    let (num, unit) = t.split_at(split);
    let value: f64 = num.trim().parse().ok()?;
    let unit = match unit.trim().strip_prefix('e')? {
        "" | "d" | "dy" | "day" | "days" => DurationUnit::Days,
        "m" | "min" | "mins" | "minute" | "minutes" => DurationUnit::Minutes,
        "h" | "hr" | "hrs" | "hour" | "hours" => DurationUnit::Hours,
        "w" | "wk" | "wks" | "week" | "weeks" => DurationUnit::Weeks,
        "mo" | "mon" | "mons" | "month" | "months" => DurationUnit::Months,
        _ => return None,
    };
    let minutes = to_minutes(value * unit.elapsed_minutes() as f64)?;
    Some(TypedDuration { minutes, estimated, elapsed: true })
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
    let split =
        t.char_indices().find(|(_, c)| !(c.is_ascii_digit() || *c == '.' || *c == '-' || *c == '+')).map(|(i, _)| i).unwrap_or(t.len());
    let (num, unit) = t.split_at(split);
    let value: f64 = num.trim().parse().ok()?;
    if !value.is_finite() {
        return None;
    }
    let unit = match unit.trim() {
        "" => default_unit,
        "m" | "min" | "mins" | "minute" | "minutes" => DurationUnit::Minutes,
        "h" | "hr" | "hrs" | "hour" | "hours" => DurationUnit::Hours,
        "d" | "dy" | "day" | "days" => DurationUnit::Days,
        "w" | "wk" | "wks" | "week" | "weeks" => DurationUnit::Weeks,
        "mo" | "mon" | "mons" | "month" | "months" => DurationUnit::Months,
        _ => return None,
    };
    Some((to_minutes(value * unit.minutes(s) as f64)?, estimated))
}

/// Rounds a number of minutes, refusing values beyond [`crate::limits::MAX_MINUTES`] (either sign).
fn to_minutes(v: f64) -> Option<Minutes> {
    let max = crate::limits::MAX_MINUTES as f64;
    (v.is_finite() && v.abs() <= max).then(|| v.round() as Minutes)
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

    /// Parses lag text such as `"2d"`, `"-1d"`, `"3 minutes"`, `"+50%"`, or elapsed time with an
    /// `e` before the unit: `"3ed"`, `"3e"` (elapsed days), `"12eh"`, `"2ew"`, `"1emo"`, `"30em"`.
    ///
    /// Working units are tried first, so a unit that merely ends in `e` (`"3 minute"`) is working
    /// time; only a unit that starts with `e` is elapsed.
    pub fn parse(text: &str, s: &TimeSettings) -> Option<Lag> {
        let t = text.trim().to_ascii_lowercase().replace(' ', "");
        if t.is_empty() {
            return Some(Lag::None);
        }
        if let Some(p) = t.strip_suffix('%') {
            let max = crate::limits::MAX_LAG_PERCENT;
            return p.parse::<i32>().ok().filter(|p| (-max..=max).contains(p)).map(Lag::Percent);
        }
        let typed = parse_task_duration(&t, DurationUnit::Days, s)?;
        Some(if typed.elapsed { Lag::Elapsed(typed.minutes) } else { Lag::Working(typed.minutes) })
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
    fn elapsed_durations_format_and_parse() {
        let s = TimeSettings::default();
        let typed = |text: &str| parse_task_duration(text, DurationUnit::Days, &s);
        let elapsed = |minutes, estimated| Some(TypedDuration { minutes, estimated, elapsed: true });
        assert_eq!(typed("33ed"), elapsed(33 * 1440, false));
        assert_eq!(typed("33 edays"), elapsed(33 * 1440, false));
        assert_eq!(typed("1 eday?"), elapsed(1440, true));
        assert_eq!(typed("12eh"), elapsed(12 * 60, false));
        assert_eq!(typed("2 ewks"), elapsed(2 * 7 * 1440, false));
        assert_eq!(typed("1emo"), elapsed(30 * 1440, false));
        assert_eq!(typed("90 emins"), elapsed(90, false));
        assert_eq!(typed("3e"), elapsed(3 * 1440, false));
        // Working time stays working time.
        assert_eq!(typed("5d"), Some(TypedDuration { minutes: 2400, estimated: false, elapsed: false }));
        for bad in ["3ex", "e", "ed", "3e5x", "nan ed", "inf ed"] {
            assert_eq!(typed(bad), None, "{bad}");
        }
        assert_eq!(typed("100000000ed"), None, "longer than the maximum");
        assert_eq!(format_elapsed(33 * 1440, DurationUnit::Days, false), "33 edays");
        assert_eq!(format_elapsed(1440, DurationUnit::Days, true), "1 eday?");
        assert_eq!(format_elapsed(36 * 60, DurationUnit::Hours, false), "36 ehrs");
        // Formatting and parsing agree.
        for unit in [DurationUnit::Minutes, DurationUnit::Hours, DurationUnit::Days, DurationUnit::Weeks, DurationUnit::Months] {
            let text = format_elapsed(210 * 1440, unit, true);
            assert_eq!(typed(&text), elapsed(210 * 1440, true), "{text}");
        }
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

    /// Regression: "3 minute" (any unit word ending in `e`) was read as an elapsed lag with the
    /// number "3minut" and refused; only units that start with `e` are elapsed.
    #[test]
    fn lag_units_ending_in_e_are_working_time() {
        let s = TimeSettings::default();
        let day = 24 * 60;
        for (text, lag) in [
            ("3 minute", Some(Lag::Working(3))),
            ("1 minute", Some(Lag::Working(1))),
            ("3 minutes", Some(Lag::Working(3))),
            ("-1 minute", Some(Lag::Working(-1))),
            ("2 hours", Some(Lag::Working(120))),
            ("1 week", Some(Lag::Working(2400))),
            ("2", Some(Lag::Working(960))),
            ("2d?", Some(Lag::Working(960))),
            ("3e", Some(Lag::Elapsed(3 * day))),
            ("3ed", Some(Lag::Elapsed(3 * day))),
            ("-3ed", Some(Lag::Elapsed(-3 * day))),
            ("1 eday", Some(Lag::Elapsed(day))),
            ("+2 edays", Some(Lag::Elapsed(2 * day))),
            ("1.5edays", Some(Lag::Elapsed(day + day / 2))),
            ("30em", Some(Lag::Elapsed(30))),
            ("3 eminute", Some(Lag::Elapsed(3))),
            ("12eh", Some(Lag::Elapsed(12 * 60))),
            ("2ew", Some(Lag::Elapsed(14 * day))),
            ("1emo", Some(Lag::Elapsed(30 * day))),
            ("", Some(Lag::None)),
            ("50%", Some(Lag::Percent(50))),
            ("3minutx", None),
            ("3ex", None),
            ("e", None),
            ("abc", None),
            ("100000000ed", None),
        ] {
            assert_eq!(Lag::parse(text, &s), lag, "{text:?}");
        }
        // Formatting and parsing agree.
        for lag in [Lag::Working(3), Lag::Working(-960), Lag::Elapsed(3 * day), Lag::Elapsed(-day / 2), Lag::Percent(-25)] {
            assert_eq!(Lag::parse(&lag.format(&s), &s), Some(lag), "{}", lag.format(&s));
        }
    }
}
