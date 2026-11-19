//! One person's copy of a shared plan.

use super::host::{Change, Entry, settle};
use super::patch::{apply, diff, rebase};
use super::{ChangeId, Collection, Conflict, ConflictKind, LEASE_SIZE, ListPatch, Patch, SyncError, Version};
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
    /// Where the current lease started (it holds [`LEASE_SIZE`] ids of each kind).
    lease_start: IdCounters,
    /// No lease yet (the host had none to give): no new items until one arrives.
    unleased: bool,
    /// Where the lease before the current one started: its ids not used yet are still this
    /// replica's (an edit made before the new lease arrived counts on from the old one).
    previous_lease: Option<IdCounters>,
}

impl Replica {
    /// Starts from the host's plan at `version`, counting new ids from the lease `ids`.
    pub fn new(project: Project, version: Version, ids: IdCounters) -> Self {
        let mut local = project.clone();
        local.ids = ids.clone();
        Self {
            confirmed: project,
            version,
            local,
            pending: Patch::default(),
            in_flight: None,
            lease_start: ids.clone(),
            ids,
            unleased: false,
            previous_lease: None,
        }
    }

    /// Starts from the host's plan at `version` with no lease (a viewer's copy, or an editor the
    /// host had no lease for yet): [`Replica::ids_left`] is 0 and any new item is
    /// [`Replica::beyond_lease`] until [`Replica::lease`] gives a block.
    pub fn without_lease(project: Project, version: Version) -> Self {
        let ids = project.ids.clone();
        let mut r = Self::new(project, version, ids);
        r.unleased = true;
        r
    }

    /// Whether this copy has a lease to count new ids from.
    pub fn leased(&self) -> bool {
        !self.unleased
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

    /// How many new ids of the scarcest kind are left in the lease (ask for another when low).
    pub fn ids_left(&self) -> u32 {
        if self.unleased {
            return 0;
        }
        let (start, now) = (&self.lease_start, &self.ids);
        [
            (start.task, now.task),
            (start.resource, now.resource),
            (start.calendar, now.calendar),
            (start.assignment, now.assignment),
            (start.time_entry, now.time_entry),
            (start.custom_field, now.custom_field),
            (start.board_column, now.board_column),
        ]
        .into_iter()
        .map(|(s, n)| s.saturating_add(LEASE_SIZE).saturating_sub(n))
        .min()
        .unwrap_or(0)
    }

    /// Whether counters `ids` (a local edit's) reach past the current lease: the edit added more
    /// new items than the lease holds, and the host would refuse ids no lease handed out.
    pub fn beyond_lease(&self, ids: &IdCounters) -> bool {
        let start = &self.lease_start;
        // Without a lease nothing may be added: any counter past where the copy started is.
        let size = if self.unleased { 0 } else { LEASE_SIZE };
        [
            (start.task, ids.task),
            (start.resource, ids.resource),
            (start.calendar, ids.calendar),
            (start.assignment, ids.assignment),
            (start.time_entry, ids.time_entry),
            (start.custom_field, ids.custom_field),
            (start.board_column, ids.board_column),
        ]
        .into_iter()
        .any(|(s, n)| n > s.saturating_add(size))
    }

    /// Whether `after`, an edit made on a plan whose id counters were `base`, holds a new item
    /// (an id above `base`'s counter) that neither the current lease nor the one before it handed
    /// out. Such an id may be in someone else's block: an edit made before a new lease arrived
    /// counts on from the old lease, past its end.
    pub fn new_ids_outside_leases(&self, base: &IdCounters, after: &Project) -> bool {
        let size = if self.unleased { 0 } else { LEASE_SIZE };
        let leases = (&self.lease_start, size, self.previous_lease.as_ref());
        fn outside(
            mut ids: impl Iterator<Item = u32>,
            kind: fn(&IdCounters) -> u32,
            base: &IdCounters,
            leases: (&IdCounters, u32, Option<&IdCounters>),
        ) -> bool {
            let (current, size, previous) = leases;
            let within = |id: u32, start: &IdCounters, size: u32| id > kind(start) && id <= kind(start).saturating_add(size);
            ids.any(|id| id > kind(base) && !within(id, current, size) && !previous.is_some_and(|p| within(id, p, LEASE_SIZE)))
        }
        outside(after.tasks.iter().map(|t| t.id.0), |c| c.task, base, leases)
            || outside(after.resources.iter().map(|r| r.id.0), |c| c.resource, base, leases)
            || outside(after.calendars.iter().map(|c| c.id.0), |c| c.calendar, base, leases)
            || outside(after.assignments.iter().map(|a| a.id.0), |c| c.assignment, base, leases)
            || outside(after.time_entries.iter().map(|e| e.id.0), |c| c.time_entry, base, leases)
            || outside(after.custom_fields.iter().map(|f| f.id.0), |c| c.custom_field, base, leases)
            || outside(after.board_columns.iter().map(|c| c.id.0), |c| c.board_column, base, leases)
    }

    /// Takes a new block of ids from the host. A block starting below where this replica's ids
    /// already are is refused (the host lost track of a lease, see `Host::replay_lease`):
    /// counting from it would hand out ids this replica already used.
    pub fn lease(&mut self, ids: IdCounters) -> Result<(), SyncError> {
        let (new, now) = (&ids, &self.ids);
        let behind = [
            (new.task, now.task),
            (new.resource, now.resource),
            (new.calendar, now.calendar),
            (new.assignment, now.assignment),
            (new.time_entry, now.time_entry),
            (new.custom_field, now.custom_field),
            (new.board_column, now.board_column),
        ]
        .into_iter()
        .any(|(n, c)| n < c);
        if behind {
            return Err(SyncError::Shape("the new block of ids starts below ids already used".into()));
        }
        self.local.ids = ids.clone();
        self.previous_lease = (!self.unleased).then(|| self.lease_start.clone());
        self.lease_start = ids.clone();
        self.ids = ids;
        self.unleased = false;
        Ok(())
    }

    /// Takes the plan after a local edit (already checked and recalculated by `Document::edit`).
    ///
    /// The id cursor never goes back: an undo restores a snapshot with older counters, and
    /// counting from those again would hand out ids this replica already used.
    ///
    /// Whatever plan comes in is taken as the person's intent: a whole-plan undo snapshot taken
    /// before other people's changes arrived would undo theirs too. Shared plans must undo with
    /// inverse patches instead (docs/COLLABORATION.md › Undo).
    pub fn edited(&mut self, mut local: Project) -> Result<(), SyncError> {
        self.pending = diff(&self.confirmed, &local)?;
        let (ids, seen) = (&mut local.ids, &self.ids);
        for (c, s) in [
            (&mut ids.task, seen.task),
            (&mut ids.resource, seen.resource),
            (&mut ids.calendar, seen.calendar),
            (&mut ids.assignment, seen.assignment),
            (&mut ids.time_entry, seen.time_entry),
            (&mut ids.custom_field, seen.custom_field),
            (&mut ids.board_column, seen.board_column),
        ] {
            *c = (*c).max(s);
        }
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
        // The host rebases the change on its way over this entry too: keep the copy in step, so a
        // resync after a lost reply knows what the host applied.
        if let Some((_, sent)) = &mut self.in_flight {
            *sent = rebase(sent, &entry.patch, None).patch;
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

    /// The change on its way to the host, if any (after a reconnection, ask the host whether it
    /// went in: [`super::Host::was_applied`]).
    pub fn in_flight(&self) -> Option<ChangeId> {
        self.in_flight.as_ref().map(|(id, _)| *id)
    }

    /// Starts again from the host's plan (after missed changes or a reconnection), keeping the
    /// unconfirmed work that does not clash with what changed meanwhile.
    ///
    /// `in_flight_applied` says whether the host accepted the change that was on its way (its
    /// reply may have been lost). It matters: an accepted change that someone else then changed
    /// back would otherwise look unconfirmed and be sent again, silently undoing their change.
    /// Prefer catching up with [`super::Host::since`] when the host still has the entries: they
    /// confirm the change by its id.
    pub fn resync(&mut self, project: Project, version: Version, in_flight_applied: bool) -> Result<Merged, SyncError> {
        // What the host's plan grew from: the confirmed plan, plus the change on its way if the
        // host accepted it; and what is still unconfirmed on top of that.
        let (base, pending) = match self.in_flight.take() {
            Some((_, sent)) if in_flight_applied => match settle(apply(&self.confirmed, &sent)) {
                Ok(with_sent) => {
                    let rest = diff(&with_sent, &self.local)?;
                    (with_sent, rest)
                }
                // It no longer applies to the old plan as it was sent: the host's plan has it,
                // so whatever of it still differs is left to the comparison below.
                Err(_) => (self.confirmed.clone(), self.pending.clone()),
            },
            _ => (self.confirmed.clone(), self.pending.clone()),
        };
        let theirs = diff(&base, &project)?;
        self.confirmed = project;
        self.version = version;
        let r = rebase(&pending, &theirs, None);
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
            let parents = lp.parents.get(k).map(|p| (k.clone(), p.clone())).into_iter().collect();
            let piece = ListPatch { added: [(k.clone(), v.clone())].into(), parents, ..ListPatch::default() };
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
        to.parents.extend(lp.parents.iter().map(|(k, v)| (k.clone(), v.clone())));
        if lp.order.is_some() {
            to.order.clone_from(&lp.order);
            to.moved = lp.moved;
        }
    }
    out
}
