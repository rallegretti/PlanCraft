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
        baseline_slot in prop_oneof![0u8..11, any::<u8>()],
        baselines in any::<u16>(),
    ) {
        let mut doc = Document::new(1, demo_project(start()), None);
        for op in ops {
            let r = doc.try_edit("junk", ChangeSource::Agent, |p| apply(p, &op));
            prop_assert!(!matches!(r, Err(PlanError::Internal(_))), "{op:?} panicked: {r:?}");
            prop_assert!(limits::check_counts(&doc.project).is_ok());
            let mut copy = doc.project.clone();
            prop_assert!(limits::sanitize(&mut copy).is_empty(), "{op:?} left values out of range");
        }
        // Whatever the edits left exports to Microsoft Project XML and imports again.
        let (xml, _) = plancraft_core::mspdi::to_xml(&doc.project);
        check_mspdi(&xml)?;
        // Whatever the edits left, the plan prints (table and chart) to a valid PDF, also over
        // any date range and at any zoom.
        let p = &doc.project;
        let day = |n: i64| plancraft_core::calendar::shift(start().and_hms_opt(0, 0, 0).unwrap(), n * 1440).date();
        let (from, to) = range.map_or((None, None), |(a, len)| (Some(day(a)), Some(day(a.saturating_add(len)))));
        let gantt = plancraft_core::print::ChartOptions { critical: true, baseline: true, slack: true, per_day, baseline_slot, ..Default::default() };
        for chart in [Some(gantt), Some(plancraft_core::print::ChartOptions { baselines, ..gantt }), None] {
            let sheet = plancraft_core::print::task_sheet(p, chart.is_some());
            let out = plancraft_core::print::layout(&plancraft_core::print::PrintJob {
                project: p,
                setup: &p.page_setup,
                sheet: &sheet,
                chart,
                from,
                to,
                printed: start().and_hms_opt(9, 0, 0).unwrap(),
                palette: plancraft_core::print::layout::Palette::default(),
            });
            prop_assert!(!out.pages.is_empty() && out.pages.len() <= plancraft_core::print::layout::MAX_PAGES);
            prop_assert!(plancraft_core::print::pdf::write(&out).starts_with(b"%PDF"));
        }
        // And so do the Network Diagram (in both arrangements) and the Task Board.
        let pal = plancraft_core::print::layout::Palette::default();
        let shown = vec![true; p.tasks.len()];
        let boxes = plancraft_core::network::boxes_for(p, &shown, &Default::default());
        let mut drawings = Vec::new();
        for ids_only in [false, true] {
            let opts = plancraft_core::network::NetworkOptions { critical_first: ids_only, ids_only };
            let d = plancraft_core::network::layout(p, &boxes, opts);
            let net = plancraft_core::print::network::NetPrintOptions { link_labels: true, progress_marks: true, ids_only };
            drawings.push(plancraft_core::print::network::items(p, &d, net, &pal));
        }
        let columns: Vec<plancraft_core::print::board::BoardColumnPrint> = p
            .board()
            .into_iter()
            .map(|(c, rows)| plancraft_core::print::board::BoardColumnPrint { name: c.name.clone(), percent: c.percent_complete, rows })
            .collect();
        drawings.push(plancraft_core::print::board::items(p, &columns, &pal));
        for (items, width, height) in drawings {
            let out = plancraft_core::print::layout_canvas(&plancraft_core::print::CanvasJob {
                project: p,
                setup: &p.page_setup,
                title: "Drawing",
                width,
                height,
                items,
                printed: start().and_hms_opt(9, 0, 0).unwrap(),
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
    Baseline(u8, Option<usize>, bool, bool),
    Plan(usize, usize, Option<usize>),
    ClearPlan(usize, Option<usize>),
    EvBaseline(u8),
    Split(usize, i64, i64),
    SplitLength(usize, usize, i64),
    PartDuration(usize, usize, i64),
    RemoveSplits(usize),
    Recurring(Option<usize>, u8, u16, u8, u8, i64, u16, i64),
    SetRecurrence(usize, u8, u16, u8, u8, i64, u16, i64),
    AddColumn(String, Option<usize>, Option<u8>),
    RenameColumn(usize, String),
    ColumnPercent(usize, Option<u8>),
    MoveColumn(usize, usize),
    DeleteColumn(usize),
    BoardStatus(usize, usize),
    ShowOnBoard(usize, bool),
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
        (prop_oneof![0u8..12, any::<u8>()], prop::option::of(i()), any::<bool>(), any::<bool>())
            .prop_map(|(n, t, up, down)| Op::Baseline(n, t, up, down)),
        (0usize..26, 0usize..26, prop::option::of(i())).prop_map(|(a, b, t)| Op::Plan(a, b, t)),
        (0usize..26, prop::option::of(i())).prop_map(|(a, t)| Op::ClearPlan(a, t)),
        prop_oneof![0u8..12, any::<u8>()].prop_map(Op::EvBaseline),
        (i(), big(), big()).prop_map(|(a, s, r)| Op::Split(a, s, r)),
        (i(), 0usize..4, big()).prop_map(|(a, k, m)| Op::SplitLength(a, k, m)),
        (i(), 0usize..4, big()).prop_map(|(a, k, m)| Op::PartDuration(a, k, m)),
        i().prop_map(Op::RemoveSplits),
        (
            prop::option::of(i()),
            any::<u8>(),
            prop_oneof![0u16..5, any::<u16>()],
            any::<u8>(),
            any::<u8>(),
            big(),
            prop_oneof![0u16..30, any::<u16>()],
            big()
        )
            .prop_map(|(a, k, n, x, y, s, c, d)| Op::Recurring(a, k, n, x, y, s, c, d)),
        (i(), any::<u8>(), prop_oneof![0u16..5, any::<u16>()], any::<u8>(), any::<u8>(), big(), prop_oneof![0u16..30, any::<u16>()], big())
            .prop_map(|(a, k, n, x, y, s, c, d)| Op::SetRecurrence(a, k, n, x, y, s, c, d)),
        (cell_text(), prop::option::of(any::<usize>()), prop::option::of(any::<u8>())).prop_map(|(n, a, pc)| Op::AddColumn(n, a, pc)),
        (0usize..8, cell_text()).prop_map(|(k, n)| Op::RenameColumn(k, n)),
        (0usize..8, prop::option::of(any::<u8>())).prop_map(|(k, pc)| Op::ColumnPercent(k, pc)),
        (0usize..8, any::<usize>()).prop_map(|(k, to)| Op::MoveColumn(k, to)),
        (0usize..8).prop_map(Op::DeleteColumn),
        (i(), 0usize..8).prop_map(|(a, k)| Op::BoardStatus(a, k)),
        (i(), any::<bool>()).prop_map(|(a, b)| Op::ShowOnBoard(a, b)),
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

/// Any recurrence, out-of-range numbers included: `kind` picks the pattern, `n` the interval,
/// `x`/`y` the days, `s` the start (minutes from the project start), `count` the occurrences
/// (0: end by a date `s + d`), `d` the duration.
#[allow(clippy::too_many_arguments)]
fn recurrence(p: &Project, kind: u8, n: u16, x: u8, y: u8, s: i64, count: u16, d: i64) -> plancraft_core::recurring::Recurrence {
    use plancraft_core::recurring::{MonthDay, Nth, Pattern, RecurEnd, Recurrence};
    let on = if x.is_multiple_of(2) { MonthDay::Day(y) } else { MonthDay::Weekday { nth: Nth::ALL[usize::from(y) % 5], weekday: x / 2 } };
    let pattern = match kind % 4 {
        0 => Pattern::Daily { interval: n, workdays: x.is_multiple_of(2) },
        1 => Pattern::Weekly { interval: n, days: y },
        2 => Pattern::Monthly { interval: n, on },
        _ => Pattern::Yearly { month: x % 14, on },
    };
    let start = shift(p.start, s).date();
    let end = if count == 0 { RecurEnd::By(shift(p.start, s.saturating_add(d)).date()) } else { RecurEnd::After(count) };
    Recurrence { pattern, start, end, duration: d.rem_euclid(5 * 480), calendar: None, ignore_resource_calendars: y.is_multiple_of(2) }
}

/// Any plan slot, out-of-range numbers included: Scheduled, Baseline 0–12, Interim 0–11.
fn slot(k: usize) -> PlanSlot {
    match k {
        0 => PlanSlot::Scheduled,
        1..=13 => PlanSlot::Baseline((k - 1) as u8),
        _ => PlanSlot::Interim((k - 14) as u8),
    }
}

fn column_at(p: &Project, k: usize) -> plancraft_core::ids::BoardColumnId {
    p.board_columns.get(k % p.board_columns.len().max(1)).map_or(plancraft_core::ids::BoardColumnId(9999), |c| c.id)
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
        Op::Baseline(n, t, up, down) => {
            let tasks: Vec<TaskId> = t.map(|i| task_at(p, i)).into_iter().collect();
            p.set_baseline_in(n, &Scope { tasks, roll_up_to_summaries: up, roll_up_from_subtasks: down })?;
        }
        Op::Plan(a, b, t) => {
            let tasks: Vec<TaskId> = t.map(|i| task_at(p, i)).into_iter().collect();
            p.save_plan(slot(a), slot(b), &Scope::selected(tasks))?;
        }
        Op::ClearPlan(a, t) => {
            let tasks: Vec<TaskId> = t.map(|i| task_at(p, i)).into_iter().collect();
            p.clear_plan(slot(a), &tasks)?;
        }
        Op::EvBaseline(n) => p.set_earned_value_baseline(n)?,
        Op::Split(i, s, r) => p.split_task(task_at(p, i), shift(p.start, s), shift(p.start, r))?,
        Op::SplitLength(i, k, m) => p.set_split_length(task_at(p, i), k, m)?,
        Op::PartDuration(i, k, m) => p.set_part_duration(task_at(p, i), k, m)?,
        Op::RemoveSplits(i) => {
            p.remove_splits(task_at(p, i))?;
        }
        Op::Recurring(at, kind, n, x, y, s, c, d) => {
            let rec = recurrence(p, kind, n, x, y, s, c, d);
            p.add_recurring_task("Every", rec, at)?;
        }
        Op::SetRecurrence(i, kind, n, x, y, s, c, d) => {
            let rec = recurrence(p, kind, n, x, y, s, c, d);
            p.set_recurrence(task_at(p, i), None, rec)?;
        }
        Op::AddColumn(name, at, pc) => {
            p.add_board_column(&name, at, pc)?;
        }
        Op::RenameColumn(k, name) => p.rename_board_column(column_at(p, k), &name)?,
        Op::ColumnPercent(k, pc) => p.set_board_column_percent(column_at(p, k), pc)?,
        Op::MoveColumn(k, to) => p.move_board_column(column_at(p, k), to)?,
        Op::DeleteColumn(k) => {
            p.delete_board_column(column_at(p, k))?;
        }
        Op::BoardStatus(i, k) => p.set_board_status(&[task_at(p, i)], column_at(p, k))?,
        Op::ShowOnBoard(i, b) => p.set_show_on_board(&[task_at(p, i)], b)?,
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
                drawing_fit: if wide % 3 == 0 { TimescaleFit::AsShown } else { TimescaleFit::PagesWide(wide) },
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
        "drawing_fit": { "PagesWide": 255 },
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
    assert_eq!(s.drawing_fit, plancraft_core::print::TimescaleFit::PagesWide(plancraft_core::print::MAX_PAGES_WIDE));
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
fn hostile_baselines_in_a_file_are_clamped_on_open() {
    let p = demo_project(start());
    let mut v = serde_json::to_value(&p).unwrap();
    let b = |start: &str, work: i64, cost: f64| serde_json::json!({ "start": start, "finish": "2400-01-01T17:00:00", "duration": -5, "work": work, "cost": cost, "saved": "1066-10-14T09:00:00" });
    v["tasks"][1]["baselines"] = serde_json::json!({
        "0": b("2026-10-19T08:00:00", 480, 1.0),
        "3": b("1500-01-01T08:00:00", i64::MAX, 1e300),
        "11": b("2026-10-19T08:00:00", 480, 1.0),
        "255": b("2026-10-19T08:00:00", 480, 1.0),
    });
    v["tasks"][1]["interim"] = serde_json::json!({
        "0": { "start": "2026-10-19T08:00:00", "finish": "2026-10-20T17:00:00" },
        "10": { "start": "1200-01-01T08:00:00", "finish": "3000-01-01T08:00:00" },
    });
    v["settings"]["earned_value_baseline"] = serde_json::json!(200);
    let (mut loaded, notes) = plancraft_core::io::from_json_with_notes(&v.to_string(), std::path::Path::new("x.plancraft")).unwrap();
    assert!(notes.iter().any(|n| n.contains("baseline or interim plan number")), "{notes:?}");
    assert!(notes.iter().any(|n| n.contains("earned value baseline")), "{notes:?}");
    let t = &loaded.tasks[1];
    assert_eq!(t.baselines.keys().copied().collect::<Vec<_>>(), [3], "only Baseline1–10 are kept");
    assert_eq!(t.interim.keys().copied().collect::<Vec<_>>(), [10]);
    let b3 = t.baseline_in(3).unwrap();
    assert!(
        b3.start.date() >= chrono::NaiveDate::from_ymd_opt(1900, 1, 1).unwrap() && b3.work <= limits::MAX_TOTAL_MINUTES && b3.duration >= 0
    );
    assert!(b3.cost.abs() <= limits::MAX_TOTAL_MONEY);
    assert_eq!(loaded.settings.earned_value_baseline, 0);
    assert!(limits::sanitize(&mut loaded).is_empty(), "loading left nothing out of range");
    let report = recalculate(&mut loaded);
    assert!(!report.warnings.iter().any(|w| w.contains("internal error")), "{:?}", report.warnings);
    // Regression: a key that isn't a small number refused the whole file; now it is dropped.
    v["tasks"][1]["baselines"] = serde_json::json!({ "999": b("2026-10-19T08:00:00", 480, 1.0), "x": b("2026-10-19T08:00:00", 1, 1.0), "2": b("2026-10-19T08:00:00", 480, 1.0) });
    let (loaded, notes) = plancraft_core::io::from_json_with_notes(&v.to_string(), std::path::Path::new("x.plancraft")).unwrap();
    assert!(notes.iter().any(|n| n.contains("baseline or interim plan number")), "{notes:?}");
    assert_eq!(loaded.tasks[1].baselines.keys().copied().collect::<Vec<_>>(), [2]);
    // A baseline that starts after it finishes is put in order.
    let bl = loaded.tasks[1].baseline_in(2).unwrap();
    assert!(bl.start <= bl.finish);
}

#[test]
fn hostile_boards_in_a_file_are_clamped_on_open() {
    let p = demo_project(start());
    let mut v = serde_json::to_value(&p).unwrap();
    let cols: Vec<serde_json::Value> =
        (1..=300).map(|i| serde_json::json!({ "id": i, "name": if i % 3 == 0 { "Same".to_string() } else { String::new() }, "percent_complete": 255 })).collect();
    v["board_columns"] = serde_json::Value::Array(cols);
    v["tasks"][1]["board_status"] = serde_json::json!(99_999);
    let (mut loaded, notes) = plancraft_core::io::from_json_with_notes(&v.to_string(), std::path::Path::new("x.plancraft")).unwrap();
    for want in ["more than 100 columns", "column name", "% complete is above 100", "board column that does not exist"] {
        assert!(notes.iter().any(|n| n.contains(want)), "{want}: {notes:?}");
    }
    assert_eq!(loaded.board_columns.len(), plancraft_core::board::MAX_BOARD_COLUMNS);
    let names: std::collections::HashSet<String> = loaded.board_columns.iter().map(|c| c.name.to_lowercase()).collect();
    assert_eq!(names.len(), loaded.board_columns.len(), "names are unique");
    assert!(loaded.ids.board_column >= 300, "the counter stays ahead of stored ids");
    assert!(limits::sanitize(&mut loaded).is_empty());
    // Two columns with one id refuse the file, like other repeated ids; no columns at all get
    // the default board.
    v["board_columns"] = serde_json::json!([{ "id": 1, "name": "A" }, { "id": 1, "name": "B" }]);
    assert!(plancraft_core::io::from_json_with_notes(&v.to_string(), std::path::Path::new("x.plancraft")).is_err());
    v["board_columns"] = serde_json::json!([]);
    v["tasks"][1]["board_status"] = serde_json::Value::Null;
    let (loaded, _) = plancraft_core::io::from_json_with_notes(&v.to_string(), std::path::Path::new("x.plancraft")).unwrap();
    assert_eq!(loaded.board_columns.len(), 4);
    // Regression: a counter at the very top gave the default columns ids out of range, so the
    // plan opened but every edit was then refused.
    v["ids"]["board_column"] = serde_json::json!(u32::MAX);
    let (loaded, _) = plancraft_core::io::from_json_with_notes(&v.to_string(), std::path::Path::new("x.plancraft")).unwrap();
    let mut doc = Document::new(1, loaded, None);
    doc.edit("Rename", |p| p.name = "Still editable".into()).unwrap();
    assert!(limits::check_counts(&doc.project).is_ok());
}

#[test]
fn hostile_planned_splits_and_recurrences_in_a_file_are_clamped_on_open() {
    let p = demo_project(start());
    let mut v = serde_json::to_value(&p).unwrap();
    let gaps: Vec<serde_json::Value> = (0..500).map(|i| serde_json::json!({ "work": i * 977 - 3000, "length": i64::MAX - i })).collect();
    v["tasks"][1]["gaps"] = serde_json::Value::Array(gaps);
    v["tasks"][2]["recurrence"] = serde_json::json!({
        "pattern": { "Weekly": { "interval": 0, "days": 0 } },
        "start": "2026-10-19", "end": { "After": 60000 }, "duration": -5, "calendar": 999,
    });
    let (mut loaded, notes) = plancraft_core::io::from_json_with_notes(&v.to_string(), std::path::Path::new("x.plancraft")).unwrap();
    assert!(notes.iter().any(|n| n.contains("splits")), "{notes:?}");
    assert!(notes.iter().any(|n| n.contains("recurring task's pattern")), "{notes:?}");
    let t = &loaded.tasks[1];
    assert!(t.gaps.len() <= plancraft_core::schedule::MAX_SPLITS);
    assert!(t.gaps.iter().all(|g| (0..=limits::MAX_MINUTES).contains(&g.work) && (0..=limits::MAX_MINUTES).contains(&g.length)));
    assert!(loaded.tasks[2].recurrence.is_none());
    assert!(limits::sanitize(&mut loaded).is_empty());
    let report = recalculate(&mut loaded);
    assert!(!report.warnings.iter().any(|w| w.contains("internal error")), "{:?}", report.warnings);
    assert!(loaded.tasks[1].sched.finish.is_some_and(|f| f.date() <= chrono::NaiveDate::from_ymd_opt(2300, 12, 31).unwrap()));
}

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

proptest! {
    #![proptest_config(ProptestConfig { cases: cases(256), ..ProptestConfig::default() })]

    /// The Network Diagram lays out any outline and any links, cycles from a damaged file
    /// included: one box per shown task, no two in one place, every task right of its
    /// predecessors when there is no cycle, and a line for every link.
    #[test]
    fn network_diagrams_lay_out_any_plan(
        levels in prop::collection::vec(1u16..6, 0..40),
        links in prop::collection::vec((0usize..40, 0usize..40, 0usize..5), 0..80),
        shown in prop::collection::vec(any::<bool>(), 40),
        collapse in prop::collection::vec(any::<bool>(), 40),
        critical_first in any::<bool>(),
        ids_only in any::<bool>(),
    ) {
        use plancraft_core::network::{NetLinkKind, NetworkOptions, boxes_for, layout};
        let mut p = Project::new("Net", start());
        let ids: Vec<TaskId> = levels.iter().enumerate().map(|(i, _)| p.add_task(format!("T{i}"), None)).collect();
        for (t, &lvl) in p.tasks.iter_mut().zip(&levels) {
            t.outline_level = lvl;
        }
        let n = ids.len();
        let mut cyclic = false;
        for &(a, b, how) in &links {
            if n == 0 {
                break;
            }
            let (a, b) = (ids[a % n], ids[b % n]);
            if how == 4 {
                // Written straight into the plan, as a damaged file could hold it.
                p.task_mut(b).unwrap().predecessors.push(Dependency::fs(a));
                cyclic = true;
            } else {
                let _ = p.link(a.into(), b, LinkType::ALL[how], Lag::None);
            }
        }
        recalculate(&mut p);
        let shown: Vec<bool> = shown.into_iter().take(n).collect();
        let collapsed: std::collections::HashSet<TaskId> = ids.iter().zip(&collapse).filter(|(_, c)| **c).map(|(id, _)| *id).collect();
        let boxes = boxes_for(&p, &shown, &collapsed);
        let d = layout(&p, &boxes, NetworkOptions { critical_first, ids_only });
        let shown_count = shown.iter().filter(|s| **s).count();
        if collapse.iter().all(|c| !c) {
            prop_assert_eq!(d.nodes.len(), shown_count);
        } else {
            prop_assert!(d.nodes.len() <= shown_count);
        }
        let mut cells = std::collections::HashSet::new();
        for (k, node) in d.nodes.iter().enumerate() {
            prop_assert!(cells.insert((node.col, node.row)), "two boxes at {:?}", (node.col, node.row));
            prop_assert!(node.col < d.cols && node.row < d.rows);
            let r = d.rect(k).unwrap();
            prop_assert_eq!(d.node_at(r.x + 1.0, r.y + 1.0), Some(k));
        }
        // Every link between tasks in two different boxes has its line.
        let node = |task: usize| boxes.get(task).copied().flatten().and_then(|b| d.node_of(b));
        let lines: std::collections::HashSet<(usize, usize)> = d.links.iter().map(|l| (l.from, l.to)).collect();
        for (si, t) in p.tasks.iter().enumerate() {
            for dep in &t.predecessors {
                let TaskRef::Local(pid) = dep.predecessor else { continue };
                let pi = p.task_index(pid).unwrap();
                if let (Some(a), Some(b)) = (node(pi), node(si))
                    && a != b
                {
                    prop_assert!(lines.contains(&(a, b)), "no line for {pi} -> {si}");
                }
            }
        }
        // The graph the columns come from: the lines plus summary -> subtask.
        let nn = d.nodes.len();
        let mut succ: Vec<Vec<usize>> = vec![Vec::new(); nn];
        for l in &d.links {
            succ[l.from].push(l.to);
        }
        for (k, nd) in d.nodes.iter().enumerate() {
            let mut a = p.parent_index(nd.task);
            while let Some(ai) = a {
                if let Some(s) = d.node_of(ai) {
                    succ[s].push(k);
                    break;
                }
                a = p.parent_index(ai);
            }
        }
        let reaches = |from: usize, to: usize| {
            let mut seen = vec![false; nn];
            let mut stack = vec![from];
            while let Some(x) = stack.pop() {
                if x == to {
                    return true;
                }
                if !std::mem::replace(&mut seen[x], true) {
                    stack.extend(succ[x].iter().copied());
                }
            }
            false
        };
        for l in &d.links {
            prop_assert!(d.route(l).len() >= 2);
            // Lines closing a loop may point left; every other one points right.
            if matches!(l.kind, NetLinkKind::Dependency { .. } | NetLinkKind::Member) && !reaches(l.to, l.from) {
                prop_assert!(d.nodes[l.to].col > d.nodes[l.from].col, "{:?} in {:?}", l, d.nodes);
            }
        }
        let _ = cyclic;
    }
}

// ---------------------------------------------------------------------------
// Microsoft Project XML (MSPDI) import
// ---------------------------------------------------------------------------

/// A plan exported to MSPDI, with one of everything the format carries.
fn mspdi_sample() -> String {
    let mut p = demo_project(start());
    p.status_date = Some(start());
    let f = p.add_custom_field("Phase", CustomFieldKind::Text, &["A".into()]).unwrap();
    let t = p.tasks[1].id;
    p.set_custom_value(t, f, Some(CustomValue::Text("A".into()))).unwrap();
    p.calendars[0].exceptions.push(CalendarException {
        name: "Yearly".into(),
        start: start(),
        end: start(),
        working: Vec::new(),
        yearly: true,
    });
    p.resources[0].exceptions.push(CalendarException::non_working("Leave", start(), start()));
    recalculate(&mut p);
    p.save_plan(PlanSlot::Scheduled, PlanSlot::Baseline(0), &Scope::default()).unwrap();
    p.save_plan(PlanSlot::Scheduled, PlanSlot::Interim(1), &Scope::default()).unwrap();
    plancraft_core::mspdi::to_xml(&p).0
}

thread_local! {
    static PANICS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Counts panics on this thread, even those a guard turns into an error (the default report
/// still goes to stderr).
fn count_panics() {
    static HOOK: std::sync::Once = std::sync::Once::new();
    HOOK.call_once(|| {
        let default = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            PANICS.with(|p| p.set(p.get() + 1));
            default(info);
        }));
    });
}

fn panics() -> usize {
    PANICS.with(std::cell::Cell::get)
}

/// Imports MSPDI text: either an error, or a plan that is in range and schedules; never a panic,
/// not even one a guard catches.
fn check_mspdi(text: &str) -> Result<(), TestCaseError> {
    check_mspdi_bytes(text.as_bytes())
}

fn check_mspdi_bytes(bytes: &[u8]) -> Result<(), TestCaseError> {
    count_panics();
    let before = panics();
    let imported = plancraft_core::mspdi::from_bytes(bytes, "x");
    prop_assert_eq!(panics(), before, "the importer panicked");
    match imported {
        Err(_) => {}
        Ok((mut p, _)) => {
            prop_assert!(limits::check_counts(&p).is_ok());
            prop_assert!(plancraft_core::schedule::try_recalculate(&mut p).is_ok());
            let mut copy = p.clone();
            prop_assert!(limits::sanitize(&mut copy).is_empty(), "values out of range after import");
            // What was imported exports and imports again.
            let (again, _) = plancraft_core::mspdi::to_xml(&p);
            prop_assert!(plancraft_core::mspdi::from_xml(&again, "x").is_ok());
            prop_assert_eq!(panics(), before, "scheduling or exporting the import panicked");
        }
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(cases(256)))]

    /// A valid export with junk in one value is imported (clamped) or refused, never a panic.
    #[test]
    fn mutated_mspdi_files_import_or_fail_cleanly(at in 0usize..5000, junk in prop_oneof![
        Just("-1".to_string()),
        Just("99999999999999999999".to_string()),
        Just("-9223372036854775808".to_string()),
        Just("1e308".to_string()),
        Just("NaN".to_string()),
        Just("+262143-12-31T23:59:59".to_string()),
        Just("9999-12-31T24:00:00".to_string()),
        Just("0000-01-01T00:00:00".to_string()),
        Just("P99999999999Y".to_string()),
        Just("-PT99999999999H".to_string()),
        Just("PT".to_string()),
        Just("25:61:61".to_string()),
        Just(String::new()),
        "\\PC{0,12}",
    ]) {
        let xml = mspdi_sample();
        let leaves: Vec<usize> = xml.lines().enumerate().filter(|(_, l)| l.trim_start().starts_with('<') && l.contains("</")).map(|(i, _)| i).collect();
        let target = leaves[at % leaves.len()];
        let mutated: String = xml
            .lines()
            .enumerate()
            .map(|(i, l)| {
                if i != target {
                    return format!("{l}\n");
                }
                let open = l.find('>').unwrap();
                let close = l.rfind("</").unwrap();
                format!("{}{}{}\n", &l[..=open], plancraft_core::mspdi::xml::escape(&junk), &l[close..])
            })
            .collect();
        check_mspdi(&mutated)?;
    }

    /// Random markup built from MSPDI's own pieces never panics the importer.
    #[test]
    fn random_mspdi_markup_never_panics(parts in prop::collection::vec(prop_oneof![
        Just("<Project>"), Just("</Project>"), Just("<Tasks>"), Just("</Tasks>"), Just("<Task>"), Just("</Task>"),
        Just("<UID>"), Just("</UID>"), Just("<OutlineLevel>"), Just("</OutlineLevel>"), Just("<Duration>"), Just("</Duration>"),
        Just("<PredecessorLink><PredecessorUID>1</PredecessorUID><Type>9</Type><LinkLag>"), Just("</LinkLag><LagFormat>19</LagFormat></PredecessorLink>"),
        Just("<Calendars><Calendar><UID>1</UID><Exceptions><Exception><TimePeriod><FromDate>2026-01-01</FromDate><ToDate>2300-12-31</ToDate></TimePeriod><Type>"),
        Just("</Type><Period>0</Period><MonthItem>9</MonthItem><MonthPosition>4</MonthPosition><DaysOfWeek>127</DaysOfWeek></Exception></Exceptions></Calendar></Calendars>"),
        Just("<Resources><Resource><UID>1</UID><Name>R</Name><CalendarUID>1</CalendarUID></Resource></Resources>"),
        Just("<Assignments><Assignment><TaskUID>1</TaskUID><ResourceUID>1</ResourceUID><Units>"), Just("</Units></Assignment></Assignments>"),
        Just("1"), Just("0"), Just("-7"), Just("4294967296"), Just("PT8H0M0S"), Just("P1Y2M3DT4H5M6.5S"), Just("2026-10-19T08:00:00"),
        Just("&amp;"), Just("&#x1F600;"), Just("&#0;"), Just("&bogus;"), Just("<![CDATA[x<y]]>"), Just("<!-- c -->"), Just("<!DOCTYPE p>"), Just("<?pi x?>"),
        Just("<a:Task xmlns:a=\"x\">"), Just("</a:Task>"), Just("<Empty/>"), Just("<"), Just(">"), Just("\u{feff}"),
    ], 0..60)) {
        let text: String = parts.concat();
        check_mspdi(&text)?;
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(cases(256)))]

    /// Any bytes: UTF-16 of odd length, a byte order mark with nothing after it, broken UTF-8.
    #[test]
    fn arbitrary_bytes_never_panic_the_importer(bytes in prop_oneof![
        prop::collection::vec(any::<u8>(), 0..300),
        prop::collection::vec(any::<u8>(), 0..300).prop_map(|b| [vec![0xFF, 0xFE], b].concat()),
        prop::collection::vec(any::<u8>(), 0..300).prop_map(|b| [vec![0xFE, 0xFF], b].concat()),
    ]) {
        check_mspdi_bytes(&bytes)?;
    }
}

/// Regressions from reviewing the importer: each of these files once made it panic on an
/// overflow, walk dates for seconds, or fill gigabytes of memory.
#[test]
fn hostile_mspdi_calendars_are_bounded() {
    let timed = |name: &str, xml: String| {
        let t = std::time::Instant::now();
        check_mspdi(&xml).unwrap_or_else(|e| panic!("{name}: {e}"));
        let (p, _) = plancraft_core::mspdi::from_xml(&xml, "x").unwrap();
        assert!(
            t.elapsed() < std::time::Duration::from_secs(if cfg!(debug_assertions) { 60 } else { 10 }),
            "{name} took {:?}",
            t.elapsed()
        );
        p
    };
    let cal = |inner: &str| {
        format!("<Project><Calendars><Calendar><UID>1</UID><IsBaseCalendar>1</IsBaseCalendar>{inner}</Calendar></Calendars></Project>")
    };
    // Overflowing values in a recurring exception.
    for (kind, extra) in [
        (2, "<Period>9223372036854775807</Period>"),
        (6, "<Period>9223372036854775807</Period><DaysOfWeek>2</DaysOfWeek>"),
        (5, "<MonthItem>-9223372036854775808</MonthItem><MonthPosition>9223372036854775807</MonthPosition>"),
        (3, "<MonthItem>4</MonthItem><MonthPosition>9223372036854775807</MonthPosition>"),
    ] {
        let ex = format!(
            "<Exceptions><Exception><TimePeriod><FromDate>2026-01-01</FromDate><ToDate>2030-01-01</ToDate></TimePeriod><Type>{kind}</Type>{extra}</Exception></Exceptions>"
        );
        timed("overflow", cal(&ex));
    }
    // A weekly exception on no day, and a work week spanning hundreds of thousands of years.
    let ex = "<Exceptions><Exception><TimePeriod><FromDate>2026-01-01</FromDate><ToDate>2300-01-01</ToDate></TimePeriod><Type>6</Type><DaysOfWeek>128</DaysOfWeek></Exception></Exceptions>".repeat(50);
    timed("weekly on no day", cal(&ex));
    let ww = "<WorkWeeks><WorkWeek><TimePeriod><FromDate>-200000-01-01T00:00:00</FromDate><ToDate>+200000-01-01T00:00:00</ToDate></TimePeriod><WeekDay><DayType>1</DayType><DayWorking>1</DayWorking><WorkingTimes><WorkingTime><FromTime>08:00:00</FromTime><ToTime>12:00:00</ToTime></WorkingTime></WorkingTimes></WeekDay></WorkWeek></WorkWeeks>".repeat(50);
    timed("endless work weeks", cal(&ww));
    // Thousands of exceptions that each repeat thousands of times: capped per calendar.
    let ex = "<Exception><TimePeriod><FromDate>1900-01-01</FromDate></TimePeriod><Type>1</Type><Period>2</Period><EnteredByOccurrences>1</EnteredByOccurrences><Occurrences>9999</Occurrences></Exception>".repeat(3000);
    let p = timed("many exceptions", cal(&format!("<Exceptions>{ex}</Exceptions>")));
    assert!(p.calendars.iter().all(|c| c.exceptions.len() <= limits::MAX_LIST));
    // One resource calendar with a lot of time off shared by thousands of resources.
    let ex = "<Exception><TimePeriod><FromDate>1900-01-01</FromDate></TimePeriod><Type>1</Type><Period>2</Period><EnteredByOccurrences>1</EnteredByOccurrences><Occurrences>5000</Occurrences></Exception>".repeat(2);
    let resources: String =
        (1..=6000).map(|i| format!("<Resource><UID>{i}</UID><Name>R{i}</Name><CalendarUID>2</CalendarUID></Resource>")).collect();
    let xml = format!(
        "<Project><Calendars><Calendar><UID>1</UID><IsBaseCalendar>1</IsBaseCalendar></Calendar><Calendar><UID>2</UID><IsBaseCalendar>0</IsBaseCalendar><BaseCalendarUID>1</BaseCalendarUID><Exceptions>{ex}</Exceptions></Calendar></Calendars><Resources>{resources}</Resources></Project>"
    );
    let p = timed("shared resource calendar", xml);
    let copied: usize = p.resources.iter().map(|r| r.exceptions.len()).sum();
    assert!(copied <= 200_000, "{copied} exceptions copied");
}

#[test]
fn mspdi_files_with_too_many_tasks_are_refused() {
    let mut xml = String::from("<Project><Tasks>");
    for i in 0..=limits::MAX_TASKS {
        xml += &format!("<Task><UID>{}</UID><Name>t</Name></Task>", i + 1);
    }
    xml += "</Tasks></Project>";
    let e = plancraft_core::mspdi::from_xml(&xml, "x").unwrap_err();
    assert!(e.contains("too many tasks"), "{e}");
}

/// Text in any script, with direction marks, combining marks and junk mixed in.
fn any_script_text() -> impl Strategy<Value = String> {
    prop_oneof![
        "\\PC{0,40}",
        "[א-ת0-9 ()\\[\\]a-z\u{05B0}-\u{05C7}\u{200E}\u{200F}\u{202A}-\u{202E}\u{2066}-\u{2069}]{0,30}",
        "[\u{0600}-\u{06FF}\u{4E00}-\u{4E40}\u{3040}-\u{30FF}\u{AC00}-\u{AC40}\u{0E00}-\u{0E7F}\u{0900}-\u{097F} a-z]{0,30}",
        any::<String>(),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig { cases: cases(256), ..ProptestConfig::default() })]

    /// Right-to-left reordering, measuring and printing text in any script never panic, keep
    /// every character, and write a valid PDF (system fallback fonts included).
    #[test]
    fn text_in_any_script_prints(texts in prop::collection::vec(any_script_text(), 1..6), size in prop_oneof![Just(9.0f32), -5.0f32..200.0, Just(f32::NAN)]) {
        count_panics();
        let before = panics();
        use plancraft_core::print::{Document, Item, Page, Rgb, font, pt};
        let mut items = Vec::new();
        for (i, t) in texts.iter().enumerate() {
            let shown = plancraft_core::bidi::visual(t);
            prop_assert_eq!(shown.chars().count(), t.chars().count());
            let w = font::width(t, 9.0, i % 2 == 0);
            prop_assert!(w.is_finite() && w >= 0.0);
            let fitted = font::fit(t, 9.0, false, 60.0);
            prop_assert!(font::width(&fitted, 9.0, false) <= 60.0 + 0.01 || fitted.is_empty());
            items.push(Item::Text { at: pt(10.0, 20.0 + 12.0 * i as f32), size, color: Rgb(0, 0, 0), bold: i % 2 == 1, text: t.clone() });
        }
        let doc = Document { title: texts.concat(), pages: vec![Page { width: 600.0, height: 800.0, items }], truncated: false };
        let pdf = plancraft_core::print::pdf::write(&doc);
        prop_assert!(pdf.starts_with(b"%PDF") && pdf.ends_with(b"%%EOF\n"));
        prop_assert_eq!(panics(), before, "a panic was caught");
    }
}
