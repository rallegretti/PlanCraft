//! A shared plan kept safe on disk: crashes at every step of a save, outside changes, damaged
//! journals, reconnections and a failing disk.

use super::*;
use crate::protocol::{CatchUp, ServerMsg};
use plancraft_core::dates::ymd;
use plancraft_core::limits;
use plancraft_core::sample::demo_project;
use plancraft_core::schedule::recalculate;
use plancraft_core::sync::Replica;
use std::sync::mpsc::{Receiver, sync_channel};

fn setup() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("Plan.plancraft");
    io::save(&demo_project(ymd(2026, 1, 5).unwrap()), &path).unwrap();
    (dir, path)
}

fn open(path: &Path) -> SharedPlan {
    SharedPlan::open(path, "Plan.plancraft", crate::quiet()).unwrap()
}

static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

fn outbox(n: usize) -> (Outbox, Receiver<Out>) {
    outbox_in(n, Budget::standard())
}

fn outbox_in(n: usize, budget: Arc<Budget>) -> (Outbox, Receiver<Out>) {
    let (tx, rx) = sync_channel(n);
    (Outbox::new(NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst), tx, budget, None), rx)
}

fn messages(rx: &Receiver<Out>) -> Vec<ServerMsg> {
    rx.try_iter()
        .filter_map(|o| match o {
            Out::Frame(b) => Some(serde_json::from_slice(b.bytes()).unwrap()),
            Out::Close => None,
        })
        .collect()
}

/// Lets an editor in; returns their replica (from what the server sent) and their queue.
fn join(plan: &mut SharedPlan, conn: ConnId, client: ClientId) -> (Replica, Receiver<Out>) {
    let (out, rx) = outbox(64);
    plan.join(conn, Subscriber { user: format!("user{conn}"), client, role: Role::Editor, out, task: None }, None, None).unwrap();
    let opened = messages(&rx)
        .into_iter()
        .find_map(|m| match m {
            ServerMsg::Opened(o) => Some(*o),
            _ => None,
        })
        .unwrap();
    let CatchUp::Snapshot { snapshot } = opened.catch_up else { panic!("a first opening gets the whole plan") };
    let (mut project, _) = io::from_json_with_notes(&snapshot.plan, Path::new("x")).unwrap();
    recalculate(&mut project);
    (Replica::new(project, snapshot.version, opened.lease.unwrap()), rx)
}

/// Adds a task in the replica and sends it; returns the change.
fn add_task(plan: &mut SharedPlan, conn: ConnId, r: &mut Replica, rx: &Receiver<Out>, name: &str) -> Change {
    let mut p = r.local().clone();
    p.add_task(name, None);
    limits::check_edit(&mut p).unwrap();
    recalculate(&mut p);
    r.edited(p).unwrap();
    let change = r.next_change("Add Task").unwrap();
    plan.submit(conn, &change).unwrap();
    for m in messages(rx) {
        if let ServerMsg::Entry { entry } = m {
            r.receive(&entry).unwrap();
        }
    }
    change
}

fn has_task(p: &Project, name: &str) -> bool {
    p.tasks.iter().any(|t| t.name == name)
}

#[test]
fn accepted_changes_and_leases_survive_a_crash() {
    let (_dir, path) = setup();
    let mut plan = open(&path);
    let client = ClientId::new();
    let (mut r, rx) = join(&mut plan, 1, client);
    let change = add_task(&mut plan, 1, &mut r, &rx, "Survives");
    let version = plan.host().version();
    let epoch = plan.epoch();
    let lease_counter = plan.host().project().ids.task;
    drop(plan); // a crash: nothing saved
    assert!(!has_task(&io::load(&path).unwrap(), "Survives"), "the file was not saved");

    let mut plan = open(&path);
    assert_eq!(plan.host().version(), version);
    assert_eq!(plan.epoch(), epoch, "the same run of the plan, so replicas catch up");
    assert!(has_task(plan.host().project(), "Survives"));
    assert!(plan.host().was_applied(change.id), "a lost reply can still be told apart");
    assert!(plan.host().project().ids.task >= lease_counter, "the lease handed out is not handed out again");
    assert!(has_task(&io::load(&path).unwrap(), "Survives"), "recovery saves the plan file");
    // The old replica carries on with ids from its lease.
    let (out, _rx2) = outbox(64);
    plan.join(2, Subscriber { user: "user1".into(), client, role: Role::Editor, out, task: None }, Some(Known { epoch, version }), None)
        .unwrap();
    let mut p = r.local().clone();
    p.add_task("After the crash", None);
    recalculate(&mut p);
    r.edited(p).unwrap();
    let change = r.next_change("Add Task").unwrap();
    plan.submit(2, &change).unwrap();
    assert!(has_task(plan.host().project(), "After the crash"), "an id from the old lease is accepted");
}

#[test]
fn a_crash_after_the_plan_file_was_written_does_not_apply_changes_twice() {
    let (_dir, path) = setup();
    let mut plan = open(&path);
    let (mut r, rx) = join(&mut plan, 1, ClientId::new());
    add_task(&mut plan, 1, &mut r, &rx, "Once");
    add_task(&mut plan, 1, &mut r, &rx, "Twice?");
    let version = plan.host().version();
    // The first two steps of a save, then the power fails.
    let text = io::to_json(plan.host().project()).unwrap();
    plan.journal.append(&Record::Saving { version, plan_hash: plan_hash(text.as_bytes()), to: None }).unwrap();
    io::write_atomic(&path, text.as_bytes(), Privacy::KeepExisting).unwrap();
    drop(plan);

    let plan = open(&path);
    assert_eq!(plan.host().version(), version);
    assert_eq!(plan.host().project().tasks.iter().filter(|t| t.name == "Once").count(), 1);
    assert!(has_task(plan.host().project(), "Twice?"));
}

#[test]
fn a_crash_before_the_plan_file_was_written_replays_the_journal() {
    let (_dir, path) = setup();
    let mut plan = open(&path);
    let (mut r, rx) = join(&mut plan, 1, ClientId::new());
    add_task(&mut plan, 1, &mut r, &rx, "Replayed");
    let version = plan.host().version();
    let text = io::to_json(plan.host().project()).unwrap();
    plan.journal.append(&Record::Saving { version, plan_hash: plan_hash(text.as_bytes()), to: None }).unwrap();
    drop(plan);

    let plan = open(&path);
    assert_eq!(plan.host().version(), version);
    assert!(has_task(plan.host().project(), "Replayed"));
}

#[test]
fn a_clean_save_and_close_leaves_a_plain_plan_file() {
    let (dir, path) = setup();
    let mut plan = open(&path);
    let (mut r, rx) = join(&mut plan, 1, ClientId::new());
    add_task(&mut plan, 1, &mut r, &rx, "Saved");
    let epoch = plan.epoch();
    let version = plan.host().version();
    plan.close("bye");
    assert!(has_task(&io::load(&path).unwrap(), "Saved"));
    assert!(!lock::lock_path(&path).unwrap().exists(), "the lock is given back");
    let back = journal::read(&journal::path_for(&path).unwrap()).unwrap();
    assert!(matches!(back.records.as_slice(), [Record::Header { .. }]), "the journal holds only the header");

    let plan = open(&path);
    assert_eq!((plan.epoch(), plan.host().version()), (epoch, version), "reopening continues the same run");
    drop(dir);
}

#[test]
fn a_plan_changed_outside_the_server_starts_afresh() {
    let (dir, path) = setup();
    let mut plan = open(&path);
    let (mut r, rx) = join(&mut plan, 1, ClientId::new());
    add_task(&mut plan, 1, &mut r, &rx, "Shared");
    let epoch = plan.epoch();
    plan.close("bye");
    // Someone edits the file in PlanCraft while it is not shared.
    let mut p = io::load(&path).unwrap();
    p.add_task("Edited alone", None);
    io::save(&p, &path).unwrap();

    let plan = open(&path);
    assert_ne!(plan.epoch(), epoch, "versions from before must not match versions after");
    assert_eq!(plan.host().version(), 1);
    assert!(has_task(plan.host().project(), "Edited alone"));
    assert!(hidden_files(&path, "unused").is_empty(), "nothing unsaved was lost, so nothing is kept aside");
    drop(dir);
}

#[test]
fn unsaved_shared_changes_are_kept_aside_when_the_file_changed_outside() {
    let (dir, path) = setup();
    let mut plan = open(&path);
    let (mut r, rx) = join(&mut plan, 1, ClientId::new());
    add_task(&mut plan, 1, &mut r, &rx, "Only in the journal");
    drop(plan); // crash
    lock::release(&path);
    let mut p = io::load(&path).unwrap();
    p.add_task("Edited alone", None);
    io::save(&p, &path).unwrap();

    let plan = open(&path);
    assert!(has_task(plan.host().project(), "Edited alone"));
    assert!(!has_task(plan.host().project(), "Only in the journal"));
    let kept = hidden_files(&path, "unused");
    assert_eq!(kept.len(), 1);
    assert!(std::fs::read_to_string(&kept[0]).unwrap().contains("Only in the journal"));
    drop(dir);
}

#[test]
fn a_file_changed_outside_while_shared_is_not_overwritten() {
    let (_dir, path) = setup();
    let mut plan = open(&path);
    let (mut r, rx) = join(&mut plan, 1, ClientId::new());
    add_task(&mut plan, 1, &mut r, &rx, "Shared work");
    let mut outside = io::load(&path).unwrap();
    outside.name = "Saved by someone who ignored the warning, with a longer name".into();
    io::save(&outside, &path).unwrap();
    plan.save().unwrap();
    assert_eq!(io::load(&path).unwrap().name, outside.name, "their file is left alone");
    let copy = hidden_files(&path, "shared copy").pop().expect("a copy");
    assert!(has_task(&io::load(&copy).unwrap(), "Shared work"), "the shared plan goes to a copy");
    assert!(copy.file_name().unwrap().to_string_lossy().starts_with('.'), "hidden: not a plan to share, with other access rules");
}

/// Regression: after the shared plan was saved to a hidden copy, opening the plan again (a
/// restart, or everyone left) started afresh from the outside file, silently: every shared change
/// stayed in the copy, mentioned once in an old log line.
#[test]
fn a_plan_whose_shared_work_went_to_a_copy_is_not_reopened_from_the_outside_file() {
    let (_dir, path) = setup();
    let mut plan = open(&path);
    let (mut r, rx) = join(&mut plan, 1, ClientId::new());
    add_task(&mut plan, 1, &mut r, &rx, "Shared work");
    let mut outside = io::load(&path).unwrap();
    outside.name = "Changed outside".into();
    io::save(&outside, &path).unwrap();
    plan.save().unwrap();
    plan.close("bye");
    let e = SharedPlan::open(&path, "Plan.plancraft", crate::quiet()).err().expect("refused until a person picks a version");
    assert!(e.contains("shared copy") && e.contains("Keep the version you want"), "{e}");
    // Kept as the plan: it opens, with the shared work.
    let copy = hidden_files(&path, "shared copy").pop().unwrap();
    std::fs::rename(&copy, &path).unwrap();
    let again = open(&path);
    assert!(has_task(again.host.project(), "Shared work"));
}

/// Regression: a crash during the first save to the hidden copy (after the copy was written,
/// before the journal's new header) reopened the plan from the outside file, silently.
#[test]
fn a_crash_while_first_saving_to_the_copy_still_refuses_the_outside_file() {
    let (_dir, path) = setup();
    let mut plan = open(&path);
    let (mut r, rx) = join(&mut plan, 1, ClientId::new());
    add_task(&mut plan, 1, &mut r, &rx, "Shared work");
    let mut outside = io::load(&path).unwrap();
    outside.name = "Changed outside".into();
    io::save(&outside, &path).unwrap();
    // The steps of that save, up to the power failure: the record, then the copy.
    let text = io::to_json(plan.host().project()).unwrap();
    let copy = path.with_file_name(".Plan.plancraft shared copy 2026-01-01 000000.plancraft");
    let to = Some(copy.file_name().unwrap().to_string_lossy().into_owned());
    plan.journal.append(&Record::Saving { version: plan.host().version(), plan_hash: plan_hash(text.as_bytes()), to }).unwrap();
    io::write_atomic(&copy, text.as_bytes(), Privacy::OwnerOnly).unwrap();
    drop(plan);
    let e = SharedPlan::open(&path, "Plan.plancraft", crate::quiet()).err().expect("refused until a person picks a version");
    assert!(e.contains("shared copy") && !e.contains(&*path.parent().unwrap().to_string_lossy()), "names only: {e}");
    // Moving the copy away keeps the outside version.
    std::fs::remove_file(&copy).unwrap();
    let plan = open(&path);
    assert_eq!(plan.host().project().name, "Changed outside");
}

fn hidden_files(near: &Path, what: &str) -> Vec<PathBuf> {
    std::fs::read_dir(near.parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| {
            let n = p.file_name().unwrap().to_string_lossy().into_owned();
            n.starts_with('.') && n.contains(what)
        })
        .collect()
}

#[cfg(unix)]
#[test]
fn a_plan_file_replaced_by_a_link_is_not_written_through() {
    let (dir, path) = setup();
    let mut plan = open(&path);
    let (mut r, rx) = join(&mut plan, 1, ClientId::new());
    add_task(&mut plan, 1, &mut r, &rx, "Shared work");
    let target = dir.path().join("elsewhere.json");
    std::fs::write(&target, "precious").unwrap();
    std::fs::remove_file(&path).unwrap();
    std::os::unix::fs::symlink(&target, &path).unwrap();
    plan.save().unwrap();
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "precious");
    assert_eq!(hidden_files(&path, "shared copy").len(), 1);
}

#[cfg(target_os = "linux")]
#[test]
fn a_journal_that_cannot_take_the_save_record_breaks_the_plan() {
    // A save record cut short, with changes written after it, would hide those changes from
    // recovery: a failed journal write must end all writing.
    let (_dir, path) = setup();
    let mut plan = open(&path);
    let (mut r, rx) = join(&mut plan, 1, ClientId::new());
    add_task(&mut plan, 1, &mut r, &rx, "Dirty");
    let jpath = plan.journal.path().to_path_buf();
    plan.journal = Journal::on(&jpath, OpenOptions::new().append(true).open("/dev/full").unwrap());
    assert!(matches!(plan.save(), Err(SaveError::Broken(_))));
    plan.dirty = true;
    plan.last_change = Instant::now() - SAVE_AT_LEAST;
    assert!(plan.maintain(Instant::now()).is_err(), "upkeep reports it, so the server closes the plan");
}

#[test]
fn a_damaged_journal_starts_a_new_epoch_that_sticks() {
    let (_dir, path) = setup();
    let mut plan = open(&path);
    let (mut r, rx) = join(&mut plan, 1, ClientId::new());
    for name in ["c1", "c2", "c3"] {
        add_task(&mut plan, 1, &mut r, &rx, name);
    }
    let epoch = plan.epoch();
    let jpath = plan.journal.path().to_path_buf();
    drop(plan); // crash
    // The line of c2 is damaged.
    let text = std::fs::read_to_string(&jpath).unwrap();
    let damaged: Vec<String> = text.lines().map(|l| if l.contains("\"c2\"") { "garbage".to_string() } else { l.to_string() }).collect();
    std::fs::write(&jpath, damaged.join("\n") + "\n").unwrap();
    let plan = open(&path);
    assert!(has_task(plan.host().project(), "c1"));
    assert!(!has_task(plan.host().project(), "c3"), "nothing after the damage");
    let new = plan.epoch();
    assert_ne!(new, epoch, "copies that saw c2 and c3 must load the plan afresh, not catch up by version");
    drop(plan);
    let plan = open(&path);
    assert_eq!(plan.epoch(), new, "the new epoch was saved");
}

#[test]
fn a_journal_whose_header_is_unreadable_is_kept_aside() {
    let (_dir, path) = setup();
    let mut plan = open(&path);
    let (mut r, rx) = join(&mut plan, 1, ClientId::new());
    add_task(&mut plan, 1, &mut r, &rx, "Only in the journal");
    let jpath = plan.journal.path().to_path_buf();
    drop(plan); // crash
    let text = std::fs::read_to_string(&jpath).unwrap();
    let mut lines: Vec<&str> = text.lines().collect();
    lines[0] = "{\"record\":\"header\",\"epo";
    std::fs::write(&jpath, lines.join("\n") + "\n").unwrap();
    let logged = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let l = Arc::clone(&logged);
    let plan = SharedPlan::open(&path, "Plan.plancraft", Arc::new(move |s: &str| l.lock().unwrap().push(s.to_string()))).unwrap();
    assert!(!has_task(plan.host().project(), "Only in the journal"));
    let kept = hidden_files(&path, "unused");
    assert_eq!(kept.len(), 1, "the journal is kept for a person to look at");
    assert!(std::fs::read_to_string(&kept[0]).unwrap().contains("Only in the journal"));
    assert!(logged.lock().unwrap().iter().any(|l| l.contains("damaged")), "{:?}", logged.lock().unwrap());
}

#[test]
fn too_many_missed_changes_send_the_whole_plan() {
    let (_dir, path) = setup();
    let mut plan = open(&path);
    let client = ClientId::new();
    let (mut r, rx) = join(&mut plan, 1, client);
    let known = Known { epoch: plan.epoch(), version: plan.host().version() };
    for i in 0..5 {
        add_task(&mut plan, 1, &mut r, &rx, &format!("T{i}"));
    }
    plan.catch_up_limit = 100;
    let (out, rx2) = outbox(64);
    plan.join(2, Subscriber { user: "user1".into(), client, role: Role::Editor, out, task: None }, Some(known), None).unwrap();
    let opened = messages(&rx2).into_iter().find_map(|m| if let ServerMsg::Opened(o) = m { Some(o) } else { None }).unwrap();
    assert!(matches!(opened.catch_up, CatchUp::Snapshot { .. }), "too large to send as changes");
}

#[test]
fn a_change_that_would_make_the_plan_too_large_is_refused() {
    let (_dir, path) = setup();
    let mut plan = open(&path);
    let (mut r, rx) = join(&mut plan, 1, ClientId::new());
    plan.size_limit = plan.size + 2_000;
    let before = plan.host().version();
    let mut p = r.local().clone();
    for i in 0..20 {
        p.add_task(format!("Big {i} {}", "x".repeat(200)), None);
    }
    recalculate(&mut p);
    r.edited(p).unwrap();
    let change = r.next_change("Paste").unwrap();
    plan.submit(1, &change).unwrap();
    assert_eq!(plan.host().version(), before, "not applied");
    assert!(messages(&rx).iter().any(|m| matches!(m, ServerMsg::Rejected { reason: Some(why), .. } if why.contains("grow beyond"))));
    // A small one still fits.
    r.rejected(change.id, None);
    let mut p = r.confirmed().clone();
    p.ids = r.ids().clone();
    let mut r2 = Replica::new(plan.host().project().clone(), plan.host().version(), r.ids().clone());
    p.add_task("Small", None);
    recalculate(&mut p);
    r2.edited(p).unwrap();
    plan.submit(1, &r2.next_change("Add").unwrap()).unwrap();
    assert!(has_task(plan.host().project(), "Small"));
}

#[test]
fn leases_are_rationed() {
    let (_dir, path) = setup();
    let mut plan = open(&path);
    let (_r, rx) = join(&mut plan, 1, ClientId::new());
    for _ in 0..40 {
        plan.lease_for(1).unwrap();
    }
    let leased = messages(&rx).iter().filter(|m| matches!(m, ServerMsg::Leased { .. })).count();
    assert!(leased < 20, "{leased} leases in a row");
    assert!(leased >= 15);
}

#[test]
fn a_copy_cannot_be_taken_over_by_someone_else() {
    let (_dir, path) = setup();
    let mut plan = open(&path);
    let client = ClientId::new();
    let (_r, rx1) = join(&mut plan, 1, client);
    let (out, rx2) = outbox(64);
    let joined = plan.join(2, Subscriber { user: "mallory".into(), client, role: Role::Editor, out, task: None }, None, None).unwrap();
    assert_eq!(joined, Joined::No);
    assert!(messages(&rx2).iter().any(|m| matches!(m, ServerMsg::Error { .. })));
    assert!(!rx1.try_iter().any(|o| matches!(o, Out::Close)), "the owner's connection stays");
    assert_eq!(plan.people_count(), 1);
}

#[test]
fn a_torn_journal_tail_is_ignored_and_a_damaged_journal_is_kept_aside() {
    let (dir, path) = setup();
    let mut plan = open(&path);
    let (mut r, rx) = join(&mut plan, 1, ClientId::new());
    add_task(&mut plan, 1, &mut r, &rx, "Before the tear");
    let jpath = plan.journal.path().to_path_buf();
    drop(plan);
    let mut f = OpenOptions::new().append(true).open(&jpath).unwrap();
    std::io::Write::write_all(&mut f, b"{\"record\":\"entry\",\"ent").unwrap();
    drop(f);
    let plan = open(&path);
    assert!(has_task(plan.host().project(), "Before the tear"));
    drop(plan);

    // Garbage in the middle: the readable part is applied, the journal kept for a person.
    let mut plan = open(&path);
    let (mut r, rx) = join(&mut plan, 1, ClientId::new());
    add_task(&mut plan, 1, &mut r, &rx, "Readable");
    drop(plan);
    let mut text = std::fs::read_to_string(&jpath).unwrap();
    text.push_str("not json\n{\"record\":\"saving\",\"version\":1,\"plan_hash\":\"x\"}\n");
    std::fs::write(&jpath, text).unwrap();
    let plan = open(&path);
    assert!(has_task(plan.host().project(), "Readable"));
    assert_eq!(hidden_files(&path, "unused").len(), 1);
    drop(dir);
}

use std::fs::OpenOptions;

#[test]
fn a_plan_open_in_plancraft_is_not_shared() {
    let (_dir, path) = setup();
    let owner = lock::Owner {
        user: "ana".into(),
        host: "studio-pc".into(),
        pid: 4242,
        instance: "other".into(),
        opened: "2026-11-18 09:15".into(),
        app: "PlanCraft 0.6.0".into(),
    };
    std::fs::write(lock::lock_path(&path).unwrap(), serde_json::to_vec(&owner).unwrap()).unwrap();
    let err = SharedPlan::open(&path, "Plan.plancraft", crate::quiet()).err().unwrap();
    assert!(err.contains("ana") && err.contains("studio-pc"), "{err}");
    assert!(lock::lock_path(&path).unwrap().exists(), "their lock is left alone");
}

#[test]
fn a_reconnection_replaces_the_old_connection_and_its_late_requests_are_dropped() {
    let (_dir, path) = setup();
    let mut plan = open(&path);
    let client = ClientId::new();
    let (mut r, rx1) = join(&mut plan, 1, client);
    // The connection drops with a change on its way; the copy reconnects before the old
    // connection's request is handled.
    let mut p = r.local().clone();
    p.add_task("Late", None);
    recalculate(&mut p);
    r.edited(p).unwrap();
    let change = r.next_change("Add Task").unwrap();
    let (out, rx2) = outbox(64);
    let known = Known { epoch: plan.epoch(), version: plan.host().version() };
    plan.join(2, Subscriber { user: "user1".into(), client, role: Role::Editor, out, task: None }, Some(known), Some(change.id)).unwrap();
    assert!(rx1.try_iter().any(|o| matches!(o, Out::Close)), "the old connection is closed");
    let opened = messages(&rx2)
        .into_iter()
        .find_map(|m| match m {
            ServerMsg::Opened(o) => Some(o),
            _ => None,
        })
        .unwrap();
    assert!(!opened.in_flight_applied);
    assert!(matches!(&opened.catch_up, CatchUp::Entries { entries } if entries.is_empty()), "only what it missed");
    plan.submit(1, &change).unwrap();
    assert!(!has_task(plan.host().project(), "Late"), "the old connection's late request is dropped");
    plan.submit(2, &change).unwrap();
    assert!(has_task(plan.host().project(), "Late"));
    // Sent once more after another lost reply: the server says it went in.
    let (out, rx3) = outbox(64);
    plan.join(3, Subscriber { user: "user1".into(), client, role: Role::Editor, out, task: None }, Some(known), Some(change.id)).unwrap();
    let opened = messages(&rx3).into_iter().find_map(|m| if let ServerMsg::Opened(o) = m { Some(o) } else { None }).unwrap();
    assert!(opened.in_flight_applied);
    assert!(matches!(&opened.catch_up, CatchUp::Entries { entries } if entries.len() == 1));
}

#[test]
fn a_slow_connection_is_dropped_rather_than_holding_everyone_up() {
    let (_dir, path) = setup();
    let mut plan = open(&path);
    let (mut r, rx) = join(&mut plan, 1, ClientId::new());
    let (out, slow) = outbox(2);
    plan.join(2, Subscriber { user: "slow".into(), client: ClientId::new(), role: Role::Viewer, out, task: None }, None, None).unwrap();
    for i in 0..4 {
        add_task(&mut plan, 1, &mut r, &rx, &format!("T{i}"));
    }
    assert_eq!(plan.people_count(), 1, "the slow one was let go");
    drop(slow);
}

#[cfg(target_os = "linux")]
#[test]
fn a_journal_that_cannot_be_written_confirms_nothing() {
    let (_dir, path) = setup();
    let mut plan = open(&path);
    let (mut r, rx) = join(&mut plan, 1, ClientId::new());
    let jpath = plan.journal.path().to_path_buf();
    // A disk that is full.
    plan.journal = Journal::on(&jpath, OpenOptions::new().append(true).open("/dev/full").unwrap());
    let mut p = r.local().clone();
    p.add_task("Never confirmed", None);
    recalculate(&mut p);
    r.edited(p).unwrap();
    let change = r.next_change("Add Task").unwrap();
    assert!(plan.submit(1, &change).is_err());
    assert!(!messages(&rx).iter().any(|m| matches!(m, ServerMsg::Entry { .. })), "nobody hears of it");
    plan.abandon(&Broken("disk full".into()));
    let plan = open(&path);
    assert!(!has_task(plan.host().project(), "Never confirmed"));
}

#[test]
fn viewers_get_no_ids_and_people_see_who_is_where() {
    let (_dir, path) = setup();
    let mut plan = open(&path);
    let (_r, rx) = join(&mut plan, 1, ClientId::new());
    let (out, rx2) = outbox(64);
    plan.join(2, Subscriber { user: "viewer".into(), client: ClientId::new(), role: Role::Viewer, out, task: None }, None, None).unwrap();
    let opened = messages(&rx2).into_iter().find_map(|m| if let ServerMsg::Opened(o) = m { Some(o) } else { None }).unwrap();
    assert!(opened.lease.is_none());
    assert_eq!(opened.people.len(), 2);
    let task = plan.host().project().tasks.first().unwrap().id;
    let other = plan.host().project().tasks.get(1).unwrap().id;
    let _ = messages(&rx);
    // Places go out from upkeep, once however often they changed meanwhile (a person flipping
    // between tasks must not flood the others' queues).
    for _ in 0..50 {
        plan.presence(2, Some(other));
        plan.presence(2, Some(task));
    }
    assert!(messages(&rx).is_empty(), "not once per message");
    plan.maintain(Instant::now()).unwrap();
    let seen = messages(&rx);
    assert_eq!(seen.iter().filter(|m| matches!(m, ServerMsg::People { .. })).count(), 1);
    assert!(
        seen.iter().any(|m| matches!(m, ServerMsg::People { people } if people.iter().any(|p| p.user == "viewer" && p.task == Some(task))))
    );
    // The whole plan, asked for again and again: at most once per gap, the rest from upkeep.
    let _ = messages(&rx2);
    for _ in 0..20 {
        plan.resync(2, false).unwrap();
    }
    assert_eq!(messages(&rx2).iter().filter(|m| matches!(m, ServerMsg::Snapshot { .. })).count(), 1);
    plan.maintain(Instant::now() + MIN_SNAPSHOT_GAP).unwrap();
    assert_eq!(messages(&rx2).iter().filter(|m| matches!(m, ServerMsg::Snapshot { .. })).count(), 1, "the one held back");
    plan.leave(2);
    assert!(messages(&rx).iter().any(|m| matches!(m, ServerMsg::People { people } if people.len() == 1)));
}

#[test]
fn the_size_estimate_covers_what_changes_add_to_the_saved_plan() {
    // Saved plans are larger than the changes that make them (pretty-printed, with schedules):
    // the limit must hold for the plan as saved, whatever the estimate says.
    let (_dir, path) = setup();
    let mut plan = open(&path);
    let (mut r, rx) = join(&mut plan, 1, ClientId::new());
    let start = io::to_json(plan.host().project()).unwrap().len();
    // Room for the plan to grow: the estimate decides alone until the plan nears an eighth of it.
    plan.size_limit = start * 64;
    let mut refused = false;
    let mut unmeasured = 0;
    for round in 0..30 {
        if plan.size.saturating_mul(8) < plan.size_limit {
            unmeasured += 1;
        }
        let before = io::to_json(plan.host().project()).unwrap().len();
        let mut p = r.local().clone();
        for i in 0..20 {
            p.add_task(format!("Round {round} task {i}"), None);
        }
        recalculate(&mut p);
        r.edited(p).unwrap();
        let change = r.next_change("Paste").unwrap();
        plan.submit(1, &change).unwrap();
        let mut applied = false;
        for m in messages(&rx) {
            match m {
                ServerMsg::Entry { entry } => {
                    r.receive(&entry).unwrap();
                    applied = true;
                    // The estimate's assumption: a plan as saved grows by at most GROWTH times
                    // the change's size.
                    let grown = io::to_json(plan.host().project()).unwrap().len().saturating_sub(before);
                    let patch = serde_json::to_vec(&entry.patch).unwrap().len();
                    assert!(grown <= GROWTH * patch, "grew {grown} bytes from a {patch}-byte change");
                }
                ServerMsg::Rejected { reason: Some(_), .. } => refused = true,
                _ => {}
            }
        }
        let saved = io::to_json(plan.host().project()).unwrap().len();
        assert!(saved <= plan.size_limit, "round {round}: {saved} bytes, the limit is {}", plan.size_limit);
        if !applied {
            break;
        }
    }
    assert!(!refused, "there was room for all of them");
    assert!(unmeasured > 2, "the estimate alone let some changes in ({unmeasured})");
}

#[test]
fn a_full_budget_closes_the_connection_holding_the_most() {
    let budget = Budget::new(1_000_000, 10_000);
    let (hog, hog_rx) = outbox_in(4096, Arc::clone(&budget));
    let (other, other_rx) = outbox_in(4096, Arc::clone(&budget));
    // Someone who never reads holds nearly everything.
    for _ in 0..9 {
        assert!(hog.push_frame(vec![b' '; 1_000]));
    }
    // A message for someone else does not fit: the hog is closed, not the one it is for.
    assert!(other.push_frame(vec![b' '; 2_000]), "the message to someone else goes out");
    assert!(other_rx.try_iter().any(|o| matches!(o, Out::Frame(_))));
    assert!(hog_rx.try_iter().any(|o| matches!(o, Out::Close)), "the connection holding the most was closed");
    // The one that would hold the most is closed even when it is the one sending.
    let (big, big_rx) = outbox_in(4096, Arc::clone(&budget));
    assert!(!big.push_frame(vec![b' '; 20_000]));
    assert!(big_rx.try_iter().any(|o| matches!(o, Out::Close)));
}

#[test]
fn queued_bytes_are_counted_back_however_frames_end() {
    let budget = Budget::new(1_000_000, 10_000_000);
    let (out, rx) = outbox_in(16, Arc::clone(&budget));
    for _ in 0..10 {
        out.push_frame(vec![0; 1_000]);
    }
    assert_eq!(budget.total(), 10_000);
    let first = rx.recv().unwrap();
    drop(first); // sent
    assert_eq!(budget.total(), 9_000);
    drop(rx); // the connection ended with frames still queued
    assert_eq!(budget.total(), 0);
    assert!(!out.push_frame(vec![0; 1_000]), "nobody is listening");
    assert_eq!(budget.total(), 0, "a frame that could not be queued is not counted");
}

#[cfg(target_os = "linux")]
#[test]
fn after_one_failed_journal_write_nothing_more_is_written() {
    let (dir, path) = setup();
    let mut plan = open(&path);
    let (mut r, rx) = join(&mut plan, 1, ClientId::new());
    let jpath = plan.journal.path().to_path_buf();
    plan.journal = Journal::on(&jpath, OpenOptions::new().append(true).open("/dev/full").unwrap());
    let mut p = r.local().clone();
    p.add_task("Lost", None);
    recalculate(&mut p);
    r.edited(p).unwrap();
    let change = r.next_change("Add").unwrap();
    assert!(plan.submit(1, &change).is_err());
    // The disk recovers before the plan is closed: still nothing more may be written.
    let spare = dir.path().join("spare");
    plan.journal = Journal::start(&spare, &Record::Lease { start: IdCounters::default() }).unwrap();
    let size = std::fs::metadata(&spare).unwrap().len();
    assert!(plan.submit(1, &change).is_err());
    assert!(plan.lease_for(1).is_err());
    assert!(plan.save().is_err());
    assert!(plan.maintain(Instant::now()).is_err());
    assert_eq!(std::fs::metadata(&spare).unwrap().len(), size, "nothing was written after the failure");
    assert!(!messages(&rx).iter().any(|m| matches!(m, ServerMsg::Entry { .. })));
}

#[test]
fn a_large_journal_is_saved_into_the_plan_at_once() {
    let (_dir, path) = setup();
    let mut plan = open(&path);
    let (mut r, rx) = join(&mut plan, 1, ClientId::new());
    plan.journal_save_bytes = 1;
    add_task(&mut plan, 1, &mut r, &rx, "Saved straight away");
    assert!(!plan.dirty);
    assert!(has_task(&io::load(&path).unwrap(), "Saved straight away"));
}

#[test]
fn the_lease_ration_survives_reopening_the_plan() {
    let (_dir, path) = setup();
    let allowances = Allowances::default();
    let mut plan = SharedPlan::open_with(&path, "Plan.plancraft", crate::quiet(), Arc::clone(&allowances)).unwrap();
    let (_r, _rx) = join(&mut plan, 1, ClientId::new());
    for _ in 0..40 {
        plan.lease_for(1).unwrap();
    }
    plan.close("bye");
    let mut plan = SharedPlan::open_with(&path, "Plan.plancraft", crate::quiet(), allowances).unwrap();
    let (out, rx) = outbox(64);
    plan.join(1, Subscriber { user: "user1".into(), client: ClientId::new(), role: Role::Editor, out, task: None }, None, None).unwrap();
    let opened = messages(&rx).into_iter().find_map(|m| if let ServerMsg::Opened(o) = m { Some(o) } else { None }).unwrap();
    assert!(opened.lease.is_none(), "no fresh allowance from leaving and coming back");
}

#[test]
fn people_who_lose_access_are_disconnected() {
    let (_dir, path) = setup();
    let mut plan = open(&path);
    let (_r, rx1) = join(&mut plan, 1, ClientId::new());
    let (_r2, rx2) = join(&mut plan, 2, ClientId::new());
    plan.revoke(|user| if user == "user2" { Role::None } else { Role::Viewer });
    assert_eq!(plan.people_count(), 1);
    let gone: Vec<Out> = rx2.try_iter().collect();
    assert!(gone.iter().any(|o| matches!(o, Out::Close)));
    assert!(gone.iter().any(|o| matches!(o, Out::Frame(f) if String::from_utf8_lossy(f.bytes()).contains("closing"))));
    assert!(messages(&rx1).iter().any(|m| matches!(m, ServerMsg::People { people } if people.len() == 1)));
    assert_eq!(plan.subscribers.values().next().unwrap().role, Role::Viewer, "a changed role takes effect");
}

#[test]
fn a_broadcast_is_counted_once_however_many_receive_it() {
    let budget = Budget::new(10_000_000, 3_000_000);
    let (_dir, path) = setup();
    let mut plan = open(&path);
    let mut queues = Vec::new();
    for conn in 1..=10 {
        let (tx, rx) = sync_channel(64);
        let out = Outbox::new(1000 + conn, tx, Arc::clone(&budget), None);
        plan.join(conn, Subscriber { user: format!("u{conn}"), client: ClientId::new(), role: Role::Viewer, out, task: None }, None, None)
            .unwrap();
        queues.push(rx);
    }
    // A message of 1 MB to ten people: 1 MB in memory, not 10.
    plan.broadcast(&ServerMsg::Error { message: "x".repeat(1_000_000), for_good: false });
    assert_eq!(plan.people_count(), 10, "nobody was dropped");
    assert!(budget.total() < 2_000_000 + 10 * 100_000, "{}", budget.total());
}

#[test]
fn hostile_minimal_items_stay_within_the_estimate_margin() {
    // Tasks with nothing but an id grow the saved plan the most per byte of change; the plan is
    // measured exactly from a sixteenth of the limit, so even this growth cannot overshoot.
    let (_dir, path) = setup();
    let mut plan = open(&path);
    let (r, rx) = join(&mut plan, 1, ClientId::new());
    let first = r.ids().task + 1;
    let mut added = serde_json::Map::new();
    let mut order: Vec<serde_json::Value> = plan.host().project().tasks.iter().map(|t| serde_json::json!(t.id.0.to_string())).collect();
    for id in first..first + 500 {
        added.insert(id.to_string(), serde_json::json!({ "id": id, "name": "" }));
        order.push(serde_json::json!(id.to_string()));
    }
    let change: Change = serde_json::from_value(serde_json::json!({
        "id": uuid::Uuid::new_v4(), "base": plan.host().version(), "label": "junk",
        "patch": { "lists": { "tasks": { "added": added, "order": order } } }
    }))
    .unwrap();
    let incoming = serde_json::to_vec(&change.patch).unwrap().len();
    let before = io::to_json(plan.host().project()).unwrap().len();
    plan.submit(1, &change).unwrap();
    assert!(messages(&rx).iter().any(|m| matches!(m, ServerMsg::Entry { .. })), "the change was applied");
    // Unmeasured means size + incoming × GROWTH ≤ limit / 16, so incoming ≤ limit / (16 × GROWTH):
    // overshooting the limit would take growing by about 15 × 16 × GROWTH / 16 = 120 times that.
    let grown = io::to_json(plan.host().project()).unwrap().len() - before;
    let margin = 15 * GROWTH;
    assert!(grown < margin * incoming, "grew {grown} bytes from {incoming}");
}

#[test]
fn changes_wait_while_the_journal_cannot_be_saved() {
    let (_dir, path) = setup();
    let mut plan = open(&path);
    let (mut r, rx) = join(&mut plan, 1, ClientId::new());
    plan.max_unsaved_journal = 1;
    let mut p = r.local().clone();
    p.add_task("Waits", None);
    recalculate(&mut p);
    r.edited(p).unwrap();
    plan.submit(1, &r.next_change("Add").unwrap()).unwrap();
    assert!(!has_task(plan.host().project(), "Waits"));
    assert!(
        messages(&rx)
            .iter()
            .any(|m| matches!(m, ServerMsg::Rejected { reason: Some(why), retry: true, .. } if why.contains("cannot save")))
    );
}

#[cfg(unix)]
#[test]
fn a_fifo_in_place_of_a_plan_or_journal_is_refused_at_once() {
    let (dir, path) = setup();
    let jpath = journal::path_for(&path).unwrap();
    let made = std::process::Command::new("mkfifo").arg(&jpath).status();
    if !made.is_ok_and(|s| s.success()) {
        return;
    }
    assert!(SharedPlan::open(&path, "Plan.plancraft", crate::quiet()).is_err(), "a FIFO journal");
    std::fs::remove_file(&jpath).unwrap();
    std::fs::remove_file(&path).unwrap();
    std::process::Command::new("mkfifo").arg(&path).status().unwrap();
    assert!(SharedPlan::open(&path, "Plan.plancraft", crate::quiet()).is_err(), "a FIFO plan");
    drop(dir);
}

#[test]
fn the_size_is_what_the_plan_saves_at_not_the_files() {
    let (_dir, path) = setup();
    // The same plan written compactly: far fewer bytes than it saves at.
    let project = io::load(&path).unwrap();
    let compact = serde_json::to_vec(&serde_json::from_str::<serde_json::Value>(&io::to_json(&project).unwrap()).unwrap()).unwrap();
    std::fs::write(&path, &compact).unwrap();
    let plan = open(&path);
    let saved = io::to_json(plan.host().project()).unwrap().len();
    assert!(saved > compact.len(), "the test needs a compact file");
    assert_eq!(plan.size, saved);
}

#[cfg(unix)]
#[test]
fn after_a_failed_save_changes_do_not_try_again_at_once() {
    use std::os::unix::fs::PermissionsExt;
    let (dir, path) = setup();
    let mut plan = open(&path);
    let (mut r, rx) = join(&mut plan, 1, ClientId::new());
    plan.journal_save_bytes = 1;
    // A folder nobody may write in: every save fails, while the open journal still takes records.
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o555)).unwrap();
    let writable = std::fs::File::create(dir.path().join("probe")).is_ok();
    if !writable {
        for name in ["One", "Two", "Three"] {
            add_task(&mut plan, 1, &mut r, &rx, name);
        }
    }
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
    if writable {
        return; // running as root: the folder cannot be made read-only
    }
    let journal = std::fs::read_to_string(journal::path_for(&path).unwrap()).unwrap();
    assert_eq!(journal.matches("\"record\":\"saving\"").count(), 1, "one try, then the wait: {journal}");
    assert!(plan.dirty);
    assert!(has_task(plan.host().project(), "Three"), "the changes were still accepted");
}
