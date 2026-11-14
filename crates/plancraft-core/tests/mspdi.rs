#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]
//! Microsoft Project XML (MSPDI) import and export: round trips, hand-written files and hostile
//! input. Parity with the published schema reference is in `ms_parity.rs`.

use chrono::{NaiveDate, NaiveDateTime};
use plancraft_core::mspdi;
use plancraft_core::prelude::*;
use plancraft_core::sample::demo_project;

fn d(y: i32, m: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, day).unwrap()
}

fn dt(y: i32, m: u32, day: u32, h: u32) -> NaiveDateTime {
    d(y, m, day).and_hms_opt(h, 0, 0).unwrap()
}

/// The demo plan plus one of everything MSPDI can carry.
fn rich_plan() -> Project {
    let mut p = demo_project(d(2026, 10, 19));
    p.author = "A. Author".into();
    p.manager = "M. Manager".into();
    p.company = "Example & Co <Ltd>".into();
    p.status_date = Some(d(2026, 11, 2));
    p.settings.currency = "€".into();
    let cal = p.calendar;
    let c = p.calendars.iter_mut().find(|c| c.id == cal).unwrap();
    c.exceptions.push(CalendarException::non_working("Plant shutdown", d(2026, 12, 24), d(2026, 12, 31)));
    c.exceptions.push(CalendarException {
        name: "New Year".into(),
        start: d(2027, 1, 1),
        end: d(2027, 1, 1),
        working: Vec::new(),
        yearly: true,
    });
    c.exceptions.push(CalendarException {
        name: "Short Friday".into(),
        start: d(2026, 11, 13),
        end: d(2026, 11, 13),
        working: vec![TimeRange::hours(8, 12)],
        yearly: false,
    });

    let text = p.add_custom_field("Phase code", CustomFieldKind::Text, &["A".into(), "B".into()]).unwrap();
    let number = p.add_custom_field("Risk score", CustomFieldKind::Number, &[]).unwrap();
    let flag = p.add_custom_field("Approved", CustomFieldKind::Flag, &[]).unwrap();
    let when = p.add_custom_field("Review", CustomFieldKind::Date, &[]).unwrap();
    let span = p.add_custom_field("Buffer", CustomFieldKind::Duration, &[]).unwrap();
    let money = p.add_custom_field("Budget", CustomFieldKind::Cost, &[]).unwrap();
    let ids: Vec<TaskId> = p.tasks.iter().map(|t| t.id).collect();
    p.set_custom_value(ids[1], text, Some(CustomValue::Text("B".into()))).unwrap();
    p.set_custom_value(ids[1], number, Some(CustomValue::Number(3.5))).unwrap();
    p.set_custom_value(ids[2], flag, Some(CustomValue::Flag(true))).unwrap();
    p.set_custom_value(ids[2], when, Some(CustomValue::Date(dt(2026, 11, 20, 9)))).unwrap();
    p.set_custom_value(ids[2], span, Some(CustomValue::Duration(240))).unwrap();
    p.set_custom_value(ids[2], money, Some(CustomValue::Number(1250.25))).unwrap();

    let manual = p.add_task("Manual check", None);
    let inactive = p.add_task("Dropped idea", None);
    let constrained = p.add_task("Fixed date", None);
    {
        let t = p.task_mut(manual).unwrap();
        t.mode = ScheduleMode::Manual;
        t.manual_start = Some(dt(2026, 11, 3, 8));
        t.manual_finish = Some(dt(2026, 11, 4, 17));
        t.duration = 2 * 480;
        t.notes = "Line one\nLine two & <three>".into();
    }
    p.task_mut(inactive).unwrap().active = false;
    {
        let t = p.task_mut(constrained).unwrap();
        t.constraint = Constraint { kind: ConstraintKind::Snet, date: Some(dt(2026, 11, 16, 8)) };
        t.deadline = Some(dt(2026, 11, 30, 17));
        t.priority = 900;
        t.task_type = TaskType::FixedDuration;
        t.effort_driven = true;
        t.leveling_delay = 2 * 480;
        t.fixed_cost = 99.5;
        t.estimated = true;
    }
    p.link(TaskRef::Local(manual), constrained, LinkType::SS, Lag::Percent(50)).unwrap();
    p.link(TaskRef::Local(ids[1]), inactive, LinkType::FF, Lag::Elapsed(2 * 1440)).unwrap();

    let material = p.add_resource("Concrete");
    let cost = p.add_resource("Travel");
    {
        let r = p.resource_mut(material).unwrap();
        r.kind = ResourceType::Material;
        r.material_label = "tons".into();
        r.standard_rate = 120.0;
    }
    p.resource_mut(cost).unwrap().kind = ResourceType::Cost;
    let person = p.resources[0].id;
    {
        let r = p.resource_mut(person).unwrap();
        r.standard_rate = 85.0;
        r.overtime_rate = 120.0;
        r.cost_per_use = 10.0;
        r.email = "person@example.com".into();
        r.group = "Design".into();
        r.exceptions.push(CalendarException::non_working("Vacation", d(2026, 11, 9), d(2026, 11, 10)));
        r.availability.push(AvailabilityPeriod { from: Some(d(2026, 12, 1)), to: Some(d(2026, 12, 31)), units: 0.5 });
    }
    p.assign(constrained, material, Some(3.0), None).unwrap();
    let a = p.assign(constrained, cost, None, None).unwrap();
    p.assignments.iter_mut().find(|x| x.id == a).unwrap().cost = 400.0;
    recalculate(&mut p);
    p.save_plan(PlanSlot::Scheduled, PlanSlot::Baseline(0), &Scope::default()).unwrap();
    p.save_plan(PlanSlot::Scheduled, PlanSlot::Baseline(3), &Scope::default()).unwrap();
    p.save_plan(PlanSlot::Scheduled, PlanSlot::Interim(2), &Scope::default()).unwrap();
    recalculate(&mut p);
    p
}

fn round_trip(p: &Project) -> (Project, String, Vec<String>, Vec<String>) {
    let (xml, out_notes) = mspdi::to_xml(p);
    let (mut q, in_notes) = mspdi::from_xml(&xml, "fallback").unwrap_or_else(|e| panic!("{e}\n{xml}"));
    recalculate(&mut q);
    (q, xml, out_notes, in_notes)
}

#[test]
fn a_plan_survives_a_round_trip_through_mspdi() {
    let p = rich_plan();
    let (q, xml, out_notes, in_notes) = round_trip(&p);
    assert!(xml.starts_with("<?xml"), "{xml}");
    assert!(in_notes.is_empty(), "nothing is lost reading our own export: {in_notes:?}");
    let _ = out_notes;

    assert_eq!(q.name, p.name);
    assert_eq!((q.author.as_str(), q.manager.as_str(), q.company.as_str()), (p.author.as_str(), p.manager.as_str(), p.company.as_str()));
    assert_eq!(q.start, p.start);
    assert_eq!(q.status_date, p.status_date);
    assert_eq!(q.settings.currency, "€");
    assert_eq!(q.settings.time, p.settings.time);

    // Calendars: every base calendar's week and exceptions.
    assert_eq!(q.calendars.len(), p.calendars.len());
    for (a, b) in p.calendars.iter().zip(&q.calendars) {
        assert_eq!(a.name, b.name);
        assert_eq!(a.week, b.week, "{}", a.name);
        assert_eq!(a.exceptions, b.exceptions, "{}", a.name);
    }
    assert_eq!(q.base_calendar().name, p.base_calendar().name);

    // Tasks.
    assert_eq!(q.tasks.len(), p.tasks.len());
    for (a, b) in p.tasks.iter().zip(&q.tasks) {
        let n = &a.name;
        assert_eq!(a.id, b.id, "{n}");
        assert_eq!(a.name, b.name);
        assert_eq!(a.outline_level, b.outline_level, "{n}");
        assert_eq!(a.kind, b.kind, "{n}");
        assert_eq!(a.mode, b.mode, "{n}");
        assert_eq!(a.active, b.active, "{n}");
        if !a.sched.is_summary {
            assert_eq!(a.duration, b.duration, "{n}");
        }
        assert_eq!(a.estimated, b.estimated, "{n}");
        assert_eq!(a.task_type, b.task_type, "{n}");
        assert_eq!(a.effort_driven, b.effort_driven, "{n}");
        assert_eq!(a.constraint, b.constraint, "{n}");
        assert_eq!(a.deadline, b.deadline, "{n}");
        assert_eq!(a.priority, b.priority, "{n}");
        assert_eq!(a.percent_complete, b.percent_complete, "{n}");
        assert_eq!(a.actual_start, b.actual_start, "{n}");
        assert_eq!(a.actual_finish, b.actual_finish, "{n}");
        assert_eq!(a.notes, b.notes, "{n}");
        assert_eq!(a.fixed_cost, b.fixed_cost, "{n}");
        assert_eq!(a.leveling_delay, b.leveling_delay, "{n}");
        assert_eq!(a.calendar, b.calendar, "{n}");
        if a.members.is_empty() {
            assert_eq!(a.predecessors, b.predecessors, "{n}");
        } else {
            // Members come back as finish-to-start links.
            let linked: Vec<TaskRef> = b.predecessors.iter().map(|d| d.predecessor).collect();
            assert!(a.members.iter().all(|m| linked.contains(m)), "{n}");
        }
        assert_eq!(a.interim, b.interim, "{n}");
        let strip = |b: &Option<Baseline>| b.map(|b| (b.start, b.finish, b.duration, b.work, b.cost));
        assert_eq!(strip(&a.baseline), strip(&b.baseline), "{n}");
        assert_eq!(a.baselines.keys().collect::<Vec<_>>(), b.baselines.keys().collect::<Vec<_>>(), "{n}");
        // Dates come out the same when the plan is scheduled again.
        assert_eq!((a.sched.start, a.sched.finish), (b.sched.start, b.sched.finish), "{n}");
    }

    // Custom fields keep their names, kinds, pick-lists and values.
    let fields = |p: &Project| p.custom_fields.iter().map(|f| (f.name.clone(), f.kind, f.choices.clone())).collect::<Vec<_>>();
    let mut fa = fields(&p);
    let mut fb = fields(&q);
    fa.sort_by(|x, y| x.0.cmp(&y.0));
    fb.sort_by(|x, y| x.0.cmp(&y.0));
    assert_eq!(fa, fb);
    for (a, b) in p.tasks.iter().zip(&q.tasks) {
        let named = |p: &Project, t: &Task| {
            let mut v: Vec<(String, CustomValue)> =
                t.custom.iter().map(|(id, v)| (p.custom_field(*id).unwrap().name.clone(), v.clone())).collect();
            v.sort_by(|x, y| x.0.cmp(&y.0));
            v
        };
        assert_eq!(named(&p, a), named(&q, b), "{}", a.name);
    }

    // Resources and assignments.
    assert_eq!(q.resources.len(), p.resources.len());
    for (a, b) in p.resources.iter().zip(&q.resources) {
        let n = &a.name;
        assert_eq!((a.id, &a.name, a.kind), (b.id, &b.name, b.kind));
        assert_eq!(
            (a.initials.as_str(), a.group.as_str(), a.email.as_str(), a.material_label.as_str()),
            (b.initials.as_str(), b.group.as_str(), b.email.as_str(), b.material_label.as_str()),
            "{n}"
        );
        assert_eq!((a.max_units, a.standard_rate, a.cost_per_use), (b.max_units, b.standard_rate, b.cost_per_use), "{n}");
        if a.kind == ResourceType::Work {
            assert_eq!(a.overtime_rate, b.overtime_rate, "{n}");
            assert_eq!(a.exceptions, b.exceptions, "{n}");
            assert_eq!(a.calendar, b.calendar, "{n}");
        }
        assert_eq!(a.availability, b.availability, "{n}");
    }
    assert_eq!(q.assignments.len(), p.assignments.len());
    for (a, b) in p.assignments.iter().zip(&q.assignments) {
        assert_eq!((a.task, a.resource, a.units, a.work, a.contour, a.cost), (b.task, b.resource, b.units, b.work, b.contour, b.cost));
    }
    assert_eq!(q.sched.finish, p.sched.finish);
    assert_eq!(q.sched.cost, p.sched.cost);
}

#[test]
fn exporting_says_what_mspdi_cannot_carry() {
    let mut p = rich_plan();
    let id = p.tasks[1].id;
    p.task_mut(id).unwrap().estimate = Some(Estimate::Complexity { points: 3.0 });
    let (_, notes) = mspdi::to_xml(&p);
    assert!(notes.iter().any(|n| n.contains("estimates")), "{notes:?}");
}

#[test]
fn files_are_saved_and_loaded() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("plan.xml");
    let p = rich_plan();
    mspdi::save(&p, &path).unwrap();
    let (q, notes) = mspdi::load(&path).unwrap();
    assert!(notes.is_empty(), "{notes:?}");
    assert_eq!(q.tasks.len(), p.tasks.len());
    assert!(mspdi::is_mspdi_path(&path));
    assert!(!mspdi::is_mspdi_path(&dir.path().join("plan.plancraft")));

    // UTF-16 with a byte order mark.
    let (xml, _) = mspdi::to_xml(&p);
    let mut bytes = vec![0xFF, 0xFE];
    bytes.extend(xml.encode_utf16().flat_map(u16::to_le_bytes));
    std::fs::write(&path, bytes).unwrap();
    assert_eq!(mspdi::load(&path).unwrap().0.tasks.len(), p.tasks.len());

    // Not XML, not a Project file, a directory: an error, never a crash.
    std::fs::write(&path, b"\xff\xfe\x00").unwrap();
    assert!(mspdi::load(&path).is_err());
    std::fs::write(&path, "<html><body/></html>").unwrap();
    let e = mspdi::load(&path).unwrap_err().to_string();
    assert!(e.contains("not a Microsoft Project XML file"), "{e}");
    assert!(mspdi::load(dir.path()).is_err());
}

/// A minimal hand-written file: the project summary task and blank rows are not tasks, UIDs that
/// clash get fresh ids, links to missing tasks are dropped, outline jumps are evened out.
#[test]
fn hand_written_files_are_read_leniently() {
    let xml = r#"<?xml version="1.0"?>
<Project xmlns="http://schemas.microsoft.com/project">
  <Name>Hand.xml</Name>
  <StartDate>2026-10-19T08:00:00</StartDate>
  <Tasks>
    <Task><UID>0</UID><ID>0</ID><Name>Hand</Name><OutlineLevel>0</OutlineLevel></Task>
    <Task><UID>5</UID><ID>1</ID><Name>First</Name><OutlineLevel>1</OutlineLevel><Duration>PT16H0M0S</Duration></Task>
    <Task><UID>7</UID><ID>2</ID><IsNull>1</IsNull></Task>
    <Task><UID>5</UID><ID>3</ID><Name>Same UID</Name><OutlineLevel>4</OutlineLevel><Duration>PT8H0M0S</Duration>
      <PredecessorLink><PredecessorUID>5</PredecessorUID><Type>1</Type></PredecessorLink>
      <PredecessorLink><PredecessorUID>99</PredecessorUID><Type>1</Type></PredecessorLink>
    </Task>
    <Task><UID>-3</UID><Name>Bad UID</Name><OutlineLevel>1</OutlineLevel><Duration>garbage</Duration><Milestone>1</Milestone></Task>
  </Tasks>
</Project>"#;
    let (mut p, notes) = mspdi::from_xml(xml, "x").unwrap();
    recalculate(&mut p);
    assert_eq!(p.name, "Hand.xml");
    let names: Vec<&str> = p.tasks.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names, ["First", "Same UID", "Bad UID"]);
    assert_eq!(p.tasks[0].id, TaskId(5));
    assert_ne!(p.tasks[1].id, TaskId(5), "a repeated UID gets a fresh id");
    assert_eq!(p.tasks[1].outline_level, 2, "an outline jump of three levels becomes one");
    assert_eq!(p.tasks[1].predecessors.len(), 1);
    assert_eq!(p.tasks[1].predecessors[0].predecessor, TaskRef::Local(TaskId(5)));
    assert!(p.tasks[2].is_milestone());
    assert!(notes.iter().any(|n| n.contains("blank")), "{notes:?}");
    assert!(notes.iter().any(|n| n.contains("not in the file")), "{notes:?}");
}

#[test]
fn recurring_calendar_exceptions_are_expanded() {
    let xml = r#"<Project><StartDate>2026-01-05T08:00:00</StartDate><CalendarUID>1</CalendarUID><Calendars>
      <Calendar><UID>1</UID><Name>Std</Name><IsBaseCalendar>1</IsBaseCalendar><Exceptions>
        <Exception><EnteredByOccurrences>1</EnteredByOccurrences><TimePeriod><FromDate>2026-01-07T00:00:00</FromDate><ToDate>2026-12-31T23:59:00</ToDate></TimePeriod>
          <Occurrences>3</Occurrences><Name>Wednesdays</Name><Type>6</Type><Period>2</Period><DaysOfWeek>8</DaysOfWeek><DayWorking>0</DayWorking></Exception>
        <Exception><EnteredByOccurrences>0</EnteredByOccurrences><TimePeriod><FromDate>2026-01-01T00:00:00</FromDate><ToDate>2026-04-30T23:59:00</ToDate></TimePeriod>
          <Name>Last Friday</Name><Type>5</Type><Period>1</Period><MonthItem>8</MonthItem><MonthPosition>4</MonthPosition><DayWorking>0</DayWorking></Exception>
        <Exception><EnteredByOccurrences>1</EnteredByOccurrences><TimePeriod><FromDate>2026-12-25T00:00:00</FromDate><ToDate>2125-12-25T23:59:00</ToDate></TimePeriod>
          <Occurrences>100</Occurrences><Name>Christmas</Name><Type>2</Type><Period>1</Period><Month>11</Month><MonthDay>25</MonthDay><DayWorking>0</DayWorking></Exception>
      </Exceptions></Calendar></Calendars></Project>"#;
    let (p, notes) = mspdi::from_xml(xml, "x").unwrap();
    assert!(notes.is_empty(), "{notes:?}");
    let ex = &p.base_calendar().exceptions;
    let days = |name: &str| ex.iter().filter(|e| e.name == name).map(|e| (e.start, e.end, e.yearly)).collect::<Vec<_>>();
    // Every other week on Wednesday, three times.
    assert_eq!(
        days("Wednesdays"),
        [(d(2026, 1, 7), d(2026, 1, 7), false), (d(2026, 1, 21), d(2026, 1, 21), false), (d(2026, 2, 4), d(2026, 2, 4), false)]
    );
    assert_eq!(
        days("Last Friday"),
        [
            (d(2026, 1, 30), d(2026, 1, 30), false),
            (d(2026, 2, 27), d(2026, 2, 27), false),
            (d(2026, 3, 27), d(2026, 3, 27), false),
            (d(2026, 4, 24), d(2026, 4, 24), false)
        ]
    );
    assert_eq!(days("Christmas"), [(d(2026, 12, 25), d(2026, 12, 25), true)]);
}

#[test]
fn resource_calendars_become_personal_exceptions_or_a_calendar_of_their_own() {
    let xml = r#"<Project><StartDate>2026-10-19T08:00:00</StartDate><CalendarUID>1</CalendarUID><Calendars>
      <Calendar><UID>1</UID><Name>Std</Name><IsBaseCalendar>1</IsBaseCalendar></Calendar>
      <Calendar><UID>2</UID><Name>Ana</Name><IsBaseCalendar>0</IsBaseCalendar><BaseCalendarUID>1</BaseCalendarUID>
        <Exceptions><Exception><TimePeriod><FromDate>2026-10-20T00:00:00</FromDate><ToDate>2026-10-21T23:59:00</ToDate></TimePeriod><Name>Leave</Name><Type>1</Type><Period>1</Period><DayWorking>0</DayWorking></Exception></Exceptions></Calendar>
      <Calendar><UID>3</UID><Name>Ben</Name><IsBaseCalendar>0</IsBaseCalendar><BaseCalendarUID>1</BaseCalendarUID>
        <WeekDays><WeekDay><DayType>6</DayType><DayWorking>0</DayWorking></WeekDay></WeekDays></Calendar>
    </Calendars><Resources>
      <Resource><UID>0</UID><ID>0</ID><Type>1</Type></Resource>
      <Resource><UID>1</UID><Name>Ana</Name><Type>1</Type><CalendarUID>2</CalendarUID></Resource>
      <Resource><UID>2</UID><Name>Ben</Name><Type>1</Type><CalendarUID>3</CalendarUID></Resource>
    </Resources></Project>"#;
    let (p, notes) = mspdi::from_xml(xml, "x").unwrap();
    assert!(notes.is_empty(), "{notes:?}");
    assert_eq!(p.resources.len(), 2, "the nameless placeholder resource 0 is left out");
    let ana = &p.resources[0];
    assert_eq!(ana.calendar, None, "Ana works the project calendar");
    assert_eq!(ana.exceptions, vec![CalendarException::non_working("Leave", d(2026, 10, 20), d(2026, 10, 21))]);
    let ben = &p.resources[1];
    let own = p.calendar(ben.calendar.expect("Ben has a calendar of their own")).unwrap();
    assert!(own.week.friday.is_empty());
    assert_eq!(own.week.monday, p.base_calendar().week.monday);
}
