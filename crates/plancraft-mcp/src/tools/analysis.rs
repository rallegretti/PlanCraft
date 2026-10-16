//! Schedule analysis tools: schedule table, critical path, workload, leveling,
//! statistics, baselines and variance.

use std::collections::{BTreeMap, HashSet};

use chrono::NaiveDate;
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
            "Save the current start, finish, duration, work and cost as the baseline of all tasks (or the given tasks) for later variance tracking.",
            schema(json!({ "tasks": task_refs("Only these tasks (default all).") }), &[]),
            set_baseline,
        ),
        Tool::new(
            "clear_baseline",
            "Clear baseline",
            "Remove the baseline from all tasks (or the given tasks).",
            schema(json!({ "tasks": task_refs("Only these tasks (default all).") }), &[]),
            clear_baseline,
        )
        .destructive(),
        Tool::new(
            "get_variance",
            "Baseline variance",
            "Compare current schedule with the baseline per task: start/finish variance (working time), duration, work and cost variance.",
            schema(
                json!({
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

fn set_baseline(ctx: &Ctx, a: &Args) -> ToolResult {
    ctx.edit(
        a,
        "Set baseline",
        |p, _| {
            let ids = task_list_arg(p, a, "tasks")?;
            p.set_baseline(&ids);
            Ok(p.tasks.iter().filter(|t| t.baseline.is_some()).count())
        },
        |doc, n| Ok(json!({ "tasks_with_baseline": n, "baseline_finish": fmt_dt(doc.project.tasks.iter().filter_map(|t| t.baseline.map(|b| b.finish)).max()) })),
    )
}

fn clear_baseline(ctx: &Ctx, a: &Args) -> ToolResult {
    ctx.edit(
        a,
        "Clear baseline",
        |p, _| {
            let ids = task_list_arg(p, a, "tasks")?;
            p.clear_baseline(&ids);
            Ok(p.tasks.iter().filter(|t| t.baseline.is_some()).count())
        },
        |_, n| Ok(json!({ "tasks_with_baseline": n })),
    )
}

fn get_variance(ctx: &Ctx, a: &Args) -> ToolResult {
    ctx.read(a, |doc| {
        let p = &doc.project;
        let ids = task_list_arg(p, a, "tasks")?;
        let only_slipped = a.bool("only_slipped")?.unwrap_or(false);
        let wc = project_calendar(p);
        let mut rows = Vec::new();
        let mut without = 0;
        for (i, t) in p.tasks.iter().enumerate() {
            if !ids.is_empty() && !ids.contains(&t.id) {
                continue;
            }
            let Some(b) = t.baseline else {
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
            "tasks": rows,
            "tasks_without_baseline": without,
            "hint": if without > 0 && rows.is_empty() { "No baseline saved: call set_baseline first." } else { "" },
        }))
    })
}
