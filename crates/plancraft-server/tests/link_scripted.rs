#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]
//! A [`Link`] against a scripted server, to put it in states a real server rarely reaches: a
//! connection dropped while the whole plan was asked for, missed changes while catching up, a
//! refused edit followed at once by another, a server asking for an absurd wait.

use std::net::TcpListener;
use std::sync::Arc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use plancraft_core::model::Project;
use plancraft_core::sync::{ChangeId, Entry, Patch};
use plancraft_server::client::Client;
use plancraft_server::link::{Account, Link, OpenedLink};
use plancraft_server::protocol::{CatchUp, ClientMsg, Opened, PROTOCOL, Role, Secret, ServerMsg, Snapshot};
use plancraft_server::wire::{self, Keys, Receiver, Sender, Trust};
use uuid::Uuid;

/// The next message that is not a ping or presence; `None` once nothing comes within the
/// receiver's timeout.
fn recv(r: &mut Receiver) -> Option<ClientMsg> {
    loop {
        let bytes = r.recv(1 << 26).ok()?;
        let m: ClientMsg = serde_json::from_slice(&bytes).unwrap();
        if !matches!(m, ClientMsg::Ping | ClientMsg::Presence { .. }) {
            return Some(m);
        }
    }
}

/// The next message other than a request for ids (the link asks for them when it has none).
fn recv_not_lease(r: &mut Receiver) -> Option<ClientMsg> {
    loop {
        match recv(r) {
            Some(ClientMsg::Lease) => {}
            other => return other,
        }
    }
}

fn send(s: &mut Sender, m: &ServerMsg) {
    s.send(&serde_json::to_vec(m).unwrap()).unwrap();
}

/// Accepts a connection and goes through hello and sign-in; returns the open request.
fn handshake(l: &TcpListener, keys: &Keys) -> (Sender, Receiver, ClientMsg) {
    let (stream, _) = l.accept().unwrap();
    let (mut s, mut r) = wire::accept(stream, keys).unwrap();
    r.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    assert!(matches!(recv(&mut r), Some(ClientMsg::Hello { .. })));
    send(&mut s, &ServerMsg::Welcome { protocol: PROTOCOL, server: "scripted".into() });
    assert!(matches!(recv(&mut r), Some(ClientMsg::Login { .. })));
    send(&mut s, &ServerMsg::LoggedIn { user: "ana".into() });
    let open = recv(&mut r).unwrap();
    (s, r, open)
}

fn entry(version: u64) -> Entry {
    Entry { version, change: ChangeId::new(), author: "ben".into(), label: "x".into(), patch: Patch::default() }
}

struct Script {
    listener: TcpListener,
    keys: Keys,
    account: Account,
    plan: String,
    project: Project,
    epoch: Uuid,
}

fn script() -> Script {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let keys = Keys::generate().unwrap();
    let account = Account {
        address: listener.local_addr().unwrap().to_string(),
        fingerprint: keys.fingerprint(),
        user: "ana".into(),
        password: Secret("pw".into()),
    };
    let project = plancraft_core::sample::demo_project(plancraft_core::dates::ymd(2026, 1, 5).unwrap());
    let plan = plancraft_core::io::to_json(&project).unwrap();
    Script { listener, keys, account, plan, project, epoch: Uuid::new_v4() }
}

fn opened(role: Role, catch_up: CatchUp, lease: Option<plancraft_core::ids::IdCounters>, in_flight_applied: bool) -> ServerMsg {
    ServerMsg::Opened(Box::new(Opened { plan: "Plan.plancraft".into(), role, catch_up, lease, in_flight_applied, people: vec![] }))
}

fn open_link(account: &Account) -> OpenedLink {
    let mut c = Client::connect(account.address.as_str(), &Trust::Pinned(account.fingerprint.clone()), "test").unwrap();
    c.login("ana", "pw").unwrap();
    Link::open(c, account.clone(), "Plan.plancraft", Arc::new(|| {})).unwrap()
}

fn renamed(p: &Project, name: &str) -> Project {
    let mut after = p.clone();
    after.tasks[0].name = name.into();
    after
}

#[test]
fn a_gap_after_reconnecting_asks_for_the_whole_plan_again() {
    // The whole plan was asked for, and the connection dropped before it came: the next
    // connection asks again when changes are missed, rather than waiting for an answer that will
    // never come (a viewer would stay behind for good).
    let sc = script();
    let (tx, rx) = mpsc::channel();
    let (l, keys, plan, epoch) = (sc.listener, sc.keys, sc.plan.clone(), sc.epoch);
    let server = std::thread::spawn(move || {
        let (mut s, mut r, _) = handshake(&l, &keys);
        send(&mut s, &opened(Role::Viewer, CatchUp::Snapshot { snapshot: Snapshot { epoch, version: 1, plan } }, None, false));
        send(&mut s, &ServerMsg::Entry { entry: entry(3) });
        tx.send(matches!(recv(&mut r), Some(ClientMsg::Resync))).unwrap();
        s.shutdown();
        drop((s, r));
        let (mut s, mut r, _) = handshake(&l, &keys);
        send(&mut s, &opened(Role::Viewer, CatchUp::Entries { entries: vec![entry(2)] }, None, false));
        send(&mut s, &ServerMsg::Entry { entry: entry(4) });
        tx.send(matches!(recv(&mut r), Some(ClientMsg::Resync))).unwrap();
    });
    let link = open_link(&sc.account);
    assert!(rx.recv_timeout(Duration::from_secs(20)).unwrap(), "the first gap asks for the plan");
    assert!(rx.recv_timeout(Duration::from_secs(30)).unwrap(), "a gap on the next connection asks again");
    server.join().unwrap();
    drop(link);
}

#[test]
fn a_gap_with_a_change_on_its_way_reconnects_instead_of_asking_for_the_plan() {
    // A plain resync would not say whether the change went in: reconnecting asks.
    let sc = script();
    let (tx, rx) = mpsc::channel();
    let (l, keys, plan, epoch, lease) = (sc.listener, sc.keys, sc.plan.clone(), sc.epoch, sc.project.ids.clone());
    let server = std::thread::spawn(move || {
        let (mut s, mut r, _) = handshake(&l, &keys);
        send(&mut s, &opened(Role::Editor, CatchUp::Snapshot { snapshot: Snapshot { epoch, version: 1, plan } }, Some(lease), false));
        let Some(ClientMsg::Change { change }) = recv_not_lease(&mut r) else { panic!("no change came") };
        send(&mut s, &ServerMsg::Entry { entry: entry(3) });
        let (_s, _r, open) = handshake(&l, &keys);
        let ok = matches!(&open, ClientMsg::Open { in_flight: Some(id), known: Some(k), .. } if *id == change.id && k.version == 1);
        tx.send(ok).unwrap();
    });
    let opened = open_link(&sc.account);
    opened.link.edited(1, opened.project.clone(), renamed(&opened.project, "Renamed"), "Rename");
    assert!(rx.recv_timeout(Duration::from_secs(30)).unwrap(), "reopened naming the change on its way");
    server.join().unwrap();
}

#[test]
fn a_gap_while_catching_up_does_not_send_an_applied_change_again() {
    // The change on its way went in (reopening says so) but catching up failed part way: the
    // whole plan's answer cannot say it went in, and sending it again would undo whoever changed
    // the same value since.
    let sc = script();
    let (tx, rx) = mpsc::channel();
    let (l, keys, plan, epoch, lease) = (sc.listener, sc.keys, sc.plan.clone(), sc.epoch, sc.project.ids.clone());
    let server = std::thread::spawn(move || {
        let (mut s, mut r, _) = handshake(&l, &keys);
        let snapshot = Snapshot { epoch, version: 1, plan: plan.clone() };
        send(&mut s, &opened(Role::Editor, CatchUp::Snapshot { snapshot }, Some(lease), false));
        assert!(matches!(recv_not_lease(&mut r), Some(ClientMsg::Change { .. })));
        send(&mut s, &ServerMsg::Entry { entry: entry(3) });
        let (mut s, mut r, _) = handshake(&l, &keys);
        // The change went in as version 2 and someone put the name back in version 3; entry 2
        // goes missing on the way.
        send(&mut s, &opened(Role::Editor, CatchUp::Entries { entries: vec![entry(3)] }, None, true));
        tx.send(matches!(recv_not_lease(&mut r), Some(ClientMsg::Resync))).unwrap();
        send(&mut s, &ServerMsg::Snapshot { snapshot: Snapshot { epoch, version: 3, plan }, in_flight_applied: false });
        r.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        tx.send(!matches!(recv_not_lease(&mut r), Some(ClientMsg::Change { .. }))).unwrap();
    });
    let opened = open_link(&sc.account);
    opened.link.edited(1, opened.project.clone(), renamed(&opened.project, "Renamed"), "Rename");
    assert!(rx.recv_timeout(Duration::from_secs(30)).unwrap(), "catching up failed, so the whole plan is asked for");
    assert!(rx.recv_timeout(Duration::from_secs(30)).unwrap(), "the change that went in is not sent again");
    server.join().unwrap();
}

#[test]
fn an_edit_right_after_a_refused_one_is_kept() {
    // Opened with no ids to hand out: adding a task is refused. The next edit comes before the
    // app took the plan without that task, so it still counts ids from the refused one: it must
    // go through all the same.
    let sc = script();
    let (tx, rx) = mpsc::channel();
    let (l, keys, plan, epoch) = (sc.listener, sc.keys, sc.plan.clone(), sc.epoch);
    let server = std::thread::spawn(move || {
        let (mut s, mut r, _) = handshake(&l, &keys);
        send(&mut s, &opened(Role::Editor, CatchUp::Snapshot { snapshot: Snapshot { epoch, version: 1, plan } }, None, false));
        let change = match recv_not_lease(&mut r) {
            Some(ClientMsg::Change { change }) => Some(change),
            _ => None,
        };
        tx.send(change).unwrap();
        drop((s, r));
    });
    let opened = open_link(&sc.account);
    let start = opened.project.clone();
    let mut added = start.clone();
    added.add_task("Refused", None);
    opened.link.edited(1, start, added.clone(), "Add task");
    opened.link.edited(2, added.clone(), renamed(&added, "Renamed"), "Rename");
    let change = rx.recv_timeout(Duration::from_secs(30)).unwrap().expect("the rename went out");
    let text = serde_json::to_string(&change.patch).unwrap();
    assert!(text.contains("Renamed"), "the rename is in the change: {text}");
    assert!(!text.contains("Refused"), "the refused task is not: {text}");
    server.join().unwrap();
}

#[test]
fn an_absurd_wait_before_signing_in_again_is_not_taken_as_none() {
    // A wait too long to add to the clock must not turn into reconnecting at once, over and over.
    let sc = script();
    let (l, keys, plan, epoch) = (sc.listener, sc.keys, sc.plan.clone(), sc.epoch);
    let (tx, rx) = mpsc::channel();
    let server = std::thread::spawn(move || {
        let (mut s, r, _) = handshake(&l, &keys);
        send(&mut s, &opened(Role::Viewer, CatchUp::Snapshot { snapshot: Snapshot { epoch, version: 1, plan } }, None, false));
        s.shutdown();
        drop((s, r));
        // The reconnection: refused with a wait no clock can hold.
        let (stream, _) = l.accept().unwrap();
        let (mut s, mut r) = wire::accept(stream, &keys).unwrap();
        r.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        assert!(matches!(recv(&mut r), Some(ClientMsg::Hello { .. })));
        send(&mut s, &ServerMsg::Welcome { protocol: PROTOCOL, server: "scripted".into() });
        assert!(matches!(recv(&mut r), Some(ClientMsg::Login { .. })));
        send(&mut s, &ServerMsg::LoginFailed { message: "wait".into(), retry_after_secs: u64::MAX, credentials: false });
        drop((s, r));
        // No more tries for a while.
        l.set_nonblocking(true).unwrap();
        let until = Instant::now() + Duration::from_secs(3);
        let mut tries = 0;
        while Instant::now() < until {
            if l.accept().is_ok() {
                tries += 1;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        tx.send(tries).unwrap();
    });
    let link = open_link(&sc.account);
    assert_eq!(rx.recv_timeout(Duration::from_secs(30)).unwrap(), 0, "no reconnecting straight away");
    server.join().unwrap();
    drop(link);
}
