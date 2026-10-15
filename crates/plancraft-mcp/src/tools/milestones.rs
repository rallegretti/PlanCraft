//! Milestone tools: creation, membership and status.

use plancraft_core::io;
use plancraft_core::model::*;
use plancraft_core::report;
use serde_json::{Value, json};

use super::*;
use crate::args::*;
use crate::tools::tasks::{apply_task_fields, apply_task_links, insert_new_task, insert_position};
use crate::views;

fn member_refs_schema(desc: &str) -> Value {
    json!({
        "type": "array",
        "items": {
            "type": ["integer", "string", "object"],
            "properties": {
                "external_project": { "type": "string", "description": "Registered external project name or id." },
                "task": { "type": "integer", "description": "Task id inside the external project." }
            }
        },
        "description": format!("{desc} Local task ids/names, or {{\"external_project\": \"Other\", \"task\": 12}} for tasks in another document (register it with add_external_project first).")
    })
}

pub fn tools() -> Vec<Tool> {
    vec![
        Tool::new(
            "create_milestone",
            "Create a milestone",
            "Create a milestone: a zero-duration checkpoint that is reached when all of its member tasks (local or in other documents) and predecessors finish. Members do not need to be linked individually.",
            schema(
                json!({
                    "name": string("Milestone name."),
                    "deliverable": string("The output achieved at this milestone."),
                    "members": member_refs_schema("Tasks whose completion makes up this milestone."),
                    "deadline": date("Deadline; a warning is raised if the milestone is reached later."),
                    "date": date("Target date: the milestone will not occur before this date (Start No Earlier Than constraint)."),
                    "predecessors": predecessors("Predecessor links."),
                    "notes": string("Notes."),
                    "show_on_timeline": boolean("Show on the timeline (default true)."),
                    "parent": task_ref("Insert as the last child of this summary."),
                    "after": task_ref("Insert after this task."),
                    "before": task_ref("Insert before this task."),
                    "index": integer("Insert at this 1-based row."),
                }),
                &["name"],
            ),
            create_milestone,
        ),
        Tool::new(
            "update_milestone_members",
            "Change milestone members",
            "Add and/or remove member tasks of a milestone (local tasks or tasks in external projects).",
            schema(
                json!({
                    "milestone": task_ref("The milestone."),
                    "add": member_refs_schema("Members to add."),
                    "remove": member_refs_schema("Members to remove."),
                }),
                &["milestone"],
            ),
            update_milestone_members,
        ),
        Tool::new(
            "milestone_status",
            "Milestone status",
            "Status of every milestone: date, deadline, baseline, member count and completion, work-weighted progress, late and complete flags.",
            schema(json!({}), &[]),
            milestone_status,
        )
        .read_only(),
    ]
}

/// Resolves member references, refreshing snapshots of external tasks.
fn resolve_members(
    p: &mut Project,
    doc_path: Option<&std::path::Path>,
    items: &[Value],
) -> ToolResult<Vec<TaskRef>> {
    let mut refs = Vec::new();
    for m in items {
        let r = resolve_task_ref(p, m, None)?;
        if let TaskRef::External { project, task } = r {
            io::ensure_external_task(p, doc_path, project, task)?;
            if p.external(project).and_then(|e| e.task(task)).is_none() {
                return err(format!(
                    "task {} does not exist in external project '{}'",
                    task.0,
                    p.external(project)
                        .map(|e| e.name.clone())
                        .unwrap_or_default()
                ));
            }
        }
        refs.push(r);
    }
    Ok(refs)
}

fn create_milestone(ctx: &Ctx, a: &Args) -> ToolResult {
    let name = a.req_str("name")?.trim().to_string();
    if name.is_empty() {
        return err("'name' must not be empty");
    }
    ctx.edit(
        a,
        &format!("Add milestone '{name}'"),
        |p, path| {
            let (index, level) = insert_position(p, a)?;
            let id = insert_new_task(p, &name, index, level, true);
            {
                let t = p.task_mut(id).expect("inserted");
                t.show_on_timeline = a.bool("show_on_timeline")?.unwrap_or(true);
            }
            // Reuse the generic field handling for deliverable, deadline, notes.
            let mut fields = serde_json::Map::new();
            for k in ["deliverable", "deadline", "notes", "predecessors"] {
                if let Some(v) = a.map().get(k) {
                    fields.insert(k.into(), v.clone());
                }
            }
            if let Some(d) = a.get("date") {
                let dt = parse_dt(p, d, true, "date")?;
                p.task_mut(id).expect("inserted").constraint = Constraint {
                    kind: ConstraintKind::Snet,
                    date: Some(dt),
                };
            }
            let fa = Args::new(&fields);
            apply_task_fields(p, id, &fa)?;
            apply_task_links(p, id, &fa, None)?;
            if let Some(members) = a.array("members")? {
                let refs = resolve_members(p, path, members)?;
                p.add_milestone_members(id, &refs)?;
            }
            Ok(id)
        },
        |doc, id| Ok(views::task_detail(&doc.project, id)),
    )
}

fn update_milestone_members(ctx: &Ctx, a: &Args) -> ToolResult {
    ctx.edit(
        a,
        "Change milestone members",
        |p, path| {
            let id = task_arg(p, a, "milestone")?;
            if !p.task(id).is_some_and(|t| t.is_milestone()) {
                return err(format!("task {} is not a milestone (use update_task with is_milestone: true to convert it)", id.0));
            }
            if let Some(rm) = a.array("remove")? {
                let mut refs = Vec::new();
                for m in rm {
                    refs.push(resolve_task_ref(p, m, None)?);
                }
                p.remove_milestone_members(id, &refs)?;
            }
            if let Some(add) = a.array("add")? {
                let refs = resolve_members(p, path, add)?;
                p.add_milestone_members(id, &refs)?;
            }
            Ok(id)
        },
        |doc, id| Ok(views::task_detail(&doc.project, id)),
    )
}

fn milestone_status(ctx: &Ctx, a: &Args) -> ToolResult {
    ctx.read(a, |doc| {
        let p = &doc.project;
        let at = report::status_moment(p);
        let list = report::milestone_status(p, at);
        Ok(json!({
            "status_date": fmt_dt(Some(at)),
            "milestones": list.iter().map(|m| views::milestone_status_json(p, m)).collect::<Vec<_>>(),
        }))
    })
}
