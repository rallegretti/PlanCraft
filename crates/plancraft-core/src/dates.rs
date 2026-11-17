//! Dates and times: PlanCraft's thin layer over `jiff`'s civil (time-zone-free) types.
//!
//! A plan's dates are wall-clock dates and times with no time zone, like Microsoft Project's.
//! They are `jiff::civil::{Date, DateTime, Time}`, re-exported here, and serialize as ISO 8601
//! text (`2026-10-19T08:00:00`), the same text plan files have always held.
//!
//! The helpers below never panic: constructors return `Option`, and arithmetic is checked or
//! saturates at the ends of `jiff`'s range (years −9999 to 9999; plans stay within 1900–2300,
//! see [`crate::limits`]).

pub use jiff::civil::{Date, DateTime, Time, Weekday};
pub use jiff::{SignedDuration, Timestamp};

use crate::duration::Minutes;

/// The date `y-m-d`, or `None` when there is no such day.
pub fn ymd(y: i32, m: u32, d: u32) -> Option<Date> {
    Date::new(i16::try_from(y).ok()?, i8::try_from(m).ok()?, i8::try_from(d).ok()?).ok()
}

/// The time of day `h:m`, or `None` when out of range.
pub fn hm(h: u32, m: u32) -> Option<Time> {
    hms(h, m, 0)
}

/// The time of day `h:m:s`, or `None` when out of range.
pub fn hms(h: u32, m: u32, s: u32) -> Option<Time> {
    Time::new(i8::try_from(h).ok()?, i8::try_from(m).ok()?, i8::try_from(s).ok()?, 0).ok()
}

/// `y-m-d h:mi`, or `None` when there is no such moment.
pub fn ymd_hm(y: i32, m: u32, d: u32, h: u32, mi: u32) -> Option<DateTime> {
    Some(ymd(y, m, d)?.to_datetime(hm(h, mi)?))
}

/// Midnight at the start of `d`.
pub fn midnight(d: Date) -> DateTime {
    d.to_datetime(Time::midnight())
}

/// The day after `d` (`None` past the end of the range).
pub fn next_day(d: Date) -> Option<Date> {
    d.tomorrow().ok()
}

/// The day before `d` (`None` before the start of the range).
pub fn prev_day(d: Date) -> Option<Date> {
    d.yesterday().ok()
}

/// `d` moved by `n` days (`None` outside the range).
pub fn add_days(d: Date, n: i64) -> Option<Date> {
    let n = i32::try_from(n).ok()?;
    d.checked_add(jiff::Span::new().try_days(n).ok()?).ok()
}

/// `d` moved by `n` months; a day past the end of the new month becomes its last day (31 January
/// plus one month is 28 or 29 February). `None` outside the range.
pub fn add_months(d: Date, n: i64) -> Option<Date> {
    let n = i32::try_from(n).ok()?;
    d.checked_add(jiff::Span::new().try_months(n).ok()?).ok()
}

/// `d` moved by `n` years (29 February becomes 28 February in a common year).
pub fn add_years(d: Date, n: i64) -> Option<Date> {
    let n = i16::try_from(n).ok()?;
    d.checked_add(jiff::Span::new().try_years(n).ok()?).ok()
}

/// Days from `a` to `b` (negative when `b` is earlier).
pub fn days_between(a: Date, b: Date) -> i64 {
    b.duration_since(a).as_hours() / 24
}

/// Elapsed minutes from `a` to `b` (negative when `b` is earlier).
pub fn minutes_between(a: DateTime, b: DateTime) -> Minutes {
    b.duration_since(a).as_mins()
}

/// `dt` moved by `mins` elapsed minutes, saturating at the ends of the representable range.
pub fn add_minutes(dt: DateTime, mins: Minutes) -> DateTime {
    let end = if mins < 0 { DateTime::MIN } else { DateTime::MAX };
    match SignedDuration::try_from_mins(mins) {
        Some(delta) => dt.checked_add(delta).unwrap_or(end),
        None => end,
    }
}

/// The weekday's position in the week, Monday = 0 … Sunday = 6.
pub fn weekday_index(wd: Weekday) -> u32 {
    u32::try_from(wd.to_monday_zero_offset()).unwrap_or(0)
}

/// The weekday at position `i` (Monday = 0 … Sunday = 6; larger numbers wrap round).
pub fn weekday_from_index(i: u32) -> Weekday {
    let i = i8::try_from(i % 7).unwrap_or(0);
    Weekday::from_monday_zero_offset(i).unwrap_or(Weekday::Monday)
}

/// A weekday written as its English name, full or in three letters, in any case ("Monday",
/// "mon", "TUE").
pub fn parse_weekday(text: &str) -> Option<Weekday> {
    const NAMES: [&str; 7] = ["monday", "tuesday", "wednesday", "thursday", "friday", "saturday", "sunday"];
    let t = text.trim().to_ascii_lowercase();
    let i = NAMES.iter().position(|n| *n == t || (t.len() == 3 && n.starts_with(t.as_str())))?;
    Some(weekday_from_index(u32::try_from(i).ok()?))
}

/// The weekday's three-letter English name ("Mon").
pub fn weekday_short(wd: Weekday) -> &'static str {
    match wd {
        Weekday::Monday => "Mon",
        Weekday::Tuesday => "Tue",
        Weekday::Wednesday => "Wed",
        Weekday::Thursday => "Thu",
        Weekday::Friday => "Fri",
        Weekday::Saturday => "Sat",
        Weekday::Sunday => "Sun",
    }
}

/// Minutes since midnight of `t`.
pub fn minute_of_day(t: Time) -> u32 {
    u32::try_from(i32::from(t.hour()) * 60 + i32::from(t.minute())).unwrap_or(0)
}

/// The year, month and day of `d` as plain numbers.
pub fn parts(d: Date) -> (i32, u32, u32) {
    (i32::from(d.year()), u32::try_from(d.month()).unwrap_or(1), u32::try_from(d.day()).unwrap_or(1))
}

/// The current local date and time (the system's time zone; UTC if it can't be found).
pub fn now() -> DateTime {
    jiff::ZonedDateTime::now().datetime()
}

/// `dt` with its seconds (and anything smaller) cut off.
pub fn to_minute(dt: DateTime) -> DateTime {
    dt.with().second(0).subsec_nanosecond(0).build().unwrap_or(dt)
}

/// The current local time to the minute.
pub fn now_minute() -> DateTime {
    to_minute(now())
}

/// An instant (such as a file's modification time) as local date and time.
pub fn local(t: Timestamp) -> DateTime {
    t.to_zoned_datetime(jiff::tz::TimeZone::system()).datetime()
}

/// A file time as an instant (`None` outside `jiff`'s range).
pub fn timestamp(t: std::time::SystemTime) -> Option<Timestamp> {
    Timestamp::try_from(t).ok()
}

/// Today's local date.
pub fn today() -> Date {
    now().date()
}

/// `value` written with a `strftime`-style `pattern` (`%Y-%m-%d %H:%M`, `%a %b %-d`, …). An
/// unknown directive gives an empty string rather than a panic.
pub fn format(pattern: &str, value: impl Into<jiff::fmt::strtime::BrokenDownTime>) -> String {
    jiff::fmt::strtime::format(pattern, value).unwrap_or_default()
}

/// `text` read as a date with a `strftime`-style `pattern`. As with the date parsing PlanCraft
/// had before, `%B` (a full month name) also accepts a short one ("Oct"), and `%A` a short
/// weekday name.
pub fn parse_date(text: &str, pattern: &str) -> Option<Date> {
    alternatives(pattern).into_iter().find_map(|p| jiff::fmt::strtime::parse(&p, text).and_then(|t| t.to_date()).ok())
}

/// `text` read as a date and time with a `strftime`-style `pattern` (see [`parse_date`]).
pub fn parse_datetime(text: &str, pattern: &str) -> Option<DateTime> {
    alternatives(pattern).into_iter().find_map(|p| jiff::fmt::strtime::parse(&p, text).and_then(|t| t.to_datetime()).ok())
}

/// `text` read as a time of day with a `strftime`-style `pattern`.
pub fn parse_time(text: &str, pattern: &str) -> Option<Time> {
    alternatives(pattern).into_iter().find_map(|p| jiff::fmt::strtime::parse(&p, text).and_then(|t| t.to_time()).ok())
}

/// A whole number of days to move a date by (see [`DateExt::checked_add_days`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Days(pub u64);

impl Days {
    pub const fn new(n: u64) -> Self {
        Self(n)
    }
}

/// A whole number of months to move a date by (see [`DateExt::checked_add_months`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Months(pub u32);

impl Months {
    pub const fn new(n: u32) -> Self {
        Self(n)
    }
}

/// A duration of `n` days of 24 hours (saturating).
pub fn days(n: i64) -> SignedDuration {
    SignedDuration::from_hours(n.clamp(-1_000_000_000, 1_000_000_000).saturating_mul(24))
}

/// Day-based conveniences on [`Date`], each returning `None` instead of panicking.
pub trait DateExt: Sized {
    /// The day after (`None` past the end of the range).
    fn succ_opt(self) -> Option<Date>;
    /// The day before.
    fn pred_opt(self) -> Option<Date>;
    fn checked_add_days(self, n: Days) -> Option<Date>;
    fn checked_sub_days(self, n: Days) -> Option<Date>;
    /// Moves by whole months, ending on the last day of a shorter month.
    fn checked_add_months(self, n: Months) -> Option<Date>;
    fn checked_sub_months(self, n: Months) -> Option<Date>;
    /// The same month and year on day `d`.
    fn with_day(self, d: u32) -> Option<Date>;
    /// The same day and year in month `m`.
    fn with_month(self, m: u32) -> Option<Date>;
    /// The same day and month in year `y`.
    fn with_year(self, y: i32) -> Option<Date>;
    /// This date at time `t`.
    fn and_time(self, t: Time) -> DateTime;
    /// This date at `h:m:s`.
    fn and_hms_opt(self, h: u32, m: u32, s: u32) -> Option<DateTime>;
    /// The day of the year, 1-based.
    fn ordinal(self) -> u32;
    /// The month, 0-based.
    fn month0(self) -> u32;
    /// The days from this one on, until the end of the range.
    fn iter_days(self) -> DayIter;
    /// The time from `other` to this date.
    fn signed_duration_since(self, other: Date) -> SignedDuration;
    /// Moves by a duration (whole days of it count; `None` outside the range).
    fn checked_add_signed(self, d: SignedDuration) -> Option<Date>;
}

impl DateExt for Date {
    fn succ_opt(self) -> Option<Date> {
        next_day(self)
    }
    fn pred_opt(self) -> Option<Date> {
        prev_day(self)
    }
    fn checked_add_days(self, n: Days) -> Option<Date> {
        add_days(self, i64::try_from(n.0).ok()?)
    }
    fn checked_sub_days(self, n: Days) -> Option<Date> {
        add_days(self, i64::try_from(n.0).ok()?.checked_neg()?)
    }
    fn checked_add_months(self, n: Months) -> Option<Date> {
        add_months(self, i64::from(n.0))
    }
    fn checked_sub_months(self, n: Months) -> Option<Date> {
        add_months(self, -i64::from(n.0))
    }
    fn with_day(self, d: u32) -> Option<Date> {
        let (y, m, _) = parts(self);
        ymd(y, m, d)
    }
    fn with_month(self, m: u32) -> Option<Date> {
        let (y, _, d) = parts(self);
        ymd(y, m, d)
    }
    fn with_year(self, y: i32) -> Option<Date> {
        let (_, m, d) = parts(self);
        ymd(y, m, d)
    }
    fn and_time(self, t: Time) -> DateTime {
        self.to_datetime(t)
    }
    fn and_hms_opt(self, h: u32, m: u32, s: u32) -> Option<DateTime> {
        Some(self.to_datetime(hms(h, m, s)?))
    }
    fn ordinal(self) -> u32 {
        u32::try_from(self.day_of_year()).unwrap_or(1)
    }
    fn month0(self) -> u32 {
        u32::try_from(self.month()).unwrap_or(1).saturating_sub(1)
    }
    fn iter_days(self) -> DayIter {
        DayIter(Some(self))
    }
    fn signed_duration_since(self, other: Date) -> SignedDuration {
        self.duration_since(other)
    }
    fn checked_add_signed(self, d: SignedDuration) -> Option<Date> {
        add_days(self, d.as_hours() / 24)
    }
}

/// The days from a date on (see [`DateExt::iter_days`]).
#[derive(Clone, Debug)]
pub struct DayIter(Option<Date>);

impl Iterator for DayIter {
    type Item = Date;
    fn next(&mut self) -> Option<Date> {
        let d = self.0?;
        self.0 = next_day(d);
        Some(d)
    }
}

/// Conveniences on [`DateTime`], each returning `None` instead of panicking.
pub trait DateTimeExt: Sized {
    /// The time from `other` to this moment.
    fn signed_duration_since(self, other: DateTime) -> SignedDuration;
    fn checked_add_signed(self, d: SignedDuration) -> Option<DateTime>;
    fn checked_sub_signed(self, d: SignedDuration) -> Option<DateTime>;
    fn checked_add_days(self, n: Days) -> Option<DateTime>;
    fn checked_sub_days(self, n: Days) -> Option<DateTime>;
    /// The same time of day on `d`.
    fn with_date(self, d: Date) -> DateTime;
}

impl DateTimeExt for DateTime {
    fn signed_duration_since(self, other: DateTime) -> SignedDuration {
        self.duration_since(other)
    }
    fn checked_add_signed(self, d: SignedDuration) -> Option<DateTime> {
        self.checked_add(d).ok()
    }
    fn checked_sub_signed(self, d: SignedDuration) -> Option<DateTime> {
        self.checked_sub(d).ok()
    }
    fn checked_add_days(self, n: Days) -> Option<DateTime> {
        Some(self.date().checked_add_days(n)?.to_datetime(self.time()))
    }
    fn checked_sub_days(self, n: Days) -> Option<DateTime> {
        Some(self.date().checked_sub_days(n)?.to_datetime(self.time()))
    }
    fn with_date(self, d: Date) -> DateTime {
        d.to_datetime(self.time())
    }
}

/// Whole-unit readings of a [`SignedDuration`] (rounded toward zero).
pub trait DurationExt {
    fn num_minutes(&self) -> i64;
    fn num_hours(&self) -> i64;
    fn num_days(&self) -> i64;
    fn num_seconds(&self) -> i64;
}

impl DurationExt for SignedDuration {
    fn num_minutes(&self) -> i64 {
        self.as_mins()
    }
    fn num_hours(&self) -> i64 {
        self.as_hours()
    }
    fn num_days(&self) -> i64 {
        self.as_hours() / 24
    }
    fn num_seconds(&self) -> i64 {
        self.as_secs()
    }
}

/// Weekday positions as plain numbers.
pub trait WeekdayExt {
    /// Monday = 0 … Sunday = 6.
    fn num_days_from_monday(self) -> u32;
    /// Sunday = 0 … Saturday = 6.
    fn num_days_from_sunday(self) -> u32;
    /// Monday = 1 … Sunday = 7.
    fn number_from_monday(self) -> u32;
    /// The next day of the week.
    fn succ(self) -> Weekday;
    /// The previous day of the week.
    fn pred(self) -> Weekday;
}

impl WeekdayExt for Weekday {
    fn num_days_from_monday(self) -> u32 {
        weekday_index(self)
    }
    fn num_days_from_sunday(self) -> u32 {
        u32::try_from(self.to_sunday_zero_offset()).unwrap_or(0)
    }
    fn number_from_monday(self) -> u32 {
        weekday_index(self) + 1
    }
    fn succ(self) -> Weekday {
        self.next()
    }
    fn pred(self) -> Weekday {
        self.previous()
    }
}

/// Plain-number readings of a time of day (`jiff` gives `i8`).
pub trait TimeExt {
    fn hour32(self) -> u32;
    fn minute32(self) -> u32;
}

impl TimeExt for Time {
    fn hour32(self) -> u32 {
        u32::try_from(self.hour()).unwrap_or(0)
    }
    fn minute32(self) -> u32 {
        u32::try_from(self.minute()).unwrap_or(0)
    }
}

impl TimeExt for DateTime {
    fn hour32(self) -> u32 {
        self.time().hour32()
    }
    fn minute32(self) -> u32 {
        self.time().minute32()
    }
}

/// Plain-number readings of a date's parts (`jiff` gives `i16` and `i8`).
pub trait PartsExt {
    fn year32(self) -> i32;
    fn month32(self) -> u32;
    fn day32(self) -> u32;
}

impl PartsExt for Date {
    fn year32(self) -> i32 {
        i32::from(self.year())
    }
    fn month32(self) -> u32 {
        u32::try_from(self.month()).unwrap_or(1)
    }
    fn day32(self) -> u32 {
        u32::try_from(self.day()).unwrap_or(1)
    }
}

impl PartsExt for DateTime {
    fn year32(self) -> i32 {
        self.date().year32()
    }
    fn month32(self) -> u32 {
        self.date().month32()
    }
    fn day32(self) -> u32 {
        self.date().day32()
    }
}

/// `strftime`-style formatting that can't panic (an unknown directive gives an empty string).
pub trait FormatExt {
    fn format(&self, pattern: &str) -> String;
}

impl FormatExt for Date {
    fn format(&self, pattern: &str) -> String {
        format(pattern, *self)
    }
}

impl FormatExt for DateTime {
    fn format(&self, pattern: &str) -> String {
        format(pattern, *self)
    }
}

impl FormatExt for Time {
    fn format(&self, pattern: &str) -> String {
        format(pattern, *self)
    }
}

/// Text that didn't match the expected date or time pattern.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ParseError;

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("not a date or time in the expected form")
    }
}

impl std::error::Error for ParseError {}

/// Reading a value with a `strftime`-style pattern (see [`parse_date`]).
pub trait ParseExt: Sized {
    fn parse_from_str(text: &str, pattern: &str) -> Result<Self, ParseError>;
}

impl ParseExt for Date {
    fn parse_from_str(text: &str, pattern: &str) -> Result<Self, ParseError> {
        parse_date(text, pattern).ok_or(ParseError)
    }
}

impl ParseExt for DateTime {
    fn parse_from_str(text: &str, pattern: &str) -> Result<Self, ParseError> {
        parse_datetime(text, pattern).ok_or(ParseError)
    }
}

impl ParseExt for Time {
    fn parse_from_str(text: &str, pattern: &str) -> Result<Self, ParseError> {
        parse_time(text, pattern).ok_or(ParseError)
    }
}

/// Everything a module needs for dates: `use plancraft_core::dates::prelude::*;`.
pub mod prelude {
    pub use super::{
        DateExt, DateTimeExt, Days, DurationExt, FormatExt, Months, ParseExt, PartsExt, SignedDuration, TimeExt, Timestamp, WeekdayExt,
    };
    pub use jiff::civil::{Date, DateTime, Time, Weekday};
}

/// The pattern, then with full month and weekday names swapped for short ones.
fn alternatives(pattern: &str) -> Vec<String> {
    let short = pattern.replace("%B", "%b").replace("%A", "%a");
    if short == pattern { vec![pattern.to_string()] } else { vec![pattern.to_string(), short] }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constructors_refuse_impossible_dates() {
        assert!(ymd(2026, 2, 29).is_none());
        assert!(ymd(2024, 2, 29).is_some());
        assert!(ymd(100_000, 1, 1).is_none());
        assert!(ymd(2026, 13, 1).is_none());
        assert!(hm(24, 0).is_none());
        assert_eq!(ymd_hm(2026, 10, 19, 8, 0).map(|d| d.to_string()), Some("2026-10-19T08:00:00".into()));
    }

    #[test]
    fn arithmetic_saturates_or_refuses() {
        let d = ymd(2026, 1, 31).unwrap();
        assert_eq!(add_months(d, 1), ymd(2026, 2, 28));
        assert_eq!(add_days(d, 1), ymd(2026, 2, 1));
        assert_eq!(add_days(d, i64::MAX), None);
        assert_eq!(add_years(ymd(2024, 2, 29).unwrap(), 1), ymd(2025, 2, 28));
        let dt = midnight(d);
        assert_eq!(add_minutes(dt, i64::MAX), DateTime::MAX);
        assert_eq!(add_minutes(dt, i64::MIN), DateTime::MIN);
        assert_eq!(minutes_between(dt, add_minutes(dt, 90)), 90);
        assert_eq!(days_between(d, ymd(2026, 3, 1).unwrap()), 29);
        assert_eq!(days_between(ymd(2026, 3, 1).unwrap(), d), -29);
    }

    /// `now()` and `today()` read the system's time zone; if `jiff` can't find it, it quietly uses
    /// UTC and "today" would be off by hours. Each build host checks that its zone is found.
    #[test]
    fn the_system_time_zone_is_found() {
        let tz = jiff::tz::TimeZone::try_system();
        assert!(tz.is_ok(), "{tz:?}");
    }

    #[test]
    fn weekdays_and_parsing() {
        let mon = ymd(2026, 10, 19).unwrap();
        assert_eq!(weekday_index(mon.weekday()), 0);
        assert_eq!(weekday_from_index(6), Weekday::Sunday);
        assert_eq!(weekday_from_index(7), Weekday::Monday);
        assert_eq!(parse_weekday("Tue"), Some(Weekday::Tuesday));
        assert_eq!(weekday_short(Weekday::Sunday), "Sun");
        assert_eq!(parse_weekday(" SUNDAY "), Some(Weekday::Sunday));
        assert_eq!(parse_weekday("Mo"), None);
        assert_eq!(parse_weekday("mondays"), None);
        assert_eq!(parse_date("October 19, 2026", "%B %d, %Y"), Some(mon));
        assert_eq!(parse_date("19 Oct 2026", "%d %B %Y"), Some(mon));
        assert_eq!(parse_date("nonsense", "%d %B %Y"), None);
        assert_eq!(format("%a %m/%d/%y", mon), "Mon 10/19/26");
        assert_eq!(format("%Q", mon), "", "an unknown directive is no panic");
    }
}

#[cfg(test)]
mod typed_dates {
    use crate::model::Project;

    /// The forms people type into date cells give what `chrono` gave before the move to `jiff`
    /// (checked against `chrono` 0.4.45): unpadded months and days, a weekday in front, short
    /// month names in any case, extra spaces.
    #[test]
    fn typed_dates_read_as_before() {
        let p = Project::new("P", super::ymd(2026, 10, 19).unwrap());
        let start = |s: &str| p.parse_datetime(s, false).map(|d| d.to_string());
        let day = "2026-10-19T08:00:00";
        for text in
            ["2026-10-19", "10/19/2026", "Mon 10/19/26", "19 Oct 2026", "Oct 19 2026", "Oct 19, 2026", "oct 19 2026", " 2026-10-19 "]
        {
            assert_eq!(start(text).as_deref(), Some(day), "{text:?}");
        }
        assert_eq!(start("1/5/2026").as_deref(), Some("2026-01-05T08:00:00"), "unpadded month and day");
        assert_eq!(start("2026-1-5").as_deref(), Some("2026-01-05T08:00:00"), "unpadded ISO date");
        assert_eq!(start("2026-10-19 13:00").as_deref(), Some("2026-10-19T13:00:00"));
        assert_eq!(start("2026-10-19  13:00").as_deref(), Some("2026-10-19T13:00:00"), "two spaces");
        assert_eq!(start("2026-10-19T13:30").as_deref(), Some("2026-10-19T13:30:00"));
        assert_eq!(start("2026-10-19 13:30:15").as_deref(), Some("2026-10-19T13:30:15"));
        assert_eq!(start("10/19/2026 9:05").as_deref(), Some("2026-10-19T09:05:00"));
        assert_eq!(p.parse_datetime("Mon 10/19/26", true).map(|d| d.to_string()).as_deref(), Some("2026-10-19T17:00:00"));
        // Regression: a two-digit year was read as the year 26 (`%Y` took the two digits first).
        // Two digits follow `%y`: 00–68 are 2000–2068 and 69–99 are 1969–1999; four digits stay.
        assert_eq!(start("10/19/26").as_deref(), Some(day));
        assert_eq!(start("1/5/26").as_deref(), Some("2026-01-05T08:00:00"));
        assert_eq!(start("10/19/26 08:00").as_deref(), Some(day));
        assert_eq!(start("10/19/26 13:30").as_deref(), Some("2026-10-19T13:30:00"));
        assert_eq!(start("10/19/68").as_deref(), Some("2068-10-19T08:00:00"));
        assert_eq!(start("10/19/69").as_deref(), Some("1969-10-20T08:00:00"), "a Sunday: the next working day");
        assert_eq!(start("10/19/0026").as_deref(), Some("0026-10-19T08:00:00"), "four digits are taken as written");
        for bad in ["", "soon", "13/40/2026", "2026-02-30", "Tue 10/19/26", "19 Foo 2026"] {
            assert_eq!(start(bad), None, "{bad:?}");
        }
    }
}
