use super::*;
use crate::dates::ymd;
use crate::ids::TaskId;
use crate::limits;
use crate::sample::demo_project;
use crate::schedule::recalculate;

fn plan() -> Project {
    demo_project(ymd(2026, 1, 5).unwrap())
}

/// The `n`th task that is not a summary.
fn leaf(p: &Project, n: usize) -> TaskId {
    p.tasks.iter().filter(|t| !t.sched.is_summary).nth(n).unwrap().id
}

fn task(p: &Project, id: TaskId) -> &crate::model::Task {
    p.tasks.iter().find(|t| t.id == id).unwrap()
}

fn rename(p: &mut Project, id: TaskId, name: &str) {
    p.tasks.iter_mut().find(|t| t.id == id).unwrap().name = name.into();
}

/// An edit as `Document::edit` makes it: checked and recalculated.
fn local_edit(r: &mut Replica, f: impl FnOnce(&mut Project)) {
    let mut p = r.local().clone();
    f(&mut p);
    limits::check_edit(&mut p).unwrap();
    recalculate(&mut p);
    r.edited(p).unwrap();
}

fn join(host: &mut Host) -> Replica {
    let ids = host.lease().unwrap();
    Replica::new(host.project().clone(), host.version(), ids)
}

/// Sends `r`'s change; an accepted one goes to every replica in `all` (the author included).
fn send(host: &mut Host, r: &mut Replica, author: &str) -> Outcome {
    let change = r.next_change("edit").expect("something to send");
    let outcome = host.submit(&change, author);
    if let Outcome::Applied(e) = &outcome {
        r.receive(e).unwrap();
    }
    outcome
}

fn entry(o: &Outcome) -> &Entry {
    match o {
        Outcome::Applied(e) => e,
        other => panic!("not applied: {other:?}"),
    }
}

/// Same plan, schedules and id counters aside.
fn same(a: &Project, b: &Project) -> bool {
    diff(a, b).unwrap().is_empty()
}

#[test]
fn applying_the_difference_gives_back_the_changed_plan() {
    let before = plan();
    let mut after = before.clone();
    let a = leaf(&after, 1);
    let b = leaf(&after, 4);
    after.set_duration(a, 3 * 480, false).unwrap();
    rename(&mut after, b, "Renamed");
    let new = after.add_task("New one", Some(2));
    after.move_tasks_before(&[b], Some(leaf(&after, 0))).unwrap();
    after.delete_tasks(&[leaf(&after, 6)]).unwrap();
    after.name = "Other name".into();
    after.status_date = None;
    recalculate(&mut after);

    let patch = diff(&before, &after).unwrap();
    assert!(patch.lists.get(&Collection::Tasks).is_some_and(|lp| lp.moved && lp.added.len() == 1));
    let rebuilt = apply(&before, &patch).unwrap();
    assert!(same(&rebuilt, &after));
    assert_eq!(rebuilt.tasks.iter().map(|t| t.id).collect::<Vec<_>>(), after.tasks.iter().map(|t| t.id).collect::<Vec<_>>());
    assert!(rebuilt.tasks.iter().any(|t| t.id == new));
    // The wire form reads back the same.
    let wire = serde_json::to_string(&patch).unwrap();
    assert_eq!(serde_json::from_str::<Patch>(&wire).unwrap(), patch);
    assert!(diff(&after, &after).unwrap().is_empty());
}

#[test]
fn changes_to_different_fields_of_one_task_both_stand() {
    let mut host = Host::new(plan());
    let (mut ana, mut ben) = (join(&mut host), join(&mut host));
    let t = leaf(host.project(), 2);
    local_edit(&mut ana, |p| p.set_duration(t, 5 * 480, false).unwrap());
    local_edit(&mut ben, |p| rename(p, t, "Pour the slab"));

    let first = send(&mut host, &mut ana, "Ana");
    let merged = ben.receive(entry(&first)).unwrap();
    assert!(merged.conflicts.is_empty() && merged.discarded.is_none());
    let second = send(&mut host, &mut ben, "Ben");
    ana.receive(entry(&second)).unwrap();

    for p in [host.project(), ana.local(), ben.local()] {
        assert_eq!(task(p, t).duration, 5 * 480);
        assert_eq!(task(p, t).name, "Pour the slab");
    }
    assert!(same(ana.local(), host.project()) && same(ben.local(), host.project()));
    assert!(ana.pending().is_empty() && ben.pending().is_empty());
    assert_eq!(host.version(), 3);
}

#[test]
fn the_same_field_changed_twice_keeps_the_first_and_reports_the_second() {
    let mut host = Host::new(plan());
    let (mut ana, mut ben) = (join(&mut host), join(&mut host));
    let t = leaf(host.project(), 2);
    let other = leaf(host.project(), 3);
    local_edit(&mut ana, |p| p.set_duration(t, 5 * 480, false).unwrap());
    local_edit(&mut ben, |p| {
        p.set_duration(t, 2 * 480, false).unwrap();
        rename(p, other, "Ben's rename");
    });

    let first = send(&mut host, &mut ana, "Ana");
    // Ben's change went out before Ana's arrived: the host refuses it as a whole.
    let change = ben.next_change("edit").unwrap();
    let Outcome::Conflicts(conflicts) = host.submit(&change, "Ben") else { panic!("expected a conflict") };
    assert_eq!(conflicts.len(), 1);
    assert_eq!(conflicts[0].kind, ConflictKind::BothChanged);
    assert_eq!(conflicts[0].field.as_deref(), Some("duration"));
    let text = conflicts[0].describe(host.project());
    assert!(text.starts_with("Ana changed the duration of task '"), "{text}");

    // Ben's PlanCraft drops the clashing part when Ana's change comes in, keeps the rest and
    // sends it again.
    let merged = ben.receive(entry(&first)).unwrap();
    assert_eq!(merged.conflicts, conflicts);
    assert!(ben.rejected(change.id, None).discarded.is_none());
    assert_eq!(task(ben.local(), t).duration, 5 * 480);
    let again = send(&mut host, &mut ben, "Ben");
    ana.receive(entry(&again)).unwrap();
    assert_eq!(task(host.project(), other).name, "Ben's rename");
    assert!(same(ana.local(), host.project()) && same(ben.local(), host.project()));
}

#[test]
fn a_task_changed_by_one_and_deleted_by_the_other_is_a_conflict() {
    let mut host = Host::new(plan());
    let (mut ana, mut ben) = (join(&mut host), join(&mut host));
    let t = leaf(host.project(), 5);
    local_edit(&mut ana, |p| {
        p.delete_tasks(&[t]).unwrap();
    });
    local_edit(&mut ben, |p| rename(p, t, "Still needed"));
    let first = send(&mut host, &mut ana, "Ana");
    let merged = ben.receive(entry(&first)).unwrap();
    assert_eq!(merged.conflicts.len(), 1);
    assert_eq!(merged.conflicts[0].kind, ConflictKind::ChangedDeleted);
    assert!(merged.conflicts[0].describe(host.project()).starts_with("Ana deleted task "));
    assert!(!ben.local().tasks.iter().any(|x| x.id == t));
    assert!(ben.pending().is_empty());
}

#[test]
fn tasks_added_at_the_same_time_both_stay_where_they_were_put() {
    let mut host = Host::new(plan());
    let (mut ana, mut ben) = (join(&mut host), join(&mut host));
    let after_a = host.project().tasks[1].id;
    let after_b = host.project().tasks[6].id;
    let mut a_new = TaskId(0);
    let mut b_new = TaskId(0);
    local_edit(&mut ana, |p| a_new = p.add_task("Ana's task", Some(2)));
    local_edit(&mut ben, |p| b_new = p.add_task("Ben's task", Some(7)));
    assert_ne!(a_new, b_new, "leases keep new ids apart");

    let first = send(&mut host, &mut ana, "Ana");
    ben.receive(entry(&first)).unwrap();
    let second = send(&mut host, &mut ben, "Ben");
    ana.receive(entry(&second)).unwrap();

    let order: Vec<TaskId> = host.project().tasks.iter().map(|t| t.id).collect();
    let at = |id| order.iter().position(|x| *x == id).unwrap();
    assert_eq!(at(a_new), at(after_a) + 1);
    assert_eq!(at(b_new), at(after_b) + 1);
    for r in [&ana, &ben] {
        assert_eq!(r.local().tasks.iter().map(|t| t.id).collect::<Vec<_>>(), order);
    }
}

#[test]
fn both_reordering_tasks_is_a_conflict_but_new_tasks_still_go_in() {
    let mut host = Host::new(plan());
    let (mut ana, mut ben) = (join(&mut host), join(&mut host));
    let (x, y) = (leaf(host.project(), 1), leaf(host.project(), 6));
    let first_task = host.project().tasks[0].id;
    local_edit(&mut ana, |p| p.move_tasks_before(&[x], None).unwrap());
    let mut added = TaskId(0);
    local_edit(&mut ben, |p| {
        p.move_tasks_before(&[y], Some(first_task)).unwrap();
        added = p.add_task("Ben's task", Some(3));
    });
    let first = send(&mut host, &mut ana, "Ana");
    let merged = ben.receive(entry(&first)).unwrap();
    assert_eq!(merged.conflicts.iter().map(|c| c.kind).collect::<Vec<_>>(), vec![ConflictKind::BothMoved]);
    let second = send(&mut host, &mut ben, "Ben");
    ana.receive(entry(&second)).unwrap();
    let p = host.project();
    assert_eq!(p.tasks.last().unwrap().id, x, "Ana's move stands");
    assert!(p.tasks.iter().any(|t| t.id == added), "Ben's new task went in");
    assert!(same(ana.local(), p) && same(ben.local(), p));
}

#[test]
fn a_change_sent_twice_is_applied_once() {
    let mut host = Host::new(plan());
    let mut ana = join(&mut host);
    let t = leaf(host.project(), 0);
    local_edit(&mut ana, |p| rename(p, t, "Once"));
    let change = ana.next_change("Rename").unwrap();
    let first = host.submit(&change, "Ana");
    assert_eq!(entry(&first).version, 2);
    assert_eq!(host.submit(&change, "Ana"), Outcome::AlreadyApplied(2));
    assert_eq!(host.version(), 2);
}

#[test]
fn damaged_or_forbidden_changes_are_refused_and_leave_the_plan_alone() {
    let mut host = Host::new(plan());
    let before = host.project().clone();
    let t = leaf(&before, 0);
    let key = Key(t.0.to_string());
    let field = |name: &str, v: serde_json::Value| {
        let mut lp = ListPatch::default();
        lp.changed.insert(key.clone(), [(name.to_string(), Slot::Set(v))].into());
        Patch { lists: [(Collection::Tasks, lp)].into(), ..Patch::default() }
    };
    let mut patches = vec![
        Patch { fields: [("sched".to_string(), Slot::Unset)].into(), ..Patch::default() },
        Patch { fields: [("ids".to_string(), Slot::Set(serde_json::json!({})))].into(), ..Patch::default() },
        Patch { fields: [("id".to_string(), Slot::Unset)].into(), ..Patch::default() },
        Patch { fields: [("start".to_string(), Slot::Set(serde_json::json!("not a date")))].into(), ..Patch::default() },
        Patch { fields: [("tasks".to_string(), Slot::Set(serde_json::json!(7)))].into(), ..Patch::default() },
        field("id", serde_json::json!(999_999)),
        field("sched", serde_json::json!({})),
        field("duration", serde_json::json!("long")),
        field("duration", serde_json::json!(i64::MAX)),
        field("percent_complete", serde_json::json!(250)),
        field("outline_level", serde_json::json!(-1)),
    ];
    // An added task under another task's key, one whose key is taken, and a change to a task
    // that does not exist.
    let mut wrong_key = ListPatch::default();
    wrong_key.added.insert(Key("77777".into()), serde_json::to_value(task(&before, t)).unwrap());
    patches.push(Patch { lists: [(Collection::Tasks, wrong_key)].into(), ..Patch::default() });
    let mut taken = ListPatch::default();
    taken.added.insert(key.clone(), serde_json::to_value(task(&before, t)).unwrap());
    patches.push(Patch { lists: [(Collection::Tasks, taken)].into(), ..Patch::default() });
    let mut missing = ListPatch::default();
    missing.changed.insert(Key("88888".into()), [("name".to_string(), Slot::Set("x".into()))].into());
    patches.push(Patch { lists: [(Collection::Tasks, missing)].into(), ..Patch::default() });

    for patch in patches {
        let change = Change { id: ChangeId::new(), base: 1, label: "junk".into(), patch: patch.clone() };
        match host.submit(&change, "Mallory") {
            Outcome::Refused(_) => {}
            other => panic!("{patch:?} was not refused: {other:?}"),
        }
        assert_eq!(host.version(), 1);
        assert!(same(host.project(), &before));
    }
    // An unknown or forgotten base asks for a resync.
    let change = Change { id: ChangeId::new(), base: 9, label: String::new(), patch: Patch::default() };
    assert_eq!(host.submit(&change, "Mallory"), Outcome::Resync);
}

#[test]
fn work_done_while_disconnected_is_kept_after_a_resync() {
    let mut host = Host::new(plan());
    let (mut ana, mut ben) = (join(&mut host), join(&mut host));
    let (a, b) = (leaf(host.project(), 1), leaf(host.project(), 2));
    // Ana makes several changes the host confirms; Ben, offline, misses them all.
    for n in 0..3 {
        local_edit(&mut ana, |p| rename(p, a, &format!("Ana {n}")));
        send(&mut host, &mut ana, "Ana");
    }
    local_edit(&mut ben, |p| rename(p, b, "Ben offline"));
    local_edit(&mut ben, |p| rename(p, a, "Ben's name for it"));
    let merged = ben.resync(host.project().clone(), host.version()).unwrap();
    assert_eq!(merged.conflicts.len(), 1, "Ana renamed that task first");
    let done = send(&mut host, &mut ben, "Ben");
    ana.receive(entry(&done)).unwrap();
    assert_eq!(task(host.project(), a).name, "Ana 2");
    assert_eq!(task(host.project(), b).name, "Ben offline");
    assert!(same(ana.local(), host.project()) && same(ben.local(), host.project()));
}

#[test]
fn a_replica_that_misses_an_entry_is_told_to_resync() {
    let mut host = Host::new(plan());
    let (mut ana, mut ben) = (join(&mut host), join(&mut host));
    let t = leaf(host.project(), 0);
    local_edit(&mut ana, |p| rename(p, t, "One"));
    send(&mut host, &mut ana, "Ana");
    local_edit(&mut ana, |p| rename(p, t, "Two"));
    let second = send(&mut host, &mut ana, "Ana");
    assert_eq!(ben.receive(entry(&second)), Err(SyncError::Gap));
    let missed = host.since(ben.version()).unwrap();
    for e in &missed {
        ben.receive(e).unwrap();
    }
    assert_eq!(task(ben.local(), t).name, "Two");
}

#[test]
fn the_host_replays_its_entries_to_the_same_plan() {
    let start = plan();
    let mut host = Host::new(start.clone());
    let mut ana = join(&mut host);
    let t = leaf(host.project(), 3);
    let mut entries = Vec::new();
    local_edit(&mut ana, |p| p.set_duration(t, 4 * 480, true).unwrap());
    entries.push(entry(&send(&mut host, &mut ana, "Ana")).clone());
    local_edit(&mut ana, |p| {
        p.add_task("Late addition", None);
    });
    entries.push(entry(&send(&mut host, &mut ana, "Ana")).clone());

    let mut restarted = Host::new(start);
    for e in &entries {
        restarted.replay(e).unwrap();
    }
    assert_eq!(restarted.version(), host.version());
    assert!(same(restarted.project(), host.project()));
    assert!(restarted.replay(&entries[0]).is_err(), "out of order");
}

#[test]
fn leases_never_overlap_and_run_out_cleanly() {
    let mut host = Host::new(plan());
    let a = host.lease().unwrap();
    let b = host.lease().unwrap();
    assert!(b.task >= a.task + LEASE_SIZE);
    assert!(host.project().ids.task >= b.task + LEASE_SIZE);
    let mut p = plan();
    p.ids.task = limits::MAX_ID - 10;
    let mut full = Host::new(p);
    assert_eq!(full.lease(), None);
}

#[test]
fn lists_that_cannot_be_matched_by_key_are_replaced_whole() {
    let mut before = plan();
    before.tables = vec![
        crate::custom::TableLayout { name: "Entry".into(), columns: vec!["Name".into()] },
        crate::custom::TableLayout { name: "Entry".into(), columns: vec!["Start".into()] },
    ];
    let mut after = before.clone();
    after.tables[1].columns.push("Finish".into());
    let patch = diff(&before, &after).unwrap();
    assert!(patch.fields.contains_key("tables") && !patch.lists.contains_key(&Collection::Tables));
    assert_eq!(apply(&before, &patch).unwrap().tables, after.tables);
}

#[test]
fn work_that_no_longer_fits_is_dropped_piece_by_piece() {
    let mut host = Host::new(plan());
    let (mut ana, mut ben) = (join(&mut host), join(&mut host));
    let t = leaf(host.project(), 5);
    let other = leaf(host.project(), 1);
    let resource = host.project().resources[0].id;
    local_edit(&mut ana, |p| {
        p.delete_tasks(&[t]).unwrap();
    });
    local_edit(&mut ben, |p| {
        p.assign(t, resource, Some(1.0), None).unwrap();
        rename(p, other, "Kept");
    });
    let first = send(&mut host, &mut ana, "Ana");
    // Ben's change went out before Ana's arrived: the host refuses it (an assignment to a
    // deleted task), and Ben's PlanCraft keeps the rename when Ana's change comes in.
    let change = ben.next_change("edit").unwrap();
    assert!(matches!(host.submit(&change, "Ben"), Outcome::Refused(_)));
    let merged = ben.receive(entry(&first)).unwrap();
    assert!(merged.discarded.is_none(), "{merged:?}");
    assert!(merged.conflicts.iter().any(|c| c.kind == ConflictKind::NoLongerFits && c.list == Some(Collection::Assignments)));
    assert!(ben.rejected(change.id, Some("refused")).discarded.is_none());
    let again = send(&mut host, &mut ben, "Ben");
    ana.receive(entry(&again)).unwrap();
    assert_eq!(task(host.project(), other).name, "Kept");
    assert!(!host.project().assignments.iter().any(|a| a.task == t));
    assert!(same(ana.local(), host.project()) && same(ben.local(), host.project()));
}

#[test]
fn an_order_naming_no_items_keeps_every_item_and_stays_fast() {
    // A hostile change: an empty order for a big list, then one naming every key backwards.
    let mut p = crate::model::Project::new("Big", ymd(2026, 1, 5).unwrap());
    for i in 0..20_000 {
        p.add_task(format!("Task {i}"), None);
    }
    let ids: Vec<TaskId> = p.tasks.iter().map(|t| t.id).collect();
    let started = std::time::Instant::now();
    let order = |keys: Vec<Key>| Patch {
        lists: [(Collection::Tasks, ListPatch { order: Some(keys), moved: true, ..ListPatch::default() })].into(),
        ..Patch::default()
    };
    let same_order = apply(&p, &order(Vec::new())).unwrap();
    assert_eq!(same_order.tasks.iter().map(|t| t.id).collect::<Vec<_>>(), ids);
    let backwards: Vec<Key> = ids.iter().rev().map(|id| Key(id.0.to_string())).collect();
    let reversed = apply(&p, &order(backwards)).unwrap();
    assert_eq!(reversed.tasks.iter().map(|t| t.id).rev().collect::<Vec<_>>(), ids);
    assert!(started.elapsed() < std::time::Duration::from_secs(20), "took {:?}", started.elapsed());
}
