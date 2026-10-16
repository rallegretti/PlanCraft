//! Working-time calendar tools.

use chrono::{Datelike, Weekday};
use plancraft_core::calendar::{WEEKDAYS, WorkCal};
use plancraft_core::ids::CalendarId;
use plancraft_core::model::Project;
use serde_json::{Map, Value, json};

use super::*;
use crate::args::*;
use crate::tools::resources::exception_from_args;
use crate::views;

fn week_schema() -> Value {
    let mut props = Map::new();
    for wd in WEEKDAYS {
        props.insert(views::weekday_name(wd).into(), json!({ "type": "array", "items": { "type": "string" } }));
    }
    json!({
        "type": "object",
        "properties": props,
        "additionalProperties": false,
        "description": "Full work week: {\"monday\": [\"08:00-12:00\", \"13:00-17:00\"], ..., \"saturday\": []}. Days left out are unchanged."
    })
}

pub fn tools() -> Vec<Tool> {
    vec![
        Tool::new(
            "list_calendars",
            "List calendars",
            "List working-time calendars with weekly hours, number of exceptions and how many tasks/resources use them. The project calendar is flagged.",
            schema(json!({}), &[]),
            list_calendars,
        )
        .read_only(),
        Tool::new(
            "get_calendar",
            "Get a calendar",
            "Show a calendar's work week and exceptions (holidays, altered days). With from/to, also list the effective working times of each day in that range (max 400 days).",
            schema(
                json!({
                    "calendar": calendar_ref("Calendar (defaults to the project calendar)."),
                    "from": date("Start of the range to expand."),
                    "to": date("End of the range to expand."),
                }),
                &[],
            ),
            get_calendar,
        )
        .read_only(),
        Tool::new(
            "create_calendar",
            "Create a calendar",
            "Create a calendar, copied from another calendar or from the standard Monday–Friday 08:00–12:00/13:00–17:00 week. Optionally set its work week in the same call.",
            schema(
                json!({
                    "name": string("Calendar name."),
                    "copy_from": calendar_ref("Calendar to copy (work week and exceptions)."),
                    "week": week_schema(),
                    "set_as_project_calendar": boolean("Make it the project calendar."),
                }),
                &["name"],
            ),
            create_calendar,
        ),
        Tool::new(
            "set_work_week",
            "Set working hours",
            "Set the regular working times of some weekdays of a calendar. Give 'days' (e.g. [\"monday\", \"friday\"], or \"weekdays\", \"weekend\", \"all\") with 'working_times' ([] = non-working), and/or a full 'week' object. Can also rename the calendar.",
            schema(
                json!({
                    "calendar": calendar_ref("Calendar (defaults to the project calendar)."),
                    "days": { "type": ["array", "string"], "items": { "type": "string" }, "description": "Weekday names (monday..sunday, mon..sun) or groups: weekdays, weekend, all." },
                    "working_times": time_ranges("Working times for 'days'."),
                    "week": week_schema(),
                    "name": string("New calendar name."),
                }),
                &[],
            ),
            set_work_week,
        ),
        Tool::new(
            "add_calendar_exception",
            "Add calendar exception",
            "Add an exception to a calendar: non-working days (holiday, shutdown) when working_times is omitted, or special working hours for those days. Set yearly for recurring holidays.",
            schema(
                json!({
                    "calendar": calendar_ref("Calendar (defaults to the project calendar)."),
                    "name": string("Exception name, e.g. \"Christmas\"."),
                    "start": date("First day."),
                    "end": date("Last day (defaults to start)."),
                    "working_times": time_ranges("Working times on these days; omit or [] for non-working."),
                    "yearly": boolean("Repeat every year."),
                }),
                &["start"],
            ),
            add_calendar_exception,
        ),
        Tool::new(
            "remove_calendar_exception",
            "Remove calendar exception",
            "Remove a calendar exception by index (from get_calendar) or by name.",
            schema(
                json!({
                    "calendar": calendar_ref("Calendar (defaults to the project calendar)."),
                    "index": integer("Exception index."),
                    "name": string("Exception name (all exceptions with this name are removed)."),
                }),
                &[],
            ),
            remove_calendar_exception,
        ),
        Tool::new(
            "delete_calendar",
            "Delete a calendar",
            "Delete a calendar. Tasks and resources using it fall back to the project calendar. The project calendar cannot be deleted.",
            schema(json!({ "calendar": calendar_ref("Calendar to delete.") }), &["calendar"]),
            delete_calendar,
        )
        .destructive(),
    ]
}

fn cal_or_default(p: &Project, a: &Args) -> ToolResult<CalendarId> {
    match a.get("calendar") {
        Some(_) => calendar_arg(p, a, "calendar"),
        None => Ok(p.calendar),
    }
}

fn list_calendars(ctx: &Ctx, a: &Args) -> ToolResult {
    ctx.read(a, |doc| {
        let p = &doc.project;
        Ok(json!({ "calendars": p.calendars.iter().map(|c| views::calendar_brief(p, c)).collect::<Vec<_>>() }))
    })
}

fn get_calendar(ctx: &Ctx, a: &Args) -> ToolResult {
    ctx.read(a, |doc| {
        let p = &doc.project;
        let id = cal_or_default(p, a)?;
        let cal = p.calendar(id).ok_or_else(|| missing("resolved"))?;
        let mut v = views::calendar_detail(p, cal);
        if a.get("from").is_some() || a.get("to").is_some() {
            let from = match a.get("from") {
                Some(x) => parse_date(x, "from")?,
                None => p.start.date(),
            };
            let to = match a.get("to") {
                Some(x) => parse_date(x, "to")?,
                None => from + chrono::Duration::days(13),
            };
            if to < from {
                return err("'to' is before 'from'");
            }
            if (to - from).num_days() > 400 {
                return err("date range too long (max 400 days)");
            }
            let wc = WorkCal::single(cal);
            let mut days = Vec::new();
            let mut d = from;
            while d <= to {
                let r = wc.ranges(d);
                let exc = cal.exceptions.iter().rev().find(|e| e.covers(d)).map(|e| e.name.clone());
                days.push(json!({
                    "date": d.format(DATE_FMT).to_string(),
                    "weekday": views::weekday_name(d.weekday()),
                    "working": !r.is_empty(),
                    "working_times": views::ranges_json(&r),
                    "hours": hours(r.iter().map(|x| x.len() as f64).sum::<f64>()),
                    "exception": exc,
                }));
                d += chrono::Duration::days(1);
            }
            set(&mut v, "days", Value::Array(days));
        }
        Ok(v)
    })
}

fn parse_days(v: &Value) -> ToolResult<Vec<Weekday>> {
    let items: Vec<&Value> = match v {
        Value::Array(a) => a.iter().collect(),
        other => vec![other],
    };
    let mut out = Vec::new();
    for it in items {
        let s = it.as_str().ok_or_else(|| ToolError("'days' must contain weekday names".into()))?.trim().to_ascii_lowercase();
        let add: Vec<Weekday> = match s.as_str() {
            "weekdays" | "workdays" => vec![Weekday::Mon, Weekday::Tue, Weekday::Wed, Weekday::Thu, Weekday::Fri],
            "weekend" | "weekends" => vec![Weekday::Sat, Weekday::Sun],
            "all" | "every day" | "everyday" => WEEKDAYS.to_vec(),
            other => vec![other.parse::<Weekday>().map_err(|_| ToolError(format!("unknown weekday '{other}'")))?],
        };
        for d in add {
            if !out.contains(&d) {
                out.push(d);
            }
        }
    }
    Ok(out)
}

fn apply_week(p: &mut Project, id: CalendarId, week: &Args) -> ToolResult<()> {
    let mut updates = Vec::new();
    for (k, v) in week.map() {
        let wd: Weekday = k.parse().map_err(|_| ToolError(format!("unknown weekday '{k}' in 'week'")))?;
        updates.push((wd, parse_time_ranges(Some(v), k)?));
    }
    let cal = p.calendars.iter_mut().find(|c| c.id == id).ok_or_else(|| missing("resolved"))?;
    for (wd, r) in updates {
        *cal.week.day_mut(wd) = r;
    }
    Ok(())
}

fn check_not_empty(p: &Project, id: CalendarId) -> ToolResult<()> {
    let cal = p.calendar(id).ok_or_else(|| missing("resolved"))?;
    if cal.week.weekly_minutes() == 0 {
        return err("a calendar needs at least one working day per week");
    }
    Ok(())
}

fn create_calendar(ctx: &Ctx, a: &Args) -> ToolResult {
    let name = a.req_str("name")?.trim().to_string();
    if name.is_empty() {
        return err("'name' must not be empty");
    }
    ctx.edit(
        a,
        &format!("Add calendar '{name}'"),
        |p, _| {
            if p.calendars.iter().any(|c| c.name.eq_ignore_ascii_case(&name)) {
                return err(format!("a calendar named '{name}' already exists"));
            }
            let from = a.get("copy_from").map(|v| resolve_calendar(p, v)).transpose()?;
            let id = p.add_calendar(name.clone(), from)?;
            if let Some(w) = a.object("week")? {
                apply_week(p, id, &w)?;
                check_not_empty(p, id)?;
            }
            if a.bool("set_as_project_calendar")?.unwrap_or(false) {
                p.calendar = id;
            }
            Ok(id)
        },
        |doc, id| Ok(views::calendar_detail(&doc.project, doc.project.calendar(id).ok_or_else(|| missing("exists"))?)),
    )
}

fn set_work_week(ctx: &Ctx, a: &Args) -> ToolResult {
    ctx.edit(
        a,
        "Change working time",
        |p, _| {
            let id = cal_or_default(p, a)?;
            if let Some(n) = a.str("name")? {
                let n = n.trim();
                if n.is_empty() {
                    return err("'name' must not be empty");
                }
                p.calendars.iter_mut().find(|c| c.id == id).ok_or_else(|| missing("resolved"))?.name = n.to_string();
            }
            if let Some(w) = a.object("week")? {
                apply_week(p, id, &w)?;
            }
            match (a.get("days"), a.present("working_times")) {
                (Some(d), true) => {
                    let days = parse_days(d)?;
                    let ranges = parse_time_ranges(a.map().get("working_times"), "working_times")?;
                    let cal = p.calendars.iter_mut().find(|c| c.id == id).ok_or_else(|| missing("resolved"))?;
                    for wd in days {
                        *cal.week.day_mut(wd) = ranges.clone();
                    }
                }
                (Some(_), false) => {
                    return err("'days' needs 'working_times' ([] for non-working)");
                }
                (None, true) => return err("'working_times' needs 'days'"),
                (None, false) => {
                    if a.get("week").is_none() && a.get("name").is_none() {
                        return err("give 'days' + 'working_times', 'week', or 'name'");
                    }
                }
            }
            check_not_empty(p, id)?;
            Ok(id)
        },
        |doc, id| Ok(views::calendar_detail(&doc.project, doc.project.calendar(id).ok_or_else(|| missing("exists"))?)),
    )
}

fn add_calendar_exception(ctx: &Ctx, a: &Args) -> ToolResult {
    let exc = exception_from_args(a)?;
    let exc = if a.get("name").is_none() && exc.working.is_empty() {
        plancraft_core::calendar::CalendarException { name: "Holiday".into(), ..exc }
    } else {
        exc
    };
    ctx.edit(
        a,
        &format!("Add calendar exception '{}'", exc.name),
        |p, _| {
            let id = cal_or_default(p, a)?;
            p.calendars.iter_mut().find(|c| c.id == id).ok_or_else(|| missing("resolved"))?.exceptions.push(exc);
            Ok(id)
        },
        |doc, id| Ok(views::calendar_detail(&doc.project, doc.project.calendar(id).ok_or_else(|| missing("exists"))?)),
    )
}

fn remove_calendar_exception(ctx: &Ctx, a: &Args) -> ToolResult {
    ctx.edit(
        a,
        "Remove calendar exception",
        |p, _| {
            let id = cal_or_default(p, a)?;
            let index = a.u64("index")?;
            let name = a.str("name")?.map(str::to_string);
            let cal = p.calendars.iter_mut().find(|c| c.id == id).ok_or_else(|| missing("resolved"))?;
            let before = cal.exceptions.len();
            match (index, name) {
                (Some(i), _) => {
                    if i as usize >= cal.exceptions.len() {
                        return err(format!("exception index {i} out of range (calendar has {})", cal.exceptions.len()));
                    }
                    cal.exceptions.remove(i as usize);
                }
                (None, Some(n)) => cal.exceptions.retain(|e| !e.name.eq_ignore_ascii_case(n.trim())),
                (None, None) => return err("give 'index' or 'name'"),
            }
            if cal.exceptions.len() == before {
                return err("no matching exception");
            }
            Ok(id)
        },
        |doc, id| Ok(views::calendar_detail(&doc.project, doc.project.calendar(id).ok_or_else(|| missing("exists"))?)),
    )
}

fn delete_calendar(ctx: &Ctx, a: &Args) -> ToolResult {
    ctx.edit(
        a,
        "Delete calendar",
        |p, _| {
            let id = calendar_arg(p, a, "calendar")?;
            let name = p.calendar(id).map(|c| c.name.clone()).unwrap_or_default();
            p.delete_calendar(id)?;
            Ok(name)
        },
        |doc, name| Ok(json!({ "deleted": name, "calendars": doc.project.calendars.iter().map(|c| views::calendar_brief(&doc.project, c)).collect::<Vec<_>>() })),
    )
}
