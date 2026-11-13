//! Set Baseline and Clear Baseline: save the schedule into Baseline or Baseline1–10, copy between
//! baselines and interim plans, and clear them (Microsoft Project's Project ▸ Set Baseline).

use egui::{Context, RichText, Ui};
use plancraft_core::baseline::{PlanSlot, Scope};
use plancraft_core::prelude::*;
use plancraft_core::workspace::Document;

use crate::edit::EditExt;
use crate::format;
use crate::state::{ToastKind, UiState};
use crate::theme;

/// A baseline's name with when it was last saved, e.g. "Baseline1 (last saved on 11/13/26)".
pub fn slot_label(p: &Project, slot: PlanSlot) -> String {
    let info = p.plan_info(slot);
    match (slot, info.tasks, info.saved) {
        (PlanSlot::Baseline(_), _, Some(saved)) => format!("{} (last saved on {})", slot.name(), format::date(Some(saved))),
        (PlanSlot::Interim(_), n, _) if n > 0 => format!("{} ({n} tasks)", slot.name()),
        _ => slot.name(),
    }
}

fn slot_combo(ui: &mut Ui, id: &str, p: &Project, value: &mut PlanSlot, choices: &[PlanSlot]) {
    egui::ComboBox::from_id_salt(id).width(250.0).selected_text(slot_label(p, *value)).show_ui(ui, |ui| {
        for &c in choices {
            ui.selectable_value(value, c, slot_label(p, c));
        }
    });
}

fn baselines() -> Vec<PlanSlot> {
    PlanSlot::baselines().collect()
}

fn interim_plans() -> Vec<PlanSlot> {
    PlanSlot::interim_plans().collect()
}

/// "For: Entire project / Selected tasks (n)".
fn scope_row(ui: &mut Ui, selected_only: &mut bool, n: usize) {
    ui.horizontal(|ui| {
        ui.label("For:");
        ui.radio_value(selected_only, false, "Entire project");
        ui.add_enabled_ui(n > 0, |ui| ui.radio_value(selected_only, true, format!("Selected tasks ({n})")));
    });
}

/// OK and Cancel, right-aligned. Returns (ok, cancel).
fn ok_cancel(ui: &mut Ui) -> (bool, bool) {
    let mut out = (false, false);
    ui.separator();
    let row = egui::vec2(ui.available_width(), 24.0);
    ui.allocate_ui_with_layout(row, egui::Layout::right_to_left(egui::Align::Center), |ui| {
        out.1 = ui.button("Cancel").clicked();
        out.0 = ui.add(egui::Button::new(RichText::new("  OK  ").color(egui::Color32::WHITE)).fill(theme::pal().ACCENT)).clicked();
    });
    out
}

fn note(ui: &mut Ui, text: impl Into<String>) {
    ui.label(RichText::new(text.into()).small().color(theme::pal().TEXT_WEAK));
}

pub struct SetBaseline {
    /// `false`: set a baseline; `true`: set an interim plan (copy between plans).
    interim: bool,
    baseline: PlanSlot,
    copy: PlanSlot,
    into: PlanSlot,
    selected_only: bool,
    to_summaries: bool,
    from_subtasks: bool,
    /// Tasks selected when the window opened (ids of this window's document).
    selection: Vec<TaskId>,
    error: Option<String>,
}

impl SetBaseline {
    pub fn new(selection: Vec<TaskId>) -> Self {
        Self {
            interim: false,
            baseline: PlanSlot::Baseline(0),
            copy: PlanSlot::Scheduled,
            into: PlanSlot::Interim(1),
            selected_only: false,
            to_summaries: false,
            from_subtasks: false,
            selection,
            error: None,
        }
    }

    /// Follows the selection in the window's document while it is open.
    pub fn set_selection(&mut self, selection: Vec<TaskId>) {
        self.selection = selection;
    }

    /// The source and target of the copy this window would make.
    fn plan(&self) -> (PlanSlot, PlanSlot) {
        if self.interim { (self.copy, self.into) } else { (PlanSlot::Scheduled, self.baseline) }
    }

    /// Saves as one undoable edit; returns the message to show.
    fn apply(&self, doc: &mut Document) -> Result<String, String> {
        let (copy, into) = self.plan();
        if self.selected_only && self.selection.is_empty() {
            return Err("No tasks were selected; choose Entire project or select tasks first.".into());
        }
        let rolls = !self.interim && self.selected_only && matches!(into, PlanSlot::Baseline(_));
        let scope = Scope {
            tasks: if self.selected_only { self.selection.clone() } else { Vec::new() },
            roll_up_to_summaries: rolls && self.to_summaries,
            roll_up_from_subtasks: rolls && self.from_subtasks,
        };
        let label = if self.interim { "Set Interim Plan" } else { "Set Baseline" };
        let n = doc.edit_msg(label, |p| p.save_plan(copy, into, &scope).map_err(|e| e.to_string()))?;
        let tasks = if n == 1 { "1 task".to_string() } else { format!("{n} tasks") };
        Ok(match copy {
            PlanSlot::Scheduled => format!("{} saved for {tasks}", into.name()),
            _ => format!("{} copied into {} for {tasks}", copy.name(), into.name()),
        })
    }

    pub fn show(&mut self, ctx: &Context, doc: &mut Document, st: &mut UiState) -> bool {
        let mut open = true;
        let mut close = false;
        egui::Window::new("Set Baseline")
            .id(egui::Id::new("set_baseline"))
            .pivot(egui::Align2::CENTER_TOP)
            .default_pos(ctx.content_rect().center_top() + egui::vec2(0.0, 140.0))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                let p = &doc.project;
                ui.radio_value(&mut self.interim, false, "Set baseline");
                ui.add_enabled_ui(!self.interim, |ui| {
                    ui.indent("baseline", |ui| slot_combo(ui, "sb_baseline", p, &mut self.baseline, &baselines()));
                });
                ui.add_space(4.0);
                ui.radio_value(&mut self.interim, true, "Set interim plan");
                ui.add_enabled_ui(self.interim, |ui| {
                    ui.indent("interim", |ui| {
                        egui::Grid::new("sb_interim").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
                            ui.label("Copy:");
                            let sources: Vec<PlanSlot> =
                                [PlanSlot::Scheduled].into_iter().chain(baselines()).chain(interim_plans()).collect();
                            slot_combo(ui, "sb_copy", p, &mut self.copy, &sources);
                            ui.end_row();
                            ui.label("Into:");
                            let targets: Vec<PlanSlot> = interim_plans().into_iter().chain(baselines()).collect();
                            slot_combo(ui, "sb_into", p, &mut self.into, &targets);
                            ui.end_row();
                        });
                    });
                });
                ui.add_space(6.0);
                scope_row(ui, &mut self.selected_only, self.selection.len());
                let (copy, into) = self.plan();
                let rolls = !self.interim && self.selected_only && matches!(into, PlanSlot::Baseline(_));
                ui.add_enabled_ui(rolls, |ui| {
                    ui.indent("rollup", |ui| {
                        ui.label("Roll up baselines:");
                        ui.checkbox(&mut self.to_summaries, "To all summary tasks");
                        ui.checkbox(&mut self.from_subtasks, "From subtasks into selected summary task(s)");
                    });
                });
                if matches!((copy, into), (PlanSlot::Interim(_), PlanSlot::Baseline(_))) {
                    ui.colored_label(theme::pal().ERROR, "An interim plan holds only dates, so it can't be copied into a baseline.");
                } else if copy == into {
                    ui.colored_label(theme::pal().ERROR, "Choose a different plan to copy into.");
                } else if copy != PlanSlot::Scheduled && p.plan_info(copy).tasks == 0 {
                    ui.colored_label(theme::pal().ERROR, format!("{} holds no values to copy.", copy.name()));
                } else if p.plan_info(into).tasks > 0 {
                    note(ui, format!("{} already holds values; they will be replaced (Undo restores them).", into.name()));
                }
                if !self.interim {
                    note(ui, "Variance is measured against Baseline. Keep earlier snapshots in Baseline1–10.");
                }
                if let Some(e) = &self.error {
                    ui.colored_label(theme::pal().ERROR, e);
                }
                let (ok, cancel) = ok_cancel(ui);
                close |= cancel;
                if ok {
                    match self.apply(doc) {
                        Ok(msg) => {
                            st.toast(ToastKind::Success, msg);
                            close = true;
                        }
                        Err(e) => self.error = Some(e),
                    }
                }
            });
        open && !close
    }
}

pub struct ClearBaseline {
    /// `false`: clear a baseline; `true`: clear an interim plan.
    interim: bool,
    baseline: PlanSlot,
    plan: PlanSlot,
    selected_only: bool,
    selection: Vec<TaskId>,
    error: Option<String>,
}

impl ClearBaseline {
    pub fn new(selection: Vec<TaskId>) -> Self {
        Self { interim: false, baseline: PlanSlot::Baseline(0), plan: PlanSlot::Interim(1), selected_only: false, selection, error: None }
    }

    /// Follows the selection in the window's document while it is open.
    pub fn set_selection(&mut self, selection: Vec<TaskId>) {
        self.selection = selection;
    }

    fn apply(&self, doc: &mut Document) -> Result<String, String> {
        if self.selected_only && self.selection.is_empty() {
            return Err("No tasks were selected; choose Entire project or select tasks first.".into());
        }
        let which = if self.interim { self.plan } else { self.baseline };
        let ids = if self.selected_only { self.selection.clone() } else { Vec::new() };
        let n = doc.edit_msg("Clear Baseline", |p| p.clear_plan(which, &ids).map_err(|e| e.to_string()))?;
        Ok(match n {
            0 => format!("{} held no values to clear", which.name()),
            1 => format!("{} cleared from 1 task", which.name()),
            n => format!("{} cleared from {n} tasks", which.name()),
        })
    }

    pub fn show(&mut self, ctx: &Context, doc: &mut Document, st: &mut UiState) -> bool {
        let mut open = true;
        let mut close = false;
        egui::Window::new("Clear Baseline")
            .id(egui::Id::new("clear_baseline"))
            .pivot(egui::Align2::CENTER_TOP)
            .default_pos(ctx.content_rect().center_top() + egui::vec2(0.0, 140.0))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                let p = &doc.project;
                egui::Grid::new("cb").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
                    ui.radio_value(&mut self.interim, false, "Clear baseline plan");
                    ui.add_enabled_ui(!self.interim, |ui| slot_combo(ui, "cb_baseline", p, &mut self.baseline, &baselines()));
                    ui.end_row();
                    ui.radio_value(&mut self.interim, true, "Clear interim plan");
                    ui.add_enabled_ui(self.interim, |ui| slot_combo(ui, "cb_interim", p, &mut self.plan, &interim_plans()));
                    ui.end_row();
                });
                ui.add_space(6.0);
                scope_row(ui, &mut self.selected_only, self.selection.len());
                if let Some(e) = &self.error {
                    ui.colored_label(theme::pal().ERROR, e);
                }
                let (ok, cancel) = ok_cancel(ui);
                close |= cancel;
                if ok {
                    match self.apply(doc) {
                        Ok(msg) => {
                            st.toast(ToastKind::Success, msg);
                            close = true;
                        }
                        Err(e) => self.error = Some(e),
                    }
                }
            });
        open && !close
    }
}
