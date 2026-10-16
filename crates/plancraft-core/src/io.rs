//! Reading and writing `.plancraft` documents and resolving links between documents.
//!
//! Documents are pretty-printed JSON so they diff well under version control
//! and are easy for tools to inspect.

use std::collections::HashSet;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use uuid::Uuid;

use crate::ids::TaskId;
use crate::limits::{self, MAX_FILE_BYTES};
use crate::model::*;
use crate::schedule::recalculate;

#[derive(Debug, thiserror::Error)]
pub enum FileError {
    #[error("I/O error on {path}: {source}")]
    Io { path: PathBuf, source: std::io::Error },
    #[error("{path} is not a valid PlanCraft document: {source}")]
    Parse { path: PathBuf, source: serde_json::Error },
    #[error("{0} is not a PlanCraft document")]
    WrongFormat(PathBuf),
    #[error("{path} was written by a newer PlanCraft (format version {version})")]
    TooNew { path: PathBuf, version: u32 },
    #[error("could not serialize project: {0}")]
    Serialize(#[from] serde_json::Error),
    #[error("{path} is too large to open ({size} bytes; the maximum is {max} bytes)", max = MAX_FILE_BYTES)]
    TooLarge { path: PathBuf, size: u64 },
    #[error("{path} cannot be opened: {message}")]
    Limit { path: PathBuf, message: String },
}

pub type FileResult<T> = Result<T, FileError>;

/// Serializes a project to pretty JSON.
pub fn to_json(p: &Project) -> FileResult<String> {
    let mut s = serde_json::to_string_pretty(p)?;
    s.push('\n');
    Ok(s)
}

/// Parses a project from JSON text.
pub fn from_json(text: &str, path: &Path) -> FileResult<Project> {
    from_json_with_notes(text, path).map(|(p, _)| p)
}

/// The fields checked before the whole document is parsed.
#[derive(serde::Deserialize)]
struct Header {
    format: Option<String>,
    version: Option<u64>,
}

/// Parses a project from JSON text, treating it as untrusted.
///
/// Documents with more items than [`crate::limits`] allow are refused; out-of-range values are
/// clamped and described in the returned notes.
pub fn from_json_with_notes(text: &str, path: &Path) -> FileResult<(Project, Vec<String>)> {
    let header: Header = serde_json::from_str(text).map_err(|source| FileError::Parse { path: path.into(), source })?;
    if header.format.as_deref() != Some(FILE_FORMAT) {
        return Err(FileError::WrongFormat(path.into()));
    }
    let version = header.version.map_or(0, |v| u32::try_from(v).unwrap_or(u32::MAX));
    if version > FILE_VERSION {
        return Err(FileError::TooNew { path: path.into(), version });
    }
    let mut p: Project = serde_json::from_str(text).map_err(|source| FileError::Parse { path: path.into(), source })?;
    limits::check_counts(&p).map_err(|e| FileError::Limit { path: path.into(), message: e.to_string() })?;
    p.version = FILE_VERSION;
    // Keep id counters ahead of every stored id (hand-edited files may lag behind).
    p.ids.task = p.ids.task.max(p.tasks.iter().map(|t| t.id.0).max().unwrap_or(0));
    p.ids.resource = p.ids.resource.max(p.resources.iter().map(|r| r.id.0).max().unwrap_or(0));
    p.ids.calendar = p.ids.calendar.max(p.calendars.iter().map(|c| c.id.0).max().unwrap_or(0));
    p.ids.assignment = p.ids.assignment.max(p.assignments.iter().map(|a| a.id.0).max().unwrap_or(0));
    p.ids.time_entry = p.ids.time_entry.max(p.time_entries.iter().map(|e| e.id.0).max().unwrap_or(0));
    if p.calendars.is_empty() {
        let id = p.ids.next_calendar();
        p.calendars.push(crate::calendar::Calendar::standard(id));
        p.calendar = id;
    }
    let notes = limits::sanitize(&mut p);
    Ok((p, notes))
}

/// Loads a project document and recalculates it.
pub fn load(path: impl AsRef<Path>) -> FileResult<Project> {
    load_with_notes(path).map(|(p, _)| p)
}

/// Loads a project document and recalculates it, also returning what was clamped into range.
pub fn load_with_notes(path: impl AsRef<Path>) -> FileResult<(Project, Vec<String>)> {
    let path = path.as_ref();
    let io_err = |source| FileError::Io { path: path.into(), source };
    let file = std::fs::File::open(path).map_err(io_err)?;
    let size = file.metadata().map_err(io_err)?.len();
    if size > MAX_FILE_BYTES {
        return Err(FileError::TooLarge { path: path.into(), size });
    }
    let mut text = String::new();
    file.take(MAX_FILE_BYTES + 1).read_to_string(&mut text).map_err(io_err)?;
    let size = u64::try_from(text.len()).unwrap_or(u64::MAX);
    if size > MAX_FILE_BYTES {
        return Err(FileError::TooLarge { path: path.into(), size });
    }
    let (mut p, notes) = from_json_with_notes(&text, path)?;
    recalculate(&mut p);
    Ok((p, notes))
}

/// Saves a project document atomically (see [`write_atomic`]).
pub fn save(p: &Project, path: impl AsRef<Path>) -> FileResult<()> {
    let path = path.as_ref();
    let text = to_json(p)?;
    write_atomic(path, text.as_bytes(), Privacy::KeepExisting).map_err(|source| FileError::Io { path: path.into(), source })
}

/// Who may read a file written by [`write_atomic`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Privacy {
    /// A new file is readable by its owner only (0600 on Unix); a replaced file keeps its permissions.
    KeepExisting,
    /// Always readable by the owner only (settings, recovery copies, session tokens).
    OwnerOnly,
}

/// Writes `bytes` to `path` so that a crash or power cut never leaves a torn file.
///
/// The data goes to a temporary file in the same directory, is flushed to disk (`fsync`) and is
/// then renamed over `path` in one step, so `path` holds either the old or the new contents. On
/// Unix the directory is synced too, so the rename itself survives a power cut.
pub fn write_atomic(path: &Path, bytes: &[u8], privacy: Privacy) -> std::io::Result<()> {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let dir = match path.parent() {
        Some(d) if !d.as_os_str().is_empty() => d.to_path_buf(),
        _ => PathBuf::from("."),
    };
    let name = path.file_name().ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "not a file path"))?;
    let tmp = dir.join(format!(".{}.{}-{}.tmp", name.to_string_lossy(), std::process::id(), COUNTER.fetch_add(1, Ordering::Relaxed)));
    let keep = match privacy {
        Privacy::KeepExisting => std::fs::metadata(path).ok().map(|m| m.permissions()),
        Privacy::OwnerOnly => None,
    };
    let result = (|| {
        let mut f = create_private(&tmp)?;
        f.write_all(bytes)?;
        if let Some(perms) = keep {
            f.set_permissions(perms)?;
        }
        f.sync_all()?;
        drop(f);
        rename_over(&tmp, path)?;
        sync_dir(&dir);
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// Creates a new file readable and writable by its owner only.
fn create_private(path: &Path) -> std::io::Result<std::fs::File> {
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
    opts.open(path)
}

/// Creates `dir` (and its parents) if needed and makes it private to its owner (0700 on Unix).
pub fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(dir)?;
    #[cfg(unix)]
    std::fs::set_permissions(dir, std::os::unix::fs::PermissionsExt::from_mode(0o700))?;
    Ok(())
}

/// Renames `from` over `to`. On Windows a virus scanner or indexer can hold the destination open
/// for a moment, so the rename is retried briefly.
fn rename_over(from: &Path, to: &Path) -> std::io::Result<()> {
    let mut attempt = 0;
    loop {
        match std::fs::rename(from, to) {
            Err(e) if cfg!(windows) && e.kind() == std::io::ErrorKind::PermissionDenied && attempt < 10 => {
                attempt += 1;
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            other => return other,
        }
    }
}

fn sync_dir(dir: &Path) {
    #[cfg(unix)]
    if let Ok(d) = std::fs::File::open(dir) {
        let _ = d.sync_all();
    }
    #[cfg(not(unix))]
    let _ = dir;
}

/// Lexically normalizes a path (resolves `.` and `..` without touching the filesystem).
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Path of `target` relative to directory `base`, when both are absolute on the same root.
pub fn relative_path(target: &Path, base: &Path) -> PathBuf {
    let t = normalize(target);
    let b = normalize(base);
    if !t.is_absolute() || !b.is_absolute() {
        return t;
    }
    let tc: Vec<_> = t.components().collect();
    let bc: Vec<_> = b.components().collect();
    let common = tc.iter().zip(bc.iter()).take_while(|(a, b)| a == b).count();
    if common == 0 {
        return t;
    }
    let mut out = PathBuf::new();
    for _ in common..bc.len() {
        out.push("..");
    }
    for c in tc.iter().skip(common) {
        out.push(c.as_os_str());
    }
    out
}

/// Resolves a stored external path against the directory of the referencing document.
pub fn resolve_external_path(stored: &str, doc_path: Option<&Path>) -> PathBuf {
    let p = PathBuf::from(stored);
    if p.is_absolute() {
        return p;
    }
    match doc_path.and_then(|d| d.parent()) {
        Some(dir) => normalize(&dir.join(p)),
        None => p,
    }
}

fn snapshot(t: &Task) -> ExternalTaskSnapshot {
    ExternalTaskSnapshot {
        id: t.id,
        name: t.name.clone(),
        start: t.sched.start,
        finish: t.sched.finish,
        percent_complete: t.sched.percent_complete.round().clamp(0.0, 100.0) as u8,
        milestone: t.is_milestone(),
        duration: t.sched.duration,
    }
}

/// External task ids referenced by predecessors or milestone members.
pub fn referenced_external_tasks(p: &Project, project: Uuid) -> HashSet<TaskId> {
    let mut out = HashSet::new();
    for t in &p.tasks {
        for r in t.predecessors.iter().map(|d| &d.predecessor).chain(t.members.iter()) {
            if let TaskRef::External { project: pid, task } = r
                && *pid == project
            {
                out.insert(*task);
            }
        }
    }
    out
}

/// Registers another document as an external project and returns its id.
///
/// The stored path is relative to `doc_path`'s directory when `doc_path` is known.
pub fn add_external_project(p: &mut Project, doc_path: Option<&Path>, other: &Path) -> FileResult<Uuid> {
    let abs_other = if other.is_absolute() {
        other.to_path_buf()
    } else {
        std::env::current_dir().map(|c| c.join(other)).unwrap_or_else(|_| other.to_path_buf())
    };
    let other_project = load(&abs_other)?;
    let stored = match doc_path.and_then(|d| d.parent()) {
        Some(dir) => {
            let dir = if dir.is_absolute() { dir.to_path_buf() } else { std::env::current_dir().map(|c| c.join(dir)).unwrap_or_default() };
            relative_path(&abs_other, &dir)
        }
        None => abs_other.clone(),
    };
    let stored = stored.to_string_lossy().replace('\\', "/");
    if let Some(e) = p.external_projects.iter_mut().find(|e| e.id == other_project.id) {
        e.path = stored;
        e.name = other_project.name.clone();
        return Ok(e.id);
    }
    p.external_projects.push(ExternalProject {
        id: other_project.id,
        name: other_project.name.clone(),
        path: stored,
        tasks: Vec::new(),
        last_sync: Some(chrono::Local::now().naive_local()),
    });
    Ok(other_project.id)
}

/// Ensures snapshot data exists for an external task (loading the other document).
pub fn ensure_external_task(p: &mut Project, doc_path: Option<&Path>, project: Uuid, task: TaskId) -> FileResult<()> {
    let Some(ext) = p.external_projects.iter().find(|e| e.id == project) else {
        return Ok(());
    };
    let path = resolve_external_path(&ext.path, doc_path);
    let other = load(&path)?;
    if let Some(ext) = p.external_projects.iter_mut().find(|e| e.id == project)
        && let Some(t) = other.task(task)
    {
        ext.tasks.retain(|s| s.id != task);
        ext.tasks.push(snapshot(t));
    }
    Ok(())
}

/// Lists the tasks of an external project document (loading it from disk).
pub fn external_task_list(p: &Project, doc_path: Option<&Path>, project: Uuid) -> FileResult<Vec<ExternalTaskSnapshot>> {
    let Some(ext) = p.external(project) else {
        return Ok(Vec::new());
    };
    let other = load(resolve_external_path(&ext.path, doc_path))?;
    Ok(other.tasks.iter().map(snapshot).collect())
}

/// Refreshes the cached dates of every referenced external task. Returns warnings.
pub fn refresh_external(p: &mut Project, doc_path: Option<&Path>) -> Vec<String> {
    let mut warnings = Vec::new();
    let now = chrono::Local::now().naive_local();
    for i in 0..p.external_projects.len() {
        let Some((id, stored, name)) = p.external_projects.get(i).map(|e| (e.id, e.path.clone(), e.name.clone())) else {
            continue;
        };
        let wanted = referenced_external_tasks(p, id);
        let path = resolve_external_path(&stored, doc_path);
        match load(&path) {
            Ok(other) => {
                if other.id != id {
                    warnings.push(format!(
                        "{} now contains a different project ('{}'); links were not refreshed",
                        path.display(),
                        other.name
                    ));
                    continue;
                }
                let Some(ext) = p.external_projects.get_mut(i) else { continue };
                ext.name = other.name.clone();
                ext.tasks.retain(|s| wanted.contains(&s.id));
                for tid in &wanted {
                    match other.task(*tid) {
                        Some(t) => {
                            ext.tasks.retain(|s| s.id != *tid);
                            ext.tasks.push(snapshot(t));
                        }
                        None => warnings.push(format!("Task {} no longer exists in '{}'", tid.0, other.name)),
                    }
                }
                ext.tasks.sort_by_key(|s| s.id);
                ext.last_sync = Some(now);
            }
            Err(e) => warnings.push(format!("Could not refresh '{name}': {e}")),
        }
    }
    warnings
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_save_replaces_and_leaves_no_temp_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("plan.plancraft");
        let mut p = Project::new("A", chrono::NaiveDate::from_ymd_opt(2026, 10, 19).unwrap());
        save(&p, &path).unwrap();
        p.name = "B".into();
        save(&p, &path).unwrap();
        assert_eq!(load(&path).unwrap().name, "B");
        let names: Vec<_> = std::fs::read_dir(dir.path()).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(names, vec![std::ffi::OsString::from("plan.plancraft")]);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600, "new plans are private");
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
            save(&p, &path).unwrap();
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o640, "a replaced file keeps its mode");
            let private = dir.path().join("private/state");
            create_private_dir(&private).unwrap();
            assert_eq!(std::fs::metadata(&private).unwrap().permissions().mode() & 0o777, 0o700);
        }
    }

    #[test]
    fn hostile_files_are_refused_or_clamped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.plancraft");
        let mut p = Project::new("A", chrono::NaiveDate::from_ymd_opt(2026, 10, 19).unwrap());
        let a = p.add_task("A", None);
        let mut json: serde_json::Value = serde_json::from_str(&to_json(&p).unwrap()).unwrap();
        json["tasks"][0]["duration"] = serde_json::json!(i64::MAX);
        json["tasks"][0]["percent_complete"] = serde_json::json!(255);
        json["start"] = serde_json::json!("+200000-01-01T08:00:00");
        std::fs::write(&path, json.to_string()).unwrap();
        let (q, notes) = load_with_notes(&path).unwrap();
        assert_eq!(q.task(a).unwrap().duration, limits::MAX_MINUTES);
        assert_eq!(q.task(a).unwrap().percent_complete, 100);
        assert_eq!(q.start.date(), limits::max_date());
        assert!(!notes.is_empty());

        json["version"] = serde_json::json!(u64::from(u32::MAX) + 1);
        std::fs::write(&path, json.to_string()).unwrap();
        assert!(matches!(load(&path), Err(FileError::TooNew { .. })));

        json["version"] = serde_json::json!(1);
        let task = json["tasks"][0].clone();
        json["tasks"] = serde_json::Value::Array(vec![task; 2]);
        std::fs::write(&path, json.to_string()).unwrap();
        assert!(matches!(load(&path), Err(FileError::Limit { .. })), "duplicate ids are refused");

        let deep = format!("{{\"format\":\"plancraft\",\"x\":{}{}}}", "[".repeat(100_000), "]".repeat(100_000));
        std::fs::write(&path, deep).unwrap();
        assert!(load(&path).is_err());
    }

    #[test]
    fn relative_paths() {
        assert_eq!(relative_path(Path::new("/a/b/c.plancraft"), Path::new("/a/b")), PathBuf::from("c.plancraft"));
        assert_eq!(relative_path(Path::new("/a/x/c.plancraft"), Path::new("/a/b")), PathBuf::from("../x/c.plancraft"));
        assert_eq!(resolve_external_path("../x/c.plancraft", Some(Path::new("/a/b/doc.plancraft"))), PathBuf::from("/a/x/c.plancraft"));
    }
}
