#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]
//! Hostile input: parsers, files and edit sequences built from junk must never panic.
//!
//! The parsers get property tests (round trips where a value can be written back, "no panic and
//! in range" everywhere else). Random edit sequences run through `Document::try_edit`, whose
//! guard turns a panic into `PlanError::Internal`: any `Internal` error here is a bug.

use std::path::Path;

use chrono::NaiveDate;
use plancraft_core::calendar::{TimeRange, shift};
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
        let mut p = demo_project(start());
        // Fields the demo leaves empty, so the fuzzer mutates them too.
        p.status_date = Some(start());
        let t = p.tasks.iter_mut().find(|t| (1..100).contains(&t.percent_complete)).unwrap();
        t.splits.push(Split { work: 60, resume: shift(start().and_hms_opt(8, 0, 0).unwrap(), 20 * 480) });
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
    fn random_edits_never_panic(
        ops in prop::collection::vec(edit_op(), 1..40),
        range in prop::option::of((-200_000i64..200_000, 0i64..200_000)),
        per_day in prop_oneof![Just(20.0f32), 0.0f32..3000.0, Just(f32::NAN)],
    ) {
        let mut doc = Document::new(1, demo_project(start()), None);
        for op in ops {
            let r = doc.try_edit("junk", ChangeSource::Agent, |p| apply(p, &op));
            prop_assert!(!matches!(r, Err(PlanError::Internal(_))), "{op:?} panicked: {r:?}");
            prop_assert!(limits::check_counts(&doc.project).is_ok());
            let mut copy = doc.project.clone();
            prop_assert!(limits::sanitize(&mut copy).is_empty(), "{op:?} left values out of range");
        }
        // Whatever the edits left, the plan prints (table and chart) to a valid PDF, also over
        // any date range and at any zoom.
        let p = &doc.project;
        let day = |n: i64| plancraft_core::calendar::shift(start().and_hms_opt(0, 0, 0).unwrap(), n * 1440).date();
        let (from, to) = range.map_or((None, None), |(a, len)| (Some(day(a)), Some(day(a.saturating_add(len)))));
        for chart in [true, false] {
            let sheet = plancraft_core::print::task_sheet(p, chart);
            let out = plancraft_core::print::layout(&plancraft_core::print::PrintJob {
                project: p,
                setup: &p.page_setup,
                sheet: &sheet,
                chart: chart.then(|| plancraft_core::print::ChartOptions { critical: true, baseline: true, slack: true, per_day, ..Default::default() }),
                from,
                to,
                printed: start().and_hms_opt(9, 0, 0).unwrap(),
                palette: plancraft_core::print::layout::Palette::default(),
            });
            prop_assert!(!out.pages.is_empty() && out.pages.len() <= plancraft_core::print::layout::MAX_PAGES);
            prop_assert!(plancraft_core::print::pdf::write(&out).starts_with(b"%PDF"));
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
    AddField(usize, String, Vec<String>),
    SetCustom(usize, usize, String),
    DeleteField(usize),
    Columns(String, Vec<String>),
    PageSetup(usize, bool, f32, u16, u8, u8, usize, String),
    StatusDate(Option<i64>),
    UpdateThrough(Option<usize>, i64, bool),
    MarkOnTrack(usize),
    Reschedule(Option<usize>, i64),
    Resume(usize, Option<i64>),
    ActualDuration(usize, i64),
    RemainingDuration(usize, i64),
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
        (0usize..6, cell_text(), prop::collection::vec(cell_text(), 0..4)).prop_map(|(k, n, c)| Op::AddField(k, n, c)),
        (i(), i(), cell_text()).prop_map(|(a, f, t)| Op::SetCustom(a, f, t)),
        i().prop_map(Op::DeleteField),
        (cell_text(), prop::collection::vec(prop_oneof![cell_text(), (0u32..8).prop_map(|n| format!("custom:{n}"))], 0..6))
            .prop_map(|(n, c)| Op::Columns(n, c)),
        (
            0usize..5,
            any::<bool>(),
            prop_oneof![any::<f32>(), Just(f32::NAN), 0.0f32..60.0],
            any::<u16>(),
            any::<u8>(),
            any::<u8>(),
            0usize..3,
            cell_text()
        )
            .prop_map(|(a, b, c, d, e, f, g, h)| Op::PageSetup(a, b, c, d, e, f, g, h)),
        prop::option::of(big()).prop_map(Op::StatusDate),
        (prop::option::of(i()), big(), any::<bool>()).prop_map(|(a, m, z)| Op::UpdateThrough(a, m, z)),
        i().prop_map(Op::MarkOnTrack),
        (prop::option::of(i()), big()).prop_map(|(a, m)| Op::Reschedule(a, m)),
        (i(), prop::option::of(big())).prop_map(|(a, m)| Op::Resume(a, m)),
        (i(), big()).prop_map(|(a, m)| Op::ActualDuration(a, m)),
        (i(), big()).prop_map(|(a, m)| Op::RemainingDuration(a, m)),
    ]
}

fn field_at(p: &Project, i: usize) -> CustomFieldId {
    p.custom_fields.get(i % p.custom_fields.len().max(1)).map_or(CustomFieldId(9999), |f| f.id)
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
        Op::AddField(k, name, choices) => {
            let kind = CustomFieldKind::ALL[k % CustomFieldKind::ALL.len()];
            p.add_custom_field(&name, kind, &choices)?;
        }
        Op::SetCustom(i, f, text) => {
            let field = field_at(p, f);
            let v = p.parse_custom_value(field, &text)?;
            p.set_custom_value(task_at(p, i), field, v)?;
        }
        Op::DeleteField(f) => p.delete_custom_field(field_at(p, f))?,
        Op::Columns(name, cols) => p.set_table_columns(&name, &cols)?,
        Op::PageSetup(paper, portrait, margin, scale, wide, repeat, legend, text) => {
            use plancraft_core::print::{LegendPlacement, Orientation, PageSetup, Paper, TimescaleFit};
            p.set_page_setup(PageSetup {
                paper: Paper::ALL[paper % Paper::ALL.len()],
                orientation: if portrait { Orientation::Portrait } else { Orientation::Landscape },
                margin_mm: margin,
                scale_percent: scale,
                timescale: if wide % 4 == 0 { TimescaleFit::AsShown } else { TimescaleFit::PagesWide(wide) },
                repeat_columns: repeat,
                legend: LegendPlacement::ALL[legend % 3],
                header: [text.clone(), "[Page] of [Pages]".into(), text],
                footer: Default::default(),
                print_blank_pages: wide % 2 == 0,
                print_all_columns: portrait,
            });
        }
        Op::StatusDate(m) => p.set_status_date(m.map(|m| shift(p.start, m).date())),
        Op::UpdateThrough(i, m, zero) => {
            let ids: Vec<TaskId> = i.map(|i| task_at(p, i)).into_iter().collect();
            let how = if zero { ProgressUpdate::ZeroOrHundred } else { ProgressUpdate::Scheduled };
            p.update_progress_through(&ids, shift(p.start, m), how)?;
        }
        Op::MarkOnTrack(i) => {
            p.mark_on_track(&[task_at(p, i)])?;
        }
        Op::Reschedule(i, m) => {
            let ids: Vec<TaskId> = i.map(|i| task_at(p, i)).into_iter().collect();
            p.reschedule_uncompleted_work(&ids, shift(p.start, m))?;
        }
        Op::Resume(i, m) => p.set_resume(task_at(p, i), m.map(|m| shift(p.start, m)))?,
        Op::ActualDuration(i, m) => p.set_actual_duration(task_at(p, i), m)?,
        Op::RemainingDuration(i, m) => p.set_remaining_duration(task_at(p, i), m)?,
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

#[test]
fn hostile_page_setup_in_a_file_is_clamped_on_open() {
    let p = demo_project(start());
    let mut v = serde_json::to_value(&p).unwrap();
    v["page_setup"] = serde_json::json!({
        "paper": "A3",
        "margin_mm": 1e30,
        "scale_percent": 0,
        "timescale": { "PagesWide": 255 },
        "repeat_columns": 255,
        "header": ["x".repeat(10_000), "a\nb", ""],
    });
    let (loaded, notes) = plancraft_core::io::from_json_with_notes(&v.to_string(), std::path::Path::new("x.plancraft")).unwrap();
    assert!(notes.iter().any(|n| n.contains("page setup")), "{notes:?}");
    let s = &loaded.page_setup;
    assert_eq!(s.paper, plancraft_core::print::Paper::A3);
    assert_eq!(s.margin_mm, plancraft_core::print::MAX_MARGIN_MM);
    assert_eq!(s.scale_percent, plancraft_core::print::MIN_SCALE);
    assert_eq!(s.timescale, plancraft_core::print::TimescaleFit::PagesWide(plancraft_core::print::MAX_PAGES_WIDE));
    assert!(s.header[0].chars().count() <= plancraft_core::print::MAX_HEADER_CHARS);
    // A plan with the default setup does not store one.
    assert!(!plancraft_core::io::to_json(&p).unwrap().contains("page_setup"));
}

/// Hostile custom fields in a file are clamped when it is opened: values of the wrong kind or
/// for a missing field are dropped, pick-lists on non-text fields cleared, oversized lists cut,
/// repeated names renamed, and table columns of missing fields removed.
#[test]
fn hostile_custom_fields_in_a_file_are_clamped_on_open() {
    use plancraft_core::custom::{CustomFieldKind, MAX_CHOICES};
    let mut p = Project::new("Fields", start());
    let t = p.add_task("A", None);
    p.add_custom_field("Status", CustomFieldKind::Text, &[]).unwrap();
    let mut v: serde_json::Value = serde_json::from_str(&to_json(&p).unwrap()).unwrap();
    v["custom_fields"] = serde_json::json!([
        { "id": 1, "name": "Status", "kind": "text" },
        { "id": 2, "name": "status", "kind": "number", "choices": ["a", "b"] },
        { "id": 3, "name": "Field 2", "kind": "flag" },
        { "id": 4, "name": "Many", "kind": "text", "choices": vec!["x"; MAX_CHOICES + 50] },
    ]);
    v["tasks"][0]["custom"] =
        serde_json::json!({ "1": { "number": 5.0 }, "2": { "number": 1e300 }, "3": { "flag": true }, "99": { "text": "orphan" } });
    v["tables"] = serde_json::json!([{ "name": "Entry", "columns": ["name", "custom:3", "custom:99", ""] }]);
    let (q, notes) = from_json_with_notes(&v.to_string(), Path::new("fields.plancraft")).unwrap();
    assert!(!notes.is_empty());

    let names: Vec<&str> = q.custom_fields.iter().map(|f| f.name.as_str()).collect();
    let lower: std::collections::HashSet<String> = names.iter().map(|n| n.to_lowercase()).collect();
    assert_eq!(lower.len(), names.len(), "names are unique after loading: {names:?}");
    let field = |id: u32| q.custom_field(CustomFieldId(id)).unwrap();
    assert!(field(2).choices.is_empty(), "no pick-list on a number field");
    assert_eq!(field(4).choices.len(), MAX_CHOICES);

    let task = q.task(t).unwrap();
    assert!(!task.custom.contains_key(&CustomFieldId(1)), "a number in a text field is dropped");
    assert!(!task.custom.contains_key(&CustomFieldId(99)), "a value of a missing field is dropped");
    assert!(task.custom.contains_key(&CustomFieldId(3)));
    match task.custom.get(&CustomFieldId(2)) {
        Some(CustomValue::Number(n)) => assert!(n.is_finite() && n.abs() <= plancraft_core::custom::MAX_CUSTOM_NUMBER),
        other => panic!("number kept and clamped: {other:?}"),
    }
    assert_eq!(q.table_layout("Entry").unwrap().columns, vec!["name".to_string(), "custom:3".to_string()]);
    // What loaded passes the edit checks, so the next edit is not refused.
    let mut q = q;
    assert!(limits::check_edit(&mut q).is_ok());
}

/// Regression: Page Setup values outside their types (a scale of 70000, 300 pages wide, a paper
/// size from a newer version, extra header strings) made the whole plan fail to open.
#[test]
fn page_setup_values_of_the_wrong_type_never_stop_a_plan_opening() {
    let p = demo_project(start());
    let mut v = serde_json::to_value(&p).unwrap();
    v["page_setup"] = serde_json::json!({
        "paper": "B5",
        "orientation": 7,
        "scale_percent": 70000,
        "margin_mm": "wide",
        "timescale": { "PagesWide": 300 },
        "repeat_columns": -4,
        "header": ["a", "b", "c", "d", 5],
        "legend": { "nested": true },
    });
    let (loaded, notes) = plancraft_core::io::from_json_with_notes(&v.to_string(), std::path::Path::new("x.plancraft")).unwrap();
    assert!(notes.iter().any(|n| n.contains("page setup")), "{notes:?}");
    let s = &loaded.page_setup;
    assert_eq!(s.paper, plancraft_core::print::Paper::Letter);
    assert_eq!(s.scale_percent, plancraft_core::print::MAX_SCALE);
    assert_eq!(s.timescale, plancraft_core::print::TimescaleFit::PagesWide(plancraft_core::print::MAX_PAGES_WIDE));
    assert_eq!(s.repeat_columns, 0);
    assert_eq!(s.header, ["a".to_string(), "b".to_string(), "c".to_string()]);
    // Something that isn't an object at all falls back to the defaults.
    v["page_setup"] = serde_json::json!("landscape please");
    let (loaded, _) = plancraft_core::io::from_json_with_notes(&v.to_string(), std::path::Path::new("x.plancraft")).unwrap();
    assert!(loaded.page_setup.is_default());
}

/// Hostile pauses in a file are clamped when it is opened and never break scheduling: too many,
/// out of order, negative or past the duration, and resume dates far away or before the start.
#[test]
fn hostile_splits_in_a_file_are_clamped_on_open() {
    let mut p = demo_project(start());
    let idx = p.tasks.iter().position(|t| (1..100).contains(&t.percent_complete)).unwrap();
    let mut v = serde_json::to_value(&p).unwrap();
    let mut splits: Vec<serde_json::Value> = (0..5_000)
        .map(|i| serde_json::json!({ "work": (i * 7919) % 20_000 - 5_000, "resume": format!("{}-06-01T08:00:00", 1800 + i % 600) }))
        .collect();
    splits.push(serde_json::json!({ "work": i64::MAX, "resume": "2300-12-31T23:59:00" }));
    v["tasks"][idx]["splits"] = serde_json::Value::Array(splits.clone());
    let (mut loaded, notes) = plancraft_core::io::from_json_with_notes(&v.to_string(), std::path::Path::new("x.plancraft")).unwrap();
    assert!(notes.iter().any(|n| n.contains("pauses")), "{notes:?}");
    let t = &loaded.tasks[idx];
    assert!(t.splits.len() <= plancraft_core::schedule::MAX_SPLITS);
    assert!(t.splits.iter().all(|s| (0..=plancraft_core::limits::MAX_MINUTES).contains(&s.work)));
    let report = recalculate(&mut loaded);
    assert!(!report.warnings.iter().any(|w| w.contains("internal error")), "{:?}", report.warnings);
    let s = &loaded.tasks[idx].sched;
    assert!(s.parts.len() <= plancraft_core::schedule::MAX_SPLITS + 1);
    assert!(s.parts.windows(2).all(|w| w[0].1 <= w[1].0), "parts are in order: {:?}", s.parts);
    // The same pauses set through edits are refused past the limit.
    p.tasks[idx].splits = (0..200).map(|i| Split { work: i, resume: shift(p.start, 10_000 + i) }).collect();
    assert!(plancraft_core::limits::check_edit(&mut p).is_err());
}
