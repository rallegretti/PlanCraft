//! The PlanCraft desktop application: layout, command execution and document management.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration as StdDuration, Instant};

use egui::{Align, Align2, Color32, FontId, Key, KeyboardShortcut, Layout, Modifiers, Rect, RichText, Sense, Stroke, Ui, pos2, vec2};
use plancraft_core::dates::prelude::*;
use plancraft_core::leveling::{LevelingOptions, clear_leveling, level_resources};
use plancraft_core::prelude::*;
use plancraft_core::workspace::{ChangeSource, Document, SharedWorkspace, Workspace};

use crate::commands::Command;
use crate::dialogs::{self, Bound, Dialogs};
use crate::edit::EditExt;
use crate::mcp_host::McpHost;
use crate::menubar::{self, MenuInfo};
use crate::recovery::{self, Recovery, RecoveryStore, SharedRecovery};
use crate::settings::AppSettings;
use crate::start_page;
use crate::state::{ToastKind, UiState, ViewKind, ViewOrder};
use crate::theme;
use crate::views;

pub struct PlanCraftApp {
    ws: SharedWorkspace,
    st: UiState,
    dialogs: Dialogs,
    settings: AppSettings,
    mcp: McpHost,
    fonts: crate::fonts::FontLoader,
    seen: HashMap<u64, u64>,
    status: Option<(String, Instant)>,
    confirm_close: Option<CloseRequest>,
    allow_exit: bool,
    last_title: String,
    /// Developer screenshot automation (`PLANCRAFT_SCREENSHOT=out.png`).
    shot: Option<(PathBuf, u32)>,
    /// Handle of the active document last frame (agents may open or switch documents).
    last_active: Option<u64>,
    /// Recovery copies of unsaved work.
    recovery: SharedRecovery,
    /// The last recovery-copy error shown (shown once until it changes).
    recovery_error: Option<String>,
    /// Message of the last internal error caught by the frame guard, until dismissed.
    crash: Option<String>,
    /// Developer/test hook: panic inside the next frame (`PLANCRAFT_CMD=Panic`).
    inject_panic: bool,
    /// Files the system asked to open (macOS Finder), opened at the start of each frame.
    inbox: Arc<crate::open_requests::Inbox>,
    /// The window's context, for work done outside a frame's `Ui` (loading fonts for a file).
    egui_ctx: egui::Context,
    /// A Save stopped because another program changed the file: asks to overwrite (by handle).
    confirm_overwrite: Option<u64>,
    /// The close (of a document or the app) to carry on with once the overwrite question is
    /// answered and the save went through.
    close_after_save: Option<CloseRequest>,
    /// Refresh Links was chosen for this document: the files are read once the workspace is
    /// unlocked (see [`PlanCraftApp::refresh_links`]).
    refresh_links: Option<u64>,
    /// What the last Copy or Cut noted about its plan besides the tasks (`UiState::clipboard`).
    clip_source: Option<ClipSource>,
    /// Changes to the open plans' files on disk, found by a background thread.
    disk: crate::disk_watch::DiskWatch,
    /// Plans whose file changed on disk in a way that needs the user (unsaved changes, a file
    /// that can't be read, a file that is gone): the bar shown above the plan, by handle.
    disk_bars: HashMap<u64, DiskBar>,
    /// A button of the disk bar was clicked: acted on once the workspace is unlocked.
    disk_action: Option<(u64, DiskAction)>,
    /// "Open in another PlanCraft" bars dismissed: the plan's handle and the other instance.
    lock_dismissed: HashSet<(u64, String)>,
    /// Plans opened from a server, by handle.
    remotes: crate::remote::Remotes,
    /// The Open from Server window, while open.
    server_dialog: Option<dialogs::server::OpenFromServer>,
    /// "Open your version" was clicked in a shared plan's bar (by handle), acted on unlocked.
    open_dropped: Option<u64>,
    /// Plans of the person's own work the server could not take, opened when the shared plan
    /// they came from was closed (named in the close prompt).
    kept_dropped: Vec<String>,
}

/// Why a plan's file needs the user's attention (see [`PlanCraftApp::disk_bar`]).
struct DiskBar {
    /// The plan's file when this was noticed (saved elsewhere since: the bar no longer applies).
    path: Option<PathBuf>,
    kind: DiskBarKind,
    /// What Compare found, while it is shown.
    compare: Option<Vec<String>>,
}

enum DiskBarKind {
    /// Another program changed the file while the plan had unsaved changes: `theirs` is the file.
    Changed { theirs: Box<Project>, notes: Vec<String>, stamp: Option<plancraft_core::io::DiskStamp> },
    /// The file changed but is not a plan PlanCraft can open (garbage, too large, unreadable).
    Unreadable { error: String, stamp: Option<plancraft_core::io::DiskStamp> },
    /// The file was deleted or moved; the plan was marked unsaved so it is not lost. `was_clean`
    /// and `revision` tell whether nothing else changed it since (it is clean again if the file
    /// comes back).
    Missing { revision: u64, was_clean: bool },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DiskAction {
    /// Load the file, discarding unsaved changes (undoable).
    Reload,
    /// Keep the open version; the next save asks before replacing the file.
    KeepMine,
    Compare,
    SaveAs,
    /// Write the plan to its file again (after the file was deleted).
    Save,
}

/// Locks the shared workspace. A panic caught while it was held must not lock the user out of
/// their plans, so a poisoned lock is simply taken over.
pub fn lock(ws: &SharedWorkspace) -> MutexGuard<'_, Workspace> {
    ws.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Whether a summary task above task `i` is one of `sel`.
fn has_selected_ancestor(p: &Project, i: usize, sel: &[TaskId]) -> bool {
    let mut a = p.parent_index(i);
    while let Some(ai) = a {
        if p.tasks.get(ai).is_some_and(|t| sel.contains(&t.id)) {
            return true;
        }
        a = p.parent_index(ai);
    }
    false
}

/// What a Copy or Cut takes from its plan besides the tasks, so a paste can match calendars,
/// custom fields and resources by name in whichever plan it lands in (ids differ between plans).
#[derive(Clone, Default)]
struct ClipSource {
    /// The plan the tasks were copied from: pasted back into it, calendars, fields and resources
    /// keep their ids (also when renamed since the copy).
    project: uuid::Uuid,
    /// The copied tasks' ids: this belongs to the clipboard only while it holds those tasks.
    tasks: Vec<TaskId>,
    calendars: HashMap<CalendarId, String>,
    fields: HashMap<CustomFieldId, (String, CustomFieldKind)>,
    /// The copied tasks' assignments, each with its resource's name and type.
    assignments: Vec<(Assignment, String, ResourceType)>,
}

impl ClipSource {
    fn capture(p: &Project, copied: &[Task]) -> Self {
        let ids: HashSet<TaskId> = copied.iter().map(|t| t.id).collect();
        let used_calendars: HashSet<CalendarId> =
            copied.iter().flat_map(|t| [t.calendar, t.recurrence.as_ref().and_then(|r| r.calendar)]).flatten().collect();
        let used_fields: HashSet<CustomFieldId> = copied.iter().flat_map(|t| t.custom.keys().copied()).collect();
        Self {
            project: p.id,
            tasks: copied.iter().map(|t| t.id).collect(),
            calendars: p.calendars.iter().filter(|c| used_calendars.contains(&c.id)).map(|c| (c.id, c.name.clone())).collect(),
            fields: p.custom_fields.iter().filter(|f| used_fields.contains(&f.id)).map(|f| (f.id, (f.name.clone(), f.kind))).collect(),
            assignments: p
                .assignments
                .iter()
                .filter(|a| ids.contains(&a.task))
                .filter_map(|a| p.resource(a.resource).map(|r| (a.clone(), r.name.clone(), r.kind)))
                .collect(),
        }
    }

    /// Whether `clip` is still the copy this was taken with.
    fn holds(&self, clip: &[Task]) -> bool {
        self.tasks.len() == clip.len() && self.tasks.iter().zip(clip).all(|(id, t)| *id == t.id)
    }
}

/// The item of a plan matching one from another plan (or the same plan): the same id under the
/// same name first, otherwise the first of that name (ignoring case).
fn match_by_name<I: Copy + PartialEq>(items: impl Iterator<Item = (I, String)> + Clone, id: I, name: &str, same_plan: bool) -> Option<I> {
    if same_plan && items.clone().any(|(i, _)| i == id) {
        return Some(id);
    }
    let key = name.to_lowercase();
    let same = |n: &String| n.to_lowercase() == key;
    items.clone().find(|(i, n)| *i == id && same(n)).or_else(|| items.into_iter().find(|(_, n)| same(n))).map(|(i, _)| i)
}

/// Names for a paste note: 'A', 'B'.
fn quoted(names: std::collections::BTreeSet<String>) -> String {
    names.into_iter().map(|n| format!("'{n}'")).collect::<Vec<_>>().join(", ")
}

/// Pastes copied tasks before row `at` with fresh ids. Links and milestone members among the
/// pasted tasks follow them to their new ids (others are left out); calendars, custom field
/// values and assignments are matched by name in this plan (`source` says what they were called
/// where they were copied), and what has no match is left out. Returns the new ids and notes on
/// what was left out.
fn paste_tasks(p: &mut Project, clip: &[Task], source: Option<&ClipSource>, at: usize) -> (Vec<TaskId>, Vec<String>) {
    use std::collections::BTreeSet;
    let base = clip.iter().map(|t| t.outline_level).min().unwrap_or(1);
    let target_level = p.tasks.get(at).map_or(1, |t| t.outline_level);
    let at = at.min(p.tasks.len());
    let recurring_in_clip = clip.iter().any(|t| t.recurrence.is_some());
    let same_plan = source.is_some_and(|s| s.project == p.id);
    let ids: Vec<TaskId> = clip.iter().map(|_| p.ids.next_task()).collect();
    let mut new_id: HashMap<TaskId, TaskId> = HashMap::new();
    for (t, id) in clip.iter().zip(&ids) {
        new_id.entry(t.id).or_insert(*id);
    }
    let (mut lost_calendars, mut lost_fields, mut lost_resources) = (BTreeSet::new(), BTreeSet::new(), BTreeSet::new());
    let mut lost_external = false;
    // Links and milestone members are added after the tasks are in place, through the same
    // operations as by hand, so the pasted outline can't hold a link those would refuse (a loop,
    // a link between a summary and its own subtask, a finish link into a summary).
    let mut links: Vec<(TaskId, Vec<Dependency>, Vec<TaskRef>)> = Vec::new();

    for (k, (t, id)) in clip.iter().zip(&ids).enumerate() {
        let mut nt = t.clone();
        nt.id = *id;
        let mut remap = |r: TaskRef| match r {
            TaskRef::Local(old) => new_id.get(&old).map(|n| TaskRef::Local(*n)),
            TaskRef::External { project, .. } if p.external(project).is_some() => Some(r),
            TaskRef::External { .. } => {
                lost_external = true;
                None
            }
        };
        let deps: Vec<Dependency> =
            t.predecessors.iter().filter_map(|d| remap(d.predecessor).map(|predecessor| Dependency { predecessor, ..*d })).collect();
        let members: Vec<TaskRef> = t.members.iter().filter_map(|m| remap(*m)).collect();
        links.push((*id, deps, members));
        nt.predecessors.clear();
        nt.members.clear();
        nt.baseline = None;
        nt.baselines.clear();
        nt.interim.clear();
        // A pasted occurrence is an ordinary task unless its recurring task comes too.
        nt.occurrence &= recurring_in_clip;
        // Board columns belong to their plan: a column this plan lacks means none.
        if nt.board_status.is_some_and(|s| p.board_column(s).is_none()) {
            nt.board_status = None;
        }
        // Calendars by name; none (the project calendar) when this plan has no such calendar.
        let mut calendar = |c: CalendarId| match source.and_then(|s| s.calendars.get(&c)) {
            Some(name) => {
                let found = match_by_name(p.calendars.iter().map(|k| (k.id, k.name.clone())), c, name, same_plan);
                if found.is_none() {
                    lost_calendars.insert(name.clone());
                }
                found
            }
            // Nothing known about where it came from: keep a calendar this plan has.
            None => p.calendar(c).map(|k| k.id),
        };
        nt.calendar = t.calendar.and_then(&mut calendar);
        if let Some(r) = &mut nt.recurrence {
            r.calendar = r.calendar.and_then(&mut calendar);
        }
        // Custom field values by field name and kind.
        nt.custom = t
            .custom
            .iter()
            .filter_map(|(f, v)| {
                let target = match source.and_then(|s| s.fields.get(f)) {
                    Some((name, kind)) => {
                        let same_kind = p.custom_fields.iter().filter(|d| d.kind == *kind).map(|d| (d.id, d.name.clone()));
                        let found = match_by_name(same_kind, *f, name, same_plan);
                        if found.is_none() {
                            lost_fields.insert(name.clone());
                        }
                        found
                    }
                    None => Some(*f),
                };
                // Only into a field of this plan that holds this kind of value.
                target.filter(|f| p.custom_fields.iter().any(|d| d.id == *f && v.fits(d.kind))).map(|f| (f, v.clone()))
            })
            .collect();
        nt.outline_level = target_level.saturating_add(t.outline_level.saturating_sub(base));
        p.tasks.insert(at.saturating_add(k).min(p.tasks.len()), nt);
    }
    p.normalize_outline();
    let mut lost_links = false;
    for (id, deps, members) in links {
        for d in deps {
            lost_links |= p.link(d.predecessor, id, d.kind, d.lag).is_err();
        }
        for m in members {
            lost_links |= p.add_milestone_members(id, &[m]).is_err();
        }
    }

    // Assignments, to the resource of the same name and type in this plan.
    for (a, name, kind) in source.map(|s| s.assignments.as_slice()).unwrap_or_default() {
        let Some(task) = new_id.get(&a.task).copied() else { continue };
        let same_kind = p.resources.iter().filter(|r| r.kind == *kind).map(|r| (r.id, r.name.clone()));
        let Some(resource) = match_by_name(same_kind, a.resource, name, same_plan) else {
            lost_resources.insert(name.clone());
            continue;
        };
        // Two copied resources can match one here ("Ana" and "ana"): one assignment per pair.
        if p.assignments.iter().any(|x| x.task == task && x.resource == resource) {
            continue;
        }
        let id = p.ids.next_assignment();
        p.assignments.push(Assignment { id, task, resource, ..a.clone() });
    }

    let mut notes = Vec::new();
    if !lost_calendars.is_empty() {
        notes.push(format!("This plan has no calendar {}, so those tasks use the project calendar.", quoted(lost_calendars)));
    }
    if !lost_fields.is_empty() {
        notes.push(format!("This plan has no custom field {}, so those values were left out.", quoted(lost_fields)));
    }
    if !lost_resources.is_empty() {
        notes.push(format!("This plan has no resource {}, so those assignments were left out.", quoted(lost_resources)));
    }
    if lost_links {
        notes.push("Some links and milestone members can't exist in the pasted outline (a loop, or a link between a summary and its own subtasks), so they were left out.".to_string());
    }
    if lost_external {
        notes.push("Links to projects this plan is not linked with were left out.".to_string());
    }
    (ids, notes)
}

/// Whether two paths name the same file (also through a different spelling or a link).
fn same_file(a: &std::path::Path, b: &std::path::Path) -> bool {
    a == b || matches!((std::fs::canonicalize(a), std::fs::canonicalize(b)), (Ok(x), Ok(y)) if x == y)
}

/// The plan's file name for messages (its project name when it has no file).
fn file_name_of(doc: &Document) -> String {
    doc.path.as_ref().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| doc.project.name.clone())
}

/// Longest panic message shown to the user.
const MAX_CRASH_CHARS: usize = 400;

#[derive(Clone, Copy, PartialEq)]
enum CloseRequest {
    Document(u64),
    App,
}

/// A plan's name made safe as a suggested file name.
fn file_stem(name: &str) -> String {
    name.chars().map(|c| if matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') { '-' } else { c }).collect()
}

/// The path to export to, from the file dialog's `path`: given the extension `ext` when typed
/// without one (never replacing an existing file unasked: the dialog only asked about the name as
/// typed), and refused when it ends in another extension (an export must never replace a plan or
/// any other kind of file). `Err` is the sentence to show.
fn export_path(path: PathBuf, ext: &str, what: &str) -> Result<PathBuf, String> {
    match path.extension() {
        None => {
            let with_ext = path.with_extension(ext);
            if with_ext.exists() {
                return Err(format!("{} already exists. Choose it in the dialog to replace it, or pick another name.", with_ext.display()));
            }
            Ok(with_ext)
        }
        Some(e) if e.eq_ignore_ascii_case(ext) => Ok(path),
        Some(_) => Err(format!("Choose a file name ending in .{ext} to export {what}.")),
    }
}

/// A shared plan's part of the status bar: a coloured dot, the server and how the connection
/// stands, and who else is in the plan.
fn shared_status(ui: &mut Ui, r: &crate::remote::Remote) {
    use plancraft_server::link::State;
    let pal = theme::pal();
    let color = match &r.status.state {
        State::Connected if r.unconfirmed() => pal.WARN,
        State::Connected => pal.OK,
        State::Connecting | State::Offline(_) => pal.TEXT_WEAK,
        State::SignedOut(_) => pal.ERROR,
    };
    let (dot, _) = ui.allocate_exact_size(vec2(8.0, 12.0), Sense::hover());
    ui.painter().circle_filled(dot.center(), 4.0, color);
    let mut details = vec![format!("Signed in to {} as {}", r.server, r.user)];
    if !r.status.role.can_edit() {
        details.push("You may only view this plan.".into());
    }
    match &r.status.state {
        State::Offline(why) => details.push(format!("Offline ({why}): reconnecting by itself; your changes wait and are sent then.")),
        State::SignedOut(why) => details.push(format!("Signed out: {why}")),
        State::Connected if r.unconfirmed() => details.push("Sending your changes…".into()),
        _ => details.push("Every change is saved on the server.".into()),
    }
    let viewer = if r.status.role.can_edit() { "" } else { " · view only" };
    ui.label(RichText::new(format!("{} · {}{viewer}", r.server, r.state_label())).size(12.0)).on_hover_text(details.join("\n"));
    let others: Vec<&plancraft_server::protocol::Person> = r.others().collect();
    if !others.is_empty() {
        let names: Vec<&str> = others.iter().map(|p| p.user.as_str()).collect();
        let first: Vec<&str> = names.iter().take(3).copied().collect();
        let more = names.len().saturating_sub(3);
        let shown = if more > 0 { format!("{} and {more} more", first.join(", ")) } else { first.join(", ") };
        ui.label(RichText::new(format!("👥 {shown}")).size(12.0).color(pal.ACCENT_MID)).on_hover_text("Also in this plan");
    }
}

/// The file extensions offered when picking a spreadsheet to import.
fn spreadsheet_extensions() -> Vec<&'static str> {
    let text = plancraft_core::tabular::TEXT_EXTENSIONS.into_iter().filter(|e| *e != "txt");
    text.chain(plancraft_core::tabular::WORKBOOK_EXTENSIONS).collect()
}

/// Shown when an earlier session left unsaved work but the Start Page, which lists it, is not shown.
const RECOVERY_FOUND: &str = "PlanCraft found unsaved work from an earlier session. Open the Start Page (File menu) to restore it.";

impl PlanCraftApp {
    pub fn new(cc: &eframe::CreationContext<'_>, ws: SharedWorkspace) -> Self {
        let mut settings = AppSettings::load();
        if let Some(t) = std::env::var("PLANCRAFT_THEME").ok().and_then(|v| theme::ThemeChoice::parse(&v)) {
            settings.theme = t;
        }
        theme::install(&cc.egui_ctx, settings.theme);
        // Find fonts for other scripts (Chinese, Hebrew, ...) while the window opens.
        plancraft_core::print::fallback::warm_up();
        let has_doc = !lock(&ws).docs.is_empty();
        let st = UiState { table_width: settings.table_width, start_page: !has_doc, ..UiState::default() };
        let mut app = Self {
            ws,
            st,
            dialogs: Dialogs::default(),
            settings,
            mcp: McpHost::default(),
            fonts: Default::default(),
            seen: HashMap::new(),
            status: None,
            confirm_close: None,
            confirm_overwrite: None,
            close_after_save: None,
            refresh_links: None,
            clip_source: None,
            disk: Default::default(),
            disk_bars: HashMap::new(),
            disk_action: None,
            lock_dismissed: HashSet::new(),
            remotes: Default::default(),
            server_dialog: None,
            open_dropped: None,
            kept_dropped: Vec::new(),
            allow_exit: false,
            last_title: String::new(),
            shot: std::env::var_os("PLANCRAFT_SCREENSHOT").map(|p| (PathBuf::from(p), 0)),
            last_active: None,
            // Tests never touch the user's recovery folder; they inject their own store.
            recovery: Arc::new(Mutex::new(Recovery::new(if cfg!(test) { None } else { RecoveryStore::user() }))),
            recovery_error: None,
            crash: None,
            inject_panic: false,
            inbox: crate::open_requests::global(),
            egui_ctx: cc.egui_ctx.clone(),
        };
        for path in &app.settings.recent {
            app.fonts.ensure(&cc.egui_ctx, &path.to_string_lossy());
        }
        app.inbox.wake_with(&cc.egui_ctx);
        recovery::spawn_background_writer(&app.recovery, &app.ws, cc.egui_ctx.clone());
        // Tests poll the files themselves (`DiskWatch::poll_now`).
        if !cfg!(test) {
            app.disk.spawn(&app.ws, cc.egui_ctx.clone());
            let ctx = cc.egui_ctx.clone();
            plancraft_core::lock::spawn_keeper(Arc::downgrade(&app.ws), move || ctx.request_repaint());
        }
        if !app.rec().found.is_empty() && !app.st.start_page {
            app.st.toast(ToastKind::Info, RECOVERY_FOUND);
        }
        if let Ok(v) = std::env::var("PLANCRAFT_TAB")
            && let Some(tab) = crate::state::MenuTab::ALL.into_iter().find(|t| t.label().eq_ignore_ascii_case(&v))
        {
            app.st.menu_tab = tab;
        }
        if let Ok(v) = std::env::var("PLANCRAFT_VIEW") {
            let key = v.to_lowercase().replace([' ', '_', '-'], "");
            if let Some(view) = ViewKind::ALL.into_iter().find(|k| k.label().to_lowercase().replace(' ', "") == key) {
                app.st.view = view;
            }
        }
        if app.settings.mcp_autostart {
            app.mcp.start(app.ws.clone(), cc.egui_ctx.clone(), app.settings.mcp_port);
        }
        app
    }

    /// The recovery state (shared with the background writer). Lock order: workspace first.
    fn rec(&self) -> MutexGuard<'_, Recovery> {
        recovery::lock(&self.recovery)
    }

    fn set_status(&mut self, s: impl Into<String>) {
        self.status = Some((s.into(), Instant::now()));
    }

    // ------------------------------------------------------------------
    // Commands
    // ------------------------------------------------------------------

    fn shortcuts(&mut self, ctx: &egui::Context, out: &mut Vec<Command>) {
        // Only a focused text field should swallow shortcuts (the task grid holds focus too).
        let typing = ctx.text_edit_focused();
        // The Start Page or a save prompt covers the plan: shortcuts that act on it would change
        // (or save, or print) a document the user cannot see. New and Open still work.
        let covered = self.document_covered();
        let cmd = Modifiers::COMMAND;
        let map: Vec<(KeyboardShortcut, Command, bool)> = vec![
            (KeyboardShortcut::new(cmd, Key::N), Command::NewProject, true),
            (KeyboardShortcut::new(cmd, Key::O), Command::Open, true),
            (KeyboardShortcut::new(cmd | Modifiers::SHIFT, Key::S), Command::SaveAs, true),
            (KeyboardShortcut::new(cmd, Key::S), Command::Save, true),
            (KeyboardShortcut::new(cmd, Key::P), Command::Print, true),
            (KeyboardShortcut::new(cmd | Modifiers::SHIFT, Key::Z), Command::Redo, false),
            (KeyboardShortcut::new(cmd, Key::Z), Command::Undo, false),
            (KeyboardShortcut::new(cmd, Key::Y), Command::Redo, false),
            (KeyboardShortcut::new(Modifiers::ALT | Modifiers::SHIFT, Key::ArrowRight), Command::Indent, false),
            (KeyboardShortcut::new(Modifiers::ALT | Modifiers::SHIFT, Key::ArrowLeft), Command::Outdent, false),
            (KeyboardShortcut::new(Modifiers::ALT | Modifiers::SHIFT, Key::ArrowUp), Command::MoveUp, false),
            (KeyboardShortcut::new(Modifiers::ALT | Modifiers::SHIFT, Key::ArrowDown), Command::MoveDown, false),
            (KeyboardShortcut::new(cmd | Modifiers::SHIFT, Key::F2), Command::UnlinkSelected, false),
            (KeyboardShortcut::new(cmd, Key::F2), Command::LinkSelected, false),
            (KeyboardShortcut::new(Modifiers::SHIFT, Key::F2), Command::TaskInfo, false),
            (KeyboardShortcut::new(cmd, Key::Plus), Command::ZoomIn, false),
            (KeyboardShortcut::new(cmd, Key::Equals), Command::ZoomIn, false),
            (KeyboardShortcut::new(cmd, Key::Minus), Command::ZoomOut, false),
            (KeyboardShortcut::new(cmd, Key::C), Command::Copy, false),
            (KeyboardShortcut::new(cmd, Key::X), Command::Cut, false),
            (KeyboardShortcut::new(cmd, Key::V), Command::Paste, false),
            (KeyboardShortcut::new(cmd, Key::D), Command::FillDown, false),
            // Replace: Cmd+H hides the app on a Mac, so Cmd+Option+F there (and everywhere).
            (KeyboardShortcut::new(cmd | Modifiers::ALT, Key::F), Command::Find(true), false),
            (KeyboardShortcut::new(cmd, Key::F), Command::Find(false), false),
            (KeyboardShortcut::new(cmd, Key::H), Command::Find(true), false),
            (KeyboardShortcut::new(Modifiers::NONE, Key::F5), Command::GoTo, false),
        ];
        for (sc, c, global) in map {
            // New and Open belong on the Start Page; behind a save prompt nothing may act.
            let prompt_open = self.confirm_close.is_some() || self.confirm_overwrite.is_some();
            if covered && (prompt_open || !matches!(c, Command::NewProject | Command::Open)) {
                continue;
            }
            if (global || !typing) && ctx.input_mut(|i| i.consume_shortcut(&sc)) {
                out.push(c);
            }
        }
        // The desktop backend turns Ctrl+C/X/V into clipboard events rather than key presses, so
        // outside a text field they are task clipboard commands. (Paste only arrives when the
        // system clipboard holds text; Copy and Cut put the task names there for that reason.)
        if !typing && !covered {
            ctx.input_mut(|i| {
                i.events.retain(|e| {
                    let c = match e {
                        egui::Event::Copy => Command::Copy,
                        egui::Event::Cut => Command::Cut,
                        egui::Event::Paste(_) => Command::Paste,
                        _ => return true,
                    };
                    out.push(c);
                    false
                });
            });
        }
    }

    /// Whether the plan is hidden behind the Start Page or a save prompt ("Save changes?", "File
    /// changed on disk"), so keyboard shortcuts must leave it alone.
    fn document_covered(&self) -> bool {
        self.st.start_page || self.confirm_close.is_some() || self.confirm_overwrite.is_some()
    }

    fn with_doc<R>(&mut self, f: impl FnOnce(&mut Document, &mut UiState) -> R) -> Option<R> {
        let ws = self.ws.clone();
        let mut ws = lock(&ws);
        let doc = ws.active_doc_mut()?;
        Some(f(doc, &mut self.st))
    }

    /// Opens the files the system asked to open (macOS Finder and the Dock, `open_requests`) and
    /// brings the window forward, out of the Dock if it was minimized.
    fn open_requested_files(&mut self, ctx: &egui::Context) {
        let paths = self.inbox.take();
        if paths.is_empty() {
            return;
        }
        let from_start_page = self.st.start_page;
        for path in paths {
            self.open_path(path);
        }
        // Launched from Finder, the file closes the Start Page before it is ever drawn, and the
        // Start Page is where recovered work is listed: say it exists, as a launch with the file on
        // the command line does.
        if from_start_page && !self.st.start_page && !self.rec().found.is_empty() {
            self.st.toast(ToastKind::Info, RECOVERY_FOUND);
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
    }

    fn open_path(&mut self, path: PathBuf) {
        // A task list from a spreadsheet: ask what its columns hold first.
        if plancraft_core::tabular::is_table_path(&path) {
            match plancraft_core::tabular::read_path(&path) {
                Ok(sheets) => {
                    let dialog = dialogs::import::ImportSheet::new(path, sheets);
                    for text in dialog.texts() {
                        self.fonts.ensure(&self.egui_ctx, text);
                    }
                    self.dialogs.import.push(dialog);
                }
                Err(e) => self.st.toast(ToastKind::Error, e.to_string()),
            }
            return;
        }
        let r = lock(&self.ws).open(&path);
        match r {
            Ok(_) => {
                self.settings.add_recent(path.clone());
                self.st.start_page = false;
                self.st.sel = Default::default();
                self.st.gantt.initialized_for = None;
                let verb = if plancraft_core::mspdi::is_mspdi_path(&path) { "Imported" } else { "Opened" };
                self.set_status(format!("{verb} {}", path.display()));
                // A PlanCraft Server shares this plan (or stopped without saving): its journal may
                // hold changes the file does not have yet.
                let journal = plancraft_server::journal::path_for(&path);
                if journal.and_then(|j| std::fs::symlink_metadata(j).ok()).is_some_and(|m| m.is_file()) {
                    self.st.toast(
                        ToastKind::Warning,
                        "A PlanCraft Server shares this plan, or stopped before saving its changes into the file. Open it from the \
                         server (File › Open from Server…) to see every change; if the server is not running, starting it saves them.",
                    );
                }
            }
            Err(e) => self.st.toast(ToastKind::Error, e.to_string()),
        }
    }

    fn save(&mut self, save_as: bool) -> bool {
        let shared = lock(&self.ws).active_doc().filter(|d| d.shared.is_some()).map(|d| d.handle);
        if let Some(h) = shared {
            if save_as {
                return self.save_copy(h);
            }
            self.set_status("This plan is opened from a server, which saves every change as it is made. Save As keeps a copy of your own.");
            return true;
        }
        let Some((handle, name, ask)) =
            lock(&self.ws).active_doc().map(|d| (d.handle, format!("{}.{}", d.project.name, FILE_EXTENSION), save_as || d.path.is_none()))
        else {
            return false;
        };
        // The file dialog is open without the workspace lock, so agents keep working meanwhile.
        let path = if ask {
            match rfd::FileDialog::new().add_filter("PlanCraft project", &[FILE_EXTENSION]).set_file_name(name).save_file() {
                Some(mut p) => {
                    if p.extension().is_none() {
                        p.set_extension(FILE_EXTENSION);
                        // The file dialog only asked about the name as typed.
                        if p.exists() {
                            let msg =
                                format!("{} already exists. Choose it in the dialog to replace it, or pick another name.", p.display());
                            self.st.toast(ToastKind::Warning, msg);
                            return false;
                        }
                    }
                    Some(p)
                }
                None => return false,
            }
        } else {
            None
        };
        let ws = self.ws.clone();
        let mut ws = lock(&ws);
        // The plan the dialog was for, even if another became active or it was closed meanwhile.
        let Some(doc) = ws.index_of(handle).and_then(|i| ws.docs.get_mut(i)) else {
            drop(ws);
            self.st.toast(ToastKind::Warning, "The plan was closed before it could be saved.");
            return false;
        };
        let r = match path {
            Some(p) => doc.save_as(&p).map(|_| p),
            None => doc.save().map(|_| doc.path.clone().unwrap_or_default()),
        };
        drop(ws);
        if let Err(plancraft_core::io::FileError::ChangedOnDisk { .. }) = &r {
            // Never silently overwrite another program's changes: ask first.
            self.confirm_overwrite = Some(handle);
            return false;
        }
        match r {
            Ok(p) => {
                self.settings.add_recent(p.clone());
                self.set_status(format!("Saved {}", p.display()));
                true
            }
            Err(e) => {
                self.st.toast(ToastKind::Error, e.to_string());
                false
            }
        }
    }

    /// Saves the current view as a PDF, laid out with the plan's page setup.
    fn export_pdf(&mut self) {
        let Some(pending) = self.with_doc(|doc, st| {
            let view = crate::printing::PrintView::for_view(st.view);
            let pages = crate::printing::document(&doc.project, st, view, &doc.project.page_setup, None, None);
            dialogs::print::PendingPdf {
                bytes: plancraft_core::print::pdf::write(&pages),
                name: format!("{} - {}.pdf", doc.project.name, view.label()),
            }
        }) else {
            return;
        };
        self.save_pdf(pending);
    }

    /// Saves the plan as a Microsoft Project XML file, saying what the format left out.
    fn export_mspdi(&mut self) {
        let Some((name, xml, notes)) = self.with_doc(|doc, _| {
            let (xml, notes) = plancraft_core::mspdi::to_xml(&doc.project);
            (doc.project.name.clone(), xml, notes)
        }) else {
            return;
        };
        let name = file_stem(&name);
        let ext = plancraft_core::mspdi::EXTENSION;
        let Some(path) =
            rfd::FileDialog::new().add_filter("Microsoft Project XML", &[ext]).set_file_name(format!("{name}.{ext}")).save_file()
        else {
            return;
        };
        let path = match export_path(path, ext, "Microsoft Project XML") {
            Ok(p) => p,
            Err(why) => return self.st.toast(ToastKind::Warning, why),
        };
        match plancraft_core::io::write_atomic(&path, xml.as_bytes(), plancraft_core::io::Privacy::KeepExisting) {
            Ok(()) if notes.is_empty() => self.set_status(format!("Exported {}", path.display())),
            Ok(()) => {
                self.set_status(format!("Exported {}", path.display()));
                self.st.toast(
                    ToastKind::Info,
                    format!("Exported {}. Microsoft Project XML has no place for some things: {}", path.display(), notes.join("; ")),
                );
            }
            Err(e) => self.st.toast(ToastKind::Error, format!("Could not export the plan: {e}")),
        }
    }

    /// Shows the file picker for Links Between Projects ▸ Add Project File… (without the workspace
    /// lock, so agents keep working while it is open) and links the file picked.
    fn pick_external_project(&mut self) {
        let Some(b) = self.dialogs.external.as_mut().filter(|b| b.dialog.pick_requested) else { return };
        b.dialog.pick_requested = false;
        let Some(path) = rfd::FileDialog::new().add_filter("PlanCraft project", &[FILE_EXTENSION]).pick_file() else { return };
        self.add_external_project(&path);
    }

    /// Links a project file to the plan of the Links Between Projects window. The file is read
    /// without the workspace lock, so a slow disk or network share never stalls agents.
    fn add_external_project(&mut self, path: &std::path::Path) {
        let abs = plancraft_core::io::absolute_path(path);
        let read = plancraft_core::io::load(&abs);
        let Some(b) = self.dialogs.external.as_mut() else { return };
        let mut ws = lock(&self.ws);
        match ws.index_of(b.doc).and_then(|i| ws.docs.get_mut(i)) {
            Some(doc) => b.dialog.add_loaded(doc, &abs, read),
            None => {
                drop(ws);
                self.st.toast(ToastKind::Warning, "The plan was closed before the project file could be linked.");
            }
        }
    }

    /// Links Between Projects asked for a linked project's task list or for a link to one of its
    /// tasks: reads the linked file without the workspace lock, then hands it to the window.
    fn external_links_reads(&mut self, ctx: &egui::Context) {
        let Some(b) = self.dialogs.external.as_mut() else { return };
        let (list, link) = (b.dialog.list_requested, b.dialog.link_requested.take());
        if list.is_none() && link.is_none() {
            return;
        }
        let handle = b.doc;
        // Where the files are, looked up under the lock; read after it is released.
        let paths = {
            let ws = lock(&self.ws);
            ws.index_of(handle).and_then(|i| ws.docs.get(i)).map(|d| {
                let path = |project| plancraft_core::io::external_path(&d.project, d.path.as_deref(), project);
                (list.map(|e| (e, path(e))), link.map(|r| (r, path(r.project))))
            })
        };
        // The plan was closed: its window closes on the next frame.
        let Some((list, link)) = paths else { return };
        let list = list.map(|(e, path)| {
            let tasks = match path {
                Some(path) => plancraft_core::io::load_external(&path, e).map(|o| plancraft_core::io::external_tasks_of(&o)),
                None => Ok(Vec::new()),
            };
            (e, tasks)
        });
        let link = link.map(|(r, path)| (r, path.map(|path| plancraft_core::io::load_external(&path, r.project))));
        let Some(b) = self.dialogs.external.as_mut().filter(|b| b.doc == handle) else { return };
        if let Some((e, tasks)) = list {
            b.dialog.tasks_loaded(e, tasks);
        }
        if let Some((req, read)) = link {
            let mut ws = lock(&self.ws);
            match ws.index_of(handle).and_then(|i| ws.docs.get_mut(i)) {
                Some(doc) => b.dialog.link_loaded(doc, req, read, &mut self.st),
                None => {
                    drop(ws);
                    self.st.toast(ToastKind::Warning, "The plan was closed before the task could be linked.");
                }
            }
        }
        ctx.request_repaint();
    }

    /// Refresh Links (the command, or Refresh All in Links Between Projects): reads the linked files
    /// without the workspace lock, so agents keep working while a slow disk or network share
    /// answers, then updates the plan's snapshots of them.
    fn refresh_links(&mut self) {
        let from_dialog = self.dialogs.external.as_mut().filter(|b| b.dialog.refresh_requested).map(|b| {
            b.dialog.refresh_requested = false;
            b.doc
        });
        let Some(handle) = self.refresh_links.take().or(from_dialog) else { return };
        let target = {
            let ws = lock(&self.ws);
            ws.index_of(handle).and_then(|i| ws.docs.get(i)).map(|d| (d.project.external_projects.clone(), d.path.clone()))
        };
        let Some((links, path)) = target else { return };
        let reads = plancraft_core::io::read_external_projects(&links, path.as_deref(), true);
        let warnings = {
            let mut ws = lock(&self.ws);
            match ws.index_of(handle).and_then(|i| ws.docs.get_mut(i)) {
                Some(doc) => doc.apply_external_reads(reads, ChangeSource::User),
                None => vec!["The plan was closed while its links were being read.".to_string()],
            }
        };
        if let Some(b) = self.dialogs.external.as_mut().filter(|b| b.doc == handle) {
            b.dialog.refreshed();
        }
        if warnings.is_empty() {
            self.st.toast(ToastKind::Success, "External links are up to date");
        }
        for m in warnings {
            self.st.toast(ToastKind::Warning, m);
        }
    }

    /// Asks where to save a PDF and writes it (no workspace lock is held while the file dialog is open).
    fn save_pdf(&mut self, pdf: dialogs::print::PendingPdf) {
        let name = file_stem(&pdf.name);
        let Some(path) = rfd::FileDialog::new().add_filter("PDF", &["pdf"]).set_file_name(name).save_file() else { return };
        let path = match export_path(path, "pdf", "a PDF") {
            Ok(p) => p,
            Err(why) => return self.st.toast(ToastKind::Warning, why),
        };
        match plancraft_core::io::write_atomic(&path, &pdf.bytes, plancraft_core::io::Privacy::KeepExisting) {
            Ok(()) => self.set_status(format!("Saved {}", path.display())),
            Err(e) => self.st.toast(ToastKind::Error, format!("Could not save the PDF: {e}")),
        }
    }

    fn export_csv(&mut self, table: plancraft_core::tabular::CsvTable) {
        let Some((name, csv)) =
            self.with_doc(|doc, _| (doc.project.name.clone(), plancraft_core::tabular::table_to_csv(&doc.project, table)))
        else {
            return;
        };
        let file = format!("{}{}.csv", file_stem(&name), table.file_suffix());
        let Some(path) = rfd::FileDialog::new().add_filter("CSV", &["csv"]).set_file_name(file).save_file() else { return };
        let path = match export_path(path, "csv", "CSV") {
            Ok(p) => p,
            Err(why) => return self.st.toast(ToastKind::Warning, why),
        };
        match plancraft_core::io::write_atomic(&path, csv.as_bytes(), plancraft_core::io::Privacy::KeepExisting) {
            Ok(()) => self.set_status(format!("Exported {}", path.display())),
            Err(e) => self.st.toast(ToastKind::Error, format!("Could not save the CSV file: {e}")),
        }
    }

    fn exec(&mut self, ctx: &egui::Context, cmd: Command) {
        match cmd {
            Command::NewProject => {
                let start = plancraft_core::dates::today();
                let mut ws = lock(&self.ws);
                let n = ws.docs.len() + 1;
                ws.add(Project::new(format!("Project{n}"), start), None);
                drop(ws);
                self.st.start_page = false;
                self.st.sel = Default::default();
                self.st.view = ViewKind::Gantt;
            }
            Command::NewDemo => {
                let today = plancraft_core::dates::today();
                let back = i64::from(today.weekday().num_days_from_monday()) + 7;
                let monday = plancraft_core::dates::add_days(today, -back).unwrap_or(today);
                lock(&self.ws).add(plancraft_core::sample::demo_project(monday), None);
                self.st.start_page = false;
                self.st.sel = Default::default();
                self.st.view = ViewKind::Gantt;
            }
            Command::Open => {
                let xml = plancraft_core::mspdi::EXTENSION;
                let sheets = spreadsheet_extensions();
                let all: Vec<&str> = [FILE_EXTENSION, xml].into_iter().chain(sheets.iter().copied()).collect();
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter("Plans and task lists", &all)
                    .add_filter("PlanCraft project", &[FILE_EXTENSION])
                    .add_filter("Microsoft Project XML", &[xml])
                    .add_filter("Excel, CSV and other spreadsheets", &sheets)
                    .pick_file()
                {
                    self.open_path(path);
                }
            }
            Command::ImportSpreadsheet => {
                if let Some(path) =
                    rfd::FileDialog::new().add_filter("Excel, CSV and other spreadsheets", &spreadsheet_extensions()).pick_file()
                {
                    self.open_path(path);
                }
            }
            Command::OpenPath(p) => self.open_path(p),
            Command::Save => {
                self.save(false);
            }
            Command::SaveAs => {
                self.save(true);
            }
            Command::Close => {
                let info = lock(&self.ws).active_doc().map(|d| (d.handle, d.dirty));
                if let Some((h, dirty)) = info {
                    if dirty {
                        self.confirm_close = Some(CloseRequest::Document(h));
                    } else {
                        self.close_doc(h);
                    }
                }
            }
            Command::Quit => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            Command::StartPage => {
                self.st.start_page = true;
            }
            Command::RestoreRecovered(stem) => self.restore_recovered(&stem),
            Command::DiscardRecovered(stem) => {
                self.rec().discard(&stem);
                self.set_status("Recovery copy discarded");
            }
            Command::ExportCsv(table) => self.export_csv(table),
            Command::Print => {
                let ws = self.ws.clone();
                let ws = lock(&ws);
                if let Some(doc) = ws.active_doc() {
                    // Ctrl+P with the window already open for this plan keeps its changes.
                    if self.dialogs.print.as_ref().is_none_or(|b| b.doc != doc.handle) {
                        self.dialogs.print = Some(Bound::new(doc, dialogs::print::PrintDialog::new(doc, &self.st)));
                    }
                    self.st.start_page = false;
                }
            }
            Command::ExportPdf => self.export_pdf(),
            Command::ExportMspdi => self.export_mspdi(),
            Command::McpPanel => {
                self.dialogs.mcp = true;
                self.st.start_page = false;
            }
            Command::OpenFromServer => {
                if self.server_dialog.is_none() {
                    self.server_dialog = Some(dialogs::server::OpenFromServer::new(&self.settings));
                }
            }
            Command::About => self.dialogs.about = true,
            Command::SetView(v) => {
                self.st.view = v;
                self.st.start_page = false;
            }
            Command::Filter(f) => self.st.filter = f,
            Command::Table(t) => self.st.table = t,
            Command::SortBy(s) => self.st.sort = s,
            Command::GroupBy(g) => {
                self.st.group = g;
                self.st.collapsed_groups.clear();
            }
            Command::History => self.dialogs.history = !self.dialogs.history,
            Command::SetTheme(t) => {
                self.settings.theme = t;
                self.settings.save();
                theme::set_choice(ctx, t);
            }
            other => {
                let ws = self.ws.clone();
                let mut ws = lock(&ws);
                let Some(doc) = ws.active_doc_mut() else {
                    return;
                };
                self.exec_doc(ctx, doc, other);
            }
        }
    }

    /// Opens a recovery copy as an unsaved document, bound to its original file when it had one.
    fn restore_recovered(&mut self, stem: &str) {
        let Some(item) = self.rec().find(stem).cloned() else { return };
        let Some(store) = self.rec().store().cloned() else { return };
        match store.load(&item) {
            Ok((mut project, notes)) => {
                // Two documents bound to one file would overwrite each other's saves: when the
                // file is open already, the copy comes back as an unsaved plan of its own.
                let open_paths: Vec<PathBuf> = lock(&self.ws).docs.iter().filter_map(|d| d.path.clone()).collect();
                let already_open = item.original_path.as_deref().is_some_and(|o| open_paths.iter().any(|p| same_file(p, o)));
                let path = if already_open { None } else { item.original_path.clone() };
                // Links to other projects are stored relative to the plan's folder: unsaved, the
                // copy has none, so they become absolute (from the original file's folder).
                if already_open {
                    for e in &mut project.external_projects {
                        let abs = plancraft_core::io::resolve_external_path(&e.path, item.original_path.as_deref());
                        e.path = abs.to_string_lossy().replace('\\', "/");
                    }
                }
                let mut ws = lock(&self.ws);
                ws.add(project, path);
                if let Some(doc) = ws.active_doc_mut() {
                    // Restored work is the user's: an agent must not be able to discard it.
                    doc.dirty = true;
                    doc.user_changes = true;
                    doc.load_notes = notes;
                }
                // Copy the restored document under its new handle before the old copy goes.
                let copied = self.rec().write_dirty(&ws);
                drop(ws);
                match copied {
                    Ok(_) => self.rec().discard(stem),
                    // Keep the old copy and drop it from the list only (it stays on disk).
                    Err(e) => {
                        self.rec().found.retain(|i| i.stem != stem);
                        self.st.toast(ToastKind::Warning, e);
                    }
                }
                self.st.start_page = false;
                self.st.sel = Default::default();
                self.st.gantt.initialized_for = None;
                self.st.view = ViewKind::Gantt;
                let target = match &item.original_path {
                    Some(_) if already_open => "Save it to keep it".to_string(),
                    Some(p) => format!("Save writes it back to {}", p.display()),
                    None => "Save it to keep it".to_string(),
                };
                self.set_status(format!("Restored {} from a recovery copy. {target}.", item.name));
                if already_open {
                    let file = item.original_path.as_deref().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned());
                    self.st.toast(
                        ToastKind::Warning,
                        format!(
                            "{} is already open; the recovered copy opened as an unsaved plan. Save it under another name or copy what you need.",
                            file.unwrap_or_else(|| item.name.clone())
                        ),
                    );
                }
            }
            Err(e) => self.st.toast(ToastKind::Error, format!("Could not restore {}: {e}", item.name)),
        }
    }

    fn close_doc(&mut self, handle: u64) {
        let mut ws = lock(&self.ws);
        ws.close(handle);
        let empty = ws.docs.is_empty();
        // The user chose to save or discard: the recovery copy is no longer needed.
        self.rec().prune(&ws);
        drop(ws);
        self.st.sel = Default::default();
        self.st.gantt.initialized_for = None;
        if empty {
            self.st.start_page = true;
        }
    }

    fn exec_doc(&mut self, ctx: &egui::Context, doc: &mut Document, cmd: Command) {
        let st = &mut self.st;
        let sel: Vec<TaskId> = views::selected_in_order(&doc.project, &st.sel);
        let first_sel_index = sel.first().and_then(|id| doc.project.task_index(*id));
        fn report<T>(st: &mut UiState, r: PlanResult<T>) {
            if let Err(e) = r {
                st.toast(ToastKind::Warning, e.to_string());
            }
        }
        match cmd {
            Command::Undo => match doc.try_undo_as(ChangeSource::User) {
                Ok(true) => {
                    let l = doc.last_change.as_ref().map(|l| l.0.clone()).unwrap_or_default();
                    self.status = Some((l, Instant::now()));
                }
                Ok(false) => {}
                Err(why) => st.toast(ToastKind::Warning, why),
            },
            Command::Redo => {
                if let Err(why) = doc.try_redo_as(ChangeSource::User) {
                    st.toast(ToastKind::Warning, why);
                }
            }
            Command::InsertTask | Command::InsertMilestone => {
                let milestone = cmd == Command::InsertMilestone;
                let at = first_sel_index;
                let r = doc.edit(if milestone { "Insert Milestone" } else { "Insert Task" }, |p| {
                    if milestone { p.add_milestone("<New Milestone>", at) } else { p.add_task("<New Task>", at) }
                });
                let id = match r {
                    Ok(id) => id,
                    Err(e) => return st.toast(ToastKind::Warning, e.to_string()),
                };
                st.sel.select_task(id);
                if st.view == ViewKind::NetworkDiagram {
                    // No table to type the name into: show the new box (double-click names it).
                    st.network.reveal_pending = true;
                } else if st.view == ViewKind::Board {
                    st.board.reveal_pending = true;
                } else if let Some(r) = doc.project.task_index(id) {
                    let rows = views::visible_rows(&doc.project, st);
                    if let Some(row) = views::row_of_task(&rows, r) {
                        // The plan's own columns: Name may have been moved, or columns inserted.
                        let name_col = crate::fields::table_fields(&doc.project, st.table)
                            .iter()
                            .position(|f| *f == crate::fields::Field::Builtin(crate::columns::TaskCol::Name))
                            .unwrap_or(0);
                        st.task_grid.begin_edit(row, name_col, String::new());
                        st.task_grid.scroll_to = Some(row);
                    }
                }
            }
            Command::InsertSummary => {
                if sel.is_empty() {
                    match doc.edit("Insert Summary Task", |p| p.add_task("<New Summary Task>", None)) {
                        Ok(id) => st.sel.select_task(id),
                        Err(e) => st.toast(ToastKind::Warning, e.to_string()),
                    }
                } else {
                    let r = doc.try_edit("Insert Summary Task", ChangeSource::User, |p| p.add_summary_over("<New Summary Task>", &sel));
                    match r {
                        Ok(id) => st.sel.select_task(id),
                        Err(e) => st.toast(ToastKind::Warning, e.to_string()),
                    }
                }
            }
            Command::DeleteSelection => {
                if st.view == ViewKind::ResourceSheet {
                    return self.exec_doc(ctx, doc, Command::DeleteResources);
                }
                if !sel.is_empty() {
                    let n = sel.len();
                    let r = doc.try_edit(if n == 1 { "Delete Task" } else { "Delete Tasks" }, ChangeSource::User, |p| {
                        p.delete_tasks(&sel).map(|_| ())
                    });
                    report(st, r);
                    st.sel.tasks.clear();
                }
            }
            Command::Indent | Command::Outdent => {
                let indent = cmd == Command::Indent;
                // Only move top-most selected blocks (children follow their parent).
                let tops: Vec<TaskId> = sel
                    .iter()
                    .copied()
                    .filter(|id| doc.project.task_index(*id).is_some_and(|i| !has_selected_ancestor(&doc.project, i, &sel)))
                    .collect();
                let r = doc.try_edit(if indent { "Indent" } else { "Outdent" }, ChangeSource::User, |p| {
                    for id in &tops {
                        if indent { p.indent(*id)? } else { p.outdent(*id)? }
                    }
                    Ok(())
                });
                report(st, r);
            }
            Command::MoveUp | Command::MoveDown => {
                let up = cmd == Command::MoveUp;
                // Every selected block moves one step (subtasks travel with their summary). Up
                // goes top to bottom and down bottom to top, so neighbouring selected tasks move
                // together and keep their order. One at the edge of its group stops the whole move,
                // as it does for a single task.
                let mut tops: Vec<TaskId> = sel
                    .iter()
                    .copied()
                    .filter(|id| doc.project.task_index(*id).is_some_and(|i| !has_selected_ancestor(&doc.project, i, &sel)))
                    .collect();
                if !up {
                    tops.reverse();
                }
                if !tops.is_empty() {
                    let r = doc.try_edit(if up { "Move Up" } else { "Move Down" }, ChangeSource::User, |p| {
                        for id in &tops {
                            p.shift_task(*id, up)?;
                        }
                        Ok(())
                    });
                    report(st, r);
                }
            }
            Command::LinkSelected => {
                if sel.len() >= 2 {
                    let r = doc.try_edit("Link Tasks", ChangeSource::User, |p| p.link_chain(&sel));
                    report(st, r);
                } else {
                    st.toast(ToastKind::Info, "Select two or more tasks to link them in order.");
                }
            }
            Command::UnlinkSelected => {
                if !sel.is_empty() {
                    let r = doc.edit("Unlink Tasks", |p| p.unlink_tasks(&sel));
                    report(st, r);
                }
            }
            Command::SetPercent(pct) => {
                if !sel.is_empty() {
                    let r = doc.try_edit(&format!("{pct}% Complete"), ChangeSource::User, |p| {
                        for id in &sel {
                            p.set_percent_complete(*id, pct)?;
                        }
                        Ok(())
                    });
                    report(st, r);
                }
            }
            Command::MarkOnTrack => {
                // Progress up to the status date (or now), never lowering what was entered.
                let r = doc.try_edit("Mark on Track", ChangeSource::User, |p| p.mark_on_track(&sel));
                match r {
                    Ok(r) if r.updated.is_empty() => st.toast(ToastKind::Info, "Already on track: no progress needed adding"),
                    Ok(_) => {}
                    Err(e) => st.toast(ToastKind::Warning, e.to_string()),
                }
            }
            Command::UpdateProject => {
                self.dialogs.update_project = Some(Bound::new(doc, dialogs::update_project::UpdateProject::new(&doc.project, sel.clone())));
            }
            Command::ToggleActive => {
                if !sel.is_empty() {
                    let r = doc.edit("Inactivate Task", |p| {
                        let new = !sel.first().and_then(|id| p.task(*id)).map(|t| t.active).unwrap_or(true);
                        for id in &sel {
                            if let Some(t) = p.task_mut(*id) {
                                t.active = new;
                            }
                        }
                    });
                    report(st, r);
                }
            }
            Command::SetMode(mode) => {
                if !sel.is_empty() {
                    let r = doc.try_edit(
                        if mode == ScheduleMode::Manual { "Manually Schedule" } else { "Auto Schedule" },
                        ChangeSource::User,
                        |p| p.set_schedule_mode(&sel, mode),
                    );
                    report(st, r);
                }
            }
            // A recurring task opens its Recurring Task Information instead.
            Command::TaskInfo
                if sel.len() <= 1
                    && let Some(d) = st
                        .sel
                        .single_task()
                        .or(sel.first().copied())
                        .and_then(|id| dialogs::recurring::RecurringTask::edit(&doc.project, id)) =>
            {
                self.dialogs.recurring = Some(Bound::new(doc, d));
            }
            Command::RecurringTask => {
                let above = sel.first().copied();
                self.dialogs.recurring = Some(Bound::new(doc, dialogs::recurring::RecurringTask::new(&doc.project, above)));
            }
            Command::SplitTask => {
                st.gantt.split_mode = !st.gantt.split_mode;
                if st.gantt.split_mode {
                    if !matches!(st.view, ViewKind::Gantt | ViewKind::TrackingGantt) {
                        st.view = ViewKind::Gantt;
                    }
                    st.toast(ToastKind::Info, "Split Task: click a task's bar where its work should pause");
                }
            }
            Command::RejoinTasks => {
                let r = doc.edit("Rejoin Split Task", |p| {
                    let mut n = 0;
                    for id in &sel {
                        n += p.remove_splits(*id).unwrap_or(0);
                    }
                    n
                });
                match r {
                    Ok(0) => st.toast(ToastKind::Info, "The selected tasks have no splits"),
                    Ok(_) => {}
                    Err(e) => st.toast(ToastKind::Warning, e.to_string()),
                }
            }
            // Several tasks selected: Multiple Task Information.
            Command::TaskInfo if sel.len() > 1 => {
                self.dialogs.task_info = dialogs::task_info::TaskInfo::new_multi(&doc.project, &sel).map(|d| Bound::new(doc, d));
            }
            Command::TaskInfo | Command::TaskNotes => {
                if let Some(id) = st.sel.single_task().or(sel.first().copied()) {
                    self.dialogs.task_info =
                        dialogs::task_info::TaskInfo::new(&doc.project, id, cmd == Command::TaskNotes).map(|d| Bound::new(doc, d));
                }
            }
            Command::FillDown => match dialogs::find::fill_down(doc, st) {
                Ok(msg) => self.status = Some((msg, Instant::now())),
                Err(e) => st.toast(ToastKind::Info, e),
            },
            Command::Find(replacing) => match self.dialogs.find.as_mut().filter(|b| b.doc == doc.handle) {
                Some(b) => b.dialog.reopen(replacing),
                None => self.dialogs.find = Some(Bound::new(doc, dialogs::find::FindReplace::new(replacing))),
            },
            Command::GoTo => self.dialogs.goto = Some(Bound::new(doc, dialogs::find::GoTo::new())),
            Command::ToggleTimeline => {
                if !sel.is_empty() {
                    let r = doc.edit("Add to Timeline", |p| {
                        let new = !sel.first().and_then(|id| p.task(*id)).map(|t| t.show_on_timeline).unwrap_or(false);
                        for id in &sel {
                            if let Some(t) = p.task_mut(*id) {
                                t.show_on_timeline = new;
                            }
                        }
                    });
                    report(st, r);
                    st.timeline_visible = true;
                }
            }
            Command::ScrollToTask if st.view == ViewKind::NetworkDiagram => st.network.reveal_pending = true,
            Command::ScrollToTask if st.view == ViewKind::Board => st.board.reveal_pending = true,
            // The board has no timescale to zoom.
            Command::ZoomIn | Command::ZoomOut | Command::ZoomEntire | Command::ZoomSelected if st.view == ViewKind::Board => {}
            Command::ScrollToTask => {
                if let Some(id) = sel.first()
                    && let Some(t) = doc.project.task(*id)
                    && let Some(s) = t.sched.start
                {
                    st.gantt.scroll_to(s);
                    let rows = views::visible_rows(&doc.project, st);
                    if let Some(idx) = doc.project.task_index(*id)
                        && let Some(r) = views::row_of_task(&rows, idx)
                    {
                        st.task_grid.scroll_to = Some(r);
                    }
                }
            }
            Command::Copy | Command::Cut => {
                if !sel.is_empty() {
                    let mut copied = Vec::new();
                    for (i, t) in doc.project.tasks.iter().enumerate() {
                        let inside = sel.contains(&t.id) || has_selected_ancestor(&doc.project, i, &sel);
                        if inside {
                            copied.push(t.clone());
                        }
                    }
                    // Task names on the system clipboard, so they paste into other programs and
                    // so the backend reports the next Ctrl+V as a paste.
                    ctx.copy_text(copied.iter().map(|t| t.name.as_str()).collect::<Vec<_>>().join("\n"));
                    self.clip_source = Some(ClipSource::capture(&doc.project, &copied));
                    st.clipboard = copied;
                    if cmd == Command::Cut {
                        let r = doc.try_edit("Cut Tasks", ChangeSource::User, |p| p.delete_tasks(&sel).map(|_| ()));
                        report(st, r);
                        st.sel.tasks.clear();
                    }
                }
            }
            Command::Paste => {
                if !st.clipboard.is_empty() {
                    let clip = st.clipboard.clone();
                    // What the copy noted about its plan, while the clipboard still holds that copy.
                    let source = self.clip_source.as_ref().filter(|s| s.holds(&clip)).cloned();
                    let at = first_sel_index.unwrap_or(doc.project.tasks.len());
                    let r = doc.edit("Paste Tasks", |p| paste_tasks(p, &clip, source.as_ref(), at));
                    match r {
                        Ok((ids, notes)) => {
                            st.sel.tasks = ids.into_iter().collect();
                            if !notes.is_empty() {
                                st.toast(ToastKind::Info, format!("Pasted the tasks. {}", notes.join(" ")));
                            }
                        }
                        Err(e) => st.toast(ToastKind::Warning, e.to_string()),
                    }
                }
            }
            Command::InsertResource => {
                let r = doc.edit("Add Resource", |p| {
                    let n = p.resources.len() + 1;
                    p.add_resource(format!("Resource {n}"))
                });
                let id = match r {
                    Ok(id) => id,
                    Err(e) => return st.toast(ToastKind::Warning, e.to_string()),
                };
                st.sel.select_resource(id);
                if st.view != ViewKind::ResourceSheet {
                    st.view = ViewKind::ResourceSheet;
                }
                if let Some(row) = doc.project.resources.iter().position(|r| r.id == id) {
                    st.resource_grid.begin_edit(row, 1, String::new());
                    st.resource_grid.scroll_to = Some(row);
                }
            }
            Command::DeleteResources => {
                let ids: Vec<ResourceId> = st.sel.resources.iter().copied().collect();
                if !ids.is_empty() {
                    let r = doc.try_edit("Delete Resource", ChangeSource::User, |p| p.delete_resources(&ids));
                    report(st, r);
                    st.sel.resources.clear();
                }
            }
            Command::ResourceInfo => {
                if let Some(id) = st.sel.single_resource() {
                    self.dialogs.resource_info = dialogs::resource_info::ResourceInfo::new(&doc.project, id).map(|d| Bound::new(doc, d));
                }
            }
            Command::AssignResources => self.dialogs.assign = Some(Bound::new(doc, Default::default())),
            Command::LevelAll => {
                let r = match doc.edit("Level Resources", |p| level_resources(p, &LevelingOptions::default())) {
                    Ok(r) => r,
                    Err(e) => return st.toast(ToastKind::Warning, e.to_string()),
                };
                let msg = if r.delayed.is_empty() && r.unresolved.is_empty() {
                    "No overallocations needed leveling.".to_string()
                } else {
                    format!(
                        "Leveling delayed {} task(s). Finish: {} – {}",
                        r.delayed.len(),
                        crate::format::date(r.finish_before),
                        crate::format::date(r.finish_after)
                    )
                };
                st.toast(ToastKind::Success, msg);
                for u in r.unresolved {
                    st.toast(ToastKind::Warning, u);
                }
            }
            Command::ClearLeveling => report(st, doc.edit("Clear Leveling", clear_leveling)),
            Command::ProjectInfo => {
                self.dialogs.project_info = Some(Bound::new(doc, dialogs::project_info::ProjectInfo::new(&doc.project)))
            }
            Command::ChangeWorkingTime => {
                self.dialogs.working_time = Some(Bound::new(doc, dialogs::working_time::WorkingTime::new(&doc.project)));
            }
            Command::SetBaseline => {
                self.dialogs.set_baseline = Some(Bound::new(doc, dialogs::baseline::SetBaseline::new(sel.clone())));
            }
            Command::ClearBaseline => {
                self.dialogs.clear_baseline = Some(Bound::new(doc, dialogs::baseline::ClearBaseline::new(sel.clone())));
            }
            Command::UpdateProgressFromTime => match doc.edit("Update Progress from Time", |p| p.update_progress_from_time()) {
                Ok(_) => st.toast(ToastKind::Success, "Progress updated from tracked time"),
                Err(e) => st.toast(ToastKind::Warning, e.to_string()),
            },
            Command::ExternalLinks => self.dialogs.external = Some(Bound::new(doc, Default::default())),
            Command::RefreshExternal => self.refresh_links = Some(doc.handle),
            Command::Statistics => self.dialogs.stats = true,
            Command::ZoomIn | Command::ZoomOut if st.view == ViewKind::NetworkDiagram => {
                let n = &mut st.network;
                let half = n.view_rect.size().max(egui::Vec2::ZERO) / 2.0;
                let centre = n.offset + half / n.zoom;
                let factor = if cmd == Command::ZoomIn { 1.25 } else { 0.8 };
                n.zoom = (n.zoom * factor).clamp(views::network::MIN_ZOOM, views::network::MAX_ZOOM);
                n.offset = centre - half / n.zoom;
            }
            Command::ZoomEntire if st.view == ViewKind::NetworkDiagram => st.network.fit_pending = true,
            Command::ZoomSelected if st.view == ViewKind::NetworkDiagram => st.network.fit_selected_pending = true,
            Command::ZoomIn | Command::ZoomOut => {
                let g = &mut st.gantt;
                g.zoom(if cmd == Command::ZoomIn { 1 } else { -1 }, 400.0, 0.0);
            }
            Command::ZoomEntire | Command::ZoomSelected => {
                let ts: Vec<&Task> = if cmd == Command::ZoomEntire {
                    doc.project.tasks.iter().collect()
                } else {
                    sel.iter().filter_map(|id| doc.project.task(*id)).collect()
                };
                // The Multiple Baselines Gantt shows baseline bars, which can lie far from the schedule.
                let spans: Vec<(DateTime, DateTime)> = if st.view == ViewKind::MultipleBaselines {
                    let slots: Vec<u8> = views::gantt_chart::baseline_slots(st.gantt.multi_baselines).collect();
                    ts.iter().flat_map(|t| slots.iter().filter_map(|&n| t.baseline_in(n)).map(|b| (b.start, b.finish))).collect()
                } else {
                    ts.iter().filter_map(|t| t.sched.start.zip(t.sched.finish)).collect()
                };
                let (s, f) = (spans.iter().map(|x| x.0).min(), spans.iter().map(|x| x.1).max());
                if let (Some(s), Some(f)) = (s, f) {
                    let width = (ctx.content_rect().width() - st.table_width - 80.0).max(200.0);
                    let days = (f.signed_duration_since(s).num_minutes() as f32 / 1440.0).max(1.0) * 1.1 + 2.0;
                    st.gantt.px_per_day = (width / days).clamp(0.5, 140.0);
                    st.gantt.left = plancraft_core::dates::add_minutes(s, -1440);
                }
            }
            Command::CollapseAll => {
                for (i, t) in doc.project.tasks.iter().enumerate() {
                    if doc.project.has_children(i) {
                        st.collapsed.insert(t.id);
                    }
                }
            }
            Command::ExpandAll => st.collapsed.clear(),
            Command::InsertColumn { .. } | Command::HideColumn(_) | Command::MoveColumn { .. } | Command::ResetColumns => {
                let table = st.table;
                let before = crate::fields::table_fields(&doc.project, table);
                let mut cols = before.clone();
                match &cmd {
                    Command::InsertColumn { at, key } => {
                        let Some(field) = crate::fields::Field::from_key(&doc.project, key) else {
                            return st.toast(ToastKind::Warning, "that field no longer exists");
                        };
                        cols.retain(|f| *f != field);
                        cols.insert((*at).min(cols.len()), field);
                    }
                    Command::HideColumn(at) if cols.len() > 1 && *at < cols.len() => {
                        cols.remove(*at);
                    }
                    Command::MoveColumn { at, right } => {
                        let to = if *right { at.saturating_add(1) } else { at.saturating_sub(1) };
                        if *at < cols.len() && to < cols.len() {
                            cols.swap(*at, to);
                        }
                    }
                    _ => {}
                }
                let reset = cmd == Command::ResetColumns;
                // Nothing to change: no undo step, and the plan stays unmodified.
                if (reset && doc.project.table_layout(table.label()).is_none()) || (!reset && cols == before) {
                    return;
                }
                let r = doc.edit_msg("Change Columns", |p| {
                    if reset {
                        p.set_table_columns(table.label(), &[]).map_err(|e| e.to_string())
                    } else {
                        crate::fields::save_columns(p, table, &cols)
                    }
                });
                if let Err(e) = r {
                    st.toast(ToastKind::Warning, e);
                }
            }
            Command::CustomFields => {
                self.dialogs.custom_fields = Some(Bound::new(doc, dialogs::custom_fields::CustomFields::default()));
            }
            _ => {}
        }
    }

    // ------------------------------------------------------------------
    // Layout pieces
    // ------------------------------------------------------------------

    fn menu_info(&self, ws: &Workspace) -> MenuInfo {
        let doc = ws.active_doc();
        // Sort/group choices: the table's columns, then custom fields not shown.
        let fields = doc
            .map(|d| {
                let p = &d.project;
                let mut list = crate::fields::table_fields(p, self.st.table);
                for f in p.custom_fields.iter().map(|f| crate::fields::Field::Custom(f.id)) {
                    if !list.contains(&f) {
                        list.push(f);
                    }
                }
                list.into_iter()
                    .filter(|f| *f != crate::fields::Field::Builtin(crate::columns::TaskCol::Indicators))
                    .map(|f| (f.key(), f.title(p)))
                    .collect()
            })
            .unwrap_or_default();
        MenuInfo {
            has_doc: doc.is_some(),
            title: doc.map(|d| d.title()).unwrap_or_default(),
            can_undo: doc.is_some_and(|d| d.can_undo()),
            can_redo: doc.is_some_and(|d| d.can_redo()),
            undo_label: doc.and_then(|d| d.undo_label().map(String::from)),
            task_selected: !self.st.sel.tasks.is_empty(),
            resource_selected: !self.st.sel.resources.is_empty(),
            mcp_running: self.mcp.running(),
            mcp_url: self.mcp.url(),
            history_open: self.dialogs.history,
            fields,
        }
    }

    fn doc_tabs(&mut self, ui: &mut Ui, ws: &mut Workspace, out: &mut Vec<Command>) {
        if ws.docs.len() < 2 {
            return;
        }
        let rect = ui.available_rect_before_wrap();
        let rect = Rect::from_min_size(rect.min, vec2(rect.width(), 26.0));
        ui.painter().rect_filled(rect, 0.0, theme::pal().PANEL_BG);
        let mut child =
            ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink2(vec2(8.0, 2.0))).layout(Layout::left_to_right(Align::Center)));
        let mut switch = None;
        let mut close = None;
        for (i, d) in ws.docs.iter().enumerate() {
            let active = ws.active == Some(i);
            let mark = if self.disk_bars.contains_key(&d.handle) || d.lock.elsewhere().is_some() { "⚠ " } else { "" };
            let remote = self.remotes.get(d.handle);
            let icon = if remote.is_some() { "👥" } else { "📄" };
            let mut resp = child.selectable_label(active, format!("{icon} {mark}{}", d.title()));
            if let Some(r) = remote {
                resp = resp.on_hover_text(format!("Opened from {} as {}: {}", r.server, r.user, r.state_label()));
            }
            if resp.clicked() {
                switch = Some(i);
            }
            if child.small_button("✖").on_hover_text("Close").clicked() {
                close = Some((d.handle, d.dirty));
            }
            child.add_space(6.0);
        }
        if let Some(i) = switch {
            ws.active = Some(i);
            self.st.sel = Default::default();
            self.st.gantt.initialized_for = None;
        }
        if let Some((h, dirty)) = close {
            if dirty {
                self.confirm_close = Some(CloseRequest::Document(h));
            } else {
                ws.close(h);
                if ws.docs.is_empty() {
                    self.st.start_page = true;
                }
            }
        }
        let _ = out;
        ui.allocate_rect(rect, Sense::hover());
    }

    fn status_bar(&mut self, ui: &mut Ui, ws: &mut Workspace, out: &mut Vec<Command>) {
        let doc = ws.active_doc_mut();
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 10.0;
            let msg = match &self.status {
                Some((m, at)) if at.elapsed() < StdDuration::from_secs(8) => m.clone(),
                _ => "Ready".to_string(),
            };
            ui.label(RichText::new(msg).size(12.0));
            if let Some(doc) = doc {
                if let Some(r) = self.remotes.get(doc.handle) {
                    ui.separator();
                    shared_status(ui, r);
                }
                ui.separator();
                let auto = doc.project.settings.new_tasks_auto;
                let resp = ui.add(
                    egui::Label::new(
                        RichText::new(if auto { "⟳ New Tasks: Auto Scheduled" } else { "📌 New Tasks: Manually Scheduled" }).size(12.0),
                    )
                    .sense(Sense::click()),
                );
                if resp.on_hover_text("Click to change how new tasks are scheduled").clicked()
                    && let Err(e) = doc.edit("New Task Mode", |p| p.settings.new_tasks_auto = !auto)
                {
                    self.st.toast(ToastKind::Warning, e.to_string());
                }
                if self.st.filter != crate::state::TaskFilter::All {
                    ui.separator();
                    ui.label(RichText::new(format!("Filter: {}", self.st.filter.label())).size(12.0).color(theme::pal().ACCENT_MID));
                }
                let warnings = doc.project.sched.warnings.len();
                if warnings > 0 {
                    ui.separator();
                    let r = ui.add(
                        egui::Label::new(RichText::new(format!("⚠ {warnings} scheduling issue(s)")).size(12.0).color(theme::pal().ERROR))
                            .sense(Sense::click()),
                    );
                    if r.on_hover_text(doc.project.sched.warnings.join("\n")).clicked() {
                        out.push(Command::Statistics);
                    }
                }
                if let Some((label, ChangeSource::Agent)) = &doc.last_change {
                    ui.separator();
                    ui.label(RichText::new(format!("⚡ Last AI change: {label}")).size(12.0).color(theme::pal().ACCENT_MID));
                }
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if self.st.view == ViewKind::NetworkDiagram {
                    let n = &mut self.st.network;
                    let mut z = n.zoom;
                    if ui
                        .add(
                            egui::Slider::new(&mut z, views::network::MIN_ZOOM..=views::network::MAX_ZOOM)
                                .logarithmic(true)
                                .show_value(false),
                        )
                        .on_hover_text("Diagram zoom")
                        .changed()
                    {
                        // About the centre of the view.
                        let half = n.view_rect.size().max(egui::Vec2::ZERO) / 2.0;
                        let centre = n.offset + half / n.zoom;
                        n.zoom = z;
                        n.offset = centre - half / n.zoom;
                    }
                } else {
                    let mut z = self.st.gantt.px_per_day;
                    if ui
                        .add(egui::Slider::new(&mut z, 1.0..=140.0).logarithmic(true).show_value(false))
                        .on_hover_text("Timescale zoom")
                        .changed()
                    {
                        self.st.gantt.px_per_day = z;
                    }
                }
                ui.label(RichText::new("🔍").size(12.0));
                ui.separator();
                for v in [ViewKind::Timesheet, ViewKind::ResourceSheet, ViewKind::TeamPlanner, ViewKind::TaskUsage, ViewKind::Gantt] {
                    let active = self.st.view == v;
                    if ui.selectable_label(active, RichText::new(v.icon()).size(13.0)).on_hover_text(v.label()).clicked() {
                        out.push(Command::SetView(v));
                    }
                }
            });
        });
    }

    fn view_bar(&self, ui: &Ui, rect: Rect) {
        let p = ui.painter();
        p.rect_filled(rect, 0.0, theme::pal().TITLE_BAR);
        let label = self.st.view.label().to_uppercase();
        let galley = p.layout_no_wrap(label, FontId::proportional(12.0), Color32::WHITE);
        let w = galley.size().x;
        let pos = pos2(rect.center().x - galley.size().y / 2.0, rect.center().y + w / 2.0);
        p.add(egui::epaint::TextShape::new(pos, galley, Color32::WHITE).with_angle(-std::f32::consts::FRAC_PI_2));
    }

    fn toasts(&mut self, ctx: &egui::Context) {
        self.st.toasts.retain(|t| t.at.elapsed() < StdDuration::from_secs(6));
        if self.st.toasts.is_empty() {
            return;
        }
        let screen = ctx.content_rect();
        egui::Area::new(egui::Id::new("toasts")).order(egui::Order::Foreground).anchor(Align2::RIGHT_BOTTOM, vec2(-16.0, -36.0)).show(
            ctx,
            |ui| {
                ui.set_max_width(420.0f32.min(screen.width() - 40.0));
                let mut dismiss = None;
                for (i, t) in self.st.toasts.iter().enumerate() {
                    let (icon, color) = match t.kind {
                        ToastKind::Info => ("ℹ", theme::pal().ACCENT_MID),
                        ToastKind::Success => ("✔", theme::pal().OK),
                        ToastKind::Warning => ("⚠", theme::pal().WARN),
                        ToastKind::Error => ("✖", theme::pal().ERROR),
                    };
                    let resp = egui::Frame::popup(ui.style())
                        .stroke(Stroke::new(1.0, color))
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(icon).color(color).size(15.0));
                                ui.label(&t.text);
                            });
                        })
                        .response;
                    if resp.interact(Sense::click()).clicked() {
                        dismiss = Some(i);
                    }
                }
                if let Some(i) = dismiss {
                    self.st.toasts.remove(i);
                }
            },
        );
        ctx.request_repaint_after(StdDuration::from_millis(500));
    }

    fn confirm_dialog(&mut self, ctx: &egui::Context) {
        let Some(req) = self.confirm_close else {
            self.kept_dropped.clear();
            return;
        };
        // Work the server could not take would go with the shared plan: it goes into a plan of
        // its own first, unsaved (so it is asked about, and kept by recovery copies).
        let closing: Vec<u64> = match req {
            CloseRequest::Document(h) => vec![h],
            CloseRequest::App => lock(&self.ws).docs.iter().map(|d| d.handle).collect(),
        };
        for h in closing {
            if let Some(name) = self.keep_dropped(h) {
                self.kept_dropped.push(name);
            }
        }
        let names: Vec<String> = {
            let ws = lock(&self.ws);
            match req {
                CloseRequest::Document(h) => ws.docs.iter().filter(|d| d.handle == h).map(|d| d.project.name.clone()).collect(),
                CloseRequest::App => ws.docs.iter().filter(|d| d.dirty).map(|d| d.project.name.clone()).collect(),
            }
        };
        let shared: Vec<u64> = {
            let ws = lock(&self.ws);
            ws.docs
                .iter()
                .filter(|d| {
                    d.shared.is_some() && d.dirty && matches!(req, CloseRequest::App)
                        || matches!(req, CloseRequest::Document(h) if h == d.handle && d.shared.is_some())
                })
                .map(|d| d.handle)
                .collect()
        };
        let mut choice = None;
        egui::Modal::new(egui::Id::new("confirm_close")).show(ctx, |ui| {
            ui.set_width(380.0);
            ui.heading("Save changes?");
            ui.label(format!("Do you want to save changes to {}?", names.join(", ")));
            if !shared.is_empty() {
                ui.label(
                    RichText::new(
                        "Changes to plans opened from a server that the server has not confirmed yet are lost on closing. Save keeps a copy of your own.",
                    )
                    .color(theme::pal().TEXT_WEAK),
                );
            }
            if !self.kept_dropped.is_empty() {
                ui.label(
                    RichText::new(format!(
                        "Your changes the server could not take are in a plan of their own: {}. It stays open until you save or close it.",
                        self.kept_dropped.join(", ")
                    ))
                    .color(theme::pal().TEXT_WEAK),
                );
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.add(egui::Button::new(RichText::new("Save").color(Color32::WHITE)).fill(theme::pal().ACCENT)).clicked() {
                    choice = Some(0);
                }
                if ui.button("Don't Save").clicked() {
                    choice = Some(1);
                }
                if ui.button("Cancel").clicked() {
                    choice = Some(2);
                }
            });
        });
        match (choice, req) {
            (Some(0), CloseRequest::Document(h)) => {
                {
                    let mut ws = lock(&self.ws);
                    ws.active = ws.index_of(h);
                }
                let saved = if shared.contains(&h) { self.save_copy(h) } else { self.save(false) };
                if saved {
                    self.close_doc(h);
                } else if self.confirm_overwrite.is_some() {
                    self.close_after_save = Some(CloseRequest::Document(h));
                }
                self.confirm_close = None;
            }
            (Some(1), CloseRequest::Document(h)) => {
                self.close_doc(h);
                self.confirm_close = None;
            }
            (Some(0), CloseRequest::App) => {
                let dirty: Vec<usize> = lock(&self.ws).docs.iter().enumerate().filter(|(_, d)| d.dirty).map(|(i, _)| i).collect();
                let mut all = true;
                for i in dirty {
                    lock(&self.ws).active = Some(i);
                    let h = lock(&self.ws).docs.get(i).map(|d| d.handle);
                    let saved = match h {
                        Some(h) if shared.contains(&h) => self.save_copy(h),
                        _ => self.save(false),
                    };
                    if !saved {
                        // Stop at the first file that needs an answer (or failed); exiting carries
                        // on once the overwrite question is answered.
                        all = false;
                        if self.confirm_overwrite.is_some() {
                            self.close_after_save = Some(CloseRequest::App);
                        }
                        break;
                    }
                }
                self.confirm_close = None;
                if all {
                    self.allow_exit = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
            (Some(1), CloseRequest::App) => {
                self.confirm_close = None;
                self.allow_exit = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            (Some(_), _) => self.confirm_close = None,
            _ => {}
        }
    }

    /// Asks before replacing a file another program changed since it was opened or saved.
    fn overwrite_dialog(&mut self, ctx: &egui::Context) {
        let Some(handle) = self.confirm_overwrite else {
            return;
        };
        let name = {
            let ws = lock(&self.ws);
            match ws.index_of(handle).and_then(|i| ws.docs.get(i)) {
                Some(d) => {
                    d.path.as_ref().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or(d.project.name.clone())
                }
                None => {
                    self.confirm_overwrite = None;
                    return;
                }
            }
        };
        let mut choice = None;
        egui::Modal::new(egui::Id::new("confirm_overwrite")).show(ctx, |ui| {
            ui.set_width(420.0);
            ui.heading("File changed on disk");
            ui.label(format!(
                "{name} was changed by another program since you opened or last saved it. Overwrite it with your version, or save yours under another name?"
            ));
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.add(egui::Button::new(RichText::new("Save As…").color(Color32::WHITE)).fill(theme::pal().ACCENT)).clicked() {
                    choice = Some(0);
                }
                if ui.button("Overwrite").clicked() {
                    choice = Some(1);
                }
                if ui.button("Cancel").clicked() {
                    choice = Some(2);
                }
            });
        });
        let Some(choice) = choice else { return };
        self.confirm_overwrite = None;
        {
            let mut ws = lock(&self.ws);
            ws.active = ws.index_of(handle);
        }
        let saved = match choice {
            0 => self.save(true),
            1 => {
                let r = {
                    let mut ws = lock(&self.ws);
                    ws.active_doc_mut().map(|d| d.save_overwriting().map(|_| d.path.clone().unwrap_or_default()))
                };
                match r {
                    Some(Ok(p)) => {
                        self.settings.add_recent(p.clone());
                        self.set_status(format!("Saved {}", p.display()));
                        true
                    }
                    Some(Err(e)) => {
                        self.st.toast(ToastKind::Error, e.to_string());
                        false
                    }
                    None => false,
                }
            }
            _ => false,
        };
        // Carry on with the close that stopped to ask (Cancel or a failed save stops it).
        match self.close_after_save.take() {
            Some(CloseRequest::Document(h)) if saved => self.close_doc(h),
            // Asks again about any other unsaved plans, then exits.
            Some(CloseRequest::App) if saved => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            _ => {}
        }
    }

    /// Developer automation used to capture screenshots of views and dialogs:
    /// `PLANCRAFT_SCREENSHOT=out.png [PLANCRAFT_SELECT=row] [PLANCRAFT_CMD=TaskInfo,...]`.
    fn screenshot_hook(&mut self, ctx: &egui::Context) {
        if self.shot.as_ref().is_some_and(|(_, f)| *f == 3) {
            let row: usize = std::env::var("PLANCRAFT_SELECT").ok().and_then(|v| v.parse().ok()).unwrap_or(3);
            if let Some(doc) = lock(&self.ws).active_doc() {
                if let Some(t) = doc.project.tasks.get(row.saturating_sub(1)) {
                    self.st.sel.select_task(t.id);
                }
                if let Some(r) = doc.project.resources.first() {
                    self.st.sel.select_resource(r.id);
                }
            }
            for name in std::env::var("PLANCRAFT_CMD").unwrap_or_default().split(',').filter(|s| !s.is_empty()) {
                let cmd = match name {
                    "TaskInfo" => Command::TaskInfo,
                    "ResourceInfo" => Command::ResourceInfo,
                    "ProjectInfo" => Command::ProjectInfo,
                    "WorkingTime" => Command::ChangeWorkingTime,
                    "Statistics" => Command::Statistics,
                    "Mcp" => Command::McpPanel,
                    "OpenServer" => Command::OpenFromServer,
                    "Assign" => Command::AssignResources,
                    "External" => Command::ExternalLinks,
                    "About" => Command::About,
                    "Level" => Command::LevelAll,
                    "McpStart" => {
                        self.mcp.start(self.ws.clone(), ctx.clone(), self.settings.mcp_port);
                        continue;
                    }
                    "Critical" => {
                        self.st.gantt.show_critical = true;
                        continue;
                    }
                    "ZoomEntire" => Command::ZoomEntire,
                    "FileMenu" => {
                        egui::Popup::open_id(ctx, egui::Id::new("tab_file").with("popup"));
                        continue;
                    }
                    "ViewMenu" => {
                        egui::Popup::open_id(ctx, egui::Id::new("view_selector").with("popup"));
                        continue;
                    }
                    "StartPage" => {
                        self.st.start_page = true;
                        continue;
                    }
                    "Panic" => {
                        self.inject_panic = true;
                        continue;
                    }
                    // The bar shown when the plan's file changed on disk while it had unsaved
                    // changes, with Compare open (the "file" is the plan with one task renamed).
                    "DiskChanged" => {
                        if let Some(doc) = lock(&self.ws).active_doc() {
                            let mut theirs = doc.project.clone();
                            if let Some(t) = theirs.tasks.iter_mut().find(|t| t.outline_level > 1) {
                                t.name.push_str(" (revised)");
                            }
                            let compare = Some(plancraft_core::watch::compare(&doc.project, &theirs));
                            let kind = DiskBarKind::Changed { theirs: Box::new(theirs), notes: Vec::new(), stamp: None };
                            self.disk_bars.insert(doc.handle, DiskBar { path: doc.path.clone(), kind, compare });
                        }
                        continue;
                    }
                    "CustomFields" => Command::CustomFields,
                    "Find" => Command::Find(true),
                    "GoTo" => Command::GoTo,
                    "Print" => Command::Print,
                    "UpdateProject" => Command::UpdateProject,
                    "SetBaseline" => Command::SetBaseline,
                    "RecurringTask" => Command::RecurringTask,
                    "ClearBaseline" => Command::ClearBaseline,
                    "Track" => {
                        // Progress through day 10, then a week's pause: split bars to look at.
                        if let Some(doc) = lock(&self.ws).active_doc_mut() {
                            let _ = doc.try_edit("Track", ChangeSource::User, |p| {
                                let day = |n: i64| plancraft_core::calendar::shift(p.start, n * 24 * 60).date();
                                let (status, after) = (day(11), day(18));
                                p.set_status_date(Some(status));
                                p.update_progress_through(
                                    &[],
                                    p.end_of_working_day(status),
                                    plancraft_core::ops::ProgressUpdate::Scheduled,
                                )?;
                                p.reschedule_uncompleted_work(&[], p.end_of_working_day(after))
                            });
                        }
                        continue;
                    }
                    "Recur" => {
                        // A split task and a weekly recurring task to look at.
                        if let Some(doc) = lock(&self.ws).active_doc_mut() {
                            let _ = doc.try_edit("Recur", ChangeSource::User, |p| {
                                use plancraft_core::recurring::{Pattern, RecurEnd, Recurrence};
                                let t = p
                                    .tasks
                                    .iter()
                                    .find(|t| !t.sched.is_summary && t.duration >= 4 * 480 && t.percent_complete == 0)
                                    .map(|t| t.id);
                                if let Some(t) = t
                                    && let Some(s) = p.task(t).and_then(|t| t.sched.start)
                                {
                                    let day = |n: i64| plancraft_core::calendar::shift(s, n * 24 * 60);
                                    p.split_task(t, day(1).date().and_hms_opt(17, 0, 0).unwrap_or(s), day(5))?;
                                }
                                let start = p.start.date();
                                let rec = Recurrence {
                                    pattern: Pattern::Weekly { interval: 1, days: 0b10 },
                                    start,
                                    end: RecurEnd::After(6),
                                    duration: 60,
                                    calendar: None,
                                    ignore_resource_calendars: false,
                                };
                                p.add_recurring_task("Team meeting", rec, Some(1))?;
                                Ok::<(), PlanError>(())
                            });
                        }
                        continue;
                    }
                    "Rebaseline" => {
                        // Two later snapshots to compare: Baseline1 after a slip, Baseline2 after another.
                        if let Some(doc) = lock(&self.ws).active_doc_mut() {
                            let _ = doc.try_edit("Rebaseline", ChangeSource::User, |p| {
                                use plancraft_core::baseline::Scope;
                                for n in 1..=2u8 {
                                    let ids: Vec<TaskId> = p
                                        .tasks
                                        .iter()
                                        .filter(|t| !t.sched.is_summary && !t.is_milestone())
                                        .map(|t| t.id)
                                        .step_by(3)
                                        .collect();
                                    for id in ids {
                                        let d = p.task(id).map_or(0, |t| t.duration);
                                        p.set_duration(id, d.saturating_add(d / 2), false)?;
                                    }
                                    plancraft_core::schedule::recalculate(p);
                                    p.set_baseline_in(n, &Scope::entire_project())?;
                                }
                                Ok::<(), PlanError>(())
                            });
                        }
                        continue;
                    }
                    // `GroupBy=<field>` and `SortBy=<field>[ desc]`, by column key or custom field name.
                    other => {
                        let field_key = |name: &str| {
                            let ws = lock(&self.ws);
                            let p = &ws.active_doc()?.project;
                            p.find_custom_field(name)
                                .map(|f| plancraft_core::custom::custom_column_key(f.id))
                                .or_else(|| crate::fields::Field::from_key(p, name).map(|f| f.key()))
                        };
                        if let Some(name) = other.strip_prefix("GroupBy=") {
                            Command::GroupBy(field_key(name))
                        } else if let Some(spec) = other.strip_prefix("SortBy=") {
                            let (name, desc) = spec.strip_suffix(" desc").map_or((spec, false), |n| (n, true));
                            Command::SortBy(field_key(name).map(|k| (k, desc)))
                        } else {
                            continue;
                        }
                    }
                };
                self.exec(ctx, cmd);
            }
        }
        let Some((path, frame)) = &mut self.shot else {
            return;
        };
        *frame += 1;
        let delay: u32 = std::env::var("PLANCRAFT_SHOT_DELAY").ok().and_then(|v| v.parse().ok()).unwrap_or(0);
        if *frame == 40 + delay * 60 {
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
        }
        let shot = ctx.input(|i| {
            i.events.iter().find_map(|e| if let egui::Event::Screenshot { image, .. } = e { Some(image.clone()) } else { None })
        });
        if let Some(img) = shot {
            let [w, h] = img.size;
            let bytes: Vec<u8> = img.pixels.iter().flat_map(|c| c.to_array()).collect();
            if let Some(buf) = image::RgbaImage::from_raw(w as u32, h as u32, bytes) {
                let _ = buf.save(&*path);
            }
            self.allow_exit = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        ctx.request_repaint();
    }

    fn welcome(&mut self, ui: &mut Ui, rect: Rect, out: &mut Vec<Command>) {
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect));
        child.vertical_centered(|ui| {
            ui.add_space(rect.height() * 0.3);
            ui.label(RichText::new("No project open").size(18.0).color(theme::pal().TEXT_WEAK));
            if ui.button("Create a new project").clicked() {
                out.push(Command::NewProject);
            }
            if ui.button("Open the sample project").clicked() {
                out.push(Command::NewDemo);
            }
        });
    }
}

impl PlanCraftApp {
    /// Clean exit: settings saved, the MCP server stopped and this session's recovery copies
    /// deleted (the user saved or chose to discard every change).
    fn shutdown(&mut self) {
        // Screenshot automation may override the theme; keep the user's settings as they were.
        if self.shot.is_none() {
            self.settings.table_width = self.st.table_width;
            self.settings.save();
        }
        self.mcp.stop();
        self.disk.stop();
        lock(&self.ws).release_locks();
        self.rec().forget_all();
    }

    /// Called after the frame guard caught a panic: secure the user's work first, then tell them.
    fn after_panic(&mut self, ctx: &egui::Context, message: &str) {
        let (any_dirty, saved) = {
            let ws = lock(&self.ws);
            (ws.docs.iter().any(|d| d.dirty), self.rec().write_dirty(&ws))
        };
        let message: String = match message.char_indices().nth(MAX_CRASH_CHARS) {
            Some((cut, _)) => format!("{}…", message.get(..cut).unwrap_or(message)),
            None => message.to_string(),
        };
        self.crash = Some(match (any_dirty, saved) {
            (true, Ok(_)) => {
                format!("PlanCraft hit an internal error; your unsaved work was saved as a recovery copy. Please report this: {message}")
            }
            (true, Err(e)) => format!("PlanCraft hit an internal error. {e}, so please save your work now. Please report this: {message}"),
            (false, _) => format!("PlanCraft hit an internal error; you had no unsaved changes. Please report this: {message}"),
        });
        ctx.request_repaint();
    }

    /// The non-modal notice shown after an internal error, until dismissed.
    fn crash_notice(&mut self, ctx: &egui::Context) {
        let Some(msg) = self.crash.clone() else { return };
        let mut dismiss = false;
        egui::Area::new(egui::Id::new("crash_notice")).order(egui::Order::Foreground).anchor(Align2::CENTER_TOP, vec2(0.0, 72.0)).show(
            ctx,
            |ui| {
                egui::Frame::popup(ui.style()).stroke(Stroke::new(1.5, theme::pal().ERROR)).inner_margin(egui::Margin::same(12)).show(
                    ui,
                    |ui| {
                        ui.set_max_width(560.0);
                        ui.horizontal_top(|ui| {
                            ui.label(RichText::new("✖").color(theme::pal().ERROR).size(18.0));
                            ui.vertical(|ui| {
                                ui.label(RichText::new(&msg).color(theme::pal().TEXT));
                                ui.add_space(6.0);
                                ui.horizontal(|ui| {
                                    if ui.button("Copy details").clicked() {
                                        ui.ctx().copy_text(msg.clone());
                                    }
                                    if ui.button("Dismiss").clicked() {
                                        dismiss = true;
                                    }
                                });
                            });
                        });
                    },
                );
            },
        );
        if dismiss {
            self.crash = None;
        }
    }

    /// Shows what opening a file clamped into range, once per document.
    fn report_load_notes(&mut self, ws: &mut Workspace) {
        for doc in &mut ws.docs {
            if doc.load_notes.is_empty() {
                continue;
            }
            let notes = std::mem::take(&mut doc.load_notes);
            const SHOWN: usize = 5;
            let mut list = notes.iter().take(SHOWN).cloned().collect::<Vec<_>>().join("; ");
            if notes.len() > SHOWN {
                list.push_str(&format!("; and {} more", notes.len() - SHOWN));
            }
            let msg = if doc.imported {
                let from = match &doc.imported_from {
                    Some(p) if plancraft_core::mspdi::is_mspdi_path(p) => "Microsoft Project XML",
                    _ => "a spreadsheet",
                };
                format!("Imported {} from {from}. Save it to keep it as a PlanCraft plan. Notes: {list}", doc.project.name)
            } else {
                format!("Opened {}; PlanCraft adjusted out-of-range values: {list}", doc.title())
            };
            self.st.toast(ToastKind::Warning, msg);
        }
    }

    /// Acts on what the file watcher found: a plan without unsaved changes reloads (undoably), a
    /// plan with some gets a bar to choose, a deleted file keeps the plan as unsaved work, and
    /// changed linked files refresh the plan's snapshots of them.
    fn apply_disk_changes(&mut self, ws: &mut Workspace) {
        // Bars that no longer apply: the plan was closed or saved elsewhere, or the version on
        // disk was reloaded or saved over (by the user or an agent).
        self.disk_bars.retain(|h, bar| {
            let Some(doc) = ws.index_of(*h).and_then(|i| ws.docs.get(i)) else { return false };
            let settled = match &bar.kind {
                DiskBarKind::Changed { stamp, .. } | DiskBarKind::Unreadable { stamp, .. } => stamp.is_some() && doc.disk_stamp() == *stamp,
                DiskBarKind::Missing { .. } => false,
            };
            doc.path == bar.path && !settled
        });
        for change in self.disk.take() {
            match change {
                plancraft_core::watch::Change::Modified { handle, stamp, read } => {
                    let Some(doc) = ws.index_of(handle).and_then(|i| ws.docs.get_mut(i)) else { continue };
                    // Saved or reloaded since the watcher looked.
                    if stamp.is_some() && doc.disk_stamp() == stamp {
                        continue;
                    }
                    let name = file_name_of(doc);
                    // Unsaved only because its file went missing: nothing of the user's to lose.
                    let clean = !doc.dirty
                        || matches!(self.disk_bars.get(&handle), Some(DiskBar { kind: DiskBarKind::Missing { revision, was_clean: true }, .. }) if *revision == doc.revision);
                    let path = doc.path.clone();
                    match *read {
                        Ok((theirs, notes)) if clean => match doc.reload_from(theirs, notes, stamp, ChangeSource::User) {
                            Ok(()) => {
                                self.disk_bars.remove(&handle);
                                let msg = format!("Reloaded {name}: it was changed on disk. Undo brings back the version you had open.");
                                self.set_status(msg.clone());
                                self.st.toast(ToastKind::Info, msg);
                            }
                            Err(e) => self.st.toast(ToastKind::Error, format!("{name} changed on disk but could not be reloaded: {e}")),
                        },
                        Ok((theirs, notes)) => {
                            self.st.toast(
                                ToastKind::Warning,
                                format!(
                                    "{name} was changed on disk by another program, and you have unsaved changes. Choose which to keep."
                                ),
                            );
                            let kind = DiskBarKind::Changed { theirs: Box::new(theirs), notes, stamp };
                            self.disk_bars.insert(handle, DiskBar { path, kind, compare: None });
                        }
                        Err(e) => {
                            let error = e.to_string();
                            self.st.toast(ToastKind::Error, format!("{name} changed on disk but can't be opened: {error}"));
                            let kind = DiskBarKind::Unreadable { error, stamp };
                            self.disk_bars.insert(handle, DiskBar { path, kind, compare: None });
                        }
                    }
                }
                plancraft_core::watch::Change::Missing { handle } => {
                    let Some(doc) = ws.index_of(handle).and_then(|i| ws.docs.get_mut(i)) else { continue };
                    let was_clean = !doc.dirty;
                    // The open plan is now the only copy: keep it as unsaved work (a recovery copy
                    // is written, closing asks to save, an agent may not discard it).
                    doc.dirty = true;
                    doc.user_changes = true;
                    let name = file_name_of(doc);
                    self.st.toast(ToastKind::Warning, format!("{name} was deleted or moved. The plan stays open as unsaved work."));
                    let kind = DiskBarKind::Missing { revision: doc.revision, was_clean };
                    self.disk_bars.insert(handle, DiskBar { path: doc.path.clone(), kind, compare: None });
                }
                plancraft_core::watch::Change::InSync { handle } => {
                    if let Some(DiskBar { kind: DiskBarKind::Missing { revision, was_clean: true }, .. }) = self.disk_bars.get(&handle)
                        && let Some(doc) = ws.index_of(handle).and_then(|i| ws.docs.get_mut(i))
                        && doc.revision == *revision
                    {
                        // The file came back as it was, and nothing changed the plan meanwhile.
                        doc.dirty = false;
                        doc.user_changes = false;
                    }
                    self.disk_bars.remove(&handle);
                }
                plancraft_core::watch::Change::Links { handle, names, reads } => {
                    let Some(doc) = ws.index_of(handle).and_then(|i| ws.docs.get_mut(i)) else { continue };
                    let warnings = doc.refresh_external_from(reads);
                    if warnings.is_empty() {
                        let names = names.iter().map(|n| format!("'{n}'")).collect::<Vec<_>>().join(", ");
                        self.set_status(format!("Updated the links to {names} in {}: changed on disk", doc.project.name));
                    }
                    for w in warnings {
                        self.st.toast(ToastKind::Warning, w);
                    }
                }
            }
        }
    }

    /// The bar above the active plan when its file changed on disk and PlanCraft needs the user
    /// to choose (non-blocking: the plan stays usable underneath).
    fn disk_bar(&mut self, ui: &mut Ui, ws: &Workspace) {
        let Some(doc) = ws.active_doc() else { return };
        let handle = doc.handle;
        let Some(bar) = self.disk_bars.get_mut(&handle) else {
            self.lock_bar(ui, doc);
            return;
        };
        let name = file_name_of(doc);
        let (message, buttons): (String, &[(DiskAction, &str, &str)]) = match &bar.kind {
            DiskBarKind::Changed { .. } => (
                format!("{name} was changed on disk by another program, and you have unsaved changes."),
                &[
                    (DiskAction::Reload, "Reload", "Load the file from disk, discarding your unsaved changes (Undo brings them back)"),
                    (DiskAction::KeepMine, "Keep Mine", "Keep your version; saving asks before replacing the file on disk"),
                    (DiskAction::Compare, "Compare", "List what differs between your version and the file on disk"),
                    (DiskAction::SaveAs, "Save As…", "Save your version under another name"),
                ],
            ),
            DiskBarKind::Unreadable { error, .. } => (
                format!("{name} changed on disk but can't be opened ({error}). Your plan is unchanged."),
                &[
                    (DiskAction::KeepMine, "Keep Mine", "Keep your version; saving asks before replacing the file on disk"),
                    (DiskAction::SaveAs, "Save As…", "Save your version under another name"),
                ],
            ),
            DiskBarKind::Missing { .. } => (
                format!("{name} was deleted or moved. Your plan is still open as unsaved work."),
                &[
                    (DiskAction::Save, "Save", "Write the plan to its file again"),
                    (DiskAction::SaveAs, "Save As…", "Save the plan under another name"),
                    (DiskAction::KeepMine, "Dismiss", "Hide this message; the plan stays unsaved"),
                ],
            ),
        };
        let mut clicked = None;
        egui::Frame::new()
            .fill(theme::pal().PANEL_BG)
            .stroke(Stroke::new(1.0, theme::pal().WARN))
            .inner_margin(egui::Margin::symmetric(10, 5))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new("⚠").color(theme::pal().WARN).size(15.0));
                    ui.label(RichText::new(message).color(theme::pal().TEXT));
                    for (action, label, hover) in buttons {
                        let selected = *action == DiskAction::Compare && bar.compare.is_some();
                        if ui.add(egui::Button::new(*label).selected(selected)).on_hover_text(*hover).clicked() {
                            clicked = Some(*action);
                        }
                    }
                });
                if let Some(lines) = &bar.compare {
                    ui.add_space(2.0);
                    if lines.is_empty() {
                        ui.label(
                            RichText::new("The file on disk holds the same tasks and resources as your version.")
                                .color(theme::pal().TEXT_WEAK),
                        );
                    }
                    for line in lines {
                        ui.label(RichText::new(format!("• {line}")).color(theme::pal().TEXT_WEAK));
                    }
                }
            });
        if clicked == Some(DiskAction::Compare) {
            bar.compare = match (&bar.compare, &bar.kind) {
                (None, DiskBarKind::Changed { theirs, .. }) => Some(plancraft_core::watch::compare(&doc.project, theirs)),
                _ => None,
            };
        } else if let Some(action) = clicked {
            self.disk_action = Some((handle, action));
        }
    }

    /// The bar above the active plan while it is open in another PlanCraft too (its lock file
    /// says so): changes saved there reach this window, but both editing at once means choosing
    /// one version later.
    fn lock_bar(&mut self, ui: &mut Ui, doc: &Document) {
        let Some(owner) = doc.lock.elsewhere() else { return };
        let key = (doc.handle, owner.instance.clone());
        if self.lock_dismissed.contains(&key) {
            return;
        }
        let message = format!(
            "{} is also open by {}. Changes saved there show up here; if you both edit it, you will have to choose one version.",
            file_name_of(doc),
            owner.describe()
        );
        let mut clicked = None;
        egui::Frame::new()
            .fill(theme::pal().PANEL_BG)
            .stroke(Stroke::new(1.0, theme::pal().WARN))
            .inner_margin(egui::Margin::symmetric(10, 5))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new("⚠").color(theme::pal().WARN).size(15.0));
                    ui.label(RichText::new(message).color(theme::pal().TEXT));
                    if ui.button("Save As…").on_hover_text("Work on a copy of the plan under another name").clicked() {
                        clicked = Some(DiskAction::SaveAs);
                    }
                    if ui.button("Dismiss").on_hover_text("Hide this message").clicked() {
                        clicked = Some(DiskAction::KeepMine);
                    }
                });
            });
        match clicked {
            Some(DiskAction::KeepMine) => {
                self.lock_dismissed.insert(key);
            }
            Some(action) => self.disk_action = Some((doc.handle, action)),
            None => {}
        }
    }

    /// Carries out the disk bar's button (outside the frame's workspace lock).
    fn disk_bar_action(&mut self) {
        let Some((handle, action)) = self.disk_action.take() else { return };
        match action {
            DiskAction::Compare => {}
            DiskAction::KeepMine => {
                if self.disk_bars.remove(&handle).is_some_and(|b| !matches!(b.kind, DiskBarKind::Missing { .. })) {
                    self.set_status("Kept your version. Saving asks before replacing the file on disk.");
                }
            }
            DiskAction::Reload => {
                let Some(DiskBar { kind: DiskBarKind::Changed { theirs, notes, stamp }, .. }) = self.disk_bars.remove(&handle) else {
                    return;
                };
                let r = {
                    let mut ws = lock(&self.ws);
                    ws.index_of(handle)
                        .and_then(|i| ws.docs.get_mut(i))
                        .map(|doc| (file_name_of(doc), doc.reload_from(*theirs, notes, stamp, ChangeSource::User)))
                };
                match r {
                    Some((name, Ok(()))) => self.set_status(format!("Reloaded {name} from disk. Undo brings back your changes.")),
                    Some((name, Err(e))) => self.st.toast(ToastKind::Error, format!("Could not reload {name}: {e}")),
                    None => {}
                }
            }
            DiskAction::Save | DiskAction::SaveAs => {
                {
                    let mut ws = lock(&self.ws);
                    let Some(i) = ws.index_of(handle) else { return };
                    ws.active = Some(i);
                }
                if self.save(action == DiskAction::SaveAs) {
                    self.disk_bars.remove(&handle);
                }
            }
        }
    }

    /// Everything drawn in one frame (run behind the frame guard).
    fn frame(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        theme::sync(&ctx);
        self.screenshot_hook(&ctx);
        if std::mem::take(&mut self.inject_panic) {
            injected_panic();
        }
        self.open_requested_files(&ctx);
        let mut out: Vec<Command> = Vec::new();

        // Intercept window close when there are unsaved changes.
        if ctx.input(|i| i.viewport().close_requested()) {
            let dirty = lock(&self.ws).docs.iter().any(|d| d.dirty);
            if dirty && !self.allow_exit {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                self.confirm_close = Some(CloseRequest::App);
            } else {
                self.shutdown();
            }
        }

        self.shortcuts(&ctx, &mut out);

        let ws_arc = self.ws.clone();
        let mut ws = lock(&ws_arc);
        self.fonts.frame(&ctx, &ws);

        // Keep recovery copies of unsaved work up to date.
        // (The background writer also ticks, so copies are made while no frames are drawn.)
        let error = {
            let mut r = self.rec();
            let _ = r.tick(&ws);
            r.last_error.clone()
        };
        match error {
            None => self.recovery_error = None,
            Some(e) => {
                if self.recovery_error.as_ref() != Some(&e) {
                    self.st.toast(ToastKind::Error, e.clone());
                }
                self.recovery_error = Some(e);
            }
        }
        self.apply_disk_changes(&mut ws);
        self.report_load_notes(&mut ws);
        // Plans opened from a server: other people's changes in, this person's edits out.
        for n in self.remotes.sync(&mut ws) {
            self.st.toast(ToastKind::Warning, n);
        }
        // Who else is on which task in the active plan (the task table marks them).
        self.st.others_on.clear();
        if let Some(r) = ws.active_doc().and_then(|d| self.remotes.get(d.handle)) {
            for p in r.others() {
                if let Some(t) = p.task {
                    let names = self.st.others_on.entry(t).or_default();
                    if !names.is_empty() {
                        names.push_str(", ");
                    }
                    names.push_str(&p.user);
                }
            }
        }

        // Notice changes made by AI agents through MCP.
        let mut agent_msgs = Vec::new();
        for d in &ws.docs {
            let prev = self.seen.insert(d.handle, d.revision);
            if let (Some(prev), Some((label, ChangeSource::Agent))) = (prev, &d.last_change)
                && prev != d.revision
            {
                agent_msgs.push(format!("⚡ AI agent: {label}"));
            }
        }
        for m in agent_msgs {
            self.set_status(m);
        }
        // Split Task waits for one click on a bar: Escape or leaving the Gantt views cancels it.
        if self.st.gantt.split_mode
            && (ctx.input(|i| i.key_pressed(egui::Key::Escape)) || !matches!(self.st.view, ViewKind::Gantt | ViewKind::TrackingGantt))
        {
            self.st.gantt.split_mode = false;
        }
        // A document opened or activated elsewhere (e.g. by an agent) becomes the view.
        let active = ws.active_doc().map(|d| d.handle);
        if active != self.last_active {
            if self.last_active.is_none() && active.is_some() {
                self.st.start_page = false;
            }
            self.st.sel = Default::default();
            self.st.gantt.initialized_for = None;
            self.st.gantt.split_mode = false;
            // The board's column window and drag belong to the plan they started in.
            self.st.board = Default::default();
            // An open cell editor belongs to the plan it was opened in: never commit it to this one.
            self.st.task_grid.reset_editing();
            self.st.resource_grid.reset_editing();
            self.st.misc_grid.reset_editing();
            // The Print window prints its plan as the view shows it; the view now shows another
            // plan, so it saves its Page Setup and closes.
            if let Some(mut b) = self.dialogs.print.take() {
                if Some(b.doc) == active {
                    self.dialogs.print = Some(b);
                } else if let Some(doc) = ws.index_of(b.doc).and_then(|i| ws.docs.get_mut(i)) {
                    b.dialog.commit_setup(doc);
                }
            }
            // Each document keeps its own sort and grouping.
            let st = &mut self.st;
            let leaving =
                ViewOrder { sort: st.sort.take(), group: st.group.take(), collapsed_groups: std::mem::take(&mut st.collapsed_groups) };
            if let Some(old) = self.last_active {
                st.view_orders.insert(old, leaving);
            }
            let open: HashSet<u64> = ws.docs.iter().map(|d| d.handle).collect();
            st.view_orders.retain(|h, _| open.contains(h));
            if let Some(order) = active.and_then(|h| st.view_orders.remove(&h)) {
                st.sort = order.sort;
                st.group = order.group;
                st.collapsed_groups = order.collapsed_groups;
            }
            self.last_active = active;
        }
        // Drop selections that no longer exist (e.g. deleted by an agent).
        if let Some(doc) = ws.active_doc() {
            let p = &doc.project;
            self.st.sel.tasks.retain(|id| p.task(*id).is_some());
            self.st.sel.resources.retain(|id| p.resource(*id).is_some());
            // Nor tasks the filter or search hides: commands on "the selected tasks" (Task
            // Information, dragging rows, clearing a field) would change tasks nobody can see.
            if (self.st.filter != crate::state::TaskFilter::All || !self.st.search.trim().is_empty()) && !self.st.sel.tasks.is_empty() {
                let shown: HashSet<TaskId> =
                    views::search_order(p, &mut self.st).into_iter().filter_map(|i| p.tasks.get(i).map(|t| t.id)).collect();
                self.st.sel.tasks.retain(|id| shown.contains(id));
            }
        }

        // Window title.
        let title = ws.active_doc().map(|d| format!("{} - PlanCraft", d.title())).unwrap_or_else(|| "PlanCraft".into());
        if title != self.last_title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.last_title = title;
        }

        let info = self.menu_info(&ws);
        let start_open = self.st.start_page;

        if start_open {
            let stats = ws.active_doc().map(|d| plancraft_core::report::project_stats(&d.project));
            let recovered = self.rec().found.clone();
            let sinfo = start_page::StartInfo {
                has_doc: info.has_doc,
                doc_title: ws.active_doc().map(|d| d.project.name.clone()).unwrap_or_default(),
                doc_path: ws.active_doc().and_then(|d| d.path.as_ref().map(|p| p.display().to_string())),
                stats,
                settings: &self.settings,
                recovered: &recovered,
            };
            egui::Panel::top("title").frame(egui::Frame::new().fill(theme::pal().BG)).show(ui, |ui| {
                menubar::title_bar(ui, &info, &mut out);
            });
            egui::CentralPanel::no_frame().show(ui, |ui| {
                if start_page::show(ui, &sinfo, &mut out) {
                    self.st.start_page = false;
                }
            });
        } else {
            egui::Panel::top("title").frame(egui::Frame::new().fill(theme::pal().BG)).show(ui, |ui| {
                menubar::title_bar(ui, &info, &mut out);
                menubar::command_bar(ui, &mut self.st, &info, &self.settings, &mut out);
                self.doc_tabs(ui, &mut ws, &mut out);
                self.disk_bar(ui, &ws);
                self.shared_bar(ui, &ws);
            });
            egui::Panel::bottom("status")
                .frame(egui::Frame::new().fill(theme::pal().HEADER_BG).inner_margin(egui::Margin::symmetric(10, 3)))
                .show(ui, |ui| {
                    self.status_bar(ui, &mut ws, &mut out);
                });
            egui::CentralPanel::no_frame().show(ui, |ui| {
                let full = ui.max_rect();
                ui.painter().rect_filled(full, 0.0, theme::pal().BG);
                let Some(doc) = ws.active_doc_mut() else {
                    self.welcome(ui, full, &mut out);
                    return;
                };
                let bar = Rect::from_min_size(full.min, vec2(22.0, full.height()));
                self.view_bar(ui, bar);
                let mut rect = Rect::from_min_max(pos2(full.left() + 22.0, full.top()), full.max);
                let st = &mut self.st;
                let show_timeline =
                    st.timeline_visible && matches!(st.view, ViewKind::Gantt | ViewKind::TrackingGantt | ViewKind::MultipleBaselines);
                if show_timeline {
                    let tl = Rect::from_min_size(rect.min, vec2(rect.width(), views::timeline::HEIGHT));
                    let chart_w = (rect.width() - st.table_width - 16.0).max(100.0);
                    let shown: Vec<bool> = doc.project.tasks.iter().map(|t| views::task_matches(&doc.project, st, t)).collect();
                    views::timeline::show(ui, tl, &doc.project, &shown, &mut st.gantt, chart_w);
                    rect = Rect::from_min_max(pos2(rect.left(), tl.bottom()), rect.max);
                }
                match st.view {
                    ViewKind::Gantt => views::gantt::show(ui, rect, doc, st, views::gantt_chart::ChartKind::Gantt, &mut out),
                    ViewKind::TrackingGantt => views::gantt::show(ui, rect, doc, st, views::gantt_chart::ChartKind::Tracking, &mut out),
                    ViewKind::Board => views::board::show(ui, rect, doc, st, &mut out),
                    ViewKind::MultipleBaselines => {
                        views::gantt::show(ui, rect, doc, st, views::gantt_chart::ChartKind::Baselines, &mut out)
                    }
                    ViewKind::NetworkDiagram => views::network::show(ui, rect, doc, st, &mut out),
                    ViewKind::TaskUsage => views::task_usage::show(ui, rect, doc, st, &mut out),
                    ViewKind::Milestones => views::milestones::show(ui, rect, doc, st, &mut out),
                    ViewKind::Calendar => views::calendar::show(ui, rect, doc, st, &mut out),
                    ViewKind::ResourceSheet => views::resource_sheet::show(ui, rect, doc, st, &mut out),
                    ViewKind::ResourceUsage => views::resource_usage::show(ui, rect, doc, st, &mut out),
                    ViewKind::Workload => views::workload::show(ui, rect, doc, st),
                    ViewKind::TeamPlanner => views::team_planner::show(ui, rect, doc, st, &mut out),
                    ViewKind::Timesheet => views::timesheet::show(ui, rect, doc, st),
                }
            });
            views::gantt::context_menu_popup(&ctx, &mut out);
            if let Some(doc) = ws.active_doc() {
                views::gantt::column_menu_popup(&ctx, &doc.project, &self.st, &mut out);
            }
        }

        // Dialogs. Editing dialogs stay bound to the document they were opened for.
        dialogs::show_bound(&mut self.dialogs.task_info, &mut ws, |d, doc| d.show(&ctx, doc));
        dialogs::show_bound(&mut self.dialogs.resource_info, &mut ws, |d, doc| d.show(&ctx, doc));
        dialogs::show_bound(&mut self.dialogs.project_info, &mut ws, |d, doc| d.show(&ctx, doc));
        dialogs::show_bound(&mut self.dialogs.working_time, &mut ws, |d, doc| d.show(&ctx, doc));
        dialogs::show_bound(&mut self.dialogs.custom_fields, &mut ws, |d, doc| d.show(&ctx, doc));
        // Windows that act on "Selected tasks" follow the selection while their plan is active.
        if let Some(doc) = ws.active_doc() {
            let sel = || views::selected_in_order(&doc.project, &self.st.sel);
            if let Some(b) = self.dialogs.update_project.as_mut().filter(|b| b.doc == doc.handle) {
                b.dialog.set_selection(sel());
            }
            if let Some(b) = self.dialogs.set_baseline.as_mut().filter(|b| b.doc == doc.handle) {
                b.dialog.set_selection(sel());
            }
            if let Some(b) = self.dialogs.clear_baseline.as_mut().filter(|b| b.doc == doc.handle) {
                b.dialog.set_selection(sel());
            }
        }
        {
            let st = &mut self.st;
            dialogs::show_bound(&mut self.dialogs.external, &mut ws, |d, doc| d.show(&ctx, doc, st));
            dialogs::show_bound(&mut self.dialogs.update_project, &mut ws, |d, doc| d.show(&ctx, doc, st));
            dialogs::show_bound(&mut self.dialogs.set_baseline, &mut ws, |d, doc| d.show(&ctx, doc, st));
            dialogs::show_bound(&mut self.dialogs.clear_baseline, &mut ws, |d, doc| d.show(&ctx, doc, st));
            dialogs::show_bound(&mut self.dialogs.recurring, &mut ws, |d, doc| d.show(&ctx, doc, st));
            dialogs::show_bound(&mut self.dialogs.find, &mut ws, |d, doc| d.show(&ctx, doc, st));
            dialogs::show_bound(&mut self.dialogs.goto, &mut ws, |d, doc| d.show(&ctx, doc, st));
            let st = &*st;
            dialogs::show_bound(&mut self.dialogs.print, &mut ws, |d, doc| d.show(&ctx, doc, st));
        }
        // Import Spreadsheet makes a new document.
        if let Some(dialog) = self.dialogs.import.first_mut() {
            match dialog.show(&ctx, &mut ws) {
                dialogs::import::Outcome::Open => {}
                dialogs::import::Outcome::Cancelled => {
                    self.dialogs.import.remove(0);
                }
                dialogs::import::Outcome::Imported(handle) => {
                    let path = dialog.path.clone();
                    let name = ws.index_of(handle).and_then(|i| ws.docs.get(i)).map(|d| d.project.name.clone()).unwrap_or_default();
                    self.dialogs.import.remove(0);
                    self.settings.add_recent(path);
                    self.st.start_page = false;
                    self.st.sel = Default::default();
                    self.st.gantt.initialized_for = None;
                    self.set_status(format!("Imported {name}. Save it to keep it as a PlanCraft plan."));
                }
            }
        }
        // Assign Resources works on the active document's selection: start afresh when the
        // active document changes, so typed units never carry over to another plan.
        let active = ws.active_doc().map(|d| d.handle);
        if let Some(b) = &mut self.dialogs.assign
            && let Some(h) = active
            && b.doc != h
        {
            *b = Bound { doc: h, dialog: Default::default() };
        }
        {
            let mut toasts = Vec::new();
            let sel = &self.st.sel;
            dialogs::show_bound(&mut self.dialogs.assign, &mut ws, |d, doc| d.show(&ctx, doc, sel, &mut toasts));
            for (k, t) in toasts {
                self.st.toast(k, t);
            }
        }
        if let Some(doc) = ws.active_doc_mut() {
            if self.dialogs.stats {
                dialogs::misc::statistics(&ctx, doc, &mut self.dialogs.stats);
            }
            if self.dialogs.history {
                dialogs::misc::history(&ctx, doc, &mut self.dialogs.history);
            }
        }
        drop(ws);
        if self.dialogs.about {
            dialogs::misc::about(&ctx, &mut self.dialogs.about);
        }
        if self.dialogs.mcp {
            let ws = self.ws.clone();
            dialogs::mcp_panel::show(&ctx, &mut self.dialogs.mcp, &mut self.mcp, &mut self.settings, &ws);
        }
        if let Some(d) = self.server_dialog.as_mut() {
            match d.show(&ctx, &mut self.settings) {
                dialogs::server::Outcome::Open => {}
                dialogs::server::Outcome::Closed => self.server_dialog = None,
                dialogs::server::Outcome::Opened(opened) => {
                    self.server_dialog = None;
                    self.add_remote(*opened);
                }
            }
        }
        self.open_dropped_version();
        if let Some(pdf) = self.dialogs.print.as_mut().and_then(|b| b.dialog.pending_pdf.take()) {
            self.save_pdf(pdf);
        }
        self.pick_external_project();
        self.external_links_reads(&ctx);
        self.refresh_links();
        self.disk_bar_action();
        self.confirm_dialog(&ctx);
        self.overwrite_dialog(&ctx);
        self.toasts(&ctx);

        for c in out {
            self.exec(&ctx, c);
        }
        // Edits the commands just made go out now, not at the next frame (which may be a while).
        let ws_arc = self.ws.clone();
        let mut ws = lock(&ws_arc);
        for n in self.remotes.sync(&mut ws) {
            self.st.toast(ToastKind::Warning, n);
        }
        let active = ws.active_doc().map(|d| d.handle);
        self.remotes.presence(active, self.st.sel.tasks.iter().next().copied());
    }

    /// Shows a plan just opened from a server (or the one already open, if it is).
    fn add_remote(&mut self, opened: dialogs::server::Opened) {
        let dialogs::server::Opened { opened, address, user, plan } = opened;
        let mut ws = lock(&self.ws);
        if let Some(h) = self.remotes.find(&ws, &address, &plan) {
            ws.active = ws.index_of(h);
            drop(ws);
            self.set_status(format!("{plan} from {address} is already open."));
            return;
        }
        let h = ws.add_shared(opened.project, &plan);
        self.remotes.add(h, crate::remote::Remote::new(opened.link, &address, &user, opened.role, opened.people));
        let _ = self.remotes.sync(&mut ws);
        drop(ws);
        self.st.start_page = false;
        self.st.sel = Default::default();
        self.st.gantt.initialized_for = None;
        let viewer = if opened.role.can_edit() { "" } else { " (view only)" };
        self.set_status(format!("Opened {plan} from {address}{viewer}. Every change is saved on the server."));
    }

    /// The bar above a shared plan: work of this person's the server's copy could not take.
    fn shared_bar(&mut self, ui: &mut Ui, ws: &Workspace) {
        let Some(handle) = ws.active_doc().map(|d| d.handle) else { return };
        let Some(r) = self.remotes.get(handle) else { return };
        let Some(dropped) = &r.dropped else { return };
        let mut open = false;
        let mut dismiss = false;
        egui::Frame::new()
            .fill(theme::pal().PANEL_BG)
            .stroke(Stroke::new(1.0, theme::pal().WARN))
            .inner_margin(egui::Margin::symmetric(10, 5))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new("⚠").color(theme::pal().WARN).size(15.0));
                    ui.label(RichText::new("Some of your changes were not kept:").color(theme::pal().TEXT));
                    open = ui
                        .button("Open Your Version")
                        .on_hover_text("Your plan just before the latest of these, as a new unsaved plan")
                        .clicked();
                    dismiss = ui.button("Dismiss").clicked();
                });
                for line in dropped.lines.iter().rev().take(5) {
                    ui.label(RichText::new(format!("• {line}")).color(theme::pal().TEXT_WEAK));
                }
                let more = dropped.lines.len().saturating_sub(5);
                if more > 0 {
                    ui.label(RichText::new(format!("… and {more} more")).color(theme::pal().TEXT_WEAK));
                }
            });
        if open {
            self.open_dropped = Some(handle);
        }
        if dismiss && let Some(r) = self.remotes.get_mut(handle) {
            r.dropped = None;
        }
    }

    /// Opens the person's version from before their work was dropped, as a new unsaved plan.
    fn open_dropped_version(&mut self) {
        let Some(handle) = self.open_dropped.take() else { return };
        if self.keep_dropped(handle).is_some() {
            self.set_status("Opened your version as a new plan. Save it to keep it.");
        }
    }

    /// Puts the work of this person's that the shared plan `handle` could not take into a new
    /// unsaved plan (made active). Returns its name, or `None` when there was none.
    fn keep_dropped(&mut self, handle: u64) -> Option<String> {
        let dropped = self.remotes.get_mut(handle).and_then(|r| r.dropped.take())?;
        let mut mine = *dropped.before;
        plancraft_core::sync::fit_counters(&mut mine);
        mine.name = format!("{} (my version)", mine.name);
        let name = mine.name.clone();
        let mut ws = lock(&self.ws);
        let h = ws.add(mine, None);
        if let Some(doc) = ws.index_of(h).and_then(|i| ws.docs.get_mut(i)) {
            // Unsaved work: closing it asks, and a recovery copy keeps it.
            doc.dirty = true;
            doc.user_changes = true;
        }
        drop(ws);
        self.st.sel = Default::default();
        self.st.gantt.initialized_for = None;
        Some(name)
    }

    /// Saves a copy of a plan opened from a server as a file of its own. Returns whether it was
    /// saved.
    fn save_copy(&mut self, handle: u64) -> bool {
        let Some(name) = self.copy_name(handle) else { return false };
        let Some(mut path) = rfd::FileDialog::new().add_filter("PlanCraft project", &[FILE_EXTENSION]).set_file_name(name).save_file()
        else {
            return false;
        };
        if path.extension().is_none() {
            path.set_extension(FILE_EXTENSION);
        }
        self.write_copy(handle, path)
    }

    /// The file name offered for a copy of plan `handle`.
    fn copy_name(&self, handle: u64) -> Option<String> {
        // One lock for the lookup: locking again while it is held would wait for ever.
        let ws = lock(&self.ws);
        ws.index_of(handle).and_then(|i| ws.docs.get(i)).map(|d| format!("{}.{FILE_EXTENSION}", d.title().trim_end_matches('*')))
    }

    /// Writes the copy of plan `handle` chosen in [`PlanCraftApp::save_copy`].
    fn write_copy(&mut self, handle: u64, path: PathBuf) -> bool {
        let r = {
            let ws = lock(&self.ws);
            match ws.index_of(handle).and_then(|i| ws.docs.get(i)) {
                Some(doc) => doc.save_copy(&path),
                None => return false,
            }
        };
        match r {
            Ok(()) => {
                self.settings.add_recent(path.clone());
                self.set_status(format!("Saved a copy to {}", path.display()));
                true
            }
            Err(e) => {
                self.st.toast(ToastKind::Error, e.to_string());
                false
            }
        }
    }
}

/// Developer/test hook behind `PLANCRAFT_CMD=Panic`: exercises the frame guard.
#[allow(clippy::panic)]
fn injected_panic() {
    panic!("test panic requested with PLANCRAFT_CMD=Panic");
}

impl eframe::App for PlanCraftApp {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        theme::pal().BG.to_normalized_gamma_f32()
    }

    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        // Last line of defence: a bug while drawing must never take the user's plans with it.
        if let Err(p) = plancraft_core::guard::catch("drawing the window", || self.frame(ui)) {
            self.after_panic(&ctx, &p.message);
        }
        if let Err(p) = plancraft_core::guard::catch("showing an error", || self.crash_notice(&ctx)) {
            eprintln!("plancraft: {p}");
        }
    }
}

#[cfg(test)]
mod ui_tests {
    use super::*;
    use egui_kittest::Harness;

    fn harness() -> Harness<'static, PlanCraftApp> {
        let mut ws = Workspace::default();
        ws.add(plancraft_core::sample::demo_project(plancraft_core::dates::ymd(2026, 10, 19).unwrap()), None);
        let shared = ws.shared();
        let mut h = Harness::builder().with_size(vec2(1440.0, 900.0)).build_eframe(move |cc| PlanCraftApp::new(cc, shared));
        h.run_steps(3);
        h
    }

    /// Regression: CSV export wrote over whatever file was chosen (the open plan included), and
    /// suggested names with path separators in them.
    #[test]
    fn exports_never_replace_another_kind_of_file() {
        let dir = crate::recovery::tests::TempDir::new("export");
        assert_eq!(file_stem("Q1/Q2: plan?"), "Q1-Q2- plan-");
        let plan = dir.0.join("Plan.plancraft");
        assert!(export_path(plan, "csv", "CSV").unwrap_err().contains(".csv"));
        assert_eq!(export_path(dir.0.join("tasks.CSV"), "csv", "CSV").unwrap(), dir.0.join("tasks.CSV"));
        assert_eq!(export_path(dir.0.join("tasks"), "csv", "CSV").unwrap(), dir.0.join("tasks.csv"));
        std::fs::write(dir.0.join("tasks.csv"), "kept").unwrap();
        assert!(export_path(dir.0.join("tasks"), "csv", "CSV").unwrap_err().contains("already exists"));
    }

    /// Regression: tasks hidden by a search stayed selected, so Task Information, dragging rows
    /// and clearing a cell changed them too.
    #[test]
    fn tasks_a_search_hides_leave_the_selection() {
        let mut h = harness();
        let (a, b, name) = {
            let ws = lock(&h.state().ws);
            let p = &ws.active_doc().unwrap().project;
            let leaves: Vec<&plancraft_core::model::Task> = p.tasks.iter().filter(|t| !t.sched.is_summary).take(2).collect();
            (leaves[0].id, leaves[1].id, leaves[0].name.clone())
        };
        h.state_mut().st.sel.tasks = [a, b].into_iter().collect();
        h.state_mut().st.search = name;
        h.run_steps(2);
        assert_eq!(h.state().st.sel.tasks.iter().copied().collect::<Vec<_>>(), vec![a], "only the task still shown stays selected");
    }

    fn focus_task_grid(h: &mut Harness<'static, PlanCraftApp>) {
        let id = egui::Id::new(("task_grid", "Entry")).with("body");
        h.ctx.memory_mut(|m| m.request_focus(id));
        h.run_steps(2);
    }

    fn tasks(h: &Harness<'static, PlanCraftApp>) -> Vec<String> {
        let ws = h.state().ws.lock().unwrap();
        ws.active_doc().unwrap().project.tasks.iter().map(|t| t.name.clone()).collect()
    }

    #[test]
    fn typing_into_blank_row_creates_task_and_undo_removes_it() {
        let mut h = harness();
        let n = tasks(&h).len();
        // Put the cursor on the first blank row, Task Name column, and type.
        h.state_mut().st.task_grid.cursor = Some((n, 2));
        focus_task_grid(&mut h);
        h.event(egui::Event::Text("Write release notes".into()));
        h.run_steps(2);
        h.key_press(egui::Key::Enter);
        h.run_steps(3);
        let names = tasks(&h);
        assert_eq!(names.len(), n + 1, "{names:?}");
        assert_eq!(names.last().unwrap(), "Write release notes");

        h.key_press_modifiers(Modifiers::COMMAND, egui::Key::Z);
        h.run_steps(3);
        assert_eq!(tasks(&h).len(), n);
    }

    #[test]
    fn insert_key_adds_task_and_edits_its_name() {
        let mut h = harness();
        let n = tasks(&h).len();
        let target = { h.state().ws.lock().unwrap().active_doc().unwrap().project.tasks[2].id };
        h.state_mut().st.sel.select_task(target);
        h.state_mut().st.task_grid.cursor = Some((2, 2));
        focus_task_grid(&mut h);
        h.key_press(egui::Key::Insert);
        h.run_steps(3);
        h.event(egui::Event::Text("Inserted step".into()));
        h.run_steps(2);
        h.key_press(egui::Key::Enter);
        h.run_steps(3);
        let names = tasks(&h);
        assert_eq!(names.len(), n + 1);
        assert_eq!(names[2], "Inserted step", "{names:?}");
    }

    /// Regression: the name editor of a new task used the default column positions, so with the
    /// Name column moved it opened on another column.
    #[test]
    fn insert_edits_the_name_column_wherever_it_is() {
        let mut h = harness();
        let ctx = h.ctx.clone();
        let name_at = |h: &Harness<'static, PlanCraftApp>| {
            let ws = h.state().ws.lock().unwrap();
            crate::fields::table_fields(&ws.active_doc().unwrap().project, crate::columns::TaskTable::Entry)
                .iter()
                .position(|f| *f == crate::fields::Field::Builtin(crate::columns::TaskCol::Name))
                .unwrap()
        };
        let at = name_at(&h);
        h.state_mut().exec(&ctx, Command::MoveColumn { at, right: false });
        assert_eq!(name_at(&h), at - 1);
        let target = { h.state().ws.lock().unwrap().active_doc().unwrap().project.tasks[2].id };
        h.state_mut().st.sel.select_task(target);
        h.state_mut().exec(&ctx, Command::InsertTask);
        h.run_steps(2);
        assert_eq!(h.state().st.task_grid.editing.as_ref().map(|e| e.col), Some(at - 1));
    }

    /// Regression: in a sorted view the grid cursor stayed on its row number when an edit
    /// re-sorted the rows, so the next keystroke edited a different task from the selected one.
    #[test]
    fn the_cursor_follows_its_task_when_an_edit_re_sorts_the_rows() {
        let mut h = harness();
        let ctx = h.ctx.clone();
        h.state_mut().exec(&ctx, Command::SortBy(Some(("duration".into(), false))));
        h.run_steps(2);
        let rows_now = |h: &Harness<'static, PlanCraftApp>| {
            let ws = h.state().ws.lock().unwrap();
            views::visible_rows(&ws.active_doc().unwrap().project, &h.state().st)
        };
        let rows = rows_now(&h);
        let (row, id) = {
            let ws = h.state().ws.lock().unwrap();
            let p = &ws.active_doc().unwrap().project;
            let row =
                rows.iter().position(|r| r.task().is_some_and(|i| !p.tasks[i].sched.is_summary && !p.tasks[i].is_milestone())).unwrap();
            (row, p.tasks[rows[row].task().unwrap()].id)
        };
        h.state_mut().st.sel.select_task(id);
        h.state_mut().st.task_grid.cursor = Some((row, 3));
        h.run_steps(2);
        // The edit makes it the longest task, so it sorts to another row.
        {
            let mut ws = h.state().ws.lock().unwrap();
            ws.active_doc_mut().unwrap().edit("Longer", |p| p.set_duration(id, 400 * 480, false)).unwrap().unwrap();
        }
        h.run_steps(3);
        let rows = rows_now(&h);
        let (now, _) = h.state().st.task_grid.cursor.unwrap();
        let ws = h.state().ws.lock().unwrap();
        let p = &ws.active_doc().unwrap().project;
        let pos = rows.iter().position(|r| r.task().is_some_and(|i| p.tasks[i].id == id));
        assert_ne!(now, row, "the task moved: was {row}, now at {pos:?}; rows {:?}", rows.iter().map(|r| r.task()).collect::<Vec<_>>());
        assert_eq!(rows[now].task().map(|i| p.tasks[i].id), Some(id), "the cursor is on the edited task");
    }

    /// Regression: sort and grouping were shared by every open plan, and a custom-field key names
    /// a different field in another plan.
    #[test]
    fn each_open_plan_keeps_its_own_sort_and_grouping() {
        let mut ws = Workspace::default();
        let a = ws.add(plancraft_core::sample::demo_project(plancraft_core::dates::ymd(2026, 10, 19).unwrap()), None);
        let b = ws.add(Project::new("Other plan", plancraft_core::dates::ymd(2026, 10, 19).unwrap()), None);
        ws.active = ws.index_of(a);
        let shared = ws.shared();
        let mut h = Harness::builder().with_size(vec2(1440.0, 900.0)).build_eframe(move |cc| PlanCraftApp::new(cc, shared));
        h.run_steps(3);
        let ctx = h.ctx.clone();
        h.state_mut().exec(&ctx, Command::SortBy(Some(("start".into(), true))));
        h.state_mut().exec(&ctx, Command::GroupBy(Some("custom:1".into())));
        let activate = |h: &mut Harness<'static, PlanCraftApp>, handle: u64| {
            {
                let mut ws = h.state().ws.lock().unwrap();
                ws.active = ws.index_of(handle);
            }
            h.run_steps(2);
        };
        activate(&mut h, b);
        assert_eq!((h.state().st.sort.clone(), h.state().st.group.clone()), (None, None), "the other plan starts unsorted");
        activate(&mut h, a);
        assert_eq!(h.state().st.sort, Some(("start".into(), true)));
        assert_eq!(h.state().st.group.as_deref(), Some("custom:1"));
    }

    /// Regression: arrowing onto a group header selected every task in the group, so a Delete
    /// pressed next removed them all.
    #[test]
    fn arrowing_onto_a_group_header_selects_nothing() {
        let mut h = harness();
        let ctx = h.ctx.clone();
        let status = { h.state().ws.lock().unwrap().active_doc().unwrap().project.find_custom_field("Status").unwrap().id };
        h.state_mut().exec(&ctx, Command::GroupBy(Some(plancraft_core::custom::custom_column_key(status))));
        h.run_steps(2);
        let first = {
            let ws = h.state().ws.lock().unwrap();
            let p = &ws.active_doc().unwrap().project;
            let rows = views::visible_rows(p, &h.state().st);
            assert!(matches!(rows[0], views::Row::Group(_)));
            p.tasks[rows[1].task().unwrap()].id
        };
        h.state_mut().st.sel.select_task(first);
        h.state_mut().st.task_grid.cursor = Some((1, 3));
        focus_task_grid(&mut h);
        h.key_press(egui::Key::ArrowUp);
        h.run_steps(2);
        assert_eq!(h.state().st.task_grid.cursor.map(|c| c.0), Some(0));
        assert!(h.state().st.sel.tasks.is_empty(), "{:?}", h.state().st.sel.tasks);
    }

    #[test]
    fn outline_and_link_shortcuts_work_while_grid_has_focus() {
        let mut h = harness();
        // Rows 17 and 18 ("Launch event", "Product live") under the Launch summary.
        let (a, b, level) = {
            let ws = h.state().ws.lock().unwrap();
            let p = &ws.active_doc().unwrap().project;
            (p.tasks[16].id, p.tasks[17].id, p.tasks[17].outline_level)
        };
        h.state_mut().st.sel.select_task(b);
        focus_task_grid(&mut h);
        h.key_press_modifiers(Modifiers::ALT | Modifiers::SHIFT, egui::Key::ArrowRight);
        h.run_steps(3);
        {
            let ws = h.state().ws.lock().unwrap();
            assert_eq!(ws.active_doc().unwrap().project.task(b).unwrap().outline_level, level + 1);
        }
        h.key_press_modifiers(Modifiers::ALT | Modifiers::SHIFT, egui::Key::ArrowLeft);
        h.run_steps(3);
        h.state_mut().st.sel.tasks = [a, b].into_iter().collect();
        h.key_press_modifiers(Modifiers::COMMAND, egui::Key::F2);
        h.run_steps(3);
        let ws = h.state().ws.lock().unwrap();
        let p = &ws.active_doc().unwrap().project;
        assert_eq!(p.task(b).unwrap().outline_level, level);
        assert!(p.task(b).unwrap().predecessors.iter().any(|d| d.predecessor == plancraft_core::model::TaskRef::Local(a)));
    }

    #[test]
    fn columns_can_be_inserted_hidden_and_moved_with_undo() {
        use crate::fields::{Field, table_fields};
        let mut h = harness();
        let ctx = h.ctx.clone();
        let (status, before) = {
            let ws = h.state().ws.lock().unwrap();
            let p = &ws.active_doc().unwrap().project;
            (p.find_custom_field("Status").unwrap().id, table_fields(p, crate::columns::TaskTable::Entry))
        };
        let key = Field::Custom(status).key();
        h.state_mut().exec(&ctx, Command::InsertColumn { at: 3, key });
        h.run_steps(2);
        let after = { table_fields(&h.state().ws.lock().unwrap().active_doc().unwrap().project, crate::columns::TaskTable::Entry) };
        assert_eq!(after.len(), before.len() + 1);
        assert_eq!(after[3], Field::Custom(status));
        h.state_mut().exec(&ctx, Command::MoveColumn { at: 3, right: false });
        h.state_mut().exec(&ctx, Command::HideColumn(0));
        h.run_steps(2);
        {
            let ws = h.state().ws.lock().unwrap();
            let doc = ws.active_doc().unwrap();
            let cols = table_fields(&doc.project, crate::columns::TaskTable::Entry);
            assert_eq!(cols[1], Field::Custom(status), "moved left, then the first column was hidden");
            assert!(doc.dirty, "column choices are saved with the plan");
        }
        for _ in 0..3 {
            h.state_mut().exec(&ctx, Command::Undo);
        }
        h.run_steps(2);
        let ws = h.state().ws.lock().unwrap();
        assert_eq!(table_fields(&ws.active_doc().unwrap().project, crate::columns::TaskTable::Entry), before);
    }

    #[test]
    fn print_window_previews_and_saves_page_setup_with_the_plan() {
        use egui_kittest::kittest::Queryable;
        let mut h = harness();
        h.key_press_modifiers(Modifiers::COMMAND, egui::Key::P);
        h.run_steps(3);
        assert!(h.state().dialogs.print.is_some(), "Ctrl+P opens the Print window");
        h.get_by_label("Portrait").click();
        h.run_steps(2);
        h.get_by_label("Close").click();
        h.run_steps(3);
        assert!(h.state().dialogs.print.is_none());
        let orientation =
            |h: &Harness<'static, PlanCraftApp>| h.state().ws.lock().unwrap().active_doc().unwrap().project.page_setup.orientation;
        assert_eq!(orientation(&h), plancraft_core::print::Orientation::Portrait, "saved with the plan");
        let ctx = h.ctx.clone();
        h.state_mut().exec(&ctx, Command::Undo);
        assert_eq!(orientation(&h), plancraft_core::print::Orientation::Landscape, "one undoable edit");
        // Every printable view renders in the preview.
        h.state_mut().exec(&ctx, Command::Print);
        for v in [ViewKind::ResourceSheet, ViewKind::TrackingGantt, ViewKind::Calendar] {
            h.state_mut().st.view = v;
            h.state_mut().exec(&ctx, Command::Print);
            h.run_steps(2);
        }
    }

    #[test]
    fn set_baseline_window_saves_numbered_baselines_interim_plans_and_clears_them() {
        use egui_kittest::kittest::Queryable;
        use plancraft_core::baseline::PlanSlot;
        let mut h = harness();
        let ctx = h.ctx.clone();
        let count = |h: &Harness<'static, PlanCraftApp>, slot: PlanSlot| {
            let ws = h.state().ws.lock().unwrap();
            ws.active_doc().unwrap().project.plan_info(slot).tasks
        };
        let all = h.state().ws.lock().unwrap().active_doc().unwrap().project.tasks.len();
        let label = |h: &Harness<'static, PlanCraftApp>, slot: PlanSlot| {
            crate::dialogs::baseline::slot_label(&h.state().ws.lock().unwrap().active_doc().unwrap().project, slot)
        };
        assert_eq!(count(&h, PlanSlot::Baseline(0)), all, "the demo plan has a Baseline");
        assert_eq!(count(&h, PlanSlot::Baseline(1)), 0);

        // Set baseline: pick Baseline1 from the list, entire project.
        h.state_mut().exec(&ctx, Command::SetBaseline);
        h.run_steps(2);
        h.get_by_value(&label(&h, PlanSlot::Baseline(0))).click();
        h.run_steps(2);
        h.get_by_label("Baseline1").click();
        h.run_steps(2);
        h.get_by_label("  OK  ").click();
        h.run_steps(3);
        assert!(h.state().dialogs.set_baseline.is_none(), "OK closes the window");
        assert_eq!(count(&h, PlanSlot::Baseline(1)), all);

        // Set interim plan: the current Start/Finish into Start1/Finish1 (the defaults).
        h.state_mut().exec(&ctx, Command::SetBaseline);
        h.run_steps(2);
        h.get_by_label("Set interim plan").click();
        h.run_steps(2);
        h.get_by_label("  OK  ").click();
        h.run_steps(3);
        assert_eq!(count(&h, PlanSlot::Interim(1)), all);

        // The new columns show the saved values.
        {
            let ws = h.state().ws.lock().unwrap();
            let p = &ws.active_doc().unwrap().project;
            let none = |_: TaskId| false;
            let ctx_cells = crate::columns::CellCtx { overallocated: &none, others_on: None };
            let col = crate::columns::TaskCol::Baseline(1, crate::columns::BlField::Finish);
            assert_eq!(crate::columns::cell(col, p, 0, &ctx_cells).text, crate::format::date(p.tasks[0].sched.finish));
            let interim = crate::columns::cell(crate::columns::TaskCol::Interim(1, false), p, 0, &ctx_cells).text;
            assert_eq!(interim, crate::format::date(p.tasks[0].sched.start));
        }

        // The Multiple Baselines Gantt and its print preview render.
        h.state_mut().exec(&ctx, Command::SetView(ViewKind::MultipleBaselines));
        h.run_steps(3);
        h.state_mut().exec(&ctx, Command::Print);
        h.run_steps(2);
        h.state_mut().dialogs.print = None;

        // Clear Baseline: Baseline1 from the entire project.
        h.state_mut().exec(&ctx, Command::ClearBaseline);
        h.run_steps(2);
        h.get_by_value(&label(&h, PlanSlot::Baseline(0))).click();
        h.run_steps(2);
        h.get_by_label(&label(&h, PlanSlot::Baseline(1))).click();
        h.run_steps(2);
        h.get_by_label("  OK  ").click();
        h.run_steps(3);
        assert!(h.state().dialogs.clear_baseline.is_none());
        assert_eq!(count(&h, PlanSlot::Baseline(1)), 0);
        assert_eq!(count(&h, PlanSlot::Baseline(0)), all, "Baseline is untouched");
        assert_eq!(count(&h, PlanSlot::Interim(1)), all);
        h.state_mut().exec(&ctx, Command::Undo);
        assert_eq!(count(&h, PlanSlot::Baseline(1)), all, "clearing is one undoable edit");

        // Regression: the window kept the selection it was opened with. Open it with nothing
        // selected, then select one task: "Selected tasks (1)" saves Baseline only for it, and
        // the roll-up options (left unticked) leave its summary alone.
        h.state_mut().st.sel = Default::default();
        h.state_mut().exec(&ctx, Command::SetBaseline);
        h.run_steps(2);
        let (leaf, summary) = {
            let ws = h.state().ws.lock().unwrap();
            let p = &ws.active_doc().unwrap().project;
            let i = p.tasks.iter().position(|t| !t.sched.is_summary).unwrap();
            (p.tasks[i].id, p.tasks[p.parent_index(i).unwrap()].id)
        };
        h.state_mut().st.sel.select_task(leaf);
        h.run_steps(2);
        h.get_by_value(&label(&h, PlanSlot::Baseline(0))).click();
        h.run_steps(2);
        h.get_by_label("Baseline3").click();
        h.run_steps(2);
        h.get_by_label("Selected tasks (1)").click();
        h.run_steps(1);
        h.get_by_label("  OK  ").click();
        h.run_steps(3);
        let ws = h.state().ws.lock().unwrap();
        let p = &ws.active_doc().unwrap().project;
        assert_eq!(p.plan_info(PlanSlot::Baseline(3)).tasks, 1);
        assert!(p.task(leaf).unwrap().baseline_in(3).is_some() && p.task(summary).unwrap().baseline_in(3).is_none());
    }

    #[test]
    fn recurring_tasks_and_split_tasks_from_the_window_and_the_chart() {
        use egui::accesskit::Role;
        use egui_kittest::kittest::Queryable;
        let mut h = harness();
        let ctx = h.ctx.clone();
        let count = |h: &Harness<'static, PlanCraftApp>| h.state().ws.lock().unwrap().active_doc().unwrap().project.tasks.len();
        let before = count(&h);

        // Task ▸ Recurring: name it, keep weekly on Mondays, 10 occurrences.
        h.state_mut().st.sel = Default::default();
        h.state_mut().exec(&ctx, Command::RecurringTask);
        h.run_steps(2);
        let name = h.query_all_by_role(Role::TextInput).next().unwrap();
        name.focus();
        name.type_text("Weekly sync");
        h.run_steps(2);
        h.get_by_label("  OK  ").click();
        h.run_steps(3);
        assert!(h.state().dialogs.recurring.is_none(), "OK closes the window");
        assert_eq!(count(&h), before + 11, "the recurring task and its 10 occurrences");
        let id = {
            let ws = h.state().ws.lock().unwrap();
            let p = &ws.active_doc().unwrap().project;
            let t = p.tasks.iter().find(|t| t.name == "Weekly sync").unwrap();
            assert!(t.recurrence.is_some());
            let row = p.task_index(t.id).unwrap();
            assert_eq!(p.tasks[row + 1].name, "Weekly sync 1");
            t.id
        };

        // Task Information on a recurring task opens the recurring task's window.
        h.state_mut().st.sel.select_task(id);
        h.state_mut().exec(&ctx, Command::TaskInfo);
        h.run_steps(2);
        assert!(h.state().dialogs.recurring.is_some() && h.state().dialogs.task_info.is_none());
        h.get_by_label("Monthly").click();
        h.run_steps(2);
        h.get_by_label("  OK  ").click();
        h.run_steps(3);
        assert!(matches!(
            h.state().ws.lock().unwrap().active_doc().unwrap().project.task(id).unwrap().recurrence.unwrap().pattern,
            plancraft_core::recurring::Pattern::Monthly { .. }
        ));

        // Split Task waits for one click; Escape or another view cancels it (regression: it
        // stayed on and split the next bar clicked, even in another plan or view).
        h.state_mut().exec(&ctx, Command::SplitTask);
        assert!(h.state().st.gantt.split_mode);
        h.key_press(egui::Key::Escape);
        h.run_steps(1);
        assert!(!h.state().st.gantt.split_mode, "Escape cancels it");
        h.state_mut().exec(&ctx, Command::SplitTask);
        h.state_mut().exec(&ctx, Command::SetView(ViewKind::ResourceSheet));
        h.run_steps(1);
        assert!(!h.state().st.gantt.split_mode, "another view cancels it");
        h.state_mut().exec(&ctx, Command::SetView(ViewKind::Gantt));
        // Split Task on the chart: a click splits the bar; dragging the later part back rejoins it.
        h.state_mut().exec(&ctx, Command::SplitTask);
        assert!(h.state().st.gantt.split_mode);
        let (task, at) = {
            let ws = h.state().ws.lock().unwrap();
            let p = &ws.active_doc().unwrap().project;
            let t = p.tasks.iter().find(|t| !t.sched.is_summary && t.duration >= 3 * 480 && t.percent_complete == 0).unwrap();
            (t.id, t.sched.start.unwrap() + SignedDuration::from_hours(2))
        };
        let parts = |h: &mut Harness<'static, PlanCraftApp>, action: views::gantt_chart::ChartAction| {
            let app = h.state_mut();
            let mut ws = app.ws.lock().unwrap();
            let doc = ws.active_doc_mut().unwrap();
            let rows = views::visible_rows(&doc.project, &app.st);
            views::gantt::apply_chart_actions(doc, &mut app.st, &rows, vec![action], &mut Vec::new());
            doc.project.task(task).unwrap().sched.parts.clone()
        };
        let split = parts(&mut h, views::gantt_chart::ChartAction::Split { task, at });
        assert_eq!(split.len(), 2, "split into two parts");
        let back = parts(&mut h, views::gantt_chart::ChartAction::MovePart { task, k: 1, start: split[0].1 });
        assert!(back.is_empty(), "moving the part back onto the first rejoins them: {back:?}");
        parts(&mut h, views::gantt_chart::ChartAction::Split { task, at });
        h.state_mut().st.sel.select_task(task);
        h.state_mut().exec(&ctx, Command::RejoinTasks);
        assert!(h.state().ws.lock().unwrap().active_doc().unwrap().project.task(task).unwrap().gaps.is_empty());
    }

    /// Regressions in the Print window: Ctrl+P again threw away unsaved Page Setup changes,
    /// closing after an Undo saved the undone setup again, and after a tab switch the window
    /// printed its plan with the other plan's view.
    #[test]
    fn the_print_window_keeps_changes_follows_undo_and_closes_on_tab_switch() {
        use egui_kittest::kittest::Queryable;
        use plancraft_core::print::Orientation;
        let mut ws = Workspace::default();
        let a = ws.add(plancraft_core::sample::demo_project(plancraft_core::dates::ymd(2026, 10, 19).unwrap()), None);
        let b = ws.add(Project::new("Other plan", plancraft_core::dates::ymd(2026, 10, 19).unwrap()), None);
        ws.active = ws.index_of(a);
        let shared = ws.shared();
        let mut h = Harness::builder().with_size(vec2(1440.0, 900.0)).build_eframe(move |cc| PlanCraftApp::new(cc, shared));
        h.run_steps(3);
        let orientation = |h: &Harness<'static, PlanCraftApp>, handle: u64| {
            let ws = h.state().ws.lock().unwrap();
            ws.docs[ws.index_of(handle).unwrap()].project.page_setup.orientation
        };
        let ctx = h.ctx.clone();

        h.state_mut().exec(&ctx, Command::Print);
        h.run_steps(2);
        h.get_by_label("Portrait").click();
        h.run_steps(2);
        h.state_mut().exec(&ctx, Command::Print);
        h.run_steps(2);
        h.get_by_label("Close").click();
        h.run_steps(3);
        assert_eq!(orientation(&h, a), Orientation::Portrait, "a second Ctrl+P kept the change");

        h.state_mut().exec(&ctx, Command::Print);
        h.run_steps(2);
        h.state_mut().exec(&ctx, Command::Undo);
        h.run_steps(2);
        h.get_by_label("Close").click();
        h.run_steps(3);
        assert_eq!(orientation(&h, a), Orientation::Landscape, "closing did not undo the Undo");

        h.state_mut().exec(&ctx, Command::Print);
        h.run_steps(2);
        h.get_by_label("Portrait").click();
        h.run_steps(2);
        {
            let mut ws = h.state().ws.lock().unwrap();
            ws.active = ws.index_of(b);
        }
        h.run_steps(3);
        assert!(h.state().dialogs.print.is_none(), "switching plans closes the window");
        assert_eq!(orientation(&h, a), Orientation::Portrait, "and keeps its plan's change");
        assert_eq!(orientation(&h, b), Orientation::Landscape);
    }

    /// The Task Board: a click selects a card, dragging cards to Done completes them (one undo
    /// step), a double-click opens Task Information, and the column menu edits a column.
    #[test]
    fn task_board_moves_cards_and_edits_columns() {
        use egui_kittest::kittest::Queryable;
        fn pointer(h: &mut Harness<'static, PlanCraftApp>, pos: egui::Pos2, button: Option<(egui::PointerButton, bool)>) {
            h.input_mut().events.push(egui::Event::PointerMoved(pos));
            if let Some((button, pressed)) = button {
                h.input_mut().events.push(egui::Event::PointerButton { pos, button, pressed, modifiers: egui::Modifiers::NONE });
            }
            h.run_steps(1);
        }
        let primary = |pressed| Some((egui::PointerButton::Primary, pressed));
        let mut h = harness();
        let ctx = h.ctx.clone();
        h.state_mut().exec(&ctx, Command::SetView(ViewKind::Board));
        h.run_steps(3);
        let (body, scroll) = (h.state().st.board.body, h.state().st.board.scroll);
        let first_card = |h: &Harness<'static, PlanCraftApp>| {
            let ws = h.state().ws.lock().unwrap();
            let p = &ws.active_doc().unwrap().project;
            let (_, rows) = p.board().into_iter().next().unwrap();
            p.tasks[rows[0]].id
        };
        let card = first_card(&h);

        // Click selects.
        let a = views::board::card_centre(body, scroll, 0, 0);
        pointer(&mut h, a, None);
        pointer(&mut h, a, primary(true));
        pointer(&mut h, a, primary(false));
        h.run_steps(2);
        assert_eq!(h.state().st.sel.single_task(), Some(card));

        // Drag it to Done: it is 100% complete, in one undoable step.
        h.run_steps(30);
        let done = views::board::card_centre(body, scroll, 3, 1);
        pointer(&mut h, a, primary(true));
        for k in 1..=10 {
            pointer(&mut h, a + (done - a) * (k as f32 / 10.0), None);
        }
        pointer(&mut h, done, primary(false));
        h.run_steps(2);
        let status = |h: &Harness<'static, PlanCraftApp>| {
            let ws = h.state().ws.lock().unwrap();
            let p = &ws.active_doc().unwrap().project;
            let i = p.task_index(card).unwrap();
            (p.board_column(p.board_column_of(i).unwrap()).unwrap().name.clone(), p.tasks[i].percent_complete)
        };
        assert_eq!(status(&h), ("Done".to_string(), 100));
        h.state_mut().exec(&ctx, Command::Undo);
        h.run_steps(2);
        assert_eq!(status(&h).0, "Not Started", "one undo puts it back");

        // Double-click opens Task Information.
        h.run_steps(30);
        for _ in 0..2 {
            let button = |pressed| egui::Event::PointerButton {
                pos: a,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            };
            h.input_mut().events.extend([button(true), button(false)]);
            h.run_steps(1);
        }
        h.run_steps(3);
        assert!(h.state().dialogs.task_info.is_some(), "double-click opens Task Information");
        h.state_mut().dialogs.task_info = None;
        h.run_steps(2);

        // Right-click a header: rename the column and give it a % complete.
        let head = views::board::header_centre(body, scroll, 1);
        pointer(&mut h, head, Some((egui::PointerButton::Secondary, true)));
        pointer(&mut h, head, Some((egui::PointerButton::Secondary, false)));
        h.run_steps(2);
        h.get_by_label_contains("Rename / Set % Complete").click();
        h.run_steps(2);
        assert!(h.state().st.board.edit.as_ref().is_some_and(|e| e.name == "Next up"));
        if let Some(e) = h.state_mut().st.board.edit.as_mut() {
            e.name = "Ready".into();
            e.percent = "10".into();
        }
        h.run_steps(1);
        h.get_by_label("  OK  ").click();
        h.run_steps(3);
        assert!(h.state().st.board.edit.is_none());
        {
            let ws = h.state().ws.lock().unwrap();
            let p = &ws.active_doc().unwrap().project;
            let c = &p.board_columns[1];
            assert_eq!((c.name.as_str(), c.percent_complete), ("Ready", Some(10)));
        }

        // Regression: dragging a selected card moved the whole selection, summaries and hidden
        // tasks included (refused, or silently completed). Only the cards shown move.
        let (summary, hidden) = {
            let mut ws = h.state().ws.lock().unwrap();
            let doc = ws.active_doc_mut().unwrap();
            let summary = doc.project.tasks.iter().find(|t| t.sched.is_summary).unwrap().id;
            let hidden = doc.project.tasks.iter().find(|t| !t.sched.is_summary && t.id != card).unwrap().id;
            doc.edit("Hide", |p| p.set_show_on_board(&[hidden], false)).unwrap().unwrap();
            (summary, hidden)
        };
        h.state_mut().st.sel.tasks = [card, summary, hidden].into_iter().collect();
        h.run_steps(30);
        let to = views::board::card_centre(body, scroll, 2, 1);
        pointer(&mut h, a, primary(true));
        for k in 1..=10 {
            pointer(&mut h, a + (to - a) * (k as f32 / 10.0), None);
        }
        pointer(&mut h, to, primary(false));
        h.run_steps(2);
        {
            let ws = h.state().ws.lock().unwrap();
            let p = &ws.active_doc().unwrap().project;
            let col = p.board_columns[2].id;
            assert_eq!(p.task(card).unwrap().board_status, Some(col), "the card moved");
            assert_ne!(p.task(hidden).unwrap().board_status, Some(col), "the hidden task stayed");
        }

        // Regression: the column window outlived a switch to another plan and then renamed that
        // plan's column with the same id.
        h.state_mut().st.board.edit = Some(crate::state::ColumnEdit::new_column());
        {
            let mut ws = h.state().ws.lock().unwrap();
            let other = ws.add(Project::new("Other plan", plancraft_core::dates::ymd(2026, 10, 19).unwrap()), None);
            ws.active = ws.index_of(other);
        }
        h.run_steps(2);
        assert!(h.state().st.board.edit.is_none(), "the window closes with its plan");
    }

    /// Regression: pasting a task copied from another plan (or from a deleted column) was
    /// refused, because its Board Status named a column this plan doesn't have.
    #[test]
    fn pasting_a_card_from_another_board_lands_in_the_first_column() {
        let mut h = harness();
        let ctx = h.ctx.clone();
        let mut t = Task::new(TaskId(999), "From elsewhere", 480);
        t.board_status = Some(plancraft_core::ids::BoardColumnId(77));
        h.state_mut().st.clipboard = vec![t];
        let before = h.state().ws.lock().unwrap().active_doc().unwrap().project.tasks.len();
        h.state_mut().exec(&ctx, Command::Paste);
        h.run_steps(2);
        let ws = h.state().ws.lock().unwrap();
        let p = &ws.active_doc().unwrap().project;
        assert_eq!(p.tasks.len(), before + 1, "pasted");
        let pasted = p.tasks.iter().find(|t| t.name == "From elsewhere").unwrap();
        assert_eq!(pasted.board_status, None);
    }

    /// The Network Diagram: a click selects a box, a double-click opens Task Information,
    /// dragging from one box to another links them, and zoom and the Format options apply.
    #[test]
    fn network_diagram_selects_opens_and_links_tasks() {
        fn pointer(h: &mut Harness<'static, PlanCraftApp>, pos: egui::Pos2, pressed: Option<bool>) {
            h.input_mut().events.push(egui::Event::PointerMoved(pos));
            if let Some(pressed) = pressed {
                h.input_mut().events.push(egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                });
            }
            h.run_steps(1);
        }
        let mut h = harness();
        let ctx = h.ctx.clone();
        h.state_mut().st.view = ViewKind::NetworkDiagram;
        h.state_mut().exec(&ctx, Command::ZoomEntire);
        h.run_steps(3);
        let task_named = |h: &Harness<'static, PlanCraftApp>, name: &str| {
            let ws = h.state().ws.lock().unwrap();
            let p = &ws.active_doc().unwrap().project;
            let i = p.tasks.iter().position(|t| t.name == name).unwrap();
            (i, p.tasks[i].id)
        };
        let centre_of = |h: &Harness<'static, PlanCraftApp>, idx: usize| {
            let n = &h.state().st.network;
            let d = n.cache.as_ref().unwrap().1.clone();
            let r = d.rect(d.node_of(idx).unwrap()).unwrap();
            let (x, y) = (r.x + r.w / 2.0, r.y + r.h / 2.0);
            egui::pos2(n.view_rect.left() + (x - n.offset.x) * n.zoom, n.view_rect.top() + (y - n.offset.y) * n.zoom)
        };
        let (scope_idx, scope) = task_named(&h, "Define scope and goals");
        let (infra_idx, infra) = task_named(&h, "Hosting and infrastructure");

        // Click selects.
        let a = centre_of(&h, scope_idx);
        pointer(&mut h, a, None);
        pointer(&mut h, a, Some(true));
        pointer(&mut h, a, Some(false));
        h.run_steps(2);
        assert_eq!(h.state().st.sel.single_task(), Some(scope));
        // Double-click opens Task Information.
        h.run_steps(30); // past the double-click window
        // (Each test frame is a quarter of a second: both clicks' presses and releases land in
        // two frames so they count as one double-click.)
        for _ in 0..2 {
            let button = |pressed| egui::Event::PointerButton {
                pos: a,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            };
            h.input_mut().events.extend([button(true), button(false)]);
            h.run_steps(1);
        }
        h.run_steps(3);
        assert!(h.state().dialogs.task_info.is_some(), "double-click opens Task Information");
        h.state_mut().dialogs.task_info = None;
        h.run_steps(2);

        // Drag from one box to another links them.
        let b = centre_of(&h, infra_idx);
        h.run_steps(30); // past the double-click window
        pointer(&mut h, a, Some(true));
        for k in 1..=10 {
            pointer(&mut h, a + (b - a) * (k as f32 / 10.0), None);
        }
        pointer(&mut h, b, Some(false));
        h.run_steps(2);
        {
            let ws = h.state().ws.lock().unwrap();
            let p = &ws.active_doc().unwrap().project;
            let t = p.task(infra).unwrap();
            assert!(t.predecessors.iter().any(|d| d.predecessor == TaskRef::Local(scope)), "{:?}", t.predecessors);
        }
        h.state_mut().exec(&ctx, Command::Undo);
        let linked = |h: &Harness<'static, PlanCraftApp>| {
            let ws = h.state().ws.lock().unwrap();
            ws.active_doc().unwrap().project.task(infra).unwrap().predecessors.iter().any(|d| d.predecessor == TaskRef::Local(scope))
        };
        assert!(!linked(&h), "one undoable edit");

        // Grouping applies to tables, not to the diagram (it kept a stale layout, then lost
        // every summary box on the next edit).
        let summaries = |h: &Harness<'static, PlanCraftApp>| {
            let d = h.state().st.network.cache.as_ref().unwrap().1.clone();
            let ws = h.state().ws.lock().unwrap();
            let p = &ws.active_doc().unwrap().project;
            d.nodes.iter().filter(|n| p.tasks[n.task].sched.is_summary).count()
        };
        let before = summaries(&h);
        assert!(before > 0);
        h.state_mut().st.group = Some("duration".into());
        h.state_mut().exec(&ctx, Command::LevelAll);
        h.run_steps(2);
        assert_eq!(summaries(&h), before);
        h.state_mut().st.group = None;

        // Zoom and the Format options.
        let z = h.state().st.network.zoom;
        h.state_mut().exec(&ctx, Command::ZoomIn);
        assert!(h.state().st.network.zoom > z);
        h.state_mut().st.network.opts.ids_only = true;
        h.state_mut().st.network.opts.critical_first = true;
        h.state_mut().st.network.link_labels = true;
        h.state_mut().st.network.show_summaries = false;
        h.state_mut().st.menu_tab = crate::state::MenuTab::Format;
        h.run_steps(3);
        let d = h.state().st.network.cache.as_ref().unwrap().1.clone();
        let ws = h.state().ws.lock().unwrap();
        let p = &ws.active_doc().unwrap().project;
        assert!(d.nodes.iter().all(|n| !p.tasks[n.task].sched.is_summary), "summaries hidden");
        assert!(d.geometry.box_w < 60.0, "IDs only: small boxes");
        let critical_on_top = d.nodes.iter().filter(|n| n.row == 0).all(|n| p.tasks[n.task].sched.critical);
        assert!(critical_on_top, "Critical First: the top row is the critical path");
    }

    #[test]
    fn update_project_window_updates_progress_then_reschedules() {
        use egui_kittest::kittest::Queryable;
        let mut h = harness();
        let ctx = h.ctx.clone();
        let complete = |h: &Harness<'static, PlanCraftApp>| {
            let ws = h.state().ws.lock().unwrap();
            ws.active_doc().unwrap().project.tasks.iter().filter(|t| t.percent_complete > 0).count()
        };
        {
            let mut ws = h.state().ws.lock().unwrap();
            let doc = ws.active_doc_mut().unwrap();
            doc.edit("Status date", |p| p.set_status_date(plancraft_core::dates::ymd(2026, 10, 30))).unwrap();
        }
        let before = complete(&h);
        // Defaults: update work as complete through the status date, as scheduled.
        h.state_mut().exec(&ctx, Command::UpdateProject);
        h.run_steps(2);
        h.get_by_label("  OK  ").click();
        h.run_steps(3);
        assert!(h.state().dialogs.update_project.is_none(), "OK closes the window");
        assert!(complete(&h) > before, "tasks scheduled before the status date have progress");

        // Then move the work not done by the status date after it.
        h.state_mut().exec(&ctx, Command::UpdateProject);
        h.run_steps(2);
        h.get_by_label("Reschedule uncompleted work to start after:").click();
        h.run_steps(1);
        h.get_by_label("  OK  ").click();
        h.run_steps(3);
        let ws = h.state().ws.lock().unwrap();
        let p = &ws.active_doc().unwrap().project;
        let after = plancraft_core::dates::ymd(2026, 10, 30).unwrap().and_hms_opt(17, 0, 0).unwrap();
        for t in p.tasks.iter().filter(|t| !t.sched.is_summary && t.active && !t.is_complete()) {
            let left_from = t.sched.resume.or(t.sched.start).unwrap();
            assert!(
                left_from >= after || t.mode == ScheduleMode::Manual || t.constraint.kind != ConstraintKind::Asap,
                "'{}' still has work before the status date ({left_from})",
                t.name
            );
        }
        drop(ws);
        // The Tracking table's new columns and split bars draw.
        h.state_mut().exec(&ctx, Command::Table(crate::columns::TaskTable::Tracking));
        h.run_steps(2);
    }

    #[test]
    fn grouped_and_sorted_views_render() {
        let mut h = harness();
        let ctx = h.ctx.clone();
        let status = { h.state().ws.lock().unwrap().active_doc().unwrap().project.find_custom_field("Status").unwrap().id };
        h.state_mut().exec(&ctx, Command::GroupBy(Some(plancraft_core::custom::custom_column_key(status))));
        h.state_mut().exec(&ctx, Command::SortBy(Some(("start".into(), true))));
        for v in [ViewKind::Gantt, ViewKind::TrackingGantt, ViewKind::TaskUsage] {
            h.state_mut().st.view = v;
            h.run_steps(2);
        }
        h.state_mut().exec(&ctx, Command::CustomFields);
        h.run_steps(2);
        assert!(h.state().dialogs.custom_fields.is_some());
    }

    /// Regression: dialogs applied to whichever document was active, and task ids repeat across
    /// documents, so OK after a tab switch wrote one plan's task into another.
    #[test]
    fn dialogs_stay_with_the_document_they_were_opened_for() {
        use egui_kittest::kittest::Queryable;
        let mut ws = Workspace::default();
        let a = ws.add(plancraft_core::sample::demo_project(plancraft_core::dates::ymd(2026, 10, 19).unwrap()), None);
        let mut other = Project::new("Other plan", plancraft_core::dates::ymd(2026, 10, 19).unwrap());
        for k in 0..30 {
            other.add_task(format!("Other {k}"), None);
        }
        let b = ws.add(other, None);
        ws.active = ws.index_of(a);
        let shared = ws.shared();
        let mut h = Harness::builder().with_size(vec2(1440.0, 900.0)).build_eframe(move |cc| PlanCraftApp::new(cc, shared));
        h.run_steps(3);

        let id = { h.state().ws.lock().unwrap().active_doc().unwrap().project.tasks[3].id };
        let before_b = {
            let ws = h.state().ws.lock().unwrap();
            ws.docs[ws.index_of(b).unwrap()].project.clone()
        };
        h.state_mut().st.sel.select_task(id);
        let ctx = h.ctx.clone();
        h.state_mut().exec(&ctx, Command::TaskInfo);
        h.run_steps(2);
        assert!(h.state().dialogs.task_info.is_some());

        // Switch to the other plan, then press OK on the open dialog.
        {
            let state = h.state_mut();
            let mut ws = state.ws.lock().unwrap();
            ws.active = ws.index_of(b);
        }
        h.run_steps(2);
        h.get_by_label_contains("OK").click();
        h.run_steps(3);
        let ws = h.state().ws.lock().unwrap();
        assert_eq!(ws.docs[ws.index_of(b).unwrap()].project.tasks, before_b.tasks, "the other plan is untouched");
        assert!(ws.docs[ws.index_of(b).unwrap()].history().is_empty());
        assert!(h.state().dialogs.task_info.is_none(), "OK applied to its own plan and closed");
        let own = ws.docs[ws.index_of(a).unwrap()].history();
        assert!(own.iter().any(|(l, _)| l.starts_with("Task Information")), "{own:?}");
    }

    /// macOS Finder hands files to the running app as events (`mac_open.rs`); the next frame opens
    /// them like File ▸ Open, and a file already open is only brought to the front.
    #[test]
    fn files_the_system_asks_to_open_are_opened_on_the_next_frame() {
        let tmp = crate::recovery::tests::TempDir::new("open-requests");
        let path = tmp.0.join("From Finder.plancraft");
        plancraft_core::io::save(&Project::new("From Finder", plancraft_core::dates::ymd(2026, 10, 19).unwrap()), &path).unwrap();
        let mut h = harness();
        // Tests never touch the app-wide inbox; each gets its own.
        let inbox = Arc::new(crate::open_requests::Inbox::default());
        inbox.wake_with(&h.ctx);
        h.state_mut().inbox = inbox.clone();
        let before = h.state().ws.lock().unwrap().docs.len();

        inbox.push([path.clone(), tmp.0.join("missing.plancraft")]);
        h.run_steps(2);
        {
            let ws = h.state().ws.lock().unwrap();
            assert_eq!(ws.docs.len(), before + 1);
            assert_eq!(ws.active_doc().unwrap().project.name, "From Finder");
        }
        assert!(h.state().st.toasts.iter().any(|t| t.text.contains("missing.plancraft")), "a file that can't be read is reported");

        h.state().ws.lock().unwrap().active = Some(0);
        inbox.push([path]);
        h.run_steps(2);
        let ws = h.state().ws.lock().unwrap();
        assert_eq!(ws.docs.len(), before + 1, "opening it again does not add a second copy");
        assert_eq!(ws.active_doc().unwrap().project.name, "From Finder");
    }

    /// Regression (review of the Finder support): launched by double-clicking a plan, the file
    /// closed the Start Page before it was drawn, so unsaved work from an earlier session was never
    /// mentioned.
    #[test]
    fn a_file_opened_at_launch_still_mentions_recovered_work() {
        let tmp = crate::recovery::tests::TempDir::new("open-request-recovery");
        let store = RecoveryStore::new(tmp.0.join("recovery"));
        {
            let mut ws = Workspace::default();
            ws.add(Project::new("Crashed Plan", plancraft_core::dates::ymd(2026, 10, 19).unwrap()), None);
            ws.docs[0].edit("Add", |p| p.add_task("Recovered task", None)).unwrap();
            Recovery::new(Some(store.clone())).write_dirty(&ws).unwrap();
        }
        let path = tmp.0.join("From Finder.plancraft");
        plancraft_core::io::save(&Project::new("From Finder", plancraft_core::dates::ymd(2026, 10, 19).unwrap()), &path).unwrap();
        let shared = Workspace::default().shared();
        let mut h = Harness::builder().with_size(vec2(1440.0, 900.0)).build_eframe(move |cc| PlanCraftApp::new(cc, shared));
        *h.state().rec() = Recovery::new(Some(store));
        let inbox = Arc::new(crate::open_requests::Inbox::default());
        h.state_mut().inbox = inbox.clone();
        assert!(h.state().st.start_page);

        inbox.push([path]);
        h.run_steps(2);
        assert!(!h.state().st.start_page);
        assert_eq!(h.state().ws.lock().unwrap().active_doc().unwrap().project.name, "From Finder");
        assert!(h.state().st.toasts.iter().any(|t| t.text == RECOVERY_FOUND), "recovered work is mentioned");
    }

    /// Regression: Save silently replaced a file another program had changed since it was opened.
    #[test]
    fn save_asks_before_overwriting_a_file_changed_on_disk() {
        use egui_kittest::kittest::Queryable;
        let tmp = crate::recovery::tests::TempDir::new("overwrite");
        let path = tmp.0.join("plan.plancraft");
        plancraft_core::io::save(&Project::new("Plan", plancraft_core::dates::ymd(2026, 10, 19).unwrap()), &path).unwrap();
        let mut ws = Workspace::default();
        ws.open(&path).unwrap();
        let shared = ws.shared();
        let mut h = Harness::builder().with_size(vec2(1440.0, 900.0)).build_eframe(move |cc| PlanCraftApp::new(cc, shared));
        h.run_steps(3);

        let mut other = plancraft_core::io::load(&path).unwrap();
        other.add_task("Written elsewhere", None);
        plancraft_core::io::save(&other, &path).unwrap();
        h.state().ws.lock().unwrap().active_doc_mut().unwrap().edit("Add", |p| p.add_task("Mine", None)).unwrap();

        let ctx = h.ctx.clone();
        h.state_mut().exec(&ctx, Command::Save);
        h.run_steps(2);
        assert!(h.state().confirm_overwrite.is_some(), "the user is asked");
        assert!(plancraft_core::io::load(&path).unwrap().tasks.iter().any(|t| t.name == "Written elsewhere"));

        h.get_by_label("Overwrite").click();
        h.run_steps(3);
        assert!(h.state().confirm_overwrite.is_none());
        let saved = plancraft_core::io::load(&path).unwrap();
        assert!(saved.tasks.iter().any(|t| t.name == "Mine"), "overwritten on request");
        assert!(!h.state().ws.lock().unwrap().active_doc().unwrap().dirty);
    }

    /// A plan opened from `path` in a fresh window.
    fn harness_on(path: &std::path::Path) -> Harness<'static, PlanCraftApp> {
        let mut ws = Workspace::default();
        ws.open(path).unwrap();
        harness_with(ws)
    }

    /// Lets the file watcher look (twice, as a change must settle) and draws what it found.
    fn watch(h: &mut Harness<'static, PlanCraftApp>) {
        let ws = h.state().ws.clone();
        h.state_mut().disk.poll_now(&ws);
        h.run_steps(2);
    }

    fn write_elsewhere(path: &std::path::Path, task: &str) {
        let mut other = plancraft_core::io::load(path).unwrap();
        other.add_task(task, None);
        plancraft_core::io::save(&other, path).unwrap();
    }

    fn active_names(h: &Harness<'static, PlanCraftApp>) -> Vec<String> {
        lock(&h.state().ws).active_doc().unwrap().project.tasks.iter().map(|t| t.name.clone()).collect()
    }

    /// A plan with no unsaved changes reloads by itself when its file changes on disk, keeping the
    /// selection and view; Undo brings back the version that was open.
    #[test]
    fn a_plan_changed_on_disk_reloads_itself_and_undo_brings_it_back() {
        let tmp = crate::recovery::tests::TempDir::new("reload");
        let path = tmp.0.join("plan.plancraft");
        let mut p = Project::new("Plan", plancraft_core::dates::ymd(2026, 10, 19).unwrap());
        let a = p.add_task("A", None);
        plancraft_core::io::save(&p, &path).unwrap();
        let mut h = harness_on(&path);
        h.state_mut().st.view = ViewKind::TaskUsage;
        h.state_mut().st.sel.tasks.insert(a);
        h.run_steps(2);
        watch(&mut h);
        assert_eq!(active_names(&h), ["A"], "nothing changed yet");

        write_elsewhere(&path, "Written elsewhere");
        watch(&mut h);
        assert_eq!(active_names(&h), ["A", "Written elsewhere"]);
        let st = &h.state().st;
        assert!(st.sel.tasks.contains(&a) && st.view == ViewKind::TaskUsage, "selection and view stay");
        assert!(st.toasts.iter().any(|t| t.text.starts_with("Reloaded plan.plancraft")));
        assert!(h.state().disk_bars.is_empty());
        {
            let ws = lock(&h.state().ws);
            let doc = ws.active_doc().unwrap();
            assert!(!doc.dirty);
            assert_eq!(doc.undo_label(), Some(plancraft_core::workspace::RELOAD_LABEL));
        }
        let ctx = h.ctx.clone();
        h.state_mut().exec(&ctx, Command::Undo);
        h.run_steps(2);
        assert_eq!(active_names(&h), ["A"]);
        assert!(lock(&h.state().ws).active_doc().unwrap().dirty, "the undone reload is unsaved");
    }

    /// With unsaved changes, a change on disk shows a bar instead: Compare lists the differences,
    /// Keep Mine leaves the next save to ask, Reload loads the file (undoably).
    #[test]
    fn unsaved_changes_and_a_change_on_disk_show_a_bar_to_choose() {
        use egui_kittest::kittest::Queryable;
        let tmp = crate::recovery::tests::TempDir::new("conflict");
        let path = tmp.0.join("plan.plancraft");
        plancraft_core::io::save(&Project::new("Plan", plancraft_core::dates::ymd(2026, 10, 19).unwrap()), &path).unwrap();
        let mut h = harness_on(&path);
        watch(&mut h);
        lock(&h.state().ws).active_doc_mut().unwrap().edit("Add", |p| p.add_task("Mine", None)).unwrap();

        write_elsewhere(&path, "Theirs");
        watch(&mut h);
        assert_eq!(active_names(&h), ["Mine"], "unsaved changes are never replaced unasked");
        assert!(matches!(h.state().disk_bars.values().next().map(|b| &b.kind), Some(DiskBarKind::Changed { .. })));
        h.get_by_label("Compare").click();
        h.run_steps(2);
        // Both sides added task 1, under different names.
        h.get_by_label_contains("1 task changed on disk: 'Theirs'");

        h.get_by_label("Keep Mine").click();
        h.run_steps(2);
        assert!(h.state().disk_bars.is_empty());
        let ctx = h.ctx.clone();
        h.state_mut().exec(&ctx, Command::Save);
        h.run_steps(2);
        assert!(h.state().confirm_overwrite.is_some(), "the next save asks before overwriting");
        h.get_by_label("Cancel").click();
        h.run_steps(2);
        watch(&mut h);
        assert!(h.state().disk_bars.is_empty(), "the version kept is not reported again");

        // Another change on disk asks again; Reload discards mine, and Undo brings it back.
        write_elsewhere(&path, "Theirs again");
        watch(&mut h);
        assert!(!h.state().disk_bars.is_empty());
        h.get_by_label("Reload").click();
        h.run_steps(2);
        assert_eq!(active_names(&h), ["Theirs", "Theirs again"]);
        assert!(h.state().disk_bars.is_empty());
        assert!(!lock(&h.state().ws).active_doc().unwrap().dirty);
        h.state_mut().exec(&ctx, Command::Undo);
        h.run_steps(2);
        assert_eq!(active_names(&h), ["Mine"]);
    }

    /// A deleted or moved file keeps the plan open as unsaved work; Save writes it again.
    #[test]
    fn a_deleted_file_keeps_the_plan_as_unsaved_work() {
        let tmp = crate::recovery::tests::TempDir::new("deleted");
        let path = tmp.0.join("plan.plancraft");
        plancraft_core::io::save(&Project::new("Plan", plancraft_core::dates::ymd(2026, 10, 19).unwrap()), &path).unwrap();
        let mut h = harness_on(&path);
        watch(&mut h);
        std::fs::remove_file(&path).unwrap();
        watch(&mut h);
        let handle = {
            let ws = lock(&h.state().ws);
            let doc = ws.active_doc().unwrap();
            assert!(doc.dirty && doc.user_changes, "kept as the user's unsaved work");
            doc.handle
        };
        assert!(matches!(h.state().disk_bars.get(&handle).map(|b| &b.kind), Some(DiskBarKind::Missing { .. })));
        assert_eq!(h.state().last_title, "plan* - PlanCraft");

        h.state_mut().disk_action = Some((handle, DiskAction::Save));
        h.run_steps(2);
        assert!(path.exists(), "written again");
        assert!(h.state().disk_bars.is_empty());
        assert!(!lock(&h.state().ws).active_doc().unwrap().dirty);
        watch(&mut h);
        assert!(h.state().disk_bars.is_empty());
    }

    /// A file deleted and put back unchanged (a sync tool, git) leaves the plan as it was.
    #[test]
    fn a_file_that_comes_back_unchanged_leaves_the_plan_saved() {
        let tmp = crate::recovery::tests::TempDir::new("comeback");
        let path = tmp.0.join("plan.plancraft");
        plancraft_core::io::save(&Project::new("Plan", plancraft_core::dates::ymd(2026, 10, 19).unwrap()), &path).unwrap();
        let mut h = harness_on(&path);
        watch(&mut h);
        let aside = tmp.0.join("aside.plancraft");
        std::fs::rename(&path, &aside).unwrap();
        watch(&mut h);
        assert!(lock(&h.state().ws).active_doc().unwrap().dirty);
        std::fs::rename(&aside, &path).unwrap();
        watch(&mut h);
        assert!(!lock(&h.state().ws).active_doc().unwrap().dirty);
        assert!(h.state().disk_bars.is_empty());
    }

    /// The middle of row `row`'s row number in the task table (where rows are dragged).
    fn row_number_at(h: &Harness<'static, PlanCraftApp>, row: usize) -> egui::Pos2 {
        let g = &h.state().st.task_grid;
        let body = g.body.unwrap();
        egui::pos2(body.left() + crate::widgets::grid::ROW_NUM_W / 2.0, body.top() + (row as f32 + 0.5) * theme::ROW_HEIGHT - g.scroll_y)
    }

    fn drag(h: &mut Harness<'static, PlanCraftApp>, from: egui::Pos2, to: egui::Pos2) {
        let button =
            |pos, pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Modifiers::NONE };
        h.event(egui::Event::PointerMoved(from));
        h.run_steps(1);
        h.event(button(from, true));
        h.run_steps(1);
        for k in 1..=8 {
            h.event(egui::Event::PointerMoved(from + (to - from) * (k as f32 / 8.0)));
            h.run_steps(1);
        }
        h.event(button(to, false));
        h.run_steps(3);
    }

    /// Dragging rows by their row number moves the selected tasks above the row they are dropped
    /// on, as one undo step; sorted, the rows can't be dragged.
    #[test]
    fn rows_dragged_by_their_number_move_the_tasks() {
        let mut h = harness();
        let before = tasks(&h);
        // Select rows 2 and 3 (two subtasks of the first summary), drag them below row 4.
        let picked: std::collections::BTreeSet<TaskId> = {
            let ws = lock(&h.state().ws);
            let p = &ws.active_doc().unwrap().project;
            [p.tasks[1].id, p.tasks[2].id].into_iter().collect()
        };
        h.state_mut().st.sel.tasks = picked;
        h.run_steps(2);
        let from = row_number_at(&h, 1);
        let to = row_number_at(&h, 3) + vec2(0.0, theme::ROW_HEIGHT / 2.0);
        drag(&mut h, from, to);
        let after = tasks(&h);
        let expected: Vec<String> = [&before[0], &before[3], &before[1], &before[2]].into_iter().chain(&before[4..]).cloned().collect();
        assert_eq!(after, expected);
        let ctx = h.ctx.clone();
        h.state_mut().exec(&ctx, Command::Undo);
        h.run_steps(2);
        assert_eq!(tasks(&h), before, "one undo puts them back");

        // Sorted rows have no outline order to drop into.
        h.state_mut().exec(&ctx, Command::SortBy(Some(("name".into(), false))));
        h.run_steps(2);
        let (from, to) = (row_number_at(&h, 1), row_number_at(&h, 5));
        drag(&mut h, from, to);
        assert_eq!(tasks(&h), before);
    }

    /// Regression: a row drag could not be cancelled, and one released over the chart still moved
    /// the rows (to the top or the end of the plan).
    #[test]
    fn a_row_drag_is_cancelled_by_escape_or_dropping_off_the_rows() {
        let mut h = harness();
        let before = tasks(&h);
        let button =
            |pos, pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Modifiers::NONE };
        let (from, to) = (row_number_at(&h, 1), row_number_at(&h, 4));
        h.event(egui::Event::PointerMoved(from));
        h.run_steps(1);
        h.event(button(from, true));
        h.run_steps(1);
        for k in 1..=8 {
            h.event(egui::Event::PointerMoved(from + (to - from) * (k as f32 / 8.0)));
            h.run_steps(1);
        }
        h.event(egui::Event::Key { key: egui::Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE });
        h.run_steps(1);
        h.event(button(to, false));
        h.run_steps(3);
        assert_eq!(tasks(&h), before, "Escape cancels the drag");
        // Released far to the right, over the chart.
        drag(&mut h, from, egui::pos2(1300.0, to.y));
        assert_eq!(tasks(&h), before, "dropped off the rows: nothing moves");
    }

    /// Fill Down copies the top selected cell into the selected cells below it, in one undo step.
    #[test]
    fn fill_down_copies_the_top_cell_into_the_selected_rows() {
        let mut h = harness();
        let ctx = h.ctx.clone();
        let table = h.state().st.table;
        let (ids, dur_col, finish_col, first_duration) = {
            let ws = lock(&h.state().ws);
            let p = &ws.active_doc().unwrap().project;
            let ids: Vec<TaskId> = p.tasks.iter().filter(|t| !t.sched.is_summary && !t.is_milestone()).take(3).map(|t| t.id).collect();
            let shown = crate::fields::table_fields(p, table);
            let col = |k: &str| shown.iter().position(|f| f.key() == k).unwrap();
            (ids.clone(), col("duration"), col("finish"), p.task(ids[0]).unwrap().duration)
        };
        h.state_mut().st.sel.tasks = ids.iter().copied().collect();
        h.state_mut().st.task_grid.cursor = Some((1, dur_col));
        h.state_mut().exec(&ctx, Command::FillDown);
        h.run_steps(2);
        {
            let ws = lock(&h.state().ws);
            let doc = ws.active_doc().unwrap();
            assert!(ids.iter().all(|id| doc.project.task(*id).unwrap().duration == first_duration));
            assert_eq!(doc.undo_label(), Some("Fill Down Duration"));
        }
        // Finish fills like a typed date; one row has nothing below it to fill.
        h.state_mut().st.task_grid.cursor = Some((1, finish_col));
        assert!(h.state_mut().with_doc(dialogs::find::fill_down).unwrap().is_ok());
        h.state_mut().st.sel.tasks = ids[..1].iter().copied().collect();
        h.state_mut().st.task_grid.cursor = Some((1, dur_col));
        let r = h.state_mut().with_doc(dialogs::find::fill_down).unwrap();
        assert!(r.is_err_and(|e| e.contains("Select the rows below")));
    }

    /// Find selects the next task whose cell matches, expanding a collapsed summary; Replace All
    /// replaces in every task as one undo step; Go To selects a task by its ID.
    #[test]
    fn find_replace_and_go_to() {
        use egui_kittest::kittest::Queryable;
        let mut h = harness();
        let ctx = h.ctx.clone();
        let (target, summary, n) = {
            let ws = lock(&h.state().ws);
            let p = &ws.active_doc().unwrap().project;
            let t = p.tasks.iter().find(|t| t.outline_level > 1).unwrap();
            let summary = p.tasks[p.parent_index(p.task_index(t.id).unwrap()).unwrap()].id;
            (t.clone(), summary, p.tasks.iter().filter(|t| t.name.to_lowercase().contains("design")).count())
        };
        h.state_mut().st.collapsed.insert(summary);
        h.state_mut().exec(&ctx, Command::Find(false));
        h.run_steps(2);
        h.state_mut().dialogs.find.as_mut().unwrap().dialog.find = target.name.to_uppercase();
        h.get_by_label("Find Next").click();
        h.run_steps(2);
        assert_eq!(h.state().st.sel.single_task(), Some(target.id));
        assert!(!h.state().st.collapsed.contains(&summary), "its summary was expanded");

        // Replace All, ignoring case, in the Task Name column.
        h.state_mut().exec(&ctx, Command::Find(true));
        h.run_steps(2);
        {
            let d = &mut h.state_mut().dialogs.find.as_mut().unwrap().dialog;
            d.find = "design".into();
            d.replace = "Layout".into();
        }
        h.get_by_label("Replace All").click();
        h.run_steps(2);
        assert!(n > 0);
        assert!(tasks(&h).iter().all(|t| !t.to_lowercase().contains("design")), "{:?}", tasks(&h));
        assert_eq!(lock(&h.state().ws).active_doc().unwrap().undo_label(), Some("Replace All"));
        h.get_by_label_contains("Replaced");
        h.state_mut().exec(&ctx, Command::Undo);
        h.run_steps(2);
        assert_eq!(tasks(&h).iter().filter(|t| t.to_lowercase().contains("design")).count(), n);

        // Go To row 5; a row that doesn't exist says so.
        h.state_mut().dialogs.find = None;
        h.state_mut().exec(&ctx, Command::GoTo);
        h.run_steps(2);
        h.state_mut().dialogs.goto.as_mut().unwrap().dialog.id = "5".into();
        h.get_by_label("Go").click();
        h.run_steps(2);
        let fifth = lock(&h.state().ws).active_doc().unwrap().project.tasks[4].id;
        assert_eq!(h.state().st.sel.single_task(), Some(fifth));
        assert!(h.state().dialogs.goto.is_none());
        h.state_mut().exec(&ctx, Command::GoTo);
        h.run_steps(2);
        h.state_mut().dialogs.goto.as_mut().unwrap().dialog.id = "999".into();
        h.get_by_label("Go").click();
        h.run_steps(2);
        h.get_by_label_contains("There is no task 999");
    }

    /// Several tasks selected: Task Information opens as Multiple Task Information.
    #[test]
    fn task_information_on_several_tasks_edits_them_all() {
        use egui_kittest::kittest::Queryable;
        let mut h = harness();
        let ctx = h.ctx.clone();
        let ids: Vec<TaskId> = {
            let ws = lock(&h.state().ws);
            ws.active_doc().unwrap().project.tasks.iter().filter(|t| !t.sched.is_summary).take(3).map(|t| t.id).collect()
        };
        h.state_mut().st.sel.tasks = ids.iter().copied().collect();
        h.state_mut().exec(&ctx, Command::TaskInfo);
        h.run_steps(2);
        h.get_by_label_contains("Multiple Task Information (3 tasks)");
    }

    /// A plan open in another PlanCraft opens with a bar saying who has it; Dismiss hides it.
    #[test]
    fn a_plan_open_in_another_plancraft_shows_who_has_it() {
        use egui_kittest::kittest::Queryable;
        let tmp = crate::recovery::tests::TempDir::new("locked");
        let path = tmp.0.join("plan.plancraft");
        plancraft_core::io::save(&Project::new("Plan", plancraft_core::dates::ymd(2026, 10, 19).unwrap()), &path).unwrap();
        let owner = plancraft_core::lock::Owner {
            user: "ana".into(),
            host: "studio-pc".into(),
            pid: 7,
            instance: "another-plancraft".into(),
            opened: "2026-11-18 09:15".into(),
            app: "PlanCraft 0.6.0".into(),
        };
        let lock_file = plancraft_core::lock::lock_path(&path).unwrap();
        std::fs::write(&lock_file, serde_json::to_vec(&owner).unwrap()).unwrap();
        let mut h = harness_on(&path);
        h.run_steps(2);
        h.get_by_label_contains("is also open by ana in PlanCraft 0.6.0 on studio-pc since 2026-11-18 09:15");
        h.get_by_label("Dismiss").click();
        h.run_steps(2);
        assert!(h.query_by_label_contains("is also open by ana").is_none());
        drop(h);
        assert!(lock_file.is_file(), "their lock is never removed");
    }

    /// A file replaced by garbage is reported, never loaded, and the plan stays as it was.
    #[test]
    fn a_file_replaced_by_garbage_is_reported_and_the_plan_kept() {
        let tmp = crate::recovery::tests::TempDir::new("garbage");
        let path = tmp.0.join("plan.plancraft");
        let mut p = Project::new("Plan", plancraft_core::dates::ymd(2026, 10, 19).unwrap());
        p.add_task("A", None);
        plancraft_core::io::save(&p, &path).unwrap();
        let mut h = harness_on(&path);
        watch(&mut h);
        std::fs::write(&path, [0xffu8, 0, 1, 2, b'{']).unwrap();
        watch(&mut h);
        assert_eq!(active_names(&h), ["A"]);
        assert!(matches!(h.state().disk_bars.values().next().map(|b| &b.kind), Some(DiskBarKind::Unreadable { .. })));
        assert!(h.state().st.toasts.iter().any(|t| t.kind == ToastKind::Error && t.text.contains("can't be opened")));
    }

    /// When a plan this one links to is saved (here, from the same window), the links refresh.
    #[test]
    fn links_refresh_when_a_linked_plan_changes_on_disk() {
        let tmp = crate::recovery::tests::TempDir::new("links");
        let day = plancraft_core::dates::ymd(2026, 10, 19).unwrap();
        let other_path = tmp.0.join("other.plancraft");
        let mut other = Project::new("Other", day);
        let api = other.add_task("API", None);
        plancraft_core::io::save(&other, &other_path).unwrap();
        let main_path = tmp.0.join("main.plancraft");
        let mut main = Project::new("Main", day);
        let ext = plancraft_core::io::add_external_project(&mut main, Some(&main_path), &other_path).unwrap();
        plancraft_core::io::ensure_external_task(&mut main, Some(&main_path), ext, api).unwrap();
        let mine = main.add_task("Mine", None);
        main.link(TaskRef::External { project: ext, task: api }, mine, LinkType::FS, Lag::None).unwrap();
        plancraft_core::io::save(&main, &main_path).unwrap();
        let mut ws = Workspace::default();
        ws.open(&other_path).unwrap();
        ws.open(&main_path).unwrap();
        let mut h = harness_with(ws);
        watch(&mut h);
        {
            let mut ws = lock(&h.state().ws);
            let doc = ws.docs.iter_mut().find(|d| d.project.name == "Other").unwrap();
            doc.edit("Rename", |p| p.task_mut(api).map(|t| t.name = "API v2".into())).unwrap();
            doc.save().unwrap();
        }
        watch(&mut h);
        let ws = lock(&h.state().ws);
        let main = ws.docs.iter().find(|d| d.project.name == "Main").unwrap();
        assert_eq!(main.project.external_projects[0].tasks[0].name, "API v2");
        assert!(!main.dirty);
        drop(ws);
        assert!(h.state().status.as_ref().is_some_and(|(m, _)| m.contains("Updated the links to 'Other'")));
    }

    /// Opening a spreadsheet shows the Import Spreadsheet window; Import makes an unsaved plan
    /// and reports what it left out.
    #[test]
    fn opening_a_spreadsheet_imports_it_through_the_import_window() {
        use egui_kittest::kittest::Queryable;
        let tmp = crate::recovery::tests::TempDir::new("csv-import");
        let path = tmp.0.join("Team plan.csv");
        std::fs::write(&path, "Task,Duration,Start,Predecessors,Owner,Budget\nDesign,3d,2026-10-19,,Ana,100\nBuild,5d,,1,Ben,200\n")
            .unwrap();
        let shared = Workspace::default().shared();
        let mut h = Harness::builder().with_size(vec2(1440.0, 900.0)).build_eframe(move |cc| PlanCraftApp::new(cc, shared));
        h.run_steps(2);
        let ctx = h.ctx.clone();
        h.state_mut().exec(&ctx, Command::OpenPath(path.clone()));
        h.run_steps(2);
        assert_eq!(h.state().dialogs.import.len(), 1, "the window opens instead of a failed load");
        assert!(h.state().ws.lock().unwrap().docs.is_empty(), "nothing is imported before Import");

        h.get_by_label("  Import  ").click();
        h.run_steps(2);
        assert!(h.state().dialogs.import.is_empty());
        {
            let ws = h.state().ws.clone();
            let ws = ws.lock().unwrap();
            let doc = ws.active_doc().unwrap();
            assert_eq!(doc.project.name, "Team plan");
            assert_eq!(doc.project.tasks.len(), 2);
            assert_eq!(doc.project.resources.len(), 2);
            assert!(doc.imported && doc.path.is_none() && doc.dirty);
        }
        assert!(!h.state().st.start_page);
        assert!(
            h.state().st.toasts.iter().any(|t| t.text.contains("from a spreadsheet") && t.text.contains("Budget")),
            "{:?}",
            h.state().st.toasts.iter().map(|t| &t.text).collect::<Vec<_>>()
        );
    }

    /// Regression (review): opening several spreadsheets at once kept only the last window.
    #[test]
    fn several_spreadsheets_opened_at_once_each_get_a_window() {
        use egui_kittest::kittest::Queryable;
        let tmp = crate::recovery::tests::TempDir::new("csv-queue");
        let (a, b) = (tmp.0.join("First.csv"), tmp.0.join("Second.csv"));
        std::fs::write(&a, "Task\nA\n").unwrap();
        std::fs::write(&b, "Task\nB\n").unwrap();
        let shared = Workspace::default().shared();
        let mut h = Harness::builder().with_size(vec2(1440.0, 900.0)).build_eframe(move |cc| PlanCraftApp::new(cc, shared));
        h.run_steps(2);
        let ctx = h.ctx.clone();
        h.state_mut().exec(&ctx, Command::OpenPath(a));
        h.state_mut().exec(&ctx, Command::OpenPath(b));
        h.run_steps(2);
        assert_eq!(h.state().dialogs.import.len(), 2);
        h.get_by_label("  Import  ").click();
        h.run_steps(2);
        h.get_by_label("  Import  ").click();
        h.run_steps(2);
        let names: Vec<String> = h.state().ws.lock().unwrap().docs.iter().map(|d| d.project.name.clone()).collect();
        assert_eq!(names, ["First", "Second"]);
    }

    /// Opening a Microsoft Project XML file imports it as a new plan: the user's unsaved work,
    /// with no file of its own, and a toast saying what the import could not carry over.
    #[test]
    fn opening_microsoft_project_xml_imports_it() {
        let tmp = crate::recovery::tests::TempDir::new("mspdi");
        let path = tmp.0.join("from project.xml");
        let mut plan = Project::new("Exchange", plancraft_core::dates::ymd(2026, 10, 19).unwrap());
        plan.add_task("Design", None);
        plancraft_core::mspdi::save(&plan, &path).unwrap();
        // A link to a task that is not in the file: the import leaves it out and says so.
        let xml = std::fs::read_to_string(&path)
            .unwrap()
            .replace("<Name>Design</Name>", "<Name>Design</Name><PredecessorLink><PredecessorUID>77</PredecessorUID></PredecessorLink>");
        std::fs::write(&path, xml).unwrap();
        let shared = Workspace::default().shared();
        let mut h = Harness::builder().with_size(vec2(1440.0, 900.0)).build_eframe(move |cc| PlanCraftApp::new(cc, shared));
        h.run_steps(2);
        let ctx = h.ctx.clone();
        h.state_mut().exec(&ctx, Command::OpenPath(path.clone()));
        h.run_steps(2);
        {
            let ws = h.state().ws.clone();
            let ws = ws.lock().unwrap();
            let doc = ws.active_doc().unwrap();
            assert_eq!(doc.project.tasks.len(), 1);
            assert!(doc.imported && doc.path.is_none() && doc.dirty && doc.user_changes);
        }
        assert!(h.state().st.toasts.iter().any(|t| t.text.contains("Imported Exchange from") && t.text.contains("not in the file")));
        // Opening it again shows the same unsaved import.
        h.state_mut().exec(&ctx, Command::OpenPath(path.clone()));
        h.run_steps(2);
        assert_eq!(h.state().ws.lock().unwrap().docs.len(), 1);
    }

    /// Regression: opening a cell editor and leaving with Enter used to commit the displayed text,
    /// which added a Start No Earlier Than constraint (dates show without their time) and drifted
    /// durations by display rounding, each with its own undo step.
    #[test]
    fn leaving_a_cell_unchanged_changes_nothing() {
        let mut h = harness();
        let before = { h.state().ws.lock().unwrap().active_doc().unwrap().project.tasks.clone() };
        let history = { h.state().ws.lock().unwrap().active_doc().unwrap().history().len() };
        // Entry table: 3 = Duration, 4 = Start, 5 = Finish.
        for col in [3, 4, 5] {
            h.state_mut().st.task_grid.cursor = Some((3, col));
            focus_task_grid(&mut h);
            h.key_press(egui::Key::F2);
            h.run_steps(2);
            assert!(h.state().st.task_grid.editing.is_some(), "F2 opens the editor in column {col}");
            h.key_press(egui::Key::Enter);
            h.run_steps(3);
        }
        let ws = h.state().ws.lock().unwrap();
        let doc = ws.active_doc().unwrap();
        assert_eq!(doc.history().len(), history, "no undo steps: {:?}", doc.history());
        for (a, b) in before.iter().zip(&doc.project.tasks) {
            assert_eq!((a.duration, a.constraint, a.manual_start), (b.duration, b.constraint, b.manual_start), "{}", a.name);
        }
    }

    /// The Entry table's column for a field.
    fn column_of(h: &Harness<'static, PlanCraftApp>, col: crate::columns::TaskCol) -> usize {
        let ws = h.state().ws.lock().unwrap();
        crate::fields::table_fields(&ws.active_doc().unwrap().project, crate::columns::TaskTable::Entry)
            .iter()
            .position(|f| *f == crate::fields::Field::Builtin(col))
            .unwrap()
    }

    /// The display row of a task (the demo plan opens unsorted and expanded).
    fn row_of(h: &Harness<'static, PlanCraftApp>, id: TaskId) -> usize {
        let ws = h.state().ws.lock().unwrap();
        let p = &ws.active_doc().unwrap().project;
        views::row_of_task(&views::visible_rows(p, &h.state().st), p.task_index(id).unwrap()).unwrap()
    }

    fn agent_edit<T>(h: &Harness<'static, PlanCraftApp>, f: impl FnOnce(&mut Project) -> PlanResult<T>) -> T {
        let mut ws = h.state().ws.lock().unwrap();
        ws.active_doc_mut().unwrap().try_edit("Agent", ChangeSource::Agent, f).unwrap()
    }

    fn leaf(h: &Harness<'static, PlanCraftApp>, n: usize) -> TaskId {
        let ws = h.state().ws.lock().unwrap();
        ws.active_doc().unwrap().project.tasks.iter().filter(|t| !t.sched.is_summary && !t.is_milestone()).nth(n).unwrap().id
    }

    fn name_of(h: &Harness<'static, PlanCraftApp>, id: TaskId) -> Option<String> {
        h.state().ws.lock().unwrap().active_doc().unwrap().project.task(id).map(|t| t.name.clone())
    }

    /// Regression: an open cell editor kept its row number, so when an agent added a task above
    /// it, the typing was saved into the task that moved onto that row.
    #[test]
    fn an_open_cell_edit_follows_its_task_when_an_agent_adds_a_row_above() {
        let mut h = harness();
        let id = leaf(&h, 3);
        let name = name_of(&h, id).unwrap();
        let col = column_of(&h, crate::columns::TaskCol::Name);
        let row = row_of(&h, id);
        h.state_mut().st.task_grid.cursor = Some((row, col));
        focus_task_grid(&mut h);
        h.key_press(egui::Key::F2);
        h.run_steps(2);
        let added = agent_edit(&h, |p| Ok(p.add_task("Added by the agent", Some(0))));
        h.run_steps(2);
        assert_eq!(h.state().st.task_grid.editing.as_ref().map(|e| e.row), Some(row + 1), "the editor moved with its task");
        h.event(egui::Event::Text(" (checked)".into()));
        h.run_steps(2);
        h.key_press(egui::Key::Enter);
        h.run_steps(3);
        assert_eq!(name_of(&h, id), Some(format!("{name} (checked)")));
        assert_eq!(name_of(&h, added).as_deref(), Some("Added by the agent"));
    }

    /// When the task being edited is deleted meanwhile, the edit is dropped (with a message)
    /// instead of landing on the next task.
    #[test]
    fn an_edit_of_a_task_an_agent_deletes_is_dropped() {
        let mut h = harness();
        let id = leaf(&h, 3);
        let next = leaf(&h, 4);
        let next_name = name_of(&h, next).unwrap();
        let col = column_of(&h, crate::columns::TaskCol::Name);
        h.state_mut().st.task_grid.cursor = Some((row_of(&h, id), col));
        focus_task_grid(&mut h);
        h.key_press(egui::Key::F2);
        h.run_steps(2);
        h.event(egui::Event::Text("X".into()));
        h.run_steps(2);
        agent_edit(&h, |p| p.delete_tasks(&[id]));
        h.run_steps(2);
        assert!(h.state().st.task_grid.editing.is_none());
        assert!(h.state().st.toasts.iter().any(|t| t.text.contains("was deleted")));
        h.key_press(egui::Key::Enter);
        h.run_steps(3);
        assert_eq!(name_of(&h, next), Some(next_name));
    }

    /// Typing into a blank row stays a new task when an agent adds a task at the end meanwhile.
    #[test]
    fn typing_into_a_blank_row_stays_new_when_an_agent_adds_a_task() {
        let mut h = harness();
        let n = tasks(&h).len();
        let col = column_of(&h, crate::columns::TaskCol::Name);
        h.state_mut().st.task_grid.cursor = Some((n, col));
        focus_task_grid(&mut h);
        h.event(egui::Event::Text("Typed by the user".into()));
        h.run_steps(2);
        let added = agent_edit(&h, |p| Ok(p.add_task("Added by the agent", None)));
        h.run_steps(2);
        h.key_press(egui::Key::Enter);
        h.run_steps(3);
        let names = tasks(&h);
        assert_eq!(names.len(), n + 2, "{names:?}");
        assert_eq!(name_of(&h, added).as_deref(), Some("Added by the agent"));
        assert_eq!(names.last().map(String::as_str), Some("Typed by the user"));
    }

    /// An agent switching the active plan closes an open cell editor instead of saving it into
    /// the other plan.
    #[test]
    fn switching_plans_drops_an_open_cell_edit() {
        let mut ws = Workspace::default();
        let a = ws.add(plancraft_core::sample::demo_project(plancraft_core::dates::ymd(2026, 10, 19).unwrap()), None);
        let mut other = Project::new("Other plan", plancraft_core::dates::ymd(2026, 10, 19).unwrap());
        for k in 0..30 {
            other.add_task(format!("Other {k}"), None);
        }
        let b = ws.add(other, None);
        ws.active = ws.index_of(a);
        let shared = ws.shared();
        let mut h = Harness::builder().with_size(vec2(1440.0, 900.0)).build_eframe(move |cc| PlanCraftApp::new(cc, shared));
        h.run_steps(3);
        let col = column_of(&h, crate::columns::TaskCol::Name);
        h.state_mut().st.task_grid.cursor = Some((3, col));
        focus_task_grid(&mut h);
        h.key_press(egui::Key::F2);
        h.run_steps(2);
        h.event(egui::Event::Text("X".into()));
        h.run_steps(1);
        {
            let state = h.state_mut();
            let mut ws = state.ws.lock().unwrap();
            ws.active = ws.index_of(b);
        }
        h.run_steps(2);
        h.key_press(egui::Key::Enter);
        h.run_steps(3);
        let ws = h.state().ws.lock().unwrap();
        assert!(ws.docs[ws.index_of(b).unwrap()].history().is_empty(), "nothing typed into the other plan");
        assert!(ws.docs[ws.index_of(a).unwrap()].history().is_empty());
    }

    /// The task on the grid cursor's row.
    fn task_at_cursor(h: &Harness<'static, PlanCraftApp>) -> Option<TaskId> {
        let (row, _) = h.state().st.task_grid.cursor?;
        let ws = h.state().ws.lock().unwrap();
        let p = &ws.active_doc().unwrap().project;
        views::task_at(p, &views::visible_rows(p, &h.state().st), row).map(|t| t.id)
    }

    /// Regression (review): every cell editor shares one widget id, and a new editor kept the last
    /// one's undo history, so Ctrl+Z in it brought back another task's name.
    #[test]
    fn undo_in_a_new_cell_editor_never_brings_back_another_cells_text() {
        let mut h = harness();
        let col = column_of(&h, crate::columns::TaskCol::Name);
        let (a, b) = (leaf(&h, 0), leaf(&h, 3));
        let b_name = name_of(&h, b).unwrap();
        h.state_mut().st.task_grid.cursor = Some((row_of(&h, a), col));
        focus_task_grid(&mut h);
        h.key_press(egui::Key::F2);
        h.run_steps(2);
        h.event(egui::Event::Text("zzz".into()));
        h.run_steps(2);
        h.key_press(egui::Key::Escape);
        h.run_steps(2);
        h.state_mut().st.task_grid.cursor = Some((row_of(&h, b), col));
        focus_task_grid(&mut h);
        h.key_press(egui::Key::F2);
        h.run_steps(2);
        h.key_press_modifiers(Modifiers::COMMAND, egui::Key::Z);
        h.run_steps(2);
        h.key_press(egui::Key::Enter);
        h.run_steps(3);
        assert_eq!(name_of(&h, b), Some(b_name));
    }

    /// Regression (found in review, also on main): the key that ended an edit was handled twice,
    /// so Enter moved two rows down and Tab two columns right.
    #[test]
    fn enter_and_tab_after_an_edit_move_one_cell() {
        let mut h = harness();
        let col = column_of(&h, crate::columns::TaskCol::Name);
        h.state_mut().st.task_grid.cursor = Some((2, col));
        focus_task_grid(&mut h);
        h.key_press(egui::Key::F2);
        h.run_steps(2);
        h.key_press(egui::Key::Enter);
        h.run_steps(3);
        assert_eq!(h.state().st.task_grid.cursor, Some((3, col)));
        h.key_press(egui::Key::F2);
        h.run_steps(2);
        h.key_press(egui::Key::Tab);
        h.run_steps(3);
        assert_eq!(h.state().st.task_grid.cursor, Some((3, col + 1)));
    }

    /// Regression (review): in a sorted view, typing a new task into a blank row and leaving it
    /// upwards put the cursor on whatever task sorted onto that row.
    #[test]
    fn leaving_a_typed_blank_row_keeps_the_cursor_on_the_right_task() {
        let mut h = harness();
        let ctx = h.ctx.clone();
        h.state_mut().exec(&ctx, Command::SortBy(Some(("name".into(), false))));
        h.run_steps(2);
        let n = tasks(&h).len();
        let col = column_of(&h, crate::columns::TaskCol::Name);

        // Up: the cursor stays on the task that was above.
        h.state_mut().st.task_grid.cursor = Some((n, col));
        let above = {
            let ws = h.state().ws.lock().unwrap();
            let p = &ws.active_doc().unwrap().project;
            views::task_at(p, &views::visible_rows(p, &h.state().st), n - 1).unwrap().id
        };
        focus_task_grid(&mut h);
        h.event(egui::Event::Text("Aaa first".into()));
        h.run_steps(2);
        h.key_press(egui::Key::ArrowUp);
        h.run_steps(3);
        assert_eq!(tasks(&h).len(), n + 1);
        assert_eq!(task_at_cursor(&h), Some(above));

        // Tab: the cursor stays on the new task, wherever it sorts.
        h.state_mut().st.task_grid.cursor = Some((n + 1, col));
        focus_task_grid(&mut h);
        h.event(egui::Event::Text("Aab second".into()));
        h.run_steps(2);
        h.key_press(egui::Key::Tab);
        h.run_steps(3);
        let added = task_at_cursor(&h).and_then(|id| name_of(&h, id));
        assert_eq!(added.as_deref(), Some("Aab second"));
    }

    /// Regression: Delete in any cell deleted the whole task. Now it clears the cell; the row
    /// number or the Task Name column deletes the task.
    #[test]
    fn delete_clears_a_cell_and_deletes_the_task_from_its_row_number() {
        let mut h = harness();
        let n = tasks(&h).len();
        let linked = {
            let ws = h.state().ws.lock().unwrap();
            ws.active_doc().unwrap().project.tasks.iter().find(|t| !t.predecessors.is_empty()).unwrap().id
        };
        let preds = column_of(&h, crate::columns::TaskCol::Predecessors);
        let duration = column_of(&h, crate::columns::TaskCol::Duration);
        let row = row_of(&h, linked);
        h.state_mut().st.sel.select_task(linked);

        h.state_mut().st.task_grid.cursor = Some((row, preds));
        focus_task_grid(&mut h);
        h.key_press(egui::Key::Delete);
        h.run_steps(3);
        assert_eq!(tasks(&h).len(), n, "a cell's Delete keeps the task");
        {
            let ws = h.state().ws.lock().unwrap();
            assert!(ws.active_doc().unwrap().project.task(linked).unwrap().predecessors.is_empty(), "Predecessors cleared");
        }

        // A duration can't be empty: a hint, nothing deleted.
        h.state_mut().st.task_grid.cursor = Some((row, duration));
        focus_task_grid(&mut h);
        h.key_press(egui::Key::Delete);
        h.run_steps(3);
        assert_eq!(tasks(&h).len(), n);
        assert!(h.state().st.toasts.iter().any(|t| t.text.contains("row number")));

        // The row number selects the whole row: Delete removes the task.
        h.state_mut().st.task_grid.whole_row = true;
        focus_task_grid(&mut h);
        h.key_press(egui::Key::Delete);
        h.run_steps(3);
        assert_eq!(tasks(&h).len(), n - 1);
        assert!(name_of(&h, linked).is_none());
    }

    /// Regression: the desktop backend delivers Ctrl+C/X/V as `Event::Copy/Cut/Paste` (no key
    /// event), so key-based shortcuts never fired and the clipboard keys did nothing.
    #[test]
    fn clipboard_events_copy_cut_and_paste_tasks() {
        let mut h = harness();
        let (id, name) = {
            let ws = h.state().ws.lock().unwrap();
            let t = &ws.active_doc().unwrap().project.tasks[3];
            (t.id, t.name.clone())
        };
        let n = tasks(&h).len();
        h.state_mut().st.sel.select_task(id);
        focus_task_grid(&mut h);
        h.event(egui::Event::Copy);
        h.run_steps(2);
        assert_eq!(h.state().st.clipboard.len(), 1);
        assert_eq!(h.state().st.clipboard[0].name, name);
        h.event(egui::Event::Paste(name.clone()));
        h.run_steps(3);
        assert_eq!(tasks(&h).len(), n + 1);
        assert_eq!(tasks(&h).iter().filter(|t| **t == name).count(), 2);

        let pasted = h.state().st.sel.tasks.iter().copied().next().unwrap();
        h.state_mut().st.sel.tasks = [pasted].into_iter().collect();
        h.event(egui::Event::Cut);
        h.run_steps(3);
        assert_eq!(tasks(&h).len(), n);
    }

    #[test]
    fn frame_guard_turns_a_panic_into_a_message_and_saves_unsaved_work() {
        let tmp = crate::recovery::tests::TempDir::new("frame-guard");
        let mut h = harness();
        *h.state().rec() = Recovery::new(Some(RecoveryStore::new(&tmp.0)));
        h.state().ws.lock().unwrap().active_doc_mut().unwrap().edit("Add", |p| p.add_task("Unsaved work", None)).unwrap();

        h.state_mut().inject_panic = true;
        h.run_steps(2);
        let msg = h.state().crash.clone().expect("the error is shown");
        assert!(msg.contains("saved as a recovery copy"), "{msg}");
        assert!(msg.contains("PLANCRAFT_CMD=Panic"), "{msg}");
        let copies = RecoveryStore::new(&tmp.0).list();
        assert_eq!(copies.len(), 1, "unsaved work was copied right away");

        // The app keeps running: frames draw and commands still work.
        h.run_steps(3);
        assert!(tasks(&h).iter().any(|t| t == "Unsaved work"));
        let ctx = h.ctx.clone();
        h.state_mut().exec(&ctx, Command::InsertTask);
        h.run_steps(2);
        assert!(tasks(&h).iter().any(|t| t == "<New Task>"));
        assert!(h.state().crash.is_some(), "the notice stays until dismissed");
    }

    #[test]
    fn recovered_work_restores_bound_to_its_file_and_can_be_discarded() {
        let tmp = crate::recovery::tests::TempDir::new("restore");
        let store = RecoveryStore::new(&tmp.0);
        // An earlier session left two copies behind.
        {
            let mut ws = Workspace::default();
            ws.add(
                Project::new("Crashed Plan", plancraft_core::dates::ymd(2026, 10, 19).unwrap()),
                Some(PathBuf::from("/plans/crashed.plancraft")),
            );
            ws.add(Project::new("Scratch", plancraft_core::dates::ymd(2026, 10, 19).unwrap()), None);
            for d in &mut ws.docs {
                d.edit("Add", |p| p.add_task("Recovered task", None)).unwrap();
            }
            Recovery::new(Some(store.clone())).write_dirty(&ws).unwrap();
        }
        let mut h = harness();
        *h.state().rec() = Recovery::new(Some(store.clone()));
        assert_eq!(h.state().rec().found.len(), 2);
        h.state_mut().st.start_page = true;
        h.run_steps(2);

        let crashed = h.state().rec().found.iter().find(|i| i.name == "Crashed Plan").unwrap().stem.clone();
        let ctx = h.ctx.clone();
        h.state_mut().exec(&ctx, Command::RestoreRecovered(crashed.clone()));
        h.run_steps(2);
        {
            let ws = h.state().ws.lock().unwrap();
            let doc = ws.active_doc().unwrap();
            assert_eq!(doc.project.name, "Crashed Plan");
            assert!(doc.dirty, "a restored copy is unsaved work");
            assert_eq!(doc.path.as_deref(), Some(std::path::Path::new("/plans/crashed.plancraft")));
            assert!(doc.project.tasks.iter().any(|t| t.name == "Recovered task"));
        }
        let left: Vec<String> = store.list().into_iter().map(|i| i.name).collect();
        assert_eq!(left.len(), 2, "the restored document has its own copy now: {left:?}");
        assert!(store.list().iter().all(|i| i.stem != crashed), "the old copy is gone");

        let scratch = h.state().rec().found.iter().find(|i| i.name == "Scratch").unwrap().stem.clone();
        let ctx = h.ctx.clone();
        h.state_mut().exec(&ctx, Command::DiscardRecovered(scratch.clone()));
        assert!(h.state().rec().found.is_empty());
        assert!(store.list().iter().all(|i| i.stem != scratch));
    }

    #[test]
    fn every_view_renders() {
        let mut h = harness();
        for v in ViewKind::ALL {
            h.state_mut().st.view = v;
            h.run_steps(2);
        }
        h.state_mut().st.start_page = true;
        h.run_steps(2);
    }

    /// Review (P2, app date arithmetic): views scrolled to the first or last representable day draw, scroll and zoom
    /// without overflowing a date (the Gantt Chart's non-working-day loop added a day past the
    /// last one and panicked).
    #[test]
    fn every_view_draws_at_the_ends_of_the_date_range() {
        use plancraft_core::dates::DateTime;
        let mut h = harness();
        let id = h.state().ws.lock().unwrap().active_doc().unwrap().project.id;
        for (left, day) in [(DateTime::MAX, Date::MAX), (DateTime::MIN, Date::MIN)] {
            for v in ViewKind::ALL {
                let st = &mut h.state_mut().st;
                st.view = v;
                st.gantt.left = left;
                st.gantt.initialized_for = Some(id);
                st.calendar_month = day.with_day(1).unwrap_or(day);
                st.timesheet_week = day;
                st.usage_from = Some(day);
                h.run_steps(2);
                // Scroll both ways, plain and with Shift (which pans the time axis), and zoom.
                for modifiers in [Modifiers::NONE, Modifiers::SHIFT, Modifiers::COMMAND] {
                    for delta in [vec2(-4000.0, -4000.0), vec2(4000.0, 4000.0)] {
                        h.event(egui::Event::PointerMoved(pos2(900.0, 500.0)));
                        h.event(egui::Event::MouseWheel {
                            unit: egui::MouseWheelUnit::Point,
                            delta,
                            modifiers,
                            phase: egui::TouchPhase::Move,
                        });
                        h.run_steps(1);
                    }
                }
                // The Calendar's month buttons stay on the first or last month.
                if v == ViewKind::Calendar {
                    use egui_kittest::kittest::Queryable;
                    for button in ["⏴", "⏵", "⏵", "⏴", "⏴"] {
                        h.get_by_label(button).click();
                        h.run_steps(1);
                    }
                    let month = h.state().st.calendar_month;
                    assert_eq!((month.year32(), month.day32()), (day.year32(), 1), "{month}");
                }
                assert!(h.state().crash.is_none(), "{v:?} at {left}: {:?}", h.state().crash);
            }
        }
    }

    fn harness_with(ws: Workspace) -> Harness<'static, PlanCraftApp> {
        let shared = ws.shared();
        let mut h = Harness::builder().with_size(vec2(1440.0, 900.0)).build_eframe(move |cc| PlanCraftApp::new(cc, shared));
        h.run_steps(3);
        h
    }

    fn day() -> Date {
        plancraft_core::dates::ymd(2026, 10, 19).unwrap()
    }

    /// Regression: the History button read a flag nothing ever set, so it never showed as pressed
    /// while the Undo History window was open.
    #[test]
    fn the_history_button_shows_whether_the_history_window_is_open() {
        let mut h = harness();
        let ctx = h.ctx.clone();
        let open = |h: &Harness<'static, PlanCraftApp>| {
            let ws = h.state().ws.clone();
            let ws = lock(&ws);
            h.state().menu_info(&ws).history_open
        };
        h.state_mut().st.menu_tab = crate::state::MenuTab::View;
        assert!(!open(&h));
        h.state_mut().exec(&ctx, Command::History);
        h.run_steps(2);
        assert!(h.state().dialogs.history && open(&h), "the button shows the window is open");
        h.state_mut().exec(&ctx, Command::History);
        h.run_steps(2);
        assert!(!open(&h));
    }

    /// Regression: Move Up/Down moved only the first selected task.
    #[test]
    fn move_up_and_down_move_every_selected_task() {
        let mut p = Project::new("Order", day());
        let ids: Vec<TaskId> = ["A", "B", "C", "D", "E"].into_iter().map(|n| p.add_task(n, None)).collect();
        let mut ws = Workspace::default();
        ws.add(p, None);
        let mut h = harness_with(ws);
        let ctx = h.ctx.clone();
        let select = |h: &mut Harness<'static, PlanCraftApp>, names: &[usize]| {
            h.state_mut().st.sel.tasks = names.iter().map(|&i| ids[i]).collect();
        };
        let order = |h: &Harness<'static, PlanCraftApp>| tasks(h).concat();

        select(&mut h, &[1, 3]);
        h.state_mut().exec(&ctx, Command::MoveUp);
        assert_eq!(order(&h), "BADCE");
        assert_eq!(h.state().st.sel.tasks, [ids[1], ids[3]].into_iter().collect(), "the selection stays on the moved tasks");
        h.state_mut().exec(&ctx, Command::MoveDown);
        assert_eq!(order(&h), "ABCDE");

        // Neighbours move together and keep their order.
        select(&mut h, &[1, 2]);
        h.state_mut().exec(&ctx, Command::MoveUp);
        assert_eq!(order(&h), "BCADE");
        h.state_mut().exec(&ctx, Command::MoveDown);
        h.state_mut().exec(&ctx, Command::MoveDown);
        assert_eq!(order(&h), "ADBCE");

        // A task at the edge of its group stops the whole move.
        select(&mut h, &[0, 2]);
        h.state_mut().exec(&ctx, Command::MoveUp);
        assert_eq!(order(&h), "ADBCE");
        assert!(h.state().st.toasts.iter().any(|t| t.text.contains("first")), "the edge is reported");
    }

    /// Regression: shortcuts acted on the plan hidden behind the Start Page or a save prompt.
    #[test]
    fn shortcuts_leave_a_plan_behind_the_start_page_or_a_save_prompt_alone() {
        let mut h = harness();
        let ctx = h.ctx.clone();
        h.state().ws.lock().unwrap().active_doc_mut().unwrap().edit("Add", |p| p.add_task("Mine", None)).unwrap();
        let id = leaf(&h, 2);
        h.state_mut().st.sel.select_task(id);
        h.state_mut().exec(&ctx, Command::Copy);
        let before = tasks(&h);
        let press = |h: &mut Harness<'static, PlanCraftApp>| {
            h.key_press_modifiers(Modifiers::COMMAND, Key::Z);
            h.run_steps(1);
            h.key_press_modifiers(Modifiers::COMMAND, Key::V);
            h.run_steps(1);
            h.event(egui::Event::Paste("x".into()));
            h.run_steps(1);
            h.key_press_modifiers(Modifiers::ALT | Modifiers::SHIFT, Key::ArrowUp);
            h.run_steps(2);
        };

        h.state_mut().st.start_page = true;
        h.run_steps(2);
        press(&mut h);
        assert_eq!(tasks(&h), before, "the plan behind the Start Page is unchanged");

        h.state_mut().st.start_page = false;
        h.state_mut().confirm_close = Some(CloseRequest::App);
        h.run_steps(1);
        press(&mut h);
        assert_eq!(tasks(&h), before, "the plan behind the save prompt is unchanged");
        // Regression (review): New still worked behind the save prompt (while quitting, say).
        let docs = h.state().ws.lock().unwrap().docs.len();
        h.key_press_modifiers(Modifiers::COMMAND, Key::N);
        h.run_steps(2);
        assert_eq!(h.state().ws.lock().unwrap().docs.len(), docs, "no new plan behind the save prompt");
        h.state_mut().confirm_close = None;

        // New still works from the Start Page.
        h.state_mut().st.start_page = true;
        h.run_steps(1);
        let docs = h.state().ws.lock().unwrap().docs.len();
        h.key_press_modifiers(Modifiers::COMMAND, Key::N);
        h.run_steps(2);
        assert_eq!(h.state().ws.lock().unwrap().docs.len(), docs + 1);
    }

    /// Regression (review): pasting X and Y (Y one level deeper, its own summary not copied) made
    /// Y a subtask of X but kept the X → Y link, which can't be made by hand; and two copied
    /// resources matching one in this plan gave a task two assignments to it.
    #[test]
    fn pasting_leaves_out_summary_to_subtask_links_and_duplicate_assignments() {
        let mut src = Project::new("Source", day());
        let x = src.add_task("X", None);
        let s = src.add_task("S", None);
        let y = src.add_task("Y", None);
        src.indent(y).unwrap();
        src.link(x.into(), y, LinkType::FS, Lag::None).unwrap();
        let ana = src.add_resource("Ana");
        let ana2 = src.add_resource("ana");
        src.assign(y, ana, Some(1.0), None).unwrap();
        src.assign(y, ana2, Some(1.0), None).unwrap();
        plancraft_core::schedule::recalculate(&mut src);
        let _ = s;
        let clip: Vec<Task> = [x, y].iter().map(|id| src.task(*id).unwrap().clone()).collect();
        let source = ClipSource::capture(&src, &clip);

        let mut dst = Project::new("Target", day());
        dst.add_resource("Ana");
        let (ids, notes) = super::paste_tasks(&mut dst, &clip, Some(&source), 0);
        let (nx, ny) = (ids[0], ids[1]);
        assert_eq!(dst.task(ny).unwrap().outline_level, 2, "Y is pasted under X");
        assert!(dst.task(ny).unwrap().predecessors.iter().all(|d| d.predecessor != TaskRef::Local(nx)), "no link from X to Y");
        assert!(notes.iter().any(|n| n.contains("own subtasks")), "{notes:?}");
        assert_eq!(dst.assignments.iter().filter(|a| a.task == ny).count(), 1, "one assignment to Ana");
        assert!(plancraft_core::limits::check_edit(&mut dst).is_ok());
    }

    /// Regression (second review): pasting back into the plan it came from lost a calendar or
    /// resource renamed since the copy (matched by name only). The same plan keeps them by id.
    #[test]
    fn pasting_into_the_same_plan_keeps_renamed_calendars_and_resources() {
        let mut p = Project::new("Plan", day());
        let t = p.add_task("T", None);
        let cal = p.add_calendar("Late crew", None).unwrap();
        p.task_mut(t).unwrap().calendar = Some(cal);
        let ana = p.add_resource("Ana");
        p.assign(t, ana, Some(1.0), None).unwrap();
        plancraft_core::schedule::recalculate(&mut p);
        let clip = vec![p.task(t).unwrap().clone()];
        let source = ClipSource::capture(&p, &clip);
        p.calendars.iter_mut().find(|c| c.id == cal).unwrap().name = "Night crew".into();
        p.resource_mut(ana).unwrap().name = "Ana Lima".into();
        let at = p.tasks.len();
        let (ids, notes) = super::paste_tasks(&mut p, &clip, Some(&source), at);
        assert_eq!(p.task(ids[0]).unwrap().calendar, Some(cal), "{notes:?}");
        assert!(p.assignments.iter().any(|a| a.task == ids[0] && a.resource == ana), "{notes:?}");
        assert!(notes.is_empty(), "{notes:?}");
    }

    /// Regression (second review): a loop through a pasted summary survived the paste (only a
    /// warning on recalculation), though linking it by hand is refused.
    #[test]
    fn pasting_leaves_out_a_loop_through_the_new_outline() {
        let mut src = Project::new("Source", day());
        let x = src.add_task("X", None);
        let _s = src.add_task("S", None);
        let y = src.add_task("Y", None);
        src.indent(y).unwrap();
        let z = src.add_task("Z", None);
        src.outdent(z).ok();
        src.link(y.into(), z, LinkType::FS, Lag::None).unwrap();
        src.link(z.into(), x, LinkType::FS, Lag::None).unwrap();
        plancraft_core::schedule::recalculate(&mut src);
        let clip: Vec<Task> = [x, y, z].iter().map(|id| src.task(*id).unwrap().clone()).collect();
        let source = ClipSource::capture(&src, &clip);
        let mut dst = Project::new("Target", day());
        let (ids, notes) = super::paste_tasks(&mut dst, &clip, Some(&source), 0);
        assert_eq!(dst.task(ids[1]).unwrap().outline_level, 2, "Y is pasted under X");
        let report = plancraft_core::schedule::recalculate(&mut dst);
        assert!(!report.warnings.iter().any(|w| w.contains("ircular")), "{:?}", report.warnings);
        assert!(notes.iter().any(|n| n.contains("left out")), "{notes:?}");
    }

    /// Regression: pasted tasks lost their links and milestone memberships, even to each other,
    /// and their resource assignments.
    #[test]
    fn pasting_keeps_links_members_and_assignments_among_the_pasted_tasks() {
        let mut p = Project::new("Plan", day());
        let before = p.add_task("Before", None);
        let a = p.add_task("A", None);
        let b = p.add_task("B", None);
        let m = p.add_milestone("M", None);
        p.link(before.into(), a, LinkType::FS, Lag::None).unwrap();
        p.link(a.into(), b, LinkType::SS, Lag::None).unwrap();
        p.add_milestone_members(m, &[TaskRef::Local(a), TaskRef::Local(before)]).unwrap();
        let ana = p.add_resource("Ana");
        p.assign(b, ana, Some(0.5), None).unwrap();
        recalculate(&mut p);
        let mut ws = Workspace::default();
        ws.add(p, None);
        let mut h = harness_with(ws);
        let ctx = h.ctx.clone();
        h.state_mut().st.sel.tasks = [a, b, m].into_iter().collect();
        h.state_mut().exec(&ctx, Command::Copy);
        h.state_mut().st.sel = Default::default();
        h.state_mut().exec(&ctx, Command::Paste);
        h.run_steps(2);

        let ws = h.state().ws.lock().unwrap();
        let p = &ws.active_doc().unwrap().project;
        let [na, nb, nm] = [-3, -2, -1].map(|k: isize| &p.tasks[(p.tasks.len() as isize + k) as usize]);
        assert_eq!((na.name.as_str(), nb.name.as_str(), nm.name.as_str()), ("A", "B", "M"));
        assert!(na.predecessors.is_empty(), "the link from a task left behind is not pasted");
        assert_eq!(nb.predecessors.len(), 1);
        assert_eq!(nb.predecessors[0].predecessor, TaskRef::Local(na.id), "the copy of B follows the copy of A");
        assert_eq!(nb.predecessors[0].kind, LinkType::SS);
        assert_eq!(nm.members, vec![TaskRef::Local(na.id)]);
        let pasted: Vec<&Assignment> = p.assignments_for_task(nb.id).collect();
        assert_eq!(pasted.len(), 1, "{:?}", p.assignments);
        assert_eq!((pasted[0].resource, pasted[0].units), (ana, 0.5));
        assert_eq!(p.assignments.len(), 2, "the original assignment stays");
        assert_eq!(p.task(b).unwrap().predecessors[0].predecessor, TaskRef::Local(a), "the originals are untouched");
    }

    /// Regression: pasting into another plan kept the copied calendar and custom field ids, which
    /// mean something else there (or nothing, and then the paste was refused).
    #[test]
    fn pasting_into_another_plan_matches_calendars_fields_and_resources_by_name() {
        let mut src = Project::new("Source", day());
        let late = src.add_calendar("Late crew", None).unwrap();
        let only_here = src.add_calendar("Only here", None).unwrap();
        let risk = src.add_custom_field("Risk", CustomFieldKind::Text, &[]).unwrap();
        let code = src.add_custom_field("Cost code", CustomFieldKind::Number, &[]).unwrap();
        let (ana, ben) = (src.add_resource("Ana"), src.add_resource("Ben"));
        let t1 = src.add_task("T1", None);
        let t2 = src.add_task("T2", None);
        src.task_mut(t1).unwrap().calendar = Some(late);
        src.task_mut(t2).unwrap().calendar = Some(only_here);
        src.set_custom_value(t1, risk, Some(CustomValue::Text("High".into()))).unwrap();
        src.set_custom_value(t1, code, Some(CustomValue::Number(3.0))).unwrap();
        src.assign(t1, ana, None, None).unwrap();
        src.assign(t2, ben, None, None).unwrap();
        recalculate(&mut src);

        // The same names under other ids, and some missing.
        let mut dst = Project::new("Target", day());
        dst.add_calendar("Filler", None).unwrap();
        let dst_late = dst.add_calendar("Late crew", None).unwrap();
        dst.add_custom_field("Filler", CustomFieldKind::Text, &[]).unwrap();
        let dst_risk = dst.add_custom_field("risk", CustomFieldKind::Text, &[]).unwrap();
        dst.add_resource("Filler");
        let dst_ana = dst.add_resource("Ana");
        recalculate(&mut dst);
        assert_eq!(dst_late, only_here, "the copied id names another calendar in the target");

        let mut ws = Workspace::default();
        let a = ws.add(src, None);
        let b = ws.add(dst, None);
        ws.active = ws.index_of(a);
        let mut h = harness_with(ws);
        let ctx = h.ctx.clone();
        h.state_mut().st.sel.tasks = [t1, t2].into_iter().collect();
        h.state_mut().exec(&ctx, Command::Copy);
        {
            let mut ws = h.state().ws.lock().unwrap();
            ws.active = ws.index_of(b);
        }
        h.run_steps(2);
        h.state_mut().exec(&ctx, Command::Paste);
        h.run_steps(2);

        let ws = h.state().ws.lock().unwrap();
        let p = &ws.active_doc().unwrap().project;
        assert_eq!(p.name, "Target");
        let n1 = p.tasks.iter().find(|t| t.name == "T1").expect("pasted");
        let n2 = p.tasks.iter().find(|t| t.name == "T2").expect("pasted");
        assert_eq!(n1.calendar, Some(dst_late), "the calendar of the same name");
        assert_eq!(n2.calendar, None, "no calendar of that name: the project calendar");
        assert_eq!(n1.custom.len(), 1, "{:?}", n1.custom);
        assert_eq!(n1.custom.get(&dst_risk), Some(&CustomValue::Text("High".into())), "the field of the same name");
        let r: Vec<ResourceId> = p.assignments_for_task(n1.id).map(|a| a.resource).collect();
        assert_eq!(r, vec![dst_ana], "the resource of the same name");
        assert_eq!(p.assignments_for_task(n2.id).count(), 0, "no resource of that name");
        let note = h.state().st.toasts.iter().map(|t| t.text.clone()).collect::<Vec<_>>().join("\n");
        for left_out in ["Only here", "Cost code", "Ben"] {
            assert!(note.contains(left_out), "{left_out} is mentioned: {note}");
        }
    }

    /// Regression: restoring a recovery copy of a file that was open bound two documents to that
    /// file, so each save overwrote the other's.
    #[test]
    fn a_recovery_copy_of_a_file_already_open_restores_as_an_unsaved_plan() {
        let tmp = crate::recovery::tests::TempDir::new("restore-open");
        let path = tmp.0.join("plan.plancraft");
        plancraft_core::io::save(&Project::new("Plan", day()), &path).unwrap();
        let store = RecoveryStore::new(tmp.0.join("recovery"));
        {
            let mut ws = Workspace::default();
            ws.add(Project::new("Plan", day()), Some(path.clone()));
            ws.docs[0]
                .edit("Add", |p| {
                    p.add_task("Recovered task", None);
                    p.external_projects.push(plancraft_core::model::ExternalProject {
                        id: uuid::Uuid::new_v4(),
                        name: "Other".into(),
                        path: "other.plancraft".into(),
                        tasks: Vec::new(),
                        last_sync: None,
                    });
                })
                .unwrap();
            Recovery::new(Some(store.clone())).write_dirty(&ws).unwrap();
        }
        let mut ws = Workspace::default();
        ws.open(&path).unwrap();
        let mut h = harness_with(ws);
        *h.state().rec() = Recovery::new(Some(store));
        let stem = h.state().rec().found.first().unwrap().stem.clone();
        let ctx = h.ctx.clone();
        h.state_mut().exec(&ctx, Command::RestoreRecovered(stem));
        h.run_steps(2);
        let ws = h.state().ws.lock().unwrap();
        assert_eq!(ws.docs.len(), 2);
        assert_eq!(ws.docs.iter().filter(|d| d.path.is_some()).count(), 1, "only the open document is bound to the file");
        let doc = ws.active_doc().unwrap();
        assert!(doc.path.is_none() && doc.dirty);
        assert!(doc.project.tasks.iter().any(|t| t.name == "Recovered task"));
        // Regression (second review): the copy has no folder of its own, so its relative link
        // to another plan would have pointed at the working directory.
        let link = std::path::PathBuf::from(&doc.project.external_projects[0].path);
        assert!(link.is_absolute() && link.ends_with("other.plancraft"), "{link:?}");
        assert_eq!(link.parent(), path.parent());
        assert!(h.state().st.toasts.iter().any(|t| t.text.contains("plan.plancraft is already open")), "the user is told");
    }

    /// Links Between Projects reads linked files in the app, after the workspace is unlocked:
    /// adding a file, listing its tasks and linking to one still work that way.
    #[test]
    fn links_between_projects_add_list_and_link_through_the_app() {
        use egui_kittest::kittest::Queryable;
        let tmp = crate::recovery::tests::TempDir::new("external-links");
        let other_path = tmp.0.join("Other.plancraft");
        let mut other = Project::new("Other", day());
        let delivery = other.add_task("Supplier delivery", None);
        recalculate(&mut other);
        plancraft_core::io::save(&other, &other_path).unwrap();

        let mut h = harness();
        let ctx = h.ctx.clone();
        let target = leaf(&h, 1);
        h.state_mut().st.sel.select_task(target);
        h.state_mut().exec(&ctx, Command::ExternalLinks);
        h.run_steps(1);
        h.state_mut().add_external_project(&other_path);
        h.run_steps(3);
        assert!(h.state().dialogs.external.as_ref().is_some_and(|b| b.dialog.list_requested.is_none()), "the list was read");
        h.get_by_label("Supplier delivery");
        h.query_all_by_label("Predecessor").next().unwrap().click();
        h.run_steps(3);
        {
            let ws = h.state().ws.lock().unwrap();
            let p = &ws.active_doc().unwrap().project;
            let ext = p.external_projects.iter().find(|e| e.name == "Other").expect("linked");
            assert!(ext.task(delivery).is_some(), "the task's dates are cached");
            let link = TaskRef::External { project: other.id, task: delivery };
            assert!(p.task(target).unwrap().predecessors.iter().any(|d| d.predecessor == link));
        }
        assert!(h.state().st.toasts.iter().any(|t| t.text == "Linked to external task"));

        // The plan closed while the file was being read: nothing is linked anywhere.
        let handle = h.state().dialogs.external.as_ref().unwrap().doc;
        h.state().ws.lock().unwrap().close(handle);
        h.state_mut().add_external_project(&other_path);
        assert!(h.state().st.toasts.iter().any(|t| t.text.contains("closed")));
    }

    /// The start of the first leaf task and a bar drag of it in progress.
    fn start_bar_drag(h: &mut Harness<'static, PlanCraftApp>) -> (TaskId, Option<plancraft_core::dates::DateTime>) {
        let (id, start) = {
            let ws = h.state().ws.lock().unwrap();
            let t = ws.active_doc().unwrap().project.tasks.iter().find(|t| !t.sched.is_summary && !t.is_milestone()).unwrap();
            (t.id, t.sched.start)
        };
        let origin_start = start.unwrap();
        h.state_mut().st.gantt.drag = Some(crate::state::BarDrag::Move { task: id, grab_dx: -400.0, origin_start });
        (id, start)
    }

    fn start_of(h: &Harness<'static, PlanCraftApp>, id: TaskId) -> Option<plancraft_core::dates::DateTime> {
        h.state().ws.lock().unwrap().active_doc().unwrap().project.task(id).unwrap().sched.start
    }

    /// Regression: a bar drag whose mouse button was released outside the window never ended:
    /// the drag only reset when the chart saw the drag stop, with the pointer over the window.
    #[test]
    fn a_gantt_bar_drag_ends_when_the_button_is_released_elsewhere() {
        let mut h = harness();
        // The button came up outside the window and the pointer is back: no drag stop arrives.
        let (id, start) = start_bar_drag(&mut h);
        h.event(egui::Event::PointerMoved(egui::pos2(1100.0, 400.0)));
        h.run_steps(3);
        assert!(h.state().st.gantt.drag.is_none(), "the drag ended");
        assert_eq!(start_of(&h, id), start, "without moving the task");

        // The pointer left the window with no button down.
        let (id, start) = start_bar_drag(&mut h);
        h.event(egui::Event::PointerGone);
        h.run_steps(3);
        assert!(h.state().st.gantt.drag.is_none(), "pointer gone");
        assert_eq!(start_of(&h, id), start);

        // Regression (review): with the button still held, leaving the window for a moment (to
        // drag a bar towards the edge) cancelled the drag. It goes on.
        let at = egui::pos2(1100.0, 400.0);
        h.event(egui::Event::PointerMoved(at));
        h.event(egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
        h.run_steps(1);
        let (_, _) = start_bar_drag(&mut h);
        h.run_steps(1);
        h.event(egui::Event::PointerGone);
        h.run_steps(2);
        assert!(h.state().st.gantt.drag.is_some(), "the drag survives the pointer leaving while the button is held");

        // Regression (second review): the release outside the window never arrived (egui still
        // thinks the button is down); the next click must cancel the drag, not drop the bar there.
        let (id, start) = (id, start_of(&h, id));
        let back = egui::pos2(1200.0, 300.0);
        h.event(egui::Event::PointerMoved(back));
        h.event(egui::Event::PointerButton { pos: back, button: egui::PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
        h.run_steps(1);
        assert!(h.state().st.gantt.drag.is_none(), "a new press ends the lost drag");
        h.event(egui::Event::PointerButton { pos: back, button: egui::PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
        h.run_steps(2);
        assert_eq!(start_of(&h, id), start, "nothing moved");
    }

    /// Escape cancels a bar drag with the button still down, and the release then changes nothing.
    #[test]
    fn escape_cancels_a_gantt_bar_drag() {
        let mut h = harness();
        let at = egui::pos2(1100.0, 400.0);
        h.event(egui::Event::PointerMoved(at));
        h.event(egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
        h.run_steps(1);
        let (id, start) = start_bar_drag(&mut h);
        h.run_steps(1);
        assert!(h.state().st.gantt.drag.is_some(), "the drag goes on while the button is down");
        h.key_press(egui::Key::Escape);
        h.run_steps(2);
        assert!(h.state().st.gantt.drag.is_none(), "Escape ended it");
        h.event(egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
        h.run_steps(3);
        assert_eq!(start_of(&h, id), start);
    }

    /// Regression: Task Usage and Resource Usage shared one grid state, so widening Resource Name
    /// widened Task Name, and the cursor and scrolling carried over.
    #[test]
    fn task_usage_and_resource_usage_keep_their_own_grid() {
        let mut h = harness();
        h.state_mut().st.view = ViewKind::TaskUsage;
        h.run_steps(2);
        let task_widths = h.state().st.task_usage_grid.widths.clone();
        assert_eq!(task_widths.len(), 2);
        h.state_mut().st.task_usage_grid.widths[0] += 90.0;
        h.state_mut().st.task_usage_grid.cursor = Some((3, 1));
        h.run_steps(2);

        h.state_mut().st.view = ViewKind::ResourceUsage;
        h.run_steps(2);
        let res = &h.state().st.resource_usage_grid;
        assert_eq!(res.widths.first().copied(), Some(220.0), "Resource Name keeps its own width");
        assert_eq!(res.cursor, None, "and its own cursor");
        h.state_mut().st.resource_usage_grid.widths[0] = 150.0;
        h.run_steps(2);

        h.state_mut().st.view = ViewKind::TaskUsage;
        h.run_steps(2);
        let task = &h.state().st.task_usage_grid;
        assert_eq!(task.widths.first().copied(), task_widths.first().map(|w| w + 90.0));
        assert_eq!(task.cursor, Some((3, 1)));
    }
}

/// A plan opened from a real PlanCraft Server in the app: edits go out, other people's come in,
/// undo keeps their changes, a viewer cannot change anything, closing disconnects.
#[cfg(test)]
mod shared_tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]
    use std::time::{Duration, Instant};

    use super::*;
    use crate::remote::test_server::TestServer;
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;
    use plancraft_server::link::{Event, OpenedLink};

    /// The app with `user`'s copy of the server's plan open.
    fn app_on(server: &TestServer, user: &str) -> Harness<'static, PlanCraftApp> {
        let shared = Workspace::default().shared();
        let mut h = Harness::builder().with_size(vec2(1440.0, 900.0)).build_eframe(move |cc| PlanCraftApp::new(cc, shared));
        h.run_steps(2);
        let opened = server.open(user);
        h.state_mut().add_remote(dialogs::server::Opened {
            opened,
            address: server.address(),
            user: user.into(),
            plan: "Plan.plancraft".into(),
        });
        h.run_steps(2);
        h
    }

    /// Runs frames until `done` holds.
    fn until(h: &mut Harness<'static, PlanCraftApp>, what: &str, done: impl Fn(&PlanCraftApp) -> bool) {
        let end = Instant::now() + Duration::from_secs(30);
        loop {
            h.run_steps(1);
            if done(h.state()) {
                return;
            }
            assert!(Instant::now() < end, "waited too long for {what}");
            std::thread::sleep(Duration::from_millis(15));
        }
    }

    fn doc_has(app: &PlanCraftApp, name: &str) -> bool {
        lock(&app.ws).active_doc().is_some_and(|d| d.project.tasks.iter().any(|t| t.name == name))
    }

    fn settled(app: &PlanCraftApp) -> bool {
        let ws = lock(&app.ws);
        let Some(doc) = ws.active_doc() else { return false };
        let connected = app.remotes.get(doc.handle).is_some_and(|r| r.status.state == plancraft_server::link::State::Connected);
        connected && !doc.dirty && !doc.has_unsent()
    }

    /// Someone else's copy, driven by hand: waits until its plan shows `name`.
    fn other_sees(other: &mut OpenedLink, name: &str) {
        let end = Instant::now() + Duration::from_secs(30);
        loop {
            for e in other.link.events() {
                if let Event::Plan { project, acked: 0 } = e {
                    other.project = project;
                }
            }
            if other.project.tasks.iter().any(|t| t.name == name) {
                return;
            }
            assert!(Instant::now() < end, "the other copy never showed {name}");
            std::thread::sleep(Duration::from_millis(15));
        }
    }

    #[test]
    fn edits_go_both_ways_and_the_plan_shows_it_is_shared() {
        let server = TestServer::start();
        let mut h = app_on(&server, "ana");
        {
            let ws = lock(&h.state().ws);
            let doc = ws.active_doc().unwrap();
            assert!(doc.shared.is_some() && doc.path.is_none());
            assert_eq!(doc.title(), "Plan");
        }
        assert_eq!(h.state().last_title, "Plan - PlanCraft");
        h.get_by_label_contains("· Connected");
        let mut ben = server.open("ben");
        // Ana's edit, made in the app, reaches Ben.
        lock(&h.state().ws).active_doc_mut().unwrap().edit("Add", |p| p.add_task("From Ana", None)).unwrap();
        h.run_steps(1);
        other_sees(&mut ben, "From Ana");
        until(&mut h, "Ana's edit confirmed", settled);
        // Ben's edit appears in the app.
        let before = ben.project.clone();
        let mut after = before.clone();
        after.add_task("From Ben", None);
        plancraft_core::schedule::recalculate(&mut after);
        ben.link.edited(1, before, after, "Add");
        until(&mut h, "Ben's edit", |app| doc_has(app, "From Ben"));
        until(&mut h, "people", |app| {
            lock(&app.ws).active_doc().and_then(|d| app.remotes.get(d.handle)).is_some_and(|r| r.others().count() == 1)
        });
        h.run_steps(2);
        h.get_by_label_contains("👥 ben");
        // Ben's place shows in Ana's task table.
        let task = ben.project.tasks[2].id;
        ben.link.presence(Some(task));
        until(&mut h, "Ben's place", |app| app.st.others_on.get(&task).is_some_and(|n| n == "ben"));
        // Saving is the server's job; Save says so and changes nothing.
        let ctx = h.ctx.clone();
        h.state_mut().exec(&ctx, Command::Save);
        assert!(h.state().status.as_ref().is_some_and(|(m, _)| m.contains("saves every change")));
    }

    #[test]
    fn undo_keeps_other_peoples_changes_and_is_refused_over_theirs() {
        let server = TestServer::start();
        let mut h = app_on(&server, "ana");
        let mut ben = server.open("ben");
        let (a, b) = {
            let ws = lock(&h.state().ws);
            let p = &ws.active_doc().unwrap().project;
            let leaves: Vec<TaskId> = p.tasks.iter().filter(|t| !t.sched.is_summary).map(|t| t.id).take(2).collect();
            (leaves[0], leaves[1])
        };
        lock(&h.state().ws).active_doc_mut().unwrap().edit("Rename", |p| p.task_mut(a).unwrap().name = "Ana's".into()).unwrap();
        h.run_steps(1);
        other_sees(&mut ben, "Ana's");
        // Ben renames another task, then the same one.
        let mut after = ben.project.clone();
        after.task_mut(b).unwrap().name = "Ben's".into();
        ben.link.edited(1, ben.project.clone(), after.clone(), "Rename");
        until(&mut h, "Ben's rename", |app| doc_has(app, "Ben's"));
        until(&mut h, "settled", settled);
        let ctx = h.ctx.clone();
        h.state_mut().exec(&ctx, Command::Undo);
        until(&mut h, "undone", |app| !doc_has(app, "Ana's") && doc_has(app, "Ben's"));
        // Redo, then Ben changes the same task: undoing again is refused.
        h.state_mut().exec(&ctx, Command::Redo);
        until(&mut h, "redone", |app| doc_has(app, "Ana's") && settled(app));
        other_sees(&mut ben, "Ana's");
        let mut theirs = ben.project.clone();
        theirs.task_mut(a).unwrap().name = "Ben changed it".into();
        ben.link.edited(2, ben.project.clone(), theirs, "Rename");
        until(&mut h, "Ben's change", |app| doc_has(app, "Ben changed it"));
        h.state_mut().st.toasts.clear();
        h.state_mut().exec(&ctx, Command::Undo);
        assert!(doc_has(h.state(), "Ben changed it"), "nothing was undone");
        assert!(h.state().st.toasts.iter().any(|t| t.text.contains("Cannot undo")), "the person is told why");
    }

    #[test]
    fn a_viewer_cannot_change_the_plan() {
        let server = TestServer::start();
        let mut h = app_on(&server, "vic");
        let before = lock(&h.state().ws).active_doc().unwrap().project.clone();
        h.state_mut().st.toasts.clear();
        let ctx = h.ctx.clone();
        h.state_mut().exec(&ctx, Command::InsertTask);
        h.run_steps(2);
        assert_eq!(lock(&h.state().ws).active_doc().unwrap().project, before);
        assert!(
            h.state().st.toasts.iter().any(|t| t.text.contains("only view")),
            "{:?}",
            h.state().st.toasts.iter().map(|t| &t.text).collect::<Vec<_>>()
        );
        h.get_by_label_contains("· view only");
    }

    #[test]
    fn save_as_writes_a_copy_and_the_plan_stays_shared() {
        // Regression: the file name was looked up holding the workspace lock twice, which froze
        // the app on Save As (and on Save in the close prompt).
        let server = TestServer::start();
        let mut h = app_on(&server, "ana");
        let handle = lock(&h.state().ws).active_doc().unwrap().handle;
        assert_eq!(h.state().copy_name(handle).as_deref(), Some("Plan.plancraft"));
        let dir = crate::recovery::tests::TempDir::new("copy");
        let path = dir.0.join("Mine.plancraft");
        assert!(h.state_mut().write_copy(handle, path.clone()));
        let copy = plancraft_core::io::load(&path).unwrap();
        assert!(!copy.tasks.is_empty());
        let ws = lock(&h.state().ws);
        let doc = ws.active_doc().unwrap();
        assert!(doc.shared.is_some() && doc.path.is_none(), "the open plan is still the server's");
    }

    #[test]
    fn work_the_server_dropped_is_kept_when_the_plan_closes() {
        let server = TestServer::start();
        let mut h = app_on(&server, "ana");
        until(&mut h, "settled", settled);
        let (handle, mut mine) = {
            let ws = lock(&h.state().ws);
            let doc = ws.active_doc().unwrap();
            (doc.handle, doc.project.clone())
        };
        mine.add_task("Only in my version", None);
        h.state_mut().remotes.get_mut(handle).unwrap().dropped =
            Some(crate::remote::DroppedWork { lines: vec!["Your new task was not kept".into()], before: Box::new(mine) });
        h.run_steps(2);
        assert!(lock(&h.state().ws).active_doc().unwrap().dirty, "work left out counts as unsaved: closing asks");
        let ctx = h.ctx.clone();
        h.state_mut().exec(&ctx, Command::Close);
        h.run_steps(2);
        h.get_by_label_contains("in a plan of their own").focus();
        h.get_by_label("Don't Save").click();
        h.run_steps(3);
        let ws = lock(&h.state().ws);
        assert!(ws.index_of(handle).is_none(), "the shared plan closed");
        let kept = ws.docs.iter().find(|d| d.project.name.ends_with("(my version)")).expect("the dropped work is a plan of its own");
        assert!(kept.dirty && kept.project.tasks.iter().any(|t| t.name == "Only in my version"));
    }

    #[test]
    fn a_signed_out_plan_is_not_reused_and_says_where_it_stands() {
        let server = TestServer::start();
        let mut h = app_on(&server, "ana");
        let handle = lock(&h.state().ws).active_doc().unwrap().handle;
        let address = server.address();
        {
            let app = h.state();
            assert_eq!(app.remotes.find(&lock(&app.ws), &address, "Plan.plancraft"), Some(handle));
        }
        let r = h.state_mut().remotes.get_mut(handle).unwrap();
        r.status.state = plancraft_server::link::State::Connecting;
        assert_eq!(r.state_label(), "Connecting");
        r.status.state = plancraft_server::link::State::SignedOut("the password changed".into());
        assert_eq!(r.state_label(), "Signed out");
        let app = h.state();
        assert_eq!(app.remotes.find(&lock(&app.ws), &address, "Plan.plancraft"), None, "opening it again signs in afresh");
    }

    #[test]
    fn opening_a_shared_plans_file_directly_warns() {
        let dir = crate::recovery::tests::TempDir::new("journal");
        let path = dir.0.join("Plan.plancraft");
        plancraft_core::io::save(&plancraft_core::sample::demo_project(plancraft_core::dates::ymd(2026, 1, 5).unwrap()), &path).unwrap();
        let shared = Workspace::default().shared();
        let mut h = Harness::builder().with_size(vec2(1440.0, 900.0)).build_eframe(move |cc| PlanCraftApp::new(cc, shared));
        h.run_steps(2);
        h.state_mut().open_path(path.clone());
        assert!(h.state().st.toasts.is_empty(), "a plain plan file opens quietly");
        let ctx = h.ctx.clone();
        h.state_mut().exec(&ctx, Command::Close);
        std::fs::write(plancraft_server::journal::path_for(&path).unwrap(), "{}\n").unwrap();
        h.state_mut().open_path(path);
        assert!(h.state().st.toasts.iter().any(|t| t.text.contains("PlanCraft Server shares this plan")));
    }

    #[test]
    fn closing_the_plan_disconnects() {
        let server = TestServer::start();
        let mut h = app_on(&server, "ana");
        let ben = server.open("ben");
        until(&mut h, "settled", settled);
        let ctx = h.ctx.clone();
        h.state_mut().exec(&ctx, Command::Close);
        h.run_steps(2);
        assert!(lock(&h.state().ws).docs.is_empty());
        let end = Instant::now() + Duration::from_secs(20);
        let mut people = 2;
        while people != 1 {
            for e in ben.link.events() {
                if let Event::People(p) = e {
                    people = p.len();
                }
            }
            assert!(Instant::now() < end, "Ana's copy is still connected");
            std::thread::sleep(Duration::from_millis(15));
        }
    }
}
