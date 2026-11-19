#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]
//! A real server over the network: signing in, access, editing together, conflicts, restarts.

mod common;

use std::path::Path;
use std::time::Duration;

use common::*;
use plancraft_core::model::Project;
use plancraft_core::schedule::recalculate;
use plancraft_core::sync::{Replica, Version};
use plancraft_server::client::{Client, ClientError};
use plancraft_server::protocol::{CatchUp, ClientId, ClientMsg, Known, Opened, PROTOCOL, Role, Secret, ServerMsg};
use plancraft_server::wire::{Fingerprint, Trust, WireError};

/// The next message that is not a list of people.
fn next(c: &mut Client) -> ServerMsg {
    loop {
        match c.recv().unwrap() {
            ServerMsg::People { .. } => continue,
            m => return m,
        }
    }
}

fn replica(opened: &Opened) -> Replica {
    let CatchUp::Snapshot { snapshot } = &opened.catch_up else { panic!("expected the whole plan") };
    let (mut p, _) = plancraft_core::io::from_json_with_notes(&snapshot.plan, Path::new("x")).unwrap();
    recalculate(&mut p);
    Replica::new(p, snapshot.version, opened.lease.clone().unwrap_or_default())
}

fn edit(r: &mut Replica, f: impl FnOnce(&mut Project)) {
    let mut p = r.local().clone();
    f(&mut p);
    plancraft_core::limits::check_edit(&mut p).unwrap();
    recalculate(&mut p);
    r.edited(p).unwrap();
}

fn rename_first_task(r: &mut Replica, name: &str) {
    edit(r, |p| p.tasks.first_mut().unwrap().name = name.into());
}

fn first_task_name(r: &Replica) -> String {
    r.local().tasks.first().unwrap().name.clone()
}

#[test]
fn signing_in_and_what_each_person_may_see() {
    let s = setup();
    let server = s.start();
    let mut c = connect(&server);
    match c.login("ana", "wrong password") {
        Err(ClientError::LoginFailed { message: m, credentials, .. }) => assert!(credentials && m.contains("wrong user name or password")),
        other => panic!("{:?}", other.map(|_| ())),
    }
    assert!(c.login("nobody", PASSWORD).is_err());
    assert_eq!(c.login("ANA", PASSWORD).unwrap(), "ana", "names ignore case");

    let names: Vec<String> = c.list().unwrap().into_iter().map(|p| p.name).collect();
    assert_eq!(names, vec!["Plan.plancraft", "Secret.plancraft"], "only plans, sorted");
    let mut eve = signed_in(&server, "eve");
    let plans = eve.list().unwrap();
    assert_eq!(plans.len(), 1, "a plan someone may not open is not listed");
    assert!(matches!(eve.open("Secret.plancraft", ClientId::new(), None, None), Err(ClientError::Refused(_))));
    // "plan.plancraft" is the same file as "Plan.plancraft" where case is ignored (Windows, the
    // Mac): it must never be shared a second time under another spelling.
    for bad in ["../Plan.plancraft", "notes.txt", ".Plan.plancraft.journal", "Missing.plancraft", "", "plan.plancraft", "PLAN.PLANCRAFT"] {
        assert!(matches!(eve.open(bad, ClientId::new(), None, None), Err(ClientError::Refused(_))), "{bad}");
    }
    server.stop();
}

#[test]
fn requests_out_of_turn_are_refused() {
    let s = setup();
    let server = s.start();
    let mut c = Client::connect(server.addr(), &Trust::FirstUse, "test").unwrap();
    assert!(matches!(c.request(&ClientMsg::List).unwrap(), ServerMsg::Error { .. }), "sign in first");
    assert!(matches!(c.request(&ClientMsg::Ping).unwrap(), ServerMsg::Pong));
    c.login("ana", PASSWORD).unwrap();
    assert!(matches!(c.request(&ClientMsg::Lease).unwrap(), ServerMsg::Error { .. }), "open a plan first");
    c.open("Plan.plancraft", ClientId::new(), None, None).unwrap();
    assert!(matches!(next_reply(&mut c, &ClientMsg::List), ServerMsg::Error { .. }), "one plan per connection");
    server.stop();
}

fn next_reply(c: &mut Client, msg: &ClientMsg) -> ServerMsg {
    c.send(msg).unwrap();
    next(c)
}

#[test]
fn another_protocol_is_turned_away() {
    let s = setup();
    let server = s.start();
    let (sender, receiver, _) =
        plancraft_server::wire::connect(std::net::TcpStream::connect(server.addr()).unwrap(), &Trust::FirstUse).unwrap();
    let (mut sender, mut receiver) = (sender, receiver);
    sender.send(&serde_json::to_vec(&ClientMsg::Hello { protocol: PROTOCOL + 1, app: String::new() }).unwrap()).unwrap();
    let reply: ServerMsg = serde_json::from_slice(&receiver.recv(1 << 20).unwrap()).unwrap();
    assert!(matches!(reply, ServerMsg::Error { message, for_good: true } if message.contains("protocol")));
    assert!(receiver.recv(1 << 20).is_err(), "and the connection is closed");
    server.stop();
}

#[test]
fn a_pinned_client_refuses_a_server_with_another_key() {
    let s = setup();
    let server = s.start();
    let wrong = Fingerprint("0000 0000 0000 0000 0000 0000 0000 0000".into());
    match Client::connect(server.addr(), &Trust::Pinned(wrong), "test") {
        Err(ClientError::Wire(WireError::WrongServer { found })) => assert_eq!(&found, server.fingerprint()),
        other => panic!("{:?}", other.map(|_| ())),
    }
    server.stop();
}

#[test]
fn guessing_passwords_is_slowed_down() {
    let s = setup();
    let server = s.start();
    let mut c = connect(&server);
    let mut waits = Vec::new();
    for _ in 0..7 {
        c.send(&ClientMsg::Login { user: "ana".into(), password: Secret("guess guess".into()) }).unwrap();
        match c.recv().unwrap() {
            ServerMsg::LoginFailed { retry_after_secs, .. } => waits.push(retry_after_secs),
            other => panic!("{other:?}"),
        }
    }
    assert_eq!(waits[0], 0);
    assert!(waits[5] > 0, "{waits:?}");
    // Even the right password waits now.
    assert!(
        matches!(c.login("ana", PASSWORD), Err(ClientError::LoginFailed { message: m, credentials: false, retry_after_secs }) if m.contains("try again") && retry_after_secs > 0)
    );
    server.stop();
}

#[test]
fn a_large_frame_before_signing_in_closes_the_connection() {
    let s = setup();
    let server = s.start();
    let mut c = connect(&server);
    let _ = c.send_raw(&vec![b' '; 200_000]); // the server may close before it is all sent
    assert!(c.recv().is_err());
    // The server carries on.
    let mut d = signed_in(&server, "ana");
    assert!(matches!(d.request(&ClientMsg::Ping).unwrap(), ServerMsg::Pong));
    server.stop();
}

#[test]
fn two_people_edit_together_and_clashes_are_reported() {
    let s = setup();
    let server = s.start();
    let mut ana = signed_in(&server, "ana");
    let mut ben = signed_in(&server, "ben");
    let opened_a = ana.open("Plan.plancraft", ClientId::new(), None, None).unwrap();
    let opened_b = ben.open("Plan.plancraft", ClientId::new(), None, None).unwrap();
    assert_eq!(opened_b.people.len(), 2);
    let (mut ra, mut rb) = (replica(&opened_a), replica(&opened_b));

    // Ana renames the first task: both hear of it.
    rename_first_task(&mut ra, "Ana's name");
    ana.send(&ClientMsg::Change { change: ra.next_change("Rename").unwrap() }).unwrap();
    let ServerMsg::Entry { entry } = next(&mut ana) else { panic!() };
    assert_eq!(entry.author, "ana");
    ra.receive(&entry).unwrap();
    let ServerMsg::Entry { entry: seen } = next(&mut ben) else { panic!() };
    assert_eq!(seen, entry);

    // Ben renamed it too before he heard: his change clashes.
    rename_first_task(&mut rb, "Ben's name");
    let change = rb.next_change("Rename").unwrap();
    ben.send(&ClientMsg::Change { change: change.clone() }).unwrap();
    match next(&mut ben) {
        ServerMsg::Rejected { change: id, conflicts, .. } => {
            assert_eq!(id, change.id);
            assert_eq!(conflicts.len(), 1);
            assert_eq!(conflicts[0].by.as_deref(), Some("ana"));
        }
        other => panic!("{other:?}"),
    }
    rb.receive(&seen).unwrap();
    rb.rejected(change.id, None);
    assert_eq!(first_task_name(&rb), "Ana's name");
    assert_eq!(first_task_name(&ra), "Ana's name");

    // Changes to different tasks both go in.
    edit(&mut ra, |p| p.tasks[1].name = "Second, by Ana".into());
    edit(&mut rb, |p| p.tasks[2].name = "Third, by Ben".into());
    ana.send(&ClientMsg::Change { change: ra.next_change("Rename").unwrap() }).unwrap();
    ben.send(&ClientMsg::Change { change: rb.next_change("Rename").unwrap() }).unwrap();
    for _ in 0..2 {
        let ServerMsg::Entry { entry } = next(&mut ana) else { panic!() };
        ra.receive(&entry).unwrap();
        let ServerMsg::Entry { entry } = next(&mut ben) else { panic!() };
        rb.receive(&entry).unwrap();
    }
    assert_eq!(ra.local().tasks[1].name, "Second, by Ana");
    assert_eq!(ra.local().tasks[2].name, "Third, by Ben");
    assert!(plancraft_core::sync::diff(ra.local(), rb.local()).unwrap().is_empty(), "both see the same plan");
    server.stop();
}

#[test]
fn a_reconnecting_copy_closes_its_old_connection() {
    let s = setup();
    let server = s.start();
    let client = ClientId::new();
    let mut old = signed_in(&server, "ana");
    old.open("Plan.plancraft", client, None, None).unwrap();
    let mut new = signed_in(&server, "ana");
    let opened = new.open("Plan.plancraft", client, None, None).unwrap();
    assert_eq!(opened.people.len(), 1, "the old connection no longer counts");
    loop {
        match old.recv() {
            Ok(ServerMsg::People { .. }) => continue,
            Ok(other) => panic!("{other:?}"),
            Err(_) => break,
        }
    }
    assert!(matches!(next_reply(&mut new, &ClientMsg::Ping), ServerMsg::Pong));
    server.stop();
}

#[test]
fn viewers_cannot_change_anything() {
    let s = setup();
    let server = s.start();
    let mut vic = signed_in(&server, "vic");
    let opened = vic.open("Plan.plancraft", ClientId::new(), None, None).unwrap();
    assert_eq!(opened.role, Role::Viewer);
    assert!(opened.lease.is_none());
    let mut r = replica(&opened);
    rename_first_task(&mut r, "Not allowed");
    let reply = next_reply(&mut vic, &ClientMsg::Change { change: r.next_change("Rename").unwrap() });
    assert!(matches!(reply, ServerMsg::Rejected { reason: Some(m), role: Some(Role::Viewer), retry: false, .. } if m.contains("view")));
    assert!(matches!(next_reply(&mut vic, &ClientMsg::Lease), ServerMsg::Error { .. }));
    server.stop();
}

#[test]
fn the_plan_is_saved_and_its_lock_released_when_the_last_person_leaves() {
    let s = setup();
    let server = s.start();
    let path = s.folder.join("Plan.plancraft");
    let mut ana = signed_in(&server, "ana");
    let mut r = replica(&ana.open("Plan.plancraft", ClientId::new(), None, None).unwrap());
    assert!(plancraft_core::lock::lock_path(&path).unwrap().exists(), "the server holds the lock");
    rename_first_task(&mut r, "Saved on leaving");
    ana.send(&ClientMsg::Change { change: r.next_change("Rename").unwrap() }).unwrap();
    assert!(matches!(next(&mut ana), ServerMsg::Entry { .. }));
    drop(ana);
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while plancraft_core::lock::lock_path(&path).unwrap().exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(!plancraft_core::lock::lock_path(&path).unwrap().exists(), "the lock is given back");
    assert_eq!(plancraft_core::io::load(&path).unwrap().tasks[0].name, "Saved on leaving");
    server.stop();
}

#[test]
fn after_a_restart_a_copy_catches_up_with_only_what_it_missed() {
    let s = setup();
    let server = s.start();
    let fingerprint = server.fingerprint().clone();
    let client = ClientId::new();
    let mut ana = signed_in(&server, "ana");
    let mut ben = signed_in(&server, "ben");
    let opened = ana.open("Plan.plancraft", client, None, None).unwrap();
    let CatchUp::Snapshot { snapshot } = &opened.catch_up else { panic!() };
    let (epoch, version): (_, Version) = (snapshot.epoch, snapshot.version);
    let mut rb = replica(&ben.open("Plan.plancraft", ClientId::new(), None, None).unwrap());
    rename_first_task(&mut rb, "While Ana was away");
    ben.send(&ClientMsg::Change { change: rb.next_change("Rename").unwrap() }).unwrap();
    assert!(matches!(next(&mut ben), ServerMsg::Entry { .. }));
    drop(ana);
    drop(ben);
    server.stop();

    let server = s.start();
    assert_eq!(server.fingerprint(), &fingerprint, "the same key after a restart");
    let mut ana = signed_in(&server, "ana");
    let opened = ana.open("Plan.plancraft", client, Some(Known { epoch, version }), None).unwrap();
    // After a restart the server keeps no log of entries, so the whole plan comes, in the same
    // run (epoch) and with Ben's change.
    let CatchUp::Snapshot { snapshot } = &opened.catch_up else { panic!("{:?}", opened.catch_up) };
    assert_eq!(snapshot.epoch, epoch, "the same run of the plan");
    assert!(snapshot.version > version);
    assert!(snapshot.plan.contains("While Ana was away"));
    server.stop();
}

#[test]
fn stopping_the_server_tells_everyone_and_saves() {
    let s = setup();
    let server = s.start();
    let mut ana = signed_in(&server, "ana");
    let mut r = replica(&ana.open("Plan.plancraft", ClientId::new(), None, None).unwrap());
    rename_first_task(&mut r, "Saved on stop");
    ana.send(&ClientMsg::Change { change: r.next_change("Rename").unwrap() }).unwrap();
    assert!(matches!(next(&mut ana), ServerMsg::Entry { .. }));
    server.stop();
    assert!(matches!(next(&mut ana), ServerMsg::Closing { .. }));
    let path = s.folder.join("Plan.plancraft");
    assert_eq!(plancraft_core::io::load(&path).unwrap().tasks[0].name, "Saved on stop");
    assert!(!plancraft_core::lock::lock_path(&path).unwrap().exists());
}

#[test]
fn nobody_can_sign_in_until_a_user_is_added() {
    let dir = tempfile::tempdir().unwrap();
    let err = plancraft_server::server::start(plancraft_server::server::Options::new(
        dir.path().to_path_buf(),
        dir.path().join("config"),
        "127.0.0.1:0".parse().unwrap(),
        plancraft_server::quiet(),
    ))
    .err()
    .unwrap();
    assert!(err.contains("user add"), "{err}");
}

/// Sends `msg` over and over from another connection that never reads what comes back.
fn flood(server: &plancraft_server::server::Running, user: &str, msg: ClientMsg) -> std::thread::JoinHandle<()> {
    let mut c = signed_in(server, user);
    c.open("Plan.plancraft", ClientId::new(), None, None).unwrap();
    let bytes = serde_json::to_vec(&msg).unwrap();
    std::thread::spawn(move || {
        let started = std::time::Instant::now();
        while started.elapsed() < Duration::from_secs(20) {
            if c.send_raw(&bytes).is_err() {
                return;
            }
        }
    })
}

/// Ana's change goes in and comes back quickly while someone floods the server.
fn still_answers_quickly(server: &plancraft_server::server::Running) {
    let mut ana = signed_in(server, "ana");
    let mut r = replica(&ana.open("Plan.plancraft", ClientId::new(), None, None).unwrap());
    rename_first_task(&mut r, "Still answered");
    let started = std::time::Instant::now();
    ana.send(&ClientMsg::Change { change: r.next_change("Rename").unwrap() }).unwrap();
    assert!(matches!(next(&mut ana), ServerMsg::Entry { .. }));
    assert!(started.elapsed() < Duration::from_secs(5), "took {:?}", started.elapsed());
}

#[test]
fn a_viewer_asking_for_the_plan_again_and_again_is_cut_off() {
    let s = setup();
    let server = s.start();
    let flooder = flood(&server, "vic", ClientMsg::Resync);
    std::thread::sleep(Duration::from_millis(500));
    still_answers_quickly(&server);
    flooder.join().unwrap();
    server.stop();
}

#[test]
fn a_viewer_sending_refused_requests_without_reading_holds_nobody_up() {
    let s = setup();
    let server = s.start();
    let flooder = flood(&server, "vic", ClientMsg::Lease);
    std::thread::sleep(Duration::from_millis(500));
    still_answers_quickly(&server);
    flooder.join().unwrap();
    server.stop();
}

#[test]
fn viewers_and_people_without_a_plan_may_only_send_small_frames() {
    let s = setup();
    let server = s.start();
    let mut vic = signed_in(&server, "vic");
    vic.open("Plan.plancraft", ClientId::new(), None, None).unwrap();
    let _ = vic.send_raw(&vec![b' '; 200_000]); // the server may close before it is all sent
    loop {
        match vic.recv() {
            Ok(ServerMsg::People { .. }) => continue,
            Ok(other) => panic!("{other:?}"),
            Err(_) => break,
        }
    }
    let mut ana = signed_in(&server, "ana");
    let _ = ana.send_raw(&vec![b' '; 200_000]);
    assert!(ana.recv().is_err());
    server.stop();
}

#[test]
fn a_connection_that_does_not_sign_in_is_closed() {
    let s = setup();
    let server = s.start();
    let mut c = connect(&server);
    let mut answered = 0;
    for _ in 0..20 {
        match c.request(&ClientMsg::Ping) {
            Ok(ServerMsg::Pong) => answered += 1,
            _ => break,
        }
    }
    assert!(answered < 10, "{answered} pings answered before signing in");
    server.stop();
}

#[test]
fn many_sign_ins_at_once_cannot_all_try_a_password() {
    let s = setup();
    let server = s.start();
    let threads: Vec<_> = (0..12)
        .map(|_| {
            let addr = server.addr();
            let fp = server.fingerprint().clone();
            std::thread::spawn(move || {
                let mut c = Client::connect(addr, &Trust::Pinned(fp), "test").unwrap();
                match c.login("ana", "guess guess") {
                    Err(ClientError::LoginFailed { message: m, .. }) => m.contains("wrong user name"),
                    _ => false,
                }
            })
        })
        .collect();
    let checked = threads.into_iter().map(|t| t.join().unwrap()).filter(|wrong| *wrong).count();
    assert!(checked <= 5, "{checked} passwords were checked");
    server.stop();
}

#[test]
fn someone_removed_from_the_users_file_is_shut_out_of_open_plans() {
    let s = setup();
    let server = s.start();
    let mut ben = signed_in(&server, "ben");
    ben.open("Plan.plancraft", ClientId::new(), None, None).unwrap();
    let path = plancraft_server::server::users_path(&s.config);
    let mut users = plancraft_server::users::Users::load(&path).unwrap();
    users.remove("ben").unwrap();
    // A different size, so the change shows even within one tick of a coarse clock.
    users.save().unwrap();
    assert!(matches!(next_reply(&mut ben, &ClientMsg::Resync), ServerMsg::Closing { .. }));
    server.stop();
}

#[test]
fn someone_removed_who_only_listens_or_pings_is_shut_out_too() {
    let s = setup();
    let server = s.start();
    let mut ben = signed_in(&server, "ben");
    ben.open("Plan.plancraft", ClientId::new(), None, None).unwrap();
    let mut eve = signed_in(&server, "eve");
    let mut ana = signed_in(&server, "ana");
    let mut r = replica(&ana.open("Plan.plancraft", ClientId::new(), None, None).unwrap());
    let path = plancraft_server::server::users_path(&s.config);
    let mut users = plancraft_server::users::Users::load(&path).unwrap();
    users.remove("ben").unwrap();
    users.remove("eve").unwrap();
    users.save().unwrap();
    std::thread::sleep(Duration::from_millis(800));
    // Ben only listens: he is told and let go, and never sees Ana's next change.
    rename_first_task(&mut r, "Not for Ben");
    ana.send(&ClientMsg::Change { change: r.next_change("Rename").unwrap() }).unwrap();
    assert!(matches!(next(&mut ana), ServerMsg::Entry { .. }));
    loop {
        match ben.recv() {
            Ok(ServerMsg::Entry { .. }) => panic!("a removed person still got a change"),
            Ok(ServerMsg::Closing { .. } | ServerMsg::People { .. }) => continue,
            Ok(other) => panic!("{other:?}"),
            Err(_) => break,
        }
    }
    // Eve only pings.
    assert!(matches!(eve.request(&ClientMsg::Ping), Ok(ServerMsg::Closing { .. }) | Err(_)));
    server.stop();
}

#[test]
fn stopping_from_another_thread_returns_only_once_everything_is_saved() {
    // As Ctrl+C does: a handler stops the server while the main thread waits.
    let s = setup();
    // A plan large enough that saving it takes a while.
    let mut big = plancraft_core::io::load(s.folder.join("Plan.plancraft")).unwrap();
    for i in 0..20_000 {
        big.add_task(format!("Task {i}"), None);
    }
    plancraft_core::io::save(&big, s.folder.join("Plan.plancraft")).unwrap();
    let server = s.start();
    let mut ana = signed_in(&server, "ana");
    let mut r = replica(&ana.open("Plan.plancraft", ClientId::new(), None, None).unwrap());
    rename_first_task(&mut r, "Saved before exiting");
    ana.send(&ClientMsg::Change { change: r.next_change("Rename").unwrap() }).unwrap();
    assert!(matches!(next(&mut ana), ServerMsg::Entry { .. }));
    let stopper = server.stopper();
    let handler = std::thread::spawn(move || stopper.stop());
    server.wait();
    let path = s.folder.join("Plan.plancraft");
    assert!(!plancraft_core::lock::lock_path(&path).unwrap().exists(), "the lock was given back before wait returned");
    assert_eq!(plancraft_core::io::load(&path).unwrap().tasks[0].name, "Saved before exiting");
    handler.join().unwrap();
}

#[test]
fn a_connection_trickling_bytes_cannot_stay_without_signing_in() {
    use std::io::Write;
    let s = setup();
    let mut options = s.options();
    options.login_timeout = Duration::from_secs(1);
    let server = plancraft_server::server::start(options).unwrap();
    let mut raw = std::net::TcpStream::connect(server.addr()).unwrap();
    let started = std::time::Instant::now();
    // A handshake message of 48 bytes, sent one byte at a time, each well within the timeout of
    // a single read.
    raw.write_all(&[0, 48]).unwrap();
    let mut closed = false;
    while started.elapsed() < Duration::from_secs(6) {
        if raw.write_all(&[0]).is_err() {
            closed = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    assert!(closed, "still open after {:?}", started.elapsed());
    assert!(started.elapsed() < Duration::from_secs(4));
    server.stop();
}

#[test]
fn someone_no_longer_allowed_one_plan_is_let_go_while_listening() {
    let s = setup();
    let server = s.start();
    let mut eve = signed_in(&server, "eve");
    eve.open("Plan.plancraft", ClientId::new(), None, None).unwrap();
    let path = plancraft_server::server::users_path(&s.config);
    let mut users = plancraft_server::users::Users::load(&path).unwrap();
    users.set_role("eve", Role::None, Some("plan.PLANCRAFT")).unwrap();
    users.save().unwrap();
    let mut closed = false;
    for _ in 0..50 {
        match eve.recv() {
            Ok(ServerMsg::Closing { .. }) => {
                closed = true;
                break;
            }
            Ok(_) => continue,
            Err(_) => break,
        }
    }
    assert!(closed, "she was told");
    server.stop();
}

#[test]
fn a_few_computers_cannot_hold_every_place_for_signing_in() {
    let s = setup();
    let mut options = s.options();
    options.not_signed_in_per_address = plancraft_server::server::MAX_NOT_SIGNED_IN_PER_ADDRESS;
    let server = plancraft_server::server::start(options).unwrap();
    // Silent connections from one address: only a few are let in.
    let held: Vec<std::net::TcpStream> = (0..20).filter_map(|_| std::net::TcpStream::connect(server.addr()).ok()).collect();
    std::thread::sleep(Duration::from_millis(300));
    let open = held
        .iter()
        .filter(|c| {
            c.set_read_timeout(Some(Duration::from_millis(50))).unwrap();
            let mut b = [0u8; 1];
            match std::io::Read::read(&mut &**c, &mut b) {
                Ok(0) => false,
                Err(e) => matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut),
                Ok(_) => true,
            }
        })
        .count();
    assert!(open <= plancraft_server::server::MAX_NOT_SIGNED_IN_PER_ADDRESS, "{open} held");
    drop(held);
    std::thread::sleep(Duration::from_millis(200));
    let mut ana = signed_in(&server, "ana");
    assert!(matches!(ana.request(&ClientMsg::Ping).unwrap(), ServerMsg::Pong));
    server.stop();
}
