//! Working-time calendars and calendar-aware date arithmetic.
//!
//! A [`Calendar`] describes the working times of a regular week plus a list of
//! exceptions (holidays, shutdowns, overtime days...).  Resources reference a
//! base calendar and may add their own personal exceptions (vacations, part-time
//! days).  [`WorkCal`] combines one or more such layers; when several layers are
//! combined only the time where *all* layers are working counts as working time.

use chrono::{Datelike, Duration, NaiveDate, NaiveDateTime, NaiveTime, Weekday};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;

use crate::duration::Minutes;
use crate::ids::CalendarId;

/// Days scanned without finding any working time before date math gives up.
const MAX_IDLE_SCAN_DAYS: i64 = 366 * 5;

/// A span of working time inside a single day, in minutes after midnight.
///
/// Serialized as `"08:00-12:00"`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TimeRange {
    pub start: u32,
    pub end: u32,
}

impl TimeRange {
    pub const fn new(start: u32, end: u32) -> Self {
        Self { start, end }
    }
    pub const fn hours(start_h: u32, end_h: u32) -> Self {
        Self { start: start_h * 60, end: end_h * 60 }
    }
    pub fn len(&self) -> u32 {
        self.end.saturating_sub(self.start)
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn parse(s: &str) -> Option<TimeRange> {
        let (a, b) = s.trim().split_once('-')?;
        let start = parse_hhmm(a)?;
        let end = parse_hhmm(b)?;
        (start < end && end <= 1440).then_some(TimeRange { start, end })
    }
}

fn parse_hhmm(s: &str) -> Option<u32> {
    let s = s.trim();
    let (h, m) = s.split_once(':').unwrap_or((s, "0"));
    let h: u32 = h.trim().parse().ok()?;
    let m: u32 = m.trim().parse().ok()?;
    (h <= 24 && m < 60 && h * 60 + m <= 1440).then_some(h * 60 + m)
}

impl fmt::Display for TimeRange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:02}:{:02}-{:02}:{:02}", self.start / 60, self.start % 60, self.end / 60, self.end % 60)
    }
}

impl Serialize for TimeRange {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for TimeRange {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        TimeRange::parse(&s).ok_or_else(|| serde::de::Error::custom(format!("invalid time range '{s}', expected HH:MM-HH:MM")))
    }
}

/// Normalizes a list of ranges: sorted, non-overlapping, non-empty.
pub fn normalize_ranges(ranges: &mut Vec<TimeRange>) {
    ranges.retain(|r| !r.is_empty());
    ranges.sort();
    let mut out: Vec<TimeRange> = Vec::with_capacity(ranges.len());
    for r in ranges.drain(..) {
        match out.last_mut() {
            Some(last) if r.start <= last.end => last.end = last.end.max(r.end),
            _ => out.push(r),
        }
    }
    *ranges = out;
}

/// Working times of each weekday.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct WorkWeek {
    pub monday: Vec<TimeRange>,
    pub tuesday: Vec<TimeRange>,
    pub wednesday: Vec<TimeRange>,
    pub thursday: Vec<TimeRange>,
    pub friday: Vec<TimeRange>,
    pub saturday: Vec<TimeRange>,
    pub sunday: Vec<TimeRange>,
}

impl WorkWeek {
    pub fn uniform(weekdays: &[TimeRange], weekend: &[TimeRange]) -> Self {
        Self {
            monday: weekdays.to_vec(),
            tuesday: weekdays.to_vec(),
            wednesday: weekdays.to_vec(),
            thursday: weekdays.to_vec(),
            friday: weekdays.to_vec(),
            saturday: weekend.to_vec(),
            sunday: weekend.to_vec(),
        }
    }

    pub fn day(&self, wd: Weekday) -> &Vec<TimeRange> {
        match wd {
            Weekday::Mon => &self.monday,
            Weekday::Tue => &self.tuesday,
            Weekday::Wed => &self.wednesday,
            Weekday::Thu => &self.thursday,
            Weekday::Fri => &self.friday,
            Weekday::Sat => &self.saturday,
            Weekday::Sun => &self.sunday,
        }
    }

    pub fn day_mut(&mut self, wd: Weekday) -> &mut Vec<TimeRange> {
        match wd {
            Weekday::Mon => &mut self.monday,
            Weekday::Tue => &mut self.tuesday,
            Weekday::Wed => &mut self.wednesday,
            Weekday::Thu => &mut self.thursday,
            Weekday::Fri => &mut self.friday,
            Weekday::Sat => &mut self.saturday,
            Weekday::Sun => &mut self.sunday,
        }
    }

    pub fn weekly_minutes(&self) -> Minutes {
        WEEKDAYS.iter().map(|wd| self.day(*wd).iter().map(|r| r.len() as Minutes).sum::<Minutes>()).sum()
    }
}

pub const WEEKDAYS: [Weekday; 7] = [Weekday::Mon, Weekday::Tue, Weekday::Wed, Weekday::Thu, Weekday::Fri, Weekday::Sat, Weekday::Sun];

/// A dated deviation from the regular work week.
///
/// An empty `working` list makes the whole period non-working (holiday,
/// vacation); otherwise the listed ranges replace the regular working times.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct CalendarException {
    pub name: String,
    pub start: NaiveDate,
    pub end: NaiveDate,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub working: Vec<TimeRange>,
    /// Repeat every year on the same month/day span (e.g. public holidays).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub yearly: bool,
}

impl CalendarException {
    pub fn non_working(name: impl Into<String>, start: NaiveDate, end: NaiveDate) -> Self {
        Self { name: name.into(), start, end, working: Vec::new(), yearly: false }
    }

    pub fn covers(&self, d: NaiveDate) -> bool {
        if !self.yearly {
            return d >= self.start && d <= self.end;
        }
        let key = |x: NaiveDate| (x.month(), x.day());
        let (s, e, k) = (key(self.start), key(self.end), key(d));
        if s <= e { k >= s && k <= e } else { k >= s || k <= e }
    }
}

/// A named working-time calendar.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Calendar {
    pub id: CalendarId,
    pub name: String,
    pub week: WorkWeek,
    #[serde(default)]
    pub exceptions: Vec<CalendarException>,
}

impl Calendar {
    /// Monday–Friday, 08:00–12:00 and 13:00–17:00.
    pub fn standard(id: CalendarId) -> Self {
        Self {
            id,
            name: "Standard".into(),
            week: WorkWeek::uniform(&[TimeRange::hours(8, 12), TimeRange::hours(13, 17)], &[]),
            exceptions: Vec::new(),
        }
    }

    /// Every day, around the clock.
    pub fn twenty_four_hours(id: CalendarId) -> Self {
        let all = [TimeRange::new(0, 1440)];
        Self { id, name: "24 Hours".into(), week: WorkWeek::uniform(&all, &all), exceptions: Vec::new() }
    }

    /// Monday–Friday night shift, 23:00–08:00 with a break at 03:00.
    pub fn night_shift(id: CalendarId) -> Self {
        let mut week = WorkWeek::default();
        let night_start = [TimeRange::hours(23, 24)];
        let night_end = [TimeRange::hours(0, 3), TimeRange::hours(4, 8)];
        week.monday = night_start.to_vec();
        for wd in [Weekday::Tue, Weekday::Wed, Weekday::Thu, Weekday::Fri] {
            let mut v = night_end.to_vec();
            v.extend_from_slice(&night_start);
            *week.day_mut(wd) = v;
        }
        week.saturday = night_end.to_vec();
        Self { id, name: "Night Shift".into(), week, exceptions: Vec::new() }
    }
}

fn exception_ranges(exceptions: &[CalendarException], d: NaiveDate) -> Option<&[TimeRange]> {
    // Later exceptions win, so users can override an earlier broad exception.
    exceptions.iter().rev().find(|e| e.covers(d)).map(|e| e.working.as_slice())
}

/// One layer of working time: a base calendar plus personal exceptions.
#[derive(Clone, Copy, Debug)]
pub struct CalLayer<'a> {
    pub calendar: &'a Calendar,
    pub personal: &'a [CalendarException],
}

impl<'a> CalLayer<'a> {
    pub fn new(calendar: &'a Calendar, personal: &'a [CalendarException]) -> Self {
        Self { calendar, personal }
    }

    pub fn ranges(&self, d: NaiveDate) -> Vec<TimeRange> {
        let r = exception_ranges(self.personal, d)
            .or_else(|| exception_ranges(&self.calendar.exceptions, d))
            .unwrap_or_else(|| self.calendar.week.day(d.weekday()).as_slice());
        let mut v = r.to_vec();
        normalize_ranges(&mut v);
        v
    }
}

fn intersect(a: &[TimeRange], b: &[TimeRange]) -> Vec<TimeRange> {
    let mut out = Vec::new();
    let (mut i, mut j) = (0, 0);
    while let (Some(x), Some(y)) = (a.get(i), b.get(j)) {
        let s = x.start.max(y.start);
        let e = x.end.min(y.end);
        if s < e {
            out.push(TimeRange::new(s, e));
        }
        if x.end < y.end { i += 1 } else { j += 1 }
    }
    out
}

/// Effective working time used for date arithmetic: the intersection of its layers.
#[derive(Clone, Debug)]
pub struct WorkCal<'a> {
    layers: Vec<CalLayer<'a>>,
}

/// `minute` minutes after midnight on `d` (1440 is the next midnight; clamped at the end of time).
fn at(d: NaiveDate, minute: u32) -> NaiveDateTime {
    shift(d.and_time(NaiveTime::MIN), Minutes::from(minute.min(1440)))
}

/// `dt` moved by `mins` elapsed minutes, saturating at chrono's representable range.
pub fn shift(dt: NaiveDateTime, mins: Minutes) -> NaiveDateTime {
    let delta = Duration::try_minutes(mins).unwrap_or(if mins < 0 { Duration::MIN } else { Duration::MAX });
    dt.checked_add_signed(delta).unwrap_or(if mins < 0 { NaiveDateTime::MIN } else { NaiveDateTime::MAX })
}

/// Last day date arithmetic walks to: computed dates beyond it are clamped (hostile durations
/// can't make the scheduler walk to the year 200,000).
pub fn horizon_end() -> NaiveDate {
    NaiveDate::from_ymd_opt(2500, 12, 31).unwrap_or(NaiveDate::MAX)
}

/// First day date arithmetic walks back to.
pub fn horizon_start() -> NaiveDate {
    NaiveDate::from_ymd_opt(1800, 1, 1).unwrap_or(NaiveDate::MIN)
}

fn next_day(d: NaiveDate) -> Option<NaiveDate> {
    d.succ_opt().filter(|n| *n <= horizon_end())
}

fn prev_day(d: NaiveDate) -> Option<NaiveDate> {
    d.pred_opt().filter(|n| *n >= horizon_start())
}

fn minute_of_day(dt: NaiveDateTime) -> u32 {
    let t = dt.time();
    use chrono::Timelike;
    t.hour() * 60 + t.minute()
}

impl<'a> WorkCal<'a> {
    pub fn single(calendar: &'a Calendar) -> Self {
        Self { layers: vec![CalLayer::new(calendar, &[])] }
    }

    /// Combines layers; with no layers there is no working time at all.
    pub fn from_layers(layers: Vec<CalLayer<'a>>) -> Self {
        Self { layers }
    }

    pub fn layers(&self) -> &[CalLayer<'a>] {
        &self.layers
    }

    /// Working ranges on a date.
    pub fn ranges(&self, d: NaiveDate) -> Vec<TimeRange> {
        let mut it = self.layers.iter();
        let mut acc = it.next().map(|l| l.ranges(d)).unwrap_or_default();
        for l in it {
            if acc.is_empty() {
                break;
            }
            acc = intersect(&acc, &l.ranges(d));
        }
        acc
    }

    pub fn is_working_day(&self, d: NaiveDate) -> bool {
        !self.ranges(d).is_empty()
    }

    pub fn working_minutes_on(&self, d: NaiveDate) -> Minutes {
        self.ranges(d).iter().map(|r| r.len() as Minutes).sum()
    }

    /// Start of the working day `d` (first working minute), or 08:00 equivalent if non-working.
    pub fn day_start(&self, d: NaiveDate) -> NaiveDateTime {
        match self.ranges(d).first() {
            Some(r) => at(d, r.start),
            None => self.snap_forward(at(d, 0)),
        }
    }

    /// End of the working day `d` (last working minute), or the previous working end if non-working.
    pub fn day_finish(&self, d: NaiveDate) -> NaiveDateTime {
        match self.ranges(d).last() {
            Some(r) => at(d, r.end),
            None => self.snap_backward(at(d, 0)),
        }
    }

    /// First working instant at or after `dt`.
    pub fn snap_forward(&self, dt: NaiveDateTime) -> NaiveDateTime {
        let mut d = dt.date();
        let mut m = minute_of_day(dt);
        for _ in 0..MAX_IDLE_SCAN_DAYS {
            for r in self.ranges(d) {
                if m < r.end {
                    return at(d, m.max(r.start));
                }
            }
            let Some(next) = next_day(d) else { break };
            d = next;
            m = 0;
        }
        dt
    }

    /// Last working instant at or before `dt` (a range end counts as working).
    pub fn snap_backward(&self, dt: NaiveDateTime) -> NaiveDateTime {
        let mut d = dt.date();
        let mut m = minute_of_day(dt);
        if m == 0 {
            let Some(prev) = prev_day(d) else { return dt };
            d = prev;
            m = 1440;
        }
        for _ in 0..MAX_IDLE_SCAN_DAYS {
            for r in self.ranges(d).iter().rev() {
                if m > r.start {
                    return at(d, m.min(r.end));
                }
            }
            let Some(prev) = prev_day(d) else { break };
            d = prev;
            m = 1440;
        }
        dt
    }

    /// Placement of a zero-length event: kept at the end of a working range, otherwise snapped forward.
    pub fn snap_event(&self, dt: NaiveDateTime) -> NaiveDateTime {
        let m = minute_of_day(dt);
        let (d, m) = match dt.date().pred_opt() {
            Some(prev) if m == 0 => (prev, 1440),
            _ => (dt.date(), m),
        };
        if self.ranges(d).iter().any(|r| r.end == m) {
            return dt;
        }
        self.snap_forward(dt)
    }

    /// Adds working minutes to `dt` (negative values subtract).
    ///
    /// A positive result ends exactly at the end of the last consumed working
    /// minute (e.g. 17:00), not at the start of the next working period.
    pub fn add(&self, dt: NaiveDateTime, mins: Minutes) -> NaiveDateTime {
        if mins < 0 {
            return self.sub(dt, -mins);
        }
        if mins == 0 {
            return self.snap_forward(dt);
        }
        let start = self.snap_forward(dt);
        let mut d = start.date();
        let mut m = minute_of_day(start);
        let mut remaining = mins;
        let mut idle = 0;
        while idle < MAX_IDLE_SCAN_DAYS {
            let ranges = self.ranges(d);
            let mut worked = false;
            for r in ranges {
                if r.end <= m {
                    continue;
                }
                worked = true;
                let s = m.max(r.start);
                let avail = Minutes::from(r.end - s);
                if remaining <= avail {
                    return at(d, s + u32::try_from(remaining).unwrap_or(0));
                }
                remaining -= avail;
                m = r.end;
            }
            idle = if worked { 0 } else { idle + 1 };
            let Some(next) = next_day(d) else {
                return at(d, 1440);
            };
            d = next;
            m = 0;
        }
        shift(dt, mins)
    }

    /// Subtracts working minutes from `dt`; the result is the start of the earliest consumed minute.
    pub fn sub(&self, dt: NaiveDateTime, mins: Minutes) -> NaiveDateTime {
        if mins < 0 {
            return self.add(dt, -mins);
        }
        if mins == 0 {
            return self.snap_backward(dt);
        }
        let end = self.snap_backward(dt);
        let mut d = end.date();
        let mut m = minute_of_day(end);
        if m == 0 {
            let Some(prev) = prev_day(d) else { return end };
            d = prev;
            m = 1440;
        }
        let mut remaining = mins;
        let mut idle = 0;
        while idle < MAX_IDLE_SCAN_DAYS {
            let ranges = self.ranges(d);
            let mut worked = false;
            for r in ranges.iter().rev() {
                if r.start >= m {
                    continue;
                }
                worked = true;
                let e = m.min(r.end);
                let avail = Minutes::from(e - r.start);
                if remaining <= avail {
                    return at(d, e - u32::try_from(remaining).unwrap_or(0));
                }
                remaining -= avail;
                m = r.start;
            }
            idle = if worked { 0 } else { idle + 1 };
            let Some(prev) = prev_day(d) else {
                return at(d, 0);
            };
            d = prev;
            m = 1440;
        }
        shift(dt, -mins)
    }

    /// Working minutes in `[a, b)`; negative if `b < a`.
    pub fn between(&self, a: NaiveDateTime, b: NaiveDateTime) -> Minutes {
        if b < a {
            return -self.between(b, a);
        }
        let mut total: Minutes = 0;
        let mut d = a.date().max(horizon_start());
        let last = b.date().min(horizon_end());
        while d <= last {
            let lo = if d == a.date() { minute_of_day(a) } else { 0 };
            let hi = if d == last { minute_of_day(b) } else { 1440 };
            if hi > lo {
                for r in self.ranges(d) {
                    let s = r.start.max(lo);
                    let e = r.end.min(hi);
                    if e > s {
                        total = total.saturating_add(Minutes::from(e - s));
                    }
                }
            }
            match d.succ_opt() {
                Some(n) => d = n,
                None => break,
            }
        }
        total
    }

    /// Working minutes on day `d` that fall inside `[a, b)`.
    pub fn working_minutes_within(&self, d: NaiveDate, a: NaiveDateTime, b: NaiveDateTime) -> Minutes {
        let day_start = at(d, 0);
        let day_end = at(d, 1440);
        let lo = a.max(day_start);
        let hi = b.min(day_end);
        if hi <= lo { 0 } else { self.between(lo, hi) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dt(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(y, mo, d).unwrap().and_hms_opt(h, mi, 0).unwrap()
    }

    #[test]
    fn time_range_serde() {
        let r = TimeRange::hours(8, 12);
        assert_eq!(serde_json::to_string(&r).unwrap(), "\"08:00-12:00\"");
        let back: TimeRange = serde_json::from_str("\"13:30-17:00\"").unwrap();
        assert_eq!(back, TimeRange::new(13 * 60 + 30, 17 * 60));
        assert!(TimeRange::parse("12:00-08:00").is_none());
    }

    #[test]
    fn add_and_sub_working_time() {
        let cal = Calendar::standard(CalendarId(1));
        let wc = WorkCal::single(&cal);
        // Monday 2026-10-19 08:00 + 1 day = Monday 17:00
        assert_eq!(wc.add(dt(2026, 10, 19, 8, 0), 480), dt(2026, 10, 19, 17, 0));
        // Friday 13:00 + 1 day crosses the weekend: Monday 12:00
        assert_eq!(wc.add(dt(2026, 10, 23, 13, 0), 480), dt(2026, 10, 26, 12, 0));
        // Start snapping from 17:00 Friday to Monday 08:00
        assert_eq!(wc.snap_forward(dt(2026, 10, 23, 17, 0)), dt(2026, 10, 26, 8, 0));
        // Backward: Monday 17:00 - 1 day = Monday 08:00
        assert_eq!(wc.sub(dt(2026, 10, 19, 17, 0), 480), dt(2026, 10, 19, 8, 0));
        // Monday 08:00 snapped backward is Friday 17:00
        assert_eq!(wc.snap_backward(dt(2026, 10, 26, 8, 0)), dt(2026, 10, 23, 17, 0));
        assert_eq!(wc.between(dt(2026, 10, 19, 8, 0), dt(2026, 10, 23, 17, 0)), 5 * 480);
        assert_eq!(wc.between(dt(2026, 10, 23, 17, 0), dt(2026, 10, 19, 8, 0)), -5 * 480);
        assert_eq!(wc.snap_event(dt(2026, 10, 19, 17, 0)), dt(2026, 10, 19, 17, 0));
    }

    #[test]
    fn exceptions_and_layers() {
        let mut cal = Calendar::standard(CalendarId(1));
        let wed = NaiveDate::from_ymd_opt(2026, 10, 21).unwrap();
        cal.exceptions.push(CalendarException::non_working("Holiday", wed, wed));
        let wc = WorkCal::single(&cal);
        // Tue 08:00 + 2 days skips Wednesday
        assert_eq!(wc.add(dt(2026, 10, 20, 8, 0), 960), dt(2026, 10, 22, 17, 0));

        let thu = NaiveDate::from_ymd_opt(2026, 10, 22).unwrap();
        let vacation = [CalendarException::non_working("Vacation", thu, thu)];
        let both = WorkCal::from_layers(vec![CalLayer::new(&cal, &[]), CalLayer::new(&cal, &vacation)]);
        assert_eq!(both.add(dt(2026, 10, 20, 8, 0), 960), dt(2026, 10, 23, 17, 0));

        let xmas = CalendarException {
            name: "Christmas".into(),
            start: NaiveDate::from_ymd_opt(2020, 12, 25).unwrap(),
            end: NaiveDate::from_ymd_opt(2020, 12, 25).unwrap(),
            working: vec![],
            yearly: true,
        };
        assert!(xmas.covers(NaiveDate::from_ymd_opt(2031, 12, 25).unwrap()));
    }

    #[test]
    fn night_shift_crosses_midnight() {
        let cal = Calendar::night_shift(CalendarId(2));
        let wc = WorkCal::single(&cal);
        // Monday 23:00 + 8h work = Tuesday 08:00 (with 03:00-04:00 break)
        assert_eq!(wc.add(dt(2026, 10, 19, 23, 0), 480), dt(2026, 10, 20, 8, 0));
        assert_eq!(wc.sub(dt(2026, 10, 20, 8, 0), 480), dt(2026, 10, 19, 23, 0));
    }

    #[test]
    fn empty_calendar_does_not_hang() {
        let mut cal = Calendar::standard(CalendarId(1));
        cal.week = WorkWeek::default();
        let wc = WorkCal::single(&cal);
        let start = dt(2026, 10, 19, 8, 0);
        assert_eq!(wc.add(start, 60), start + Duration::minutes(60));
    }
}
