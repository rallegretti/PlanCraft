#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]
//! A plan opened from a real server through a [`Link`], with a stand-in for the app: people see
//! each other's changes, edits made offline go out on reconnection, clashes are reported, and
//! the link stops trying when the server will not have it back.

mod common;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::*;
use plancraft_core::model::Project;
use plancraft_core::schedule::recalculate;
use plancraft_server::link::{Account, Event, Link, State, Status};
use plancraft_server::protocol::{Person, Role, Secret};
use plancraft_server::server::{self, Running};

/// What the app does with a link: shows its plans, hands on edits, keeps the news.
struct Mirror {
    link: Link,
    project: Project,
    sent: u64,
    status: Option<Status>,
    people: Vec<Person>,
    dropped: Vec<String>,
    notices: Vec<String>,
}

impl Mirror {
    fn open(server: &Running, user: &str, plan: &str) -> Mirror {
        let client = signed_in(server, user);
        let account = Account {
            address: server.addr().to_string(),
            fingerprint: server.fingerprint().clone(),
            user: user.into(),
            password: Secret(PASSWORD.into()),
        };
        let opened = Link::open(client, account, plan, Arc::new(|| {})).unwrap();
        Mirror {
            link: opened.link,
            project: opened.project,
            sent: 0,
            status: None,
            people: opened.people,
            dropped: Vec::new(),
            notices: Vec::new(),
        }
    }

    fn pump(&mut self) {
        for e in self.link.events() {
            match e {
                // Only a plan that includes every edit handed on is shown.
                Event::Plan { project, acked } if acked == self.sent => self.project = project,
                Event::Plan { .. } | Event::Acked(_) => {}
                Event::Status(s) => self.status = Some(s),
                Event::People(p) => self.people = p,
                Event::Dropped { conflicts, discarded, before } => {
                    self.dropped.extend(conflicts.iter().map(|c| c.describe(&before)));
                    self.dropped.extend(discarded);
                }
                Event::Notice(n) => self.notices.push(n),
            }
        }
    }

    fn edit(&mut self, f: impl FnOnce(&mut Project)) {
        let before = self.project.clone();
        f(&mut self.project);
        recalculate(&mut self.project);
        self.sent += 1;
        self.link.edited(self.sent, before, self.project.clone(), "Edit");
    }

    fn wait(&mut self, what: &str, done: impl Fn(&Mirror) -> bool) {
        let end = Instant::now() + Duration::from_secs(40);
        loop {
            self.pump();
            if done(self) {
                return;
            }
            assert!(Instant::now() < end, "waited too long for {what}: status {:?}, notices {:?}", self.status, self.notices);
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Connected, and every edit handed on is confirmed by the server.
    fn connected_and_confirmed(&self) -> bool {
        self.status.as_ref().is_some_and(|s| s.state == State::Connected && !s.unconfirmed && s.acked == self.sent)
    }

    fn has_task(&self, name: &str) -> bool {
        self.project.tasks.iter().any(|t| t.name == name)
    }
}

fn names(p: &Project) -> Vec<(u32, String)> {
    let mut v: Vec<_> = p.tasks.iter().map(|t| (t.id.0, t.name.clone())).collect();
    v.sort();
    v
}

fn restart(s: &Setup, addr: SocketAddr) -> Running {
    let mut o = s.options();
    o.bind = addr;
    // The port is free again once the old server has stopped; give the system a moment.
    for _ in 0..50 {
        if let Ok(r) = server::start(o.clone()) {
            return r;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("the server could not start again on {addr}");
}

#[test]
fn two_people_see_each_others_changes() {
    let s = setup();
    let server = s.start();
    let mut ana = Mirror::open(&server, "ana", "Plan.plancraft");
    let mut ben = Mirror::open(&server, "ben", "Plan.plancraft");
    ana.edit(|p| p.tasks[0].name = "Renamed by Ana".into());
    ben.wait("Ana's rename", |m| m.has_task("Renamed by Ana"));
    ben.edit(|p| {
        p.add_task("Added by Ben", None);
    });
    ana.wait("Ben's task", |m| m.has_task("Added by Ben"));
    ana.wait("ana settled", Mirror::connected_and_confirmed);
    ben.wait("ben settled", Mirror::connected_and_confirmed);
    assert_eq!(names(&ana.project), names(&ben.project));
    ana.wait("people", |m| m.people.len() == 2);
    ana.link.presence(Some(ana.project.tasks[0].id));
    ben.wait("Ana's place", |m| m.people.iter().any(|p| p.user == "ana" && p.task.is_some()));
    server.stop();
}

#[test]
fn edits_made_offline_are_sent_on_reconnection() {
    let s = setup();
    let server = s.start();
    let addr = server.addr();
    let mut ana = Mirror::open(&server, "ana", "Plan.plancraft");
    ana.wait("connected", Mirror::connected_and_confirmed);
    server.stop();
    ana.wait("offline", |m| m.status.as_ref().is_some_and(|s| matches!(s.state, State::Offline(_) | State::Connecting)));
    ana.edit(|p| {
        p.add_task("Written offline", None);
    });
    ana.wait("unconfirmed", |m| m.status.as_ref().is_some_and(|s| s.unconfirmed));
    let server = restart(&s, addr);
    ana.wait("sent after reconnecting", Mirror::connected_and_confirmed);
    let ben = Mirror::open(&server, "ben", "Plan.plancraft");
    assert!(ben.has_task("Written offline"), "the server has it");
    server.stop();
}

#[test]
fn a_clash_is_reported_and_both_end_with_one_plan() {
    let s = setup();
    let server = s.start();
    let addr = server.addr();
    let mut ana = Mirror::open(&server, "ana", "Plan.plancraft");
    let mut ben = Mirror::open(&server, "ben", "Plan.plancraft");
    ana.wait("connected", Mirror::connected_and_confirmed);
    ben.wait("connected", Mirror::connected_and_confirmed);
    // Both offline, both rename the same task: whoever reconnects second clashes.
    server.stop();
    ana.wait("offline", |m| m.status.as_ref().is_some_and(|s| s.state != State::Connected));
    ben.wait("offline", |m| m.status.as_ref().is_some_and(|s| s.state != State::Connected));
    ana.edit(|p| p.tasks[1].name = "Ana's name".into());
    ben.edit(|p| p.tasks[1].name = "Ben's name".into());
    let server = restart(&s, addr);
    ana.wait("ana settled", Mirror::connected_and_confirmed);
    ben.wait("ben settled", Mirror::connected_and_confirmed);
    ana.wait("the same plan", |m| m.project.tasks[1].name == ben.project.tasks[1].name);
    ben.pump();
    assert_eq!(names(&ana.project), names(&ben.project));
    let reported = ana.dropped.len() + ben.dropped.len();
    assert!(reported >= 1, "the clash was reported");
    let loser = if ana.dropped.is_empty() { &ben } else { &ana };
    assert!(loser.dropped.iter().any(|d| d.contains("at the same time")), "{:?}", loser.dropped);
    server.stop();
}

#[test]
fn a_viewer_cannot_change_the_plan() {
    let s = setup();
    let server = s.start();
    let mut vic = Mirror::open(&server, "vic", "Plan.plancraft");
    vic.wait("connected", |m| m.status.as_ref().is_some_and(|s| s.role == Role::Viewer));
    let original = names(&vic.project);
    vic.edit(|p| p.tasks[0].name = "Not allowed".into());
    vic.wait("the plan back as it was", |m| names(&m.project) == original);
    assert!(vic.notices.iter().any(|n| n.contains("only view")), "{:?}", vic.notices);
    server.stop();
}

#[test]
fn the_link_stops_trying_when_the_password_changed() {
    let s = setup();
    let server = s.start();
    let addr = server.addr();
    let mut ana = Mirror::open(&server, "ana", "Plan.plancraft");
    ana.wait("connected", Mirror::connected_and_confirmed);
    server.stop();
    let path = server::users_path(&s.config);
    let mut users = plancraft_server::users::Users::load(&path).unwrap();
    users.set_password("ana", "a new password").unwrap();
    users.save().unwrap();
    let server = restart(&s, addr);
    ana.wait("signed out", |m| m.status.as_ref().is_some_and(|s| matches!(&s.state, State::SignedOut(why) if why.contains("wrong"))));
    server.stop();
}

#[test]
fn new_ids_are_topped_up_and_too_many_at_once_are_refused() {
    let s = setup();
    let server = s.start();
    let mut ana = Mirror::open(&server, "ana", "Plan.plancraft");
    ana.wait("connected", Mirror::connected_and_confirmed);
    // More new tasks than a lease holds: refused, the plan back as it was.
    let before = names(&ana.project);
    ana.edit(|p| {
        for i in 0..plancraft_core::sync::LEASE_SIZE + 1 {
            p.add_task(format!("Bulk {i}"), None);
        }
    });
    ana.wait("refused", |m| !m.notices.is_empty() && names(&m.project) == before);
    assert!(ana.notices.iter().any(|n| n.contains("smaller parts")), "{:?}", ana.notices);
    // Most of a lease, then more: the link asked for another block in between.
    ana.edit(|p| {
        for i in 0..800 {
            p.add_task(format!("First {i}"), None);
        }
    });
    ana.wait("first batch confirmed", |m| m.connected_and_confirmed() && m.has_task("First 799"));
    // A new block starts past every id in use (the old one ends right after the batch).
    ana.wait("a new block", |m| m.project.ids.task > m.project.tasks.iter().map(|t| t.id.0).max().unwrap() + 100);
    ana.edit(|p| {
        for i in 0..500 {
            p.add_task(format!("Second {i}"), None);
        }
    });
    ana.wait("second batch confirmed", |m| m.connected_and_confirmed() && m.has_task("Second 499"));
    let ben = Mirror::open(&server, "ben", "Plan.plancraft");
    assert!(ben.has_task("First 799") && ben.has_task("Second 499"));
    server.stop();
}

#[test]
fn a_plan_changed_outside_the_server_is_loaded_again() {
    let s = setup();
    let server = s.start();
    let addr = server.addr();
    let mut ana = Mirror::open(&server, "ana", "Plan.plancraft");
    ana.wait("connected", Mirror::connected_and_confirmed);
    server.stop();
    // Someone edits the file directly while the server is down: a new run of the plan.
    let path = s.folder.join("Plan.plancraft");
    let mut p = plancraft_core::io::load(&path).unwrap();
    p.add_task("Added outside", None);
    plancraft_core::io::save(&p, &path).unwrap();
    let server = restart(&s, addr);
    ana.wait("loaded again", |m| m.connected_and_confirmed() && m.has_task("Added outside"));
    ana.edit(|p| {
        p.add_task("After the new start", None);
    });
    ana.wait("confirmed", |m| m.connected_and_confirmed() && m.has_task("After the new start"));
    let ben = Mirror::open(&server, "ben", "Plan.plancraft");
    assert_eq!(names(&ana.project), names(&ben.project));
    server.stop();
}

#[test]
fn an_editor_opened_without_a_lease_adds_nothing_until_one_comes() {
    let s = setup();
    let server = s.start();
    // Every opening uses one of a person's 20 rationed leases for the plan.
    for _ in 0..20 {
        drop(Mirror::open(&server, "ana", "Plan.plancraft"));
    }
    let mut ana = Mirror::open(&server, "ana", "Plan.plancraft");
    ana.wait("connected", Mirror::connected_and_confirmed);
    let before = names(&ana.project);
    ana.edit(|p| {
        p.add_task("Needs an id", None);
    });
    ana.wait("refused", |m| m.notices.iter().any(|n| n.contains("not handed out ids")) && names(&m.project) == before);
    // Changing what is there needs no ids, and nothing was dropped.
    ana.edit(|p| p.tasks[0].name = "Renamed without a lease".into());
    ana.wait("confirmed", |m| m.connected_and_confirmed() && m.has_task("Renamed without a lease"));
    assert!(ana.dropped.is_empty(), "{:?}", ana.dropped);
    server.stop();
}

#[test]
fn an_editor_made_a_viewer_keeps_unsent_changes() {
    let s = setup();
    let server = s.start();
    let mut ana = Mirror::open(&server, "ana", "Plan.plancraft");
    ana.wait("connected", Mirror::connected_and_confirmed);
    let path = server::users_path(&s.config);
    let mut users = plancraft_server::users::Users::load(&path).unwrap();
    users.set_role("ana", Role::Viewer, None).unwrap();
    users.save().unwrap();
    // The server reads the users file again when it changes; the next change finds out.
    ana.edit(|p| p.tasks[0].name = "Written as an editor".into());
    ana.wait("told", |m| m.notices.iter().any(|n| n.contains("only view")));
    ana.wait("viewer", |m| m.status.as_ref().is_some_and(|s| s.role == Role::Viewer && s.unconfirmed));
    assert!(ana.has_task("Written as an editor"), "the change stays in the copy");
    assert!(ana.dropped.is_empty(), "nothing was thrown away: {:?}", ana.dropped);
    server.stop();
}
