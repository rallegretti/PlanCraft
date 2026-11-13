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
use crate::ids::{CalendarId, ResourceId, TaskId};
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
    /// Completed and remaining parts of an in-progress task.
    parts: Option<Parts>,
}

/// The completed and remaining parts of an in-progress task.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Parts {
    /// End of the completed part (Microsoft Project's Stop field).
    pub stop: NaiveDateTime,
    /// Start of the remaining part: the next working moment after `stop`, or the start of the next
    /// working part when the task pauses at `stop` (Microsoft Project's Resume field).
    pub resume: NaiveDateTime,
    /// Working minutes left to do.
    pub remaining: Minutes,
    /// End of the last working part: the task's finish.
    pub finish: NaiveDateTime,
    /// Start of the last working part and the working minutes in it (the part that can still
    /// slip); the actual start and the whole duration when the task is not split.
    pub last: (NaiveDateTime, Minutes),
    /// The task has pauses (see [`segments_of`]).
    pub split: bool,
}

/// Most pauses one task can have; further ones are ignored.
pub const MAX_SPLITS: usize = 100;

/// The planned splits of a task that take effect: inside its work (`0 < work < dur`), with a
/// length, one per work point (the first), in work order, and together at most
/// [`MAX_MINUTES`](crate::limits::MAX_MINUTES) long (later ones are cut short).
pub fn planned_gaps(gaps: &[Gap], dur: Minutes) -> Vec<Gap> {
    let mut v: Vec<Gap> = gaps.iter().take(MAX_SPLITS).copied().filter(|g| g.work > 0 && g.work < dur && g.length > 0).collect();
    v.sort_by_key(|g| g.work);
    v.dedup_by_key(|g| g.work);
    let mut left = crate::limits::MAX_MINUTES;
    for g in &mut v {
        g.length = g.length.min(left);
        left -= g.length;
    }
    v.retain(|g| g.length > 0);
    v
}

/// Working time a task spans from start to finish: its duration plus its planned splits.
pub fn span_of(t: &Task, dur: Minutes) -> Minutes {
    planned_gaps(&t.gaps, dur).iter().fold(dur, |s, g| s.saturating_add(g.length))
}

/// The working parts `(start, finish, work before it)` of a task laid out from `start`. After a
/// planned split's work the task pauses for the split's length; after an in-progress split's work
/// it stops until that split's resume date. Splits that would not leave a gap (or lie past the
/// duration) are ignored, so a task with no real pause has one part.
pub fn segments_of(
    start: NaiveDateTime,
    splits: &[Split],
    gaps: &[Gap],
    cal: &WorkCal,
    dur: Minutes,
) -> Vec<(NaiveDateTime, NaiveDateTime, Minutes)> {
    enum Until {
        Date(NaiveDateTime),
        For(Minutes),
    }
    let mut breaks: Vec<(Minutes, Until)> = planned_gaps(gaps, dur).into_iter().map(|g| (g.work, Until::For(g.length))).collect();
    breaks.extend(splits.iter().take(MAX_SPLITS).map(|s| (s.work, Until::Date(s.resume))));
    // Planned splits first at the same work point; the sort is stable.
    breaks.sort_by_key(|b| b.0);
    let mut out = Vec::new();
    let (mut at, mut used) = (start, 0);
    for (work, until) in breaks {
        if work <= used {
            continue;
        }
        if work >= dur {
            break;
        }
        let end = cal.add(at, work - used);
        let resume = match until {
            Until::Date(d) => cal.snap_forward(d),
            Until::For(len) => cal.snap_forward(cal.add(cal.snap_forward(end), len)),
        };
        if resume <= cal.snap_forward(end) {
            // No gap: the part simply goes on.
            continue;
        }
        out.push((at, end, used));
        at = resume;
        used = work;
    }
    out.push((at, cal.add(at, dur.saturating_sub(used)), used));
    out
}

/// Splits an in-progress task (actual start, 1-99% complete, no actual finish) into the part done
/// and the part left; `None` for any other task.
pub fn parts_of(t: &Task, cal: &WorkCal, dur: Minutes) -> Option<Parts> {
    let start = t.actual_start?;
    if t.actual_finish.is_some() || t.is_milestone() || dur <= 0 || t.mode == ScheduleMode::Manual {
        return None;
    }
    let pct = Minutes::from(t.percent_complete);
    if !(1..100).contains(&pct) {
        return None;
    }
    let done = dur.saturating_mul(pct) / 100;
    let remaining = dur.saturating_sub(done);
    if t.splits.is_empty() && planned_gaps(&t.gaps, dur).is_empty() {
        let stop = cal.add(start, done);
        return Some(Parts {
            stop,
            resume: cal.snap_forward(stop),
            remaining,
            finish: cal.add(start, dur),
            last: (start, dur),
            split: false,
        });
    }
    let segs = segments_of(start, &t.splits, &t.gaps, cal, dur);
    let (stop, resume) = stop_and_resume(&segs, cal, done)?;
    let &(last_start, finish, before) = segs.last()?;
    Some(Parts { stop, resume, remaining, finish, last: (last_start, dur.saturating_sub(before)), split: segs.len() > 1 })
}

/// Where `done` working minutes end within `segs` (the Stop), and where the rest starts (the
/// Resume: the next part when the work done ends exactly at a pause).
fn stop_and_resume(
    segs: &[(NaiveDateTime, NaiveDateTime, Minutes)],
    cal: &WorkCal,
    done: Minutes,
) -> Option<(NaiveDateTime, NaiveDateTime)> {
    for (k, &(s, f, before)) in segs.iter().enumerate() {
        let next = segs.get(k + 1);
        let len = next.map_or(Minutes::MAX, |n| n.2.saturating_sub(before));
        if done.saturating_sub(before) < len {
            let stop = cal.add(s, done.saturating_sub(before).max(0));
            return Some((stop, cal.snap_forward(stop)));
        }
        if let Some(&(ns, _, nb)) = next
            && done == nb
        {
            return Some((f, ns));
        }
    }
    segs.last().map(|&(_, f, _)| (f, f))
}

/// Working minutes of `segs` done by `upto`.
pub fn work_done_by(segs: &[(NaiveDateTime, NaiveDateTime)], cal: &WorkCal, upto: NaiveDateTime) -> Minutes {
    segs.iter().filter(|(s, _)| *s < upto).map(|&(s, f)| cal.between(s, f.min(upto)).max(0)).fold(0, Minutes::saturating_add)
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
    // Fall back to the task/project calendar only when the resources share no working time at
    // all. Search well past a long absence (a month of leave at the project start must delay
    // the task, not drop the resource's calendar); the search stops at the first working day.
    let any = p.start.date().iter_days().take(RESOURCE_CALENDAR_SEARCH_DAYS).any(|d| wc.is_working_day(d));
    if any { wc } else { WorkCal::single(base) }
}

/// How far ahead [`calendar_for`] looks for working time shared by a task's resources.
const RESOURCE_CALENDAR_SEARCH_DAYS: usize = 3 * 366;

/// What makes two tasks' scheduling calendars identical (see [`calendar_for`]): the task
/// calendar and, when resource calendars apply, the assigned work resources.
type CalKey = (Option<CalendarId>, Vec<ResourceId>);

fn cal_key(p: &Project, task: &Task, resources: &[&Resource]) -> CalKey {
    let mut ids: Vec<ResourceId> = if p.settings.use_resource_calendars && !task.ignore_resource_calendars {
        resources.iter().filter(|r| r.kind == ResourceType::Work).map(|r| r.id).collect()
    } else {
        Vec::new()
    };
    ids.sort();
    ids.dedup();
    (task.calendar, ids)
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
    /// Distinct scheduling calendars; tasks with the same calendar and resources share one, so
    /// its cached spans serve all of them.
    cals: Vec<WorkCal<'a>>,
    /// Index into `cals` for each task.
    cal_of: Vec<usize>,
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
    /// Working parts of split tasks (by task index).
    segments: HashMap<usize, Vec<(NaiveDateTime, NaiveDateTime)>>,
    /// Per task: the latest Start No Earlier Than date and the earliest deadline set on its
    /// ancestor summaries (both apply to every subtask).
    inherited: Vec<Inherited>,
}

#[derive(Clone, Copy, Debug, Default)]
struct Inherited {
    snet: Option<NaiveDateTime>,
    deadline: Option<NaiveDateTime>,
}

/// Constraints a summary passes to its subtasks: its Start No Earlier Than date drives their
/// starts, and its deadline bounds their late finish (and so their slack).
fn inherited_from_summaries(p: &Project, parent: &[Option<usize>]) -> Vec<Inherited> {
    let mut out = vec![Inherited::default(); p.tasks.len()];
    // Parents come before their children, so each parent's value is final when a child reads it.
    for i in 0..p.tasks.len() {
        let Some(pi) = parent.get(i).copied().flatten() else { continue };
        let (Some(par), Some(up)) = (p.tasks.get(pi), out.get(pi).copied()) else { continue };
        // An inactive summary passes on what its own summaries set, not its own date or deadline.
        let own_snet = par.constraint.date.filter(|_| par.active && par.constraint.kind == ConstraintKind::Snet);
        let own_deadline = par.deadline.filter(|_| par.active);
        let v = Inherited { snet: up.snet.max(own_snet), deadline: min_opt(up.deadline, own_deadline) };
        if let Some(slot) = out.get_mut(i) {
            *slot = v;
        }
    }
    out
}

fn min_opt(a: Option<NaiveDateTime>, b: Option<NaiveDateTime>) -> Option<NaiveDateTime> {
    match (a, b) {
        (Some(x), Some(y)) => Some(x.min(y)),
        (x, None) => x,
        (None, y) => y,
    }
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
        let mut cals: Vec<WorkCal<'a>> = Vec::new();
        let mut cal_keys: HashMap<CalKey, usize> = HashMap::new();
        let cal_of = p
            .tasks
            .iter()
            .map(|t| {
                let resources = assigned.get(&t.id).map(Vec::as_slice).unwrap_or_default();
                *cal_keys.entry(cal_key(p, t, resources)).or_insert_with(|| {
                    cals.push(calendar_for(p, t, resources));
                    cals.len().saturating_sub(1)
                })
            })
            .collect();
        let parent = parents(p);
        let inherited = inherited_from_summaries(p, &parent);
        Self {
            p,
            proj_cal: project_calendar(p),
            cals,
            cal_of,
            leaves,
            is_leaf,
            parent,
            position: p.tasks.iter().enumerate().map(|(i, t)| (t.id, i)).collect(),
            edges: Vec::new(),
            incoming: vec![Vec::new(); n],
            outgoing: vec![Vec::new(); n],
            order: Vec::new(),
            warnings: Vec::new(),
            dates: vec![Dates::default(); n],
            segments: HashMap::new(),
            inherited,
        }
    }

    fn inherited(&self, i: usize) -> Inherited {
        self.inherited.get(i).copied().unwrap_or_default()
    }

    /// Whether a backward-pass or slack calculation should follow `edge` from leaf `i`.
    ///
    /// An inactive successor does not affect how other tasks are scheduled. A link from a
    /// summary's start constrains only the subtask that sets that start (the earliest), not
    /// every subtask, or all of them would get the same late start and negative slack.
    /// `group_start[e]` is the earliest start of a summary-start edge's group (see
    /// [`Engine::summary_group_starts`]).
    fn edge_constrains(&self, e: usize, edge: &Edge, i: usize, group_start: &[Option<NaiveDateTime>]) -> bool {
        if self.task(edge.succ).is_none_or(|t| !t.active) {
            return false;
        }
        match &edge.pred {
            PredSource::SummaryStart(_) => {
                let earliest = group_start.get(e).copied().flatten();
                earliest.is_some() && self.date(i).es == earliest
            }
            _ => true,
        }
    }

    /// The earliest start within each summary-start edge's group (early dates are final once the
    /// forward pass is done), so each edge's group is scanned once per pass, not once per leaf.
    fn summary_group_starts(&self) -> Vec<Option<NaiveDateTime>> {
        self.edges
            .iter()
            .map(|edge| match &edge.pred {
                PredSource::SummaryStart(group) => group.iter().filter_map(|&g| self.date(g).es).min(),
                _ => None,
            })
            .collect()
    }

    fn task(&self, i: usize) -> Option<&'a Task> {
        self.p.tasks.get(i)
    }

    fn leaf(&self, i: usize) -> bool {
        self.is_leaf.get(i).copied().unwrap_or(false)
    }

    fn cal(&self, i: usize) -> &WorkCal<'a> {
        self.cal_of.get(i).and_then(|&c| self.cals.get(c)).unwrap_or(&self.proj_cal)
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

    fn set_parts(&mut self, i: usize, parts: Option<Parts>) {
        if let Some(d) = self.dates.get_mut(i) {
            d.parts = parts;
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

    /// The leaf itself, or a summary's active leaf subtasks (inactive ones drive nothing).
    fn leaf_descendants(&self, idx: usize) -> Vec<usize> {
        if self.leaf(idx) {
            return vec![idx];
        }
        self.p.descendants_range(idx).filter(|&i| self.leaf(i) && self.task(i).is_some_and(|t| t.active)).collect()
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
            // Working time from start to finish: the duration plus any planned splits.
            let span = if t.mode == ScheduleMode::Auto { span_of(t, dur) } else { dur };

            let place = |start: NaiveDateTime| -> (NaiveDateTime, NaiveDateTime) {
                if dur > 0 {
                    let s = cal.snap_forward(start);
                    (s, cal.add(s, span))
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
                    LinkType::FF => cal.sub(apply_lag(&self.proj_cal, pf, edge.lag, pdur), span),
                    LinkType::SF => cal.sub(apply_lag(&self.proj_cal, ps, edge.lag, pdur), span),
                };
                if cand_start > es {
                    es = cand_start;
                    driven_by_link = true;
                }
            }

            // A Start No Earlier Than date on a summary drives its subtasks' starts.
            if let Some(snet) = self.inherited(i).snet {
                es = es.max(snet);
            }

            let mut warnings = Vec::new();
            if let Some(date) = t.constraint.date.filter(|_| t.constraint.kind.needs_date()) {
                let finish_of = |s: NaiveDateTime| place(s).1;
                match t.constraint.kind {
                    ConstraintKind::Snet => es = es.max(date),
                    ConstraintKind::Fnet => es = es.max(cal.sub(date, span)),
                    ConstraintKind::Mso => {
                        if warn && driven_by_link && place(es).0 > place(date).0 {
                            warnings.push(format!("'{}': links require a later start than its Must Start On date", t.name));
                        }
                        es = date;
                    }
                    ConstraintKind::Mfo => {
                        let s = if dur > 0 { cal.sub(date, span) } else { date };
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
                            es = if dur > 0 { cal.sub(date, span) } else { date };
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
            let parts = parts_of(t, cal, dur);
            if let Some(a) = t.actual_start {
                s = a;
                f = match parts {
                    Some(pt) => pt.finish,
                    None if dur > 0 => cal.add(a, span),
                    None => a,
                };
            }
            if let Some(af) = t.actual_finish {
                if t.actual_start.is_none() {
                    s = if dur > 0 { cal.sub(af, span) } else { af };
                }
                f = af.max(s);
            }
            let segments: Vec<(NaiveDateTime, NaiveDateTime)> = match (parts, t.actual_start, t.actual_finish) {
                (Some(pt), Some(a), _) if pt.split => {
                    segments_of(a, &t.splits, &t.gaps, cal, dur).into_iter().map(|(s, f, _)| (s, f)).collect()
                }
                // Planned splits in a task not under way (or finished as planned): its parts from its start.
                (None, _, None) if span > dur => {
                    let segs = segments_of(s, &[], &t.gaps, cal, dur);
                    if segs.len() > 1 { segs.into_iter().map(|(s, f, _)| (s, f)).collect() } else { Vec::new() }
                }
                _ => Vec::new(),
            };
            self.warnings.extend(warnings);
            self.set_parts(i, parts);
            if segments.is_empty() {
                self.segments.remove(&i);
            } else {
                self.segments.insert(i, segments);
            }
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
        let group_start = self.summary_group_starts();
        for i in self.order.clone().into_iter().rev() {
            let Some(t) = self.task(i) else { continue };
            let cal = self.cal(i);
            let dur = self.duration_of(i);
            let span = if t.mode == ScheduleMode::Auto { span_of(t, dur) } else { dur };
            let (Some(es), Some(ef)) = (self.date(i).es, self.date(i).ef) else {
                continue;
            };

            if t.mode == ScheduleMode::Manual || t.actual_finish.is_some() {
                self.set_late(i, es, ef);
                continue;
            }

            let mut lf = project_finish.max(ef);
            // Deadlines (the task's own and its summaries') limit the late finish, so they
            // reduce total slack; they don't move the task.
            if let Some(d) = min_opt(t.deadline, self.inherited(i).deadline) {
                lf = lf.min(d);
            }
            for &e in self.outgoing(i) {
                let Some(edge) = self.edges.get(e) else { continue };
                if !self.edge_constrains(e, edge, i, &group_start) {
                    continue;
                }
                let (Some(sls), Some(slf)) = (self.date(edge.succ).ls, self.date(edge.succ).lf) else {
                    continue;
                };
                let cand_lf = match edge.kind {
                    LinkType::FS => unapply_lag(&self.proj_cal, sls, edge.lag, dur),
                    LinkType::FF => unapply_lag(&self.proj_cal, slf, edge.lag, dur),
                    LinkType::SS => cal.add(unapply_lag(&self.proj_cal, sls, edge.lag, dur), span),
                    LinkType::SF => cal.add(unapply_lag(&self.proj_cal, slf, edge.lag, dur), span),
                };
                lf = lf.min(cand_lf);
            }
            if let Some(date) = t.constraint.date.filter(|_| t.constraint.kind.needs_date()) {
                match t.constraint.kind {
                    ConstraintKind::Snlt => lf = lf.min(if dur > 0 { cal.add(date, span) } else { date }),
                    ConstraintKind::Fnlt => lf = lf.min(date),
                    ConstraintKind::Mso | ConstraintKind::Mfo => lf = ef,
                    _ => {}
                }
            }
            let lf = cal.snap_backward(lf);
            // A split task's last part is what can still move: its Late Start is that part's.
            let ls = match self.date(i).parts {
                Some(pt) if pt.split => cal.sub(lf, pt.last.1),
                _ if dur > 0 => cal.sub(lf, span),
                _ => lf,
            };
            self.set_late(i, ls, lf);
        }
    }

    /// Total and free slack of every leaf (indexed like the tasks).
    fn slack(&self) -> (Vec<Minutes>, Vec<Minutes>) {
        let n = self.p.tasks.len();
        let mut total_slack = vec![0; n];
        let mut free_slack = vec![0; n];
        let group_start = self.summary_group_starts();
        for &i in &self.order {
            let d = self.date(i);
            let cal = self.cal(i);
            let (Some(es), Some(ef), Some(ls), Some(lf)) = (d.es, d.ef, d.ls, d.lf) else {
                continue;
            };
            let ts = match d.parts {
                // The start of a split task is fixed; only its remaining part can slip.
                Some(pt) if pt.split => cal.between(ef, lf),
                _ => cal.between(es, ls).min(cal.between(ef, lf)),
            };
            let dur = self.duration_of(i);
            let mut fs = Minutes::MAX;
            for &e in self.outgoing(i) {
                let Some(edge) = self.edges.get(e) else { continue };
                if !self.edge_constrains(e, edge, i, &group_start) {
                    continue;
                }
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
        // A summary's work is its subtasks' (plus any assignments of its own), never an estimate.
        if !has_work_assignment
            && !t.sched.is_summary
            && let Some(est) = &t.estimate
        {
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

    let (dates, order_warn, total_slack, free_slack, mut segments) = {
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
        (eng.dates, eng.warnings, total_slack, free_slack, eng.segments)
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
        t.sched.stop = d.parts.map(|pt| pt.stop);
        t.sched.resume = d.parts.map(|pt| pt.resume);
        t.sched.parts = segments.remove(&i).unwrap_or_default();
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

    // Summaries' own fixed costs and assignments (active summaries only), counted once at the
    // project level on top of the leaves.
    let own = rollup_summaries(p);

    // Project-level rollup.
    let leaves: Vec<&Task> = p.tasks.iter().filter(|t| !t.sched.is_summary && t.active).collect();
    let start = leaves.iter().filter_map(|t| t.sched.start).min();
    let finish = leaves.iter().filter_map(|t| t.sched.finish).max();
    let work = leaves.iter().fold(own.work, |acc: Minutes, t| acc.saturating_add(t.sched.work));
    let actual = leaves.iter().fold(own.actual, |acc: Minutes, t| acc.saturating_add(t.sched.actual_work));
    let cost: f64 = own.cost + leaves.iter().map(|t| t.sched.cost).sum::<f64>();
    let (num, den) = leaves.iter().fold((0.0f64, 0.0f64), |(n, d), t| {
        let w = percent_weight(t);
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
    /// (weighted progress, weight) over all subtasks below, for the parent's rollup.
    weight: (f64, f64),
}

/// Weight of a subtask in % complete rollups: its duration (a milestone counts as one minute),
/// so a summary's % complete is the duration-weighted progress of all subtasks below it.
fn percent_weight(t: &Task) -> f64 {
    if t.is_milestone() { 1.0 } else { t.duration.max(1) as f64 }
}

/// Work, actual work and cost a summary carries itself (fixed cost and its own assignments).
#[derive(Clone, Copy, Default)]
struct Own {
    work: Minutes,
    actual: Minutes,
    cost: f64,
}

/// `weights[c]` is the (weighted progress, weight) of task `c`: its own for a subtask, the sum
/// over its subtasks for a summary (so nested summaries weigh by subtask durations, not span).
fn rollup_of(p: &Project, children: &[usize], own: Own, weights: &[(f64, f64)]) -> Rollup {
    let kids: Vec<(usize, &Task)> = children.iter().filter_map(|&c| p.tasks.get(c).map(|t| (c, t))).collect();
    let active: Vec<(usize, &Task)> = kids.iter().copied().filter(|(_, t)| t.active).collect();
    let pairs = if active.is_empty() { &kids } else { &active };
    let src: Vec<&Task> = pairs.iter().map(|(_, t)| *t).collect();
    let (num, den) = pairs.iter().fold((0.0f64, 0.0f64), |(nn, d), (c, _)| {
        let (wn, wd) = weights.get(*c).copied().unwrap_or_default();
        (nn + wn, d + wd)
    });
    let start = src.iter().filter_map(|t| t.sched.start).min();
    let finish = src.iter().filter_map(|t| t.sched.finish).max();
    Rollup {
        start,
        finish,
        es: src.iter().filter_map(|t| t.sched.early_start).min(),
        ef: src.iter().filter_map(|t| t.sched.early_finish).max(),
        ls: src.iter().filter_map(|t| t.sched.late_start).min(),
        lf: src.iter().filter_map(|t| t.sched.late_finish).max(),
        work: src.iter().fold(own.work, |acc: Minutes, t| acc.saturating_add(t.sched.work)),
        actual: src.iter().fold(own.actual, |acc: Minutes, t| acc.saturating_add(t.sched.actual_work)),
        cost: src.iter().map(|t| t.sched.cost).sum::<f64>() + own.cost,
        slack: src.iter().map(|t| t.sched.total_slack).min().unwrap_or(0),
        free: src.iter().map(|t| t.sched.free_slack).min().unwrap_or(0),
        critical: src.iter().any(|t| t.sched.critical),
        percent: if den > 0.0 { (num / den) as f32 } else { 0.0 },
        weight: (num, den),
    }
}

/// Rolls subtasks up into their summaries. Returns the summaries' own work and cost (fixed costs
/// and assignments on summary rows), totalled over active summaries for the project level.
fn rollup_summaries(p: &mut Project) -> Own {
    let parent = parents(p);
    let mut total_own = Own::default();
    // Leaves weigh themselves; summaries get the sum of their children below.
    let mut weights: Vec<(f64, f64)> = p
        .tasks
        .iter()
        .map(|t| if t.sched.is_summary { (0.0, 0.0) } else { (percent_weight(t) * f64::from(t.sched.percent_complete), percent_weight(t)) })
        .collect();
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
        // Before the rollup overwrites them, `sched.work/actual_work/cost` hold the summary's own
        // values (rollup_tracking: its assignments and fixed cost). Indenting a task under one
        // with assignments must not lose that work.
        let own = Own { work: t.sched.work, actual: t.sched.actual_work, cost: t.sched.cost };
        if t.active {
            total_own.work = total_own.work.saturating_add(own.work);
            total_own.actual = total_own.actual.saturating_add(own.actual);
            total_own.cost += own.cost;
        }
        let r = rollup_of(p, kids, own, &weights);
        if let Some(w) = weights.get_mut(i) {
            *w = r.weight;
        }
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
        t.sched.stop = None;
        t.sched.resume = None;
        t.sched.parts.clear();
        t.sched.warnings.clear();
        if let (Some(dl), Some(f)) = (t.deadline, r.finish)
            && f > dl
        {
            t.sched.warnings.push(format!("Finishes after its deadline ({})", dl.format("%Y-%m-%d")));
        }
    }
    total_own
}
