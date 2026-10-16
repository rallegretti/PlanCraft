//! Two-tier timescale used by the Gantt chart, usage views and timeline.

use chrono::{Datelike, Duration, NaiveDate, NaiveDateTime, NaiveTime, Timelike};
use egui::{Align2, FontId, Painter, Rect, Stroke, pos2};

use crate::state::GanttState;
use crate::theme;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unit {
    Hours6,
    Day,
    Week,
    Month,
    Quarter,
    Year,
}

impl Unit {
    pub fn floor(self, t: NaiveDateTime) -> NaiveDateTime {
        let d = t.date();
        let midnight = |d: NaiveDate| d.and_time(NaiveTime::MIN);
        match self {
            Unit::Hours6 => midnight(d) + Duration::hours((t.hour() / 6 * 6) as i64),
            Unit::Day => midnight(d),
            Unit::Week => midnight(d - Duration::days(d.weekday().num_days_from_monday() as i64)),
            Unit::Month => midnight(d.with_day(1).unwrap_or(d)),
            Unit::Quarter => midnight(NaiveDate::from_ymd_opt(d.year(), (d.month0() / 3) * 3 + 1, 1).unwrap_or(d)),
            Unit::Year => midnight(NaiveDate::from_ymd_opt(d.year(), 1, 1).unwrap_or(d)),
        }
    }

    pub fn next(self, t: NaiveDateTime) -> NaiveDateTime {
        let add_months = |t: NaiveDateTime, m: u32| {
            let d = t.date();
            let total = d.year() * 12 + d.month0() as i32 + m as i32;
            // Out of chrono's range: still move forward so callers stepping through units terminate.
            NaiveDate::from_ymd_opt(total.div_euclid(12), total.rem_euclid(12) as u32 + 1, 1)
                .map_or(t + Duration::days(31 * i64::from(m)), |d| d.and_time(NaiveTime::MIN))
        };
        match self {
            Unit::Hours6 => t + Duration::hours(6),
            Unit::Day => t + Duration::days(1),
            Unit::Week => t + Duration::days(7),
            Unit::Month => add_months(t, 1),
            Unit::Quarter => add_months(t, 3),
            Unit::Year => add_months(t, 12),
        }
    }

    pub fn label(self, t: NaiveDateTime, width: f32) -> String {
        let d = t.date();
        match self {
            Unit::Hours6 => format!("{:02}", t.hour()),
            Unit::Day => {
                if width >= 90.0 {
                    d.format("%a %b %-d").to_string()
                } else if width >= 26.0 {
                    d.format("%-d").to_string()
                } else {
                    d.format("%a").to_string().chars().next().map(String::from).unwrap_or_default()
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
                    d.format("%b").to_string().chars().next().map(String::from).unwrap_or_default()
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

/// (top tier, bottom tier) for a zoom level.
pub fn tiers(px_per_day: f32) -> (Unit, Unit) {
    if px_per_day >= 72.0 {
        (Unit::Day, Unit::Hours6)
    } else if px_per_day >= 14.0 {
        (Unit::Week, Unit::Day)
    } else if px_per_day >= 4.0 {
        (Unit::Month, Unit::Week)
    } else if px_per_day >= 1.5 {
        (Unit::Quarter, Unit::Month)
    } else {
        (Unit::Year, Unit::Quarter)
    }
}

/// Paints the two-tier header into `rect`. Returns the bottom-tier boundaries (x positions) for gridlines.
pub fn paint_header(painter: &Painter, rect: Rect, g: &GanttState) -> Vec<(f32, NaiveDateTime)> {
    let (top, bottom) = tiers(g.px_per_day);
    painter.rect_filled(rect, 0.0, theme::pal().HEADER_BG);
    let mid = rect.top() + rect.height() / 2.0;
    let font = FontId::proportional(11.5);
    let end = g.time_at(rect.left(), rect.right());
    let mut out = Vec::new();
    for (unit, y0, y1, collect) in [(top, rect.top(), mid, false), (bottom, mid, rect.bottom(), true)] {
        let mut t = unit.floor(g.left);
        let mut guard = 0;
        while t <= end && guard < 2000 {
            guard += 1;
            let n = unit.next(t);
            let x0 = g.x_of(rect.left(), t);
            let x1 = g.x_of(rect.left(), n);
            painter.vline(x0.round() + 0.5, y0..=y1, Stroke::new(1.0, theme::pal().GRID_LINE_STRONG));
            let cell = Rect::from_x_y_ranges(x0.max(rect.left())..=x1.min(rect.right()), y0..=y1);
            if cell.width() > 8.0 {
                let label = unit.label(t, x1 - x0);
                let anchor = if collect || x1 - x0 < 60.0 { Align2::CENTER_CENTER } else { Align2::LEFT_CENTER };
                let pos = if anchor == Align2::CENTER_CENTER { cell.center() } else { pos2(cell.left() + 4.0, cell.center().y) };
                painter.with_clip_rect(cell).text(pos, anchor, label, font.clone(), theme::pal().TEXT);
            }
            if collect {
                out.push((x0, t));
            }
            t = n;
        }
    }
    painter.hline(rect.x_range(), mid, Stroke::new(1.0, theme::pal().GRID_LINE));
    painter.hline(rect.x_range(), rect.bottom() - 0.5, Stroke::new(1.0, theme::pal().GRID_LINE_STRONG));
    out
}
