//! Timescale units shared by the Gantt chart, usage views, the timeline and printing.

use chrono::{Datelike, NaiveDate, NaiveDateTime, NaiveTime, Timelike};

use crate::calendar::shift;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unit {
    Hours6,
    Day,
    Week,
    Month,
    Quarter,
    Year,
}

fn midnight(d: NaiveDate) -> NaiveDateTime {
    d.and_time(NaiveTime::MIN)
}

impl Unit {
    /// Start of the unit containing `t`.
    pub fn floor(self, t: NaiveDateTime) -> NaiveDateTime {
        let d = t.date();
        match self {
            Unit::Hours6 => shift(midnight(d), i64::from(t.hour() / 6 * 6) * 60),
            Unit::Day => midnight(d),
            Unit::Week => midnight(d.checked_sub_days(chrono::Days::new(u64::from(d.weekday().num_days_from_monday()))).unwrap_or(d)),
            Unit::Month => midnight(d.with_day(1).unwrap_or(d)),
            Unit::Quarter => midnight(NaiveDate::from_ymd_opt(d.year(), (d.month0() / 3) * 3 + 1, 1).unwrap_or(d)),
            Unit::Year => midnight(NaiveDate::from_ymd_opt(d.year(), 1, 1).unwrap_or(d)),
        }
    }

    /// Start of the next unit. Always later than `t` unless `t` is at the end of the date range.
    pub fn next(self, t: NaiveDateTime) -> NaiveDateTime {
        let add_months = |t: NaiveDateTime, m: u32| {
            let d = t.date();
            let total = d.year() * 12 + d.month0() as i32 + m as i32;
            // Out of chrono's range: still move forward so callers stepping through units terminate.
            NaiveDate::from_ymd_opt(total.div_euclid(12), total.rem_euclid(12) as u32 + 1, 1)
                .map_or_else(|| shift(t, 31 * 1440 * i64::from(m)), midnight)
        };
        match self {
            Unit::Hours6 => shift(t, 6 * 60),
            Unit::Day => shift(t, 1440),
            Unit::Week => shift(t, 7 * 1440),
            Unit::Month => add_months(t, 1),
            Unit::Quarter => add_months(t, 3),
            Unit::Year => add_months(t, 12),
        }
    }

    /// Label for the unit starting at `t`, shortened to fit `width` (pixels or points).
    pub fn label(self, t: NaiveDateTime, width: f32) -> String {
        let d = t.date();
        let first = |s: String| s.chars().next().map(String::from).unwrap_or_default();
        match self {
            Unit::Hours6 => format!("{:02}", t.hour()),
            Unit::Day => {
                if width >= 90.0 {
                    d.format("%a %b %-d").to_string()
                } else if width >= 26.0 {
                    d.format("%-d").to_string()
                } else {
                    first(d.format("%a").to_string())
                }
            }
            Unit::Week => {
                if width >= 110.0 {
                    format!("Week of {}", d.format("%b %-d, '%y"))
                } else if width >= 60.0 {
                    d.format("%b %-d, '%y").to_string()
                } else {
                    d.format("%-d").to_string()
                }
            }
            Unit::Month => {
                if width >= 90.0 {
                    d.format("%B %Y").to_string()
                } else if width >= 40.0 {
                    d.format("%b '%y").to_string()
                } else {
                    first(d.format("%b").to_string())
                }
            }
            Unit::Quarter => {
                let q = d.month0() / 3 + 1;
                if width >= 60.0 { format!("Q{q} {}", d.year()) } else { format!("Q{q}") }
            }
            Unit::Year => d.year().to_string(),
        }
    }
}

/// (top tier, bottom tier) for a zoom level in pixels (or points) per day.
pub fn tiers(per_day: f32) -> (Unit, Unit) {
    if per_day >= 72.0 {
        (Unit::Day, Unit::Hours6)
    } else if per_day >= 14.0 {
        (Unit::Week, Unit::Day)
    } else if per_day >= 4.0 {
        (Unit::Month, Unit::Week)
    } else if per_day >= 1.5 {
        (Unit::Quarter, Unit::Month)
    } else {
        (Unit::Year, Unit::Quarter)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn units_step_forward_even_at_the_edge_of_the_date_range() {
        let all = [Unit::Hours6, Unit::Day, Unit::Week, Unit::Month, Unit::Quarter, Unit::Year];
        let t = NaiveDate::from_ymd_opt(2026, 11, 12).and_then(|d| d.and_hms_opt(13, 0, 0)).unwrap();
        for u in all {
            assert!(u.floor(t) <= t);
            assert!(u.next(u.floor(t)) > t, "{u:?}");
        }
        for u in all {
            // Never panics at chrono's limits.
            let _ = u.next(NaiveDateTime::MAX);
            let _ = u.floor(NaiveDateTime::MIN);
            let _ = u.label(NaiveDateTime::MAX, 100.0);
        }
    }
}
