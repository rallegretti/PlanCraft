//! Modal-style windows.

pub mod custom_fields;
pub mod mcp_panel;
pub mod misc;
pub mod print;
pub mod project_info;
pub mod resource_info;
pub mod task_info;
pub mod update_project;
pub mod working_time;

use plancraft_core::workspace::{Document, Workspace};

/// A dialog bound to the document it was opened for (by document handle). Task and resource ids
/// repeat across documents, so a dialog that applied to "the active document" would write one
/// plan's edits into another after a tab switch.
pub struct Bound<T> {
    pub doc: u64,
    pub dialog: T,
}

impl<T> Bound<T> {
    pub fn new(doc: &Document, dialog: T) -> Self {
        Self { doc: doc.handle, dialog }
    }
}

/// Shows a bound dialog against its own document; `show` returns whether it stays open. The
/// dialog closes when its document has been closed.
pub fn show_bound<T>(slot: &mut Option<Bound<T>>, ws: &mut Workspace, show: impl FnOnce(&mut T, &mut Document) -> bool) {
    let Some(b) = slot else { return };
    let keep = match ws.index_of(b.doc).and_then(|i| ws.docs.get_mut(i)) {
        Some(doc) => show(&mut b.dialog, doc),
        None => false,
    };
    if !keep {
        *slot = None;
    }
}

#[derive(Default)]
pub struct Dialogs {
    pub task_info: Option<Bound<task_info::TaskInfo>>,
    pub resource_info: Option<Bound<resource_info::ResourceInfo>>,
    pub project_info: Option<Bound<project_info::ProjectInfo>>,
    pub working_time: Option<Bound<working_time::WorkingTime>>,
    pub custom_fields: Option<Bound<custom_fields::CustomFields>>,
    pub print: Option<Bound<print::PrintDialog>>,
    pub update_project: Option<Bound<update_project::UpdateProject>>,
    /// Follows the active document's selection; reset when the active document changes.
    pub assign: Option<Bound<misc::AssignResources>>,
    pub external: Option<Bound<misc::ExternalLinks>>,
    pub stats: bool,
    pub about: bool,
    pub history: bool,
    pub mcp: bool,
}
