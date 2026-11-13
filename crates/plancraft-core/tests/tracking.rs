#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]
//! Progress tracking on split tasks: Update Project, rescheduling, Resume, and actual and
//! remaining duration (regressions found reviewing the status-date work).
//!
//! Default calendar: Mon–Fri, 08:00–12:00 and 13:00–17:00, from Monday 2026-10-19.

use chrono::{NaiveDate, NaiveDateTime};
use plancraft_core::prelude::*;

const DAY: Minutes = 8 * 60;

fn d(m: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, m, day).unwrap()
}

fn dt(m: u32, day: u32, h: u32) -> NaiveDateTime {
    d(m, day).and_hms_opt(h, 0, 0).unwrap()
}

fn project() -> Project {
    Project::new("Tracking", d(10, 19))
}

fn task(p: &mut Project, name: &str, duration: Minutes) -> TaskId {
    let id = p.add_task(name, None);
    p.set_duration(id, duration, false).unwrap();
    id
}

fn sched(p: &Project, id: TaskId) -> &TaskSchedule {
    &p.task(id).unwrap().sched
}

/// A 4-day task from Monday, a day done, its rest rescheduled after Friday 10/23.
fn paused_task() -> (Project, TaskId) {
    let mut p = project();
    let t = task(&mut p, "Build", 4 * DAY);
    recalculate(&mut p);
    p.set_percent_complete(t, 25).unwrap();
    recalculate(&mut p);
    let after = p.end_of_working_day(d(10, 23));
    p.reschedule_uncompleted_work(&[], after).unwrap();
    recalculate(&mut p);
    let s = sched(&p, t);
    assert_eq!((s.stop, s.resume, s.finish), (Some(dt(10, 19, 17)), Some(dt(10, 26, 8)), Some(dt(10, 28, 17))));
    (p, t)
}

/// Regression: progress past the pause was laid out inside it, so updating after a reschedule
/// pulled the finish earlier, and updating twice with the same date completed the task.
#[test]
fn updating_a_paused_task_keeps_the_pause_and_its_finish() {
    let (mut p, t) = paused_task();
    let through = p.end_of_working_day(d(10, 27));
    for _ in 0..2 {
        p.update_progress_through(&[], through, ProgressUpdate::Scheduled).unwrap();
        recalculate(&mut p);
        assert_eq!(p.task(t).unwrap().percent_complete, 75, "a day before the pause and two after it");
        let s = sched(&p, t);
        assert_eq!(s.stop, Some(dt(10, 27, 17)));
        assert_eq!(s.resume, Some(dt(10, 28, 8)));
        assert_eq!(s.finish, Some(dt(10, 28, 17)), "the finish does not move");
        assert_eq!(s.parts, vec![(dt(10, 19, 8), dt(10, 19, 17)), (dt(10, 26, 8), dt(10, 28, 17))]);
    }
}

/// Regression: typing a higher % complete on a paused task did the same.
#[test]
fn more_progress_on_a_paused_task_fills_the_part_after_the_pause() {
    let (mut p, t) = paused_task();
    p.set_percent_complete(t, 50).unwrap();
    recalculate(&mut p);
    let s = sched(&p, t);
    assert_eq!((s.stop, s.resume, s.finish), (Some(dt(10, 26, 17)), Some(dt(10, 27, 8)), Some(dt(10, 28, 17))));
    // Lowering progress again keeps the pause: the work left runs up to it, then waits.
    p.set_percent_complete(t, 10).unwrap();
    recalculate(&mut p);
    assert_eq!(sched(&p, t).finish, Some(dt(10, 28, 17)));
    assert_eq!(sched(&p, t).parts.len(), 2);
}

/// Regression: a second reschedule dropped the first pause.
#[test]
fn rescheduling_again_keeps_the_earlier_pause() {
    let (mut p, t) = paused_task();
    p.update_progress_through(&[], p.end_of_working_day(d(10, 27)), ProgressUpdate::Scheduled).unwrap();
    let r = p.reschedule_uncompleted_work(&[], p.end_of_working_day(d(10, 30))).unwrap();
    recalculate(&mut p);
    assert_eq!(r.split, vec![t]);
    let s = sched(&p, t);
    assert_eq!(s.parts, vec![(dt(10, 19, 8), dt(10, 19, 17)), (dt(10, 26, 8), dt(10, 27, 17)), (dt(11, 2, 8), dt(11, 2, 17))]);
    assert_eq!((s.stop, s.resume), (Some(dt(10, 27, 17)), Some(dt(11, 2, 8))));
}

/// Regression: a task in progress with no pause was drawn split overnight or over a weekend,
/// because its resume (the next morning) was compared with its stop (17:00).
#[test]
fn progress_alone_does_not_split_a_task() {
    let mut p = project();
    let t = task(&mut p, "Build", 5 * DAY);
    recalculate(&mut p);
    p.set_percent_complete(t, 80).unwrap();
    recalculate(&mut p);
    let s = sched(&p, t);
    assert_eq!((s.stop, s.resume), (Some(dt(10, 22, 17)), Some(dt(10, 23, 8))));
    assert!(s.parts.is_empty());
}

/// Regression: Resume accepted a date before the stop and kept it, so lowering progress later
/// split the task at a date nobody chose.
#[test]
fn resume_must_be_after_the_stop() {
    let mut p = project();
    let t = task(&mut p, "Build", 4 * DAY);
    recalculate(&mut p);
    p.set_percent_complete(t, 50).unwrap();
    recalculate(&mut p);
    assert!(p.set_resume(t, Some(dt(10, 20, 8))).is_err(), "before the stop (10/20 17:00)");
    assert!(p.task(t).unwrap().splits.is_empty());
    // The next working moment after the stop is no pause at all.
    p.set_resume(t, Some(dt(10, 21, 8))).unwrap();
    assert!(p.task(t).unwrap().splits.is_empty());
    p.set_resume(t, Some(dt(10, 26, 8))).unwrap();
    recalculate(&mut p);
    assert_eq!(sched(&p, t).finish, Some(dt(10, 27, 17)));
    p.set_resume(t, None).unwrap();
    recalculate(&mut p);
    assert_eq!(sched(&p, t).finish, Some(dt(10, 22, 17)));
    assert!(p.task(t).unwrap().splits.is_empty());
}

/// Regression: an actual duration past the plan on a task already complete stretched the
/// duration but kept the old actual finish.
#[test]
fn actual_duration_past_the_plan_moves_the_actual_finish() {
    let mut p = project();
    let t = task(&mut p, "Build", 2 * DAY);
    recalculate(&mut p);
    p.set_percent_complete(t, 100).unwrap();
    recalculate(&mut p);
    assert_eq!(p.task(t).unwrap().actual_finish, Some(dt(10, 20, 17)));
    p.set_actual_duration(t, 4 * DAY).unwrap();
    recalculate(&mut p);
    let tk = p.task(t).unwrap();
    assert_eq!((tk.duration, tk.percent_complete, tk.actual_finish), (4 * DAY, 100, Some(dt(10, 22, 17))));
}

/// Regression: entering remaining (or zero actual) duration on a task with an actual start but
/// no progress cleared the actual start.
#[test]
fn duration_entries_keep_an_actual_start_without_progress() {
    let mut p = project();
    let t = task(&mut p, "Build", 4 * DAY);
    p.task_mut(t).unwrap().actual_start = Some(dt(10, 20, 8));
    recalculate(&mut p);
    p.set_remaining_duration(t, 2 * DAY).unwrap();
    assert_eq!(p.task(t).unwrap().duration, 2 * DAY);
    assert_eq!(p.task(t).unwrap().actual_start, Some(dt(10, 20, 8)));
    p.set_actual_duration(t, 0).unwrap();
    assert_eq!(p.task(t).unwrap().actual_start, Some(dt(10, 20, 8)));
}

/// Regression: resource usage spread a paused task's work across its pause.
#[test]
fn a_paused_task_puts_no_work_in_its_pause() {
    let mut p = project();
    let t = task(&mut p, "Build", 2 * DAY);
    let r = p.add_resource("Ana");
    p.assign(t, r, Some(1.0), None).unwrap();
    recalculate(&mut p);
    p.set_percent_complete(t, 50).unwrap();
    p.set_resume(t, Some(dt(11, 2, 8))).unwrap();
    recalculate(&mut p);
    let a = p.assignments.iter().find(|a| a.task == t).unwrap().clone();
    let days: Vec<(NaiveDate, i64)> =
        plancraft_core::workload::assignment_daily_work(&p, &a).into_iter().map(|(d, w)| (d, w.round() as i64)).collect();
    assert_eq!(days, vec![(d(10, 19), DAY), (d(11, 2), DAY)]);
}

/// Regression: rescheduling gave a Start No Earlier Than date to tasks that the paused task's
/// link already moved past the date.
#[test]
fn rescheduling_leaves_successors_of_a_paused_task_to_their_links() {
    let mut p = project();
    let a = task(&mut p, "A", 2 * DAY);
    let b = task(&mut p, "B", DAY);
    p.link_chain(&[a, b]).unwrap();
    recalculate(&mut p);
    p.set_percent_complete(a, 50).unwrap();
    let r = p.reschedule_uncompleted_work(&[], p.end_of_working_day(d(10, 22))).unwrap();
    recalculate(&mut p);
    assert_eq!(r.split, vec![a]);
    assert!(r.constrained.is_empty(), "{:?}", r.constrained);
    assert_eq!(sched(&p, b).start, Some(dt(10, 26, 8)));
}

/// Regression: rescheduling after 2300-12-31 failed with "a date is outside 1900–2300".
#[test]
fn rescheduling_after_the_last_date_says_why() {
    let (mut p, _) = paused_task();
    let last = p.end_of_working_day(NaiveDate::from_ymd_opt(2300, 12, 31).unwrap());
    let err = p.reschedule_uncompleted_work(&[], last).unwrap_err().to_string();
    assert!(err.contains("2300"), "{err}");
}

/// A summary's actual and remaining duration come from its rolled-up values (the MCP server
/// showed a summary's own stored duration and % complete).
#[test]
fn summary_actual_and_remaining_duration_roll_up() {
    let mut p = project();
    let s = p.add_task("Phase", None);
    let a = p.add_task("A", None);
    let b = p.add_task("B", None);
    p.task_mut(a).unwrap().outline_level = 2;
    p.task_mut(b).unwrap().outline_level = 2;
    p.set_duration(a, 4 * DAY, false).unwrap();
    p.set_duration(b, 4 * DAY, false).unwrap();
    p.link_chain(&[a, b]).unwrap();
    recalculate(&mut p);
    p.set_percent_complete(a, 100).unwrap();
    recalculate(&mut p);
    assert_eq!(p.task(s).unwrap().duration_progress(), (4 * DAY, 4 * DAY));
}
