//! Smaller dialogs: Assign Resources, Project Statistics, Links Between Projects, About, History.

use std::path::PathBuf;

use egui::{Context, RichText};
use plancraft_core::io;
use plancraft_core::prelude::*;
use plancraft_core::report;
use plancraft_core::workspace::{ChangeSource, Document};
use uuid::Uuid;

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
                        ui.label(if assigned_all { RichText::new("✔").color(theme::OK) } else { RichText::new("") });
                        ui.label(&r.name);
                        ui.label(RichText::new(r.category.label()).color(theme::TEXT_WEAK));
                        let current = tasks.first().and_then(|t| p.assignments.iter().find(|a| a.task == *t && a.resource == r.id)).map(|a| format::units(a.units));
                        let text = self.units.entry(r.id).or_insert_with(|| current.unwrap_or_else(|| "100%".into()));
                        ui.add(egui::TextEdit::singleline(text).desired_width(56.0));
                        ui.horizontal(|ui| {
                            if ui.add_enabled(!tasks.is_empty(), egui::Button::new(if assigned_all { "Update" } else { "Assign" })).clicked() {
                                actions.push((r.id, format::parse_units(text)));
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
            ui.label(RichText::new("Tip: units are a percentage of the resource's time (100% = full time). Type new names in the Resource Names column to create resources on the fly.").small().color(theme::TEXT_WEAK));
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
                    theme::ERROR
                } else {
                    theme::TEXT
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
                ui.label(RichText::new("Earned value (vs. baseline)").strong());
                egui::Grid::new("stats_ev").num_columns(4).spacing([24.0, 4.0]).show(ui, |ui| {
                    ui.label("Planned value (BCWS)");
                    ui.label(format::money(p, ev.bcws));
                    ui.label("Schedule perf. (SPI)");
                    ui.label(RichText::new(format!("{:.2}", ev.spi)).color(if ev.spi < 0.95 { theme::ERROR } else { theme::OK }));
                    ui.end_row();
                    ui.label("Earned value (BCWP)");
                    ui.label(format::money(p, ev.bcwp));
                    ui.label("Cost perf. (CPI)");
                    ui.label(RichText::new(format!("{:.2}", ev.cpi)).color(if ev.cpi < 0.95 { theme::ERROR } else { theme::OK }));
                    ui.end_row();
                    ui.label("Actual cost (ACWP)");
                    ui.label(format::money(p, ev.acwp));
                    ui.label("SV / CV");
                    ui.label(format!("{} / {}", format::money(p, ev.sv), format::money(p, ev.cv)));
                    ui.end_row();
                });
            } else {
                ui.label(
                    RichText::new("Set a baseline (Project > Set Baseline) to track variance and earned value.")
                        .small()
                        .color(theme::TEXT_WEAK),
                );
            }
            if !s.warnings.is_empty() {
                ui.separator();
                ui.label(RichText::new("Scheduling warnings").strong().color(theme::ERROR));
                for w in &s.warnings {
                    ui.label(RichText::new(format!("⚠ {w}")).color(theme::ERROR));
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
}

impl ExternalLinks {
    pub fn show(&mut self, ctx: &Context, doc: &mut Document, st: &mut UiState) -> bool {
        let mut open = true;
        egui::Window::new("Links Between Projects").id(egui::Id::new("external_links"))
            .pivot(egui::Align2::CENTER_TOP)
            .default_pos(ctx.content_rect().center_top() + egui::vec2(0.0, 120.0)).open(&mut open).default_width(640.0).show(ctx, |ui| {
            ui.label("Tasks and milestones can depend on tasks stored in other PlanCraft files. Linked dates are cached in this file and refreshed when it opens.");
            if doc.path.is_none() {
                ui.colored_label(theme::WARN, "Save this project first so links can be stored relative to its location.");
            }
            ui.horizontal(|ui| {
                if ui.button("➕ Add Project File…").clicked()
                    && let Some(path) = rfd::FileDialog::new().add_filter("PlanCraft project", &["plancraft"]).pick_file()
                {
                    let doc_path = doc.path.clone();
                    match doc.try_edit("Add External Project", ChangeSource::User, |p| io::add_external_project(p, doc_path.as_deref(), &path)) {
                        Ok(id) => {
                            self.selected = Some(id);
                            self.loaded_for = None;
                        }
                        Err(e) => self.error = Some(e.to_string()),
                    }
                }
                if ui.button("⟳ Refresh All").clicked() {
                    let w = doc.refresh_external();
                    if w.is_empty() {
                        st.toast(ToastKind::Success, "External links refreshed");
                    } else {
                        for m in w {
                            st.toast(ToastKind::Warning, m);
                        }
                    }
                    self.loaded_for = None;
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
                        ui.label(RichText::new("None yet").color(theme::TEXT_WEAK));
                    }
                });
                ui.separator();
                ui.vertical(|ui| {
                    let Some(ext_id) = self.selected else {
                        ui.label("Select a linked project to browse its tasks.");
                        return;
                    };
                    if self.loaded_for != Some(ext_id) {
                        match io::external_task_list(&doc.project, doc.path.as_deref(), ext_id) {
                            Ok(t) => {
                                self.tasks = t;
                                self.error = None;
                            }
                            Err(e) => {
                                self.tasks.clear();
                                self.error = Some(e.to_string());
                            }
                        }
                        self.loaded_for = Some(ext_id);
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
                                ui.label(RichText::new(format!("{} – {}", format::date(t.start), format::date(t.finish))).color(theme::TEXT_WEAK));
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
                    if let (Some((tid, member)), Some(target)) = (link, target) {
                        let doc_path = doc.path.clone();
                        let r = doc.try_edit("Link External Task", ChangeSource::User, |p| {
                            io::ensure_external_task(p, doc_path.as_deref(), ext_id, tid).map_err(|e| e.to_string())?;
                            let r = TaskRef::External { project: ext_id, task: tid };
                            if member { p.add_milestone_members(target, &[r]) } else { p.link(r, target, LinkType::FS, Lag::None) }.map_err(|e| e.to_string())
                        });
                        match r {
                            Ok(()) => st.toast(ToastKind::Success, "Linked to external task"),
                            Err(e) => self.error = Some(e),
                        }
                    }
                });
            });
            if let Some(e) = &self.error {
                ui.colored_label(theme::ERROR, e);
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
                ui.label(RichText::new("No changes yet").color(theme::TEXT_WEAK));
            }
            let mut undo_to = None;
            egui::ScrollArea::vertical().max_height(400.0).show(ui, |ui| {
                for (i, (label, src)) in items.iter().enumerate() {
                    let tag = match src {
                        plancraft_core::workspace::ChangeSource::Agent => RichText::new("⚡ AI").color(theme::ACCENT_MID),
                        plancraft_core::workspace::ChangeSource::User => RichText::new("👤").color(theme::TEXT_WEAK),
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
    if let Some(x) = std::env::var_os("XDG_CONFIG_HOME") {
        return Some(PathBuf::from(x).join("plancraft"));
    }
    if let Some(a) = std::env::var_os("APPDATA") {
        return Some(PathBuf::from(a).join("PlanCraft"));
    }
    std::env::var_os("HOME").map(|h| {
        let h = PathBuf::from(h);
        if cfg!(target_os = "macos") { h.join("Library/Application Support/PlanCraft") } else { h.join(".config/plancraft") }
    })
}
