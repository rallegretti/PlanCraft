//! Project Information and Options dialog.

use chrono::NaiveDate;
use egui::{Context, RichText};
use plancraft_core::prelude::*;
use plancraft_core::workspace::{ChangeSource, Document};

use crate::format;
use crate::theme;

pub struct ProjectInfo {
    name: String,
    manager: String,
    company: String,
    author: String,
    notes: String,
    start: String,
    status_date: String,
    calendar: CalendarId,
    currency: String,
    hours_per_day: String,
    hours_per_week: String,
    days_per_month: String,
    hours_per_point: String,
    unit: DurationUnit,
    new_auto: bool,
    use_res_cal: bool,
    progress_from_time: bool,
    critical_slack: String,
    error: Option<String>,
}

impl ProjectInfo {
    pub fn new(p: &Project) -> Self {
        let s = &p.settings;
        Self {
            name: p.name.clone(),
            manager: p.manager.clone(),
            company: p.company.clone(),
            author: p.author.clone(),
            notes: p.notes.clone(),
            start: format::iso(Some(p.start)),
            status_date: p.status_date.map(|d| d.to_string()).unwrap_or_default(),
            calendar: p.calendar,
            currency: s.currency.clone(),
            hours_per_day: format!("{}", s.time.minutes_per_day as f64 / 60.0),
            hours_per_week: format!("{}", s.time.minutes_per_week as f64 / 60.0),
            days_per_month: s.time.days_per_month.to_string(),
            hours_per_point: format!("{}", s.minutes_per_point as f64 / 60.0),
            unit: s.duration_unit,
            new_auto: s.new_tasks_auto,
            use_res_cal: s.use_resource_calendars,
            progress_from_time: s.progress_from_time,
            critical_slack: p.format_duration(s.critical_slack, false),
            error: None,
        }
    }

    fn apply(&self, p: &mut Project) -> Result<(), String> {
        let num = |s: &str, what: &str| s.trim().parse::<f64>().map_err(|_| format!("{what} must be a number"));
        p.name = self.name.trim().to_string();
        p.manager = self.manager.clone();
        p.company = self.company.clone();
        p.author = self.author.clone();
        p.notes = self.notes.clone();
        p.start = p.parse_datetime(&self.start, false).ok_or("Start date must look like YYYY-MM-DD")?;
        p.status_date = if self.status_date.trim().is_empty() {
            None
        } else {
            Some(NaiveDate::parse_from_str(self.status_date.trim(), "%Y-%m-%d").map_err(|_| "Status date must look like YYYY-MM-DD")?)
        };
        p.calendar = self.calendar;
        let s = &mut p.settings;
        s.currency = self.currency.clone();
        s.time.minutes_per_day = (num(&self.hours_per_day, "Hours per day")? * 60.0).round() as i64;
        s.time.minutes_per_week = (num(&self.hours_per_week, "Hours per week")? * 60.0).round() as i64;
        s.time.days_per_month = num(&self.days_per_month, "Days per month")?.round() as i64;
        s.minutes_per_point = (num(&self.hours_per_point, "Hours per point")? * 60.0).round() as i64;
        s.duration_unit = self.unit;
        s.new_tasks_auto = self.new_auto;
        s.use_resource_calendars = self.use_res_cal;
        s.progress_from_time = self.progress_from_time;
        let ts = s.time.clone();
        s.critical_slack = parse_duration(&self.critical_slack, DurationUnit::Days, &ts).map(|(m, _)| m).ok_or("Invalid critical slack")?;
        if s.time.minutes_per_day <= 0 {
            return Err("Hours per day must be positive".into());
        }
        Ok(())
    }

    pub fn show(&mut self, ctx: &Context, doc: &mut Document) -> bool {
        let mut open = true;
        let mut close = false;
        egui::Window::new(format!("Project Information for '{}'", doc.project.name))
            .id(egui::Id::new("project_info"))
            .pivot(egui::Align2::CENTER_TOP)
            .default_pos(ctx.content_rect().center_top() + egui::vec2(0.0, 120.0))
            .open(&mut open)
            .collapsible(false)
            .default_width(600.0)
            .show(ctx, |ui| {
                egui::Grid::new("pi").num_columns(4).spacing([12.0, 8.0]).show(ui, |ui| {
                    ui.label("Project name:");
                    ui.text_edit_singleline(&mut self.name);
                    ui.label("Calendar:");
                    let cal_name = doc.project.calendar(self.calendar).map(|c| c.name.clone()).unwrap_or_default();
                    egui::ComboBox::from_id_salt("pi_cal").selected_text(cal_name).show_ui(ui, |ui| {
                        for c in &doc.project.calendars {
                            ui.selectable_value(&mut self.calendar, c.id, &c.name);
                        }
                    });
                    ui.end_row();
                    ui.label("Start date:");
                    ui.text_edit_singleline(&mut self.start);
                    ui.label("Status date:");
                    ui.add(egui::TextEdit::singleline(&mut self.status_date).hint_text("NA (today)"));
                    ui.end_row();
                    ui.label("Finish date:");
                    ui.label(RichText::new(format::date_long(doc.project.sched.finish)).color(theme::pal().TEXT_WEAK));
                    ui.label("Manager:");
                    ui.text_edit_singleline(&mut self.manager);
                    ui.end_row();
                    ui.label("Company:");
                    ui.text_edit_singleline(&mut self.company);
                    ui.label("Author:");
                    ui.text_edit_singleline(&mut self.author);
                    ui.end_row();
                });
                ui.label("Notes:");
                ui.add(egui::TextEdit::multiline(&mut self.notes).desired_rows(3).desired_width(f32::INFINITY));
                ui.add_space(6.0);
                egui::CollapsingHeader::new(RichText::new("Options: schedule, calendar & estimates").strong()).default_open(true).show(
                    ui,
                    |ui| {
                        egui::Grid::new("pi_opts").num_columns(4).spacing([12.0, 8.0]).show(ui, |ui| {
                            ui.label("Hours per day:");
                            ui.add(egui::TextEdit::singleline(&mut self.hours_per_day).desired_width(60.0));
                            ui.label("Hours per week:");
                            ui.add(egui::TextEdit::singleline(&mut self.hours_per_week).desired_width(60.0));
                            ui.end_row();
                            ui.label("Days per month:");
                            ui.add(egui::TextEdit::singleline(&mut self.days_per_month).desired_width(60.0));
                            ui.label("Hours per complexity point:");
                            ui.add(egui::TextEdit::singleline(&mut self.hours_per_point).desired_width(60.0));
                            ui.end_row();
                            ui.label("Duration shown in:");
                            egui::ComboBox::from_id_salt("pi_unit").selected_text(format!("{:?}", self.unit)).show_ui(ui, |ui| {
                                for u in [
                                    DurationUnit::Minutes,
                                    DurationUnit::Hours,
                                    DurationUnit::Days,
                                    DurationUnit::Weeks,
                                    DurationUnit::Months,
                                ] {
                                    ui.selectable_value(&mut self.unit, u, format!("{u:?}"));
                                }
                            });
                            ui.label("Currency symbol:");
                            ui.add(egui::TextEdit::singleline(&mut self.currency).desired_width(40.0));
                            ui.end_row();
                            ui.label("Critical if slack ≤");
                            ui.add(egui::TextEdit::singleline(&mut self.critical_slack).desired_width(60.0));
                            ui.end_row();
                        });
                        ui.checkbox(&mut self.new_auto, "New tasks are auto scheduled");
                        ui.checkbox(&mut self.use_res_cal, "Schedule assigned tasks on their resources' calendars (time off delays work)");
                        ui.checkbox(&mut self.progress_from_time, "Derive % complete from tracked time");
                    },
                );
                if let Some(e) = &self.error {
                    ui.colored_label(theme::pal().ERROR, e);
                }
                ui.separator();
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Cancel").clicked() {
                        close = true;
                    }
                    if ui.add(egui::Button::new(RichText::new("  OK  ").color(egui::Color32::WHITE)).fill(theme::pal().ACCENT)).clicked() {
                        match doc.try_edit("Project Information", ChangeSource::User, |p| self.apply(p)) {
                            Ok(()) => close = true,
                            Err(e) => self.error = Some(e),
                        }
                    }
                });
            });
        open && !close
    }
}
