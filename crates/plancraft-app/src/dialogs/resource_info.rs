//! Resource Information dialog: general details, availability and time off.

use chrono::NaiveDate;
use egui::{Context, RichText};
use plancraft_core::prelude::*;
use plancraft_core::workspace::{ChangeSource, Document};

use crate::format;
use crate::theme;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    General,
    TimeOff,
    Availability,
    Notes,
}

pub struct ResourceInfo {
    id: ResourceId,
    tab: Tab,
    draft: Resource,
    max_units: String,
    rate: String,
    ovt: String,
    per_use: String,
    new_off_name: String,
    new_off_start: String,
    new_off_end: String,
    new_off_hours: String,
    new_avail_from: String,
    new_avail_to: String,
    new_avail_units: String,
    error: Option<String>,
}

impl ResourceInfo {
    pub fn new(p: &Project, id: ResourceId) -> Option<Self> {
        let r = p.resource(id)?.clone();
        Some(Self {
            id,
            tab: Tab::General,
            max_units: format::units(r.max_units),
            rate: format!("{:.2}", r.standard_rate),
            ovt: format!("{:.2}", r.overtime_rate),
            per_use: format!("{:.2}", r.cost_per_use),
            draft: r,
            new_off_name: "Vacation".into(),
            new_off_start: String::new(),
            new_off_end: String::new(),
            new_off_hours: String::new(),
            new_avail_from: String::new(),
            new_avail_to: String::new(),
            new_avail_units: "50%".into(),
            error: None,
        })
    }

    pub fn show(&mut self, ctx: &Context, doc: &mut Document) -> bool {
        let mut open = true;
        let mut close = false;
        egui::Window::new("Resource Information")
            .id(egui::Id::new("resource_info"))
            .open(&mut open)
            .collapsible(false)
            .default_width(560.0)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    for (t, l) in [(Tab::General, "General"), (Tab::TimeOff, "Time Off & Working Time"), (Tab::Availability, "Availability"), (Tab::Notes, "Notes")] {
                        ui.selectable_value(&mut self.tab, t, l);
                    }
                });
                ui.separator();
                let r = &mut self.draft;
                match self.tab {
                    Tab::General => {
                        egui::Grid::new("ri_gen").num_columns(4).spacing([12.0, 8.0]).show(ui, |ui| {
                            ui.label("Resource name:");
                            ui.text_edit_singleline(&mut r.name);
                            ui.label("Initials:");
                            ui.add(egui::TextEdit::singleline(&mut r.initials).desired_width(60.0));
                            ui.end_row();
                            ui.label("Email:");
                            ui.text_edit_singleline(&mut r.email);
                            ui.label("Group:");
                            ui.add(egui::TextEdit::singleline(&mut r.group).desired_width(120.0));
                            ui.end_row();
                            ui.label("Type:");
                            egui::ComboBox::from_id_salt("ri_type")
                                .selected_text(match r.kind {
                                    ResourceType::Work => "Work",
                                    ResourceType::Material => "Material",
                                    ResourceType::Cost => "Cost",
                                })
                                .show_ui(ui, |ui| {
                                    ui.selectable_value(&mut r.kind, ResourceType::Work, "Work");
                                    ui.selectable_value(&mut r.kind, ResourceType::Material, "Material");
                                    ui.selectable_value(&mut r.kind, ResourceType::Cost, "Cost");
                                });
                            ui.label("Category:");
                            egui::ComboBox::from_id_salt("ri_cat").selected_text(r.category.label()).show_ui(ui, |ui| {
                                for c in ResourceCategory::ALL {
                                    ui.selectable_value(&mut r.category, c, c.label());
                                }
                            });
                            ui.end_row();
                            if r.kind == ResourceType::Material {
                                ui.label("Material label:");
                                ui.add(egui::TextEdit::singleline(&mut r.material_label).desired_width(80.0).hint_text("e.g. tons"));
                                ui.end_row();
                            }
                            ui.label("Max. units:");
                            ui.add_enabled(r.kind == ResourceType::Work, egui::TextEdit::singleline(&mut self.max_units).desired_width(70.0));
                            ui.label("Base calendar:");
                            let cal_name = r.calendar.and_then(|c| doc.project.calendar(c)).map(|c| c.name.clone()).unwrap_or_else(|| "(Project calendar)".into());
                            egui::ComboBox::from_id_salt("ri_cal").selected_text(cal_name).show_ui(ui, |ui| {
                                ui.selectable_value(&mut r.calendar, None, "(Project calendar)");
                                for c in &doc.project.calendars {
                                    ui.selectable_value(&mut r.calendar, Some(c.id), &c.name);
                                }
                            });
                            ui.end_row();
                            ui.label("Standard rate:");
                            ui.add(egui::TextEdit::singleline(&mut self.rate).desired_width(80.0));
                            ui.label("Overtime rate:");
                            ui.add(egui::TextEdit::singleline(&mut self.ovt).desired_width(80.0));
                            ui.end_row();
                            ui.label("Cost/use:");
                            ui.add(egui::TextEdit::singleline(&mut self.per_use).desired_width(80.0));
                            ui.end_row();
                        });
                        ui.label(RichText::new("Max. units: 100% is one full-time person; a team of three is 300%. Rates are per hour for work resources and per unit for materials.").small().color(theme::TEXT_WEAK));
                    }
                    Tab::TimeOff => {
                        ui.label("Personal exceptions to the base calendar: vacations, leave, part-time days or extra hours.");
                        let mut remove = None;
                        egui::Grid::new("ri_off").striped(true).num_columns(5).spacing([10.0, 4.0]).show(ui, |ui| {
                            for h in ["Name", "Start", "Finish", "Working times", ""] {
                                ui.label(RichText::new(h).strong());
                            }
                            ui.end_row();
                            for (i, e) in r.exceptions.iter().enumerate() {
                                ui.label(&e.name);
                                ui.label(format::day(e.start));
                                ui.label(format::day(e.end));
                                ui.label(if e.working.is_empty() { "Nonworking".into() } else { e.working.iter().map(|r| r.to_string()).collect::<Vec<_>>().join(", ") });
                                if ui.small_button("✖").clicked() {
                                    remove = Some(i);
                                }
                                ui.end_row();
                            }
                        });
                        if let Some(i) = remove {
                            r.exceptions.remove(i);
                        }
                        ui.separator();
                        ui.horizontal(|ui| {
                            ui.add(egui::TextEdit::singleline(&mut self.new_off_name).desired_width(100.0).hint_text("Name"));
                            ui.add(egui::TextEdit::singleline(&mut self.new_off_start).desired_width(90.0).hint_text("YYYY-MM-DD"));
                            ui.label("to");
                            ui.add(egui::TextEdit::singleline(&mut self.new_off_end).desired_width(90.0).hint_text("YYYY-MM-DD"));
                            ui.add(egui::TextEdit::singleline(&mut self.new_off_hours).desired_width(130.0).hint_text("hours (empty = off)"));
                            if ui.button("Add").clicked() {
                                let parse = |s: &str| NaiveDate::parse_from_str(s.trim(), "%Y-%m-%d").ok();
                                match (parse(&self.new_off_start), parse(&self.new_off_end).or(parse(&self.new_off_start))) {
                                    (Some(s), Some(e)) if e >= s => {
                                        let working: Option<Vec<TimeRange>> =
                                            if self.new_off_hours.trim().is_empty() { Some(vec![]) } else { self.new_off_hours.split(',').map(TimeRange::parse).collect() };
                                        match working {
                                            Some(w) => {
                                                r.exceptions.push(CalendarException { name: self.new_off_name.clone(), start: s, end: e, working: w, yearly: false });
                                                self.error = None;
                                            }
                                            None => self.error = Some("Working times look like 08:00-12:00, 13:00-17:00".into()),
                                        }
                                    }
                                    _ => self.error = Some("Enter a start and end date as YYYY-MM-DD".into()),
                                }
                            }
                        });
                    }
                    Tab::Availability => {
                        ui.label("Override the maximum units for date ranges (e.g. 50% while on another project).");
                        let mut remove = None;
                        egui::Grid::new("ri_av").striped(true).num_columns(4).spacing([10.0, 4.0]).show(ui, |ui| {
                            for h in ["Available From", "Available To", "Units", ""] {
                                ui.label(RichText::new(h).strong());
                            }
                            ui.end_row();
                            for (i, a) in r.availability.iter().enumerate() {
                                ui.label(a.from.map(format::day).unwrap_or_else(|| "NA".into()));
                                ui.label(a.to.map(format::day).unwrap_or_else(|| "NA".into()));
                                ui.label(format::units(a.units));
                                if ui.small_button("✖").clicked() {
                                    remove = Some(i);
                                }
                                ui.end_row();
                            }
                        });
                        if let Some(i) = remove {
                            r.availability.remove(i);
                        }
                        ui.horizontal(|ui| {
                            ui.add(egui::TextEdit::singleline(&mut self.new_avail_from).desired_width(90.0).hint_text("from (opt.)"));
                            ui.add(egui::TextEdit::singleline(&mut self.new_avail_to).desired_width(90.0).hint_text("to (opt.)"));
                            ui.add(egui::TextEdit::singleline(&mut self.new_avail_units).desired_width(60.0));
                            if ui.button("Add").clicked() {
                                let parse = |s: &str| if s.trim().is_empty() { Ok(None) } else { NaiveDate::parse_from_str(s.trim(), "%Y-%m-%d").map(Some) };
                                match (parse(&self.new_avail_from), parse(&self.new_avail_to), format::parse_units(&self.new_avail_units)) {
                                    (Ok(f), Ok(t), Some(u)) => r.availability.push(AvailabilityPeriod { from: f, to: t, units: u }),
                                    _ => self.error = Some("Dates are YYYY-MM-DD, units like 50%".into()),
                                }
                            }
                        });
                    }
                    Tab::Notes => {
                        ui.add(egui::TextEdit::multiline(&mut r.notes).desired_rows(12).desired_width(f32::INFINITY));
                    }
                }
                if let Some(e) = &self.error {
                    ui.colored_label(theme::ERROR, e);
                }
                ui.separator();
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Cancel").clicked() {
                        close = true;
                    }
                    if ui.add(egui::Button::new(RichText::new("  OK  ").color(egui::Color32::WHITE)).fill(theme::ACCENT)).clicked() {
                        let mut draft = self.draft.clone();
                        let parsed = (|| -> Result<(), String> {
                            draft.max_units = format::parse_units(&self.max_units).ok_or("Max. units must look like 100%")?;
                            draft.standard_rate = format::parse_money(&self.rate).ok_or("Invalid standard rate")?;
                            draft.overtime_rate = format::parse_money(&self.ovt).ok_or("Invalid overtime rate")?;
                            draft.cost_per_use = format::parse_money(&self.per_use).ok_or("Invalid cost/use")?;
                            if draft.name.trim().is_empty() {
                                return Err("The resource name cannot be empty".into());
                            }
                            Ok(())
                        })();
                        match parsed {
                            Err(e) => self.error = Some(e),
                            Ok(()) => {
                                let id = self.id;
                                let r = doc.try_edit(&format!("Resource Information: {}", draft.name), ChangeSource::User, |p| {
                                    let slot = p.resource_mut(id).ok_or("resource was deleted")?;
                                    *slot = draft;
                                    Ok::<(), String>(())
                                });
                                match r {
                                    Ok(()) => close = true,
                                    Err(e) => self.error = Some(e),
                                }
                            }
                        }
                    }
                });
            });
        open && !close
    }
}
