//! Task table columns: how each field is displayed and edited.

use chrono::NaiveDateTime;
use plancraft_core::prelude::*;
use plancraft_core::report;

use crate::format;
use crate::theme;
use crate::widgets::grid::{Cell, ColumnDef};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TaskCol {
    Indicators,
    Mode,
    Wbs,
    Name,
    Duration,
    Start,
    Finish,
    Predecessors,
    Successors,
    Resources,
    PercentComplete,
    Work,
    ActualWork,
    RemainingWork,
    Cost,
    FixedCost,
    BaselineStart,
    BaselineFinish,
    BaselineWork,
    BaselineCost,
    StartVariance,
    FinishVariance,
    WorkVariance,
    CostVariance,
    ActualStart,
    ActualFinish,
    EarlyStart,
    EarlyFinish,
    LateStart,
    LateFinish,
    FreeSlack,
    TotalSlack,
    ConstraintType,
    ConstraintDate,
    Deadline,
    Priority,
    TaskType,
    EstimateMethod,
    Points,
    Optimistic,
    MostLikely,
    Pessimistic,
    ExpectedWork,
    Deliverable,
    Milestones,
    Notes,
    UniqueId,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskTable {
    Entry,
    Schedule,
    Tracking,
    Work,
    Cost,
    Estimates,
    Variance,
    Summary,
    Details,
}

impl TaskTable {
    pub const ALL: [TaskTable; 9] = [
        TaskTable::Entry,
        TaskTable::Schedule,
        TaskTable::Tracking,
        TaskTable::Work,
        TaskTable::Cost,
        TaskTable::Estimates,
        TaskTable::Variance,
        TaskTable::Summary,
        TaskTable::Details,
    ];

    pub fn label(self) -> &'static str {
        match self {
            TaskTable::Entry => "Entry",
            TaskTable::Schedule => "Schedule",
            TaskTable::Tracking => "Tracking",
            TaskTable::Work => "Work",
            TaskTable::Cost => "Cost",
            TaskTable::Estimates => "Estimates",
            TaskTable::Variance => "Variance",
            TaskTable::Summary => "Summary",
            TaskTable::Details => "Details",
        }
    }

    pub fn columns(self) -> Vec<TaskCol> {
        use TaskCol::*;
        match self {
            TaskTable::Entry => vec![Indicators, Mode, Name, Duration, Start, Finish, Predecessors, Resources],
            TaskTable::Schedule => vec![
                Indicators,
                Name,
                Start,
                Finish,
                LateStart,
                LateFinish,
                FreeSlack,
                TotalSlack,
                ConstraintType,
                ConstraintDate,
                Deadline,
            ],
            TaskTable::Tracking => {
                vec![Indicators, Name, ActualStart, ActualFinish, PercentComplete, Duration, ActualWork, RemainingWork, Cost]
            }
            TaskTable::Work => vec![Indicators, Name, Work, BaselineWork, WorkVariance, ActualWork, RemainingWork, PercentComplete],
            TaskTable::Cost => vec![Indicators, Name, FixedCost, Cost, BaselineCost, CostVariance],
            TaskTable::Estimates => {
                vec![Indicators, Name, EstimateMethod, Points, Optimistic, MostLikely, Pessimistic, ExpectedWork, Work, Duration, Resources]
            }
            TaskTable::Variance => vec![Indicators, Name, Start, Finish, BaselineStart, BaselineFinish, StartVariance, FinishVariance],
            TaskTable::Summary => vec![Indicators, Wbs, Name, Duration, Start, Finish, PercentComplete, Cost, Work, Milestones],
            TaskTable::Details => {
                vec![Indicators, UniqueId, Name, TaskType, Priority, EarlyStart, EarlyFinish, Predecessors, Successors, Deliverable, Notes]
            }
        }
    }
}

impl TaskCol {
    pub fn def(self, p: &Project) -> ColumnDef {
        use TaskCol::*;
        let _ = p;
        match self {
            Indicators => ColumnDef::new("ℹ", 34.0).read_only().center(),
            Mode => ColumnDef::new("Mode", 46.0).center().choice(&["Auto Scheduled", "Manually Scheduled"]),
            Wbs => ColumnDef::new("WBS", 50.0).read_only(),
            Name => ColumnDef::new("Task Name", 230.0),
            Duration => ColumnDef::new("Duration", 76.0).right(),
            Start => ColumnDef::new("Start", 96.0),
            Finish => ColumnDef::new("Finish", 96.0),
            Predecessors => ColumnDef::new("Predecessors", 96.0),
            Successors => ColumnDef::new("Successors", 96.0).read_only(),
            Resources => ColumnDef::new("Resource Names", 160.0),
            PercentComplete => ColumnDef::new("% Comp.", 62.0).right(),
            Work => ColumnDef::new("Work", 70.0).right(),
            ActualWork => ColumnDef::new("Act. Work", 72.0).right().read_only(),
            RemainingWork => ColumnDef::new("Rem. Work", 72.0).right().read_only(),
            Cost => ColumnDef::new("Total Cost", 90.0).right().read_only(),
            FixedCost => ColumnDef::new("Fixed Cost", 86.0).right(),
            BaselineStart => ColumnDef::new("Baseline Start", 100.0).read_only(),
            BaselineFinish => ColumnDef::new("Baseline Finish", 100.0).read_only(),
            BaselineWork => ColumnDef::new("Baseline Work", 86.0).right().read_only(),
            BaselineCost => ColumnDef::new("Baseline Cost", 90.0).right().read_only(),
            StartVariance => ColumnDef::new("Start Var.", 76.0).right().read_only(),
            FinishVariance => ColumnDef::new("Finish Var.", 76.0).right().read_only(),
            WorkVariance => ColumnDef::new("Work Var.", 72.0).right().read_only(),
            CostVariance => ColumnDef::new("Cost Var.", 86.0).right().read_only(),
            ActualStart => ColumnDef::new("Actual Start", 100.0),
            ActualFinish => ColumnDef::new("Actual Finish", 100.0),
            EarlyStart => ColumnDef::new("Early Start", 100.0).read_only(),
            EarlyFinish => ColumnDef::new("Early Finish", 100.0).read_only(),
            LateStart => ColumnDef::new("Late Start", 100.0).read_only(),
            LateFinish => ColumnDef::new("Late Finish", 100.0).read_only(),
            FreeSlack => ColumnDef::new("Free Slack", 76.0).right().read_only(),
            TotalSlack => ColumnDef::new("Total Slack", 76.0).right().read_only(),
            ConstraintType => ColumnDef::new("Constraint", 150.0).choice(&ConstraintKind::ALL.map(|c| c.label())),
            ConstraintDate => ColumnDef::new("Constraint Date", 104.0),
            Deadline => ColumnDef::new("Deadline", 100.0),
            Priority => ColumnDef::new("Priority", 60.0).right(),
            TaskType => ColumnDef::new("Type", 100.0).choice(&["Fixed Units", "Fixed Duration", "Fixed Work"]),
            EstimateMethod => ColumnDef::new("Estimate", 96.0).choice(&["None", "Complexity", "Three-point", "Work"]),
            Points => ColumnDef::new("Points", 58.0).right(),
            Optimistic => ColumnDef::new("Optimistic", 80.0).right(),
            MostLikely => ColumnDef::new("Most Likely", 80.0).right(),
            Pessimistic => ColumnDef::new("Pessimistic", 80.0).right(),
            ExpectedWork => ColumnDef::new("Expected Work", 90.0).right().read_only(),
            Deliverable => ColumnDef::new("Deliverable", 160.0),
            Milestones => ColumnDef::new("Milestones", 140.0).read_only(),
            Notes => ColumnDef::new("Notes", 200.0),
            UniqueId => ColumnDef::new("Unique ID", 64.0).right().read_only(),
        }
    }
}

fn var_text(p: &Project, m: Minutes) -> String {
    if m == 0 { p.format_duration(0, false) } else { format!("{}{}", if m > 0 { "+" } else { "-" }, p.format_duration(m.abs(), false)) }
}

/// Indicator glyphs for a task.
pub fn indicators(p: &Project, t: &Task, overallocated: &dyn Fn(TaskId) -> bool) -> Vec<(String, egui::Color32, String)> {
    let mut v = Vec::new();
    if t.is_complete() {
        v.push(("✔".into(), theme::OK, "This task was completed".into()));
    }
    if !t.sched.warnings.is_empty() {
        v.push(("⚠".into(), theme::ERROR, t.sched.warnings.join("\n")));
    } else if t.deadline.is_some() {
        v.push(("⚑".into(), theme::DEADLINE, format!("Deadline: {}", format::date(t.deadline))));
    }
    if t.constraint.kind.needs_date() {
        v.push((
            "📌".into(),
            theme::ACCENT_MID,
            format!("{} constraint on {}", t.constraint.kind.label(), format::date(t.constraint.date)),
        ));
    }
    if overallocated(t.id) {
        v.push(("👤".into(), theme::ERROR, "A resource assigned to this task is overallocated".into()));
    }
    if t.predecessors.iter().any(|d| matches!(d.predecessor, TaskRef::External { .. }))
        || t.members.iter().any(|m| matches!(m, TaskRef::External { .. }))
    {
        v.push(("🔗".into(), theme::ACCENT_MID, "Linked to a task in another project".into()));
    }
    if !t.notes.is_empty() {
        v.push(("📝".into(), theme::TEXT_WEAK, t.notes.clone()));
    }
    if !t.active {
        v.push(("⏸".into(), theme::TEXT_WEAK, "Inactive task".into()));
    }
    let _ = p;
    v.truncate(2);
    v
}

pub struct CellCtx<'a> {
    pub overallocated: &'a dyn Fn(TaskId) -> bool,
}

/// Cell content of `col` for the task at `idx`.
pub fn cell(col: TaskCol, p: &Project, idx: usize, ctx: &CellCtx) -> Cell {
    use TaskCol::*;
    let t = &p.tasks[idx];
    let s = &t.sched;
    let summary = s.is_summary;
    let mut c = match col {
        Indicators => Cell { icons: indicators(p, t, ctx.overallocated), ..Default::default() },
        Mode => {
            let (g, tip) = match t.mode {
                ScheduleMode::Auto => ("⟳", "Auto Scheduled"),
                ScheduleMode::Manual => ("📌", "Manually Scheduled"),
            };
            Cell {
                text: g.into(),
                color: Some(theme::ACCENT_MID),
                tooltip: Some(tip.into()),
                edit_text: Some(tip.into()),
                ..Default::default()
            }
        }
        Wbs => Cell::weak(p.wbs(idx)),
        Name => {
            let indent = (t.outline_level.saturating_sub(1)) as f32 * 16.0;
            Cell { text: t.name.clone(), bold: summary, indent, expander: if summary { Some(true) } else { None }, ..Default::default() }
        }
        Duration => Cell::text(p.format_duration(s.duration, t.estimated && !summary)),
        Start => Cell { text: format::date(s.start), edit_text: Some(format::iso_date(s.start)), ..Default::default() },
        Finish => Cell { text: format::date(s.finish), edit_text: Some(format::iso_date(s.finish)), ..Default::default() },
        Predecessors => Cell::text(p.format_predecessors(t.id)),
        Successors => Cell::text(
            p.successors(t.id).iter().filter_map(|(sid, _)| p.row_number(*sid)).map(|r| r.to_string()).collect::<Vec<_>>().join(","),
        ),
        Resources => Cell::text(p.format_resource_names(t.id)),
        PercentComplete => Cell::text(format::percent(s.percent_complete)),
        Work => Cell::text(format_work(s.work)),
        ActualWork => Cell::text(format_work(s.actual_work)),
        RemainingWork => Cell::text(format_work((s.work - s.actual_work).max(0))),
        Cost => Cell::text(format::money(p, s.cost)),
        FixedCost => Cell::text(format::money(p, t.fixed_cost)),
        BaselineStart => Cell::text(t.baseline.map(|b| format::date(Some(b.start))).unwrap_or_else(|| "NA".into())),
        BaselineFinish => Cell::text(t.baseline.map(|b| format::date(Some(b.finish))).unwrap_or_else(|| "NA".into())),
        BaselineWork => Cell::text(t.baseline.map(|b| format_work(b.work)).unwrap_or_default()),
        BaselineCost => Cell::text(t.baseline.map(|b| format::money(p, b.cost)).unwrap_or_default()),
        StartVariance | FinishVariance => {
            let wc = plancraft_core::schedule::project_calendar(p);
            let v = match (t.baseline, s.start, s.finish) {
                (Some(b), Some(st), Some(fi)) => {
                    Some(if col == StartVariance { wc.between(b.start, st) } else { wc.between(b.finish, fi) })
                }
                _ => None,
            };
            match v {
                Some(m) => Cell { text: var_text(p, m), color: Some(if m > 0 { theme::ERROR } else { theme::TEXT }), ..Default::default() },
                None => Cell::default(),
            }
        }
        WorkVariance => t.baseline.map(|b| Cell::text(format_work(s.work - b.work))).unwrap_or_default(),
        CostVariance => t.baseline.map(|b| Cell::text(format::money(p, s.cost - b.cost))).unwrap_or_default(),
        ActualStart => Cell { text: format::date(t.actual_start), edit_text: Some(format::iso_date(t.actual_start)), ..Default::default() },
        ActualFinish => {
            Cell { text: format::date(t.actual_finish), edit_text: Some(format::iso_date(t.actual_finish)), ..Default::default() }
        }
        EarlyStart => Cell::text(format::date(s.early_start)),
        EarlyFinish => Cell::text(format::date(s.early_finish)),
        LateStart => Cell::text(format::date(s.late_start)),
        LateFinish => Cell::text(format::date(s.late_finish)),
        FreeSlack => Cell::text(format::slack(p, s.free_slack)),
        TotalSlack => Cell {
            text: format::slack(p, s.total_slack),
            color: if s.total_slack < 0 { Some(theme::ERROR) } else { None },
            ..Default::default()
        },
        ConstraintType => Cell::text(t.constraint.kind.label()),
        ConstraintDate => {
            Cell { text: format::date(t.constraint.date), edit_text: Some(format::iso_date(t.constraint.date)), ..Default::default() }
        }
        Deadline => Cell { text: format::date(t.deadline), edit_text: Some(format::iso_date(t.deadline)), ..Default::default() },
        Priority => Cell::text(t.priority.to_string()),
        TaskType => Cell::text(match t.task_type {
            plancraft_core::model::TaskType::FixedUnits => "Fixed Units",
            plancraft_core::model::TaskType::FixedDuration => "Fixed Duration",
            plancraft_core::model::TaskType::FixedWork => "Fixed Work",
        }),
        EstimateMethod => Cell::text(match t.estimate {
            None => "None",
            Some(Estimate::Complexity { .. }) => "Complexity",
            Some(Estimate::ThreePoint { .. }) => "Three-point",
            Some(Estimate::Work { .. }) => "Work",
        }),
        Points => match t.estimate {
            Some(Estimate::Complexity { points }) => Cell::text(format!("{points}")),
            _ => Cell::default(),
        },
        Optimistic | MostLikely | Pessimistic => match t.estimate {
            Some(Estimate::ThreePoint { optimistic, most_likely, pessimistic }) => Cell::text(format_work(match col {
                Optimistic => optimistic,
                MostLikely => most_likely,
                _ => pessimistic,
            })),
            _ => Cell::default(),
        },
        ExpectedWork => t.estimate.map(|e| Cell::text(format_work(e.expected_work(&p.settings)))).unwrap_or_default(),
        Deliverable => Cell::text(t.deliverable.clone()),
        Milestones => {
            Cell::weak(p.milestones_of(t.id).iter().filter_map(|m| p.task(*m)).map(|m| m.name.clone()).collect::<Vec<_>>().join(", "))
        }
        Notes => Cell::text(t.notes.lines().next().unwrap_or_default().to_string()),
        UniqueId => Cell::weak(t.id.0.to_string()),
    };
    if summary && col != Name && col != Indicators {
        c.bold = true;
    }
    if !t.active {
        c.color = Some(theme::TEXT_WEAK);
        c.strike = col == Name;
    }
    if col == Name && t.sched.critical && t.mode == ScheduleMode::Auto && !summary {
        // Critical tasks are shown in the chart; keep table readable.
    }
    c
}

fn parse_date(p: &Project, text: &str, finish: bool) -> Result<Option<NaiveDateTime>, String> {
    let t = text.trim();
    if t.is_empty() || t.eq_ignore_ascii_case("na") {
        return Ok(None);
    }
    p.parse_datetime(t, finish).map(Some).ok_or_else(|| format!("'{t}' is not a valid date (use YYYY-MM-DD)"))
}

fn parse_work_text(p: &Project, text: &str) -> Result<Minutes, String> {
    parse_work(text, &p.settings.time).ok_or_else(|| format!("'{text}' is not a valid amount of work"))
}

/// Applies an edited value to a task. Runs inside an undoable edit.
pub fn commit(col: TaskCol, p: &mut Project, id: TaskId, text: &str) -> Result<(), String> {
    use TaskCol::*;
    let idx = p.task_index(id).ok_or("task not found")?;
    let summary = p.has_children(idx);
    let err = |e: PlanError| e.to_string();
    match col {
        Name => {
            if text.trim().is_empty() {
                return Err("task name cannot be empty".into());
            }
            p.tasks[idx].name = text.trim().to_string();
        }
        Mode => {
            p.tasks[idx].mode = if text.starts_with('M') { ScheduleMode::Manual } else { ScheduleMode::Auto };
            if p.tasks[idx].mode == ScheduleMode::Manual {
                let t = &mut p.tasks[idx];
                t.manual_start = t.sched.start;
                t.manual_finish = t.sched.finish;
            }
        }
        Duration => {
            let (m, est) = p.parse_duration(text).ok_or_else(|| format!("'{text}' is not a valid duration"))?;
            p.set_duration(id, m, est).map_err(err)?;
        }
        Start | Finish => {
            let finish = col == Finish;
            let date = parse_date(p, text, finish)?.ok_or("a date is required")?;
            if summary {
                return Err("summary task dates are calculated from their subtasks".into());
            }
            let t = &mut p.tasks[idx];
            if t.mode == ScheduleMode::Manual {
                if finish {
                    t.manual_finish = Some(date);
                } else {
                    let dur = t.manual_finish.zip(t.manual_start).map(|(f, s)| f - s);
                    t.manual_start = Some(date);
                    t.manual_finish = dur.map(|d| date + d);
                }
            } else {
                t.constraint = Constraint { kind: if finish { ConstraintKind::Fnet } else { ConstraintKind::Snet }, date: Some(date) };
            }
        }
        Predecessors => {
            let deps = p.parse_predecessors(text).map_err(err)?;
            p.set_predecessors(id, deps).map_err(err)?;
        }
        Resources => p.apply_resource_names(id, text).map_err(err)?,
        PercentComplete => {
            let v: f64 = text.trim().trim_end_matches('%').trim().parse().map_err(|_| format!("'{text}' is not a percentage"))?;
            p.set_percent_complete(id, v.clamp(0.0, 100.0).round() as u8).map_err(err)?;
        }
        Work => {
            if summary {
                return Err("summary work is calculated from subtasks".into());
            }
            let w = parse_work_text(p, text)?;
            let assignments: Vec<(AssignmentId, Minutes)> = p
                .assignments_for_task(id)
                .filter(|a| p.resource(a.resource).is_some_and(|r| r.kind == ResourceType::Work))
                .map(|a| (a.id, a.work))
                .collect();
            if assignments.is_empty() {
                p.tasks[idx].estimate = Some(Estimate::Work { minutes: w });
            } else {
                let total: Minutes = assignments.iter().map(|(_, w)| w).sum();
                for (aid, aw) in &assignments {
                    let share = if total > 0 { *aw as f64 / total as f64 } else { 1.0 / assignments.len() as f64 };
                    p.set_assignment_work(*aid, (w as f64 * share).round() as Minutes).map_err(err)?;
                }
            }
        }
        FixedCost => p.tasks[idx].fixed_cost = format::parse_money(text).ok_or("invalid amount")?,
        ActualStart => p.tasks[idx].actual_start = parse_date(p, text, false)?,
        ActualFinish => {
            let d = parse_date(p, text, true)?;
            let t = &mut p.tasks[idx];
            t.actual_finish = d;
            if d.is_some() {
                t.percent_complete = 100;
                if t.actual_start.is_none() {
                    t.actual_start = t.sched.start;
                }
            }
        }
        ConstraintType => {
            let kind = ConstraintKind::ALL.into_iter().find(|k| k.label() == text).unwrap_or_default();
            let t = &mut p.tasks[idx];
            t.constraint.kind = kind;
            if kind.needs_date() && t.constraint.date.is_none() {
                t.constraint.date = if kind.is_start_constraint() { t.sched.start } else { t.sched.finish };
            }
            if !kind.needs_date() {
                t.constraint.date = None;
            }
        }
        ConstraintDate => {
            let finish = !p.tasks[idx].constraint.kind.is_start_constraint();
            let d = parse_date(p, text, finish)?;
            let t = &mut p.tasks[idx];
            t.constraint.date = d;
            if d.is_some() && !t.constraint.kind.needs_date() {
                t.constraint.kind = ConstraintKind::Snet;
            }
            if d.is_none() {
                t.constraint.kind = ConstraintKind::Asap;
            }
        }
        Deadline => p.tasks[idx].deadline = parse_date(p, text, true)?,
        Priority => p.tasks[idx].priority = text.trim().parse::<u16>().map_err(|_| "priority must be 0-1000")?.min(1000),
        TaskType => {
            p.tasks[idx].task_type = match text {
                "Fixed Duration" => plancraft_core::model::TaskType::FixedDuration,
                "Fixed Work" => plancraft_core::model::TaskType::FixedWork,
                _ => plancraft_core::model::TaskType::FixedUnits,
            }
        }
        EstimateMethod => {
            let t = &mut p.tasks[idx];
            let work_guess = t.sched.work.max(t.duration);
            t.estimate = match text {
                "Complexity" => Some(Estimate::Complexity { points: 3.0 }),
                "Three-point" => {
                    Some(Estimate::ThreePoint { optimistic: work_guess * 3 / 4, most_likely: work_guess, pessimistic: work_guess * 3 / 2 })
                }
                "Work" => Some(Estimate::Work { minutes: work_guess }),
                _ => None,
            };
        }
        Points => {
            let v: f64 = text.trim().parse().map_err(|_| "points must be a number")?;
            p.tasks[idx].estimate = Some(Estimate::Complexity { points: v.max(0.0) });
        }
        Optimistic | MostLikely | Pessimistic => {
            let w = parse_work_text(p, text)?;
            let t = &mut p.tasks[idx];
            let (mut o, mut m, mut pe) = match t.estimate {
                Some(Estimate::ThreePoint { optimistic, most_likely, pessimistic }) => (optimistic, most_likely, pessimistic),
                _ => (w, w, w),
            };
            match col {
                Optimistic => o = w,
                MostLikely => m = w,
                _ => pe = w,
            }
            t.estimate = Some(Estimate::ThreePoint { optimistic: o, most_likely: m, pessimistic: pe });
        }
        Deliverable => p.tasks[idx].deliverable = text.to_string(),
        Notes => p.tasks[idx].notes = text.to_string(),
        _ => return Err("this field is calculated and cannot be edited".into()),
    }
    Ok(())
}

/// Late-task check used by filters and indicators.
pub fn is_late(p: &Project, t: &Task) -> bool {
    report::is_late(t, report::status_moment(p), p)
}
