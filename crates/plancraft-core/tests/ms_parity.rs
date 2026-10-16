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
