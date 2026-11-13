//! Recurring tasks: a summary task whose subtasks are the occurrences of a repeating pattern
//! (daily, weekly, monthly or yearly), each pinned to its date with a Start No Earlier Than
//! constraint.
//!
//! Behaviour follows Microsoft's published description of recurring tasks: a duration for each
//! occurrence (0 makes them milestones), the four patterns, a range ending after a number of
//! occurrences or by a date, an optional calendar, and resources assigned to the recurring task
//! going to every occurrence. Where the pages say nothing (an occurrence on a non-working day,
//! days past a month's end), the choices are documented on the functions below.

use chrono::{Datelike, Days, Months, NaiveDate, Weekday};
use serde::{Deserialize, Serialize};

use crate::calendar::WorkCal;
use crate::duration::Minutes;
use crate::ids::{CalendarId, TaskId};
use crate::limits::{MAX_MINUTES, clamp_date};
use crate::model::{Constraint, ConstraintKind, Project, ScheduleMode, Task, TaskKind};
use crate::ops::{PlanError, PlanResult};

/// Most occurrences one recurring task can have.
pub const MAX_OCCURRENCES: u16 = 999;
/// Largest "every N days/weeks/months".
pub const MAX_INTERVAL: u16 = 999;

/// Which occurrence of a weekday in a month.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Nth {
    First,
    Second,
    Third,
    Fourth,
    Last,
}

impl Nth {
    pub const ALL: [Nth; 5] = [Nth::First, Nth::Second, Nth::Third, Nth::Fourth, Nth::Last];

    pub fn label(self) -> &'static str {
        match self {
            Nth::First => "first",
            Nth::Second => "second",
            Nth::Third => "third",
            Nth::Fourth => "fourth",
            Nth::Last => "last",
        }
    }
}

/// The day a monthly or yearly occurrence falls on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MonthDay {
    /// Day 1–31; past the end of a shorter month it means the month's last day.
    Day(u8),
    /// The first … fourth or last of a weekday (0 = Monday … 6 = Sunday).
    Weekday { nth: Nth, weekday: u8 },
}

/// How often the task repeats.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Pattern {
    /// Every `interval` days, or every `interval` working days when `workdays`.
    Daily { interval: u16, workdays: bool },
    /// Every `interval` weeks on the given days (bit 0 = Monday … bit 6 = Sunday).
    Weekly { interval: u16, days: u8 },
    /// Every `interval` months on the given day.
    Monthly { interval: u16, on: MonthDay },
    /// Every year in `month` (1–12) on the given day.
    Yearly { month: u8, on: MonthDay },
}

/// When the series ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RecurEnd {
    /// After this many occurrences.
    After(u16),
    /// On or before this date.
    By(NaiveDate),
}

/// A recurring task's definition (kept on its summary task).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Recurrence {
    pub pattern: Pattern,
    /// First day the pattern may fall on.
    pub start: NaiveDate,
    pub end: RecurEnd,
    /// Duration of each occurrence (0: a milestone).
    pub duration: Minutes,
    /// The occurrences' calendar (`None`: the project calendar).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub calendar: Option<CalendarId>,
    /// Schedule the occurrences even when their resources are not working.
    #[serde(default)]
    pub ignore_resource_calendars: bool,
}

fn invalid<T>(msg: impl Into<String>) -> PlanResult<T> {
    Err(PlanError::Invalid(msg.into()))
}

fn weekday(n: u8) -> Option<Weekday> {
    Weekday::try_from(n).ok()
}

/// The date of `on` in the month of `first` (the month's first day), or `None` when out of range.
fn in_month(first: NaiveDate, on: MonthDay) -> Option<NaiveDate> {
    let last = first.checked_add_months(Months::new(1))?.pred_opt()?;
    match on {
        MonthDay::Day(d) => first.with_day(u32::from(d).clamp(1, last.day())),
        MonthDay::Weekday { nth, weekday: w } => {
            let w = weekday(w)?;
            match nth {
                Nth::Last => {
                    let back = (7 + last.weekday().num_days_from_monday() - w.num_days_from_monday()) % 7;
                    last.checked_sub_days(Days::new(u64::from(back)))
                }
                n => {
                    let k = match n {
                        Nth::First => 0,
                        Nth::Second => 1,
                        Nth::Third => 2,
                        _ => 3,
                    };
                    let ahead = (7 + w.num_days_from_monday() - first.weekday().num_days_from_monday()) % 7;
                    first.checked_add_days(Days::new(u64::from(ahead + 7 * k)))
                }
            }
        }
    }
}

impl Recurrence {
    /// Checks the definition's numbers (intervals, days, months, the range).
    pub fn validate(&self) -> PlanResult<()> {
        let interval_ok = |i: u16| (1..=MAX_INTERVAL).contains(&i);
        let day_ok = |on: MonthDay| match on {
            MonthDay::Day(d) => (1..=31).contains(&d),
            MonthDay::Weekday { weekday: w, .. } => w < 7,
        };
        let ok = match self.pattern {
            Pattern::Daily { interval, .. } => interval_ok(interval),
            Pattern::Weekly { interval, days } => interval_ok(interval) && days != 0 && days < 0x80,
            Pattern::Monthly { interval, on } => interval_ok(interval) && day_ok(on),
            Pattern::Yearly { month, on } => (1..=12).contains(&month) && day_ok(on),
        };
        if !ok {
            return invalid(format!(
                "a recurrence repeats every 1 to {MAX_INTERVAL} days, weeks or months, on at least one weekday, on day 1 to 31 of month 1 to 12"
            ));
        }
        match self.end {
            RecurEnd::After(n) if !(1..=MAX_OCCURRENCES).contains(&n) => {
                return invalid(format!("a recurring task has 1 to {MAX_OCCURRENCES} occurrences"));
            }
            RecurEnd::By(d) if d < self.start => return invalid("the recurrence ends before it starts"),
            _ => {}
        }
        if clamp_date(self.start) != self.start {
            return invalid("the recurrence starts outside 1900–2300");
        }
        if !(0..=MAX_MINUTES).contains(&self.duration) {
            return invalid("an occurrence's duration can be at most 5 years");
        }
        Ok(())
    }

    /// The occurrence dates, in order: the days the pattern falls on, each moved off non-working
    /// days of `cal` to the next working day; one that lands on the same day as the previous one
    /// doesn't count.
    fn pattern_dates(&self, cal: &WorkCal) -> PlanResult<Vec<NaiveDate>> {
        let limit = match self.end {
            RecurEnd::After(n) => usize::from(n.min(MAX_OCCURRENCES)),
            RecurEnd::By(_) => usize::from(MAX_OCCURRENCES),
        };
        let last_day = match self.end {
            RecurEnd::By(d) => clamp_date(d),
            RecurEnd::After(_) => clamp_date(NaiveDate::MAX),
        };
        let mut out: Vec<NaiveDate> = Vec::new();
        let mut problem: Option<String> = None;
        let mut push = |d: NaiveDate| -> bool {
            if d > last_day {
                return false;
            }
            if d < self.start {
                return true;
            }
            let mut w = d;
            for _ in 0..366 {
                if cal.is_working_day(w) {
                    break;
                }
                let Some(next) = w.succ_opt() else { break };
                w = next;
            }
            if !cal.is_working_day(w) {
                problem = Some("the calendar has no working days for the occurrences".into());
                return false;
            }
            if out.last().is_some_and(|&prev| w <= prev) {
                return true;
            }
            if out.len() >= limit {
                if matches!(self.end, RecurEnd::By(_)) {
                    problem = Some(format!("that range has more than {MAX_OCCURRENCES} occurrences; choose an earlier end date"));
                }
                return false;
            }
            out.push(w);
            true
        };
        let start = self.start;
        match self.pattern {
            Pattern::Daily { interval, workdays: false } => {
                let mut d = start;
                while push(d) {
                    let Some(next) = d.checked_add_days(Days::new(u64::from(interval))) else { break };
                    d = next;
                }
            }
            Pattern::Daily { interval, workdays: true } => {
                // Every `interval`-th working day, counting from the first one on or after the start.
                let (mut d, mut count) = (start, 0u16);
                while d <= last_day {
                    if cal.is_working_day(d) {
                        if count % interval == 0 && !push(d) {
                            break;
                        }
                        count = (count + 1) % interval;
                    }
                    let Some(next) = d.succ_opt() else { break };
                    d = next;
                }
            }
            Pattern::Weekly { interval, days } => {
                let back = u64::from(start.weekday().num_days_from_monday());
                let Some(mut monday) = start.checked_sub_days(Days::new(back)) else { return Ok(out) };
                'weeks: loop {
                    for k in 0..7u8 {
                        if days & (1 << k) != 0 {
                            let Some(d) = monday.checked_add_days(Days::new(u64::from(k))) else { break 'weeks };
                            if !push(d) {
                                break 'weeks;
                            }
                        }
                    }
                    let Some(next) = monday.checked_add_days(Days::new(7 * u64::from(interval))) else { break };
                    monday = next;
                }
            }
            Pattern::Monthly { interval, on } => {
                let Some(mut first) = start.with_day(1) else { return Ok(out) };
                while first <= last_day {
                    if let Some(d) = in_month(first, on)
                        && !push(d)
                    {
                        break;
                    }
                    let Some(next) = first.checked_add_months(Months::new(u32::from(interval))) else { break };
                    first = next;
                }
            }
            Pattern::Yearly { month, on } => {
                let Some(mut first) = NaiveDate::from_ymd_opt(start.year(), u32::from(month), 1) else { return Ok(out) };
                while first <= last_day {
                    if let Some(d) = in_month(first, on)
                        && !push(d)
                    {
                        break;
                    }
                    let Some(next) = first.checked_add_months(Months::new(12)) else { break };
                    first = next;
                }
            }
        }
        if let Some(p) = problem {
            return invalid(p);
        }
        if let RecurEnd::After(n) = self.end
            && out.len() < usize::from(n)
        {
            return invalid("the occurrences would run past the year 2300");
        }
        Ok(out)
    }

    /// The occurrence dates. An occurrence that falls on a non-working day of `cal` moves to the
    /// next working day; when that is the day of the previous occurrence it doesn't count, so the
    /// series still has the number of occurrences asked for.
    pub fn dates(&self, cal: &WorkCal) -> PlanResult<Vec<NaiveDate>> {
        self.validate()?;
        let out = self.pattern_dates(cal)?;
        if out.is_empty() {
            return invalid("the pattern has no occurrences in that range");
        }
        Ok(out)
    }
}

impl Project {
    /// The recurring task an occurrence belongs to (its parent, when that is a recurring task).
    pub fn recurring_parent(&self, idx: usize) -> Option<usize> {
        let parent = self.parent_index(idx)?;
        self.tasks.get(parent)?.recurrence.is_some().then_some(parent)
    }

    /// Whether the task at `idx` is a recurring task or one of its occurrences (the Recurring field).
    pub fn is_recurring(&self, idx: usize) -> bool {
        self.tasks.get(idx).is_some_and(|t| t.recurrence.is_some() || t.occurrence)
    }

    /// The occurrences of the recurring task at `row`: its direct subtasks it made (not tasks
    /// added under it by hand), in order.
    fn occurrences_of(&self, row: usize) -> Vec<TaskId> {
        let level = self.tasks.get(row).map_or(1, |t| t.outline_level).saturating_add(1);
        self.descendants_range(row)
            .filter(|&i| !self.has_children(i))
            .filter_map(|i| self.tasks.get(i))
            .filter(|t| t.outline_level == level && t.occurrence && t.recurrence.is_none())
            .map(|t| t.id)
            .collect()
    }

    /// The row a new recurring task goes above when asked for `at`: never inside another
    /// recurring task (above that recurring task instead).
    fn outside_recurring(&self, at: usize) -> usize {
        let mut at = at;
        for _ in 0..crate::limits::MAX_OUTLINE_LEVEL {
            match self.tasks.get(at) {
                Some(t) if t.occurrence => {}
                _ => match self.recurring_parent(at) {
                    Some(_) => {}
                    None => return at,
                },
            }
            match self.parent_index(at) {
                Some(p) => at = p,
                None => return at,
            }
        }
        at
    }

    fn occurrence_dates(&self, rec: &Recurrence) -> PlanResult<Vec<NaiveDate>> {
        if let Some(c) = rec.calendar
            && self.calendar(c).is_none()
        {
            return Err(PlanError::CalendarNotFound(c));
        }
        let cal = rec.calendar.and_then(|c| self.calendar(c)).unwrap_or_else(|| self.base_calendar()).clone();
        rec.dates(&WorkCal::single(&cal))
    }

    /// Sets occurrence `k` (1-based) of a recurring task named `name` to fall on `date`.
    fn shape_occurrence(&self, t: &mut Task, name: &str, k: usize, date: NaiveDate, rec: &Recurrence) {
        let cal = rec.calendar.and_then(|c| self.calendar(c)).unwrap_or_else(|| self.base_calendar());
        t.name = format!("{name} {k}");
        t.kind = if rec.duration == 0 { TaskKind::Milestone } else { TaskKind::Task };
        t.duration = rec.duration;
        t.estimated = false;
        t.mode = ScheduleMode::Auto;
        t.calendar = rec.calendar;
        t.ignore_resource_calendars = rec.ignore_resource_calendars;
        t.constraint = Constraint { kind: ConstraintKind::Snet, date: Some(WorkCal::single(cal).day_start(date)) };
        t.occurrence = true;
    }

    /// Inserts a recurring task above row `index`, at that row's outline level (appended at the
    /// top level when `None`): a summary named `name`
    /// with one subtask per occurrence, "name 1", "name 2", … Returns the summary's id.
    pub fn add_recurring_task(&mut self, name: &str, rec: Recurrence, index: Option<usize>) -> PlanResult<TaskId> {
        let dates = self.occurrence_dates(&rec)?;
        let name = name.trim();
        if name.is_empty() {
            return invalid("a recurring task needs a name");
        }
        let at = self.outside_recurring(index.unwrap_or(self.tasks.len()).min(self.tasks.len()));
        let level = self.tasks.get(at).map_or(1, |t| t.outline_level);
        let mut summary = Task::new(self.ids.next_task(), name, self.settings.time.minutes_per_day);
        summary.outline_level = level;
        let id = self.insert_task(at, summary);
        let row = self.task_index(id).ok_or(PlanError::TaskNotFound(id))?;
        let level = self.tasks.get(row).map_or(1, |t| t.outline_level);
        if let Some(t) = self.tasks.get_mut(row) {
            t.recurrence = Some(rec);
            t.mode = ScheduleMode::Auto;
            t.calendar = rec.calendar;
            t.ignore_resource_calendars = rec.ignore_resource_calendars;
        }
        for (k, date) in dates.into_iter().enumerate() {
            let mut t = Task::new(self.ids.next_task(), name, self.settings.time.minutes_per_day);
            self.shape_occurrence(&mut t, name, k + 1, date, &rec);
            t.outline_level = level.saturating_add(1);
            self.tasks.insert(row + 1 + k, t);
        }
        Ok(id)
    }

    /// Changes a recurring task: its name (when given) and definition. Existing occurrences are
    /// moved to the new dates in order, extra ones are deleted, and new ones get the resources of
    /// the first occurrence. An occurrence with progress (or logged time) is never deleted and
    /// keeps its dates; the change is refused when it would have to go. Tasks added under the
    /// recurring task by hand are left alone. Returns the number of occurrences deleted.
    pub fn set_recurrence(&mut self, id: TaskId, name: Option<&str>, rec: Recurrence) -> PlanResult<usize> {
        let row = self.task_index(id).ok_or(PlanError::TaskNotFound(id))?;
        if self.tasks.get(row).is_none_or(|t| t.recurrence.is_none()) {
            return invalid("this is not a recurring task");
        }
        let dates = self.occurrence_dates(&rec)?;
        let name = match name.map(str::trim) {
            Some("") => return invalid("a recurring task needs a name"),
            Some(n) => n.to_string(),
            None => self.tasks.get(row).map(|t| t.name.clone()).unwrap_or_default(),
        };
        let level = self.tasks.get(row).map_or(1, |t| t.outline_level);
        let kids = self.occurrences_of(row);
        let started = |p: &Project, id: TaskId| {
            p.task(id).is_some_and(|t| t.percent_complete > 0 || t.actual_start.is_some() || t.actual_finish.is_some())
                || p.time_entries.iter().any(|e| e.task == id)
        };
        let doomed: Vec<TaskId> = kids.get(dates.len()..).unwrap_or_default().to_vec();
        if let Some(t) = doomed.iter().find(|&&k| started(self, k)).and_then(|&k| self.task(k)) {
            return invalid(format!(
                "'{}' has progress or logged time, so it can't be removed; keep at least {} occurrences",
                t.name,
                kids.len()
            ));
        }
        let template: Vec<(crate::ids::ResourceId, f64)> =
            kids.first().map(|&k| self.assignments_for_task(k).map(|a| (a.resource, a.units)).collect()).unwrap_or_default();
        if let Some(t) = self.tasks.get_mut(row) {
            t.name = name.clone();
            t.recurrence = Some(rec);
            t.calendar = rec.calendar;
            t.ignore_resource_calendars = rec.ignore_resource_calendars;
        }
        if !doomed.is_empty() {
            self.delete_tasks(&doomed)?;
        }
        for (k, date) in dates.into_iter().enumerate() {
            if let Some(&kid) = kids.get(k) {
                let keep = started(self, kid);
                let mut t = self.task(kid).cloned().ok_or(PlanError::TaskNotFound(kid))?;
                if keep {
                    // Work already done stays where it happened; only the name follows.
                    t.name = format!("{name} {}", k + 1);
                } else {
                    self.shape_occurrence(&mut t, &name, k + 1, date, &rec);
                }
                if let Some(slot) = self.task_mut(kid) {
                    *slot = t;
                }
            } else {
                let row = self.task_index(id).ok_or(PlanError::TaskNotFound(id))?;
                let end = self.descendants_range(row).end;
                let mut t = Task::new(self.ids.next_task(), &name, self.settings.time.minutes_per_day);
                self.shape_occurrence(&mut t, &name, k + 1, date, &rec);
                t.outline_level = level.saturating_add(1);
                let new = t.id;
                self.tasks.insert(end, t);
                if rec.duration > 0 {
                    for &(r, units) in &template {
                        self.assign(new, r, Some(units), None)?;
                    }
                }
            }
        }
        Ok(doomed.len())
    }

    /// The occurrences a resource assigned to the recurring task at `row` works on.
    pub fn occurrences_for_assignment(&self, row: usize) -> Vec<TaskId> {
        self.occurrences_of(row).into_iter().filter(|&k| self.task(k).is_some_and(|t| !t.is_milestone())).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::calendar::Calendar;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    fn rec(pattern: Pattern, start: NaiveDate, end: RecurEnd) -> Recurrence {
        Recurrence { pattern, start, end, duration: 60, calendar: None, ignore_resource_calendars: false }
    }

    fn dates(r: Recurrence) -> Vec<NaiveDate> {
        let cal = Calendar::standard(CalendarId(1));
        r.dates(&WorkCal::single(&cal)).unwrap()
    }

    #[test]
    fn weekly_on_tuesdays_and_thursdays() {
        let r = rec(Pattern::Weekly { interval: 1, days: 0b1010 }, d(2026, 10, 21), RecurEnd::After(4));
        assert_eq!(dates(r), [d(2026, 10, 22), d(2026, 10, 27), d(2026, 10, 29), d(2026, 11, 3)]);
    }

    #[test]
    fn every_three_weeks_on_monday() {
        let r = rec(Pattern::Weekly { interval: 3, days: 1 }, d(2026, 10, 19), RecurEnd::By(d(2026, 12, 1)));
        assert_eq!(dates(r), [d(2026, 10, 19), d(2026, 11, 9), d(2026, 11, 30)]);
    }

    #[test]
    fn daily_skips_to_working_days() {
        // Every day from a Friday: Saturday and Sunday move to Monday, which counts once; the
        // series still has four occurrences.
        let r = rec(Pattern::Daily { interval: 1, workdays: false }, d(2026, 10, 23), RecurEnd::After(4));
        assert_eq!(dates(r), [d(2026, 10, 23), d(2026, 10, 26), d(2026, 10, 27), d(2026, 10, 28)]);
        let r = rec(Pattern::Daily { interval: 2, workdays: true }, d(2026, 10, 23), RecurEnd::After(3));
        assert_eq!(dates(r), [d(2026, 10, 23), d(2026, 10, 27), d(2026, 10, 29)]);
    }

    #[test]
    fn monthly_by_day_and_by_weekday() {
        let r = rec(Pattern::Monthly { interval: 1, on: MonthDay::Day(31) }, d(2027, 1, 1), RecurEnd::After(3));
        // Jan 31 is a Sunday (moves to Feb 1); Feb has 28 days (Sun 28 → Mon Mar 1); Mar 31 is a Wednesday.
        assert_eq!(dates(r), [d(2027, 2, 1), d(2027, 3, 1), d(2027, 3, 31)]);
        let r =
            rec(Pattern::Monthly { interval: 2, on: MonthDay::Weekday { nth: Nth::Last, weekday: 4 } }, d(2026, 10, 1), RecurEnd::After(2));
        assert_eq!(dates(r), [d(2026, 10, 30), d(2026, 12, 25)]);
        let r =
            rec(Pattern::Yearly { month: 11, on: MonthDay::Weekday { nth: Nth::Fourth, weekday: 3 } }, d(2026, 1, 1), RecurEnd::After(2));
        assert_eq!(dates(r), [d(2026, 11, 26), d(2027, 11, 25)]);
    }

    #[test]
    fn bad_definitions_are_refused() {
        let cal = Calendar::standard(CalendarId(1));
        let wc = WorkCal::single(&cal);
        let start = d(2026, 10, 19);
        for r in [
            rec(Pattern::Daily { interval: 0, workdays: false }, start, RecurEnd::After(3)),
            rec(Pattern::Weekly { interval: 1, days: 0 }, start, RecurEnd::After(3)),
            rec(Pattern::Monthly { interval: 1, on: MonthDay::Day(32) }, start, RecurEnd::After(3)),
            rec(Pattern::Yearly { month: 13, on: MonthDay::Day(1) }, start, RecurEnd::After(3)),
            rec(Pattern::Monthly { interval: 1, on: MonthDay::Weekday { nth: Nth::First, weekday: 7 } }, start, RecurEnd::After(3)),
            rec(Pattern::Daily { interval: 1, workdays: false }, start, RecurEnd::After(0)),
            rec(Pattern::Daily { interval: 1, workdays: false }, start, RecurEnd::After(1000)),
            rec(Pattern::Daily { interval: 1, workdays: false }, start, RecurEnd::By(d(2026, 10, 1))),
        ] {
            assert!(r.dates(&wc).is_err(), "{r:?}");
        }
        // Regression: a long range was cut at 999 pattern days without a word.
        let r = rec(Pattern::Daily { interval: 1, workdays: false }, start, RecurEnd::By(d(2035, 1, 1)));
        assert!(r.dates(&wc).unwrap_err().to_string().contains("999"));
        let r = rec(Pattern::Yearly { month: 1, on: MonthDay::Day(5) }, d(2299, 1, 1), RecurEnd::After(5));
        assert!(r.dates(&wc).unwrap_err().to_string().contains("2300"));
        // A range with no matching day.
        let r = rec(Pattern::Weekly { interval: 1, days: 1 }, d(2026, 10, 20), RecurEnd::By(d(2026, 10, 25)));
        assert!(r.dates(&wc).is_err());
    }

    fn weekly_plan() -> (Project, TaskId) {
        let mut p = Project::new("R", d(2026, 10, 19));
        let r = rec(Pattern::Weekly { interval: 1, days: 1 }, d(2026, 10, 19), RecurEnd::After(3));
        let id = p.add_recurring_task("Sync", r, None).unwrap();
        crate::schedule::recalculate(&mut p);
        (p, id)
    }

    /// Regression: a recurring task inserted at an occurrence landed inside the other recurring
    /// task, whose next change then renamed and reshaped it; a task added under a recurring
    /// task by hand was treated as an occurrence too.
    #[test]
    fn only_real_occurrences_change_with_their_recurring_task() {
        let (mut p, outer) = weekly_plan();
        let second = p.tasks[2].id;
        let r = rec(Pattern::Daily { interval: 1, workdays: false }, d(2026, 10, 19), RecurEnd::After(2));
        let inner = p.add_recurring_task("Inner", r, p.task_index(second)).unwrap();
        assert_eq!(p.task_index(inner), Some(0), "above the other recurring task, not inside it");
        assert_eq!(p.tasks[0].outline_level, 1);
        // A task added by hand under the recurring task.
        let prep = p.add_task("Prep", Some(p.task_index(outer).unwrap() + 1));
        assert_eq!(p.parent_index(p.task_index(prep).unwrap()), p.task_index(outer));
        let ana = p.add_resource("Ana");
        p.assign(outer, ana, Some(1.0), None).unwrap();
        assert_eq!(p.assignments_for_task(prep).count(), 0, "only occurrences get the resource");
        let r = rec(Pattern::Weekly { interval: 1, days: 1 }, d(2026, 10, 19), RecurEnd::After(2));
        assert_eq!(p.set_recurrence(outer, None, r).unwrap(), 1);
        let t = p.task(prep).unwrap();
        assert_eq!((t.name.as_str(), t.constraint.kind), ("Prep", ConstraintKind::Asap), "left alone");
        assert!(p.task(inner).unwrap().recurrence.is_some());
        let row = p.task_index(outer).unwrap();
        assert_eq!(p.occurrences_of(row).len(), 2);
        assert!(p.is_recurring(row + 2) && !p.is_recurring(p.task_index(prep).unwrap()));
    }

    /// Regression: changing a recurring task deleted occurrences that had progress.
    #[test]
    fn occurrences_with_progress_are_kept() {
        let (mut p, id) = weekly_plan();
        let third = p.tasks[3].id;
        p.set_percent_complete(third, 100).unwrap();
        crate::schedule::recalculate(&mut p);
        let fewer = rec(Pattern::Weekly { interval: 1, days: 1 }, d(2026, 10, 19), RecurEnd::After(2));
        assert!(p.set_recurrence(id, None, fewer).is_err());
        assert!(p.task(third).is_some());
        // Moving the series keeps the done occurrence where it was.
        let before = p.task(third).unwrap().constraint;
        let later = rec(Pattern::Weekly { interval: 1, days: 1 }, d(2026, 11, 2), RecurEnd::After(3));
        p.set_recurrence(id, None, later).unwrap();
        assert_eq!(p.task(third).unwrap().constraint, before);
    }

    /// Regression: a calendar a recurring task used could not be deleted (the edit rolled back).
    #[test]
    fn deleting_a_recurring_task_calendar_works() {
        let mut p = Project::new("R", d(2026, 10, 19));
        let cal = p.add_calendar("Shift", None).unwrap();
        let r = Recurrence { calendar: Some(cal), ..rec(Pattern::Weekly { interval: 1, days: 1 }, d(2026, 10, 19), RecurEnd::After(2)) };
        let id = p.add_recurring_task("Check", r, None).unwrap();
        p.delete_calendar(cal).unwrap();
        assert!(crate::limits::sanitize(&mut p).is_empty());
        assert_eq!(p.task(id).unwrap().recurrence.unwrap().calendar, None);
    }

    #[test]
    fn far_dates_stop_at_the_date_limit() {
        let r = rec(Pattern::Yearly { month: 2, on: MonthDay::Day(29) }, d(2290, 1, 1), RecurEnd::By(d(2400, 1, 1)));
        let v = dates(r);
        assert!(v.len() <= 11 && v.iter().all(|x| x.year() <= 2300), "{v:?}");
    }
}
