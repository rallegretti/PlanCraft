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
