//! Task tools: listing, details, creation (single and bulk WBS), updates and outline editing.

use std::collections::HashSet;

use plancraft_core::ids::TaskId;
use plancraft_core::model::*;
use plancraft_core::report;
use plancraft_core::schedule::recalculate;
use serde_json::{Map, Value, json};

use super::*;
use crate::args::*;
use crate::views;

pub fn tools() -> Vec<Tool> {
    let position = json!({
        "parent": task_ref("Insert as the last child of this task (it becomes a summary)."),
        "after": task_ref("Insert right after this task (and its subtasks), at the same outline level."),
        "before": task_ref("Insert right before this task, at the same outline level."),
        "index": integer("Insert at this 1-based row position."),
    });
    let mut create_props = task_field_props();
    if let Value::Object(p) = position.clone() {
        create_props.extend(p);
    }
    let mut update_props = task_field_props();
    update_props.insert("task".into(), task_ref("Task to update."));

    let mut bulk_props = Map::new();
    bulk_props.insert(
        "tasks".into(),
        json!({
            "type": "array",
            "items": {
                "type": "object",
                "properties": {
                    "name": { "type": "string" },
                    "key": { "type": "string", "description": "Batch-local key other items can use in 'predecessors'/'members'." },
                    "children": { "type": "array", "items": { "type": "object" }, "description": "Nested subtasks (same item format)." },
                    "level": { "type": "integer", "description": "Relative outline level (1 = top of batch) for flat lists instead of 'children'." },
                    "duration": { "type": ["string", "number"] },
                    "predecessors": { "type": ["array", "string"], "description": "Like create_task; may use batch keys." },
                    "resources": { "type": ["string", "array"], "description": "Text like \"Ana[50%], Bob\" or an array of names/ids/{resource, units}. Unknown names create resources." },
                    "milestone": { "type": "boolean" },
                    "members": { "type": "array", "description": "Milestone members: ids, names or batch keys." }
                },
                "required": ["name"]
            },
            "description": "Tasks in outline order. Each item takes 'name' and ANY create_task field (estimate, start, deadline, notes, percent_complete, ...); nest with 'children' or use relative 'level'."
        }),
    );
    if let Value::Object(p) = position {
        bulk_props.extend(p);
    }

    vec![
        Tool::new(
            "list_tasks",
            "List tasks",
            "List tasks in outline order with id, row, WBS, level, kind, duration, start/finish, % complete, critical flag, slack, work, predecessors and resources. Filter by category, search text or subtree.",
            schema(
                json!({
                    "filter": string_enum("Which tasks to include (default all).", &["all", "critical", "late", "overdue", "milestones", "summary", "incomplete", "complete", "in_progress", "unassigned", "manual", "warnings"]),
                    "search": string("Case-insensitive text to match in task names or notes."),
                    "parent": task_ref("Only list the subtasks of this summary task."),
                    "max_level": integer("Only include tasks up to this outline level."),
                    "include_schedule": boolean("Include dates, slack, work, predecessors and resources (default true). Set false for a compact id/name outline."),
                    "limit": integer("Maximum number of tasks to return (default 500)."),
                    "offset": integer("Number of matching tasks to skip (for paging)."),
                }),
                &[],
            ),
            list_tasks,
        )
        .read_only(),
        Tool::new(
            "get_task",
            "Get task details",
            "Get every detail of one task: schedule (early/late dates, slack), constraint, deadline, progress and actuals, estimate, cost, predecessors and successors, assignments, milestone membership, parent/children, baseline and variance, notes and warnings.",
            schema(json!({ "task": task_ref("Task to show.") }), &["task"]),
            get_task,
        )
        .read_only(),
        Tool::new(
            "create_task",
            "Create a task",
            "Create one task. Position it with parent/after/before/index (default: appended at the top level). New tasks default to an estimated 1-day duration; set 'duration', 'estimate', 'predecessors', 'resources' etc. in the same call. For many tasks use create_tasks.",
            schema(Value::Object(create_props), &["name"]),
            create_task,
        ),
        Tool::new(
            "create_tasks",
            "Create many tasks (WBS)",
            "Create a whole work breakdown structure in one call (one undo step). Items nest via 'children' (or flat items with 'level'); each item accepts every create_task field. Use 'key' on items and reference them in other items' 'predecessors' (e.g. [{\"task\": \"design\", \"type\": \"FS\"}]) or milestone 'members'. Returns the created ids.",
            schema(Value::Object(bulk_props), &["tasks"]),
            create_tasks,
        ),
        Tool::new(
            "update_task",
            "Update a task",
            "Change any editable field of a task; only the given fields change. 'start'/'finish' create SNET/FNET constraints for auto-scheduled tasks. Returns the updated task.",
            schema(Value::Object(update_props), &["task"]),
            update_task,
        ),
        Tool::new(
            "update_tasks",
            "Update many tasks",
            "Apply updates to several tasks in one call (one undo step). Each item is {\"task\": id, ...any update_task fields}.",
            schema(
                json!({ "updates": { "type": "array", "items": { "type": "object", "properties": { "task": { "type": ["integer", "string"] } }, "required": ["task"] }, "description": "List of {\"task\": id, ...any update_task fields}." } }),
                &["updates"],
            ),
            update_tasks,
        ),
        Tool::new(
            "delete_tasks",
            "Delete tasks",
            "Delete tasks together with their subtasks, links, assignments and time entries.",
            schema(json!({ "tasks": task_refs("Tasks to delete.") }), &["tasks"]),
            delete_tasks,
        )
        .destructive(),
        Tool::new(
            "move_task",
            "Move a task",
            "Move a task (with its subtasks) in the outline: before/after another task (taking that task's level), as the last child of a parent, or to a row index.",
            schema(
                json!({
                    "task": task_ref("Task to move."),
                    "before": task_ref("Place before this task."),
                    "after": task_ref("Place after this task (and its subtasks)."),
                    "parent": task_ref("Make it the last child of this task."),
                    "index": integer("Move to this 1-based row position."),
                }),
                &["task"],
            ),
            move_task,
        ),
        Tool::new(
            "indent_tasks",
            "Indent tasks",
            "Demote tasks one outline level, making them subtasks of the task above.",
            schema(json!({ "tasks": task_refs("Tasks to indent.") }), &["tasks"]),
            indent_tasks,
        ),
        Tool::new(
            "outdent_tasks",
            "Outdent tasks",
            "Promote tasks one outline level.",
            schema(json!({ "tasks": task_refs("Tasks to outdent.") }), &["tasks"]),
            outdent_tasks,
        ),
    ]
}

// ---------------------------------------------------------------------------
// Listing
// ---------------------------------------------------------------------------

fn list_tasks(ctx: &Ctx, a: &Args) -> ToolResult {
    ctx.read(a, |doc| {
        let p = &doc.project;
        let filter = a.str("filter")?.unwrap_or("all").to_ascii_lowercase();
        let search = a.str("search")?.map(|s| s.to_lowercase());
        let max_level = a.u64("max_level")?;
        let include = a.bool("include_schedule")?.unwrap_or(true);
        let limit = a.u64("limit")?.unwrap_or(500) as usize;
        let offset = a.u64("offset")?.unwrap_or(0) as usize;
        let range = match a.get("parent") {
            Some(v) => {
                let id = resolve_task(p, v)?;
                let i = p.task_index(id).ok_or_else(|| missing("resolved"))?;
                p.descendants_range(i)
            }
            None => 0..p.tasks.len(),
        };
        let at = report::status_moment(p);
        let mut rows = Vec::new();
        for i in range {
            let t = row(p, i)?;
            let leaf = !t.sched.is_summary;
            let keep = match filter.as_str() {
                "all" => true,
                "critical" => t.sched.critical && leaf,
                "late" => report::is_late(t, at, p),
                "overdue" => leaf && !t.is_complete() && t.sched.finish.is_some_and(|f| f < at),
                "milestones" | "milestone" => t.is_milestone(),
                "summary" | "summaries" => t.sched.is_summary,
                "incomplete" => leaf && !t.is_complete(),
                "complete" | "completed" => leaf && t.is_complete(),
                "in_progress" => leaf && t.percent_complete > 0 && !t.is_complete(),
                "unassigned" => leaf && !t.is_milestone() && p.assignments_for_task(t.id).next().is_none(),
                "manual" => t.mode == ScheduleMode::Manual,
                "warnings" => !t.sched.warnings.is_empty(),
                other => return err(format!("unknown filter '{other}'")),
            };
            if !keep || max_level.is_some_and(|m| t.outline_level as u64 > m) {
                continue;
            }
            if let Some(s) = &search
                && !t.name.to_lowercase().contains(s)
                && !t.notes.to_lowercase().contains(s)
            {
                continue;
            }
            rows.push(i);
        }
        let total = rows.len();
        let tasks: Vec<Value> = rows
            .iter()
            .skip(offset)
            .take(limit)
            .map(|&i| {
                if include {
                    views::task_row(p, i)
                } else {
                    let Some(t) = p.tasks.get(i) else { return Value::Null };
                    json!({ "id": t.id.0, "row": i + 1, "wbs": p.wbs(i), "name": t.name, "level": t.outline_level, "kind": if t.sched.is_summary { "summary" } else if t.is_milestone() { "milestone" } else { "task" }, "percent_complete": round2(t.sched.percent_complete as f64) })
                }
            })
            .collect();
        Ok(json!({
            "project": p.name,
            "filter": filter,
            "total": total,
            "returned": tasks.len(),
            "offset": offset,
            "tasks": tasks,
        }))
    })
}

fn get_task(ctx: &Ctx, a: &Args) -> ToolResult {
    ctx.read(a, |doc| {
        let id = task_arg(&doc.project, a, "task")?;
        Ok(views::task_detail(&doc.project, id))
    })
}

// ---------------------------------------------------------------------------
// Field application shared by create/update
// ---------------------------------------------------------------------------

fn parse_color(v: &Value) -> ToolResult<Option<[u8; 3]>> {
    match v {
        Value::Null => Ok(None),
        Value::String(s) if s.trim().is_empty() => Ok(None),
        Value::String(s) => {
            let h = s.trim().trim_start_matches('#');
            if h.len() != 6 {
                return err("'bar_color' must look like \"#RRGGBB\"");
            }
            // `get` refuses ranges inside multi-byte characters ("ééé" is 6 bytes too).
            let c = |i: usize| {
                h.get(i..i + 2)
                    .and_then(|pair| u8::from_str_radix(pair, 16).ok())
                    .ok_or_else(|| ToolError("'bar_color' must look like \"#RRGGBB\"".into()))
            };
            Ok(Some([c(0)?, c(2)?, c(4)?]))
        }
        other => err(format!("'bar_color' must be a string, got {other}")),
    }
}

fn opt_dt(p: &Project, a: &Args, key: &str, finish: bool) -> ToolResult<Option<Option<chrono::NaiveDateTime>>> {
    if a.is_clear(key) {
        return Ok(Some(None));
    }
    match a.get(key) {
        Some(v) => Ok(Some(Some(parse_dt(p, v, finish, key)?))),
        None => Ok(None),
    }
}

fn idx_of(p: &Project, id: TaskId) -> ToolResult<usize> {
    p.task_index(id).ok_or_else(|| ToolError(format!("task {} not found", id.0)))
}

/// Applies scalar fields (everything except links, resources, members and progress).
pub fn apply_task_fields(p: &mut Project, id: TaskId, a: &Args) -> ToolResult<()> {
    if let Some(name) = a.str("name")? {
        let name = name.trim();
        if name.is_empty() {
            return err("'name' must not be empty");
        }
        let i = idx_of(p, id)?;
        row_mut(p, i)?.name = name.to_string();
    }
    let milestone_flag = match (a.bool("is_milestone")?, a.bool("milestone").ok().flatten()) {
        (Some(b), _) | (None, Some(b)) => Some(b),
        _ => None,
    };
    if let Some(b) = milestone_flag {
        let is_ms = p.task(id).is_some_and(|t| t.is_milestone());
        if b != is_ms {
            p.set_milestone(id, b)?;
        }
    }
    if let Some(v) = a.get("mode") {
        let mode: ScheduleMode = parse_enum(v, "mode", &["auto", "manual"])?;
        let i = idx_of(p, id)?;
        let t = row_mut(p, i)?;
        if mode == ScheduleMode::Manual && t.mode == ScheduleMode::Auto {
            // Freeze the current dates as the manual dates.
            t.manual_start = t.manual_start.or(t.sched.start);
        }
        t.mode = mode;
    }
    if let Some(v) = a.get("task_type") {
        let tt: TaskType = parse_enum(v, "task_type", &["fixed_units", "fixed_duration", "fixed_work"])?;
        let i = idx_of(p, id)?;
        row_mut(p, i)?.task_type = tt;
    }
    if let Some(b) = a.bool("effort_driven")? {
        let i = idx_of(p, id)?;
        row_mut(p, i)?.effort_driven = b;
    }
    if a.present("estimate") {
        let est = match a.map().get("estimate") {
            Some(v) => parse_estimate(p, v)?,
            None => None,
        };
        let i = idx_of(p, id)?;
        row_mut(p, i)?.estimate = est;
    }
    if let Some(v) = a.get("duration") {
        let (m, mut est) = parse_duration_value(p, v, "duration")?;
        if let Some(e) = a.bool("estimated")? {
            est = e;
        }
        p.set_duration(id, m, est)?;
    } else if let Some(e) = a.bool("estimated")? {
        let i = idx_of(p, id)?;
        row_mut(p, i)?.estimated = e;
    }
    {
        let i = idx_of(p, id)?;
        if let Some(b) = a.bool("active")? {
            row_mut(p, i)?.active = b;
        }
        if let Some(pr) = a.u64("priority")? {
            if pr > 1000 {
                return err("'priority' must be between 0 and 1000");
            }
            row_mut(p, i)?.priority = pr as u16;
        }
        if let Some(n) = a.str("notes")? {
            row_mut(p, i)?.notes = n.to_string();
        }
        if let Some(c) = a.f64("fixed_cost")? {
            row_mut(p, i)?.fixed_cost = c;
        }
        if let Some(d) = a.str("deliverable")? {
            row_mut(p, i)?.deliverable = d.to_string();
        }
        if let Some(b) = a.bool("show_on_timeline")? {
            row_mut(p, i)?.show_on_timeline = b;
        }
        if let Some(b) = a.bool("ignore_resource_calendars")? {
            row_mut(p, i)?.ignore_resource_calendars = b;
        }
        if a.present("bar_color") {
            row_mut(p, i)?.bar_color = parse_color(a.map().get("bar_color").unwrap_or(&Value::Null))?;
        }
    }
    if a.is_clear("calendar") {
        let i = idx_of(p, id)?;
        row_mut(p, i)?.calendar = None;
    } else if let Some(v) = a.get("calendar") {
        let c = resolve_calendar(p, v)?;
        let i = idx_of(p, id)?;
        row_mut(p, i)?.calendar = Some(c);
    }

    // Constraints and dates.
    if let Some(v) = a.get("constraint_type") {
        let kind = parse_constraint_kind(v)?;
        let date = match a.get("constraint_date") {
            Some(d) if kind.needs_date() => Some(parse_dt(p, d, !kind.is_start_constraint(), "constraint_date")?),
            _ if kind.needs_date() => {
                let existing = p.task(id).and_then(|t| t.constraint.date);
                Some(existing.ok_or_else(|| ToolError(format!("constraint_type '{}' requires 'constraint_date'", constraint_code(kind))))?)
            }
            _ => None,
        };
        let i = idx_of(p, id)?;
        row_mut(p, i)?.constraint = Constraint { kind, date };
    } else if let Some(d) = a.get("constraint_date") {
        let kind = p.task(id).map(|t| t.constraint.kind).unwrap_or_default();
        if !kind.needs_date() {
            return err("set 'constraint_type' (e.g. \"snet\") together with 'constraint_date'");
        }
        let date = parse_dt(p, d, !kind.is_start_constraint(), "constraint_date")?;
        let i = idx_of(p, id)?;
        row_mut(p, i)?.constraint.date = Some(date);
    }
    let manual = p.task(id).is_some_and(|t| t.mode == ScheduleMode::Manual);
    if let Some(v) = a.get("start") {
        let d = parse_dt(p, v, false, "start")?;
        let i = idx_of(p, id)?;
        if manual {
            row_mut(p, i)?.manual_start = Some(d);
        } else {
            row_mut(p, i)?.constraint = Constraint { kind: ConstraintKind::Snet, date: Some(d) };
        }
    }
    if let Some(v) = a.get("finish") {
        let d = parse_dt(p, v, true, "finish")?;
        let i = idx_of(p, id)?;
        if manual {
            row_mut(p, i)?.manual_finish = Some(d);
        } else {
            row_mut(p, i)?.constraint = Constraint { kind: ConstraintKind::Fnet, date: Some(d) };
        }
    }
    for (key, finish) in
        [("deadline", true), ("manual_start", false), ("manual_finish", true), ("actual_start", false), ("actual_finish", true)]
    {
        if let Some(val) = opt_dt(p, a, key, finish)? {
            let i = idx_of(p, id)?;
            let t = row_mut(p, i)?;
            match key {
                "deadline" => t.deadline = val,
                "manual_start" => t.manual_start = val,
                "manual_finish" => t.manual_finish = val,
                "actual_start" => t.actual_start = val,
                _ => {
                    t.actual_finish = val;
                    if val.is_some() {
                        t.percent_complete = 100;
                        if t.actual_start.is_none() {
                            t.actual_start = t.sched.start;
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

/// Applies predecessors, resources and milestone members.
pub fn apply_task_links(p: &mut Project, id: TaskId, a: &Args, keys: Option<&TaskKeys>) -> ToolResult<()> {
    if a.present("predecessors") {
        let deps = parse_predecessor_list(p, a.map().get("predecessors").unwrap_or(&Value::Null), keys)
            .map_err(|e| ToolError(format!("predecessors: {e}")))?;
        p.set_predecessors(id, deps)?;
    }
    if a.present("resources") {
        let text = resources_text(p, a.map().get("resources").unwrap_or(&Value::Null))?;
        p.apply_resource_names(id, &text)?;
    }
    if let Some(members) = a.array("members")? {
        let mut refs = Vec::new();
        for m in members {
            refs.push(resolve_task_ref(p, m, keys)?);
        }
        if !p.task(id).is_some_and(|t| t.is_milestone()) {
            p.set_milestone(id, true)?;
        }
        p.add_milestone_members(id, &refs)?;
    }
    Ok(())
}

/// Applies % complete (needs an up-to-date schedule for actual dates).
pub fn apply_task_progress(p: &mut Project, id: TaskId, a: &Args) -> ToolResult<()> {
    if let Some(pct) = a.f64("percent_complete")? {
        if !(0.0..=100.0).contains(&pct) {
            return err("'percent_complete' must be between 0 and 100");
        }
        p.set_percent_complete(id, pct.round() as u8)?;
    }
    Ok(())
}

/// Applies every field of an update in the right order.
pub fn apply_all(p: &mut Project, id: TaskId, a: &Args) -> ToolResult<()> {
    apply_task_fields(p, id, a)?;
    apply_task_links(p, id, a, None)?;
    if a.get("percent_complete").is_some() {
        recalculate(p);
        apply_task_progress(p, id, a)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Creation
// ---------------------------------------------------------------------------

/// Where to insert new rows: index and the outline level of the first new row.
pub fn insert_position(p: &mut Project, a: &Args) -> ToolResult<(usize, Option<u16>)> {
    if let Some(v) = a.get("parent") {
        let pid = resolve_task(p, v).map_err(|e| ToolError(format!("'parent': {e}")))?;
        let pi = p.task_index(pid).ok_or_else(|| missing("resolved"))?;
        if row(p, pi)?.is_milestone() {
            if !row(p, pi)?.members.is_empty() {
                return err(format!("'{}' is a milestone with members and cannot become a summary task", row(p, pi)?.name));
            }
            row_mut(p, pi)?.kind = TaskKind::Task;
        }
        let end = p.descendants_range(pi).end;
        return Ok((end, Some(row(p, pi)?.outline_level + 1)));
    }
    if let Some(v) = a.get("after") {
        let aid = resolve_task(p, v).map_err(|e| ToolError(format!("'after': {e}")))?;
        let ai = p.task_index(aid).ok_or_else(|| missing("resolved"))?;
        return Ok((p.descendants_range(ai).end, Some(row(p, ai)?.outline_level)));
    }
    if let Some(v) = a.get("before") {
        let bid = resolve_task(p, v).map_err(|e| ToolError(format!("'before': {e}")))?;
        let bi = p.task_index(bid).ok_or_else(|| missing("resolved"))?;
        return Ok((bi, Some(row(p, bi)?.outline_level)));
    }
    if let Some(i) = a.u64("index")? {
        let i = (i.max(1) as usize - 1).min(p.tasks.len());
        return Ok((i, None));
    }
    Ok((p.tasks.len(), Some(1)))
}

/// Inserts a new task row.
pub fn insert_new_task(p: &mut Project, name: &str, index: usize, level: Option<u16>, milestone: bool) -> TaskId {
    let id = match level {
        None => p.add_task(name, Some(index)),
        Some(level) => {
            let id = p.ids.next_task();
            let mut t = Task::new(id, name, p.settings.time.minutes_per_day);
            t.outline_level = level;
            p.insert_task(index, t)
        }
    };
    if milestone && let Some(t) = p.task_mut(id) {
        t.kind = TaskKind::Milestone;
        t.duration = 0;
        t.estimated = false;
    }
    id
}

fn create_task(ctx: &Ctx, a: &Args) -> ToolResult {
    let name = a.req_str("name")?.trim().to_string();
    if name.is_empty() {
        return err("'name' must not be empty");
    }
    ctx.edit(
        a,
        &format!("Add task '{name}'"),
        |p, _| {
            let (index, level) = insert_position(p, a)?;
            let ms = a.bool("is_milestone")?.unwrap_or(false);
            let id = insert_new_task(p, &name, index, level, ms);
            apply_all(p, id, a)?;
            Ok(id)
        },
        |doc, id| Ok(views::task_detail(&doc.project, id)),
    )
}

struct FlatItem<'a> {
    args: Args<'a>,
    level: u16,
}

const ITEM_ONLY_KEYS: [&str; 5] = ["key", "level", "children", "milestone", "members"];

fn flatten<'a>(items: &'a [Value], base_level: u16, out: &mut Vec<FlatItem<'a>>, allowed: &HashSet<String>) -> ToolResult<()> {
    let mut prev_level = base_level.saturating_sub(1);
    for (n, item) in items.iter().enumerate() {
        let args = Args::from_value(item, "each task item")?;
        if let Some(bad) = args.map().keys().find(|k| !allowed.contains(k.as_str())) {
            return err(format!("task item {}: unknown field '{bad}'", n + 1));
        }
        let name = args.str("name")?.unwrap_or("").trim();
        if name.is_empty() {
            return err(format!("task item {} needs a non-empty 'name'", n + 1));
        }
        let level = match args.u64("level")? {
            Some(l) if base_level == 1 => {
                let l = l.max(1) as u16;
                if l > prev_level + 1 {
                    return err(format!(
                        "task item '{name}' has level {l} but the previous item has level {prev_level}; levels may only increase by one"
                    ));
                }
                l
            }
            _ => base_level,
        };
        prev_level = level;
        out.push(FlatItem { args, level });
        if let Some(children) = args.array("children")? {
            flatten(children, level + 1, out, allowed)?;
        }
    }
    Ok(())
}

fn create_tasks(ctx: &Ctx, a: &Args) -> ToolResult {
    let items = a.array("tasks")?.ok_or_else(|| ToolError("missing 'tasks'".into()))?;
    if items.is_empty() {
        return err("'tasks' must not be empty");
    }
    let mut allowed: HashSet<String> = task_field_props().keys().cloned().collect();
    allowed.extend(ITEM_ONLY_KEYS.iter().map(|s| s.to_string()));
    let mut flat = Vec::new();
    flatten(items, 1, &mut flat, &allowed)?;
    if flat.len() > 5000 {
        return err("too many tasks in one call (max 5000)");
    }
    ctx.edit(
        a,
        &format!("Add {} tasks", flat.len()),
        |p, _| {
            let (mut index, level) = insert_position(p, a)?;
            let base = match level {
                Some(l) => l,
                None => {
                    if index > 0 && index <= p.tasks.len() {
                        let above = index - 1;
                        if p.has_children(above) { row(p, above)?.outline_level + 1 } else { row(p, above)?.outline_level }
                    } else {
                        1
                    }
                }
            };
            let mut keys = TaskKeys::new();
            let mut created = Vec::new();
            for it in &flat {
                let name = it.args.str("name")?.unwrap_or("").trim().to_string();
                let ms = it.args.bool("is_milestone")?.or(it.args.bool("milestone")?).unwrap_or(false)
                    || it.args.array("members")?.is_some_and(|m| !m.is_empty());
                let id = insert_new_task(p, &name, index, Some(base + it.level - 1), ms);
                index += 1;
                if let Some(k) = it.args.text("key")?
                    && keys.insert(k.clone(), id).is_some()
                {
                    return err(format!("duplicate key '{k}' in tasks"));
                }
                created.push(id);
            }
            // Scalar fields after the outline exists (summary detection needs children).
            for (it, &id) in flat.iter().zip(&created) {
                let has_kids = p.task_index(id).is_some_and(|i| p.has_children(i));
                if has_kids && it.args.get("duration").is_some() {
                    return err(format!(
                        "'{}' has children, so its duration is calculated; remove 'duration' from it",
                        p.task(id).map(|t| t.name.clone()).unwrap_or_default()
                    ));
                }
                apply_task_fields(p, id, &it.args)
                    .map_err(|e| ToolError(format!("task '{}': {e}", p.task(id).map(|t| t.name.clone()).unwrap_or_default())))?;
            }
            for (it, &id) in flat.iter().zip(&created) {
                apply_task_links(p, id, &it.args, Some(&keys))
                    .map_err(|e| ToolError(format!("task '{}': {e}", p.task(id).map(|t| t.name.clone()).unwrap_or_default())))?;
            }
            if flat.iter().any(|it| it.args.get("percent_complete").is_some()) {
                recalculate(p);
                for (it, &id) in flat.iter().zip(&created) {
                    apply_task_progress(p, id, &it.args)?;
                }
            }
            let keyed: Vec<(TaskId, String)> = keys.iter().map(|(k, v)| (*v, k.clone())).collect();
            Ok((created, keyed))
        },
        |doc, (created, keyed)| {
            let p = &doc.project;
            let rows: Vec<Value> = created
                .iter()
                .filter_map(|id| {
                    let i = p.task_index(*id)?;
                    let t = p.tasks.get(i)?;
                    let mut v = json!({
                        "id": id.0, "row": i + 1, "wbs": p.wbs(i), "name": t.name, "level": t.outline_level,
                        "duration": p.format_duration(t.sched.duration, t.estimated && !t.sched.is_summary),
                        "start": fmt_dt(t.sched.start), "finish": fmt_dt(t.sched.finish),
                    });
                    if let Some((_, k)) = keyed.iter().find(|(tid, _)| tid == id) {
                        set(&mut v, "key", json!(k));
                    }
                    Some(v)
                })
                .collect();
            Ok(json!({
                "created": rows.len(),
                "tasks": rows,
                "project_finish": fmt_dt(p.sched.finish),
            }))
        },
    )
}

fn update_task(ctx: &Ctx, a: &Args) -> ToolResult {
    ctx.edit(
        a,
        "Update task",
        |p, _| {
            let id = task_arg(p, a, "task")?;
            apply_all(p, id, a)?;
            Ok(id)
        },
        |doc, id| Ok(views::task_detail(&doc.project, id)),
    )
}

fn update_tasks(ctx: &Ctx, a: &Args) -> ToolResult {
    let updates = a.array("updates")?.ok_or_else(|| ToolError("missing 'updates'".into()))?;
    let mut allowed: HashSet<String> = task_field_props().keys().cloned().collect();
    allowed.insert("task".into());
    let mut parsed = Vec::new();
    for (n, u) in updates.iter().enumerate() {
        let ua = Args::from_value(u, "each update")?;
        if let Some(bad) = ua.map().keys().find(|k| !allowed.contains(k.as_str())) {
            return err(format!("update {}: unknown field '{bad}'", n + 1));
        }
        parsed.push(ua);
    }
    ctx.edit(
        a,
        &format!("Update {} tasks", parsed.len()),
        |p, _| {
            let mut ids = Vec::new();
            for (n, ua) in parsed.iter().enumerate() {
                let id = task_arg(p, ua, "task").map_err(|e| ToolError(format!("update {}: {e}", n + 1)))?;
                apply_task_fields(p, id, ua).map_err(|e| ToolError(format!("update {} (task {}): {e}", n + 1, id.0)))?;
                apply_task_links(p, id, ua, None).map_err(|e| ToolError(format!("update {} (task {}): {e}", n + 1, id.0)))?;
                ids.push(id);
            }
            if parsed.iter().any(|ua| ua.get("percent_complete").is_some()) {
                recalculate(p);
                for (ua, id) in parsed.iter().zip(&ids) {
                    apply_task_progress(p, *id, ua)?;
                }
            }
            Ok(ids)
        },
        |doc, ids| {
            let p = &doc.project;
            Ok(json!({
                "updated": ids.len(),
                "tasks": ids.iter().filter_map(|id| p.task_index(*id)).map(|i| views::task_row(p, i)).collect::<Vec<_>>(),
            }))
        },
    )
}

fn delete_tasks(ctx: &Ctx, a: &Args) -> ToolResult {
    ctx.edit(
        a,
        "Delete tasks",
        |p, _| {
            let ids = task_list_arg(p, a, "tasks")?;
            if ids.is_empty() {
                return err("'tasks' must not be empty");
            }
            let names: Vec<(TaskId, String)> = p.tasks.iter().map(|t| (t.id, t.name.clone())).collect();
            let mut deleted = p.delete_tasks(&ids)?;
            deleted.sort();
            Ok(deleted
                .into_iter()
                .map(|d| json!({ "id": d.0, "name": names.iter().find(|(i, _)| *i == d).map(|(_, n)| n.clone()) }))
                .collect::<Vec<_>>())
        },
        |doc, deleted| Ok(json!({ "deleted": deleted, "remaining_tasks": doc.project.tasks.len() })),
    )
}

fn move_task(ctx: &Ctx, a: &Args) -> ToolResult {
    ctx.edit(
        a,
        "Move task",
        |p, _| {
            let id = task_arg(p, a, "task")?;
            let idx = p.task_index(id).ok_or_else(|| missing("resolved"))?;
            let block_len = p.descendants_range(idx).len() + 1;
            let in_block = |p: &Project, other: TaskId| p.task_index(other).is_some_and(|o| o >= idx && o < idx + block_len);
            let (target, level) = if let Some(v) = a.get("before") {
                let o = resolve_task(p, v)?;
                if in_block(p, o) {
                    return err("cannot move a task relative to itself or its own subtasks");
                }
                let oi = p.task_index(o).ok_or_else(|| missing("resolved"))?;
                (oi, Some(row(p, oi)?.outline_level))
            } else if let Some(v) = a.get("after") {
                let o = resolve_task(p, v)?;
                if in_block(p, o) {
                    return err("cannot move a task relative to itself or its own subtasks");
                }
                let oi = p.task_index(o).ok_or_else(|| missing("resolved"))?;
                (p.descendants_range(oi).end, Some(row(p, oi)?.outline_level))
            } else if let Some(v) = a.get("parent") {
                let o = resolve_task(p, v)?;
                if in_block(p, o) {
                    return err("cannot move a task under itself or its own subtasks");
                }
                let oi = p.task_index(o).ok_or_else(|| missing("resolved"))?;
                if row(p, oi)?.is_milestone() {
                    if !row(p, oi)?.members.is_empty() {
                        return err("a milestone with members cannot become a summary task");
                    }
                    row_mut(p, oi)?.kind = TaskKind::Task;
                }
                (p.descendants_range(oi).end, Some(row(p, oi)?.outline_level + 1))
            } else if let Some(i) = a.u64("index")? {
                ((i.max(1) as usize) - 1, None)
            } else {
                return err("give one of 'before', 'after', 'parent' or 'index'");
            };
            p.move_task(id, target)?;
            if let Some(level) = level {
                let ni = p.task_index(id).ok_or_else(|| missing("moved"))?;
                let delta = i32::from(level) - i32::from(row(p, ni)?.outline_level);
                for t in p.tasks.iter_mut().skip(ni).take(block_len) {
                    t.outline_level =
                        u16::try_from((i32::from(t.outline_level) + delta).clamp(1, i32::from(plancraft_core::limits::MAX_OUTLINE_LEVEL)))
                            .unwrap_or(1);
                }
                p.normalize_outline();
            }
            Ok(id)
        },
        |doc, id| Ok(views::task_detail(&doc.project, id)),
    )
}

/// Drops tasks whose ancestor is also selected (they move with it), sorted by row.
fn top_level_selection(p: &Project, ids: &[TaskId]) -> Vec<TaskId> {
    let set: HashSet<TaskId> = ids.iter().copied().collect();
    let mut out: Vec<TaskId> = ids
        .iter()
        .copied()
        .filter(|&id| {
            let mut cur = p.task_index(id).and_then(|i| p.parent_index(i));
            while let Some(c) = cur {
                if p.tasks.get(c).is_some_and(|t| set.contains(&t.id)) {
                    return false;
                }
                cur = p.parent_index(c);
            }
            true
        })
        .collect();
    out.sort_by_key(|id| p.task_index(*id));
    out
}

fn indent_tasks(ctx: &Ctx, a: &Args) -> ToolResult {
    ctx.edit(
        a,
        "Indent tasks",
        |p, _| {
            let ids = task_list_arg(p, a, "tasks")?;
            let ids = top_level_selection(p, &ids);
            for id in &ids {
                p.indent(*id).map_err(|e| ToolError(format!("task {}: {e}", id.0)))?;
            }
            Ok(ids)
        },
        |doc, ids| Ok(json!({ "tasks": ids.iter().filter_map(|id| doc.project.task_index(*id)).map(|i| views::task_row(&doc.project, i)).collect::<Vec<_>>() })),
    )
}

fn outdent_tasks(ctx: &Ctx, a: &Args) -> ToolResult {
    ctx.edit(
        a,
        "Outdent tasks",
        |p, _| {
            let ids = task_list_arg(p, a, "tasks")?;
            let mut ids = top_level_selection(p, &ids);
            // Outdent bottom-up so earlier promotions do not capture later siblings.
            ids.reverse();
            for id in &ids {
                p.outdent(*id).map_err(|e| ToolError(format!("task {}: {e}", id.0)))?;
            }
            ids.reverse();
            Ok(ids)
        },
        |doc, ids| Ok(json!({ "tasks": ids.iter().filter_map(|id| doc.project.task_index(*id)).map(|i| views::task_row(&doc.project, i)).collect::<Vec<_>>() })),
    )
}

/// Accepts resources as text (`"Ana[50%], Bob"`) or as an array of names, ids or
/// `{resource, units}` objects, and returns the text form understood by the core.
fn resources_text(p: &Project, v: &Value) -> ToolResult<String> {
    let item = |x: &Value| -> ToolResult<String> {
        match x {
            Value::String(s) => Ok(s.clone()),
            Value::Number(_) => {
                let rid = resolve_resource(p, x)?;
                Ok(p.resource(rid).map(|r| r.name.clone()).unwrap_or_default())
            }
            Value::Object(o) => {
                let r = o.get("resource").or_else(|| o.get("name")).ok_or_else(|| ToolError("resource objects need 'resource'".into()))?;
                let rid = resolve_resource(p, r)?;
                let name = p.resource(rid).map(|r| r.name.clone()).unwrap_or_default();
                match o.get("units") {
                    Some(u) => Ok(format!("{name}[{}]", pct_text(parse_units_value(u, "units")?))),
                    None => Ok(name),
                }
            }
            other => err(format!("invalid resource entry {other}")),
        }
    };
    match v {
        Value::Null => Ok(String::new()),
        Value::Array(items) => Ok(items.iter().map(item).collect::<ToolResult<Vec<_>>>()?.join(",")),
        other => item(other),
    }
}
