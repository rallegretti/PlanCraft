//! # PlanCraft MCP server
//!
//! Exposes PlanCraft project documents to AI agents (Claude Code and other
//! Model Context Protocol clients): documents, tasks, milestones, dependencies
//! (including cross-document links), resources, assignments, calendars, time
//! tracking, workload, leveling, baselines, statistics and undo history.
//!
//! The server is transport agnostic ([`McpServer::handle_message`]) and ships
//! two transports:
//!
//! * [`run_stdio`] — newline-delimited JSON over stdin/stdout, for
//!   `claude mcp add plancraft -- plancraft-mcp project.plancraft`;
//! * [`serve_http`] — MCP Streamable HTTP (JSON responses) on a background
//!   thread, used by the desktop app for a live connection.
//!
//! ```no_run
//! use std::sync::Arc;
//! use plancraft_mcp::{McpServer, serve_http, workspace_from_files};
//!
//! let ws = workspace_from_files(&["plan.plancraft"]).unwrap();
//! let server = Arc::new(McpServer::new(ws).with_change_callback(Arc::new(|| println!("changed"))));
//! let handle = serve_http(server, "127.0.0.1:7878").unwrap();
//! println!("MCP endpoint: {}", handle.url());
//! ```
//!
//! All mutations go through `Document::try_edit` with `ChangeSource::Agent`,
//! so every agent change is undoable in the app and triggers recalculation.

#![recursion_limit = "512"]

pub mod args;
pub mod cli;
pub mod http;
pub mod prompts;
pub mod protocol;
pub mod resources;
pub mod server;
pub mod stdio;
pub mod tools;
pub mod views;

use std::path::{Path, PathBuf};

use plancraft_core::model::{FILE_EXTENSION, Project};
use plancraft_core::workspace::{SharedWorkspace, Workspace};

pub use http::{HttpHandle, serve_http};
pub use server::{ChangeCallback, McpServer, ServerOptions};
pub use stdio::{run_stdio, run_stdio_with};

/// Default address for the live HTTP connection of the desktop app.
pub const DEFAULT_HTTP_ADDR: &str = "127.0.0.1:7878";

/// Usage guide returned to clients in the `initialize` result.
pub const INSTRUCTIONS: &str = "\
PlanCraft MCP server: create, inspect and edit Microsoft-Project-style schedules (.plancraft documents).

Documents: every tool takes an optional `document` (handle number from list_documents, file path, file name or project name); \
by default the active document is used. Start with list_documents / get_project / get_schedule. If nothing is open, call new_project or open_project. \
Changes are kept in memory: call save_project to write them to disk.

References: tasks are referenced by their unique `id` (a number, stable for the task's lifetime; preferred), by exact name, or by \"#N\" for row N. \
`row` is the current 1-based position in the outline and changes when tasks move. Resources and calendars accept id or name.

Formats: dates \"YYYY-MM-DD\" or \"YYYY-MM-DD HH:MM\" (a bare date means start of the working day, or end of day for finish/deadline fields). \
Durations: \"3d\", \"4h\", \"2w\", \"1mo\", \"1.5 days\", \"3d?\" (estimated), or a number of working days. Work: \"16h\", \"2d\", or a number of hours. \
Lags: \"2d\", \"-1d\" (lead), \"50%\", \"3ed\" (elapsed days). Units: \"50%\" or 0.5. Outputs give both human strings and *_minutes values.

Efficient workflow: build a whole WBS in one create_tasks call (nested `children`, per-item `key`, `predecessors` referencing keys, \
`resources` text like \"Ana[50%], Bob\", milestones with `milestone: true` and `members`). Use update_tasks for bulk edits. \
Durations of summary tasks are computed from their subtasks. Setting `start` on an auto-scheduled task adds a Start-No-Earlier-Than constraint; \
prefer dependencies over fixed dates.

Analysis: get_schedule (Gantt table), critical_path, get_workload / get_overallocations, level_resources, project_statistics, milestone_status, \
set_baseline + get_variance. Cross-project links: add_external_project, list_external_tasks, link_external_task, refresh_external_links.

Every change is a single undoable step attributed to the agent (undo / redo / get_history), and the schedule is recalculated automatically. \
Tool errors are returned as results with isError=true and an explanation; fix the arguments and retry.";

/// Builds a workspace with the given documents open (the last one active).
///
/// Existing files are opened. A path that does not exist yet becomes a new,
/// empty, unsaved project bound to that path (nothing is written to disk
/// until `save_project` is called). With no paths the workspace starts empty.
pub fn workspace_from_files<P: AsRef<Path>>(paths: &[P]) -> Result<SharedWorkspace, String> {
    let mut ws = Workspace::default();
    for p in paths {
        let p = p.as_ref();
        if p.exists() {
            ws.open(p).map_err(|e| e.to_string())?;
        } else {
            let path: PathBuf = if p.extension().is_none() {
                p.with_extension(FILE_EXTENSION)
            } else {
                p.to_path_buf()
            };
            if path.exists() {
                ws.open(&path).map_err(|e| e.to_string())?;
                continue;
            }
            if let Some(dir) = path.parent()
                && !dir.as_os_str().is_empty()
                && !dir.exists()
            {
                return Err(format!(
                    "cannot create {}: directory {} does not exist",
                    path.display(),
                    dir.display()
                ));
            }
            let name = path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "New Project".into());
            let abs = if path.is_absolute() {
                path
            } else {
                std::env::current_dir()
                    .map(|c| c.join(&path))
                    .unwrap_or(path)
            };
            let handle = ws.add(
                Project::new(name, chrono::Local::now().date_naive()),
                Some(abs),
            );
            if let Some(i) = ws.index_of(handle) {
                // A brand-new document has unsaved content.
                ws.docs[i].dirty = true;
            }
        }
    }
    Ok(ws.shared())
}
