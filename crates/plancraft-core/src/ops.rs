//! Editing operations shared by the desktop UI and the MCP server.
//!
//! All mutations of a [`Project`] that need to keep invariants (outline
//! structure, referential integrity, duration/work/units relationships) live
//! here.  Callers should run [`crate::schedule::recalculate`] afterwards.

use chrono::{NaiveDate, NaiveDateTime};
use std::collections::HashSet;

use crate::duration::{DurationUnit, Lag, Minutes, parse_duration};
use crate::ids::*;
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
}

pub type PlanResult<T> = Result<T, PlanError>;

fn invalid<T>(msg: impl Into<String>) -> PlanResult<T> {
    Err(PlanError::Invalid(msg.into()))
}

impl Project {
    fn require_task(&self, id: TaskId) -> PlanResult<usize> {
        self.task_index(id).ok_or(PlanError::TaskNotFound(id))
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
        let max_level = if index == 0 { 1 } else { self.tasks[index - 1].outline_level + 1 };
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
        let level = if index > 0 && index <= self.tasks.len() {
            // Inherit the level of the row above, unless it is a summary (then become its child).
            let above = &self.tasks[index - 1];
            if self.has_children(index - 1) { above.outline_level + 1 } else { above.outline_level }
        } else {
            1
        };
        let mut t = Task::new(id, name, self.settings.time.minutes_per_day);
        t.outline_level = level;
        self.insert_task(index, t)
    }

    /// Creates a milestone and inserts it.
    pub fn add_milestone(&mut self, name: impl Into<String>, index: Option<usize>) -> TaskId {
        let id = self.add_task(name, index);
        let t = self.task_mut(id).expect("just inserted");
        t.kind = TaskKind::Milestone;
        t.duration = 0;
        t.estimated = false;
        id
    }

    /// Inserts a summary task above the given contiguous tasks and indents them under it.
    pub fn add_summary_over(&mut self, name: impl Into<String>, ids: &[TaskId]) -> PlanResult<TaskId> {
        let mut idxs: Vec<usize> = ids.iter().map(|&id| self.require_task(id)).collect::<PlanResult<_>>()?;
        idxs.sort_unstable();
        let first = *idxs.first().ok_or_else(|| PlanError::Invalid("no tasks selected".into()))?;
        let level = self.tasks[first].outline_level;
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
            for d in self.descendants_range(idx) {
                doomed.insert(self.tasks[d].id);
            }
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
            t.outline_level = t.outline_level.clamp(1, prev + 1);
            prev = t.outline_level;
        }
    }

    /// Demotes a task (and its subtasks) one outline level.
    pub fn indent(&mut self, id: TaskId) -> PlanResult<()> {
        let idx = self.require_task(id)?;
        if idx == 0 {
            return invalid("the first task cannot be indented");
        }
        let above = self.tasks[idx - 1].outline_level;
        if self.tasks[idx].outline_level > above {
            return invalid("task is already a subtask of the row above");
        }
        if self.tasks[idx - 1].is_milestone() && self.tasks[idx].outline_level == above {
            // A milestone becoming a summary would be confusing; convert it to a task.
            self.tasks[idx - 1].kind = TaskKind::Task;
        }
        let range = self.descendants_range(idx);
        self.tasks[idx].outline_level += 1;
        for i in range {
            self.tasks[i].outline_level += 1;
        }
        Ok(())
    }

    /// Promotes a task (and its subtasks) one outline level.
    pub fn outdent(&mut self, id: TaskId) -> PlanResult<()> {
        let idx = self.require_task(id)?;
        if self.tasks[idx].outline_level <= 1 {
            return invalid("task is already at the top level");
        }
        let range = self.descendants_range(idx);
        self.tasks[idx].outline_level -= 1;
        for i in range {
            self.tasks[i].outline_level -= 1;
        }
        Ok(())
    }

    /// Moves a task with its subtasks so that it starts at `new_index` in the current ordering.
    pub fn move_task(&mut self, id: TaskId, new_index: usize) -> PlanResult<()> {
        let idx = self.require_task(id)?;
        let range = idx..self.descendants_range(idx).end;
        let block: Vec<Task> = self.tasks.drain(range.clone()).collect();
        let len = block.len();
        let mut target = new_index.min(self.tasks.len() + len);
        if target > idx {
            target = target.saturating_sub(len);
        }
        let target = target.min(self.tasks.len());
        let base_level = block[0].outline_level;
        let max_level = if target == 0 { 1 } else { self.tasks[target - 1].outline_level + 1 };
        let new_level = base_level.min(max_level);
        let delta = base_level as i32 - new_level as i32;
        for (k, mut t) in block.into_iter().enumerate() {
            t.outline_level = (t.outline_level as i32 - delta).max(1) as u16;
            self.tasks.insert(target + k, t);
        }
        self.normalize_outline();
        Ok(())
    }

    /// Moves a task block one row up (`delta < 0`) or down among its siblings.
    pub fn shift_task(&mut self, id: TaskId, up: bool) -> PlanResult<()> {
        let idx = self.require_task(id)?;
        let level = self.tasks[idx].outline_level;
        if up {
            let prev_sibling =
                (0..idx).rev().take_while(|&i| self.tasks[i].outline_level >= level).find(|&i| self.tasks[i].outline_level == level);
            match prev_sibling {
                Some(p) => self.move_task(id, p),
                None => invalid("task is already first in its group"),
            }
        } else {
            let end = self.descendants_range(idx).end;
            if end >= self.tasks.len() || self.tasks[end].outline_level != level {
                return invalid("task is already last in its group");
            }
            let next_end = self.descendants_range(end).end;
            self.move_task(id, next_end)
        }
    }

    // ------------------------------------------------------------------
    // Dependencies
    // ------------------------------------------------------------------

    /// Whether `to` is reachable from `from` following successor links (local only, summaries expanded).
    fn reaches(&self, from: TaskId, to: TaskId) -> bool {
        // Build successor map including summary inheritance: a link to/from a summary relates to its subtree.
        let group = |id: TaskId| -> Vec<TaskId> {
            match self.task_index(id) {
                Some(i) => {
                    let mut v = vec![id];
                    v.extend(self.descendants_range(i).map(|d| self.tasks[d].id));
                    // ancestors too: their links apply to this task
                    let mut a = self.parent_index(i);
                    while let Some(ai) = a {
                        v.push(self.tasks[ai].id);
                        a = self.parent_index(ai);
                    }
                    v
                }
                None => vec![id],
            }
        };
        let mut stack = vec![from];
        let mut seen = HashSet::new();
        while let Some(cur) = stack.pop() {
            if !seen.insert(cur) {
                continue;
            }
            let cur_group: HashSet<TaskId> = group(cur).into_iter().collect();
            if cur_group.contains(&to) && cur != from {
                return true;
            }
            for t in &self.tasks {
                let preds = t.predecessors.iter().map(|d| d.predecessor).chain(t.members.iter().copied());
                for p in preds {
                    if let Some(pid) = p.local()
                        && cur_group.contains(&pid)
                    {
                        if t.id == to {
                            return true;
                        }
                        stack.push(t.id);
                    }
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
        let t = &mut self.tasks[si];
        if let Some(d) = t.predecessors.iter_mut().find(|d| d.predecessor == pred) {
            d.kind = kind;
            d.lag = lag;
        } else {
            t.predecessors.push(Dependency { predecessor: pred, kind, lag });
        }
        Ok(())
    }

    /// Links the tasks in order with Finish-to-Start links (like "Link the selected tasks").
    pub fn link_chain(&mut self, ids: &[TaskId]) -> PlanResult<()> {
        for w in ids.windows(2) {
            self.link(TaskRef::Local(w[0]), w[1], LinkType::FS, Lag::None)?;
        }
        Ok(())
    }

    /// Removes the dependency `pred -> succ`. Returns whether a link was removed.
    pub fn unlink(&mut self, pred: TaskRef, succ: TaskId) -> PlanResult<bool> {
        let si = self.require_task(succ)?;
        let before = self.tasks[si].predecessors.len();
        self.tasks[si].predecessors.retain(|d| d.predecessor != pred);
        Ok(before != self.tasks[si].predecessors.len())
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
            let (pred, rest) = if let Some((proj, rest)) = part.split_once('\\') {
                let ext = self
                    .external_projects
                    .iter()
                    .find(|e| e.name.eq_ignore_ascii_case(proj.trim()) || e.id.to_string() == proj.trim())
                    .ok_or_else(|| PlanError::ExternalNotFound(proj.to_string()))?;
                let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
                let tid: u32 = digits.parse().map_err(|_| PlanError::Invalid(format!("bad external task id in '{part}'")))?;
                (TaskRef::External { project: ext.id, task: TaskId(tid) }, rest[digits.len()..].to_string())
            } else {
                let digits: String = part.chars().take_while(|c| c.is_ascii_digit()).collect();
                let row: usize = digits.parse().map_err(|_| PlanError::Invalid(format!("'{part}' is not a valid predecessor")))?;
                let t = self.tasks.get(row.wrapping_sub(1)).ok_or_else(|| PlanError::Invalid(format!("there is no task with ID {row}")))?;
                (TaskRef::Local(t.id), part[digits.len()..].to_string())
            };
            let rest = rest.trim();
            let (kind, lag_text) = if rest.len() >= 2 && LinkType::parse(&rest[..2]).is_some() {
                (LinkType::parse(&rest[..2]).unwrap(), &rest[2..])
            } else {
                (LinkType::FS, rest)
            };
            let lag = Lag::parse(lag_text, &self.settings.time).ok_or_else(|| PlanError::Invalid(format!("invalid lag '{lag_text}'")))?;
            out.push(Dependency { predecessor: pred, kind, lag });
        }
        Ok(out)
    }

    /// Replaces all predecessors of a task, validating each link.
    pub fn set_predecessors(&mut self, id: TaskId, deps: Vec<Dependency>) -> PlanResult<()> {
        let idx = self.require_task(id)?;
        let old = std::mem::take(&mut self.tasks[idx].predecessors);
        for d in &deps {
            if let Err(e) = self.link(d.predecessor, id, d.kind, d.lag) {
                let idx = self.task_index(id).expect("exists");
                self.tasks[idx].predecessors = old;
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
        if !self.tasks[mi].is_milestone() {
            return invalid(format!("'{}' is not a milestone", self.tasks[mi].name));
        }
        for m in members {
            if let TaskRef::Local(id) = m {
                self.require_task(*id)?;
                if *id == milestone {
                    return Err(PlanError::SelfLink);
                }
                if self.reaches(milestone, *id) {
                    return Err(PlanError::CircularDependency);
                }
            }
            let t = &mut self.tasks[mi];
            if !t.members.contains(m) {
                t.members.push(*m);
            }
        }
        Ok(())
    }

    pub fn remove_milestone_members(&mut self, milestone: TaskId, members: &[TaskRef]) -> PlanResult<()> {
        let mi = self.require_task(milestone)?;
        self.tasks[mi].members.retain(|m| !members.contains(m));
        Ok(())
    }

    /// Milestones that a task contributes to.
    pub fn milestones_of(&self, id: TaskId) -> Vec<TaskId> {
        self.tasks.iter().filter(|t| t.members.contains(&TaskRef::Local(id))).map(|t| t.id).collect()
    }

    // ------------------------------------------------------------------
    // Duration / work / units
    // ------------------------------------------------------------------

    fn work_assignment_ids(&self, task: TaskId) -> Vec<usize> {
        self.assignments
            .iter()
            .enumerate()
            .filter(|(_, a)| a.task == task && self.resource(a.resource).is_some_and(|r| r.kind == ResourceType::Work))
            .map(|(i, _)| i)
            .collect()
    }

    /// Duration implied by the work assignments (longest assignment).
    fn duration_from_assignments(&self, task: TaskId) -> Option<Minutes> {
        self.work_assignment_ids(task)
            .into_iter()
            .map(|ai| {
                let a = &self.assignments[ai];
                if a.units > 0.0 { (a.work as f64 / a.units).round() as Minutes } else { 0 }
            })
            .max()
    }

    /// Sets a task's duration, adjusting assignments according to the task type.
    pub fn set_duration(&mut self, id: TaskId, minutes: Minutes, estimated: bool) -> PlanResult<()> {
        let idx = self.require_task(id)?;
        if self.has_children(idx) {
            return invalid("the duration of a summary task is calculated from its subtasks");
        }
        let minutes = minutes.max(0);
        let task_type = self.tasks[idx].task_type;
        {
            let t = &mut self.tasks[idx];
            t.duration = minutes;
            t.estimated = estimated;
            if minutes == 0 && t.kind == TaskKind::Task && self.settings.new_tasks_auto {
                t.kind = TaskKind::Milestone;
            } else if minutes > 0 && t.kind == TaskKind::Milestone && t.members.is_empty() {
                t.kind = TaskKind::Task;
            }
        }
        for ai in self.work_assignment_ids(id) {
            let a = &mut self.assignments[ai];
            match task_type {
                TaskType::FixedUnits | TaskType::FixedDuration => a.work = (minutes as f64 * a.units).round() as Minutes,
                TaskType::FixedWork => {
                    if minutes > 0 {
                        a.units = a.work as f64 / minutes as f64;
                    }
                }
            }
        }
        // A manual task's finish follows its new duration.
        if self.tasks[idx].mode == ScheduleMode::Manual {
            self.tasks[idx].manual_finish = None;
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
        let t = &mut self.tasks[idx];
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

    /// Assigns a resource to a task (or updates the existing assignment).
    ///
    /// * `units` defaults to 100% (or 1 unit of material).
    /// * `work` sets the assignment work explicitly; otherwise work follows the
    ///   task's duration, or its estimate when this is the first assignment of an
    ///   estimated task.
    pub fn assign(&mut self, task: TaskId, resource: ResourceId, units: Option<f64>, work: Option<Minutes>) -> PlanResult<AssignmentId> {
        let ti = self.require_task(task)?;
        if self.has_children(ti) {
            return invalid("resources cannot be assigned to summary tasks");
        }
        let r = self.resource(resource).ok_or(PlanError::ResourceNotFound(resource))?.clone();
        let units = units.unwrap_or(1.0).max(0.0);

        if let Some(existing) = self.assignments.iter().position(|a| a.task == task && a.resource == resource) {
            let aid = self.assignments[existing].id;
            if units != self.assignments[existing].units {
                self.set_assignment_units(aid, units)?;
            }
            if let Some(w) = work {
                self.set_assignment_work(aid, w)?;
            }
            return Ok(aid);
        }

        let id = self.ids.next_assignment();
        let mut a = Assignment { id, task, resource, units, work: 0, actual_work: 0, cost: 0.0, contour: WorkContour::Flat };
        if r.kind != ResourceType::Work {
            self.assignments.push(a);
            return Ok(id);
        }
        let existing_work = self.work_assignment_ids(task);
        let t = self.tasks[ti].clone();
        if let Some(w) = work {
            a.work = w;
            self.assignments.push(a);
            self.recompute_duration_from_work(task);
        } else if existing_work.is_empty() {
            if let (Some(est), true) = (t.estimate, t.estimated || t.duration == 0) {
                // First assignment of an estimated task: the estimate drives work and duration.
                a.work = est.expected_work(&self.settings);
                self.assignments.push(a);
                self.recompute_duration_from_work(task);
                if let Some(t) = self.task_mut(task) {
                    t.estimated = false;
                }
            } else {
                a.work = (t.duration as f64 * units).round() as Minutes;
                self.assignments.push(a);
            }
        } else if t.effort_driven && t.task_type != TaskType::FixedDuration {
            // Effort driven: total work stays the same and is redistributed by units.
            let total: Minutes = existing_work.iter().map(|&i| self.assignments[i].work).sum();
            self.assignments.push(a);
            let all = self.work_assignment_ids(task);
            let total_units: f64 = all.iter().map(|&i| self.assignments[i].units).sum();
            if total_units > 0.0 {
                for i in all {
                    let share = self.assignments[i].units / total_units;
                    self.assignments[i].work = (total as f64 * share).round() as Minutes;
                }
            }
            self.recompute_duration_from_work(task);
        } else {
            a.work = (t.duration as f64 * units).round() as Minutes;
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

    pub fn unassign(&mut self, id: AssignmentId) -> PlanResult<()> {
        let pos = self.assignments.iter().position(|a| a.id == id).ok_or(PlanError::AssignmentNotFound(id))?;
        self.assignments.remove(pos);
        Ok(())
    }

    pub fn set_assignment_units(&mut self, id: AssignmentId, units: f64) -> PlanResult<()> {
        let pos = self.assignments.iter().position(|a| a.id == id).ok_or(PlanError::AssignmentNotFound(id))?;
        let units = units.max(0.0);
        let task = self.assignments[pos].task;
        let tt = self.task(task).map(|t| t.task_type).unwrap_or_default();
        let dur = self.task(task).map(|t| t.duration).unwrap_or(0);
        let is_work = self.resource(self.assignments[pos].resource).is_some_and(|r| r.kind == ResourceType::Work);
        self.assignments[pos].units = units;
        if is_work {
            match tt {
                TaskType::FixedDuration => self.assignments[pos].work = (dur as f64 * units).round() as Minutes,
                TaskType::FixedUnits | TaskType::FixedWork => self.recompute_duration_from_work(task),
            }
        }
        Ok(())
    }

    pub fn set_assignment_work(&mut self, id: AssignmentId, work: Minutes) -> PlanResult<()> {
        let pos = self.assignments.iter().position(|a| a.id == id).ok_or(PlanError::AssignmentNotFound(id))?;
        let task = self.assignments[pos].task;
        let tt = self.task(task).map(|t| t.task_type).unwrap_or_default();
        let dur = self.task(task).map(|t| t.duration).unwrap_or(0);
        self.assignments[pos].work = work.max(0);
        match tt {
            TaskType::FixedDuration if dur > 0 => self.assignments[pos].units = work as f64 / dur as f64,
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
            let rid = match self.resource_by_name(name) {
                Some(r) => r.id,
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
            let leaves: Vec<TaskId> = self.descendants_range(idx).filter(|&i| !self.has_children(i)).map(|i| self.tasks[i].id).collect();
            for l in leaves {
                self.set_percent_complete(l, pct)?;
            }
            return Ok(());
        }
        let t = &mut self.tasks[idx];
        t.percent_complete = pct;
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
        let id = self.ids.next_time_entry();
        self.time_entries.push(TimeEntry { id, task, resource, date, minutes, note: note.into() });
        // Starting work on a task records its actual start.
        let start_of_day = self.base_cal_day_start(date);
        let t = &mut self.tasks[ti];
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
            let pct = (t.sched.actual_work as f64 / t.sched.work as f64 * 100.0).floor().min(99.0) as u8;
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

fn trim_f(v: f64) -> String {
    if (v - v.round()).abs() < 1e-9 { format!("{}", v.round() as i64) } else { format!("{v:.2}") }
}
