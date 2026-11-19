//! Open documents with undo/redo, shared by the desktop app and the MCP server.
//!
//! The desktop app and its embedded MCP server share one [`Workspace`] behind
//! a mutex, so changes made by an AI agent appear live in the UI and can be
//! undone like any user edit.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::guard;
use crate::io::{self, FileResult};
use crate::limits;
use crate::lock::{self, LockState};
use crate::model::Project;
use crate::ops::{PlanError, PlanResult};
use crate::schedule::{recalculate, try_recalculate};

const UNDO_LIMIT: usize = 200;
/// Memory the undo history of one document may use (estimated). Every step is a whole-project
/// snapshot, so 200 steps of a plan near the size limits would need gigabytes; older steps are
/// dropped first, and the latest step is always kept.
/// (Unit tests use a small budget so a small plan exercises the trimming.)
const UNDO_BUDGET_BYTES: usize = if cfg!(test) { 4 * 1024 * 1024 } else { 256 * 1024 * 1024 };

/// Rough heap size of a project: proportional to its items and text, cheap to compute.
fn approx_bytes(p: &Project) -> usize {
    use std::mem::size_of;
    let text = |s: &String| s.len();
    let custom_value = |v: &crate::custom::CustomValue| {
        size_of::<crate::custom::CustomValue>() + if let crate::custom::CustomValue::Text(s) = v { s.len() } else { 0 }
    };
    let tasks: usize = p
        .tasks
        .iter()
        .map(|t| {
            size_of::<crate::model::Task>()
                + text(&t.name)
                + text(&t.notes)
                + text(&t.deliverable)
                + t.predecessors.len() * size_of::<crate::model::Dependency>()
                + t.members.len() * size_of::<crate::model::TaskRef>()
                + t.splits.len() * size_of::<crate::model::Split>()
                + t.gaps.len() * size_of::<crate::model::Gap>()
                + t.baselines.len() * (size_of::<crate::model::Baseline>() + 32)
                + t.interim.len() * (size_of::<crate::model::Interim>() + 32)
                + t.sched.parts.len() * size_of::<(crate::dates::DateTime, crate::dates::DateTime)>()
                + t.sched.warnings.iter().map(|w| size_of::<String>() + w.len()).sum::<usize>()
                + t.custom.values().map(custom_value).sum::<usize>()
        })
        .sum();
    let resources: usize = p
        .resources
        .iter()
        .map(|r| {
            size_of::<crate::model::Resource>()
                + text(&r.name)
                + text(&r.initials)
                + text(&r.group)
                + text(&r.email)
                + text(&r.material_label)
                + text(&r.notes)
                + r.availability.len() * size_of::<crate::model::AvailabilityPeriod>()
                + r.exceptions.iter().map(|e| size_of::<crate::calendar::CalendarException>() + e.name.len()).sum::<usize>()
        })
        .sum();
    let fields: usize = p
        .custom_fields
        .iter()
        .map(|f| {
            size_of::<crate::custom::CustomFieldDef>()
                + text(&f.name)
                + f.choices.iter().map(|c| size_of::<String>() + c.len()).sum::<usize>()
        })
        .sum();
    let tables: usize = p
        .tables
        .iter()
        .map(|t| {
            size_of::<crate::custom::TableLayout>() + text(&t.name) + t.columns.iter().map(|c| size_of::<String>() + c.len()).sum::<usize>()
        })
        .sum();
    let calendars: usize = p
        .calendars
        .iter()
        .map(|c| {
            size_of::<crate::calendar::Calendar>()
                + text(&c.name)
                + c.exceptions.iter().map(|e| size_of::<crate::calendar::CalendarException>() + e.name.len()).sum::<usize>()
        })
        .sum();
    let external: usize = p
        .external_projects
        .iter()
        .map(|e| e.path.len() + e.tasks.iter().map(|t| size_of::<crate::model::ExternalTaskSnapshot>() + t.name.len()).sum::<usize>())
        .sum();
    size_of::<Project>()
        + text(&p.notes)
        + tasks
        + resources
        + calendars
        + external
        + fields
        + tables
        + text(&p.name)
        + text(&p.manager)
        + text(&p.company)
        + text(&p.author)
        + p.assignments.len() * size_of::<crate::model::Assignment>()
        + p.time_entries.iter().map(|e| size_of::<crate::model::TimeEntry>() + e.note.len()).sum::<usize>()
}

/// The undo label of a reload from disk.
pub const RELOAD_LABEL: &str = "Reload from disk";

/// How an open document's file compares with the version the document matches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiskStatus {
    /// The document has no file (new or imported, never saved).
    NoFile,
    /// The file is the version last loaded or saved.
    InSync,
    /// Another program (or PlanCraft instance, git, a sync tool) changed the file since; or the
    /// document was never read from it (a restored recovery copy), so its version is unknown.
    Changed,
    /// The file is gone (deleted, moved or renamed); saving writes it again.
    Missing,
}

impl DiskStatus {
    /// Short name used by the MCP tools.
    pub fn as_str(self) -> &'static str {
        match self {
            DiskStatus::NoFile => "no_file",
            DiskStatus::InSync => "in_sync",
            DiskStatus::Changed => "changed",
            DiskStatus::Missing => "missing",
        }
    }
}

/// How the file at `path` compares with `known`, the version a document matches (see
/// [`Document::disk_status`]; this form needs no lock).
pub fn disk_status(path: Option<&Path>, known: Option<io::DiskStamp>) -> DiskStatus {
    let Some(path) = path else { return DiskStatus::NoFile };
    match (io::disk_stamp(path), known) {
        (None, Some(_)) => DiskStatus::Missing,
        (None, None) => DiskStatus::NoFile,
        (Some(now), Some(known)) if now == known => DiskStatus::InSync,
        (Some(_), _) => DiskStatus::Changed,
    }
}

/// Reads a document's file for a reload: the project, its load notes and the file's stamp taken
/// before reading. A file that changes while it is read (a program still writing it) is read
/// again, a few times at most, so a half-written file is not taken for the new version.
pub fn read_for_reload(path: &Path) -> FileResult<(Project, Vec<String>, Option<io::DiskStamp>)> {
    let mut last = None;
    for _ in 0..3 {
        let stamp = io::disk_stamp(path);
        let read = guard::catch("reading a plan", || io::load_with_notes(path))
            .unwrap_or_else(|p| Err(io::FileError::Limit { path: path.into(), message: p.to_string() }));
        if io::disk_stamp(path) == stamp {
            return read.map(|(p, notes)| (p, notes, stamp));
        }
        last = Some(read.map(|(p, notes)| (p, notes, None)));
    }
    // Still changing: report what the last read found, with no stamp (so a later save asks).
    last.unwrap_or_else(|| Err(io::FileError::ChangedOnDisk { path: path.into() }))
}

/// Who made a change (shown in the undo history and status bar).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ChangeSource {
    #[default]
    User,
    Agent,
}

#[derive(Clone, Debug)]
struct Snapshot {
    label: String,
    source: ChangeSource,
    /// The plan before the step.
    project: Project,
    /// The plan right after the step, in a shared plan: undoing takes back only what the step
    /// changed (`sync::reverse`), not other people's changes made since.
    after: Option<Project>,
    /// Estimated size, computed once when the snapshot is taken.
    bytes: usize,
}

impl Snapshot {
    fn new(label: String, source: ChangeSource, project: Project, after: Option<Project>) -> Self {
        let bytes = approx_bytes(&project).saturating_add(after.as_ref().map_or(0, approx_bytes));
        Self { label, source, project, after, bytes }
    }
}

/// A plan opened from a server (docs/COLLABORATION.md › Phase 3): its edits go to the server's
/// copy, not to a file. The app hands them on ([`Document::take_unsent`]) and puts other people's
/// changes in with [`Document::apply_remote`].
#[derive(Clone, Debug, Default)]
pub struct Shared {
    /// The plan's name on the server (`Plan.plancraft`).
    pub name: String,
    /// Why it cannot be changed (only allowed to view it, signed out), if so.
    pub read_only: Option<String>,
    /// The plan before the edits made since they were last handed on.
    unsent: Option<Project>,
}

impl Shared {
    pub fn new(name: &str) -> Self {
        Self { name: name.to_string(), read_only: None, unsent: None }
    }
}

/// An open project document.
#[derive(Clone, Debug)]
pub struct Document {
    /// Session-unique handle.
    pub handle: u64,
    pub path: Option<PathBuf>,
    pub project: Project,
    pub dirty: bool,
    /// Unsaved changes include some the user made (not only an agent's). An agent may discard
    /// its own unsaved work, never the user's.
    pub user_changes: bool,
    /// Incremented on every change (edits, undo, redo, reload).
    pub revision: u64,
    pub last_change: Option<(String, ChangeSource)>,
    /// What opening the file adjusted (values clamped into range), shown once by the app.
    pub load_notes: Vec<String>,
    /// The plan was imported from another format (Microsoft Project XML): it has no file of its
    /// own yet, and its load notes say what the import could not carry over.
    pub imported: bool,
    /// The file an imported plan came from (opening it again shows this document while it is
    /// still unsaved, instead of importing a second copy).
    pub imported_from: Option<PathBuf>,
    /// The file as it was when last loaded or saved; `save` refuses to overwrite a file that has
    /// changed since (another program or PlanCraft instance wrote it).
    disk_stamp: Option<io::DiskStamp>,
    /// Whether this PlanCraft holds the plan's lock file, or another one has the plan open
    /// ([`crate::lock`]).
    pub lock: LockState,
    /// Opened from a server rather than from a file.
    pub shared: Option<Shared>,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
}

impl Document {
    pub fn new(handle: u64, project: Project, path: Option<PathBuf>) -> Self {
        Self {
            handle,
            path,
            project,
            dirty: false,
            user_changes: false,
            revision: 1,
            last_change: None,
            load_notes: Vec::new(),
            imported: false,
            imported_from: None,
            disk_stamp: None,
            lock: LockState::None,
            shared: None,
            undo: Vec::new(),
            redo: Vec::new(),
        }
    }

    /// A plan opened from a server, named `name` there.
    pub fn new_shared(handle: u64, project: Project, name: &str) -> Self {
        let mut doc = Self::new(handle, project, None);
        doc.shared = Some(Shared::new(name));
        doc
    }

    /// The edits made since they were last handed on: the plan before them and as it is now.
    /// `None` when there are none (or the plan is not shared).
    pub fn take_unsent(&mut self) -> Option<(Project, Project)> {
        let before = self.shared.as_mut()?.unsent.take()?;
        Some((before, self.project.clone()))
    }

    /// Edits not handed on yet.
    pub fn has_unsent(&self) -> bool {
        self.shared.as_ref().is_some_and(|s| s.unsent.is_some())
    }

    /// Puts in the plan as the server's copy now has it (other people's changes merged with this
    /// person's): no undo step, and the undo steps stay valid (shared undo takes back only what
    /// each step changed).
    pub fn apply_remote(&mut self, project: Project) {
        self.project = project;
        self.revision = self.revision.wrapping_add(1);
        // The latest change is no longer this person's (or their agent's): the app must not
        // announce an old agent edit again each time someone else's change arrives.
        self.last_change = None;
    }

    /// Notes a change to a shared plan that has to go to the server.
    fn note_unsent(&mut self, before: &Project) {
        if let Some(s) = self.shared.as_mut()
            && s.unsent.is_none()
        {
            s.unsent = Some(before.clone());
        }
    }

    fn refusal(&self) -> Option<PlanError> {
        self.shared.as_ref().and_then(|s| s.read_only.clone()).map(PlanError::Invalid)
    }

    /// Window/tab title: file name (or the plan's name on its server) or project name, with `*`
    /// when modified.
    pub fn title(&self) -> String {
        let shared = self.shared.as_ref().map(|s| Path::new(&s.name));
        let base = self
            .path
            .as_deref()
            .or(shared)
            .and_then(|p| p.file_stem())
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.project.name.clone());
        if self.dirty { format!("{base}*") } else { base }
    }

    fn touched(&mut self, label: &str, source: ChangeSource) {
        self.dirty = true;
        if source == ChangeSource::User {
            self.user_changes = true;
        }
        self.revision = self.revision.wrapping_add(1);
        self.last_change = Some((label.into(), source));
    }

    /// Applies an edit with undo support and recalculates the schedule.
    ///
    /// The edit is validated against [`crate::limits`]; if it leaves the project out of range, or
    /// anything panics while it runs, the project is restored and an error returned.
    pub fn edit<R>(&mut self, label: &str, f: impl FnOnce(&mut Project) -> R) -> PlanResult<R> {
        self.edit_as(label, ChangeSource::User, f)
    }

    pub fn edit_as<R>(&mut self, label: &str, source: ChangeSource, f: impl FnOnce(&mut Project) -> R) -> PlanResult<R> {
        self.try_edit(label, source, |p| Ok::<R, PlanError>(f(p)))
    }

    /// Applies a fallible edit; on error the project is restored and no undo step is recorded.
    pub fn try_edit<R, E: From<PlanError>>(
        &mut self,
        label: &str,
        source: ChangeSource,
        f: impl FnOnce(&mut Project) -> Result<R, E>,
    ) -> Result<R, E> {
        if let Some(refused) = self.refusal() {
            return Err(E::from(refused));
        }
        let before = self.project.clone();
        let project = &mut self.project;
        let outcome = guard::catch(&format!("edit '{label}'"), || -> Result<R, E> {
            let r = f(project)?;
            limits::check_edit(project)?;
            try_recalculate(project).map_err(|p| E::from(PlanError::from(p)))?;
            Ok(r)
        });
        match outcome {
            Ok(Ok(r)) => {
                self.note_unsent(&before);
                let after = self.shared.is_some().then(|| self.project.clone());
                self.undo.push(Snapshot::new(label.into(), source, before, after));
                self.redo.clear();
                self.trim_undo();
                self.touched(label, source);
                Ok(r)
            }
            Ok(Err(e)) => {
                self.project = before;
                Err(e)
            }
            Err(panicked) => {
                self.project = before;
                Err(E::from(PlanError::from(panicked)))
            }
        }
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }
    pub fn undo_label(&self) -> Option<&str> {
        self.undo.last().map(|s| s.label.as_str())
    }
    pub fn redo_label(&self) -> Option<&str> {
        self.redo.last().map(|s| s.label.as_str())
    }

    /// Labels of undoable steps, most recent first, with their source.
    pub fn history(&self) -> Vec<(String, ChangeSource)> {
        self.undo.iter().rev().map(|s| (s.label.clone(), s.source)).collect()
    }

    /// Drops the oldest undo steps beyond the step limit or the memory budget (the latest step
    /// always stays, so the last change can be undone).
    fn trim_undo(&mut self) {
        let excess = self.undo.len().saturating_sub(UNDO_LIMIT);
        if excess > 0 {
            self.undo.drain(..excess);
        }
        let sizes: Vec<usize> = self.undo.iter().map(|s| s.bytes).collect();
        let mut total: usize = sizes.iter().sum();
        let mut drop = 0;
        for size in &sizes {
            if total <= UNDO_BUDGET_BYTES || drop + 1 >= sizes.len() {
                break;
            }
            total = total.saturating_sub(*size);
            drop += 1;
        }
        if drop > 0 {
            self.undo.drain(..drop);
        }
    }

    /// Undoes the last change on the user's behalf.
    pub fn undo(&mut self) -> bool {
        self.undo_as(ChangeSource::User)
    }

    /// Redoes the last undone change on the user's behalf.
    pub fn redo(&mut self) -> bool {
        self.redo_as(ChangeSource::User)
    }

    /// Undoes the last change, attributed to `source` (`false`: nothing to undo, or it was
    /// refused; see [`Document::try_undo_as`]).
    pub fn undo_as(&mut self, source: ChangeSource) -> bool {
        self.try_undo_as(source).unwrap_or(false)
    }

    /// Redoes the last undone change, attributed to `source`.
    pub fn redo_as(&mut self, source: ChangeSource) -> bool {
        self.try_redo_as(source).unwrap_or(false)
    }

    /// Undoes the last change, attributed to `source`. `Ok(false)`: nothing to undo. In a shared
    /// plan it takes back only what that change did, and is refused (with why) when someone
    /// changed the same values since, or the plan may only be viewed.
    pub fn try_undo_as(&mut self, source: ChangeSource) -> Result<bool, String> {
        self.step_back(source, true)
    }

    /// Redoes the last undone change; see [`Document::try_undo_as`].
    pub fn try_redo_as(&mut self, source: ChangeSource) -> Result<bool, String> {
        self.step_back(source, false)
    }

    /// Undo (`undo`) or redo: moves the top step of one stack to the other.
    fn step_back(&mut self, source: ChangeSource, undo: bool) -> Result<bool, String> {
        if let Some(refused) = self.refusal() {
            return Err(refused.to_string());
        }
        let verb = if undo { "undo" } else { "redo" };
        // A step that no longer changes anything (in a shared plan: an edit the server did not
        // keep) is passed over, so one Undo always does something when it can.
        let (restored, s) = loop {
            let from = if undo { &mut self.undo } else { &mut self.redo };
            let Some(s) = from.pop() else {
                return Ok(false);
            };
            let restored = match &s.after {
                // Shared: take back only what the step changed, on the plan as it is now.
                Some(after) => match crate::sync::reverse(&self.project, &s.project, after) {
                    Ok(p) => p,
                    Err(why) => {
                        // Someone changed the same values since: this step can no longer be taken
                        // back (or redone) without undoing theirs. It leaves the list, so the
                        // other steps can still be undone (or redone).
                        let next = if undo { "the step before it" } else { "the step after it" };
                        return Err(format!(
                            "Cannot {verb} {}: {}. It was taken off the {verb} list; {verb} again for {next}.",
                            s.label,
                            why.describe(&self.project)
                        ));
                    }
                },
                None => break (s.project.clone(), s),
            };
            if crate::sync::diff(&self.project, &restored).is_ok_and(|d| d.is_empty()) {
                continue;
            }
            break (restored, s);
        };
        let to = if undo { &mut self.redo } else { &mut self.undo };
        let current = std::mem::replace(&mut self.project, restored);
        let after = s.after.is_some().then(|| self.project.clone());
        to.push(Snapshot::new(s.label.clone(), s.source, current.clone(), after));
        self.note_unsent(&current);
        self.touched(&format!("{} {}", if undo { "Undo" } else { "Redo" }, s.label), source);
        Ok(true)
    }

    /// Saves to the current path. Fails with [`io::FileError::ChangedOnDisk`] when another program
    /// changed the file since it was opened or last saved; [`Document::save_overwriting`] replaces
    /// it anyway.
    pub fn save(&mut self) -> FileResult<()> {
        self.save_checked(true)
    }

    /// Saves to the current path even if the file changed on disk since it was opened.
    pub fn save_overwriting(&mut self) -> FileResult<()> {
        self.save_checked(false)
    }

    /// Whether the file changed on disk since it was opened or last saved (a deleted file has not:
    /// saving simply writes it again).
    pub fn changed_on_disk(&self) -> bool {
        match (&self.path, self.disk_stamp) {
            (Some(path), Some(known)) => io::disk_stamp(path).is_some_and(|now| now != known),
            // A file this document never read or wrote (a restored recovery copy bound to its
            // original): its version is unknown, so treat it as changed rather than overwrite it.
            (Some(path), None) => io::disk_stamp(path).is_some(),
            (None, _) => false,
        }
    }

    /// Records the file's current stamp as the version this document matches (after loading).
    pub fn mark_in_sync_with_disk(&mut self) {
        self.disk_stamp = self.path.as_ref().and_then(io::disk_stamp);
    }

    /// The version of the file this document matches (as last loaded or saved), if known.
    pub fn disk_stamp(&self) -> Option<io::DiskStamp> {
        self.disk_stamp
    }

    /// How the document's file on disk compares with the version the document matches. Reads the
    /// file's metadata: on a slow network share, take [`Document::path`] and
    /// [`Document::disk_stamp`] and call [`disk_status`] without holding the workspace lock.
    pub fn disk_status(&self) -> DiskStatus {
        disk_status(self.path.as_deref(), self.disk_stamp)
    }

    /// Replaces the project with `project`, read from the document's file (whose stamp, taken
    /// before reading, is `stamp`), as an undoable edit: Undo brings back the version that was
    /// open, unsaved changes included. Afterwards the document matches the file: not modified,
    /// and the next save needs no confirmation. `notes` are the file's load notes.
    ///
    /// Callers decide whether unsaved changes may be discarded (an agent may never discard the
    /// user's, see [`Document::user_changes`]).
    pub fn reload_from(
        &mut self,
        project: Project,
        notes: Vec<String>,
        stamp: Option<io::DiskStamp>,
        source: ChangeSource,
    ) -> PlanResult<()> {
        self.edit_as(RELOAD_LABEL, source, |p| *p = project)?;
        self.disk_stamp = stamp;
        self.dirty = false;
        self.user_changes = false;
        self.load_notes = notes;
        // As when the file is opened: its snapshots of linked plans may be older than those files.
        let _ = self.refresh_external_on_open();
        Ok(())
    }

    /// Reads the document's file again and replaces the project with it (see
    /// [`Document::reload_from`]). Reads the file while the caller holds the document; to keep a
    /// slow disk from stalling others, use [`read_for_reload`] without the lock, then
    /// [`Document::reload_from`].
    pub fn reload(&mut self, source: ChangeSource) -> FileResult<()> {
        let Some(path) = self.path.clone() else {
            return Err(io::FileError::Io {
                path: PathBuf::new(),
                source: std::io::Error::new(std::io::ErrorKind::NotFound, "document has no file to reload"),
            });
        };
        let (project, notes, stamp) = read_for_reload(&path)?;
        self.reload_from(project, notes, stamp, source).map_err(|e| io::FileError::Limit { path, message: e.to_string() })
    }

    /// Updates the snapshots of linked plans from files read because they changed on disk, the
    /// way opening the plan does: the snapshots are a copy of other files, not the user's edit, so
    /// this is no undo step and leaves the document's unsaved state as it was. Returns warnings.
    pub fn refresh_external_from(&mut self, reads: io::ExternalReads) -> Vec<String> {
        // A plan from a server changes only through edits it hands on (and links to files on
        // this computer mean nothing to the others).
        if self.project.external_projects.is_empty() || self.shared.is_some() {
            return Vec::new();
        }
        let before = self.project.external_projects.clone();
        let mut probe = self.project.clone();
        let warnings = io::apply_external_projects(&mut probe, reads);
        // Recalculated on a copy: a failure leaves the plan as it was.
        match try_recalculate(&mut probe) {
            Ok(_) => {
                if probe.external_projects != before {
                    self.project = probe;
                    self.revision = self.revision.wrapping_add(1);
                }
                warnings
            }
            Err(e) => vec![format!("Links were not refreshed: {}", PlanError::from(e))],
        }
    }

    fn save_checked(&mut self, check: bool) -> FileResult<()> {
        if let Some(e) = self.shared_save_refusal() {
            return Err(e);
        }
        let Some(path) = self.path.clone() else {
            return Err(io::FileError::Io {
                path: PathBuf::new(),
                source: std::io::Error::new(std::io::ErrorKind::NotFound, "document has no file path; use save as"),
            });
        };
        if check && self.changed_on_disk() {
            return Err(io::FileError::ChangedOnDisk { path });
        }
        self.disk_stamp = Some(io::save_stamped(&self.project, &path)?);
        self.dirty = false;
        self.user_changes = false;
        // The first save of a plan bound to a new file, or a lock file someone removed.
        self.claim_lock();
        Ok(())
    }

    /// Takes (or keeps) the lock of the document's file, if the file exists (see [`lock::claim`]).
    pub fn claim_lock(&mut self) {
        if let Some(path) = &self.path
            && path.is_file()
        {
            self.lock = lock::claim(path);
        }
    }

    /// Gives back the document's lock, if this PlanCraft holds it.
    pub fn release_lock(&mut self) {
        if self.lock == LockState::Held
            && let Some(path) = &self.path
        {
            lock::release(path);
        }
        self.lock = LockState::None;
    }

    /// Saves to a new path, rewriting relative external links so they still resolve. A `.xml`
    /// path is refused: plans are saved as `.plancraft`, and an `.xml` file would be taken for
    /// Microsoft Project XML when opened (use [`crate::mspdi::save`] to export one).
    pub fn save_as(&mut self, path: impl AsRef<Path>) -> FileResult<()> {
        let path = path.as_ref().to_path_buf();
        if let Some(e) = self.shared_save_refusal() {
            return Err(e);
        }
        if crate::mspdi::is_mspdi_path(&path) {
            return Err(io::FileError::Io {
                path,
                source: std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "a plan is saved as a .plancraft file; export it to Microsoft Project XML to write an .xml file",
                ),
            });
        }
        let old = self.path.clone();
        let new_dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
        let new_dir = if new_dir.is_absolute() { new_dir } else { std::env::current_dir().map(|c| c.join(&new_dir)).unwrap_or(new_dir) };
        // Rewrite the links on a copy: a failed save must leave the document's links pointing
        // where they did, or the next plain Save writes paths relative to a folder never used.
        let mut saved = self.project.clone();
        for e in &mut saved.external_projects {
            let abs = io::resolve_external_path(&e.path, old.as_deref());
            let abs = if abs.is_absolute() { abs } else { std::env::current_dir().map(|c| c.join(&abs)).unwrap_or(abs) };
            e.path = io::relative_path(&abs, &new_dir).to_string_lossy().replace('\\', "/");
        }
        let stamp = io::save_stamped(&saved, &path)?;
        let same_file = old.as_ref().is_some_and(|o| o == &path || std::fs::canonicalize(o).ok() == std::fs::canonicalize(&path).ok());
        if !same_file {
            self.release_lock();
        }
        self.disk_stamp = Some(stamp);
        self.project.external_projects = saved.external_projects;
        self.path = Some(path);
        self.dirty = false;
        self.user_changes = false;
        self.claim_lock();
        Ok(())
    }

    /// A plan opened from a server is saved by the server: saving it here would leave a file
    /// nobody else sees. [`Document::save_copy`] writes a copy instead.
    fn shared_save_refusal(&self) -> Option<io::FileError> {
        let shared = self.shared.as_ref()?;
        Some(io::FileError::Io {
            path: PathBuf::from(&shared.name),
            source: std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "this plan is opened from a server, which saves every change; use Save As to keep a copy of your own",
            ),
        })
    }

    /// Writes the plan to `path` as a file of its own, leaving this document as it is (for a plan
    /// opened from a server: the copy can be opened and changed like any other plan). Id
    /// counters are raised past every id in use first, so the copy stands on its own.
    pub fn save_copy(&self, path: impl AsRef<Path>) -> FileResult<()> {
        let path = path.as_ref();
        if crate::mspdi::is_mspdi_path(path) {
            return Err(io::FileError::Io {
                path: path.into(),
                source: std::io::Error::new(std::io::ErrorKind::InvalidInput, "a plan is saved as a .plancraft file"),
            });
        }
        let mut copy = self.project.clone();
        crate::sync::fit_counters(&mut copy);
        io::save(&copy, path)
    }

    /// Re-reads external documents (an explicit Refresh Links, following every link) and updates
    /// their snapshots as an undoable edit by the user. Returns warnings.
    pub fn refresh_external(&mut self) -> Vec<String> {
        let reads = io::read_external_projects(&self.project.external_projects, self.path.as_deref(), true);
        self.apply_external_reads(reads, ChangeSource::User)
    }

    /// Updates the external-link snapshots from documents read (without holding the workspace
    /// lock) by [`io::read_external_projects`], as an undoable edit attributed to `source`.
    /// Returns warnings.
    pub fn apply_external_reads(&mut self, reads: io::ExternalReads, source: ChangeSource) -> Vec<String> {
        if self.project.external_projects.is_empty() {
            return Vec::new();
        }
        if self.shared.is_some() {
            return vec!["Links to other plans are not refreshed in a plan opened from a server.".into()];
        }
        let mut probe = self.project.clone();
        let warnings = io::apply_external_projects(&mut probe, reads);
        let links = probe.external_projects;
        // Nothing new in the linked files: only their "last synced" times change, which is no
        // edit to undo and no reason to save (as before Refresh Links became an edit).
        let without_sync = |l: &[crate::model::ExternalProject]| {
            l.iter().map(|e| crate::model::ExternalProject { last_sync: None, ..e.clone() }).collect::<Vec<_>>()
        };
        if without_sync(&links) == without_sync(&self.project.external_projects) {
            self.project.external_projects = links;
            self.revision = self.revision.wrapping_add(1);
            return warnings;
        }
        match self.edit_as("Refresh Links", source, |p| p.external_projects = links) {
            Ok(()) => warnings,
            Err(e) => vec![format!("Links were not refreshed: {e}")],
        }
    }

    /// The refresh done when a document opens: network-share links wait for an explicit refresh,
    /// so opening someone else's file never connects to a server it names.
    pub fn refresh_external_on_open(&mut self) -> Vec<String> {
        self.refresh_external_with(io::refresh_external_on_open)
    }

    fn refresh_external_with(&mut self, refresh: fn(&mut Project, Option<&Path>) -> Vec<String>) -> Vec<String> {
        if self.project.external_projects.is_empty() || self.shared.is_some() {
            return Vec::new();
        }
        let before = self.project.clone();
        let warnings = refresh(&mut self.project, self.path.as_deref());
        recalculate(&mut self.project);
        if self.project.external_projects != before.external_projects {
            self.revision = self.revision.wrapping_add(1);
        }
        warnings
    }
}

/// The set of open documents.
#[derive(Debug, Default)]
pub struct Workspace {
    pub docs: Vec<Document>,
    pub active: Option<usize>,
    next_handle: u64,
}

pub type SharedWorkspace = Arc<Mutex<Workspace>>;

impl Drop for Workspace {
    /// Closing the workspace (the app or the MCP server quitting) gives back its lock files.
    fn drop(&mut self) {
        self.release_locks();
    }
}

impl Workspace {
    pub fn shared(self) -> SharedWorkspace {
        Arc::new(Mutex::new(self))
    }

    /// Adds a document and makes it active. Returns its handle.
    pub fn add(&mut self, mut project: Project, path: Option<PathBuf>) -> u64 {
        self.next_handle = self.next_handle.saturating_add(1);
        recalculate(&mut project);
        let mut doc = Document::new(self.next_handle, project, path);
        // A plan opened from its file, or a recovery copy bound to one (a path that does not
        // exist yet is locked when first saved).
        doc.claim_lock();
        let _ = doc.refresh_external_on_open();
        self.docs.push(doc);
        self.active = self.docs.len().checked_sub(1);
        self.next_handle
    }

    /// Adds a plan opened from a server (named `name` there) and makes it active: no file, no lock
    /// file, no file watching. Returns its handle.
    pub fn add_shared(&mut self, project: Project, name: &str) -> u64 {
        self.next_handle = self.next_handle.saturating_add(1);
        self.docs.push(Document::new_shared(self.next_handle, project, name));
        self.active = self.docs.len().checked_sub(1);
        self.next_handle
    }

    /// Opens a document from disk (or activates it if already open). A Microsoft Project XML
    /// file (`.xml`) is imported instead, see [`Workspace::import_mspdi`].
    pub fn open(&mut self, path: impl AsRef<Path>) -> FileResult<u64> {
        let path = path.as_ref();
        if crate::mspdi::is_mspdi_path(path) {
            return self.import_mspdi(path);
        }
        if crate::tabular::is_table_path(path) {
            return self.import_spreadsheet(path, 0, None, true);
        }
        let abs = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        if let Some(i) =
            self.docs.iter().position(|d| d.path.as_ref().is_some_and(|p| std::fs::canonicalize(p).unwrap_or_else(|_| p.clone()) == abs))
            && let Some(doc) = self.docs.get(i)
        {
            self.active = Some(i);
            return Ok(doc.handle);
        }
        // Stamp before reading: if the file changes while it loads, the next save notices.
        let stamp = io::disk_stamp(&abs);
        let (project, notes) = io::load_with_notes(&abs)?;
        let handle = self.add(project, Some(abs));
        if let Some(doc) = self.active_doc_mut() {
            doc.load_notes = notes;
            doc.disk_stamp = stamp;
        }
        Ok(handle)
    }

    /// Imports a Microsoft Project XML file as a new document. It is unsaved (no path): saving
    /// asks where to keep it as a `.plancraft` plan. The load notes say what was left out. It
    /// counts as the user's unsaved work; an agent importing a file clears `user_changes`.
    /// While an earlier import of the same file is still unsaved, that document is activated.
    pub fn import_mspdi(&mut self, path: impl AsRef<Path>) -> FileResult<u64> {
        let path = path.as_ref();
        let abs = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        if let Some(i) = self.docs.iter().position(|d| d.path.is_none() && d.imported_from.as_ref() == Some(&abs))
            && let Some(doc) = self.docs.get(i)
        {
            self.active = Some(i);
            return Ok(doc.handle);
        }
        let (project, notes) = crate::mspdi::load(&abs)?;
        Ok(self.add_imported(project, notes, abs))
    }

    /// Imports a task list from a spreadsheet (CSV or a workbook) as a new unsaved document.
    /// `sheet` picks the worksheet (the first is 0); `options` says what the columns hold
    /// (`None`: guessed from the headings). With `reuse`, a still-unsaved import of the same file
    /// is activated instead of importing it again.
    pub fn import_spreadsheet(
        &mut self,
        path: impl AsRef<Path>,
        sheet: usize,
        options: Option<crate::tabular::ImportOptions>,
        reuse: bool,
    ) -> FileResult<u64> {
        let path = path.as_ref();
        let abs = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        if reuse
            && let Some(i) = self.docs.iter().position(|d| d.path.is_none() && d.imported_from.as_ref() == Some(&abs))
            && let Some(doc) = self.docs.get(i)
        {
            self.active = Some(i);
            return Ok(doc.handle);
        }
        let import = |message: String| io::FileError::Import { path: path.into(), message };
        let sheets = crate::tabular::read_path(&abs)?;
        let (_, grid) = sheets.sheets.get(sheet).ok_or_else(|| import(format!("the file has no worksheet {}", sheet.saturating_add(1))))?;
        let name = path.file_stem().map_or_else(|| "Imported plan".to_string(), |s| s.to_string_lossy().into_owned());
        let options = options.unwrap_or_else(|| crate::tabular::ImportOptions::guess(grid, name));
        let (project, mut notes) = crate::tabular::build(grid, &options).map_err(import)?;
        let mut all = sheets.notes;
        all.append(&mut notes);
        Ok(self.add_imported(project, all, abs))
    }

    /// Adds a plan imported from another format (a spreadsheet, Microsoft Project XML) as a new,
    /// unsaved document that remembers where it came from; makes it active.
    pub fn add_imported(&mut self, project: Project, notes: Vec<String>, from: PathBuf) -> u64 {
        let handle = self.add(project, None);
        if let Some(doc) = self.active_doc_mut() {
            doc.load_notes = notes;
            doc.imported = true;
            doc.imported_from = Some(from);
            doc.dirty = true;
            doc.user_changes = true;
        }
        handle
    }

    pub fn close(&mut self, handle: u64) -> bool {
        let Some(i) = self.index_of(handle) else {
            return false;
        };
        let mut doc = self.docs.remove(i);
        // Another open document on the same file (Save As onto a plan already open) keeps the
        // lock: it is handed over rather than released.
        let canonical = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
        let closing = doc.path.as_deref().map(canonical);
        let same_file = |d: &Document| closing.is_some() && d.path.as_deref().map(canonical) == closing;
        if doc.lock == LockState::Held
            && let Some(other) = self.docs.iter_mut().find(|d| same_file(d))
        {
            other.lock = LockState::Held;
            doc.lock = LockState::None;
        }
        doc.release_lock();
        self.active = match self.active {
            _ if self.docs.is_empty() => None,
            Some(a) if a > i => Some(a - 1),
            Some(a) if a == i => Some(i.min(self.docs.len().saturating_sub(1))),
            other => other,
        };
        true
    }

    /// Writes again the lock files of documents that hold one, where it went missing: after a
    /// Save As away from a file another open document also has (Save As onto a plan already
    /// open), the first gave that file's lock back, which the other still relies on.
    pub fn keep_locks(&mut self) {
        for doc in &mut self.docs {
            if doc.lock == LockState::Held {
                doc.claim_lock();
            }
        }
    }

    /// Gives back every lock file this workspace holds (when quitting).
    pub fn release_locks(&mut self) {
        for doc in &mut self.docs {
            doc.release_lock();
        }
    }

    pub fn index_of(&self, handle: u64) -> Option<usize> {
        self.docs.iter().position(|d| d.handle == handle)
    }

    pub fn active_doc(&self) -> Option<&Document> {
        self.active.and_then(|i| self.docs.get(i))
    }

    pub fn active_doc_mut(&mut self) -> Option<&mut Document> {
        self.active.and_then(|i| self.docs.get_mut(i))
    }

    /// Finds a document by handle, file path / file name, or project name.
    pub fn find(&self, key: &str) -> Option<usize> {
        let key = key.trim();
        if let Ok(h) = key.parse::<u64>()
            && let Some(i) = self.index_of(h)
        {
            return Some(i);
        }
        self.docs.iter().position(|d| {
            d.path.as_ref().is_some_and(|p| {
                p.to_string_lossy() == key
                    || p.file_name().is_some_and(|f| f.to_string_lossy() == key)
                    || p.file_stem().is_some_and(|f| f.to_string_lossy() == key)
            }) || d.project.name.eq_ignore_ascii_case(key)
                || d.project.id.to_string() == key
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shared_pair() -> (Workspace, u64) {
        let mut p = Project::new("Shared", crate::dates::ymd(2026, 10, 19).unwrap());
        p.add_task("A", None);
        p.add_task("B", None);
        recalculate(&mut p);
        let mut ws = Workspace::default();
        let h = ws.add_shared(p, "Shared.plancraft");
        (ws, h)
    }

    fn task_named<'a>(p: &'a Project, name: &str) -> &'a crate::model::Task {
        p.tasks.iter().find(|t| t.name == name).unwrap()
    }

    #[test]
    fn a_shared_plan_hands_on_its_edits_once() {
        let (mut ws, h) = shared_pair();
        let i = ws.index_of(h).unwrap();
        let doc = &mut ws.docs[i];
        assert_eq!(doc.title(), "Shared");
        assert!(doc.take_unsent().is_none(), "nothing edited yet");
        let start = doc.project.clone();
        doc.edit("Add", |p| p.add_task("C", None)).unwrap();
        doc.edit("Add", |p| p.add_task("D", None)).unwrap();
        assert!(doc.has_unsent());
        let (before, after) = doc.take_unsent().unwrap();
        assert_eq!(before, start, "both edits go together, from the plan before the first");
        assert_eq!(after, doc.project);
        assert!(doc.take_unsent().is_none());
        assert!(doc.undo());
        assert!(doc.take_unsent().is_some(), "an undo is an edit to hand on too");
    }

    #[test]
    fn undo_in_a_shared_plan_keeps_other_peoples_changes() {
        let (mut ws, h) = shared_pair();
        let i = ws.index_of(h).unwrap();
        let doc = &mut ws.docs[i];
        let a = task_named(&doc.project, "A").id;
        doc.edit("Rename", |p| p.tasks.iter_mut().find(|t| t.id == a).unwrap().name = "A2".into()).unwrap();
        // Someone else renames B meanwhile.
        let mut theirs = doc.project.clone();
        theirs.tasks.iter_mut().find(|t| t.name == "B").unwrap().name = "B (Ana)".into();
        doc.apply_remote(theirs);
        assert_eq!(doc.try_undo_as(ChangeSource::User), Ok(true));
        assert!(doc.project.tasks.iter().any(|t| t.name == "A"), "my rename was taken back");
        assert!(doc.project.tasks.iter().any(|t| t.name == "B (Ana)"), "hers stayed");
        assert_eq!(doc.try_redo_as(ChangeSource::User), Ok(true));
        assert!(doc.project.tasks.iter().any(|t| t.name == "A2"));
        assert!(doc.project.tasks.iter().any(|t| t.name == "B (Ana)"));
    }

    #[test]
    fn undo_is_refused_when_someone_changed_the_same_value_since() {
        let (mut ws, h) = shared_pair();
        let i = ws.index_of(h).unwrap();
        let doc = &mut ws.docs[i];
        let a = task_named(&doc.project, "A").id;
        doc.edit("Rename", |p| p.tasks.iter_mut().find(|t| t.id == a).unwrap().name = "Mine".into()).unwrap();
        let mut theirs = doc.project.clone();
        theirs.tasks.iter_mut().find(|t| t.id == a).unwrap().name = "Hers".into();
        doc.apply_remote(theirs);
        doc.take_unsent();
        let why = doc.try_undo_as(ChangeSource::User).unwrap_err();
        assert!(why.contains("Cannot undo Rename") && why.contains("since"), "{why}");
        assert_eq!(task_named(&doc.project, "Hers").id, a, "nothing was undone");
        assert!(why.contains("taken off"), "{why}");
        assert!(!doc.can_undo(), "the step left the list, so it no longer blocks the steps before it");
        assert!(doc.take_unsent().is_none(), "a refused undo sends nothing");
    }

    #[test]
    fn an_undo_step_whose_edit_the_server_dropped_is_passed_over() {
        let (mut ws, h) = shared_pair();
        let i = ws.index_of(h).unwrap();
        let doc = &mut ws.docs[i];
        let (a, b) = (task_named(&doc.project, "A").id, task_named(&doc.project, "B").id);
        doc.edit("Rename A", |p| p.tasks.iter_mut().find(|t| t.id == a).unwrap().name = "A2".into()).unwrap();
        let kept = doc.project.clone();
        doc.edit("Rename B", |p| p.tasks.iter_mut().find(|t| t.id == b).unwrap().name = "B2".into()).unwrap();
        doc.take_unsent();
        // The server did not keep "Rename B": the plan comes back without it.
        doc.apply_remote(kept);
        assert!(doc.last_change.is_none(), "others' changes are not this person's last change");
        assert!(doc.try_undo_as(ChangeSource::User).unwrap(), "one Undo does something");
        assert_eq!(task_named(&doc.project, "A").id, a, "it took back Rename A, passing over the dropped step");
        assert_eq!(doc.redo.len(), 1);
        // Redo brings back Rename A, and its refusal names the right direction.
        assert!(doc.try_redo_as(ChangeSource::User).unwrap());
        assert_eq!(task_named(&doc.project, "A2").id, a);
        doc.try_undo_as(ChangeSource::User).unwrap();
        let mut theirs = doc.project.clone();
        theirs.tasks.iter_mut().find(|t| t.id == a).unwrap().name = "Hers".into();
        doc.apply_remote(theirs);
        let why = doc.try_redo_as(ChangeSource::User).unwrap_err();
        assert!(why.contains("redo again for the step after it"), "{why}");
    }

    #[test]
    fn a_plan_that_may_only_be_viewed_refuses_every_change() {
        let (mut ws, h) = shared_pair();
        let i = ws.index_of(h).unwrap();
        let doc = &mut ws.docs[i];
        doc.edit("Add", |p| p.add_task("C", None)).unwrap();
        doc.shared.as_mut().unwrap().read_only = Some("You may only view this plan".into());
        let before = doc.project.clone();
        let e = doc.edit("Add", |p| p.add_task("D", None)).unwrap_err();
        assert!(e.to_string().contains("only view"));
        assert!(doc.try_undo_as(ChangeSource::Agent).is_err());
        assert_eq!(doc.project, before);
        assert!(doc.save().is_err() && doc.save_as(std::env::temp_dir().join("x.plancraft")).is_err(), "the server saves it");
    }

    #[test]
    fn a_copy_of_a_shared_plan_stands_on_its_own() {
        let (mut ws, h) = shared_pair();
        let i = ws.index_of(h).unwrap();
        let doc = &mut ws.docs[i];
        // A replica's counters sit in its own lease, far from its ids.
        doc.project.ids.task = 1;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("copy.plancraft");
        doc.save_copy(&path).unwrap();
        // The file itself, not as loading fixes it up.
        let raw: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let counter = raw["ids"]["task"].as_u64().unwrap();
        let max = doc.project.tasks.iter().map(|t| u64::from(t.id.0)).max().unwrap();
        assert!(counter >= max, "the saved counter {counter} is below task id {max}");
        assert!(doc.path.is_none() && doc.shared.is_some(), "the document stays shared");
    }

    #[test]
    fn undo_redo_and_failed_edits() {
        let mut ws = Workspace::default();
        let h = ws.add(Project::new("P", crate::dates::ymd(2026, 10, 19).unwrap()), None);
        let i = ws.index_of(h).unwrap();
        let doc = &mut ws.docs[i];
        doc.edit("Add task", |p| p.add_task("A", None)).unwrap();
        assert_eq!(doc.project.tasks.len(), 1);
        let r: PlanResult<()> = doc.try_edit("Bad", ChangeSource::Agent, |p| {
            p.add_task("B", None);
            Err(PlanError::Invalid("nope".into()))
        });
        assert!(r.is_err());
        assert_eq!(doc.project.tasks.len(), 1);
        assert!(doc.undo());
        assert!(doc.project.tasks.is_empty());
        assert!(doc.redo());
        assert_eq!(doc.project.tasks.len(), 1);
        assert_eq!(doc.title(), "P*");
    }

    #[test]
    fn panicking_and_out_of_range_edits_roll_back() {
        let mut ws = Workspace::default();
        let h = ws.add(Project::new("P", crate::dates::ymd(2026, 10, 19).unwrap()), None);
        let i = ws.index_of(h).unwrap();
        let doc = &mut ws.docs[i];
        let id = doc.edit("Add task", |p| p.add_task("A", None)).unwrap();
        let before = doc.project.clone();
        let steps = doc.history().len();

        let r = doc.edit("Explode", |p| {
            p.tasks.clear();
            panic!("bug in an edit")
        });
        assert!(matches!(r, Err(PlanError::Internal(m)) if m.contains("bug in an edit")));
        assert_eq!(doc.project, before, "a panicking edit is rolled back");

        let r = doc.edit("Too long", |p| p.task_mut(id).map(|t| t.duration = crate::limits::MAX_MINUTES * 2));
        assert!(matches!(r, Err(PlanError::Limit(_))));
        assert_eq!(doc.project, before, "an out-of-range edit is rolled back");
        assert_eq!(doc.history().len(), steps, "failed edits leave no undo step");
    }

    /// Regression: undo history kept 200 whole-project snapshots with no memory limit, so a
    /// large plan edited many times (by an agent, say) could exhaust memory.
    #[test]
    fn undo_history_stays_within_its_memory_budget() {
        let mut ws = Workspace::default();
        let mut p = Project::new("Big", crate::dates::ymd(2026, 10, 19).unwrap());
        // About 64 KB per snapshot by the estimate; the test budget holds about 64 of them.
        for i in 0..32 {
            let id = p.add_task(format!("Task {i}"), None);
            p.task_mut(id).unwrap().notes = "x".repeat(1800);
        }
        let h = ws.add(p, None);
        let i = ws.index_of(h).unwrap();
        let doc = &mut ws.docs[i];
        let per_step = approx_bytes(&doc.project);
        assert!(per_step > 60_000 && per_step < 100_000, "{per_step}");
        for k in 0..120 {
            doc.edit("Rename", |p| p.tasks[0].name = format!("Renamed {k}")).unwrap();
        }
        let kept = doc.history().len();
        let total: usize = doc.undo.iter().map(|s| s.bytes).sum();
        assert!(total <= UNDO_BUDGET_BYTES, "{total}");
        // About 50 snapshots fit (the exact number depends on the size of the model types).
        assert!((40..120).contains(&kept), "kept {kept} steps");
        // The most recent steps are the ones kept.
        assert_eq!(doc.history()[0].0, "Rename");
        assert!(doc.undo());
        assert_eq!(doc.project.tasks[0].name, "Renamed 118");
    }

    /// Regression: custom-field pick-lists, resource notes and other text were left out of the
    /// estimate, so an agent could fill undo history with huge snapshots counted as tiny.
    #[test]
    fn undo_estimate_counts_custom_fields_and_resource_text() {
        let mut p = Project::new("Big", crate::dates::ymd(2026, 10, 19).unwrap());
        let base = approx_bytes(&p);
        let choices: Vec<String> = (0..100).map(|i| format!("{i}{}", "c".repeat(900))).collect();
        p.add_custom_field("Status", crate::custom::CustomFieldKind::Text, &choices).unwrap();
        let r = p.add_resource("Ana");
        p.resource_mut(r).unwrap().notes = "n".repeat(50_000);
        p.set_table_columns("Entry", &["name".into(), "duration".into()]).unwrap();
        assert!(approx_bytes(&p) > base + 140_000, "{}", approx_bytes(&p));
    }

    /// Regression: a recovery copy restored against its original file had no record of that
    /// file's version, so Save replaced a newer file (from a sync tool, say) without asking.
    #[test]
    fn a_document_bound_to_a_file_it_never_read_does_not_overwrite_it() {
        let dir = std::env::temp_dir().join(format!("plancraft-unknown-stamp-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("plan.plancraft");
        std::fs::write(&path, "newer work").unwrap();
        let mut ws = Workspace::default();
        let h = ws.add(Project::new("Restored", crate::dates::ymd(2026, 10, 19).unwrap()), Some(path.clone()));
        let i = ws.index_of(h).unwrap();
        let doc = &mut ws.docs[i];
        assert!(doc.changed_on_disk());
        assert!(matches!(doc.save(), Err(io::FileError::ChangedOnDisk { .. })));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "newer work");
        doc.save_overwriting().unwrap();
        assert!(!doc.changed_on_disk(), "after saving, the file is known");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Reloading a file another program changed is one undo step: Undo brings back the version
    /// that was open, unsaved changes included, and marks it unsaved again.
    #[test]
    fn reload_is_undoable_and_leaves_the_document_matching_its_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("plan.plancraft");
        let start = crate::dates::ymd(2026, 10, 19).unwrap();
        io::save(&Project::new("Plan", start), &path).unwrap();
        let mut ws = Workspace::default();
        ws.open(&path).unwrap();
        let doc = ws.active_doc_mut().unwrap();
        doc.edit("Add mine", |p| p.add_task("Mine", None)).unwrap();
        let mut theirs = Project::new("Plan", start);
        theirs.add_task("Theirs", None);
        io::save(&theirs, &path).unwrap();
        assert_eq!(doc.disk_status(), DiskStatus::Changed);
        assert!(matches!(doc.save(), Err(io::FileError::ChangedOnDisk { .. })));

        doc.reload(ChangeSource::User).unwrap();
        assert_eq!(doc.project.tasks[0].name, "Theirs");
        assert!(!doc.dirty && !doc.user_changes);
        assert_eq!(doc.disk_status(), DiskStatus::InSync);
        assert_eq!(doc.undo_label(), Some(RELOAD_LABEL));

        assert!(doc.undo());
        assert_eq!(doc.project.tasks[0].name, "Mine");
        assert!(doc.dirty && doc.user_changes, "the undone reload is unsaved work again");
        // The file on disk is the version reloaded, so saving now replaces it without asking.
        doc.save().unwrap();
        assert_eq!(io::load(&path).unwrap().tasks[0].name, "Mine");
        assert_eq!(disk_status(None, None), DiskStatus::NoFile);
    }

    /// Regression: a failed Save As left the external links rewritten relative to the folder it
    /// could not write to, so the next plain Save stored broken links.
    #[test]
    fn failed_save_as_keeps_external_links() {
        let dir = std::env::temp_dir().join(format!("plancraft-saveas-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("plans")).unwrap();
        let mut ws = Workspace::default();
        let mut p = Project::new("P", crate::dates::ymd(2026, 10, 19).unwrap());
        p.external_projects.push(crate::model::ExternalProject {
            id: uuid::Uuid::new_v4(),
            name: "Other".into(),
            path: "other.plancraft".into(),
            tasks: Vec::new(),
            last_sync: None,
        });
        let h = ws.add(p, Some(dir.join("plans").join("main.plancraft")));
        let i = ws.index_of(h).unwrap();
        let doc = &mut ws.docs[i];

        let missing = dir.join("no").join("such").join("folder").join("copy.plancraft");
        assert!(doc.save_as(&missing).is_err());
        assert_eq!(doc.project.external_projects[0].path, "other.plancraft");
        assert_eq!(doc.path.as_deref(), Some(dir.join("plans").join("main.plancraft").as_path()));

        // A successful Save As still rewrites the link relative to the new folder.
        std::fs::create_dir_all(dir.join("copies")).unwrap();
        doc.save_as(dir.join("copies").join("copy.plancraft")).unwrap();
        assert_eq!(doc.project.external_projects[0].path, "../plans/other.plancraft");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
