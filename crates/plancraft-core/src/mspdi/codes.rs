//! MSPDI value formats and code tables, from Microsoft's published schema reference
//! (learn.microsoft.com, "Project XML Data Interchange Schema Reference") and the `PjField`
//! enumeration (learn.microsoft.com/office/vba/api/project.pjfield).

use chrono::{NaiveDate, NaiveDateTime, NaiveTime};

use crate::custom::CustomFieldKind;
use crate::duration::{DurationUnit, Minutes};
use crate::limits::MAX_MINUTES;
use crate::model::{ConstraintKind, LinkType, TaskType, WorkContour};

/// The schema's target namespace (mspdi_pj12.xsd).
pub const NAMESPACE: &str = "http://schemas.microsoft.com/project/2007";
/// `SaveVersion`: the schema documents 12 (Project 2007), the version whose schema we follow.
pub const SAVE_VERSION: u32 = 12;

/// `true`/`false` or `1`/`0` (xsd:boolean).
pub fn bool_of(s: &str) -> Option<bool> {
    match s.trim() {
        "1" | "true" => Some(true),
        "0" | "false" => Some(false),
        _ => None,
    }
}

pub fn int_of(s: &str) -> Option<i64> {
    s.trim().parse::<i64>().ok()
}

/// A finite number (xsd:float / xsd:decimal).
pub fn num_of(s: &str) -> Option<f64> {
    s.trim().parse::<f64>().ok().filter(|v| v.is_finite())
}

/// An xsd:dateTime. A time zone or fraction of a second is dropped (plans keep local times);
/// a bare date means midnight.
pub fn datetime_of(s: &str) -> Option<NaiveDateTime> {
    let s = s.trim();
    let (date, time) = match s.split_once('T') {
        Some((d, t)) => (d, Some(t)),
        None => (s, None),
    };
    let date = NaiveDate::parse_from_str(date, "%Y-%m-%d").ok()?;
    let time = match time {
        None => NaiveTime::MIN,
        Some(t) => {
            // Drop a zone (Z, +hh:mm, -hh:mm) and a fraction of a second.
            let t = t.trim_end_matches('Z');
            let t = match t.find(['+', '-']) {
                Some(i) => t.get(..i).unwrap_or(t),
                None => t,
            };
            let t = t.split('.').next().unwrap_or(t);
            time_of_day(t)?
        }
    };
    Some(date.and_time(time))
}

fn time_of_day(t: &str) -> Option<NaiveTime> {
    let mut parts = t.split(':');
    let h: u32 = parts.next()?.trim().parse().ok()?;
    let m: u32 = parts.next().map_or(Some(0), |p| p.trim().parse().ok())?;
    let sec: u32 = parts.next().map_or(Some(0), |p| p.trim().parse().ok())?;
    if h == 24 && m == 0 && sec == 0 {
        // 24:00:00 is the end of the day; a date-time keeps it as the last second.
        return NaiveTime::from_hms_opt(23, 59, 59);
    }
    NaiveTime::from_hms_opt(h, m, sec)
}

/// An xsd:time as minutes after midnight (`24:00:00` is 1440).
pub fn time_of(s: &str) -> Option<u32> {
    let s = s.trim();
    let s = s.split('.').next().unwrap_or(s);
    let mut parts = s.split(':');
    let h: u32 = parts.next()?.trim().parse().ok()?;
    let m: u32 = parts.next().map_or(Some(0), |p| p.trim().parse().ok())?;
    let sec: u32 = parts.next().map_or(Some(0), |p| p.trim().parse().ok())?;
    if h > 24 || m > 59 || sec > 59 || (h == 24 && (m > 0 || sec > 0)) {
        return None;
    }
    Some(h * 60 + m)
}

/// An xsd:duration (`PT8H0M0S`, `-PT30M`, `P1DT2H`) in minutes, clamped to ±5 years. Days are
/// 24 hours, months 30 days and years 365 days, as in XML Schema's own arithmetic.
pub fn duration_of(s: &str) -> Option<Minutes> {
    let s = s.trim();
    let (neg, rest) = match s.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, s),
    };
    let rest = rest.strip_prefix('P')?;
    let mut minutes = 0f64;
    let mut in_time = false;
    let mut number = String::new();
    let mut any = false;
    for c in rest.chars() {
        match c {
            'T' if number.is_empty() && !in_time => in_time = true,
            '0'..='9' | '.' => number.push(c),
            unit => {
                let v: f64 = number.parse().ok()?;
                number.clear();
                any = true;
                let factor = match (unit, in_time) {
                    ('Y', false) => 365.0 * 1440.0,
                    ('M', false) => 30.0 * 1440.0,
                    ('W', false) => 7.0 * 1440.0,
                    ('D', false) => 1440.0,
                    ('H', true) => 60.0,
                    ('M', true) => 1.0,
                    ('S', true) => 1.0 / 60.0,
                    _ => return None,
                };
                minutes += v * factor;
            }
        }
    }
    if !number.is_empty() || !any || !minutes.is_finite() {
        return None;
    }
    let m = minutes.round().clamp(-(MAX_MINUTES as f64), MAX_MINUTES as f64) as Minutes;
    Some(if neg { -m } else { m })
}

/// Minutes as an xsd:duration in hours and minutes (`PT8H0M0S`).
pub fn duration_text(m: Minutes) -> String {
    let sign = if m < 0 { "-" } else { "" };
    let m = m.unsigned_abs();
    format!("{sign}PT{}H{}M0S", m / 60, m % 60)
}

pub fn datetime_text(dt: NaiveDateTime) -> String {
    dt.format("%Y-%m-%dT%H:%M:%S").to_string()
}

/// Minutes after midnight as an xsd:time; the end of the day (1440) is written `00:00:00`.
pub fn time_text(m: u32) -> String {
    let m = m % 1440;
    format!("{:02}:{:02}:00", m / 60, m % 60)
}

/// A duration format code (DurationFormat, LagFormat, LevelingDelayFormat): 3=m, 4=em, 5=h,
/// 6=eh, 7=d, 8=ed, 9=w, 10=ew, 11=mo, 12=emo, 19=%, 20=e%, 21=null, and the same plus 32 for
/// an estimated (`?`) value; 53=null.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Format {
    /// `None` for the percent formats.
    pub unit: Option<DurationUnit>,
    pub elapsed: bool,
    pub estimated: bool,
}

pub fn format_of(code: i64) -> Option<Format> {
    let (base, estimated) = if (35..=52).contains(&code) { (code - 32, true) } else { (code, false) };
    let (unit, elapsed) = match base {
        3 => (Some(DurationUnit::Minutes), false),
        4 => (Some(DurationUnit::Minutes), true),
        5 => (Some(DurationUnit::Hours), false),
        6 => (Some(DurationUnit::Hours), true),
        7 => (Some(DurationUnit::Days), false),
        8 => (Some(DurationUnit::Days), true),
        9 => (Some(DurationUnit::Weeks), false),
        10 => (Some(DurationUnit::Weeks), true),
        11 => (Some(DurationUnit::Months), false),
        12 => (Some(DurationUnit::Months), true),
        19 => (None, false),
        20 => (None, true),
        _ => return None,
    };
    Some(Format { unit, elapsed, estimated })
}

pub fn format_code(unit: DurationUnit, elapsed: bool, estimated: bool) -> i64 {
    let base = match unit {
        DurationUnit::Minutes => 3,
        DurationUnit::Hours => 5,
        DurationUnit::Days => 7,
        DurationUnit::Weeks => 9,
        DurationUnit::Months => 11,
    } + i64::from(elapsed);
    if estimated { base + 32 } else { base }
}

/// PredecessorLink `Type`: 0=FF, 1=FS, 2=SF, 3=SS.
pub fn link_type_of(code: i64) -> Option<LinkType> {
    match code {
        0 => Some(LinkType::FF),
        1 => Some(LinkType::FS),
        2 => Some(LinkType::SF),
        3 => Some(LinkType::SS),
        _ => None,
    }
}

pub fn link_type_code(t: LinkType) -> i64 {
    match t {
        LinkType::FF => 0,
        LinkType::FS => 1,
        LinkType::SF => 2,
        LinkType::SS => 3,
    }
}

/// `ConstraintType`: 0=As Soon As Possible, 1=As Late As Possible, 2=Must Start On, 3=Must
/// Finish On, 4=Start No Earlier Than, 5=Start No Later Than, 6=Finish No Earlier Than, 7=Finish
/// No Later Than.
pub fn constraint_of(code: i64) -> Option<ConstraintKind> {
    Some(match code {
        0 => ConstraintKind::Asap,
        1 => ConstraintKind::Alap,
        2 => ConstraintKind::Mso,
        3 => ConstraintKind::Mfo,
        4 => ConstraintKind::Snet,
        5 => ConstraintKind::Snlt,
        6 => ConstraintKind::Fnet,
        7 => ConstraintKind::Fnlt,
        _ => return None,
    })
}

pub fn constraint_code(k: ConstraintKind) -> i64 {
    match k {
        ConstraintKind::Asap => 0,
        ConstraintKind::Alap => 1,
        ConstraintKind::Mso => 2,
        ConstraintKind::Mfo => 3,
        ConstraintKind::Snet => 4,
        ConstraintKind::Snlt => 5,
        ConstraintKind::Fnet => 6,
        ConstraintKind::Fnlt => 7,
    }
}

/// Task `Type`: 0=Fixed Units, 1=Fixed Duration, 2=Fixed Work.
pub fn task_type_of(code: i64) -> Option<TaskType> {
    match code {
        0 => Some(TaskType::FixedUnits),
        1 => Some(TaskType::FixedDuration),
        2 => Some(TaskType::FixedWork),
        _ => None,
    }
}

pub fn task_type_code(t: TaskType) -> i64 {
    match t {
        TaskType::FixedUnits => 0,
        TaskType::FixedDuration => 1,
        TaskType::FixedWork => 2,
    }
}

/// Assignment `WorkContour`: 0=Flat, 1=Back Loaded, 2=Front Loaded, 3=Double Peak, 4=Early
/// Peak, 5=Late Peak, 6=Bell, 7=Turtle, 8=Contoured. PlanCraft has four shapes; the others map to
/// the nearest one (the second value is false when that is an approximation).
pub fn contour_of(code: i64) -> (WorkContour, bool) {
    match code {
        0 => (WorkContour::Flat, true),
        1 => (WorkContour::BackLoaded, true),
        2 => (WorkContour::FrontLoaded, true),
        6 => (WorkContour::Bell, true),
        3 | 7 => (WorkContour::Bell, false),
        4 => (WorkContour::FrontLoaded, false),
        5 => (WorkContour::BackLoaded, false),
        _ => (WorkContour::Flat, false),
    }
}

pub fn contour_code(c: WorkContour) -> i64 {
    match c {
        WorkContour::Flat => 0,
        WorkContour::BackLoaded => 1,
        WorkContour::FrontLoaded => 2,
        WorkContour::Bell => 6,
    }
}

/// Calendar `DayType` 1–7 (1=Sunday … 7=Saturday).
pub fn weekday_of(code: i64) -> Option<chrono::Weekday> {
    use chrono::Weekday::*;
    Some(match code {
        1 => Sun,
        2 => Mon,
        3 => Tue,
        4 => Wed,
        5 => Thu,
        6 => Fri,
        7 => Sat,
        _ => return None,
    })
}

pub fn weekday_code(d: chrono::Weekday) -> i64 {
    i64::from(d.num_days_from_sunday()) + 1
}

/// A task custom field slot: its kind and number (`Text1`, `Number3`, …).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Slot {
    pub kind: CustomFieldKind,
    pub number: usize,
}

impl Slot {
    pub fn field_name(self) -> String {
        let kind = match self.kind {
            CustomFieldKind::Text => "Text",
            CustomFieldKind::Number => "Number",
            CustomFieldKind::Cost => "Cost",
            CustomFieldKind::Date => "Date",
            CustomFieldKind::Duration => "Duration",
            CustomFieldKind::Flag => "Flag",
        };
        format!("{kind}{}", self.number)
    }
}

/// `PjField` values of the task custom fields, by kind, in number order (Text1 first).
const TEXT: [u32; 30] = [
    188743731, 188743734, 188743737, 188743740, 188743743, 188743746, 188743747, 188743748, 188743749, 188743750, 188743997, 188743998,
    188743999, 188744000, 188744001, 188744002, 188744003, 188744004, 188744005, 188744006, 188744007, 188744008, 188744009, 188744010,
    188744011, 188744012, 188744013, 188744014, 188744015, 188744016,
];
const NUMBER: [u32; 20] = [
    188743767, 188743768, 188743769, 188743770, 188743771, 188743982, 188743983, 188743984, 188743985, 188743986, 188743987, 188743988,
    188743989, 188743990, 188743991, 188743992, 188743993, 188743994, 188743995, 188743996,
];
const COST: [u32; 10] = [188743786, 188743787, 188743788, 188743938, 188743939, 188743940, 188743941, 188743942, 188743943, 188743944];
const DATE: [u32; 10] = [188743945, 188743946, 188743947, 188743948, 188743949, 188743950, 188743951, 188743952, 188743953, 188743954];
const DURATION: [u32; 10] = [188743783, 188743784, 188743785, 188743955, 188743956, 188743957, 188743958, 188743959, 188743960, 188743961];
const FLAG: [u32; 20] = [
    188743752, 188743753, 188743754, 188743755, 188743756, 188743757, 188743758, 188743759, 188743760, 188743761, 188743972, 188743973,
    188743974, 188743975, 188743976, 188743977, 188743978, 188743979, 188743980, 188743981,
];
/// Start1–10 and Finish1–10 (the interim plans).
const START: [u32; 10] = [188743732, 188743735, 188743738, 188743741, 188743744, 188743962, 188743964, 188743966, 188743968, 188743970];
const FINISH: [u32; 10] = [188743733, 188743736, 188743739, 188743742, 188743745, 188743963, 188743965, 188743967, 188743969, 188743971];

fn table(kind: CustomFieldKind) -> &'static [u32] {
    match kind {
        CustomFieldKind::Text => &TEXT,
        CustomFieldKind::Number => &NUMBER,
        CustomFieldKind::Cost => &COST,
        CustomFieldKind::Date => &DATE,
        CustomFieldKind::Duration => &DURATION,
        CustomFieldKind::Flag => &FLAG,
    }
}

/// The field id of a custom field slot (`None` past the last one of its kind).
pub fn field_id(slot: Slot) -> Option<u32> {
    slot.number.checked_sub(1).and_then(|i| table(slot.kind).get(i)).copied()
}

/// The custom field slot a field id names.
pub fn slot_of(field_id: u32) -> Option<Slot> {
    CustomFieldKind::ALL.into_iter().find_map(|kind| table(kind).iter().position(|&f| f == field_id).map(|i| Slot { kind, number: i + 1 }))
}

/// How many slots of a kind exist.
pub fn slots(kind: CustomFieldKind) -> usize {
    table(kind).len()
}

/// The interim plan (1–10) whose start (`true`) or finish (`false`) a field id holds.
pub fn interim_of(field_id: u32) -> Option<(u8, bool)> {
    let n = |i: usize| u8::try_from(i + 1).ok();
    START
        .iter()
        .position(|&f| f == field_id)
        .and_then(n)
        .map(|n| (n, true))
        .or_else(|| FINISH.iter().position(|&f| f == field_id).and_then(n).map(|n| (n, false)))
}

pub fn interim_ids(n: u8) -> Option<(u32, u32)> {
    let i = usize::from(n).checked_sub(1)?;
    Some((*START.get(i)?, *FINISH.get(i)?))
}

/// `CFType` of an extended attribute: 0=Cost, 1=Date, 2=Duration, 3=Finish, 4=Flag, 5=Number,
/// 6=Start, 7=Text.
pub fn cf_type_of(code: i64) -> Option<CustomFieldKind> {
    Some(match code {
        0 => CustomFieldKind::Cost,
        1 | 3 | 6 => CustomFieldKind::Date,
        2 => CustomFieldKind::Duration,
        4 => CustomFieldKind::Flag,
        5 => CustomFieldKind::Number,
        7 => CustomFieldKind::Text,
        _ => return None,
    })
}

pub fn cf_type_code(kind: CustomFieldKind) -> i64 {
    match kind {
        CustomFieldKind::Cost => 0,
        CustomFieldKind::Date => 1,
        CustomFieldKind::Duration => 2,
        CustomFieldKind::Flag => 4,
        CustomFieldKind::Number => 5,
        CustomFieldKind::Text => 7,
    }
}

/// An ISO 4217 code for a currency symbol (`CurrencyCode` is required by the schema); `XXX`
/// (no currency) when the symbol is not one of the common ones.
pub fn currency_code(symbol: &str) -> &'static str {
    match symbol.trim() {
        "$" | "US$" | "USD" => "USD",
        "€" | "EUR" => "EUR",
        "£" | "GBP" => "GBP",
        "¥" | "JPY" => "JPY",
        "A$" | "AUD" => "AUD",
        "C$" | "CAD" => "CAD",
        "CHF" => "CHF",
        "₹" | "INR" => "INR",
        "kr" | "SEK" => "SEK",
        _ => "XXX",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_parse_and_print() {
        assert_eq!(duration_of("PT8H0M0S"), Some(480));
        assert_eq!(duration_of("PT0H30M0S"), Some(30));
        assert_eq!(duration_of("-PT1H"), Some(-60));
        assert_eq!(duration_of("P1DT2H"), Some(1440 + 120));
        assert_eq!(duration_of("PT1.5H"), Some(90));
        assert_eq!(duration_of("P99999999Y"), Some(MAX_MINUTES));
        for bad in ["", "P", "PT", "8H", "PT8", "PTH", "PT8X", "PT1.2.3H", "P1H"] {
            assert_eq!(duration_of(bad), None, "{bad}");
        }
        assert_eq!(duration_text(495), "PT8H15M0S");
        assert_eq!(duration_of(&duration_text(-75)), Some(-75));
    }

    #[test]
    fn dates_and_times() {
        let d = |s| datetime_of(s).map(|d| d.to_string());
        assert_eq!(d("2026-11-16T08:00:00").as_deref(), Some("2026-11-16 08:00:00"));
        assert_eq!(d("2026-11-16T08:00:00.000+01:00").as_deref(), Some("2026-11-16 08:00:00"));
        assert_eq!(d("2026-11-16T08:00:00Z").as_deref(), Some("2026-11-16 08:00:00"));
        assert_eq!(d("2026-11-16").as_deref(), Some("2026-11-16 00:00:00"));
        assert_eq!(d("2026-13-16T08:00:00"), None);
        assert_eq!(d("nonsense"), None);
        assert_eq!(time_of("17:00:00"), Some(1020));
        assert_eq!(time_of("24:00:00"), Some(1440));
        assert_eq!(time_of("25:00:00"), None);
        assert_eq!(time_text(1440), "00:00:00");
        assert_eq!(time_text(510), "08:30:00");
    }

    #[test]
    fn field_ids_round_trip() {
        for kind in CustomFieldKind::ALL {
            for number in 1..=slots(kind) {
                let slot = Slot { kind, number };
                assert_eq!(field_id(slot).and_then(slot_of), Some(slot));
            }
            assert_eq!(field_id(Slot { kind, number: slots(kind) + 1 }), None);
            assert_eq!(field_id(Slot { kind, number: 0 }), None);
        }
        for n in 1..=10u8 {
            let (s, f) = interim_ids(n).unwrap();
            assert_eq!(interim_of(s), Some((n, true)));
            assert_eq!(interim_of(f), Some((n, false)));
        }
        assert_eq!(interim_ids(0), None);
        assert_eq!(interim_ids(11), None);
    }

    #[test]
    fn format_codes_round_trip() {
        for unit in [DurationUnit::Minutes, DurationUnit::Hours, DurationUnit::Days, DurationUnit::Weeks, DurationUnit::Months] {
            for elapsed in [false, true] {
                for estimated in [false, true] {
                    let code = format_code(unit, elapsed, estimated);
                    assert_eq!(format_of(code), Some(Format { unit: Some(unit), elapsed, estimated }));
                }
            }
        }
        assert_eq!(format_of(21), None);
        assert_eq!(format_of(53), None);
        assert_eq!(format_of(19).map(|f| f.unit), Some(None));
    }
}
