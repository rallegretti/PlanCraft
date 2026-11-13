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
                + t.sched.parts.len() * size_of::<(chrono::NaiveDateTime, chrono::NaiveDateTime)>()
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
    project: Project,
    /// Estimated size, computed once when the snapshot is taken.
    bytes: usize,
}

impl Snapshot {
    fn new(label: String, source: ChangeSource, project: Project) -> Self {
        let bytes = approx_bytes(&project);
        Self { label, source, project, bytes }
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
    /// The file as it was when last loaded or saved; `save` refuses to overwrite a file that has
    /// changed since (another program or PlanCraft instance wrote it).
    disk_stamp: Option<io::DiskStamp>,
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
            disk_stamp: None,
            undo: Vec::new(),
            redo: Vec::new(),
        }
    }

    /// Window/tab title: file name or project name, with `*` when modified.
    pub fn title(&self) -> String {
        let base = self
            .path
            .as_ref()
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
                self.undo.push(Snapshot::new(label.into(), source, before));
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

    /// Undoes the last change, attributed to `source`.
    pub fn undo_as(&mut self, source: ChangeSource) -> bool {
        let Some(s) = self.undo.pop() else {
            return false;
        };
        let current = std::mem::replace(&mut self.project, s.project);
        self.redo.push(Snapshot::new(s.label.clone(), s.source, current));
        self.touched(&format!("Undo {}", s.label), source);
        true
    }

    /// Redoes the last undone change, attributed to `source`.
    pub fn redo_as(&mut self, source: ChangeSource) -> bool {
        let Some(s) = self.redo.pop() else {
            return false;
        };
        let current = std::mem::replace(&mut self.project, s.project);
        self.undo.push(Snapshot::new(s.label.clone(), s.source, current));
        self.touched(&format!("Redo {}", s.label), source);
        true
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

    fn save_checked(&mut self, check: bool) -> FileResult<()> {
        let Some(path) = self.path.clone() else {
            return Err(io::FileError::Io {
                path: PathBuf::new(),
                source: std::io::Error::new(std::io::ErrorKind::NotFound, "document has no file path; use save as"),
            });
        };
        if check && self.changed_on_disk() {
            return Err(io::FileError::ChangedOnDisk { path });
        }
        io::save(&self.project, &path)?;
        self.disk_stamp = io::disk_stamp(&path);
        self.dirty = false;
        self.user_changes = false;
        Ok(())
    }

    /// Saves to a new path, rewriting relative external links so they still resolve.
    pub fn save_as(&mut self, path: impl AsRef<Path>) -> FileResult<()> {
        let path = path.as_ref().to_path_buf();
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
        io::save(&saved, &path)?;
        self.disk_stamp = io::disk_stamp(&path);
        self.project.external_projects = saved.external_projects;
        self.path = Some(path);
        self.dirty = false;
        self.user_changes = false;
        Ok(())
    }

    /// Re-reads external documents and recalculates. Returns warnings.
    pub fn refresh_external(&mut self) -> Vec<String> {
        self.refresh_external_with(io::refresh_external)
    }

    /// The refresh done when a document opens: network-share links wait for an explicit refresh,
    /// so opening someone else's file never connects to a server it names.
    pub fn refresh_external_on_open(&mut self) -> Vec<String> {
        self.refresh_external_with(io::refresh_external_on_open)
    }

    fn refresh_external_with(&mut self, refresh: fn(&mut Project, Option<&Path>) -> Vec<String>) -> Vec<String> {
        if self.project.external_projects.is_empty() {
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

impl Workspace {
    pub fn shared(self) -> SharedWorkspace {
        Arc::new(Mutex::new(self))
    }

    /// Adds a document and makes it active. Returns its handle.
    pub fn add(&mut self, mut project: Project, path: Option<PathBuf>) -> u64 {
        self.next_handle = self.next_handle.saturating_add(1);
        recalculate(&mut project);
        let mut doc = Document::new(self.next_handle, project, path);
        let _ = doc.refresh_external_on_open();
        self.docs.push(doc);
        self.active = self.docs.len().checked_sub(1);
        self.next_handle
    }

    /// Opens a document from disk (or activates it if already open).
    pub fn open(&mut self, path: impl AsRef<Path>) -> FileResult<u64> {
        let path = path.as_ref();
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

    pub fn close(&mut self, handle: u64) -> bool {
        let Some(i) = self.index_of(handle) else {
            return false;
        };
        self.docs.remove(i);
        self.active = match self.active {
            _ if self.docs.is_empty() => None,
            Some(a) if a > i => Some(a - 1),
            Some(a) if a == i => Some(i.min(self.docs.len().saturating_sub(1))),
            other => other,
        };
        true
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
    use chrono::NaiveDate;

    #[test]
    fn undo_redo_and_failed_edits() {
        let mut ws = Workspace::default();
        let h = ws.add(Project::new("P", NaiveDate::from_ymd_opt(2026, 10, 19).unwrap()), None);
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
        let h = ws.add(Project::new("P", NaiveDate::from_ymd_opt(2026, 10, 19).unwrap()), None);
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
        let mut p = Project::new("Big", NaiveDate::from_ymd_opt(2026, 10, 19).unwrap());
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
        assert!((50..120).contains(&kept), "kept {kept} steps");
        // The most recent steps are the ones kept.
        assert_eq!(doc.history()[0].0, "Rename");
        assert!(doc.undo());
        assert_eq!(doc.project.tasks[0].name, "Renamed 118");
    }

    /// Regression: custom-field pick-lists, resource notes and other text were left out of the
    /// estimate, so an agent could fill undo history with huge snapshots counted as tiny.
    #[test]
    fn undo_estimate_counts_custom_fields_and_resource_text() {
        let mut p = Project::new("Big", NaiveDate::from_ymd_opt(2026, 10, 19).unwrap());
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
        let h = ws.add(Project::new("Restored", NaiveDate::from_ymd_opt(2026, 10, 19).unwrap()), Some(path.clone()));
        let i = ws.index_of(h).unwrap();
        let doc = &mut ws.docs[i];
        assert!(doc.changed_on_disk());
        assert!(matches!(doc.save(), Err(io::FileError::ChangedOnDisk { .. })));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "newer work");
        doc.save_overwriting().unwrap();
        assert!(!doc.changed_on_disk(), "after saving, the file is known");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Regression: a failed Save As left the external links rewritten relative to the folder it
    /// could not write to, so the next plain Save stored broken links.
    #[test]
    fn failed_save_as_keeps_external_links() {
        let dir = std::env::temp_dir().join(format!("plancraft-saveas-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("plans")).unwrap();
        let mut ws = Workspace::default();
        let mut p = Project::new("P", NaiveDate::from_ymd_opt(2026, 10, 19).unwrap());
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
