//! Editing operations shared by the desktop UI and the MCP server.
//!
//! All mutations of a [`Project`] that need to keep invariants (outline
//! structure, referential integrity, duration/work/units relationships) live
//! here.  Callers should run [`crate::schedule::recalculate`] afterwards.

use chrono::{NaiveDate, NaiveDateTime};
use std::collections::{HashMap, HashSet};

use crate::duration::{DurationUnit, Lag, Minutes, parse_duration};
use crate::ids::*;
use crate::limits::{
    MAX_ASSIGNMENTS, MAX_LINKS_PER_TASK, MAX_MINUTES, MAX_OUTLINE_LEVEL, MAX_RESOURCES, MAX_UNITS, clamp_f64, clamp_minutes,
};
use crate::model::*;

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum PlanError {
    #[error("task {0} not found")]
    TaskNotFound(TaskId),
    #[error("resource {0} not found")]
    ResourceNotFound(ResourceId),
    #[error("assignment {0} not found")]
    AssignmentNotFound(AssignmentId),
    #[error("calendar {0} not found")]
    CalendarNotFound(CalendarId),
    #[error("time entry {0} not found")]
    TimeEntryNotFound(TimeEntryId),
    #[error("external project {0} not found")]
    ExternalNotFound(String),
    #[error("linking these tasks would create a circular dependency")]
    CircularDependency,
    #[error("a task cannot be linked to itself or to its own summary")]
    SelfLink,
    #[error("{0}")]
    Invalid(String),
    /// A count, size or value is beyond what PlanCraft supports (see [`crate::limits`]).
    #[error("{0}")]
    Limit(String),
    /// A bug was caught (a panic); the change was rolled back.
    #[error("{0}")]
    Internal(String),
}

impl From<crate::guard::Panicked> for PlanError {
    fn from(p: crate::guard::Panicked) -> Self {
        PlanError::Internal(format!("PlanCraft hit an internal error and undid the change ({}). Please report this.", p.message))
    }
}

pub type PlanResult<T> = Result<T, PlanError>;

/// How "Update work as complete through" sets progress.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ProgressUpdate {
    /// Each task gets the % complete its schedule says it should have (0-100%).
    #[default]
    Scheduled,
    /// Tasks that finish by the date become 100% complete; others are left alone.
    ZeroOrHundred,
}

/// What a tracking update (Update Project, Mark on Track) changed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TrackingReport {
    /// Tasks whose % complete went up.
    pub updated: Vec<TaskId>,
    /// In-progress tasks whose remaining work now resumes later.
    pub split: Vec<TaskId>,
    /// Tasks not started that got a Start No Earlier Than constraint.
    pub constrained: Vec<TaskId>,
    /// Tasks left alone, and why.
    pub skipped: Vec<(TaskId, String)>,
}

impl TrackingReport {
    /// Number of tasks changed.
    pub fn changed(&self) -> usize {
        self.updated.len() + self.split.len() + self.constrained.len()
    }
}

fn invalid<T>(msg: impl Into<String>) -> PlanResult<T> {
    Err(PlanError::Invalid(msg.into()))
}

impl Project {
    fn require_task(&self, id: TaskId) -> PlanResult<usize> {
        self.task_index(id).ok_or(PlanError::TaskNotFound(id))
    }

    /// The task at row `idx` (an internal error when out of range).
    fn row(&self, idx: usize) -> PlanResult<&Task> {
        self.tasks.get(idx).ok_or_else(|| PlanError::Internal(format!("row {idx} is out of range")))
    }

    fn row_mut(&mut self, idx: usize) -> PlanResult<&mut Task> {
        self.tasks.get_mut(idx).ok_or_else(|| PlanError::Internal(format!("row {idx} is out of range")))
    }

    fn level_at(&self, idx: usize) -> Option<u16> {
        self.tasks.get(idx).map(|t| t.outline_level)
    }

    // ------------------------------------------------------------------
    // Outline
    // ------------------------------------------------------------------

    /// Inserts a new task at `index` (clamped) with the given outline level.
    ///
    /// The level is clamped so that the outline stays valid (at most one deeper
    /// than the previous row).
    pub fn insert_task(&mut self, index: usize, mut task: Task) -> TaskId {
        let index = index.min(self.tasks.len());
        let max_level = index.checked_sub(1).and_then(|i| self.level_at(i)).map_or(1, |l| l.saturating_add(1).min(MAX_OUTLINE_LEVEL));
        task.outline_level = task.outline_level.clamp(1, max_level);
        if task.mode == ScheduleMode::Auto && !self.settings.new_tasks_auto && task.manual_start.is_none() {
            task.mode = ScheduleMode::Manual;
        }
        let id = task.id;
        self.tasks.insert(index, task);
        id
    }

    /// Creates a task and inserts it; `None` appends at the end at level 1.
    pub fn add_task(&mut self, name: impl Into<String>, index: Option<usize>) -> TaskId {
        let id = self.ids.next_task();
        let index = index.unwrap_or(self.tasks.len());
        // Inherit the level of the row above, unless it is a summary (then become its child).
        let level = match index.checked_sub(1).filter(|_| index <= self.tasks.len()).and_then(|i| Some((i, self.level_at(i)?))) {
            Some((above, level)) if self.has_children(above) => level.saturating_add(1),
            Some((_, level)) => level,
            None => 1,
        };
        let mut t = Task::new(id, name, self.settings.time.minutes_per_day);
        t.outline_level = level;
        self.insert_task(index, t)
    }

    /// Creates a milestone and inserts it.
    pub fn add_milestone(&mut self, name: impl Into<String>, index: Option<usize>) -> TaskId {
        let id = self.add_task(name, index);
        if let Some(t) = self.task_mut(id) {
            t.kind = TaskKind::Milestone;
            t.duration = 0;
            t.estimated = false;
        }
        id
    }

    /// Inserts a summary task above the given contiguous tasks and indents them under it.
    pub fn add_summary_over(&mut self, name: impl Into<String>, ids: &[TaskId]) -> PlanResult<TaskId> {
        let mut idxs: Vec<usize> = ids.iter().map(|&id| self.require_task(id)).collect::<PlanResult<_>>()?;
        idxs.sort_unstable();
        let first = *idxs.first().ok_or_else(|| PlanError::Invalid("no tasks selected".into()))?;
        let level = self.row(first)?.outline_level;
        let sid = self.ids.next_task();
        let mut t = Task::new(sid, name, self.settings.time.minutes_per_day);
        t.outline_level = level;
        self.tasks.insert(first, t);
        // Indent the selected blocks (shifted by one).
        for id in ids {
            self.indent(*id)?;
        }
        Ok(sid)
    }

    /// Deletes tasks (and their subtasks), cleaning up every reference to them.
    pub fn delete_tasks(&mut self, ids: &[TaskId]) -> PlanResult<Vec<TaskId>> {
        let mut doomed: HashSet<TaskId> = HashSet::new();
        for &id in ids {
            let idx = self.require_task(id)?;
            doomed.insert(id);
            let range = self.descendants_range(idx);
            doomed.extend(self.tasks.get(range).unwrap_or_default().iter().map(|t| t.id));
        }
        self.tasks.retain(|t| !doomed.contains(&t.id));
        for t in &mut self.tasks {
            t.predecessors.retain(|d| d.predecessor.local().is_none_or(|p| !doomed.contains(&p)));
            t.members.retain(|m| m.local().is_none_or(|p| !doomed.contains(&p)));
        }
        self.assignments.retain(|a| !doomed.contains(&a.task));
        self.time_entries.retain(|e| !doomed.contains(&e.task));
        self.normalize_outline();
        Ok(doomed.into_iter().collect())
    }

    /// Ensures the first task is level 1 and no task is more than one level deeper than its predecessor row.
    pub fn normalize_outline(&mut self) {
        let mut prev = 0u16;
        for t in &mut self.tasks {
            t.outline_level = t.outline_level.clamp(1, prev.saturating_add(1).min(MAX_OUTLINE_LEVEL));
            prev = t.outline_level;
        }
    }

    /// Shifts the outline level of rows `range` by `delta` (+1 or -1).
    fn shift_levels(&mut self, range: std::ops::Range<usize>, deeper: bool) {
        for t in self.tasks.get_mut(range).unwrap_or_default() {
            t.outline_level = if deeper { t.outline_level.saturating_add(1) } else { t.outline_level.saturating_sub(1).max(1) };
        }
    }

    /// Demotes a task (and its subtasks) one outline level.
    pub fn indent(&mut self, id: TaskId) -> PlanResult<()> {
        let idx = self.require_task(id)?;
        let Some(above_idx) = idx.checked_sub(1) else {
            return invalid("the first task cannot be indented");
        };
        let above = self.row(above_idx)?.outline_level;
        let level = self.row(idx)?.outline_level;
        if level > above {
            return invalid("task is already a subtask of the row above");
        }
        let range = idx..self.descendants_range(idx).end;
        let deepest = self.tasks.get(range.clone()).unwrap_or_default().iter().map(|t| t.outline_level).max().unwrap_or(level);
        if deepest >= MAX_OUTLINE_LEVEL {
            return invalid(format!("tasks can be nested at most {MAX_OUTLINE_LEVEL} levels deep"));
        }
        let row_above = self.row_mut(above_idx)?;
        if row_above.is_milestone() && level == above {
            // A milestone becoming a summary would be confusing; convert it to a task.
            row_above.kind = TaskKind::Task;
        }
        self.shift_levels(range, true);
        Ok(())
    }

    /// Promotes a task (and its subtasks) one outline level.
    pub fn outdent(&mut self, id: TaskId) -> PlanResult<()> {
        let idx = self.require_task(id)?;
        if self.row(idx)?.outline_level <= 1 {
            return invalid("task is already at the top level");
        }
        let range = idx..self.descendants_range(idx).end;
        self.shift_levels(range, false);
        Ok(())
    }

    /// Moves a task with its subtasks so that it starts at `new_index` in the current ordering.
    pub fn move_task(&mut self, id: TaskId, new_index: usize) -> PlanResult<()> {
        let idx = self.require_task(id)?;
        let range = idx..self.descendants_range(idx).end;
        let block: Vec<Task> = self.tasks.drain(range).collect();
        let len = block.len();
        let base_level = block.first().map_or(1, |t| t.outline_level);
        let mut target = new_index.min(self.tasks.len() + len);
        if target > idx {
            target = target.saturating_sub(len);
        }
        let target = target.min(self.tasks.len());
        let max_level = target.checked_sub(1).and_then(|i| self.level_at(i)).map_or(1, |l| l.saturating_add(1));
        let new_level = base_level.min(max_level);
        let delta = i32::from(base_level) - i32::from(new_level);
        for (k, mut t) in block.into_iter().enumerate() {
            t.outline_level = u16::try_from((i32::from(t.outline_level) - delta).max(1)).unwrap_or(1);
            self.tasks.insert(target + k, t);
        }
        self.normalize_outline();
        Ok(())
    }

    /// Moves a task block one row up or down among its siblings.
    pub fn shift_task(&mut self, id: TaskId, up: bool) -> PlanResult<()> {
        let idx = self.require_task(id)?;
        let level = self.row(idx)?.outline_level;
        if up {
            let before = self.tasks.get(..idx).unwrap_or_default();
            let prev_sibling = before
                .iter()
                .enumerate()
                .rev()
                .take_while(|(_, t)| t.outline_level >= level)
                .find(|(_, t)| t.outline_level == level)
                .map(|(i, _)| i);
            match prev_sibling {
                Some(p) => self.move_task(id, p),
                None => invalid("task is already first in its group"),
            }
        } else {
            let end = self.descendants_range(idx).end;
            if self.level_at(end) != Some(level) {
                return invalid("task is already last in its group");
            }
            let next_end = self.descendants_range(end).end;
            self.move_task(id, next_end)
        }
    }

    // ------------------------------------------------------------------
    // Dependencies
    // ------------------------------------------------------------------

    /// Whether `to` is reachable from `from` following successor links (local only, summaries
    /// expanded: a link to or from a summary relates to its whole subtree, and a summary's links
    /// apply to all of its subtasks).
    ///
    /// The successor lists and the outline (parents, subtree ends) are built once per call, so the
    /// search is linear in tasks plus links.
    fn reaches(&self, from: TaskId, to: TaskId) -> bool {
        let n = self.tasks.len();
        let index: HashMap<TaskId, usize> = self.tasks.iter().enumerate().map(|(i, t)| (t.id, i)).collect();
        let (Some(&from_i), Some(&to_i)) = (index.get(&from), index.get(&to)) else {
            return false;
        };
        // Parent of each row and the end of its subtree (exclusive), from one pass over levels.
        let mut parent = vec![None; n];
        let mut end = vec![n; n];
        let mut stack: Vec<usize> = Vec::new();
        for (i, t) in self.tasks.iter().enumerate() {
            while let Some(&top) = stack.last() {
                if self.tasks.get(top).is_some_and(|s| s.outline_level >= t.outline_level) {
                    if let Some(e) = end.get_mut(top) {
                        *e = i;
                    }
                    stack.pop();
                } else {
                    break;
                }
            }
            if let Some(slot) = parent.get_mut(i) {
                *slot = stack.last().copied();
            }
            stack.push(i);
        }
        // Successors of each row (as predecessor index -> successor indices).
        let mut succs: Vec<Vec<usize>> = vec![Vec::new(); n];
        for (si, t) in self.tasks.iter().enumerate() {
            for pid in t.predecessors.iter().map(|d| d.predecessor).chain(t.members.iter().copied()).filter_map(|r| r.local()) {
                if let Some(list) = index.get(&pid).and_then(|&pi| succs.get_mut(pi)) {
                    list.push(si);
                }
            }
        }
        let subtree = |i: usize| i..end.get(i).copied().unwrap_or(i.saturating_add(1));
        // `to` is in the group of `i` when it is `i`, inside `i`'s subtree, or one of its ancestors.
        let in_group = |i: usize| subtree(i).contains(&to_i) || subtree(to_i).contains(&i);

        let mut visited = vec![false; n];
        // Rows whose successors were already queued (groups overlap; each row is expanded once).
        let mut expanded = vec![false; n];
        let mut queue = vec![from_i];
        while let Some(cur) = queue.pop() {
            match visited.get_mut(cur) {
                Some(v) if !*v => *v = true,
                _ => continue,
            }
            if cur != from_i && in_group(cur) {
                return true;
            }
            let ancestors = std::iter::successors(parent.get(cur).copied().flatten(), |&a| parent.get(a).copied().flatten());
            for g in subtree(cur).chain(ancestors) {
                match expanded.get_mut(g) {
                    Some(e) if !*e => *e = true,
                    _ => continue,
                }
                for &s in succs.get(g).map(Vec::as_slice).unwrap_or_default() {
                    if s == to_i {
                        return true;
                    }
                    queue.push(s);
                }
            }
        }
        false
    }

    /// Adds (or updates) a dependency `pred -> succ`.
    pub fn link(&mut self, pred: TaskRef, succ: TaskId, kind: LinkType, lag: Lag) -> PlanResult<()> {
        let si = self.require_task(succ)?;
        if let TaskRef::Local(pid) = pred {
            let pi = self.require_task(pid)?;
            if pid == succ || self.descendants_range(pi).contains(&si) || self.descendants_range(si).contains(&pi) {
                return Err(PlanError::SelfLink);
            }
            if self.reaches(succ, pid) {
                return Err(PlanError::CircularDependency);
            }
        } else if let TaskRef::External { project, .. } = pred
            && self.external(project).is_none()
        {
            return Err(PlanError::ExternalNotFound(project.to_string()));
        }
        let t = self.row_mut(si)?;
        if let Some(d) = t.predecessors.iter_mut().find(|d| d.predecessor == pred) {
            d.kind = kind;
            d.lag = lag;
        } else if t.predecessors.len() >= MAX_LINKS_PER_TASK {
            return invalid(format!("a task can have at most {MAX_LINKS_PER_TASK} predecessors"));
        } else {
            t.predecessors.push(Dependency { predecessor: pred, kind, lag });
        }
        Ok(())
    }

    /// Links the tasks in order with Finish-to-Start links (like "Link the selected tasks").
    pub fn link_chain(&mut self, ids: &[TaskId]) -> PlanResult<()> {
        for w in ids.windows(2) {
            if let [a, b] = w {
                self.link(TaskRef::Local(*a), *b, LinkType::FS, Lag::None)?;
            }
        }
        Ok(())
    }

    /// Removes the dependency `pred -> succ`. Returns whether a link was removed.
    pub fn unlink(&mut self, pred: TaskRef, succ: TaskId) -> PlanResult<bool> {
        let si = self.require_task(succ)?;
        let t = self.row_mut(si)?;
        let before = t.predecessors.len();
        t.predecessors.retain(|d| d.predecessor != pred);
        Ok(before != t.predecessors.len())
    }

    /// Removes all links among the given tasks (and from/to them when only one task is given).
    pub fn unlink_tasks(&mut self, ids: &[TaskId]) {
        let set: HashSet<TaskId> = ids.iter().copied().collect();
        let single = ids.len() == 1;
        for t in &mut self.tasks {
            let succ_in = set.contains(&t.id);
            t.predecessors.retain(|d| {
                let pred_in = d.predecessor.local().is_some_and(|p| set.contains(&p));
                if single { !(pred_in || succ_in) } else { !(pred_in && succ_in) }
            });
        }
    }

    /// Successor ids of a task (local links only).
    pub fn successors(&self, id: TaskId) -> Vec<(TaskId, Dependency)> {
        self.tasks
            .iter()
            .flat_map(|t| t.predecessors.iter().filter(|d| d.predecessor == TaskRef::Local(id)).map(move |d| (t.id, *d)))
            .collect()
    }

    /// Formats the predecessors column, e.g. `"2,3SS+1 day,Website\5"`.
    pub fn format_predecessors(&self, id: TaskId) -> String {
        let Some(t) = self.task(id) else {
            return String::new();
        };
        t.predecessors
            .iter()
            .map(|d| {
                let base = match d.predecessor {
                    TaskRef::Local(p) => self.row_number(p).map(|r| r.to_string()).unwrap_or_else(|| "?".into()),
                    TaskRef::External { project, task } => {
                        let name = self.external(project).map(|e| e.name.clone()).unwrap_or_else(|| "?".into());
                        format!("{name}\\{}", task.0)
                    }
                };
                let kind = if d.kind == LinkType::FS && d.lag.is_zero() { String::new() } else { d.kind.code().to_string() };
                format!("{base}{kind}{}", d.lag.format(&self.settings.time))
            })
            .collect::<Vec<_>>()
            .join(",")
    }

    /// Parses predecessor text using row numbers (`"3"`, `"3FS+2d"`, `"4SS-50%"`, `"Website\12FF"`).
    pub fn parse_predecessors(&self, text: &str) -> PlanResult<Vec<Dependency>> {
        let mut out = Vec::new();
        for part in text.split([',', ';']) {
            let part = part.trim();
            if part.is_empty() {
                continue;
            }
            if out.len() >= MAX_LINKS_PER_TASK {
                return invalid(format!("a task can have at most {MAX_LINKS_PER_TASK} predecessors"));
            }
            let (pred, rest) = if let Some((proj, rest)) = part.split_once('\\') {
                let ext = self
                    .external_projects
                    .iter()
                    .find(|e| e.name.eq_ignore_ascii_case(proj.trim()) || e.id.to_string() == proj.trim())
                    .ok_or_else(|| PlanError::ExternalNotFound(proj.to_string()))?;
                let (digits, rest) = split_digits(rest);
                let tid: u32 = digits.parse().map_err(|_| PlanError::Invalid(format!("bad external task id in '{part}'")))?;
                (TaskRef::External { project: ext.id, task: TaskId(tid) }, rest)
            } else {
                let (digits, rest) = split_digits(part);
                let row: usize = digits.parse().map_err(|_| PlanError::Invalid(format!("'{part}' is not a valid predecessor")))?;
                let t = row
                    .checked_sub(1)
                    .and_then(|i| self.tasks.get(i))
                    .ok_or_else(|| PlanError::Invalid(format!("there is no task with ID {row}")))?;
                (TaskRef::Local(t.id), rest)
            };
            let rest = rest.trim();
            // `get` refuses to split inside a multi-byte character, so odd input can't panic.
            let (kind, lag_text) = match (rest.get(..2).and_then(LinkType::parse), rest.get(2..)) {
                (Some(kind), Some(lag)) => (kind, lag),
                _ => (LinkType::FS, rest),
            };
            let lag = Lag::parse(lag_text, &self.settings.time).ok_or_else(|| PlanError::Invalid(format!("invalid lag '{lag_text}'")))?;
            out.push(Dependency { predecessor: pred, kind, lag });
        }
        Ok(out)
    }

    /// Replaces all predecessors of a task, validating each link.
    pub fn set_predecessors(&mut self, id: TaskId, deps: Vec<Dependency>) -> PlanResult<()> {
        let idx = self.require_task(id)?;
        let old = std::mem::take(&mut self.row_mut(idx)?.predecessors);
        for d in &deps {
            if let Err(e) = self.link(d.predecessor, id, d.kind, d.lag) {
                if let Some(t) = self.task_mut(id) {
                    t.predecessors = old;
                }
                return Err(e);
            }
        }
        Ok(())
    }

    // ------------------------------------------------------------------
    // Milestones
    // ------------------------------------------------------------------

    /// Adds tasks to a milestone's member list.
    pub fn add_milestone_members(&mut self, milestone: TaskId, members: &[TaskRef]) -> PlanResult<()> {
        let mi = self.require_task(milestone)?;
        let m = self.row(mi)?;
        if !m.is_milestone() {
            return invalid(format!("'{}' is not a milestone", m.name));
        }
        for member in members {
            if let TaskRef::Local(id) = member {
                self.require_task(*id)?;
                if *id == milestone {
                    return Err(PlanError::SelfLink);
                }
                if self.reaches(milestone, *id) {
                    return Err(PlanError::CircularDependency);
                }
            }
            let t = self.row_mut(mi)?;
            if !t.members.contains(member) {
                if t.members.len() >= MAX_LINKS_PER_TASK {
                    return invalid(format!("a milestone can have at most {MAX_LINKS_PER_TASK} member tasks"));
                }
                t.members.push(*member);
            }
        }
        Ok(())
    }

    pub fn remove_milestone_members(&mut self, milestone: TaskId, members: &[TaskRef]) -> PlanResult<()> {
        let mi = self.require_task(milestone)?;
        self.row_mut(mi)?.members.retain(|m| !members.contains(m));
        Ok(())
    }

    /// Milestones that a task contributes to.
    pub fn milestones_of(&self, id: TaskId) -> Vec<TaskId> {
        self.tasks.iter().filter(|t| t.members.contains(&TaskRef::Local(id))).map(|t| t.id).collect()
    }

    // ------------------------------------------------------------------
    // Duration / work / units
    // ------------------------------------------------------------------

    /// Ids of the task's assignments to work resources.
    fn work_assignment_ids(&self, task: TaskId) -> Vec<AssignmentId> {
        self.assignments
            .iter()
            .filter(|a| a.task == task && self.resource(a.resource).is_some_and(|r| r.kind == ResourceType::Work))
            .map(|a| a.id)
            .collect()
    }

    fn assignment_mut(&mut self, id: AssignmentId) -> PlanResult<&mut Assignment> {
        self.assignments.iter_mut().find(|a| a.id == id).ok_or(PlanError::AssignmentNotFound(id))
    }

    /// Duration implied by the work assignments (longest assignment).
    fn duration_from_assignments(&self, task: TaskId) -> Option<Minutes> {
        self.assignments
            .iter()
            .filter(|a| a.task == task && self.resource(a.resource).is_some_and(|r| r.kind == ResourceType::Work))
            .map(|a| if a.units > 0.0 { clamp_minutes((a.work as f64 / a.units).round() as Minutes) } else { 0 })
            .max()
    }

    /// Sets a task's duration, adjusting assignments according to the task type.
    pub fn set_duration(&mut self, id: TaskId, minutes: Minutes, estimated: bool) -> PlanResult<()> {
        let idx = self.require_task(id)?;
        if self.has_children(idx) {
            return invalid("the duration of a summary task is calculated from its subtasks");
        }
        if minutes > MAX_MINUTES {
            return invalid("durations can be at most 5 years");
        }
        let minutes = minutes.max(0);
        let auto_milestone = self.settings.new_tasks_auto;
        let t = self.row_mut(idx)?;
        let task_type = t.task_type;
        t.duration = minutes;
        t.estimated = estimated;
        if minutes == 0 && t.kind == TaskKind::Task && auto_milestone {
            t.kind = TaskKind::Milestone;
        } else if minutes > 0 && t.kind == TaskKind::Milestone && t.members.is_empty() {
            t.kind = TaskKind::Task;
        }
        // A manual task's finish follows its new duration.
        if t.mode == ScheduleMode::Manual {
            t.manual_finish = None;
        }
        for aid in self.work_assignment_ids(id) {
            let a = self.assignment_mut(aid)?;
            match task_type {
                TaskType::FixedUnits | TaskType::FixedDuration => a.work = clamp_minutes((minutes as f64 * a.units).round() as Minutes),
                TaskType::FixedWork => {
                    if minutes > 0 {
                        a.units = a.work as f64 / minutes as f64;
                    }
                }
            }
        }
        Ok(())
    }

    /// Converts a task into a milestone or back.
    pub fn set_milestone(&mut self, id: TaskId, milestone: bool) -> PlanResult<()> {
        let idx = self.require_task(id)?;
        if milestone && self.has_children(idx) {
            return invalid("a summary task cannot be a milestone");
        }
        let mpd = self.settings.time.minutes_per_day;
        let t = self.row_mut(idx)?;
        if milestone {
            t.kind = TaskKind::Milestone;
            t.duration = 0;
            t.estimated = false;
        } else {
            t.kind = TaskKind::Task;
            if t.duration == 0 {
                t.duration = mpd;
            }
            t.members.clear();
        }
        Ok(())
    }

    // ------------------------------------------------------------------
    // Resources & assignments
    // ------------------------------------------------------------------

    pub fn add_resource(&mut self, name: impl Into<String>) -> ResourceId {
        let id = self.ids.next_resource();
        self.resources.push(Resource::new(id, name));
        id
    }

    /// Deletes resources and their assignments and time entries.
    pub fn delete_resources(&mut self, ids: &[ResourceId]) -> PlanResult<()> {
        for id in ids {
            self.resource(*id).ok_or(PlanError::ResourceNotFound(*id))?;
        }
        self.resources.retain(|r| !ids.contains(&r.id));
        self.assignments.retain(|a| !ids.contains(&a.resource));
        self.time_entries.retain(|e| !ids.contains(&e.resource));
        Ok(())
    }

    /// Spreads `total` work over the task's work assignments in proportion to their units.
    ///
    /// Fixed Duration tasks keep their duration and get new units; the others get a new duration.
    fn redistribute_work(&mut self, task: TaskId, total: Minutes) -> PlanResult<()> {
        let all = self.work_assignment_ids(task);
        let total_units: f64 = self.assignments.iter().filter(|a| all.contains(&a.id)).map(|a| a.units).sum();
        if total_units <= 0.0 {
            return Ok(());
        }
        let (task_type, duration) = self.task(task).map(|t| (t.task_type, t.duration)).unwrap_or_default();
        for aid in all {
            let a = self.assignment_mut(aid)?;
            a.work = clamp_minutes((total as f64 * a.units / total_units).round() as Minutes);
            if task_type == TaskType::FixedDuration && duration > 0 {
                a.units = a.work as f64 / duration as f64;
            }
        }
        if task_type != TaskType::FixedDuration {
            self.recompute_duration_from_work(task);
        }
        Ok(())
    }

    /// Assigns a resource to a task (or updates the existing assignment).
    ///
    /// * `units` defaults to 100% (or 1 unit of material).
    /// * `work` sets the assignment work explicitly; otherwise work follows the
    ///   task's duration, or its estimate when this is the first assignment of an
    ///   estimated task.
    /// * On effort-driven tasks (Fixed Work tasks always are), adding a person keeps the total
    ///   work and shares it out by units, like Microsoft Project.
    pub fn assign(&mut self, task: TaskId, resource: ResourceId, units: Option<f64>, work: Option<Minutes>) -> PlanResult<AssignmentId> {
        let ti = self.require_task(task)?;
        if self.has_children(ti) {
            return invalid("resources cannot be assigned to summary tasks");
        }
        let kind = self.resource(resource).ok_or(PlanError::ResourceNotFound(resource))?.kind;
        let units = clamp_f64(units.unwrap_or(1.0), 0.0, MAX_UNITS);
        if let Some(w) = work
            && !(0..=MAX_MINUTES).contains(&w)
        {
            return invalid("work can be at most 5 years");
        }

        if let Some(existing) = self.assignments.iter().find(|a| a.task == task && a.resource == resource) {
            let (aid, old_units) = (existing.id, existing.units);
            if units != old_units {
                self.set_assignment_units(aid, units)?;
            }
            if let Some(w) = work {
                self.set_assignment_work(aid, w)?;
            }
            return Ok(aid);
        }
        if self.assignments.len() >= MAX_ASSIGNMENTS {
            return invalid(format!("a project can have at most {MAX_ASSIGNMENTS} assignments"));
        }

        let id = self.ids.next_assignment();
        let mut a = Assignment { id, task, resource, units, work: 0, actual_work: 0, cost: 0.0, contour: WorkContour::Flat };
        if kind != ResourceType::Work {
            self.assignments.push(a);
            return Ok(id);
        }
        let existing_work = self.work_assignment_ids(task);
        let t = self.row(ti)?.clone();
        let by_duration = clamp_minutes((t.duration as f64 * units).round() as Minutes);
        if let Some(w) = work {
            a.work = w;
            self.assignments.push(a);
            self.recompute_duration_from_work(task);
        } else if existing_work.is_empty() {
            if let (Some(est), true) = (t.estimate, t.estimated || t.duration == 0) {
                // First assignment of an estimated task: the estimate drives work and duration.
                a.work = clamp_minutes(est.expected_work(&self.settings));
                self.assignments.push(a);
                self.recompute_duration_from_work(task);
                if let Some(t) = self.task_mut(task) {
                    t.estimated = false;
                }
            } else {
                a.work = by_duration;
                self.assignments.push(a);
            }
        } else if t.is_effort_driven() {
            // Effort driven: total work stays the same and is redistributed by units.
            let total =
                self.assignments.iter().filter(|a| existing_work.contains(&a.id)).fold(0, |acc: Minutes, a| acc.saturating_add(a.work));
            self.assignments.push(a);
            self.redistribute_work(task, total)?;
        } else {
            a.work = by_duration;
            self.assignments.push(a);
        }
        Ok(id)
    }

    fn recompute_duration_from_work(&mut self, task: TaskId) {
        if let Some(d) = self.duration_from_assignments(task)
            && let Some(t) = self.task_mut(task)
            && t.kind == TaskKind::Task
        {
            t.duration = d;
        }
    }

    /// Removes an assignment. On effort-driven tasks the remaining people take over its work
    /// (total work is unchanged), like Microsoft Project.
    pub fn unassign(&mut self, id: AssignmentId) -> PlanResult<()> {
        let pos = self.assignments.iter().position(|a| a.id == id).ok_or(PlanError::AssignmentNotFound(id))?;
        let removed = self.assignments.remove(pos);
        let effort_driven = self.task(removed.task).is_some_and(Task::is_effort_driven);
        let is_work = self.resource(removed.resource).is_some_and(|r| r.kind == ResourceType::Work);
        if effort_driven && is_work {
            let remaining = self.work_assignment_ids(removed.task);
            if !remaining.is_empty() {
                let total = self
                    .assignments
                    .iter()
                    .filter(|a| remaining.contains(&a.id))
                    .fold(removed.work, |acc: Minutes, a| acc.saturating_add(a.work));
                self.redistribute_work(removed.task, clamp_minutes(total))?;
            }
        }
        Ok(())
    }

    pub fn set_assignment_units(&mut self, id: AssignmentId, units: f64) -> PlanResult<()> {
        let units = clamp_f64(units, 0.0, MAX_UNITS);
        let (task, resource) = {
            let a = self.assignment_mut(id)?;
            (a.task, a.resource)
        };
        let tt = self.task(task).map(|t| t.task_type).unwrap_or_default();
        let dur = self.task(task).map(|t| t.duration).unwrap_or(0);
        let is_work = self.resource(resource).is_some_and(|r| r.kind == ResourceType::Work);
        let a = self.assignment_mut(id)?;
        a.units = units;
        if is_work {
            match tt {
                TaskType::FixedDuration => a.work = clamp_minutes((dur as f64 * units).round() as Minutes),
                TaskType::FixedUnits | TaskType::FixedWork => self.recompute_duration_from_work(task),
            }
        }
        Ok(())
    }

    pub fn set_assignment_work(&mut self, id: AssignmentId, work: Minutes) -> PlanResult<()> {
        if work > MAX_MINUTES {
            return invalid("work can be at most 5 years");
        }
        let task = self.assignment_mut(id)?.task;
        let tt = self.task(task).map(|t| t.task_type).unwrap_or_default();
        let dur = self.task(task).map(|t| t.duration).unwrap_or(0);
        let a = self.assignment_mut(id)?;
        a.work = work.max(0);
        match tt {
            TaskType::FixedDuration if dur > 0 => a.units = a.work as f64 / dur as f64,
            _ => self.recompute_duration_from_work(task),
        }
        Ok(())
    }

    /// Resource names assigned to a task, e.g. `"Ana[50%],Build Co"`.
    pub fn format_resource_names(&self, task: TaskId) -> String {
        self.assignments_for_task(task)
            .filter_map(|a| {
                let r = self.resource(a.resource)?;
                Some(match r.kind {
                    ResourceType::Work if (a.units - 1.0).abs() > 1e-6 => {
                        format!("{}[{}%]", r.name, (a.units * 100.0).round())
                    }
                    ResourceType::Material => {
                        format!("{}[{} {}]", r.name, trim_f(a.units), r.material_label)
                    }
                    _ => r.name.clone(),
                })
            })
            .collect::<Vec<_>>()
            .join(",")
    }

    /// Applies resource-name text like `"Ana[50%], Bob"`; unknown names create new resources.
    pub fn apply_resource_names(&mut self, task: TaskId, text: &str) -> PlanResult<()> {
        let mut wanted: Vec<(ResourceId, f64)> = Vec::new();
        for part in text.split(',') {
            let part = part.trim();
            if part.is_empty() {
                continue;
            }
            let (name, units) = match part.split_once('[') {
                Some((n, rest)) => {
                    let inner = rest.trim_end_matches(']').trim();
                    let num: String = inner.chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
                    let v: f64 = num.parse().unwrap_or(100.0);
                    let units = if inner.contains('%') { v / 100.0 } else { v };
                    (n.trim(), units)
                }
                None => (part, 1.0),
            };
            if name.is_empty() {
                continue;
            }
            let rid = match self.resource_by_name(name) {
                Some(r) => r.id,
                None if self.resources.len() >= MAX_RESOURCES => {
                    return invalid(format!("a project can have at most {MAX_RESOURCES} resources"));
                }
                None => self.add_resource(name),
            };
            wanted.push((rid, units));
        }
        let current: Vec<AssignmentId> =
            self.assignments_for_task(task).filter(|a| !wanted.iter().any(|(r, _)| *r == a.resource)).map(|a| a.id).collect();
        for a in current {
            self.unassign(a)?;
        }
        for (rid, units) in wanted {
            self.assign(task, rid, Some(units), None)?;
        }
        Ok(())
    }

    // ------------------------------------------------------------------
    // Progress & time tracking
    // ------------------------------------------------------------------

    /// Sets % complete, maintaining actual start/finish like Microsoft Project.
    pub fn set_percent_complete(&mut self, id: TaskId, pct: u8) -> PlanResult<()> {
        let idx = self.require_task(id)?;
        let pct = pct.min(100);
        if self.has_children(idx) {
            // Apply to all leaf descendants.
            let leaves: Vec<TaskId> =
                self.descendants_range(idx).filter(|&i| !self.has_children(i)).filter_map(|i| self.tasks.get(i)).map(|t| t.id).collect();
            for l in leaves {
                self.set_percent_complete(l, pct)?;
            }
            return Ok(());
        }
        let t = self.row_mut(idx)?;
        t.percent_complete = pct;
        if pct == 0 || pct == 100 {
            // Nothing left to resume.
            t.resume = None;
        }
        if pct == 0 {
            t.actual_start = None;
            t.actual_finish = None;
        } else {
            if t.actual_start.is_none() {
                t.actual_start = t.sched.start;
            }
            if pct == 100 {
                if t.actual_finish.is_none() {
                    t.actual_finish = t.sched.finish;
                }
            } else {
                t.actual_finish = None;
            }
        }
        Ok(())
    }

    /// Sets the status date: the date progress is reported as of (`None` = today).
    pub fn set_status_date(&mut self, date: Option<NaiveDate>) {
        self.status_date = date;
    }

    /// End of the working day `d` on the project calendar (17:00 on the standard calendar).
    pub fn end_of_working_day(&self, d: NaiveDate) -> NaiveDateTime {
        crate::calendar::WorkCal::single(self.base_calendar()).day_finish(d)
    }

    /// Leaf rows that a tracking update applies to: every active leaf when `ids` is empty,
    /// otherwise the given tasks (summaries stand for their subtasks), in row order.
    fn tracking_rows(&self, ids: &[TaskId]) -> PlanResult<Vec<usize>> {
        let mut rows = Vec::new();
        if ids.is_empty() {
            rows.extend((0..self.tasks.len()).filter(|&i| !self.has_children(i)));
        } else {
            for &id in ids {
                let idx = self.require_task(id)?;
                rows.extend(self.descendants_range(idx).chain(std::iter::once(idx)).filter(|&i| !self.has_children(i)));
            }
            rows.sort_unstable();
            rows.dedup();
        }
        rows.retain(|&i| self.tasks.get(i).is_some_and(|t| t.active));
        Ok(rows)
    }

    /// Microsoft Project's "Update work as complete through": brings each task's % complete up to
    /// where its schedule says it should be at `through` (`Scheduled`), or only marks the tasks
    /// that finish by then as 100% complete (`ZeroOrHundred`). Progress already entered is never
    /// lowered. `ids` empty = the entire project.
    pub fn update_progress_through(&mut self, ids: &[TaskId], through: NaiveDateTime, how: ProgressUpdate) -> PlanResult<TrackingReport> {
        crate::schedule::recalculate(self);
        let mut report = TrackingReport::default();
        for idx in self.tracking_rows(ids)? {
            let t = self.row(idx)?;
            let (Some(start), Some(finish)) = (t.sched.start, t.sched.finish) else { continue };
            let pct = if finish <= through {
                100
            } else if how == ProgressUpdate::ZeroOrHundred || through <= start || t.duration <= 0 || t.is_milestone() {
                0
            } else {
                let cal = crate::schedule::task_calendar(self, idx);
                let upto = through.min(finish);
                let done = match (t.sched.stop, t.sched.resume) {
                    // A split task does no work between its stop and resume dates.
                    (Some(stop), Some(resume)) if resume > cal.snap_forward(stop) => {
                        cal.between(start, upto.min(stop)).max(0) + if upto > resume { cal.between(resume, upto).max(0) } else { 0 }
                    }
                    _ => cal.between(start, upto).max(0),
                };
                let pct = done.saturating_mul(100).saturating_add(t.duration / 2) / t.duration.max(1);
                u8::try_from(pct.clamp(0, 99)).unwrap_or(99)
            };
            if pct > t.percent_complete {
                let id = t.id;
                self.set_percent_complete(id, pct)?;
                report.updated.push(id);
            }
        }
        Ok(report)
    }

    /// Microsoft Project's "Mark on Track": sets the tasks' % complete to what their schedule says
    /// at the status date (today when none is set), never lowering progress already entered.
    pub fn mark_on_track(&mut self, ids: &[TaskId]) -> PlanResult<TrackingReport> {
        if ids.is_empty() {
            return invalid("select the tasks to mark on track");
        }
        let at = crate::report::status_moment(self);
        self.update_progress_through(ids, at, ProgressUpdate::Scheduled)
    }

    /// Microsoft Project's "Reschedule uncompleted work to start after": work not done by `after`
    /// moves to start after it, and actual dates stay where they are. A task in progress is split
    /// (its remaining part resumes after `after`); a task not started gets a Start No Earlier
    /// Than `after` constraint. `ids` empty = the entire project.
    pub fn reschedule_uncompleted_work(&mut self, ids: &[TaskId], after: NaiveDateTime) -> PlanResult<TrackingReport> {
        crate::schedule::recalculate(self);
        let mut report = TrackingReport::default();
        for idx in self.tracking_rows(ids)? {
            let t = self.row(idx)?;
            let id = t.id;
            if t.is_complete() || t.actual_finish.is_some() {
                continue;
            }
            if t.mode == ScheduleMode::Manual {
                if t.sched.start.is_some_and(|s| s < after) {
                    report.skipped.push((id, "it is manually scheduled".into()));
                }
                continue;
            }
            let started = t.percent_complete > 0 || t.actual_start.is_some();
            if started {
                if t.percent_complete == 0 || t.sched.resume.is_none() {
                    // Started but no work done yet: the whole task moves.
                    if t.sched.start.is_some_and(|s| s < after) {
                        report.skipped.push((id, "it has an actual start but no progress".into()));
                    }
                    continue;
                }
                if t.sched.resume.is_some_and(|r| r < after) {
                    self.row_mut(idx)?.resume = Some(after);
                    report.split.push(id);
                }
                continue;
            }
            if !t.sched.start.is_some_and(|s| s < after) {
                continue;
            }
            match t.constraint.kind {
                ConstraintKind::Asap => {}
                ConstraintKind::Snet if t.constraint.date.is_none_or(|d| d < after) => {}
                kind => {
                    report.skipped.push((id, format!("its {} constraint was kept", kind.label())));
                    continue;
                }
            }
            self.row_mut(idx)?.constraint = Constraint { kind: ConstraintKind::Snet, date: Some(after) };
            report.constrained.push(id);
        }
        Ok(report)
    }

    /// The leaf task a duration-based progress entry applies to.
    fn progress_leaf(&self, id: TaskId, what: &str) -> PlanResult<usize> {
        let idx = self.require_task(id)?;
        if self.has_children(idx) {
            return invalid(format!("a summary task's {what} comes from its subtasks"));
        }
        if self.row(idx)?.is_milestone() {
            return invalid("a milestone has no duration; mark it 0% or 100% complete instead");
        }
        Ok(idx)
    }

    /// Enters progress as actual duration: the working time spent so far. % complete follows; an
    /// actual duration at or past the planned one stretches the duration to match and completes
    /// the task.
    pub fn set_actual_duration(&mut self, id: TaskId, minutes: Minutes) -> PlanResult<()> {
        let idx = self.progress_leaf(id, "actual duration")?;
        if !(0..=MAX_MINUTES).contains(&minutes) {
            return invalid("actual duration must be between 0 and 5 years");
        }
        let t = self.row(idx)?;
        let (dur, estimated) = (t.duration, t.estimated);
        if minutes >= dur && minutes > 0 {
            if minutes > dur {
                self.set_duration(id, minutes, estimated)?;
                // The actual finish is the new scheduled finish.
                crate::schedule::recalculate(self);
            }
            return self.set_percent_complete(id, 100);
        }
        let pct = percent_of(minutes, dur);
        self.set_percent_complete(id, pct)
    }

    /// Enters progress as remaining duration: the working time still needed. The duration becomes
    /// the time done so far plus `minutes`, and % complete follows.
    pub fn set_remaining_duration(&mut self, id: TaskId, minutes: Minutes) -> PlanResult<()> {
        let idx = self.progress_leaf(id, "remaining duration")?;
        if !(0..=MAX_MINUTES).contains(&minutes) {
            return invalid("remaining duration must be between 0 and 5 years");
        }
        let t = self.row(idx)?;
        let (done, estimated) = (t.actual_duration(), t.estimated);
        let total = done.saturating_add(minutes).min(MAX_MINUTES);
        if total == 0 {
            return invalid("a task needs some duration; enter remaining duration above zero");
        }
        let pct = if minutes == 0 { 100 } else { percent_of(done, total) };
        if total != t.duration {
            self.set_duration(id, total, estimated)?;
            if pct == 100 {
                // The actual finish is the new scheduled finish.
                crate::schedule::recalculate(self);
            }
        }
        self.set_percent_complete(id, pct)
    }

    /// Sets when the remaining work of an in-progress task resumes (`None` = straight after the
    /// completed part). A date after the completed part splits the task.
    pub fn set_resume(&mut self, id: TaskId, resume: Option<NaiveDateTime>) -> PlanResult<()> {
        let idx = self.require_task(id)?;
        if resume.is_some() {
            if self.has_children(idx) {
                return invalid("a summary task's dates come from its subtasks");
            }
            let t = self.row(idx)?;
            if t.is_milestone() || t.mode == ScheduleMode::Manual || !(1..100).contains(&t.percent_complete) {
                return invalid("only an automatically scheduled task that is 1-99% complete can resume later");
            }
        }
        self.row_mut(idx)?.resume = resume;
        Ok(())
    }

    /// Records time spent by a resource on a task.
    pub fn log_time(
        &mut self,
        task: TaskId,
        resource: ResourceId,
        date: NaiveDate,
        minutes: Minutes,
        note: impl Into<String>,
    ) -> PlanResult<TimeEntryId> {
        let ti = self.require_task(task)?;
        if self.has_children(ti) {
            return invalid("time cannot be logged against a summary task");
        }
        self.resource(resource).ok_or(PlanError::ResourceNotFound(resource))?;
        if minutes <= 0 {
            return invalid("logged time must be positive");
        }
        if minutes > MAX_MINUTES {
            return invalid("logged time can be at most 5 years");
        }
        let id = self.ids.next_time_entry();
        self.time_entries.push(TimeEntry { id, task, resource, date, minutes, note: note.into() });
        // Starting work on a task records its actual start.
        let start_of_day = self.base_cal_day_start(date);
        let t = self.row_mut(ti)?;
        if t.actual_start.is_none_or(|s| s > start_of_day) {
            t.actual_start = Some(start_of_day);
        }
        Ok(id)
    }

    fn base_cal_day_start(&self, date: NaiveDate) -> NaiveDateTime {
        crate::calendar::WorkCal::single(self.base_calendar()).day_start(date)
    }

    pub fn delete_time_entry(&mut self, id: TimeEntryId) -> PlanResult<()> {
        let pos = self.time_entries.iter().position(|e| e.id == id).ok_or(PlanError::TimeEntryNotFound(id))?;
        self.time_entries.remove(pos);
        Ok(())
    }

    /// Sets each task's % complete from tracked work (actual / planned), capped at 99% until finished.
    pub fn update_progress_from_time(&mut self) {
        crate::schedule::recalculate(self);
        for t in &mut self.tasks {
            if t.sched.is_summary || t.actual_finish.is_some() || t.sched.work <= 0 || t.sched.actual_work <= 0 {
                continue;
            }
            let pct = (t.sched.actual_work as f64 / t.sched.work as f64 * 100.0).floor().clamp(0.0, 99.0) as u8;
            t.percent_complete = pct.max(t.percent_complete);
        }
    }

    // ------------------------------------------------------------------
    // Baselines
    // ------------------------------------------------------------------

    /// Saves the current schedule as baseline (all tasks when `ids` is empty).
    pub fn set_baseline(&mut self, ids: &[TaskId]) {
        use chrono::Timelike;
        let now = chrono::Local::now().naive_local();
        let now = now.with_second(0).and_then(|n| n.with_nanosecond(0)).unwrap_or(now);
        for t in &mut self.tasks {
            if !ids.is_empty() && !ids.contains(&t.id) {
                continue;
            }
            if let (Some(start), Some(finish)) = (t.sched.start, t.sched.finish) {
                t.baseline =
                    Some(Baseline { start, finish, duration: t.sched.duration, work: t.sched.work, cost: t.sched.cost, saved: now });
            }
        }
    }

    pub fn clear_baseline(&mut self, ids: &[TaskId]) {
        for t in &mut self.tasks {
            if ids.is_empty() || ids.contains(&t.id) {
                t.baseline = None;
            }
        }
    }

    // ------------------------------------------------------------------
    // Calendars
    // ------------------------------------------------------------------

    /// Creates a new calendar copied from `from` (or the standard week).
    pub fn add_calendar(&mut self, name: impl Into<String>, from: Option<CalendarId>) -> PlanResult<CalendarId> {
        let id = self.ids.next_calendar();
        let mut cal = match from {
            Some(f) => self.calendar(f).ok_or(PlanError::CalendarNotFound(f))?.clone(),
            None => crate::calendar::Calendar::standard(id),
        };
        cal.id = id;
        cal.name = name.into();
        self.calendars.push(cal);
        Ok(id)
    }

    pub fn delete_calendar(&mut self, id: CalendarId) -> PlanResult<()> {
        if id == self.calendar {
            return invalid("the project calendar cannot be deleted");
        }
        self.calendar(id).ok_or(PlanError::CalendarNotFound(id))?;
        self.calendars.retain(|c| c.id != id);
        for t in &mut self.tasks {
            if t.calendar == Some(id) {
                t.calendar = None;
            }
        }
        for r in &mut self.resources {
            if r.calendar == Some(id) {
                r.calendar = None;
            }
        }
        Ok(())
    }

    /// Parses a date or date-time entered by a user: `2026-10-19`, `2026-10-19 13:00`, `10/19/2026`.
    ///
    /// Dates without time use the start (or end when `finish` is set) of the project's working day.
    pub fn parse_datetime(&self, text: &str, finish: bool) -> Option<NaiveDateTime> {
        let t = text.trim();
        for f in ["%Y-%m-%d %H:%M", "%Y-%m-%dT%H:%M", "%Y-%m-%dT%H:%M:%S", "%Y-%m-%d %H:%M:%S", "%m/%d/%Y %H:%M"] {
            if let Ok(dt) = NaiveDateTime::parse_from_str(t, f) {
                return Some(dt);
            }
        }
        let date = ["%Y-%m-%d", "%m/%d/%Y", "%m/%d/%y", "%a %m/%d/%y", "%d %b %Y", "%b %d %Y", "%b %d, %Y"]
            .iter()
            .find_map(|f| NaiveDate::parse_from_str(t, f).ok())?;
        let wc = crate::calendar::WorkCal::single(self.base_calendar());
        Some(if finish { wc.day_finish(date) } else { wc.day_start(date) })
    }

    /// Parses a duration with the project's default unit.
    pub fn parse_duration(&self, text: &str) -> Option<(Minutes, bool)> {
        parse_duration(text, self.settings.duration_unit, &self.settings.time)
    }

    pub fn format_duration(&self, minutes: Minutes, estimated: bool) -> String {
        crate::duration::format_duration(minutes, self.settings.duration_unit, &self.settings.time, estimated)
    }

    pub fn format_duration_unit(&self, minutes: Minutes, unit: DurationUnit) -> String {
        crate::duration::format_duration(minutes, unit, &self.settings.time, false)
    }
}

/// Splits leading ASCII digits off `s`.
fn split_digits(s: &str) -> (&str, &str) {
    let end = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    s.split_at(end)
}

fn trim_f(v: f64) -> String {
    if (v - v.round()).abs() < 1e-9 { format!("{}", v.round() as i64) } else { format!("{v:.2}") }
}

/// `part` of `whole` as a whole percentage, kept within 1-99% while the task is under way.
fn percent_of(part: Minutes, whole: Minutes) -> u8 {
    if part <= 0 {
        return 0;
    }
    let pct = part.saturating_mul(100).saturating_add(whole / 2) / whole.max(1);
    u8::try_from(pct.clamp(1, 99)).unwrap_or(99)
}
