//! MCP prompts: reusable starting points for common planning conversations.

use serde_json::{Value, json};

use crate::protocol::RpcError;

/// Prompts that build or change a plan (left out on a read-only server).
const CHANGING: [&str; 1] = ["plan_project"];

/// The prompts offered: on a read-only server, only those that read.
pub fn list(read_only: bool) -> Value {
    let mut all = all();
    if read_only && let Some(list) = all.get_mut("prompts").and_then(Value::as_array_mut) {
        list.retain(|p| p.get("name").and_then(Value::as_str).is_none_or(|n| !CHANGING.contains(&n)));
    }
    all
}

fn all() -> Value {
    json!({
        "prompts": [
            {
                "name": "plan_project",
                "title": "Plan a new project",
                "description": "Turn a goal into a scheduled PlanCraft project: WBS, estimates, dependencies, milestones and resources.",
                "arguments": [
                    { "name": "goal", "description": "What the project should achieve.", "required": true },
                    { "name": "start", "description": "Start date (YYYY-MM-DD).", "required": false },
                    { "name": "team", "description": "People or companies available, e.g. 'Ana (designer), Bob (dev, 50%)'.", "required": false }
                ]
            },
            {
                "name": "status_report",
                "title": "Project status report",
                "description": "Analyse the active project and write a status report: progress, critical path, late tasks, overallocations, milestones and recommendations.",
                "arguments": [
                    { "name": "document", "description": "Document handle or name (default: active document).", "required": false }
                ]
            }
        ]
    })
}

fn arg<'a>(params: &'a Value, name: &str) -> Option<&'a str> {
    params.get("arguments").and_then(|a| a.get(name)).and_then(Value::as_str).filter(|s| !s.trim().is_empty())
}

pub fn get(params: &Value, read_only: bool) -> Result<Value, RpcError> {
    let name = params.get("name").and_then(Value::as_str).ok_or_else(|| RpcError::invalid_params("missing prompt 'name'"))?;
    if read_only && CHANGING.contains(&name) {
        return Err(RpcError::invalid_params(format!("this server is read-only: the prompt '{name}' changes plans")));
    }
    let (description, text) = match name {
        "plan_project" => {
            let goal = arg(params, "goal").ok_or_else(|| RpcError::invalid_params("missing argument 'goal'"))?;
            let start = arg(params, "start").map(|s| format!(" starting {s}")).unwrap_or_default();
            let team = arg(params, "team").map(|t| format!("\nAvailable team: {t}.")).unwrap_or_default();
            (
                "Plan a new project",
                format!(
                    "Plan a project in PlanCraft{start} for this goal: {goal}{team}\n\n\
                     Steps:\n\
                     1. Call new_project (or use the active document after checking get_project).\n\
                     2. Create resources with create_resource (category, max_units, rates).\n\
                     3. Build the whole work breakdown structure in ONE create_tasks call: phases with nested 'children', \
                     realistic durations or estimates, 'key's on items and 'predecessors' referencing those keys, \
                     milestones (milestone: true, members) for key deliverables, and 'resources' text such as \"Ana[50%]\".\n\
                     4. Review with get_schedule and critical_path; check get_workload / get_overallocations and fix overloads \
                     (level_resources or adjust assignments).\n\
                     5. Call set_baseline once the plan is agreed and save_project if a path is known.\n\
                     Summarise the plan (finish date, critical path, milestones, risks) at the end."
                ),
            )
        }
        "status_report" => {
            let doc = arg(params, "document").map(|d| format!(" (document '{d}')")).unwrap_or_default();
            (
                "Project status report",
                format!(
                    "Write a status report for the PlanCraft project{doc}.\n\
                     Use project_statistics, get_schedule, critical_path, list_tasks with filter 'late' and 'overdue', \
                     milestone_status, get_overallocations and get_variance (if a baseline exists).\n\
                     Report: overall progress and forecast finish vs baseline, milestones at risk, late tasks and their owners, \
                     resource overallocations, earned value (if available), and concrete recommendations."
                ),
            )
        }
        other => {
            return Err(RpcError::invalid_params(format!("unknown prompt '{other}'")));
        }
    };
    Ok(json!({
        "description": description,
        "messages": [{ "role": "user", "content": { "type": "text", "text": text } }]
    }))
}
