#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]
//! Clean-room parity tests against Microsoft Project's *published* behaviour.
//!
//! Every test here is derived only from public Microsoft documentation
//! (support.microsoft.com articles): the scheduling formula, the task-type
//! recalculation table, the effort-driven rules, the link-type definitions,
//! lead/lag rules and the constraint descriptions, plus the worked numeric
//! examples those pages give. No Microsoft code, files or product output were
//! used, and page text is paraphrased (at most a short quote). Each test cites
//! its source page and section directly above it and states the documented
//! expectation in one sentence.
//!
//! All scenarios run on the default PlanCraft calendar (Mon–Fri, 08:00–12:00
//! and 13:00–17:00, 8 h days) from a Monday project start (2026-10-19), so the
//! expected dates are easy to verify by hand.
//!
//! Where PlanCraft deliberately or accidentally differs from the documented
//! behaviour, the test is kept but marked `#[ignore = "parity gap: ..."]`.

use chrono::{NaiveDate, NaiveDateTime};
use plancraft_core::prelude::*;

const HOUR: Minutes = 60;
const DAY: Minutes = 8 * HOUR;

fn d(y: i32, m: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, day).unwrap()
}

fn dt(y: i32, m: u32, day: u32, h: u32) -> NaiveDateTime {
    d(y, m, day).and_hms_opt(h, 0, 0).unwrap()
}

/// Empty project starting Monday 2026-10-19 08:00.
fn project() -> Project {
    Project::new("MS parity", d(2026, 10, 19))
}

fn task(p: &mut Project, name: &str, duration: Minutes) -> TaskId {
    let id = p.add_task(name, None);
    p.set_duration(id, duration, false).unwrap();
    id
}

fn start(p: &Project, id: TaskId) -> NaiveDateTime {
    p.task(id).unwrap().sched.start.unwrap()
}

fn finish(p: &Project, id: TaskId) -> NaiveDateTime {
    p.task(id).unwrap().sched.finish.unwrap()
}

fn duration(p: &Project, id: TaskId) -> Minutes {
    p.task(id).unwrap().duration
}

/// Total assignment work on a task.
fn work(p: &Project, id: TaskId) -> Minutes {
    p.assignments_for_task(id).map(|a| a.work).sum()
}

/// Total assignment units on a task (1.0 = 100%).
fn units(p: &Project, id: TaskId) -> f64 {
    p.assignments_for_task(id).map(|a| a.units).sum()
}

fn assignment(p: &Project, id: AssignmentId) -> &Assignment {
    p.assignment(id).unwrap()
}

fn assert_units(actual: f64, expected: f64) {
    assert!((actual - expected).abs() < 1e-9, "units: expected {expected}, got {actual}");
}

/// Microsoft's running example: one full-time person (100% units) on a
/// 10-day task with 80 hours of work (8 h days).
fn ms_example(task_type: TaskType, effort_driven: bool) -> (Project, TaskId, AssignmentId) {
    let mut p = project();
    let t = task(&mut p, "Example task", 10 * DAY);
    {
        let tk = p.task_mut(t).unwrap();
        tk.task_type = task_type;
        tk.effort_driven = effort_driven;
    }
    let r = p.add_resource("Person 1");
    let a = p.assign(t, r, Some(1.0), None).unwrap();
    assert_eq!(duration(&p, t), 10 * DAY);
    assert_eq!(work(&p, t), 80 * HOUR);
    (p, t, a)
}

fn add_second_person(p: &mut Project, t: TaskId) -> AssignmentId {
    let r = p.add_resource("Person 2");
    p.assign(t, r, Some(1.0), None).unwrap()
}

/// Sets a constraint directly on the task (the UI and MCP tools do the same).
fn constrain(p: &mut Project, id: TaskId, kind: ConstraintKind, date: NaiveDateTime) {
    p.task_mut(id).unwrap().constraint = Constraint { kind, date: Some(date) };
}

// ===========================================================================
// Work = Duration x Units and the task-type recalculation table
// ===========================================================================

// Source: https://support.microsoft.com/en-us/project/change-the-task-type-for-more-accurate-scheduling — "Look closer into task types"
// Project relates the three values by Work = Duration x Units, so a 4-day task with one person at 50% has 16 hours of work.
#[test]
fn work_equals_duration_times_units() {
    let mut p = project();
    let t = task(&mut p, "Half-time task", 4 * DAY);
    let r = p.add_resource("Person");
    p.assign(t, r, Some(0.5), None).unwrap();
    recalculate(&mut p);
    assert_eq!(work(&p, t), 16 * HOUR);
    assert_eq!(p.task(t).unwrap().sched.work, 16 * HOUR);
}

// Source: https://support.microsoft.com/en-us/project/change-the-task-type-for-more-accurate-scheduling — "Look closer into task types"
// Fixed Units is the default task type for new tasks.
#[test]
fn default_task_type_is_fixed_units() {
    let mut p = project();
    let t = p.add_task("New task", None);
    assert_eq!(p.task(t).unwrap().task_type, TaskType::FixedUnits);
}

// Source: https://support.microsoft.com/en-us/project/change-the-task-type-for-more-accurate-scheduling — "How task types change your schedule"
// On a Fixed Units task, revising the units recalculates the duration (work is kept): 80 h at 200% takes 5 days.
#[test]
fn fixed_units_change_units_recalculates_duration() {
    let (mut p, t, a) = ms_example(TaskType::FixedUnits, false);
    p.set_assignment_units(a, 2.0).unwrap();
    assert_eq!(duration(&p, t), 5 * DAY);
    assert_eq!(work(&p, t), 80 * HOUR);
}

// Source: https://support.microsoft.com/en-us/project/change-the-task-type-for-more-accurate-scheduling — "How task types change your schedule"
// On a Fixed Work task, revising the units recalculates the duration: 80 h at 50% takes 20 days.
#[test]
fn fixed_work_change_units_recalculates_duration() {
    let (mut p, t, a) = ms_example(TaskType::FixedWork, false);
    p.set_assignment_units(a, 0.5).unwrap();
    assert_eq!(duration(&p, t), 20 * DAY);
    assert_eq!(work(&p, t), 80 * HOUR);
}

// Source: https://support.microsoft.com/en-us/project/change-the-task-type-for-more-accurate-scheduling — "How task types change your schedule"
// On a Fixed Duration task, revising the units recalculates the work: 10 days at 50% is 40 h.
#[test]
fn fixed_duration_change_units_recalculates_work() {
    let (mut p, t, a) = ms_example(TaskType::FixedDuration, false);
    p.set_assignment_units(a, 0.5).unwrap();
    assert_eq!(duration(&p, t), 10 * DAY);
    assert_eq!(work(&p, t), 40 * HOUR);
}

// ===========================================================================
// Microsoft's worked example: Fixed Units
// ===========================================================================

// Source: https://support.microsoft.com/en-us/project/change-the-task-type-for-more-accurate-scheduling — "Scenario one: Changing task type to fixed-units"
// Adding a second full-time person to the (effort-driven) 10-day / 80 h task gives 2 units, 5 days and still 80 h.
#[test]
fn fixed_units_add_full_time_resource_halves_duration() {
    let (mut p, t, _) = ms_example(TaskType::FixedUnits, true);
    add_second_person(&mut p, t);
    assert_units(units(&p, t), 2.0);
    assert_eq!(duration(&p, t), 5 * DAY);
    assert_eq!(work(&p, t), 80 * HOUR);
    recalculate(&mut p);
    assert_eq!(finish(&p, t), dt(2026, 10, 23, 17));
}

// Source: https://support.microsoft.com/en-us/project/change-the-task-type-for-more-accurate-scheduling — "Scenario one: Changing task type to fixed-units"
// Shortening the Fixed Units task to 8 days lowers the work to 64 h with units unchanged at 100%.
#[test]
fn fixed_units_shorten_duration_reduces_work() {
    let (mut p, t, a) = ms_example(TaskType::FixedUnits, false);
    p.set_duration(t, 8 * DAY, false).unwrap();
    assert_eq!(work(&p, t), 64 * HOUR);
    assert_units(assignment(&p, a).units, 1.0);
}

// Source: https://support.microsoft.com/en-us/project/change-the-task-type-for-more-accurate-scheduling — "Scenario one: Changing task type to fixed-units"
// Adding 20 h of work to the Fixed Units task gives 100 h over 12.5 days at 100%.
#[test]
fn fixed_units_add_work_extends_duration() {
    let (mut p, t, a) = ms_example(TaskType::FixedUnits, false);
    p.set_assignment_work(a, 100 * HOUR).unwrap();
    assert_eq!(work(&p, t), 100 * HOUR);
    assert_eq!(duration(&p, t), 12 * DAY + 4 * HOUR);
    assert_units(assignment(&p, a).units, 1.0);
    recalculate(&mut p);
    // 12 full days (Mon 19 Oct .. Tue 3 Nov) plus a morning: Wed 4 Nov 12:00.
    assert_eq!(finish(&p, t), dt(2026, 11, 4, 12));
}

// ===========================================================================
// Microsoft's worked example: Fixed Work
// ===========================================================================

// Source: https://support.microsoft.com/en-us/project/change-the-task-type-for-more-accurate-scheduling — "Scenario two: Changing task type to fixed-work"
// Fixed Work tasks are always effort-driven, so adding a second full-time person gives 2 units, 5 days and still 80 h.
#[test]

fn fixed_work_add_full_time_resource_is_always_effort_driven() {
    // `effort_driven` is left at its default (false): Microsoft does not allow
    // turning effort-driven scheduling off for Fixed Work tasks.
    let (mut p, t, _) = ms_example(TaskType::FixedWork, false);
    add_second_person(&mut p, t);
    assert_units(units(&p, t), 2.0);
    assert_eq!(work(&p, t), 80 * HOUR);
    assert_eq!(duration(&p, t), 5 * DAY);
}

// Source: https://support.microsoft.com/en-us/project/change-the-task-type-for-more-accurate-scheduling — "Scenario two: Changing task type to fixed-work"
// With the effort-driven flag explicitly on, adding a second full-time person to the Fixed Work task gives 2 units, 5 days, 80 h.
#[test]
fn fixed_work_effort_driven_add_full_time_resource_halves_duration() {
    let (mut p, t, _) = ms_example(TaskType::FixedWork, true);
    add_second_person(&mut p, t);
    assert_units(units(&p, t), 2.0);
    assert_eq!(work(&p, t), 80 * HOUR);
    assert_eq!(duration(&p, t), 5 * DAY);
}

// Source: https://support.microsoft.com/en-us/project/change-the-task-type-for-more-accurate-scheduling — "Scenario two: Changing task type to fixed-work"
// Shortening the Fixed Work task to 8 days keeps 80 h and raises the units to 125%.
#[test]
fn fixed_work_shorten_duration_raises_units() {
    let (mut p, t, a) = ms_example(TaskType::FixedWork, false);
    p.set_duration(t, 8 * DAY, false).unwrap();
    assert_eq!(work(&p, t), 80 * HOUR);
    assert_units(assignment(&p, a).units, 1.25);
}

// Source: https://support.microsoft.com/en-us/project/change-the-task-type-for-more-accurate-scheduling — "Scenario two: Changing task type to fixed-work"
// Adding 20 h of work to the Fixed Work task gives 100 h over 12.5 days at 100%.
#[test]
fn fixed_work_add_work_extends_duration() {
    let (mut p, t, a) = ms_example(TaskType::FixedWork, false);
    p.set_assignment_work(a, 100 * HOUR).unwrap();
    assert_eq!(work(&p, t), 100 * HOUR);
    assert_eq!(duration(&p, t), 12 * DAY + 4 * HOUR);
    assert_units(assignment(&p, a).units, 1.0);
}

// ===========================================================================
// Microsoft's worked example: Fixed Duration
// ===========================================================================

// Source: https://support.microsoft.com/en-us/project/change-the-task-type-for-more-accurate-scheduling — "Scenario 3: Changing task type to fixed-duration"
// Adding a second full-time person to the (effort-driven) Fixed Duration task keeps 10 days and 80 h: 40 h each at 50%.
#[test]

fn fixed_duration_effort_driven_add_resource_lowers_units() {
    let (mut p, t, a1) = ms_example(TaskType::FixedDuration, true);
    let a2 = add_second_person(&mut p, t);
    assert_eq!(duration(&p, t), 10 * DAY);
    assert_eq!(work(&p, t), 80 * HOUR);
    for a in [a1, a2] {
        assert_eq!(assignment(&p, a).work, 40 * HOUR);
        assert_units(assignment(&p, a).units, 0.5);
    }
}

// Source: https://support.microsoft.com/en-us/project/change-the-task-type-for-more-accurate-scheduling — "Scenario 3: Changing task type to fixed-duration"
// Shortening the Fixed Duration task to 8 days lowers the work to 64 h at 100%.
#[test]
fn fixed_duration_shorten_duration_reduces_work() {
    let (mut p, t, a) = ms_example(TaskType::FixedDuration, false);
    p.set_duration(t, 8 * DAY, false).unwrap();
    assert_eq!(work(&p, t), 64 * HOUR);
    assert_units(assignment(&p, a).units, 1.0);
}

// Source: https://support.microsoft.com/en-us/project/change-the-task-type-for-more-accurate-scheduling — "Scenario 3: Changing task type to fixed-duration"
// Adding 20 h of work to the Fixed Duration task keeps 10 days and raises the units to 125%.
#[test]
fn fixed_duration_add_work_raises_units() {
    let (mut p, t, a) = ms_example(TaskType::FixedDuration, false);
    p.set_assignment_work(a, 100 * HOUR).unwrap();
    assert_eq!(work(&p, t), 100 * HOUR);
    assert_eq!(duration(&p, t), 10 * DAY);
    assert_units(assignment(&p, a).units, 1.25);
}

// ===========================================================================
// Effort-driven scheduling
// ===========================================================================

// Source: https://support.microsoft.com/en-us/project/change-the-effort-driven-setting-for-task-types — "First assignment"
// Effort-driven calculations only start after the first assignment, so the first person on a 10-day task gets 80 h and the duration stays.
#[test]
fn effort_driven_does_not_apply_to_first_assignment() {
    let (p, t, a) = ms_example(TaskType::FixedUnits, true);
    assert_eq!(duration(&p, t), 10 * DAY);
    assert_eq!(assignment(&p, a).work, 80 * HOUR);
}

// Source: https://support.microsoft.com/en-us/project/change-the-effort-driven-setting-for-task-types — "Fixed unit tasks"
// On an effort-driven Fixed Units task, adding a half-time person to an 80 h task at 100% shortens it to 80/1.5 h while total work stays 80 h.
#[test]
fn effort_driven_fixed_units_partial_resource_shortens_duration() {
    let (mut p, t, _) = ms_example(TaskType::FixedUnits, true);
    let r = p.add_resource("Half-timer");
    p.assign(t, r, Some(0.5), None).unwrap();
    assert_eq!(work(&p, t), 80 * HOUR);
    // 80 h / 1.5 units = 53h20m.
    assert_eq!(duration(&p, t), 80 * HOUR * 2 / 3);
}

// Source: https://support.microsoft.com/en-us/project/change-the-effort-driven-setting-for-task-types — "Change the effort driven setting for task types"
// Removing a person from an effort-driven task lengthens the duration while the task's total work is unchanged (80 h back on one person: 10 days).
#[test]

fn effort_driven_remove_resource_lengthens_duration() {
    let (mut p, t, _) = ms_example(TaskType::FixedUnits, true);
    let a2 = add_second_person(&mut p, t);
    assert_eq!(duration(&p, t), 5 * DAY);
    p.unassign(a2).unwrap();
    assert_eq!(work(&p, t), 80 * HOUR);
    assert_eq!(duration(&p, t), 10 * DAY);
}

// Source: https://support.microsoft.com/en-us/project/change-the-effort-driven-setting-for-task-types — "Change the effort driven setting for task types"
// With effort-driven scheduling off, total work grows as people are added: a second full-time person doubles the work and keeps the duration.
#[test]
fn not_effort_driven_add_resource_increases_work() {
    let (mut p, t, _) = ms_example(TaskType::FixedUnits, false);
    add_second_person(&mut p, t);
    assert_eq!(duration(&p, t), 10 * DAY);
    assert_eq!(work(&p, t), 160 * HOUR);
}

// ===========================================================================
// Link types
// ===========================================================================

// Source: https://support.microsoft.com/en-us/project/link-tasks-in-a-project — "Types of tasks"
// Finish-to-start: B cannot begin until A is complete (A Mon–Wed, so B starts Thursday morning).
#[test]
fn finish_to_start_successor_starts_after_predecessor_finishes() {
    let mut p = project();
    let a = task(&mut p, "A", 3 * DAY);
    let b = task(&mut p, "B", 2 * DAY);
    p.link(a.into(), b, LinkType::FS, Lag::None).unwrap();
    recalculate(&mut p);
    assert_eq!(finish(&p, a), dt(2026, 10, 21, 17));
    assert_eq!(start(&p, b), dt(2026, 10, 22, 8));
    assert_eq!(finish(&p, b), dt(2026, 10, 23, 17));
}

// Source: https://support.microsoft.com/en-us/project/link-tasks-in-a-project — "Types of tasks"
// Start-to-start: B cannot begin until A begins (A starts Wednesday after a 2-day predecessor, so B starts Wednesday too).
#[test]
fn start_to_start_successor_starts_with_predecessor() {
    let mut p = project();
    let x = task(&mut p, "X", 2 * DAY);
    let a = task(&mut p, "A", 5 * DAY);
    let b = task(&mut p, "B", 2 * DAY);
    p.link(x.into(), a, LinkType::FS, Lag::None).unwrap();
    p.link(a.into(), b, LinkType::SS, Lag::None).unwrap();
    recalculate(&mut p);
    assert_eq!(start(&p, a), dt(2026, 10, 21, 8));
    assert_eq!(start(&p, b), dt(2026, 10, 21, 8));
}

// Source: https://support.microsoft.com/en-us/project/link-tasks-in-a-project — "Types of tasks"
// Finish-to-finish: B cannot be completed until A is completed (A Mon–Fri, so the 2-day B runs Thu–Fri).
#[test]
fn finish_to_finish_successor_finishes_with_predecessor() {
    let mut p = project();
    let a = task(&mut p, "A", 5 * DAY);
    let b = task(&mut p, "B", 2 * DAY);
    p.link(a.into(), b, LinkType::FF, Lag::None).unwrap();
    recalculate(&mut p);
    assert_eq!(finish(&p, b), finish(&p, a));
    assert_eq!(finish(&p, b), dt(2026, 10, 23, 17));
    assert_eq!(start(&p, b), dt(2026, 10, 22, 8));
}

// Source: https://support.microsoft.com/en-us/project/link-tasks-in-a-project — "Types of tasks"
// Start-to-finish: B cannot be completed until A begins (A starts Wednesday, so the 1-day B is done by Tuesday evening).
#[test]
fn start_to_finish_successor_finishes_when_predecessor_starts() {
    let mut p = project();
    let x = task(&mut p, "X", 2 * DAY);
    let a = task(&mut p, "A", 3 * DAY);
    let b = task(&mut p, "B", DAY);
    p.link(x.into(), a, LinkType::FS, Lag::None).unwrap();
    p.link(a.into(), b, LinkType::SF, Lag::None).unwrap();
    recalculate(&mut p);
    assert_eq!(start(&p, a), dt(2026, 10, 21, 8));
    assert_eq!(start(&p, b), dt(2026, 10, 20, 8));
    assert_eq!(finish(&p, b), dt(2026, 10, 20, 17));
}

// Source: https://support.microsoft.com/en-us/project/link-tasks-in-a-project — "Link tasks in a project"
// Every change to the predecessor affects the successor: lengthening A from 2 to 4 days moves its FS successor from Wednesday to Friday.
#[test]
fn successor_follows_predecessor_changes() {
    let mut p = project();
    let a = task(&mut p, "A", 2 * DAY);
    let b = task(&mut p, "B", DAY);
    let c = task(&mut p, "C", DAY);
    p.link_chain(&[a, b, c]).unwrap();
    recalculate(&mut p);
    assert_eq!(start(&p, b), dt(2026, 10, 21, 8));
    assert_eq!(start(&p, c), dt(2026, 10, 22, 8));
    p.set_duration(a, 4 * DAY, false).unwrap();
    recalculate(&mut p);
    assert_eq!(start(&p, b), dt(2026, 10, 23, 8));
    assert_eq!(start(&p, c), dt(2026, 10, 26, 8));
}

// ===========================================================================
// Lead and lag time
// ===========================================================================

// Source: https://support.microsoft.com/en-us/project/add-lead-or-lag-time-to-a-task — "Add lead or lag time"
// A finish-to-start link with 2d lag puts two days between A's finish and B's start (A ends Wed, B starts Mon after the weekend).
#[test]
fn finish_to_start_two_day_lag_delays_successor() {
    let mut p = project();
    let a = task(&mut p, "A", 3 * DAY);
    let b = task(&mut p, "B", DAY);
    p.link(a.into(), b, LinkType::FS, Lag::Working(2 * DAY)).unwrap();
    recalculate(&mut p);
    assert_eq!(finish(&p, a), dt(2026, 10, 21, 17));
    assert_eq!(start(&p, b), dt(2026, 10, 26, 8));
}

// Source: https://support.microsoft.com/en-us/project/add-lead-or-lag-time-to-a-task — "Add lead or lag time"
// A finish-to-start link with -50% lead lets B start once A is half complete (4-day A from Monday: B starts Wednesday morning).
#[test]
fn finish_to_start_fifty_percent_lead_overlaps_successor() {
    let mut p = project();
    let a = task(&mut p, "A", 4 * DAY);
    let b = task(&mut p, "B", 2 * DAY);
    p.link(a.into(), b, LinkType::FS, Lag::Percent(-50)).unwrap();
    recalculate(&mut p);
    assert_eq!(finish(&p, a), dt(2026, 10, 22, 17));
    assert_eq!(start(&p, b), dt(2026, 10, 21, 8));
}

// Source: https://support.microsoft.com/en-us/project/link-tasks-in-a-project — "Change or remove task dependencies"
// Lead time is a negative lag: -2 days lets B start two working days before A finishes (A Mon–Fri, B starts Thursday).
#[test]
fn negative_lag_is_lead_time() {
    let mut p = project();
    let a = task(&mut p, "A", 5 * DAY);
    let b = task(&mut p, "B", DAY);
    p.link(a.into(), b, LinkType::FS, Lag::Working(-2 * DAY)).unwrap();
    recalculate(&mut p);
    assert_eq!(start(&p, b), dt(2026, 10, 22, 8));
}

// Source: https://support.microsoft.com/en-us/project/task-information-dialog-box-predecessors-tab — "Lag"
// A percentage lag is a percentage of the predecessor's duration: +20% of a 5-day A is 1 day, so B starts Tuesday after A ends Friday.
#[test]
fn percentage_lag_uses_predecessor_duration() {
    let mut p = project();
    let a = task(&mut p, "A", 5 * DAY);
    let b = task(&mut p, "B", 10 * DAY);
    p.link(a.into(), b, LinkType::FS, Lag::Percent(20)).unwrap();
    recalculate(&mut p);
    assert_eq!(finish(&p, a), dt(2026, 10, 23, 17));
    assert_eq!(start(&p, b), dt(2026, 10, 27, 8));
}

// Source: https://support.microsoft.com/en-us/project/task-information-dialog-box-predecessors-tab — "Lag"
// A percentage lag also applies to start-to-start links: SS+50% of a 4-day A starts B halfway through A (Wednesday morning).
#[test]
fn start_to_start_percentage_lag() {
    let mut p = project();
    let a = task(&mut p, "A", 4 * DAY);
    let b = task(&mut p, "B", DAY);
    p.link(a.into(), b, LinkType::SS, Lag::Percent(50)).unwrap();
    recalculate(&mut p);
    assert_eq!(start(&p, b), dt(2026, 10, 21, 8));
}

// Source: https://support.microsoft.com/en-us/project/task-information-dialog-box-predecessors-tab — "Lag"
// An elapsed lag (e.g. 2ed) counts calendar time rather than working time: A ends Fri 17:00, +2 elapsed days is Sun 17:00, so B starts Monday.
#[test]
fn elapsed_lag_counts_calendar_time() {
    let mut p = project();
    let a = task(&mut p, "A", 5 * DAY);
    let b = task(&mut p, "B", DAY);
    p.link(a.into(), b, LinkType::FS, Lag::Elapsed(2 * 24 * HOUR)).unwrap();
    recalculate(&mut p);
    assert_eq!(start(&p, b), dt(2026, 10, 26, 8));
}

// ===========================================================================
// Constraints
// ===========================================================================

// Source: https://support.microsoft.com/en-us/project/set-a-task-start-or-finish-date-constraint-for-a-task — "Available constraints"
// Start No Earlier Than: the task starts on or after the date, so a later SNET date delays a linked task past its predecessor.
#[test]
fn start_no_earlier_than_delays_task() {
    let mut p = project();
    let a = task(&mut p, "A", 2 * DAY);
    let b = task(&mut p, "B", DAY);
    p.link(a.into(), b, LinkType::FS, Lag::None).unwrap();
    constrain(&mut p, b, ConstraintKind::Snet, dt(2026, 10, 23, 8));
    recalculate(&mut p);
    assert_eq!(start(&p, b), dt(2026, 10, 23, 8));
}

// Source: https://support.microsoft.com/en-us/project/set-a-task-start-or-finish-date-constraint-for-a-task — "Available constraints"
// Start No Earlier Than only sets a lower bound: with a SNET date before the link-driven start, the task still waits for its predecessor.
#[test]
fn start_no_earlier_than_does_not_pull_task_earlier() {
    let mut p = project();
    let a = task(&mut p, "A", 2 * DAY);
    let b = task(&mut p, "B", DAY);
    p.link(a.into(), b, LinkType::FS, Lag::None).unwrap();
    constrain(&mut p, b, ConstraintKind::Snet, dt(2026, 10, 19, 8));
    recalculate(&mut p);
    assert_eq!(start(&p, b), dt(2026, 10, 21, 8));
}

// Source: https://support.microsoft.com/en-us/project/set-a-task-start-or-finish-date-constraint-for-a-task — "Available constraints"
// Finish No Earlier Than: the task finishes on or after the date (a 2-day FNET Friday task runs Thu–Fri).
#[test]
fn finish_no_earlier_than_delays_finish() {
    let mut p = project();
    let t = task(&mut p, "T", 2 * DAY);
    constrain(&mut p, t, ConstraintKind::Fnet, dt(2026, 10, 23, 17));
    recalculate(&mut p);
    assert_eq!(finish(&p, t), dt(2026, 10, 23, 17));
    assert_eq!(start(&p, t), dt(2026, 10, 22, 8));
}

// Source: https://support.microsoft.com/en-us/project/set-a-task-start-or-finish-date-constraint-for-a-task — "Available constraints"
// Start No Later Than / Finish No Later Than are upper bounds only: an unlinked ASAP task still starts at the project start.
#[test]
fn no_later_than_constraints_do_not_delay_task() {
    let mut p = project();
    let s = task(&mut p, "SNLT", 2 * DAY);
    let f = task(&mut p, "FNLT", 2 * DAY);
    constrain(&mut p, s, ConstraintKind::Snlt, dt(2026, 10, 22, 8));
    constrain(&mut p, f, ConstraintKind::Fnlt, dt(2026, 10, 30, 17));
    recalculate(&mut p);
    assert_eq!(start(&p, s), dt(2026, 10, 19, 8));
    assert_eq!(start(&p, f), dt(2026, 10, 19, 8));
    assert_eq!(finish(&p, f), dt(2026, 10, 20, 17));
}

// Source: https://support.microsoft.com/en-us/project/set-a-task-start-or-finish-date-constraint-for-a-task — "Available constraints"
// Must Start On: the task starts on the given date even with no predecessor pushing it there.
#[test]
fn must_start_on_fixes_start() {
    let mut p = project();
    let t = task(&mut p, "T", 2 * DAY);
    constrain(&mut p, t, ConstraintKind::Mso, dt(2026, 10, 28, 8));
    recalculate(&mut p);
    assert_eq!(start(&p, t), dt(2026, 10, 28, 8));
    assert_eq!(finish(&p, t), dt(2026, 10, 29, 17));
}

// Source: https://support.microsoft.com/en-us/project/link-tasks-in-a-project — "Troubleshooting"
// With no progress entered, an inflexible constraint (Must Start On) can take precedence over a task dependency.
#[test]
fn must_start_on_takes_precedence_over_link() {
    let mut p = project();
    let a = task(&mut p, "A", 3 * DAY);
    let b = task(&mut p, "B", DAY);
    p.link(a.into(), b, LinkType::FS, Lag::None).unwrap();
    constrain(&mut p, b, ConstraintKind::Mso, dt(2026, 10, 20, 8));
    recalculate(&mut p);
    assert_eq!(start(&p, b), dt(2026, 10, 20, 8));
}

// Source: https://support.microsoft.com/en-us/project/set-a-task-start-or-finish-date-constraint-for-a-task — "Available constraints"
// Must Finish On: the task finishes on the given date (a 2-day MFO Wednesday task runs Tue–Wed).
#[test]
fn must_finish_on_fixes_finish() {
    let mut p = project();
    let t = task(&mut p, "T", 2 * DAY);
    constrain(&mut p, t, ConstraintKind::Mfo, dt(2026, 10, 28, 17));
    recalculate(&mut p);
    assert_eq!(finish(&p, t), dt(2026, 10, 28, 17));
    assert_eq!(start(&p, t), dt(2026, 10, 27, 8));
}

// ===========================================================================
// Inactive tasks, summary constraints and deadlines, earned value
// ===========================================================================

// Source: https://support.microsoft.com/en-us/project/inactivate-a-task — "Inactivate a task"
// An inactive task does not affect the schedule or how other tasks are scheduled, so it must not
// shorten its predecessors' slack.
#[test]
fn inactive_successor_does_not_drive_predecessor_slack() {
    let mut p = project();
    let _long = task(&mut p, "R", 10 * DAY);
    let pred = task(&mut p, "P", 2 * DAY);
    let inactive = task(&mut p, "Q", 20 * DAY);
    p.link(pred.into(), inactive, LinkType::FS, Lag::None).unwrap();
    p.task_mut(inactive).unwrap().active = false;
    recalculate(&mut p);
    assert_eq!(finish(&p, pred), dt(2026, 10, 20, 17));
    assert_eq!(p.task(pred).unwrap().sched.total_slack, 8 * DAY);
    assert!(!p.task(pred).unwrap().sched.critical);
}

// Source: https://support.microsoft.com/en-us/office/view-and-track-scheduling-factors-647becca-89da-4f58-a790-ba0ee8765683 — "Summary Task"
// A Start No Earlier Than constraint on a summary task drives its subtasks' start dates.
#[test]
fn summary_start_no_earlier_than_drives_subtasks() {
    let mut p = project();
    let s = p.add_task("Phase", None);
    let a = task(&mut p, "A", 2 * DAY);
    let b = task(&mut p, "B", DAY);
    p.indent(a).unwrap();
    p.indent(b).unwrap();
    constrain(&mut p, s, ConstraintKind::Snet, dt(2026, 11, 2, 8));
    recalculate(&mut p);
    assert_eq!(start(&p, a), dt(2026, 11, 2, 8));
    assert_eq!(start(&p, b), dt(2026, 11, 2, 8));
    assert_eq!(start(&p, s), dt(2026, 11, 2, 8));
}

// Source: https://support.microsoft.com/en-us/project/how-project-schedules-tasks-behind-the-scenes — "How do deadline dates affect the schedule?"
// Deadlines can be set on summary tasks; they affect total slack (not the dates), and a summary
// whose subtasks finish after its deadline shows a missed deadline.
#[test]
fn summary_deadline_reduces_subtask_slack_and_is_flagged() {
    let mut p = project();
    let s = p.add_task("Phase", None);
    let a = task(&mut p, "A", 5 * DAY);
    p.indent(a).unwrap();
    p.task_mut(s).unwrap().deadline = Some(dt(2026, 10, 21, 17));
    recalculate(&mut p);
    assert_eq!(start(&p, a), dt(2026, 10, 19, 8), "a deadline does not move the task");
    assert_eq!(p.task(a).unwrap().sched.total_slack, -2 * DAY);
    assert!(p.task(a).unwrap().sched.critical);
    assert!(p.task(s).unwrap().sched.warnings.iter().any(|w| w.contains("deadline")), "{:?}", p.task(s).unwrap().sched.warnings);
}

// Source: https://support.microsoft.com/en-us/office/acwp-fields-710a1c21-b9f1-4516-9c78-986e87345277 — "How Calculated"
// ACWP is the cost of the work done so far; progress entered as % complete (not only actual work)
// makes Project calculate it. A 10-day task at $100/h that is 50% complete has cost $4,000 so far.
#[test]
fn acwp_counts_progress_entered_as_percent_complete() {
    let mut p = project();
    let t = task(&mut p, "Build", 10 * DAY);
    let r = p.add_resource("Ana");
    p.resource_mut(r).unwrap().standard_rate = 100.0;
    p.assign(t, r, Some(1.0), None).unwrap();
    recalculate(&mut p);
    p.set_baseline(&[]);
    p.set_percent_complete(t, 50).unwrap();
    recalculate(&mut p);
    let ev = plancraft_core::report::earned_value(&p, dt(2026, 10, 23, 17)).unwrap();
    assert!((ev.acwp - 4000.0).abs() < 1e-6, "ACWP {}", ev.acwp);
    assert!((ev.bcwp - 4000.0).abs() < 1e-6, "BCWP {}", ev.bcwp);
    assert!((ev.cpi - 1.0).abs() < 1e-9);
}

// ---------------------------------------------------------------------------
// Tracking: Stop/Resume, Update Project, Mark on Track
// ---------------------------------------------------------------------------

/// The plan used by the Update Project examples: A (2 days) → B (3 days) → C (2 days).
fn chain_abc() -> (Project, TaskId, TaskId, TaskId) {
    let mut p = project();
    let a = task(&mut p, "A", 2 * DAY);
    let b = task(&mut p, "B", 3 * DAY);
    let c = task(&mut p, "C", 2 * DAY);
    p.link_chain(&[a, b, c]).unwrap();
    recalculate(&mut p);
    (p, a, b, c)
}

fn pct(p: &Project, id: TaskId) -> u8 {
    p.task(id).unwrap().percent_complete
}

// Source: https://support.microsoft.com/en-us/office/stop-task-field-9664ffef-0df9-4b37-aeb2-8e26bdadcbcf — "Example"
// A two-day task marked 50% complete on its first day stops at the end of that day, and its
// remaining half is scheduled from the next morning.
#[test]
fn stop_marks_the_end_of_the_completed_part() {
    let mut p = project();
    let t = task(&mut p, "Conduct client meetings", 2 * DAY);
    recalculate(&mut p);
    p.set_percent_complete(t, 50).unwrap();
    recalculate(&mut p);
    let s = &p.task(t).unwrap().sched;
    assert_eq!(s.stop, Some(dt(2026, 10, 19, 17)));
    assert_eq!(s.resume, Some(dt(2026, 10, 20, 8)));
    assert_eq!(finish(&p, t), dt(2026, 10, 20, 17));
}

// Source: https://support.microsoft.com/en-us/office/resume-task-field-a6ae83cc-bde9-44f6-8bc0-aca8514de2b1 — "Examples"
// When the remaining half of a two-day, 50%-complete task can only happen on a later date,
// entering that date as the Resume date schedules the rest of the task there.
#[test]
fn resume_date_moves_the_remaining_work() {
    let mut p = project();
    let t = task(&mut p, "Conduct client meetings", 2 * DAY);
    recalculate(&mut p);
    p.set_percent_complete(t, 50).unwrap();
    p.set_resume(t, Some(dt(2026, 11, 2, 8))).unwrap();
    recalculate(&mut p);
    let s = &p.task(t).unwrap().sched;
    assert_eq!(start(&p, t), dt(2026, 10, 19, 8), "the actual start stays");
    assert_eq!(s.stop, Some(dt(2026, 10, 19, 17)));
    assert_eq!(s.resume, Some(dt(2026, 11, 2, 8)));
    assert_eq!(finish(&p, t), dt(2026, 11, 2, 17));
    // PlanCraft's choice (the page does not say): the pause does not count as duration.
    assert_eq!(duration(&p, t), 2 * DAY, "the gap is not working time");
}

// Source: https://support.microsoft.com/en-us/office/update-completed-tasks-quickly-9c963543-f767-4b5b-ac5b-f0f465206fdb — "Update tasks to 100% complete for a specific date"
// Update Project with "Set 0% or 100% complete only" marks the tasks that finish by the date as
// 100% complete and leaves the others alone.
#[test]
fn update_project_zero_or_hundred_completes_tasks_finished_by_the_date() {
    let (mut p, a, b, c) = chain_abc();
    let through = p.end_of_working_day(d(2026, 10, 21));
    let r = p.update_progress_through(&[], through, ProgressUpdate::ZeroOrHundred).unwrap();
    recalculate(&mut p);
    assert_eq!((pct(&p, a), pct(&p, b), pct(&p, c)), (100, 0, 0));
    assert_eq!(r.updated, vec![a]);
    assert_eq!(p.task(a).unwrap().actual_finish, Some(dt(2026, 10, 20, 17)));
}

// Source: https://support.microsoft.com/en-us/office/update-completed-tasks-quickly-9c963543-f767-4b5b-ac5b-f0f465206fdb — "Update tasks to scheduled percent complete for a specific date"
// With "Set 0% - 100% complete" each task gets the progress its schedule says it should have by
// the date: one day into a three-day task is a third done.
#[test]
fn update_project_sets_scheduled_percent_complete() {
    let (mut p, a, b, c) = chain_abc();
    let through = p.end_of_working_day(d(2026, 10, 21));
    p.update_progress_through(&[], through, ProgressUpdate::Scheduled).unwrap();
    recalculate(&mut p);
    assert_eq!((pct(&p, a), pct(&p, b), pct(&p, c)), (100, 33, 0));
    assert_eq!(p.task(b).unwrap().actual_start, Some(dt(2026, 10, 21, 8)));
}

// Source: https://support.microsoft.com/en-us/office/update-completed-tasks-quickly-9c963543-f767-4b5b-ac5b-f0f465206fdb — "Update tasks to scheduled percent complete for the current or status date"
// Mark on Track sets the selected tasks to their scheduled percent complete as of the status date.
#[test]
fn mark_on_track_uses_the_status_date() {
    let (mut p, a, b, c) = chain_abc();
    p.set_status_date(Some(d(2026, 10, 22)));
    p.mark_on_track(&[b]).unwrap();
    recalculate(&mut p);
    assert_eq!((pct(&p, a), pct(&p, b), pct(&p, c)), (0, 67, 0), "only the selected task changes");
}

// Source: https://support.microsoft.com/en-us/project/set-the-start-date-or-finish-date-for-your-project-in-project-desktop — "Move the start time of a project"
// "Reschedule uncompleted work to start after" schedules work not yet done after the chosen date,
// and actual dates on tasks do not move.
#[test]
fn reschedule_uncompleted_work_keeps_actuals_and_moves_the_rest() {
    let mut p = project();
    let a = task(&mut p, "A", 2 * DAY);
    let b = task(&mut p, "B", 4 * DAY);
    let c = task(&mut p, "C", 2 * DAY);
    let free = task(&mut p, "Unlinked", DAY);
    p.link_chain(&[a, b, c]).unwrap();
    recalculate(&mut p);
    p.set_percent_complete(a, 100).unwrap();
    recalculate(&mut p);
    p.set_percent_complete(b, 25).unwrap();
    recalculate(&mut p);
    assert_eq!(start(&p, b), dt(2026, 10, 21, 8));

    let after = p.end_of_working_day(d(2026, 10, 26));
    let r = p.reschedule_uncompleted_work(&[], after).unwrap();
    recalculate(&mut p);
    // How the work moves is PlanCraft's choice (the page only says it moves after the date and
    // actual dates stay): a task in progress pauses until then, and an unlinked task not started
    // gets a Start No Earlier Than date.
    assert_eq!(r.split, vec![b]);
    assert_eq!(r.constrained, vec![free], "C follows B through its link; only the unlinked task needs a date");
    assert!(r.skipped.is_empty(), "{:?}", r.skipped);

    assert_eq!((start(&p, a), finish(&p, a)), (dt(2026, 10, 19, 8), dt(2026, 10, 20, 17)), "actual dates stay");
    let s = &p.task(b).unwrap().sched;
    assert_eq!(start(&p, b), dt(2026, 10, 21, 8), "the actual start stays");
    assert_eq!(s.stop, Some(dt(2026, 10, 21, 17)));
    assert_eq!(s.resume, Some(dt(2026, 10, 27, 8)));
    assert_eq!(finish(&p, b), dt(2026, 10, 29, 17));
    assert_eq!(start(&p, c), dt(2026, 10, 30, 8));
    assert_eq!(start(&p, free), dt(2026, 10, 27, 8));
}

// Source: https://support.microsoft.com/en-us/project/update-work-on-a-project — "Update actual and remaining duration"
// Entering actual duration calculates % complete and remaining duration.
#[test]
fn actual_duration_sets_percent_complete() {
    let mut p = project();
    let t = task(&mut p, "Build", 10 * DAY);
    recalculate(&mut p);
    p.set_actual_duration(t, 4 * DAY).unwrap();
    recalculate(&mut p);
    let task = p.task(t).unwrap();
    assert_eq!(task.percent_complete, 40);
    assert_eq!(task.remaining_duration(), 6 * DAY);
    assert_eq!(task.actual_start, Some(dt(2026, 10, 19, 8)));
}

// Source: https://support.microsoft.com/en-us/project/update-work-on-a-project — "Update actual and remaining duration"
// Entering remaining duration recalculates the duration and % complete.
#[test]
fn remaining_duration_recalculates_duration_and_percent() {
    let mut p = project();
    let t = task(&mut p, "Build", 10 * DAY);
    recalculate(&mut p);
    p.set_percent_complete(t, 40).unwrap();
    p.set_remaining_duration(t, 12 * DAY).unwrap();
    recalculate(&mut p);
    let task = p.task(t).unwrap();
    assert_eq!(task.duration, 16 * DAY, "4 days done + 12 days left");
    assert_eq!(task.percent_complete, 25);
    assert_eq!(finish(&p, t), dt(2026, 11, 9, 17));
}

// Source: https://support.microsoft.com/en-us/project/update-work-on-a-project — "Update actual and remaining duration"
// An actual duration longer than the scheduled duration raises the duration to match, leaves no
// remaining duration and marks the task 100% complete.
#[test]
fn actual_duration_past_the_plan_completes_the_task() {
    let mut p = project();
    let t = task(&mut p, "Build", 2 * DAY);
    recalculate(&mut p);
    p.set_actual_duration(t, 3 * DAY).unwrap();
    recalculate(&mut p);
    let task = p.task(t).unwrap();
    assert_eq!((task.duration, task.percent_complete, task.remaining_duration()), (3 * DAY, 100, 0));
    assert_eq!(task.actual_finish, Some(dt(2026, 10, 21, 17)));
}

// ---------------------------------------------------------------------------
// Baselines and interim plans
// ---------------------------------------------------------------------------

/// Phase (summary) over A (2 days, fixed cost 100), plus D (1 day, fixed cost 50) added under it
/// after Baseline was saved for the whole plan.
fn baselined_phase() -> (Project, TaskId, TaskId, TaskId) {
    let mut p = project();
    let s = p.add_task("Phase", None);
    let a = task(&mut p, "A", 2 * DAY);
    p.indent(a).unwrap();
    p.task_mut(a).unwrap().fixed_cost = 100.0;
    recalculate(&mut p);
    p.set_baseline(&[]);
    let d = p.add_task("D", None);
    p.set_duration(d, DAY, false).unwrap();
    p.task_mut(d).unwrap().fixed_cost = 50.0;
    p.link(a.into(), d, LinkType::FS, Lag::None).unwrap();
    recalculate(&mut p);
    assert_eq!(p.parent_index(p.task_index(d).unwrap()), Some(0), "D is a subtask of Phase, like A");
    (p, s, a, d)
}

// Source: https://support.microsoft.com/en-us/office/create-or-update-a-baseline-or-an-interim-plan-in-project-desktop-7e775482-ac84-4f4a-bbd0-592f9ac91953 — "Update a baseline or an interim plan"
// Saving a baseline for selected tasks only does not roll it up into their summary task unless
// the roll-up options are chosen, so the summary (and the project total built from it) keeps
// its older baseline figures.
#[test]
fn baselining_selected_tasks_does_not_update_their_summary_baseline() {
    let (mut p, s, _, d) = baselined_phase();
    p.set_baseline_in(0, &Scope::selected(vec![d])).unwrap();
    assert_eq!(p.task(d).unwrap().baseline.unwrap().cost, 50.0);
    assert_eq!(p.task(s).unwrap().baseline.unwrap().cost, 100.0, "the summary keeps its baseline");
    assert_eq!(plancraft_core::report::project_stats(&p).baseline_cost, 100.0);
}

// Source: https://support.microsoft.com/en-us/office/create-or-update-a-baseline-or-an-interim-plan-in-project-desktop-7e775482-ac84-4f4a-bbd0-592f9ac91953 — "Set a baseline"
// "To all summary tasks" rolls the selected tasks' new baseline values up into their summary
// tasks, so the summary's baseline matches its subtasks again.
#[test]
fn rolling_a_selected_baseline_up_to_all_summary_tasks() {
    let (mut p, s, a, d) = baselined_phase();
    p.set_baseline_in(0, &Scope { tasks: vec![d], roll_up_to_summaries: true, ..Scope::default() }).unwrap();
    let sb = p.task(s).unwrap().baseline.unwrap();
    assert_eq!(sb.cost, 150.0, "A's 100 plus D's 50");
    assert_eq!(sb.start, p.task(a).unwrap().baseline.unwrap().start);
    assert_eq!(sb.finish, dt(2026, 10, 21, 17), "D's baseline finish (day 3)");
    assert_eq!(sb.duration, 3 * DAY);
    assert_eq!(plancraft_core::report::project_stats(&p).baseline_cost, 150.0);
}

// Source: https://support.microsoft.com/en-us/office/create-or-update-a-baseline-or-an-interim-plan-in-project-desktop-7e775482-ac84-4f4a-bbd0-592f9ac91953 — "Set a baseline"
// "From subtasks into selected summary task(s)" updates a selected summary's baseline from its
// subtasks' saved baselines, picking up a subtask that was added and baselined later.
#[test]
fn rolling_subtask_baselines_into_a_selected_summary() {
    let (mut p, s, _, d) = baselined_phase();
    p.set_baseline_in(0, &Scope::selected(vec![d])).unwrap();
    assert_eq!(p.task(s).unwrap().baseline.unwrap().cost, 100.0);
    p.set_baseline_in(0, &Scope { tasks: vec![s], roll_up_from_subtasks: true, ..Scope::default() }).unwrap();
    assert_eq!(p.task(s).unwrap().baseline.unwrap().cost, 150.0);
    assert_eq!(p.task(s).unwrap().baseline.unwrap().finish, dt(2026, 10, 21, 17));
}

// Source: https://support.microsoft.com/en-us/office/create-or-update-a-baseline-or-an-interim-plan-in-project-desktop-7e775482-ac84-4f4a-bbd0-592f9ac91953 — "Learn more about baselines and interim plans"
// A plan keeps up to 11 baselines (Baseline and Baseline1–10); saving one leaves the others as
// they were, so later snapshots can be compared with the original plan.
#[test]
fn eleven_independent_baselines() {
    let mut p = project();
    let a = task(&mut p, "A", 2 * DAY);
    recalculate(&mut p);
    p.set_baseline_in(0, &Scope::entire_project()).unwrap();
    p.set_duration(a, 4 * DAY, false).unwrap();
    recalculate(&mut p);
    for n in 1..=10 {
        p.set_baseline_in(n, &Scope::entire_project()).unwrap();
    }
    assert!(p.set_baseline_in(11, &Scope::entire_project()).is_err(), "there is no Baseline11");
    let t = p.task(a).unwrap();
    assert_eq!(t.baseline_in(0).unwrap().finish, dt(2026, 10, 20, 17));
    assert!((1..=10).all(|n| t.baseline_in(n).unwrap().finish == dt(2026, 10, 22, 17)));
}

// Source: https://support.microsoft.com/en-us/office/create-or-update-a-baseline-or-an-interim-plan-in-project-desktop-7e775482-ac84-4f4a-bbd0-592f9ac91953 — "Analyze baseline and interim data"
// Variance is calculated from Baseline only, not from Baseline1–10: with only Baseline1 saved,
// a task has no baseline to vary from.
#[test]
fn variance_uses_baseline_not_the_numbered_ones() {
    let mut p = project();
    let a = task(&mut p, "A", 2 * DAY);
    recalculate(&mut p);
    p.set_baseline_in(1, &Scope::entire_project()).unwrap();
    let stats = plancraft_core::report::project_stats(&p);
    assert_eq!(stats.baseline_finish, None);
    assert_eq!(stats.baseline_cost, 0.0);
    assert!(p.task(a).unwrap().baseline.is_none());
}

// Source: https://support.microsoft.com/en-us/office/create-or-update-a-baseline-or-an-interim-plan-in-project-desktop-7e775482-ac84-4f4a-bbd0-592f9ac91953 — "Set an interim plan"
// Copying into an interim plan keeps only start and finish dates; copying one baseline into
// another copies all of its values.
#[test]
fn interim_plans_hold_dates_and_baseline_copies_hold_everything() {
    let (mut p, _, a, _) = baselined_phase();
    let all = Scope::entire_project();
    assert_eq!(p.save_plan(PlanSlot::Scheduled, PlanSlot::Interim(1), &all).unwrap(), 3);
    let t = p.task(a).unwrap();
    assert_eq!(t.interim_in(1).map(|i| (i.start, i.finish)), t.sched.start.zip(t.sched.finish));
    p.save_plan(PlanSlot::Baseline(0), PlanSlot::Baseline(4), &all).unwrap();
    let (b0, b4) = (p.task(a).unwrap().baseline_in(0).copied().unwrap(), p.task(a).unwrap().baseline_in(4).copied().unwrap());
    assert_eq!((b4.start, b4.finish, b4.duration, b4.work, b4.cost), (b0.start, b0.finish, b0.duration, b0.work, b0.cost));
    p.save_plan(PlanSlot::Baseline(4), PlanSlot::Interim(2), &all).unwrap();
    assert_eq!(p.task(a).unwrap().interim_in(2).map(|i| (i.start, i.finish)), Some((b0.start, b0.finish)));
}

// Source: https://learn.microsoft.com/en-us/office/vba/api/project.application.baselineclear — "Parameters"
// Clearing works on all tasks or only the selected ones, for a baseline or for a Start/Finish
// (interim) pair.
#[test]
fn clearing_a_numbered_baseline_or_interim_plan() {
    let (mut p, s, a, d) = baselined_phase();
    let all = Scope::entire_project();
    p.set_baseline_in(2, &all).unwrap();
    p.save_plan(PlanSlot::Scheduled, PlanSlot::Interim(5), &all).unwrap();
    assert_eq!(p.clear_plan(PlanSlot::Baseline(2), &[a]).unwrap(), 1);
    assert!(p.task(a).unwrap().baseline_in(2).is_none());
    assert!(p.task(d).unwrap().baseline_in(2).is_some() && p.task(s).unwrap().baseline_in(2).is_some());
    assert!(p.task(a).unwrap().baseline.is_some(), "Baseline is untouched");
    assert_eq!(p.clear_plan(PlanSlot::Interim(5), &[]).unwrap(), 3);
    assert!(p.tasks.iter().all(|t| t.interim.is_empty()));
}

// Source: https://support.microsoft.com/en-us/office/earned-value-method-task-field-4f2df39a-c696-4dd5-bd62-bb0ea26a4a33 — "Remarks"
// Project Options let you pick which of the 11 baselines earned value is calculated from.
#[test]
fn earned_value_uses_the_chosen_baseline() {
    let mut p = project();
    let t = task(&mut p, "Build", 10 * DAY);
    p.task_mut(t).unwrap().fixed_cost = 1000.0;
    recalculate(&mut p);
    p.set_baseline_in(0, &Scope::entire_project()).unwrap();
    p.task_mut(t).unwrap().fixed_cost = 3000.0;
    recalculate(&mut p);
    p.set_baseline_in(3, &Scope::entire_project()).unwrap();
    p.set_percent_complete(t, 50).unwrap();
    recalculate(&mut p);
    let at = dt(2026, 10, 30, 17);
    let ev0 = plancraft_core::report::earned_value(&p, at).unwrap();
    assert!((ev0.bcwp - 500.0).abs() < 1e-6, "BCWP against Baseline {}", ev0.bcwp);
    p.set_earned_value_baseline(3).unwrap();
    let ev3 = plancraft_core::report::earned_value(&p, at).unwrap();
    assert_eq!(ev3.baseline, 3);
    assert!((ev3.bcwp - 1500.0).abs() < 1e-6, "BCWP against Baseline3 {}", ev3.bcwp);
    p.set_earned_value_baseline(5).unwrap();
    assert!(plancraft_core::report::earned_value(&p, at).is_none(), "Baseline5 was never saved");
}

// ---------------------------------------------------------------------------
// Network Diagram layout
// ---------------------------------------------------------------------------

fn network(p: &Project, critical_first: bool) -> plancraft_core::network::Diagram {
    use plancraft_core::network::{NetworkOptions, boxes_for, layout};
    let shown = vec![true; p.tasks.len()];
    layout(p, &boxes_for(p, &shown, &Default::default()), NetworkOptions { critical_first, ids_only: false })
}

fn cell(d: &plancraft_core::network::Diagram, p: &Project, id: TaskId) -> (u32, u32) {
    let n = &d.nodes[d.node_of(p.task_index(id).unwrap()).unwrap()];
    (n.col, n.row)
}

// Source: https://support.microsoft.com/en-us/office/layout-dialog-box-aab7e026-13f6-428e-a20a-0825865537cc — "Box Layout" (Top Down From Left)
// The default arrangement starts the first path at the upper left and runs it across; each further
// path starts on a new row below.
#[test]
fn network_diagram_runs_each_path_along_its_own_row() {
    let mut p = project();
    let a = task(&mut p, "A", DAY);
    let b = task(&mut p, "B", DAY);
    let c = task(&mut p, "C", DAY);
    let other = task(&mut p, "Other start", DAY);
    let d2 = task(&mut p, "D", DAY);
    p.link_chain(&[a, b, c]).unwrap();
    p.link_chain(&[other, d2]).unwrap();
    recalculate(&mut p);
    let d = network(&p, false);
    assert_eq!([cell(&d, &p, a), cell(&d, &p, b), cell(&d, &p, c)], [(0, 0), (1, 0), (2, 0)]);
    assert_eq!([cell(&d, &p, other), cell(&d, &p, d2)], [(0, 1), (1, 1)]);
}

// Source: https://support.microsoft.com/en-us/office/layout-dialog-box-aab7e026-13f6-428e-a20a-0825865537cc — "Box Layout" (Top Down - Critical First)
// With "Critical First" the critical path is laid out first, from the upper left; other paths go
// on the rows below it.
#[test]
fn network_diagram_critical_first_puts_the_critical_path_on_top() {
    let mut p = project();
    let side = task(&mut p, "Side", DAY);
    let main = task(&mut p, "Main", 5 * DAY);
    let end = task(&mut p, "End", DAY);
    p.link(side.into(), end, LinkType::FS, Lag::None).unwrap();
    p.link(main.into(), end, LinkType::FS, Lag::None).unwrap();
    recalculate(&mut p);
    let d = network(&p, true);
    assert_eq!([cell(&d, &p, main), cell(&d, &p, end)], [(0, 0), (1, 0)]);
    assert_eq!(cell(&d, &p, side), (0, 1));
}

// Source: https://support.microsoft.com/en-us/office/layout-dialog-box-aab7e026-13f6-428e-a20a-0825865537cc — "Link style" (Rectilinear)
// Right-angled link lines leave a predecessor's box on its right side and go into the successor's
// box on its left side.
#[test]
fn network_diagram_links_leave_on_the_right_and_enter_on_the_left() {
    let mut p = project();
    let a = task(&mut p, "A", DAY);
    let b = task(&mut p, "B", DAY);
    let c = task(&mut p, "C", DAY);
    p.link(a.into(), b, LinkType::FS, Lag::None).unwrap();
    p.link(b.into(), c, LinkType::FS, Lag::None).unwrap();
    p.link(a.into(), c, LinkType::SS, Lag::None).unwrap();
    recalculate(&mut p);
    let d = network(&p, false);
    assert!(d.links.iter().any(|l| d.route(l).len() == 6), "A -> C skips B's column");
    for l in &d.links {
        let (from, to) = (d.rect(l.from).unwrap(), d.rect(l.to).unwrap());
        let pts = d.route(l);
        let (first, last) = (pts[0], pts[pts.len() - 1]);
        assert_eq!(first, (from.right(), from.mid_y()));
        assert_eq!(last, (to.x, to.mid_y()));
        // Right angles only.
        assert!(pts.windows(2).all(|w| w[0].0 == w[1].0 || w[0].1 == w[1].1), "{pts:?}");
    }
}
