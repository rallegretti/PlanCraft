//! A plan opened from a server, kept in step in the background (docs/COLLABORATION.md › Phase 3).
//!
//! A [`Link`] owns the connection and this person's [`Replica`]. The app hands it each local edit
//! as the plan before and after it ([`Link::edited`]); the link merges, sends, receives other
//! people's changes, pings, reconnects and catches up on its own threads, and hands back the
//! plan to show ([`Event::Plan`]). Merging never runs on the app's thread: on a large plan, fitting
//! someone's unconfirmed work around other people's changes can take seconds.
//!
//! Edits and plans are matched by number: each edit the app hands on gets the next number, and
//! each plan the link hands back says which edits it includes (`acked`). The app shows a plan only
//! when it includes every edit handed on, so a plan worked out before the person's latest edit
//! never hides that edit.

use std::fmt;
use std::sync::Arc;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use plancraft_core::ids::TaskId;
use plancraft_core::model::Project;
use plancraft_core::sync::{Conflict, Merged, Replica, apply, diff, rebase};
use plancraft_core::{guard, limits, schedule};
use uuid::Uuid;

use crate::client::{Client, ClientError};
use crate::protocol::{
    CatchUp, ClientId, ClientMsg, DEFAULT_PORT, Known, MAX_SERVER_FRAME, Opened, Person, Role, Secret, ServerMsg, Snapshot,
};
use crate::wire::{self, Fingerprint, Trust, WireError};

/// How often the link pings the server while connected (the server closes connections silent for
/// two minutes).
pub const PING_EVERY: Duration = Duration::from_secs(30);
/// Silence after which the connection counts as lost (a ping is answered within it).
const READ_TIMEOUT: Duration = Duration::from_secs(75);
const WRITE_TIMEOUT: Duration = Duration::from_secs(30);
/// Waits between tries to reconnect: doubling from the first to the last.
const RETRY_FIRST: Duration = Duration::from_secs(1);
const RETRY_MOST: Duration = Duration::from_secs(30);
/// New ids left in the lease below which the link asks for another block.
const LEASE_LOW: u32 = 250;
/// Wait before asking for ids again after the server said no for now (it hands out one block
/// every 30 seconds once a person's first 20 are used).
const LEASE_RETRY: Duration = Duration::from_secs(30);
/// Wait before sending changes again after the server could not take them for now.
const HOLD_AFTER_REFUSAL: Duration = Duration::from_secs(30);

/// Called when there is news for the app (it wakes the app's window).
pub type Waker = Arc<dyn Fn() + Send + Sync>;

/// A server and how to sign in to it again after the connection drops.
#[derive(Clone)]
pub struct Account {
    /// `host:port`, resolved again on each connection.
    pub address: String,
    /// The server's key fingerprint, as the person confirmed it.
    pub fingerprint: Fingerprint,
    pub user: String,
    pub password: Secret,
}

impl fmt::Debug for Account {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Account").field("address", &self.address).field("user", &self.user).finish_non_exhaustive()
    }
}

/// A server address as typed (`host`, `host:port`, an IPv6 address with or without brackets)
/// as `host:port`, with PlanCraft's port when none is given.
pub fn normalise_address(typed: &str) -> Result<String, String> {
    let s = typed.trim();
    if s.is_empty() {
        return Err("type the server's address".into());
    }
    if s.chars().any(|c| c.is_whitespace() || c.is_control() || c == '/') {
        return Err(format!("'{s}' is not a server address"));
    }
    if let Some(rest) = s.strip_prefix('[') {
        // [v6] or [v6]:port
        let Some((host, after)) = rest.split_once(']') else { return Err(format!("'{s}' is not a server address")) };
        return match after.strip_prefix(':') {
            Some(port) => Ok(format!("[{host}]:{}", parse_port(port)?)),
            None if after.is_empty() => Ok(format!("[{host}]:{DEFAULT_PORT}")),
            None => Err(format!("'{s}' is not a server address")),
        };
    }
    match s.matches(':').count() {
        0 => Ok(format!("{s}:{DEFAULT_PORT}")),
        1 => {
            let (host, port) = s.split_once(':').unwrap_or((s, ""));
            if host.is_empty() {
                return Err(format!("'{s}' has no host name"));
            }
            Ok(format!("{host}:{}", parse_port(port)?))
        }
        // A bare IPv6 address.
        _ => Ok(format!("[{s}]:{DEFAULT_PORT}")),
    }
}

fn parse_port(port: &str) -> Result<u16, String> {
    port.parse::<u16>().ok().filter(|p| *p != 0).ok_or_else(|| format!("'{port}' is not a port number"))
}

/// Where the link stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum State {
    Connected,
    /// Connecting again after the connection dropped.
    Connecting,
    /// Not connected; tries again by itself. Edits wait and are sent on reconnection.
    Offline(String),
    /// The server will not let this person in again (the password changed, access was taken
    /// away, the plan is gone, the server's key changed). Edits stay in the plan, unsent.
    SignedOut(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Status {
    pub state: State,
    pub role: Role,
    /// Edits the server has not confirmed yet.
    pub unconfirmed: bool,
    /// The number of the last edit this status takes into account: edits handed on after it are
    /// not confirmed either.
    pub acked: u64,
}

/// News from the link, for the app.
#[derive(Clone, Debug)]
pub enum Event {
    /// The plan to show, including this person's edits up to number `acked`.
    Plan {
        project: Project,
        acked: u64,
    },
    /// Edits up to number `acked` are in; the plan the app has is the one to show.
    Acked(u64),
    Status(Status),
    /// Who has the plan open, and which task each is on.
    People(Vec<Person>),
    /// Some of this person's unconfirmed work was dropped: `conflicts` say what and why
    /// (`Conflict::describe`, against `before`), or `discarded` when all of it went. `before` is
    /// their plan just before, to keep as a copy.
    Dropped {
        conflicts: Vec<Conflict>,
        discarded: Option<String>,
        before: Project,
    },
    /// Something to tell the person (a change the server refused, a problem).
    Notice(String),
}

enum Input {
    Edited { number: u64, before: Box<Project>, after: Box<Project>, label: String },
    Presence(Option<TaskId>),
    Close,
    Server { generation: u64, msg: Box<ServerMsg> },
    Lost { generation: u64, why: String },
}

/// A plan opened from a server, kept in step on background threads. Dropping it closes the
/// connection.
pub struct Link {
    inputs: mpsc::Sender<Input>,
    events: mpsc::Receiver<Event>,
}

/// What [`Link::open`] gives back.
pub struct OpenedLink {
    pub link: Link,
    /// The plan as it opened.
    pub project: Project,
    pub role: Role,
    pub people: Vec<Person>,
}

impl Link {
    /// Opens `plan` over `client` (connected and signed in as `account.user`) and keeps it in step
    /// from then on. `waker` is called whenever there are events to collect.
    pub fn open(mut client: Client, account: Account, plan: &str, waker: Waker) -> Result<OpenedLink, String> {
        let client_id = ClientId::new();
        client.set_read_timeout(Some(READ_TIMEOUT)).map_err(|e| e.to_string())?;
        let opened = client.open(plan, client_id, None, None).map_err(|e| e.to_string())?;
        let CatchUp::Snapshot { snapshot } = opened.catch_up else {
            return Err("the server sent changes for a plan this PlanCraft does not have".into());
        };
        let project = parse_plan(&snapshot.plan, plan)?;
        // No lease (a viewer, or the server rations them): no new items until one comes.
        let replica = match opened.lease.clone() {
            Some(ids) => Replica::new(project, snapshot.version, ids),
            None => Replica::without_lease(project, snapshot.version),
        };
        let local = replica.local().clone();
        let (inputs, inputs_rx) = mpsc::channel();
        let (events_tx, events) = mpsc::channel();
        let (sender, receiver) = client.into_parts();
        let _ = sender.set_write_timeout(Some(WRITE_TIMEOUT));
        let mut session = Session {
            account,
            plan: plan.to_string(),
            client_id,
            epoch: snapshot.epoch,
            replica,
            role: opened.role,
            conn: None,
            generation: 0,
            inputs: inputs.clone(),
            events: events_tx,
            waker,
            acked: 0,
            label: String::new(),
            lease_asked: false,
            lease_retry_at: None,
            hold_until: None,
            resync_asked: false,
            presence: None,
            retry_at: None,
            retry_wait: RETRY_FIRST,
            signed_out: None,
            offline: None,
            last_status: None,
        };
        session.install(sender, receiver);
        let people = opened.people.clone();
        let (last_words, last_waker, role) = (session.events.clone(), Arc::clone(&session.waker), opened.role);
        thread::Builder::new()
            .name("plancraft-link".into())
            .spawn(move || {
                if let Err(p) = guard::catch("keeping a shared plan in step", move || session.run(inputs_rx)) {
                    // Never a link that silently stopped: the plan says so, and keeps its edits.
                    let why =
                        format!("PlanCraft hit an internal error keeping the plan in step ({}); use Save As to keep a copy", p.message);
                    let _ = last_words.send(Event::Status(Status { state: State::SignedOut(why), role, unconfirmed: true, acked: 0 }));
                    last_waker();
                }
            })
            .map_err(|e| format!("cannot start keeping the plan in step: {e}"))?;
        Ok(OpenedLink { link: Link { inputs, events }, project: local, role: opened.role, people })
    }

    /// Hands on a local edit (number `number`, counting up from 1): the plan before it and after
    /// it (several edits may go as one).
    pub fn edited(&self, number: u64, before: Project, after: Project, label: &str) {
        let _ = self.inputs.send(Input::Edited { number, before: Box::new(before), after: Box::new(after), label: label.to_string() });
    }

    /// The task this person is on, shown to the others.
    pub fn presence(&self, task: Option<TaskId>) {
        let _ = self.inputs.send(Input::Presence(task));
    }

    /// The events since last asked.
    pub fn events(&self) -> Vec<Event> {
        self.events.try_iter().collect()
    }
}

impl Drop for Link {
    fn drop(&mut self) {
        let _ = self.inputs.send(Input::Close);
    }
}

fn parse_plan(text: &str, name: &str) -> Result<Project, String> {
    let (mut p, _) = plancraft_core::io::from_json_with_notes(text, std::path::Path::new(name)).map_err(|e| e.to_string())?;
    schedule::try_recalculate(&mut p).map_err(|e| e.to_string())?;
    Ok(p)
}

/// Checks and recalculates a merged plan (an edit fitted around other people's changes).
fn checked(mut p: Project) -> Result<Project, String> {
    match guard::catch("merging an edit", move || -> Result<Project, String> {
        limits::check_edit(&mut p).map_err(|e| e.to_string())?;
        schedule::try_recalculate(&mut p).map_err(|e| e.message)?;
        Ok(p)
    }) {
        Ok(r) => r,
        Err(panicked) => Err(panicked.to_string()),
    }
}

struct Conn {
    sender: wire::Sender,
    last_sent: Instant,
}

struct Session {
    account: Account,
    plan: String,
    client_id: ClientId,
    /// The server's run of the plan the versions count in.
    epoch: Uuid,
    replica: Replica,
    role: Role,
    conn: Option<Conn>,
    /// Counts connections, so a reader of an old one is not mistaken for the current one.
    generation: u64,
    inputs: mpsc::Sender<Input>,
    events: mpsc::Sender<Event>,
    waker: Waker,
    /// The number of the last edit taken in.
    acked: u64,
    /// The label of the last edit (sent with the next change).
    label: String,
    lease_asked: bool,
    /// Asking for a lease again (the server refused the last request for now).
    lease_retry_at: Option<Instant>,
    /// Changes wait until then (the server could not take them for now).
    hold_until: Option<Instant>,
    /// The whole plan was asked for (entries were missed): further gaps until it comes are the
    /// same gap.
    resync_asked: bool,
    presence: Option<TaskId>,
    retry_at: Option<Instant>,
    retry_wait: Duration,
    signed_out: Option<String>,
    offline: Option<String>,
    last_status: Option<Status>,
}

impl Session {
    fn run(&mut self, inputs: mpsc::Receiver<Input>) {
        self.publish_status();
        // Opened without a lease (the server rations them): ask for one from the start.
        self.top_up_lease();
        loop {
            let wait = self.next_wake().saturating_duration_since(Instant::now()).max(Duration::from_millis(10));
            match inputs.recv_timeout(wait) {
                Ok(Input::Close) | Err(RecvTimeoutError::Disconnected) => break,
                Ok(input) => self.handle(input),
                Err(RecvTimeoutError::Timeout) => {}
            }
            self.tick();
            self.publish_status();
        }
        if let Some(c) = self.conn.take() {
            c.sender.shutdown();
        }
    }

    fn next_wake(&self) -> Instant {
        let now = Instant::now();
        let main = match (&self.conn, self.retry_at) {
            (Some(c), _) => c.last_sent.checked_add(PING_EVERY).unwrap_or(now),
            (None, Some(at)) => at,
            (None, None) => now.checked_add(PING_EVERY).unwrap_or(now),
        };
        [Some(main), self.hold_until, self.lease_retry_at].into_iter().flatten().min().unwrap_or(main)
    }

    fn tick(&mut self) {
        let now = Instant::now();
        if self.conn.as_ref().is_some_and(|c| now.saturating_duration_since(c.last_sent) >= PING_EVERY) {
            self.send(&ClientMsg::Ping);
        }
        if self.hold_until.is_some_and(|t| now >= t) {
            self.hold_until = None;
            self.send_pending();
        }
        if self.lease_retry_at.is_some_and(|t| now >= t) {
            self.lease_retry_at = None;
            self.top_up_lease();
        }
        if self.conn.is_none() && self.signed_out.is_none() && self.retry_at.is_none_or(|t| now >= t) {
            // Shown while the try lasts (it can take a while when the server is slow).
            self.offline = None;
            self.publish_status();
            self.reconnect();
        }
    }

    fn emit(&self, event: Event) {
        if self.events.send(event).is_ok() {
            (self.waker)();
        }
    }

    fn status(&self) -> Status {
        let state = if let Some(why) = &self.signed_out {
            State::SignedOut(why.clone())
        } else if self.conn.is_some() {
            State::Connected
        } else if let Some(why) = &self.offline {
            State::Offline(why.clone())
        } else {
            State::Connecting
        };
        Status { state, role: self.role, unconfirmed: self.replica.waiting() || !self.replica.pending().is_empty(), acked: self.acked }
    }

    fn publish_status(&mut self) {
        let now = self.status();
        if self.last_status.as_ref() != Some(&now) {
            self.last_status = Some(now.clone());
            self.emit(Event::Status(now));
        }
    }

    fn show_plan(&self) {
        self.emit(Event::Plan { project: self.replica.local().clone(), acked: self.acked });
    }

    fn report(&self, merged: Merged, before: Project) {
        if !merged.conflicts.is_empty() || merged.discarded.is_some() {
            self.emit(Event::Dropped { conflicts: merged.conflicts, discarded: merged.discarded, before });
        }
    }

    fn notice(&self, text: impl Into<String>) {
        self.emit(Event::Notice(text.into()));
    }

    /// Sends a message; a failure drops the connection (the link reconnects).
    fn send(&mut self, msg: &ClientMsg) -> bool {
        let Some(conn) = self.conn.as_mut() else { return false };
        let sent = serde_json::to_vec(msg).map_err(|e| e.to_string()).and_then(|bytes| conn.sender.send(&bytes).map_err(|e| e.to_string()));
        match sent {
            Ok(()) => {
                conn.last_sent = Instant::now();
                true
            }
            Err(why) => {
                self.lost(&why);
                false
            }
        }
    }

    /// Starts using a connection whose plan is open: a reader thread passes on what arrives.
    fn install(&mut self, sender: wire::Sender, mut receiver: wire::Receiver) {
        self.generation = self.generation.saturating_add(1);
        let generation = self.generation;
        let _ = receiver.set_read_timeout(Some(READ_TIMEOUT));
        let inputs = self.inputs.clone();
        let started = thread::Builder::new().name("plancraft-link-reader".into()).spawn(move || {
            loop {
                let input = match receiver.recv(MAX_SERVER_FRAME) {
                    Ok(bytes) => match serde_json::from_slice::<ServerMsg>(&bytes) {
                        Ok(msg) => Input::Server { generation, msg: Box::new(msg) },
                        Err(e) => Input::Lost { generation, why: format!("the server sent something unexpected: {e}") },
                    },
                    Err(e) => Input::Lost { generation, why: describe_wire(&e) },
                };
                let last = matches!(input, Input::Lost { .. });
                if inputs.send(input).is_err() || last {
                    break;
                }
            }
        });
        match started {
            Ok(_) => {
                self.conn = Some(Conn { sender, last_sent: Instant::now() });
                self.offline = None;
                self.retry_wait = RETRY_FIRST;
                self.retry_at = None;
            }
            Err(e) => {
                sender.shutdown();
                self.offline = Some(format!("cannot read from the server: {e}"));
                self.schedule_retry();
            }
        }
    }

    fn lost(&mut self, why: &str) {
        if let Some(c) = self.conn.take() {
            c.sender.shutdown();
        }
        self.offline = Some(why.to_string());
        self.lease_asked = false;
        self.schedule_retry();
    }

    fn schedule_retry(&mut self) {
        let now = Instant::now();
        self.retry_at = now.checked_add(self.retry_wait);
        self.retry_wait = self.retry_wait.saturating_mul(2).min(RETRY_MOST);
    }

    fn handle(&mut self, input: Input) {
        match input {
            Input::Edited { number, before, after, label } => self.edited(number, *before, *after, label),
            Input::Presence(task) => {
                self.presence = task;
                self.send(&ClientMsg::Presence { task });
            }
            Input::Server { generation, msg } if generation == self.generation => self.server(*msg),
            Input::Lost { generation, why } if generation == self.generation && self.conn.is_some() => self.lost(&why),
            Input::Server { .. } | Input::Lost { .. } | Input::Close => {}
        }
    }

    /// A local edit: fitted around other people's changes the app had not shown yet, taken into
    /// the replica, and sent.
    fn edited(&mut self, number: u64, before: Project, after: Project, label: String) {
        self.acked = number;
        self.label = label;
        if !self.role.can_edit() {
            self.notice("You may only view this plan; your change was not kept.");
            self.show_plan();
            return;
        }
        let app_ids = after.ids.clone();
        let fitted = before != *self.replica.local();
        let mut clashes = None;
        let next = if fitted {
            match self.fit(&before, after) {
                Ok((p, dropped)) => {
                    clashes = dropped;
                    p
                }
                Err(why) => {
                    self.notice(format!("Your change could not be fitted around other people's changes: {why}"));
                    self.show_plan();
                    return;
                }
            }
        } else {
            after
        };
        if self.replica.beyond_lease(&next.ids) {
            if self.replica.leased() {
                self.notice(format!(
                    "That change adds more new items than a shared plan takes at once ({}); it was not kept. Add them in smaller parts.",
                    plancraft_core::sync::LEASE_SIZE
                ));
            } else {
                self.notice("The server has not handed out ids for new items yet, so that change was not kept. Try again in a moment.");
            }
            self.ask_lease();
            self.show_plan();
            return;
        }
        if let Err(e) = self.replica.edited(next) {
            self.notice(format!("Your change could not be kept: {e}"));
            self.show_plan();
            return;
        }
        if let Some(dropped) = clashes {
            self.emit(dropped);
        }
        // The app's plan is the one to show unless the edit was fitted, or the replica moved the
        // id cursor on (the app then counts on from there).
        if fitted || self.replica.local().ids != app_ids {
            self.show_plan();
        } else {
            self.emit(Event::Acked(number));
        }
        self.send_pending();
        self.top_up_lease();
    }

    /// Fits an edit made on `before` onto the plan as it is now (other people's changes came in
    /// after the app last showed a plan). Where both changed the same value, theirs stays: the
    /// event saying so is returned, to report once the edit is kept.
    fn fit(&self, before: &Project, after: Project) -> Result<(Project, Option<Event>), String> {
        let now = self.replica.local();
        let mine = diff(before, &after).map_err(|e| e.to_string())?;
        let theirs = diff(before, now).map_err(|e| e.to_string())?;
        let r = rebase(&mine, &theirs, None);
        let dropped = (!r.conflicts.is_empty()).then(|| Event::Dropped { conflicts: r.conflicts, discarded: None, before: after.clone() });
        let mut p = checked(apply(now, &r.patch).map_err(|e| e.to_string())?)?;
        p.ids = after.ids;
        Ok((p, dropped))
    }

    fn send_pending(&mut self) {
        if self.conn.is_none() || !self.role.can_edit() || self.hold_until.is_some() {
            return;
        }
        let label = if self.label.is_empty() { "Edit".to_string() } else { self.label.clone() };
        if let Some(change) = self.replica.next_change(&label) {
            self.send(&ClientMsg::Change { change });
        }
    }

    fn ask_lease(&mut self) {
        if self.role.can_edit() && !self.lease_asked && self.conn.is_some() {
            self.lease_asked = self.send(&ClientMsg::Lease);
        }
    }

    fn top_up_lease(&mut self) {
        if self.replica.ids_left() < LEASE_LOW && self.lease_retry_at.is_none() {
            self.ask_lease();
        }
    }

    /// Entries were missed (or one does not apply here). With a change on its way, reconnecting
    /// asks the server whether it went in (a plain resync could send it again and undo someone's
    /// change made since); otherwise the whole plan is asked for, once.
    fn missed(&mut self) {
        if self.replica.in_flight().is_some() {
            self.lost("changes were missed; loading the plan again");
        } else if !self.resync_asked {
            self.resync_asked = self.send(&ClientMsg::Resync);
        }
    }

    fn server(&mut self, msg: ServerMsg) {
        match msg {
            ServerMsg::Entry { entry } => {
                let before = self.replica.local().clone();
                match self.replica.receive(&entry) {
                    Ok(merged) => {
                        if *self.replica.local() != before {
                            self.show_plan();
                        }
                        self.report(merged, before);
                        self.send_pending();
                    }
                    Err(_) => self.missed(),
                }
            }
            ServerMsg::Rejected { change, reason, retry, role, .. } => {
                let before = self.replica.local().clone();
                if let Some(role) = role {
                    self.role = role;
                }
                // For now (the server cannot save) or no longer an editor: the work is kept, not
                // dropped; it goes again after a wait, or stays for Save As.
                let keep = retry || role.is_some_and(|r| !r.can_edit());
                let merged = self.replica.rejected(change, if keep { None } else { reason.as_deref() });
                if retry {
                    self.hold_until = Instant::now().checked_add(HOLD_AFTER_REFUSAL);
                    self.notice(format!(
                        "The server could not take your change for now; PlanCraft sends it again shortly. ({})",
                        reason.as_deref().unwrap_or_default()
                    ));
                } else if keep {
                    self.notice("You may now only view this plan. Changes not sent yet stay in your copy; use Save As to keep them.");
                } else if let Some(why) = &reason
                    && merged.discarded.is_none()
                {
                    self.notice(format!("The server did not take your last change: {why}"));
                }
                if *self.replica.local() != before {
                    self.show_plan();
                }
                self.report(merged, before);
                self.send_pending();
            }
            ServerMsg::Snapshot { snapshot, in_flight_applied } => {
                self.resync_asked = false;
                self.snapshot(snapshot, in_flight_applied);
                self.send_pending();
            }
            ServerMsg::Leased { ids } => {
                self.lease_asked = false;
                match self.replica.lease(ids) {
                    Ok(()) => self.show_plan(),
                    Err(e) => self.notice(format!("The server's new block of ids could not be used: {e}")),
                }
            }
            ServerMsg::People { people } => self.emit(Event::People(people)),
            ServerMsg::Error { message, .. } => {
                if self.lease_asked {
                    // Most likely the answer to the lease request (rationed): ask again later,
                    // and only bother the person when the plan has no ids left at all.
                    self.lease_asked = false;
                    self.lease_retry_at = Instant::now().checked_add(LEASE_RETRY);
                    if message.contains("run out") {
                        self.notice(message);
                    }
                } else {
                    self.notice(message);
                }
            }
            ServerMsg::Closing { message, .. } => self.lost(&message),
            ServerMsg::Pong
            | ServerMsg::Welcome { .. }
            | ServerMsg::LoggedIn { .. }
            | ServerMsg::LoginFailed { .. }
            | ServerMsg::Plans { .. }
            | ServerMsg::Opened(_) => {}
        }
    }

    /// The whole plan from the server: this person's unconfirmed work is fitted onto it.
    fn snapshot(&mut self, snapshot: Snapshot, in_flight_applied: bool) {
        let project = match parse_plan(&snapshot.plan, &self.plan) {
            Ok(p) => p,
            Err(why) => {
                self.notice(format!("The server sent a plan this PlanCraft cannot read: {why}"));
                return;
            }
        };
        self.epoch = snapshot.epoch;
        let before = self.replica.local().clone();
        match self.replica.resync(project, snapshot.version, in_flight_applied) {
            Ok(merged) => {
                self.show_plan();
                self.report(merged, before);
            }
            Err(e) => self.notice(format!("The plan could not be loaded again: {e}")),
        }
    }

    fn reconnect(&mut self) {
        self.retry_at = None;
        let attempt = (|| -> Result<(Client, Opened), ClientError> {
            let mut client = Client::connect(self.account.address.as_str(), &Trust::Pinned(self.account.fingerprint.clone()), "PlanCraft")?;
            client.login(&self.account.user, &self.account.password.0)?;
            client.set_read_timeout(Some(READ_TIMEOUT))?;
            let known = Known { epoch: self.epoch, version: self.replica.version() };
            let opened = client.open(&self.plan, self.client_id, Some(known), self.replica.in_flight())?;
            Ok((client, opened))
        })();
        match attempt {
            Ok((client, opened)) => {
                let (sender, receiver) = client.into_parts();
                let _ = sender.set_write_timeout(Some(WRITE_TIMEOUT));
                self.install(sender, receiver);
                if self.conn.is_some() {
                    self.opened(opened);
                }
            }
            Err(ClientError::Wire(WireError::WrongServer { .. })) => {
                self.signed_out = Some(
                    "the server's key has changed: it may be another computer pretending to be the server. Ask whoever runs it.".into(),
                );
            }
            Err(ClientError::LoginFailed { message, credentials: true, .. }) => self.signed_out = Some(message),
            Err(ClientError::LoginFailed { message, retry_after_secs, .. }) => {
                self.offline = Some(message);
                self.retry_at = Instant::now().checked_add(Duration::from_secs(retry_after_secs.max(1)));
            }
            Err(ClientError::Refused(message)) => self.signed_out = Some(message),
            Err(ClientError::Temporary(message)) => {
                self.offline = Some(message);
                self.schedule_retry();
            }
            Err(e) => {
                self.offline = Some(e.to_string());
                self.schedule_retry();
            }
        }
    }

    /// Catches up after reconnecting: missed changes, or the whole plan; a new lease.
    fn opened(&mut self, opened: Opened) {
        self.role = opened.role;
        let before = self.replica.local().clone();
        match opened.catch_up {
            CatchUp::Entries { entries } => {
                let mut gap = false;
                for entry in &entries {
                    match self.replica.receive(entry) {
                        Ok(merged) => self.report(merged, before.clone()),
                        Err(_) => {
                            gap = true;
                            break;
                        }
                    }
                }
                if gap {
                    // Catching up failed part way: the whole plan, once.
                    self.resync_asked = self.send(&ClientMsg::Resync);
                } else if self.replica.in_flight().is_some() {
                    // The change on its way when the connection dropped is not among the
                    // missed ones: it never went in, and goes again with the rest.
                    let (confirmed, version) = (self.replica.confirmed().clone(), self.replica.version());
                    if let Ok(merged) = self.replica.resync(confirmed, version, opened.in_flight_applied) {
                        self.report(merged, before.clone());
                    }
                }
            }
            CatchUp::Snapshot { snapshot } => self.snapshot(snapshot, opened.in_flight_applied),
        }
        if let Some(ids) = opened.lease
            && self.replica.lease(ids.clone()).is_err()
        {
            // The server started the plan afresh and handed out ids this copy already used:
            // only a fresh start is safe. The person keeps their plan as it was, as a copy.
            let dropped = self.replica.local().clone();
            self.replica = Replica::new(self.replica.confirmed().clone(), self.replica.version(), ids);
            self.emit(Event::Dropped {
                conflicts: Vec::new(),
                discarded: Some("the server started this plan afresh, so changes it had not confirmed could not be sent".into()),
                before: dropped,
            });
        }
        self.show_plan();
        self.emit(Event::People(opened.people));
        if self.presence.is_some() {
            self.send(&ClientMsg::Presence { task: self.presence });
        }
        self.send_pending();
        self.top_up_lease();
    }
}

fn describe_wire(e: &WireError) -> String {
    match e {
        WireError::Io(io) if matches!(io.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {
            "the server stopped answering".into()
        }
        WireError::Io(io) if io.kind() == std::io::ErrorKind::UnexpectedEof => "the server closed the connection".into(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_get_the_default_port() {
        assert_eq!(normalise_address(" plans.local ").unwrap(), format!("plans.local:{DEFAULT_PORT}"));
        assert_eq!(normalise_address("10.0.0.5:9000").unwrap(), "10.0.0.5:9000");
        assert_eq!(normalise_address("fe80::1").unwrap(), format!("[fe80::1]:{DEFAULT_PORT}"));
        assert_eq!(normalise_address("[fe80::1]:9000").unwrap(), "[fe80::1]:9000");
        assert_eq!(normalise_address("[fe80::1]").unwrap(), format!("[fe80::1]:{DEFAULT_PORT}"));
        for bad in ["", "  ", "host:0", "host:99999", "host:x", ":80", "a b", "http://x", "[fe80::1", "[::1]x"] {
            assert!(normalise_address(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_password_never_shows_in_debug_output() {
        let a = Account {
            address: "x:1".into(),
            fingerprint: Fingerprint("f".into()),
            user: "ana".into(),
            password: Secret("hunter22".into()),
        };
        assert!(!format!("{a:?}").contains("hunter22"));
    }
}
