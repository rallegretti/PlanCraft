//! Dependency tools, including cross-document links to external projects.

use std::path::PathBuf;

use plancraft_core::ids::TaskId;
use plancraft_core::io;
use plancraft_core::model::*;
use serde_json::{Value, json};

use super::*;
use crate::args::*;
use crate::views;

pub fn tools() -> Vec<Tool> {
    vec![
        Tool::new(
            "link_tasks",
            "Link two tasks",
            "Create (or update) a dependency predecessor → successor with a type (FS/SS/FF/SF) and optional lag/lead. Circular dependencies are rejected.",
            schema(
                json!({
                    "predecessor": task_ref("Predecessor task."),
                    "successor": task_ref("Successor task."),
                    "type": link_type(),
                    "lag": lag("Lag or lead."),
                }),
                &["predecessor", "successor"],
            ),
            link_tasks,
        ),
        Tool::new(
            "unlink_tasks",
            "Remove dependencies",
            "Remove the dependency predecessor → successor, or with 'tasks' remove all links among the given tasks (a single task: all of its links).",
            schema(
                json!({
                    "predecessor": task_ref("Predecessor task."),
                    "successor": task_ref("Successor task."),
                    "tasks": task_refs("Alternative: remove all links among these tasks."),
                }),
                &[],
            ),
            unlink_tasks,
        ),
        Tool::new(
            "link_chain",
            "Link tasks in sequence",
            "Link the tasks in the given order (each one a predecessor of the next), like Microsoft Project's 'Link Tasks'.",
            schema(
                json!({
                    "tasks": task_refs("Tasks in order (at least two)."),
                    "type": link_type(),
                    "lag": lag("Lag applied to every link."),
                }),
                &["tasks"],
            ),
            link_chain,
        ),
        Tool::new(
            "list_dependencies",
            "List dependencies",
            "List dependency links (all, or those of one task) with predecessor/successor ids, names, rows, type and lag. External predecessors are included.",
            schema(json!({ "task": task_ref("Only links into or out of this task.") }), &[]),
            list_dependencies,
        )
        .read_only(),
        Tool::new(
            "add_external_project",
            "Register external project",
            "Register another .plancraft document so its tasks can be used as predecessors or milestone members (cross-project dependencies). Save the current document first so a relative path can be stored.",
            schema(json!({ "path": string("Path of the other .plancraft file.") }), &["path"]),
            add_external_project,
        ),
        Tool::new(
            "remove_external_project",
            "Unregister external project",
            "Remove an external project and every link/milestone membership that references its tasks.",
            schema(json!({ "external_project": string("External project name, id or file name.") }), &["external_project"]),
            remove_external_project,
        )
        .destructive(),
        Tool::new(
            "list_external_projects",
            "List external projects",
            "List the external projects registered in this document, with resolved paths, whether the file exists, last sync time and the cached tasks currently referenced.",
            schema(json!({}), &[]),
            list_external_projects,
        )
        .read_only(),
        Tool::new(
            "list_external_tasks",
            "List tasks of an external project",
            "Read the other document from disk and list its tasks (id, name, dates, % complete, milestone flag) so you can pick one to link.",
            schema(json!({ "external_project": string("External project name, id or file name."), "search": string("Optional case-insensitive name filter.") }), &["external_project"]),
            list_external_tasks,
        )
        .read_only(),
        Tool::new(
            "link_external_task",
            "Link a task from another document",
            "Use a task from an external project as predecessor of a local task (successor) and/or as a member of a local milestone.",
            schema(
                json!({
                    "external_project": string("External project name, id or file name (see list_external_projects)."),
                    "task": { "type": ["integer", "string"], "description": "Task id (or exact name) inside the external project." },
                    "successor": task_ref("Local task that depends on the external task."),
                    "milestone": task_ref("Local milestone that should include the external task as a member."),
                    "type": link_type(),
                    "lag": lag("Lag or lead (for 'successor')."),
                }),
                &["external_project", "task"],
            ),
            link_external_task,
        ),
        Tool::new(
            "refresh_external_links",
            "Refresh external links",
            "Re-read every external project from disk, update the cached dates of referenced tasks and recalculate. Returns warnings for missing files or tasks.",
            schema(json!({}), &[]),
            refresh_external_links,
        ),
    ]
}

fn link_tasks(ctx: &Ctx, a: &Args) -> ToolResult {
    ctx.edit(
        a,
        "Link tasks",
        |p, _| {
            let pred = task_arg(p, a, "predecessor")?;
            let succ = task_arg(p, a, "successor")?;
            let kind = parse_link_type(a.get("type"))?;
            let lag = parse_lag_value(p, a.get("lag"), "lag")?;
            p.link(TaskRef::Local(pred), succ, kind, lag)?;
            Ok((pred, succ))
        },
        |doc, (pred, succ)| {
            let p = &doc.project;
            Ok(json!({
                "linked": true,
                "predecessor": views::task_row(p, p.task_index(pred).ok_or_else(|| missing("exists"))?),
                "successor": views::task_row(p, p.task_index(succ).ok_or_else(|| missing("exists"))?),
            }))
        },
    )
}

fn unlink_tasks(ctx: &Ctx, a: &Args) -> ToolResult {
    ctx.edit(
        a,
        "Unlink tasks",
        |p, _| {
            if a.get("tasks").is_some() {
                let ids = task_list_arg(p, a, "tasks")?;
                let before: usize = p.tasks.iter().map(|t| t.predecessors.len()).sum();
                p.unlink_tasks(&ids);
                let after: usize = p.tasks.iter().map(|t| t.predecessors.len()).sum();
                return Ok(before - after);
            }
            if a.get("predecessor").is_none() || a.get("successor").is_none() {
                return err("give 'predecessor' and 'successor', or 'tasks'");
            }
            let succ = task_arg(p, a, "successor")?;
            let pred = resolve_task_ref(p, a.get("predecessor").ok_or_else(|| missing("checked"))?, None)?;
            if !p.unlink(pred, succ)? {
                return err("these tasks are not linked");
            }
            Ok(1)
        },
        |_, removed| Ok(json!({ "removed_links": removed })),
    )
}

fn link_chain(ctx: &Ctx, a: &Args) -> ToolResult {
    ctx.edit(
        a,
        "Link tasks",
        |p, _| {
            let ids = task_list_arg(p, a, "tasks")?;
            if ids.len() < 2 {
                return err("'tasks' needs at least two tasks");
            }
            let kind = parse_link_type(a.get("type"))?;
            let lag = parse_lag_value(p, a.get("lag"), "lag")?;
            for pair in ids.windows(2) {
                if let [pred, succ] = pair {
                    p.link(TaskRef::Local(*pred), *succ, kind, lag).map_err(|e| ToolError(format!("linking {} → {}: {e}", pred.0, succ.0)))?;
                }
            }
            Ok(ids)
        },
        |doc, ids| {
            let p = &doc.project;
            Ok(json!({ "links": ids.len() - 1, "tasks": ids.iter().filter_map(|id| p.task_index(*id)).map(|i| views::task_row(p, i)).collect::<Vec<_>>() }))
        },
    )
}

fn link_json(p: &Project, succ: &Task, d: &Dependency) -> Value {
    let mut v = json!({
        "predecessor": views::task_ref_json(p, &d.predecessor),
        "successor": { "id": succ.id.0, "row": p.row_number(succ.id), "name": succ.name },
        "type": d.kind.code(),
        "lag": views::lag_json(p, d.lag),
    });
    if matches!(d.predecessor, TaskRef::External { .. }) {
        set(&mut v, "external", json!(true));
    }
    v
}

fn list_dependencies(ctx: &Ctx, a: &Args) -> ToolResult {
    ctx.read(a, |doc| {
        let p = &doc.project;
        let only = match a.get("task") {
            Some(v) => Some(resolve_task(p, v)?),
            None => None,
        };
        let mut links = Vec::new();
        for t in &p.tasks {
            for d in &t.predecessors {
                if let Some(o) = only
                    && t.id != o
                    && d.predecessor != TaskRef::Local(o)
                {
                    continue;
                }
                links.push(link_json(p, t, d));
            }
        }
        let mut memberships = Vec::new();
        for t in p.tasks.iter().filter(|t| !t.members.is_empty()) {
            for m in &t.members {
                if let Some(o) = only
                    && t.id != o
                    && *m != TaskRef::Local(o)
                {
                    continue;
                }
                memberships.push(json!({ "milestone": { "id": t.id.0, "name": t.name }, "member": views::task_ref_json(p, m) }));
            }
        }
        Ok(json!({ "count": links.len(), "links": links, "milestone_memberships": memberships }))
    })
}

fn add_external_project(ctx: &Ctx, a: &Args) -> ToolResult {
    ctx.require_file_access("Linking external files")?;
    let path = PathBuf::from(a.req_str("path")?.trim());
    ctx.edit(
        a,
        "Add external project",
        |p, doc_path| {
            let id = io::add_external_project(p, doc_path, &path)?;
            if id == p.id {
                return err("a project cannot reference itself");
            }
            Ok((id, doc_path.is_none()))
        },
        |doc, (id, unsaved)| {
            let p = &doc.project;
            let e = p.external(id).ok_or_else(|| missing("added"))?;
            Ok(json!({
                "external_project": { "id": e.id.to_string(), "name": e.name, "path": e.path },
                "note": if unsaved { "This document has not been saved yet, so an absolute path was stored." } else { "" },
                "next": "Use list_external_tasks to find task ids, then link_external_task.",
            }))
        },
    )
}

fn remove_external_project(ctx: &Ctx, a: &Args) -> ToolResult {
    ctx.edit(
        a,
        "Remove external project",
        |p, _| {
            let ext = resolve_external(p, a.get("external_project").unwrap_or(&Value::Null))?;
            let mut removed = 0;
            for t in &mut p.tasks {
                let before = t.predecessors.len() + t.members.len();
                t.predecessors.retain(|d| !matches!(d.predecessor, TaskRef::External { project, .. } if project == ext));
                t.members.retain(|m| !matches!(m, TaskRef::External { project, .. } if *project == ext));
                removed += before - t.predecessors.len() - t.members.len();
            }
            let name = p.external(ext).map(|e| e.name.clone()).unwrap_or_default();
            p.external_projects.retain(|e| e.id != ext);
            Ok((name, removed))
        },
        |_, (name, removed)| Ok(json!({ "removed": name, "links_removed": removed })),
    )
}

fn list_external_projects(ctx: &Ctx, a: &Args) -> ToolResult {
    ctx.read(a, |doc| {
        let p = &doc.project;
        let list: Vec<Value> = p
            .external_projects
            .iter()
            .map(|e| {
                let resolved = io::resolve_external_path(&e.path, doc.path.as_deref());
                json!({
                    "id": e.id.to_string(),
                    "name": e.name,
                    "path": e.path,
                    "resolved_path": resolved.display().to_string(),
                    "exists": resolved.exists(),
                    "last_sync": fmt_dt(e.last_sync),
                    "referenced_tasks": e.tasks.iter().map(|s| json!({
                        "id": s.id.0, "name": s.name, "start": fmt_dt(s.start), "finish": fmt_dt(s.finish),
                        "percent_complete": s.percent_complete, "milestone": s.milestone,
                    })).collect::<Vec<_>>(),
                })
            })
            .collect();
        Ok(json!({ "external_projects": list }))
    })
}

fn list_external_tasks(ctx: &Ctx, a: &Args) -> ToolResult {
    ctx.read(a, |doc| {
        let p = &doc.project;
        let ext = resolve_external(p, a.get("external_project").unwrap_or(&Value::Null))?;
        let search = a.str("search")?.map(|s| s.to_lowercase());
        let tasks = io::external_task_list(p, doc.path.as_deref(), ext)?;
        let list: Vec<Value> = tasks
            .iter()
            .filter(|s| search.as_ref().is_none_or(|q| s.name.to_lowercase().contains(q)))
            .map(|s| {
                json!({
                    "id": s.id.0, "name": s.name, "start": fmt_dt(s.start), "finish": fmt_dt(s.finish),
                    "duration": views::dur(p, s.duration), "percent_complete": s.percent_complete, "milestone": s.milestone,
                })
            })
            .collect();
        Ok(json!({ "external_project": p.external(ext).map(|e| e.name.clone()), "count": list.len(), "tasks": list }))
    })
}

fn link_external_task(ctx: &Ctx, a: &Args) -> ToolResult {
    ctx.edit(
        a,
        "Link external task",
        |p, doc_path| {
            let ext = resolve_external(p, a.get("external_project").unwrap_or(&Value::Null))?;
            let task_v = a.get("task").ok_or_else(|| ToolError("missing 'task'".into()))?;
            let task = match task_v.as_u64().or_else(|| task_v.as_str().and_then(|s| s.trim().parse().ok())) {
                Some(n) => TaskId(n as u32),
                None => {
                    let name = task_v.as_str().unwrap_or_default();
                    let list = io::external_task_list(p, doc_path, ext)?;
                    let hits: Vec<_> = list.iter().filter(|s| s.name.eq_ignore_ascii_case(name.trim())).collect();
                    match hits.as_slice() {
                        [one] => one.id,
                        [] => return err(format!("no task named '{name}' in the external project (see list_external_tasks)")),
                        _ => return err(format!("task name '{name}' is ambiguous in the external project; use its id")),
                    }
                }
            };
            io::ensure_external_task(p, doc_path, ext, task)?;
            if p.external(ext).and_then(|e| e.task(task)).is_none() {
                return err(format!("task {} does not exist in the external project (see list_external_tasks)", task.0));
            }
            let r = TaskRef::External { project: ext, task };
            let succ = match a.get("successor") {
                Some(v) => Some(resolve_task(p, v)?),
                None => None,
            };
            let ms = match a.get("milestone") {
                Some(v) => Some(resolve_task(p, v)?),
                None => None,
            };
            if succ.is_none() && ms.is_none() {
                return err("give 'successor' and/or 'milestone'");
            }
            if let Some(s) = succ {
                let kind = parse_link_type(a.get("type"))?;
                let lag = parse_lag_value(p, a.get("lag"), "lag")?;
                p.link(r, s, kind, lag)?;
            }
            if let Some(m) = ms {
                p.add_milestone_members(m, &[r])?;
            }
            Ok((r, succ, ms))
        },
        |doc, (r, succ, ms)| {
            let p = &doc.project;
            Ok(json!({
                "external_task": views::task_ref_json(p, &r),
                "successor": succ.and_then(|s| p.task_index(s)).map(|i| views::task_row(p, i)),
                "milestone": ms.and_then(|s| p.task_index(s)).map(|i| views::task_row(p, i)),
            }))
        },
    )
}

fn refresh_external_links(ctx: &Ctx, a: &Args) -> ToolResult {
    let out = ctx.with_doc(a, |doc| {
        let warnings = doc.refresh_external();
        let p = &doc.project;
        Ok(json!({
            "refreshed": p.external_projects.len(),
            "warnings": warnings,
            "project_finish": fmt_dt(p.sched.finish),
        }))
    })?;
    ctx.mark_changed();
    Ok(out)
}
