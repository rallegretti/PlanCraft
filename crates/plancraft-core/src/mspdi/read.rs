//! MSPDI → PlanCraft. Values outside PlanCraft's limits are clamped by [`crate::limits::sanitize`]
//! afterwards; what cannot be represented is described in the returned notes.

use std::collections::{BTreeMap, HashMap, HashSet};

use chrono::{Datelike, Days, Months, NaiveDate, NaiveDateTime, Weekday};

use super::codes::{self, Slot};
use super::xml::{self, Element, Item};
use crate::calendar::{Calendar, CalendarException, TimeRange, WorkWeek, normalize_ranges};
use crate::custom::{CustomFieldDef, CustomFieldKind, CustomValue};
use crate::duration::{DurationUnit, Lag, Minutes};
use crate::ids::*;
use crate::limits::{self, MAX_ASSIGNMENTS, MAX_CALENDARS, MAX_ID, MAX_RESOURCES, MAX_TASKS};
use crate::model::*;

/// Most calendar exceptions made from one recurring exception or work week.
const MAX_EXPANDED: usize = 5_000;
/// Most custom field definitions read.
const MAX_DEFINITIONS: usize = 10_000;
/// A yearly exception repeating at least this many times is kept as "every year".
const YEARLY_FOREVER: i64 = 50;

/// What could not be carried over, counted by kind.
#[derive(Default)]
struct Notes(BTreeMap<String, usize>);

impl Notes {
    fn add(&mut self, msg: impl Into<String>) {
        *self.0.entry(msg.into()).or_insert(0) += 1;
    }

    fn into_vec(self) -> Vec<String> {
        self.0.into_iter().map(|(m, n)| if n > 1 { format!("{m} ({n} times)") } else { m }).collect()
    }
}

/// The records of a document, collected before they are converted.
#[derive(Default)]
struct Records {
    scalars: HashMap<String, String>,
    calendars: Vec<Element>,
    tasks: Vec<Element>,
    resources: Vec<Element>,
    assignments: Vec<Element>,
    definitions: Vec<Element>,
}

fn collect(text: &str) -> Result<Records, String> {
    let mut r = Records::default();
    xml::read(text, |item| {
        match item {
            Item::Scalar(name, value) => {
                r.scalars.entry(name).or_insert(value);
            }
            Item::Record { collection, element } => {
                let (list, max, what) = match (collection.as_str(), element.name.as_str()) {
                    ("Calendars", "Calendar") => (&mut r.calendars, MAX_CALENDARS + MAX_RESOURCES, "calendars"),
                    ("Tasks", "Task") => (&mut r.tasks, MAX_TASKS + 1, "tasks"),
                    ("Resources", "Resource") => (&mut r.resources, MAX_RESOURCES + 1, "resources"),
                    ("Assignments", "Assignment") => (&mut r.assignments, MAX_ASSIGNMENTS, "assignments"),
                    ("ExtendedAttributes", "ExtendedAttribute") => (&mut r.definitions, MAX_DEFINITIONS, "custom field definitions"),
                    _ => return Ok(()),
                };
                if list.len() >= max {
                    return Err(format!("the file has too many {what} (the maximum is {max})"));
                }
                list.push(element);
            }
        }
        Ok(())
    })?;
    Ok(r)
}

fn int(e: &Element, name: &str) -> Option<i64> {
    e.text_of(name).and_then(codes::int_of)
}
fn num(e: &Element, name: &str) -> Option<f64> {
    e.text_of(name).and_then(codes::num_of)
}
fn flag(e: &Element, name: &str) -> Option<bool> {
    e.text_of(name).and_then(codes::bool_of)
}
fn when(e: &Element, name: &str) -> Option<NaiveDateTime> {
    e.text_of(name).and_then(codes::datetime_of)
}
fn span(e: &Element, name: &str) -> Option<Minutes> {
    e.text_of(name).and_then(codes::duration_of)
}
fn text(e: &Element, name: &str) -> String {
    e.text_of(name).unwrap_or_default().to_string()
}

/// Converts a document. `fallback_name` names the plan when the file gives no name.
pub fn import(text: &str, fallback_name: &str) -> Result<(Project, Vec<String>), String> {
    let r = collect(text)?;
    let mut notes = Notes::default();
    let sc = |k: &str| r.scalars.get(k).map(|v| v.trim()).filter(|v| !v.is_empty());

    let name = sc("Title").or(sc("Name")).unwrap_or(fallback_name).to_string();
    let first_task_start = r.tasks.iter().filter_map(|t| when(t, "Start")).min();
    let start = sc("StartDate").and_then(codes::datetime_of).or(first_task_start).unwrap_or_else(|| chrono::Local::now().naive_local());
    let mut p = Project::new(name, limits::clamp_date(start.date()));
    p.start = limits::clamp_datetime(start);
    p.author = sc("Author").unwrap_or_default().to_string();
    p.manager = sc("Manager").unwrap_or_default().to_string();
    p.company = sc("Company").unwrap_or_default().to_string();
    p.status_date = sc("StatusDate").and_then(codes::datetime_of).map(|d| d.date());
    if sc("ScheduleFromStart").and_then(codes::bool_of) == Some(false) {
        notes.add("the plan was scheduled from its finish date; PlanCraft schedules it from its start date");
    }

    let time = &mut p.settings.time;
    if let Some(v) = sc("MinutesPerDay").and_then(codes::int_of).filter(|v| *v > 0) {
        time.minutes_per_day = v;
    }
    if let Some(v) = sc("MinutesPerWeek").and_then(codes::int_of).filter(|v| *v > 0) {
        time.minutes_per_week = v;
    }
    if let Some(v) = sc("DaysPerMonth").and_then(codes::int_of).filter(|v| *v > 0) {
        time.days_per_month = v;
    }
    if let Some(v) = sc("DefaultStartTime").and_then(codes::time_of).filter(|v| *v < 1440) {
        time.default_start_time = v;
    }
    if let Some(v) = sc("DefaultFinishTime").and_then(codes::time_of).filter(|v| *v > 0) {
        time.default_finish_time = v;
    }
    if let Some(sym) = sc("CurrencySymbol") {
        p.settings.currency = sym.to_string();
    }
    if let Some(unit) = sc("DurationFormat").and_then(codes::int_of).and_then(codes::format_of).and_then(|f| f.unit) {
        p.settings.duration_unit = unit;
    }
    if let Some(days) = sc("CriticalSlackLimit").and_then(codes::int_of) {
        p.settings.critical_slack = days.clamp(0, 3650).saturating_mul(p.settings.time.minutes_per_day);
    }
    if let Some(n) = sc("BaselineForEarnedValue").and_then(codes::int_of).and_then(|n| u8::try_from(n).ok()).filter(|n| *n <= 10) {
        p.settings.earned_value_baseline = n;
    }

    let cals = calendars(&mut p, &r.calendars, &mut notes);
    if let Some(id) = sc("CalendarUID").and_then(codes::int_of).and_then(|u| cals.base_of(u)) {
        p.calendar = id;
    }
    let fields = definitions(&mut p, &r.definitions, &mut notes);
    let links = tasks(&mut p, &r.tasks, &cals, fields, &mut notes);
    let resources = resources(&mut p, &r.resources, &cals, &mut notes);
    assignments(&mut p, &r.assignments, &links.ids, &resources, &mut notes);
    predecessors(&mut p, links, &mut notes);

    p.ids.task = p.tasks.iter().map(|t| t.id.0).max().unwrap_or(0);
    p.ids.resource = p.resources.iter().map(|r| r.id.0).max().unwrap_or(0);
    p.ids.assignment = p.assignments.iter().map(|a| a.id.0).max().unwrap_or(0);
    p.ids.calendar = p.calendars.iter().map(|c| c.id.0).max().unwrap_or(0);
    limits::check_counts(&p).map_err(|e| e.to_string())?;
    let mut out = notes.into_vec();
    out.extend(limits::sanitize(&mut p));
    Ok((p, out))
}

// ---------------------------------------------------------------------------
// Ids
// ---------------------------------------------------------------------------

/// Gives each record an id: its UID when that is a usable, unused id, otherwise a fresh one
/// above every UID. Returns the ids in record order and the UID → id map (first record wins).
fn assign_ids(uids: &[Option<i64>]) -> (Vec<u32>, HashMap<i64, u32>) {
    let valid = |u: i64| u32::try_from(u).ok().filter(|v| (1..=MAX_ID).contains(v));
    let mut next = uids.iter().filter_map(|u| u.and_then(valid)).max().unwrap_or(0);
    let mut used = HashSet::new();
    let mut map = HashMap::new();
    let mut ids = Vec::with_capacity(uids.len());
    for uid in uids {
        let id = match uid.and_then(valid) {
            Some(v) if used.insert(v) => v,
            _ => {
                next = next.saturating_add(1);
                used.insert(next);
                next
            }
        };
        if let Some(u) = uid {
            map.entry(*u).or_insert(id);
        }
        ids.push(id);
    }
    (ids, map)
}

// ---------------------------------------------------------------------------
// Calendars
// ---------------------------------------------------------------------------

/// A calendar based on another (resource calendars): what it changes.
struct Derived {
    name: String,
    base: Option<i64>,
    days: Vec<(Weekday, Vec<TimeRange>)>,
    exceptions: Vec<CalendarException>,
}

#[derive(Default)]
struct Calendars {
    base: HashMap<i64, CalendarId>,
    derived: HashMap<i64, Derived>,
}

impl Calendars {
    /// The PlanCraft calendar for a UID: a base calendar, or the base of a derived one.
    fn base_of(&self, uid: i64) -> Option<CalendarId> {
        self.base.get(&uid).copied().or_else(|| self.derived.get(&uid).and_then(|d| d.base).and_then(|b| self.base.get(&b).copied()))
    }
}

fn standard_day(d: Weekday) -> Vec<TimeRange> {
    Calendar::standard(CalendarId(0)).week.day(d).clone()
}

const WEEK: [Weekday; 7] = [Weekday::Mon, Weekday::Tue, Weekday::Wed, Weekday::Thu, Weekday::Fri, Weekday::Sat, Weekday::Sun];

/// The working times of a `WorkingTimes` element; `None` when it has none.
fn working_times(e: &Element, notes: &mut Notes) -> Option<Vec<TimeRange>> {
    let times = e.child("WorkingTimes")?;
    let mut out = Vec::new();
    for t in times.all("WorkingTime") {
        let (Some(from), Some(to)) = (t.text_of("FromTime").and_then(codes::time_of), t.text_of("ToTime").and_then(codes::time_of)) else {
            continue;
        };
        // A period ending at midnight is written 00:00:00.
        let to = if to == 0 { 1440 } else { to };
        if to <= from {
            notes.add("a working time ends before it starts and was left out");
            continue;
        }
        out.push(TimeRange::new(from, to));
    }
    normalize_ranges(&mut out);
    Some(out)
}

/// A day's working time from a `WeekDay` or exception: non-working, the listed times, or `None`
/// (working, times not given).
fn day_ranges(e: &Element, notes: &mut Notes) -> Option<Vec<TimeRange>> {
    match flag(e, "DayWorking") {
        Some(false) => Some(Vec::new()),
        _ => working_times(e, notes).filter(|r| !r.is_empty()),
    }
}

fn calendars(p: &mut Project, records: &[Element], notes: &mut Notes) -> Calendars {
    let mut out = Calendars::default();
    if records.is_empty() {
        return out;
    }
    let is_base = |c: &Element| flag(c, "IsBaseCalendar").unwrap_or_else(|| int(c, "BaseCalendarUID").is_none_or(|b| b < 0));
    // Base calendars first, so derived ones can refer to them.
    let base: Vec<&Element> = records.iter().filter(|c| is_base(c)).take(MAX_CALENDARS).collect();
    if base.len() < records.iter().filter(|c| is_base(c)).count() {
        notes.add(format!("only the first {MAX_CALENDARS} base calendars were imported"));
    }
    if base.is_empty() {
        return out;
    }
    p.calendars.clear();
    p.ids.calendar = 0;
    for c in base {
        let id = p.ids.next_calendar();
        let name = c.text_of("Name").map_or_else(|| format!("Calendar {}", id.0), str::to_string);
        let mut week = WorkWeek::default();
        for d in WEEK {
            *week.day_mut(d) = standard_day(d);
        }
        let mut exceptions = Vec::new();
        read_week(c, &mut week, &mut exceptions, notes);
        read_exceptions(c, &mut exceptions, notes);
        if let Some(uid) = int(c, "UID") {
            out.base.entry(uid).or_insert(id);
        }
        p.calendars.push(Calendar { id, name, week, exceptions });
    }
    for c in records.iter().filter(|c| !is_base(c)) {
        let Some(uid) = int(c, "UID") else { continue };
        let base = int(c, "BaseCalendarUID");
        let mut days = Vec::new();
        let mut exceptions = Vec::new();
        let base_week = base.and_then(|b| out.base.get(&b)).and_then(|id| p.calendar(*id)).map(|c| c.week.clone()).unwrap_or_default();
        let mut week = base_week.clone();
        read_week(c, &mut week, &mut exceptions, notes);
        for d in WEEK {
            if week.day(d) != base_week.day(d) {
                days.push((d, week.day(d).clone()));
            }
        }
        read_exceptions(c, &mut exceptions, notes);
        out.derived.entry(uid).or_insert(Derived { name: text(c, "Name"), base, days, exceptions });
    }
    out
}

/// Applies a calendar's `WeekDays` (DayType 1–7; DayType 0 is an exception) and `WorkWeeks`.
fn read_week(c: &Element, week: &mut WorkWeek, exceptions: &mut Vec<CalendarException>, notes: &mut Notes) {
    if let Some(days) = c.child("WeekDays") {
        for wd in days.all("WeekDay") {
            match int(wd, "DayType") {
                Some(0) => {
                    let Some(period) = wd.child("TimePeriod") else { continue };
                    let (Some(from), Some(to)) = (when(period, "FromDate"), when(period, "ToDate")) else { continue };
                    let working = day_ranges(wd, notes).unwrap_or_default();
                    let (from, to) = (from.date(), to.date().max(from.date()));
                    exceptions.push(CalendarException { name: "Exception".into(), start: from, end: to, working, yearly: false });
                }
                Some(code) => {
                    let Some(d) = codes::weekday_of(code) else { continue };
                    if let Some(ranges) = day_ranges(wd, notes) {
                        *week.day_mut(d) = ranges;
                    }
                }
                None => {}
            }
        }
    }
    // Work weeks change the week for a period: written as dated exceptions on the days that differ.
    if let Some(weeks) = c.child("WorkWeeks") {
        let mut days: Vec<(NaiveDate, Vec<TimeRange>, String)> = Vec::new();
        for ww in weeks.all("WorkWeek") {
            let Some(period) = ww.child("TimePeriod") else { continue };
            let (Some(from), Some(to)) = (when(period, "FromDate"), when(period, "ToDate")) else { continue };
            let name = ww.text_of("Name").unwrap_or("Work week").to_string();
            let mut changed: Vec<(Weekday, Vec<TimeRange>)> = Vec::new();
            for wd in ww.all("WeekDay") {
                let Some(d) = int(wd, "DayType").and_then(codes::weekday_of) else { continue };
                if let Some(ranges) = day_ranges(wd, notes) {
                    changed.push((d, ranges));
                }
            }
            let mut d = from.date();
            while d <= to.date() {
                if let Some((_, ranges)) = changed.iter().find(|(w, _)| *w == d.weekday())
                    && ranges != week.day(d.weekday())
                {
                    if days.len() >= MAX_EXPANDED {
                        notes.add("a work week covers too many days; only the first were imported");
                        break;
                    }
                    days.push((d, ranges.clone(), name.clone()));
                }
                let Some(next) = d.checked_add_days(Days::new(1)) else { break };
                d = next;
            }
        }
        exceptions.extend(merge_days(days));
    }
}

/// Joins consecutive days with the same name and working time into one exception each.
fn merge_days(mut days: Vec<(NaiveDate, Vec<TimeRange>, String)>) -> Vec<CalendarException> {
    days.sort_by_key(|(d, _, _)| *d);
    let mut out: Vec<CalendarException> = Vec::new();
    for (d, working, name) in days {
        if let Some(last) = out.last_mut()
            && last.name == name
            && last.working == working
            && last.end.checked_add_days(Days::new(1)) == Some(d)
        {
            last.end = d;
            continue;
        }
        out.push(CalendarException { name, start: d, end: d, working, yearly: false });
    }
    out
}

/// The nth (0–3) or last (4) given weekday of a month.
fn nth_weekday(year: i32, month: u32, nth: i64, weekday: Weekday) -> Option<NaiveDate> {
    if nth >= 4 {
        let first_next =
            if month == 12 { NaiveDate::from_ymd_opt(year.checked_add(1)?, 1, 1)? } else { NaiveDate::from_ymd_opt(year, month + 1, 1)? };
        let mut d = first_next.pred_opt()?;
        while d.weekday() != weekday {
            d = d.pred_opt()?;
        }
        return Some(d);
    }
    NaiveDate::from_weekday_of_month_opt(year, month, weekday, u8::try_from(nth + 1).ok()?)
}

/// `MonthItem` 3–9 is Sunday–Saturday.
fn month_item_weekday(code: i64) -> Option<Weekday> {
    codes::weekday_of(code - 2)
}

/// The `Exceptions` of a calendar (Project 2007 and later), recurring ones expanded into days.
fn read_exceptions(c: &Element, out: &mut Vec<CalendarException>, notes: &mut Notes) {
    let Some(list) = c.child("Exceptions") else { return };
    for ex in list.all("Exception") {
        let Some(period) = ex.child("TimePeriod") else { continue };
        let Some(from) = when(period, "FromDate").map(|d| d.date()) else { continue };
        let to = when(period, "ToDate").map_or(from, |d| d.date()).max(from);
        let name = ex.text_of("Name").unwrap_or("Exception").to_string();
        let working = day_ranges(ex, notes).unwrap_or_default();
        let by_count = flag(ex, "EnteredByOccurrences").unwrap_or(false);
        let count = int(ex, "Occurrences").filter(|n| *n > 0);
        let period_n = int(ex, "Period").filter(|n| *n > 0).unwrap_or(1);
        let kind = int(ex, "Type").unwrap_or(1);
        let single = |out: &mut Vec<CalendarException>| {
            out.push(CalendarException { name: name.clone(), start: from, end: to, working: working.clone(), yearly: false });
        };
        // A yearly exception repeating for decades is kept as "every year".
        if kind == 2 {
            let years = if by_count { count.unwrap_or(1) } else { i64::from(to.year() - from.year()) + 1 };
            if years >= YEARLY_FOREVER && period_n == 1 {
                out.push(CalendarException { name, start: from, end: from, working, yearly: true });
                continue;
            }
        }
        let limit = if by_count { usize::try_from(count.unwrap_or(1)).unwrap_or(MAX_EXPANDED) } else { MAX_EXPANDED };
        let mut dates: Vec<NaiveDate> = Vec::new();
        let in_range = |d: NaiveDate, n: usize| if by_count { n < limit } else { d <= to };
        match kind {
            // Daily every `Period` days (one day a time, or the whole period when daily).
            1 | 7 | 8 | 9 => {
                if period_n == 1 && !by_count {
                    single(out);
                    continue;
                }
                let mut d = from;
                while in_range(d, dates.len()) && dates.len() < MAX_EXPANDED {
                    dates.push(d);
                    let Some(next) = d.checked_add_days(Days::new(period_n.unsigned_abs())) else { break };
                    d = next;
                }
            }
            // Weekly every `Period` weeks, on the `DaysOfWeek` (1=Sunday … 64=Saturday).
            6 => {
                let mask = int(ex, "DaysOfWeek").unwrap_or(0);
                let Some(mut week_start) = from.checked_sub_days(Days::new(u64::from(from.weekday().num_days_from_sunday()))) else {
                    continue;
                };
                'weeks: while dates.len() < MAX_EXPANDED {
                    for i in 0..7u64 {
                        let Some(d) = week_start.checked_add_days(Days::new(i)) else { break 'weeks };
                        if d < from || mask & (1 << d.weekday().num_days_from_sunday()) == 0 {
                            continue;
                        }
                        if !in_range(d, dates.len()) {
                            break 'weeks;
                        }
                        dates.push(d);
                    }
                    let Some(next) = week_start.checked_add_days(Days::new(7 * period_n.unsigned_abs())) else { break };
                    week_start = next;
                }
            }
            // Monthly and yearly, by day of the month or by position.
            2..=5 => {
                let yearly = kind <= 3;
                let by_day = kind == 2 || kind == 4;
                let step = if yearly { 12 * period_n.unsigned_abs() } else { period_n.unsigned_abs() };
                let month0 = if yearly {
                    int(ex, "Month").and_then(|m| u32::try_from(m).ok()).filter(|m| *m < 12).map_or(from.month(), |m| m + 1)
                } else {
                    from.month()
                };
                let Some(mut first) = NaiveDate::from_ymd_opt(from.year(), month0, 1) else { continue };
                let item = int(ex, "MonthItem").unwrap_or(0);
                let weekday = month_item_weekday(item);
                if !by_day && weekday.is_none() && item != 0 {
                    notes.add("a calendar exception repeating on a weekday or weekend day of the month was imported once");
                    single(out);
                    continue;
                }
                let mut guard = 0;
                while dates.len() < MAX_EXPANDED && guard < MAX_EXPANDED {
                    guard += 1;
                    let d = if by_day {
                        int(ex, "MonthDay")
                            .and_then(|md| u32::try_from(md).ok())
                            .and_then(|md| NaiveDate::from_ymd_opt(first.year(), first.month(), md))
                    } else {
                        let pos = int(ex, "MonthPosition").unwrap_or(0);
                        match weekday {
                            Some(w) => nth_weekday(first.year(), first.month(), pos, w),
                            // MonthItem 0: the nth day of the month (last: the month's last day).
                            None => {
                                if pos >= 4 {
                                    last_day(first)
                                } else {
                                    first.checked_add_days(Days::new(pos.unsigned_abs()))
                                }
                            }
                        }
                    };
                    if let Some(d) = d
                        && d >= from
                    {
                        if !in_range(d, dates.len()) {
                            break;
                        }
                        dates.push(d);
                    } else if first > to && !by_count {
                        break;
                    }
                    let Some(next) = first.checked_add_months(Months::new(u32::try_from(step).unwrap_or(u32::MAX))) else { break };
                    first = next;
                }
            }
            _ => {
                single(out);
                continue;
            }
        }
        if dates.len() >= MAX_EXPANDED {
            notes.add("a recurring calendar exception repeats too often; only its first occurrences were imported");
        }
        out.extend(merge_days(dates.into_iter().map(|d| (d, working.clone(), name.clone())).collect()));
    }
}

fn last_day(first: NaiveDate) -> Option<NaiveDate> {
    first.checked_add_months(Months::new(1))?.pred_opt()
}

// ---------------------------------------------------------------------------
// Custom fields
// ---------------------------------------------------------------------------

/// The custom fields made from extended attribute definitions, by field id.
#[derive(Default)]
struct Fields {
    by_id: HashMap<u32, (CustomFieldId, CustomFieldKind)>,
    /// Value lists: field id → value id → text.
    lists: HashMap<u32, HashMap<i64, String>>,
}

fn field_id_of(e: &Element) -> Option<u32> {
    e.text_of("FieldID").and_then(|f| f.parse::<u32>().ok())
}

fn definitions(p: &mut Project, defs: &[Element], notes: &mut Notes) -> Fields {
    let mut out = Fields::default();
    for d in defs {
        let Some(fid) = field_id_of(d) else { continue };
        let Some(slot) = codes::slot_of(fid) else {
            if codes::interim_of(fid).is_none() {
                notes.add("custom fields other than task Text, Number, Cost, Date, Duration and Flag fields were not imported");
            }
            continue;
        };
        if out.by_id.contains_key(&fid) {
            continue;
        }
        if p.custom_fields.len() >= crate::custom::MAX_CUSTOM_FIELDS {
            notes.add("there were too many custom fields; the rest were not imported");
            break;
        }
        let name = d.text_of("Alias").or(d.text_of("FieldName")).map_or_else(|| slot.field_name(), str::to_string);
        let mut list = HashMap::new();
        let mut choices = Vec::new();
        if let Some(values) = d.child("ValueList") {
            for v in values.all("Value") {
                let Some(t) = v.text_of("Value") else { continue };
                if let Some(id) = int(v, "ID") {
                    list.insert(id, t.to_string());
                }
                if slot.kind == CustomFieldKind::Text && flag(d, "RestrictValues").unwrap_or(true) {
                    choices.push(t.to_string());
                }
            }
        }
        let id = p.ids.next_custom_field();
        p.custom_fields.push(CustomFieldDef { id, name, kind: slot.kind, choices });
        out.by_id.insert(fid, (id, slot.kind));
        out.lists.insert(fid, list);
    }
    out
}

fn custom_value(kind: CustomFieldKind, raw: &str) -> Option<CustomValue> {
    Some(match kind {
        CustomFieldKind::Text => CustomValue::Text(raw.to_string()),
        CustomFieldKind::Number | CustomFieldKind::Cost => CustomValue::Number(codes::num_of(raw)?),
        CustomFieldKind::Date => CustomValue::Date(codes::datetime_of(raw)?),
        CustomFieldKind::Duration => {
            CustomValue::Duration(codes::duration_of(raw).or_else(|| codes::num_of(raw).map(|v| v.round() as Minutes))?)
        }
        CustomFieldKind::Flag => CustomValue::Flag(codes::bool_of(raw)?),
    })
}

// ---------------------------------------------------------------------------
// Tasks
// ---------------------------------------------------------------------------

struct RawLink {
    successor: TaskId,
    predecessor: i64,
    kind: LinkType,
    lag: Lag,
}

struct Links {
    ids: HashMap<i64, TaskId>,
    links: Vec<RawLink>,
}

/// Minutes of `value` in `format`'s unit (working time; elapsed units use 24-hour days).
fn amount(value: f64, format: codes::Format, p: &Project) -> Option<Minutes> {
    let unit = format.unit?;
    let per = if format.elapsed {
        match unit {
            DurationUnit::Minutes => 1,
            DurationUnit::Hours => 60,
            DurationUnit::Days => 1440,
            DurationUnit::Weeks => 7 * 1440,
            DurationUnit::Months => 30 * 1440,
        }
    } else {
        unit.minutes(&p.settings.time)
    };
    let v = (value * per as f64).round();
    v.is_finite().then(|| v.clamp(-(limits::MAX_MINUTES as f64), limits::MAX_MINUTES as f64) as Minutes)
}

fn tasks(p: &mut Project, records: &[Element], cals: &Calendars, mut fields: Fields, notes: &mut Notes) -> Links {
    // Blank rows and the project summary task (outline level 0) are not tasks.
    let rows: Vec<&Element> = records
        .iter()
        .filter(|t| {
            if flag(t, "IsNull") == Some(true) {
                notes.add("blank task rows were left out");
                return false;
            }
            match int(t, "OutlineLevel") {
                Some(level) => level > 0,
                None => int(t, "UID") != Some(0),
            }
        })
        .collect();
    let uids: Vec<Option<i64>> = rows.iter().map(|t| int(t, "UID")).collect();
    let (ids, map) = assign_ids(&uids);
    let mut links = Vec::new();
    let mut prev_level = 0u16;
    let now = chrono::Local::now().naive_local();
    let mpd = p.settings.time.minutes_per_day;
    for (t, id) in rows.iter().zip(ids) {
        let id = TaskId(id);
        let mut task = Task::new(id, text(t, "Name"), mpd);
        let level = int(t, "OutlineLevel").unwrap_or(1).clamp(1, i64::from(limits::MAX_OUTLINE_LEVEL));
        let level = u16::try_from(level).unwrap_or(1).min(prev_level.saturating_add(1));
        task.outline_level = level;
        prev_level = level;

        let format = int(t, "DurationFormat").and_then(codes::format_of);
        if format.is_some_and(|f| f.elapsed) {
            notes.add("elapsed durations were imported as working time");
        }
        task.duration = span(t, "Duration").unwrap_or(0).max(0);
        task.estimated = flag(t, "Estimated").unwrap_or(false) || format.is_some_and(|f| f.estimated);
        if flag(t, "Milestone") == Some(true) {
            if task.duration == 0 {
                task.kind = TaskKind::Milestone;
                task.estimated = false;
            } else {
                notes.add("tasks marked as milestones with a duration were imported as ordinary tasks");
            }
        }
        if flag(t, "Manual") == Some(true) {
            task.mode = ScheduleMode::Manual;
            task.manual_start = when(t, "Start");
            task.manual_finish = when(t, "Finish");
        } else {
            task.mode = ScheduleMode::Auto;
        }
        task.active = flag(t, "Active").unwrap_or(true);
        if let Some(tt) = int(t, "Type").and_then(codes::task_type_of) {
            task.task_type = tt;
        }
        task.effort_driven = flag(t, "EffortDriven").unwrap_or(false);
        if let Some(pr) = int(t, "Priority") {
            task.priority = u16::try_from(pr.clamp(0, i64::from(limits::MAX_PRIORITY))).unwrap_or(500);
        }
        if let Some(kind) = int(t, "ConstraintType").and_then(codes::constraint_of) {
            let date = when(t, "ConstraintDate");
            if kind.needs_date() && date.is_none() {
                notes.add("a constraint without a date was left out");
            } else {
                task.constraint = Constraint { kind, date: if kind.needs_date() { date } else { None } };
            }
        }
        task.deadline = when(t, "Deadline");
        if let Some(uid) = int(t, "CalendarUID").filter(|u| *u >= 0) {
            task.calendar = cals.base_of(uid);
        }
        task.ignore_resource_calendars = flag(t, "IgnoreResourceCalendar").unwrap_or(false);
        task.percent_complete = u8::try_from(int(t, "PercentComplete").unwrap_or(0).clamp(0, 100)).unwrap_or(0);
        task.actual_start = when(t, "ActualStart");
        task.actual_finish = when(t, "ActualFinish");
        task.notes = text(t, "Notes");
        task.fixed_cost = num(t, "FixedCost").unwrap_or(0.0);
        if let Some(delay) = num(t, "LevelingDelay").filter(|d| *d != 0.0) {
            let fmt = int(t, "LevelingDelayFormat").and_then(codes::format_of).unwrap_or(codes::Format {
                unit: Some(DurationUnit::Days),
                elapsed: false,
                estimated: false,
            });
            if fmt.elapsed {
                notes.add("elapsed leveling delays were imported as working time");
            }
            let fmt = codes::Format { elapsed: false, ..fmt };
            task.leveling_delay = amount(delay, fmt, p).unwrap_or(0).max(0);
        }
        if flag(t, "Recurring") == Some(true) && level > 0 {
            notes.add("recurring tasks were imported as ordinary tasks (the repeat pattern is not in the file)");
        }

        // Baselines (Number 0–10) and interim plans.
        let mut interim: BTreeMap<u8, (Option<NaiveDateTime>, Option<NaiveDateTime>)> = BTreeMap::new();
        for b in t.all("Baseline") {
            let Some(n) = int(b, "Number").and_then(|n| u8::try_from(n).ok()).filter(|n| *n <= 10) else { continue };
            let (start, finish) = (when(b, "Start"), when(b, "Finish"));
            if flag(b, "Interim") == Some(true) {
                if n >= 1 {
                    interim.insert(n, (start, finish));
                }
                continue;
            }
            let (Some(start), Some(finish)) = (start, finish) else {
                notes.add("a baseline without start and finish dates was left out");
                continue;
            };
            let saved = Baseline {
                start,
                finish,
                duration: span(b, "Duration").unwrap_or(0).max(0),
                work: span(b, "Work").unwrap_or(0).max(0),
                cost: num(b, "Cost").unwrap_or(0.0),
                saved: now,
            };
            if n == 0 {
                task.baseline = Some(saved);
            } else {
                task.baselines.insert(n, saved);
            }
        }

        // Custom fields and the interim plan fields (Start1–10, Finish1–10).
        for ea in t.all("ExtendedAttribute") {
            let Some(fid) = field_id_of(ea) else { continue };
            if let Some((n, is_start)) = codes::interim_of(fid) {
                let v = ea.text_of("Value").and_then(codes::datetime_of);
                let entry = interim.entry(n).or_default();
                if is_start {
                    entry.0 = v
                } else {
                    entry.1 = v
                }
                continue;
            }
            let Some(slot) = codes::slot_of(fid) else { continue };
            let (field, kind) = match fields.by_id.get(&fid) {
                Some(f) => *f,
                None => {
                    // A value for a field the file did not define: define it under its own name.
                    if p.custom_fields.len() >= crate::custom::MAX_CUSTOM_FIELDS {
                        continue;
                    }
                    let id = p.ids.next_custom_field();
                    p.custom_fields.push(CustomFieldDef { id, name: slot.field_name(), kind: slot.kind, choices: Vec::new() });
                    fields.by_id.insert(fid, (id, slot.kind));
                    (id, slot.kind)
                }
            };
            let raw = ea
                .text_of("Value")
                .map(str::to_string)
                .or_else(|| int(ea, "ValueID").and_then(|vid| fields.lists.get(&fid).and_then(|l| l.get(&vid)).cloned()));
            let Some(raw) = raw else { continue };
            match custom_value(kind, &raw) {
                Some(v) => {
                    task.custom.insert(field, v);
                }
                None => {
                    notes.add(format!("a value of the custom field {} could not be read", Slot { kind, number: slot.number }.field_name()))
                }
            }
        }
        for (n, (s, f)) in interim {
            if let (Some(start), Some(finish)) = (s, f) {
                task.interim.insert(n, Interim { start, finish });
            }
        }

        for l in t.all("PredecessorLink") {
            let Some(pred) = int(l, "PredecessorUID") else { continue };
            if flag(l, "CrossProject") == Some(true) {
                notes.add("links to tasks in other projects were left out");
                continue;
            }
            let kind = int(l, "Type").and_then(codes::link_type_of).unwrap_or(LinkType::FS);
            let lag = match (num(l, "LinkLag"), int(l, "LagFormat").and_then(codes::format_of)) {
                // LinkLag is in tenths of a minute; a percent lag is read as tenths of a percent.
                (Some(v), Some(f)) if f.unit.is_none() => Lag::Percent((v / 10.0).round().clamp(-1e6, 1e6) as i32),
                (None, _) | (Some(0.0), _) => Lag::None,
                (Some(v), f) => {
                    let m = (v / 10.0).round().clamp(-(limits::MAX_MINUTES as f64), limits::MAX_MINUTES as f64) as Minutes;
                    if f.is_some_and(|f| f.elapsed) { Lag::Elapsed(m) } else { Lag::Working(m) }
                }
            };
            links.push(RawLink { successor: id, predecessor: pred, kind, lag });
        }
        p.tasks.push(task);
    }
    if p.tasks.len() > MAX_TASKS {
        notes.add(format!("the plan has more than {MAX_TASKS} tasks"));
    }
    let ids = map.into_iter().map(|(u, i)| (u, TaskId(i))).collect();
    Links { ids, links }
}

fn predecessors(p: &mut Project, links: Links, notes: &mut Notes) {
    let rows: HashMap<TaskId, usize> = p.tasks.iter().enumerate().map(|(i, t)| (t.id, i)).collect();
    for l in links.links {
        let Some(pred) = links.ids.get(&l.predecessor).copied() else {
            notes.add("a link to a task that is not in the file was left out");
            continue;
        };
        if pred == l.successor {
            notes.add("a task linked to itself was left unlinked");
            continue;
        }
        let Some(task) = rows.get(&l.successor).and_then(|i| p.tasks.get_mut(*i)) else { continue };
        if task.predecessors.iter().any(|d| d.predecessor == TaskRef::Local(pred)) {
            notes.add("a repeated link between the same two tasks was left out");
            continue;
        }
        task.predecessors.push(Dependency { predecessor: TaskRef::Local(pred), kind: l.kind, lag: l.lag });
    }
}

// ---------------------------------------------------------------------------
// Resources and assignments
// ---------------------------------------------------------------------------

fn resources(p: &mut Project, records: &[Element], cals: &Calendars, notes: &mut Notes) -> HashMap<i64, ResourceId> {
    // Resource UID 0 without a name is the placeholder for "unassigned".
    let rows: Vec<&Element> =
        records.iter().filter(|r| flag(r, "IsNull") != Some(true) && !(int(r, "UID") == Some(0) && r.text_of("Name").is_none())).collect();
    let uids: Vec<Option<i64>> = rows.iter().map(|r| int(r, "UID")).collect();
    let (ids, map) = assign_ids(&uids);
    for (r, id) in rows.iter().zip(ids) {
        let id = ResourceId(id);
        let name = r.text_of("Name").map_or_else(|| format!("Resource {}", id.0), str::to_string);
        let mut res = Resource::new(id, name);
        res.kind = if flag(r, "IsCostResource") == Some(true) {
            ResourceType::Cost
        } else if int(r, "Type") == Some(0) {
            ResourceType::Material
        } else {
            ResourceType::Work
        };
        if let Some(i) = r.text_of("Initials") {
            res.initials = i.to_string();
        }
        res.group = text(r, "Group");
        res.email = text(r, "EmailAddress");
        res.material_label = text(r, "MaterialLabel");
        res.max_units = num(r, "MaxUnits").unwrap_or(1.0);
        res.standard_rate = num(r, "StandardRate").unwrap_or(0.0);
        res.overtime_rate = num(r, "OvertimeRate").unwrap_or(0.0);
        res.cost_per_use = num(r, "CostPerUse").unwrap_or(0.0);
        res.notes = text(r, "Notes");
        if let Some(periods) = r.child("AvailabilityPeriods") {
            for a in periods.all("AvailabilityPeriod") {
                // AvailableUnits is a percentage.
                let Some(units) = num(a, "AvailableUnits") else { continue };
                res.availability.push(AvailabilityPeriod {
                    from: when(a, "AvailableFrom").map(|d| d.date()),
                    to: when(a, "AvailableTo").map(|d| d.date()),
                    units: units / 100.0,
                });
            }
        }
        if let Some(uid) = int(r, "CalendarUID").filter(|u| *u >= 0) {
            match cals.derived.get(&uid) {
                Some(d) => {
                    let base = d.base.and_then(|b| cals.base.get(&b).copied());
                    res.exceptions = d.exceptions.clone();
                    if d.days.is_empty() {
                        res.calendar = base;
                    } else if p.calendars.len() < MAX_CALENDARS {
                        // The resource works a different week: a calendar of its own.
                        let mut cal = base.and_then(|b| p.calendar(b)).cloned().unwrap_or_else(|| p.base_calendar().clone());
                        cal.id = p.ids.next_calendar();
                        cal.name = if d.name.is_empty() { res.name.clone() } else { d.name.clone() };
                        cal.exceptions.clear();
                        for (day, ranges) in &d.days {
                            *cal.week.day_mut(*day) = ranges.clone();
                        }
                        res.calendar = Some(cal.id);
                        p.calendars.push(cal);
                    } else {
                        res.calendar = base;
                        notes.add("a resource's own working week was left out (too many calendars)");
                    }
                }
                None => res.calendar = cals.base.get(&uid).copied(),
            }
            if res.calendar == Some(p.calendar) {
                res.calendar = None;
            }
        }
        p.resources.push(res);
    }
    map.into_iter().map(|(u, i)| (u, ResourceId(i))).collect()
}

fn assignments(
    p: &mut Project,
    records: &[Element],
    tasks: &HashMap<i64, TaskId>,
    resources: &HashMap<i64, ResourceId>,
    notes: &mut Notes,
) {
    let uids: Vec<Option<i64>> = records.iter().map(|a| int(a, "UID")).collect();
    let (ids, _) = assign_ids(&uids);
    let kinds: HashMap<ResourceId, ResourceType> = p.resources.iter().map(|r| (r.id, r.kind)).collect();
    let mut seen = HashSet::new();
    for (a, id) in records.iter().zip(ids) {
        let Some(task) = int(a, "TaskUID").and_then(|u| tasks.get(&u)).copied() else { continue };
        let Some(res_uid) = int(a, "ResourceUID").filter(|u| *u >= 0) else { continue };
        let Some(resource) = resources.get(&res_uid).copied() else {
            if res_uid != 0 {
                notes.add("an assignment of a resource that is not in the file was left out");
            }
            continue;
        };
        if !seen.insert((task, resource)) {
            notes.add("a repeated assignment of the same resource to a task was left out");
            continue;
        }
        let (contour, exact) = codes::contour_of(int(a, "WorkContour").unwrap_or(0));
        if !exact {
            notes.add("work contours PlanCraft does not have were imported as the nearest one");
        }
        let cost = if kinds.get(&resource) == Some(&ResourceType::Cost) { num(a, "Cost").unwrap_or(0.0) } else { 0.0 };
        p.assignments.push(Assignment {
            id: AssignmentId(id),
            task,
            resource,
            units: num(a, "Units").unwrap_or(1.0),
            work: span(a, "Work").unwrap_or(0).max(0),
            actual_work: span(a, "ActualWork").unwrap_or(0).max(0),
            cost,
            contour,
        });
    }
}
