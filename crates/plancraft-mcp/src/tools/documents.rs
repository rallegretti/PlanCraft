//! Document-level tools: open, create, save, close, select and project settings.

use std::path::PathBuf;

use plancraft_core::duration::DurationUnit;
use plancraft_core::model::{FILE_EXTENSION, Project};
use plancraft_core::report;
use plancraft_core::workspace::Document;
use serde_json::{Value, json};

use super::*;
use crate::args::*;
use crate::views;

pub fn tools() -> Vec<Tool> {
    vec![
        Tool::new(
            "list_documents",
            "List open documents",
            "List the project documents currently open in this PlanCraft session, with their handle numbers, file paths, unsaved-change flag and which one is active. Use the handle as the 'document' argument of other tools.",
            json!({ "type": "object", "properties": {}, "additionalProperties": false }),
            list_documents,
        )
        .read_only(),
        Tool::new(
            "new_project",
            "Create a new project",
            "Create a new empty project document and make it the active document. Optionally give a file path to save it immediately (required before it can be linked from other documents).",
            json!({
                "type": "object",
                "properties": {
                    "name": string("Project name."),
                    "start": date("Project start date (defaults to today)."),
                    "path": string("Optional .plancraft file path to save the new project to."),
                    "overwrite": boolean("Allow overwriting an existing file at 'path' (default false)."),
                    "manager": string("Project manager."),
                    "company": string("Company."),
                    "author": string("Author."),
                    "notes": string("Project notes."),
                },
                "required": ["name"],
                "additionalProperties": false
            }),
            new_project,
        ),
        Tool::new(
            "open_project",
            "Open a project file",
            "Open a .plancraft project file (or activate it if it is already open) and make it the active document.",
            json!({
                "type": "object",
                "properties": { "path": string("Path of the .plancraft file to open.") },
                "required": ["path"],
                "additionalProperties": false
            }),
            open_project,
        ),
        Tool::new(
            "save_project",
            "Save a project",
            "Save the document to its file. With 'path', save as a new file (relative links to external projects are rewritten so they keep working).",
            schema(json!({ "path": string("Optional new file path (Save As).") }), &[]),
            save_project,
        ),
        Tool::new(
            "close_project",
            "Close a project",
            "Close a document. Fails when it has unsaved changes unless discard_changes is true.",
            schema(json!({ "discard_changes": boolean("Close even if there are unsaved changes (default false).") }), &[]),
            close_project,
        )
        .destructive(),
        Tool::new(
            "set_active_document",
            "Switch active document",
            "Make another open document the active one (the default target of all tools and the document shown in the app).",
            json!({
                "type": "object",
                "properties": { "document": document_prop() },
                "required": ["document"],
                "additionalProperties": false
            }),
            set_active_document,
        ),
        Tool::new(
            "get_project",
            "Get project information",
            "Get project information (name, dates, manager, file), settings (hours per day, currency, points, scheduling options), calendars and key statistics (task counts, late/critical tasks, work, cost, % complete, warnings).",
            schema(json!({}), &[]),
            get_project,
        )
        .read_only(),
        Tool::new(
            "update_project",
            "Update project settings",
            "Update project properties and settings. Only the given fields change.",
            schema(
                json!({
                    "name": string("Project name."),
                    "start": date("Project start date (tasks without predecessors or constraints start here)."),
                    "manager": string("Project manager."),
                    "company": string("Company."),
                    "author": string("Author."),
                    "notes": string("Project notes."),
                    "status_date": date_or_null("Status date used for progress/late calculations (null = today)."),
                    "currency": string("Currency symbol, e.g. \"$\" or \"€\"."),
                    "hours_per_day": number("Working hours in one 'day' of duration (default 8)."),
                    "hours_per_week": number("Working hours in one 'week' of duration (default 40)."),
                    "days_per_month": integer("Working days in one 'month' of duration (default 20)."),
                    "default_start_time": string("Default task start time \"HH:MM\"."),
                    "default_finish_time": string("Default task finish time \"HH:MM\"."),
                    "duration_unit": string_enum("Unit used to display durations and to read bare numbers.", &["minutes", "hours", "days", "weeks", "months"]),
                    "hours_per_point": number("Work represented by one complexity point, in hours."),
                    "minutes_per_point": integer("Work represented by one complexity point, in minutes."),
                    "calendar": calendar_ref("Project base calendar."),
                    "new_tasks_auto": boolean("New tasks are auto-scheduled (true) or manual (false)."),
                    "use_resource_calendars": boolean("Assigned tasks follow resource calendars and personal time off."),
                    "progress_from_time": boolean("Compute task % complete from tracked time automatically."),
                    "critical_slack": duration("Tasks with total slack at or below this are critical (default 0)."),
                }),
                &[],
            ),
            update_project,
        ),
    ]
}

fn list_documents(ctx: &Ctx, _a: &Args) -> ToolResult {
    ctx.with_workspace(|ws| {
        let docs: Vec<Value> = ws.docs.iter().enumerate().map(|(i, d)| views::document_brief(d, ws.active == Some(i))).collect();
        Ok(json!({
            "documents": docs,
            "active": ws.active_doc().map(|d| d.handle),
            "hint": if ws.docs.is_empty() { "No document is open: call new_project or open_project." } else { "" },
        }))
    })
}

fn today() -> chrono::NaiveDate {
    chrono::Local::now().date_naive()
}

/// Adds the `.plancraft` extension when missing and makes the path absolute.
fn with_extension(path: &str) -> PathBuf {
    let p = PathBuf::from(path);
    let p = if p.extension().is_none() { p.with_extension(FILE_EXTENSION) } else { p };
    if p.is_absolute() { p } else { std::env::current_dir().map(|c| c.join(&p)).unwrap_or(p) }
}

fn new_project(ctx: &Ctx, a: &Args) -> ToolResult {
    let name = a.req_str("name")?.trim().to_string();
    if name.is_empty() {
        return err("'name' must not be empty");
    }
    let start = match a.get("start") {
        Some(v) => parse_date(v, "start")?,
        None => today(),
    };
    let path = match a.str("path")? {
        Some(p) if !p.trim().is_empty() => {
            ctx.require_file_access("Saving to a file path")?;
            let p = with_extension(p.trim());
            if p.exists() && !a.bool("overwrite")?.unwrap_or(false) {
                return err(format!("{} already exists; use open_project to open it, or pass overwrite: true", p.display()));
            }
            Some(p)
        }
        _ => None,
    };
    let mut project = Project::new(name, start);
    for (key, field) in [
        ("manager", &mut project.manager),
        ("company", &mut project.company),
        ("author", &mut project.author),
        ("notes", &mut project.notes),
    ] {
        if let Some(v) = a.str(key)? {
            *field = v.to_string();
        }
    }
    let out = ctx.with_workspace(|ws| {
        let handle = ws.add(project, None);
        let i = ws.index_of(handle).expect("just added");
        if let Some(p) = path
            && let Err(e) = ws.docs[i].save_as(&p)
        {
            ws.close(handle);
            return Err(e.into());
        }
        Ok(project_overview(&ws.docs[i]))
    })?;
    ctx.mark_changed();
    Ok(out)
}

fn open_project(ctx: &Ctx, a: &Args) -> ToolResult {
    ctx.require_file_access("Opening files")?;
    let path = a.req_str("path")?.trim().to_string();
    let p = PathBuf::from(&path);
    let p = if p.exists() { p } else { with_extension(&path) };
    let out = ctx.with_workspace(|ws| {
        let handle = ws.open(&p)?;
        let i = ws.index_of(handle).expect("opened");
        Ok(project_overview(&ws.docs[i]))
    })?;
    ctx.mark_changed();
    Ok(out)
}

fn save_project(ctx: &Ctx, a: &Args) -> ToolResult {
    let new_path = match a.str("path")? {
        Some(p) if !p.trim().is_empty() => Some(with_extension(p.trim())),
        _ => None,
    };
    let out = ctx.with_doc(a, |doc| {
        match &new_path {
            Some(p) => {
                let same =
                    doc.path.as_ref().is_some_and(|cur| std::fs::canonicalize(cur).ok() == std::fs::canonicalize(p).ok() && p.exists());
                if !same {
                    ctx.require_file_access("Saving to a new path")?;
                }
                doc.save_as(p)?
            }
            None => {
                if doc.path.is_none() {
                    return err("the document has never been saved; pass 'path' to choose a file");
                }
                doc.save()?
            }
        }
        Ok(json!({
            "saved": true,
            "document": doc.handle,
            "path": doc.path.as_ref().map(|p| p.display().to_string()),
        }))
    })?;
    ctx.mark_changed();
    Ok(out)
}

fn close_project(ctx: &Ctx, a: &Args) -> ToolResult {
    let discard = a.bool("discard_changes")?.unwrap_or(false);
    let out = ctx.with_workspace(|ws| {
        let i = Ctx::doc_index(ws, a)?;
        let doc = &ws.docs[i];
        if doc.dirty && !discard {
            return err(format!("'{}' has unsaved changes; call save_project first or pass discard_changes: true", doc.title()));
        }
        let handle = doc.handle;
        let name = doc.project.name.clone();
        ws.close(handle);
        Ok(json!({
            "closed": handle,
            "name": name,
            "active": ws.active_doc().map(|d| d.handle),
            "remaining": ws.docs.len(),
        }))
    })?;
    ctx.mark_changed();
    Ok(out)
}

fn set_active_document(ctx: &Ctx, a: &Args) -> ToolResult {
    let out = ctx.with_workspace(|ws| {
        let i = Ctx::doc_index(ws, a)?;
        ws.active = Some(i);
        Ok(views::document_brief(&ws.docs[i], true))
    })?;
    ctx.mark_changed();
    Ok(out)
}

/// Project info + settings + calendars + statistics.
pub fn project_overview(doc: &Document) -> Value {
    let p = &doc.project;
    let st = report::project_stats(p);
    json!({
        "document": doc.handle,
        "project": views::project_info(doc),
        "settings": views::settings_json(p),
        "calendars": p.calendars.iter().map(|c| views::calendar_brief(p, c)).collect::<Vec<_>>(),
        "statistics": views::stats_json(p, &st),
        "external_projects": p.external_projects.iter().map(|e| json!({ "name": e.name, "path": e.path })).collect::<Vec<_>>(),
    })
}

fn get_project(ctx: &Ctx, a: &Args) -> ToolResult {
    ctx.read(a, |doc| Ok(project_overview(doc)))
}

fn update_project(ctx: &Ctx, a: &Args) -> ToolResult {
    ctx.edit(
        a,
        "Update project settings",
        |p, _| {
            for (key, field) in [
                ("name", &mut p.name),
                ("manager", &mut p.manager),
                ("company", &mut p.company),
                ("author", &mut p.author),
                ("notes", &mut p.notes),
            ] {
                if let Some(v) = a.str(key)? {
                    *field = v.to_string();
                }
            }
            if let Some(c) = a.str("currency")? {
                p.settings.currency = c.to_string();
            }
            if let Some(h) = a.f64("hours_per_day")? {
                if !(0.5..=24.0).contains(&h) {
                    return err("'hours_per_day' must be between 0.5 and 24");
                }
                p.settings.time.minutes_per_day = (h * 60.0).round() as i64;
            }
            if let Some(h) = a.f64("hours_per_week")? {
                if !(1.0..=168.0).contains(&h) {
                    return err("'hours_per_week' must be between 1 and 168");
                }
                p.settings.time.minutes_per_week = (h * 60.0).round() as i64;
            }
            if let Some(d) = a.u64("days_per_month")? {
                if !(1..=31).contains(&d) {
                    return err("'days_per_month' must be between 1 and 31");
                }
                p.settings.time.days_per_month = d as i64;
            }
            if let Some(t) = a.str("default_start_time")? {
                p.settings.time.default_start_time = parse_hhmm(t, "default_start_time")?;
            }
            if let Some(t) = a.str("default_finish_time")? {
                p.settings.time.default_finish_time = parse_hhmm(t, "default_finish_time")?;
            }
            if let Some(v) = a.get("duration_unit") {
                p.settings.duration_unit =
                    parse_enum::<DurationUnit>(v, "duration_unit", &["minutes", "hours", "days", "weeks", "months"])?;
            }
            if let Some(h) = a.f64("hours_per_point")? {
                if h <= 0.0 {
                    return err("'hours_per_point' must be positive");
                }
                p.settings.minutes_per_point = (h * 60.0).round() as i64;
            }
            if let Some(m) = a.u64("minutes_per_point")? {
                if m == 0 {
                    return err("'minutes_per_point' must be positive");
                }
                p.settings.minutes_per_point = m as i64;
            }
            if let Some(v) = a.get("calendar") {
                p.calendar = resolve_calendar(p, v)?;
            }
            if let Some(b) = a.bool("new_tasks_auto")? {
                p.settings.new_tasks_auto = b;
            }
            if let Some(b) = a.bool("use_resource_calendars")? {
                p.settings.use_resource_calendars = b;
            }
            if let Some(b) = a.bool("progress_from_time")? {
                p.settings.progress_from_time = b;
            }
            if let Some(v) = a.get("critical_slack") {
                p.settings.critical_slack = parse_duration_value(p, v, "critical_slack")?.0;
            }
            if a.is_clear("status_date") {
                p.status_date = None;
            } else if let Some(v) = a.get("status_date") {
                p.status_date = Some(parse_date(v, "status_date")?);
            }
            if let Some(v) = a.get("start") {
                p.start = parse_dt(p, v, false, "start")?;
            }
            Ok(())
        },
        |doc, _| Ok(project_overview(doc)),
    )
}
