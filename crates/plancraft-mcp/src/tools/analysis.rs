//! Schedule analysis tools: schedule table, critical path, workload, leveling,
//! statistics, baselines and variance.

use std::collections::{BTreeMap, HashSet};

use chrono::NaiveDate;
use plancraft_core::baseline::{MAX_BASELINE, MAX_INTERIM, PlanSlot, Scope};
use plancraft_core::duration::format_work;
use plancraft_core::ids::{ResourceId, TaskId};
use plancraft_core::leveling::{LevelingOptions, clear_leveling, level_resources};
use plancraft_core::model::*;
use plancraft_core::report;
use plancraft_core::schedule::project_calendar;
use plancraft_core::workload::{self, Granularity};
use serde_json::{Value, json};

use super::*;
use crate::args::*;
use crate::views;

pub fn tools() -> Vec<Tool> {
    vec![
        Tool::new(
            "get_schedule",
            "Get the schedule (Gantt table)",
            "Compact Gantt-like table of the whole outline: row, id, WBS, indented name, duration, start, finish, % complete, predecessors (row syntax), resources and flags (summary/milestone/critical/manual/warning). Best first call to understand a project.",
            schema(
                json!({
                    "max_level": integer("Only show tasks up to this outline level."),
                    "hide_completed": boolean("Hide completed leaf tasks."),
                }),
                &[],
            ),
            get_schedule,
        )
        .read_only()
        .text("table"),
        Tool::new(
            "critical_path",
            "Critical path",
            "List the critical tasks (zero or critical-threshold total slack) in start order, plus optional near-critical tasks, with the project finish date.",
            schema(json!({ "near_critical_slack": duration("Also list non-critical tasks with total slack at or below this.") }), &[]),
            critical_path,
        )
        .read_only(),
        Tool::new(
            "get_workload",
            "Resource workload",
            "Time-phased workload per resource: planned load vs capacity (hours) and utilization per day/week/month, overallocated periods and days, total/actual work, complexity points, and estimated work of unassigned tasks. Defaults to the project span by week.",
            schema(
                json!({
                    "from": date("First day (default: project start)."),
                    "to": date("Last day (default: project finish)."),
                    "granularity": string_enum("Period size (default week).", &["day", "week", "month"]),
                    "resources": { "type": "array", "items": { "type": ["integer", "string"] }, "description": "Only these resources (ids or names)." },
                    "include_assignments": boolean("Include per-assignment load per period (default false)."),
                }),
                &[],
            ),
            get_workload,
        )
        .read_only(),
        Tool::new(
            "get_overallocations",
            "Overallocations",
            "Days on which resources have more work than capacity, with load, capacity and the tasks causing it.",
            schema(json!({ "resource": resource_ref("Only this resource."), "max_days": integer("Maximum days listed per resource (default 60).") }), &[]),
            get_overallocations,
        )
        .read_only(),
        Tool::new(
            "level_resources",
            "Level resources",
            "Resolve overallocations by delaying tasks (leveling delay), visiting tasks by start, priority and slack. Started, manual and must-start/finish tasks are not moved. Undoable.",
            schema(
                json!({
                    "clear_existing": boolean("Remove existing leveling delays first (default true)."),
                    "max_delay_days": integer("Maximum delay tried per task in working days (default 260)."),
                    "resources": { "type": "array", "items": { "type": ["integer", "string"] }, "description": "Only level these resources (default all)." },
                }),
                &[],
            ),
            level_resources_tool,
        ),
        Tool::new("clear_leveling", "Clear leveling", "Remove all leveling delays.", schema(json!({}), &[]), clear_leveling_tool),
        Tool::new(
            "project_statistics",
            "Project statistics",
            "Project statistics: dates, duration, work, cost, % complete, task counts (complete, in progress, late, critical), overallocated resources, baseline variance, milestone status and earned value (SPI/CPI, needs a baseline).",
            schema(json!({}), &[]),
            project_statistics,
        )
        .read_only(),
        Tool::new(
            "set_baseline",
            "Set baseline",
            "Save the current start, finish, duration, work and cost into a baseline (Baseline, or Baseline1-10) for all tasks or the given tasks. Variance is measured against Baseline; the numbered ones keep earlier snapshots (e.g. one per phase). Saving for selected tasks can roll the new values up into their summary tasks. Overwrites what the baseline held for those tasks.",
            schema(
                json!({
                    "baseline": baseline_ref("Which baseline: 0 or \"baseline\" (default), 1-10 or \"baseline1\"..\"baseline10\"."),
                    "tasks": task_refs("Only these tasks (default all)."),
                    "roll_up_to_summaries": boolean("With tasks: roll their new baseline values up into all their summary tasks (default false)."),
                    "roll_up_from_subtasks": boolean("With tasks: recompute each given summary task's baseline from its subtasks' baselines (default false)."),
                }),
                &[],
            ),
            set_baseline,
        ),
        Tool::new(
            "set_interim_plan",
            "Set interim plan",
            "Copy plan values between the current schedule, baselines and interim plans (Start1/Finish1 .. Start10/Finish10). Copying into an interim plan copies start and finish only; copying a baseline into another baseline copies all its values; tasks with no value in the source lose theirs in the target. An interim plan can't be copied into a baseline.",
            schema(
                json!({
                    "copy": string("Source: \"scheduled\" (current start/finish), \"baseline\", \"baseline1\"..\"baseline10\" or \"interim1\"..\"interim10\"."),
                    "into": string("Target: \"interim1\"..\"interim10\", \"baseline\" or \"baseline1\"..\"baseline10\"."),
                    "tasks": task_refs("Only these tasks (default all)."),
                }),
                &["copy", "into"],
            ),
            set_interim_plan,
        )
        .destructive(),
        Tool::new(
            "clear_baseline",
            "Clear baseline",
            "Remove a baseline (default Baseline) or an interim plan from all tasks (or the given tasks).",
            schema(
                json!({
                    "baseline": baseline_ref("Which baseline to clear: 0-10 or \"baseline\"..\"baseline10\" (default Baseline)."),
                    "interim_plan": { "type": ["integer", "string"], "description": "Clear this interim plan instead of a baseline: 1-10 or \"interim1\"..\"interim10\"." },
                    "tasks": task_refs("Only these tasks (default all)."),
                }),
                &[],
            ),
            clear_baseline,
        )
        .destructive(),
        Tool::new(
            "list_baselines",
            "List baselines",
            "Every baseline (Baseline, Baseline1-10) and the interim plans that hold values: per slot the number of tasks with a value, when it was last saved, its start and finish, and its total work and cost. Also says which baseline variance and earned value use.",
            schema(json!({}), &[]),
            list_baselines,
        )
        .read_only(),
        Tool::new(
            "get_variance",
            "Baseline variance",
            "Compare current schedule with a baseline per task (default Baseline): start/finish variance (working time), duration, work and cost variance.",
            schema(
                json!({
                    "baseline": baseline_ref("Compare against this baseline (default Baseline, the one variance fields use)."),
                    "tasks": task_refs("Only these tasks."),
                    "only_slipped": boolean("Only tasks finishing later than baseline."),
                }),
                &[],
            ),
            get_variance,
        )
        .read_only(),
    ]
}

fn get_schedule(ctx: &Ctx, a: &Args) -> ToolResult {
    ctx.read(a, |doc| {
        let p = &doc.project;
        let max_level = a.u64("max_level")?;
        let hide_completed = a.bool("hide_completed")?.unwrap_or(false);
        let rows: Vec<usize> = p
            .tasks
            .iter()
            .enumerate()
            .filter(|(_, t)| {
                max_level.is_none_or(|m| t.outline_level as u64 <= m) && !(hide_completed && !t.sched.is_summary && t.is_complete())
            })
            .map(|(i, _)| i)
            .collect();
        let table = views::schedule_table(p, &rows);
        let tasks: Vec<Value> = rows
            .iter()
            .filter_map(|&i| Some((i, p.tasks.get(i)?)))
            .map(|(i, t)| {
                json!({
                    "row": i + 1, "id": t.id.0, "wbs": p.wbs(i), "name": t.name, "level": t.outline_level,
                    "duration": p.format_duration(t.sched.duration, t.estimated && !t.sched.is_summary),
                    "start": fmt_dt(t.sched.start), "finish": fmt_dt(t.sched.finish),
                    "percent_complete": round2(t.sched.percent_complete as f64),
                    "predecessors": p.format_predecessors(t.id), "resources": p.format_resource_names(t.id),
                    "summary": t.sched.is_summary, "milestone": t.is_milestone(), "critical": t.sched.critical,
                })
            })
            .collect();
        Ok(json!({
            "project": p.name,
            "start": fmt_dt(p.sched.start),
            "finish": fmt_dt(p.sched.finish),
            "table": table,
            "tasks": tasks,
            "warnings": p.sched.warnings,
        }))
    })
}

fn critical_path(ctx: &Ctx, a: &Args) -> ToolResult {
    ctx.read(a, |doc| {
        let p = &doc.project;
        let near = a.get("near_critical_slack").map(|v| parse_duration_value(p, v, "near_critical_slack")).transpose()?.map(|x| x.0);
        let start_of = |i: usize| p.tasks.get(i).and_then(|t| t.sched.start);
        let mut crit: Vec<usize> =
            p.tasks.iter().enumerate().filter(|(_, t)| t.sched.critical && !t.sched.is_summary && t.active).map(|(i, _)| i).collect();
        crit.sort_by_key(|&i| (start_of(i), i));
        let mut near_list: Vec<usize> = match near {
            Some(n) => p
                .tasks
                .iter()
                .enumerate()
                .filter(|(_, t)| !t.sched.critical && !t.sched.is_summary && t.active && t.sched.total_slack <= n)
                .map(|(i, _)| i)
                .collect(),
            None => Vec::new(),
        };
        near_list.sort_by_key(|&i| (start_of(i), i));
        let row = |i: &usize| {
            let Some(t) = p.tasks.get(*i) else { return Value::Null };
            json!({
                "id": t.id.0, "row": i + 1, "name": t.name,
                "duration": p.format_duration(t.sched.duration, t.estimated),
                "start": fmt_dt(t.sched.start), "finish": fmt_dt(t.sched.finish),
                "total_slack": views::dur(p, t.sched.total_slack), "percent_complete": round2(t.sched.percent_complete as f64),
                "resources": p.format_resource_names(t.id),
            })
        };
        Ok(json!({
            "project_finish": fmt_dt(p.sched.finish),
            "critical_count": crit.len(),
            "critical_tasks": crit.iter().map(row).collect::<Vec<_>>(),
            "near_critical_tasks": near_list.iter().map(row).collect::<Vec<_>>(),
        }))
    })
}

fn default_window(p: &Project, a: &Args) -> ToolResult<(NaiveDate, NaiveDate)> {
    let from = match a.get("from") {
        Some(v) => parse_date(v, "from")?,
        None => p.sched.start.unwrap_or(p.start).date(),
    };
    let to = match a.get("to") {
        Some(v) => parse_date(v, "to")?,
        None => p.sched.finish.map(|f| f.date()).unwrap_or(from + chrono::Duration::days(27)).max(from),
    };
    if to < from {
        return err("'to' is before 'from'");
    }
    if (to - from).num_days() > 3660 {
        return err("date range too long (max 10 years)");
    }
    Ok((from, to))
}

fn resource_filter(p: &Project, a: &Args) -> ToolResult<HashSet<ResourceId>> {
    let mut set = HashSet::new();
    for v in a.list("resources") {
        set.insert(resolve_resource(p, v)?);
    }
    Ok(set)
}

fn get_workload(ctx: &Ctx, a: &Args) -> ToolResult {
    ctx.read(a, |doc| {
        let p = &doc.project;
        let (from, to) = default_window(p, a)?;
        let granularity: Granularity = match a.get("granularity") {
            Some(v) => parse_enum(v, "granularity", &["day", "week", "month"])?,
            None => Granularity::Week,
        };
        let periods_count = granularity.buckets(from, to).len();
        if periods_count > 800 {
            return err(format!("{periods_count} periods requested; use a coarser granularity or a shorter range (max 800)"));
        }
        let filter = resource_filter(p, a)?;
        let include_asg = a.bool("include_assignments")?.unwrap_or(false);
        let rep = workload::workload(p, from, to, granularity);
        let resources: Vec<Value> = rep
            .resources
            .iter()
            .filter(|rl| if filter.is_empty() { p.resource(rl.resource).is_some_and(|r| r.kind == ResourceType::Work) } else { filter.contains(&rl.resource) })
            .map(|rl| {
                let over_periods: Vec<String> = rl.periods.iter().filter(|x| x.overallocated).map(|x| x.start.format(DATE_FMT).to_string()).collect();
                let mut v = json!({
                    "id": rl.resource.0,
                    "name": rl.name,
                    "total_work": format_work(rl.total_work),
                    "actual_work": format_work(rl.actual_work),
                    "complexity_points": round2(rl.complexity_points),
                    "window_load_hours": hours(rl.window_load),
                    "window_capacity_hours": hours(rl.window_capacity),
                    "utilization_pct": round2(rl.utilization() * 100.0),
                    "overallocated": rl.overallocated,
                    "overallocated_day_count": rl.overallocated_days.len(),
                    "overallocated_days": rl.overallocated_days.iter().take(31).map(|d| d.format(DATE_FMT).to_string()).collect::<Vec<_>>(),
                    "overallocated_periods": over_periods,
                    "load_hours": rl.periods.iter().map(|x| hours(x.load)).collect::<Vec<_>>(),
                    "capacity_hours": rl.periods.iter().map(|x| hours(x.capacity)).collect::<Vec<_>>(),
                    "utilization_pct_by_period": rl.periods.iter().map(|x| { let u = x.utilization(); if u.is_finite() { round2(u * 100.0) } else { 9999.0 } }).collect::<Vec<_>>(),
                });
                if include_asg {
                    set(
                        &mut v,
                        "assignments",
                        Value::Array(
                            rl.assignments.iter().map(|al| json!({ "assignment": al.assignment.0, "task_id": al.task.0, "task": al.task_name, "load_hours": al.load.iter().map(|x| hours(*x)).collect::<Vec<_>>() })).collect(),
                        ),
                    );
                }
                v
            })
            .collect();
        let unassigned_total: f64 = rep.unassigned.iter().sum();
        Ok(json!({
            "from": from.format(DATE_FMT).to_string(),
            "to": to.format(DATE_FMT).to_string(),
            "granularity": enum_str(&granularity),
            "periods": rep.periods.iter().map(|d| d.format(DATE_FMT).to_string()).collect::<Vec<_>>(),
            "resources": resources,
            "unassigned_hours": rep.unassigned.iter().map(|x| hours(*x)).collect::<Vec<_>>(),
            "unassigned_total_hours": hours(unassigned_total),
            "note": "load/capacity in hours per period, aligned with 'periods' (period start dates).",
        }))
    })
}

fn get_overallocations(ctx: &Ctx, a: &Args) -> ToolResult {
    ctx.read(a, |doc| {
        let p = &doc.project;
        let only = a.get("resource").map(|v| resolve_resource(p, v)).transpose()?;
        let max_days = a.u64("max_days")?.unwrap_or(60) as usize;
        let mut out = Vec::new();
        for (rid, days) in workload::overallocations(p) {
            if only.is_some_and(|o| o != rid) {
                continue;
            }
            let r = p.resource(rid).ok_or_else(|| missing("exists"))?;
            let cal = workload::resource_calendar(p, r);
            let mut per_day: BTreeMap<NaiveDate, Vec<(TaskId, f64)>> = BTreeMap::new();
            let wanted: HashSet<NaiveDate> = days.iter().copied().collect();
            for asg in p.assignments_for_resource(rid) {
                if p.task(asg.task).is_none_or(|t| !t.active) {
                    continue;
                }
                for (d, w) in workload::assignment_daily_work(p, asg) {
                    if wanted.contains(&d) {
                        per_day.entry(d).or_default().push((asg.task, w));
                    }
                }
            }
            let list: Vec<Value> = per_day
                .iter()
                .take(max_days)
                .map(|(d, tasks)| {
                    let load: f64 = tasks.iter().map(|(_, w)| w).sum();
                    json!({
                        "date": d.format(DATE_FMT).to_string(),
                        "load_hours": hours(load),
                        "capacity_hours": hours(workload::capacity_on(&cal, r, *d)),
                        "tasks": tasks.iter().map(|(t, w)| json!({ "id": t.0, "name": p.task(*t).map(|x| x.name.clone()), "hours": hours(*w) })).collect::<Vec<_>>(),
                    })
                })
                .collect();
            out.push(json!({ "resource_id": rid.0, "resource": r.name, "max_units": pct_text(r.max_units), "overallocated_days": days.len(), "first_day": fmt_date(days.first().copied()), "last_day": fmt_date(days.last().copied()), "days": list }));
        }
        Ok(json!({
            "overallocated_resources": out.len(),
            "resources": out,
            "hint": if out.is_empty() { "No overallocations." } else { "Use level_resources, change assignment units, or reassign work to resolve." },
        }))
    })
}

fn level_resources_tool(ctx: &Ctx, a: &Args) -> ToolResult {
    ctx.edit(
        a,
        "Level resources",
        |p, _| {
            let mut opts = LevelingOptions::default();
            if let Some(b) = a.bool("clear_existing")? {
                opts.clear_existing = b;
            }
            if let Some(d) = a.u64("max_delay_days")? {
                opts.max_delay_days = d.min(5000) as u32;
            }
            opts.resources = resource_filter(p, a)?.into_iter().collect();
            Ok(level_resources(p, &opts))
        },
        |doc, rep| {
            let p = &doc.project;
            let delayed: Vec<Value> = rep
                .delayed
                .iter()
                .filter_map(|(id, m)| {
                    let t = p.task(*id)?;
                    Some(json!({ "id": id.0, "name": t.name, "delay": views::dur(p, *m), "delay_minutes": m, "start": fmt_dt(t.sched.start), "finish": fmt_dt(t.sched.finish) }))
                })
                .collect();
            let remaining = workload::overallocations(p);
            Ok(json!({
                "delayed_tasks": delayed,
                "unresolved": rep.unresolved,
                "finish_before": fmt_dt(rep.finish_before),
                "finish_after": fmt_dt(rep.finish_after),
                "remaining_overallocated_resources": remaining.iter().map(|(r, d)| json!({ "id": r.0, "name": p.resource(*r).map(|x| x.name.clone()), "days": d.len() })).collect::<Vec<_>>(),
            }))
        },
    )
}

fn clear_leveling_tool(ctx: &Ctx, a: &Args) -> ToolResult {
    ctx.edit(
        a,
        "Clear leveling",
        |p, _| {
            let n = p.tasks.iter().filter(|t| t.leveling_delay != 0).count();
            clear_leveling(p);
            Ok(n)
        },
        |doc, n| Ok(json!({ "cleared_tasks": n, "project_finish": fmt_dt(doc.project.sched.finish) })),
    )
}

fn project_statistics(ctx: &Ctx, a: &Args) -> ToolResult {
    ctx.read(a, |doc| {
        let p = &doc.project;
        let st = report::project_stats(p);
        let mut v = views::stats_json(p, &st);
        set(&mut v, "name", json!(p.name));
        set(&mut v, "status_date", fmt_dt(Some(report::status_moment(p))));
        set(&mut v, "milestones_status", Value::Array(st.milestone_status.iter().map(|m| views::milestone_status_json(p, m)).collect()));
        set(&mut v, "earned_value", views::earned_value_json(&st.earned_value));
        Ok(v)
    })
}

fn baseline_ref(desc: &str) -> Value {
    json!({ "type": ["integer", "string"], "description": desc })
}

/// The baseline an argument names (`default` when absent).
fn baseline_arg(a: &Args, key: &str, default: u8) -> ToolResult<u8> {
    let slot = match a.get(key) {
        None | Some(Value::Null) => return Ok(default),
        Some(Value::Number(_)) => match a.u64(key)? {
            Some(n) if n <= u64::from(MAX_BASELINE) => PlanSlot::Baseline(n as u8),
            _ => return err(format!("'{key}' must be a baseline number from 0 to 10")),
        },
        Some(Value::String(s)) => match s.trim().parse::<u8>() {
            Ok(n) if n <= MAX_BASELINE => PlanSlot::Baseline(n),
            _ => PlanSlot::parse(s)
                .ok_or_else(|| ToolError(format!("'{key}': unknown baseline '{s}' (use baseline, baseline1 .. baseline10)")))?,
        },
        Some(other) => return err(format!("'{key}' must be a baseline number or name, got {other}")),
    };
    match slot {
        PlanSlot::Baseline(n) => Ok(n),
        _ => err(format!("'{key}' must name a baseline (baseline, baseline1 .. baseline10)")),
    }
}

/// The interim plan an argument names, if given (null counts as not given).
fn interim_arg(a: &Args, key: &str) -> ToolResult<Option<u8>> {
    let n = match a.get(key) {
        None | Some(Value::Null) => return Ok(None),
        Some(Value::Number(_)) => a.u64(key)?.and_then(|n| u8::try_from(n).ok()),
        Some(Value::String(s)) => match s.trim().parse::<u8>() {
            Ok(n) => Some(n),
            Err(_) => match PlanSlot::parse(s) {
                Some(PlanSlot::Interim(n)) => Some(n),
                _ => None,
            },
        },
        Some(_) => None,
    };
    match n {
        Some(n) if (1..=MAX_INTERIM).contains(&n) => Ok(Some(n)),
        _ => err(format!("'{key}' must be an interim plan from 1 to 10 (or \"interim1\"..\"interim10\")")),
    }
}

/// The tasks a baseline tool works on: all when `tasks` is left out. An empty list is refused,
/// so a filter that matched nothing never turns into "the entire project".
fn scope_tasks(p: &Project, a: &Args) -> ToolResult<Vec<TaskId>> {
    if matches!(a.get("tasks"), Some(Value::Array(v)) if v.is_empty()) {
        return err("'tasks' is empty; leave it out to cover the entire project");
    }
    task_list_arg(p, a, "tasks")
}

pub fn baseline_number(a: &Args, key: &str) -> ToolResult<Option<u8>> {
    match a.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(_) => baseline_arg(a, key, 0).map(Some),
    }
}

fn slot_arg(a: &Args, key: &str) -> ToolResult<PlanSlot> {
    let s = a.req_str(key)?;
    PlanSlot::parse(s).ok_or_else(|| {
        ToolError(format!("'{key}': unknown plan '{s}' (use scheduled, baseline, baseline1 .. baseline10, interim1 .. interim10)"))
    })
}

fn plan_info_json(p: &Project, slot: PlanSlot) -> Value {
    let info = p.plan_info(slot);
    let mut v = json!({
        "name": slot.name(),
        "key": slot.key(),
        "tasks": info.tasks,
        "start": fmt_dt(info.start),
        "finish": fmt_dt(info.finish),
    });
    if let PlanSlot::Baseline(_) = slot {
        // Kept from the single-baseline tools.
        set(&mut v, "tasks_with_baseline", json!(info.tasks));
        set(&mut v, "saved", fmt_dt(info.saved));
        set(&mut v, "work", json!(format_work(info.work)));
        set(&mut v, "cost", json!(round2(info.cost)));
    }
    v
}

fn set_baseline(ctx: &Ctx, a: &Args) -> ToolResult {
    ctx.edit(
        a,
        "Set baseline",
        |p, _| {
            let n = baseline_arg(a, "baseline", 0)?;
            let scope = Scope {
                tasks: scope_tasks(p, a)?,
                roll_up_to_summaries: a.bool("roll_up_to_summaries")?.unwrap_or(false),
                roll_up_from_subtasks: a.bool("roll_up_from_subtasks")?.unwrap_or(false),
            };
            let written = p.set_baseline_in(n, &scope)?;
            Ok((n, written))
        },
        |doc, (n, written)| {
            let mut v = plan_info_json(&doc.project, PlanSlot::Baseline(n));
            set(&mut v, "tasks_written", json!(written));
            Ok(v)
        },
    )
}

fn set_interim_plan(ctx: &Ctx, a: &Args) -> ToolResult {
    ctx.edit(
        a,
        "Set interim plan",
        |p, _| {
            let (copy, into) = (slot_arg(a, "copy")?, slot_arg(a, "into")?);
            let scope = Scope::selected(scope_tasks(p, a)?);
            let written = p.save_plan(copy, into, &scope)?;
            Ok((copy, into, written))
        },
        |doc, (copy, into, written)| {
            let mut v = plan_info_json(&doc.project, into);
            set(&mut v, "copied_from", json!(copy.name()));
            set(&mut v, "tasks_written", json!(written));
            Ok(v)
        },
    )
}

fn clear_baseline(ctx: &Ctx, a: &Args) -> ToolResult {
    ctx.edit(
        a,
        "Clear baseline",
        |p, _| {
            let slot = match (interim_arg(a, "interim_plan")?, baseline_number(a, "baseline")?) {
                (Some(_), Some(_)) => return err("give 'baseline' or 'interim_plan', not both"),
                (Some(n), None) => PlanSlot::Interim(n),
                (None, n) => PlanSlot::Baseline(n.unwrap_or(0)),
            };
            let ids = scope_tasks(p, a)?;
            let cleared = p.clear_plan(slot, &ids)?;
            Ok((slot, cleared))
        },
        |doc, (slot, cleared)| {
            let mut v = plan_info_json(&doc.project, slot);
            set(&mut v, "tasks_cleared", json!(cleared));
            Ok(v)
        },
    )
}

fn list_baselines(ctx: &Ctx, a: &Args) -> ToolResult {
    ctx.read(a, |doc| {
        let p = &doc.project;
        let baselines: Vec<Value> = PlanSlot::baselines().map(|s| plan_info_json(p, s)).collect();
        let interim: Vec<Value> =
            PlanSlot::interim_plans().filter(|&s| p.tasks.iter().any(|t| t.dates_in(s).is_some())).map(|s| plan_info_json(p, s)).collect();
        Ok(json!({
            "baselines": baselines,
            "interim_plans": interim,
            "variance_baseline": PlanSlot::Baseline(0).name(),
            "earned_value_baseline": PlanSlot::Baseline(p.settings.earned_value_baseline).name(),
        }))
    })
}

fn get_variance(ctx: &Ctx, a: &Args) -> ToolResult {
    ctx.read(a, |doc| {
        let p = &doc.project;
        let ids = task_list_arg(p, a, "tasks")?;
        let n = baseline_arg(a, "baseline", 0)?;
        let only_slipped = a.bool("only_slipped")?.unwrap_or(false);
        let wc = project_calendar(p);
        let mut rows = Vec::new();
        let mut without = 0;
        for (i, t) in p.tasks.iter().enumerate() {
            if !ids.is_empty() && !ids.contains(&t.id) {
                continue;
            }
            let Some(b) = t.baseline_in(n) else {
                without += 1;
                continue;
            };
            let sv = t.sched.start.map(|s| wc.between(b.start, s)).unwrap_or(0);
            let fv = t.sched.finish.map(|f| wc.between(b.finish, f)).unwrap_or(0);
            if only_slipped && fv <= 0 {
                continue;
            }
            rows.push(json!({
                "id": t.id.0, "row": i + 1, "name": t.name, "summary": t.sched.is_summary,
                "baseline_start": fmt_dt(Some(b.start)), "start": fmt_dt(t.sched.start),
                "start_variance": views::dur(p, sv), "start_variance_minutes": sv,
                "baseline_finish": fmt_dt(Some(b.finish)), "finish": fmt_dt(t.sched.finish),
                "finish_variance": views::dur(p, fv), "finish_variance_minutes": fv,
                "baseline_duration": views::dur(p, b.duration), "duration": views::dur(p, t.sched.duration),
                "duration_variance_minutes": t.sched.duration - b.duration,
                "baseline_work": format_work(b.work), "work": format_work(t.sched.work), "work_variance": format_work(t.sched.work - b.work),
                "baseline_cost": round2(b.cost), "cost": round2(t.sched.cost), "cost_variance": round2(t.sched.cost - b.cost),
            }));
        }
        Ok(json!({
            "baseline": PlanSlot::Baseline(n).name(),
            "tasks": rows,
            "tasks_without_baseline": without,
            "hint": if without > 0 && rows.is_empty() { "No values in this baseline: call set_baseline first." } else { "" },
        }))
    })
}
