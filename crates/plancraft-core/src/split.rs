//! Split Task: pauses planned in a task's work ([`Gap`]s), which move with the task.
//!
//! Microsoft's published description of splitting:
//! - A task can be split into as many parts as needed.
//! - The parts are rejoined by dragging one until it touches another.
//! - A later part can be moved on its own (moving the first part moves the whole task).
//! - The task's duration is the work in all its parts.
//!
//! A pause in work already under way (Update Project, a later Resume date) is a [`crate::model::Split`]
//! instead: it ends on a fixed date.

use chrono::NaiveDateTime;

use crate::duration::Minutes;
use crate::ids::TaskId;
use crate::limits::MAX_MINUTES;
use crate::model::{Gap, Project, ScheduleMode};
use crate::ops::{PlanError, PlanResult};
use crate::schedule::{MAX_SPLITS, planned_gaps, segments_of, task_calendar, work_done_by};

fn invalid<T>(msg: impl Into<String>) -> PlanResult<T> {
    Err(PlanError::Invalid(msg.into()))
}

/// Working minutes of a task already done (0 when it has not started).
fn work_done(t: &crate::model::Task) -> Minutes {
    if t.actual_start.is_some() { t.duration.saturating_mul(Minutes::from(t.percent_complete)) / 100 } else { 0 }
}

/// The pauses in one task add up to at most five years.
fn check_total(gaps: &[Gap]) -> PlanResult<()> {
    if gaps.iter().fold(0 as Minutes, |s, g| s.saturating_add(g.length)) > MAX_MINUTES {
        return invalid("the splits in one task can add up to at most 5 years");
    }
    Ok(())
}

impl Project {
    /// The row of a task that can be split: auto scheduled, not a summary or milestone, with work
    /// left to do and a schedule.
    fn splittable(&self, id: TaskId) -> PlanResult<usize> {
        let idx = self.task_index(id).ok_or(PlanError::TaskNotFound(id))?;
        let t = self.tasks.get(idx).ok_or(PlanError::TaskNotFound(id))?;
        if self.has_children(idx) || t.is_milestone() || t.duration <= 0 {
            return invalid(format!("'{}' can't be split: only tasks with a duration can", t.name));
        }
        if t.mode == ScheduleMode::Manual {
            return invalid(format!("'{}' is manually scheduled; split it after switching it to auto scheduling", t.name));
        }
        if t.actual_finish.is_some() || t.percent_complete >= 100 {
            return invalid(format!("'{}' is complete", t.name));
        }
        if t.sched.start.is_none() {
            return invalid(format!("'{}' has not been scheduled yet", t.name));
        }
        Ok(idx)
    }

    /// The task's working parts laid out from its (actual or scheduled) start with the splits it
    /// has now, so several splits in one edit see each other.
    fn parts_now(&self, idx: usize) -> Vec<(NaiveDateTime, NaiveDateTime)> {
        let Some(t) = self.tasks.get(idx) else { return Vec::new() };
        let Some(start) = t.actual_start.or(t.sched.start) else { return Vec::new() };
        let cal = task_calendar(self, idx);
        segments_of(start, &t.splits, &t.gaps, &cal, t.duration).into_iter().map(|(s, f, _)| (s, f)).collect()
    }

    /// Splits a task: its work stops at `at` and goes on at `resume` (the Split Task command).
    ///
    /// The split moves with the task, so the pause keeps its length when the task's dates change.
    /// Splitting inside an existing pause lengthens that pause. Work already done can't be split.
    pub fn split_task(&mut self, id: TaskId, at: NaiveDateTime, resume: NaiveDateTime) -> PlanResult<()> {
        let idx = self.splittable(id)?;
        if crate::limits::clamp_datetime(at) != at || crate::limits::clamp_datetime(resume) != resume {
            return invalid("a split's dates must be between 1900 and 2300");
        }
        if resume <= at {
            return invalid("work must resume after the split starts");
        }
        let parts = self.parts_now(idx);
        let cal = task_calendar(self, idx);
        let t = self.tasks.get(idx).ok_or(PlanError::TaskNotFound(id))?;
        let dur = t.duration;
        let before = work_done_by(&parts, &cal, at);
        if before <= 0 || before >= dur {
            return invalid(format!("the split must fall inside the work of '{}'", t.name));
        }
        if before < work_done(t) {
            return invalid(format!("work on '{}' already done can't be split", t.name));
        }
        // The moment that much work ends, and the working time from there to `resume`.
        let stop = parts
            .iter()
            .scan(0 as Minutes, |used, &(s, f)| {
                let len = cal.between(s, f).max(0);
                let upto = (*used).saturating_add(len);
                let out = (before <= upto).then(|| cal.add(s, before.saturating_sub(*used)));
                *used = upto;
                Some(out)
            })
            .flatten()
            .next()
            .unwrap_or(at);
        let mut length = cal.between(cal.snap_forward(stop), resume).max(0);
        if length <= 0 {
            return invalid("no working time falls inside the split");
        }
        // A split inside an existing pause lengthens it (by the pause already there).
        let existing = planned_gaps(&t.gaps, dur).into_iter().find(|g| g.work == before);
        if let Some(g) = existing {
            length = length.max(g.length);
        }
        let length = length.min(MAX_MINUTES);
        let mut gaps = planned_gaps(&t.gaps, dur);
        let full = gaps.len() >= MAX_SPLITS;
        match gaps.iter_mut().find(|g| g.work == before) {
            Some(g) => g.length = length,
            None if full => return invalid(format!("a task can have at most {MAX_SPLITS} splits")),
            None => gaps.push(Gap { work: before, length }),
        }
        gaps.sort_by_key(|g| g.work);
        check_total(&gaps)?;
        if let Some(t) = self.tasks.get_mut(idx) {
            t.gaps = gaps;
        }
        Ok(())
    }

    /// Sets the length (working minutes) of the task's `k`-th planned split, counting from 0 in
    /// work order. A length of 0 rejoins the parts on both sides.
    pub fn set_split_length(&mut self, id: TaskId, k: usize, length: Minutes) -> PlanResult<()> {
        let idx = self.splittable(id)?;
        let t = self.tasks.get(idx).ok_or(PlanError::TaskNotFound(id))?;
        let done = work_done(t);
        let mut gaps = planned_gaps(&t.gaps, t.duration);
        let Some(g) = gaps.get_mut(k) else {
            return invalid(format!("'{}' has {} split(s); there is no split {}", t.name, gaps.len(), k + 1));
        };
        if g.work < done {
            return invalid(format!("work on '{}' after that split is already under way; it can't move", t.name));
        }
        if !(0..=MAX_MINUTES).contains(&length) {
            return invalid("a split can be at most 5 years long");
        }
        g.length = length;
        gaps.retain(|g| g.length > 0);
        check_total(&gaps)?;
        if let Some(t) = self.tasks.get_mut(idx) {
            t.gaps = gaps;
        }
        Ok(())
    }

    /// Changes the work in part `k` (from 0) of a split task to `minutes`, keeping the other
    /// parts' work: the task's duration changes by the difference.
    pub fn set_part_duration(&mut self, id: TaskId, k: usize, minutes: Minutes) -> PlanResult<()> {
        let idx = self.splittable(id)?;
        let t = self.tasks.get(idx).ok_or(PlanError::TaskNotFound(id))?;
        let dur = t.duration;
        let mut gaps = planned_gaps(&t.gaps, dur);
        let bounds: Vec<Minutes> = std::iter::once(0).chain(gaps.iter().map(|g| g.work)).chain(std::iter::once(dur)).collect();
        let (Some(&from), Some(&to)) = (bounds.get(k), bounds.get(k + 1)) else {
            return invalid(format!("'{}' has {} part(s); there is no part {}", t.name, gaps.len() + 1, k + 1));
        };
        if minutes <= 0 {
            return invalid("a part needs some work; rejoin the parts to remove one");
        }
        if from < work_done(t) {
            return invalid(format!("work on '{}' in that part is already under way", t.name));
        }
        let delta = minutes.saturating_sub(to.saturating_sub(from));
        let new_dur = dur.saturating_add(delta);
        if new_dur > MAX_MINUTES {
            return invalid("a duration can be at most 5 years");
        }
        for g in gaps.iter_mut().filter(|g| g.work >= to) {
            g.work = g.work.saturating_add(delta);
        }
        if let Some(t) = self.tasks.get_mut(idx) {
            t.gaps = gaps;
        }
        self.set_duration(id, new_dur, false)
    }

    /// Removes every planned split from a task (rejoins its parts). Returns how many there were.
    pub fn remove_splits(&mut self, id: TaskId) -> PlanResult<usize> {
        let idx = self.task_index(id).ok_or(PlanError::TaskNotFound(id))?;
        let t = self.tasks.get_mut(idx).ok_or(PlanError::TaskNotFound(id))?;
        let n = planned_gaps(&t.gaps, t.duration).len();
        t.gaps.clear();
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use chrono::NaiveDate;

    use crate::prelude::*;

    fn dt(m: u32, d: u32, h: u32) -> chrono::NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, m, d).unwrap().and_hms_opt(h, 0, 0).unwrap()
    }

    /// A (4 days, Mon 10/19 – Thu 10/22) → B (1 day).
    fn plan() -> (Project, TaskId, TaskId) {
        let mut p = Project::new("S", NaiveDate::from_ymd_opt(2026, 10, 19).unwrap());
        let a = p.add_task("A", None);
        p.set_duration(a, 4 * 480, false).unwrap();
        let b = p.add_task("B", None);
        p.link(a.into(), b, LinkType::FS, Lag::None).unwrap();
        recalculate(&mut p);
        (p, a, b)
    }

    #[test]
    fn a_split_pauses_the_task_and_moves_with_it() {
        let (mut p, a, b) = plan();
        // Stop after day 2 (Tue 17:00), go on Mon 10/26: a 3-working-day pause.
        p.split_task(a, dt(10, 20, 17), dt(10, 26, 8)).unwrap();
        recalculate(&mut p);
        let t = p.task(a).unwrap();
        assert_eq!(t.duration, 4 * 480, "splitting keeps the duration");
        assert_eq!(t.sched.parts, [(dt(10, 19, 8), dt(10, 20, 17)), (dt(10, 26, 8), dt(10, 27, 17))]);
        assert_eq!(p.task(b).unwrap().sched.start, Some(dt(10, 28, 8)));
        // Moving the task keeps the pause's length.
        p.task_mut(a).unwrap().constraint = Constraint { kind: ConstraintKind::Snet, date: Some(dt(10, 21, 8)) };
        recalculate(&mut p);
        assert_eq!(p.task(a).unwrap().sched.parts, [(dt(10, 21, 8), dt(10, 22, 17)), (dt(10, 28, 8), dt(10, 29, 17))]);
    }

    /// Regression: a pause in work under way and a planned split at the same point cancelled
    /// out (the earlier resume won), so Reschedule did nothing and Split Task shortened a pause.
    #[test]
    fn a_pause_and_a_split_at_the_same_point_resume_at_the_later() {
        let (mut p, a, b) = plan();
        p.split_task(a, dt(10, 20, 17), dt(10, 26, 8)).unwrap();
        recalculate(&mut p);
        p.set_percent_complete(a, 50).unwrap();
        recalculate(&mut p);
        let r = p.reschedule_uncompleted_work(&[a], dt(11, 6, 17)).unwrap();
        assert_eq!(r.split, [a]);
        recalculate(&mut p);
        assert_eq!(p.task(a).unwrap().sched.parts.last().map(|x| x.0), Some(dt(11, 9, 8)));
        assert_eq!(p.task(b).unwrap().sched.start, Some(dt(11, 11, 8)));
        // Splitting inside that pause lengthens it, never shortens it.
        p.split_task(a, dt(10, 21, 8), dt(10, 22, 8)).unwrap_or_default();
        recalculate(&mut p);
        assert_eq!(p.task(a).unwrap().sched.parts.last().map(|x| x.0), Some(dt(11, 9, 8)));
    }

    /// Regression: parts holding work already done could be dragged, moving that work.
    #[test]
    fn work_already_done_stays_put() {
        let (mut p, a, _) = plan();
        p.split_task(a, dt(10, 19, 17), dt(10, 21, 8)).unwrap();
        recalculate(&mut p);
        p.set_percent_complete(a, 50).unwrap();
        recalculate(&mut p);
        assert!(p.set_split_length(a, 0, 5 * 480).is_err(), "part 2 is under way");
        assert!(p.set_part_duration(a, 1, 480).is_err());
        assert!(p.split_task(a, dt(10, 21, 12), dt(10, 26, 8)).is_err(), "inside work done");
        assert!(p.split_task(a, dt(10, 22, 17), dt(10, 26, 8)).is_ok(), "work not done yet can still be split");
    }

    /// Regression: a split far past 2300 was accepted and silently shortened.
    #[test]
    fn split_dates_stay_in_range() {
        let (mut p, a, _) = plan();
        let far = NaiveDate::from_ymd_opt(9999, 1, 1).unwrap().and_hms_opt(8, 0, 0).unwrap();
        assert!(p.split_task(a, dt(10, 20, 17), far).is_err());
    }

    /// Regression: leveling moved a delayed task as one block, ignoring its planned split, and
    /// left an overallocation it then didn't report.
    #[test]
    fn leveling_moves_planned_splits_with_the_task() {
        let mut p = Project::new("L", NaiveDate::from_ymd_opt(2026, 10, 19).unwrap());
        let r = p.add_resource("R");
        let x = p.add_task("X", None);
        p.task_mut(x).unwrap().priority = 900;
        let y = p.add_task("Y", None);
        p.set_duration(y, 2 * 480, false).unwrap();
        p.task_mut(y).unwrap().priority = 100;
        let z = p.add_task("Z", None);
        p.task_mut(z).unwrap().constraint = Constraint { kind: ConstraintKind::Mso, date: Some(dt(10, 27, 8)) };
        for t in [x, y, z] {
            p.assign(t, r, Some(1.0), None).unwrap();
        }
        recalculate(&mut p);
        p.split_task(y, dt(10, 19, 17), dt(10, 26, 8)).unwrap();
        let report = crate::leveling::level_resources(&mut p, &Default::default());
        let over = crate::workload::overallocations(&p);
        assert!(over.is_empty() || !report.unresolved.is_empty(), "no silent overallocation: {over:?}");
    }

    #[test]
    fn parts_rejoin_resize_and_refuse_bad_splits() {
        let (mut p, a, _) = plan();
        assert!(p.split_task(a, dt(10, 26, 8), dt(10, 27, 8)).is_err(), "after the task");
        assert!(p.split_task(a, dt(10, 20, 17), dt(10, 20, 12)).is_err(), "resume before the stop");
        p.split_task(a, dt(10, 19, 17), dt(10, 21, 8)).unwrap();
        recalculate(&mut p);
        // The second part (3 days) shrinks to 2: the task is a day shorter.
        p.set_part_duration(a, 1, 2 * 480).unwrap();
        recalculate(&mut p);
        assert_eq!(p.task(a).unwrap().duration, 3 * 480);
        // Mon, a day's pause on Tue, then Wed and Thu.
        assert_eq!(p.task(a).unwrap().sched.finish, Some(dt(10, 22, 17)));
        p.set_split_length(a, 0, 0).unwrap();
        recalculate(&mut p);
        assert!(p.task(a).unwrap().sched.parts.is_empty(), "rejoined");
        assert_eq!(p.task(a).unwrap().sched.finish, Some(dt(10, 21, 17)));
        assert!(p.set_split_length(a, 0, 480).is_err(), "no split left");
    }
}
