#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]
use plancraft_core::dates::prelude::*;
use plancraft_core::leveling::{LevelingOptions, level_resources};
use plancraft_core::prelude::*;

const DAY: Minutes = 480;

fn d(y: i32, m: u32, day: u32) -> Date {
    plancraft_core::dates::ymd(y, m, day).unwrap()
}
fn dt(y: i32, m: u32, day: u32, h: u32) -> DateTime {
    d(y, m, day).and_hms_opt(h, 0, 0).unwrap()
}

/// Monday 2026-10-19.
fn project() -> Project {
    Project::new("Test", d(2026, 10, 19))
}

fn task(p: &mut Project, name: &str, days: i64) -> TaskId {
    let id = p.add_task(name, None);
    p.set_duration(id, days * DAY, false).unwrap();
    id
}

fn start(p: &Project, id: TaskId) -> DateTime {
    p.task(id).unwrap().sched.start.unwrap()
}
fn finish(p: &Project, id: TaskId) -> DateTime {
    p.task(id).unwrap().sched.finish.unwrap()
}

#[test]
fn finish_to_start_chain_crosses_weekend() {
    let mut p = project();
    let a = task(&mut p, "A", 3);
    let b = task(&mut p, "B", 4);
    p.link(a.into(), b, LinkType::FS, Lag::None).unwrap();
    recalculate(&mut p);
    assert_eq!(start(&p, a), dt(2026, 10, 19, 8));
    assert_eq!(finish(&p, a), dt(2026, 10, 21, 17));
    assert_eq!(start(&p, b), dt(2026, 10, 22, 8));
    assert_eq!(finish(&p, b), dt(2026, 10, 27, 17));
    assert!(p.task(a).unwrap().sched.critical && p.task(b).unwrap().sched.critical);
}

#[test]
fn link_types_and_lags() {
    let mut p = project();
    let a = task(&mut p, "A", 5);
    let ss = task(&mut p, "SS+2d", 2);
    let ff = task(&mut p, "FF", 2);
    let sf = task(&mut p, "SF", 1);
    let lead = task(&mut p, "FS-1d", 2);
    let pct = task(&mut p, "FS+40%", 1);
    p.link(a.into(), ss, LinkType::SS, Lag::Working(2 * DAY)).unwrap();
    p.link(a.into(), ff, LinkType::FF, Lag::None).unwrap();
    p.link(a.into(), sf, LinkType::SF, Lag::Working(2 * DAY)).unwrap();
    p.link(a.into(), lead, LinkType::FS, Lag::Working(-DAY)).unwrap();
    p.link(a.into(), pct, LinkType::FS, Lag::Percent(40)).unwrap();
    recalculate(&mut p);
    // A: Mon 19 -> Fri 23
    assert_eq!(start(&p, ss), dt(2026, 10, 21, 8));
    assert_eq!(finish(&p, ff), dt(2026, 10, 23, 17));
    assert_eq!(start(&p, ff), dt(2026, 10, 22, 8));
    // SF: finish >= A.start + 2d = Tue 17:00 -> 1 day task Tue
    assert_eq!(finish(&p, sf), dt(2026, 10, 20, 17));
    assert_eq!(start(&p, lead), dt(2026, 10, 23, 8));
    // 40% of 5 days = 2 days after Fri 17:00 -> Tue 17:00, starts Wed
    assert_eq!(start(&p, pct), dt(2026, 10, 28, 8));
}

#[test]
fn elapsed_lag_ignores_calendar() {
    let mut p = project();
    let a = task(&mut p, "A", 5); // finishes Fri 23 17:00
    let b = task(&mut p, "B", 1);
    p.link(a.into(), b, LinkType::FS, Lag::Elapsed(2 * 24 * 60)).unwrap(); // Sun 17:00
    recalculate(&mut p);
    assert_eq!(start(&p, b), dt(2026, 10, 26, 8));
}

#[test]
fn milestones_and_members() {
    let mut p = project();
    let a = task(&mut p, "A", 2);
    let b = task(&mut p, "B", 4);
    let m = p.add_milestone("Done", None);
    p.add_milestone_members(m, &[a.into(), b.into()]).unwrap();
    recalculate(&mut p);
    assert_eq!(start(&p, m), finish(&p, b));
    assert_eq!(finish(&p, m), dt(2026, 10, 22, 17));
    let c = task(&mut p, "After", 1);
    p.link(m.into(), c, LinkType::FS, Lag::None).unwrap();
    recalculate(&mut p);
    assert_eq!(start(&p, c), dt(2026, 10, 23, 8));
    // Members can't create cycles.
    assert_eq!(p.add_milestone_members(m, &[c.into()]), Err(PlanError::CircularDependency));
}

#[test]
fn summaries_roll_up_and_pass_links_to_children() {
    let mut p = project();
    let s1 = p.add_task("Phase 1", None);
    let a = task(&mut p, "A", 2);
    let b = task(&mut p, "B", 3);
    p.indent(a).unwrap();
    p.indent(b).unwrap();
    let s2 = p.add_task("Phase 2", None);
    p.outdent(s2).unwrap();
    let c = task(&mut p, "C", 1);
    p.indent(c).unwrap();
    p.link(s1.into(), s2, LinkType::FS, Lag::None).unwrap();
    recalculate(&mut p);
    assert!(p.task(s1).unwrap().is_summary());
    assert_eq!(start(&p, s1), dt(2026, 10, 19, 8));
    assert_eq!(finish(&p, s1), dt(2026, 10, 21, 17));
    assert_eq!(start(&p, c), dt(2026, 10, 22, 8));
    assert_eq!(p.task(s1).unwrap().sched.duration, 3 * DAY);
    assert_eq!(p.wbs(p.task_index(c).unwrap()), "2.1");
    // Linking a task to its own summary is rejected.
    assert_eq!(p.link(s1.into(), a, LinkType::FS, Lag::None), Err(PlanError::SelfLink));
}

#[test]
fn cycles_are_rejected_and_reported() {
    let mut p = project();
    let a = task(&mut p, "A", 1);
    let b = task(&mut p, "B", 1);
    p.link(a.into(), b, LinkType::FS, Lag::None).unwrap();
    assert_eq!(p.link(b.into(), a, LinkType::FS, Lag::None), Err(PlanError::CircularDependency));
    // Force a cycle through the raw model and make sure scheduling terminates with a warning.
    p.task_mut(a).unwrap().predecessors.push(Dependency::fs(b));
    let report = recalculate(&mut p);
    assert!(report.warnings.iter().any(|w| w.contains("Circular")));
}

#[test]
fn constraints() {
    let mut p = project();
    let a = task(&mut p, "SNET", 2);
    p.task_mut(a).unwrap().constraint = Constraint { kind: ConstraintKind::Snet, date: Some(dt(2026, 10, 21, 8)) };
    let b = task(&mut p, "MFO", 2);
    p.task_mut(b).unwrap().constraint = Constraint { kind: ConstraintKind::Mfo, date: Some(dt(2026, 10, 30, 17)) };
    let c = task(&mut p, "ALAP", 1);
    p.task_mut(c).unwrap().constraint.kind = ConstraintKind::Alap;
    let e = task(&mut p, "succ of ALAP", 1);
    p.link(c.into(), e, LinkType::FS, Lag::None).unwrap();
    recalculate(&mut p);
    assert_eq!(start(&p, a), dt(2026, 10, 21, 8));
    assert_eq!(start(&p, b), dt(2026, 10, 29, 8));
    assert_eq!(finish(&p, b), dt(2026, 10, 30, 17));
    // Project finishes Oct 30; ALAP C is pushed late, its successor right after.
    assert_eq!(finish(&p, e), dt(2026, 10, 30, 17));
    assert_eq!(start(&p, c), dt(2026, 10, 29, 8));
}

#[test]
fn deadline_creates_negative_slack_warning() {
    let mut p = project();
    let a = task(&mut p, "A", 5);
    p.task_mut(a).unwrap().deadline = Some(dt(2026, 10, 21, 17));
    recalculate(&mut p);
    let t = p.task(a).unwrap();
    assert!(t.sched.total_slack < 0);
    assert!(t.sched.critical);
    assert!(!t.sched.warnings.is_empty());
}

#[test]
fn resource_calendars_and_vacation_push_tasks() {
    let mut p = project();
    let a = task(&mut p, "A", 3);
    let r = p.add_resource("Ana");
    p.resource_mut(r).unwrap().exceptions.push(CalendarException::non_working("Vacation", d(2026, 10, 20), d(2026, 10, 20)));
    p.assign(a, r, None, None).unwrap();
    recalculate(&mut p);
    // Mon, (Tue off), Wed, Thu
    assert_eq!(finish(&p, a), dt(2026, 10, 22, 17));
    p.task_mut(a).unwrap().ignore_resource_calendars = true;
    recalculate(&mut p);
    assert_eq!(finish(&p, a), dt(2026, 10, 21, 17));
}

#[test]
fn work_units_duration_relationships() {
    let mut p = project();
    let a = task(&mut p, "A", 4);
    let r = p.add_resource("Ana");
    let r2 = p.add_resource("Ben");
    let asg = p.assign(a, r, Some(0.5), None).unwrap();
    assert_eq!(p.assignment(asg).unwrap().work, 2 * DAY);
    // Fixed units: more work -> longer duration.
    p.set_assignment_work(asg, 4 * DAY).unwrap();
    assert_eq!(p.task(a).unwrap().duration, 8 * DAY);
    // Effort driven: adding a resource splits the work.
    p.task_mut(a).unwrap().effort_driven = true;
    p.assign(a, r2, Some(0.5), None).unwrap();
    assert_eq!(p.task(a).unwrap().duration, 4 * DAY);
    // Fixed duration: changing duration scales work.
    p.task_mut(a).unwrap().task_type = TaskType::FixedDuration;
    p.set_duration(a, 2 * DAY, false).unwrap();
    assert_eq!(p.assignment(asg).unwrap().work, DAY);
}

#[test]
fn estimates_drive_work_and_duration() {
    let mut p = project();
    let a = p.add_task("Estimated", None);
    p.task_mut(a).unwrap().estimate = Some(Estimate::Complexity { points: 5.0 }); // 5 * 4h = 20h
    recalculate(&mut p);
    assert_eq!(p.task(a).unwrap().sched.work, 20 * 60);
    let r = p.add_resource("Ana");
    p.assign(a, r, Some(0.5), None).unwrap();
    // 20h at 50% = 40h duration = 5 days
    assert_eq!(p.task(a).unwrap().duration, 5 * DAY);
    let b = p.add_task("PERT", None);
    p.task_mut(b).unwrap().estimate = Some(Estimate::ThreePoint { optimistic: 600, most_likely: 1200, pessimistic: 3000 });
    recalculate(&mut p);
    assert_eq!(p.task(b).unwrap().sched.work, 1400);
}

#[test]
fn time_tracking_and_progress() {
    let mut p = project();
    let a = task(&mut p, "A", 2);
    let r = p.add_resource("Ana");
    p.assign(a, r, None, None).unwrap();
    p.log_time(a, r, d(2026, 10, 19), 480, "day one").unwrap();
    p.update_progress_from_time();
    recalculate(&mut p);
    let t = p.task(a).unwrap();
    assert_eq!(t.sched.actual_work, 480);
    assert_eq!(t.percent_complete, 50);
    assert_eq!(t.actual_start, Some(dt(2026, 10, 19, 8)));
    p.set_percent_complete(a, 100).unwrap();
    assert!(p.task(a).unwrap().actual_finish.is_some());
}

#[test]
fn workload_detects_overallocation_and_leveling_fixes_it() {
    let mut p = project();
    let a = task(&mut p, "A", 3);
    let b = task(&mut p, "B", 2);
    let r = p.add_resource("Ana");
    p.assign(a, r, None, None).unwrap();
    p.assign(b, r, None, None).unwrap();
    recalculate(&mut p);
    let report = workload(&p, d(2026, 10, 19), d(2026, 10, 30), Granularity::Day);
    let ana = &report.resources[0];
    assert!(ana.overallocated);
    assert_eq!(ana.periods[0].load.round() as i64, 960);
    assert_eq!(ana.periods[0].capacity as i64, 480);

    let lr = level_resources(&mut p, &LevelingOptions::default());
    assert_eq!(lr.delayed.len(), 1);
    assert!(plancraft_core::workload::overallocations(&p).is_empty());
    assert_eq!(finish(&p, a).max(finish(&p, b)), dt(2026, 10, 23, 17));

    let weekly = workload(&p, d(2026, 10, 19), d(2026, 10, 30), Granularity::Week);
    assert_eq!(weekly.periods.len(), 2);
    assert_eq!(weekly.resources[0].periods[0].load.round() as i64, 5 * DAY);
}

#[test]
fn predecessor_text_round_trip() {
    let mut p = project();
    let a = task(&mut p, "A", 1);
    let b = task(&mut p, "B", 1);
    let c = task(&mut p, "C", 1);
    let deps = p.parse_predecessors("1, 2SS+2d").unwrap();
    p.set_predecessors(c, deps).unwrap();
    assert_eq!(p.format_predecessors(c), "1,2SS+2 days");
    assert!(p.parse_predecessors("9").is_err());
    let _ = (a, b);
    p.apply_resource_names(c, "Ana[50%], Ben").unwrap();
    assert_eq!(p.format_resource_names(c), "Ana[50%],Ben");
}

#[test]
fn delete_cleans_references() {
    let mut p = project();
    let a = task(&mut p, "A", 1);
    let b = task(&mut p, "B", 1);
    let r = p.add_resource("Ana");
    p.assign(a, r, None, None).unwrap();
    p.link(a.into(), b, LinkType::FS, Lag::None).unwrap();
    p.delete_tasks(&[a]).unwrap();
    assert!(p.task(b).unwrap().predecessors.is_empty());
    assert!(p.assignments.is_empty());
}

#[test]
fn move_and_shift_tasks() {
    let mut p = project();
    let a = task(&mut p, "A", 1);
    let b = task(&mut p, "B", 1);
    let c = task(&mut p, "C", 1);
    p.shift_task(c, true).unwrap();
    let names: Vec<_> = p.tasks.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names, ["A", "C", "B"]);
    p.move_task(a, 3).unwrap();
    let names: Vec<_> = p.tasks.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names, ["C", "B", "A"]);
    let _ = b;
}

/// Dragging rows: the moved tasks keep their order and subtasks, and become siblings of the task
/// they were dropped above.
#[test]
fn tasks_dragged_above_another_task() {
    let mut p = project();
    let a = task(&mut p, "A", 1);
    let s = task(&mut p, "S", 1);
    let s1 = task(&mut p, "S1", 1);
    let s2 = task(&mut p, "S2", 1);
    let b = task(&mut p, "B", 1);
    let c = task(&mut p, "C", 1);
    p.indent(s1).unwrap();
    p.indent(s2).unwrap();
    let outline =
        |p: &Project| p.tasks.iter().map(|t| format!("{}{}", " ".repeat(usize::from(t.outline_level - 1)), t.name)).collect::<Vec<_>>();

    // Two top-level tasks dropped among subtasks become subtasks, in their outline order.
    p.move_tasks_before(&[c, a], Some(s2)).unwrap();
    assert_eq!(outline(&p), ["S", " S1", " A", " C", " S2", "B"]);

    // A summary moves with its subtasks; a selected subtask of it does not move apart.
    p.move_tasks_before(&[s, s1], None).unwrap();
    assert_eq!(outline(&p), ["B", "S", " S1", " A", " C", " S2"]);
    p.move_tasks_before(&[s], Some(b)).unwrap();
    assert_eq!(outline(&p), ["S", " S1", " A", " C", " S2", "B"]);

    // Dropped on itself: nothing changes; inside itself: refused.
    p.move_tasks_before(&[s], Some(s)).unwrap();
    assert!(p.move_tasks_before(&[s], Some(a)).is_err());
    assert_eq!(outline(&p), ["S", " S1", " A", " C", " S2", "B"]);
    assert!(p.move_tasks_before(&[TaskId(9999)], None).is_err());
}

/// Regression: dragging a task in among the subtasks of a summary it is linked to (or indenting
/// it there) made it a subtask of a summary it is linked to, which `link` refuses: the summary
/// started before its predecessor finished, with no warning.
#[test]
fn an_outline_change_never_links_a_task_to_its_own_summary() {
    let mut p = project();
    let a = task(&mut p, "A", 1);
    let s = task(&mut p, "S", 1);
    let s1 = task(&mut p, "S1", 1);
    p.indent(s1).unwrap();
    p.link(a.into(), s, LinkType::FS, Lag::None).unwrap();
    let before = p.tasks.clone();
    let e = p.move_tasks_before(&[a], Some(s1)).unwrap_err().to_string();
    assert!(e.contains("own summary"), "{e}");
    assert_eq!(p.tasks, before, "nothing moved");
    // The same by indenting: a summary right after its predecessor.
    p.move_tasks_before(&[a], None).unwrap();
    let mut q = project();
    let s = task(&mut q, "S", 1);
    let x = task(&mut q, "X", 1);
    q.link(s.into(), x, LinkType::FS, Lag::None).unwrap();
    assert!(q.indent(x).is_err(), "X would become a subtask of its own predecessor");
    assert_eq!(q.tasks.iter().find(|t| t.id == x).unwrap().outline_level, 1);
    assert!(q.summary_self_links().is_empty());
}

/// Regression: an outline change could make links go round in a circle through a summary (S
/// links to X, X to Y, then Y moved under S), which linking itself refuses.
#[test]
fn an_outline_change_never_makes_links_go_round_in_a_circle() {
    let mut p = project();
    let s = task(&mut p, "S", 1);
    let s1 = task(&mut p, "S1", 1);
    let x = task(&mut p, "X", 1);
    let y = task(&mut p, "Y", 1);
    p.indent(s1).unwrap();
    p.link(s.into(), x, LinkType::FS, Lag::None).unwrap();
    p.link(x.into(), y, LinkType::FS, Lag::None).unwrap();
    let before = p.tasks.clone();
    let e = p.move_tasks_before(&[y], Some(s1)).unwrap_err().to_string();
    assert!(e.contains("circle"), "{e}");
    assert_eq!(p.tasks, before, "nothing moved");
    assert!(!recalculate(&mut p).warnings.iter().any(|w| w.contains("Circular")));
    // Moves that make no circle still go through.
    p.move_tasks_before(&[y], Some(s)).unwrap();
}

/// Regression: dropping a selection onto the first row of one of its own blocks moved nothing.
#[test]
fn a_selection_dropped_on_its_own_first_row_gathers_there() {
    let mut p = project();
    let t1 = task(&mut p, "T1", 1);
    let _t2 = task(&mut p, "T2", 1);
    let t3 = task(&mut p, "T3", 1);
    p.move_tasks_before(&[t1, t3], Some(t1)).unwrap();
    assert_eq!(p.tasks.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(), ["T1", "T3", "T2"]);
}

#[test]
fn save_load_and_external_links() {
    let dir = tempfile::tempdir().unwrap();
    let other_path = dir.path().join("platform.plancraft");
    let main_path = dir.path().join("app.plancraft");

    let mut other = Project::new("Platform", d(2026, 10, 19));
    let api = task(&mut other, "API ready", 10);
    recalculate(&mut other);
    plancraft_core::io::save(&other, &other_path).unwrap();

    let mut p = project();
    let ui = task(&mut p, "UI integration", 2);
    let ext = plancraft_core::io::add_external_project(&mut p, Some(&main_path), &other_path).unwrap();
    assert_eq!(p.external_projects[0].path, "platform.plancraft");
    plancraft_core::io::ensure_external_task(&mut p, Some(&main_path), ext, api).unwrap();
    p.link(TaskRef::External { project: ext, task: api }, ui, LinkType::FS, Lag::None).unwrap();
    recalculate(&mut p);
    // API: Oct 19 - Oct 30, so UI starts Nov 2.
    assert_eq!(start(&p, ui), dt(2026, 11, 2, 8));
    assert_eq!(p.format_predecessors(ui), format!("Platform\\{}", api.0));

    // The other project slips by 5 days; refreshing moves our task.
    other.set_duration(api, 15 * DAY, false).unwrap();
    recalculate(&mut other);
    plancraft_core::io::save(&other, &other_path).unwrap();
    let warnings = plancraft_core::io::refresh_external(&mut p, Some(&main_path));
    assert!(warnings.is_empty(), "{warnings:?}");
    recalculate(&mut p);
    assert_eq!(start(&p, ui), dt(2026, 11, 9, 8));

    plancraft_core::io::save(&p, &main_path).unwrap();
    let loaded = plancraft_core::io::load(&main_path).unwrap();
    assert_eq!(loaded.tasks.len(), 1);
    assert_eq!(start(&loaded, ui), dt(2026, 11, 9, 8));
}

#[test]
fn demo_project_is_consistent() {
    let p = plancraft_core::sample::demo_project(d(2026, 10, 19));
    assert!(p.sched.warnings.iter().all(|w| !w.contains("Circular")), "{:?}", p.sched.warnings);
    assert!(p.sched.finish.is_some());
    let stats = plancraft_core::report::project_stats(&p);
    assert_eq!(stats.milestones, 4);
    assert!(stats.work > 0);
    let json = plancraft_core::io::to_json(&p).unwrap();
    let back = plancraft_core::io::from_json(&json, std::path::Path::new("demo")).unwrap();
    assert_eq!(back.tasks.len(), p.tasks.len());
}

/// Summary S{A→B} with S –SS→ C. Returns (project, A, B, C).
fn summary_ss_link() -> (Project, TaskId, TaskId, TaskId) {
    let mut p = project();
    let s = p.add_task("S", None);
    let a = task(&mut p, "A", 5);
    let b = task(&mut p, "B", 5);
    p.indent(a).unwrap();
    p.indent(b).unwrap();
    p.link(a.into(), b, LinkType::FS, Lag::None).unwrap();
    let c = task(&mut p, "C", 20);
    while p.task(c).unwrap().outline_level > 1 {
        p.outdent(c).unwrap();
    }
    p.link(s.into(), c, LinkType::SS, Lag::None).unwrap();
    recalculate(&mut p);
    (p, a, b, c)
}

/// Regression: a link from a summary's start constrained every subtask, so later subtasks got
/// negative slack and were flagged critical although nothing could be late.
#[test]
fn a_start_link_from_a_summary_only_binds_the_subtask_that_starts_it() {
    let (p, a, b, c) = summary_ss_link();
    for id in [a, b, c] {
        assert!(p.task(id).unwrap().sched.total_slack >= 0, "{}: {}", p.task(id).unwrap().name, p.task(id).unwrap().sched.total_slack);
    }
    // C (20d) drives the finish and cannot start before A does, so A's start is critical (no
    // slack). B only follows A and has 10 days of slack (it used to get -5 days).
    assert_eq!(p.task(a).unwrap().sched.total_slack, 0);
    assert_eq!(p.task(b).unwrap().sched.total_slack, 10 * DAY);
    assert!(!p.task(b).unwrap().sched.critical);
    assert!(p.task(c).unwrap().sched.critical);
}

/// Regression: when a resource had no working day in the first 28 days after the project start,
/// its calendar was dropped and its task was scheduled in the middle of its leave.
#[test]
fn a_long_leave_at_the_project_start_delays_the_task() {
    let mut p = project();
    let t = task(&mut p, "Work", 5);
    let r = p.add_resource("Ana");
    p.resource_mut(r).unwrap().exceptions.push(CalendarException::non_working("Leave", d(2026, 10, 19), d(2026, 11, 20)));
    p.assign(t, r, Some(1.0), None).unwrap();
    recalculate(&mut p);
    assert_eq!(start(&p, t), dt(2026, 11, 23, 8));
}

/// Regression: the project's cost and baseline cost left out fixed costs on summary rows, so
/// the project total disagreed with its own top-level summary.
#[test]
fn summary_fixed_costs_count_in_project_and_baseline_totals() {
    let mut p = project();
    let s = p.add_task("Phase", None);
    let a = task(&mut p, "A", 2);
    p.indent(a).unwrap();
    p.task_mut(s).unwrap().fixed_cost = 1000.0;
    p.task_mut(a).unwrap().fixed_cost = 50.0;
    recalculate(&mut p);
    assert_eq!(p.task(s).unwrap().sched.cost, 1050.0);
    assert_eq!(p.sched.cost, 1050.0);
    p.set_baseline(&[]);
    assert_eq!(plancraft_core::report::project_stats(&p).baseline_cost, 1050.0);
}

/// Regression: indenting a task under one with resource assignments made it a summary and its
/// work and cost vanished from every total (the assignment was still there).
#[test]
fn indenting_under_an_assigned_task_keeps_its_work_and_cost() {
    let mut p = project();
    let parent = task(&mut p, "Design", 5);
    let r = p.add_resource("Ana");
    p.resource_mut(r).unwrap().standard_rate = 100.0;
    p.assign(parent, r, Some(1.0), None).unwrap();
    let child = task(&mut p, "Sketch", 1);
    recalculate(&mut p);
    let (work, cost) = (p.sched.work, p.sched.cost);
    assert_eq!(work, 5 * DAY);
    p.indent(child).unwrap();
    recalculate(&mut p);
    assert!(p.task(parent).unwrap().sched.is_summary);
    assert_eq!(p.sched.work, work, "project work");
    assert_eq!(p.sched.cost, cost, "project cost");
    assert_eq!(p.task(parent).unwrap().sched.work, work, "the summary carries its own assignment");
}

/// Regression: a summary weighted a nested summary by its span instead of by the durations of
/// the subtasks inside it, so the top summary disagreed with the project's own % complete.
#[test]
fn nested_summaries_weigh_progress_by_subtask_duration() {
    let mut p = project();
    let top = p.add_task("Top", None);
    let inner = p.add_task("Inner", None);
    let x = task(&mut p, "X", 10);
    let y = task(&mut p, "Y", 10);
    let z = task(&mut p, "Z", 10);
    p.indent(inner).unwrap();
    for id in [x, y] {
        p.indent(id).unwrap();
        p.indent(id).unwrap();
    }
    p.indent(z).unwrap();
    recalculate(&mut p);
    p.set_percent_complete(x, 100).unwrap();
    p.set_percent_complete(y, 100).unwrap();
    recalculate(&mut p);
    let top_pct = p.task(top).unwrap().sched.percent_complete;
    assert!((top_pct - 200.0 / 3.0).abs() < 0.01, "{top_pct}");
    assert!((p.sched.percent_complete - top_pct).abs() < 0.01);
}

/// Regression: an inactive summary still passed its Start No Earlier Than date and deadline down
/// to its active subtasks.
#[test]
fn an_inactive_summary_passes_no_constraint_or_deadline_down() {
    let mut p = project();
    let s = p.add_task("S", None);
    let a = task(&mut p, "A", 5);
    p.indent(a).unwrap();
    {
        let st = p.task_mut(s).unwrap();
        st.constraint = Constraint { kind: ConstraintKind::Snet, date: Some(dt(2026, 12, 1, 8)) };
        st.deadline = Some(dt(2026, 10, 20, 17));
        st.active = false;
    }
    recalculate(&mut p);
    assert_eq!(start(&p, a), dt(2026, 10, 19, 8));
    assert!(p.task(a).unwrap().sched.total_slack >= 0, "no slack lost to the inactive summary's deadline");
}

/// Regression: inactive subtasks still counted when a link from a summary's start was resolved,
/// so an inactive subtask that started earliest took the link and its active sibling was left
/// with slack it did not have.
#[test]
fn inactive_subtasks_do_not_take_a_link_from_their_summarys_start() {
    let mut p = project();
    let s = p.add_task("S", None);
    let a = task(&mut p, "A", 5);
    let b = task(&mut p, "B", 5);
    p.indent(a).unwrap();
    p.indent(b).unwrap();
    p.task_mut(b).unwrap().constraint = Constraint { kind: ConstraintKind::Snet, date: Some(dt(2026, 10, 21, 8)) };
    p.task_mut(a).unwrap().active = false;
    let c = task(&mut p, "C", 20);
    while p.task(c).unwrap().outline_level > 1 {
        p.outdent(c).unwrap();
    }
    p.link(s.into(), c, LinkType::SS, Lag::None).unwrap();
    recalculate(&mut p);
    assert_eq!(start(&p, c), dt(2026, 10, 21, 8), "C follows the active subtask's start");
    assert_eq!(p.task(b).unwrap().sched.total_slack, 0, "B's start drives C, which drives the finish");
}

// ---------------------------------------------------------------------------
// Manually scheduled summary tasks
// ---------------------------------------------------------------------------

/// `level`-1 task added at the end (outdented out of any summary above it).
fn top(p: &mut Project, name: &str, days: i64) -> TaskId {
    let id = task(p, name, days);
    while p.task(id).unwrap().outline_level > 1 {
        p.outdent(id).unwrap();
    }
    id
}

/// "Phase" (made manual over 2026-11-02 to 2026-11-06) with subtask A (2 days).
fn manual_phase(p: &mut Project) -> (TaskId, TaskId) {
    let s = top(p, "Phase", 1);
    let a = task(p, "A", 2);
    p.indent(a).unwrap();
    recalculate(p);
    p.set_manual_date(s, dt(2026, 11, 2, 8), false).unwrap();
    p.set_manual_date(s, dt(2026, 11, 6, 17), true).unwrap();
    recalculate(p);
    (s, a)
}

#[test]
fn successors_of_a_manual_summary_follow_its_own_dates() {
    let mut p = project();
    let (s, a) = manual_phase(&mut p);
    let next = top(&mut p, "Next", 1);
    p.link(s.into(), next, LinkType::FS, Lag::None).unwrap();
    recalculate(&mut p);
    // A finishes on 11-03, but the summary's own finish (11-06) drives the successor.
    assert_eq!(finish(&p, a), dt(2026, 11, 3, 17));
    assert_eq!(start(&p, next), dt(2026, 11, 9, 8));
    // Its dates are its own early and late dates, like a manual task's.
    let sched = &p.task(s).unwrap().sched;
    assert_eq!((sched.early_start, sched.late_finish), (Some(dt(2026, 11, 2, 8)), Some(dt(2026, 11, 6, 17))));
}

#[test]
fn links_into_summaries_around_a_manual_summary_stop_at_it() {
    let mut p = project();
    let driver = top(&mut p, "Driver", 10);
    let outer = top(&mut p, "Outer", 1);
    let (s, a) = manual_phase(&mut p);
    p.indent(s).unwrap();
    let auto = task(&mut p, "Auto", 1);
    while p.task(auto).unwrap().outline_level > 2 {
        p.outdent(auto).unwrap();
    }
    p.link(driver.into(), outer, LinkType::FS, Lag::None).unwrap();
    recalculate(&mut p);
    assert_eq!(p.task(a).unwrap().outline_level, 3);
    // Driver finishes 10-30: it pushes Outer's other subtask, not the manual summary's.
    assert_eq!(start(&p, auto), dt(2026, 11, 2, 8));
    assert_eq!(start(&p, a), dt(2026, 11, 2, 8));
    p.set_duration(driver, 20 * DAY, false).unwrap();
    recalculate(&mut p);
    assert_eq!(start(&p, auto), dt(2026, 11, 16, 8));
    assert_eq!(start(&p, a), dt(2026, 11, 2, 8));
    // The outer summary spans its subtasks, taking the manual summary's own dates.
    assert_eq!(start(&p, outer), dt(2026, 11, 2, 8));
    assert_eq!(finish(&p, outer), dt(2026, 11, 16, 17));
}

#[test]
fn the_project_spans_manual_summaries() {
    let mut p = project();
    let (s, _) = manual_phase(&mut p);
    p.set_manual_date(s, dt(2026, 12, 18, 17), true).unwrap();
    recalculate(&mut p);
    assert_eq!(p.sched.start, Some(dt(2026, 11, 2, 8)));
    assert_eq!(p.sched.finish, Some(dt(2026, 12, 18, 17)));
}

#[test]
fn a_manual_summary_without_a_typed_start_is_scheduled_like_an_automatic_one() {
    let mut p = project();
    let (s, a) = manual_phase(&mut p);
    let t = p.task_mut(s).unwrap();
    (t.manual_start, t.manual_finish) = (None, None);
    recalculate(&mut p);
    assert_eq!((start(&p, s), finish(&p, s)), (start(&p, a), finish(&p, a)));
    assert_eq!(start(&p, a), dt(2026, 10, 19, 8));
    assert_eq!(p.task(s).unwrap().sched.rollup_start, None);
}

#[test]
fn switching_a_summary_to_manual_keeps_its_dates_and_back_to_auto_rolls_up_again() {
    let mut p = project();
    let s = top(&mut p, "Phase", 1);
    let a = task(&mut p, "A", 3);
    p.indent(a).unwrap();
    recalculate(&mut p);
    p.set_schedule_mode(&[s], ScheduleMode::Manual).unwrap();
    recalculate(&mut p);
    assert_eq!((start(&p, s), finish(&p, s)), (dt(2026, 10, 19, 8), dt(2026, 10, 21, 17)));
    assert_eq!(p.task(s).unwrap().duration, 3 * DAY);
    p.set_duration(a, 5 * DAY, false).unwrap();
    recalculate(&mut p);
    assert_eq!(finish(&p, s), dt(2026, 10, 21, 17));
    assert_eq!(p.task(s).unwrap().sched.rollup_finish, Some(dt(2026, 10, 23, 17)));
    p.set_schedule_mode(&[s], ScheduleMode::Auto).unwrap();
    recalculate(&mut p);
    assert_eq!(finish(&p, s), dt(2026, 10, 23, 17));
    assert_eq!(p.task(s).unwrap().sched.rollup_finish, None);
}

#[test]
fn typed_dates_on_tasks() {
    let mut p = project();
    let a = task(&mut p, "A", 2);
    // Only manual tasks (and summaries) take typed dates.
    assert!(p.set_manual_date(a, dt(2026, 10, 21, 8), false).is_err());
    p.set_schedule_mode(&[a], ScheduleMode::Manual).unwrap();
    p.set_manual_date(a, dt(2026, 10, 19, 8), false).unwrap();
    p.set_manual_date(a, dt(2026, 10, 20, 17), true).unwrap();
    // A new start keeps the length; a finish before the start moves the start too.
    p.set_manual_date(a, dt(2026, 10, 26, 8), false).unwrap();
    assert_eq!(p.task(a).unwrap().manual_finish, Some(dt(2026, 10, 27, 17)));
    p.set_manual_date(a, dt(2026, 10, 22, 17), true).unwrap();
    assert_eq!(p.task(a).unwrap().manual_start, Some(dt(2026, 10, 22, 17)));
    recalculate(&mut p);
}

/// Issue 21: a plan from Microsoft Project with manually scheduled summaries (one linked from a
/// longer task, one unlinked) keeps their dates, and their subtasks follow them.
#[test]
fn manual_summaries_from_a_project_xml_file_keep_their_dates() {
    let task = |uid: u32, name: &str, level: u32, hours: u32, inner: &str| {
        format!(
            "<Task><UID>{uid}</UID><Name>{name}</Name><OutlineLevel>{level}</OutlineLevel><Duration>PT{hours}H0M0S</Duration>{inner}</Task>"
        )
    };
    let link = |pred: u32| format!("<PredecessorLink><PredecessorUID>{pred}</PredecessorUID><Type>1</Type></PredecessorLink>");
    let manual =
        |s: &str, f: &str| format!("<Summary>1</Summary><Manual>1</Manual><Start>{s}T08:00:00</Start><Finish>{f}T17:00:00</Finish>");
    let tasks = [
        task(1, "Driver", 1, 160, ""),
        task(2, "Linked phase", 1, 40, &(manual("2026-03-09", "2026-03-13") + &link(1))),
        task(3, "Work A", 2, 40, ""),
        task(4, "Unlinked phase", 1, 16, &manual("2026-04-13", "2026-04-14")),
        task(5, "Work B", 2, 16, ""),
    ]
    .concat();
    let xml = format!("<Project><StartDate>2026-03-02T08:00:00</StartDate><Tasks>{tasks}</Tasks></Project>");
    let (mut p, notes) = plancraft_core::mspdi::from_xml(&xml, "x").unwrap();
    assert!(notes.is_empty(), "{notes:?}");
    recalculate(&mut p);
    let dates = |i: usize| (p.tasks[i].sched.start.unwrap(), p.tasks[i].sched.finish.unwrap());
    assert_eq!(dates(0), (dt(2026, 3, 2, 8), dt(2026, 3, 27, 17)));
    for i in [1, 2] {
        assert_eq!(dates(i), (dt(2026, 3, 9, 8), dt(2026, 3, 13, 17)), "{}", p.tasks[i].name);
    }
    for i in [3, 4] {
        assert_eq!(dates(i), (dt(2026, 4, 13, 8), dt(2026, 4, 14, 17)), "{}", p.tasks[i].name);
    }
}

/// Issue 21, second report: a manual summary's start places only the subtasks that have nothing
/// else to go by. A subtask with its own constraint keeps its own date, even before the
/// summary's start, and so do its successors.
#[test]
fn a_subtask_with_its_own_constraint_is_not_held_to_a_manual_summarys_start() {
    let xml = "<Project><StartDate>2026-03-02T08:00:00</StartDate><Tasks>\
        <Task><UID>1</UID><Name>Build phase</Name><OutlineLevel>1</OutlineLevel><Summary>1</Summary><Manual>1</Manual>\
          <Start>2026-04-13T08:00:00</Start><Finish>2026-04-17T17:00:00</Finish><Duration>PT40H0M0S</Duration></Task>\
        <Task><UID>2</UID><Name>Preliminary checks</Name><OutlineLevel>2</OutlineLevel><Duration>PT24H0M0S</Duration>\
          <ConstraintType>4</ConstraintType><ConstraintDate>2026-03-09T08:00:00</ConstraintDate></Task>\
        <Task><UID>3</UID><Name>Assembly</Name><OutlineLevel>2</OutlineLevel><Duration>PT16H0M0S</Duration></Task>\
        <Task><UID>4</UID><Name>Review</Name><OutlineLevel>1</OutlineLevel><Duration>PT8H0M0S</Duration>\
          <PredecessorLink><PredecessorUID>2</PredecessorUID><Type>1</Type></PredecessorLink></Task>\
        </Tasks></Project>";
    let (mut p, notes) = plancraft_core::mspdi::from_xml(xml, "x").unwrap();
    assert!(notes.is_empty(), "every task lands where the file has it: {notes:?}");
    recalculate(&mut p);
    let dates = |i: usize| (p.tasks[i].sched.start.unwrap(), p.tasks[i].sched.finish.unwrap());
    assert_eq!(dates(0), (dt(2026, 4, 13, 8), dt(2026, 4, 17, 17)));
    assert_eq!(dates(1), (dt(2026, 3, 9, 8), dt(2026, 3, 11, 17)));
    assert_eq!(dates(2), (dt(2026, 4, 13, 8), dt(2026, 4, 14, 17)));
    assert_eq!(dates(3), (dt(2026, 3, 12, 8), dt(2026, 3, 12, 17)));
    // The summary keeps its dates; its subtasks' span runs from the early one.
    let sched = &p.tasks[0].sched;
    assert_eq!((sched.rollup_start, sched.rollup_finish), (Some(dt(2026, 3, 9, 8)), Some(dt(2026, 4, 14, 17))));
}

/// A subtask driven by its own link follows the link, not the manual summary's start; a
/// summary Start No Earlier Than date (on an automatic summary) still holds every subtask.
#[test]
fn a_linked_subtask_follows_its_link_not_a_manual_summarys_start() {
    let mut p = project();
    let driver = top(&mut p, "Driver", 2);
    let (s, a) = manual_phase(&mut p);
    let linked = task(&mut p, "Linked", 1);
    assert_eq!(p.task(linked).unwrap().outline_level, 2);
    p.link(driver.into(), linked, LinkType::FS, Lag::None).unwrap();
    recalculate(&mut p);
    assert_eq!(start(&p, s), dt(2026, 11, 2, 8));
    assert_eq!(start(&p, a), dt(2026, 11, 2, 8), "nothing else to go by: the summary's start");
    assert_eq!(start(&p, linked), dt(2026, 10, 21, 8), "after Driver, before the summary's start");
    // An automatic summary's Start No Earlier Than date is a constraint on every subtask.
    p.set_schedule_mode(&[s], ScheduleMode::Auto).unwrap();
    p.task_mut(s).unwrap().constraint = Constraint { kind: ConstraintKind::Snet, date: Some(dt(2026, 11, 2, 8)) };
    recalculate(&mut p);
    assert_eq!(start(&p, linked), dt(2026, 11, 2, 8));
}

// ---------------------------------------------------------------------------
// Elapsed durations
// ---------------------------------------------------------------------------

fn elapsed_task(p: &mut Project, name: &str, text: &str) -> TaskId {
    let id = top(p, name, 1);
    let typed = p.parse_task_duration(text).unwrap();
    p.set_typed_duration(id, typed).unwrap();
    id
}

#[test]
fn an_elapsed_task_ending_on_a_weekend_is_critical_and_its_successor_starts_on_monday() {
    let mut p = project();
    // 5 elapsed days from Monday 08:00 end Saturday 08:00.
    let cure = elapsed_task(&mut p, "Cure", "5ed");
    let next = top(&mut p, "Next", 2);
    p.link(cure.into(), next, LinkType::FS, Lag::None).unwrap();
    recalculate(&mut p);
    assert_eq!(finish(&p, cure), dt(2026, 10, 24, 8));
    assert_eq!(start(&p, next), dt(2026, 10, 26, 8));
    let t = p.task(cure).unwrap();
    assert_eq!(t.sched.total_slack, 0);
    assert!(t.sched.critical);
    assert_eq!(t.sched.late_start, Some(dt(2026, 10, 21, 8)), "late finish Monday 08:00, less 5 elapsed days");
}

/// Issue 21: the reporter checked this plan in Project. An elapsed task starts the moment its
/// predecessor finishes, outside working time too (Friday 17:00), and runs over the weekend; the
/// working task after it starts on Monday morning.
#[test]
fn an_elapsed_task_starts_when_its_predecessor_finishes_even_outside_working_time() {
    let mut p = project();
    let a = top(&mut p, "A", 5);
    let b = elapsed_task(&mut p, "B", "2ed");
    let c = top(&mut p, "C", 1);
    p.link(a.into(), b, LinkType::FS, Lag::None).unwrap();
    p.link(b.into(), c, LinkType::FS, Lag::None).unwrap();
    recalculate(&mut p);
    assert_eq!(finish(&p, a), dt(2026, 10, 23, 17));
    assert_eq!((start(&p, b), finish(&p, b)), (dt(2026, 10, 23, 17), dt(2026, 10, 25, 17)));
    assert_eq!((start(&p, c), finish(&p, c)), (dt(2026, 10, 26, 8), dt(2026, 10, 26, 17)));
}

#[test]
fn elapsed_tasks_take_links_constraints_and_actual_dates() {
    let mut p = project();
    let a = top(&mut p, "A", 3);
    // Finish-to-finish: finishes with A (Wednesday 17:00), starting 36 elapsed hours earlier.
    let ff = elapsed_task(&mut p, "FF", "36eh");
    p.link(a.into(), ff, LinkType::FF, Lag::None).unwrap();
    // Finish No Earlier Than Sunday noon: starts 2 elapsed days before.
    let fnet = elapsed_task(&mut p, "FNET", "2ed");
    p.task_mut(fnet).unwrap().constraint = Constraint { kind: ConstraintKind::Fnet, date: Some(dt(2026, 11, 1, 12)) };
    // Started on Tuesday 10:00: finishes 3 elapsed days later, whatever the calendar.
    let started = elapsed_task(&mut p, "Started", "3ed");
    p.task_mut(started).unwrap().actual_start = Some(dt(2026, 10, 20, 10));
    recalculate(&mut p);
    assert_eq!(finish(&p, ff), dt(2026, 10, 21, 17));
    assert_eq!(start(&p, ff), dt(2026, 10, 20, 5));
    assert_eq!((start(&p, fnet), finish(&p, fnet)), (dt(2026, 10, 30, 12), dt(2026, 11, 1, 12)));
    assert_eq!(finish(&p, started), dt(2026, 10, 23, 10));
}

#[test]
fn elapsed_progress_runs_around_the_clock() {
    let mut p = project();
    let t = elapsed_task(&mut p, "Soak", "4ed");
    recalculate(&mut p);
    // Through Wednesday 08:00, two of the four elapsed days have passed.
    p.update_progress_through(&[t], dt(2026, 10, 21, 8), plancraft_core::ops::ProgressUpdate::Scheduled).unwrap();
    recalculate(&mut p);
    let task = p.task(t).unwrap();
    assert_eq!(task.percent_complete, 50);
    assert_eq!(task.sched.stop, Some(dt(2026, 10, 21, 8)));
    assert_eq!(task.sched.resume, task.sched.stop);
    assert_eq!(finish(&p, t), dt(2026, 10, 23, 8));
    // Remaining duration is elapsed time too, and so is the text for it.
    assert_eq!(p.parse_task_minutes(t, "3ed").unwrap(), 3 * 1440);
    assert_eq!(p.parse_task_minutes(t, "3").unwrap(), 3 * 1440);
    assert!(p.parse_task_minutes(t, "3d").is_err());
    p.set_remaining_duration(t, 3 * 1440).unwrap();
    recalculate(&mut p);
    assert_eq!(finish(&p, t), dt(2026, 10, 24, 8));
}

#[test]
fn elapsed_work_spreads_over_every_hour() {
    let mut p = project();
    let t = elapsed_task(&mut p, "Watch", "1ed");
    let r = p.add_resource("Guard");
    p.assign(t, r, None, None).unwrap();
    recalculate(&mut p);
    // One elapsed day at 100% is 24 hours of work: 16 on Monday from 08:00, 8 on Tuesday.
    assert_eq!(p.task(t).unwrap().sched.work, 24 * 60);
    let a = p.assignments_for_task(t).next().unwrap().clone();
    let daily = plancraft_core::workload::assignment_daily_work(&p, &a);
    assert_eq!(daily.get(&d(2026, 10, 19)).copied().map(f64::round), Some(16.0 * 60.0));
    assert_eq!(daily.get(&d(2026, 10, 20)).copied().map(f64::round), Some(8.0 * 60.0));
}

#[test]
fn what_takes_an_elapsed_duration() {
    let mut p = project();
    let t = elapsed_task(&mut p, "Cure", "2ed");
    recalculate(&mut p);
    // Elapsed time runs without pauses.
    assert!(p.split_task(t, dt(2026, 10, 19, 12), dt(2026, 10, 22, 8)).is_err());
    // A summary's duration is working time.
    let s = top(&mut p, "Phase", 1);
    let sub = task(&mut p, "Sub", 1);
    p.indent(sub).unwrap();
    let typed = p.parse_task_duration("3ed").unwrap();
    assert!(p.set_typed_duration(s, typed).is_err());
    // Typing working time makes it working time again; a plain set keeps the kind.
    p.set_duration(t, 3 * 1440, false).unwrap();
    assert!(p.task(t).unwrap().elapsed);
    p.set_typed_duration(t, p.parse_task_duration("3d").unwrap()).unwrap();
    assert!(!p.task(t).unwrap().elapsed);
    assert_eq!(p.task(t).unwrap().duration, 3 * DAY);
    // A split task takes no elapsed duration.
    recalculate(&mut p);
    p.split_task(t, dt(2026, 10, 19, 12), dt(2026, 10, 22, 8)).unwrap();
    assert!(p.set_typed_duration(t, typed).is_err());
}

#[test]
fn elapsed_durations_survive_files() {
    let mut p = project();
    let t = elapsed_task(&mut p, "Cure", "33ed");
    p.task_mut(t).unwrap().estimated = true;
    recalculate(&mut p);
    // Microsoft Project XML.
    let (xml, _) = plancraft_core::mspdi::to_xml(&p);
    let (mut q, notes) = plancraft_core::mspdi::from_xml(&xml, "x").unwrap();
    assert!(notes.is_empty(), "{notes:?}");
    recalculate(&mut q);
    assert!(q.tasks[0].elapsed && q.tasks[0].estimated);
    assert_eq!((q.tasks[0].duration, q.tasks[0].sched.finish), (33 * 1440, p.tasks[0].sched.finish));
    // CSV.
    let csv = plancraft_core::tabular::to_csv(&p);
    assert!(csv.contains("33 edays?"), "{csv}");
    let sheets = plancraft_core::tabular::read_bytes(csv.as_bytes(), "plan.csv").unwrap();
    let grid = &sheets.sheets[0].1;
    let (r, _) = plancraft_core::tabular::build(grid, &plancraft_core::tabular::ImportOptions::guess(grid, "x")).unwrap();
    assert!(r.tasks[0].elapsed);
    assert_eq!(r.tasks[0].duration, 33 * 1440);
    // A PlanCraft file.
    let back: Project = serde_json::from_str(&serde_json::to_string(&p).unwrap()).unwrap();
    assert!(back.tasks[0].elapsed);
}

#[test]
fn an_elapsed_task_starts_when_its_predecessor_ends_even_after_hours() {
    let mut p = project();
    // Friday's pour ends at 17:00; the 3-day cure runs through the weekend to Monday 17:00.
    let prep = top(&mut p, "Prep", 4);
    let pour = top(&mut p, "Pour", 1);
    let cure = elapsed_task(&mut p, "Cure", "3ed");
    let strip = top(&mut p, "Strip forms", 1);
    p.link(prep.into(), pour, LinkType::FS, Lag::None).unwrap();
    p.link(pour.into(), cure, LinkType::FS, Lag::None).unwrap();
    p.link(cure.into(), strip, LinkType::FS, Lag::None).unwrap();
    recalculate(&mut p);
    assert_eq!((start(&p, cure), finish(&p, cure)), (dt(2026, 10, 23, 17), dt(2026, 10, 26, 17)));
    assert_eq!(start(&p, strip), dt(2026, 10, 27, 8));
    assert!(p.task(cure).unwrap().sched.critical);
}

/// Regression (bug 18): a finish-to-finish link into an automatic summary was pushed onto every
/// subtask, so X –FF→ S{A→B} delayed A and made S run past what X required. A summary's finish
/// comes from its subtasks, so such links are refused; start links into a summary still work, and
/// a manually scheduled summary (which keeps its own dates) takes any link.
#[test]
fn finish_links_into_an_automatic_summary_are_refused() {
    let mut p = project();
    let x = task(&mut p, "X", 7);
    let s = p.add_task("S", None);
    let a = task(&mut p, "A", 3);
    let b = task(&mut p, "B", 2);
    p.indent(a).unwrap();
    p.indent(b).unwrap();
    p.link(a.into(), b, LinkType::FS, Lag::None).unwrap();
    for kind in [LinkType::FF, LinkType::SF] {
        let err = p.link(x.into(), s, kind, Lag::None).unwrap_err();
        assert!(err.to_string().contains("subtasks"), "{err}");
        let err = p.set_predecessors(s, vec![Dependency { predecessor: x.into(), kind, lag: Lag::None }]).unwrap_err();
        assert!(err.to_string().contains("subtasks"), "{err}");
    }
    assert!(p.task(s).unwrap().predecessors.is_empty(), "a refused link leaves nothing behind");
    p.link(x.into(), s, LinkType::SS, Lag::None).unwrap();
    p.link(x.into(), a, LinkType::FF, Lag::None).unwrap();

    p.set_schedule_mode(&[s], ScheduleMode::Manual).unwrap();
    p.link(x.into(), s, LinkType::FF, Lag::None).unwrap();
}

/// Regression (bug 18): a finish-to-finish link that already ends at an automatic summary (from an
/// older file, or on a task that later became a summary) no longer moves its subtasks; the
/// recalculation warns that it is ignored.
#[test]
fn an_existing_finish_link_into_a_summary_is_ignored_with_a_warning() {
    let mut p = project();
    let x = task(&mut p, "X", 7);
    let s = task(&mut p, "S", 1);
    p.link(x.into(), s, LinkType::FF, Lag::None).unwrap();
    let a = task(&mut p, "A", 3);
    let b = task(&mut p, "B", 2);
    p.indent(a).unwrap();
    p.indent(b).unwrap();
    p.link(a.into(), b, LinkType::FS, Lag::None).unwrap();
    assert!(p.task(s).unwrap().predecessors.iter().any(|d| d.kind == LinkType::FF), "S kept its link when it became a summary");
    let report = recalculate(&mut p);
    assert_eq!(start(&p, a), dt(2026, 10, 19, 8), "A is not pushed by X's finish");
    assert_eq!(finish(&p, b), dt(2026, 10, 23, 17), "S spans its subtasks only");
    assert!(report.warnings.iter().any(|w| w.contains("FF") && w.contains("'S'") && w.contains("ignored")), "{:?}", report.warnings);
}

/// Names and outline levels, row by row.
fn outline(p: &Project) -> Vec<(String, u16)> {
    p.tasks.iter().map(|t| (t.name.clone(), t.outline_level)).collect()
}

fn rows(spec: &[(&str, u16)]) -> Vec<(String, u16)> {
    spec.iter().map(|(n, l)| (n.to_string(), *l)).collect()
}

/// A plan built from (name, level) rows; returns the ids in row order.
fn outlined(spec: &[(&str, u16)]) -> (Project, Vec<TaskId>) {
    let mut p = project();
    let ids: Vec<TaskId> = spec
        .iter()
        .map(|(name, level)| {
            let id = task(&mut p, name, 1);
            while p.task(id).unwrap().outline_level < *level {
                p.indent(id).unwrap();
            }
            while p.task(id).unwrap().outline_level > *level {
                p.outdent(id).unwrap();
            }
            id
        })
        .collect();
    assert_eq!(outline(&p), rows(spec));
    (p, ids)
}

#[test]
fn insert_summary_over_adjacent_tasks() {
    let (mut p, ids) = outlined(&[("A", 1), ("B", 1), ("C", 1)]);
    p.add_summary_over("S", &[ids[1], ids[0]]).unwrap();
    assert_eq!(outline(&p), rows(&[("S", 1), ("A", 2), ("B", 2), ("C", 1)]));
}

/// Regression: a summary over A and C (B between them, unselected) put A under it and made C a
/// subtask of B. Tasks that are not next to each other at one level are refused, unchanged.
#[test]
fn insert_summary_over_separated_tasks_is_refused() {
    for (spec, pick) in [
        (&[("A", 1), ("B", 1), ("C", 1)][..], &[0, 2][..]),
        // Different levels: a subtask and a task after its summary.
        (&[("P", 1), ("X", 2), ("Q", 1)][..], &[1, 2][..]),
        // Adjacent rows, but a subtask and the next summary's sibling at a lower level.
        (&[("P", 1), ("X", 2), ("Y", 2), ("Q", 1), ("R", 1)][..], &[2, 3][..]),
    ] {
        let (mut p, ids) = outlined(spec);
        let before = p.clone();
        let picked: Vec<TaskId> = pick.iter().map(|&i| ids[i]).collect();
        let e = p.add_summary_over("S", &picked).unwrap_err();
        assert!(e.to_string().contains("adjacent tasks at the same outline level"), "{e}");
        assert_eq!(p, before, "nothing changed for {spec:?}");
    }
}

/// Regression: selecting a summary and its own subtask failed ("already a subtask of the row
/// above"); the subtask goes along with its summary.
#[test]
fn insert_summary_over_a_summary_and_its_subtask() {
    let (mut p, ids) = outlined(&[("P", 1), ("X", 2), ("Q", 1), ("R", 1)]);
    p.add_summary_over("S", &[ids[0], ids[1]]).unwrap();
    assert_eq!(outline(&p), rows(&[("S", 1), ("P", 2), ("X", 3), ("Q", 1), ("R", 1)]));

    let (mut p, ids) = outlined(&[("P", 1), ("X", 2), ("Q", 1), ("R", 1)]);
    p.add_summary_over("S", &[ids[1], ids[0], ids[2], ids[1]]).unwrap();
    assert_eq!(outline(&p), rows(&[("S", 1), ("P", 2), ("X", 3), ("Q", 2), ("R", 1)]));
}

#[test]
fn insert_summary_inside_a_summary() {
    // Subtasks of a summary, one of them a summary itself, move under a new summary at their level.
    let (mut p, ids) = outlined(&[("P", 1), ("X", 2), ("X1", 3), ("Y", 2), ("Z", 2), ("Q", 1)]);
    let s = p.add_summary_over("S", &[ids[1], ids[3]]).unwrap();
    assert_eq!(outline(&p), rows(&[("P", 1), ("S", 2), ("X", 3), ("X1", 4), ("Y", 3), ("Z", 2), ("Q", 1)]));
    recalculate(&mut p);
    assert!(p.task(s).unwrap().is_summary());
    // The nested summary alone (with its subtask selected too).
    let (mut p, ids) = outlined(&[("P", 1), ("X", 2), ("X1", 3), ("Y", 2)]);
    p.add_summary_over("S", &[ids[2], ids[1]]).unwrap();
    assert_eq!(outline(&p), rows(&[("P", 1), ("S", 2), ("X", 3), ("X1", 4), ("Y", 2)]));
}

/// Regression: a manually scheduled task with an Actual Start showed its typed start instead
/// (its finish already put the actual finish first).
#[test]
fn a_manual_task_starts_at_its_actual_start() {
    let mut p = project();
    let a = task(&mut p, "A", 2);
    p.set_schedule_mode(&[a], ScheduleMode::Manual).unwrap();
    p.set_manual_date(a, dt(2026, 10, 21, 8), false).unwrap();
    p.task_mut(a).unwrap().actual_start = Some(dt(2026, 10, 20, 8));
    recalculate(&mut p);
    assert_eq!(start(&p, a), dt(2026, 10, 20, 8), "the actual start wins over the typed one");
    // Without an actual start, the typed start stands.
    p.task_mut(a).unwrap().actual_start = None;
    recalculate(&mut p);
    assert_eq!(start(&p, a), dt(2026, 10, 21, 8));
}

/// Regression (review): with a typed finish, a later actual start squeezed the task to nothing
/// (start and finish both on the actual start) and an earlier one stretched it. It keeps its
/// duration from the actual start instead.
#[test]
fn a_manual_task_with_a_typed_finish_keeps_its_duration_from_its_actual_start() {
    let mut p = project();
    let a = task(&mut p, "A", 3);
    p.set_schedule_mode(&[a], ScheduleMode::Manual).unwrap();
    p.set_manual_date(a, dt(2026, 10, 26, 8), false).unwrap();
    p.set_manual_date(a, dt(2026, 10, 28, 17), true).unwrap();
    recalculate(&mut p);
    assert_eq!((start(&p, a), finish(&p, a)), (dt(2026, 10, 26, 8), dt(2026, 10, 28, 17)));
    // Started on Thursday: three working days from there.
    p.task_mut(a).unwrap().actual_start = Some(dt(2026, 10, 29, 8));
    recalculate(&mut p);
    assert_eq!((start(&p, a), finish(&p, a)), (dt(2026, 10, 29, 8), dt(2026, 11, 2, 17)));
    // Started as typed: the typed finish stands.
    p.task_mut(a).unwrap().actual_start = Some(dt(2026, 10, 26, 8));
    recalculate(&mut p);
    assert_eq!(finish(&p, a), dt(2026, 10, 28, 17));
}

/// Regression (second review): typing a start and finish doesn't change a manual task's Duration
/// field, so taking the length from the duration still squeezed a typed Mon–Fri task to one day
/// when it started on Tuesday. The length of the typed dates is kept.
#[test]
fn a_late_start_keeps_the_length_of_the_typed_dates() {
    let mut p = project();
    let a = task(&mut p, "A", 1);
    p.set_schedule_mode(&[a], ScheduleMode::Manual).unwrap();
    p.set_manual_date(a, dt(2026, 10, 26, 8), false).unwrap();
    p.set_manual_date(a, dt(2026, 10, 30, 17), true).unwrap();
    recalculate(&mut p);
    assert_eq!((start(&p, a), finish(&p, a)), (dt(2026, 10, 26, 8), dt(2026, 10, 30, 17)));
    p.task_mut(a).unwrap().actual_start = Some(dt(2026, 10, 27, 8));
    recalculate(&mut p);
    assert_eq!((start(&p, a), finish(&p, a)), (dt(2026, 10, 27, 8), dt(2026, 11, 2, 17)), "five working days from Tuesday");
}
