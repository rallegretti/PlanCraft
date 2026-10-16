#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]
//! Hostile input: parsers, files and edit sequences built from junk must never panic.
//!
//! The parsers get property tests (round trips where a value can be written back, "no panic and
//! in range" everywhere else). Random edit sequences run through `Document::try_edit`, whose
//! guard turns a panic into `PlanError::Internal`: any `Internal` error here is a bug.

use std::path::Path;

use chrono::NaiveDate;
use plancraft_core::calendar::TimeRange;
use plancraft_core::duration::{DurationUnit, Lag, TimeSettings, best_unit, format_duration, parse_duration};
use plancraft_core::io::{from_json_with_notes, to_json};
use plancraft_core::limits::{self, MAX_MINUTES};
use plancraft_core::prelude::*;
use plancraft_core::sample::demo_project;
use plancraft_core::workspace::{ChangeSource, Document};
use proptest::prelude::*;

fn start() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 10, 19).unwrap()
}

fn units() -> impl Strategy<Value = DurationUnit> {
    prop::sample::select(vec![DurationUnit::Minutes, DurationUnit::Hours, DurationUnit::Days, DurationUnit::Weeks, DurationUnit::Months])
}

/// Text that looks like what people type into duration, lag and predecessor cells, plus noise.
fn cell_text() -> impl Strategy<Value = String> {
    prop_oneof![
        "\\PC{0,24}",
        "[-+]?[0-9]{0,22}(\\.[0-9]{0,4})?[ ]?(m|h|d|w|mo|ed|e|%|days|wks|mins|edays|é|ß)?[?]?",
        "[0-9]{1,4}(FS|SS|FF|SF|fs|xx|é)?[-+]?[0-9]{0,6}(d|h|%|ed|é)?(,[0-9]{1,3})*",
        "[a-zA-Z ]{0,8}\\\\[0-9]{0,4}(FS|SS)?",
    ]
}

proptest! {
    #![proptest_config(ProptestConfig { cases: cases(512), ..ProptestConfig::default() })]

    #[test]
    fn durations_parse_in_range(text in cell_text(), unit in units()) {
        let s = TimeSettings::default();
        if let Some((m, _)) = parse_duration(&text, unit, &s) {
            prop_assert!(m.abs() <= MAX_MINUTES, "{text:?} parsed to {m}");
        }
    }

    #[test]
    fn durations_round_trip(m in 0..=MAX_MINUTES, unit in units(), estimated: bool) {
        let s = TimeSettings::default();
        let text = format_duration(m, best_unit(m, &s), &s, estimated);
        prop_assert_eq!(parse_duration(&text, unit, &s), Some((m, estimated)), "{}", text);
    }

    #[test]
    fn lags_parse_in_range(text in cell_text()) {
        match Lag::parse(&text, &TimeSettings::default()) {
            Some(Lag::Working(m) | Lag::Elapsed(m)) => prop_assert!(m.abs() <= MAX_MINUTES),
            Some(Lag::Percent(p)) => prop_assert!(p.abs() <= limits::MAX_LAG_PERCENT),
            Some(Lag::None) | None => {}
        }
    }

    #[test]
    fn working_lags_round_trip(m in -MAX_MINUTES..=MAX_MINUTES) {
        let s = TimeSettings::default();
        let lag = if m == 0 { Lag::None } else { Lag::Working(m) };
        prop_assert_eq!(Lag::parse(&lag.format(&s), &s), Some(lag));
    }

    #[test]
    fn predecessor_text_never_panics(text in cell_text()) {
        let p = demo_project(start());
        if let Ok(deps) = p.parse_predecessors(&text) {
            for d in deps {
                if let TaskRef::Local(id) = d.predecessor {
                    prop_assert!(p.task(id).is_some());
                }
            }
        }
    }

    #[test]
    fn predecessors_round_trip(rows in prop::collection::vec((1usize..=19, 0usize..4, -40i64..40), 0..6)) {
        let mut p = demo_project(start());
        let s = p.settings.time.clone();
        let target = p.add_task("Target", None);
        let deps: Vec<Dependency> = rows
            .iter()
            .map(|&(row, kind, lag_days)| Dependency {
                predecessor: TaskRef::Local(p.tasks[row - 1].id),
                kind: LinkType::ALL[kind],
                lag: if lag_days == 0 { Lag::None } else { Lag::Working(lag_days * s.minutes_per_day) },
            })
            .collect();
        // Only links the project accepts (no cycles, no links to summaries containing the task).
        if p.set_predecessors(target, deps).is_ok() {
            let text = p.format_predecessors(target);
            let parsed = p.parse_predecessors(&text).unwrap();
            prop_assert_eq!(&parsed, &p.task(target).unwrap().predecessors, "{}", text);
        }
    }

    #[test]
    fn time_ranges_and_dates_never_panic(text in "\\PC{0,30}") {
        if let Some(r) = TimeRange::parse(&text) {
            prop_assert!(r.start < r.end && r.end <= 1440);
        }
        let p = Project::new("P", start());
        let _ = p.parse_datetime(&text, false);
        let _ = p.parse_datetime(&text, true);
    }

    /// Valid documents with junk spliced into one value are refused or clamped, never a panic.
    #[test]
    fn mutated_documents_load_or_fail_cleanly(path in 0usize..400, junk in prop_oneof![
        Just(serde_json::json!(null)),
        Just(serde_json::json!(-1)),
        Just(serde_json::json!(i64::MAX)),
        Just(serde_json::json!(u64::MAX)),
        Just(serde_json::json!(1e308)),
        Just(serde_json::json!("+262143-12-31T23:59:59")),
        Just(serde_json::json!("-262143-01-01")),
        Just(serde_json::json!("24:00-23:00")),
        Just(serde_json::json!([])),
        Just(serde_json::json!({})),
        "\\PC{0,12}".prop_map(serde_json::Value::from),
    ]) {
        let p = demo_project(start());
        let mut json: serde_json::Value = serde_json::from_str(&to_json(&p).unwrap()).unwrap();
        let mut leaves = Vec::new();
        collect_leaves(&mut json, &mut leaves);
        let n = leaves.len();
        *leaves.swap_remove(path % n) = junk;
        if let Ok((mut q, _)) = from_json_with_notes(&json.to_string(), Path::new("junk.plancraft")) {
            let report = recalculate(&mut q);
            prop_assert!(!report.warnings.iter().any(|w| w.contains("internal error")), "{:?}", report.warnings);
        }
    }

}

proptest! {
    // Fewer cases: each runs up to 40 edits, leveling included.
    #![proptest_config(ProptestConfig { cases: cases(48), ..ProptestConfig::default() })]

    /// Random edit sequences never panic, and every accepted edit leaves a valid project.
    #[test]
    fn random_edits_never_panic(ops in prop::collection::vec(edit_op(), 1..40)) {
        let mut doc = Document::new(1, demo_project(start()), None);
        for op in ops {
            let r = doc.try_edit("junk", ChangeSource::Agent, |p| apply(p, &op));
            prop_assert!(!matches!(r, Err(PlanError::Internal(_))), "{op:?} panicked: {r:?}");
            prop_assert!(limits::check_counts(&doc.project).is_ok());
            let mut copy = doc.project.clone();
            prop_assert!(limits::sanitize(&mut copy).is_empty(), "{op:?} left values out of range");
        }
    }
}

fn collect_leaves<'a>(v: &'a mut serde_json::Value, out: &mut Vec<&'a mut serde_json::Value>) {
    match v {
        serde_json::Value::Array(items) => items.iter_mut().for_each(|i| collect_leaves(i, out)),
        serde_json::Value::Object(map) => map.values_mut().for_each(|i| collect_leaves(i, out)),
        other => out.push(other),
    }
}

#[derive(Clone, Debug)]
enum Op {
    Add(usize),
    Milestone(usize),
    Indent(usize),
    Outdent(usize),
    Move(usize, usize),
    Shift(usize, bool),
    Delete(usize),
    Link(usize, usize, usize, i64),
    Unlink(usize),
    Duration(usize, i64),
    Percent(usize, u8),
    Assign(usize, usize, f64, Option<i64>),
    Unassign(usize),
    Units(usize, f64),
    Work(usize, i64),
    Log(usize, usize, i64, i64),
    Names(usize, String),
    Predecessors(usize, String),
    Summary(usize, usize),
    Level,
    Baseline,
}

fn edit_op() -> impl Strategy<Value = Op> {
    let i = || 0usize..40;
    let big = || prop_oneof![any::<i64>(), -10_000i64..10_000, Just(MAX_MINUTES), Just(MAX_MINUTES + 1), Just(i64::MIN)];
    let units = || prop_oneof![any::<f64>(), 0.0f64..4.0, Just(f64::NAN), Just(f64::INFINITY), Just(-1.0)];
    prop_oneof![
        i().prop_map(Op::Add),
        i().prop_map(Op::Milestone),
        i().prop_map(Op::Indent),
        i().prop_map(Op::Outdent),
        (i(), any::<usize>()).prop_map(|(a, b)| Op::Move(a, b)),
        (i(), any::<bool>()).prop_map(|(a, b)| Op::Shift(a, b)),
        i().prop_map(Op::Delete),
        (i(), i(), 0usize..4, big()).prop_map(|(a, b, k, l)| Op::Link(a, b, k, l)),
        i().prop_map(Op::Unlink),
        (i(), big()).prop_map(|(a, d)| Op::Duration(a, d)),
        (i(), any::<u8>()).prop_map(|(a, p)| Op::Percent(a, p)),
        (i(), 0usize..12, units(), prop::option::of(big())).prop_map(|(a, r, u, w)| Op::Assign(a, r, u, w)),
        i().prop_map(Op::Unassign),
        (i(), units()).prop_map(|(a, u)| Op::Units(a, u)),
        (i(), big()).prop_map(|(a, w)| Op::Work(a, w)),
        (i(), 0usize..12, big(), -100_000i64..100_000).prop_map(|(a, r, m, d)| Op::Log(a, r, m, d)),
        (i(), cell_text()).prop_map(|(a, t)| Op::Names(a, t)),
        (i(), cell_text()).prop_map(|(a, t)| Op::Predecessors(a, t)),
        (i(), i()).prop_map(|(a, b)| Op::Summary(a, b)),
        Just(Op::Level),
        Just(Op::Baseline),
    ]
}

fn task_at(p: &Project, i: usize) -> TaskId {
    p.tasks.get(i % p.tasks.len().max(1)).map_or(TaskId(9999), |t| t.id)
}

fn apply(p: &mut Project, op: &Op) -> PlanResult<()> {
    match op.clone() {
        Op::Add(i) => {
            p.add_task("New", Some(i));
        }
        Op::Milestone(i) => {
            p.add_milestone("M", Some(i));
        }
        Op::Indent(i) => p.indent(task_at(p, i))?,
        Op::Outdent(i) => p.outdent(task_at(p, i))?,
        Op::Move(i, to) => p.move_task(task_at(p, i), to)?,
        Op::Shift(i, up) => p.shift_task(task_at(p, i), up)?,
        Op::Delete(i) => {
            p.delete_tasks(&[task_at(p, i)])?;
        }
        Op::Link(a, b, k, lag) => p.link(task_at(p, a).into(), task_at(p, b), LinkType::ALL[k], Lag::Working(lag))?,
        Op::Unlink(i) => p.unlink_tasks(&[task_at(p, i)]),
        Op::Duration(i, d) => p.set_duration(task_at(p, i), d, false)?,
        Op::Percent(i, pct) => p.set_percent_complete(task_at(p, i), pct)?,
        Op::Assign(i, r, u, w) => {
            let rid = p.resources.get(r % p.resources.len().max(1)).map_or(ResourceId(999), |r| r.id);
            p.assign(task_at(p, i), rid, Some(u), w)?;
        }
        Op::Unassign(i) => {
            if let Some(a) = p.assignments.get(i % p.assignments.len().max(1)).map(|a| a.id) {
                p.unassign(a)?;
            }
        }
        Op::Units(i, u) => {
            if let Some(a) = p.assignments.get(i % p.assignments.len().max(1)).map(|a| a.id) {
                p.set_assignment_units(a, u)?;
            }
        }
        Op::Work(i, w) => {
            if let Some(a) = p.assignments.get(i % p.assignments.len().max(1)).map(|a| a.id) {
                p.set_assignment_work(a, w)?;
            }
        }
        Op::Log(i, r, m, days) => {
            let rid = p.resources.get(r % p.resources.len().max(1)).map_or(ResourceId(999), |r| r.id);
            let date = start().checked_add_signed(chrono::Duration::days(days)).unwrap_or(start());
            p.log_time(task_at(p, i), rid, date, m, "junk")?;
        }
        Op::Names(i, text) => p.apply_resource_names(task_at(p, i), &text)?,
        Op::Predecessors(i, text) => {
            let deps = p.parse_predecessors(&text)?;
            p.set_predecessors(task_at(p, i), deps)?;
        }
        Op::Summary(a, b) => {
            p.add_summary_over("Summary", &[task_at(p, a), task_at(p, b)])?;
        }
        Op::Level => {
            plancraft_core::leveling::level_resources(p, &plancraft_core::leveling::LevelingOptions::default());
        }
        Op::Baseline => p.set_baseline(&[]),
    }
    Ok(())
}

#[test]
fn deep_and_huge_documents_are_refused() {
    let deep = format!("{{\"format\":\"plancraft\",\"tasks\":{}{}}}", "[".repeat(50_000), "]".repeat(50_000));
    assert!(from_json_with_notes(&deep, Path::new("deep.plancraft")).is_err());

    let mut p = Project::new("Big", start());
    for i in 0..=limits::MAX_TASKS {
        p.tasks.push(Task::new(TaskId(i as u32 + 1), "t", 480));
    }
    let text = to_json(&p).unwrap();
    let err = from_json_with_notes(&text, Path::new("big.plancraft")).unwrap_err();
    assert!(err.to_string().contains("too many tasks"), "{err}");
}

#[test]
fn edits_beyond_the_task_limit_are_rolled_back() {
    let mut p = Project::new("Big", start());
    for i in 0..limits::MAX_TASKS {
        p.tasks.push(Task::new(TaskId(i as u32 + 1), "t", 480));
    }
    p.ids.task = limits::MAX_TASKS as u32;
    let mut doc = Document::new(1, p, None);
    let r = doc.edit("One too many", |p| p.add_task("t", None));
    assert!(matches!(r, Err(PlanError::Limit(m)) if m.contains("too many tasks")));
    assert_eq!(doc.project.tasks.len(), limits::MAX_TASKS);
}

/// The number of random cases: `default`, or `PROPTEST_CASES` when set (to search harder locally).
fn cases(default: u32) -> u32 {
    std::env::var("PROPTEST_CASES").ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}
