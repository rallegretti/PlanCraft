#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]
//! Speed gate: the scheduler must stay fast enough to recalculate on every keystroke.
//!
//! A 200-task project (summaries, a web of links of every type, lags, constraints, assignments
//! with resource calendars) must recalculate in under 10 ms. Debug builds are slower, so they get
//! a looser bound; CI runs this test in release mode too (`cargo test --release --test speed`).

use std::time::{Duration, Instant};

use chrono::NaiveDate;
use plancraft_core::prelude::*;

fn project(tasks: usize) -> Project {
    let mut p = Project::new("Speed", NaiveDate::from_ymd_opt(2026, 10, 19).unwrap());
    let people: Vec<ResourceId> = (0..12).map(|i| p.add_resource(format!("Person {i}"))).collect();
    let mut ids = Vec::new();
    for i in 0..tasks {
        if i % 20 == 0 {
            let s = p.add_task(format!("Phase {}", i / 20), None);
            p.task_mut(s).unwrap().outline_level = 1;
            continue;
        }
        let id = p.add_task(format!("Task {i}"), None);
        p.task_mut(id).unwrap().outline_level = 2;
        p.set_duration(id, ((i % 7) as i64 + 1) * 240, false).unwrap();
        p.assign(id, people[i % people.len()], Some(if i % 3 == 0 { 0.5 } else { 1.0 }), None).unwrap();
        ids.push(id);
    }
    // Links: a chain plus cross links of every type with lags and leads.
    for (k, w) in ids.windows(2).enumerate() {
        let kind = LinkType::ALL[k % 4];
        let lag = match k % 5 {
            0 => Lag::Working(120),
            1 => Lag::Working(-60),
            2 => Lag::Percent(25),
            _ => Lag::None,
        };
        let _ = p.link(w[0].into(), w[1], kind, lag);
        if k >= 7 && k % 3 == 0 {
            let _ = p.link(ids[k - 7].into(), w[1], LinkType::FS, Lag::None);
        }
    }
    p
}

#[test]
fn recalculating_200_tasks_takes_under_10ms() {
    let mut p = project(200);
    assert_eq!(p.tasks.len(), 200);
    recalculate(&mut p);
    assert!(p.sched.warnings.iter().all(|w| !w.contains("Circular")), "{:?}", p.sched.warnings);
    let best = (0..7)
        .map(|_| {
            let t = Instant::now();
            recalculate(&mut p);
            t.elapsed()
        })
        .min()
        .unwrap();
    let limit = if cfg!(debug_assertions) { Duration::from_millis(30) } else { Duration::from_millis(10) };
    eprintln!("recalculate(200 tasks): best of 7 = {best:?} (limit {limit:?})");
    assert!(best < limit, "recalculating 200 tasks took {best:?}; the limit is {limit:?}");
}

/// Best of `runs` timings of `f`.
fn best_of(runs: usize, mut f: impl FnMut()) -> Duration {
    (0..runs)
        .map(|_| {
            let t = Instant::now();
            f();
            t.elapsed()
        })
        .min()
        .unwrap()
}

fn flat_tasks(p: &mut Project, n: usize) -> Vec<TaskId> {
    (0..n)
        .map(|i| {
            let id = p.add_task(format!("Task {i}"), None);
            p.set_duration(id, 480, false).unwrap();
            id
        })
        .collect()
}

/// Regression: the cycle check behind every new link rescanned all tasks for every task it
/// visited, so linking 2,000 tasks into a chain (in reverse order) took about 28 s.
#[test]
fn linking_a_2000_task_chain_is_fast() {
    let mut p = Project::new("Chain", NaiveDate::from_ymd_opt(2026, 10, 19).unwrap());
    let ids = flat_tasks(&mut p, 2000);
    let t = Instant::now();
    for w in ids.windows(2).rev() {
        p.link(w[0].into(), w[1], LinkType::FS, Lag::None).unwrap();
    }
    let took = t.elapsed();
    // A cycle is still refused.
    assert!(matches!(p.link(ids[1999].into(), ids[0], LinkType::FS, Lag::None), Err(PlanError::CircularDependency)));
    let limit = if cfg!(debug_assertions) { Duration::from_secs(20) } else { Duration::from_secs(1) };
    eprintln!("linking 2,000 tasks: {took:?} (limit {limit:?})");
    assert!(took < limit, "linking 2,000 tasks took {took:?}; the limit is {limit:?}");
}

/// Regression: working time between two dates was counted one day at a time, so one date far in
/// the future (a typo such as 2062, or the 2299 limit) made every recalculation take seconds.
#[test]
fn a_far_future_date_keeps_recalculation_fast() {
    let mut p = Project::new("Far", NaiveDate::from_ymd_opt(2026, 10, 19).unwrap());
    let ids = flat_tasks(&mut p, 2000);
    let far = NaiveDate::from_ymd_opt(2299, 6, 1).unwrap().and_hms_opt(8, 0, 0).unwrap();
    p.task_mut(ids[0]).unwrap().constraint = Constraint { kind: ConstraintKind::Mso, date: Some(far) };
    recalculate(&mut p);
    let best = best_of(3, || {
        recalculate(&mut p);
    });
    let limit = if cfg!(debug_assertions) { Duration::from_secs(3) } else { Duration::from_millis(150) };
    eprintln!("recalculate(2,000 tasks, one in 2299): {best:?} (limit {limit:?})");
    assert!(best < limit, "took {best:?}; the limit is {limit:?}");
}

/// Regression: leveling re-sorted, rebuilt every load and recalculated the whole project for each
/// delayed task (1,000 tasks took about 10 s).
#[test]
fn leveling_1000_tasks_is_fast() {
    use plancraft_core::leveling::{LevelingOptions, level_resources};
    let mut p = Project::new("Level", NaiveDate::from_ymd_opt(2026, 10, 19).unwrap());
    let people: Vec<ResourceId> = (0..5).map(|i| p.add_resource(format!("Person {i}"))).collect();
    let ids = flat_tasks(&mut p, 1000);
    for (i, id) in ids.iter().enumerate() {
        p.assign(*id, people[i % people.len()], Some(1.0), None).unwrap();
    }
    recalculate(&mut p);
    let t = Instant::now();
    level_resources(&mut p, &LevelingOptions::default());
    let took = t.elapsed();
    assert!(plancraft_core::workload::overallocations(&p).is_empty(), "leveling still resolves every overallocation");
    let limit = if cfg!(debug_assertions) { Duration::from_secs(30) } else { Duration::from_secs(2) };
    eprintln!("leveling 1,000 tasks: {took:?} (limit {limit:?})");
    assert!(took < limit, "leveling 1,000 tasks took {took:?}; the limit is {limit:?}");
}

/// Regression: printing looked tasks up by scanning the plan, and every page walked every link,
/// so a 10,000-task plan took tens of seconds to lay out (and froze the Print preview).
#[test]
fn printing_10000_linked_tasks_is_fast() {
    use plancraft_core::print::{self, PageSetup};
    let mut p = Project::new("Big print", NaiveDate::from_ymd_opt(2026, 10, 19).unwrap());
    let ids = flat_tasks(&mut p, 10_000);
    for w in ids.windows(2).step_by(2) {
        p.link(w[0].into(), w[1], LinkType::FS, Lag::None).unwrap();
    }
    recalculate(&mut p);
    let setup = PageSetup::default();
    let sheet = print::task_sheet(&p, true);
    let t = Instant::now();
    let doc = print::layout(&print::PrintJob {
        project: &p,
        setup: &setup,
        sheet: &sheet,
        chart: Some(print::ChartOptions::default()),
        from: None,
        to: None,
        printed: p.start,
        palette: print::layout::Palette::default(),
    });
    let took = t.elapsed();
    assert!(doc.pages.len() > 100);
    let limit = if cfg!(debug_assertions) { Duration::from_secs(30) } else { Duration::from_secs(2) };
    eprintln!("laying out 10,000 tasks on {} pages: {took:?} (limit {limit:?})", doc.pages.len());
    assert!(took < limit, "laying out 10,000 tasks took {took:?}; the limit is {limit:?}");
}

/// The Network Diagram lays out on every edit, so a 10,000-task plan (phases, chains and cross
/// links) must lay out quickly.
#[test]
fn laying_out_a_10000_task_network_diagram_is_fast() {
    use plancraft_core::network::{NetworkOptions, boxes_for, layout};
    let mut p = Project::new("Big network", NaiveDate::from_ymd_opt(2026, 10, 19).unwrap());
    let mut leaves = Vec::new();
    for i in 0..10_000 {
        let id = p.add_task(format!("Task {i}"), None);
        if i % 25 == 0 {
            continue; // a phase
        }
        p.task_mut(id).unwrap().outline_level = 2;
        p.set_duration(id, 480, false).unwrap();
        leaves.push(id);
    }
    for (k, w) in leaves.windows(2).enumerate() {
        if k % 7 != 0 {
            p.link(w[0].into(), w[1], LinkType::FS, Lag::None).unwrap();
        }
        if k >= 30 && k % 11 == 0 {
            p.link(leaves[k - 30].into(), w[1], LinkType::SS, Lag::None).unwrap();
        }
    }
    recalculate(&mut p);
    let shown = vec![true; p.tasks.len()];
    let best = best_of(3, || {
        for opts in [NetworkOptions::default(), NetworkOptions { critical_first: true, ..Default::default() }] {
            let d = layout(&p, &boxes_for(&p, &shown, &Default::default()), opts);
            assert_eq!(d.nodes.len(), 10_000);
        }
    });
    let limit = if cfg!(debug_assertions) { Duration::from_secs(10) } else { Duration::from_millis(500) };
    eprintln!("laying out 10,000 tasks twice: {best:?} (limit {limit:?})");
    assert!(best < limit, "took {best:?}; the limit is {limit:?}");
}
