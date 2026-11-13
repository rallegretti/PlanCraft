//! Split Task and recurring task tools.

use chrono::Datelike;
use plancraft_core::prelude::*;
use plancraft_core::recurring::{MonthDay, Nth, Pattern, RecurEnd, Recurrence};
use plancraft_core::schedule::planned_gaps;
use serde_json::{Value, json};

use super::*;
use crate::args::*;
use crate::views;

const PATTERNS: [&str; 4] = ["daily", "weekly", "monthly", "yearly"];
const NTHS: [&str; 5] = ["first", "second", "third", "fourth", "last"];
const WEEKDAYS: [&str; 7] = ["monday", "tuesday", "wednesday", "thursday", "friday", "saturday", "sunday"];
const MONTHS: [&str; 12] =
    ["january", "february", "march", "april", "may", "june", "july", "august", "september", "october", "november", "december"];

fn recurrence_props() -> Value {
    json!({
        "duration": duration("Duration of each occurrence (0 makes them milestones; default 1 day when creating)."),
        "pattern": string_enum("How often it repeats.", &PATTERNS),
        "every": integer("Repeat every N days, weeks or months (default 1; ignored for yearly)."),
        "workdays": boolean("Daily: count working days only (every N working days)."),
        "days": { "type": "array", "items": { "type": "string" }, "description": "Weekly: the weekdays, e.g. [\"tuesday\", \"thursday\"] (default the start date's weekday)." },
        "day": integer("Monthly/yearly: day of the month 1-31 (past a shorter month's end: its last day)."),
        "nth": string_enum("Monthly/yearly: which weekday of the month, with 'weekday' (instead of 'day').", &NTHS),
        "weekday": string_enum("Monthly/yearly: the weekday for 'nth'.", &WEEKDAYS),
        "month": { "type": ["integer", "string"], "description": "Yearly: the month, 1-12 or its name (default the start date's month)." },
        "start": date("First day the pattern may fall on (default: the project start when creating)."),
        "occurrences": integer("End after this many occurrences (1-999)."),
        "end": date("End on or before this date (instead of 'occurrences')."),
        "calendar": calendar_ref("Calendar for the occurrences (default the project calendar; null clears)."),
        "ignore_resource_calendars": boolean("Schedule the occurrences even when their resources are not working."),
    })
}

pub fn tools() -> Vec<Tool> {
    let mut create = match recurrence_props() {
        Value::Object(m) => m,
        _ => serde_json::Map::new(),
    };
    create.insert("name".into(), string("Name of the recurring task; occurrences are named \"name 1\", \"name 2\", ..."));
    create.insert("before".into(), task_ref("Insert above this task, at its outline level (default: at the end)."));
    let mut update = match recurrence_props() {
        Value::Object(m) => m,
        _ => serde_json::Map::new(),
    };
    update.insert("task".into(), task_ref("The recurring (summary) task."));
    update.insert("name".into(), string("New name (the occurrences are renamed too)."));
    vec![
        Tool::new(
            "split_task",
            "Split a task",
            "Split Task: the task's work stops at 'start' and goes on at 'resume', leaving a pause. The duration (the work) is unchanged, so the finish moves later. The pause moves with the task when its dates change. Splitting inside an existing pause lengthens it; work already done can't be split. Only auto-scheduled tasks with a duration can be split.",
            schema(
                json!({
                    "task": task_ref("Task to split."),
                    "start": date("When the pause starts (a date alone means the end of that working day)."),
                    "resume": date("When work goes on (a date alone means the start of that working day)."),
                }),
                &["task", "start", "resume"],
            ),
            split_task,
        ),
        Tool::new(
            "set_split",
            "Change a split",
            "Change the length of a task's split (its pause), numbered 1, 2, ... in order: the parts after it move. A length of 0 rejoins the parts on both sides. With 'part' and 'duration' instead, change the work in one part (numbered from 1); the task's duration changes by the difference.",
            schema(
                json!({
                    "task": task_ref("The split task."),
                    "split": integer("Which pause (1 = the first)."),
                    "length": duration("New length of the pause (0 rejoins)."),
                    "part": integer("Which part (1 = the first), to change its work with 'duration'."),
                    "duration": duration("New work in that part."),
                }),
                &["task"],
            ),
            set_split,
        ),
        Tool::new(
            "remove_splits",
            "Rejoin a split task",
            "Remove every split planned in the task, so its work runs without pauses. Pauses in work already under way (from a later resume date or reschedule_uncompleted_work) stay.",
            schema(json!({ "task": task_ref("The split task.") }), &["task"]),
            remove_splits,
        ),
        Tool::new(
            "create_recurring_task",
            "Create a recurring task",
            "Create a recurring task: a summary task with one subtask per occurrence (\"name 1\", \"name 2\", ...), each starting no earlier than its date. Patterns: daily (every N days or N working days), weekly (every N weeks on given weekdays), monthly (every N months on a day, or the first..last weekday), yearly (a day or nth weekday of a month). End after a number of occurrences or by a date. An occurrence on a non-working day moves to the next working day. Resources assigned to the recurring task go to every occurrence; leveling never delays occurrences. Duration 0 makes recurring milestones.",
            schema(Value::Object(create), &["name", "pattern"]),
            create_recurring_task,
        ),
        Tool::new(
            "update_recurring_task",
            "Change a recurring task",
            "Change a recurring task's name, pattern, range, duration or calendar; omitted settings keep their values. Occurrences move to the new dates in order, extra ones are deleted and new ones get the resources of the first occurrence.",
            schema(Value::Object(update), &["task"]),
            update_recurring_task,
        ),
    ]
}

fn split_task(ctx: &Ctx, a: &Args) -> ToolResult {
    ctx.edit(
        a,
        "Split Task",
        |p, _| {
            let id = task_arg(p, a, "task")?;
            let start = parse_dt(p, a.get("start").ok_or_else(|| missing("start"))?, true, "start")?;
            let resume = parse_dt(p, a.get("resume").ok_or_else(|| missing("resume"))?, false, "resume")?;
            p.split_task(id, start, resume)?;
            Ok(id)
        },
        |doc, id| Ok(split_json(&doc.project, id)),
    )
}

fn set_split(ctx: &Ctx, a: &Args) -> ToolResult {
    ctx.edit(
        a,
        "Change Split",
        |p, _| {
            let id = task_arg(p, a, "task")?;
            let index = |key: &str| -> ToolResult<Option<usize>> {
                match a.u64(key)? {
                    Some(0) => err(format!("'{key}' counts from 1")),
                    Some(n) => Ok(Some(usize::try_from(n - 1).unwrap_or(usize::MAX))),
                    None => Ok(None),
                }
            };
            match (index("split")?, index("part")?) {
                (Some(k), None) => {
                    let v = a.get("length").ok_or_else(|| missing("length"))?;
                    let length = parse_duration_value(p, v, "length")?.0;
                    p.set_split_length(id, k, length)?;
                }
                (None, Some(k)) => {
                    let v = a.get("duration").ok_or_else(|| missing("duration"))?;
                    let minutes = parse_duration_value(p, v, "duration")?.0;
                    p.set_part_duration(id, k, minutes)?;
                }
                _ => return err("give 'split' with 'length', or 'part' with 'duration'"),
            }
            Ok(id)
        },
        |doc, id| Ok(split_json(&doc.project, id)),
    )
}

fn remove_splits(ctx: &Ctx, a: &Args) -> ToolResult {
    ctx.edit(
        a,
        "Rejoin Task",
        |p, _| {
            let id = task_arg(p, a, "task")?;
            Ok((id, p.remove_splits(id)?))
        },
        |doc, (id, n)| {
            let mut v = split_json(&doc.project, id);
            set(&mut v, "removed", json!(n));
            Ok(v)
        },
    )
}

/// A task's parts and planned splits.
pub fn split_json(p: &Project, id: TaskId) -> Value {
    let Some(t) = p.task(id) else { return Value::Null };
    json!({
        "id": id.0,
        "name": t.name,
        "duration": p.format_duration(t.duration, false),
        "start": fmt_dt(t.sched.start),
        "finish": fmt_dt(t.sched.finish),
        "parts": t.sched.parts.iter().map(|(s, f)| json!({ "start": fmt_dt(Some(*s)), "finish": fmt_dt(Some(*f)) })).collect::<Vec<_>>(),
        "splits": planned_splits_json(p, t),
    })
}

/// The splits planned in a task: after how much work, and how long.
pub fn planned_splits_json(p: &Project, t: &Task) -> Value {
    planned_gaps(&t.gaps, t.duration)
        .iter()
        .map(|g| json!({ "after_work": views::dur(p, g.work), "length": views::dur(p, g.length), "length_minutes": g.length }))
        .collect()
}

fn named(v: &Value, key: &str, names: &[&str]) -> ToolResult<usize> {
    let s = v.as_str().map(|s| s.trim().to_ascii_lowercase()).unwrap_or_default();
    names
        .iter()
        .position(|n| *n == s || (s.len() >= 3 && n.starts_with(&s)))
        .ok_or_else(|| ToolError(format!("'{key}' must be one of {}", names.join(", "))))
}

fn small(a: &Args, key: &str, lo: u64, hi: u64) -> ToolResult<Option<u64>> {
    match a.u64(key)? {
        Some(n) if (lo..=hi).contains(&n) => Ok(Some(n)),
        Some(_) => err(format!("'{key}' must be from {lo} to {hi}")),
        None => Ok(None),
    }
}

/// Reads a recurrence from the arguments over `base` (the current one when updating).
fn parse_recurrence(p: &Project, a: &Args, base: Option<Recurrence>) -> ToolResult<Recurrence> {
    let start = match a.get("start").filter(|v| !v.is_null()) {
        Some(v) => parse_date(v, "start")?,
        None => base.map_or(p.start.date(), |b| b.start),
    };
    let every = small(a, "every", 1, 999)?.map(|n| n as u16);
    let month_day = |old: Option<MonthDay>| -> ToolResult<MonthDay> {
        match (small(a, "day", 1, 31)?, a.get("nth").filter(|v| !v.is_null()), a.get("weekday").filter(|v| !v.is_null())) {
            (Some(_), Some(_), _) => err("give 'day', or 'nth' with 'weekday', not both"),
            (Some(d), None, _) => Ok(MonthDay::Day(d as u8)),
            (None, Some(n), Some(w)) => {
                let nth = Nth::ALL.get(named(n, "nth", &NTHS)?).copied().unwrap_or(Nth::First);
                Ok(MonthDay::Weekday { nth, weekday: named(w, "weekday", &WEEKDAYS)? as u8 })
            }
            (None, Some(_), None) | (None, None, Some(_)) => err("give 'nth' and 'weekday' together"),
            (None, None, None) => Ok(old.unwrap_or(MonthDay::Day(start.day() as u8))),
        }
    };
    let kind = match a.str("pattern")? {
        Some(k) => named(&json!(k), "pattern", &PATTERNS)?,
        None => match base.map(|b| b.pattern) {
            Some(Pattern::Daily { .. }) => 0,
            Some(Pattern::Weekly { .. }) => 1,
            Some(Pattern::Monthly { .. }) => 2,
            Some(Pattern::Yearly { .. }) => 3,
            None => return err("'pattern' is required"),
        },
    };
    let old = base.map(|b| b.pattern);
    let pattern = match kind {
        0 => {
            let (i, w) = match old {
                Some(Pattern::Daily { interval, workdays }) => (interval, workdays),
                _ => (1, false),
            };
            Pattern::Daily { interval: every.unwrap_or(i), workdays: a.bool("workdays")?.unwrap_or(w) }
        }
        1 => {
            let (i, d) = match old {
                Some(Pattern::Weekly { interval, days }) => (interval, days),
                _ => (1, 1u8 << start.weekday().num_days_from_monday()),
            };
            let days = match a.array("days")? {
                Some(list) => {
                    let mut bits = 0u8;
                    for v in list {
                        bits |= 1 << named(v, "days", &WEEKDAYS)?;
                    }
                    bits
                }
                None => d,
            };
            Pattern::Weekly { interval: every.unwrap_or(i), days }
        }
        2 => {
            let (i, on) = match old {
                Some(Pattern::Monthly { interval, on }) => (interval, Some(on)),
                _ => (1, None),
            };
            Pattern::Monthly { interval: every.unwrap_or(i), on: month_day(on)? }
        }
        _ => {
            let (m, on) = match old {
                Some(Pattern::Yearly { month, on }) => (month, Some(on)),
                _ => (start.month() as u8, None),
            };
            let month = match a.get("month").filter(|v| !v.is_null()) {
                Some(Value::String(s)) if s.trim().parse::<u8>().is_err() => named(&json!(s), "month", &MONTHS)? as u8 + 1,
                Some(_) => small(a, "month", 1, 12)?.map_or(m, |n| n as u8),
                None => m,
            };
            Pattern::Yearly { month, on: month_day(on)? }
        }
    };
    let end = match (small(a, "occurrences", 1, 999)?, a.get("end").filter(|v| !v.is_null())) {
        (Some(_), Some(_)) => return err("give 'occurrences' or 'end', not both"),
        (Some(n), None) => RecurEnd::After(n as u16),
        (None, Some(v)) => RecurEnd::By(parse_date(v, "end")?),
        (None, None) => base.map_or(RecurEnd::After(10), |b| b.end),
    };
    let duration = match a.get("duration").filter(|v| !v.is_null()) {
        Some(v) => parse_duration_value(p, v, "duration")?.0,
        None => base.map_or(p.settings.time.minutes_per_day, |b| b.duration),
    };
    let calendar = if a.is_clear("calendar") {
        None
    } else if a.present("calendar") {
        Some(calendar_arg(p, a, "calendar")?)
    } else {
        base.and_then(|b| b.calendar)
    };
    let ignore_resource_calendars = a.bool("ignore_resource_calendars")?.unwrap_or(base.is_some_and(|b| b.ignore_resource_calendars));
    Ok(Recurrence { pattern, start, end, duration, calendar, ignore_resource_calendars })
}

/// A recurrence as JSON, the same words the tools take.
pub fn recurrence_json(p: &Project, r: &Recurrence) -> Value {
    let word = |list: &[&str], i: usize| list.get(i).copied().unwrap_or("").to_string();
    let mut v = json!({
        "start": r.start.to_string(),
        "duration": views::dur(p, r.duration),
        "ignore_resource_calendars": r.ignore_resource_calendars,
    });
    let on = |v: &mut Value, on: MonthDay| match on {
        MonthDay::Day(d) => set(v, "day", json!(d)),
        MonthDay::Weekday { nth, weekday } => {
            set(v, "nth", json!(word(&NTHS, Nth::ALL.iter().position(|n| *n == nth).unwrap_or(0))));
            set(v, "weekday", json!(word(&WEEKDAYS, usize::from(weekday))));
        }
    };
    match r.pattern {
        Pattern::Daily { interval, workdays } => {
            set(&mut v, "pattern", json!("daily"));
            set(&mut v, "every", json!(interval));
            set(&mut v, "workdays", json!(workdays));
        }
        Pattern::Weekly { interval, days } => {
            set(&mut v, "pattern", json!("weekly"));
            set(&mut v, "every", json!(interval));
            let names: Vec<String> = (0..7).filter(|k| days & (1 << k) != 0).map(|k| word(&WEEKDAYS, k)).collect();
            set(&mut v, "days", json!(names));
        }
        Pattern::Monthly { interval, on: o } => {
            set(&mut v, "pattern", json!("monthly"));
            set(&mut v, "every", json!(interval));
            on(&mut v, o);
        }
        Pattern::Yearly { month, on: o } => {
            set(&mut v, "pattern", json!("yearly"));
            set(&mut v, "month", json!(word(&MONTHS, usize::from(month).saturating_sub(1))));
            on(&mut v, o);
        }
    }
    match r.end {
        RecurEnd::After(n) => set(&mut v, "occurrences", json!(n)),
        RecurEnd::By(d) => set(&mut v, "end", json!(d.to_string())),
    }
    if let Some(c) = r.calendar.and_then(|c| p.calendar(c)) {
        set(&mut v, "calendar", json!({ "id": c.id.0, "name": c.name }));
    }
    v
}

fn recurring_result(p: &Project, id: TaskId) -> Value {
    let Some(idx) = p.task_index(id) else { return Value::Null };
    let Some(t) = p.tasks.get(idx) else { return Value::Null };
    let occurrences: Vec<Value> = p
        .tasks
        .get(p.descendants_range(idx))
        .unwrap_or_default()
        .iter()
        .map(|o| json!({ "id": o.id.0, "name": o.name, "start": fmt_dt(o.sched.start), "finish": fmt_dt(o.sched.finish) }))
        .collect();
    json!({
        "id": id.0,
        "row": idx + 1,
        "name": t.name,
        "recurrence": t.recurrence.as_ref().map(|r| recurrence_json(p, r)),
        "occurrences": occurrences,
        "start": fmt_dt(t.sched.start),
        "finish": fmt_dt(t.sched.finish),
    })
}

fn create_recurring_task(ctx: &Ctx, a: &Args) -> ToolResult {
    ctx.edit(
        a,
        "Recurring Task",
        |p, _| {
            let name = a.req_str("name")?;
            let rec = parse_recurrence(p, a, None)?;
            let at = match a.get("before").filter(|v| !v.is_null()) {
                Some(v) => Some(p.task_index(resolve_task(p, v)?).unwrap_or(p.tasks.len())),
                None => None,
            };
            Ok(p.add_recurring_task(name, rec, at)?)
        },
        |doc, id| Ok(recurring_result(&doc.project, id)),
    )
}

fn update_recurring_task(ctx: &Ctx, a: &Args) -> ToolResult {
    ctx.edit(
        a,
        "Recurring Task",
        |p, _| {
            let id = task_arg(p, a, "task")?;
            let base = p.task(id).and_then(|t| t.recurrence);
            if base.is_none() {
                return err("this is not a recurring task (create one with create_recurring_task)");
            }
            let rec = parse_recurrence(p, a, base)?;
            let name = a.text("name")?;
            p.set_recurrence(id, name.as_deref(), rec)?;
            Ok(id)
        },
        |doc, id| Ok(recurring_result(&doc.project, id)),
    )
}
