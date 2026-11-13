//! The PlanCraft desktop application: layout, command execution and document management.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration as StdDuration, Instant};

use chrono::{Duration, NaiveDate};
use egui::{Align, Align2, Color32, FontId, Key, KeyboardShortcut, Layout, Modifiers, Rect, RichText, Sense, Stroke, Ui, pos2, vec2};
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
    /// A Save stopped because another program changed the file: asks to overwrite (by handle).
    confirm_overwrite: Option<u64>,
    /// The close (of a document or the app) to carry on with once the overwrite question is
    /// answered and the save went through.
    close_after_save: Option<CloseRequest>,
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

/// Longest panic message shown to the user.
const MAX_CRASH_CHARS: usize = 400;

#[derive(Clone, Copy, PartialEq)]
enum CloseRequest {
    Document(u64),
    App,
}

impl PlanCraftApp {
    pub fn new(cc: &eframe::CreationContext<'_>, ws: SharedWorkspace) -> Self {
        let mut settings = AppSettings::load();
        if let Some(t) = std::env::var("PLANCRAFT_THEME").ok().and_then(|v| theme::ThemeChoice::parse(&v)) {
            settings.theme = t;
        }
        theme::install(&cc.egui_ctx, settings.theme);
        let has_doc = !lock(&ws).docs.is_empty();
        let st = UiState { table_width: settings.table_width, start_page: !has_doc, ..UiState::default() };
        let mut app = Self {
            ws,
            st,
            dialogs: Dialogs::default(),
            settings,
            mcp: McpHost::default(),
            seen: HashMap::new(),
            status: None,
            confirm_close: None,
            confirm_overwrite: None,
            close_after_save: None,
            allow_exit: false,
            last_title: String::new(),
            shot: std::env::var_os("PLANCRAFT_SCREENSHOT").map(|p| (PathBuf::from(p), 0)),
            last_active: None,
            // Tests never touch the user's recovery folder; they inject their own store.
            recovery: Arc::new(Mutex::new(Recovery::new(if cfg!(test) { None } else { RecoveryStore::user() }))),
            recovery_error: None,
            crash: None,
            inject_panic: false,
        };
        recovery::spawn_background_writer(&app.recovery, &app.ws, cc.egui_ctx.clone());
        if !app.rec().found.is_empty() && !app.st.start_page {
            app.st.toast(
                ToastKind::Info,
                "PlanCraft found unsaved work from an earlier session. Open the Start Page (File menu) to restore it.",
            );
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
        ];
        for (sc, c, global) in map {
            if (global || !typing) && ctx.input_mut(|i| i.consume_shortcut(&sc)) {
                out.push(c);
            }
        }
        // The desktop backend turns Ctrl+C/X/V into clipboard events rather than key presses, so
        // outside a text field they are task clipboard commands. (Paste only arrives when the
        // system clipboard holds text; Copy and Cut put the task names there for that reason.)
        if !typing {
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

    fn with_doc<R>(&mut self, f: impl FnOnce(&mut Document, &mut UiState) -> R) -> Option<R> {
        let ws = self.ws.clone();
        let mut ws = lock(&ws);
        let doc = ws.active_doc_mut()?;
        Some(f(doc, &mut self.st))
    }

    fn open_path(&mut self, path: PathBuf) {
        let r = lock(&self.ws).open(&path);
        match r {
            Ok(_) => {
                self.settings.add_recent(path.clone());
                self.st.start_page = false;
                self.st.sel = Default::default();
                self.st.gantt.initialized_for = None;
                self.set_status(format!("Opened {}", path.display()));
            }
            Err(e) => self.st.toast(ToastKind::Error, e.to_string()),
        }
    }

    fn save(&mut self, save_as: bool) -> bool {
        let ws = self.ws.clone();
        let mut ws = lock(&ws);
        let Some(doc) = ws.active_doc_mut() else {
            return false;
        };
        let path = if save_as || doc.path.is_none() {
            let name = format!("{}.{}", doc.project.name, FILE_EXTENSION);
            match rfd::FileDialog::new().add_filter("PlanCraft project", &[FILE_EXTENSION]).set_file_name(name).save_file() {
                Some(mut p) => {
                    if p.extension().is_none() {
                        p.set_extension(FILE_EXTENSION);
                        // The file dialog only asked about the name as typed.
                        if p.exists() {
                            let msg =
                                format!("{} already exists. Choose it in the dialog to replace it, or pick another name.", p.display());
                            drop(ws);
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
        let handle = doc.handle;
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

    /// Asks where to save a PDF and writes it (no workspace lock is held while the file dialog is open).
    fn save_pdf(&mut self, pdf: dialogs::print::PendingPdf) {
        let name: String =
            pdf.name.chars().map(|c| if matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') { '-' } else { c }).collect();
        let Some(path) = rfd::FileDialog::new().add_filter("PDF", &["pdf"]).set_file_name(name).save_file() else { return };
        let path = if path.extension().is_none() {
            // The file dialog only asked about the name as typed: never replace "x.pdf" unasked.
            let with_ext = path.with_extension("pdf");
            if with_ext.exists() {
                return self.st.toast(
                    ToastKind::Warning,
                    format!("{} already exists. Choose it in the dialog to replace it, or pick another name.", with_ext.display()),
                );
            }
            with_ext
        } else {
            path
        };
        match plancraft_core::io::write_atomic(&path, &pdf.bytes, plancraft_core::io::Privacy::KeepExisting) {
            Ok(()) => self.set_status(format!("Saved {}", path.display())),
            Err(e) => self.st.toast(ToastKind::Error, format!("Could not save the PDF: {e}")),
        }
    }

    fn export_csv(&mut self) {
        let Some((name, csv)) = self.with_doc(|doc, _| {
            let p = &doc.project;
            // UTF-8 byte order mark: Excel then reads names with accents correctly.
            let mut s = String::from("\u{feff}ID,Unique ID,WBS,Outline Level,Name,Milestone,Duration,Start,Finish,% Complete,Work (h),Cost,Predecessors,Resources,Critical");
            // A cell starting with = + - @ (or a tab/CR/LF, also after leading spaces) would run as
            // a formula in a spreadsheet: prefix it with an apostrophe so it stays text.
            let q = |v: &str| {
                let risky = v.starts_with(['\t', '\r', '\n']) || v.trim_start().starts_with(['=', '+', '-', '@', '＝', '＋', '－', '＠']);
                let guarded = if risky { format!("'{v}") } else { v.to_string() };
                format!("\"{}\"", guarded.replace('"', "\"\""))
            };
            for f in &p.custom_fields {
                s += &format!(",{}", q(&f.name));
            }
            s.push('\n');
            for (i, t) in p.tasks.iter().enumerate() {
                s += &format!(
                    "{},{},{},{},{},{},{},{},{},{:.0},{:.2},{:.2},{},{},{}",
                    i + 1,
                    t.id.0,
                    q(&p.wbs(i)),
                    t.outline_level,
                    q(&t.name),
                    t.is_milestone(),
                    q(&p.format_duration(t.sched.duration, t.estimated)),
                    crate::format::iso(t.sched.start),
                    crate::format::iso(t.sched.finish),
                    t.sched.percent_complete,
                    t.sched.work as f64 / 60.0,
                    t.sched.cost,
                    q(&p.format_predecessors(t.id)),
                    q(&p.format_resource_names(t.id)),
                    t.sched.critical
                );
                for f in &p.custom_fields {
                    let v = t.custom.get(&f.id).map(|v| p.custom_value_text(f.id, v)).unwrap_or_default();
                    s += &format!(",{}", q(&v));
                }
                s.push('\n');
            }
            (p.name.clone(), s)
        }) else {
            return;
        };
        if let Some(path) = rfd::FileDialog::new().add_filter("CSV", &["csv"]).set_file_name(format!("{name}.csv")).save_file() {
            match std::fs::write(&path, csv) {
                Ok(()) => self.set_status(format!("Exported {}", path.display())),
                Err(e) => self.st.toast(ToastKind::Error, e.to_string()),
            }
        }
    }

    fn exec(&mut self, ctx: &egui::Context, cmd: Command) {
        match cmd {
            Command::NewProject => {
                let start = chrono::Local::now().date_naive();
                let mut ws = lock(&self.ws);
                let n = ws.docs.len() + 1;
                ws.add(Project::new(format!("Project{n}"), start), None);
                drop(ws);
                self.st.start_page = false;
                self.st.sel = Default::default();
                self.st.view = ViewKind::Gantt;
            }
            Command::NewDemo => {
                let today = chrono::Local::now().date_naive();
                let monday = today - Duration::days(chrono::Datelike::weekday(&today).num_days_from_monday() as i64) - Duration::days(7);
                lock(&self.ws).add(plancraft_core::sample::demo_project(monday), None);
                self.st.start_page = false;
                self.st.sel = Default::default();
                self.st.view = ViewKind::Gantt;
            }
            Command::Open => {
                if let Some(path) = rfd::FileDialog::new().add_filter("PlanCraft project", &[FILE_EXTENSION]).pick_file() {
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
            Command::ExportCsv => self.export_csv(),
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
            Command::McpPanel => {
                self.dialogs.mcp = true;
                self.st.start_page = false;
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
            Ok((project, notes)) => {
                let mut ws = lock(&self.ws);
                ws.add(project, item.original_path.clone());
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
                    Some(p) => format!("Save writes it back to {}", p.display()),
                    None => "Save it to keep it".to_string(),
                };
                self.set_status(format!("Restored {} from a recovery copy. {target}.", item.name));
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
            Command::Undo => {
                if doc.undo() {
                    let l = doc.last_change.as_ref().map(|l| l.0.clone()).unwrap_or_default();
                    self.status = Some((l, Instant::now()));
                }
            }
            Command::Redo => {
                doc.redo();
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
                if let Some(r) = doc.project.task_index(id) {
                    let rows = views::visible_rows(&doc.project, st);
                    if let Some(row) = views::row_of_task(&rows, r) {
                        // The plan's own columns: Name may have been moved, or columns inserted.
                        let name_col = crate::fields::table_fields(&doc.project, st.table)
                            .iter()
                            .position(|f| *f == crate::fields::Field::Builtin(crate::columns::TaskCol::Name))
                            .unwrap_or(0);
                        st.task_grid.begin_edit(row, name_col, String::new());
                        st.task_grid.scroll_to = Some(row);
                        st.cursor_task = Some((row, id));
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
                if let Some(id) = sel.first().copied() {
                    let up = cmd == Command::MoveUp;
                    let r = doc.try_edit(if up { "Move Up" } else { "Move Down" }, ChangeSource::User, |p| p.shift_task(id, up));
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
                // Progress up to the status date (or now).
                let at = plancraft_core::report::status_moment(&doc.project);
                let r = doc.try_edit("Mark on Track", ChangeSource::User, |p| {
                    let targets: Vec<(TaskId, u8)> = {
                        let wc = plancraft_core::schedule::project_calendar(p);
                        sel.iter()
                            .filter_map(|id| {
                                let t = p.task(*id)?;
                                let (s, f) = (t.sched.start?, t.sched.finish?);
                                let pct = if at >= f {
                                    100
                                } else if at <= s {
                                    0
                                } else {
                                    (wc.between(s, at) * 100 / wc.between(s, f).max(1)) as u8
                                };
                                Some((*id, pct))
                            })
                            .collect()
                    };
                    for (id, pct) in targets {
                        p.set_percent_complete(id, pct)?;
                    }
                    Ok(())
                });
                report(st, r);
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
                    let r = doc.edit(if mode == ScheduleMode::Manual { "Manually Schedule" } else { "Auto Schedule" }, |p| {
                        for id in &sel {
                            if let Some(t) = p.task_mut(*id) {
                                if mode == ScheduleMode::Manual && t.mode == ScheduleMode::Auto {
                                    t.manual_start = t.sched.start;
                                    t.manual_finish = t.sched.finish;
                                }
                                t.mode = mode;
                            }
                        }
                    });
                    report(st, r);
                }
            }
            Command::TaskInfo | Command::TaskNotes => {
                if let Some(id) = st.sel.single_task().or(sel.first().copied()) {
                    self.dialogs.task_info =
                        dialogs::task_info::TaskInfo::new(&doc.project, id, cmd == Command::TaskNotes).map(|d| Bound::new(doc, d));
                }
            }
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
                    let at = first_sel_index.unwrap_or(doc.project.tasks.len());
                    let r = doc.edit("Paste Tasks", |p| {
                        let base = clip.iter().map(|t| t.outline_level).min().unwrap_or(1);
                        let target_level = p.tasks.get(at).map_or(1, |t| t.outline_level);
                        let at = at.min(p.tasks.len());
                        let mut ids = Vec::new();
                        for (k, t) in clip.iter().enumerate() {
                            let mut nt = t.clone();
                            nt.id = p.ids.next_task();
                            nt.predecessors.clear();
                            nt.members.clear();
                            nt.baseline = None;
                            nt.outline_level = target_level.saturating_add(t.outline_level.saturating_sub(base));
                            ids.push(nt.id);
                            p.tasks.insert(at + k, nt);
                        }
                        p.normalize_outline();
                        ids
                    });
                    match r {
                        Ok(ids) => st.sel.tasks = ids.into_iter().collect(),
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
            Command::SetBaseline => match doc.edit("Set Baseline", |p| p.set_baseline(&[])) {
                Ok(()) => st.toast(ToastKind::Success, "Baseline saved for all tasks"),
                Err(e) => st.toast(ToastKind::Warning, e.to_string()),
            },
            Command::ClearBaseline => report(st, doc.edit("Clear Baseline", |p| p.clear_baseline(&[]))),
            Command::UpdateProgressFromTime => match doc.edit("Update Progress from Time", |p| p.update_progress_from_time()) {
                Ok(_) => st.toast(ToastKind::Success, "Progress updated from tracked time"),
                Err(e) => st.toast(ToastKind::Warning, e.to_string()),
            },
            Command::ExternalLinks => self.dialogs.external = Some(Bound::new(doc, Default::default())),
            Command::RefreshExternal => {
                let w = doc.refresh_external();
                if w.is_empty() {
                    st.toast(ToastKind::Success, "External links are up to date");
                }
                for m in w {
                    st.toast(ToastKind::Warning, m);
                }
            }
            Command::Statistics => self.dialogs.stats = true,
            Command::ZoomIn | Command::ZoomOut => {
                let g = &mut st.gantt;
                g.zoom(if cmd == Command::ZoomIn { 1 } else { -1 }, 400.0, 0.0);
            }
            Command::ZoomEntire | Command::ZoomSelected => {
                let (s, f) = if cmd == Command::ZoomEntire {
                    (doc.project.sched.start, doc.project.sched.finish)
                } else {
                    let ts: Vec<&Task> = sel.iter().filter_map(|id| doc.project.task(*id)).collect();
                    (ts.iter().filter_map(|t| t.sched.start).min(), ts.iter().filter_map(|t| t.sched.finish).max())
                };
                if let (Some(s), Some(f)) = (s, f) {
                    let width = (ctx.content_rect().width() - st.table_width - 80.0).max(200.0);
                    let days = ((f - s).num_minutes() as f32 / 1440.0).max(1.0) * 1.1 + 2.0;
                    st.gantt.px_per_day = (width / days).clamp(0.5, 140.0);
                    st.gantt.left = s - Duration::days(1);
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
            let resp = child.selectable_label(active, format!("📄 {}", d.title()));
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
                let mut z = self.st.gantt.px_per_day;
                if ui
                    .add(egui::Slider::new(&mut z, 1.0..=140.0).logarithmic(true).show_value(false))
                    .on_hover_text("Timescale zoom")
                    .changed()
                {
                    self.st.gantt.px_per_day = z;
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
            return;
        };
        let names: Vec<String> = {
            let ws = lock(&self.ws);
            match req {
                CloseRequest::Document(h) => ws.docs.iter().filter(|d| d.handle == h).map(|d| d.project.name.clone()).collect(),
                CloseRequest::App => ws.docs.iter().filter(|d| d.dirty).map(|d| d.project.name.clone()).collect(),
            }
        };
        let mut choice = None;
        egui::Modal::new(egui::Id::new("confirm_close")).show(ctx, |ui| {
            ui.set_width(380.0);
            ui.heading("Save changes?");
            ui.label(format!("Do you want to save changes to {}?", names.join(", ")));
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
                if self.save(false) {
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
                    if !self.save(false) {
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
                    "CustomFields" => Command::CustomFields,
                    "Print" => Command::Print,
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
            self.st.toast(ToastKind::Warning, format!("Opened {}; PlanCraft adjusted out-of-range values: {list}", doc.title()));
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
        self.report_load_notes(&mut ws);

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
        // A document opened or activated elsewhere (e.g. by an agent) becomes the view.
        let active = ws.active_doc().map(|d| d.handle);
        if active != self.last_active {
            if self.last_active.is_none() && active.is_some() {
                self.st.start_page = false;
            }
            self.st.sel = Default::default();
            self.st.gantt.initialized_for = None;
            self.st.cursor_task = None;
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
                let show_timeline = st.timeline_visible && matches!(st.view, ViewKind::Gantt | ViewKind::TrackingGantt);
                if show_timeline {
                    let tl = Rect::from_min_size(rect.min, vec2(rect.width(), views::timeline::HEIGHT));
                    let chart_w = (rect.width() - st.table_width - 16.0).max(100.0);
                    views::timeline::show(ui, tl, &doc.project, &mut st.gantt, chart_w);
                    rect = Rect::from_min_max(pos2(rect.left(), tl.bottom()), rect.max);
                }
                match st.view {
                    ViewKind::Gantt => views::gantt::show(ui, rect, doc, st, false, &mut out),
                    ViewKind::TrackingGantt => views::gantt::show(ui, rect, doc, st, true, &mut out),
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
        {
            let st = &mut self.st;
            dialogs::show_bound(&mut self.dialogs.external, &mut ws, |d, doc| d.show(&ctx, doc, st));
            let st = &*st;
            dialogs::show_bound(&mut self.dialogs.print, &mut ws, |d, doc| d.show(&ctx, doc, st));
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
        if let Some(pdf) = self.dialogs.print.as_mut().and_then(|b| b.dialog.pending_pdf.take()) {
            self.save_pdf(pdf);
        }
        self.confirm_dialog(&ctx);
        self.overwrite_dialog(&ctx);
        self.toasts(&ctx);

        for c in out {
            self.exec(&ctx, c);
        }
        let _ = NaiveDate::MIN;
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
        ws.add(plancraft_core::sample::demo_project(NaiveDate::from_ymd_opt(2026, 10, 19).unwrap()), None);
        let shared = ws.shared();
        let mut h = Harness::builder().with_size(vec2(1440.0, 900.0)).build_eframe(move |cc| PlanCraftApp::new(cc, shared));
        h.run_steps(3);
        h
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
        let a = ws.add(plancraft_core::sample::demo_project(NaiveDate::from_ymd_opt(2026, 10, 19).unwrap()), None);
        let b = ws.add(Project::new("Other plan", NaiveDate::from_ymd_opt(2026, 10, 19).unwrap()), None);
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
            (p.tasks[15].id, p.tasks[16].id, p.tasks[16].outline_level)
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

    /// Regressions in the Print window: Ctrl+P again threw away unsaved Page Setup changes,
    /// closing after an Undo saved the undone setup again, and after a tab switch the window
    /// printed its plan with the other plan's view.
    #[test]
    fn the_print_window_keeps_changes_follows_undo_and_closes_on_tab_switch() {
        use egui_kittest::kittest::Queryable;
        use plancraft_core::print::Orientation;
        let mut ws = Workspace::default();
        let a = ws.add(plancraft_core::sample::demo_project(NaiveDate::from_ymd_opt(2026, 10, 19).unwrap()), None);
        let b = ws.add(Project::new("Other plan", NaiveDate::from_ymd_opt(2026, 10, 19).unwrap()), None);
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
        let a = ws.add(plancraft_core::sample::demo_project(NaiveDate::from_ymd_opt(2026, 10, 19).unwrap()), None);
        let mut other = Project::new("Other plan", NaiveDate::from_ymd_opt(2026, 10, 19).unwrap());
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

    /// Regression: Save silently replaced a file another program had changed since it was opened.
    #[test]
    fn save_asks_before_overwriting_a_file_changed_on_disk() {
        use egui_kittest::kittest::Queryable;
        let tmp = crate::recovery::tests::TempDir::new("overwrite");
        let path = tmp.0.join("plan.plancraft");
        plancraft_core::io::save(&Project::new("Plan", NaiveDate::from_ymd_opt(2026, 10, 19).unwrap()), &path).unwrap();
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
                Project::new("Crashed Plan", NaiveDate::from_ymd_opt(2026, 10, 19).unwrap()),
                Some(PathBuf::from("/plans/crashed.plancraft")),
            );
            ws.add(Project::new("Scratch", NaiveDate::from_ymd_opt(2026, 10, 19).unwrap()), None);
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
}
