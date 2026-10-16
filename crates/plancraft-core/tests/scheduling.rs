#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]
use chrono::{NaiveDate, NaiveDateTime};
use plancraft_core::leveling::{LevelingOptions, level_resources};
use plancraft_core::prelude::*;

const DAY: Minutes = 480;

fn d(y: i32, m: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, day).unwrap()
}
fn dt(y: i32, m: u32, day: u32, h: u32) -> NaiveDateTime {
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

fn start(p: &Project, id: TaskId) -> NaiveDateTime {
    p.task(id).unwrap().sched.start.unwrap()
}
fn finish(p: &Project, id: TaskId) -> NaiveDateTime {
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
