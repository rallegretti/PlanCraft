//! The host's copy of a shared plan: the one order of changes everyone follows.

use super::patch::{apply, diff, fit_counters, rebase};
use super::{ChangeId, Conflict, LEASE_SIZE, LOG_LIMIT, Patch, SyncError, Version};
use crate::ids::IdCounters;
use crate::limits::{self, MAX_ID};
use crate::model::Project;
use crate::{guard, schedule};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

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
    /// The change was accepted before (it was sent twice) and made this version.
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
}

impl Host {
    /// Shares a plan (as opened and recalculated) at version 1.
    pub fn new(mut project: Project) -> Self {
        fit_counters(&mut project);
        Self { project, version: 1, log: VecDeque::new() }
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

    /// Rebases, checks and applies a change from `author`.
    pub fn submit(&mut self, change: &Change, author: &str) -> Outcome {
        if let Some(e) = self.log.iter().find(|e| e.change == change.id) {
            return Outcome::AlreadyApplied(e.version);
        }
        let Some(since) = self.since(change.base) else {
            return Outcome::Resync;
        };
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
        let effect = match diff(before, &after) {
            Ok(p) => p,
            Err(e) => return Outcome::Refused(e.to_string()),
        };
        self.version = self.version.saturating_add(1);
        let entry =
            Entry { version: self.version, change: change.id, author: author.to_string(), label: change.label.clone(), patch: effect };
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

    fn remember(&mut self, entry: Entry) {
        self.log.push_back(entry);
        while self.log.len() > LOG_LIMIT {
            self.log.pop_front();
        }
    }
}

/// Validates and recalculates a patched plan, the same way every edit is checked.
pub(super) fn settle(patched: Result<Project, SyncError>) -> Result<Project, String> {
    let mut p = patched.map_err(|e| e.to_string())?;
    limits::check_edit(&mut p).map_err(|e| e.to_string())?;
    schedule::try_recalculate(&mut p).map_err(|e| e.message)?;
    fit_counters(&mut p);
    Ok(p)
}
