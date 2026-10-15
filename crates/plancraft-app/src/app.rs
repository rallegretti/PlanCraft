//! The PlanCraft desktop application: layout, command execution and document management.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration as StdDuration, Instant};

use chrono::{Duration, NaiveDate};
use egui::{Align, Align2, Color32, FontId, Key, KeyboardShortcut, Layout, Modifiers, Rect, RichText, Sense, Stroke, Ui, pos2, vec2};
use plancraft_core::leveling::{LevelingOptions, clear_leveling, level_resources};
use plancraft_core::prelude::*;
use plancraft_core::workspace::{ChangeSource, Document, SharedWorkspace, Workspace};

use crate::backstage;
use crate::commands::Command;
use crate::dialogs::{self, Dialogs};
use crate::mcp_host::McpHost;
use crate::ribbon::{self, RibbonInfo};
use crate::settings::AppSettings;
use crate::state::{ToastKind, UiState, ViewKind};
use crate::theme;
use crate::views;

pub struct PlanCraftApp {
    ws: SharedWorkspace,
    st: UiState,
    dialogs: Dialogs,
    settings: AppSettings,
    mcp: McpHost,
    backstage_page: backstage::Page,
    seen: HashMap<u64, u64>,
    status: Option<(String, Instant)>,
    confirm_close: Option<CloseRequest>,
    allow_exit: bool,
    last_title: String,
    /// Developer screenshot automation (`PLANCRAFT_SCREENSHOT=out.png`).
    shot: Option<(PathBuf, u32)>,
    /// Handle of the active document last frame (agents may open or switch documents).
    last_active: Option<u64>,
}

#[derive(Clone, Copy, PartialEq)]
enum CloseRequest {
    Document(u64),
    App,
}

impl PlanCraftApp {
    pub fn new(cc: &eframe::CreationContext<'_>, ws: SharedWorkspace) -> Self {
        theme::install(&cc.egui_ctx);
        let settings = AppSettings::load();
        let has_doc = ws.lock().map(|w| !w.docs.is_empty()).unwrap_or(false);
        let st = UiState { table_width: settings.table_width, backstage: !has_doc, ..UiState::default() };
        let mut app = Self {
            ws,
            st,
            dialogs: Dialogs::default(),
            settings,
            mcp: McpHost::default(),
            backstage_page: backstage::Page::Home,
            seen: HashMap::new(),
            status: None,
            confirm_close: None,
            allow_exit: false,
            last_title: String::new(),
            shot: std::env::var_os("PLANCRAFT_SCREENSHOT").map(|p| (PathBuf::from(p), 0)),
            last_active: None,
        };
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

    fn set_status(&mut self, s: impl Into<String>) {
        self.status = Some((s.into(), Instant::now()));
    }

    // ------------------------------------------------------------------
    // Commands
    // ------------------------------------------------------------------

    fn shortcuts(&mut self, ctx: &egui::Context, out: &mut Vec<Command>) {
        let typing = ctx.egui_wants_keyboard_input();
        let cmd = Modifiers::COMMAND;
        let map: Vec<(KeyboardShortcut, Command, bool)> = vec![
            (KeyboardShortcut::new(cmd, Key::N), Command::NewProject, true),
            (KeyboardShortcut::new(cmd, Key::O), Command::Open, true),
            (KeyboardShortcut::new(cmd | Modifiers::SHIFT, Key::S), Command::SaveAs, true),
            (KeyboardShortcut::new(cmd, Key::S), Command::Save, true),
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
    }

    fn with_doc<R>(&mut self, f: impl FnOnce(&mut Document, &mut UiState) -> R) -> Option<R> {
        let ws = self.ws.clone();
        let mut ws = ws.lock().ok()?;
        let doc = ws.active_doc_mut()?;
        Some(f(doc, &mut self.st))
    }

    fn open_path(&mut self, path: PathBuf) {
        let r = self.ws.lock().map(|mut ws| ws.open(&path)).map_err(|_| ());
        match r {
            Ok(Ok(_)) => {
                self.settings.add_recent(path.clone());
                self.st.backstage = false;
                self.st.sel = Default::default();
                self.st.gantt.initialized_for = None;
                self.set_status(format!("Opened {}", path.display()));
            }
            Ok(Err(e)) => self.st.toast(ToastKind::Error, e.to_string()),
            Err(_) => {}
        }
    }

    fn save(&mut self, save_as: bool) -> bool {
        let ws = self.ws.clone();
        let Ok(mut ws) = ws.lock() else { return false };
        let Some(doc) = ws.active_doc_mut() else {
            return false;
        };
        let path = if save_as || doc.path.is_none() {
            let name = format!("{}.{}", doc.project.name, FILE_EXTENSION);
            match rfd::FileDialog::new().add_filter("PlanCraft project", &[FILE_EXTENSION]).set_file_name(name).save_file() {
                Some(mut p) => {
                    if p.extension().is_none() {
                        p.set_extension(FILE_EXTENSION);
                    }
                    Some(p)
                }
                None => return false,
            }
        } else {
            None
        };
        let r = match path {
            Some(p) => doc.save_as(&p).map(|_| p),
            None => doc.save().map(|_| doc.path.clone().unwrap_or_default()),
        };
        drop(ws);
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

    fn export_csv(&mut self) {
        let Some((name, csv)) = self.with_doc(|doc, _| {
            let p = &doc.project;
            let mut s = String::from("ID,Unique ID,WBS,Outline Level,Name,Milestone,Duration,Start,Finish,% Complete,Work (h),Cost,Predecessors,Resources,Critical\n");
            let q = |v: &str| format!("\"{}\"", v.replace('"', "\"\""));
            for (i, t) in p.tasks.iter().enumerate() {
                s += &format!(
                    "{},{},{},{},{},{},{},{},{},{:.0},{:.2},{:.2},{},{},{}\n",
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
                let mut ws = self.ws.lock().unwrap();
                let n = ws.docs.len() + 1;
                ws.add(Project::new(format!("Project{n}"), start), None);
                drop(ws);
                self.st.backstage = false;
                self.st.sel = Default::default();
                self.st.view = ViewKind::Gantt;
            }
            Command::NewDemo => {
                let today = chrono::Local::now().date_naive();
                let monday = today - Duration::days(chrono::Datelike::weekday(&today).num_days_from_monday() as i64) - Duration::days(7);
                self.ws.lock().unwrap().add(plancraft_core::sample::demo_project(monday), None);
                self.st.backstage = false;
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
                let info = self.ws.lock().unwrap().active_doc().map(|d| (d.handle, d.dirty));
                if let Some((h, dirty)) = info {
                    if dirty {
                        self.confirm_close = Some(CloseRequest::Document(h));
                    } else {
                        self.close_doc(h);
                    }
                }
            }
            Command::Quit => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            Command::ExportCsv => self.export_csv(),
            Command::McpPanel => {
                self.dialogs.mcp = true;
                self.st.backstage = false;
            }
            Command::About => self.dialogs.about = true,
            Command::SetView(v) => {
                self.st.view = v;
                self.st.backstage = false;
            }
            Command::Filter(f) => self.st.filter = f,
            Command::Table(t) => self.st.table = t,
            Command::History => self.dialogs.history = !self.dialogs.history,
            other => {
                let ws = self.ws.clone();
                let mut ws = ws.lock().unwrap();
                let Some(doc) = ws.active_doc_mut() else {
                    return;
                };
                self.exec_doc(ctx, doc, other);
            }
        }
    }

    fn close_doc(&mut self, handle: u64) {
        let mut ws = self.ws.lock().unwrap();
        ws.close(handle);
        let empty = ws.docs.is_empty();
        drop(ws);
        self.st.sel = Default::default();
        self.st.gantt.initialized_for = None;
        if empty {
            self.st.backstage = true;
        }
    }

    fn exec_doc(&mut self, ctx: &egui::Context, doc: &mut Document, cmd: Command) {
        let st = &mut self.st;
        let sel: Vec<TaskId> = views::selected_in_order(&doc.project, &st.sel);
        let first_sel_index = sel.first().and_then(|id| doc.project.task_index(*id));
        let report = |st: &mut UiState, r: Result<(), PlanError>| {
            if let Err(e) = r {
                st.toast(ToastKind::Warning, e.to_string());
            }
        };
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
                let id = doc.edit(if milestone { "Insert Milestone" } else { "Insert Task" }, |p| {
                    if milestone { p.add_milestone("<New Milestone>", at) } else { p.add_task("<New Task>", at) }
                });
                st.sel.select_task(id);
                if let Some(r) = doc.project.task_index(id) {
                    let rows = views::visible_rows(&doc.project, st);
                    if let Some(row) = rows.iter().position(|&i| i == r) {
                        let name_col = st.table.columns().iter().position(|c| *c == crate::columns::TaskCol::Name).unwrap_or(0);
                        st.task_grid.begin_edit(row, name_col, String::new());
                        st.task_grid.scroll_to = Some(row);
                    }
                }
            }
            Command::InsertSummary => {
                if sel.is_empty() {
                    let id = doc.edit("Insert Summary Task", |p| p.add_task("<New Summary Task>", None));
                    st.sel.select_task(id);
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
                    .filter(|id| {
                        let i = doc.project.task_index(*id).unwrap();
                        let mut a = doc.project.parent_index(i);
                        while let Some(ai) = a {
                            if sel.contains(&doc.project.tasks[ai].id) {
                                return false;
                            }
                            a = doc.project.parent_index(ai);
                        }
                        true
                    })
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
                    doc.edit("Unlink Tasks", |p| p.unlink_tasks(&sel));
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
                    doc.edit("Inactivate Task", |p| {
                        let new = !p.task(sel[0]).map(|t| t.active).unwrap_or(true);
                        for id in &sel {
                            if let Some(t) = p.task_mut(*id) {
                                t.active = new;
                            }
                        }
                    });
                }
            }
            Command::SetMode(mode) => {
                if !sel.is_empty() {
                    doc.edit(if mode == ScheduleMode::Manual { "Manually Schedule" } else { "Auto Schedule" }, |p| {
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
                }
            }
            Command::TaskInfo | Command::TaskNotes => {
                if let Some(id) = st.sel.single_task().or(sel.first().copied()) {
                    self.dialogs.task_info = dialogs::task_info::TaskInfo::new(&doc.project, id, cmd == Command::TaskNotes);
                }
            }
            Command::ToggleTimeline => {
                if !sel.is_empty() {
                    doc.edit("Add to Timeline", |p| {
                        let new = !p.task(sel[0]).map(|t| t.show_on_timeline).unwrap_or(false);
                        for id in &sel {
                            if let Some(t) = p.task_mut(*id) {
                                t.show_on_timeline = new;
                            }
                        }
                    });
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
                    let idx = doc.project.task_index(*id).unwrap();
                    if let Some(r) = rows.iter().position(|&i| i == idx) {
                        st.task_grid.scroll_to = Some(r);
                    }
                }
            }
            Command::Copy | Command::Cut => {
                if !sel.is_empty() {
                    let mut copied = Vec::new();
                    for (i, t) in doc.project.tasks.iter().enumerate() {
                        let inside = sel.contains(&t.id) || {
                            let mut a = doc.project.parent_index(i);
                            let mut found = false;
                            while let Some(ai) = a {
                                if sel.contains(&doc.project.tasks[ai].id) {
                                    found = true;
                                    break;
                                }
                                a = doc.project.parent_index(ai);
                            }
                            found
                        };
                        if inside {
                            copied.push(t.clone());
                        }
                    }
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
                    let ids = doc.edit("Paste Tasks", |p| {
                        let base = clip.iter().map(|t| t.outline_level).min().unwrap_or(1);
                        let target_level = if at < p.tasks.len() { p.tasks[at].outline_level } else { 1 };
                        let mut ids = Vec::new();
                        for (k, t) in clip.iter().enumerate() {
                            let mut nt = t.clone();
                            nt.id = p.ids.next_task();
                            nt.predecessors.clear();
                            nt.members.clear();
                            nt.baseline = None;
                            nt.outline_level = target_level + (t.outline_level - base);
                            ids.push(nt.id);
                            p.tasks.insert(at + k, nt);
                        }
                        p.normalize_outline();
                        ids
                    });
                    st.sel.tasks = ids.into_iter().collect();
                }
            }
            Command::InsertResource => {
                let id = doc.edit("Add Resource", |p| {
                    let n = p.resources.len() + 1;
                    p.add_resource(format!("Resource {n}"))
                });
                st.sel.select_resource(id);
                if st.view != ViewKind::ResourceSheet {
                    st.view = ViewKind::ResourceSheet;
                }
                let row = doc.project.resources.len() - 1;
                st.resource_grid.begin_edit(row, 1, String::new());
                st.resource_grid.scroll_to = Some(row);
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
                    self.dialogs.resource_info = dialogs::resource_info::ResourceInfo::new(&doc.project, id);
                }
            }
            Command::AssignResources => self.dialogs.assign = Some(Default::default()),
            Command::LevelAll => {
                let r = doc.edit("Level Resources", |p| level_resources(p, &LevelingOptions::default()));
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
            Command::ClearLeveling => doc.edit("Clear Leveling", clear_leveling),
            Command::ProjectInfo | Command::Options => {
                self.dialogs.project_info = Some(dialogs::project_info::ProjectInfo::new(&doc.project))
            }
            Command::ChangeWorkingTime => self.dialogs.working_time = Some(dialogs::working_time::WorkingTime::new(&doc.project)),
            Command::SetBaseline => {
                doc.edit("Set Baseline", |p| p.set_baseline(&[]));
                st.toast(ToastKind::Success, "Baseline saved for all tasks");
            }
            Command::ClearBaseline => doc.edit("Clear Baseline", |p| p.clear_baseline(&[])),
            Command::UpdateProgressFromTime => {
                doc.edit("Update Progress from Time", |p| p.update_progress_from_time());
                st.toast(ToastKind::Success, "Progress updated from tracked time");
            }
            Command::ExternalLinks => self.dialogs.external = Some(Default::default()),
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
            _ => {}
        }
    }

    // ------------------------------------------------------------------
    // Layout pieces
    // ------------------------------------------------------------------

    fn ribbon_info(&self, ws: &Workspace) -> RibbonInfo {
        let doc = ws.active_doc();
        RibbonInfo {
            has_doc: doc.is_some(),
            title: doc.map(|d| d.title()).unwrap_or_default(),
            can_undo: doc.is_some_and(|d| d.can_undo()),
            can_redo: doc.is_some_and(|d| d.can_redo()),
            undo_label: doc.and_then(|d| d.undo_label().map(String::from)),
            task_selected: !self.st.sel.tasks.is_empty(),
            resource_selected: !self.st.sel.resources.is_empty(),
            mcp_running: self.mcp.running(),
            mcp_url: self.mcp.url(),
        }
    }

    fn doc_tabs(&mut self, ui: &mut Ui, ws: &mut Workspace, out: &mut Vec<Command>) {
        if ws.docs.len() < 2 {
            return;
        }
        let rect = ui.available_rect_before_wrap();
        let rect = Rect::from_min_size(rect.min, vec2(rect.width(), 26.0));
        ui.painter().rect_filled(rect, 0.0, theme::PANEL_BG);
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
                    self.st.backstage = true;
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
                if resp.on_hover_text("Click to change how new tasks are scheduled").clicked() {
                    doc.edit("New Task Mode", |p| p.settings.new_tasks_auto = !auto);
                }
                if self.st.filter != crate::state::TaskFilter::All {
                    ui.separator();
                    ui.label(RichText::new(format!("Filter: {}", self.st.filter.label())).size(12.0).color(theme::ACCENT_MID));
                }
                let warnings = doc.project.sched.warnings.len();
                if warnings > 0 {
                    ui.separator();
                    let r = ui.add(
                        egui::Label::new(RichText::new(format!("⚠ {warnings} scheduling issue(s)")).size(12.0).color(theme::ERROR))
                            .sense(Sense::click()),
                    );
                    if r.on_hover_text(doc.project.sched.warnings.join("\n")).clicked() {
                        out.push(Command::Statistics);
                    }
                }
                if let Some((label, ChangeSource::Agent)) = &doc.last_change {
                    ui.separator();
                    ui.label(RichText::new(format!("⚡ Last AI change: {label}")).size(12.0).color(theme::ACCENT_MID));
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
        p.rect_filled(rect, 0.0, theme::ACCENT_DARK);
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
                        ToastKind::Info => ("ℹ", theme::ACCENT_MID),
                        ToastKind::Success => ("✔", theme::OK),
                        ToastKind::Warning => ("⚠", theme::WARN),
                        ToastKind::Error => ("✖", theme::ERROR),
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
            let ws = self.ws.lock().unwrap();
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
                if ui.add(egui::Button::new(RichText::new("Save").color(Color32::WHITE)).fill(theme::ACCENT)).clicked() {
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
                let idx = self.ws.lock().unwrap().index_of(h);
                self.ws.lock().unwrap().active = idx;
                if self.save(false) {
                    self.close_doc(h);
                }
                self.confirm_close = None;
            }
            (Some(1), CloseRequest::Document(h)) => {
                self.close_doc(h);
                self.confirm_close = None;
            }
            (Some(0), CloseRequest::App) => {
                let dirty: Vec<usize> = self.ws.lock().unwrap().docs.iter().enumerate().filter(|(_, d)| d.dirty).map(|(i, _)| i).collect();
                let mut all = true;
                for i in dirty {
                    self.ws.lock().unwrap().active = Some(i);
                    all &= self.save(false);
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

    /// Developer automation used to capture screenshots of views and dialogs:
    /// `PLANCRAFT_SCREENSHOT=out.png [PLANCRAFT_SELECT=row] [PLANCRAFT_CMD=TaskInfo,...]`.
    fn screenshot_hook(&mut self, ctx: &egui::Context) {
        if self.shot.as_ref().is_some_and(|(_, f)| *f == 3) {
            let row: usize = std::env::var("PLANCRAFT_SELECT").ok().and_then(|v| v.parse().ok()).unwrap_or(3);
            if let Ok(ws) = self.ws.lock()
                && let Some(doc) = ws.active_doc()
            {
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
                    "Backstage" => {
                        self.st.backstage = true;
                        continue;
                    }
                    _ => continue,
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
            ui.label(RichText::new("No project open").size(18.0).color(theme::TEXT_WEAK));
            if ui.button("Create a new project").clicked() {
                out.push(Command::NewProject);
            }
            if ui.button("Open the sample project").clicked() {
                out.push(Command::NewDemo);
            }
        });
    }
}

impl eframe::App for PlanCraftApp {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        theme::BG.to_normalized_gamma_f32()
    }

    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.screenshot_hook(&ctx);
        let mut out: Vec<Command> = Vec::new();

        // Intercept window close when there are unsaved changes.
        if ctx.input(|i| i.viewport().close_requested()) && !self.allow_exit {
            let dirty = self.ws.lock().map(|w| w.docs.iter().any(|d| d.dirty)).unwrap_or(false);
            if dirty {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                self.confirm_close = Some(CloseRequest::App);
            } else {
                self.settings.table_width = self.st.table_width;
                self.settings.save();
                self.mcp.stop();
            }
        }

        self.shortcuts(&ctx, &mut out);

        let ws_arc = self.ws.clone();
        let mut ws = ws_arc.lock().unwrap();

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
                self.st.backstage = false;
            }
            self.st.sel = Default::default();
            self.st.gantt.initialized_for = None;
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

        let info = self.ribbon_info(&ws);
        let backstage_open = self.st.backstage;

        if backstage_open {
            let stats = ws.active_doc().map(|d| plancraft_core::report::project_stats(&d.project));
            let binfo = backstage::BackstageInfo {
                has_doc: info.has_doc,
                doc_title: ws.active_doc().map(|d| d.project.name.clone()).unwrap_or_default(),
                doc_path: ws.active_doc().and_then(|d| d.path.as_ref().map(|p| p.display().to_string())),
                stats,
                settings: &self.settings,
                can_close: info.has_doc,
            };
            egui::CentralPanel::no_frame().show(ui, |ui| {
                if backstage::show(ui, &mut self.backstage_page, &binfo, &mut out) {
                    self.st.backstage = false;
                }
            });
        } else {
            egui::Panel::top("title").frame(egui::Frame::new().fill(theme::BG)).show(ui, |ui| {
                ribbon::title_bar(ui, &info, &mut out);
                ribbon::tabs(ui, &mut self.st, &info, &mut out);
                if !self.st.ribbon_collapsed {
                    egui::Frame::new().fill(theme::BG).inner_margin(egui::Margin { left: 4, right: 4, top: 4, bottom: 2 }).show(ui, |ui| {
                        ui.set_min_width(ui.available_width());
                        ribbon::body(ui, &mut self.st, &info, &mut out);
                    });
                    let r = ui.max_rect();
                    ui.painter().hline(r.x_range(), ui.cursor().top(), Stroke::new(1.0, theme::GRID_LINE_STRONG));
                }
                self.doc_tabs(ui, &mut ws, &mut out);
            });
            egui::Panel::bottom("status")
                .frame(egui::Frame::new().fill(theme::HEADER_BG).inner_margin(egui::Margin::symmetric(10, 3)))
                .show(ui, |ui| {
                    self.status_bar(ui, &mut ws, &mut out);
                });
            egui::CentralPanel::no_frame().show(ui, |ui| {
                let full = ui.max_rect();
                ui.painter().rect_filled(full, 0.0, theme::BG);
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
        }

        // Dialogs.
        if let Some(doc) = ws.active_doc_mut() {
            if let Some(d) = &mut self.dialogs.task_info
                && !d.show(&ctx, doc)
            {
                self.dialogs.task_info = None;
            }
            if let Some(d) = &mut self.dialogs.resource_info
                && !d.show(&ctx, doc)
            {
                self.dialogs.resource_info = None;
            }
            if let Some(d) = &mut self.dialogs.project_info
                && !d.show(&ctx, doc)
            {
                self.dialogs.project_info = None;
            }
            if let Some(d) = &mut self.dialogs.working_time
                && !d.show(&ctx, doc)
            {
                self.dialogs.working_time = None;
            }
            if let Some(d) = &mut self.dialogs.assign {
                let mut toasts = Vec::new();
                if !d.show(&ctx, doc, &self.st.sel, &mut toasts) {
                    self.dialogs.assign = None;
                }
                for (k, t) in toasts {
                    self.st.toast(k, t);
                }
            }
            if let Some(d) = &mut self.dialogs.external
                && !d.show(&ctx, doc, &mut self.st)
            {
                self.dialogs.external = None;
            }
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
        self.confirm_dialog(&ctx);
        self.toasts(&ctx);

        for c in out {
            self.exec(&ctx, c);
        }
        let _ = NaiveDate::MIN;
    }
}
