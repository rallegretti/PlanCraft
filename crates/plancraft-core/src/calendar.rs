//! Working-time calendars and calendar-aware date arithmetic.
//!
//! A [`Calendar`] describes the working times of a regular week plus a list of
//! exceptions (holidays, shutdowns, overtime days...).  Resources reference a
//! base calendar and may add their own personal exceptions (vacations, part-time
//! days).  [`WorkCal`] combines one or more such layers; when several layers are
//! combined only the time where *all* layers are working counts as working time.

use chrono::{Datelike, Duration, NaiveDate, NaiveDateTime, NaiveTime, Weekday};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::cell::RefCell;
use std::collections::HashMap;
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
    /// Working time of whole months (`(year, month)`) and whole years (`(year, 0)`), filled on
    /// demand so [`WorkCal::between`], [`WorkCal::add`] and [`WorkCal::sub`] over a long span cost
    /// a few dozen lookups, not one calendar evaluation per day (one date centuries out used to
    /// stall every recalculation; a 5-year task took 1,800 steps to place).
    span_cache: RefCell<HashMap<(i32, u32), Span>>,
}

/// The working time of a whole month or year.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Span {
    minutes: Minutes,
    days: i64,
    /// Days without working time at its start and at its end (all of them when it has none).
    lead: i64,
    trail: i64,
}

impl Span {
    /// Two spans one after the other.
    fn then(self, next: Span) -> Span {
        Span {
            minutes: self.minutes.saturating_add(next.minutes),
            days: self.days.saturating_add(next.days),
            lead: if self.lead == self.days { self.days.saturating_add(next.lead) } else { self.lead },
            trail: if next.trail == next.days { next.days.saturating_add(self.trail) } else { next.trail },
        }
    }
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

/// The last day of `d`'s month.
/// The last day of `d`'s year.
fn year_last_day(d: NaiveDate) -> Option<NaiveDate> {
    NaiveDate::from_ymd_opt(d.year(), 12, 31)
}

fn month_last_day(d: NaiveDate) -> Option<NaiveDate> {
    let (y, m) = if d.month() == 12 { (d.year().checked_add(1)?, 1) } else { (d.year(), d.month() + 1) };
    NaiveDate::from_ymd_opt(y, m, 1).and_then(|first| first.pred_opt())
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
        Self::from_layers(vec![CalLayer::new(calendar, &[])])
    }

    /// Combines layers; with no layers there is no working time at all.
    pub fn from_layers(layers: Vec<CalLayer<'a>>) -> Self {
        Self { layers, span_cache: RefCell::new(HashMap::new()) }
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
        'days: while idle < MAX_IDLE_SCAN_DAYS {
            // At the start of a year or month that ends before the time does, step over it whole.
            // The result is what walking it day by day gives, idle-day limit included.
            if m == 0 {
                for (first, last) in [(d, year_last_day(d)), (d, month_last_day(d))] {
                    let Some(last) = last.filter(|l| *l <= horizon_end()) else { continue };
                    let span = if first.ordinal() == 1 && last.month() == 12 && last.day() == 31 {
                        self.span(first.year(), 0)
                    } else if first.day() == 1 && Some(last) == month_last_day(first) {
                        self.span(first.year(), first.month())
                    } else {
                        continue;
                    };
                    let idle_after = if span.minutes == 0 { idle.saturating_add(span.days) } else { span.trail };
                    let fits = if span.minutes == 0 {
                        idle_after < MAX_IDLE_SCAN_DAYS
                    } else {
                        remaining > span.minutes && idle.saturating_add(span.lead) < MAX_IDLE_SCAN_DAYS
                    };
                    if !fits {
                        continue;
                    }
                    remaining -= span.minutes;
                    idle = idle_after;
                    match next_day(last) {
                        Some(next) => {
                            d = next;
                            continue 'days;
                        }
                        None => return at(last, 1440),
                    }
                }
            }
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
        'days: while idle < MAX_IDLE_SCAN_DAYS {
            // At the end of a year or month that starts after the time runs out, step over it
            // whole (as walking it back day by day would, idle-day limit included).
            if m == 1440 {
                for first in [NaiveDate::from_ymd_opt(d.year(), 1, 1), NaiveDate::from_ymd_opt(d.year(), d.month(), 1)] {
                    let Some(first) = first.filter(|f| *f >= horizon_start()) else { continue };
                    let span = if first.ordinal() == 1 && year_last_day(first) == Some(d) {
                        self.span(first.year(), 0)
                    } else if first.day() == 1 && month_last_day(first) == Some(d) {
                        self.span(first.year(), first.month())
                    } else {
                        continue;
                    };
                    let idle_after = if span.minutes == 0 { idle.saturating_add(span.days) } else { span.lead };
                    let fits = if span.minutes == 0 {
                        idle_after < MAX_IDLE_SCAN_DAYS
                    } else {
                        remaining > span.minutes && idle.saturating_add(span.trail) < MAX_IDLE_SCAN_DAYS
                    };
                    if !fits {
                        continue;
                    }
                    remaining -= span.minutes;
                    idle = idle_after;
                    match prev_day(first) {
                        Some(prev) => {
                            d = prev;
                            continue 'days;
                        }
                        None => return at(first, 0),
                    }
                }
            }
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
            return self.between(b, a).saturating_neg();
        }
        let first = a.date().max(horizon_start());
        let last = b.date().min(horizon_end());
        if first > last {
            return 0;
        }
        let lo = if first == a.date() { minute_of_day(a) } else { 0 };
        let hi = if last == b.date() { minute_of_day(b) } else { 1440 };
        if first == last {
            return self.minutes_in_day(first, lo, hi);
        }
        // Partial first day, the whole days in between, partial last day.
        let mut total = self.minutes_in_day(first, lo, 1440);
        if let (Some(from), Some(to)) = (next_day(first), prev_day(last))
            && from <= to
        {
            total = total.saturating_add(self.whole_days(from, to));
        }
        total.saturating_add(self.minutes_in_day(last, 0, hi))
    }

    /// Working minutes on `d` between minute `lo` and minute `hi` of the day.
    fn minutes_in_day(&self, d: NaiveDate, lo: u32, hi: u32) -> Minutes {
        if hi <= lo {
            return 0;
        }
        self.ranges(d)
            .iter()
            .map(|r| {
                let (s, e) = (r.start.max(lo), r.end.min(hi));
                if e > s { Minutes::from(e - s) } else { 0 }
            })
            .sum()
    }

    /// Working minutes of the whole days `from..=to`, stepping by cached years and months where
    /// they fit and by days only up to the next month boundary.
    fn whole_days(&self, from: NaiveDate, to: NaiveDate) -> Minutes {
        let mut total: Minutes = 0;
        let mut d = from;
        while d <= to {
            let year_end = NaiveDate::from_ymd_opt(d.year(), 12, 31);
            let month_end = month_last_day(d);
            let (minutes, next) = if d.ordinal() == 1 && year_end.is_some_and(|e| e <= to) {
                (self.cached_span(d.year(), 0), year_end.and_then(next_day))
            } else if d.day() == 1 && month_end.is_some_and(|e| e <= to) {
                (self.cached_span(d.year(), d.month()), month_end.and_then(next_day))
            } else {
                (self.working_minutes_on(d), next_day(d))
            };
            total = total.saturating_add(minutes);
            match next {
                Some(n) if n > d => d = n,
                _ => break,
            }
        }
        total
    }

    /// Working minutes of a whole month (`month` 1–12) or year (`month` 0), cached.
    fn cached_span(&self, year: i32, month: u32) -> Minutes {
        self.span(year, month).minutes
    }

    /// The working time of a whole month (`month` 1–12) or year (`month` 0), cached.
    fn span(&self, year: i32, month: u32) -> Span {
        if let Some(span) = self.span_cache.borrow().get(&(year, month)).copied() {
            return span;
        }
        let empty = Span { minutes: 0, days: 0, lead: 0, trail: 0 };
        let span = if month == 0 {
            (1..=12).map(|m| self.span(year, m)).fold(empty, Span::then)
        } else {
            match NaiveDate::from_ymd_opt(year, month, 1) {
                Some(first) => first
                    .iter_days()
                    .take_while(|d| d.month() == month)
                    .map(|d| {
                        let minutes = self.working_minutes_on(d);
                        let idle = i64::from(minutes == 0);
                        Span { minutes, days: 1, lead: idle, trail: idle }
                    })
                    .fold(empty, Span::then),
                None => empty,
            }
        };
        self.span_cache.borrow_mut().insert((year, month), span);
        span
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

    /// `add` and `sub` as they were before they stepped over whole months and years: one day at a
    /// time. The stepping must give exactly these results.
    fn walk_add(wc: &WorkCal, dt: NaiveDateTime, mins: Minutes) -> NaiveDateTime {
        if mins <= 0 {
            return if mins == 0 { wc.snap_forward(dt) } else { walk_sub(wc, dt, -mins) };
        }
        let start = wc.snap_forward(dt);
        let (mut d, mut m, mut remaining, mut idle) = (start.date(), minute_of_day(start), mins, 0);
        while idle < MAX_IDLE_SCAN_DAYS {
            let mut worked = false;
            for r in wc.ranges(d) {
                if r.end <= m {
                    continue;
                }
                worked = true;
                let s = m.max(r.start);
                let avail = Minutes::from(r.end - s);
                if remaining <= avail {
                    return at(d, s + u32::try_from(remaining).unwrap());
                }
                remaining -= avail;
                m = r.end;
            }
            idle = if worked { 0 } else { idle + 1 };
            let Some(next) = next_day(d) else { return at(d, 1440) };
            d = next;
            m = 0;
        }
        shift(dt, mins)
    }

    fn walk_sub(wc: &WorkCal, dt: NaiveDateTime, mins: Minutes) -> NaiveDateTime {
        if mins <= 0 {
            return if mins == 0 { wc.snap_backward(dt) } else { walk_add(wc, dt, -mins) };
        }
        let end = wc.snap_backward(dt);
        let (mut d, mut m) = (end.date(), minute_of_day(end));
        if m == 0 {
            let Some(prev) = prev_day(d) else { return end };
            d = prev;
            m = 1440;
        }
        let (mut remaining, mut idle) = (mins, 0);
        while idle < MAX_IDLE_SCAN_DAYS {
            let mut worked = false;
            for r in wc.ranges(d).iter().rev() {
                if r.start >= m {
                    continue;
                }
                worked = true;
                let e = m.min(r.end);
                let avail = Minutes::from(e - r.start);
                if remaining <= avail {
                    return at(d, e - u32::try_from(remaining).unwrap());
                }
                remaining -= avail;
                m = r.start;
            }
            idle = if worked { 0 } else { idle + 1 };
            let Some(prev) = prev_day(d) else { return at(d, 0) };
            d = prev;
            m = 1440;
        }
        shift(dt, -mins)
    }

    /// Stepping over whole months and years in `add` and `sub` gives exactly what walking day by
    /// day gives: holidays, a combined resource calendar, a month-long shutdown, a leave long
    /// enough to reach the idle-day limit, durations equal to a month's working time, and dates
    /// near the ends of time.
    #[test]
    fn add_and_sub_match_a_day_by_day_walk() {
        let mut cal = Calendar::standard(CalendarId(1));
        let mut xmas = CalendarException::non_working(
            "Christmas",
            NaiveDate::from_ymd_opt(2026, 12, 24).unwrap(),
            NaiveDate::from_ymd_opt(2026, 12, 26).unwrap(),
        );
        xmas.yearly = true;
        cal.exceptions.push(xmas);
        cal.exceptions.push(CalendarException::non_working(
            "Shutdown",
            NaiveDate::from_ymd_opt(2027, 7, 1).unwrap(),
            NaiveDate::from_ymd_opt(2027, 7, 31).unwrap(),
        ));
        let other = Calendar::standard(CalendarId(2));
        let leave = [
            CalendarException::non_working(
                "Leave",
                NaiveDate::from_ymd_opt(2026, 11, 2).unwrap(),
                NaiveDate::from_ymd_opt(2027, 2, 15).unwrap(),
            ),
            // Longer than the idle-day limit: date math gives up inside it.
            CalendarException::non_working(
                "Sabbatical",
                NaiveDate::from_ymd_opt(2030, 1, 1).unwrap(),
                NaiveDate::from_ymd_opt(2035, 3, 1).unwrap(),
            ),
        ];
        let wcs = [WorkCal::single(&cal), WorkCal::from_layers(vec![CalLayer::new(&cal, &[]), CalLayer::new(&other, &leave)])];
        let points = [
            dt(2026, 10, 19, 8, 0),
            dt(2026, 10, 31, 23, 0),
            dt(2026, 11, 30, 17, 0),
            dt(2026, 12, 1, 0, 0),
            dt(2026, 12, 31, 23, 59),
            dt(2027, 1, 1, 0, 0),
            dt(2027, 6, 30, 17, 0),
            dt(2029, 12, 31, 12, 0),
            dt(2033, 6, 1, 8, 0),
            dt(2499, 12, 1, 8, 0),
            dt(1800, 2, 1, 8, 0),
        ];
        let month = 21 * 480; // about one month of working time
        let mut durations: Vec<Minutes> =
            vec![0, 1, 60, 480, 2400, month, month + 1, month - 1, 2 * month, 250 * 480, 251 * 480, 260 * 480];
        durations.extend([3 * 260 * 480, 5 * 260 * 480, crate::limits::MAX_MINUTES, 2 * crate::limits::MAX_MINUTES]);
        // Exactly the working time of whole months and years from the first of a month.
        for (y, mo) in [(2026, 11), (2027, 1), (2027, 7), (2028, 2)] {
            for wc in &wcs {
                durations.push(wc.span(y, mo).minutes);
                durations.push(wc.span(y, 0).minutes);
                durations.push(wc.span(y, mo).minutes + wc.span(y, mo + 1).minutes);
            }
        }
        for wc in &wcs {
            for &p in &points {
                for &d in &durations {
                    assert_eq!(wc.add(p, d), walk_add(wc, p, d), "add {p} + {d}");
                    assert_eq!(wc.sub(p, d), walk_sub(wc, p, d), "sub {p} - {d}");
                }
            }
        }
    }

    proptest::proptest! {
        #![proptest_config(proptest::prelude::ProptestConfig::with_cases(
            std::env::var("PROPTEST_CASES").ok().and_then(|v| v.parse().ok()).unwrap_or(64)
        ))]

        /// Random holidays (some yearly, some months long), times and durations up to ten years:
        /// stepping over months and years agrees with walking day by day, both ways.
        #[test]
        fn add_and_sub_match_a_day_by_day_walk_for_random_calendars(
            holidays in proptest::collection::vec((0i64..4000, 0i64..400, proptest::bool::ANY), 0..8),
            start_day in 0i64..4000,
            minute in 0u32..1440,
            mins in 0i64..(2 * crate::limits::MAX_MINUTES),
        ) {
            let base = NaiveDate::from_ymd_opt(2024, 1, 1).unwrap();
            let mut cal = Calendar::standard(CalendarId(1));
            for (from, len, yearly) in holidays {
                let a = base + chrono::Duration::days(from);
                let mut h = CalendarException::non_working("Off", a, a + chrono::Duration::days(len));
                h.yearly = yearly && len < 30;
                cal.exceptions.push(h);
            }
            let wc = WorkCal::single(&cal);
            let p = at(base + chrono::Duration::days(start_day), minute);
            proptest::prop_assert_eq!(wc.add(p, mins), walk_add(&wc, p, mins));
            proptest::prop_assert_eq!(wc.sub(p, mins), walk_sub(&wc, p, mins));
        }
    }

    /// The cached month/year stepping in `between` must agree with counting every day, across
    /// month and year boundaries, leap years, holidays (yearly and one-off) and partial days.
    #[test]
    fn between_matches_a_day_by_day_count() {
        let mut cal = Calendar::standard(CalendarId(1));
        let mut xmas = CalendarException::non_working(
            "Christmas",
            NaiveDate::from_ymd_opt(2026, 12, 24).unwrap(),
            NaiveDate::from_ymd_opt(2026, 12, 26).unwrap(),
        );
        xmas.yearly = true;
        cal.exceptions.push(xmas);
        cal.exceptions.push(CalendarException::non_working(
            "Shutdown",
            NaiveDate::from_ymd_opt(2027, 7, 1).unwrap(),
            NaiveDate::from_ymd_opt(2027, 7, 31).unwrap(),
        ));
        let leave = [CalendarException::non_working(
            "Leave",
            NaiveDate::from_ymd_opt(2026, 11, 2).unwrap(),
            NaiveDate::from_ymd_opt(2027, 2, 15).unwrap(),
        )];
        let other = Calendar::standard(CalendarId(2));
        let wcs = [WorkCal::single(&cal), WorkCal::from_layers(vec![CalLayer::new(&cal, &[]), CalLayer::new(&other, &leave)])];
        let naive = |wc: &WorkCal, a: NaiveDateTime, b: NaiveDateTime| -> Minutes {
            let mut total = 0;
            let mut d = a.date();
            while d <= b.date() {
                let lo = if d == a.date() { minute_of_day(a) } else { 0 };
                let hi = if d == b.date() { minute_of_day(b) } else { 1440 };
                total += wc.minutes_in_day(d, lo, hi);
                d = d.succ_opt().unwrap();
            }
            total
        };
        let points = [
            dt(2026, 10, 19, 8, 0),
            dt(2026, 10, 21, 13, 30),
            dt(2026, 11, 30, 17, 0),
            dt(2026, 12, 1, 0, 0),
            dt(2026, 12, 31, 23, 59),
            dt(2027, 1, 1, 0, 0),
            dt(2027, 3, 15, 10, 0),
            dt(2028, 2, 29, 9, 0),
            dt(2030, 6, 30, 16, 0),
            dt(2031, 1, 1, 12, 0),
        ];
        for wc in &wcs {
            for &a in &points {
                for &b in &points {
                    let expected = if b < a { -naive(wc, b, a) } else { naive(wc, a, b) };
                    assert_eq!(wc.between(a, b), expected, "{a} .. {b}");
                }
            }
        }
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
