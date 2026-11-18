//! One shared plan on the server: its [`Host`], its journal, its lock file and the people who
//! have it open.
//!
//! Durability, in order (see `journal.rs`):
//! 1. an accepted change is in memory, then on disk in the journal, and only then does anyone
//!    hear of it;
//! 2. a block of ids is in the journal before it is handed out;
//! 3. the plan file is saved a few seconds after changes stop (and at least every minute), with
//!    `io::write_atomic`, and the journal then starts afresh;
//! 4. opening the plan replays whatever the journal holds beyond the saved file.
//!
//! Any failure to write the journal (a full or failing disk) is [`Broken`]: nothing more is
//! written to it (a half-written line followed by more records would hide them on recovery), the
//! change is not confirmed, and the plan is closed for everyone without saving. Opening it again
//! starts from what is on disk.

use std::collections::{BTreeMap, HashMap};
use std::io::Read;
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::SyncSender;
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use plancraft_core::ids::{IdCounters, TaskId};
use plancraft_core::io::{self, DiskStamp, Privacy, write_atomic};
use plancraft_core::limits::MAX_FILE_BYTES;
use plancraft_core::lock::{self, LockState};
use plancraft_core::model::Project;
use plancraft_core::sync::{Change, ChangeId, Host, Outcome, Version};
use uuid::Uuid;

use crate::Log;
use crate::journal::{self, Journal, Record, plan_hash};
use crate::protocol::{CatchUp, ClientId, Known, Opened, Person, Role, ServerMsg, Snapshot};

/// Saves this long after the last change.
pub const SAVE_WHEN_IDLE: Duration = Duration::from_secs(3);
/// Saves at least this often while changes keep coming.
pub const SAVE_AT_LEAST: Duration = Duration::from_secs(60);
/// Waits this long after a failed save before trying again.
pub const SAVE_RETRY: Duration = Duration::from_secs(30);
/// The largest a shared plan may grow, as saved (below what a plan file may be, so it always
/// opens again, and its whole-plan message always fits a frame).
pub const MAX_SHARED_BYTES: usize = 56 * 1024 * 1024;
/// Missed changes sent on reconnection, at most; beyond that the whole plan is sent instead.
pub const MAX_CATCH_UP_BYTES: usize = 32 * 1024 * 1024;
/// Bytes waiting for one connection before it counts as too slow (one message always fits an
/// empty queue, however large).
pub const MAX_QUEUED_BYTES: usize = 160 * 1024 * 1024;
/// Bytes waiting for all connections together.
pub const MAX_TOTAL_QUEUED_BYTES: usize = 512 * 1024 * 1024;
/// Blocks of ids one person may ask for in a row, and how fast that allowance comes back.
const LEASE_BURST: f64 = 20.0;
const LEASE_REFILL: Duration = Duration::from_secs(30);
/// The journal is saved into the plan file (and starts afresh) once this much was written to it,
/// so it stays far below what recovery reads ([`journal::MAX_JOURNAL_BYTES`]).
pub const SAVE_AFTER_JOURNAL_BYTES: u64 = 64 * 1024 * 1024;
/// While saves keep failing, changes are refused once the journal holds this much, well below
/// what recovery reads, so the plan always opens again.
pub const MAX_UNSAVED_JOURNAL_BYTES: u64 = 512 * 1024 * 1024;
/// A plan as saved is larger than the changes that made it: pretty-printed, and with schedules
/// and default values the changes leave out. Changes count this many times their size until the
/// size is measured.
pub(crate) const GROWTH: usize = 8;

/// How many blocks of ids each person may still ask for, per plan, kept by the server so that
/// leaving and opening a plan again does not refill it.
pub type Allowances = Arc<Mutex<HashMap<String, (f64, Instant)>>>;

/// One connection.
pub type ConnId = u64;

/// The bytes waiting to go out, for every connection and for each one. Over the limit for one
/// connection, that connection is closed; over the limit for all of them, the connection holding
/// the most is closed (not whoever happened to send next), so one person who never reads cannot
/// cut everyone else off.
#[derive(Debug)]
pub struct Budget {
    total: AtomicUsize,
    per_connection: usize,
    everyone: usize,
    queues: Mutex<HashMap<ConnId, Weak<Queue>>>,
}

impl Budget {
    pub fn new(per_connection: usize, everyone: usize) -> Arc<Budget> {
        Arc::new(Budget { total: AtomicUsize::new(0), per_connection, everyone, queues: Mutex::new(HashMap::new()) })
    }

    /// The server's limits ([`MAX_QUEUED_BYTES`], [`MAX_TOTAL_QUEUED_BYTES`]).
    pub fn standard() -> Arc<Budget> {
        Budget::new(MAX_QUEUED_BYTES, MAX_TOTAL_QUEUED_BYTES)
    }

    pub fn total(&self) -> usize {
        self.total.load(Ordering::SeqCst)
    }
}

/// One connection's queue.
#[derive(Debug)]
pub struct Queue {
    conn: ConnId,
    mine: AtomicUsize,
    tx: SyncSender<Out>,
    /// Shut down to close the connection at once (its reader is waiting on it).
    socket: Option<Arc<TcpStream>>,
}

impl Queue {
    fn kick(&self) {
        let _ = self.tx.try_send(Out::Close);
        if let Some(s) = &self.socket {
            let _ = s.shutdown(std::net::Shutdown::Both);
        }
    }

    pub fn queued(&self) -> usize {
        self.mine.load(Ordering::SeqCst)
    }
}

/// A message's bytes, held once however many connections it goes to, and counted once in the
/// budget's total while any of them still has it.
#[derive(Debug)]
pub struct Payload {
    bytes: Vec<u8>,
    budget: Arc<Budget>,
}

impl Payload {
    pub fn new(bytes: Vec<u8>, budget: &Arc<Budget>) -> Arc<Payload> {
        budget.total.fetch_add(bytes.len(), Ordering::SeqCst);
        Arc::new(Payload { bytes, budget: Arc::clone(budget) })
    }
}

impl Drop for Payload {
    fn drop(&mut self) {
        self.budget.total.fetch_sub(self.bytes.len(), Ordering::SeqCst);
    }
}

/// A frame on its way to one connection. Its bytes count in that connection's queue while it
/// waits, and are counted back when it is dropped, sent or not, so the counts never drift.
#[derive(Debug)]
pub struct Counted {
    payload: Arc<Payload>,
    queue: Arc<Queue>,
}

impl Counted {
    pub fn bytes(&self) -> &[u8] {
        &self.payload.bytes
    }
}

impl Drop for Counted {
    fn drop(&mut self) {
        self.queue.mine.fetch_sub(self.payload.bytes.len(), Ordering::SeqCst);
    }
}

/// What a connection's writer sends next.
#[derive(Debug)]
pub enum Out {
    Frame(Counted),
    /// Close the connection.
    Close,
}

/// A connection's way out. Nothing ever waits on it: a connection that cannot take a message
/// is closed instead, and catches up when its PlanCraft reconnects. So one slow or hostile reader
/// never holds up a plan or the server.
#[derive(Clone, Debug)]
pub struct Outbox {
    queue: Arc<Queue>,
    budget: Arc<Budget>,
}

impl Outbox {
    pub fn new(conn: ConnId, tx: SyncSender<Out>, budget: Arc<Budget>, socket: Option<Arc<TcpStream>>) -> Self {
        let queue = Arc::new(Queue { conn, mine: AtomicUsize::new(0), tx, socket });
        if let Ok(mut q) = budget.queues.lock() {
            q.retain(|_, w| w.strong_count() > 0);
            q.insert(conn, Arc::downgrade(&queue));
        }
        Outbox { queue, budget }
    }

    pub fn queue(&self) -> &Arc<Queue> {
        &self.queue
    }

    /// Queues a message; `false` when the connection was closed instead.
    pub fn push(&self, msg: &ServerMsg) -> bool {
        match serde_json::to_vec(msg) {
            Ok(b) => self.push_payload(&Payload::new(b, &self.budget)),
            Err(_) => false,
        }
    }

    /// The budget messages for this connection count in.
    pub fn budget(&self) -> &Arc<Budget> {
        &self.budget
    }

    /// Queues raw bytes as one frame.
    pub fn push_frame(&self, bytes: Vec<u8>) -> bool {
        self.push_payload(&Payload::new(bytes, &self.budget))
    }

    pub fn push_payload(&self, payload: &Arc<Payload>) -> bool {
        let n = payload.bytes.len();
        let mine = self.queue.mine.fetch_add(n, Ordering::SeqCst);
        // Counted from here on: dropping it counts the bytes back.
        let frame = Counted { payload: Arc::clone(payload), queue: Arc::clone(&self.queue) };
        // One message always fits an empty queue (a whole plan is large).
        if mine > 0 && mine.saturating_add(n) > self.budget.per_connection {
            self.kick();
            return false;
        }
        if self.budget.total() > self.budget.everyone && !self.make_room() {
            self.kick();
            return false;
        }
        if self.queue.tx.try_send(Out::Frame(frame)).is_err() {
            self.kick();
            return false;
        }
        true
    }

    /// Everyone's queues are full: closes the connection holding the most, unless that is this
    /// one. Returns whether this one may go on.
    fn make_room(&self) -> bool {
        let biggest = self
            .budget
            .queues
            .lock()
            .ok()
            .and_then(|q| q.values().filter_map(Weak::upgrade).filter(|other| other.queued() > 0).max_by_key(|other| other.queued()));
        match biggest {
            Some(q) if q.conn != self.queue.conn && q.queued() >= self.queue.queued() => {
                q.kick();
                true
            }
            _ => false,
        }
    }

    /// Closes the connection once what is queued has gone out.
    pub fn finish(&self) {
        let _ = self.queue.tx.try_send(Out::Close);
    }

    /// Closes the connection now.
    pub fn kick(&self) {
        self.queue.kick();
    }
}

/// Someone with the plan open.
#[derive(Clone, Debug)]
pub struct Subscriber {
    pub user: String,
    pub client: ClientId,
    pub role: Role,
    pub out: Outbox,
    pub task: Option<TaskId>,
}

/// The plan can no longer be kept safe (its journal cannot be written): close it for everyone,
/// without saving.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Broken(pub String);

/// Why a save did not happen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SaveError {
    /// The plan file could not be written; the journal is intact: try again later.
    Retry(String),
    /// The journal could not be written: close the plan.
    Broken(Broken),
}

/// What a join came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Joined {
    Yes,
    /// Refused (the message was sent to the connection).
    No,
}

pub struct SharedPlan {
    name: String,
    path: PathBuf,
    host: Host,
    /// Which run of the shared plan this is (a new one when the file was changed outside, or
    /// when a damaged journal lost changes that copies may already have).
    epoch: Uuid,
    journal: Journal,
    /// The plan file as this server last read or wrote it.
    stamp: Option<DiskStamp>,
    /// Where saves go: the plan file, or a hidden copy once the file was changed outside.
    save_to: PathBuf,
    /// The saved size, plus every change since (an upper bound, checked exactly near the limit).
    size: usize,
    dirty: bool,
    last_change: Instant,
    last_save: Instant,
    last_lock: Instant,
    /// After a failed save, the next try waits until then.
    retry_save: Option<Instant>,
    subscribers: BTreeMap<ConnId, Subscriber>,
    /// The connection that speaks for each copy (a reconnection replaces the old one).
    current: HashMap<ClientId, ConnId>,
    /// Leases each person may still ask for (refilled over time).
    allowances: Allowances,
    /// Set by the first failed journal write: nothing is written after it (a torn line followed by
    /// more records would hide them from recovery) until the plan is closed.
    broken: Option<Broken>,
    /// [`MAX_SHARED_BYTES`] and [`MAX_CATCH_UP_BYTES`] (smaller in tests).
    size_limit: usize,
    catch_up_limit: usize,
    /// [`SAVE_AFTER_JOURNAL_BYTES`] and [`MAX_UNSAVED_JOURNAL_BYTES`] (smaller in tests).
    journal_save_bytes: u64,
    max_unsaved_journal: u64,
    log: Log,
}

fn read_plan(path: &Path) -> Result<(Vec<u8>, Project, Vec<String>), String> {
    let meta = std::fs::symlink_metadata(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    if !meta.is_file() {
        return Err(format!("{} is not a regular file", path.display()));
    }
    let mut bytes = Vec::new();
    io::open_regular(path, false, io::Links::Refuse)
        .and_then(|f| f.take(MAX_FILE_BYTES + 1).read_to_end(&mut bytes))
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(format!("{} is too large to share", path.display()));
    }
    let text = std::str::from_utf8(&bytes).map_err(|_| format!("{} is not a PlanCraft plan", path.display()))?;
    let (mut project, notes) = io::from_json_with_notes(text, path).map_err(|e| e.to_string())?;
    plancraft_core::schedule::recalculate(&mut project);
    Ok((bytes, project, notes))
}

/// A new file next to `near`, named `.STEM <what> <time>[ n].EXT`: hidden, never one that exists
/// (nor a link planted in its place), readable by its owner only.
fn new_hidden_file(near: &Path, what: &str, ext: &str) -> Option<(PathBuf, std::fs::File)> {
    let stem = near.file_name()?.to_string_lossy().trim_start_matches('.').to_string();
    let stamp = plancraft_core::dates::format("%Y-%m-%d %H%M%S", plancraft_core::dates::now());
    for n in 0..100 {
        let extra = if n == 0 { String::new() } else { format!(" {n}") };
        let path = near.with_file_name(format!(".{stem} {what} {stamp}{extra}.{ext}"));
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
        if let Ok(f) = opts.open(&path) {
            return Some((path, f));
        }
    }
    None
}

/// Keeps a journal that could not be used for a person to look at; says where.
fn keep_aside(journal: &Path) -> String {
    let kept = (|| {
        let (path, mut to) = new_hidden_file(journal, "unused", "journal")?;
        let mut from = io::open_regular(journal, false, io::Links::Refuse).ok()?;
        std::io::copy(&mut from, &mut to).ok()?;
        to.sync_all().ok()?;
        Some(path)
    })();
    kept.map(|p| p.display().to_string()).unwrap_or_else(|| "nowhere: it could not be copied".into())
}

/// The epoch in a journal's header replaced (see [`SharedPlan::epoch`]).
fn with_epoch(records: &[Record], new: Uuid) -> Vec<Record> {
    records
        .iter()
        .map(|r| match r {
            Record::Header { version, plan_hash, applied, .. } => {
                Record::Header { epoch: new, version: *version, plan_hash: plan_hash.clone(), applied: applied.clone() }
            }
            other => other.clone(),
        })
        .collect()
}

impl SharedPlan {
    /// Opens the plan at `path` for sharing: takes its lock, loads it, and replays its journal.
    pub fn open(path: &Path, name: &str, log: Log) -> Result<SharedPlan, String> {
        Self::open_with(path, name, log, Allowances::default())
    }

    /// [`SharedPlan::open`], with the server's lease allowances.
    pub fn open_with(path: &Path, name: &str, log: Log, allowances: Allowances) -> Result<SharedPlan, String> {
        match lock::claim(path) {
            LockState::OpenElsewhere(owner) => {
                return Err(format!("{name} is open elsewhere ({}); it can be shared once it is closed there", owner.describe()));
            }
            LockState::Held | LockState::None => {}
        }
        let opened = Self::load(path, name, log, allowances);
        if opened.is_err() {
            lock::release(path);
        }
        opened
    }

    fn load(path: &Path, name: &str, log: Log, allowances: Allowances) -> Result<SharedPlan, String> {
        let (bytes, project, notes) = read_plan(path)?;
        for note in notes {
            log(&format!("{name}: {note}"));
        }
        let file_hash = plan_hash(&bytes);
        let jpath = journal::path_for(path).ok_or_else(|| format!("{} is not a file path", path.display()))?;
        let back = journal::read(&jpath).map_err(|e| format!("cannot read the journal of {name}: {e}"))?;
        // A journal that does not start with a header is damaged too.
        let headed = matches!(back.records.first(), Some(Record::Header { .. }));
        let mut damaged = back.damaged || (!back.records.is_empty() && !headed);

        // Which version the plan file holds, and what to replay on top of it.
        let mut start: Option<(Uuid, Version, Vec<ChangeId>, &[Record])> = None;
        if let Some((Record::Header { epoch, version, plan_hash, applied }, rest)) = back.records.split_first() {
            if *plan_hash == file_hash {
                start = Some((*epoch, *version, applied.clone(), rest));
            } else if let Some(saved) = rest.iter().rev().find_map(|r| match r {
                Record::Saving { version, plan_hash } if *plan_hash == file_hash => Some(*version),
                _ => None,
            }) {
                // The power failed after the plan was saved, before the journal started afresh.
                let mut ids = applied.clone();
                ids.extend(rest.iter().filter_map(|r| match r {
                    Record::Entry { entry } if entry.version <= saved => Some(entry.change),
                    _ => None,
                }));
                start = Some((*epoch, saved, ids, rest));
            }
        }
        let (mut epoch, version, applied, rest) = match start {
            Some(s) => s,
            None => {
                // No journal, the plan file was changed outside the server, or the journal's
                // start is unreadable: a new run from the file.
                let lost = back.records.iter().filter(|r| matches!(r, Record::Entry { .. })).count();
                if lost > 0 || damaged {
                    let kept = keep_aside(&jpath);
                    let why = if damaged { "its journal is damaged" } else { "the plan file was changed outside the server" };
                    log(&format!("{name}: {why}; {lost} readable shared changes in the journal were not applied (it is kept in {kept})"));
                }
                damaged = false;
                (Uuid::new_v4(), 1, Vec::new(), &[][..])
            }
        };

        let mut host = Host::resume(project, version, applied);
        let mut replayed = 0usize;
        for record in rest {
            match record {
                Record::Lease { start } => {
                    host.replay_lease(start);
                    replayed += 1;
                }
                Record::Entry { entry } if entry.version > version => {
                    if let Err(e) = host.replay(entry) {
                        log(&format!("{name}: replaying the journal stopped at version {}: {e}", entry.version));
                        damaged = true;
                        break;
                    }
                    replayed += 1;
                }
                Record::Entry { .. } | Record::Saving { .. } => {}
                Record::Header { .. } => {
                    damaged = true;
                    break;
                }
            }
        }
        let mut records = back.records.clone();
        if damaged {
            let kept = keep_aside(&jpath);
            log(&format!("{name}: its journal is damaged; the readable part was applied (the journal is kept in {kept})"));
            // Copies may know changes that were lost with the damaged part, at versions this run
            // will give to other changes: a new epoch makes them load the plan afresh instead of
            // catching up by version.
            epoch = Uuid::new_v4();
            records = with_epoch(&records, epoch);
        }

        // With records replayed, the journal keeps them until the plan file holds them (the save
        // below); the readable records are written back without a torn or damaged tail, so the
        // save's own record lands on a line of its own.
        let journal = if replayed > 0 || damaged {
            Journal::start_with(&jpath, &records)
        } else {
            Journal::start(&jpath, &Record::Header { epoch, version: host.version(), plan_hash: file_hash, applied: host.applied_ids() })
        }
        .map_err(|e| format!("cannot write the journal of {name}: {e}"))?;
        let now = Instant::now();
        let mut plan = SharedPlan {
            name: name.to_string(),
            path: path.to_path_buf(),
            host,
            epoch,
            journal,
            stamp: io::disk_stamp(path),
            save_to: path.to_path_buf(),
            size: bytes.len(),
            dirty: false,
            last_change: now,
            last_save: now,
            last_lock: now,
            retry_save: None,
            subscribers: BTreeMap::new(),
            current: HashMap::new(),
            allowances,
            broken: None,
            size_limit: MAX_SHARED_BYTES,
            catch_up_limit: MAX_CATCH_UP_BYTES,
            journal_save_bytes: SAVE_AFTER_JOURNAL_BYTES,
            max_unsaved_journal: MAX_UNSAVED_JOURNAL_BYTES,
            log,
        };
        if replayed > 0 || damaged {
            // What the journal added (changes, handed-out ids, a new epoch) goes into the plan
            // file now.
            plan.save().map_err(|e| match e {
                SaveError::Retry(e) | SaveError::Broken(Broken(e)) => format!("cannot save {name}: {e}"),
            })?;
            (plan.log)(&format!("{name}: recovered {replayed} journal records after an unclean stop"));
        }
        Ok(plan)
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn host(&self) -> &Host {
        &self.host
    }

    pub fn epoch(&self) -> Uuid {
        self.epoch
    }

    pub fn people_count(&self) -> usize {
        self.subscribers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.subscribers.is_empty()
    }

    pub fn snapshot(&self) -> Result<Snapshot, String> {
        let plan = io::to_json(self.host.project()).map_err(|e| e.to_string())?;
        Ok(Snapshot { epoch: self.epoch, version: self.host.version(), plan })
    }

    fn people(&self) -> Vec<Person> {
        let mut people: Vec<Person> = self.subscribers.values().map(|s| Person { user: s.user.clone(), task: s.task }).collect();
        people.sort_by_key(|p| p.user.to_lowercase());
        people
    }

    /// Sends `msg` to one connection (closed instead if it cannot keep up).
    fn send(&mut self, conn: ConnId, msg: &ServerMsg) {
        let closed = self.subscribers.get(&conn).is_some_and(|s| !s.out.push(msg));
        if closed {
            self.drop_subscriber(conn);
        }
    }

    pub fn broadcast(&mut self, msg: &ServerMsg) {
        let Ok(bytes) = serde_json::to_vec(msg) else { return };
        let Some(budget) = self.subscribers.values().next().map(|s| Arc::clone(s.out.budget())) else { return };
        // One copy for everyone, counted once.
        let payload = Payload::new(bytes, &budget);
        let closed: Vec<ConnId> = self.subscribers.iter().filter(|(_, s)| !s.out.push_payload(&payload)).map(|(c, _)| *c).collect();
        for conn in closed {
            self.drop_subscriber(conn);
        }
    }

    fn drop_subscriber(&mut self, conn: ConnId) {
        if let Some(s) = self.subscribers.remove(&conn) {
            s.out.kick();
            if self.current.get(&s.client) == Some(&conn) {
                self.current.remove(&s.client);
            }
            (self.log)(&format!("{}: {} could not keep up and was disconnected", self.name, s.user));
        }
    }

    fn journal(&mut self, record: &Record) -> Result<(), Broken> {
        if let Some(b) = &self.broken {
            return Err(b.clone());
        }
        let written = self.journal.append(record).map_err(|e| Broken(format!("the server cannot write the journal of {}: {e}", self.name)));
        if let Err(b) = &written {
            self.broken = Some(b.clone());
        }
        written
    }

    /// The plan can no longer be kept safe (see [`SharedPlan::broken`]): close it.
    pub fn broken(&self) -> Option<&Broken> {
        self.broken.as_ref()
    }

    /// Takes away access as the users file now says: someone removed, or no longer allowed to
    /// open the plan, is told and disconnected; a changed role takes effect.
    pub fn revoke(&mut self, role_of: impl Fn(&str) -> Role) {
        let mut gone = Vec::new();
        for (conn, s) in &mut self.subscribers {
            let role = role_of(&s.user);
            if role == Role::None {
                gone.push(*conn);
            } else {
                s.role = role;
            }
        }
        for conn in gone {
            if let Some(s) = self.subscribers.remove(&conn) {
                s.out.push(&ServerMsg::Closing { message: "you may no longer open this plan".into() });
                s.out.finish();
                if self.current.get(&s.client) == Some(&conn) {
                    self.current.remove(&s.client);
                }
                (self.log)(&format!("{}: {} no longer has access and was disconnected", self.name, s.user));
            }
        }
        let people = self.people();
        self.broadcast(&ServerMsg::People { people });
    }

    /// Lets a connection in and sends it [`ServerMsg::Opened`]. A connection of the same copy that
    /// was still open (the old one of a reconnection) is closed first, and whatever it sent is
    /// either done or dropped: requests on one copy are handled one at a time, under this plan's
    /// lock, and only from the connection that speaks for it. A copy belongs to the person who
    /// opened it: someone else cannot take it over.
    pub fn join(&mut self, conn: ConnId, sub: Subscriber, known: Option<Known>, in_flight: Option<ChangeId>) -> Result<Joined, Broken> {
        if let Some(b) = &self.broken {
            return Err(b.clone());
        }
        if let Some(old) = self.current.get(&sub.client).and_then(|c| self.subscribers.get(c))
            && old.user.to_lowercase() != sub.user.to_lowercase()
        {
            sub.out.push(&ServerMsg::Error { message: "this copy of the plan belongs to someone else".into() });
            return Ok(Joined::No);
        }
        if let Some(old) = self.current.insert(sub.client, conn)
            && old != conn
            && let Some(s) = self.subscribers.remove(&old)
        {
            s.out.kick();
        }
        let in_flight_applied = in_flight.is_some_and(|id| self.host.was_applied(id));
        let entries = known.filter(|k| k.epoch == self.epoch).and_then(|k| self.host.since(k.version)).filter(|e| {
            e.iter().map(|e| serde_json::to_vec(e).map_or(usize::MAX, |b| b.len())).fold(0usize, usize::saturating_add)
                <= self.catch_up_limit
        });
        let catch_up = match entries {
            Some(entries) => CatchUp::Entries { entries },
            None => CatchUp::Snapshot { snapshot: self.snapshot().map_err(Broken)? },
        };
        let lease = if sub.role.can_edit() && self.allow_lease(&sub.user) { self.lease()? } else { None };
        let role = sub.role;
        (self.log)(&format!("{}: {} joined ({} now)", self.name, sub.user, self.subscribers.len() + 1));
        self.subscribers.insert(conn, sub);
        let people = self.people();
        let opened = Opened { plan: self.name.clone(), role, catch_up, lease, in_flight_applied, people: people.clone() };
        self.send(conn, &ServerMsg::Opened(Box::new(opened)));
        let others: Vec<ConnId> = self.subscribers.keys().copied().filter(|c| *c != conn).collect();
        for other in others {
            self.send(other, &ServerMsg::People { people: people.clone() });
        }
        Ok(Joined::Yes)
    }

    pub fn leave(&mut self, conn: ConnId) {
        if let Some(s) = self.subscribers.remove(&conn) {
            if self.current.get(&s.client) == Some(&conn) {
                self.current.remove(&s.client);
            }
            (self.log)(&format!("{}: {} left ({} now)", self.name, s.user, self.subscribers.len()));
            let people = self.people();
            self.broadcast(&ServerMsg::People { people });
        }
    }

    /// Whether `conn` still speaks for its copy (not replaced by a reconnection).
    fn speaks(&self, conn: ConnId) -> Option<&Subscriber> {
        self.subscribers.get(&conn).filter(|s| self.current.get(&s.client) == Some(&conn))
    }

    /// Whether `user` may have another block of ids now: a few in a row, then one every 30
    /// seconds. Ids are finite (`limits::MAX_ID`); without this, one editor could use them up.
    fn allow_lease(&mut self, user: &str) -> bool {
        let now = Instant::now();
        let key = format!("{}\u{0}{}", self.name.to_lowercase(), user.to_lowercase());
        let Ok(mut all) = self.allowances.lock() else { return false };
        let refill = |(left, since): (f64, Instant)| {
            (left + now.saturating_duration_since(since).as_secs_f64() / LEASE_REFILL.as_secs_f64()).min(LEASE_BURST)
        };
        // Full allowances need no entry: forget them, so the map stays small.
        all.retain(|_, a| refill(*a) < LEASE_BURST);
        let left = all.get(&key).copied().map_or(LEASE_BURST, refill);
        if left < 1.0 {
            all.insert(key, (left, now));
            return false;
        }
        all.insert(key, (left - 1.0, now));
        true
    }

    /// Hands out a block of ids, journaled first.
    fn lease(&mut self) -> Result<Option<IdCounters>, Broken> {
        let Some(start) = self.host.lease() else { return Ok(None) };
        self.journal(&Record::Lease { start: start.clone() })?;
        self.dirty = true;
        self.last_change = Instant::now();
        Ok(Some(start))
    }

    pub fn lease_for(&mut self, conn: ConnId) -> Result<(), Broken> {
        let Some(user) = self.speaks(conn).map(|s| s.user.clone()) else { return Ok(()) };
        if !self.allow_lease(&user) {
            self.send(conn, &ServerMsg::Error { message: "too many requests for new ids; wait a moment".into() });
            return Ok(());
        }
        match self.lease()? {
            Some(ids) => self.send(conn, &ServerMsg::Leased { ids }),
            None => self.send(conn, &ServerMsg::Error { message: "this plan has run out of new ids".into() }),
        }
        Ok(())
    }

    /// A change from `conn`: applied, journaled and sent to everyone, or turned down.
    pub fn submit(&mut self, conn: ConnId, change: &Change) -> Result<(), Broken> {
        let Some(sub) = self.speaks(conn) else { return Ok(()) };
        let author = sub.user.clone();
        if let Some(b) = &self.broken {
            return Err(b.clone());
        }
        if self.journal.written() > self.max_unsaved_journal {
            let reason = "the server cannot save this plan at the moment (see its log); changes wait until it can".to_string();
            self.send(conn, &ServerMsg::Rejected { change: change.id, conflicts: Vec::new(), reason: Some(reason) });
            return Ok(());
        }
        let incoming = serde_json::to_vec(&change.patch).map_or(usize::MAX, |b| b.len());
        // Past a sixteenth of the limit (by a generous estimate), the plan the change would make
        // is measured exactly, and refused if it is too large to open again. Below that, a change
        // would have to grow the saved plan by over 120 times its own size to overshoot (adding
        // tasks grows it about 3.5 times; the most found, with hostile minimal tasks, about 40).
        let limit = self.size_limit;
        let measure = self.size.saturating_add(incoming.saturating_mul(GROWTH)) > limit / 16;
        let mut measured = None;
        let outcome = self.host.submit_checked(change, &author, |after| {
            if !measure {
                return Ok(());
            }
            let size = io::to_json(after).map_or(usize::MAX, |t| t.len());
            measured = Some(size);
            if size > limit {
                return Err(format!("the plan would grow beyond {} MB, the most a shared plan may hold", limit / (1024 * 1024)));
            }
            Ok(())
        });
        match outcome {
            Outcome::Applied(entry) => {
                self.journal(&Record::Entry { entry: entry.clone() })?;
                self.size = measured.unwrap_or_else(|| {
                    self.size.saturating_add(serde_json::to_vec(&entry.patch).map_or(usize::MAX, |b| b.len()).saturating_mul(GROWTH))
                });
                self.dirty = true;
                self.last_change = Instant::now();
                self.broadcast(&ServerMsg::Entry { entry });
                if self.journal.written() > self.journal_save_bytes {
                    match self.save() {
                        Ok(()) => {}
                        Err(SaveError::Retry(e)) => (self.log)(&format!("{}: saving failed ({e}); trying again shortly", self.name)),
                        Err(SaveError::Broken(b)) => return Err(b),
                    }
                }
            }
            Outcome::Conflicts(conflicts) => self.send(conn, &ServerMsg::Rejected { change: change.id, conflicts, reason: None }),
            Outcome::Refused(reason) => {
                (self.log)(&format!("{}: a change from {author} was refused: {reason}", self.name));
                self.send(conn, &ServerMsg::Rejected { change: change.id, conflicts: Vec::new(), reason: Some(reason) });
            }
            Outcome::AlreadyApplied(_) => self.resync(conn, true)?,
            Outcome::Resync => self.resync(conn, false)?,
        }
        Ok(())
    }

    pub fn resync(&mut self, conn: ConnId, in_flight_applied: bool) -> Result<(), Broken> {
        let snapshot = self.snapshot().map_err(Broken)?;
        self.send(conn, &ServerMsg::Snapshot { snapshot, in_flight_applied });
        Ok(())
    }

    pub fn presence(&mut self, conn: ConnId, task: Option<TaskId>) {
        let Some(s) = self.subscribers.get_mut(&conn) else { return };
        if s.task == task {
            return;
        }
        s.task = task;
        let people = self.people();
        self.broadcast(&ServerMsg::People { people });
    }

    /// Whether the plan file is still the one this server last read or wrote (a regular file,
    /// not replaced by a link, unchanged).
    fn file_is_ours(&self) -> bool {
        std::fs::symlink_metadata(&self.path).is_ok_and(|m| m.is_file()) && io::disk_stamp(&self.path) == self.stamp
    }

    /// Saves the plan file and starts the journal afresh.
    pub fn save(&mut self) -> Result<(), SaveError> {
        let text = io::to_json(self.host.project()).map_err(|e| SaveError::Retry(e.to_string()))?;
        let hash = plan_hash(text.as_bytes());
        let version = self.host.version();
        self.journal(&Record::Saving { version, plan_hash: hash.clone() }).map_err(SaveError::Broken)?;
        if self.save_to == self.path && !self.file_is_ours() {
            // Someone changed the file while it was shared (ignoring the lock): leave theirs
            // alone. The copy is hidden (no plan of its own to share, with other access rules)
            // and never written through a link.
            match new_hidden_file(&self.path, "shared copy", "plancraft") {
                Some((copy, _)) => {
                    (self.log)(&format!(
                        "{}: the plan file was changed outside the server; the shared plan is saved to {} instead",
                        self.name,
                        copy.display()
                    ));
                    self.save_to = copy;
                }
                None => return Err(SaveError::Retry("the plan file was changed outside the server, and no copy could be made".into())),
            }
        }
        let privacy = if self.save_to == self.path { Privacy::NoLinks } else { Privacy::OwnerOnly };
        write_atomic(&self.save_to, text.as_bytes(), privacy).map_err(|e| SaveError::Retry(e.to_string()))?;
        if self.save_to == self.path {
            self.stamp = io::disk_stamp(&self.path);
        }
        self.size = text.len();
        let header = Record::Header { epoch: self.epoch, version, plan_hash: hash, applied: self.host.applied_ids() };
        let jpath = self.journal.path().to_path_buf();
        // If the new journal is in place but cannot be opened, the old handle points at a file
        // that is gone: writing on there would confirm changes no recovery can see.
        match Journal::start(&jpath, &header) {
            Ok(j) => self.journal = j,
            Err(e) => {
                let b = Broken(format!("the server cannot start the journal of {} afresh: {e}", self.name));
                self.broken = Some(b.clone());
                return Err(SaveError::Broken(b));
            }
        }
        self.dirty = false;
        self.last_save = Instant::now();
        Ok(())
    }

    /// Regular upkeep: keeps the lock file fresh and saves when it is time.
    pub fn maintain(&mut self, now: Instant) -> Result<(), Broken> {
        if let Some(b) = &self.broken {
            return Err(b.clone());
        }
        if now.saturating_duration_since(self.last_lock) >= lock::REFRESH {
            self.last_lock = now;
            if let LockState::OpenElsewhere(owner) = lock::claim(&self.path) {
                (self.log)(&format!("{}: the lock file was taken by {}", self.name, owner.describe()));
            }
        }
        let due = now.saturating_duration_since(self.last_change) >= SAVE_WHEN_IDLE
            || now.saturating_duration_since(self.last_save) >= SAVE_AT_LEAST;
        let waiting = self.retry_save.is_some_and(|t| now < t);
        if self.dirty && due && !waiting {
            match self.save() {
                Ok(()) => self.retry_save = None,
                Err(SaveError::Retry(e)) => {
                    (self.log)(&format!("{}: saving failed ({e}); trying again in 30 seconds", self.name));
                    self.retry_save = Some(now + SAVE_RETRY);
                }
                Err(SaveError::Broken(b)) => return Err(b),
            }
        }
        Ok(())
    }

    /// Closes the plan: tells everyone, saves it and gives back its lock.
    pub fn close(mut self, message: &str) {
        self.broadcast(&ServerMsg::Closing { message: message.to_string() });
        for s in self.subscribers.values() {
            s.out.finish();
        }
        if self.dirty
            && self.broken.is_none()
            && let Err(e) = self.save()
        {
            let e = match e {
                SaveError::Retry(e) | SaveError::Broken(Broken(e)) => e,
            };
            (self.log)(&format!("{}: saving on close failed ({e}); its journal keeps the changes", self.name));
        }
        lock::release(&self.path);
    }

    /// Closes the plan without saving, after [`Broken`]: the journal and file on disk are what
    /// the next opening starts from.
    pub fn abandon(mut self, why: &Broken) {
        (self.log)(&format!("{}: closed without saving: {}", self.name, why.0));
        self.broadcast(&ServerMsg::Closing { message: why.0.clone() });
        for s in self.subscribers.values() {
            s.out.finish();
        }
        lock::release(&self.path);
    }
}

#[cfg(test)]
mod tests;
