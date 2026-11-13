//! Page layout: a table (task or resource sheet) with an optional Gantt chart, split into pages
//! down (rows) and across (columns, then the timescale).
//!
//! Everything is laid out in "virtual points" (the size at 100% scale) inside the printable
//! area, then scaled and moved inside the margins.

use std::collections::HashMap;

use chrono::{NaiveDate, NaiveDateTime, NaiveTime};

use super::{Document, Item, LegendPlacement, Page, PageSetup, Pt, Rect, Rgb, Stroke, TimescaleFit, font, pt};
use crate::calendar::shift;
use crate::ids::TaskId;
use crate::model::{LinkType, Project, ScheduleMode};
use crate::schedule::{project_calendar, task_calendar};
use crate::timescale::{Unit, tiers};

/// Most pages one print produces (hostile plans and settings can't make millions).
pub const MAX_PAGES: usize = 2000;
/// Pages looked at (printed or skipped as blank) before giving up.
const MAX_PAGE_VISITS: usize = 20 * MAX_PAGES;
/// Smallest printable area in virtual points: a row, the timescale and the bands fit.
const MIN_AREA_W: f32 = 200.0;
const MIN_AREA_H: f32 = 160.0;

const TEXT: f32 = 8.0;
const SMALL: f32 = 7.0;
const ROW_H: f32 = 14.0;
const HEAD_H: f32 = 28.0;
const BAND_H: f32 = 16.0;
const LEGEND_H: f32 = 46.0;
const PAD: f32 = 3.0;
const BAR_H: f32 = 7.0;
/// The chart starts beside the table only if at least this much room is left on the page.
const MIN_CHART_W: f32 = 120.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Align {
    #[default]
    Left,
    Center,
    Right,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PrintColumn {
    pub title: String,
    /// Width in points.
    pub width: f32,
    pub align: Align,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct PrintCell {
    pub text: String,
    pub bold: bool,
    pub color: Option<Rgb>,
    /// Left indent in points (outline level).
    pub indent: f32,
}

impl PrintCell {
    pub fn text(t: impl Into<String>) -> Self {
        Self { text: t.into(), ..Default::default() }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum RowKind {
    /// A task row: its bar prints in the chart.
    Task(TaskId),
    /// A group header: a bar spanning its tasks.
    Group { start: Option<NaiveDateTime>, finish: Option<NaiveDateTime> },
    /// Any other row (resources): no bar.
    Other,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PrintRow {
    pub kind: RowKind,
    pub cells: Vec<PrintCell>,
}

/// The table to print: the view's name, its columns and its rows (already sorted, filtered and
/// formatted by the caller).
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Sheet {
    pub title: String,
    pub columns: Vec<PrintColumn>,
    pub rows: Vec<PrintRow>,
}

/// What the chart shows (the view's options).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ChartOptions {
    pub critical: bool,
    pub baseline: bool,
    pub progress: bool,
    pub links: bool,
    pub slack: bool,
    pub resource_names: bool,
    /// The view's zoom, for [`TimescaleFit::AsShown`].
    pub per_day: f32,
}

impl Default for ChartOptions {
    fn default() -> Self {
        Self { critical: false, baseline: false, progress: true, links: true, slack: false, resource_names: true, per_day: 20.0 }
    }
}

/// Print colours (always the light palette: paper is white).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Palette {
    pub text: Rgb,
    pub text_weak: Rgb,
    pub header_bg: Rgb,
    pub grid: Rgb,
    pub grid_strong: Rgb,
    pub nonworking: Rgb,
    pub chart_grid: Rgb,
    pub accent: Rgb,
    pub accent_mid: Rgb,
    pub accent_dark: Rgb,
    pub gold: Rgb,
    pub bar_task: Rgb,
    pub bar_task_edge: Rgb,
    pub bar_progress: Rgb,
    pub bar_critical: Rgb,
    pub bar_critical_edge: Rgb,
    pub bar_summary: Rgb,
    pub bar_manual: Rgb,
    pub bar_baseline: Rgb,
    pub bar_slack: Rgb,
    pub bar_inactive: Rgb,
    pub milestone: Rgb,
    pub link: Rgb,
    pub status_date: Rgb,
    pub deadline: Rgb,
    pub error: Rgb,
    pub white: Rgb,
}

impl Default for Palette {
    /// The app's light palette.
    fn default() -> Self {
        Self {
            text: Rgb(0x26, 0x26, 0x2B),
            text_weak: Rgb(0x58, 0x5D, 0x67),
            header_bg: Rgb(0xF0, 0xF1, 0xF4),
            grid: Rgb(0xDD, 0xDF, 0xE4),
            grid_strong: Rgb(0xC4, 0xC8, 0xD0),
            nonworking: Rgb(0xEE, 0xF0, 0xF4),
            chart_grid: Rgb(0xEC, 0xEE, 0xF2),
            accent: Rgb(0x1B, 0x3A, 0x8C),
            accent_mid: Rgb(0x2F, 0x5B, 0xC4),
            accent_dark: Rgb(0x12, 0x28, 0x66),
            gold: Rgb(0xF2, 0xB1, 0x1C),
            bar_task: Rgb(0x5A, 0x86, 0xD6),
            bar_task_edge: Rgb(0x2F, 0x5B, 0xC4),
            bar_progress: Rgb(0x15, 0x2E, 0x73),
            bar_critical: Rgb(0xD9, 0x4F, 0x4F),
            bar_critical_edge: Rgb(0xA8, 0x2E, 0x2E),
            bar_summary: Rgb(0x33, 0x38, 0x45),
            bar_manual: Rgb(0x9B, 0xBC, 0xEB),
            bar_baseline: Rgb(0xA9, 0xAE, 0xB8),
            bar_slack: Rgb(0x2C, 0x9C, 0x8F),
            bar_inactive: Rgb(0xE6, 0xE8, 0xEC),
            milestone: Rgb(0x1B, 0x3A, 0x8C),
            link: Rgb(0x3C, 0x4A, 0x66),
            status_date: Rgb(0x8E, 0x44, 0xAD),
            deadline: Rgb(0x2E, 0x8B, 0x57),
            error: Rgb(0xC6, 0x28, 0x28),
            white: Rgb(0xFF, 0xFF, 0xFF),
        }
    }
}

/// Everything one print needs.
pub struct PrintJob<'a> {
    pub project: &'a Project,
    pub setup: &'a PageSetup,
    pub sheet: &'a Sheet,
    /// Print a Gantt chart beside the table.
    pub chart: Option<ChartOptions>,
    /// Dates to print (inclusive); `None` = the whole project.
    pub from: Option<NaiveDate>,
    pub to: Option<NaiveDate>,
    /// When it was printed, for `[Date]`.
    pub printed: NaiveDateTime,
    pub palette: Palette,
}

/// One vertical slice of the printout: some table columns and/or a stretch of the timescale.
#[derive(Clone, Debug)]
struct Strip {
    cols: Vec<usize>,
    /// Chart x offset on the page and width; the chart's offset from the chart start.
    chart: Option<(f32, f32, f32)>,
}

fn text(at: Pt, size: f32, color: Rgb, bold: bool, s: impl Into<String>) -> Item {
    Item::Text { at, size, color, bold, text: s.into() }
}

/// Text placed in a box: fitted (ellipsis), aligned, vertically centred.
fn boxed_text(out: &mut Vec<Item>, r: Rect, s: &str, size: f32, color: Rgb, bold: bool, align: Align) {
    let avail = (r.w - 2.0 * PAD).max(0.0);
    let fitted = font::fit(s, size, bold, avail);
    if fitted.is_empty() {
        return;
    }
    let w = font::width(&fitted, size, bold);
    let x = match align {
        Align::Left => r.x + PAD,
        Align::Center => r.x + (r.w - w) / 2.0,
        Align::Right => r.right() - PAD - w,
    };
    let m = font::metrics();
    let baseline = r.y + r.h / 2.0 + (m.cap_height * size) / 2.0;
    out.push(text(pt(x, baseline), size, color, bold, fitted));
}

fn hline(out: &mut Vec<Item>, x0: f32, x1: f32, y: f32, stroke: Stroke) {
    out.push(Item::Line { points: vec![pt(x0, y), pt(x1, y)], stroke });
}

fn vline(out: &mut Vec<Item>, x: f32, y0: f32, y1: f32, stroke: Stroke) {
    out.push(Item::Line { points: vec![pt(x, y0), pt(x, y1)], stroke });
}

fn diamond(out: &mut Vec<Item>, c: Pt, r: f32, fill: Rgb) {
    out.push(Item::Polygon { points: vec![pt(c.x, c.y - r), pt(c.x + r, c.y), pt(c.x, c.y + r), pt(c.x - r, c.y)], fill });
}

fn arrow(out: &mut Vec<Item>, tip: Pt, dx: f32, dy: f32, fill: Rgb) {
    let (bx, by) = (tip.x - dx * 4.0, tip.y - dy * 4.0);
    let (nx, ny) = (-dy * 2.2, dx * 2.2);
    out.push(Item::Polygon { points: vec![tip, pt(bx + nx, by + ny), pt(bx - nx, by - ny)], fill });
}

/// The timescale range: the requested dates, or the project's dates with a little room.
fn date_range(job: &PrintJob) -> (NaiveDateTime, NaiveDateTime) {
    let p = job.project;
    let mut start = p.sched.start.unwrap_or(p.start);
    let mut finish = p.sched.finish.unwrap_or(start);
    for t in &p.tasks {
        if let Some(b) = t.baseline {
            start = start.min(b.start);
            finish = finish.max(b.finish);
        }
    }
    let from = job.from.map(|d| d.and_time(NaiveTime::MIN));
    let to = job.to.map(|d| shift(d.and_time(NaiveTime::MIN), 1440));
    let days = (finish - start).num_days().max(1);
    // A little room before the start, more after the finish for labels beside the last bars.
    let before = (days / 30).clamp(1, 30) * 1440;
    let after = (days / 6).clamp(3, 120) * 1440;
    let a = from.unwrap_or_else(|| shift(start.date().and_time(NaiveTime::MIN), -before));
    let b = to.unwrap_or_else(|| shift(finish.date().and_time(NaiveTime::MIN), after + 1440));
    if b > a { (a, b) } else { (a, shift(a, 1440)) }
}

/// Replaces header and footer codes.
fn fill_codes(s: &str, job: &PrintJob, page: usize, pages: usize) -> String {
    let p = job.project;
    let day = |d: Option<NaiveDateTime>| d.map(|d| d.format("%a %m/%d/%y").to_string()).unwrap_or_default();
    let value = |code: &str| -> Option<String> {
        Some(match code {
            "Project" => p.name.clone(),
            "Manager" => p.manager.clone(),
            "Company" => p.company.clone(),
            "View" => job.sheet.title.clone(),
            "Date" => job.printed.format("%b %-d, %Y").to_string(),
            "Pages" => pages.to_string(),
            "Page" => page.to_string(),
            "Start" => day(p.sched.start.or(Some(p.start))),
            "Finish" => day(p.sched.finish),
            _ => return None,
        })
    };
    // One pass over the template only: a value that looks like a code (a project named
    // "[Manager]") is printed as is, never expanded again.
    let mut out = String::new();
    let mut rest = s;
    while let Some(open) = rest.find('[') {
        let (before, from) = rest.split_at(open);
        out.push_str(before);
        let filled = from.find(']').and_then(|close| Some((value(from.get(1..close)?)?, close)));
        match filled {
            Some((v, close)) => {
                out.push_str(&v);
                rest = from.get(close + 1..).unwrap_or_default();
            }
            None => {
                out.push('[');
                rest = from.get(1..).unwrap_or_default();
            }
        }
    }
    out.push_str(rest);
    out
}

struct Chart<'a> {
    job: &'a PrintJob<'a>,
    opts: ChartOptions,
    start: NaiveDateTime,
    per_day: f32,
    /// Index in the project of each task (lookups by id are linear otherwise).
    index_of: HashMap<TaskId, usize>,
    /// Links between printed rows, sorted by successor row and by predecessor row, so each page
    /// finds its links without scanning the whole plan.
    by_succ: Vec<LinkRef>,
    by_pred: Vec<LinkRef>,
}

/// A link between two printed rows.
#[derive(Clone, Copy)]
struct LinkRef {
    succ_row: usize,
    pred_row: usize,
    /// Indexes in the project of the successor and the predecessor, and of the link in the
    /// successor's predecessor list.
    succ: usize,
    pred: usize,
    dep: usize,
}

impl Chart<'_> {
    fn task(&self, id: TaskId) -> Option<&crate::model::Task> {
        self.index_of.get(&id).and_then(|&i| self.job.project.tasks.get(i))
    }

    /// Links with either end on `rows`, each once.
    fn links_touching(&self, rows: std::ops::Range<usize>) -> impl Iterator<Item = LinkRef> + '_ {
        let span = |v: &'_ [LinkRef], key: fn(&LinkRef) -> usize, rows: &std::ops::Range<usize>| {
            let a = v.partition_point(|l| key(l) < rows.start);
            let b = v.partition_point(|l| key(l) < rows.end);
            (a, b)
        };
        let (a, b) = span(&self.by_succ, |l| l.succ_row, &rows);
        let (c, d) = span(&self.by_pred, |l| l.pred_row, &rows);
        let by_succ = self.by_succ.get(a..b).unwrap_or_default();
        let by_pred = self.by_pred.get(c..d).unwrap_or_default();
        by_succ.iter().copied().chain(by_pred.iter().copied().filter(move |l| !rows.contains(&l.succ_row)))
    }

    fn x(&self, x0: f32, offset: f32, t: NaiveDateTime) -> f32 {
        let mins = (t - self.start).num_minutes() as f32;
        x0 - offset + mins / 1440.0 * self.per_day
    }

    fn time_at(&self, offset: f32) -> NaiveDateTime {
        let mins = (offset / self.per_day * 1440.0) as i64;
        shift(self.start, mins)
    }

    /// Whether anything is drawn for rows `rows` in the chart stretch `[off, off + w)`.
    fn has_bars(&self, rows: std::ops::Range<usize>, off: f32, w: f32) -> bool {
        let (a, b) = (self.time_at(off), self.time_at(off + w));
        self.job.sheet.rows.get(rows).unwrap_or_default().iter().any(|r| {
            let span = match &r.kind {
                RowKind::Task(id) => self.task(*id).and_then(|t| Some((t.sched.start?, t.sched.finish?))),
                RowKind::Group { start, finish } => start.zip(*finish),
                RowKind::Other => None,
            };
            span.is_some_and(|(s, f)| s <= b && f >= a)
        })
    }

    fn timescale(&self, out: &mut Vec<Item>, r: Rect, off: f32) {
        let pal = &self.job.palette;
        out.push(Item::Rect { rect: r, fill: Some(pal.header_bg), stroke: None });
        let (top, bottom) = tiers(self.per_day);
        let end = self.time_at(off + r.w);
        let mid = r.y + r.h / 2.0;
        for (unit, y0, y1) in [(top, r.y, mid), (bottom, mid, r.bottom())] {
            let mut t = unit.floor(self.time_at(off));
            for _ in 0..2000 {
                if t > end {
                    break;
                }
                let n = unit.next(t);
                if n <= t {
                    break;
                }
                let (x0, x1) = (self.x(r.x, off, t), self.x(r.x, off, n));
                vline(out, x0, y0, y1, Stroke::new(0.5, pal.grid_strong));
                let label = unit.label(t, x1 - x0);
                // Labels of units cut by the page edge are kept to the visible part.
                let vis = Rect::new(x0.max(r.x), y0, (x1.min(r.right()) - x0.max(r.x)).max(0.0), y1 - y0);
                boxed_text(out, vis, &label, SMALL, pal.text, false, Align::Center);
                t = n;
            }
        }
        hline(out, r.x, r.right(), mid, Stroke::new(0.5, pal.grid_strong));
    }

    fn background(&self, out: &mut Vec<Item>, body: Rect, off: f32) {
        let p = self.job.project;
        let pal = &self.job.palette;
        let cal = project_calendar(p);
        let (a, b) = (self.time_at(off), self.time_at(off + body.w));
        if self.per_day >= 4.0 {
            let mut d = a.date();
            for _ in 0..4000 {
                if d > b.date() {
                    break;
                }
                if !cal.is_working_day(d) {
                    let t0 = d.and_time(NaiveTime::MIN);
                    let (x0, x1) = (self.x(body.x, off, t0), self.x(body.x, off, shift(t0, 1440)));
                    out.push(Item::Rect { rect: Rect::new(x0, body.y, x1 - x0, body.h), fill: Some(pal.nonworking), stroke: None });
                }
                let Some(n) = d.succ_opt() else { break };
                d = n;
            }
        }
        let (_, bottom) = tiers(self.per_day);
        if bottom != Unit::Day || self.per_day >= 14.0 {
            let mut t = bottom.floor(a);
            for _ in 0..2000 {
                if t > b {
                    break;
                }
                let n = bottom.next(t);
                if n <= t {
                    break;
                }
                vline(out, self.x(body.x, off, t), body.y, body.bottom(), Stroke::new(0.4, pal.chart_grid));
                t = n;
            }
        }
        if let Some(sd) = p.status_date {
            let x = self.x(body.x, off, cal.day_finish(sd));
            vline(out, x, body.y, body.bottom(), Stroke::dashed(0.8, pal.status_date, 3.0));
        }
    }

    /// Bars and links for sheet rows `rows`, the first drawn at `body.y`.
    fn bars(&self, out: &mut Vec<Item>, body: Rect, off: f32, rows: std::ops::Range<usize>) {
        let p = self.job.project;
        let pal = &self.job.palette;
        let o = &self.opts;
        let first = rows.start;
        let y_of = |r: usize| body.y + (r as f32 - first as f32) * ROW_H + ROW_H / 2.0;
        let xs = |t: NaiveDateTime| self.x(body.x, off, t);

        // Links that touch a row on this page (the other end may be off the page: clipped).
        if o.links {
            for link in self.links_touching(rows.clone()) {
                let (r, pr) = (link.succ_row, link.pred_row);
                let (Some(t), Some(pt_)) = (p.tasks.get(link.succ), p.tasks.get(link.pred)) else { continue };
                let Some(d) = t.predecessors.get(link.dep) else { continue };
                let (Some(ss), Some(sf)) = (t.sched.start, t.sched.finish) else { continue };
                {
                    let (Some(ps), Some(pf)) = (pt_.sched.start, pt_.sched.finish) else { continue };
                    let critical = o.critical && t.sched.critical && pt_.sched.critical;
                    let color = if critical { pal.bar_critical_edge } else { pal.link };
                    let stroke = Stroke::new(0.6, color);
                    let (py, sy) = (y_of(pr), y_of(r));
                    let down = sy > py;
                    let (px0, px1, sx0, sx1) = (xs(ps), xs(pf), xs(ss), xs(sf));
                    let half = BAR_H / 2.0 + 0.5;
                    let pts: Vec<Pt>;
                    match d.kind {
                        LinkType::FS => {
                            let sx = if t.is_milestone() { sx0 } else { sx0 + 2.0 };
                            if sx >= px1 + 4.0 {
                                let ty = if down { sy - half } else { sy + half };
                                pts = vec![pt(px1, py), pt(sx, py), pt(sx, ty)];
                                arrow(out, pt(sx, ty), 0.0, if down { 1.0 } else { -1.0 }, color);
                            } else {
                                let mid = if down { py + ROW_H / 2.0 } else { py - ROW_H / 2.0 };
                                pts = vec![
                                    pt(px1, py),
                                    pt(px1 + 4.0, py),
                                    pt(px1 + 4.0, mid),
                                    pt(sx0 - 6.0, mid),
                                    pt(sx0 - 6.0, sy),
                                    pt(sx0, sy),
                                ];
                                arrow(out, pt(sx0, sy), 1.0, 0.0, color);
                            }
                        }
                        LinkType::SS => {
                            let xa = px0.min(sx0) - 6.0;
                            pts = vec![pt(px0, py), pt(xa, py), pt(xa, sy), pt(sx0, sy)];
                            arrow(out, pt(sx0, sy), 1.0, 0.0, color);
                        }
                        LinkType::FF => {
                            let xa = px1.max(sx1) + 6.0;
                            pts = vec![pt(px1, py), pt(xa, py), pt(xa, sy), pt(sx1, sy)];
                            arrow(out, pt(sx1, sy), -1.0, 0.0, color);
                        }
                        LinkType::SF => {
                            let mid = if down { py + ROW_H / 2.0 } else { py - ROW_H / 2.0 };
                            pts = vec![
                                pt(px0, py),
                                pt(px0 - 6.0, py),
                                pt(px0 - 6.0, mid),
                                pt(sx1 + 6.0, mid),
                                pt(sx1 + 6.0, sy),
                                pt(sx1, sy),
                            ];
                            arrow(out, pt(sx1, sy), -1.0, 0.0, color);
                        }
                    }
                    out.push(Item::Line { points: pts, stroke });
                }
            }
        }

        for r in rows.clone() {
            let Some(row) = self.job.sheet.rows.get(r) else { continue };
            let yc = y_of(r);
            let id = match &row.kind {
                RowKind::Task(id) => *id,
                RowKind::Group { start: Some(s), finish: Some(f) } => {
                    let (x0, x1) = (xs(*s), xs(*f).max(xs(*s) + 1.0));
                    out.push(Item::Rect { rect: Rect::new(x0, yc - 2.0, x1 - x0, 3.5), fill: Some(pal.accent_mid), stroke: None });
                    continue;
                }
                _ => continue,
            };
            let Some(&idx) = self.index_of.get(&id) else { continue };
            let Some(t) = p.tasks.get(idx) else { continue };
            let (Some(s), Some(f)) = (t.sched.start, t.sched.finish) else { continue };
            let (x0, x1) = (xs(s), xs(f).max(xs(s)));
            let critical = o.critical && t.sched.critical;
            let with_baseline = o.baseline && t.baseline.is_some();
            if let (true, Some(b)) = (o.baseline, t.baseline) {
                let (bx0, bx1) = (xs(b.start), xs(b.finish));
                if t.is_milestone() {
                    diamond(out, pt(bx0, yc + 4.0), 3.0, pal.bar_baseline);
                } else {
                    out.push(Item::Rect {
                        rect: Rect::new(bx0, yc + 2.0, (bx1 - bx0).max(1.0), 3.0),
                        fill: Some(pal.bar_baseline),
                        stroke: None,
                    });
                }
            }
            let yc = if with_baseline { yc - 2.0 } else { yc };
            if t.is_milestone() {
                let fill = if critical {
                    pal.bar_critical
                } else if t.is_complete() {
                    pal.accent_dark
                } else {
                    pal.milestone
                };
                diamond(out, pt(x0, yc), 4.5, fill);
                out.push(text(pt(x0 + 7.0, yc + 2.5), SMALL, pal.text, false, f.format("%-m/%-d").to_string()));
            } else if t.sched.is_summary {
                let fill = if critical { pal.bar_critical_edge } else { pal.bar_summary };
                out.push(Item::Rect { rect: Rect::new(x0, yc - 2.5, (x1 - x0).max(1.0), 3.0), fill: Some(fill), stroke: None });
                for x in [x0, x1] {
                    out.push(Item::Polygon { points: vec![pt(x - 3.0, yc + 0.5), pt(x + 3.0, yc + 0.5), pt(x, yc + 4.0)], fill });
                }
                if o.progress && t.sched.percent_complete > 0.0 {
                    let px = x0 + (x1 - x0) * t.sched.percent_complete.clamp(0.0, 100.0) / 100.0;
                    out.push(Item::Rect { rect: Rect::new(x0, yc - 1.75, px - x0, 1.5), fill: Some(pal.gold), stroke: None });
                }
            } else {
                let manual = t.mode == ScheduleMode::Manual;
                let (fill, edge) = if !t.active {
                    (pal.bar_inactive, pal.bar_baseline)
                } else if critical {
                    (pal.bar_critical, pal.bar_critical_edge)
                } else if manual {
                    (pal.bar_manual, pal.bar_task_edge)
                } else if let Some([r, g, b]) = t.bar_color {
                    (Rgb(r, g, b), Rgb(r / 2, g / 2, b / 2))
                } else {
                    (pal.bar_task, pal.bar_task_edge)
                };
                let bar = Rect::new(x0, yc - BAR_H / 2.0, (x1 - x0).max(1.0), BAR_H);
                // A split task: the completed part, a dotted gap, then the remaining part.
                let split = match (t.sched.stop, t.sched.resume) {
                    (Some(stop), Some(resume)) if xs(resume) > xs(stop) + 1.0 => Some((xs(stop), xs(resume))),
                    _ => None,
                };
                match split {
                    Some((xa, xb)) => {
                        for (a, b) in [(x0, xa), (xb, x1)] {
                            let r = Rect::new(a, bar.y, (b - a).max(1.0), BAR_H);
                            out.push(Item::Rect { rect: r, fill: Some(fill), stroke: Some(Stroke::new(0.6, edge)) });
                        }
                        out.push(Item::Line { points: vec![pt(xa, yc), pt(xb, yc)], stroke: Stroke::dashed(0.8, edge, 1.5) });
                    }
                    None => out.push(Item::Rect { rect: bar, fill: Some(fill), stroke: Some(Stroke::new(0.6, edge)) }),
                }
                if manual {
                    for (x, dir) in [(bar.x, 1.0), (bar.right(), -1.0)] {
                        out.push(Item::Line {
                            points: vec![pt(x + 2.0 * dir, bar.y - 1.5), pt(x, bar.y - 1.5), pt(x, bar.bottom() + 1.5)],
                            stroke: Stroke::new(1.2, edge),
                        });
                    }
                }
                if o.progress && t.sched.percent_complete > 0.0 {
                    let px = match split {
                        Some((xa, _)) => xa,
                        None => x0 + (x1 - x0) * t.sched.percent_complete.clamp(0.0, 100.0) / 100.0,
                    };
                    out.push(Item::Rect { rect: Rect::new(x0, yc - 1.0, px - x0, 2.0), fill: Some(pal.bar_progress), stroke: None });
                }
                if o.slack && t.sched.total_slack > 0 {
                    let lf = task_calendar(p, idx).add(f, t.sched.total_slack);
                    hline(out, x1, xs(lf), yc + BAR_H / 2.0 + 1.5, Stroke::new(1.0, pal.bar_slack));
                }
                if o.resource_names {
                    let names = p.format_resource_names(t.id);
                    if !names.is_empty() {
                        out.push(text(pt(x1 + 4.0, yc + 2.5), SMALL, pal.text, false, names));
                    }
                }
            }
            if let Some(dl) = t.deadline {
                let xd = xs(dl);
                out.push(Item::Polygon { points: vec![pt(xd, yc - 4.5), pt(xd + 3.5, yc - 4.5), pt(xd, yc - 1.0)], fill: pal.deadline });
                vline(out, xd, yc - 4.5, yc + 4.5, Stroke::new(0.6, pal.deadline));
                if f > dl {
                    out.push(text(pt(x1 + 2.0, yc - 4.0), SMALL, pal.error, true, "!"));
                }
            }
        }
    }
}

/// The table columns of `strip` at the page's left edge. Returns the width used.
fn draw_table(out: &mut Vec<Item>, job: &PrintJob, widths: &[f32], cols: &[usize], top: f32, rows: std::ops::Range<usize>) -> f32 {
    let pal = &job.palette;
    let n = rows.len() as f32;
    let mut x = 0.0;
    let grid = Stroke::new(0.4, pal.grid);
    let first_row = rows.start;
    for &c in cols {
        let (Some(col), Some(&w)) = (job.sheet.columns.get(c), widths.get(c)) else { continue };
        let head = Rect::new(x, top, w, HEAD_H);
        out.push(Item::Rect { rect: head, fill: Some(pal.header_bg), stroke: Some(Stroke::new(0.5, pal.grid_strong)) });
        boxed_text(out, head, &col.title, SMALL, pal.text, true, Align::Center);
        for r in rows.clone() {
            let Some(row) = job.sheet.rows.get(r) else { continue };
            let y = top + HEAD_H + (r - first_row) as f32 * ROW_H;
            let cell_rect = Rect::new(x, y, w, ROW_H);
            if matches!(row.kind, RowKind::Group { .. }) {
                out.push(Item::Rect { rect: cell_rect, fill: Some(pal.header_bg), stroke: None });
            }
            if let Some(cell) = row.cells.get(c) {
                let indent = if cell.indent.is_finite() { cell.indent.clamp(0.0, w / 2.0) } else { 0.0 };
                let r = Rect::new(x + indent, y, w - indent, ROW_H);
                boxed_text(out, r, &cell.text, TEXT, cell.color.unwrap_or(pal.text), cell.bold, col.align);
            }
            hline(out, x, x + w, y + ROW_H, grid);
        }
        vline(out, x + w, top, top + HEAD_H + n * ROW_H, grid);
        x += w;
    }
    if x > 0.0 {
        vline(out, 0.0, top, top + HEAD_H + n * ROW_H, Stroke::new(0.5, pal.grid_strong));
    }
    x
}

fn legend(out: &mut Vec<Item>, job: &PrintJob, opts: &ChartOptions, r: Rect) {
    let pal = &job.palette;
    out.push(Item::Rect { rect: r, fill: None, stroke: Some(Stroke::new(0.6, pal.grid_strong)) });
    let left_w = (r.w * 0.28).min(200.0);
    let p = job.project;
    boxed_text(out, Rect::new(r.x, r.y + 4.0, left_w, 14.0), &format!("Project: {}", p.name), TEXT, pal.text, true, Align::Left);
    boxed_text(
        out,
        Rect::new(r.x, r.y + 20.0, left_w, 14.0),
        &format!("Date: {}", job.printed.format("%a %m/%d/%y")),
        TEXT,
        pal.text,
        false,
        Align::Left,
    );
    vline(out, r.x + left_w, r.y, r.bottom(), Stroke::new(0.6, pal.grid_strong));
    type Swatch = fn(&mut Vec<Item>, Pt, &Palette);
    let mut entries: Vec<(&str, Swatch)> = vec![
        ("Task", |o, c, p| {
            o.push(Item::Rect {
                rect: Rect::new(c.x, c.y - 3.5, 22.0, 7.0),
                fill: Some(p.bar_task),
                stroke: Some(Stroke::new(0.6, p.bar_task_edge)),
            })
        }),
        ("Milestone", |o, c, p| diamond(o, pt(c.x + 11.0, c.y), 4.5, p.milestone)),
        ("Summary", |o, c, p| {
            o.push(Item::Rect { rect: Rect::new(c.x, c.y - 2.5, 22.0, 3.0), fill: Some(p.bar_summary), stroke: None });
            for x in [c.x, c.x + 22.0] {
                o.push(Item::Polygon {
                    points: vec![pt(x - 3.0, c.y + 0.5), pt(x + 3.0, c.y + 0.5), pt(x, c.y + 4.0)],
                    fill: p.bar_summary,
                });
            }
        }),
        ("Manual Task", |o, c, p| {
            o.push(Item::Rect {
                rect: Rect::new(c.x, c.y - 3.5, 22.0, 7.0),
                fill: Some(p.bar_manual),
                stroke: Some(Stroke::new(0.6, p.bar_task_edge)),
            })
        }),
        ("Inactive Task", |o, c, p| {
            o.push(Item::Rect {
                rect: Rect::new(c.x, c.y - 3.5, 22.0, 7.0),
                fill: Some(p.bar_inactive),
                stroke: Some(Stroke::new(0.6, p.bar_baseline)),
            })
        }),
        ("Deadline", |o, c, p| {
            o.push(Item::Polygon {
                points: vec![pt(c.x + 11.0, c.y - 4.5), pt(c.x + 14.5, c.y - 4.5), pt(c.x + 11.0, c.y - 1.0)],
                fill: p.deadline,
            });
            vline(o, c.x + 11.0, c.y - 4.5, c.y + 4.5, Stroke::new(0.6, p.deadline));
        }),
    ];
    if opts.critical {
        entries.insert(
            1,
            ("Critical Task", |o, c, p| {
                o.push(Item::Rect {
                    rect: Rect::new(c.x, c.y - 3.5, 22.0, 7.0),
                    fill: Some(p.bar_critical),
                    stroke: Some(Stroke::new(0.6, p.bar_critical_edge)),
                })
            }),
        );
    }
    if opts.progress {
        entries.push(("Progress", |o, c, p| {
            o.push(Item::Rect { rect: Rect::new(c.x, c.y - 1.0, 22.0, 2.0), fill: Some(p.bar_progress), stroke: None })
        }));
    }
    if opts.baseline {
        entries.push(("Baseline", |o, c, p| {
            o.push(Item::Rect { rect: Rect::new(c.x, c.y - 1.5, 22.0, 3.0), fill: Some(p.bar_baseline), stroke: None })
        }));
    }
    if opts.slack {
        entries.push(("Slack", |o, c, p| hline(o, c.x, c.x + 22.0, c.y, Stroke::new(1.0, p.bar_slack))));
    }
    let per_col = 2;
    let cols = entries.len().div_ceil(per_col).max(1);
    let col_w = ((r.w - left_w - 8.0) / cols as f32).max(1.0);
    for (i, (label, draw)) in entries.iter().enumerate() {
        let (cx, cy) = (r.x + left_w + 8.0 + (i / per_col) as f32 * col_w, r.y + 13.0 + (i % per_col) as f32 * 20.0);
        draw(out, pt(cx, cy), pal);
        boxed_text(out, Rect::new(cx + 26.0, cy - 7.0, (col_w - 28.0).max(0.0), 14.0), label, SMALL, pal.text, false, Align::Left);
    }
}

fn scale_item(item: &mut Item, s: f32, dx: f32, dy: f32) {
    let tp = |p: &mut Pt| {
        p.x = dx + p.x * s;
        p.y = dy + p.y * s;
    };
    let tr = |r: &mut Rect| {
        r.x = dx + r.x * s;
        r.y = dy + r.y * s;
        r.w *= s;
        r.h *= s;
    };
    match item {
        Item::Rect { rect, stroke, .. } => {
            tr(rect);
            if let Some(st) = stroke {
                st.width *= s;
                st.dash = st.dash.map(|d| d * s);
            }
        }
        Item::Line { points, stroke } => {
            points.iter_mut().for_each(tp);
            stroke.width *= s;
            stroke.dash = stroke.dash.map(|d| d * s);
        }
        Item::Polygon { points, .. } => points.iter_mut().for_each(tp),
        Item::Text { at, size, .. } => {
            tp(at);
            *size *= s;
        }
        Item::Clip { rect, items } => {
            tr(rect);
            for i in items {
                scale_item(i, s, dx, dy);
            }
        }
    }
}

/// Lays out the sheet (and chart) on pages.
pub fn layout(job: &PrintJob) -> Document {
    let setup = job.setup;
    let (pw, ph) = setup.page_size();
    let margin = setup.margin_pt().min(pw / 4.0).min(ph / 4.0);
    // Printable area in virtual points. A scale too large for the paper and margins is reduced
    // so a row, the timescale and the bands still fit, rather than printing off the page.
    let (aw, ah) = ((pw - 2.0 * margin).max(1.0), (ph - 2.0 * margin).max(1.0));
    let s = setup.scale().min(aw / MIN_AREA_W).min(ah / MIN_AREA_H).max(0.001);
    let (w, h) = (aw / s, ah / s);
    let has_header = setup.header.iter().any(|t| !t.trim().is_empty());
    let has_footer = setup.footer.iter().any(|t| !t.trim().is_empty());
    let chart_opts = job.chart;
    let legend_every = chart_opts.is_some() && setup.legend == LegendPlacement::EveryPage;
    let top = if has_header { BAND_H } else { 0.0 };
    let bottom_bands = if has_footer { BAND_H } else { 0.0 } + if legend_every { LEGEND_H + 4.0 } else { 0.0 };
    let rows_per_page = (((h - top - bottom_bands - HEAD_H) / ROW_H).floor().max(1.0)) as usize;

    // Column widths fit on a page.
    let widths: Vec<f32> =
        job.sheet.columns.iter().map(|c| if c.width.is_finite() { c.width.clamp(12.0, w - 20.0) } else { 60.0 }).collect();
    let repeat: Vec<usize> = (0..usize::from(setup.repeat_columns).min(widths.len())).collect();
    let mut repeat_w: f32 = repeat.iter().filter_map(|&c| widths.get(c)).sum();
    let repeat = if repeat_w > w / 2.0 {
        repeat_w = 0.0;
        Vec::new()
    } else {
        repeat
    };
    // Every other column fits beside the repeated ones on a page of its own.
    let widths: Vec<f32> =
        widths.iter().enumerate().map(|(c, &cw)| if repeat.contains(&c) { cw } else { cw.min((w - repeat_w).max(12.0)) }).collect();

    // Strips across: table columns first, then the timescale.
    let mut strips: Vec<Strip> = Vec::new();
    let mut cur = Strip { cols: Vec::new(), chart: None };
    let mut x = 0.0;
    for (c, &cw) in widths.iter().enumerate() {
        if x + cw > w + 0.01 && !cur.cols.is_empty() {
            strips.push(std::mem::replace(&mut cur, Strip { cols: repeat.clone(), chart: None }));
            x = repeat_w;
        }
        if !cur.cols.contains(&c) {
            cur.cols.push(c);
            x += cw;
        }
    }
    let mut chart: Option<Chart> = None;
    let mut strips_cut = false;
    if let Some(opts) = chart_opts {
        let (start, end) = date_range(job);
        let days = ((end - start).num_minutes() as f32 / 1440.0).max(1.0);
        let mut first_w = w - x;
        if first_w < MIN_CHART_W && !cur.cols.is_empty() {
            strips.push(std::mem::replace(&mut cur, Strip { cols: repeat.clone(), chart: None }));
            x = repeat_w;
            first_w = w - x;
        }
        let later_w = (w - repeat_w).max(1.0);
        let per_day = match setup.timescale {
            TimescaleFit::PagesWide(n) => (first_w + f32::from(n.clamp(1, super::MAX_PAGES_WIDE).saturating_sub(1)) * later_w) / days,
            TimescaleFit::AsShown => opts.per_day,
        };
        let per_day = if per_day.is_finite() { per_day.clamp(0.01, 2000.0) } else { 1.0 };
        let total = days * per_day;
        cur.chart = Some((x, first_w, 0.0));
        strips.push(cur);
        let mut done = first_w;
        while done < total - 0.5 && strips.len() < MAX_PAGES {
            strips.push(Strip { cols: repeat.clone(), chart: Some((repeat_w, later_w, done)) });
            done += later_w;
        }
        let p = job.project;
        let index_of: HashMap<TaskId, usize> = p.tasks.iter().enumerate().map(|(i, t)| (t.id, i)).collect();
        let rows_of: HashMap<TaskId, usize> = job
            .sheet
            .rows
            .iter()
            .enumerate()
            .filter_map(|(i, r)| if let RowKind::Task(id) = r.kind { Some((id, i)) } else { None })
            .collect();
        let mut by_succ = Vec::new();
        for (&id, &succ_row) in &rows_of {
            let Some(&succ) = index_of.get(&id) else { continue };
            let Some(t) = p.tasks.get(succ) else { continue };
            for (dep, d) in t.predecessors.iter().enumerate() {
                let Some(pid) = d.predecessor.local() else { continue };
                if let (Some(&pred_row), Some(&pred)) = (rows_of.get(&pid), index_of.get(&pid)) {
                    by_succ.push(LinkRef { succ_row, pred_row, succ, pred, dep });
                }
            }
        }
        by_succ.sort_by_key(|l| (l.succ_row, l.dep));
        let mut by_pred = by_succ.clone();
        by_pred.sort_by_key(|l| (l.pred_row, l.succ_row, l.dep));
        // The timescale was cut at the page limit.
        strips_cut = done < total - 0.5;
        chart = Some(Chart { job, opts, start, per_day, index_of, by_succ, by_pred });
    } else if !cur.cols.is_empty() || strips.is_empty() {
        strips.push(cur);
    }

    // Pages: across, then down.
    let row_count = job.sheet.rows.len();
    let chunks: Vec<std::ops::Range<usize>> = if row_count == 0 {
        std::iter::once(0..0).collect()
    } else {
        (0..row_count.div_ceil(rows_per_page)).map(|i| i * rows_per_page..((i + 1) * rows_per_page).min(row_count)).collect()
    };
    let mut pages: Vec<Vec<Item>> = Vec::new();
    let mut truncated = strips_cut;
    // Pages looked at, printed or skipped as blank: skipping is cheap but not free.
    let mut visits = 0usize;
    'outer: for rows in &chunks {
        for strip in &strips {
            visits = visits.saturating_add(1);
            if visits > MAX_PAGE_VISITS {
                truncated = true;
                break 'outer;
            }
            if let (Some(c), Some((_, cw, off))) = (&chart, strip.chart)
                && strip.cols.iter().all(|col| repeat.contains(col))
                && !setup.print_blank_pages
                && !c.has_bars(rows.clone(), off, cw)
            {
                continue;
            }
            if pages.len() >= MAX_PAGES {
                truncated = true;
                break 'outer;
            }
            let mut items = Vec::new();
            let table_w = draw_table(&mut items, job, &widths, &strip.cols, top, rows.clone());
            if let (Some(c), Some((cx, cw, off))) = (&chart, strip.chart) {
                let body_h = rows.len() as f32 * ROW_H;
                let head = Rect::new(cx.max(table_w), top, cw, HEAD_H);
                let body = Rect::new(head.x, top + HEAD_H, cw, body_h.max(ROW_H));
                let mut scale = Vec::new();
                c.timescale(&mut scale, head, off);
                items.push(Item::Clip { rect: head, items: scale });
                // Bars and links are clipped to the body: a link from a row on an earlier page
                // must not cross the timescale.
                let mut clipped = Vec::new();
                c.background(&mut clipped, body, off);
                for r in rows.clone() {
                    let y = body.y + (r - rows.start) as f32 * ROW_H + ROW_H;
                    hline(&mut clipped, body.x, body.right(), y, Stroke::new(0.3, job.palette.chart_grid));
                }
                c.bars(&mut clipped, body, off, rows.clone());
                items.push(Item::Clip { rect: body, items: clipped });
                items.push(Item::Rect {
                    rect: Rect::new(head.x, head.y, cw, HEAD_H + body.h),
                    fill: None,
                    stroke: Some(Stroke::new(0.5, job.palette.grid_strong)),
                });
            }
            if legend_every && let Some(opts) = &chart_opts {
                let y = h - bottom_bands;
                legend(&mut items, job, opts, Rect::new(0.0, y + 4.0, w, LEGEND_H));
            }
            pages.push(items);
        }
    }
    if setup.legend == LegendPlacement::LegendPage
        && let Some(opts) = &chart_opts
        && !truncated
    {
        if pages.len() < MAX_PAGES {
            let mut items = Vec::new();
            legend(&mut items, job, opts, Rect::new(0.0, top + 4.0, w, LEGEND_H));
            pages.push(items);
        } else {
            truncated = true;
        }
    }
    // Never a document without pages (some PDF readers refuse one).
    if pages.is_empty() {
        pages.push(Vec::new());
    }

    // Headers and footers (now that the page count is known), then scale into the margins.
    let total = pages.len();
    let pal = job.palette;
    let mut out = Vec::with_capacity(total);
    for (i, mut items) in pages.into_iter().enumerate() {
        for (texts, y) in [(&setup.header, 0.0), (&setup.footer, h - BAND_H)] {
            for (k, t) in texts.iter().enumerate() {
                let filled = fill_codes(t, job, i + 1, total);
                if filled.trim().is_empty() {
                    continue;
                }
                let align = [Align::Left, Align::Center, Align::Right].get(k).copied().unwrap_or_default();
                let third = w / 3.0;
                // Centre text may use the full width; side texts their third.
                let r = if k == 1 { Rect::new(third / 2.0, y, w - third, BAND_H) } else { Rect::new(k as f32 * third, y, third, BAND_H) };
                boxed_text(&mut items, r, &filled, TEXT, if k == 1 { pal.text } else { pal.text_weak }, k == 1 && y == 0.0, align);
            }
        }
        for item in &mut items {
            scale_item(item, s, margin, margin);
        }
        out.push(Page { width: pw, height: ph, items });
    }
    Document { title: format!("{} – {}", job.project.name, job.sheet.title), pages: out, truncated }
}

/// The default printed task table: what agents print with `export_pdf`.
pub fn task_sheet(p: &Project, with_chart: bool) -> Sheet {
    let day = |d: Option<NaiveDateTime>| d.map(|d| d.format("%a %m/%d/%y").to_string()).unwrap_or_else(|| "NA".into());
    let mut columns = vec![
        PrintColumn { title: "ID".into(), width: 24.0, align: Align::Right },
        PrintColumn { title: "Task Name".into(), width: if with_chart { 180.0 } else { 220.0 }, align: Align::Left },
        PrintColumn { title: "Duration".into(), width: 52.0, align: Align::Left },
        PrintColumn { title: "Start".into(), width: 66.0, align: Align::Left },
        PrintColumn { title: "Finish".into(), width: 66.0, align: Align::Left },
    ];
    if !with_chart {
        columns.push(PrintColumn { title: "Predecessors".into(), width: 72.0, align: Align::Left });
        columns.push(PrintColumn { title: "Resource Names".into(), width: 120.0, align: Align::Left });
    }
    let rows = p
        .tasks
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let summary = t.sched.is_summary;
            let mut cells = vec![
                PrintCell::text((i + 1).to_string()),
                PrintCell { text: t.name.clone(), bold: summary, color: None, indent: f32::from(t.outline_level.saturating_sub(1)) * 10.0 },
                PrintCell::text(p.format_duration(t.sched.duration, t.estimated && !summary)),
                PrintCell::text(day(t.sched.start)),
                PrintCell::text(day(t.sched.finish)),
            ];
            if !with_chart {
                cells.push(PrintCell::text(p.format_predecessors(t.id)));
                cells.push(PrintCell::text(p.format_resource_names(t.id)));
            }
            for c in cells.iter_mut().skip(2) {
                c.bold = summary;
            }
            PrintRow { kind: RowKind::Task(t.id), cells }
        })
        .collect();
    Sheet { title: if with_chart { "Gantt Chart".into() } else { "Task Sheet".into() }, columns, rows }
}

#[cfg(test)]
mod tests {
    use super::super::Orientation;
    use super::*;
    use crate::sample;

    fn job<'a>(p: &'a Project, setup: &'a PageSetup, sheet: &'a Sheet, chart: bool) -> PrintJob<'a> {
        PrintJob {
            project: p,
            setup,
            sheet,
            chart: chart.then(ChartOptions::default),
            from: None,
            to: None,
            printed: NaiveDate::from_ymd_opt(2026, 11, 12).unwrap().and_hms_opt(9, 0, 0).unwrap(),
            palette: Palette::default(),
        }
    }

    fn texts(page: &Page) -> Vec<String> {
        fn walk(items: &[Item], out: &mut Vec<String>) {
            for i in items {
                match i {
                    Item::Text { text, .. } => out.push(text.clone()),
                    Item::Clip { items, .. } => walk(items, out),
                    _ => {}
                }
            }
        }
        let mut out = Vec::new();
        walk(&page.items, &mut out);
        out
    }

    #[test]
    fn a_gantt_chart_fits_one_page_wide_with_headers_and_legend() {
        let p = sample::demo_project(NaiveDate::from_ymd_opt(2026, 11, 2).unwrap());
        let setup = PageSetup::default();
        let sheet = task_sheet(&p, true);
        let doc = layout(&job(&p, &setup, &sheet, true));
        assert!(!doc.pages.is_empty());
        let pages_down = sheet.rows.len().div_ceil(doc.pages.len().max(1));
        assert!(pages_down >= 1);
        let first = texts(&doc.pages[0]);
        assert!(first.iter().any(|t| t == &p.name), "header shows the project name: {first:?}");
        assert!(first.iter().any(|t| t == &format!("Page 1 of {}", doc.pages.len())));
        assert!(first.iter().any(|t| t == "Task Name"));
        assert!(first.iter().any(|t| t == "Milestone"), "legend");
        for page in &doc.pages {
            assert_eq!((page.width, page.height), (792.0, 612.0));
        }
    }

    /// Every point an item draws at (a clip counts as its rectangle: what it holds is cut to it).
    fn points(items: &[Item], out: &mut Vec<(f32, f32)>) {
        for i in items {
            match i {
                Item::Rect { rect, .. } => out.extend([(rect.x, rect.y), (rect.right(), rect.bottom())]),
                Item::Line { points: ps, .. } | Item::Polygon { points: ps, .. } => out.extend(ps.iter().map(|q| (q.x, q.y))),
                Item::Text { at, .. } => out.push((at.x, at.y)),
                Item::Clip { rect, .. } => out.extend([(rect.x, rect.y), (rect.right(), rect.bottom())]),
            }
        }
    }

    /// Regression: header and footer codes were replaced one after another, so a value that
    /// contained a code was expanded again; nested names could grow a header to gigabytes.
    #[test]
    fn header_codes_are_filled_in_once() {
        let mut p = Project::new("[Manager] plan", NaiveDate::from_ymd_opt(2026, 10, 19).unwrap());
        p.manager = "[Company]".repeat(50);
        p.company = "x".repeat(1000);
        let setup =
            PageSetup { header: ["[Project]".repeat(20), "[Page] of [Pages] [Unknown] [Page".into(), String::new()], ..Default::default() };
        let sheet = task_sheet(&p, false);
        let j = job(&p, &setup, &sheet, false);
        assert_eq!(fill_codes("[Project]", &j, 1, 3), "[Manager] plan");
        assert_eq!(fill_codes("[Page] of [Pages] [Unknown] [Page", &j, 1, 3), "1 of 3 [Unknown] [Page");
        assert_eq!(fill_codes("[Manager]", &j, 1, 1).len(), 50 * "[Company]".len(), "not expanded again");
        assert_eq!(layout(&j).pages.len(), 1);
    }

    /// Regression: when the timescale was cut at the page limit the document did not say it was
    /// cut, and a legend page could make it 2,001 pages.
    #[test]
    fn the_page_limit_is_kept_and_reported() {
        let mut p = Project::new("Long", NaiveDate::from_ymd_opt(2026, 10, 19).unwrap());
        p.add_task("A", None);
        crate::schedule::recalculate(&mut p);
        let setup = PageSetup { timescale: TimescaleFit::AsShown, legend: LegendPlacement::LegendPage, ..Default::default() };
        let sheet = task_sheet(&p, true);
        let mut j = job(&p, &setup, &sheet, true);
        j.chart = Some(ChartOptions { per_day: 2000.0, ..Default::default() });
        j.from = NaiveDate::from_ymd_opt(1950, 1, 1);
        j.to = NaiveDate::from_ymd_opt(2250, 1, 1);
        let doc = layout(&j);
        assert!(doc.pages.len() <= MAX_PAGES, "{}", doc.pages.len());
        assert!(doc.truncated, "the cut timescale is reported");
    }

    /// Regression: a wide column next to repeated columns, or a scale too big for the paper and
    /// margins, printed past the edge of the paper; a sheet with nothing to print made a PDF
    /// without pages.
    #[test]
    fn everything_prints_inside_the_paper() {
        let p = sample::demo_project(NaiveDate::from_ymd_opt(2026, 11, 2).unwrap());
        let mut sheet = task_sheet(&p, false);
        sheet.columns.push(PrintColumn { title: "Notes".into(), width: 600.0, align: Align::Left });
        for row in &mut sheet.rows {
            row.cells.push(PrintCell { text: "n".repeat(300), ..Default::default() });
        }
        for setup in [
            PageSetup { repeat_columns: 2, ..Default::default() },
            PageSetup { margin_mm: 50.0, scale_percent: 400, orientation: Orientation::Portrait, ..Default::default() },
        ] {
            for chart in [false, true] {
                let doc = layout(&job(&p, &setup, &sheet, chart));
                for page in &doc.pages {
                    let mut pts = Vec::new();
                    points(&page.items, &mut pts);
                    for (x, y) in pts {
                        assert!(
                            (-0.5..=page.width + 0.5).contains(&x) && (-0.5..=page.height + 0.5).contains(&y),
                            "({x}, {y}) is off the page"
                        );
                    }
                }
            }
        }
        let empty = Sheet { title: "Empty".into(), columns: Vec::new(), rows: Vec::new() };
        let setup = PageSetup { print_blank_pages: false, ..Default::default() };
        assert_eq!(layout(&job(&p, &setup, &empty, true)).pages.len(), 1);
    }

    /// Regression: links from a row on an earlier page were drawn across the timescale.
    #[test]
    fn bars_and_links_are_clipped_below_the_timescale() {
        let p = sample::demo_project(NaiveDate::from_ymd_opt(2026, 11, 2).unwrap());
        let sheet = task_sheet(&p, true);
        let setup = PageSetup::default();
        let doc = layout(&job(&p, &setup, &sheet, true));
        let clips: Vec<Rect> =
            doc.pages[0].items.iter().filter_map(|i| if let Item::Clip { rect, .. } = i { Some(*rect) } else { None }).collect();
        assert_eq!(clips.len(), 2, "timescale and chart body are clipped apart");
        assert!(clips[1].y >= clips[0].bottom() - 0.01);
    }

    #[test]
    fn split_tasks_print_as_two_bars_joined_by_dots() {
        fn dotted_gaps(items: &[Item]) -> usize {
            items
                .iter()
                .map(|i| match i {
                    Item::Line { points, stroke } if stroke.dash.is_some() && points.len() == 2 && points[0].y == points[1].y => 1,
                    Item::Clip { items, .. } => dotted_gaps(items),
                    _ => 0,
                })
                .sum()
        }
        let mut p = Project::new("Split", NaiveDate::from_ymd_opt(2026, 10, 19).unwrap());
        let t = p.add_task("Build", None);
        p.set_duration(t, 4 * 480, false).unwrap();
        crate::schedule::recalculate(&mut p);
        let setup = PageSetup::default();
        let count = |p: &Project| {
            let sheet = task_sheet(p, true);
            layout(&job(p, &setup, &sheet, true)).pages.iter().map(|pg| dotted_gaps(&pg.items)).sum::<usize>()
        };
        let none = count(&p);
        p.set_percent_complete(t, 25).unwrap();
        p.set_resume(t, NaiveDate::from_ymd_opt(2026, 10, 26).and_then(|d| d.and_hms_opt(8, 0, 0))).unwrap();
        crate::schedule::recalculate(&mut p);
        assert_eq!(count(&p), none + 1, "one dotted gap between the done and remaining parts");
    }

    #[test]
    fn rows_continue_on_following_pages_in_order() {
        let p = sample::demo_project(NaiveDate::from_ymd_opt(2026, 11, 2).unwrap());
        let setup = PageSetup { orientation: Orientation::Landscape, scale_percent: 400, ..Default::default() };
        let sheet = task_sheet(&p, false);
        let doc = layout(&job(&p, &setup, &sheet, false));
        assert!(doc.pages.len() > 1);
        let mut seen = Vec::new();
        for page in &doc.pages {
            for t in texts(page) {
                if let Some(t) = p.tasks.iter().find(|x| x.name == t) {
                    seen.push(t.id);
                }
            }
        }
        // Long names are shortened, so only check that what is seen is in outline order.
        let order: Vec<usize> = seen.iter().filter_map(|id| p.task_index(*id)).collect();
        assert!(order.windows(2).all(|w| w[0] <= w[1]), "{order:?}");
    }

    #[test]
    fn more_pages_wide_spread_the_timescale() {
        let p = sample::demo_project(NaiveDate::from_ymd_opt(2026, 11, 2).unwrap());
        let sheet = task_sheet(&p, true);
        let one = PageSetup::default();
        let three = PageSetup { timescale: TimescaleFit::PagesWide(3), ..Default::default() };
        let a = layout(&job(&p, &one, &sheet, true)).pages.len();
        let b = layout(&job(&p, &three, &sheet, true)).pages.len();
        assert_eq!(b, a * 3);
    }

    #[test]
    fn blank_pages_can_be_skipped() {
        let p = sample::demo_project(NaiveDate::from_ymd_opt(2026, 11, 2).unwrap());
        let sheet = task_sheet(&p, true);
        let mut setup = PageSetup { timescale: TimescaleFit::PagesWide(MAX_PAGES_WIDE_TEST), ..Default::default() };
        // Print a year past the end of the plan: the pages after the last bar are blank.
        let to = p.sched.finish.map(|f| f.date()).and_then(|d| d.checked_add_days(chrono::Days::new(365)));
        let all = layout(&PrintJob { to, ..job(&p, &setup, &sheet, true) }).pages.len();
        setup.print_blank_pages = false;
        let some = layout(&PrintJob { to, ..job(&p, &setup, &sheet, true) }).pages.len();
        assert!(some < all, "{some} < {all}");
    }
    const MAX_PAGES_WIDE_TEST: u8 = 12;

    #[test]
    fn hostile_input_never_panics_and_stays_bounded() {
        let mut p = sample::demo_project(NaiveDate::from_ymd_opt(2026, 11, 2).unwrap());
        p.name = "x".repeat(10_000);
        let sheet = Sheet {
            title: String::new(),
            columns: vec![
                PrintColumn { title: "W".into(), width: f32::NAN, align: Align::Right },
                PrintColumn { title: String::new(), width: 1e9, align: Align::Center },
            ],
            rows: (0..50)
                .map(|i| PrintRow {
                    kind: RowKind::Task(TaskId(i)),
                    cells: vec![PrintCell { text: "y".repeat(500), bold: true, color: None, indent: f32::INFINITY }],
                })
                .collect(),
        };
        let setup = PageSetup { margin_mm: 1e9, scale_percent: u16::MAX, timescale: TimescaleFit::AsShown, ..Default::default() };
        let mut j = job(&p, &setup, &sheet, true);
        j.chart = Some(ChartOptions { per_day: f32::INFINITY, critical: true, baseline: true, slack: true, ..Default::default() });
        j.from = NaiveDate::from_ymd_opt(1900, 1, 1);
        j.to = NaiveDate::from_ymd_opt(2300, 1, 1);
        let doc = layout(&j);
        assert!(doc.pages.len() <= MAX_PAGES);
        let pdf = super::super::pdf::write(&doc);
        assert!(pdf.starts_with(b"%PDF"));
        // An empty sheet still prints a page.
        let empty = Sheet::default();
        let d = layout(&job(&p, &setup, &empty, false));
        assert_eq!(d.pages.len(), 1);
    }
}
