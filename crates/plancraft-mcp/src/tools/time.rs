//! Time tracking tools.

use std::collections::BTreeMap;

use plancraft_core::duration::{Minutes, format_work};
use plancraft_core::ids::TimeEntryId;
use serde_json::{Value, json};

use super::*;
use crate::args::*;
use crate::views;

pub fn tools() -> Vec<Tool> {
    vec![
        Tool::new(
            "log_time",
            "Log time",
            "Record time a resource spent on a task (timesheet entry). Sets the task's actual start. Give 'hours' or 'duration'. If the task has exactly one assigned resource, 'resource' may be omitted.",
            schema(
                json!({
                    "task": task_ref("Task worked on (not a summary)."),
                    "resource": resource_ref("Who did the work."),
                    "date": date("Day of the work (defaults to today)."),
                    "hours": number("Hours worked."),
                    "duration": work("Time worked, e.g. \"90m\", \"1.5h\", \"1d\" (alternative to hours)."),
                    "note": string("Optional note."),
                    "update_progress": boolean("Also update % complete of tasks from tracked time (default false)."),
                }),
                &["task"],
            ),
            log_time,
        ),
        Tool::new(
            "list_time_entries",
            "List time entries",
            "List logged time with optional task/resource/date filters, with totals per resource and per task.",
            schema(
                json!({
                    "task": task_ref("Only this task."),
                    "resource": resource_ref("Only this resource."),
                    "from": date("First day."),
                    "to": date("Last day."),
                    "limit": integer("Maximum entries returned (default 500); totals always cover all matches."),
                }),
                &[],
            ),
            list_time_entries,
        )
        .read_only(),
        Tool::new(
            "delete_time_entry",
            "Delete a time entry",
            "Delete a logged time entry by id.",
            schema(json!({ "entry": integer("Time entry id (from list_time_entries).") }), &["entry"]),
            delete_time_entry,
        )
        .destructive(),
        Tool::new(
            "update_progress_from_time",
            "Update progress from time",
            "Set each task's % complete from tracked work (actual ÷ planned), capped at 99% until the task is marked finished.",
            schema(json!({}), &[]),
            update_progress_from_time,
        ),
    ]
}

fn log_time(ctx: &Ctx, a: &Args) -> ToolResult {
    ctx.edit(
        a,
        "Log time",
        |p, _| {
            let task = task_arg(p, a, "task")?;
            let resource = match a.get("resource") {
                Some(_) => resource_arg(p, a, "resource")?,
                None => {
                    let rs: Vec<_> = p.assignments_for_task(task).map(|x| x.resource).collect();
                    match rs.as_slice() {
                        [one] => *one,
                        [] => return err("the task has no assigned resource; give 'resource'"),
                        _ => return err("the task has several resources; give 'resource'"),
                    }
                }
            };
            let date = match a.get("date") {
                Some(v) => parse_date(v, "date")?,
                None => chrono::Local::now().date_naive(),
            };
            let minutes: Minutes = match (a.f64("hours")?, a.get("duration")) {
                (Some(h), _) => (h * 60.0).round() as Minutes,
                (None, Some(d)) => parse_work_value(p, d, "duration")?,
                (None, None) => return err("give 'hours' or 'duration'"),
            };
            let note = a.str("note")?.unwrap_or("").to_string();
            let id = p.log_time(task, resource, date, minutes, note)?;
            if a.bool("update_progress")?.unwrap_or(false) {
                p.update_progress_from_time();
            }
            Ok(id)
        },
        |doc, id| {
            let p = &doc.project;
            let e = p.time_entries.iter().find(|e| e.id == id).expect("logged");
            Ok(json!({
                "entry": views::time_entry_json(p, e),
                "task": views::task_row(p, p.task_index(e.task).expect("exists")),
            }))
        },
    )
}

fn list_time_entries(ctx: &Ctx, a: &Args) -> ToolResult {
    ctx.read(a, |doc| {
        let p = &doc.project;
        let t = a.get("task").map(|v| resolve_task(p, v)).transpose()?;
        let r = a.get("resource").map(|v| resolve_resource(p, v)).transpose()?;
        let from = a.get("from").map(|v| parse_date(v, "from")).transpose()?;
        let to = a.get("to").map(|v| parse_date(v, "to")).transpose()?;
        let limit = a.u64("limit")?.unwrap_or(500) as usize;
        let mut entries: Vec<_> = p
            .time_entries
            .iter()
            .filter(|e| {
                t.is_none_or(|t| e.task == t)
                    && r.is_none_or(|r| e.resource == r)
                    && from.is_none_or(|f| e.date >= f)
                    && to.is_none_or(|x| e.date <= x)
            })
            .collect();
        entries.sort_by_key(|e| (e.date, e.id));
        let total: Minutes = entries.iter().map(|e| e.minutes).sum();
        let mut by_resource: BTreeMap<String, Minutes> = BTreeMap::new();
        let mut by_task: BTreeMap<String, Minutes> = BTreeMap::new();
        for e in &entries {
            *by_resource.entry(p.resource(e.resource).map(|r| r.name.clone()).unwrap_or_default()).or_default() += e.minutes;
            *by_task.entry(format!("{} (id {})", p.task(e.task).map(|t| t.name.clone()).unwrap_or_default(), e.task.0)).or_default() +=
                e.minutes;
        }
        let totals = |m: BTreeMap<String, Minutes>| -> Value {
            m.into_iter().map(|(k, v)| (k, json!(hours(v as f64)))).collect::<serde_json::Map<_, _>>().into()
        };
        Ok(json!({
            "count": entries.len(),
            "total": format_work(total),
            "total_hours": hours(total as f64),
            "hours_by_resource": totals(by_resource),
            "hours_by_task": totals(by_task),
            "entries": entries.iter().take(limit).map(|e| views::time_entry_json(p, e)).collect::<Vec<_>>(),
        }))
    })
}

fn delete_time_entry(ctx: &Ctx, a: &Args) -> ToolResult {
    let id = TimeEntryId(a.u64("entry")?.ok_or_else(|| ToolError("missing 'entry'".into()))? as u32);
    ctx.edit(a, "Delete time entry", |p, _| Ok(p.delete_time_entry(id)?), |_, _| Ok(json!({ "deleted": id.0 })))
}

fn update_progress_from_time(ctx: &Ctx, a: &Args) -> ToolResult {
    ctx.edit(
        a,
        "Update progress from time",
        |p, _| {
            let before: Vec<_> = p.tasks.iter().map(|t| (t.id, t.percent_complete)).collect();
            p.update_progress_from_time();
            Ok(before)
        },
        |doc, before| {
            let p = &doc.project;
            let changed: Vec<Value> = before
                .iter()
                .filter_map(|(id, old)| {
                    let t = p.task(*id)?;
                    (t.percent_complete != *old).then(|| json!({ "id": id.0, "name": t.name, "from": old, "to": t.percent_complete }))
                })
                .collect();
            Ok(json!({ "updated": changed.len(), "tasks": changed, "project_percent_complete": round2(p.sched.percent_complete as f64) }))
        },
    )
}
