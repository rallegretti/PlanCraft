//! Open documents with undo/redo, shared by the desktop app and the MCP server.
//!
//! The desktop app and its embedded MCP server share one [`Workspace`] behind
//! a mutex, so changes made by an AI agent appear live in the UI and can be
//! undone like any user edit.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::io::{self, FileResult};
use crate::model::Project;
use crate::schedule::recalculate;

const UNDO_LIMIT: usize = 200;

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
}

/// An open project document.
#[derive(Clone, Debug)]
pub struct Document {
    /// Session-unique handle.
    pub handle: u64,
    pub path: Option<PathBuf>,
    pub project: Project,
    pub dirty: bool,
    /// Incremented on every change (edits, undo, redo, reload).
    pub revision: u64,
    pub last_change: Option<(String, ChangeSource)>,
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
            revision: 1,
            last_change: None,
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

    fn push_undo(&mut self, label: &str, source: ChangeSource) {
        self.undo.push(Snapshot {
            label: label.into(),
            source,
            project: self.project.clone(),
        });
        if self.undo.len() > UNDO_LIMIT {
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    fn touched(&mut self, label: &str, source: ChangeSource) {
        self.dirty = true;
        self.revision += 1;
        self.last_change = Some((label.into(), source));
    }

    /// Applies an edit with undo support and recalculates the schedule.
    pub fn edit<R>(&mut self, label: &str, f: impl FnOnce(&mut Project) -> R) -> R {
        self.edit_as(label, ChangeSource::User, f)
    }

    pub fn edit_as<R>(
        &mut self,
        label: &str,
        source: ChangeSource,
        f: impl FnOnce(&mut Project) -> R,
    ) -> R {
        self.push_undo(label, source);
        let r = f(&mut self.project);
        recalculate(&mut self.project);
        self.touched(label, source);
        r
    }

    /// Applies a fallible edit; on error the project is restored and no undo step is recorded.
    pub fn try_edit<R, E>(
        &mut self,
        label: &str,
        source: ChangeSource,
        f: impl FnOnce(&mut Project) -> Result<R, E>,
    ) -> Result<R, E> {
        let before = self.project.clone();
        match f(&mut self.project) {
            Ok(r) => {
                self.undo.push(Snapshot {
                    label: label.into(),
                    source,
                    project: before,
                });
                if self.undo.len() > UNDO_LIMIT {
                    self.undo.remove(0);
                }
                self.redo.clear();
                recalculate(&mut self.project);
                self.touched(label, source);
                Ok(r)
            }
            Err(e) => {
                self.project = before;
                Err(e)
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
        self.undo
            .iter()
            .rev()
            .map(|s| (s.label.clone(), s.source))
            .collect()
    }

    pub fn undo(&mut self) -> bool {
        let Some(s) = self.undo.pop() else {
            return false;
        };
        let current = std::mem::replace(&mut self.project, s.project);
        self.redo.push(Snapshot {
            label: s.label.clone(),
            source: s.source,
            project: current,
        });
        self.touched(&format!("Undo {}", s.label), ChangeSource::User);
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(s) = self.redo.pop() else {
            return false;
        };
        let current = std::mem::replace(&mut self.project, s.project);
        self.undo.push(Snapshot {
            label: s.label.clone(),
            source: s.source,
            project: current,
        });
        self.touched(&format!("Redo {}", s.label), ChangeSource::User);
        true
    }

    /// Saves to the current path.
    pub fn save(&mut self) -> FileResult<()> {
        let Some(path) = self.path.clone() else {
            return Err(io::FileError::Io {
                path: PathBuf::new(),
                source: std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "document has no file path; use save as",
                ),
            });
        };
        io::save(&self.project, &path)?;
        self.dirty = false;
        Ok(())
    }

    /// Saves to a new path, rewriting relative external links so they still resolve.
    pub fn save_as(&mut self, path: impl AsRef<Path>) -> FileResult<()> {
        let path = path.as_ref().to_path_buf();
        let old = self.path.clone();
        let new_dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
        let new_dir = if new_dir.is_absolute() {
            new_dir
        } else {
            std::env::current_dir()
                .map(|c| c.join(&new_dir))
                .unwrap_or(new_dir)
        };
        for e in &mut self.project.external_projects {
            let abs = io::resolve_external_path(&e.path, old.as_deref());
            let abs = if abs.is_absolute() {
                abs
            } else {
                std::env::current_dir().map(|c| c.join(&abs)).unwrap_or(abs)
            };
            e.path = io::relative_path(&abs, &new_dir)
                .to_string_lossy()
                .replace('\\', "/");
        }
        io::save(&self.project, &path)?;
        self.path = Some(path);
        self.dirty = false;
        Ok(())
    }

    /// Re-reads external documents and recalculates. Returns warnings.
    pub fn refresh_external(&mut self) -> Vec<String> {
        if self.project.external_projects.is_empty() {
            return Vec::new();
        }
        let before = self.project.clone();
        let warnings = io::refresh_external(&mut self.project, self.path.as_deref());
        recalculate(&mut self.project);
        if self.project.external_projects != before.external_projects {
            self.revision += 1;
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
        self.next_handle += 1;
        recalculate(&mut project);
        let mut doc = Document::new(self.next_handle, project, path);
        let _ = doc.refresh_external();
        self.docs.push(doc);
        self.active = Some(self.docs.len() - 1);
        self.next_handle
    }

    /// Opens a document from disk (or activates it if already open).
    pub fn open(&mut self, path: impl AsRef<Path>) -> FileResult<u64> {
        let path = path.as_ref();
        let abs = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        if let Some(i) = self.docs.iter().position(|d| {
            d.path
                .as_ref()
                .is_some_and(|p| std::fs::canonicalize(p).unwrap_or_else(|_| p.clone()) == abs)
        }) {
            self.active = Some(i);
            return Ok(self.docs[i].handle);
        }
        let project = io::load(&abs)?;
        Ok(self.add(project, Some(abs)))
    }

    pub fn close(&mut self, handle: u64) -> bool {
        let Some(i) = self.index_of(handle) else {
            return false;
        };
        self.docs.remove(i);
        self.active = match self.active {
            _ if self.docs.is_empty() => None,
            Some(a) if a > i => Some(a - 1),
            Some(a) if a == i => Some(i.min(self.docs.len() - 1)),
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
        let h = ws.add(
            Project::new("P", NaiveDate::from_ymd_opt(2026, 10, 19).unwrap()),
            None,
        );
        let i = ws.index_of(h).unwrap();
        let doc = &mut ws.docs[i];
        doc.edit("Add task", |p| p.add_task("A", None));
        assert_eq!(doc.project.tasks.len(), 1);
        let r: Result<(), &str> = doc.try_edit("Bad", ChangeSource::Agent, |p| {
            p.add_task("B", None);
            Err("nope")
        });
        assert!(r.is_err());
        assert_eq!(doc.project.tasks.len(), 1);
        assert!(doc.undo());
        assert!(doc.project.tasks.is_empty());
        assert!(doc.redo());
        assert_eq!(doc.project.tasks.len(), 1);
        assert_eq!(doc.title(), "P*");
    }
}
