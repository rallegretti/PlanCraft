//! Assignment tools: resources working on tasks.

use plancraft_core::ids::AssignmentId;
use plancraft_core::model::*;
use serde_json::{Value, json};

use super::*;
use crate::args::*;
use crate::views;

const CONTOURS: [&str; 4] = ["flat", "front_loaded", "back_loaded", "bell"];

fn assignment_selector() -> Value {
    json!({
        "assignment": integer("Assignment id (from get_task / list_assignments)."),
        "task": task_ref("Task (with 'resource', alternative to 'assignment')."),
        "resource": resource_ref("Resource (with 'task')."),
    })
}

pub fn tools() -> Vec<Tool> {
    let mut update = match assignment_selector() {
        Value::Object(m) => m,
        _ => Default::default(),
    };
    update.insert("units".into(), units("Assignment units (for material resources: quantity)."));
    update.insert("work".into(), work("Planned work."));
    update.insert("actual_work".into(), work("Actual work entered directly (in addition to logged time)."));
    update.insert("contour".into(), string_enum("How work is spread over the task.", &CONTOURS));
    update.insert("cost".into(), number("Cost amount (for cost resources)."));
    vec![
        Tool::new(
            "assign_resource",
            "Assign a resource",
            "Assign a resource to a task (or update the existing assignment). Units default to 100%. Without 'work', work follows the task duration, or the task's estimate for the first assignment of an estimated task. Effort-driven tasks redistribute work among resources.",
            schema(
                json!({
                    "task": task_ref("Task."),
                    "resource": resource_ref("Resource."),
                    "units": units("Units, e.g. \"50%\" (material: quantity)."),
                    "work": work("Planned work; the duration follows from work ÷ units."),
                    "cost": number("Cost amount (for cost resources)."),
                    "contour": string_enum("Work contour.", &CONTOURS),
                }),
                &["task", "resource"],
            ),
            assign_resource,
        ),
        Tool::new(
            "update_assignment",
            "Update an assignment",
            "Change an assignment's units, work, actual work, contour or cost. Identify it by 'assignment' id or by 'task' + 'resource'.",
            schema(Value::Object(update), &[]),
            update_assignment,
        ),
        Tool::new(
            "unassign_resource",
            "Remove an assignment",
            "Remove a resource from a task. Identify the assignment by id or by 'task' + 'resource'.",
            schema(assignment_selector(), &[]),
            unassign_resource,
        ),
        Tool::new(
            "list_assignments",
            "List assignments",
            "List assignments (all, or for a task or a resource) with units, work, actual and remaining work, cost and dates.",
            schema(json!({ "task": task_ref("Only this task."), "resource": resource_ref("Only this resource.") }), &[]),
            list_assignments,
        )
        .read_only(),
    ]
}

fn find_assignment(p: &Project, a: &Args) -> ToolResult<AssignmentId> {
    if let Some(id) = a.u64("assignment")? {
        let id = AssignmentId(id as u32);
        return p.assignment(id).map(|x| x.id).ok_or_else(|| ToolError(format!("no assignment with id {}", id.0)));
    }
    if a.get("task").is_none() || a.get("resource").is_none() {
        return err("give 'assignment' or both 'task' and 'resource'");
    }
    let t = task_arg(p, a, "task")?;
    let r = resource_arg(p, a, "resource")?;
    p.assignments
        .iter()
        .find(|x| x.task == t && x.resource == r)
        .map(|x| x.id)
        .ok_or_else(|| ToolError(format!("resource {} is not assigned to task {}", r.0, t.0)))
}

fn apply_assignment_extras(p: &mut Project, id: AssignmentId, a: &Args) -> ToolResult<()> {
    let actual = a.get("actual_work").map(|v| parse_work_value(p, v, "actual_work")).transpose()?;
    let contour: Option<WorkContour> = a.get("contour").map(|v| parse_enum(v, "contour", &CONTOURS)).transpose()?;
    let cost = a.f64("cost")?;
    let asg = p.assignments.iter_mut().find(|x| x.id == id).ok_or_else(|| ToolError("assignment vanished".into()))?;
    if let Some(m) = actual {
        asg.actual_work = m;
    }
    if let Some(c) = contour {
        asg.contour = c;
    }
    if let Some(c) = cost {
        asg.cost = c;
    }
    Ok(())
}

fn assign_resource(ctx: &Ctx, a: &Args) -> ToolResult {
    ctx.edit(
        a,
        "Assign resource",
        |p, _| {
            let t = task_arg(p, a, "task")?;
            let r = resource_arg(p, a, "resource")?;
            let units = a.get("units").map(|v| parse_units_value(v, "units")).transpose()?;
            let work = a.get("work").map(|v| parse_work_value(p, v, "work")).transpose()?;
            let id = p.assign(t, r, units, work)?;
            apply_assignment_extras(p, id, a)?;
            Ok(id)
        },
        |doc, id| {
            let p = &doc.project;
            let asg = p.assignment(id).expect("exists");
            Ok(json!({
                "assignment": views::assignment_json(p, asg),
                "task": views::task_row(p, p.task_index(asg.task).expect("exists")),
            }))
        },
    )
}

fn update_assignment(ctx: &Ctx, a: &Args) -> ToolResult {
    ctx.edit(
        a,
        "Update assignment",
        |p, _| {
            let id = find_assignment(p, a)?;
            if let Some(v) = a.get("units") {
                let u = parse_units_value(v, "units")?;
                p.set_assignment_units(id, u)?;
            }
            if let Some(v) = a.get("work") {
                let w = parse_work_value(p, v, "work")?;
                p.set_assignment_work(id, w)?;
            }
            apply_assignment_extras(p, id, a)?;
            Ok(id)
        },
        |doc, id| {
            let p = &doc.project;
            let asg = p.assignment(id).expect("exists");
            Ok(json!({
                "assignment": views::assignment_json(p, asg),
                "task": views::task_row(p, p.task_index(asg.task).expect("exists")),
            }))
        },
    )
}

fn unassign_resource(ctx: &Ctx, a: &Args) -> ToolResult {
    ctx.edit(
        a,
        "Remove assignment",
        |p, _| {
            let id = find_assignment(p, a)?;
            let task = p.assignment(id).expect("exists").task;
            p.unassign(id)?;
            Ok(task)
        },
        |doc, task| {
            let p = &doc.project;
            Ok(json!({ "removed": true, "task": p.task_index(task).map(|i| views::task_row(p, i)) }))
        },
    )
}

fn list_assignments(ctx: &Ctx, a: &Args) -> ToolResult {
    ctx.read(a, |doc| {
        let p = &doc.project;
        let t = a.get("task").map(|v| resolve_task(p, v)).transpose()?;
        let r = a.get("resource").map(|v| resolve_resource(p, v)).transpose()?;
        let list: Vec<Value> = p
            .assignments
            .iter()
            .filter(|x| t.is_none_or(|t| x.task == t) && r.is_none_or(|r| x.resource == r))
            .map(|x| views::assignment_json(p, x))
            .collect();
        let total_work: i64 =
            p.assignments.iter().filter(|x| t.is_none_or(|t| x.task == t) && r.is_none_or(|r| x.resource == r)).map(|x| x.work).sum();
        Ok(json!({
            "count": list.len(),
            "total_work": plancraft_core::duration::format_work(total_work),
            "total_work_minutes": total_work,
            "assignments": list,
        }))
    })
}
