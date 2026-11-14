//! PlanCraft → MSPDI. Elements are written in the schema's order. What MSPDI cannot carry is
//! described in the returned notes.

use std::collections::{BTreeMap, HashMap};

use chrono::Datelike;

use super::codes::{self, Slot};
use super::xml::Writer;
use crate::calendar::{CalendarException, TimeRange, WorkWeek};
use crate::custom::{CustomFieldKind, CustomValue};
use crate::duration::{Lag, Minutes, best_unit};
use crate::ids::{CalendarId, CustomFieldId, ResourceId, TaskId};
use crate::model::*;

/// The most working times a day may have in MSPDI.
const MAX_TIMES: usize = 5;
/// How many years a yearly exception is written for.
const YEARLY_OCCURRENCES: i32 = 100;

/// Writes a plan as an MSPDI document. Returns the XML and notes on what it leaves out.
pub fn export(p: &Project) -> (String, Vec<String>) {
    let mut notes: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut note = |m: &'static str| *notes.entry(m).or_insert(0) += 1;
    let mut w = Writer::new();
    w.open_ns("Project", Some(codes::NAMESPACE));
    w.leaf("SaveVersion", codes::SAVE_VERSION);
    w.leaf("Name", &p.name);
    w.leaf("Title", &p.name);
    if !p.company.is_empty() {
        w.leaf("Company", &p.company);
    }
    if !p.manager.is_empty() {
        w.leaf("Manager", &p.manager);
    }
    if !p.author.is_empty() {
        w.leaf("Author", &p.author);
    }
    w.leaf("ScheduleFromStart", 1);
    w.leaf("StartDate", codes::datetime_text(p.start));
    if let Some(f) = p.sched.finish {
        w.leaf("FinishDate", codes::datetime_text(f));
    }
    let mpd = p.settings.time.minutes_per_day.max(1);
    w.leaf("CriticalSlackLimit", p.settings.critical_slack / mpd);
    w.leaf("CurrencyDigits", 2);
    w.leaf("CurrencySymbol", &p.settings.currency);
    w.leaf("CurrencyCode", codes::currency_code(&p.settings.currency));
    w.leaf("CurrencySymbolPosition", 0);

    // Calendars: base calendars keep their ids; each resource gets a calendar of its own based on
    // its base calendar, with its personal exceptions, numbered after them.
    let base_uid = |id: CalendarId| i64::from(id.0);
    let mut next_uid = p.calendars.iter().map(|c| i64::from(c.id.0)).max().unwrap_or(0);
    let resource_cal: HashMap<ResourceId, i64> = p
        .resources
        .iter()
        .filter(|r| r.kind == ResourceType::Work)
        .map(|r| {
            next_uid = next_uid.saturating_add(1);
            (r.id, next_uid)
        })
        .collect();
    w.leaf("CalendarUID", base_uid(p.base_calendar().id));
    w.leaf("DefaultStartTime", codes::time_text(p.settings.time.default_start_time));
    w.leaf("DefaultFinishTime", codes::time_text(p.settings.time.default_finish_time));
    w.leaf("MinutesPerDay", p.settings.time.minutes_per_day);
    w.leaf("MinutesPerWeek", p.settings.time.minutes_per_week);
    w.leaf("DaysPerMonth", p.settings.time.days_per_month);
    w.leaf("DefaultTaskType", 0);
    w.leaf("DurationFormat", codes::format_code(p.settings.duration_unit, false, false));
    w.leaf("WorkFormat", 2);
    w.leaf("HonorConstraints", 1);
    w.leaf("NewTasksEffortDriven", 0);
    w.leaf("NewTasksEstimated", 1);
    w.leaf("WeekStartDay", 1);
    w.leaf("BaselineForEarnedValue", p.settings.earned_value_baseline);
    if let Some(d) = p.status_date {
        w.leaf("StatusDate", codes::datetime_text(d.and_time(chrono::NaiveTime::MIN)));
    }
    w.leaf("CurrentDate", codes::datetime_text(chrono::Local::now().naive_local()));

    // Custom fields: the first fields of each kind take Text1, Text2, … (and Number1, …).
    let mut slots: HashMap<CustomFieldId, (u32, CustomFieldKind)> = HashMap::new();
    let mut used: HashMap<CustomFieldKind, usize> = HashMap::new();
    let mut defs = Vec::new();
    for f in &p.custom_fields {
        let n = used.entry(f.kind).or_insert(0);
        *n += 1;
        match codes::field_id(Slot { kind: f.kind, number: *n }) {
            Some(fid) => {
                slots.insert(f.id, (fid, f.kind));
                defs.push((fid, Slot { kind: f.kind, number: *n }, f));
            }
            None => note("custom fields beyond the number Microsoft Project has of their kind were left out"),
        }
    }
    if !defs.is_empty() {
        w.open("ExtendedAttributes");
        for (fid, slot, f) in &defs {
            w.open("ExtendedAttribute");
            w.leaf("FieldID", fid);
            w.leaf("FieldName", slot.field_name());
            w.leaf("CFType", codes::cf_type_code(f.kind));
            w.leaf("ElemType", 20);
            w.leaf("UserDef", 1);
            w.leaf("Alias", &f.name);
            if !f.choices.is_empty() {
                w.leaf("RestrictValues", 1);
                w.open("ValueList");
                for (i, c) in f.choices.iter().enumerate() {
                    w.open("Value");
                    w.leaf("ID", i + 1);
                    w.leaf("Value", c);
                    w.close("Value");
                }
                w.close("ValueList");
            }
            w.close("ExtendedAttribute");
        }
        w.close("ExtendedAttributes");
    }

    w.open("Calendars");
    for c in &p.calendars {
        w.open("Calendar");
        w.leaf("UID", base_uid(c.id));
        w.leaf("Name", &c.name);
        w.leaf("IsBaseCalendar", 1);
        week(&mut w, &c.week);
        exceptions(&mut w, &c.exceptions, &mut note);
        w.close("Calendar");
    }
    for r in &p.resources {
        let Some(uid) = resource_cal.get(&r.id) else { continue };
        w.open("Calendar");
        w.leaf("UID", uid);
        w.leaf("Name", &r.name);
        w.leaf("IsBaseCalendar", 0);
        w.leaf("BaseCalendarUID", base_uid(r.calendar.unwrap_or(p.base_calendar().id)));
        exceptions(&mut w, &r.exceptions, &mut note);
        w.close("Calendar");
    }
    w.close("Calendars");

    w.open("Tasks");
    // The project summary task (outline level 0).
    w.open("Task");
    w.leaf("UID", 0);
    w.leaf("ID", 0);
    w.leaf("Name", &p.name);
    w.leaf("OutlineLevel", 0);
    if let Some(s) = p.sched.start {
        w.leaf("Start", codes::datetime_text(s));
    }
    if let Some(f) = p.sched.finish {
        w.leaf("Finish", codes::datetime_text(f));
    }
    w.leaf("Summary", 1);
    w.close("Task");
    for (i, t) in p.tasks.iter().enumerate() {
        task(&mut w, p, i, t, &slots, &mut note);
    }
    w.close("Tasks");

    w.open("Resources");
    for (i, r) in p.resources.iter().enumerate() {
        w.open("Resource");
        w.leaf("UID", r.id.0);
        w.leaf("ID", i + 1);
        w.leaf("Name", &r.name);
        w.leaf("Type", if r.kind == ResourceType::Material { 0 } else { 1 });
        if !r.initials.is_empty() {
            w.leaf("Initials", &r.initials);
        }
        if !r.material_label.is_empty() {
            w.leaf("MaterialLabel", &r.material_label);
        }
        if !r.group.is_empty() {
            w.leaf("Group", &r.group);
        }
        if !r.email.is_empty() {
            w.leaf("EmailAddress", &r.email);
        }
        if r.kind != ResourceType::Cost {
            w.leaf("MaxUnits", r.max_units);
            w.leaf("StandardRate", r.standard_rate);
            if r.kind == ResourceType::Work {
                w.leaf("StandardRateFormat", 2);
                w.leaf("OvertimeRate", r.overtime_rate);
                w.leaf("OvertimeRateFormat", 2);
            }
            w.leaf("CostPerUse", r.cost_per_use);
        }
        if let Some(uid) = resource_cal.get(&r.id) {
            w.leaf("CalendarUID", uid);
        }
        if !r.notes.is_empty() {
            w.leaf("Notes", &r.notes);
        }
        if r.kind == ResourceType::Cost {
            w.leaf("IsCostResource", 1);
        }
        if !r.availability.is_empty() {
            w.open("AvailabilityPeriods");
            for a in &r.availability {
                w.open("AvailabilityPeriod");
                if let Some(f) = a.from {
                    w.leaf("AvailableFrom", codes::datetime_text(f.and_time(chrono::NaiveTime::MIN)));
                }
                if let Some(t) = a.to {
                    w.leaf("AvailableTo", codes::datetime_text(t.and_hms_opt(23, 59, 0).unwrap_or(t.and_time(chrono::NaiveTime::MIN))));
                }
                // AvailableUnits is a percentage.
                w.leaf("AvailableUnits", a.units * 100.0);
                w.close("AvailabilityPeriod");
            }
            w.close("AvailabilityPeriods");
        }
        w.close("Resource");
    }
    w.close("Resources");

    w.open("Assignments");
    let tasks: HashMap<TaskId, &Task> = p.tasks.iter().map(|t| (t.id, t)).collect();
    let mut tracked: HashMap<(TaskId, ResourceId), Minutes> = HashMap::new();
    for e in &p.time_entries {
        let v = tracked.entry((e.task, e.resource)).or_insert(0);
        *v = v.saturating_add(e.minutes);
    }
    if !p.time_entries.is_empty() {
        note("logged time entries were written as the assignments' actual work");
    }
    for a in &p.assignments {
        w.open("Assignment");
        w.leaf("UID", a.id.0);
        w.leaf("TaskUID", a.task.0);
        w.leaf("ResourceUID", a.resource.0);
        let actual = a.actual_work.saturating_add(tracked.get(&(a.task, a.resource)).copied().unwrap_or(0));
        if actual > 0 {
            w.leaf("ActualWork", codes::duration_text(actual));
        }
        if a.cost != 0.0 {
            w.leaf("Cost", a.cost);
        }
        let t = tasks.get(&a.task);
        if let Some(f) = t.and_then(|t| t.sched.finish) {
            w.leaf("Finish", codes::datetime_text(f));
        }
        if let Some(s) = t.and_then(|t| t.sched.start) {
            w.leaf("Start", codes::datetime_text(s));
        }
        w.leaf("Units", a.units);
        w.leaf("Work", codes::duration_text(a.work));
        w.leaf("WorkContour", codes::contour_code(a.contour));
        w.close("Assignment");
    }
    w.close("Assignments");
    w.close("Project");

    if p.tasks.iter().any(|t| t.estimate.is_some()) {
        note("work estimates (points, three-point) were left out; durations and work are kept");
    }
    if p.tasks.iter().any(|t| !t.members.is_empty()) {
        note("milestone members were written as finish-to-start links from those tasks to the milestone");
    }
    if p.tasks.iter().any(|t| !t.deliverable.is_empty()) {
        note("milestone deliverables were left out");
    }
    if p.tasks.iter().any(|t| !t.gaps.is_empty() || !t.splits.is_empty()) {
        note("split tasks were written without their splits");
    }
    if p.tasks.iter().any(|t| t.recurrence.is_some()) {
        note("recurring tasks were written as summary tasks with their occurrences; the repeat pattern was left out");
    }
    if p.tasks.iter().any(|t| t.board_status.is_some() || !t.show_on_board) {
        note("Task Board columns were left out");
    }
    if p.resources.iter().any(|r| r.category != ResourceCategory::TeamMember) {
        note("resource categories were left out");
    }
    let out: Vec<String> = notes.into_iter().map(|(m, n)| if n > 1 { format!("{m} ({n} times)") } else { m.to_string() }).collect();
    (w.finish(), out)
}

fn week(w: &mut Writer, week: &WorkWeek) {
    use chrono::Weekday::*;
    w.open("WeekDays");
    for d in [Sun, Mon, Tue, Wed, Thu, Fri, Sat] {
        w.open("WeekDay");
        w.leaf("DayType", codes::weekday_code(d));
        times(w, week.day(d));
        w.close("WeekDay");
    }
    w.close("WeekDays");
}

/// `DayWorking` and `WorkingTimes` (at most five: later ones are joined into the fifth).
fn times(w: &mut Writer, ranges: &[TimeRange]) {
    w.leaf("DayWorking", u8::from(!ranges.is_empty()));
    if ranges.is_empty() {
        return;
    }
    let mut list: Vec<TimeRange> = ranges.iter().take(MAX_TIMES).copied().collect();
    if ranges.len() > MAX_TIMES
        && let (Some(last), Some(end)) = (list.last_mut(), ranges.last())
    {
        last.end = end.end;
    }
    w.open("WorkingTimes");
    for r in list {
        w.open("WorkingTime");
        w.leaf("FromTime", codes::time_text(r.start));
        w.leaf("ToTime", codes::time_text(r.end));
        w.close("WorkingTime");
    }
    w.close("WorkingTimes");
}

fn exceptions(w: &mut Writer, list: &[CalendarException], note: &mut impl FnMut(&'static str)) {
    if list.is_empty() {
        return;
    }
    w.open("Exceptions");
    for e in list {
        if e.yearly && e.start == e.end {
            // Every year on this day (Type 2: yearly by day of the month).
            w.open("Exception");
            w.leaf("EnteredByOccurrences", 1);
            w.open("TimePeriod");
            w.leaf("FromDate", codes::datetime_text(e.start.and_time(chrono::NaiveTime::MIN)));
            let last = e.start.with_year(e.start.year().saturating_add(YEARLY_OCCURRENCES - 1)).unwrap_or(e.end);
            w.leaf("ToDate", codes::datetime_text(last.and_hms_opt(23, 59, 0).unwrap_or(last.and_time(chrono::NaiveTime::MIN))));
            w.close("TimePeriod");
            w.leaf("Occurrences", YEARLY_OCCURRENCES);
            w.leaf("Name", &e.name);
            w.leaf("Type", 2);
            w.leaf("Period", 1);
            w.leaf("Month", e.start.month0());
            w.leaf("MonthDay", e.start.day());
            times(w, &e.working);
            w.close("Exception");
            continue;
        }
        let years = if e.yearly {
            note("yearly calendar exceptions longer than a day were written for 20 years");
            20
        } else {
            1
        };
        for y in 0..years {
            let (Some(start), Some(end)) = (shift_year(e.start, y), shift_year(e.end, y)) else { continue };
            w.open("Exception");
            w.leaf("EnteredByOccurrences", 0);
            w.open("TimePeriod");
            w.leaf("FromDate", codes::datetime_text(start.and_time(chrono::NaiveTime::MIN)));
            w.leaf("ToDate", codes::datetime_text(end.and_hms_opt(23, 59, 0).unwrap_or(end.and_time(chrono::NaiveTime::MIN))));
            w.close("TimePeriod");
            w.leaf("Occurrences", 1);
            w.leaf("Name", &e.name);
            w.leaf("Type", 1);
            w.leaf("Period", 1);
            times(w, &e.working);
            w.close("Exception");
        }
    }
    w.close("Exceptions");
}

fn shift_year(d: chrono::NaiveDate, years: i32) -> Option<chrono::NaiveDate> {
    let y = d.year().checked_add(years)?;
    d.with_year(y).or_else(|| chrono::NaiveDate::from_ymd_opt(y, d.month(), 28))
}

fn task(
    w: &mut Writer,
    p: &Project,
    row: usize,
    t: &Task,
    slots: &HashMap<CustomFieldId, (u32, CustomFieldKind)>,
    note: &mut impl FnMut(&'static str),
) {
    let s = &p.settings;
    w.open("Task");
    w.leaf("UID", t.id.0);
    w.leaf("ID", row + 1);
    w.leaf("Name", &t.name);
    w.leaf("Type", codes::task_type_code(t.task_type));
    if !t.active {
        w.leaf("Active", 0);
    }
    if t.mode == ScheduleMode::Manual {
        w.leaf("Manual", 1);
    }
    w.leaf("WBS", p.wbs(row));
    w.leaf("OutlineNumber", p.wbs(row));
    w.leaf("OutlineLevel", t.outline_level);
    w.leaf("Priority", t.priority);
    if let Some(v) = t.sched.start {
        w.leaf("Start", codes::datetime_text(v));
    }
    if let Some(v) = t.sched.finish {
        w.leaf("Finish", codes::datetime_text(v));
    }
    let duration = if t.sched.is_summary { t.sched.duration } else { t.duration };
    w.leaf("Duration", codes::duration_text(duration));
    w.leaf("DurationFormat", codes::format_code(s.duration_unit, false, t.estimated));
    w.leaf("Work", codes::duration_text(t.sched.work));
    if let Some(v) = t.sched.stop {
        w.leaf("Stop", codes::datetime_text(v));
    }
    if let Some(v) = t.sched.resume {
        w.leaf("Resume", codes::datetime_text(v));
    }
    w.leaf("EffortDriven", u8::from(t.effort_driven));
    w.leaf("Recurring", u8::from(t.recurrence.is_some()));
    w.leaf("Estimated", u8::from(t.estimated));
    w.leaf("Milestone", u8::from(t.is_milestone()));
    w.leaf("Summary", u8::from(t.sched.is_summary));
    w.leaf("Critical", u8::from(t.sched.critical));
    if t.fixed_cost != 0.0 {
        w.leaf("FixedCost", t.fixed_cost);
    }
    w.leaf("PercentComplete", t.percent_complete);
    w.leaf("Cost", t.sched.cost);
    if let Some(v) = t.actual_start {
        w.leaf("ActualStart", codes::datetime_text(v));
    }
    if let Some(v) = t.actual_finish {
        w.leaf("ActualFinish", codes::datetime_text(v));
    }
    let (done, left) = t.duration_progress();
    w.leaf("ActualDuration", codes::duration_text(done));
    w.leaf("ActualWork", codes::duration_text(t.sched.actual_work));
    w.leaf("RemainingDuration", codes::duration_text(left));
    w.leaf("ConstraintType", codes::constraint_code(t.constraint.kind));
    w.leaf("CalendarUID", t.calendar.map_or(-1, |c| i64::from(c.0)));
    if let Some(d) = t.constraint.date.filter(|_| t.constraint.kind.needs_date()) {
        w.leaf("ConstraintDate", codes::datetime_text(d));
    }
    if let Some(d) = t.deadline {
        w.leaf("Deadline", codes::datetime_text(d));
    }
    if t.leveling_delay > 0 {
        // In the unit of LevelingDelayFormat (days).
        let days = t.leveling_delay as f64 / s.time.minutes_per_day.max(1) as f64;
        w.leaf("LevelingDelay", days);
        w.leaf("LevelingDelayFormat", 7);
    }
    w.leaf("IgnoreResourceCalendar", u8::from(t.ignore_resource_calendars));
    if !t.notes.is_empty() {
        w.leaf("Notes", &t.notes);
    }
    for d in &t.predecessors {
        let TaskRef::Local(pred) = d.predecessor else {
            note("links to tasks in other projects were left out");
            continue;
        };
        w.open("PredecessorLink");
        w.leaf("PredecessorUID", pred.0);
        w.leaf("Type", codes::link_type_code(d.kind));
        w.leaf("CrossProject", 0);
        // LinkLag is in tenths of a minute (a percent lag in tenths of a percent).
        let (lag, format) = match d.lag {
            Lag::None => (0, codes::format_code(s.duration_unit, false, false)),
            Lag::Working(m) => (m.saturating_mul(10), codes::format_code(best_unit(m, &s.time), false, false)),
            Lag::Elapsed(m) => (m.saturating_mul(10), codes::format_code(best_unit(m, &s.time), true, false)),
            Lag::Percent(pc) => (i64::from(pc).saturating_mul(10), 19),
        };
        w.leaf("LinkLag", lag);
        w.leaf("LagFormat", format);
        w.close("PredecessorLink");
    }
    // A milestone is reached when its member tasks finish: links from each member to it.
    for m in &t.members {
        let TaskRef::Local(member) = *m else {
            note("links to tasks in other projects were left out");
            continue;
        };
        if member == t.id || t.predecessors.iter().any(|d| d.predecessor == TaskRef::Local(member)) {
            continue;
        }
        w.open("PredecessorLink");
        w.leaf("PredecessorUID", member.0);
        w.leaf("Type", codes::link_type_code(LinkType::FS));
        w.leaf("CrossProject", 0);
        w.leaf("LinkLag", 0);
        w.leaf("LagFormat", codes::format_code(s.duration_unit, false, false));
        w.close("PredecessorLink");
    }
    for (field, value) in &t.custom {
        let Some((fid, kind)) = slots.get(field) else { continue };
        let text = match value {
            CustomValue::Text(s) => s.clone(),
            CustomValue::Number(n) => n.to_string(),
            CustomValue::Date(d) => codes::datetime_text(*d),
            CustomValue::Duration(m) => codes::duration_text(*m),
            CustomValue::Flag(b) => u8::from(*b).to_string(),
        };
        w.open("ExtendedAttribute");
        w.leaf("FieldID", fid);
        w.leaf("Value", text);
        if *kind == CustomFieldKind::Duration {
            w.leaf("DurationFormat", codes::format_code(s.duration_unit, false, false));
        }
        w.close("ExtendedAttribute");
    }
    for (n, i) in &t.interim {
        let Some((start_id, finish_id)) = codes::interim_ids(*n) else { continue };
        for (fid, v) in [(start_id, i.start), (finish_id, i.finish)] {
            w.open("ExtendedAttribute");
            w.leaf("FieldID", fid);
            w.leaf("Value", codes::datetime_text(v));
            w.close("ExtendedAttribute");
        }
    }
    let baselines = t.baseline.iter().map(|b| (0u8, b)).chain(t.baselines.iter().map(|(n, b)| (*n, b)));
    for (n, b) in baselines {
        w.open("Baseline");
        w.leaf("Number", n);
        w.leaf("Interim", 0);
        w.leaf("Start", codes::datetime_text(b.start));
        w.leaf("Finish", codes::datetime_text(b.finish));
        w.leaf("Duration", codes::duration_text(b.duration));
        w.leaf("DurationFormat", codes::format_code(s.duration_unit, false, false));
        w.leaf("Work", codes::duration_text(b.work));
        w.leaf("Cost", b.cost);
        w.close("Baseline");
    }
    w.close("Task");
}
