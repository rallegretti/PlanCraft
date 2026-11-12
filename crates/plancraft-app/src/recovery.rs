//! Recovery copies of unsaved work, so a crash or power cut never loses someone's plan.
//!
//! Every minute (when something changed) each modified document is written to the per-user
//! recovery directory as `<project-uuid>-<handle>.plancraft`, next to a small `.json` sidecar
//! with its name, original file and time. A copy is deleted once its document is saved or
//! closed, and on a clean exit; copies left behind by a crash are offered on the Start Page.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::{Duration, Instant};

use chrono::{DateTime, Local};
use plancraft_core::io::{self, FileResult, Privacy, create_private_dir, write_atomic};
use plancraft_core::model::{FILE_EXTENSION, Project};
use plancraft_core::workspace::{Document, SharedWorkspace, Workspace};
use serde::{Deserialize, Serialize};

/// How often modified documents are copied.
pub const INTERVAL: Duration = Duration::from_secs(60);
/// Sidecars are tiny; anything bigger is not ours.
const MAX_SIDECAR_BYTES: u64 = 64 * 1024;
/// Longest name shown for a recovered document.
const MAX_NAME_CHARS: usize = 200;

#[derive(Serialize, Deserialize)]
struct Sidecar {
    name: String,
    #[serde(default)]
    original_path: Option<PathBuf>,
    saved_at: DateTime<Local>,
}

/// A recovery copy found on disk.
#[derive(Clone, Debug, PartialEq)]
pub struct RecoveredItem {
    /// File stem inside the recovery directory (identifies the copy).
    pub stem: String,
    /// Project name.
    pub name: String,
    /// The file the document was opened from or last saved to, if any.
    pub original_path: Option<PathBuf>,
    pub saved_at: Option<DateTime<Local>>,
}

/// The directory holding recovery copies (injectable so tests never touch the real one).
#[derive(Clone, Debug)]
pub struct RecoveryStore {
    dir: PathBuf,
}

impl RecoveryStore {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// The per-user recovery directory.
    pub fn user() -> Option<Self> {
        plancraft_core::paths::recovery_dir().map(Self::new)
    }

    fn file(&self, stem: &str) -> PathBuf {
        self.dir.join(format!("{stem}.{FILE_EXTENSION}"))
    }

    fn sidecar(&self, stem: &str) -> PathBuf {
        self.dir.join(format!("{stem}.json"))
    }

    /// Writes `doc` as the copy `stem` (atomically, readable by the owner only).
    pub fn write(&self, stem: &str, doc: &Document) -> std::io::Result<()> {
        create_private_dir(&self.dir)?;
        let text = io::to_json(&doc.project).map_err(std::io::Error::other)?;
        write_atomic(&self.file(stem), text.as_bytes(), Privacy::OwnerOnly)?;
        let meta = Sidecar { name: doc.project.name.clone(), original_path: doc.path.clone(), saved_at: Local::now() };
        let meta = serde_json::to_vec_pretty(&meta).map_err(std::io::Error::other)?;
        write_atomic(&self.sidecar(stem), &meta, Privacy::OwnerOnly)
    }

    /// Deletes the copy `stem` and its sidecar (missing files are fine).
    pub fn remove(&self, stem: &str) {
        let _ = std::fs::remove_file(self.file(stem));
        let _ = std::fs::remove_file(self.sidecar(stem));
    }

    /// The copies in the directory, newest first.
    pub fn list(&self) -> Vec<RecoveredItem> {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        let mut items: Vec<RecoveredItem> = entries
            .filter_map(Result::ok)
            .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
            .filter_map(|e| {
                let path = e.path();
                if path.extension().and_then(|x| x.to_str()) != Some(FILE_EXTENSION) {
                    return None;
                }
                // Skip temporary files of interrupted atomic writes and anything not named by us.
                let stem = path.file_stem()?.to_str()?.to_string();
                (!stem.is_empty() && !stem.starts_with('.')).then(|| self.describe(stem, &path))
            })
            .collect();
        items.sort_by(|a, b| b.saved_at.cmp(&a.saved_at).then_with(|| a.stem.cmp(&b.stem)));
        items
    }

    /// Reads the sidecar of a copy; a missing or damaged sidecar falls back to the file itself.
    fn describe(&self, stem: String, file: &Path) -> RecoveredItem {
        let meta = read_capped(&self.sidecar(&stem), MAX_SIDECAR_BYTES).and_then(|text| serde_json::from_str::<Sidecar>(&text).ok());
        match meta {
            Some(m) => RecoveredItem {
                name: truncate_chars(m.name.trim(), MAX_NAME_CHARS),
                original_path: m.original_path.filter(|p| !p.as_os_str().is_empty()),
                saved_at: Some(m.saved_at),
                stem,
            },
            None => RecoveredItem {
                name: stem.clone(),
                original_path: None,
                saved_at: std::fs::metadata(file).and_then(|m| m.modified()).ok().map(DateTime::<Local>::from),
                stem,
            },
        }
    }

    /// Loads a copy, treating it as untrusted like any other file.
    pub fn load(&self, item: &RecoveredItem) -> FileResult<(Project, Vec<String>)> {
        io::load_with_notes(self.file(&item.stem))
    }
}

fn read_capped(path: &Path, max: u64) -> Option<String> {
    let file = std::fs::File::open(path).ok()?;
    if file.metadata().ok()?.len() > max {
        return None;
    }
    let mut text = String::new();
    file.take(max).read_to_string(&mut text).ok()?;
    Some(text)
}

fn truncate_chars(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((cut, _)) => format!("{}…", s.get(..cut).unwrap_or(s)),
        None => s.to_string(),
    }
}

/// Keeps recovery copies in step with the open documents.
pub struct Recovery {
    store: Option<RecoveryStore>,
    /// Copies written by this session: document handle → (stem, revision copied).
    written: HashMap<u64, (String, u64)>,
    last_write: Instant,
    /// Copies left by an earlier session, offered on the Start Page.
    pub found: Vec<RecoveredItem>,
    /// Outcome of the last timed copy (`tick`), shown by the app once per distinct error.
    pub last_error: Option<String>,
    /// Set on a clean exit: no more copies may be written after they were deleted.
    stopped: bool,
}

/// Recovery state shared by the app and its background writer.
pub type SharedRecovery = Arc<Mutex<Recovery>>;

/// Locks the shared recovery state (a panic elsewhere never makes it unusable).
pub fn lock(r: &SharedRecovery) -> MutexGuard<'_, Recovery> {
    r.lock().unwrap_or_else(PoisonError::into_inner)
}

/// How often the background writer checks whether a copy is due.
const WRITER_POLL: Duration = Duration::from_secs(5);

/// Starts a thread that keeps recovery copies current even when no frames are drawn (the window
/// is minimized, hidden or simply idle; egui only runs the UI on input). It takes the workspace
/// lock before the recovery lock, the same order as the UI, and ends with the app.
pub fn spawn_background_writer(rec: &SharedRecovery, ws: &SharedWorkspace, ctx: egui::Context) {
    spawn_writer_every(rec, ws, ctx, WRITER_POLL);
}

fn spawn_writer_every(rec: &SharedRecovery, ws: &SharedWorkspace, ctx: egui::Context, poll: Duration) {
    let rec: Weak<Mutex<Recovery>> = Arc::downgrade(rec);
    let ws = Arc::downgrade(ws);
    let spawned = std::thread::Builder::new().name("plancraft-recovery".into()).spawn(move || {
        loop {
            std::thread::sleep(poll);
            let (Some(rec), Some(ws)) = (rec.upgrade(), ws.upgrade()) else { return };
            let ws = ws.lock().unwrap_or_else(PoisonError::into_inner);
            let mut r = lock(&rec);
            if r.stopped {
                return;
            }
            let before = r.last_error.clone();
            // Already guarded inside: a failure is reported, never raised.
            let _ = r.tick(&ws);
            if r.last_error != before {
                ctx.request_repaint();
            }
        }
    });
    if let Err(e) = spawned {
        eprintln!("plancraft: could not start the recovery writer ({e}); copies are written while the window is in use");
    }
}

impl Recovery {
    pub fn new(store: Option<RecoveryStore>) -> Self {
        let found = store.as_ref().map(RecoveryStore::list).unwrap_or_default();
        Self { store, written: HashMap::new(), last_write: Instant::now(), found, last_error: None, stopped: false }
    }

    pub fn store(&self) -> Option<&RecoveryStore> {
        self.store.as_ref()
    }

    /// Called every frame: drops copies of documents that were saved or closed and, once a
    /// minute, copies documents that changed. Returns an error message when a copy failed.
    pub fn tick(&mut self, ws: &Workspace) -> Result<(), String> {
        if self.stopped {
            return Ok(());
        }
        self.prune(ws);
        if self.last_write.elapsed() < INTERVAL {
            return Ok(());
        }
        self.last_write = Instant::now();
        let r = self.write_dirty(ws).map(|_| ());
        self.last_error = r.clone().err();
        r
    }

    /// Deletes the copies of documents that are no longer open or no longer modified.
    pub fn prune(&mut self, ws: &Workspace) {
        let Some(store) = &self.store else { return };
        self.written.retain(|handle, (stem, _)| {
            let keep = ws.docs.iter().any(|d| d.handle == *handle && d.dirty);
            if !keep {
                store.remove(stem);
            }
            keep
        });
    }

    /// Copies every modified document that changed since its last copy, now.
    /// Returns how many were written.
    pub fn write_dirty(&mut self, ws: &Workspace) -> Result<usize, String> {
        let Some(store) = &self.store else {
            return Err("there is no per-user folder for recovery copies".into());
        };
        let mut count = 0;
        let mut errors = Vec::new();
        for doc in ws.docs.iter().filter(|d| d.dirty) {
            let stem = match self.written.get(&doc.handle) {
                Some((_, rev)) if *rev == doc.revision => continue,
                Some((stem, _)) => stem.clone(),
                None => self.fresh_stem(doc),
            };
            match store.write(&stem, doc) {
                Ok(()) => {
                    self.written.insert(doc.handle, (stem, doc.revision));
                    count += 1;
                }
                Err(e) => errors.push(format!("{}: {e}", doc.project.name)),
            }
        }
        if errors.is_empty() { Ok(count) } else { Err(format!("Could not write a recovery copy of {}", errors.join("; "))) }
    }

    /// `<project-uuid>-<handle>`, made unique against copies still waiting on the Start Page.
    fn fresh_stem(&self, doc: &Document) -> String {
        let base = format!("{}-{}", doc.project.id, doc.handle);
        let taken = |s: &str| self.found.iter().any(|i| i.stem == s) || self.written.values().any(|(w, _)| w == s);
        if !taken(&base) {
            return base;
        }
        (2u32..).map(|n| format!("{base}-{n}")).find(|s| !taken(s)).unwrap_or(base)
    }

    /// The stem this session uses for a document, once it has been copied.
    #[cfg(test)]
    pub fn stem_of(&self, handle: u64) -> Option<&str> {
        self.written.get(&handle).map(|(s, _)| s.as_str())
    }

    /// Clean exit: deletes every copy this session wrote and stops timed copies.
    pub fn forget_all(&mut self) {
        self.stopped = true;
        if let Some(store) = &self.store {
            for (stem, _) in self.written.values() {
                store.remove(stem);
            }
        }
        self.written.clear();
    }

    pub fn find(&self, stem: &str) -> Option<&RecoveredItem> {
        self.found.iter().find(|i| i.stem == stem)
    }

    /// Deletes a copy found at startup.
    pub fn discard(&mut self, stem: &str) {
        self.found.retain(|i| i.stem != stem);
        let owned = self.written.values().any(|(s, _)| s == stem);
        if let Some(store) = &self.store
            && !owned
        {
            store.remove(stem);
        }
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;

    /// A fresh, empty directory under the system temp dir (removed by [`TempDir`]'s drop).
    pub struct TempDir(pub PathBuf);

    impl TempDir {
        pub fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("plancraft-{tag}-{}-{}", std::process::id(), uuid::Uuid::new_v4().simple()));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn workspace_with_edit() -> Workspace {
        let mut ws = Workspace::default();
        ws.add(
            Project::new("Recovered Plan", chrono::NaiveDate::from_ymd_opt(2026, 10, 19).unwrap()),
            Some(PathBuf::from("/plans/x.plancraft")),
        );
        ws.active_doc_mut().unwrap().edit("Add", |p| p.add_task("Unsaved task", None)).unwrap();
        ws
    }

    #[test]
    fn copies_round_trip_and_follow_saves_and_closes() {
        let tmp = TempDir::new("recovery");
        let store = RecoveryStore::new(tmp.0.join("recovery"));
        let mut ws = workspace_with_edit();
        let handle = ws.active_doc().unwrap().handle;

        // Write: only modified documents that changed since the last copy.
        let mut rec = Recovery::new(Some(store.clone()));
        assert!(rec.found.is_empty());
        assert_eq!(rec.write_dirty(&ws), Ok(1));
        assert_eq!(rec.write_dirty(&ws), Ok(0), "unchanged documents are not copied again");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let stem = rec.stem_of(handle).unwrap().to_string();
            let mode = std::fs::metadata(store.file(&stem)).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
            let dir_mode = std::fs::metadata(tmp.0.join("recovery")).unwrap().permissions().mode();
            assert_eq!(dir_mode & 0o777, 0o700);
        }

        // List: a new session (after a crash) finds the copy with its sidecar details.
        let mut next = Recovery::new(Some(store.clone()));
        assert_eq!(next.found.len(), 1);
        let item = next.found[0].clone();
        assert_eq!(item.name, "Recovered Plan");
        assert_eq!(item.original_path.as_deref(), Some(Path::new("/plans/x.plancraft")));
        assert!(item.saved_at.is_some());

        // Restore: the copy loads as a normal (hostile-file safe) document.
        let (project, notes) = store.load(&item).unwrap();
        assert!(notes.is_empty());
        assert_eq!(project.tasks.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(), ["Unsaved task"]);

        // Discard: the copy and its sidecar are gone.
        next.discard(&item.stem);
        assert!(next.found.is_empty());
        assert!(store.list().is_empty());
        assert_eq!(std::fs::read_dir(tmp.0.join("recovery")).unwrap().count(), 0);

        // Saving (no longer dirty) or closing deletes this session's copy.
        ws.active_doc_mut().unwrap().edit("More", |p| p.add_task("More", None)).unwrap();
        assert_eq!(rec.write_dirty(&ws), Ok(1));
        assert_eq!(store.list().len(), 1);
        ws.active_doc_mut().unwrap().dirty = false;
        rec.prune(&ws);
        assert!(store.list().is_empty());
        ws.active_doc_mut().unwrap().dirty = true;
        rec.write_dirty(&ws).unwrap();
        ws.close(handle);
        rec.prune(&ws);
        assert!(store.list().is_empty());

        // A clean exit deletes what this session wrote, but not copies awaiting a decision.
        let ws = workspace_with_edit();
        rec.write_dirty(&ws).unwrap();
        std::fs::write(store.file("left-over"), b"{}").unwrap();
        rec.forget_all();
        assert_eq!(store.list().iter().map(|i| i.stem.as_str()).collect::<Vec<_>>(), ["left-over"]);
    }

    #[test]
    fn new_copies_never_overwrite_unresolved_ones_and_garbage_is_tolerated() {
        let tmp = TempDir::new("recovery-names");
        let store = RecoveryStore::new(&tmp.0);
        let ws = workspace_with_edit();
        let mut first = Recovery::new(Some(store.clone()));
        first.write_dirty(&ws).unwrap();
        let old = first.stem_of(ws.active_doc().unwrap().handle).unwrap().to_string();

        // The same project in the same slot in a later session gets its own copy.
        let mut second = Recovery::new(Some(store.clone()));
        second.write_dirty(&ws).unwrap();
        let new = second.stem_of(ws.active_doc().unwrap().handle).unwrap().to_string();
        assert_ne!(old, new);
        assert_eq!(store.list().len(), 2);

        // A damaged sidecar or copy is listed by file name and fails to load cleanly.
        std::fs::write(store.sidecar(&old), b"\xff not json").unwrap();
        std::fs::write(store.file(&old), b"garbage").unwrap();
        let item = store.list().into_iter().find(|i| i.stem == old).unwrap();
        assert_eq!(item.name, old);
        assert!(item.original_path.is_none());
        assert!(store.load(&item).is_err());
    }

    /// Regression: copies were only written while frames were drawn, and egui draws none while
    /// the window is idle, hidden or minimized, so a crash after a quiet spell lost the work.
    #[test]
    fn the_background_writer_copies_without_frames_and_stops_on_exit() {
        let tmp = TempDir::new("recovery-writer");
        let store = RecoveryStore::new(&tmp.0);
        let ws = workspace_with_edit().shared();
        let mut rec = Recovery::new(Some(store.clone()));
        rec.last_write = Instant::now().checked_sub(INTERVAL).unwrap();
        let rec: SharedRecovery = Arc::new(Mutex::new(rec));
        spawn_writer_every(&rec, &ws, egui::Context::default(), Duration::from_millis(10));
        let waited = Instant::now();
        while store.list().is_empty() && waited.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(store.list().len(), 1, "a copy appears with no frame drawn");

        // A clean exit deletes the copies and no later tick writes new ones.
        lock(&rec).forget_all();
        ws.lock().unwrap().active_doc_mut().unwrap().edit("More", |p| p.add_task("More", None)).unwrap();
        lock(&rec).last_write = Instant::now().checked_sub(INTERVAL).unwrap();
        std::thread::sleep(Duration::from_millis(100));
        assert!(store.list().is_empty());
    }

    #[test]
    fn names_are_cut_at_char_boundaries() {
        assert_eq!(truncate_chars("äöü", 2), "äö…");
        assert_eq!(truncate_chars("ab", 5), "ab");
    }
}
