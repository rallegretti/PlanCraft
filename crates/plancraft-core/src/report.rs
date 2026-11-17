//! Project statistics, variance and earned-value metrics.

use crate::dates::prelude::*;
use serde::{Deserialize, Serialize};

use crate::calendar::WorkCal;
use crate::duration::Minutes;
use crate::ids::TaskId;
use crate::model::*;
use crate::schedule::project_calendar;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct MilestoneStatus {
    pub id: TaskId,
    pub name: String,
    pub deliverable: String,
    pub date: Option<DateTime>,
    pub deadline: Option<DateTime>,
    pub baseline: Option<DateTime>,
    pub members: usize,
    pub members_complete: usize,
    /// Work-weighted completion of member tasks (0–100).
    pub progress: f32,
    pub late: bool,
    pub complete: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
pub struct EarnedValue {
    /// Budgeted cost of work scheduled (planned value) at the status date.
    pub bcws: f64,
    /// Budgeted cost of work performed (earned value).
    pub bcwp: f64,
    /// Actual cost of work performed.
    pub acwp: f64,
    pub sv: f64,
    pub cv: f64,
    pub spi: f64,
    pub cpi: f64,
    /// The baseline (0–10) the values are measured against.
    pub baseline: u8,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
pub struct ProjectStats {
    pub name: String,
    pub start: Option<DateTime>,
    pub finish: Option<DateTime>,
    pub baseline_finish: Option<DateTime>,
    /// Working minutes the finish has slipped against baseline (negative = ahead).
    pub finish_variance: Minutes,
    pub duration: Minutes,
    pub work: Minutes,
    pub actual_work: Minutes,
    pub remaining_work: Minutes,
    pub cost: f64,
    pub baseline_cost: f64,
    pub percent_complete: f32,
    pub tasks: usize,
    pub summary_tasks: usize,
    pub milestones: usize,
    pub completed_tasks: usize,
    pub in_progress_tasks: usize,
    pub late_tasks: usize,
    pub critical_tasks: usize,
    pub resources: usize,
    pub overallocated_resources: usize,
    pub warnings: Vec<String>,
    pub milestone_status: Vec<MilestoneStatus>,
    pub earned_value: Option<EarnedValue>,
}

/// Status date used for "late" checks: explicit status date, else now.
pub fn status_moment(p: &Project) -> DateTime {
    let wc = project_calendar(p);
    match p.status_date {
        Some(d) => wc.day_finish(d),
        None => crate::dates::now(),
    }
}

/// Fraction of the baseline (or current) span that should be complete at `at`, in the project
/// calendar `wc`.
fn planned_fraction(start: DateTime, finish: DateTime, at: DateTime, wc: &WorkCal, elapsed: bool) -> f64 {
    if at <= start {
        return 0.0;
    }
    if at >= finish {
        return 1.0;
    }
    if elapsed {
        // Elapsed time runs around the clock.
        let total = finish.signed_duration_since(start).num_minutes();
        return if total <= 0 { 1.0 } else { at.signed_duration_since(start).num_minutes() as f64 / total as f64 };
    }
    let total = wc.between(start, finish);
    if total <= 0 { 1.0 } else { wc.between(start, at) as f64 / total as f64 }
}

/// Whether a task is behind schedule at `at` (should have progressed further).
pub fn is_late(t: &Task, at: DateTime, p: &Project) -> bool {
    is_late_in(t, at, &project_calendar(p))
}

/// [`is_late`] with the project calendar already made: checking many tasks with one calendar
/// fills its month-and-year cache once.
pub fn is_late_in(t: &Task, at: DateTime, wc: &WorkCal) -> bool {
    if t.sched.is_summary || t.is_complete() || !t.active {
        return false;
    }
    let (Some(s), Some(f)) = (t.sched.start, t.sched.finish) else {
        return false;
    };
    if let Some(dl) = t.deadline
        && f > dl
    {
        return true;
    }
    let expected = planned_fraction(s, f, at, wc, t.elapsed) * 100.0;
    (t.sched.percent_complete as f64) + 0.5 < expected
}

pub fn milestone_status(p: &Project, at: DateTime) -> Vec<MilestoneStatus> {
    p.tasks
        .iter()
        .filter(|t| t.is_milestone())
        .map(|m| {
            let mut members = 0;
            let mut done = 0;
            let (mut num, mut den) = (0.0f64, 0.0f64);
            for r in &m.members {
                let (pct, w) = match r {
                    TaskRef::Local(id) => match p.task(*id) {
                        Some(t) => (t.sched.percent_complete as f64, t.sched.work.max(t.sched.duration).max(1) as f64),
                        None => continue,
                    },
                    TaskRef::External { project, task } => match p.external(*project).and_then(|e| e.task(*task)) {
                        Some(s) => (s.percent_complete as f64, s.duration.max(1) as f64),
                        None => continue,
                    },
                };
                members += 1;
                if pct >= 100.0 {
                    done += 1;
                }
                num += pct * w;
                den += w;
            }
            let progress = if den > 0.0 { (num / den) as f32 } else { m.sched.percent_complete };
            let complete = m.is_complete() || (members > 0 && done == members);
            let late = !complete && (m.deadline.zip(m.sched.finish).is_some_and(|(d, f)| f > d) || m.sched.finish.is_some_and(|f| f < at));
            MilestoneStatus {
                id: m.id,
                name: m.name.clone(),
                deliverable: m.deliverable.clone(),
                date: m.sched.finish,
                deadline: m.deadline,
                baseline: m.baseline.map(|b| b.finish),
                members,
                members_complete: done,
                progress,
                late,
                complete,
            }
        })
        .collect()
}

/// Share (0–1) of a task's completed part that lies before `at`: the completed part runs from the
/// task's start to its stop (the end of the completed work), or to its finish once it is
/// complete, leaving out the pauses between its parts.
fn done_share_by(t: &Task, at: DateTime, wc: &WorkCal) -> f64 {
    let Some(start) = t.sched.start else { return 1.0 };
    let end = match t.sched.stop {
        Some(stop) => stop,
        None if t.sched.percent_complete >= 100.0 => t.sched.finish.unwrap_or(start),
        // No completed part laid out (no progress): whatever was done counts from the start.
        None => return if at >= start { 1.0 } else { 0.0 },
    };
    let len = |a: DateTime, b: DateTime| -> f64 {
        if b <= a {
            0.0
        } else if t.elapsed {
            b.signed_duration_since(a).num_minutes() as f64
        } else {
            wc.between(a, b) as f64
        }
    };
    let whole = [(start, t.sched.finish.unwrap_or(end))];
    let parts: &[(DateTime, DateTime)] = if t.sched.parts.is_empty() { &whole } else { &t.sched.parts };
    let (mut total, mut before) = (0.0, 0.0);
    for &(a, b) in parts {
        total += len(a, b.min(end));
        before += len(a, b.min(end).min(at));
    }
    if total <= 0.0 {
        return if at >= end { 1.0 } else { 0.0 };
    }
    (before / total).clamp(0.0, 1.0)
}

pub fn earned_value(p: &Project, at: DateTime) -> Option<EarnedValue> {
    let leaves = p.tasks.iter().filter(|t| !t.sched.is_summary && t.active);
    let mut ev = EarnedValue::default();
    let mut any_baseline = false;
    let slot = p.settings.earned_value_baseline;
    let wc = project_calendar(p);
    // Tracked time per task: (all of it, the part logged up to the status date).
    let mut logged: std::collections::HashMap<TaskId, (Minutes, Minutes)> = std::collections::HashMap::new();
    for e in &p.time_entries {
        let slot = logged.entry(e.task).or_default();
        slot.0 = slot.0.saturating_add(e.minutes);
        if e.date <= at.date() {
            slot.1 = slot.1.saturating_add(e.minutes);
        }
    }
    for t in leaves {
        let Some(b) = t.baseline_in(slot) else { continue };
        any_baseline = true;
        ev.bcws += b.cost * planned_fraction(b.start, b.finish, at, &wc, t.elapsed);
        ev.bcwp += b.cost * t.sched.percent_complete as f64 / 100.0;
        // Actual cost (ACWP): the cost of the work done up to the status date. Tracked time counts
        // on the day it was logged; other actual work, and progress entered as % complete (as for
        // tasks with no resources), count as they were laid out from the task's start.
        let share = done_share_by(t, at, &wc);
        let actual_ratio = if t.sched.work > 0 && t.sched.actual_work > 0 {
            let (all_logged, logged_by) = logged.get(&t.id).copied().unwrap_or_default();
            let other = t.sched.actual_work.saturating_sub(all_logged).max(0);
            (logged_by as f64 + other as f64 * share) / t.sched.work as f64
        } else {
            t.sched.percent_complete as f64 / 100.0 * share
        };
        ev.acwp += t.sched.cost * actual_ratio;
    }
    if !any_baseline {
        return None;
    }
    ev.baseline = slot;
    ev.sv = ev.bcwp - ev.bcws;
    ev.cv = ev.bcwp - ev.acwp;
    ev.spi = if ev.bcws > 0.0 { ev.bcwp / ev.bcws } else { 1.0 };
    ev.cpi = if ev.acwp > 0.0 { ev.bcwp / ev.acwp } else { 1.0 };
    Some(ev)
}

/// Computes project statistics (assumes the schedule is up to date).
pub fn project_stats(p: &Project) -> ProjectStats {
    let at = status_moment(p);
    let wc = project_calendar(p);
    let leaves: Vec<&Task> = p.tasks.iter().filter(|t| !t.sched.is_summary).collect();
    let baseline_finish = p.tasks.iter().filter_map(|t| t.baseline.map(|b| b.finish)).max();
    let baseline_cost = crate::baseline::baseline_totals(p, 0).0;
    let over = crate::workload::overallocations(p).len();
    ProjectStats {
        name: p.name.clone(),
        start: p.sched.start,
        finish: p.sched.finish,
        baseline_finish,
        finish_variance: match (baseline_finish, p.sched.finish) {
            (Some(b), Some(f)) => wc.between(b, f),
            _ => 0,
        },
        duration: match (p.sched.start, p.sched.finish) {
            (Some(s), Some(f)) => wc.between(s, f),
            _ => 0,
        },
        work: p.sched.work,
        actual_work: p.sched.actual_work,
        remaining_work: (p.sched.work - p.sched.actual_work).max(0),
        cost: p.sched.cost,
        baseline_cost,
        percent_complete: p.sched.percent_complete,
        tasks: leaves.iter().filter(|t| !t.is_milestone()).count(),
        summary_tasks: p.tasks.len() - leaves.len(),
        milestones: leaves.iter().filter(|t| t.is_milestone()).count(),
        completed_tasks: leaves.iter().filter(|t| t.is_complete()).count(),
        in_progress_tasks: leaves.iter().filter(|t| t.percent_complete > 0 && !t.is_complete()).count(),
        late_tasks: leaves.iter().filter(|t| is_late_in(t, at, &wc)).count(),
        critical_tasks: leaves.iter().filter(|t| t.sched.critical).count(),
        resources: p.resources.len(),
        overallocated_resources: over,
        warnings: p.sched.warnings.clone(),
        milestone_status: milestone_status(p, at),
        earned_value: earned_value(p, at),
    }
}
