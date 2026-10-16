//! Hard limits on what a project may contain.
//!
//! Files and MCP arguments are treated as hostile: every count, duration, date, number and text a
//! document can carry is bounded, so that scheduling stays fast and date arithmetic can't
//! overflow. Opening a file clamps out-of-range values into range and reports what it changed
//! ([`sanitize`]); a file with more items than the limits allow is refused ([`check_counts`]). An
//! edit that would leave a project out of range is rejected and rolled back ([`check_edit`], run
//! by `Document::edit`).

use std::collections::{BTreeMap, HashSet};

use chrono::{NaiveDate, NaiveDateTime, NaiveTime};

use crate::calendar::{Calendar, CalendarException, TimeRange};
use crate::duration::{Lag, Minutes};
use crate::model::*;
use crate::ops::{PlanError, PlanResult};

/// Largest `.plancraft` file PlanCraft opens.
pub const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;
pub const MAX_TASKS: usize = 50_000;
pub const MAX_RESOURCES: usize = 10_000;
pub const MAX_ASSIGNMENTS: usize = 200_000;
pub const MAX_TIME_ENTRIES: usize = 500_000;
pub const MAX_CALENDARS: usize = 1_000;
pub const MAX_EXTERNAL_PROJECTS: usize = 200;
/// Exceptions per calendar or resource, availability periods per resource, snapshots per external project.
pub const MAX_LIST: usize = 10_000;
/// Predecessors (and, separately, milestone members) per task.
pub const MAX_LINKS_PER_TASK: usize = 1_000;
/// Working-time ranges per day.
pub const MAX_RANGES_PER_DAY: usize = 96;
pub const MAX_OUTLINE_LEVEL: u16 = 100;
/// Characters in a name, deliverable, e-mail, group or other short text.
pub const MAX_NAME_CHARS: usize = 1_000;
/// Characters in notes and file paths.
pub const MAX_NOTES_CHARS: usize = 100_000;
/// Longest duration, amount of work, lag or logged time: five years around the clock.
pub const MAX_MINUTES: Minutes = 5 * 366 * 24 * 60;
/// Largest percentage lag (either sign).
pub const MAX_LAG_PERCENT: i32 = 10_000;
/// Largest units of a resource or assignment (100,000%).
pub const MAX_UNITS: f64 = 1_000.0;
/// Largest rate, cost per use or cost (either sign for costs).
pub const MAX_MONEY: f64 = 1e12;
pub const MAX_COMPLEXITY_POINTS: f64 = 1e6;
pub const MAX_PRIORITY: u16 = 1_000;
/// Largest stored id; id counters never get near `u32::MAX`.
pub const MAX_ID: u32 = 1 << 30;

/// Earliest date a project may use.
pub fn min_date() -> NaiveDate {
    NaiveDate::from_ymd_opt(1900, 1, 1).unwrap_or(NaiveDate::MIN)
}

/// Latest date a project may use.
pub fn max_date() -> NaiveDate {
    NaiveDate::from_ymd_opt(2300, 12, 31).unwrap_or(NaiveDate::MAX)
}

pub fn clamp_date(d: NaiveDate) -> NaiveDate {
    d.clamp(min_date(), max_date())
}

pub fn clamp_datetime(dt: NaiveDateTime) -> NaiveDateTime {
    dt.clamp(min_date().and_time(NaiveTime::MIN), max_date().and_time(NaiveTime::MIN))
}

/// Clamps a duration or amount of work into `0..=MAX_MINUTES`.
pub fn clamp_minutes(m: Minutes) -> Minutes {
    m.clamp(0, MAX_MINUTES)
}

/// Clamps a float into `lo..=hi`; NaN becomes `lo`.
pub fn clamp_f64(v: f64, lo: f64, hi: f64) -> f64 {
    if v.is_nan() { lo } else { v.clamp(lo, hi) }
}

/// Refuses projects with more items than the limits allow, duplicate ids or ids out of range.
pub fn check_counts(p: &Project) -> PlanResult<()> {
    let too_many = |what: &str, n: usize, max: usize| -> PlanResult<()> {
        if n > max { Err(PlanError::Limit(format!("too many {what}: {n} (the maximum is {max})"))) } else { Ok(()) }
    };
    too_many("tasks", p.tasks.len(), MAX_TASKS)?;
    too_many("resources", p.resources.len(), MAX_RESOURCES)?;
    too_many("assignments", p.assignments.len(), MAX_ASSIGNMENTS)?;
    too_many("time entries", p.time_entries.len(), MAX_TIME_ENTRIES)?;
    too_many("calendars", p.calendars.len(), MAX_CALENDARS)?;
    too_many("linked projects", p.external_projects.len(), MAX_EXTERNAL_PROJECTS)?;
    unique("task", p.tasks.iter().map(|t| t.id.0))?;
    unique("resource", p.resources.iter().map(|r| r.id.0))?;
    unique("assignment", p.assignments.iter().map(|a| a.id.0))?;
    unique("time entry", p.time_entries.iter().map(|e| e.id.0))?;
    unique("calendar", p.calendars.iter().map(|c| c.id.0))?;
    Ok(())
}

fn unique(what: &str, ids: impl Iterator<Item = u32>) -> PlanResult<()> {
    let mut seen = HashSet::new();
    for id in ids {
        if id > MAX_ID {
            return Err(PlanError::Limit(format!("{what} id {id} is out of range")));
        }
        if !seen.insert(id) {
            return Err(PlanError::Limit(format!("two {what}s share the id {id}")));
        }
    }
    Ok(())
}

/// Validates a project after an edit: counts, ids and every value must be in range.
///
/// It may clamp values while checking; callers roll the project back on error.
pub fn check_edit(p: &mut Project) -> PlanResult<()> {
    check_counts(p)?;
    match sanitize(p).into_iter().next() {
        Some(problem) => Err(PlanError::Limit(problem)),
        None => Ok(()),
    }
}

/// Clamps every value of a project into range and drops dangling references.
///
/// Returns one line per kind of change made (empty when the project was already in range).
pub fn sanitize(p: &mut Project) -> Vec<String> {
    let mut s = Fixer::default();
    s.name(&mut p.name);
    s.name(&mut p.author);
    s.name(&mut p.manager);
    s.name(&mut p.company);
    s.notes(&mut p.notes);
    s.datetime(&mut p.start);
    s.opt_date(&mut p.status_date);
    s.settings(&mut p.settings);

    for c in &mut p.calendars {
        s.calendar(c);
    }
    let calendars: HashSet<_> = p.calendars.iter().map(|c| c.id).collect();
    if !calendars.contains(&p.calendar)
        && let Some(first) = p.calendars.first()
    {
        p.calendar = first.id;
        s.note("the project calendar did not exist");
    }

    for t in &mut p.tasks {
        s.task(t, &calendars);
    }
    for r in &mut p.resources {
        s.resource(r, &calendars);
    }

    let tasks: HashSet<_> = p.tasks.iter().map(|t| t.id).collect();
    let resources: HashSet<_> = p.resources.iter().map(|r| r.id).collect();
    let before = p.assignments.len();
    p.assignments.retain(|a| tasks.contains(&a.task) && resources.contains(&a.resource));
    if p.assignments.len() != before {
        s.note("an assignment referred to a missing task or resource");
    }
    for a in &mut p.assignments {
        s.f64(&mut a.units, 0.0, MAX_UNITS, "assignment units are out of range");
        s.minutes(&mut a.work, "assignment work is out of range");
        s.minutes(&mut a.actual_work, "actual work is out of range");
        s.f64(&mut a.cost, -MAX_MONEY, MAX_MONEY, "a cost is out of range");
    }
    let before = p.time_entries.len();
    p.time_entries.retain(|e| tasks.contains(&e.task) && resources.contains(&e.resource));
    if p.time_entries.len() != before {
        s.note("a time entry referred to a missing task or resource");
    }
    for e in &mut p.time_entries {
        s.date(&mut e.date);
        s.minutes(&mut e.minutes, "logged time is out of range");
        s.notes(&mut e.note);
    }

    for e in &mut p.external_projects {
        s.name(&mut e.name);
        s.notes(&mut e.path);
        s.list(&mut e.tasks, MAX_LIST, "a linked project has too many cached tasks");
        for t in &mut e.tasks {
            s.name(&mut t.name);
            s.opt_datetime(&mut t.start);
            s.opt_datetime(&mut t.finish);
            s.minutes(&mut t.duration, "a duration is out of range");
            if t.percent_complete > 100 {
                t.percent_complete = 100;
                s.note("% complete is above 100");
            }
        }
    }

    let ids = &mut p.ids;
    for counter in [&mut ids.task, &mut ids.resource, &mut ids.calendar, &mut ids.assignment, &mut ids.time_entry] {
        if *counter > MAX_ID {
            *counter = MAX_ID;
            s.note("an id counter is out of range");
        }
    }
    p.normalize_outline();
    s.finish()
}

/// Collects what [`sanitize`] changed, one line per kind of change.
#[derive(Default)]
struct Fixer {
    notes: BTreeMap<&'static str, usize>,
}

impl Fixer {
    fn note(&mut self, what: &'static str) {
        *self.notes.entry(what).or_default() += 1;
    }

    fn finish(self) -> Vec<String> {
        self.notes.into_iter().map(|(what, n)| if n == 1 { what.to_string() } else { format!("{what} ({n} times)") }).collect()
    }

    fn text(&mut self, s: &mut String, max: usize, what: &'static str) {
        if let Some((cut, _)) = s.char_indices().nth(max) {
            s.truncate(cut);
            self.note(what);
        }
    }

    fn name(&mut self, s: &mut String) {
        self.text(s, MAX_NAME_CHARS, "a name or short text is longer than 1,000 characters");
    }

    fn notes(&mut self, s: &mut String) {
        self.text(s, MAX_NOTES_CHARS, "notes are longer than 100,000 characters");
    }

    fn list<T>(&mut self, v: &mut Vec<T>, max: usize, what: &'static str) {
        if v.len() > max {
            v.truncate(max);
            self.note(what);
        }
    }

    fn date(&mut self, d: &mut NaiveDate) {
        let c = clamp_date(*d);
        if c != *d {
            *d = c;
            self.note("a date is outside 1900–2300");
        }
    }

    fn opt_date(&mut self, d: &mut Option<NaiveDate>) {
        if let Some(d) = d {
            self.date(d);
        }
    }

    fn datetime(&mut self, dt: &mut NaiveDateTime) {
        let c = clamp_datetime(*dt);
        if c != *dt {
            *dt = c;
            self.note("a date is outside 1900–2300");
        }
    }

    fn opt_datetime(&mut self, dt: &mut Option<NaiveDateTime>) {
        if let Some(dt) = dt {
            self.datetime(dt);
        }
    }

    fn minutes(&mut self, m: &mut Minutes, what: &'static str) {
        let c = clamp_minutes(*m);
        if c != *m {
            *m = c;
            self.note(what);
        }
    }

    fn signed_minutes(&mut self, m: &mut Minutes, what: &'static str) {
        let c = (*m).clamp(-MAX_MINUTES, MAX_MINUTES);
        if c != *m {
            *m = c;
            self.note(what);
        }
    }

    fn f64(&mut self, v: &mut f64, lo: f64, hi: f64, what: &'static str) {
        let c = clamp_f64(*v, lo, hi);
        if c != *v || v.is_nan() {
            *v = c;
            self.note(what);
        }
    }

    fn lag(&mut self, lag: &mut Lag) {
        match lag {
            Lag::None => {}
            Lag::Working(m) | Lag::Elapsed(m) => self.signed_minutes(m, "a lag is longer than the maximum of 5 years"),
            Lag::Percent(p) => {
                let c = (*p).clamp(-MAX_LAG_PERCENT, MAX_LAG_PERCENT);
                if c != *p {
                    *p = c;
                    self.note("a percentage lag is out of range");
                }
            }
        }
    }

    fn ranges(&mut self, v: &mut Vec<TimeRange>) {
        self.list(v, MAX_RANGES_PER_DAY, "a day has too many working-time ranges");
        let before = v.len();
        v.retain(|r| r.start < r.end && r.end <= 1440);
        if v.len() != before {
            self.note("a working-time range is invalid");
        }
    }

    fn exception(&mut self, e: &mut CalendarException) {
        self.name(&mut e.name);
        self.date(&mut e.start);
        self.date(&mut e.end);
        if e.end < e.start {
            std::mem::swap(&mut e.start, &mut e.end);
            self.note("an exception ends before it starts");
        }
        self.ranges(&mut e.working);
    }

    fn calendar(&mut self, c: &mut Calendar) {
        self.name(&mut c.name);
        for wd in crate::calendar::WEEKDAYS {
            self.ranges(c.week.day_mut(wd));
        }
        self.list(&mut c.exceptions, MAX_LIST, "a calendar has too many exceptions");
        for e in &mut c.exceptions {
            self.exception(e);
        }
    }

    fn settings(&mut self, s: &mut ProjectSettings) {
        let t = &mut s.time;
        let before = (t.minutes_per_day, t.minutes_per_week, t.days_per_month, t.default_start_time, t.default_finish_time);
        t.minutes_per_day = t.minutes_per_day.clamp(1, 24 * 60);
        t.minutes_per_week = t.minutes_per_week.clamp(1, 7 * 24 * 60);
        t.days_per_month = t.days_per_month.clamp(1, 31);
        t.default_start_time = t.default_start_time.min(24 * 60);
        t.default_finish_time = t.default_finish_time.min(24 * 60);
        if before != (t.minutes_per_day, t.minutes_per_week, t.days_per_month, t.default_start_time, t.default_finish_time) {
            self.note("hours per day, hours per week or days per month are out of range");
        }
        self.name(&mut s.currency);
        self.minutes(&mut s.minutes_per_point, "work per complexity point is out of range");
        self.signed_minutes(&mut s.critical_slack, "the critical slack threshold is out of range");
    }

    fn task(&mut self, t: &mut Task, calendars: &HashSet<crate::ids::CalendarId>) {
        self.name(&mut t.name);
        self.name(&mut t.deliverable);
        self.notes(&mut t.notes);
        if t.outline_level > MAX_OUTLINE_LEVEL {
            t.outline_level = MAX_OUTLINE_LEVEL;
            self.note("tasks are nested more than 100 levels deep");
        }
        self.minutes(&mut t.duration, "a duration is longer than the maximum of 5 years");
        self.minutes(&mut t.leveling_delay, "a leveling delay is out of range");
        self.opt_datetime(&mut t.constraint.date);
        self.opt_datetime(&mut t.deadline);
        self.opt_datetime(&mut t.actual_start);
        self.opt_datetime(&mut t.actual_finish);
        self.opt_datetime(&mut t.manual_start);
        self.opt_datetime(&mut t.manual_finish);
        if t.calendar.is_some_and(|c| !calendars.contains(&c)) {
            t.calendar = None;
            self.note("a task used a calendar that does not exist");
        }
        if t.priority > MAX_PRIORITY {
            t.priority = MAX_PRIORITY;
            self.note("a priority is above 1000");
        }
        if t.percent_complete > 100 {
            t.percent_complete = 100;
            self.note("% complete is above 100");
        }
        self.list(&mut t.predecessors, MAX_LINKS_PER_TASK, "a task has more than 1,000 predecessors");
        for d in &mut t.predecessors {
            self.lag(&mut d.lag);
        }
        self.list(&mut t.members, MAX_LINKS_PER_TASK, "a milestone has more than 1,000 member tasks");
        match &mut t.estimate {
            None => {}
            Some(Estimate::Complexity { points }) => self.f64(points, 0.0, MAX_COMPLEXITY_POINTS, "complexity points are out of range"),
            Some(Estimate::Work { minutes }) => self.minutes(minutes, "an estimate is out of range"),
            Some(Estimate::ThreePoint { optimistic, most_likely, pessimistic }) => {
                for m in [optimistic, most_likely, pessimistic] {
                    self.minutes(m, "an estimate is out of range");
                }
            }
        }
        self.f64(&mut t.fixed_cost, -MAX_MONEY, MAX_MONEY, "a cost is out of range");
        if let Some(b) = &mut t.baseline {
            self.datetime(&mut b.start);
            self.datetime(&mut b.finish);
            self.datetime(&mut b.saved);
            self.minutes(&mut b.duration, "a baseline duration is out of range");
            self.minutes(&mut b.work, "baseline work is out of range");
            self.f64(&mut b.cost, -MAX_MONEY, MAX_MONEY, "a cost is out of range");
        }
    }

    fn resource(&mut self, r: &mut Resource, calendars: &HashSet<crate::ids::CalendarId>) {
        self.name(&mut r.name);
        self.name(&mut r.initials);
        self.name(&mut r.group);
        self.name(&mut r.email);
        self.name(&mut r.material_label);
        self.notes(&mut r.notes);
        self.f64(&mut r.max_units, 0.0, MAX_UNITS, "a resource's maximum units are out of range");
        self.f64(&mut r.standard_rate, 0.0, MAX_MONEY, "a rate is out of range");
        self.f64(&mut r.overtime_rate, 0.0, MAX_MONEY, "a rate is out of range");
        self.f64(&mut r.cost_per_use, 0.0, MAX_MONEY, "a rate is out of range");
        if r.calendar.is_some_and(|c| !calendars.contains(&c)) {
            r.calendar = None;
            self.note("a resource used a calendar that does not exist");
        }
        self.list(&mut r.availability, MAX_LIST, "a resource has too many availability periods");
        for a in &mut r.availability {
            self.opt_date(&mut a.from);
            self.opt_date(&mut a.to);
            self.f64(&mut a.units, 0.0, MAX_UNITS, "available units are out of range");
        }
        self.list(&mut r.exceptions, MAX_LIST, "a resource has too many calendar exceptions");
        for e in &mut r.exceptions {
            self.exception(e);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::TaskId;

    fn project() -> Project {
        Project::new("P", NaiveDate::from_ymd_opt(2026, 10, 19).unwrap())
    }

    #[test]
    fn sanitize_clamps_and_reports() {
        let mut p = project();
        let a = p.add_task("A", None);
        let t = p.task_mut(a).unwrap();
        t.duration = Minutes::MAX;
        t.percent_complete = 250;
        t.outline_level = 9;
        t.deadline = NaiveDate::from_ymd_opt(9999, 1, 1).and_then(|d| d.and_hms_opt(0, 0, 0));
        t.name = "x".repeat(5000);
        t.predecessors.push(crate::model::Dependency { predecessor: TaskId(99).into(), kind: LinkType::FS, lag: Lag::Percent(i32::MIN) });
        p.settings.time.minutes_per_day = 0;
        let notes = sanitize(&mut p);
        let t = p.task(a).unwrap();
        assert_eq!(t.duration, MAX_MINUTES);
        assert_eq!(t.percent_complete, 100);
        assert_eq!(t.outline_level, 1);
        assert_eq!(t.deadline.map(|d| d.date()), Some(max_date()));
        assert_eq!(t.name.chars().count(), MAX_NAME_CHARS);
        assert_eq!(t.predecessors[0].lag, Lag::Percent(-MAX_LAG_PERCENT));
        assert_eq!(p.settings.time.minutes_per_day, 1);
        assert!(notes.len() >= 5, "{notes:?}");
        assert!(sanitize(&mut p).is_empty(), "sanitizing twice changes nothing");
    }

    #[test]
    fn counts_and_duplicates_are_refused() {
        let mut p = project();
        let a = p.add_task("A", None);
        p.add_task("B", None);
        assert!(check_counts(&p).is_ok());
        p.tasks[1].id = a;
        assert!(matches!(check_counts(&p), Err(PlanError::Limit(m)) if m.contains("share the id")));
        p.tasks[1].id = TaskId(u32::MAX);
        assert!(check_counts(&p).is_err());
    }

    #[test]
    fn edits_out_of_range_are_rejected() {
        let mut p = project();
        let a = p.add_task("A", None);
        assert!(check_edit(&mut p).is_ok());
        p.task_mut(a).unwrap().duration = MAX_MINUTES + 1;
        assert!(matches!(check_edit(&mut p), Err(PlanError::Limit(m)) if m.contains("5 years")));
    }
}
