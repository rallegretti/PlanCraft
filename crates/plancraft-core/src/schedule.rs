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

use std::collections::HashMap;

use chrono::{Duration, NaiveDateTime};

use crate::calendar::{CalLayer, WorkCal};
use crate::duration::Minutes;
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

use crate::duration::Lag;

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
    pub critical_tasks: Vec<crate::ids::TaskId>,
}

/// Builds the effective working calendar of the task at `idx`.
///
/// Assigned work resources contribute their own calendars (base calendar plus
/// personal exceptions); the task is scheduled in time common to all of them.
pub fn task_calendar(p: &Project, idx: usize) -> WorkCal<'_> {
    let task = &p.tasks[idx];
    let base = task.calendar.and_then(|c| p.calendar(c)).unwrap_or_else(|| p.base_calendar());
    let mut layers = Vec::new();
    if task.calendar.is_some() {
        layers.push(CalLayer::new(base, &[]));
    }
    if p.settings.use_resource_calendars && !task.ignore_resource_calendars {
        for a in p.assignments_for_task(task.id) {
            if let Some(r) = p.resource(a.resource).filter(|r| r.kind == ResourceType::Work) {
                let cal = r.calendar.and_then(|c| p.calendar(c)).unwrap_or_else(|| p.base_calendar());
                layers.push(CalLayer::new(cal, &r.exceptions));
            }
        }
    }
    if layers.is_empty() {
        return WorkCal::single(base);
    }
    let wc = WorkCal::from_layers(layers);
    // Fall back to the task/project calendar when resources share no working time at all.
    let probe = p.start.date();
    let any = (0..28).any(|i| wc.is_working_day(probe + Duration::days(i)));
    if any { wc } else { WorkCal::single(base) }
}

/// The project calendar used for lags and summary durations.
pub fn project_calendar(p: &Project) -> WorkCal<'_> {
    WorkCal::single(p.base_calendar())
}

fn apply_lag(cal: &WorkCal, base: NaiveDateTime, lag: Lag, pred_duration: Minutes) -> NaiveDateTime {
    match lag {
        Lag::None => base,
        Lag::Working(0) => base,
        Lag::Working(m) => cal.add(base, m),
        Lag::Elapsed(m) => base + Duration::minutes(m),
        Lag::Percent(pct) => {
            let m = pred_duration * pct as i64 / 100;
            if m == 0 { base } else { cal.add(base, m) }
        }
    }
}

fn unapply_lag(cal: &WorkCal, base: NaiveDateTime, lag: Lag, pred_duration: Minutes) -> NaiveDateTime {
    match lag {
        Lag::None => base,
        Lag::Working(0) => base,
        Lag::Working(m) => cal.sub(base, m),
        Lag::Elapsed(m) => base - Duration::minutes(m),
        Lag::Percent(pct) => {
            let m = pred_duration * pct as i64 / 100;
            if m == 0 { base } else { cal.sub(base, m) }
        }
    }
}

struct Engine<'a> {
    p: &'a Project,
    proj_cal: WorkCal<'a>,
    cals: Vec<WorkCal<'a>>,
    leaves: Vec<usize>,
    is_leaf: Vec<bool>,
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
        let leaves: Vec<usize> = (0..n).filter(|&i| is_leaf[i]).collect();
        let cals = (0..n).map(|i| task_calendar(p, i)).collect();
        Self {
            p,
            proj_cal: project_calendar(p),
            cals,
            leaves,
            is_leaf,
            edges: Vec::new(),
            incoming: vec![Vec::new(); n],
            outgoing: vec![Vec::new(); n],
            order: Vec::new(),
            warnings: Vec::new(),
            dates: vec![Dates::default(); n],
        }
    }

    fn leaf_descendants(&self, idx: usize) -> Vec<usize> {
        if self.is_leaf[idx] {
            return vec![idx];
        }
        self.p.descendants_range(idx).filter(|&i| self.is_leaf[i]).collect()
    }

    fn resolve_pred(&mut self, r: &TaskRef, kind: LinkType, succ_name: &str) -> Vec<PredSource> {
        match r {
            TaskRef::Local(id) => {
                let Some(pi) = self.p.task_index(*id) else {
                    self.warnings.push(format!("'{succ_name}' references missing predecessor {id}"));
                    return Vec::new();
                };
                if !self.p.tasks[pi].active {
                    return Vec::new();
                }
                let leaves = self.leaf_descendants(pi);
                let pred_is_start = matches!(kind, LinkType::SS | LinkType::SF);
                if !self.is_leaf[pi] && pred_is_start {
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
        for li in 0..self.leaves.len() {
            let succ = self.leaves[li];
            let task = &p.tasks[succ];
            // Own links, links inherited from ancestor summaries, and milestone membership.
            let mut deps: Vec<Dependency> = task.predecessors.clone();
            let mut anc = p.parent_index(succ);
            while let Some(a) = anc {
                deps.extend(p.tasks[a].predecessors.iter().copied());
                anc = p.parent_index(a);
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
                    match &src {
                        PredSource::Leaf(pi) => self.outgoing[*pi].push(e),
                        PredSource::SummaryStart(ls) => ls.iter().for_each(|pi| self.outgoing[*pi].push(e)),
                        PredSource::Fixed { .. } => {}
                    }
                    self.incoming[succ].push(e);
                    self.edges.push(Edge { pred: src, succ, kind: d.kind, lag: d.lag });
                }
            }
        }
    }

    fn pred_leaves(edge: &Edge) -> Vec<usize> {
        match &edge.pred {
            PredSource::Leaf(i) => vec![*i],
            PredSource::SummaryStart(v) => v.clone(),
            PredSource::Fixed { .. } => Vec::new(),
        }
    }

    /// Kahn's algorithm over leaves. Cycles are reported and broken by index order.
    fn topo_sort(&mut self) {
        let n = self.p.tasks.len();
        let mut indeg = vec![0usize; n];
        for e in &self.edges {
            indeg[e.succ] += Self::pred_leaves(e).len();
        }
        let mut ready: std::collections::VecDeque<usize> = self.leaves.iter().copied().filter(|&i| indeg[i] == 0).collect();
        let mut done = vec![false; n];
        let mut order = Vec::with_capacity(self.leaves.len());
        loop {
            while let Some(i) = ready.pop_front() {
                if done[i] {
                    continue;
                }
                done[i] = true;
                order.push(i);
                for &e in &self.outgoing[i] {
                    let s = self.edges[e].succ;
                    indeg[s] = indeg[s].saturating_sub(1);
                    if indeg[s] == 0 && !done[s] {
                        ready.push_back(s);
                    }
                }
            }
            if order.len() == self.leaves.len() {
                break;
            }
            // Cycle: force the first remaining leaf.
            let Some(&stuck) = self.leaves.iter().find(|&&i| !done[i]) else {
                break;
            };
            let cycle_names: Vec<String> =
                self.leaves.iter().filter(|&&i| !done[i]).take(6).map(|&i| self.p.tasks[i].name.clone()).collect();
            self.warnings.push(format!("Circular dependency involving: {}", cycle_names.join(", ")));
            indeg[stuck] = 0;
            ready.push_back(stuck);
        }
        self.order = order;
    }

    fn pred_dates(&self, src: &PredSource) -> Option<(NaiveDateTime, NaiveDateTime, Minutes)> {
        match src {
            PredSource::Leaf(i) => {
                let d = self.dates[*i];
                Some((d.es?, d.ef?, self.p.tasks[*i].duration))
            }
            PredSource::SummaryStart(v) => {
                let start = v.iter().filter_map(|i| self.dates[*i].es).min()?;
                let finish = v.iter().filter_map(|i| self.dates[*i].ef).max()?;
                Some((start, finish, self.proj_cal.between(start, finish)))
            }
            PredSource::Fixed { start, finish, duration } => Some((*start, *finish, *duration)),
        }
    }

    fn duration_of(&self, i: usize) -> Minutes {
        let t = &self.p.tasks[i];
        if t.is_milestone() { 0 } else { t.duration.max(0) }
    }

    fn forward(&mut self, alap_pins: &HashMap<usize, NaiveDateTime>, warn: bool) {
        let p = self.p;
        let project_start = p.start;
        for oi in 0..self.order.len() {
            let i = self.order[oi];
            let t = &p.tasks[i];
            let cal = &self.cals[i];
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
                self.dates[i].es = Some(start);
                self.dates[i].ef = Some(finish.max(start));
                continue;
            }

            let mut es = project_start;
            let mut driven_by_link = false;
            for &e in &self.incoming[i] {
                let edge = &self.edges[e];
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

            if let Some(date) = t.constraint.date.filter(|_| t.constraint.kind.needs_date()) {
                let finish_of = |s: NaiveDateTime| place(s).1;
                match t.constraint.kind {
                    ConstraintKind::Snet => es = es.max(date),
                    ConstraintKind::Fnet => es = es.max(cal.sub(date, dur)),
                    ConstraintKind::Mso => {
                        if warn && driven_by_link && place(es).0 > place(date).0 {
                            self.warnings.push(format!("'{}': links require a later start than its Must Start On date", t.name));
                        }
                        es = date;
                    }
                    ConstraintKind::Mfo => {
                        let s = if dur > 0 { cal.sub(date, dur) } else { date };
                        if warn && driven_by_link && place(es).0 > place(s).0 {
                            self.warnings.push(format!("'{}': links require a later finish than its Must Finish On date", t.name));
                        }
                        es = s;
                    }
                    ConstraintKind::Snlt => {
                        if place(es).0 > date {
                            if warn {
                                self.warnings.push(format!("'{}' cannot start by its Start No Later Than date", t.name));
                            }
                            es = date;
                        }
                    }
                    ConstraintKind::Fnlt => {
                        if finish_of(es) > date {
                            if warn {
                                self.warnings.push(format!("'{}' cannot finish by its Finish No Later Than date", t.name));
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
            self.dates[i].es = Some(s);
            self.dates[i].ef = Some(f);
        }
    }

    fn project_bounds(&self) -> (Option<NaiveDateTime>, Option<NaiveDateTime>) {
        let active = self.leaves.iter().filter(|&&i| self.p.tasks[i].active);
        let start = active.clone().filter_map(|&i| self.dates[i].es).min();
        let finish = active.filter_map(|&i| self.dates[i].ef).max();
        (start, finish)
    }

    fn backward(&mut self) {
        let p = self.p;
        let (_, finish) = self.project_bounds();
        let project_finish = finish.unwrap_or(p.start);
        for oi in (0..self.order.len()).rev() {
            let i = self.order[oi];
            let t = &p.tasks[i];
            let cal = &self.cals[i];
            let dur = self.duration_of(i);
            let (Some(es), Some(ef)) = (self.dates[i].es, self.dates[i].ef) else {
                continue;
            };

            if t.mode == ScheduleMode::Manual || t.actual_finish.is_some() {
                self.dates[i].ls = Some(es);
                self.dates[i].lf = Some(ef);
                continue;
            }

            let mut lf = project_finish.max(ef);
            if let Some(d) = t.deadline {
                lf = lf.min(d);
            }
            for &e in &self.outgoing[i] {
                let edge = &self.edges[e];
                let s = edge.succ;
                let (Some(sls), Some(slf)) = (self.dates[s].ls, self.dates[s].lf) else {
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
                    ConstraintKind::Mso => lf = ef,
                    ConstraintKind::Mfo => lf = ef,
                    _ => {}
                }
            }
            let (ls, lf) = if dur > 0 {
                let lf = cal.snap_backward(lf);
                (cal.sub(lf, dur), lf)
            } else {
                let lf = cal.snap_backward(lf);
                (lf, lf)
            };
            self.dates[i].ls = Some(ls);
            self.dates[i].lf = Some(lf);
        }
    }
}

/// Re-derives work, actual work and cost of every leaf task from assignments and time entries.
fn rollup_tracking(p: &mut Project) {
    let mut entry_by_task: HashMap<crate::ids::TaskId, Minutes> = HashMap::new();
    for e in &p.time_entries {
        *entry_by_task.entry(e.task).or_default() += e.minutes;
    }
    let settings = p.settings.clone();
    let n = p.tasks.len();
    for i in 0..n {
        let id = p.tasks[i].id;
        let mut work = 0;
        let mut actual = entry_by_task.get(&id).copied().unwrap_or(0);
        let mut cost = p.tasks[i].fixed_cost;
        let mut has_work_assignment = false;
        for a in p.assignments.iter().filter(|a| a.task == id) {
            let Some(r) = p.resources.iter().find(|r| r.id == a.resource) else {
                continue;
            };
            actual += a.actual_work;
            match r.kind {
                ResourceType::Work => {
                    has_work_assignment = true;
                    work += a.work;
                    cost += a.work as f64 / 60.0 * r.standard_rate + r.cost_per_use;
                }
                ResourceType::Material => cost += a.units * r.standard_rate + r.cost_per_use,
                ResourceType::Cost => cost += a.cost,
            }
        }
        let t = &mut p.tasks[i];
        if !has_work_assignment && let Some(est) = &t.estimate {
            work = est.expected_work(&settings);
        }
        t.sched.work = work;
        t.sched.actual_work = actual;
        t.sched.cost = cost;
        t.sched.percent_complete = if settings.progress_from_time && work > 0 && t.actual_finish.is_none() {
            ((actual as f64 / work as f64) * 100.0).min(99.0) as f32
        } else {
            t.percent_complete as f32
        };
    }
}

/// Recalculates the whole schedule in place.
pub fn recalculate(p: &mut Project) -> ScheduleReport {
    for t in &mut p.tasks {
        t.outline_level = t.outline_level.max(1);
    }
    let n = p.tasks.len();
    for i in 0..n {
        let summary = p.has_children(i);
        p.tasks[i].sched.is_summary = summary;
    }
    rollup_tracking(p);

    let (dates, order_warn, total_slack, free_slack) = {
        let mut eng = Engine::new(p);
        eng.build_edges();
        eng.topo_sort();
        let none = HashMap::new();
        eng.forward(&none, false);
        eng.backward();
        // As-late-as-possible tasks are pinned to their late start and the forward pass is repeated
        // so successors stay consistent.
        let pins: HashMap<usize, NaiveDateTime> = eng
            .order
            .iter()
            .copied()
            .filter(|&i| {
                let t = &p.tasks[i];
                t.mode == ScheduleMode::Auto && t.constraint.kind == ConstraintKind::Alap && t.actual_start.is_none()
            })
            .filter_map(|i| eng.dates[i].ls.map(|ls| (i, ls)))
            .collect();
        eng.forward(&pins, true);
        eng.backward();

        let mut total_slack = vec![0; n];
        let mut free_slack = vec![0; n];
        for &i in &eng.order {
            let d = eng.dates[i];
            let cal = &eng.cals[i];
            if let (Some(es), Some(ef), Some(ls), Some(lf)) = (d.es, d.ef, d.ls, d.lf) {
                let ts = cal.between(es, ls).min(cal.between(ef, lf));
                total_slack[i] = ts;
                let mut fs = Minutes::MAX;
                for &e in &eng.outgoing[i] {
                    let edge = &eng.edges[e];
                    let sd = eng.dates[edge.succ];
                    let dur = eng.duration_of(i);
                    let (Some(ses), Some(sef)) = (sd.es, sd.ef) else {
                        continue;
                    };
                    let gap = match edge.kind {
                        LinkType::FS => cal.between(apply_lag(&eng.proj_cal, ef, edge.lag, dur), ses),
                        LinkType::SS => cal.between(apply_lag(&eng.proj_cal, es, edge.lag, dur), ses),
                        LinkType::FF => cal.between(apply_lag(&eng.proj_cal, ef, edge.lag, dur), sef),
                        LinkType::SF => cal.between(apply_lag(&eng.proj_cal, es, edge.lag, dur), sef),
                    };
                    fs = fs.min(gap);
                }
                free_slack[i] = if fs == Minutes::MAX { ts } else { fs.max(0).min(ts.max(0)) };
            }
        }
        (eng.dates.clone(), eng.warnings.clone(), total_slack, free_slack)
    };

    let mut report = ScheduleReport { warnings: order_warn, ..Default::default() };
    let crit = p.settings.critical_slack;

    for i in 0..n {
        let d = dates[i];
        let t = &mut p.tasks[i];
        let mut warnings = Vec::new();
        if t.sched.is_summary {
            continue;
        }
        t.sched.start = d.es;
        t.sched.finish = d.ef;
        t.sched.early_start = d.es;
        t.sched.early_finish = d.ef;
        t.sched.late_start = d.ls;
        t.sched.late_finish = d.lf;
        t.sched.total_slack = total_slack[i];
        t.sched.free_slack = free_slack[i];
        t.sched.duration = if t.is_milestone() { 0 } else { t.duration };
        t.sched.critical = t.active && !t.is_complete() && t.mode == ScheduleMode::Auto && total_slack[i] <= crit;
        if let (Some(dl), Some(f)) = (t.deadline, d.ef)
            && f > dl
        {
            warnings.push(format!("Finishes after its deadline ({})", dl.format("%Y-%m-%d")));
        }
        t.sched.warnings = warnings;
    }

    rollup_summaries(p);

    // Project-level rollup.
    let proj_cal_finish = {
        let leaves: Vec<usize> = (0..n).filter(|&i| !p.tasks[i].sched.is_summary && p.tasks[i].active).collect();
        let start = leaves.iter().filter_map(|&i| p.tasks[i].sched.start).min();
        let finish = leaves.iter().filter_map(|&i| p.tasks[i].sched.finish).max();
        let work: Minutes = leaves.iter().map(|&i| p.tasks[i].sched.work).sum();
        let actual: Minutes = leaves.iter().map(|&i| p.tasks[i].sched.actual_work).sum();
        let cost: f64 = leaves.iter().map(|&i| p.tasks[i].sched.cost).sum();
        let (num, den) = leaves.iter().fold((0.0f64, 0.0f64), |(n, d), &i| {
            let t = &p.tasks[i];
            let w = t.duration.max(1) as f64;
            (n + w * t.sched.percent_complete as f64, d + w)
        });
        (start, finish, work, actual, cost, if den > 0.0 { (num / den) as f32 } else { 0.0 })
    };
    let (start, finish, work, actual, cost, pct) = proj_cal_finish;
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

fn rollup_summaries(p: &mut Project) {
    let n = p.tasks.len();
    let proj_cal_owned = p.base_calendar().clone();
    let pc = WorkCal::single(&proj_cal_owned);
    for i in (0..n).rev() {
        if !p.tasks[i].sched.is_summary {
            continue;
        }
        let range = p.descendants_range(i);
        let direct: Vec<usize> = range.filter(|&c| p.parent_index(c) == Some(i)).collect();
        let active: Vec<usize> = direct.iter().copied().filter(|&c| p.tasks[c].active).collect();
        let src = if active.is_empty() { &direct } else { &active };
        let start = src.iter().filter_map(|&c| p.tasks[c].sched.start).min();
        let finish = src.iter().filter_map(|&c| p.tasks[c].sched.finish).max();
        let work: Minutes = src.iter().map(|&c| p.tasks[c].sched.work).sum();
        let actual: Minutes = src.iter().map(|&c| p.tasks[c].sched.actual_work).sum();
        let cost: f64 = src.iter().map(|&c| p.tasks[c].sched.cost).sum::<f64>() + p.tasks[i].fixed_cost;
        let es = src.iter().filter_map(|&c| p.tasks[c].sched.early_start).min();
        let ef = src.iter().filter_map(|&c| p.tasks[c].sched.early_finish).max();
        let ls = src.iter().filter_map(|&c| p.tasks[c].sched.late_start).min();
        let lf = src.iter().filter_map(|&c| p.tasks[c].sched.late_finish).max();
        let slack = src.iter().map(|&c| p.tasks[c].sched.total_slack).min().unwrap_or(0);
        let free = src.iter().map(|&c| p.tasks[c].sched.free_slack).min().unwrap_or(0);
        let critical = src.iter().any(|&c| p.tasks[c].sched.critical);
        let (num, den) = src.iter().fold((0.0f64, 0.0f64), |(nn, d), &c| {
            let t = &p.tasks[c];
            let w = t.sched.duration.max(1) as f64;
            (nn + w * t.sched.percent_complete as f64, d + w)
        });
        let duration = match (start, finish) {
            (Some(s), Some(f)) => pc.between(s, f),
            _ => 0,
        };
        let t = &mut p.tasks[i];
        t.sched.start = start;
        t.sched.finish = finish;
        t.sched.early_start = es;
        t.sched.early_finish = ef;
        t.sched.late_start = ls;
        t.sched.late_finish = lf;
        t.sched.work = work;
        t.sched.actual_work = actual;
        t.sched.cost = cost;
        t.sched.total_slack = slack;
        t.sched.free_slack = free;
        t.sched.critical = critical;
        t.sched.duration = duration;
        t.sched.percent_complete = if den > 0.0 { (num / den) as f32 } else { 0.0 };
        t.sched.warnings.clear();
    }
}
