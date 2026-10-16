//! JSON renderings of project entities returned by tools and resources.
//!
//! Every value is given both in machine-friendly form (ids, minutes, fractions)
//! and as a human string (e.g. `"duration": "5 days", "duration_minutes": 2400`).

use std::collections::HashSet;

use chrono::NaiveDateTime;
use plancraft_core::calendar::{Calendar, CalendarException, TimeRange, WEEKDAYS};
use plancraft_core::duration::{Lag, Minutes, format_work};
use plancraft_core::ids::*;
use plancraft_core::model::*;
use plancraft_core::report;
use plancraft_core::schedule::project_calendar;
use plancraft_core::workspace::{ChangeSource, Document};
use serde_json::{Map, Value, json};

use crate::args::*;

pub fn weekday_name(wd: chrono::Weekday) -> &'static str {
    match wd {
        chrono::Weekday::Mon => "monday",
        chrono::Weekday::Tue => "tuesday",
        chrono::Weekday::Wed => "wednesday",
        chrono::Weekday::Thu => "thursday",
        chrono::Weekday::Fri => "friday",
        chrono::Weekday::Sat => "saturday",
        chrono::Weekday::Sun => "sunday",
    }
}

pub fn ranges_json(r: &[TimeRange]) -> Value {
    Value::Array(r.iter().map(|x| Value::String(x.to_string())).collect())
}

pub fn source_str(s: ChangeSource) -> &'static str {
    match s {
        ChangeSource::User => "user",
        ChangeSource::Agent => "agent",
    }
}

/// Short document descriptor.
pub fn document_brief(doc: &Document, active: bool) -> Value {
    json!({
        "handle": doc.handle,
        "name": doc.project.name,
        "title": doc.title(),
        "path": doc.path.as_ref().map(|p| p.display().to_string()),
        "active": active,
        "dirty": doc.dirty,
        "revision": doc.revision,
        "tasks": doc.project.tasks.len(),
        "resources": doc.project.resources.len(),
        "start": fmt_dt(doc.project.sched.start.or(Some(doc.project.start))),
        "finish": fmt_dt(doc.project.sched.finish),
    })
}

pub fn dur(p: &Project, m: Minutes) -> String {
    p.format_duration(m, false)
}

pub fn lag_json(p: &Project, lag: Lag) -> Value {
    let text = lag.format(&p.settings.time);
    match lag {
        Lag::None => json!({ "text": "" }),
        Lag::Working(m) => json!({ "text": text, "unit": "working", "minutes": m }),
        Lag::Elapsed(m) => json!({ "text": text, "unit": "elapsed", "minutes": m }),
        Lag::Percent(pc) => json!({ "text": text, "unit": "percent", "percent": pc }),
    }
}

/// Describes a task reference (local or external).
pub fn task_ref_json(p: &Project, r: &TaskRef) -> Value {
    match r {
        TaskRef::Local(id) => json!({
            "id": id.0,
            "row": p.row_number(*id),
            "name": p.task_ref_name(r),
        }),
        TaskRef::External { project, task } => {
            let ext = p.external(*project);
            let snap = ext.and_then(|e| e.task(*task));
            json!({
                "external": true,
                "external_project": ext.map(|e| e.name.clone()),
                "external_project_id": project.to_string(),
                "id": task.0,
                "name": snap.map(|s| s.name.clone()),
                "start": fmt_dt(snap.and_then(|s| s.start)),
                "finish": fmt_dt(snap.and_then(|s| s.finish)),
                "percent_complete": snap.map(|s| s.percent_complete),
            })
        }
    }
}

pub fn dependency_json(p: &Project, d: &Dependency) -> Value {
    let mut v = task_ref_json(p, &d.predecessor);
    set(&mut v, "type", json!(d.kind.code()));
    set(&mut v, "lag", lag_json(p, d.lag));
    v
}

fn kind_str(t: &Task) -> &'static str {
    if t.sched.is_summary {
        "summary"
    } else if t.is_milestone() {
        "milestone"
    } else {
        "task"
    }
}

/// Compact task row used by list views.
pub fn task_row(p: &Project, idx: usize) -> Value {
    let Some(t) = p.tasks.get(idx) else {
        return Value::Null;
    };
    let mut v = json!({
        "id": t.id.0,
        "row": idx + 1,
        "wbs": p.wbs(idx),
        "name": t.name,
        "level": t.outline_level,
        "kind": kind_str(t),
        "duration": p.format_duration(t.sched.duration, t.estimated && !t.sched.is_summary),
        "duration_minutes": t.sched.duration,
        "start": fmt_dt(t.sched.start),
        "finish": fmt_dt(t.sched.finish),
        "percent_complete": round2(t.sched.percent_complete as f64),
        "critical": t.sched.critical,
        "total_slack": dur(p, t.sched.total_slack),
        "total_slack_minutes": t.sched.total_slack,
        "work": format_work(t.sched.work),
        "work_minutes": t.sched.work,
        "predecessors": p.format_predecessors(t.id),
        "resources": p.format_resource_names(t.id),
    });
    let Some(m) = v.as_object_mut() else {
        return v;
    };
    if t.sched.is_summary {
        m.insert("is_summary".into(), json!(true));
    }
    if t.is_milestone() {
        m.insert("is_milestone".into(), json!(true));
    }
    if t.mode == ScheduleMode::Manual {
        m.insert("mode".into(), json!("manual"));
    }
    if !t.active {
        m.insert("active".into(), json!(false));
    }
    if let Some(d) = t.deadline {
        m.insert("deadline".into(), fmt_dt(Some(d)));
    }
    if t.constraint.kind != ConstraintKind::Asap {
        m.insert(
            "constraint".into(),
            json!(
                format!(
                    "{} {}",
                    constraint_code(t.constraint.kind),
                    t.constraint.date.map(|d| d.format(DATETIME_FMT).to_string()).unwrap_or_default()
                )
                .trim()
            ),
        );
    }
    if !t.sched.warnings.is_empty() {
        m.insert("warnings".into(), json!(t.sched.warnings));
    }
    v
}

pub fn estimate_json(p: &Project, e: &Estimate) -> Value {
    let expected = e.expected_work(&p.settings);
    match *e {
        Estimate::Complexity { points } => json!({
            "method": "complexity", "points": points,
            "expected_work": format_work(expected), "expected_work_minutes": expected,
        }),
        Estimate::ThreePoint { optimistic, most_likely, pessimistic } => json!({
            "method": "three_point",
            "optimistic": format_work(optimistic), "most_likely": format_work(most_likely), "pessimistic": format_work(pessimistic),
            "optimistic_minutes": optimistic, "most_likely_minutes": most_likely, "pessimistic_minutes": pessimistic,
            "expected_work": format_work(expected), "expected_work_minutes": expected,
            "std_dev": format_work(e.std_dev()),
        }),
        Estimate::Work { minutes } => json!({
            "method": "work", "work": format_work(minutes), "work_minutes": minutes,
            "expected_work": format_work(expected), "expected_work_minutes": expected,
        }),
    }
}

/// Assignment cost using the same formula as the scheduler.
pub fn assignment_cost(p: &Project, a: &Assignment) -> f64 {
    match p.resource(a.resource) {
        Some(r) => match r.kind {
            ResourceType::Work => a.work as f64 / 60.0 * r.standard_rate + r.cost_per_use,
            ResourceType::Material => a.units * r.standard_rate + r.cost_per_use,
            ResourceType::Cost => a.cost,
        },
        None => 0.0,
    }
}

/// Actual work of an assignment: direct actual work plus tracked time.
pub fn assignment_actual(p: &Project, a: &Assignment) -> Minutes {
    a.actual_work + p.time_entries.iter().filter(|e| e.task == a.task && e.resource == a.resource).map(|e| e.minutes).sum::<Minutes>()
}

pub fn assignment_json(p: &Project, a: &Assignment) -> Value {
    let r = p.resource(a.resource);
    let t = p.task(a.task);
    let actual = assignment_actual(p, a);
    let mut v = json!({
        "id": a.id.0,
        "task_id": a.task.0,
        "task": t.map(|t| t.name.clone()),
        "task_row": p.row_number(a.task),
        "resource_id": a.resource.0,
        "resource": r.map(|r| r.name.clone()),
        "resource_kind": r.map(|r| enum_str(&r.kind)),
        "units": round2(a.units * 10000.0) / 10000.0,
        "units_text": match r.map(|r| r.kind) {
            Some(ResourceType::Material) => format!("{} {}", round2(a.units), r.map(|r| r.material_label.as_str()).unwrap_or("")).trim().to_string(),
            _ => pct_text(a.units),
        },
        "work": format_work(a.work),
        "work_minutes": a.work,
        "actual_work": format_work(actual),
        "actual_work_minutes": actual,
        "remaining_work": format_work((a.work - actual).max(0)),
        "contour": enum_str(&a.contour),
        "cost": round2(assignment_cost(p, a)),
        "start": fmt_dt(t.and_then(|t| t.sched.start)),
        "finish": fmt_dt(t.and_then(|t| t.sched.finish)),
    });
    if a.cost != 0.0 {
        set(&mut v, "fixed_cost", json!(a.cost));
    }
    v
}

fn variance_json(p: &Project, t: &Task) -> Value {
    let Some(b) = t.baseline else {
        return Value::Null;
    };
    let wc = project_calendar(p);
    let sv = t.sched.start.map(|s| wc.between(b.start, s)).unwrap_or(0);
    let fv = t.sched.finish.map(|f| wc.between(b.finish, f)).unwrap_or(0);
    json!({
        "baseline_start": fmt_dt(Some(b.start)),
        "baseline_finish": fmt_dt(Some(b.finish)),
        "baseline_duration": dur(p, b.duration),
        "baseline_work": format_work(b.work),
        "baseline_cost": round2(b.cost),
        "saved": fmt_dt(Some(b.saved)),
        "start_variance": dur(p, sv),
        "start_variance_minutes": sv,
        "finish_variance": dur(p, fv),
        "finish_variance_minutes": fv,
        "duration_variance_minutes": t.sched.duration - b.duration,
        "work_variance": format_work(t.sched.work - b.work),
        "work_variance_minutes": t.sched.work - b.work,
        "cost_variance": round2(t.sched.cost - b.cost),
    })
}

/// Full task detail.
pub fn task_detail(p: &Project, id: TaskId) -> Value {
    let Some(idx) = p.task_index(id) else {
        return Value::Null;
    };
    let Some(t) = p.tasks.get(idx) else {
        return Value::Null;
    };
    let mut v = task_row(p, idx);
    let Some(m) = v.as_object_mut() else {
        return Value::Null;
    };
    let at = report::status_moment(p);
    let parent = p.parent_index(idx).and_then(|i| p.tasks.get(i)).map(|pt| json!({ "id": pt.id.0, "name": pt.name }));
    let children: Vec<Value> = if p.has_children(idx) {
        let lvl = t.outline_level;
        p.tasks
            .get(p.descendants_range(idx))
            .unwrap_or_default()
            .iter()
            .filter(|c| c.outline_level == lvl.saturating_add(1))
            .map(|c| json!({ "id": c.id.0, "name": c.name }))
            .collect()
    } else {
        Vec::new()
    };
    let succ: Vec<Value> = p
        .successors(id)
        .iter()
        .map(|(sid, d)| json!({ "id": sid.0, "row": p.row_number(*sid), "name": p.task(*sid).map(|x| x.name.clone()), "type": d.kind.code(), "lag": lag_json(p, d.lag) }))
        .collect();
    let extra = json!({
        "mode": enum_str(&t.mode),
        "active": t.active,
        "estimated": t.estimated,
        "task_type": enum_str(&t.task_type),
        "effort_driven": t.effort_driven,
        "priority": t.priority,
        "constraint_type": constraint_code(t.constraint.kind),
        "constraint_label": t.constraint.kind.label(),
        "constraint_date": fmt_dt(t.constraint.date),
        "deadline": fmt_dt(t.deadline),
        "calendar": t.calendar.and_then(|c| p.calendar(c)).map(|c| json!({ "id": c.id.0, "name": c.name })),
        "ignore_resource_calendars": t.ignore_resource_calendars,
        "early_start": fmt_dt(t.sched.early_start),
        "early_finish": fmt_dt(t.sched.early_finish),
        "late_start": fmt_dt(t.sched.late_start),
        "late_finish": fmt_dt(t.sched.late_finish),
        "free_slack": dur(p, t.sched.free_slack),
        "free_slack_minutes": t.sched.free_slack,
        "percent_complete_entered": t.percent_complete,
        "actual_start": fmt_dt(t.actual_start),
        "actual_finish": fmt_dt(t.actual_finish),
        "manual_start": fmt_dt(t.manual_start),
        "manual_finish": fmt_dt(t.manual_finish),
        "actual_work": format_work(t.sched.actual_work),
        "actual_work_minutes": t.sched.actual_work,
        "remaining_work": format_work((t.sched.work - t.sched.actual_work).max(0)),
        "cost": round2(t.sched.cost),
        "fixed_cost": t.fixed_cost,
        "leveling_delay": dur(p, t.leveling_delay),
        "leveling_delay_minutes": t.leveling_delay,
        "estimate": t.estimate.as_ref().map(|e| estimate_json(p, e)),
        "notes": t.notes,
        "deliverable": t.deliverable,
        "show_on_timeline": t.show_on_timeline,
        "bar_color": t.bar_color.map(|c| format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2])),
        "late": report::is_late(t, at, p),
        "parent": parent,
        "children": children,
        "predecessor_links": t.predecessors.iter().map(|d| dependency_json(p, d)).collect::<Vec<_>>(),
        "successor_links": succ,
        "members": t.members.iter().map(|r| task_ref_json(p, r)).collect::<Vec<_>>(),
        "member_of_milestones": p.milestones_of(id).iter().map(|m| json!({ "id": m.0, "name": p.task(*m).map(|x| x.name.clone()) })).collect::<Vec<_>>(),
        "assignments": p.assignments_for_task(id).map(|a| assignment_json(p, a)).collect::<Vec<_>>(),
        "time_logged": format_work(p.time_entries.iter().filter(|e| e.task == id).map(|e| e.minutes).sum::<Minutes>()),
        "baseline": variance_json(p, t),
        "warnings": t.sched.warnings,
    });
    if let Value::Object(e) = extra {
        for (k, val) in e {
            m.insert(k, val);
        }
    }
    v
}

pub fn exception_json(i: usize, e: &CalendarException) -> Value {
    json!({
        "index": i,
        "name": e.name,
        "start": e.start.format(DATE_FMT).to_string(),
        "end": e.end.format(DATE_FMT).to_string(),
        "working": !e.working.is_empty(),
        "working_times": ranges_json(&e.working),
        "yearly": e.yearly,
    })
}

pub fn week_json(c: &Calendar) -> Value {
    let mut m = Map::new();
    for wd in WEEKDAYS {
        m.insert(weekday_name(wd).into(), ranges_json(c.week.day(wd)));
    }
    Value::Object(m)
}

pub fn calendar_brief(p: &Project, c: &Calendar) -> Value {
    json!({
        "id": c.id.0,
        "name": c.name,
        "project_calendar": c.id == p.calendar,
        "weekly_hours": hours(c.week.weekly_minutes() as f64),
        "exceptions": c.exceptions.len(),
        "used_by_tasks": p.tasks.iter().filter(|t| t.calendar == Some(c.id)).count(),
        "used_by_resources": p.resources.iter().filter(|r| r.calendar == Some(c.id)).count(),
    })
}

pub fn calendar_detail(p: &Project, c: &Calendar) -> Value {
    let mut v = calendar_brief(p, c);
    set(&mut v, "work_week", week_json(c));
    set(&mut v, "exception_list", Value::Array(c.exceptions.iter().enumerate().map(|(i, e)| exception_json(i, e)).collect()));
    v
}

/// Resource summary with workload figures.
pub fn resource_row(p: &Project, r: &Resource, over: &HashSet<ResourceId>) -> Value {
    let work: Minutes = p.assignments_for_resource(r.id).map(|a| a.work).sum();
    let actual: Minutes = p.assignments_for_resource(r.id).map(|a| assignment_actual(p, a)).sum();
    let cost: f64 = p.assignments_for_resource(r.id).map(|a| assignment_cost(p, a)).sum();
    let mut v = json!({
        "id": r.id.0,
        "name": r.name,
        "kind": enum_str(&r.kind),
        "category": enum_str(&r.category),
        "initials": r.initials,
        "group": r.group,
        "max_units": r.max_units,
        "max_units_text": pct_text(r.max_units),
        "standard_rate": r.standard_rate,
        "work": format_work(work),
        "work_minutes": work,
        "actual_work": format_work(actual),
        "cost": round2(cost),
        "assignments": p.assignments_for_resource(r.id).count(),
        "overallocated": over.contains(&r.id),
    });
    if !r.email.is_empty() {
        set(&mut v, "email", json!(r.email));
    }
    if r.kind == ResourceType::Material {
        set(&mut v, "material_label", json!(r.material_label));
    }
    v
}

pub fn resource_detail(p: &Project, r: &Resource, over_days: &[chrono::NaiveDate]) -> Value {
    let over: HashSet<ResourceId> = if over_days.is_empty() { HashSet::new() } else { [r.id].into_iter().collect() };
    let mut v = resource_row(p, r, &over);
    let extra = json!({
        "email": r.email,
        "material_label": r.material_label,
        "overtime_rate": r.overtime_rate,
        "cost_per_use": r.cost_per_use,
        "currency": p.currency(),
        "calendar": match r.calendar.and_then(|c| p.calendar(c)) {
            Some(c) => json!({ "id": c.id.0, "name": c.name }),
            None => json!({ "id": p.calendar.0, "name": p.base_calendar().name, "inherited": true }),
        },
        "exceptions": r.exceptions.iter().enumerate().map(|(i, e)| exception_json(i, e)).collect::<Vec<_>>(),
        "availability": r.availability.iter().map(|a| json!({
            "from": fmt_date(a.from), "to": fmt_date(a.to), "units": a.units, "units_text": pct_text(a.units),
        })).collect::<Vec<_>>(),
        "notes": r.notes,
        "assignment_list": p.assignments_for_resource(r.id).map(|a| assignment_json(p, a)).collect::<Vec<_>>(),
        "overallocated_days": over_days.iter().take(60).map(|d| d.format(DATE_FMT).to_string()).collect::<Vec<_>>(),
        "overallocated_day_count": over_days.len(),
        "time_logged": format_work(p.time_entries.iter().filter(|e| e.resource == r.id).map(|e| e.minutes).sum::<Minutes>()),
    });
    if let (Value::Object(m), Value::Object(e)) = (&mut v, extra) {
        m.extend(e);
    }
    v
}

pub fn time_entry_json(p: &Project, e: &TimeEntry) -> Value {
    json!({
        "id": e.id.0,
        "task_id": e.task.0,
        "task": p.task(e.task).map(|t| t.name.clone()),
        "resource_id": e.resource.0,
        "resource": p.resource(e.resource).map(|r| r.name.clone()),
        "date": e.date.format(DATE_FMT).to_string(),
        "hours": hours(e.minutes as f64),
        "minutes": e.minutes,
        "note": e.note,
    })
}

pub fn milestone_status_json(p: &Project, m: &report::MilestoneStatus) -> Value {
    json!({
        "id": m.id.0,
        "row": p.row_number(m.id),
        "name": m.name,
        "deliverable": m.deliverable,
        "date": fmt_dt(m.date),
        "deadline": fmt_dt(m.deadline),
        "baseline": fmt_dt(m.baseline),
        "members": m.members,
        "members_complete": m.members_complete,
        "progress": round2(m.progress as f64),
        "late": m.late,
        "complete": m.complete,
    })
}

pub fn settings_json(p: &Project) -> Value {
    let s = &p.settings;
    let hh = |m: u32| format!("{:02}:{:02}", m / 60, m % 60);
    json!({
        "hours_per_day": hours(s.time.minutes_per_day as f64),
        "hours_per_week": hours(s.time.minutes_per_week as f64),
        "days_per_month": s.time.days_per_month,
        "default_start_time": hh(s.time.default_start_time),
        "default_finish_time": hh(s.time.default_finish_time),
        "duration_unit": enum_str(&s.duration_unit),
        "currency": s.currency,
        "minutes_per_point": s.minutes_per_point,
        "hours_per_point": hours(s.minutes_per_point as f64),
        "critical_slack": dur(p, s.critical_slack),
        "critical_slack_minutes": s.critical_slack,
        "new_tasks_auto": s.new_tasks_auto,
        "use_resource_calendars": s.use_resource_calendars,
        "progress_from_time": s.progress_from_time,
    })
}

pub fn stats_json(p: &Project, st: &report::ProjectStats) -> Value {
    json!({
        "start": fmt_dt(st.start),
        "finish": fmt_dt(st.finish),
        "baseline_finish": fmt_dt(st.baseline_finish),
        "finish_variance": dur(p, st.finish_variance),
        "finish_variance_minutes": st.finish_variance,
        "duration": dur(p, st.duration),
        "duration_minutes": st.duration,
        "work": format_work(st.work),
        "work_minutes": st.work,
        "actual_work": format_work(st.actual_work),
        "remaining_work": format_work(st.remaining_work),
        "cost": round2(st.cost),
        "baseline_cost": round2(st.baseline_cost),
        "currency": p.currency(),
        "percent_complete": round2(st.percent_complete as f64),
        "tasks": st.tasks,
        "summary_tasks": st.summary_tasks,
        "milestones": st.milestones,
        "completed_tasks": st.completed_tasks,
        "in_progress_tasks": st.in_progress_tasks,
        "late_tasks": st.late_tasks,
        "critical_tasks": st.critical_tasks,
        "resources": st.resources,
        "overallocated_resources": st.overallocated_resources,
        "warnings": st.warnings,
    })
}

pub fn earned_value_json(ev: &Option<report::EarnedValue>) -> Value {
    match ev {
        None => json!(null),
        Some(e) => json!({
            "bcws_planned_value": round2(e.bcws),
            "bcwp_earned_value": round2(e.bcwp),
            "acwp_actual_cost": round2(e.acwp),
            "schedule_variance": round2(e.sv),
            "cost_variance": round2(e.cv),
            "spi": round2(e.spi),
            "cpi": round2(e.cpi),
        }),
    }
}

/// Project information block used by `get_project`.
pub fn project_info(doc: &Document) -> Value {
    let p = &doc.project;
    json!({
        "handle": doc.handle,
        "id": p.id.to_string(),
        "name": p.name,
        "author": p.author,
        "manager": p.manager,
        "company": p.company,
        "notes": p.notes,
        "start": fmt_dt(Some(p.start)),
        "finish": fmt_dt(p.sched.finish),
        "status_date": fmt_date(p.status_date),
        "calendar": json!({ "id": p.calendar.0, "name": p.base_calendar().name }),
        "path": doc.path.as_ref().map(|x| x.display().to_string()),
        "dirty": doc.dirty,
        "revision": doc.revision,
        "work": format_work(p.sched.work),
        "cost": round2(p.sched.cost),
        "percent_complete": round2(p.sched.percent_complete as f64),
        "calculated_at": fmt_dt(p.sched.calculated_at),
    })
}

/// A plain-text Gantt-like table of the outline.
pub fn schedule_table(p: &Project, rows: &[usize]) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "{} — {} → {} ({} tasks)\n",
        p.name,
        p.sched.start.map(|d| d.format(DATETIME_FMT).to_string()).unwrap_or_default(),
        p.sched.finish.map(|d| d.format(DATETIME_FMT).to_string()).unwrap_or_default(),
        p.tasks.len()
    ));
    out.push_str("row | id | wbs | name | duration | start | finish | % | pred | resources | flags\n");
    for (i, t) in rows.iter().filter_map(|&i| Some((i, p.tasks.get(i)?))) {
        let indent = "  ".repeat(t.outline_level.saturating_sub(1) as usize);
        let mut flags = Vec::new();
        if t.sched.is_summary {
            flags.push("summary");
        }
        if t.is_milestone() {
            flags.push("milestone");
        }
        if t.sched.critical && !t.sched.is_summary {
            flags.push("critical");
        }
        if t.mode == ScheduleMode::Manual {
            flags.push("manual");
        }
        if !t.active {
            flags.push("inactive");
        }
        if !t.sched.warnings.is_empty() {
            flags.push("warning");
        }
        let short = |d: Option<NaiveDateTime>| d.map(|d| d.format("%Y-%m-%d").to_string()).unwrap_or_else(|| "-".into());
        out.push_str(&format!(
            "{} | {} | {} | {}{} | {} | {} | {} | {}% | {} | {} | {}\n",
            i + 1,
            t.id.0,
            p.wbs(i),
            indent,
            t.name,
            p.format_duration(t.sched.duration, t.estimated && !t.sched.is_summary),
            short(t.sched.start),
            short(t.sched.finish),
            t.sched.percent_complete.round(),
            p.format_predecessors(t.id),
            p.format_resource_names(t.id),
            flags.join(",")
        ));
    }
    out
}

/// A human-readable status summary in Markdown.
pub fn summary_text(doc: &Document) -> String {
    let p = &doc.project;
    let st = report::project_stats(p);
    let mut s = String::new();
    s.push_str(&format!("# {}\n\n", p.name));
    if let Some(path) = &doc.path {
        s.push_str(&format!("File: {}{}\n", path.display(), if doc.dirty { " (unsaved changes)" } else { "" }));
    }
    s.push_str(&format!(
        "Schedule: {} → {} ({})\n",
        st.start.map(|d| d.format(DATETIME_FMT).to_string()).unwrap_or_else(|| "-".into()),
        st.finish.map(|d| d.format(DATETIME_FMT).to_string()).unwrap_or_else(|| "-".into()),
        dur(p, st.duration)
    ));
    s.push_str(&format!(
        "Progress: {:.0}% complete; work {} ({} actual); cost {}{:.2}\n",
        st.percent_complete,
        format_work(st.work),
        format_work(st.actual_work),
        p.currency(),
        st.cost
    ));
    s.push_str(&format!(
        "Tasks: {} ({} complete, {} in progress, {} late, {} critical), {} milestones, {} summaries; {} resources ({} overallocated)\n",
        st.tasks,
        st.completed_tasks,
        st.in_progress_tasks,
        st.late_tasks,
        st.critical_tasks,
        st.milestones,
        st.summary_tasks,
        st.resources,
        st.overallocated_resources
    ));
    if let Some(bf) = st.baseline_finish {
        s.push_str(&format!("Baseline finish: {} (variance {})\n", bf.format(DATETIME_FMT), dur(p, st.finish_variance)));
    }
    if let Some(ev) = &st.earned_value {
        s.push_str(&format!("Earned value: SPI {:.2}, CPI {:.2}\n", ev.spi, ev.cpi));
    }
    if !st.milestone_status.is_empty() {
        s.push_str("\n## Milestones\n");
        for m in &st.milestone_status {
            s.push_str(&format!(
                "- {} (id {}): {} — {:.0}%{}{}\n",
                m.name,
                m.id.0,
                m.date.map(|d| d.format("%Y-%m-%d").to_string()).unwrap_or_else(|| "-".into()),
                m.progress,
                if m.complete { ", complete" } else { "" },
                if m.late { ", LATE" } else { "" }
            ));
        }
    }
    let warnings: Vec<String> = p
        .tasks
        .iter()
        .flat_map(|t| t.sched.warnings.iter().map(move |w| format!("{} (id {}): {w}", t.name, t.id.0)))
        .chain(p.sched.warnings.iter().cloned())
        .collect();
    if !warnings.is_empty() {
        s.push_str("\n## Warnings\n");
        for w in warnings.iter().take(50) {
            s.push_str(&format!("- {w}\n"));
        }
    }
    s
}
