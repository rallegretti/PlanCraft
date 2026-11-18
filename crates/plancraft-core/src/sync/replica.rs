//! One person's copy of a shared plan.

use super::host::{Change, Entry, settle};
use super::patch::{apply, diff, rebase};
use super::{ChangeId, Collection, Conflict, ConflictKind, ListPatch, Patch, SyncError, Version};
use crate::ids::IdCounters;
use crate::model::Project;

/// What happened to a replica's unconfirmed work when changes came in.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Merged {
    /// Parts of the unconfirmed work that clashed with the incoming changes and were dropped.
    pub conflicts: Vec<Conflict>,
    /// All unconfirmed work was dropped, and why (the merged plan did not pass the checks).
    pub discarded: Option<String>,
}

/// The host's plan as last confirmed, plus this person's changes it has not confirmed yet.
///
/// The person edits [`Replica::local`] (through `Document::edit`, as always) and hands the result
/// to [`Replica::edited`]. [`Replica::next_change`] gives the change to send; the host's entries
/// go to [`Replica::receive`]. At most one change is on its way at a time; edits made meanwhile go
/// out together with the next one.
#[derive(Clone, Debug)]
pub struct Replica {
    confirmed: Project,
    version: Version,
    /// `confirmed` with `pending` applied: what the person sees.
    local: Project,
    /// Everything in `local` the host has not confirmed.
    pending: Patch,
    /// The change on its way to the host, and what it carried.
    in_flight: Option<(ChangeId, Patch)>,
    /// Where this replica's leased ids continue (see [`super::Host::lease`]).
    ids: IdCounters,
}

impl Replica {
    /// Starts from the host's plan at `version`, counting new ids from the lease `ids`.
    pub fn new(project: Project, version: Version, ids: IdCounters) -> Self {
        let mut local = project.clone();
        local.ids = ids.clone();
        Self { confirmed: project, version, local, pending: Patch::default(), in_flight: None, ids }
    }

    /// The plan as this person sees it.
    pub fn local(&self) -> &Project {
        &self.local
    }

    /// The plan as the host last confirmed it.
    pub fn confirmed(&self) -> &Project {
        &self.confirmed
    }

    pub fn version(&self) -> Version {
        self.version
    }

    /// Changes the host has not confirmed yet.
    pub fn pending(&self) -> &Patch {
        &self.pending
    }

    /// A change is on its way to the host.
    pub fn waiting(&self) -> bool {
        self.in_flight.is_some()
    }

    /// Where new ids continue: when they near the end of the lease, ask the host for another.
    pub fn ids(&self) -> &IdCounters {
        &self.ids
    }

    /// Takes a new block of ids from the host.
    pub fn lease(&mut self, ids: IdCounters) {
        self.local.ids = ids.clone();
        self.ids = ids;
    }

    /// Takes the plan after a local edit (already checked and recalculated by `Document::edit`).
    pub fn edited(&mut self, local: Project) -> Result<(), SyncError> {
        self.pending = diff(&self.confirmed, &local)?;
        self.ids = local.ids.clone();
        self.local = local;
        Ok(())
    }

    /// The change to send now: everything unconfirmed, unless a change is already on its way.
    pub fn next_change(&mut self, label: &str) -> Option<Change> {
        if self.in_flight.is_some() || self.pending.is_empty() {
            return None;
        }
        let id = ChangeId::new();
        self.in_flight = Some((id, self.pending.clone()));
        Some(Change { id, base: self.version, label: label.to_string(), patch: self.pending.clone() })
    }

    /// Applies an entry from the host. Entries must come in order: a missing one is
    /// [`SyncError::Gap`], after which the replica needs [`Replica::resync`].
    pub fn receive(&mut self, entry: &Entry) -> Result<Merged, SyncError> {
        if entry.version <= self.version {
            return Ok(Merged::default());
        }
        if entry.version != self.version.saturating_add(1) {
            return Err(SyncError::Gap);
        }
        let confirmed = settle(apply(&self.confirmed, &entry.patch)).map_err(SyncError::Shape)?;
        self.confirmed = confirmed;
        self.version = entry.version;
        if self.in_flight.as_ref().is_some_and(|(id, _)| *id == entry.change) {
            // Our own change came back: what remains unconfirmed is whatever the person did since.
            self.in_flight = None;
            self.pending = diff(&self.confirmed, &self.local)?;
            return Ok(Merged::default());
        }
        let r = rebase(&self.pending, &entry.patch, Some(&entry.author));
        Ok(self.rebuild(r.patch, r.conflicts))
    }

    /// The host would not apply the change on its way. Clashes were already dropped when the
    /// entries they clashed with came in. A refused change was already trimmed to what still fits
    /// when those entries came in; if nothing changed since it went out, the host refused it for
    /// a reason this copy cannot see, and all unconfirmed work is dropped (rather than sending
    /// it again and again).
    pub fn rejected(&mut self, change: ChangeId, refused: Option<&str>) -> Merged {
        let Some((id, sent)) = self.in_flight.take() else {
            return Merged::default();
        };
        if id != change {
            self.in_flight = Some((id, sent));
            return Merged::default();
        }
        match refused {
            Some(why) if sent == self.pending => self.rebuild(Patch::default(), Vec::new()).with_discarded(why),
            _ => Merged::default(),
        }
    }

    /// Starts again from the host's plan (after missed changes or a reconnection), keeping the
    /// unconfirmed work that does not clash with what changed meanwhile.
    pub fn resync(&mut self, project: Project, version: Version) -> Result<Merged, SyncError> {
        let theirs = diff(&self.confirmed, &project)?;
        self.confirmed = project;
        self.version = version;
        self.in_flight = None;
        let r = rebase(&self.pending, &theirs, None);
        Ok(self.rebuild(r.patch, r.conflicts))
    }

    /// Makes `local` the confirmed plan with `pending` applied. If that fails the checks, keeps
    /// the pieces of `pending` that still fit ([`salvage`]) and reports the others.
    fn rebuild(&mut self, pending: Patch, conflicts: Vec<Conflict>) -> Merged {
        let mut merged = Merged { conflicts, discarded: None };
        let local = if pending.is_empty() { Ok(self.confirmed.clone()) } else { settle(apply(&self.confirmed, &pending)) };
        match local {
            Ok(local) => {
                self.local = local;
                self.pending = pending;
            }
            Err(why) => match salvage(&self.confirmed, &pending) {
                Some((kept, local, dropped)) => {
                    self.local = local;
                    self.pending = kept;
                    merged.conflicts.extend(dropped);
                }
                None => {
                    self.local = self.confirmed.clone();
                    self.pending = Patch::default();
                    merged.discarded = Some(why);
                }
            },
        }
        self.local.ids = self.ids.clone();
        // What the checks normalised (or what clashed) may leave the local plan different from
        // the rebased patch; the plan the person sees is what will be sent.
        if let Ok(p) = diff(&self.confirmed, &self.local) {
            self.pending = p;
        }
        merged
    }
}

impl Merged {
    fn with_discarded(mut self, why: &str) -> Self {
        self.discarded = Some(why.to_string());
        self
    }
}

/// Pieces of unconfirmed work tried one by one when the whole no longer fits.
const MAX_SALVAGE_PIECES: usize = 200;

/// Lists in the order new items are tried: what other items refer to comes first.
const ADD_ORDER: [Collection; 9] = [
    Collection::Calendars,
    Collection::CustomFields,
    Collection::BoardColumns,
    Collection::Resources,
    Collection::ExternalProjects,
    Collection::Tasks,
    Collection::Assignments,
    Collection::TimeEntries,
    Collection::Tables,
];

/// Splits `pending` into pieces (a top-level value, one new item, one item's changes, one
/// deletion, a list's order) and keeps those that still fit `confirmed`, trying the rest a second
/// time (a piece may need one tried after it). Returns the kept patch, the plan with it applied
/// and a conflict per dropped piece; `None` when there are too many pieces or nothing fits.
fn salvage(confirmed: &Project, pending: &Patch) -> Option<(Patch, Project, Vec<Conflict>)> {
    let pieces = pieces(pending);
    if pieces.len() > MAX_SALVAGE_PIECES {
        return None;
    }
    let mut kept = Patch::default();
    let mut local = confirmed.clone();
    let mut left = pieces;
    for _ in 0..2 {
        let mut failed = Vec::new();
        for (piece, conflict) in left {
            let candidate = combine(&kept, &piece);
            match settle(apply(confirmed, &candidate)) {
                Ok(p) => {
                    kept = candidate;
                    local = p;
                }
                Err(_) => failed.push((piece, conflict)),
            }
        }
        left = failed;
    }
    if kept.is_empty() {
        return None;
    }
    Some((kept, local, left.into_iter().map(|(_, c)| c).collect()))
}

fn no_longer_fits(list: Option<Collection>, key: Option<&super::Key>, field: Option<&str>) -> Conflict {
    Conflict { kind: ConflictKind::NoLongerFits, list, key: key.cloned(), field: field.map(str::to_string), by: None }
}

fn pieces(p: &Patch) -> Vec<(Patch, Conflict)> {
    let one = |list: Collection, lp: ListPatch| Patch { lists: [(list, lp)].into(), ..Patch::default() };
    let mut out = Vec::new();
    for list in ADD_ORDER {
        let Some(lp) = p.lists.get(&list) else { continue };
        for (k, v) in &lp.added {
            let piece = ListPatch { added: [(k.clone(), v.clone())].into(), ..ListPatch::default() };
            out.push((one(list, piece), no_longer_fits(Some(list), Some(k), None)));
        }
    }
    for (k, v) in &p.fields {
        let piece = Patch { fields: [(k.clone(), v.clone())].into(), ..Patch::default() };
        out.push((piece, no_longer_fits(None, None, Some(k))));
    }
    for list in ADD_ORDER {
        let Some(lp) = p.lists.get(&list) else { continue };
        for (k, fields) in &lp.changed {
            let piece = ListPatch { changed: [(k.clone(), fields.clone())].into(), ..ListPatch::default() };
            out.push((one(list, piece), no_longer_fits(Some(list), Some(k), None)));
        }
    }
    // Deletions go the other way round: an assignment before its task.
    for list in ADD_ORDER.into_iter().rev() {
        let Some(lp) = p.lists.get(&list) else { continue };
        for k in &lp.removed {
            let piece = ListPatch { removed: [k.clone()].into(), ..ListPatch::default() };
            out.push((one(list, piece), no_longer_fits(Some(list), Some(k), None)));
        }
    }
    for list in ADD_ORDER {
        let Some(lp) = p.lists.get(&list) else { continue };
        if let Some(order) = &lp.order {
            let piece = ListPatch { order: Some(order.clone()), moved: lp.moved, ..ListPatch::default() };
            out.push((one(list, piece), no_longer_fits(Some(list), None, None)));
        }
    }
    out
}

/// `a` and then `b`, for pieces of one patch (they never touch the same thing).
fn combine(a: &Patch, b: &Patch) -> Patch {
    let mut out = a.clone();
    out.fields.extend(b.fields.iter().map(|(k, v)| (k.clone(), v.clone())));
    for (list, lp) in &b.lists {
        let to = out.lists.entry(*list).or_default();
        to.added.extend(lp.added.iter().map(|(k, v)| (k.clone(), v.clone())));
        to.changed.extend(lp.changed.iter().map(|(k, v)| (k.clone(), v.clone())));
        to.removed.extend(lp.removed.iter().cloned());
        if lp.order.is_some() {
            to.order.clone_from(&lp.order);
            to.moved = lp.moved;
        }
    }
    out
}
