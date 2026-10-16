//! Critical-path scheduling engine.
//!
//! [`recalculate`] runs a calendar-aware forward and backward pass over the
//! leaf tasks of a project, honoring all four link types with lags/leads,
//! constraints, deadlines, actual progress, leveling delays, manual tasks,
//! milestone membership and cross-document (external) predecessors.  Results
//! are written into [`TaskSchedule`] and rolled up into summary tasks and the
//! project.
//!
//! Links attached to summary tasks apply to all of the summary's children,
//! like in Microsoft Project.
//!
//! The engine never indexes out of bounds and clamps date arithmetic; as a last resort
//! [`recalculate`] runs behind [`crate::guard`], so a bug produces a warning instead of a crash.

use std::collections::HashMap;

use chrono::NaiveDateTime;

use crate::calendar::{CalLayer, WorkCal, shift};
use crate::duration::{Lag, Minutes};
use crate::guard::{self, Panicked};
use crate::ids::{ResourceId, TaskId};
use crate::model::*;

/// Where a link's predecessor dates come from.
#[derive(Clone, Debug)]
enum PredSource {
    /// A leaf task index.
    Leaf(usize),
    /// A summary used as SS/SF predecessor: start = min of leaves' starts.
    SummaryStart(Vec<usize>),
    /// Fixed dates of an external task.
    Fixed { start: NaiveDateTime, finish: NaiveDateTime, duration: Minutes },
}

#[derive(Clone, Debug)]
struct Edge {
    pred: PredSource,
    succ: usize,
    kind: LinkType,
    lag: Lag,
}

#[derive(Clone, Copy, Debug, Default)]
struct Dates {
    es: Option<NaiveDateTime>,
    ef: Option<NaiveDateTime>,
    ls: Option<NaiveDateTime>,
    lf: Option<NaiveDateTime>,
}

/// Summary of a recalculation.
#[derive(Clone, Debug, Default)]
pub struct ScheduleReport {
    pub warnings: Vec<String>,
    pub start: Option<NaiveDateTime>,
    pub finish: Option<NaiveDateTime>,
    pub critical_tasks: Vec<TaskId>,
}

/// Builds the effective working calendar of the task at `idx`.
///
/// Assigned work resources contribute their own calendars (base calendar plus
/// personal exceptions); the task is scheduled in time common to all of them.
pub fn task_calendar(p: &Project, idx: usize) -> WorkCal<'_> {
    let Some(task) = p.tasks.get(idx) else {
        return project_calendar(p);
    };
    let resources: Vec<&Resource> = p.assignments_for_task(task.id).filter_map(|a| p.resource(a.resource)).collect();
    calendar_for(p, task, &resources)
}

/// [`task_calendar`] with the task's assigned resources already looked up.
fn calendar_for<'a>(p: &'a Project, task: &Task, resources: &[&'a Resource]) -> WorkCal<'a> {
    let base = task.calendar.and_then(|c| p.calendar(c)).unwrap_or_else(|| p.base_calendar());
    let mut layers = Vec::new();
    if task.calendar.is_some() {
        layers.push(CalLayer::new(base, &[]));
    }
    if p.settings.use_resource_calendars && !task.ignore_resource_calendars {
        for r in resources.iter().filter(|r| r.kind == ResourceType::Work) {
            let cal = r.calendar.and_then(|c| p.calendar(c)).unwrap_or_else(|| p.base_calendar());
            layers.push(CalLayer::new(cal, &r.exceptions));
        }
    }
    if layers.is_empty() {
        return WorkCal::single(base);
    }
    let wc = WorkCal::from_layers(layers);
    // Fall back to the task/project calendar when resources share no working time at all.
    let any = p.start.date().iter_days().take(28).any(|d| wc.is_working_day(d));
    if any { wc } else { WorkCal::single(base) }
}

/// The project calendar used for lags and summary durations.
pub fn project_calendar(p: &Project) -> WorkCal<'_> {
    WorkCal::single(p.base_calendar())
}

fn percent_of(duration: Minutes, pct: i32) -> Minutes {
    duration.saturating_mul(Minutes::from(pct)) / 100
}

fn apply_lag(cal: &WorkCal, base: NaiveDateTime, lag: Lag, pred_duration: Minutes) -> NaiveDateTime {
    match lag {
        Lag::None | Lag::Working(0) => base,
        Lag::Working(m) => cal.add(base, m),
        Lag::Elapsed(m) => shift(base, m),
        Lag::Percent(pct) => match percent_of(pred_duration, pct) {
            0 => base,
            m => cal.add(base, m),
        },
    }
}

fn unapply_lag(cal: &WorkCal, base: NaiveDateTime, lag: Lag, pred_duration: Minutes) -> NaiveDateTime {
    match lag {
        Lag::None | Lag::Working(0) => base,
        Lag::Working(m) => cal.sub(base, m),
        Lag::Elapsed(m) => shift(base, m.saturating_neg()),
        Lag::Percent(pct) => match percent_of(pred_duration, pct) {
            0 => base,
            m => cal.sub(base, m),
        },
    }
}

/// Index of each task's parent summary, computed in one pass.
fn parents(p: &Project) -> Vec<Option<usize>> {
    let mut stack: Vec<(usize, u16)> = Vec::new();
    p.tasks
        .iter()
        .enumerate()
        .map(|(i, t)| {
            while stack.last().is_some_and(|&(_, lvl)| lvl >= t.outline_level) {
                stack.pop();
            }
            let parent = stack.last().map(|&(pi, _)| pi);
            stack.push((i, t.outline_level));
            parent
        })
        .collect()
}

struct Engine<'a> {
    p: &'a Project,
    proj_cal: WorkCal<'a>,
    cals: Vec<WorkCal<'a>>,
    leaves: Vec<usize>,
    is_leaf: Vec<bool>,
    parent: Vec<Option<usize>>,
    position: HashMap<TaskId, usize>,
    edges: Vec<Edge>,
    /// Edges indexed by successor.
    incoming: Vec<Vec<usize>>,
    /// Edges indexed by predecessor leaf (only `Leaf` sources; summary starts are expanded).
    outgoing: Vec<Vec<usize>>,
    order: Vec<usize>,
    warnings: Vec<String>,
    dates: Vec<Dates>,
}

impl<'a> Engine<'a> {
    fn new(p: &'a Project) -> Self {
        let n = p.tasks.len();
        let is_leaf: Vec<bool> = (0..n).map(|i| !p.has_children(i)).collect();
        let leaves: Vec<usize> = is_leaf.iter().enumerate().filter(|(_, leaf)| **leaf).map(|(i, _)| i).collect();
        let resources: HashMap<ResourceId, &Resource> = p.resources.iter().map(|r| (r.id, r)).collect();
        let mut assigned: HashMap<TaskId, Vec<&Resource>> = HashMap::new();
        for a in &p.assignments {
            if let Some(r) = resources.get(&a.resource) {
                assigned.entry(a.task).or_default().push(r);
            }
        }
        let cals = p.tasks.iter().map(|t| calendar_for(p, t, assigned.get(&t.id).map(Vec::as_slice).unwrap_or_default())).collect();
        Self {
            p,
            proj_cal: project_calendar(p),
            cals,
            leaves,
            is_leaf,
            parent: parents(p),
            position: p.tasks.iter().enumerate().map(|(i, t)| (t.id, i)).collect(),
            edges: Vec::new(),
            incoming: vec![Vec::new(); n],
            outgoing: vec![Vec::new(); n],
            order: Vec::new(),
            warnings: Vec::new(),
            dates: vec![Dates::default(); n],
        }
    }

    fn task(&self, i: usize) -> Option<&'a Task> {
        self.p.tasks.get(i)
    }

    fn leaf(&self, i: usize) -> bool {
        self.is_leaf.get(i).copied().unwrap_or(false)
    }

    fn cal(&self, i: usize) -> &WorkCal<'a> {
        self.cals.get(i).unwrap_or(&self.proj_cal)
    }

    fn date(&self, i: usize) -> Dates {
        self.dates.get(i).copied().unwrap_or_default()
    }

    fn set_early(&mut self, i: usize, es: NaiveDateTime, ef: NaiveDateTime) {
        if let Some(d) = self.dates.get_mut(i) {
            d.es = Some(es);
            d.ef = Some(ef);
        }
    }

    fn set_late(&mut self, i: usize, ls: NaiveDateTime, lf: NaiveDateTime) {
        if let Some(d) = self.dates.get_mut(i) {
            d.ls = Some(ls);
            d.lf = Some(lf);
        }
    }

    fn incoming(&self, i: usize) -> &[usize] {
        self.incoming.get(i).map(Vec::as_slice).unwrap_or_default()
    }

    fn outgoing(&self, i: usize) -> &[usize] {
        self.outgoing.get(i).map(Vec::as_slice).unwrap_or_default()
    }

    fn leaf_descendants(&self, idx: usize) -> Vec<usize> {
        if self.leaf(idx) {
            return vec![idx];
        }
        self.p.descendants_range(idx).filter(|&i| self.leaf(i)).collect()
    }

    fn resolve_pred(&mut self, r: &TaskRef, kind: LinkType, succ_name: &str) -> Vec<PredSource> {
        match r {
            TaskRef::Local(id) => {
                let Some(&pi) = self.position.get(id) else {
                    self.warnings.push(format!("'{succ_name}' references missing predecessor {id}"));
                    return Vec::new();
                };
                if self.task(pi).is_none_or(|t| !t.active) {
                    return Vec::new();
                }
                let leaves = self.leaf_descendants(pi);
                let pred_is_start = matches!(kind, LinkType::SS | LinkType::SF);
                if !self.leaf(pi) && pred_is_start {
                    vec![PredSource::SummaryStart(leaves)]
                } else {
                    leaves.into_iter().map(PredSource::Leaf).collect()
                }
            }
            TaskRef::External { project, task } => {
                let snap = self.p.external(*project).and_then(|e| e.task(*task));
                match snap {
                    Some(s) => match (s.start, s.finish) {
                        (Some(start), Some(finish)) => vec![PredSource::Fixed { start, finish, duration: s.duration }],
                        _ => {
                            self.warnings.push(format!("External predecessor '{}' of '{succ_name}' has no dates", s.name));
                            Vec::new()
                        }
                    },
                    None => {
                        self.warnings.push(format!("'{succ_name}' references unknown external task {task}"));
                        Vec::new()
                    }
                }
            }
        }
    }

    fn build_edges(&mut self) {
        let p = self.p;
        for succ in self.leaves.clone() {
            let Some(task) = self.task(succ) else { continue };
            // Own links, links inherited from ancestor summaries, and milestone membership.
            let mut deps: Vec<Dependency> = task.predecessors.clone();
            let mut anc = self.parent.get(succ).copied().flatten();
            while let Some(a) = anc {
                if let Some(t) = p.tasks.get(a) {
                    deps.extend(t.predecessors.iter().copied());
                }
                anc = self.parent.get(a).copied().flatten();
            }
            deps.extend(task.members.iter().map(|m| Dependency::fs(*m)));
            for d in deps {
                for src in self.resolve_pred(&d.predecessor, d.kind, &task.name) {
                    if let PredSource::Leaf(pi) = src
                        && pi == succ
                    {
                        continue;
                    }
                    let e = self.edges.len();
                    let preds: &[usize] = match &src {
                        PredSource::Leaf(pi) => std::slice::from_ref(pi),
                        PredSource::SummaryStart(ls) => ls,
                        PredSource::Fixed { .. } => &[],
                    };
                    for pi in preds {
                        if let Some(out) = self.outgoing.get_mut(*pi) {
                            out.push(e);
                        }
                    }
                    if let Some(inc) = self.incoming.get_mut(succ) {
                        inc.push(e);
                    }
                    self.edges.push(Edge { pred: src, succ, kind: d.kind, lag: d.lag });
                }
            }
        }
    }

    fn pred_count(edge: &Edge) -> usize {
        match &edge.pred {
            PredSource::Leaf(_) => 1,
            PredSource::SummaryStart(v) => v.len(),
            PredSource::Fixed { .. } => 0,
        }
    }

    /// Kahn's algorithm over leaves. Cycles are reported and broken by index order.
    fn topo_sort(&mut self) {
        let n = self.p.tasks.len();
        let mut indeg = vec![0usize; n];
        for e in &self.edges {
            if let Some(d) = indeg.get_mut(e.succ) {
                *d += Self::pred_count(e);
            }
        }
        let mut ready: std::collections::VecDeque<usize> =
            self.leaves.iter().copied().filter(|&i| indeg.get(i).copied() == Some(0)).collect();
        let mut done = vec![false; n];
        let mut order = Vec::with_capacity(self.leaves.len());
        loop {
            while let Some(i) = ready.pop_front() {
                match done.get_mut(i) {
                    Some(d) if !*d => *d = true,
                    _ => continue,
                }
                order.push(i);
                for &e in self.outgoing(i) {
                    let Some(s) = self.edges.get(e).map(|e| e.succ) else { continue };
                    if let Some(d) = indeg.get_mut(s) {
                        *d = d.saturating_sub(1);
                        if *d == 0 && done.get(s) == Some(&false) {
                            ready.push_back(s);
                        }
                    }
                }
            }
            if order.len() >= self.leaves.len() {
                break;
            }
            // Cycle: force the first remaining leaf.
            let not_done = |i: &&usize| done.get(**i) == Some(&false);
            let Some(&stuck) = self.leaves.iter().find(not_done) else {
                break;
            };
            let cycle_names: Vec<String> =
                self.leaves.iter().filter(not_done).take(6).filter_map(|&i| self.task(i)).map(|t| t.name.clone()).collect();
            self.warnings.push(format!("Circular dependency involving: {}", cycle_names.join(", ")));
            if let Some(d) = indeg.get_mut(stuck) {
                *d = 0;
            }
            ready.push_back(stuck);
        }
        self.order = order;
    }

    fn pred_dates(&self, src: &PredSource) -> Option<(NaiveDateTime, NaiveDateTime, Minutes)> {
        match src {
            PredSource::Leaf(i) => {
                let d = self.date(*i);
                Some((d.es?, d.ef?, self.task(*i)?.duration))
            }
            PredSource::SummaryStart(v) => {
                let start = v.iter().filter_map(|i| self.date(*i).es).min()?;
                let finish = v.iter().filter_map(|i| self.date(*i).ef).max()?;
                Some((start, finish, self.proj_cal.between(start, finish)))
            }
            PredSource::Fixed { start, finish, duration } => Some((*start, *finish, *duration)),
        }
    }

    fn duration_of(&self, i: usize) -> Minutes {
        match self.task(i) {
            Some(t) if !t.is_milestone() => t.duration.max(0),
            _ => 0,
        }
    }

    fn forward(&mut self, alap_pins: &HashMap<usize, NaiveDateTime>, warn: bool) {
        let p = self.p;
        let project_start = p.start;
        for i in self.order.clone() {
            let Some(t) = self.task(i) else { continue };
            let cal = self.cal(i);
            let dur = self.duration_of(i);

            let place = |start: NaiveDateTime| -> (NaiveDateTime, NaiveDateTime) {
                if dur > 0 {
                    let s = cal.snap_forward(start);
                    (s, cal.add(s, dur))
                } else {
                    let s = cal.snap_event(start);
                    (s, s)
                }
            };

            if t.mode == ScheduleMode::Manual {
                let start = t.manual_start.or(t.actual_start).unwrap_or(project_start);
                let finish = t.actual_finish.or(t.manual_finish).unwrap_or_else(|| if dur > 0 { cal.add(start, dur) } else { start });
                self.set_early(i, start, finish.max(start));
                continue;
            }

            let mut es = project_start;
            let mut driven_by_link = false;
            for &e in self.incoming(i) {
                let Some(edge) = self.edges.get(e) else { continue };
                let Some((ps, pf, pdur)) = self.pred_dates(&edge.pred) else {
                    continue;
                };
                let cand_start = match edge.kind {
                    LinkType::FS => apply_lag(&self.proj_cal, pf, edge.lag, pdur),
                    LinkType::SS => apply_lag(&self.proj_cal, ps, edge.lag, pdur),
                    LinkType::FF => cal.sub(apply_lag(&self.proj_cal, pf, edge.lag, pdur), dur),
                    LinkType::SF => cal.sub(apply_lag(&self.proj_cal, ps, edge.lag, pdur), dur),
                };
                if cand_start > es {
                    es = cand_start;
                    driven_by_link = true;
                }
            }

            let mut warnings = Vec::new();
            if let Some(date) = t.constraint.date.filter(|_| t.constraint.kind.needs_date()) {
                let finish_of = |s: NaiveDateTime| place(s).1;
                match t.constraint.kind {
                    ConstraintKind::Snet => es = es.max(date),
                    ConstraintKind::Fnet => es = es.max(cal.sub(date, dur)),
                    ConstraintKind::Mso => {
                        if warn && driven_by_link && place(es).0 > place(date).0 {
                            warnings.push(format!("'{}': links require a later start than its Must Start On date", t.name));
                        }
                        es = date;
                    }
                    ConstraintKind::Mfo => {
                        let s = if dur > 0 { cal.sub(date, dur) } else { date };
                        if warn && driven_by_link && place(es).0 > place(s).0 {
                            warnings.push(format!("'{}': links require a later finish than its Must Finish On date", t.name));
                        }
                        es = s;
                    }
                    ConstraintKind::Snlt => {
                        if place(es).0 > date {
                            if warn {
                                warnings.push(format!("'{}' cannot start by its Start No Later Than date", t.name));
                            }
                            es = date;
                        }
                    }
                    ConstraintKind::Fnlt => {
                        if finish_of(es) > date {
                            if warn {
                                warnings.push(format!("'{}' cannot finish by its Finish No Later Than date", t.name));
                            }
                            es = if dur > 0 { cal.sub(date, dur) } else { date };
                        }
                    }
                    ConstraintKind::Asap | ConstraintKind::Alap => {}
                }
            }

            if let Some(pin) = alap_pins.get(&i) {
                es = es.max(*pin);
            }

            if t.leveling_delay > 0 {
                es = cal.add(cal.snap_forward(es), t.leveling_delay);
            }

            let (mut s, mut f) = place(es);
            if let Some(a) = t.actual_start {
                s = a;
                f = if dur > 0 { cal.add(a, dur) } else { a };
            }
            if let Some(af) = t.actual_finish {
                if t.actual_start.is_none() {
                    s = if dur > 0 { cal.sub(af, dur) } else { af };
                }
                f = af.max(s);
            }
            self.warnings.extend(warnings);
            self.set_early(i, s, f);
        }
    }

    fn project_bounds(&self) -> (Option<NaiveDateTime>, Option<NaiveDateTime>) {
        let active = self.leaves.iter().filter(|&&i| self.task(i).is_some_and(|t| t.active));
        let start = active.clone().filter_map(|&i| self.date(i).es).min();
        let finish = active.filter_map(|&i| self.date(i).ef).max();
        (start, finish)
    }

    fn backward(&mut self) {
        let p = self.p;
        let (_, finish) = self.project_bounds();
        let project_finish = finish.unwrap_or(p.start);
        for i in self.order.clone().into_iter().rev() {
            let Some(t) = self.task(i) else { continue };
            let cal = self.cal(i);
            let dur = self.duration_of(i);
            let (Some(es), Some(ef)) = (self.date(i).es, self.date(i).ef) else {
                continue;
            };

            if t.mode == ScheduleMode::Manual || t.actual_finish.is_some() {
                self.set_late(i, es, ef);
                continue;
            }

            let mut lf = project_finish.max(ef);
            if let Some(d) = t.deadline {
                lf = lf.min(d);
            }
            for &e in self.outgoing(i) {
                let Some(edge) = self.edges.get(e) else { continue };
                let (Some(sls), Some(slf)) = (self.date(edge.succ).ls, self.date(edge.succ).lf) else {
                    continue;
                };
                let cand_lf = match edge.kind {
                    LinkType::FS => unapply_lag(&self.proj_cal, sls, edge.lag, dur),
                    LinkType::FF => unapply_lag(&self.proj_cal, slf, edge.lag, dur),
                    LinkType::SS => cal.add(unapply_lag(&self.proj_cal, sls, edge.lag, dur), dur),
                    LinkType::SF => cal.add(unapply_lag(&self.proj_cal, slf, edge.lag, dur), dur),
                };
                lf = lf.min(cand_lf);
            }
            if let Some(date) = t.constraint.date.filter(|_| t.constraint.kind.needs_date()) {
                match t.constraint.kind {
                    ConstraintKind::Snlt => lf = lf.min(if dur > 0 { cal.add(date, dur) } else { date }),
                    ConstraintKind::Fnlt => lf = lf.min(date),
                    ConstraintKind::Mso | ConstraintKind::Mfo => lf = ef,
                    _ => {}
                }
            }
            let lf = cal.snap_backward(lf);
            let ls = if dur > 0 { cal.sub(lf, dur) } else { lf };
            self.set_late(i, ls, lf);
        }
    }

    /// Total and free slack of every leaf (indexed like the tasks).
    fn slack(&self) -> (Vec<Minutes>, Vec<Minutes>) {
        let n = self.p.tasks.len();
        let mut total_slack = vec![0; n];
        let mut free_slack = vec![0; n];
        for &i in &self.order {
            let d = self.date(i);
            let cal = self.cal(i);
            let (Some(es), Some(ef), Some(ls), Some(lf)) = (d.es, d.ef, d.ls, d.lf) else {
                continue;
            };
            let ts = cal.between(es, ls).min(cal.between(ef, lf));
            let dur = self.duration_of(i);
            let mut fs = Minutes::MAX;
            for &e in self.outgoing(i) {
                let Some(edge) = self.edges.get(e) else { continue };
                let sd = self.date(edge.succ);
                let (Some(ses), Some(sef)) = (sd.es, sd.ef) else {
                    continue;
                };
                let gap = match edge.kind {
                    LinkType::FS => cal.between(apply_lag(&self.proj_cal, ef, edge.lag, dur), ses),
                    LinkType::SS => cal.between(apply_lag(&self.proj_cal, es, edge.lag, dur), ses),
                    LinkType::FF => cal.between(apply_lag(&self.proj_cal, ef, edge.lag, dur), sef),
                    LinkType::SF => cal.between(apply_lag(&self.proj_cal, es, edge.lag, dur), sef),
                };
                fs = fs.min(gap);
            }
            if let Some(slot) = total_slack.get_mut(i) {
                *slot = ts;
            }
            if let Some(slot) = free_slack.get_mut(i) {
                *slot = if fs == Minutes::MAX { ts } else { fs.max(0).min(ts.max(0)) };
            }
        }
        (total_slack, free_slack)
    }
}

/// Re-derives work, actual work and cost of every leaf task from assignments and time entries.
fn rollup_tracking(p: &mut Project) {
    let mut entry_by_task: HashMap<TaskId, Minutes> = HashMap::new();
    for e in &p.time_entries {
        let slot = entry_by_task.entry(e.task).or_default();
        *slot = slot.saturating_add(e.minutes);
    }
    let resources: HashMap<ResourceId, &Resource> = p.resources.iter().map(|r| (r.id, r)).collect();
    // (work, actual work, cost, has a work assignment) per task.
    let mut by_task: HashMap<TaskId, (Minutes, Minutes, f64, bool)> = HashMap::new();
    for a in &p.assignments {
        let Some(r) = resources.get(&a.resource) else {
            continue;
        };
        let slot = by_task.entry(a.task).or_default();
        slot.1 = slot.1.saturating_add(a.actual_work);
        match r.kind {
            ResourceType::Work => {
                slot.3 = true;
                slot.0 = slot.0.saturating_add(a.work);
                slot.2 += a.work as f64 / 60.0 * r.standard_rate + r.cost_per_use;
            }
            ResourceType::Material => slot.2 += a.units * r.standard_rate + r.cost_per_use,
            ResourceType::Cost => slot.2 += a.cost,
        }
    }
    let settings = p.settings.clone();
    for t in &mut p.tasks {
        let (mut work, assigned_actual, assigned_cost, has_work_assignment) = by_task.get(&t.id).copied().unwrap_or_default();
        let actual = entry_by_task.get(&t.id).copied().unwrap_or(0).saturating_add(assigned_actual);
        if !has_work_assignment && let Some(est) = &t.estimate {
            work = est.expected_work(&settings);
        }
        t.sched.work = work;
        t.sched.actual_work = actual;
        t.sched.cost = t.fixed_cost + assigned_cost;
        t.sched.percent_complete = if settings.progress_from_time && work > 0 && t.actual_finish.is_none() {
            ((actual as f64 / work as f64) * 100.0).min(99.0) as f32
        } else {
            f32::from(t.percent_complete)
        };
    }
}

/// Recalculates the whole schedule in place.
///
/// Never panics: if the engine hits a bug, the project keeps its previous dates and gets a
/// warning saying so (see [`try_recalculate`] to roll back instead).
pub fn recalculate(p: &mut Project) -> ScheduleReport {
    match try_recalculate(p) {
        Ok(report) => report,
        Err(e) => {
            let msg =
                format!("Scheduling stopped because of an internal error ({}); dates may be out of date. Please report this.", e.message);
            p.sched.warnings = vec![msg.clone()];
            ScheduleReport { warnings: vec![msg], ..ScheduleReport::default() }
        }
    }
}

/// Recalculates the schedule, turning an engine panic into an error.
///
/// On error the computed fields may be half-updated; `Document` edits restore their snapshot.
pub fn try_recalculate(p: &mut Project) -> Result<ScheduleReport, Panicked> {
    guard::catch("recalculate", || calculate(p))
}

fn calculate(p: &mut Project) -> ScheduleReport {
    for t in &mut p.tasks {
        t.outline_level = t.outline_level.max(1);
    }
    let summary: Vec<bool> = (0..p.tasks.len()).map(|i| p.has_children(i)).collect();
    for (t, s) in p.tasks.iter_mut().zip(&summary) {
        t.sched.is_summary = *s;
    }
    rollup_tracking(p);

    let (dates, order_warn, total_slack, free_slack) = {
        let mut eng = Engine::new(p);
        eng.build_edges();
        eng.topo_sort();
        eng.forward(&HashMap::new(), false);
        eng.backward();
        // As-late-as-possible tasks are pinned to their late start and the forward pass is repeated
        // so successors stay consistent.
        let pins: HashMap<usize, NaiveDateTime> = eng
            .order
            .iter()
            .copied()
            .filter(|&i| {
                eng.task(i)
                    .is_some_and(|t| t.mode == ScheduleMode::Auto && t.constraint.kind == ConstraintKind::Alap && t.actual_start.is_none())
            })
            .filter_map(|i| eng.date(i).ls.map(|ls| (i, ls)))
            .collect();
        eng.forward(&pins, true);
        eng.backward();
        let (total_slack, free_slack) = eng.slack();
        (eng.dates, eng.warnings, total_slack, free_slack)
    };

    let mut report = ScheduleReport { warnings: order_warn, ..Default::default() };
    let crit = p.settings.critical_slack;

    for (i, t) in p.tasks.iter_mut().enumerate() {
        if t.sched.is_summary {
            continue;
        }
        let d = dates.get(i).copied().unwrap_or_default();
        let ts = total_slack.get(i).copied().unwrap_or(0);
        let mut warnings = Vec::new();
        t.sched.start = d.es;
        t.sched.finish = d.ef;
        t.sched.early_start = d.es;
        t.sched.early_finish = d.ef;
        t.sched.late_start = d.ls;
        t.sched.late_finish = d.lf;
        t.sched.total_slack = ts;
        t.sched.free_slack = free_slack.get(i).copied().unwrap_or(0);
        t.sched.duration = if t.is_milestone() { 0 } else { t.duration };
        t.sched.critical = t.active && !t.is_complete() && t.mode == ScheduleMode::Auto && ts <= crit;
        if let (Some(dl), Some(f)) = (t.deadline, d.ef)
            && f > dl
        {
            warnings.push(format!("Finishes after its deadline ({})", dl.format("%Y-%m-%d")));
        }
        t.sched.warnings = warnings;
    }

    rollup_summaries(p);

    // Project-level rollup.
    let leaves: Vec<&Task> = p.tasks.iter().filter(|t| !t.sched.is_summary && t.active).collect();
    let start = leaves.iter().filter_map(|t| t.sched.start).min();
    let finish = leaves.iter().filter_map(|t| t.sched.finish).max();
    let work = leaves.iter().fold(0, |acc: Minutes, t| acc.saturating_add(t.sched.work));
    let actual = leaves.iter().fold(0, |acc: Minutes, t| acc.saturating_add(t.sched.actual_work));
    let cost: f64 = leaves.iter().map(|t| t.sched.cost).sum();
    let (num, den) = leaves.iter().fold((0.0f64, 0.0f64), |(n, d), t| {
        let w = t.duration.max(1) as f64;
        (n + w * f64::from(t.sched.percent_complete), d + w)
    });
    let pct = if den > 0.0 { (num / den) as f32 } else { 0.0 };
    for t in &p.tasks {
        for w in &t.sched.warnings {
            report.warnings.push(format!("{}: {}", t.name, w));
        }
    }
    report.start = start;
    report.finish = finish;
    report.critical_tasks = p.tasks.iter().filter(|t| t.sched.critical && !t.sched.is_summary).map(|t| t.id).collect();
    p.sched = ProjectSchedule {
        start,
        finish,
        work,
        actual_work: actual,
        cost,
        percent_complete: pct,
        warnings: report.warnings.clone(),
        calculated_at: Some(chrono::Local::now().naive_local().with_nanosecond_zero()),
    };
    report
}

trait NoNanos {
    fn with_nanosecond_zero(self) -> Self;
}
impl NoNanos for NaiveDateTime {
    fn with_nanosecond_zero(self) -> Self {
        use chrono::Timelike;
        self.with_nanosecond(0).unwrap_or(self)
    }
}

/// Rolled-up values of one summary task.
struct Rollup {
    start: Option<NaiveDateTime>,
    finish: Option<NaiveDateTime>,
    es: Option<NaiveDateTime>,
    ef: Option<NaiveDateTime>,
    ls: Option<NaiveDateTime>,
    lf: Option<NaiveDateTime>,
    work: Minutes,
    actual: Minutes,
    cost: f64,
    slack: Minutes,
    free: Minutes,
    critical: bool,
    percent: f32,
}

fn rollup_of(p: &Project, children: &[usize], own_fixed_cost: f64) -> Rollup {
    let kids: Vec<&Task> = children.iter().filter_map(|&c| p.tasks.get(c)).collect();
    let active: Vec<&Task> = kids.iter().copied().filter(|t| t.active).collect();
    let src = if active.is_empty() { &kids } else { &active };
    let start = src.iter().filter_map(|t| t.sched.start).min();
    let finish = src.iter().filter_map(|t| t.sched.finish).max();
    let (num, den) = src.iter().fold((0.0f64, 0.0f64), |(nn, d), t| {
        let w = t.sched.duration.max(1) as f64;
        (nn + w * f64::from(t.sched.percent_complete), d + w)
    });
    Rollup {
        start,
        finish,
        es: src.iter().filter_map(|t| t.sched.early_start).min(),
        ef: src.iter().filter_map(|t| t.sched.early_finish).max(),
        ls: src.iter().filter_map(|t| t.sched.late_start).min(),
        lf: src.iter().filter_map(|t| t.sched.late_finish).max(),
        work: src.iter().fold(0, |acc: Minutes, t| acc.saturating_add(t.sched.work)),
        actual: src.iter().fold(0, |acc: Minutes, t| acc.saturating_add(t.sched.actual_work)),
        cost: src.iter().map(|t| t.sched.cost).sum::<f64>() + own_fixed_cost,
        slack: src.iter().map(|t| t.sched.total_slack).min().unwrap_or(0),
        free: src.iter().map(|t| t.sched.free_slack).min().unwrap_or(0),
        critical: src.iter().any(|t| t.sched.critical),
        percent: if den > 0.0 { (num / den) as f32 } else { 0.0 },
    }
}

fn rollup_summaries(p: &mut Project) {
    let parent = parents(p);
    let mut children: Vec<Vec<usize>> = vec![Vec::new(); p.tasks.len()];
    for (c, par) in parent.iter().enumerate() {
        if let Some(list) = par.and_then(|pi| children.get_mut(pi)) {
            list.push(c);
        }
    }
    let proj_cal_owned = p.base_calendar().clone();
    let pc = WorkCal::single(&proj_cal_owned);
    // Deepest summaries first: a summary's children are rolled up before it.
    for i in (0..p.tasks.len()).rev() {
        let Some(t) = p.tasks.get(i) else { continue };
        if !t.sched.is_summary {
            continue;
        }
        let kids = children.get(i).map(Vec::as_slice).unwrap_or_default();
        let r = rollup_of(p, kids, t.fixed_cost);
        let duration = match (r.start, r.finish) {
            (Some(s), Some(f)) => pc.between(s, f),
            _ => 0,
        };
        let Some(t) = p.tasks.get_mut(i) else { continue };
        t.sched.start = r.start;
        t.sched.finish = r.finish;
        t.sched.early_start = r.es;
        t.sched.early_finish = r.ef;
        t.sched.late_start = r.ls;
        t.sched.late_finish = r.lf;
        t.sched.work = r.work;
        t.sched.actual_work = r.actual;
        t.sched.cost = r.cost;
        t.sched.total_slack = r.slack;
        t.sched.free_slack = r.free;
        t.sched.critical = r.critical;
        t.sched.duration = duration;
        t.sched.percent_complete = r.percent;
        t.sched.warnings.clear();
    }
}
