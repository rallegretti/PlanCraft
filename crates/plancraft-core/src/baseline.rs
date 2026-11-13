//! Baselines and interim plans.
//!
//! A plan keeps up to eleven baselines (Baseline, Baseline1 … Baseline10). Each one is a snapshot of
//! every task's start, finish, duration, work and cost. A plan also keeps up to ten interim plans
//! (Start1/Finish1 … Start10/Finish10), which hold dates only.
//!
//! - Variance is measured against Baseline (number 0).
//! - Earned value is measured against the baseline chosen in the project settings.
//! - Saving into a baseline for selected tasks can roll the new values up into their summary tasks.
//!
//! The behaviour follows Microsoft's published description of Set Baseline and Clear Baseline; the
//! worked examples are in `tests/ms_parity.rs`.

use std::collections::{BTreeMap, HashSet};

use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};

use crate::calendar::WorkCal;
use crate::duration::Minutes;
use crate::ids::TaskId;
use crate::model::{Baseline, Interim, Project, Task};
use crate::ops::{PlanError, PlanResult};

/// The highest baseline number (Baseline10).
pub const MAX_BASELINE: u8 = 10;
/// The highest interim plan number (Start10/Finish10).
pub const MAX_INTERIM: u8 = 10;

/// Reads a map keyed by baseline or interim plan number. A key that isn't a number from 0 to
/// 255 is kept as 255, so opening the file drops it with a note instead of refusing the file.
pub fn lenient_slots<'de, D, T>(d: D) -> Result<BTreeMap<u8, T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    let raw: BTreeMap<String, T> = BTreeMap::deserialize(d)?;
    Ok(raw.into_iter().map(|(k, v)| (k.trim().parse::<u8>().unwrap_or(u8::MAX), v)).collect())
}

/// Where plan values are copied from or into.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PlanSlot {
    /// The current schedule (only ever a source).
    Scheduled,
    /// Baseline `0..=10` (0 is the unnumbered Baseline).
    Baseline(u8),
    /// Interim plan `1..=10` (the Start*n* and Finish*n* fields).
    Interim(u8),
}

impl PlanSlot {
    /// Display name: "Baseline", "Baseline3", "Start3/Finish3" or "Start/Finish".
    pub fn name(self) -> String {
        match self {
            PlanSlot::Scheduled => "Start/Finish".into(),
            PlanSlot::Baseline(0) => "Baseline".into(),
            PlanSlot::Baseline(n) => format!("Baseline{n}"),
            PlanSlot::Interim(n) => format!("Start{n}/Finish{n}"),
        }
    }

    /// Short key used by the MCP tools: "scheduled", "baseline", "baseline3" or "interim3".
    pub fn key(self) -> String {
        match self {
            PlanSlot::Scheduled => "scheduled".into(),
            PlanSlot::Baseline(0) => "baseline".into(),
            PlanSlot::Baseline(n) => format!("baseline{n}"),
            PlanSlot::Interim(n) => format!("interim{n}"),
        }
    }

    /// Parses a key (case and spaces ignored). Also accepts "baseline0", "current", "start3" and
    /// "start3/finish3". Returns `None` for unknown names and out-of-range numbers.
    pub fn parse(s: &str) -> Option<PlanSlot> {
        let k: String = s.chars().filter(|c| !c.is_whitespace() && *c != '_' && *c != '-').collect::<String>().to_lowercase();
        let k = k.split('/').next().unwrap_or_default();
        let num = |rest: &str| -> Option<u8> {
            if rest.is_empty() || rest.len() > 2 || !rest.bytes().all(|b| b.is_ascii_digit()) { None } else { rest.parse().ok() }
        };
        let slot = match k {
            "scheduled" | "current" | "start" | "startfinish" => PlanSlot::Scheduled,
            "baseline" => PlanSlot::Baseline(0),
            _ => {
                if let Some(rest) = k.strip_prefix("baseline") {
                    PlanSlot::Baseline(num(rest)?)
                } else {
                    PlanSlot::Interim(num(k.strip_prefix("interim").or_else(|| k.strip_prefix("start"))?)?)
                }
            }
        };
        slot.is_valid().then_some(slot)
    }

    pub fn is_valid(self) -> bool {
        match self {
            PlanSlot::Scheduled => true,
            PlanSlot::Baseline(n) => n <= MAX_BASELINE,
            PlanSlot::Interim(n) => (1..=MAX_INTERIM).contains(&n),
        }
    }

    /// Every baseline, Baseline first.
    pub fn baselines() -> impl Iterator<Item = PlanSlot> {
        (0..=MAX_BASELINE).map(PlanSlot::Baseline)
    }

    /// Every interim plan, 1 first.
    pub fn interim_plans() -> impl Iterator<Item = PlanSlot> {
        (1..=MAX_INTERIM).map(PlanSlot::Interim)
    }
}

/// Which tasks a baseline operation covers.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Scope {
    /// The selected tasks; empty means the entire project.
    pub tasks: Vec<TaskId>,
    /// Roll the selected tasks' new baseline values up into all their summary tasks.
    pub roll_up_to_summaries: bool,
    /// Recompute the baseline of each selected summary task from its subtasks' baselines.
    pub roll_up_from_subtasks: bool,
}

impl Scope {
    pub fn entire_project() -> Self {
        Self::default()
    }

    pub fn selected(tasks: Vec<TaskId>) -> Self {
        Self { tasks, ..Self::default() }
    }
}

/// What one baseline or interim plan holds across the plan.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PlanInfo {
    pub slot: PlanSlot,
    /// Tasks that have a value in this slot.
    pub tasks: usize,
    /// When the baseline was last saved (baselines only).
    pub saved: Option<NaiveDateTime>,
    pub start: Option<NaiveDateTime>,
    pub finish: Option<NaiveDateTime>,
    /// Total cost and work, each counted once at the highest outline level that has a value
    /// (baselines only).
    pub cost: f64,
    pub work: Minutes,
}

impl Task {
    /// The task's value in baseline `n` (0 is Baseline).
    pub fn baseline_in(&self, n: u8) -> Option<&Baseline> {
        if n == 0 { self.baseline.as_ref() } else { self.baselines.get(&n) }
    }

    /// The task's interim plan `n` (1–10).
    pub fn interim_in(&self, n: u8) -> Option<&Interim> {
        self.interim.get(&n)
    }

    /// The start and finish dates the task has in `slot`.
    pub fn dates_in(&self, slot: PlanSlot) -> Option<(NaiveDateTime, NaiveDateTime)> {
        match slot {
            PlanSlot::Scheduled => self.sched.start.zip(self.sched.finish),
            PlanSlot::Baseline(n) => self.baseline_in(n).map(|b| (b.start, b.finish)),
            PlanSlot::Interim(n) => self.interim_in(n).map(|i| (i.start, i.finish)),
        }
    }

    fn put_baseline(&mut self, n: u8, value: Option<Baseline>) {
        match (n, value) {
            (0, v) => self.baseline = v,
            (n, Some(b)) => {
                self.baselines.insert(n, b);
            }
            (n, None) => {
                self.baselines.remove(&n);
            }
        }
    }

    fn put_interim(&mut self, n: u8, value: Option<Interim>) {
        match value {
            Some(v) => {
                self.interim.insert(n, v);
            }
            None => {
                self.interim.remove(&n);
            }
        }
    }

    fn snapshot(&self, saved: NaiveDateTime) -> Option<Baseline> {
        let (start, finish) = self.sched.start.zip(self.sched.finish)?;
        Some(Baseline { start, finish, duration: self.sched.duration, work: self.sched.work, cost: self.sched.cost, saved })
    }
}

/// The current local time to the minute (a baseline's "saved on" stamp).
fn now_minute() -> NaiveDateTime {
    use chrono::Timelike;
    let now = chrono::Local::now().naive_local();
    now.with_second(0).and_then(|n| n.with_nanosecond(0)).unwrap_or(now)
}

fn invalid<T>(msg: impl Into<String>) -> PlanResult<T> {
    Err(PlanError::Invalid(msg.into()))
}

/// The parent of every task (by row), in one pass.
fn parents(p: &Project) -> Vec<Option<usize>> {
    let mut stack: Vec<(u16, usize)> = Vec::new();
    let mut out = Vec::with_capacity(p.tasks.len());
    for (i, t) in p.tasks.iter().enumerate() {
        while stack.last().is_some_and(|&(lvl, _)| lvl >= t.outline_level) {
            stack.pop();
        }
        out.push(stack.last().map(|&(_, idx)| idx));
        stack.push((t.outline_level, i));
    }
    out
}

impl Project {
    /// Rows of `ids` (all rows when empty); an unknown id is an error.
    fn scope_rows(&self, ids: &[TaskId]) -> PlanResult<Option<HashSet<usize>>> {
        if ids.is_empty() {
            return Ok(None);
        }
        ids.iter().map(|&id| self.task_index(id).ok_or(PlanError::TaskNotFound(id))).collect::<PlanResult<HashSet<_>>>().map(Some)
    }

    /// Saves the current schedule into baseline `n` (0–10) for the tasks in `scope`.
    ///
    /// Returns the number of tasks written.
    pub fn set_baseline_in(&mut self, n: u8, scope: &Scope) -> PlanResult<usize> {
        self.save_plan(PlanSlot::Scheduled, PlanSlot::Baseline(n), scope)
    }

    /// Copies plan values from `copy` into `into` for the tasks in `scope`.
    ///
    /// - Copying the schedule into a baseline saves the task's start, finish, duration, work and
    ///   cost. Copying it into an interim plan saves only the start and finish.
    /// - Copying one baseline into another copies every value. Copying a baseline into an interim
    ///   plan copies only its dates.
    /// - A task with no value in the source baseline or interim plan loses its value in the
    ///   target, so the target ends up matching the source. A source with no values at all for the
    ///   tasks in scope is refused: the copy would only erase the target.
    /// - An interim plan holds no duration, work or cost, so it can't be copied into a baseline.
    /// - The roll-up options in `scope` apply only when saving the schedule into a baseline for
    ///   selected tasks.
    ///
    /// Returns the number of tasks written, summaries updated by a roll-up included (each task
    /// counted once).
    pub fn save_plan(&mut self, copy: PlanSlot, into: PlanSlot, scope: &Scope) -> PlanResult<usize> {
        if !copy.is_valid() || !into.is_valid() {
            return invalid("baselines are numbered 0 to 10 and interim plans 1 to 10");
        }
        if into == PlanSlot::Scheduled {
            return invalid("the current schedule can't be overwritten; copy into a baseline or an interim plan");
        }
        if copy == into {
            return invalid(format!("{} can't be copied into itself", copy.name()));
        }
        if matches!((copy, into), (PlanSlot::Interim(_), PlanSlot::Baseline(_))) {
            return invalid("an interim plan holds only start and finish dates, so it can't be copied into a baseline");
        }
        let rows = self.scope_rows(&scope.tasks)?;
        let in_scope = |i: usize| rows.as_ref().is_none_or(|r| r.contains(&i));
        if copy != PlanSlot::Scheduled && !self.tasks.iter().enumerate().any(|(i, t)| in_scope(i) && t.dates_in(copy).is_some()) {
            let which = if rows.is_some() { " for these tasks" } else { "" };
            return invalid(format!("{} holds no values{which}, so there is nothing to copy into {}", copy.name(), into.name()));
        }
        let now = now_minute();
        let mut written: HashSet<usize> = HashSet::new();
        for (i, t) in self.tasks.iter_mut().enumerate() {
            if rows.as_ref().is_some_and(|r| !r.contains(&i)) {
                continue;
            }
            let wrote = match into {
                PlanSlot::Baseline(n) => {
                    let value = match copy {
                        PlanSlot::Scheduled => match t.snapshot(now) {
                            Some(b) => Some(b),
                            // Nothing scheduled to save: keep what the baseline had.
                            None => continue,
                        },
                        PlanSlot::Baseline(m) => t.baseline_in(m).map(|b| Baseline { saved: now, ..*b }),
                        PlanSlot::Interim(_) => continue,
                    };
                    let wrote = value.is_some();
                    t.put_baseline(n, value);
                    wrote
                }
                PlanSlot::Interim(n) => {
                    let value = match copy {
                        PlanSlot::Scheduled => match t.dates_in(copy) {
                            Some(d) => Some(d),
                            None => continue,
                        },
                        other => t.dates_in(other),
                    };
                    let wrote = value.is_some();
                    t.put_interim(n, value.map(|(start, finish)| Interim { start, finish }));
                    wrote
                }
                PlanSlot::Scheduled => false,
            };
            if wrote {
                written.insert(i);
            }
        }
        if let (PlanSlot::Scheduled, PlanSlot::Baseline(n), Some(rows)) = (copy, into, rows) {
            written.extend(self.roll_up_baseline(n, &rows, scope, now));
        }
        Ok(written.len())
    }

    /// Applies the roll-up options after a baseline was saved for `rows`. Returns the rows of the
    /// summaries updated.
    ///
    /// A summary's baseline is built the way the scheduler builds its schedule: from its active
    /// subtasks (all of them when none is active), plus the summary's own work and cost
    /// (assignments and fixed cost on the summary row).
    fn roll_up_baseline(&mut self, n: u8, rows: &HashSet<usize>, scope: &Scope, now: NaiveDateTime) -> Vec<usize> {
        if !scope.roll_up_to_summaries && !scope.roll_up_from_subtasks {
            return Vec::new();
        }
        let parent = parents(self);
        let mut roll = vec![false; self.tasks.len()];
        let mut children: Vec<Vec<usize>> = vec![Vec::new(); self.tasks.len()];
        for (i, p) in parent.iter().enumerate() {
            if let Some(kids) = p.and_then(|p| children.get_mut(p)) {
                kids.push(i);
            }
        }
        for &i in rows {
            if scope.roll_up_from_subtasks
                && children.get(i).is_some_and(|k| !k.is_empty())
                && let Some(r) = roll.get_mut(i)
            {
                *r = true;
            }
            if scope.roll_up_to_summaries {
                let mut up = parent.get(i).copied().flatten();
                while let Some(pi) = up {
                    match roll.get_mut(pi) {
                        // Already marked by this walk from another task: its ancestors are too.
                        Some(r) if *r && !rows.contains(&pi) => break,
                        Some(r) => *r = true,
                        None => break,
                    }
                    up = parent.get(pi).copied().flatten();
                }
            }
        }
        let cal = self.base_calendar().clone();
        let pc = WorkCal::single(&cal);
        let mut updated = Vec::new();
        // Deepest rows last in the list, so walking backwards finishes subtasks before their summary.
        for i in (0..self.tasks.len()).rev() {
            if !roll.get(i).copied().unwrap_or(false) {
                continue;
            }
            let Some(summary) = self.tasks.get(i) else { continue };
            let kids: Vec<&Task> = children.get(i).into_iter().flatten().filter_map(|&k| self.tasks.get(k)).collect();
            let active: Vec<&Task> = kids.iter().copied().filter(|t| t.active).collect();
            let src = if active.is_empty() { kids } else { active };
            // The summary's own work and cost: its scheduled totals less what its subtasks add.
            let own_work = src.iter().fold(summary.sched.work, |w, t| w.saturating_sub(t.sched.work)).max(0);
            let own_cost = summary.sched.cost - src.iter().map(|t| t.sched.cost).sum::<f64>();
            let own_cost = if own_cost.is_finite() { own_cost } else { 0.0 };
            let mut acc: Option<(NaiveDateTime, NaiveDateTime, Minutes, f64)> = None;
            for b in src.iter().filter_map(|t| t.baseline_in(n)) {
                acc = Some(match acc {
                    None => (b.start, b.finish, b.work, b.cost),
                    Some((s, f, w, c)) => (s.min(b.start), f.max(b.finish), w.saturating_add(b.work), c + b.cost),
                });
            }
            let Some((start, finish, work, cost)) = acc else { continue };
            let duration = pc.between(start, finish).max(0);
            let value = Baseline { start, finish, duration, work: work.saturating_add(own_work), cost: cost + own_cost, saved: now };
            if let Some(t) = self.tasks.get_mut(i) {
                t.put_baseline(n, Some(value));
                updated.push(i);
            }
        }
        updated
    }

    /// Clears a baseline or interim plan from the given tasks (all tasks when `ids` is empty).
    /// Returns the number of tasks that had a value.
    pub fn clear_plan(&mut self, which: PlanSlot, ids: &[TaskId]) -> PlanResult<usize> {
        if which == PlanSlot::Scheduled || !which.is_valid() {
            return invalid("choose a baseline (0 to 10) or an interim plan (1 to 10) to clear");
        }
        let rows = self.scope_rows(ids)?;
        let mut cleared = 0;
        for (i, t) in self.tasks.iter_mut().enumerate() {
            if rows.as_ref().is_some_and(|r| !r.contains(&i)) {
                continue;
            }
            match which {
                PlanSlot::Baseline(n) => {
                    cleared += usize::from(t.baseline_in(n).is_some());
                    t.put_baseline(n, None);
                }
                PlanSlot::Interim(n) => {
                    cleared += usize::from(t.interim.remove(&n).is_some());
                }
                PlanSlot::Scheduled => {}
            }
        }
        Ok(cleared)
    }

    /// Chooses the baseline (0–10) earned value is measured against.
    pub fn set_earned_value_baseline(&mut self, n: u8) -> PlanResult<()> {
        if n > MAX_BASELINE {
            return invalid("baselines are numbered 0 to 10");
        }
        self.settings.earned_value_baseline = n;
        Ok(())
    }

    /// What `slot` holds across the plan.
    pub fn plan_info(&self, slot: PlanSlot) -> PlanInfo {
        let mut info = PlanInfo { slot, tasks: 0, saved: None, start: None, finish: None, cost: 0.0, work: 0 };
        for t in &self.tasks {
            let Some((s, f)) = t.dates_in(slot) else { continue };
            info.tasks += 1;
            info.start = Some(info.start.map_or(s, |v| v.min(s)));
            info.finish = Some(info.finish.map_or(f, |v| v.max(f)));
            if let PlanSlot::Baseline(n) = slot
                && let Some(b) = t.baseline_in(n)
            {
                info.saved = Some(info.saved.map_or(b.saved, |v| v.max(b.saved)));
            }
        }
        if let PlanSlot::Baseline(n) = slot {
            let (cost, work) = baseline_totals(self, n);
            info.cost = cost;
            info.work = work;
        }
        info
    }
}

/// Total cost and work of baseline `n`: each task counted once at the highest level that has a
/// value (a summary's baseline already includes its subtasks and its own fixed cost).
pub fn baseline_totals(p: &Project, n: u8) -> (f64, Minutes) {
    // (outline level, a task on the path to here has a value)
    let mut stack: Vec<(u16, bool)> = Vec::new();
    let (mut cost, mut work) = (0.0, 0 as Minutes);
    for t in &p.tasks {
        while stack.last().is_some_and(|&(lvl, _)| lvl >= t.outline_level) {
            stack.pop();
        }
        let covered = stack.last().is_some_and(|&(_, c)| c);
        let b = t.baseline_in(n);
        if !covered && let Some(b) = b {
            cost += b.cost;
            work = work.saturating_add(b.work);
        }
        stack.push((t.outline_level, covered || b.is_some()));
    }
    (cost, work)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    use crate::prelude::*;

    fn plan() -> (Project, TaskId, TaskId, TaskId) {
        let mut p = Project::new("B", NaiveDate::from_ymd_opt(2026, 10, 19).unwrap());
        let phase = p.add_task("Phase", None);
        let a = p.add_task("A", None);
        let b = p.add_task("B", None);
        p.indent(a).unwrap();
        p.indent(b).unwrap();
        p.link(a.into(), b, LinkType::FS, Lag::None).unwrap();
        recalculate(&mut p);
        (p, phase, a, b)
    }

    #[test]
    fn slots_parse_and_name() {
        assert_eq!(PlanSlot::parse("Baseline"), Some(PlanSlot::Baseline(0)));
        assert_eq!(PlanSlot::parse("baseline0"), Some(PlanSlot::Baseline(0)));
        assert_eq!(PlanSlot::parse("Baseline 10"), Some(PlanSlot::Baseline(10)));
        assert_eq!(PlanSlot::parse("start3/finish3"), Some(PlanSlot::Interim(3)));
        assert_eq!(PlanSlot::parse("interim_1"), Some(PlanSlot::Interim(1)));
        assert_eq!(PlanSlot::parse("current"), Some(PlanSlot::Scheduled));
        for bad in ["baseline11", "interim0", "start11", "baseline+1", "baseline999", "", "x", "baseline1x"] {
            assert_eq!(PlanSlot::parse(bad), None, "{bad}");
        }
        for s in PlanSlot::baselines().chain(PlanSlot::interim_plans()).chain([PlanSlot::Scheduled]) {
            assert_eq!(PlanSlot::parse(&s.key()), Some(s));
            assert_eq!(PlanSlot::parse(&s.name()), Some(s));
        }
    }

    #[test]
    fn numbered_baselines_are_independent() {
        let (mut p, _, a, _) = plan();
        p.set_baseline_in(0, &Scope::entire_project()).unwrap();
        p.set_duration(a, 5 * 480, false).unwrap();
        recalculate(&mut p);
        assert_eq!(p.set_baseline_in(1, &Scope::entire_project()).unwrap(), 3);
        let t = p.task(a).unwrap();
        assert_eq!(t.baseline_in(0).unwrap().duration, 480);
        assert_eq!(t.baseline_in(1).unwrap().duration, 5 * 480);
        assert_eq!(p.clear_plan(PlanSlot::Baseline(1), &[]).unwrap(), 3);
        assert!(p.task(a).unwrap().baseline_in(1).is_none());
        assert!(p.task(a).unwrap().baseline_in(0).is_some());
    }

    #[test]
    fn bad_slots_and_tasks_are_errors() {
        let (mut p, ..) = plan();
        let all = Scope::entire_project();
        assert!(p.set_baseline_in(11, &all).is_err());
        assert!(p.save_plan(PlanSlot::Baseline(0), PlanSlot::Scheduled, &all).is_err());
        assert!(p.save_plan(PlanSlot::Baseline(2), PlanSlot::Baseline(2), &all).is_err());
        assert!(p.save_plan(PlanSlot::Interim(1), PlanSlot::Baseline(2), &all).is_err());
        assert!(p.save_plan(PlanSlot::Scheduled, PlanSlot::Interim(0), &all).is_err());
        assert!(p.set_baseline_in(1, &Scope::selected(vec![TaskId(999)])).is_err());
        assert!(p.clear_plan(PlanSlot::Scheduled, &[]).is_err());
        assert!(p.set_earned_value_baseline(11).is_err());
        assert!(p.tasks.iter().all(|t| t.baseline.is_none() && t.baselines.is_empty()));
    }

    /// Regression: a summary's baseline totals its subtasks, so a big plan's summary work went
    /// past the one-task limit and every Set Baseline was refused and rolled back.
    #[test]
    fn big_summaries_can_be_baselined() {
        use crate::workspace::{ChangeSource, Document};
        let mut p = Project::new("Big", NaiveDate::from_ymd_opt(2026, 10, 19).unwrap());
        let phase = p.add_task("Phase", None);
        let r = p.add_resource("Ana");
        for k in 0..12 {
            let t = p.add_task(format!("Part {k}"), None);
            if k == 0 {
                p.indent(t).unwrap();
            }
            p.set_duration(t, 2 * 260 * 480, false).unwrap();
            p.assign(t, r, Some(1.0), None).unwrap();
        }
        recalculate(&mut p);
        assert!(p.task(phase).unwrap().sched.work > crate::limits::MAX_MINUTES);
        let mut doc = Document::new(1, p, None);
        let n = doc.try_edit("Set Baseline", ChangeSource::User, |p| p.set_baseline_in(0, &Scope::entire_project())).unwrap();
        assert_eq!(n, 13);
        let first = doc.project.tasks[1].id;
        let up = Scope { tasks: vec![first], roll_up_to_summaries: true, ..Scope::default() };
        doc.try_edit("Roll up", ChangeSource::User, |p| p.set_baseline_in(1, &up)).unwrap();
        assert_eq!(doc.project.task(phase).unwrap().baseline_in(1).unwrap().work, 2 * 260 * 480);
    }

    /// Regression: a roll-up counted inactive subtasks and dropped the summary's own work and
    /// cost, where the scheduler leaves inactive subtasks out and keeps the summary's own.
    #[test]
    fn roll_ups_match_the_scheduler() {
        // Phase had a resource before it got subtasks; B is inactive.
        let mut p = Project::new("B", NaiveDate::from_ymd_opt(2026, 10, 19).unwrap());
        let phase = p.add_task("Phase", None);
        let r = p.add_resource("Ana");
        p.resource_mut(r).unwrap().standard_rate = 10.0;
        p.assign(phase, r, Some(1.0), None).unwrap();
        let a = p.add_task("A", None);
        p.indent(a).unwrap();
        let b = p.add_task("B", None);
        p.task_mut(a).unwrap().fixed_cost = 100.0;
        p.task_mut(b).unwrap().fixed_cost = 50.0;
        p.task_mut(b).unwrap().active = false;
        recalculate(&mut p);
        assert_eq!(p.parent_index(2), Some(0));
        assert!(p.task(phase).unwrap().sched.work > 0, "the summary keeps its own work");
        p.set_baseline_in(0, &Scope::entire_project()).unwrap();
        let whole = *p.task(phase).unwrap().baseline_in(0).unwrap();
        p.set_baseline_in(0, &Scope { tasks: vec![a], roll_up_to_summaries: true, ..Scope::default() }).unwrap();
        let rolled = *p.task(phase).unwrap().baseline_in(0).unwrap();
        assert_eq!((rolled.start, rolled.finish), (whole.start, whole.finish));
        assert_eq!((rolled.work, rolled.cost), (whole.work, whole.cost), "the same totals as the scheduler's");
    }

    /// Regression: a summary saved and then rolled up was counted twice.
    #[test]
    fn each_task_written_is_counted_once() {
        let (mut p, phase, a, b) = plan();
        let both = Scope { tasks: vec![phase, a], roll_up_to_summaries: true, roll_up_from_subtasks: true };
        assert_eq!(p.set_baseline_in(4, &both).unwrap(), 2);
        assert_eq!(p.set_baseline_in(4, &Scope { tasks: vec![b], roll_up_to_summaries: true, ..Scope::default() }).unwrap(), 2);
    }

    /// Regression: copying a plan that holds nothing erased the target and reported 0 tasks.
    #[test]
    fn copying_an_empty_plan_is_refused() {
        let (mut p, _, a, _) = plan();
        p.set_baseline_in(0, &Scope::entire_project()).unwrap();
        let all = Scope::entire_project();
        assert!(p.save_plan(PlanSlot::Baseline(5), PlanSlot::Baseline(0), &all).is_err());
        assert!(p.save_plan(PlanSlot::Interim(2), PlanSlot::Interim(1), &all).is_err());
        assert_eq!(p.plan_info(PlanSlot::Baseline(0)).tasks, 3, "Baseline is untouched");
        // A source with values for only some tasks still mirrors: the others lose theirs.
        p.set_baseline_in(5, &Scope::selected(vec![a])).unwrap();
        assert_eq!(p.save_plan(PlanSlot::Baseline(5), PlanSlot::Baseline(0), &all).unwrap(), 1);
        assert_eq!(p.plan_info(PlanSlot::Baseline(0)).tasks, 1);
    }

    /// Roll-ups belong to saving the schedule (Set Baseline), not to copies between plans.
    #[test]
    fn copies_between_baselines_do_not_roll_up() {
        let (mut p, phase, a, _) = plan();
        p.set_baseline_in(0, &Scope::entire_project()).unwrap();
        p.set_baseline_in(1, &Scope::selected(vec![a])).unwrap();
        let up = Scope { tasks: vec![a], roll_up_to_summaries: true, ..Scope::default() };
        assert_eq!(p.save_plan(PlanSlot::Baseline(1), PlanSlot::Baseline(2), &up).unwrap(), 1);
        assert!(p.task(phase).unwrap().baseline_in(2).is_none());
    }

    #[test]
    fn plan_info_totals_count_summaries_once() {
        let (mut p, phase, a, _) = plan();
        p.task_mut(a).unwrap().fixed_cost = 100.0;
        recalculate(&mut p);
        p.set_baseline_in(2, &Scope::entire_project()).unwrap();
        let info = p.plan_info(PlanSlot::Baseline(2));
        assert_eq!(info.tasks, 3);
        assert_eq!(info.cost, p.task(phase).unwrap().sched.cost);
        assert!(info.saved.is_some());
        assert_eq!(info.finish, p.sched.finish);
        assert_eq!(p.plan_info(PlanSlot::Baseline(3)).tasks, 0);
    }
}
