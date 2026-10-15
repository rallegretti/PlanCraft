//! Resource leveling.
//!
//! A serial heuristic: tasks are visited in order of early start, then
//! priority (high first), then total slack (low first).  Each task is kept
//! where it is when all of its resources have capacity for it on every day;
//! otherwise it is delayed one working day at a time (recorded as
//! `Task::leveling_delay`) until it fits.

use std::collections::{BTreeMap, HashMap, HashSet};

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

use crate::duration::Minutes;
use crate::ids::*;
use crate::model::*;
use crate::schedule::{recalculate, task_calendar};
use crate::workload::{capacity_on, distribute, resource_calendar};

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
        Self {
            clear_existing: true,
            max_delay_days: 260,
            resources: Vec::new(),
        }
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

fn task_loads(p: &Project, idx: usize, delay: Minutes, filter: &HashSet<ResourceId>) -> Load {
    let t = &p.tasks[idx];
    let mut out: Load = HashMap::new();
    let (Some(s), Some(_)) = (t.sched.start, t.sched.finish) else {
        return out;
    };
    let cal = task_calendar(p, idx);
    let start = if delay > 0 {
        cal.snap_forward(cal.add(s, delay))
    } else {
        s
    };
    let finish = cal.add(start, t.duration);
    for a in p.assignments_for_task(t.id) {
        if !filter.is_empty() && !filter.contains(&a.resource) {
            continue;
        }
        if p.resource(a.resource)
            .is_none_or(|r| r.kind != ResourceType::Work)
        {
            continue;
        }
        let e = out.entry(a.resource).or_default();
        for (d, w) in distribute(&cal, start, finish, a.work as f64, a.contour) {
            *e.entry(d).or_default() += w;
        }
    }
    out
}

fn fits(
    p: &Project,
    committed: &Load,
    candidate: &Load,
    caps: &mut HashMap<(ResourceId, NaiveDate), f64>,
) -> bool {
    for (rid, days) in candidate {
        let Some(r) = p.resource(*rid) else { continue };
        let cal = resource_calendar(p, r);
        for (d, w) in days {
            let cap = *caps
                .entry((*rid, *d))
                .or_insert_with(|| capacity_on(&cal, r, *d));
            let used = committed
                .get(rid)
                .and_then(|m| m.get(d))
                .copied()
                .unwrap_or(0.0);
            if used + w > cap + 0.5 {
                return false;
            }
        }
    }
    true
}

fn commit(committed: &mut Load, add: Load) {
    for (rid, days) in add {
        let e = committed.entry(rid).or_default();
        for (d, w) in days {
            *e.entry(d).or_default() += w;
        }
    }
}

/// Levels resources by delaying tasks. Recalculates the schedule.
pub fn level_resources(p: &mut Project, opts: &LevelingOptions) -> LevelingReport {
    if opts.clear_existing {
        for t in &mut p.tasks {
            t.leveling_delay = 0;
        }
    }
    recalculate(p);
    let mut report = LevelingReport {
        finish_before: p.sched.finish,
        ..Default::default()
    };
    let filter: HashSet<ResourceId> = opts.resources.iter().copied().collect();
    let day = p.settings.time.minutes_per_day.max(1);

    let movable = |t: &Task| -> bool {
        t.active
            && !t.sched.is_summary
            && t.mode == ScheduleMode::Auto
            && t.actual_start.is_none()
            && !matches!(t.constraint.kind, ConstraintKind::Mso | ConstraintKind::Mfo)
    };

    // Fixed tasks (started, manual, hard constraints) are committed first.
    let mut committed: Load = HashMap::new();
    let mut fixed: Vec<TaskId> = Vec::new();
    for (i, t) in p.tasks.iter().enumerate() {
        if !t.sched.is_summary && t.active && !movable(t) {
            commit(&mut committed, task_loads(p, i, 0, &filter));
            fixed.push(t.id);
        }
    }

    let mut order: Vec<TaskId> = p
        .tasks
        .iter()
        .filter(|t| movable(t) && p.assignments_for_task(t.id).next().is_some())
        .map(|t| t.id)
        .collect();
    let mut placed: Vec<TaskId> = Vec::new();
    let mut caps = HashMap::new();
    let mut delays: HashMap<TaskId, Minutes> = HashMap::new();

    while !order.is_empty() {
        // Re-sort each round: earlier delays may have moved successors.
        order.sort_by(|a, b| {
            let ta = p.task(*a).unwrap();
            let tb = p.task(*b).unwrap();
            ta.sched
                .start
                .cmp(&tb.sched.start)
                .then(tb.priority.cmp(&ta.priority))
                .then(ta.sched.total_slack.cmp(&tb.sched.total_slack))
                .then(ta.id.cmp(&tb.id))
        });
        let id = order.remove(0);
        let idx = p.task_index(id).unwrap();

        // Rebuild committed load from fixed + already placed tasks at their current dates.
        let mut current = HashMap::new();
        for tid in fixed.iter().chain(placed.iter()) {
            if let Some(i) = p.task_index(*tid) {
                commit(&mut current, task_loads(p, i, 0, &filter));
            }
        }
        committed = current;

        let alone = task_loads(p, idx, 0, &filter);
        if !fits(p, &HashMap::new(), &alone, &mut caps) {
            report.unresolved.push(format!(
                "'{}' needs more units than its resources can provide",
                p.tasks[idx].name
            ));
            placed.push(id);
            continue;
        }
        let mut delay = 0;
        let mut found = false;
        for _ in 0..=opts.max_delay_days {
            let cand = task_loads(p, idx, delay, &filter);
            if fits(p, &committed, &cand, &mut caps) {
                found = true;
                break;
            }
            delay += day;
        }
        if !found {
            report.unresolved.push(format!(
                "'{}' could not be leveled within {} days",
                p.tasks[idx].name, opts.max_delay_days
            ));
            delay = 0;
        }
        if delay > 0 {
            p.tasks[idx].leveling_delay += delay;
            *delays.entry(id).or_default() += delay;
            recalculate(p);
        }
        placed.push(id);
    }

    report.delayed = delays.into_iter().collect();
    report.delayed.sort_by_key(|(id, _)| p.task_index(*id));
    report.finish_after = p.sched.finish;
    report
}
