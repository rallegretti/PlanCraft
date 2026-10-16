//! Change Working Time: calendars, work weeks and exceptions.

use chrono::{Datelike, Duration, NaiveDate};
use egui::{Align2, Context, FontId, Rect, RichText, Sense, Stroke, Ui, pos2, vec2};
use plancraft_core::calendar::{WEEKDAYS, WorkCal};
use plancraft_core::prelude::*;
use plancraft_core::workspace::{ChangeSource, Document};

use crate::format;
use crate::theme;

pub struct WorkingTime {
    calendar: CalendarId,
    month: NaiveDate,
    selected_day: Option<NaiveDate>,
    tab: u8,
    week_text: Vec<String>,
    week_loaded_for: Option<CalendarId>,
    ex_name: String,
    ex_start: String,
    ex_end: String,
    ex_hours: String,
    ex_yearly: bool,
    new_cal_name: String,
    error: Option<String>,
}

impl WorkingTime {
    pub fn new(p: &Project) -> Self {
        let today = chrono::Local::now().date_naive();
        Self {
            calendar: p.calendar,
            month: today.with_day(1).unwrap_or(today),
            selected_day: Some(today),
            tab: 0,
            week_text: Vec::new(),
            week_loaded_for: None,
            ex_name: "Holiday".into(),
            ex_start: String::new(),
            ex_end: String::new(),
            ex_hours: String::new(),
            ex_yearly: false,
            new_cal_name: String::new(),
            error: None,
        }
    }

    fn load_week(&mut self, cal: &Calendar) {
        self.week_text = WEEKDAYS.iter().map(|wd| cal.week.day(*wd).iter().map(|r| r.to_string()).collect::<Vec<_>>().join(", ")).collect();
        self.week_loaded_for = Some(cal.id);
    }

    fn month_view(&mut self, ui: &mut Ui, cal: &Calendar) {
        let wc = WorkCal::single(cal);
        ui.horizontal(|ui| {
            if ui.small_button("⏴").clicked() {
                self.month = (self.month - Duration::days(1)).with_day(1).unwrap_or(self.month);
            }
            ui.label(RichText::new(self.month.format("%B %Y").to_string()).strong());
            if ui.small_button("⏵").clicked() {
                self.month = (self.month + Duration::days(32)).with_day(1).unwrap_or(self.month);
            }
        });
        let cell = 30.0;
        let (rect, _) = ui.allocate_exact_size(vec2(cell * 7.0, cell * 7.0), Sense::hover());
        let painter = ui.painter_at(rect);
        for (i, d) in ["M", "T", "W", "T", "F", "S", "S"].iter().enumerate() {
            painter.text(
                pos2(rect.left() + (i as f32 + 0.5) * cell, rect.top() + cell / 2.0),
                Align2::CENTER_CENTER,
                *d,
                FontId::proportional(12.0),
                theme::pal().TEXT_WEAK,
            );
        }
        let first = self.month - Duration::days(self.month.weekday().num_days_from_monday() as i64);
        for k in 0..42 {
            let day = first + Duration::days(k);
            let r = Rect::from_min_size(
                pos2(rect.left() + (k % 7) as f32 * cell, rect.top() + (k / 7 + 1) as f32 * cell),
                vec2(cell - 2.0, cell - 2.0),
            );
            if r.bottom() > rect.bottom() + 1.0 {
                continue;
            }
            let is_exception = cal.exceptions.iter().any(|e| e.covers(day));
            let working = wc.is_working_day(day);
            let fill = if is_exception {
                if working { theme::pal().EXCEPTION_WORKING } else { theme::pal().EXCEPTION_OFF }
            } else if working {
                theme::pal().BG
            } else {
                theme::pal().NONWORKING
            };
            painter.rect_filled(r, 3.0, fill);
            let color = if day.month() == self.month.month() { theme::pal().TEXT } else { theme::pal().GRID_LINE_STRONG };
            painter.text(r.center(), Align2::CENTER_CENTER, day.day().to_string(), FontId::proportional(12.0), color);
            if self.selected_day == Some(day) {
                painter.rect_stroke(r, 3.0, Stroke::new(2.0, theme::pal().ACCENT), egui::StrokeKind::Inside);
            }
            let resp = ui.interact(r, egui::Id::new(("wt_day", day)), Sense::click());
            if resp.clicked() {
                self.selected_day = Some(day);
                self.ex_start = day.to_string();
                self.ex_end = day.to_string();
            }
        }
        if let Some(d) = self.selected_day {
            let ranges = wc.ranges(d);
            let txt = if ranges.is_empty() {
                "Nonworking".to_string()
            } else {
                ranges.iter().map(|r| r.to_string()).collect::<Vec<_>>().join(", ")
            };
            ui.label(RichText::new(format!("{}: {txt}", format::day(d))).small());
            if let Some(e) = cal.exceptions.iter().rev().find(|e| e.covers(d)) {
                ui.label(RichText::new(format!("Exception: {}", e.name)).small().color(theme::pal().ACCENT_MID));
            }
        }
        ui.horizontal(|ui| {
            for (c, l) in [
                (theme::pal().BG, "Working"),
                (theme::pal().NONWORKING, "Nonworking"),
                (theme::pal().EXCEPTION_OFF, "Exception (off)"),
                (theme::pal().EXCEPTION_WORKING, "Exception (hours)"),
            ] {
                let (r, _) = ui.allocate_exact_size(vec2(12.0, 12.0), Sense::hover());
                ui.painter().rect_filled(r, 2.0, c);
                ui.painter().rect_stroke(r, 2.0, Stroke::new(1.0, theme::pal().GRID_LINE_STRONG), egui::StrokeKind::Inside);
                ui.label(RichText::new(l).small());
            }
        });
    }

    pub fn show(&mut self, ctx: &Context, doc: &mut Document) -> bool {
        let mut open = true;
        let mut close = false;
        if doc.project.calendar(self.calendar).is_none() {
            self.calendar = doc.project.calendar;
        }
        let Some(cal) = doc.project.calendar(self.calendar).cloned() else {
            return false;
        };
        if self.week_loaded_for != Some(cal.id) {
            self.load_week(&cal);
        }
        egui::Window::new("Change Working Time")
            .id(egui::Id::new("working_time"))
            .pivot(egui::Align2::CENTER_TOP)
            .default_pos(ctx.content_rect().center_top() + egui::vec2(0.0, 120.0))
            .open(&mut open)
            .collapsible(false)
            .default_width(760.0)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label("For calendar:");
                    egui::ComboBox::from_id_salt("wt_cal").selected_text(&cal.name).width(180.0).show_ui(ui, |ui| {
                        for c in &doc.project.calendars {
                            let label =
                                if c.id == doc.project.calendar { format!("{} (Project Calendar)", c.name) } else { c.name.clone() };
                            ui.selectable_value(&mut self.calendar, c.id, label);
                        }
                    });
                    if cal.id != doc.project.calendar && ui.button("Use as Project Calendar").clicked() {
                        let id = cal.id;
                        if let Err(e) = doc.edit("Set Project Calendar", |p| p.calendar = id) {
                            self.error = Some(e.to_string());
                        }
                    }
                    ui.separator();
                    ui.add(egui::TextEdit::singleline(&mut self.new_cal_name).hint_text("new calendar name").desired_width(140.0));
                    if ui.button("Create (copy)").clicked() && !self.new_cal_name.trim().is_empty() {
                        let name = self.new_cal_name.trim().to_string();
                        let from = cal.id;
                        match doc.try_edit("Create Calendar", ChangeSource::User, |p| p.add_calendar(name, Some(from))) {
                            Ok(id) => {
                                self.calendar = id;
                                self.new_cal_name.clear();
                            }
                            Err(e) => self.error = Some(e.to_string()),
                        }
                    }
                });
                ui.separator();
                ui.horizontal_top(|ui| {
                    ui.vertical(|ui| {
                        ui.set_width(230.0);
                        self.month_view(ui, &cal);
                    });
                    ui.separator();
                    ui.vertical(|ui| {
                        ui.horizontal(|ui| {
                            ui.selectable_value(&mut self.tab, 0, "Exceptions");
                            ui.selectable_value(&mut self.tab, 1, "Work Week");
                        });
                        ui.separator();
                        if self.tab == 0 {
                            let mut remove = None;
                            egui::ScrollArea::vertical().max_height(220.0).show(ui, |ui| {
                                egui::Grid::new("wt_ex").striped(true).num_columns(5).spacing([10.0, 4.0]).show(ui, |ui| {
                                    for h in ["Name", "Start", "Finish", "Working times", ""] {
                                        ui.label(RichText::new(h).strong());
                                    }
                                    ui.end_row();
                                    for (i, e) in cal.exceptions.iter().enumerate() {
                                        ui.label(if e.yearly { format!("{} (yearly)", e.name) } else { e.name.clone() });
                                        ui.label(format::day(e.start));
                                        ui.label(format::day(e.end));
                                        ui.label(if e.working.is_empty() {
                                            "Nonworking".into()
                                        } else {
                                            e.working.iter().map(|r| r.to_string()).collect::<Vec<_>>().join(", ")
                                        });
                                        if ui.small_button("✖").clicked() {
                                            remove = Some(i);
                                        }
                                        ui.end_row();
                                    }
                                });
                            });
                            if let Some(i) = remove {
                                let id = cal.id;
                                let r = doc.edit("Remove Calendar Exception", |p| {
                                    if let Some(c) = p.calendars.iter_mut().find(|c| c.id == id)
                                        && i < c.exceptions.len()
                                    {
                                        c.exceptions.remove(i);
                                    }
                                });
                                if let Err(e) = r {
                                    self.error = Some(e.to_string());
                                }
                            }
                            ui.separator();
                            ui.horizontal(|ui| {
                                ui.add(egui::TextEdit::singleline(&mut self.ex_name).desired_width(110.0).hint_text("Name"));
                                ui.add(egui::TextEdit::singleline(&mut self.ex_start).desired_width(86.0).hint_text("YYYY-MM-DD"));
                                ui.label("–");
                                ui.add(egui::TextEdit::singleline(&mut self.ex_end).desired_width(86.0).hint_text("YYYY-MM-DD"));
                            });
                            ui.horizontal(|ui| {
                                ui.add(
                                    egui::TextEdit::singleline(&mut self.ex_hours)
                                        .desired_width(190.0)
                                        .hint_text("working times (empty = nonworking)"),
                                );
                                ui.checkbox(&mut self.ex_yearly, "Every year");
                                if ui.button("Add Exception").clicked() {
                                    let parse = |s: &str| NaiveDate::parse_from_str(s.trim(), "%Y-%m-%d").ok();
                                    let working: Option<Vec<TimeRange>> = if self.ex_hours.trim().is_empty() {
                                        Some(vec![])
                                    } else {
                                        self.ex_hours.split(',').map(TimeRange::parse).collect()
                                    };
                                    match (parse(&self.ex_start), parse(&self.ex_end).or(parse(&self.ex_start)), working) {
                                        (Some(s), Some(e), Some(w)) if e >= s => {
                                            let ex = CalendarException {
                                                name: self.ex_name.clone(),
                                                start: s,
                                                end: e,
                                                working: w,
                                                yearly: self.ex_yearly,
                                            };
                                            let id = cal.id;
                                            let r = doc.edit("Add Calendar Exception", |p| {
                                                if let Some(c) = p.calendars.iter_mut().find(|c| c.id == id) {
                                                    c.exceptions.push(ex);
                                                }
                                            });
                                            self.error = r.err().map(|e| e.to_string());
                                        }
                                        (_, _, None) => self.error = Some("Working times look like 08:00-12:00, 13:00-17:00".into()),
                                        _ => self.error = Some("Enter start/end dates as YYYY-MM-DD (end on or after start)".into()),
                                    }
                                }
                            });
                        } else {
                            ui.label("Working times for each day of the default work week (comma-separated, empty = nonworking):");
                            egui::Grid::new("wt_week").num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
                                for (wd, text) in WEEKDAYS.iter().zip(self.week_text.iter_mut()) {
                                    ui.label(format!("{wd:?}"));
                                    ui.add(egui::TextEdit::singleline(text).desired_width(260.0).hint_text("nonworking"));
                                    ui.end_row();
                                }
                            });
                            ui.horizontal(|ui| {
                                if ui.button("Apply Work Week").clicked() {
                                    let mut week = WorkWeek::default();
                                    let mut ok = true;
                                    for (wd, txt) in WEEKDAYS.iter().zip(self.week_text.iter()) {
                                        let txt = txt.trim();
                                        if txt.is_empty() {
                                            continue;
                                        }
                                        match txt.split(',').map(TimeRange::parse).collect::<Option<Vec<_>>>() {
                                            Some(mut v) => {
                                                plancraft_core::calendar::normalize_ranges(&mut v);
                                                *week.day_mut(*wd) = v;
                                            }
                                            None => ok = false,
                                        }
                                    }
                                    if ok {
                                        let id = cal.id;
                                        let r = doc.edit("Change Work Week", |p| {
                                            if let Some(c) = p.calendars.iter_mut().find(|c| c.id == id) {
                                                c.week = week;
                                            }
                                        });
                                        self.week_loaded_for = None;
                                        self.error = r.err().map(|e| e.to_string());
                                    } else {
                                        self.error = Some("Working times look like 08:00-12:00, 13:00-17:00".into());
                                    }
                                }
                                if ui.button("Reset to Standard").clicked() {
                                    let std = Calendar::standard(cal.id);
                                    self.load_week(&std);
                                }
                                ui.label(
                                    RichText::new(format!("{} per week", format_work(cal.week.weekly_minutes())))
                                        .color(theme::pal().TEXT_WEAK),
                                );
                            });
                        }
                    });
                });
                if let Some(e) = &self.error {
                    ui.colored_label(theme::pal().ERROR, e);
                }
                ui.separator();
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Close").clicked() {
                        close = true;
                    }
                    if cal.id != doc.project.calendar && ui.button("Delete Calendar").clicked() {
                        let id = cal.id;
                        if let Err(e) = doc.try_edit("Delete Calendar", ChangeSource::User, |p| p.delete_calendar(id)) {
                            self.error = Some(e.to_string());
                        }
                        self.calendar = doc.project.calendar;
                    }
                });
            });
        open && !close
    }
}
