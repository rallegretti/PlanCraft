//! Plans opened from a PlanCraft Server, by document handle: each keeps a [`Link`] that holds the
//! connection and merges in the background (docs/COLLABORATION.md › Phase 3).
//!
//! Every frame, [`Remotes::sync`] puts the links' news into their documents (the plan as the
//! server's copy now has it, who is there, work that was dropped) and hands each document's new
//! edits to its link. Edits are numbered: a plan from the link is shown only once it includes
//! every edit handed on, so a plan worked out before the person's latest edit never hides it.

use std::collections::HashMap;

use plancraft_core::ids::TaskId;
use plancraft_core::model::Project;
use plancraft_core::workspace::{ChangeSource, Workspace};
use plancraft_server::link::{Event, Link, State, Status};
use plancraft_server::protocol::{Person, Role};

/// Work of this person's that the server's copy could not take, shown in a bar above the plan.
pub struct DroppedWork {
    /// What was dropped and why, one sentence each (the latest last).
    pub lines: Vec<String>,
    /// The person's plan just before it was dropped, to open as a plan of its own.
    pub before: Box<Project>,
}

/// One plan opened from a server.
pub struct Remote {
    pub link: Link,
    /// The server's address, as shown.
    pub server: String,
    pub user: String,
    pub status: Status,
    pub people: Vec<Person>,
    pub dropped: Option<DroppedWork>,
    /// The number of the last edit handed to the link.
    sent: u64,
    presence: Option<TaskId>,
}

impl Remote {
    pub fn new(link: Link, server: &str, user: &str, role: Role, people: Vec<Person>) -> Self {
        Remote {
            link,
            server: server.to_string(),
            user: user.to_string(),
            status: Status { state: State::Connected, role, unconfirmed: false, acked: 0 },
            people,
            dropped: None,
            sent: 0,
            presence: None,
        }
    }

    /// Edits handed on that the server has not confirmed yet.
    pub fn unconfirmed(&self) -> bool {
        self.status.unconfirmed || self.status.acked < self.sent
    }

    /// A short word for the connection, for the tab and the status bar.
    pub fn state_label(&self) -> &'static str {
        match self.status.state {
            State::Connected if self.unconfirmed() => "Sending",
            State::Connected => "Connected",
            State::Connecting => "Connecting",
            State::Offline(_) => "Offline",
            State::SignedOut(_) => "Signed out",
        }
    }

    /// Others with the plan open (not this person's own copy).
    pub fn others(&self) -> impl Iterator<Item = &Person> {
        let me = self.user.to_lowercase();
        let mut skipped_me = false;
        self.people.iter().filter(move |p| {
            // This person appears once for their own copy; anyone else with the same name (the
            // same account on another computer) is shown.
            if !skipped_me && p.user.to_lowercase() == me {
                skipped_me = true;
                return false;
            }
            true
        })
    }
}

/// Why a shared plan cannot be changed right now, if so.
fn refusal(status: &Status, server: &str) -> Option<String> {
    match &status.state {
        State::SignedOut(why) => Some(format!("Signed out of {server}: {why}. Use Save a Copy to keep your plan as it is.")),
        _ if !status.role.can_edit() => Some("You may only view this plan: the server gives you viewer access.".into()),
        _ => None,
    }
}

#[derive(Default)]
pub struct Remotes {
    map: HashMap<u64, Remote>,
}

impl Remotes {
    pub fn add(&mut self, handle: u64, remote: Remote) {
        self.map.insert(handle, remote);
    }

    pub fn get(&self, handle: u64) -> Option<&Remote> {
        self.map.get(&handle)
    }

    pub fn get_mut(&mut self, handle: u64) -> Option<&mut Remote> {
        self.map.get_mut(&handle)
    }

    /// The open document showing `plan` from `server`, if any (one signed out does not count:
    /// opening the plan again signs in afresh, beside it).
    pub fn find(&self, ws: &Workspace, server: &str, plan: &str) -> Option<u64> {
        self.map
            .iter()
            .filter(|(_, r)| r.server == server && !matches!(r.status.state, State::SignedOut(_)))
            .map(|(h, _)| *h)
            .find(|h| ws.index_of(*h).and_then(|i| ws.docs.get(i)).and_then(|d| d.shared.as_ref()).is_some_and(|s| s.name == plan))
    }

    /// Puts the links' news into their documents and hands on the documents' new edits. Returns
    /// what to tell the person. A document that was closed drops its link (which disconnects).
    pub fn sync(&mut self, ws: &mut Workspace) -> Vec<String> {
        let mut notices = Vec::new();
        self.map.retain(|h, _| ws.index_of(*h).is_some());
        for (handle, r) in &mut self.map {
            let Some(doc) = ws.index_of(*handle).and_then(|i| ws.docs.get_mut(i)) else { continue };
            for event in r.link.events() {
                match event {
                    Event::Plan { project, acked } => {
                        if acked == r.sent && !doc.has_unsent() {
                            doc.apply_remote(project);
                        }
                    }
                    Event::Acked(_) => {}
                    Event::Status(s) => r.status = s,
                    Event::People(people) => r.people = people,
                    Event::Dropped { conflicts, discarded, before } => {
                        let mut lines: Vec<String> = conflicts.iter().map(|c| c.describe(&before)).collect();
                        lines.extend(discarded.map(|why| format!("Your changes not yet confirmed were dropped: {why}")));
                        let mut kept = r.dropped.take().map(|d| d.lines).unwrap_or_default();
                        kept.extend(lines);
                        let skip = kept.len().saturating_sub(20);
                        kept.drain(..skip);
                        r.dropped = Some(DroppedWork { lines: kept, before: Box::new(before) });
                    }
                    Event::Notice(n) => notices.push(n),
                }
            }
            if let Some(shared) = doc.shared.as_mut() {
                shared.read_only = refusal(&r.status, &r.server);
            }
            if let Some((before, after)) = doc.take_unsent() {
                r.sent = r.sent.saturating_add(1);
                let label = match &doc.last_change {
                    Some((l, ChangeSource::Agent)) => format!("{l} (AI agent)"),
                    Some((l, ChangeSource::User)) => l.clone(),
                    None => "Edit".into(),
                };
                r.link.edited(r.sent, before, after, &label);
            }
            // Saved means confirmed by the server.
            doc.dirty = r.unconfirmed();
        }
        notices
    }

    /// The task this person is on in the plan `handle` (sent only when it changes).
    pub fn presence(&mut self, handle: u64, task: Option<TaskId>) {
        if let Some(r) = self.map.get_mut(&handle)
            && r.presence != task
        {
            r.presence = task;
            r.link.presence(task);
        }
    }
}

/// A PlanCraft Server on a free local port for the app's tests: a folder with `Plan.plancraft`,
/// users ana and ben (editors) and vic (viewer), all with [`test_server::PASSWORD`].
#[cfg(test)]
pub mod test_server {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use std::sync::Arc;

    use plancraft_server::client::Client;
    use plancraft_server::link::{Account, Link, OpenedLink};
    use plancraft_server::protocol::{Role, Secret};
    use plancraft_server::server::{self, Options, Running};
    use plancraft_server::users::Users;
    use plancraft_server::wire::Trust;

    use crate::recovery::tests::TempDir;

    pub const PASSWORD: &str = "correct horse";

    pub struct TestServer {
        pub running: Running,
        _dir: TempDir,
    }

    impl TestServer {
        pub fn start() -> TestServer {
            let dir = TempDir::new("server");
            let folder = dir.0.join("shared");
            let config = dir.0.join("config");
            std::fs::create_dir_all(&folder).unwrap();
            let plan = plancraft_core::sample::demo_project(plancraft_core::dates::ymd(2026, 1, 5).unwrap());
            plancraft_core::io::save(&plan, folder.join("Plan.plancraft")).unwrap();
            let mut users = Users::load(&server::users_path(&config)).unwrap();
            users.add("ana", PASSWORD, Role::Editor).unwrap();
            users.add("ben", PASSWORD, Role::Editor).unwrap();
            users.add("vic", PASSWORD, Role::Viewer).unwrap();
            users.save().unwrap();
            let mut o = Options::new(folder, config, "127.0.0.1:0".parse().unwrap(), plancraft_server::quiet());
            o.not_signed_in_per_address = 64;
            TestServer { running: server::start(o).unwrap(), _dir: dir }
        }

        pub fn address(&self) -> String {
            self.running.addr().to_string()
        }

        /// `user` opens Plan.plancraft.
        pub fn open(&self, user: &str) -> OpenedLink {
            let trust = Trust::Pinned(self.running.fingerprint().clone());
            let mut client = Client::connect(self.running.addr(), &trust, "test").unwrap();
            client.login(user, PASSWORD).unwrap();
            let account = Account {
                address: self.address(),
                fingerprint: self.running.fingerprint().clone(),
                user: user.into(),
                password: Secret(PASSWORD.into()),
            };
            Link::open(client, account, "Plan.plancraft", Arc::new(|| {})).unwrap()
        }
    }
}
