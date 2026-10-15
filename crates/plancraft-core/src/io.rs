//! Reading and writing `.plancraft` documents and resolving links between documents.
//!
//! Documents are pretty-printed JSON so they diff well under version control
//! and are easy for tools to inspect.

use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

use uuid::Uuid;

use crate::ids::TaskId;
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
    let value: serde_json::Value = serde_json::from_str(text).map_err(|source| FileError::Parse { path: path.into(), source })?;
    if value.get("format").and_then(|f| f.as_str()) != Some(FILE_FORMAT) {
        return Err(FileError::WrongFormat(path.into()));
    }
    let version = value.get("version").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
    if version > FILE_VERSION {
        return Err(FileError::TooNew { path: path.into(), version });
    }
    let mut p: Project = serde_json::from_value(value).map_err(|source| FileError::Parse { path: path.into(), source })?;
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
    p.normalize_outline();
    Ok(p)
}

/// Loads a project document and recalculates it.
pub fn load(path: impl AsRef<Path>) -> FileResult<Project> {
    let path = path.as_ref();
    let text = std::fs::read_to_string(path).map_err(|source| FileError::Io { path: path.into(), source })?;
    let mut p = from_json(&text, path)?;
    recalculate(&mut p);
    Ok(p)
}

/// Saves a project document atomically (write to a temporary file, then rename).
pub fn save(p: &Project, path: impl AsRef<Path>) -> FileResult<()> {
    let path = path.as_ref();
    let text = to_json(p)?;
    let tmp = path.with_extension(format!("{FILE_EXTENSION}.tmp"));
    std::fs::write(&tmp, text).map_err(|source| FileError::Io { path: tmp.clone(), source })?;
    std::fs::rename(&tmp, path).map_err(|source| FileError::Io { path: path.into(), source })?;
    Ok(())
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
    for c in &tc[common..] {
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
        percent_complete: t.sched.percent_complete.round() as u8,
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
    let ext = p.external_projects.iter_mut().find(|e| e.id == project).expect("checked");
    if let Some(t) = other.task(task) {
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
        let (id, stored) = (p.external_projects[i].id, p.external_projects[i].path.clone());
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
                let ext = &mut p.external_projects[i];
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
            Err(e) => warnings.push(format!("Could not refresh '{}': {e}", p.external_projects[i].name)),
        }
    }
    warnings
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_paths() {
        assert_eq!(relative_path(Path::new("/a/b/c.plancraft"), Path::new("/a/b")), PathBuf::from("c.plancraft"));
        assert_eq!(relative_path(Path::new("/a/x/c.plancraft"), Path::new("/a/b")), PathBuf::from("../x/c.plancraft"));
        assert_eq!(resolve_external_path("../x/c.plancraft", Some(Path::new("/a/b/doc.plancraft"))), PathBuf::from("/a/x/c.plancraft"));
    }
}
