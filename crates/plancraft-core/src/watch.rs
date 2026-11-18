//! Noticing when open plans change on disk: another PlanCraft instance, the headless MCP server,
//! an agent, git or a sync tool wrote the file, or deleted or moved it.
//!
//! Plain polling of each file's [`DiskStamp`] (modification time and size, no inode), with
//! the standard library only: it works the same on every platform and on network shares, where
//! change notifications are unreliable. [`Watcher::poll`] is meant to run every second or so on a
//! background thread, never while holding the workspace lock: [`targets`] copies what it needs
//! under the lock, then the files are checked (and, when they changed, read) without it.
//!
//! PlanCraft's own saves are not reported: a save records the new stamp in the document, so the
//! file matches it again. A change is reported once it has held for two polls in a row, so a
//! program still writing the file is not caught halfway; each version is reported once.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use uuid::Uuid;

use crate::io::{self, DiskStamp, FileResult};
use crate::model::{ExternalProject, Project};
use crate::workspace::{Workspace, read_for_reload};

/// One open document to watch, copied from the workspace by [`targets`].
#[derive(Clone, Debug)]
pub struct Target {
    pub handle: u64,
    /// The document's file (`None`: never saved, so only its links are watched).
    pub path: Option<PathBuf>,
    /// The version of the file the document matches ([`crate::workspace::Document::disk_stamp`]).
    pub known: Option<DiskStamp>,
    /// The plan's links to other plans, whose files are watched to refresh their snapshots.
    pub links: Vec<ExternalProject>,
}

/// What to watch in the workspace: every open document's file and the files it links to.
pub fn targets(ws: &Workspace) -> Vec<Target> {
    ws.docs
        .iter()
        .map(|d| Target {
            handle: d.handle,
            path: d.path.clone(),
            known: d.disk_stamp(),
            // Only what finding and reading the files needs (not the cached task snapshots).
            links: d.project.external_projects.iter().map(|e| ExternalProject { tasks: Vec::new(), ..e.clone() }).collect(),
        })
        .collect()
}

/// A change found by [`Watcher::poll`].
#[derive(Debug)]
pub enum Change {
    /// The document's file differs from the version the document matches. `read` is the file
    /// read after the change (an error for a file that is not a plan, too large or unreadable),
    /// and `stamp` the version that was read.
    Modified { handle: u64, stamp: Option<DiskStamp>, read: Box<FileResult<(Project, Vec<String>)>> },
    /// The document's file is gone (deleted, moved or renamed).
    Missing { handle: u64 },
    /// The file matches the document again (it was reloaded or saved, or the file came back).
    InSync { handle: u64 },
    /// Files the plan links to changed: they were read, ready for
    /// [`crate::workspace::Document::refresh_external_from`]. `names` are the links' names.
    Links { handle: u64, names: Vec<String>, reads: io::ExternalReads },
}

/// What was last seen of a document's file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Seen {
    /// It matches the document.
    InSync,
    /// It differs (`None`: gone) and was seen once; reported if it stays so until the next poll.
    Pending(Option<DiskStamp>),
    /// This difference was reported.
    Reported(Option<DiskStamp>),
}

#[derive(Debug)]
struct DocState {
    path: PathBuf,
    /// For a document that never read or wrote its file (a restored recovery copy): the file as
    /// first seen, so later changes are still noticed.
    baseline: Option<DiskStamp>,
    seen: Seen,
}

#[derive(Debug)]
struct LinkState {
    path: PathBuf,
    stamp: Option<DiskStamp>,
    pending: Option<Option<DiskStamp>>,
}

/// Remembers what each watched file looked like between polls.
#[derive(Debug, Default)]
pub struct Watcher {
    docs: HashMap<u64, DocState>,
    links: HashMap<(u64, Uuid), LinkState>,
}

impl Watcher {
    pub fn new() -> Self {
        Self::default()
    }

    /// Checks every target's files and returns what changed since the last poll. Reads changed
    /// files, so call it without holding the workspace lock.
    pub fn poll(&mut self, targets: &[Target]) -> Vec<Change> {
        let mut changes = Vec::new();
        self.docs.retain(|h, _| targets.iter().any(|t| t.handle == *h && t.path.is_some()));
        self.links.retain(|(h, id), _| targets.iter().any(|t| t.handle == *h && t.links.iter().any(|l| l.id == *id)));
        for t in targets {
            if let Some(path) = &t.path
                && let Some(c) = self.poll_doc(t.handle, path, t.known)
            {
                changes.push(c);
            }
            if let Some(c) = self.poll_links(t) {
                changes.push(c);
            }
        }
        changes
    }

    fn poll_doc(&mut self, handle: u64, path: &Path, known: Option<DiskStamp>) -> Option<Change> {
        let now = io::disk_stamp(path);
        let state = match self.docs.get_mut(&handle) {
            // Saved under another name: a new file to watch.
            Some(s) if s.path == path => s,
            _ => {
                self.docs.insert(handle, DocState { path: path.to_path_buf(), baseline: now, seen: Seen::InSync });
                self.docs.get_mut(&handle)?
            }
        };
        let reference = known.or(state.baseline);
        if now == reference {
            let was = std::mem::replace(&mut state.seen, Seen::InSync);
            return matches!(was, Seen::Reported(_)).then_some(Change::InSync { handle });
        }
        match state.seen {
            Seen::Reported(seen) if seen == now => None,
            Seen::Pending(seen) if seen == now => match now {
                None => {
                    state.seen = Seen::Reported(None);
                    Some(Change::Missing { handle })
                }
                Some(_) => {
                    let read = read_for_reload(path);
                    match read {
                        // Still being written: wait for it to settle.
                        Ok((_, _, stamp)) if stamp != now => {
                            state.seen = Seen::Pending(stamp);
                            None
                        }
                        Ok((project, notes, stamp)) => {
                            state.seen = Seen::Reported(stamp);
                            Some(Change::Modified { handle, stamp, read: Box::new(Ok((project, notes))) })
                        }
                        Err(e) => {
                            state.seen = Seen::Reported(now);
                            Some(Change::Modified { handle, stamp: now, read: Box::new(Err(e)) })
                        }
                    }
                }
            },
            _ => {
                state.seen = Seen::Pending(now);
                None
            }
        }
    }

    fn poll_links(&mut self, t: &Target) -> Option<Change> {
        let mut changed = Vec::new();
        for link in &t.links {
            // Network shares are only followed on an explicit Refresh Links (see
            // `io::is_network_path`): watching them would connect to the server they name.
            if io::is_network_path(&link.path) {
                continue;
            }
            let path = io::resolve_external_path(&link.path, t.path.as_deref());
            let now = io::disk_stamp(&path);
            let key = (t.handle, link.id);
            let state = match self.links.get_mut(&key) {
                Some(s) if s.path == path => s,
                _ => {
                    self.links.insert(key, LinkState { path, stamp: now, pending: None });
                    continue;
                }
            };
            if now == state.stamp {
                state.pending = None;
            } else if state.pending == Some(now) {
                state.stamp = now;
                state.pending = None;
                // A linked file that is gone keeps its last snapshot (Refresh Links reports it).
                if now.is_some() {
                    changed.push(link.clone());
                }
            } else {
                state.pending = Some(now);
            }
        }
        if changed.is_empty() {
            return None;
        }
        let names = changed.iter().map(|l| l.name.clone()).collect();
        let reads = io::read_external_projects(&changed, t.path.as_deref(), false);
        Some(Change::Links { handle: t.handle, names, reads })
    }
}

/// Most task or resource names listed in one line of [`compare`].
const NAMES_SHOWN: usize = 5;

/// A short, readable summary of how `theirs` (the file on disk) differs from `mine` (the open
/// document): tasks and resources added, removed or changed, and other changes. Empty when the
/// plans are the same apart from calculated values.
pub fn compare(mine: &Project, theirs: &Project) -> Vec<String> {
    fn names<'a>(items: impl Iterator<Item = &'a str>) -> String {
        let all: Vec<&str> = items.collect();
        let mut text = all.iter().take(NAMES_SHOWN).map(|n| format!("'{n}'")).collect::<Vec<_>>().join(", ");
        if all.len() > NAMES_SHOWN {
            text.push_str(&format!(" and {} more", all.len() - NAMES_SHOWN));
        }
        text
    }
    fn line(out: &mut Vec<String>, noun: &str, what: &str, list: Vec<&str>) {
        if !list.is_empty() {
            let plural = if list.len() == 1 { "" } else { "s" };
            out.push(format!("{} {noun}{plural} {what}: {}", list.len(), names(list.iter().copied())));
        }
    }
    let mut out = Vec::new();
    let unscheduled = |t: &crate::model::Task| crate::model::Task { sched: Default::default(), ..t.clone() };
    let added = theirs.tasks.iter().filter(|t| mine.task(t.id).is_none()).map(|t| t.name.as_str()).collect();
    let removed = mine.tasks.iter().filter(|t| theirs.task(t.id).is_none()).map(|t| t.name.as_str()).collect();
    let changed = theirs
        .tasks
        .iter()
        .filter(|t| mine.task(t.id).is_some_and(|m| unscheduled(m) != unscheduled(t)))
        .map(|t| t.name.as_str())
        .collect();
    line(&mut out, "task", "added on disk", added);
    line(&mut out, "task", "missing on disk", removed);
    line(&mut out, "task", "changed on disk", changed);
    let added = theirs.resources.iter().filter(|r| mine.resource(r.id).is_none()).map(|r| r.name.as_str()).collect();
    let removed = mine.resources.iter().filter(|r| theirs.resource(r.id).is_none()).map(|r| r.name.as_str()).collect();
    let changed = theirs.resources.iter().filter(|r| mine.resource(r.id).is_some_and(|m| m != *r)).map(|r| r.name.as_str()).collect();
    line(&mut out, "resource", "added on disk", added);
    line(&mut out, "resource", "missing on disk", removed);
    line(&mut out, "resource", "changed on disk", changed);
    if mine.assignments != theirs.assignments {
        out.push("Resource assignments differ".into());
    }
    if mine.time_entries != theirs.time_entries {
        out.push("Logged time differs".into());
    }
    if mine.calendars != theirs.calendars || mine.calendar != theirs.calendar {
        out.push("Calendars differ".into());
    }
    if mine.id != theirs.id {
        out.push(format!("The file now holds a different project ('{}')", theirs.name));
    } else if mine.name != theirs.name {
        out.push(format!("Project renamed to '{}' on disk", theirs.name));
    }
    if out.is_empty() {
        let unscheduled = |p: &Project| Project {
            sched: Default::default(),
            tasks: p.tasks.iter().map(unscheduled).collect(),
            external_projects: Vec::new(),
            ..p.clone()
        };
        if unscheduled(mine) != unscheduled(theirs) {
            out.push("Other project details differ (settings, notes, fields or views)".into());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::{ChangeSource, DiskStatus};

    fn start() -> crate::dates::Date {
        crate::dates::ymd(2026, 10, 19).unwrap()
    }

    /// Writes `project` the way another program would (replacing the file).
    fn write_elsewhere(project: &Project, path: &Path) {
        io::save(project, path).unwrap();
    }

    fn poll_twice(w: &mut Watcher, ws: &Workspace) -> Vec<Change> {
        let mut first = w.poll(&targets(ws));
        first.extend(w.poll(&targets(ws)));
        first
    }

    #[test]
    fn a_change_is_reported_once_it_settles_and_once_only() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("plan.plancraft");
        let mut p = Project::new("Plan", start());
        p.add_task("A", None);
        io::save(&p, &path).unwrap();
        let mut ws = Workspace::default();
        let h = ws.open(&path).unwrap();
        let mut w = Watcher::new();
        assert!(w.poll(&targets(&ws)).is_empty(), "the file as opened");

        p.add_task("B from another program", None);
        write_elsewhere(&p, &path);
        assert!(w.poll(&targets(&ws)).is_empty(), "seen once: it may still be being written");
        let changes = w.poll(&targets(&ws));
        let [Change::Modified { handle, stamp, read }] = changes.as_slice() else { panic!("{changes:?}") };
        assert_eq!(*handle, h);
        assert_eq!(*stamp, io::disk_stamp(&path));
        let (read, _) = read.as_ref().as_ref().unwrap();
        assert_eq!(read.tasks.len(), 2);
        assert!(w.poll(&targets(&ws)).is_empty(), "reported once");
        let summary = compare(&ws.active_doc().unwrap().project, read);
        assert_eq!(summary, vec!["1 task added on disk: 'B from another program'".to_string()]);
        assert!(compare(read, read).is_empty());

        // Reloading makes the document match the file again.
        let doc = ws.active_doc_mut().unwrap();
        doc.reload(ChangeSource::User).unwrap();
        assert_eq!(doc.disk_status(), DiskStatus::InSync);
        let changes = w.poll(&targets(&ws));
        assert!(matches!(changes.as_slice(), [Change::InSync { handle }] if *handle == h), "{changes:?}");
    }

    #[test]
    fn plancrafts_own_saves_are_not_changes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("plan.plancraft");
        io::save(&Project::new("Plan", start()), &path).unwrap();
        let mut ws = Workspace::default();
        ws.open(&path).unwrap();
        let mut w = Watcher::new();
        w.poll(&targets(&ws));
        for i in 0..3 {
            let doc = ws.active_doc_mut().unwrap();
            doc.edit("Add", |p| p.add_task(format!("T{i}"), None)).unwrap();
            doc.save().unwrap();
            assert!(poll_twice(&mut w, &ws).is_empty());
        }
    }

    #[test]
    fn a_deleted_file_is_missing_and_coming_back_is_in_sync() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("plan.plancraft");
        io::save(&Project::new("Plan", start()), &path).unwrap();
        let mut ws = Workspace::default();
        let h = ws.open(&path).unwrap();
        let mut w = Watcher::new();
        w.poll(&targets(&ws));
        let moved = dir.path().join("moved.plancraft");
        std::fs::rename(&path, &moved).unwrap();
        assert_eq!(ws.active_doc().unwrap().disk_status(), DiskStatus::Missing);
        let changes = poll_twice(&mut w, &ws);
        assert!(matches!(changes.as_slice(), [Change::Missing { handle }] if *handle == h), "{changes:?}");
        assert!(w.poll(&targets(&ws)).is_empty());
        std::fs::rename(&moved, &path).unwrap();
        let changes = w.poll(&targets(&ws));
        assert!(matches!(changes.as_slice(), [Change::InSync { .. }]), "{changes:?}");
    }

    /// A file replaced by garbage, or grown past the size limit, is reported as an error to
    /// show, never a panic, and the document is left alone.
    #[test]
    fn garbage_and_huge_files_are_read_as_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("plan.plancraft");
        io::save(&Project::new("Plan", start()), &path).unwrap();
        let mut ws = Workspace::default();
        ws.open(&path).unwrap();
        let mut w = Watcher::new();
        w.poll(&targets(&ws));

        std::fs::write(&path, b"\xff\xfe not json {{{").unwrap();
        let changes = poll_twice(&mut w, &ws);
        let [Change::Modified { read, .. }] = changes.as_slice() else { panic!("{changes:?}") };
        assert!(read.is_err());

        // A sparse file: large on paper, nothing to read.
        let f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        f.set_len(crate::limits::MAX_FILE_BYTES + 10).unwrap();
        drop(f);
        let changes = poll_twice(&mut w, &ws);
        let [Change::Modified { read, .. }] = changes.as_slice() else { panic!("{changes:?}") };
        assert!(matches!(read.as_ref(), Err(io::FileError::TooLarge { .. })));
        assert_eq!(ws.active_doc().unwrap().project.name, "Plan");

        let doc = ws.active_doc_mut().unwrap();
        assert!(doc.reload(ChangeSource::User).is_err());
        assert_eq!(doc.project.name, "Plan", "a failed reload changes nothing");
        assert!(!doc.can_undo());
    }

    #[test]
    fn a_restored_copy_bound_to_its_file_notices_later_changes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("plan.plancraft");
        io::save(&Project::new("On disk", start()), &path).unwrap();
        let mut ws = Workspace::default();
        // As a restored recovery copy: bound to the file, never read from it.
        ws.add(Project::new("Restored", start()), Some(path.clone()));
        let mut w = Watcher::new();
        assert!(poll_twice(&mut w, &ws).is_empty(), "the file as first seen is the reference");
        let mut p = Project::new("On disk", start());
        p.add_task("New", None);
        write_elsewhere(&p, &path);
        assert!(matches!(poll_twice(&mut w, &ws).as_slice(), [Change::Modified { .. }]));
    }

    #[test]
    fn linked_files_that_change_are_read_for_a_refresh() {
        let dir = tempfile::tempdir().unwrap();
        let other_path = dir.path().join("other.plancraft");
        let mut other = Project::new("Other", start());
        let api = other.add_task("API", None);
        io::save(&other, &other_path).unwrap();
        let main_path = dir.path().join("main.plancraft");
        let mut main = Project::new("Main", start());
        let ext = io::add_external_project(&mut main, Some(&main_path), &other_path).unwrap();
        io::ensure_external_task(&mut main, Some(&main_path), ext, api).unwrap();
        let mine = main.add_task("Mine", None);
        let pred = crate::model::TaskRef::External { project: ext, task: api };
        main.link(pred, mine, crate::model::LinkType::FS, crate::duration::Lag::None).unwrap();
        io::save(&main, &main_path).unwrap();
        let mut ws = Workspace::default();
        let h = ws.open(&main_path).unwrap();
        let finish_before = ws.active_doc().unwrap().project.task(mine).unwrap().sched.finish;
        let mut w = Watcher::new();
        assert!(poll_twice(&mut w, &ws).is_empty());

        other.task_mut(api).unwrap().duration *= 3;
        io::save(&other, &other_path).unwrap();
        let mut changes = poll_twice(&mut w, &ws);
        let Some(Change::Links { handle, names, reads }) = changes.pop() else { panic!("{changes:?}") };
        assert_eq!((handle, names), (h, vec!["Other".to_string()]));
        let doc = ws.active_doc_mut().unwrap();
        let warnings = doc.refresh_external_from(reads);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert!(doc.project.task(mine).unwrap().sched.finish > finish_before, "the successor moved with the linked task");
        assert!(!doc.dirty && !doc.can_undo(), "a snapshot refresh is no edit, as on open");
        assert!(w.poll(&targets(&ws)).is_empty());

        // A linked file replaced by garbage gives a warning, not a panic.
        std::fs::write(&other_path, "garbage").unwrap();
        let mut changes = poll_twice(&mut w, &ws);
        let Some(Change::Links { reads, .. }) = changes.pop() else { panic!("{changes:?}") };
        let warnings = ws.active_doc_mut().unwrap().refresh_external_from(reads);
        assert!(warnings.iter().any(|m| m.contains("Could not refresh")), "{warnings:?}");
    }
}
