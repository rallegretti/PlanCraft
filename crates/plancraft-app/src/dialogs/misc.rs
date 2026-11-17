//! Smaller dialogs: Assign Resources, Project Statistics, Links Between Projects, About, History.

use std::path::PathBuf;

use egui::{Context, RichText};
use plancraft_core::io;
use plancraft_core::prelude::*;
use plancraft_core::report;
use plancraft_core::workspace::{ChangeSource, Document};
use uuid::Uuid;

use crate::edit::EditExt;
use crate::format;
use crate::logo;
use crate::state::{Selection, ToastKind, UiState};
use crate::theme;

// ---------------------------------------------------------------------------
// Assign Resources
// ---------------------------------------------------------------------------

#[derive(Default)]
pub struct AssignResources {
    units: std::collections::HashMap<ResourceId, String>,
    filter: String,
}

impl AssignResources {
    pub fn show(&mut self, ctx: &Context, doc: &mut Document, sel: &Selection, st_toasts: &mut Vec<(ToastKind, String)>) -> bool {
        let mut open = true;
        let tasks: Vec<TaskId> =
            doc.project.tasks.iter().filter(|t| sel.tasks.contains(&t.id) && !t.sched.is_summary).map(|t| t.id).collect();
        egui::Window::new("Assign Resources").id(egui::Id::new("assign_resources"))
            .pivot(egui::Align2::CENTER_TOP)
            .default_pos(ctx.content_rect().center_top() + egui::vec2(0.0, 120.0)).open(&mut open).default_width(480.0).show(ctx, |ui| {
            let label = match tasks.as_slice() {
                [] => "Select one or more (non-summary) tasks.".to_string(),
                [one] => format!("Task: {}", doc.project.task(*one).map(|t| t.name.as_str()).unwrap_or("")),
                many => format!("{} tasks selected", many.len()),
            };
            ui.label(RichText::new(label).strong());
            ui.add(egui::TextEdit::singleline(&mut self.filter).hint_text("Filter resources…").desired_width(f32::INFINITY));
            ui.add_space(4.0);
            let mut actions: Vec<(ResourceId, Option<f64>)> = Vec::new();
            egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                egui::Grid::new("assign_grid").striped(true).num_columns(5).spacing([10.0, 6.0]).show(ui, |ui| {
                    for h in ["", "Resource Name", "Category", "Units", ""] {
                        ui.label(RichText::new(h).strong());
                    }
                    ui.end_row();
                    let p = &doc.project;
                    for r in &p.resources {
                        if !self.filter.is_empty() && !r.name.to_lowercase().contains(&self.filter.to_lowercase()) {
                            continue;
                        }
                        let assigned_all = !tasks.is_empty() && tasks.iter().all(|t| p.assignments.iter().any(|a| a.task == *t && a.resource == r.id));
                        ui.label(if assigned_all { RichText::new("✔").color(theme::pal().OK) } else { RichText::new("") });
                        ui.label(&r.name);
                        ui.label(RichText::new(r.category.label()).color(theme::pal().TEXT_WEAK));
                        let current = tasks.first().and_then(|t| p.assignments.iter().find(|a| a.task == *t && a.resource == r.id)).map(|a| format::units(a.units));
                        let text = self.units.entry(r.id).or_insert_with(|| current.unwrap_or_else(|| "100%".into()));
                        ui.add(egui::TextEdit::singleline(text).desired_width(56.0));
                        ui.horizontal(|ui| {
                            if ui.add_enabled(!tasks.is_empty(), egui::Button::new(if assigned_all { "Update" } else { "Assign" })).clicked() {
                                // Bad text is an error, never a removal (None means "remove").
                                match format::parse_units(text) {
                                    Some(u) => actions.push((r.id, Some(u))),
                                    None => st_toasts.push((ToastKind::Error, format!("'{}' is not a valid units value (try 50% or 100%)", text.trim()))),
                                }
                            }
                            if assigned_all && ui.button("Remove").clicked() {
                                actions.push((r.id, None));
                            }
                        });
                        ui.end_row();
                    }
                });
            });
            for (rid, units) in actions {
                let tasks = tasks.clone();
                let r = doc.try_edit(if units.is_some() { "Assign Resource" } else { "Remove Assignment" }, ChangeSource::User, |p| {
                    for t in &tasks {
                        match units {
                            Some(u) => {
                                p.assign(*t, rid, Some(u), None)?;
                            }
                            None => {
                                if let Some(a) = p.assignments.iter().find(|a| a.task == *t && a.resource == rid).map(|a| a.id) {
                                    p.unassign(a)?;
                                }
                            }
                        }
                    }
                    Ok::<(), PlanError>(())
                });
                if let Err(e) = r {
                    st_toasts.push((ToastKind::Error, e.to_string()));
                }
            }
            ui.add_space(4.0);
            ui.label(RichText::new("Tip: units are a percentage of the resource's time (100% = full time). Type new names in the Resource Names column to create resources on the fly.").small().color(theme::pal().TEXT_WEAK));
        });
        open
    }
}

// ---------------------------------------------------------------------------
// Project Statistics
// ---------------------------------------------------------------------------

pub fn statistics(ctx: &Context, doc: &Document, open: &mut bool) {
    let p = &doc.project;
    let s = report::project_stats(p);
    egui::Window::new(format!("Project Statistics for '{}'", p.name))
        .id(egui::Id::new("stats"))
        .pivot(egui::Align2::CENTER_TOP)
        .default_pos(ctx.content_rect().center_top() + egui::vec2(0.0, 120.0))
        .open(open)
        .default_width(560.0)
        .show(ctx, |ui| {
            egui::Grid::new("stats_dates").striped(true).num_columns(3).spacing([24.0, 6.0]).show(ui, |ui| {
                ui.label("");
                ui.label(RichText::new("Start").strong());
                ui.label(RichText::new("Finish").strong());
                ui.end_row();
                ui.label("Current");
                ui.label(format::date(s.start));
                ui.label(format::date(s.finish));
                ui.end_row();
                ui.label("Baseline");
                ui.label(format::date(p.tasks.iter().filter_map(|t| t.baseline.map(|b| b.start)).min()));
                ui.label(format::date(s.baseline_finish));
                ui.end_row();
                ui.label("Variance");
                ui.label("");
                ui.label(RichText::new(p.format_duration(s.finish_variance, false)).color(if s.finish_variance > 0 {
                    theme::pal().ERROR
                } else {
                    theme::pal().TEXT
                }));
                ui.end_row();
            });
            ui.separator();
            egui::Grid::new("stats_totals").striped(true).num_columns(4).spacing([24.0, 6.0]).show(ui, |ui| {
                ui.label(RichText::new("Duration").strong());
                ui.label(p.format_duration(s.duration, false));
                ui.label(RichText::new("Work").strong());
                ui.label(format!(
                    "{} ({} actual, {} remaining)",
                    format_work(s.work),
                    format_work(s.actual_work),
                    format_work(s.remaining_work)
                ));
                ui.end_row();
                ui.label(RichText::new("Cost").strong());
                ui.label(format::money(p, s.cost));
                ui.label(RichText::new("Baseline cost").strong());
                ui.label(format::money(p, s.baseline_cost));
                ui.end_row();
                ui.label(RichText::new("Complete").strong());
                ui.label(format::percent(s.percent_complete));
                ui.label(RichText::new("Tasks").strong());
                ui.label(format!(
                    "{} ({} done, {} in progress, {} late, {} critical)",
                    s.tasks, s.completed_tasks, s.in_progress_tasks, s.late_tasks, s.critical_tasks
                ));
                ui.end_row();
                ui.label(RichText::new("Milestones").strong());
                ui.label(s.milestones.to_string());
                ui.label(RichText::new("Resources").strong());
                ui.label(format!("{} ({} overallocated)", s.resources, s.overallocated_resources));
                ui.end_row();
            });
            if let Some(ev) = &s.earned_value {
                ui.separator();
                let against = plancraft_core::baseline::PlanSlot::Baseline(ev.baseline).name();
                ui.label(RichText::new(format!("Earned value (against {against})")).strong());
                egui::Grid::new("stats_ev").num_columns(4).spacing([24.0, 4.0]).show(ui, |ui| {
                    ui.label("Planned value (BCWS)");
                    ui.label(format::money(p, ev.bcws));
                    ui.label("Schedule perf. (SPI)");
                    ui.label(RichText::new(format!("{:.2}", ev.spi)).color(if ev.spi < 0.95 {
                        theme::pal().ERROR
                    } else {
                        theme::pal().OK
                    }));
                    ui.end_row();
                    ui.label("Earned value (BCWP)");
                    ui.label(format::money(p, ev.bcwp));
                    ui.label("Cost perf. (CPI)");
                    ui.label(RichText::new(format!("{:.2}", ev.cpi)).color(if ev.cpi < 0.95 {
                        theme::pal().ERROR
                    } else {
                        theme::pal().OK
                    }));
                    ui.end_row();
                    ui.label("Actual cost (ACWP)");
                    ui.label(format::money(p, ev.acwp));
                    ui.label("SV / CV");
                    ui.label(format!("{} / {}", format::money(p, ev.sv), format::money(p, ev.cv)));
                    ui.end_row();
                });
            } else {
                let against = plancraft_core::baseline::PlanSlot::Baseline(p.settings.earned_value_baseline).name();
                ui.label(
                    RichText::new(format!("Save {against} (Project > Set Baseline) to track earned value; variance uses Baseline."))
                        .small()
                        .color(theme::pal().TEXT_WEAK),
                );
            }
            if !s.warnings.is_empty() {
                ui.separator();
                ui.label(RichText::new("Scheduling warnings").strong().color(theme::pal().ERROR));
                for w in &s.warnings {
                    ui.label(RichText::new(format!("⚠ {w}")).color(theme::pal().ERROR));
                }
            }
        });
}

// ---------------------------------------------------------------------------
// Links Between Projects
// ---------------------------------------------------------------------------

#[derive(Default)]
pub struct ExternalLinks {
    selected: Option<Uuid>,
    tasks: Vec<ExternalTaskSnapshot>,
    loaded_for: Option<Uuid>,
    error: Option<String>,
    filter: String,
    /// "Add Project File…" was clicked: the app shows the file picker once the workspace is
    /// unlocked, then calls [`ExternalLinks::add_file`].
    pub pick_requested: bool,
    /// "Refresh All" was clicked: the app reads the linked files once the workspace is unlocked.
    pub refresh_requested: bool,
    /// The task list of this linked project is wanted: the app reads its file once the workspace
    /// is unlocked, then hands the tasks to [`ExternalLinks::tasks_loaded`].
    pub list_requested: Option<Uuid>,
    /// "Predecessor" or "Milestone member" was clicked: the app reads the linked file once the
    /// workspace is unlocked, then calls [`ExternalLinks::link_loaded`].
    pub link_requested: Option<LinkRequest>,
}

/// A link to an external task the user asked for in Links Between Projects.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LinkRequest {
    /// The linked project and its task.
    pub project: Uuid,
    pub task: TaskId,
    /// The task of this plan that gets the link.
    pub target: TaskId,
    /// A milestone member rather than a predecessor.
    pub member: bool,
}

impl ExternalLinks {
    /// The linked files were refreshed: show their tasks afresh.
    pub fn refreshed(&mut self) {
        self.loaded_for = None;
    }

    /// Links the project file picked for "Add Project File…", read by the app from `abs_path`
    /// (an absolute path) without the workspace lock.
    pub fn add_loaded(&mut self, doc: &mut Document, abs_path: &std::path::Path, read: io::FileResult<Project>) {
        let other = match read {
            Ok(other) => other,
            Err(e) => return self.error = Some(e.to_string()),
        };
        if other.id == doc.project.id {
            return self.error = Some("A plan can't link to itself: pick another plan's file.".into());
        }
        let doc_path = doc.path.clone();
        match doc.edit_msg("Add External Project", |p| Ok(io::add_loaded_external_project(p, doc_path.as_deref(), abs_path, &other))) {
            Ok(id) => {
                self.selected = Some(id);
                self.loaded_for = None;
                self.error = None;
            }
            Err(e) => self.error = Some(e),
        }
    }

    /// The tasks of linked project `project`, read by the app without the workspace lock.
    pub fn tasks_loaded(&mut self, project: Uuid, read: io::FileResult<Vec<ExternalTaskSnapshot>>) {
        if self.list_requested == Some(project) {
            self.list_requested = None;
        }
        match read {
            Ok(t) => {
                self.tasks = t;
                self.error = None;
            }
            Err(e) => {
                self.tasks.clear();
                self.error = Some(e.to_string());
            }
        }
        self.loaded_for = Some(project);
    }

    /// Makes the link asked for, with the linked document the app read without the workspace
    /// lock (`None`: the plan no longer knows where that project is stored).
    pub fn link_loaded(&mut self, doc: &mut Document, req: LinkRequest, read: Option<io::FileResult<Project>>, st: &mut UiState) {
        let other = match read {
            Some(Err(e)) => return self.error = Some(e.to_string()),
            Some(Ok(other)) => Some(other),
            None => None,
        };
        let r = doc.edit_msg("Link External Task", |p| {
            if let Some(other) = &other {
                io::ensure_external_task_from(p, req.project, other, req.task);
            }
            let r = TaskRef::External { project: req.project, task: req.task };
            if req.member { p.add_milestone_members(req.target, &[r]) } else { p.link(r, req.target, LinkType::FS, Lag::None) }
                .map_err(|e| e.to_string())
        });
        match r {
            Ok(()) => {
                self.error = None;
                st.toast(ToastKind::Success, "Linked to external task");
            }
            Err(e) => self.error = Some(e),
        }
    }

    pub fn show(&mut self, ctx: &Context, doc: &mut Document, st: &mut UiState) -> bool {
        let mut open = true;
        egui::Window::new("Links Between Projects").id(egui::Id::new("external_links"))
            .pivot(egui::Align2::CENTER_TOP)
            .default_pos(ctx.content_rect().center_top() + egui::vec2(0.0, 120.0)).open(&mut open).default_width(640.0).show(ctx, |ui| {
            ui.label("Tasks and milestones can depend on tasks stored in other PlanCraft files. Linked dates are cached in this file and refreshed when it opens.");
            if doc.path.is_none() {
                ui.colored_label(theme::pal().WARN, "Save this project first so links can be stored relative to its location.");
            }
            ui.horizontal(|ui| {
                if ui.button("➕ Add Project File…").clicked() {
                    self.pick_requested = true;
                }
                if ui.button("⟳ Refresh All").clicked() {
                    self.refresh_requested = true;
                }
            });
            ui.separator();
            ui.horizontal_top(|ui| {
                ui.vertical(|ui| {
                    ui.set_width(220.0);
                    ui.label(RichText::new("Linked projects").strong());
                    for e in &doc.project.external_projects {
                        let resp = ui.selectable_label(self.selected == Some(e.id), format!("📁 {}", e.name));
                        if resp.clicked() {
                            self.selected = Some(e.id);
                        }
                        resp.on_hover_text(format!("{}\nLast refreshed: {}", e.path, format::date_long(e.last_sync)));
                    }
                    if doc.project.external_projects.is_empty() {
                        ui.label(RichText::new("None yet").color(theme::pal().TEXT_WEAK));
                    }
                });
                ui.separator();
                ui.vertical(|ui| {
                    let Some(ext_id) = self.selected else {
                        ui.label("Select a linked project to browse its tasks.");
                        return;
                    };
                    if self.loaded_for != Some(ext_id) {
                        // The app reads the file once the workspace is unlocked.
                        if self.list_requested != Some(ext_id) {
                            self.list_requested = Some(ext_id);
                            self.tasks.clear();
                        }
                        ui.label(RichText::new("Reading the linked project…").color(theme::pal().TEXT_WEAK));
                        return;
                    }
                    let target = st.sel.single_task();
                    let target_name = target.and_then(|t| doc.project.task(t)).map(|t| t.name.clone());
                    ui.label(match &target_name {
                        Some(n) => format!("Link a task below to the selected task: '{n}'"),
                        None => "Select a task in the Gantt Chart to link to.".into(),
                    });
                    ui.add(egui::TextEdit::singleline(&mut self.filter).hint_text("Filter…").desired_width(f32::INFINITY));
                    let mut link: Option<(TaskId, bool)> = None;
                    egui::ScrollArea::vertical().max_height(300.0).show(ui, |ui| {
                        egui::Grid::new("ext_tasks").striped(true).num_columns(4).spacing([10.0, 4.0]).show(ui, |ui| {
                            for t in &self.tasks {
                                if !self.filter.is_empty() && !t.name.to_lowercase().contains(&self.filter.to_lowercase()) {
                                    continue;
                                }
                                ui.label(if t.milestone { format!("♦ {}", t.name) } else { t.name.clone() });
                                ui.label(RichText::new(format!("{} – {}", format::date(t.start), format::date(t.finish))).color(theme::pal().TEXT_WEAK));
                                if ui.add_enabled(target.is_some(), egui::Button::new("Predecessor")).on_hover_text("Selected task starts after this external task finishes").clicked() {
                                    link = Some((t.id, false));
                                }
                                let is_ms = target.and_then(|t| doc.project.task(t)).is_some_and(|t| t.is_milestone());
                                if ui.add_enabled(is_ms, egui::Button::new("Milestone member")).on_hover_text("Selected milestone requires this external task").clicked() {
                                    link = Some((t.id, true));
                                }
                                ui.end_row();
                            }
                        });
                    });
                    if let (Some((task, member)), Some(target)) = (link, target) {
                        // The app reads the linked file once the workspace is unlocked.
                        self.link_requested = Some(LinkRequest { project: ext_id, task, target, member });
                    }
                });
            });
            if let Some(e) = &self.error {
                ui.colored_label(theme::pal().ERROR, e);
            }
        });
        open
    }
}

// ---------------------------------------------------------------------------
// About & History
// ---------------------------------------------------------------------------

pub fn about(ctx: &Context, open: &mut bool) {
    egui::Window::new("About PlanCraft")
        .id(egui::Id::new("about"))
        .pivot(egui::Align2::CENTER_TOP)
        .default_pos(ctx.content_rect().center_top() + egui::vec2(0.0, 120.0))
        .open(open)
        .collapsible(false)
        .resizable(false)
        .default_width(380.0)
        .show(ctx, |ui| {
            ui.vertical_centered(|ui| {
                let (r, _) = ui.allocate_exact_size(egui::vec2(72.0, 72.0), egui::Sense::hover());
                logo::paint(ui.painter(), r);
                ui.heading("PlanCraft");
                ui.label(format!("Version {}", env!("CARGO_PKG_VERSION")));
                ui.add_space(6.0);
                ui.label("Project planning and scheduling, written entirely in Rust.");
                ui.label("Tasks · Milestones · Resources · Calendars · Workload · Time tracking");
                ui.label("Built-in MCP server for AI agents.");
            });
        });
}

pub fn history(ctx: &Context, doc: &mut Document, open: &mut bool) {
    egui::Window::new("Undo History")
        .id(egui::Id::new("history"))
        .pivot(egui::Align2::CENTER_TOP)
        .default_pos(ctx.content_rect().center_top() + egui::vec2(0.0, 120.0))
        .open(open)
        .default_width(320.0)
        .show(ctx, |ui| {
            let items = doc.history();
            if items.is_empty() {
                ui.label(RichText::new("No changes yet").color(theme::pal().TEXT_WEAK));
            }
            let mut undo_to = None;
            egui::ScrollArea::vertical().max_height(400.0).show(ui, |ui| {
                for (i, (label, src)) in items.iter().enumerate() {
                    let tag = match src {
                        plancraft_core::workspace::ChangeSource::Agent => RichText::new("⚡ AI").color(theme::pal().ACCENT_MID),
                        plancraft_core::workspace::ChangeSource::User => RichText::new("👤").color(theme::pal().TEXT_WEAK),
                    };
                    ui.horizontal(|ui| {
                        ui.label(tag);
                        if ui.link(label).on_hover_text("Undo back to before this change").clicked() {
                            undo_to = Some(i + 1);
                        }
                    });
                }
            });
            if let Some(n) = undo_to {
                for _ in 0..n {
                    doc.undo();
                }
            }
        });
}

/// Paths for recent files, stored in the user's config directory.
pub fn config_dir() -> Option<PathBuf> {
    plancraft_core::paths::config_dir()
}

#[cfg(test)]
mod tests {
    /// A plan can't be linked to itself (the MCP tool refuses it too).
    #[test]
    fn a_plan_cannot_link_to_itself() {
        let p = plancraft_core::model::Project::new("Self", plancraft_core::dates::ymd(2026, 10, 19).unwrap());
        let mut doc = Document::new(1, p.clone(), None);
        let mut d = ExternalLinks::default();
        d.add_loaded(&mut doc, std::path::Path::new("/tmp/self.plancraft"), Ok(p));
        assert!(d.error.as_deref().is_some_and(|e| e.contains("itself")), "{:?}", d.error);
        assert!(doc.project.external_projects.is_empty());
    }

    use super::*;
    use egui_kittest::kittest::Queryable;
    use plancraft_core::workspace::Workspace;

    /// Regression: unreadable Units text parsed as `None`, which the dialog treats as "remove",
    /// so clicking Update with a typo deleted the assignment.
    #[test]
    fn bad_units_text_reports_an_error_and_keeps_the_assignment() {
        let mut ws = Workspace::default();
        ws.add(plancraft_core::sample::demo_project(plancraft_core::dates::ymd(2026, 10, 19).unwrap()), None);
        let (task, res, name) = {
            let p = &ws.docs[0].project;
            let a = p.assignments.first().unwrap();
            (a.task, a.resource, p.resource(a.resource).unwrap().name.clone())
        };
        let assignments = ws.docs[0].project.assignments.len();
        let mut dialog = AssignResources { filter: name, ..Default::default() };
        dialog.units.insert(res, "abc".into());
        let sel = Selection { tasks: [task].into_iter().collect(), ..Default::default() };
        let state = (ws, dialog, sel, Vec::new());
        let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(1000.0, 800.0)).build_ui_state(
            |ui, (ws, dialog, sel, toasts): &mut (Workspace, AssignResources, Selection, Vec<(ToastKind, String)>)| {
                dialog.show(ui.ctx(), &mut ws.docs[0], sel, toasts);
            },
            state,
        );
        h.run();
        h.get_by_label("Update").click();
        h.run();
        let (ws, _, _, toasts) = h.state();
        assert_eq!(ws.docs[0].project.assignments.len(), assignments, "the assignment is kept");
        assert!(toasts.iter().any(|(k, m)| *k == ToastKind::Error && m.contains("abc")), "{toasts:?}");
    }

    /// Regression: "Add Project File…" opened the file picker while the app held the workspace
    /// lock, so every agent call waited until the picker closed. The dialog now only asks; the
    /// app shows the picker after unlocking.
    #[test]
    fn add_project_file_asks_the_app_for_the_picker() {
        let mut ws = Workspace::default();
        ws.add(plancraft_core::sample::demo_project(plancraft_core::dates::ymd(2026, 10, 19).unwrap()), None);
        let state = (ws, ExternalLinks::default(), UiState::default());
        let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(1000.0, 800.0)).build_ui_state(
            |ui, (ws, dialog, st): &mut (Workspace, ExternalLinks, UiState)| {
                dialog.show(ui.ctx(), &mut ws.docs[0], st);
            },
            state,
        );
        h.run();
        h.get_by_label_contains("Add Project File").click();
        h.run();
        let (ws, dialog, _) = h.state();
        assert!(dialog.pick_requested);
        assert!(ws.docs[0].history().is_empty());
    }

    /// Regression: the window read the linked file while the app held the workspace lock (to list
    /// its tasks, and again to link one). It now asks the app, which reads after unlocking.
    #[test]
    fn the_window_asks_the_app_to_read_linked_files() {
        let tmp = crate::recovery::tests::TempDir::new("links-window");
        let other_path = tmp.0.join("Other.plancraft");
        let mut other = Project::new("Other", plancraft_core::dates::ymd(2026, 10, 19).unwrap());
        let delivery = other.add_task("Supplier delivery", None);
        plancraft_core::schedule::recalculate(&mut other);
        io::save(&other, &other_path).unwrap();

        let mut ws = Workspace::default();
        ws.add(plancraft_core::sample::demo_project(plancraft_core::dates::ymd(2026, 10, 19).unwrap()), None);
        let target = ws.docs[0].project.tasks.iter().find(|t| !t.sched.is_summary).unwrap().id;
        let mut dialog = ExternalLinks::default();
        dialog.add_loaded(&mut ws.docs[0], &other_path, io::load(&other_path));
        assert_eq!(dialog.selected, Some(other.id), "{:?}", dialog.error);
        // The file goes away: the window must not try to read it itself.
        std::fs::remove_file(&other_path).unwrap();
        let mut st = UiState::default();
        st.sel.select_task(target);
        let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(1000.0, 800.0)).build_ui_state(
            |ui, (ws, dialog, st): &mut (Workspace, ExternalLinks, UiState)| {
                dialog.show(ui.ctx(), &mut ws.docs[0], st);
            },
            (ws, dialog, st),
        );
        h.run();
        {
            let (_, dialog, _) = h.state();
            assert_eq!(dialog.list_requested, Some(other.id), "the app is asked for the task list");
            assert!(dialog.error.is_none() && dialog.tasks.is_empty());
        }
        // The app hands over the tasks it read.
        h.state_mut().1.tasks_loaded(other.id, Ok(io::external_tasks_of(&other)));
        h.run();
        h.query_all_by_label("Predecessor").next().unwrap().click();
        h.run();
        let (ws, dialog, st) = h.state_mut();
        let req = dialog.link_requested.take().expect("the app is asked to read the file for the link");
        assert_eq!(req, LinkRequest { project: other.id, task: delivery, target, member: false });
        assert_eq!(ws.docs[0].history().len(), 1, "nothing linked yet (only the project file was added)");
        dialog.link_loaded(&mut ws.docs[0], req, Some(Ok(other.clone())), st);
        let p = &ws.docs[0].project;
        assert!(
            p.task(target).unwrap().predecessors.iter().any(|d| d.predecessor == TaskRef::External { project: other.id, task: delivery })
        );
        assert!(p.external(other.id).unwrap().task(delivery).is_some(), "its dates are cached");
        // A file that could not be read is reported, and nothing changes.
        let unreadable = io::load(&other_path);
        assert!(unreadable.is_err());
        dialog.link_loaded(&mut ws.docs[0], LinkRequest { member: true, ..req }, Some(unreadable), st);
        assert!(dialog.error.is_some());
        assert_eq!(ws.docs[0].history().len(), 2);
    }
}
