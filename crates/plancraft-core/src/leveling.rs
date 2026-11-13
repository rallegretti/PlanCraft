//! Resource leveling.
//!
//! A serial heuristic: tasks are visited in order of early start, then
//! priority (high first), then total slack (low first).  Each task is kept
//! where it is when all of its resources have capacity for it on every day;
//! otherwise it is delayed one working day at a time (recorded as
//! `Task::leveling_delay`) until it fits.

use std::collections::{BTreeMap, HashMap, HashSet};

use chrono::{NaiveDate, NaiveDateTime};
use serde::{Deserialize, Serialize};

use crate::calendar::WorkCal;
use crate::duration::Minutes;
use crate::ids::*;
use crate::model::*;
use crate::schedule::{recalculate, task_calendar};
use crate::workload::{capacity_on, distribute_spans, resource_calendar};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct LevelingOptions {
    /// Remove existing leveling delays before leveling.
    pub clear_existing: bool,
    /// Maximum delay tried per task, in working days.
    pub max_delay_days: u32,
    /// Only level the given resources (empty = all).
    pub resources: Vec<ResourceId>,
}

impl Default for LevelingOptions {
    fn default() -> Self {
        Self { clear_existing: true, max_delay_days: 260, resources: Vec::new() }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct LevelingReport {
    /// Tasks that received a leveling delay and the delay in working minutes.
    pub delayed: Vec<(TaskId, Minutes)>,
    /// Tasks that could not be resolved (e.g. assigned beyond a resource's maximum units).
    pub unresolved: Vec<String>,
    pub finish_before: Option<chrono::NaiveDateTime>,
    pub finish_after: Option<chrono::NaiveDateTime>,
}

/// Removes all leveling delays.
pub fn clear_leveling(p: &mut Project) {
    for t in &mut p.tasks {
        t.leveling_delay = 0;
    }
    recalculate(p);
}

type Load = HashMap<ResourceId, BTreeMap<NaiveDate, f64>>;

/// What a task's load depends on during leveling, looked up once per task.
struct TaskPlan<'a> {
    cal: WorkCal<'a>,
    start: Option<NaiveDateTime>,
    duration: Minutes,
    /// Working parts of a split task as scheduled.
    parts: Vec<(NaiveDateTime, NaiveDateTime)>,
    /// Splits planned in the task: they move with it when it is delayed.
    gaps: Vec<Gap>,
    /// (resource, work, contour) of its work assignments within the leveling filter.
    assignments: Vec<(ResourceId, f64, WorkContour)>,
}

impl<'a> TaskPlan<'a> {
    fn new(p: &'a Project, idx: usize, filter: &HashSet<ResourceId>) -> Option<Self> {
        let t = p.tasks.get(idx)?;
        let has_dates = t.sched.start.is_some() && t.sched.finish.is_some();
        let assignments = p
            .assignments_for_task(t.id)
            .filter(|a| filter.is_empty() || filter.contains(&a.resource))
            .filter(|a| p.resource(a.resource).is_some_and(|r| r.kind == ResourceType::Work))
            .map(|a| (a.resource, a.work as f64, a.contour))
            .collect();
        Some(Self {
            cal: task_calendar(p, idx),
            start: if has_dates { t.sched.start } else { None },
            duration: t.duration,
            parts: t.sched.parts.clone(),
            gaps: crate::schedule::planned_gaps(&t.gaps, t.duration),
            assignments,
        })
    }

    /// The task's load per resource and day when delayed by `delay`, and the start it then has.
    fn load(&self, delay: Minutes) -> (Load, Option<NaiveDateTime>) {
        let mut out: Load = HashMap::new();
        let Some(s) = self.start else {
            return (out, None);
        };
        let start = if delay > 0 { self.cal.snap_forward(self.cal.add(s, delay)) } else { s };
        let moved = self.spans_from(start);
        let spans: &[(NaiveDateTime, NaiveDateTime)] = if delay == 0 && !self.parts.is_empty() { &self.parts } else { &moved };
        for (rid, work, contour) in &self.assignments {
            let e = out.entry(*rid).or_default();
            for (d, w) in distribute_spans(&self.cal, spans, *work, *contour) {
                *e.entry(d).or_default() += w;
            }
        }
        (out, Some(start))
    }

    /// The working parts of the task when it starts at `start`: its planned splits move with it.
    fn spans_from(&self, start: NaiveDateTime) -> Vec<(NaiveDateTime, NaiveDateTime)> {
        crate::schedule::segments_of(start, &[], &self.gaps, &self.cal, self.duration).into_iter().map(|(s, f, _)| (s, f)).collect()
    }

    fn finish_after(&self, delay: Minutes) -> Option<NaiveDateTime> {
        let s = self.start?;
        let start = if delay > 0 { self.cal.snap_forward(self.cal.add(s, delay)) } else { s };
        self.spans_from(start).last().map(|&(_, f)| f)
    }
}

fn fits(p: &Project, committed: &Load, candidate: &Load, caps: &mut HashMap<(ResourceId, NaiveDate), f64>) -> bool {
    for (rid, days) in candidate {
        let Some(r) = p.resource(*rid) else { continue };
        let cal = resource_calendar(p, r);
        for (d, w) in days {
            let cap = *caps.entry((*rid, *d)).or_insert_with(|| capacity_on(&cal, r, *d));
            let used = committed.get(rid).and_then(|m| m.get(d)).copied().unwrap_or(0.0);
            if used + w > cap + 0.5 {
                return false;
            }
        }
    }
    true
}

fn add_load(committed: &mut Load, add: &Load, sign: f64) {
    for (rid, days) in add {
        let e = committed.entry(*rid).or_default();
        for (d, w) in days {
            *e.entry(*d).or_default() += sign * w;
        }
    }
}

/// Tasks whose dates can move other tasks: a predecessor (or milestone member) of anything,
/// directly or through a summary above it.
fn has_dependents(p: &Project) -> Vec<bool> {
    let mut referenced: HashSet<TaskId> = HashSet::new();
    for t in &p.tasks {
        referenced.extend(t.predecessors.iter().map(|d| d.predecessor).chain(t.members.iter().copied()).filter_map(|r| r.local()));
    }
    // (outline level, this row or one above it is referenced)
    let mut stack: Vec<(u16, bool)> = Vec::new();
    p.tasks
        .iter()
        .map(|t| {
            while stack.last().is_some_and(|&(lvl, _)| lvl >= t.outline_level) {
                stack.pop();
            }
            let v = stack.last().is_some_and(|&(_, r)| r) || referenced.contains(&t.id);
            stack.push((t.outline_level, v));
            v
        })
        .collect()
}

/// Levels resources by delaying tasks. Recalculates the schedule.
///
/// Tasks are taken one at a time by (start, priority high first, total slack low first, id). Each
/// keeps its place when its resources have room on every day; otherwise it is delayed a working
/// day at a time until it fits. The load of placed tasks is kept up to date incrementally, and the
/// schedule is recalculated only when a delay can move other tasks (the task has dependents, or
/// its new finish moves the project finish), so leveling stays fast on large plans.
pub fn level_resources(p: &mut Project, opts: &LevelingOptions) -> LevelingReport {
    if opts.clear_existing {
        for t in &mut p.tasks {
            t.leveling_delay = 0;
        }
    }
    recalculate(p);
    let mut report = LevelingReport { finish_before: p.sched.finish, ..Default::default() };
    let filter: HashSet<ResourceId> = opts.resources.iter().copied().collect();
    let day = p.settings.time.minutes_per_day.max(1);
    let dependents = has_dependents(p);

    // Occurrences of recurring tasks come first and are never delayed (Microsoft gives recurring
    // tasks the highest leveling priority).
    let movable = |t: &Task| -> bool {
        !t.occurrence
            && t.active
            && !t.sched.is_summary
            && t.mode == ScheduleMode::Auto
            && t.actual_start.is_none()
            && !matches!(t.constraint.kind, ConstraintKind::Mso | ConstraintKind::Mfo)
    };

    // Load currently committed, and each committed task's share with the start it was taken at.
    let mut committed: Load = HashMap::new();
    let mut shares: HashMap<usize, (Load, Option<NaiveDateTime>)> = HashMap::new();
    // Fixed tasks (started, manual, hard constraints) are committed first.
    for (i, t) in p.tasks.iter().enumerate() {
        if !t.sched.is_summary
            && t.active
            && !movable(t)
            && let Some(plan) = TaskPlan::new(p, i, &filter)
        {
            let share = plan.load(0);
            add_load(&mut committed, &share.0, 1.0);
            shares.insert(i, share);
        }
    }

    let mut order: Vec<usize> =
        p.tasks.iter().enumerate().filter(|(_, t)| movable(t) && p.assignments_for_task(t.id).next().is_some()).map(|(i, _)| i).collect();
    let mut caps = HashMap::new();
    let mut delays: HashMap<TaskId, Minutes> = HashMap::new();
    // Set when committed shares may be out of date with the schedule (after a recalculation).
    let mut recalculated = false;
    let mut pending_recalc = false;

    while !order.is_empty() {
        if recalculated {
            // Earlier delays moved tasks: refresh the shares of committed tasks that moved.
            let moved: Vec<usize> =
                shares.iter().filter(|(i, (_, at))| p.tasks.get(**i).map(|t| t.sched.start) != Some(*at)).map(|(i, _)| *i).collect();
            for i in moved {
                if let (Some(plan), Some((old, _))) = (TaskPlan::new(p, i, &filter), shares.remove(&i)) {
                    add_load(&mut committed, &old, -1.0);
                    let share = plan.load(0);
                    add_load(&mut committed, &share.0, 1.0);
                    shares.insert(i, share);
                }
            }
            recalculated = false;
        }
        // Next task: earliest start, then highest priority, then least slack, then id.
        let key = |i: usize| {
            let t = p.tasks.get(i);
            (
                t.and_then(|t| t.sched.start),
                std::cmp::Reverse(t.map_or(0, |t| t.priority)),
                t.map_or(0, |t| t.sched.total_slack),
                t.map(|t| t.id),
            )
        };
        let Some(pos) = order.iter().enumerate().min_by_key(|(_, i)| key(**i)).map(|(pos, _)| pos) else { break };
        let idx = order.swap_remove(pos);
        let Some((id, name)) = p.tasks.get(idx).map(|t| (t.id, t.name.clone())) else { continue };
        let Some(plan) = TaskPlan::new(p, idx, &filter) else { continue };

        let (alone, _) = plan.load(0);
        if !fits(p, &HashMap::new(), &alone, &mut caps) {
            report.unresolved.push(format!("'{}' needs more units than its resources can provide", name));
            let share = plan.load(0);
            add_load(&mut committed, &share.0, 1.0);
            shares.insert(idx, share);
            continue;
        }
        let mut delay = 0;
        let mut placed = None;
        for _ in 0..=opts.max_delay_days {
            let cand = plan.load(delay);
            if fits(p, &committed, &cand.0, &mut caps) {
                placed = Some(cand);
                break;
            }
            delay += day;
        }
        let share = match placed {
            Some(share) => share,
            None => {
                report.unresolved.push(format!("'{}' could not be leveled within {} days", name, opts.max_delay_days));
                delay = 0;
                plan.load(0)
            }
        };
        let moves_others = delay > 0
            && (dependents.get(idx).copied().unwrap_or(true) || plan.finish_after(delay).zip(p.sched.finish).is_none_or(|(f, pf)| f > pf));
        drop(plan);
        add_load(&mut committed, &share.0, 1.0);
        shares.insert(idx, share);
        if delay > 0 {
            if let Some(t) = p.tasks.get_mut(idx) {
                t.leveling_delay = crate::limits::clamp_minutes(t.leveling_delay.saturating_add(delay));
            }
            *delays.entry(id).or_default() += delay;
            if moves_others {
                recalculate(p);
                recalculated = true;
                pending_recalc = false;
            } else {
                // Only this task moved (and its summaries); its share already has its new dates.
                pending_recalc = true;
            }
        }
    }
    if pending_recalc {
        recalculate(p);
    }

    let position: HashMap<TaskId, usize> = p.tasks.iter().enumerate().map(|(i, t)| (t.id, i)).collect();
    report.delayed = delays.into_iter().collect();
    report.delayed.sort_by_key(|(id, _)| position.get(id).copied());
    report.finish_after = p.sched.finish;
    report
}
