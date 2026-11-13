//! Update Project: bring progress up to a date, or move work not done yet past it (Microsoft
//! Project's Project ▸ Update Project).

use chrono::NaiveDate;
use egui::{Context, RichText};
use plancraft_core::prelude::*;
use plancraft_core::workspace::Document;

use crate::edit::EditExt;
use crate::state::{ToastKind, UiState};
use crate::theme;

pub struct UpdateProject {
    /// `false`: update work as complete through `through`; `true`: reschedule uncompleted work.
    reschedule: bool,
    through: String,
    how: ProgressUpdate,
    after: String,
    selected_only: bool,
    /// Tasks selected when the window opened (ids of this window's document).
    selection: Vec<TaskId>,
    error: Option<String>,
}

/// Parses a date field; empty or "NA" means `default`.
fn parse_date(text: &str, default: NaiveDate) -> Result<NaiveDate, String> {
    let t = text.trim();
    if t.is_empty() || t.eq_ignore_ascii_case("na") {
        return Ok(default);
    }
    NaiveDate::parse_from_str(t, "%Y-%m-%d").map_err(|_| format!("'{t}' is not a date like YYYY-MM-DD"))
}

/// The date Update Project uses when none is typed: the status date, else today.
fn status_or_today(p: &Project) -> NaiveDate {
    p.status_date.unwrap_or_else(|| chrono::Local::now().date_naive())
}

impl UpdateProject {
    pub fn new(p: &Project, selection: Vec<TaskId>) -> Self {
        let d = status_or_today(p).to_string();
        Self {
            reschedule: false,
            through: d.clone(),
            how: ProgressUpdate::Scheduled,
            after: d,
            selected_only: false,
            selection,
            error: None,
        }
    }

    /// Applies the update as one undoable edit; returns the message to show.
    fn apply(&mut self, doc: &mut Document) -> Result<String, String> {
        let default = status_or_today(&doc.project);
        let ids: Vec<TaskId> = if self.selected_only { self.selection.clone() } else { Vec::new() };
        if self.selected_only && ids.is_empty() {
            return Err("No tasks were selected; choose Entire project or select tasks first.".into());
        }
        if self.reschedule {
            let date = parse_date(&self.after, default)?;
            let r = doc.edit_msg("Reschedule Uncompleted Work", |p| {
                let after = p.end_of_working_day(date);
                p.reschedule_uncompleted_work(&ids, after).map_err(|e| e.to_string())
            })?;
            Ok(describe(&r, &doc.project, format!("Work not done by {date} now starts after it")))
        } else {
            let date = parse_date(&self.through, default)?;
            let how = self.how;
            let r = doc.edit_msg("Update Project", |p| {
                let through = p.end_of_working_day(date);
                p.update_progress_through(&ids, through, how).map_err(|e| e.to_string())
            })?;
            Ok(describe(&r, &doc.project, format!("Progress updated through {date}")))
        }
    }

    pub fn show(&mut self, ctx: &Context, doc: &mut Document, st: &mut UiState) -> bool {
        let mut open = true;
        let mut close = false;
        egui::Window::new("Update Project")
            .id(egui::Id::new("update_project"))
            .pivot(egui::Align2::CENTER_TOP)
            .default_pos(ctx.content_rect().center_top() + egui::vec2(0.0, 140.0))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.radio_value(&mut self.reschedule, false, "Update work as complete through:");
                ui.add_enabled_ui(!self.reschedule, |ui| {
                    ui.indent("through", |ui| {
                        ui.add(egui::TextEdit::singleline(&mut self.through).desired_width(110.0).hint_text("YYYY-MM-DD"));
                        ui.radio_value(&mut self.how, ProgressUpdate::Scheduled, "Set 0% – 100% complete (as scheduled)");
                        ui.radio_value(&mut self.how, ProgressUpdate::ZeroOrHundred, "Set 0% or 100% complete only");
                    });
                });
                ui.add_space(4.0);
                ui.radio_value(&mut self.reschedule, true, "Reschedule uncompleted work to start after:");
                ui.add_enabled_ui(self.reschedule, |ui| {
                    ui.indent("after", |ui| {
                        ui.add(egui::TextEdit::singleline(&mut self.after).desired_width(110.0).hint_text("YYYY-MM-DD"));
                        ui.label(
                            RichText::new("Tasks in progress are split; tasks not started get a Start No Earlier Than date.")
                                .small()
                                .color(theme::pal().TEXT_WEAK),
                        );
                    });
                });
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.label("For:");
                    ui.radio_value(&mut self.selected_only, false, "Entire project");
                    let n = self.selection.len();
                    ui.add_enabled_ui(n > 0, |ui| ui.radio_value(&mut self.selected_only, true, format!("Selected tasks ({n})")));
                });
                ui.label(
                    RichText::new("Progress already entered is never lowered. Leave a date empty to use the status date (or today).")
                        .small()
                        .color(theme::pal().TEXT_WEAK),
                );
                if let Some(e) = &self.error {
                    ui.colored_label(theme::pal().ERROR, e);
                }
                ui.separator();
                let row = egui::vec2(ui.available_width(), 24.0);
                ui.allocate_ui_with_layout(row, egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Cancel").clicked() {
                        close = true;
                    }
                    if ui.add(egui::Button::new(RichText::new("  OK  ").color(egui::Color32::WHITE)).fill(theme::pal().ACCENT)).clicked() {
                        match self.apply(doc) {
                            Ok(msg) => {
                                st.toast(ToastKind::Success, msg);
                                close = true;
                            }
                            Err(e) => self.error = Some(e),
                        }
                    }
                });
            });
        open && !close
    }
}

/// One line saying what an update changed, e.g. "Progress updated through 2026-10-21: 3 tasks".
fn describe(r: &TrackingReport, p: &Project, what: String) -> String {
    let n = r.changed();
    let mut msg = match n {
        0 => format!("{what}: nothing needed changing"),
        1 => format!("{what}: 1 task"),
        n => format!("{what}: {n} tasks"),
    };
    if let Some((id, why)) = r.skipped.first() {
        let name = p.task(*id).map(|t| t.name.as_str()).unwrap_or("a task");
        let more = r.skipped.len().saturating_sub(1);
        msg += &format!(". Left '{name}' alone because {why}");
        if more > 0 {
            msg += &format!(" (and {more} more)");
        }
    }
    msg
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_or_na_dates_mean_the_status_date() {
        let d = NaiveDate::from_ymd_opt(2026, 10, 21).unwrap();
        assert_eq!(parse_date("", d), Ok(d));
        assert_eq!(parse_date(" NA ", d), Ok(d));
        assert_eq!(parse_date("2026-11-02", d), Ok(NaiveDate::from_ymd_opt(2026, 11, 2).unwrap()));
        assert!(parse_date("next week", d).is_err());
    }
}
