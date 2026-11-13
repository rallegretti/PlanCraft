//! Recurring Task Information: create or change a recurring task (Microsoft Project's
//! Task ▸ Recurring Task).

use chrono::{Datelike, NaiveDate};
use egui::{Context, RichText, Ui};
use plancraft_core::calendar::WorkCal;
use plancraft_core::prelude::*;
use plancraft_core::recurring::{MonthDay, Nth, Pattern, RecurEnd, Recurrence};
use plancraft_core::workspace::Document;

use crate::edit::EditExt;
use crate::state::{ToastKind, UiState};
use crate::theme;

const WEEKDAYS: [&str; 7] = ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"];
const MONTHS: [&str; 12] =
    ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Daily,
    Weekly,
    Monthly,
    Yearly,
}

pub struct RecurringTask {
    /// The recurring task being changed (`None`: a new one).
    editing: Option<TaskId>,
    /// Where a new one goes: above this row (appended when `None`).
    at: Option<usize>,
    name: String,
    duration: String,
    kind: Kind,
    every: String,
    workdays: bool,
    days: [bool; 7],
    /// Monthly/yearly: "the first Monday" rather than "day 12".
    by_weekday: bool,
    day: String,
    nth: Nth,
    weekday: u8,
    month: u8,
    start: String,
    end_by: bool,
    occurrences: String,
    end: String,
    calendar: Option<CalendarId>,
    ignore_resource_calendars: bool,
    error: Option<String>,
}

impl RecurringTask {
    /// A new recurring task, inserted above row `at`.
    pub fn new(p: &Project, at: Option<usize>) -> Self {
        let start = p.start.date();
        Self {
            editing: None,
            at,
            name: String::new(),
            duration: p.format_duration(p.settings.time.minutes_per_day, false),
            kind: Kind::Weekly,
            every: "1".into(),
            workdays: false,
            days: std::array::from_fn(|k| k == start.weekday().num_days_from_monday() as usize),
            by_weekday: false,
            day: start.day().to_string(),
            nth: Nth::First,
            weekday: start.weekday().num_days_from_monday() as u8,
            month: start.month() as u8,
            start: start.to_string(),
            end_by: false,
            occurrences: "10".into(),
            end: start.to_string(),
            calendar: None,
            ignore_resource_calendars: false,
            error: None,
        }
    }

    /// The window for an existing recurring task (`None` when the task isn't one).
    pub fn edit(p: &Project, id: TaskId) -> Option<Self> {
        let t = p.task(id)?;
        let r = t.recurrence?;
        let mut d = Self::new(p, None);
        d.editing = Some(id);
        d.name = t.name.clone();
        d.duration = p.format_duration(r.duration, false);
        d.start = r.start.to_string();
        d.calendar = r.calendar;
        d.ignore_resource_calendars = r.ignore_resource_calendars;
        match r.end {
            RecurEnd::After(n) => d.occurrences = n.to_string(),
            RecurEnd::By(e) => {
                d.end_by = true;
                d.end = e.to_string();
            }
        }
        let on = |d: &mut Self, on: MonthDay| match on {
            MonthDay::Day(n) => d.day = n.to_string(),
            MonthDay::Weekday { nth, weekday } => {
                d.by_weekday = true;
                d.nth = nth;
                d.weekday = weekday;
            }
        };
        match r.pattern {
            Pattern::Daily { interval, workdays } => {
                d.kind = Kind::Daily;
                d.every = interval.to_string();
                d.workdays = workdays;
            }
            Pattern::Weekly { interval, days } => {
                d.kind = Kind::Weekly;
                d.every = interval.to_string();
                d.days = std::array::from_fn(|k| days & (1 << k) != 0);
            }
            Pattern::Monthly { interval, on: o } => {
                d.kind = Kind::Monthly;
                d.every = interval.to_string();
                on(&mut d, o);
            }
            Pattern::Yearly { month, on: o } => {
                d.kind = Kind::Yearly;
                d.month = month;
                on(&mut d, o);
            }
        }
        Some(d)
    }

    /// The recurrence the window describes.
    fn recurrence(&self, p: &Project) -> Result<Recurrence, String> {
        let int = |s: &str, what: &str| s.trim().parse::<u16>().map_err(|_| format!("{what} must be a whole number"));
        let date =
            |s: &str, what: &str| NaiveDate::parse_from_str(s.trim(), "%Y-%m-%d").map_err(|_| format!("{what} must look like YYYY-MM-DD"));
        let duration = parse_duration(&self.duration, p.settings.duration_unit, &p.settings.time)
            .map(|(m, _)| m)
            .ok_or("The duration isn't one PlanCraft understands, e.g. 1h or 2d")?;
        let on = if self.by_weekday {
            MonthDay::Weekday { nth: self.nth, weekday: self.weekday }
        } else {
            MonthDay::Day(u8::try_from(int(&self.day, "The day")?).map_err(|_| "The day must be from 1 to 31")?)
        };
        let pattern = match self.kind {
            Kind::Daily => Pattern::Daily { interval: int(&self.every, "Every")?, workdays: self.workdays },
            Kind::Weekly => Pattern::Weekly {
                interval: int(&self.every, "Every")?,
                days: self.days.iter().enumerate().filter(|(_, on)| **on).fold(0u8, |b, (k, _)| b | (1 << k)),
            },
            Kind::Monthly => Pattern::Monthly { interval: int(&self.every, "Every")?, on },
            Kind::Yearly => Pattern::Yearly { month: self.month, on },
        };
        let end =
            if self.end_by { RecurEnd::By(date(&self.end, "End by")?) } else { RecurEnd::After(int(&self.occurrences, "End after")?) };
        Ok(Recurrence {
            pattern,
            start: date(&self.start, "Start")?,
            end,
            duration,
            calendar: self.calendar,
            ignore_resource_calendars: self.ignore_resource_calendars,
        })
    }

    /// "12 occurrences, Tue 10/20/26 to Thu 11/26/26", or why there are none.
    fn preview(&self, p: &Project) -> Result<String, String> {
        let rec = self.recurrence(p)?;
        let cal = rec.calendar.and_then(|c| p.calendar(c)).unwrap_or_else(|| p.base_calendar());
        let dates = rec.dates(&WorkCal::single(cal)).map_err(|e| e.to_string())?;
        let day = |d: Option<&NaiveDate>| d.map(|d| crate::format::day(*d)).unwrap_or_default();
        Ok(match dates.len() {
            1 => format!("1 occurrence, {}", day(dates.first())),
            n => format!("{n} occurrences, {} to {}", day(dates.first()), day(dates.last())),
        })
    }

    fn apply(&self, doc: &mut Document) -> Result<String, String> {
        let rec = self.recurrence(&doc.project)?;
        let name = self.name.trim().to_string();
        match self.editing {
            Some(id) => {
                doc.edit_msg("Recurring Task", |p| p.set_recurrence(id, Some(&name), rec).map_err(|e| e.to_string()))?;
                Ok(format!("'{name}' changed"))
            }
            None => {
                let at = self.at;
                doc.edit_msg("Recurring Task", |p| p.add_recurring_task(&name, rec, at).map_err(|e| e.to_string()))?;
                Ok(format!("Recurring task '{name}' added"))
            }
        }
    }

    fn on_day(&mut self, ui: &mut Ui, every: bool) {
        ui.horizontal(|ui| {
            ui.radio_value(&mut self.by_weekday, false, "Day");
            ui.add_enabled(!self.by_weekday, egui::TextEdit::singleline(&mut self.day).desired_width(30.0));
            if every {
                ui.label("of every");
                ui.add_enabled(!self.by_weekday, egui::TextEdit::singleline(&mut self.every).desired_width(30.0));
                ui.label("month(s)");
            }
        });
        ui.horizontal(|ui| {
            ui.radio_value(&mut self.by_weekday, true, "The");
            ui.add_enabled_ui(self.by_weekday, |ui| {
                egui::ComboBox::from_id_salt("rt_nth").width(70.0).selected_text(self.nth.label()).show_ui(ui, |ui| {
                    for n in Nth::ALL {
                        ui.selectable_value(&mut self.nth, n, n.label());
                    }
                });
                let wd = WEEKDAYS.get(usize::from(self.weekday)).copied().unwrap_or_default();
                egui::ComboBox::from_id_salt("rt_weekday").width(96.0).selected_text(wd).show_ui(ui, |ui| {
                    for (k, name) in WEEKDAYS.iter().enumerate() {
                        ui.selectable_value(&mut self.weekday, k as u8, *name);
                    }
                });
                if every {
                    ui.label("of every");
                    ui.add(egui::TextEdit::singleline(&mut self.every).desired_width(30.0));
                    ui.label("month(s)");
                }
            });
        });
    }

    pub fn show(&mut self, ctx: &Context, doc: &mut Document, st: &mut UiState) -> bool {
        let mut open = true;
        let mut close = false;
        egui::Window::new("Recurring Task Information")
            .id(egui::Id::new("recurring_task"))
            .pivot(egui::Align2::CENTER_TOP)
            .default_pos(ctx.content_rect().center_top() + egui::vec2(0.0, 120.0))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label("Task name:");
                    ui.add_sized([220.0, 20.0], egui::TextEdit::singleline(&mut self.name).hint_text("e.g. Team meeting"));
                    ui.label("Duration:");
                    ui.add_sized([60.0, 20.0], egui::TextEdit::singleline(&mut self.duration));
                });
                ui.add_space(4.0);
                ui.label(RichText::new("Recurrence pattern").strong());
                ui.horizontal(|ui| {
                    for (k, label) in [(Kind::Daily, "Daily"), (Kind::Weekly, "Weekly"), (Kind::Monthly, "Monthly"), (Kind::Yearly, "Yearly")] {
                        ui.radio_value(&mut self.kind, k, label);
                    }
                });
                ui.indent("rt_pattern", |ui| match self.kind {
                    Kind::Daily => {
                        ui.horizontal(|ui| {
                            ui.label("Every");
                            ui.add(egui::TextEdit::singleline(&mut self.every).desired_width(30.0));
                            ui.radio_value(&mut self.workdays, false, "days");
                            ui.radio_value(&mut self.workdays, true, "working days");
                        });
                    }
                    Kind::Weekly => {
                        ui.horizontal(|ui| {
                            ui.label("Recur every");
                            ui.add(egui::TextEdit::singleline(&mut self.every).desired_width(30.0));
                            ui.label("week(s) on:");
                        });
                        ui.horizontal(|ui| {
                            for (k, name) in WEEKDAYS.iter().enumerate() {
                                if let Some(on) = self.days.get_mut(k) {
                                    ui.checkbox(on, name.get(..3).unwrap_or(name));
                                }
                            }
                        });
                    }
                    Kind::Monthly => self.on_day(ui, true),
                    Kind::Yearly => {
                        self.on_day(ui, false);
                        ui.horizontal(|ui| {
                            ui.label("of");
                            let m = MONTHS.get(usize::from(self.month).saturating_sub(1)).copied().unwrap_or_default();
                            egui::ComboBox::from_id_salt("rt_month").width(110.0).selected_text(m).show_ui(ui, |ui| {
                                for (k, name) in MONTHS.iter().enumerate() {
                                    ui.selectable_value(&mut self.month, k as u8 + 1, *name);
                                }
                            });
                        });
                    }
                });
                ui.add_space(4.0);
                ui.label(RichText::new("Range of recurrence").strong());
                egui::Grid::new("rt_range").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
                    ui.label("Start:");
                    ui.add(egui::TextEdit::singleline(&mut self.start).desired_width(100.0).hint_text("YYYY-MM-DD"));
                    ui.end_row();
                    ui.radio_value(&mut self.end_by, false, "End after:");
                    ui.horizontal(|ui| {
                        ui.add_enabled(!self.end_by, egui::TextEdit::singleline(&mut self.occurrences).desired_width(40.0));
                        ui.label("occurrences");
                    });
                    ui.end_row();
                    ui.radio_value(&mut self.end_by, true, "End by:");
                    ui.add_enabled(self.end_by, egui::TextEdit::singleline(&mut self.end).desired_width(100.0).hint_text("YYYY-MM-DD"));
                    ui.end_row();
                });
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.label("Calendar:");
                    let p = &doc.project;
                    let name = self.calendar.and_then(|c| p.calendar(c)).map_or("None (project calendar)".to_string(), |c| c.name.clone());
                    egui::ComboBox::from_id_salt("rt_cal").width(170.0).selected_text(name).show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.calendar, None, "None (project calendar)");
                        for c in &p.calendars {
                            ui.selectable_value(&mut self.calendar, Some(c.id), &c.name);
                        }
                    });
                });
                ui.checkbox(&mut self.ignore_resource_calendars, "Scheduling ignores resource calendars");
                match self.preview(&doc.project) {
                    Ok(text) => ui.label(RichText::new(text).small().color(theme::pal().TEXT_WEAK)),
                    Err(e) => ui.colored_label(theme::pal().ERROR, e),
                };
                ui.label(
                    RichText::new("Occurrences on non-working days move to the next working day. Assign resources to the recurring task to put them on every occurrence.")
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
