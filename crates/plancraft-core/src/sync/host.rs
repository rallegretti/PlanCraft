//! The host's copy of a shared plan: the one order of changes everyone follows.

use super::patch::{apply, check_size, diff, fit_counters, rebase};
use super::{ChangeId, Conflict, LEASE_SIZE, LOG_BYTES, LOG_LIMIT, Patch, SyncError, Version};
use crate::ids::IdCounters;
use crate::limits::{self, MAX_ID, MAX_NAME_CHARS};
use crate::model::Project;
use crate::{guard, schedule};
use serde::{Deserialize, Serialize};
use std::collections::{HashSet, VecDeque};

/// A change a replica sends to the host.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Change {
    pub id: ChangeId,
    /// The host version the change was made from.
    pub base: Version,
    /// What the change was called (the undo label, e.g. "Set Duration").
    #[serde(default)]
    pub label: String,
    pub patch: Patch,
}

/// A change the host accepted, as every replica applies it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    /// The host version this change made.
    pub version: Version,
    pub change: ChangeId,
    /// Who made it (the signed-in user).
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub label: String,
    /// The change's whole effect on the host's plan (rebased, validated and recalculated).
    pub patch: Patch,
}

/// What the host did with a change.
#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    /// Accepted: send the entry to every replica, the author included.
    Applied(Entry),
    /// The change was accepted before (it was sent twice): at this version, or at or before it
    /// when the entry is no longer kept. The replica's reply went missing: it resyncs with
    /// `in_flight_applied` (see [`super::Replica::resync`]).
    AlreadyApplied(Version),
    /// It clashed with changes made since its base; nothing was applied.
    Conflicts(Vec<Conflict>),
    /// It would break the plan (out of range, a damaged patch, a bug caught); nothing was applied.
    Refused(String),
    /// Its base is unknown or older than the kept changes: the replica must load the plan again.
    Resync,
}

/// The plan everyone agrees on, its version and the latest changes.
#[derive(Clone, Debug)]
pub struct Host {
    project: Project,
    version: Version,
    log: VecDeque<Entry>,
    /// The size of each logged entry's patch (as JSON), and their total.
    log_sizes: VecDeque<usize>,
    log_bytes: usize,
    log_bytes_limit: usize,
    /// Ids of accepted changes, longer than the log keeps entries ([`APPLIED_MEMORY`]), so a
    /// replica whose reply went missing can tell whether its change went in.
    applied: HashSet<ChangeId>,
    applied_order: VecDeque<ChangeId>,
}

/// Accepted change ids the host remembers (16 bytes each).
pub const APPLIED_MEMORY: usize = 100_000;

impl Host {
    /// Shares a plan (as opened and recalculated) at version 1.
    pub fn new(project: Project) -> Self {
        Self::resume(project, 1, Vec::new())
    }

    /// Continues sharing a plan saved at `version`, with the ids of the changes accepted before
    /// that save ([`Host::applied_ids`], saved with it). After a restart, replay the journal
    /// written since, in order: entries with [`Host::replay`] and leases with
    /// [`Host::replay_lease`]. Replicas that knew later versions resync.
    pub fn resume(mut project: Project, version: Version, applied: Vec<ChangeId>) -> Self {
        fit_counters(&mut project);
        let mut host = Self {
            project,
            version,
            log: VecDeque::new(),
            log_sizes: VecDeque::new(),
            log_bytes: 0,
            log_bytes_limit: LOG_BYTES,
            applied: HashSet::new(),
            applied_order: VecDeque::new(),
        };
        for id in applied {
            host.note_applied(id);
        }
        host
    }

    /// Keeps at most `bytes` of entries in the log (tests; the default is [`LOG_BYTES`]).
    pub fn with_log_bytes(mut self, bytes: usize) -> Self {
        self.log_bytes_limit = bytes;
        self
    }

    /// The ids of the accepted changes the host remembers, oldest first (save them with the plan).
    pub fn applied_ids(&self) -> Vec<ChangeId> {
        self.applied_order.iter().copied().collect()
    }

    pub fn project(&self) -> &Project {
        &self.project
    }

    pub fn version(&self) -> Version {
        self.version
    }

    /// The changes after `version`, or `None` when some of them are no longer kept.
    pub fn since(&self, version: Version) -> Option<Vec<Entry>> {
        if version > self.version {
            return None;
        }
        let oldest = self.log.front().map_or(self.version.saturating_add(1), |e| e.version);
        if version.saturating_add(1) < oldest {
            return None;
        }
        Some(self.log.iter().filter(|e| e.version > version).cloned().collect())
    }

    /// Hands a replica its own block of new ids: it starts counting from the returned counters,
    /// and the host's counters move past the block. `None` when ids are running out.
    pub fn lease(&mut self) -> Option<IdCounters> {
        let start = self.project.ids.clone();
        let mut next = start.clone();
        for counter in [
            &mut next.task,
            &mut next.resource,
            &mut next.calendar,
            &mut next.assignment,
            &mut next.time_entry,
            &mut next.custom_field,
            &mut next.board_column,
        ] {
            *counter = counter.checked_add(LEASE_SIZE).filter(|c| *c <= MAX_ID)?;
        }
        self.project.ids = next;
        Some(start)
    }

    /// Whether the change was accepted (remembered for the last [`APPLIED_MEMORY`] changes).
    pub fn was_applied(&self, change: ChangeId) -> bool {
        self.applied.contains(&change)
    }

    /// Takes back a lease handed out before a restart (from the journal), so it is not handed
    /// out again and the ids taken from it are accepted.
    pub fn replay_lease(&mut self, start: &IdCounters) {
        let ids = &mut self.project.ids;
        for (counter, from) in [
            (&mut ids.task, start.task),
            (&mut ids.resource, start.resource),
            (&mut ids.calendar, start.calendar),
            (&mut ids.assignment, start.assignment),
            (&mut ids.time_entry, start.time_entry),
            (&mut ids.custom_field, start.custom_field),
            (&mut ids.board_column, start.board_column),
        ] {
            *counter = (*counter).max(from.saturating_add(LEASE_SIZE).min(MAX_ID));
        }
    }

    /// Rebases, checks and applies a change from `author`.
    pub fn submit(&mut self, change: &Change, author: &str) -> Outcome {
        self.submit_checked(change, author, |_| Ok(()))
    }

    /// [`Host::submit`], with one more check of the plan the change would make: an `Err` refuses
    /// the change (`Outcome::Refused`) and nothing changes. The server uses it to keep a shared
    /// plan small enough to open again.
    pub fn submit_checked(&mut self, change: &Change, author: &str, check: impl FnOnce(&Project) -> Result<(), String>) -> Outcome {
        if self.applied.contains(&change.id) {
            let at = self.log.iter().find(|e| e.change == change.id).map_or(self.version, |e| e.version);
            return Outcome::AlreadyApplied(at);
        }
        let Some(since) = self.since(change.base) else {
            return Outcome::Resync;
        };
        if let Err(e) = check_size(&change.patch) {
            return Outcome::Refused(e.to_string());
        }
        let mut patch = change.patch.clone();
        let mut conflicts = Vec::new();
        for e in &since {
            let r = rebase(&patch, &e.patch, Some(&e.author));
            conflicts.extend(r.conflicts);
            patch = r.patch;
        }
        if !conflicts.is_empty() {
            return Outcome::Conflicts(conflicts);
        }
        let before = &self.project;
        let after = match guard::catch("shared change", || settle(apply(before, &patch))) {
            Ok(Ok(p)) => p,
            Ok(Err(e)) => return Outcome::Refused(e),
            Err(p) => return Outcome::Refused(format!("a bug was caught: {}", p.message)),
        };
        // New items take ids from leased blocks, all below the host's counters: an id above them
        // was never handed out, and taking it would use up ids for good (up to `MAX_ID`).
        if let Some(kind) = beyond_counters(&after, &before.ids) {
            return Outcome::Refused(format!("a new {kind} has an id that was never handed out"));
        }
        if let Err(reason) = check(&after) {
            return Outcome::Refused(reason);
        }
        let effect = match diff(before, &after) {
            Ok(p) => p,
            Err(e) => return Outcome::Refused(e.to_string()),
        };
        self.version = self.version.saturating_add(1);
        let short = |s: &str| s.chars().take(MAX_NAME_CHARS).collect::<String>();
        let entry = Entry { version: self.version, change: change.id, author: short(author), label: short(&change.label), patch: effect };
        self.project = after;
        self.remember(entry.clone());
        Outcome::Applied(entry)
    }

    /// Applies an entry the host accepted earlier (replaying its journal after a restart).
    pub fn replay(&mut self, entry: &Entry) -> Result<(), String> {
        if entry.version != self.version.saturating_add(1) {
            return Err(SyncError::Gap.to_string());
        }
        let after = guard::catch("replaying a shared change", || settle(apply(&self.project, &entry.patch))).map_err(|p| p.message)??;
        self.project = after;
        self.version = entry.version;
        self.remember(entry.clone());
        Ok(())
    }

    fn note_applied(&mut self, id: ChangeId) {
        if self.applied.insert(id) {
            self.applied_order.push_back(id);
        }
        while self.applied_order.len() > APPLIED_MEMORY {
            if let Some(old) = self.applied_order.pop_front() {
                self.applied.remove(&old);
            }
        }
    }

    /// Logs an entry. The log keeps the last [`LOG_LIMIT`] entries and at most [`LOG_BYTES`] of
    /// them (the newest always), so large changes cannot fill the memory; a replica further
    /// behind resyncs from a snapshot.
    fn remember(&mut self, entry: Entry) {
        self.note_applied(entry.change);
        let size = serde_json::to_vec(&entry.patch).map_or(usize::MAX, |b| b.len());
        self.log.push_back(entry);
        self.log_sizes.push_back(size);
        self.log_bytes = self.log_bytes.saturating_add(size);
        while self.log.len() > LOG_LIMIT || (self.log_bytes > self.log_bytes_limit && self.log.len() > 1) {
            self.log.pop_front();
            if let Some(old) = self.log_sizes.pop_front() {
                self.log_bytes = self.log_bytes.saturating_sub(old);
            }
        }
    }
}

/// The first kind of item whose ids go above `counters`, if any.
fn beyond_counters(p: &Project, counters: &IdCounters) -> Option<&'static str> {
    fn over(mut ids: impl Iterator<Item = u32>, max: u32) -> bool {
        ids.any(|id| id > max)
    }
    [
        ("task", over(p.tasks.iter().map(|t| t.id.0), counters.task)),
        ("resource", over(p.resources.iter().map(|r| r.id.0), counters.resource)),
        ("calendar", over(p.calendars.iter().map(|c| c.id.0), counters.calendar)),
        ("assignment", over(p.assignments.iter().map(|a| a.id.0), counters.assignment)),
        ("time entry", over(p.time_entries.iter().map(|e| e.id.0), counters.time_entry)),
        ("custom field", over(p.custom_fields.iter().map(|f| f.id.0), counters.custom_field)),
        ("board column", over(p.board_columns.iter().map(|c| c.id.0), counters.board_column)),
    ]
    .into_iter()
    .find_map(|(kind, over)| over.then_some(kind))
}

/// Validates and recalculates a patched plan, the same way every edit is checked.
pub(super) fn settle(patched: Result<Project, SyncError>) -> Result<Project, String> {
    let mut p = patched.map_err(|e| e.to_string())?;
    limits::check_edit(&mut p).map_err(|e| e.to_string())?;
    schedule::try_recalculate(&mut p).map_err(|e| e.message)?;
    fit_counters(&mut p);
    Ok(p)
}
