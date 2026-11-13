//! Tracking tools: Microsoft Project's Update Project (progress through a date, reschedule
//! uncompleted work) and Mark on Track. The status date itself is set with
//! `update_project`.

use chrono::NaiveDate;
use plancraft_core::ops::{ProgressUpdate, TrackingReport};
use plancraft_core::prelude::*;
use serde_json::{Value, json};

use super::*;
use crate::args::*;

const MODES: [&str; 2] = ["scheduled", "zero_or_hundred"];

pub fn tools() -> Vec<Tool> {
    vec![
        Tool::new(
            "update_progress_through",
            "Update progress through a date",
            "Update Project's 'Update work as complete through': set each task's % complete to where its schedule says it should be at the end of 'date' (mode 'scheduled'), or only mark tasks that finish by then as 100% complete (mode 'zero_or_hundred'). Progress already entered is never lowered. Returns the tasks changed.",
            schema(
                json!({
                    "date": date("Update through the end of this day (default: the status date set with update_project, else today)."),
                    "mode": string_enum("'scheduled' (default): 0-100% as scheduled; 'zero_or_hundred': only tasks finished by the date become 100%.", &MODES),
                    "tasks": task_refs("Only these tasks (summaries stand for their subtasks); default the entire project."),
                }),
                &[],
            ),
            update_progress_through,
        ),
        Tool::new(
            "reschedule_uncompleted_work",
            "Reschedule uncompleted work",
            "Update Project's 'Reschedule uncompleted work to start after': work not done by the end of 'date' moves after it and actual dates stay. A task in progress pauses at its stop and resumes after the date (earlier pauses stay); a task not started, and not already moved past the date by its links, gets a Start No Earlier Than constraint. Manually scheduled tasks and other date constraints are left alone and reported.",
            schema(
                json!({
                    "date": date("Uncompleted work starts after the end of this day (default: the status date, else today)."),
                    "tasks": task_refs("Only these tasks (summaries stand for their subtasks); default the entire project."),
                }),
                &[],
            ),
            reschedule_uncompleted_work,
        ),
        Tool::new(
            "mark_on_track",
            "Mark on track",
            "Set the given tasks' % complete to what their schedule says at the status date (today if none is set), never lowering progress already entered.",
            schema(json!({ "tasks": task_refs("Tasks to mark on track (summaries stand for their subtasks).") }), &["tasks"]),
            mark_on_track,
        ),
    ]
}

/// The date argument, defaulting to the status date or today.
fn date_or_status(p: &Project, a: &Args) -> ToolResult<NaiveDate> {
    match a.get("date").filter(|v| !v.is_null()) {
        Some(v) => parse_date(v, "date"),
        None => Ok(p.status_date.unwrap_or_else(|| chrono::Local::now().date_naive())),
    }
}

fn report_json(p: &Project, r: &TrackingReport) -> Value {
    let task = |id: &TaskId| {
        let t = p.task(*id);
        json!({
            "id": id.0,
            "name": t.map(|t| t.name.clone()),
            "percent_complete": t.map(|t| t.percent_complete),
            "start": fmt_dt(t.and_then(|t| t.sched.start)),
            "finish": fmt_dt(t.and_then(|t| t.sched.finish)),
            "resume": fmt_dt(t.and_then(|t| t.sched.resume)),
        })
    };
    json!({
        "updated": r.updated.iter().map(task).collect::<Vec<_>>(),
        "split": r.split.iter().map(task).collect::<Vec<_>>(),
        "constrained": r.constrained.iter().map(task).collect::<Vec<_>>(),
        "skipped": r.skipped.iter().map(|(id, why)| json!({ "id": id.0, "name": p.task(*id).map(|t| t.name.clone()), "reason": why })).collect::<Vec<_>>(),
        "project_finish": fmt_dt(p.sched.finish),
    })
}

fn update_progress_through(ctx: &Ctx, a: &Args) -> ToolResult {
    let how = match a.str("mode")?.map(|m| m.trim().to_ascii_lowercase()) {
        None => ProgressUpdate::Scheduled,
        Some(m) if m == "scheduled" => ProgressUpdate::Scheduled,
        Some(m) if m == "zero_or_hundred" => ProgressUpdate::ZeroOrHundred,
        Some(_) => return err(format!("'mode' must be one of {}", MODES.join(", "))),
    };
    ctx.edit(
        a,
        "Update Project",
        |p, _| {
            let ids = task_list_arg(p, a, "tasks")?;
            let date = date_or_status(p, a)?;
            let through = p.end_of_working_day(date);
            Ok((date, p.update_progress_through(&ids, through, how)?))
        },
        |doc, (date, r)| {
            let mut v = report_json(&doc.project, &r);
            set(&mut v, "through", json!(date.to_string()));
            Ok(v)
        },
    )
}

fn reschedule_uncompleted_work(ctx: &Ctx, a: &Args) -> ToolResult {
    ctx.edit(
        a,
        "Reschedule Uncompleted Work",
        |p, _| {
            let ids = task_list_arg(p, a, "tasks")?;
            let date = date_or_status(p, a)?;
            let after = p.end_of_working_day(date);
            Ok((date, p.reschedule_uncompleted_work(&ids, after)?))
        },
        |doc, (date, r)| {
            let mut v = report_json(&doc.project, &r);
            set(&mut v, "after", json!(date.to_string()));
            Ok(v)
        },
    )
}

fn mark_on_track(ctx: &Ctx, a: &Args) -> ToolResult {
    ctx.edit(
        a,
        "Mark on Track",
        |p, _| {
            let ids = task_list_arg(p, a, "tasks")?;
            if ids.is_empty() {
                return err("'tasks' must name at least one task");
            }
            Ok(p.mark_on_track(&ids)?)
        },
        |doc, r| {
            let mut v = report_json(&doc.project, &r);
            set(&mut v, "status_date", json!(fmt_dt(Some(plancraft_core::report::status_moment(&doc.project)))));
            Ok(v)
        },
    )
}
