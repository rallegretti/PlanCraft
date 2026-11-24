//! Small windows for how task views are shown: Sort (by up to three fields), and the filters
//! that ask first (Using Resource…, Date Range…).

use egui::{Context, Key, RichText};
use plancraft_core::dates::prelude::*;
use plancraft_core::prelude::*;
use plancraft_core::workspace::Document;

use crate::state::{MAX_SORT_KEYS, TaskFilter, UiState};
use crate::theme;

/// Sort: a field to sort by, then up to two more for ties, each ascending or descending.
pub struct SortWindow {
    /// One row per level: the field's key (`None`: not used) and whether it is descending.
    rows: Vec<(Option<String>, bool)>,
}

impl SortWindow {
    /// Starts from the view's sort now.
    pub fn new(st: &UiState) -> Self {
        let mut rows: Vec<(Option<String>, bool)> = st.sort.iter().map(|(k, d)| (Some(k.clone()), *d)).collect();
        rows.resize(MAX_SORT_KEYS, (None, false));
        Self { rows }
    }

    /// The sort the rows make: unused rows and repeated fields left out.
    pub fn keys(&self) -> Vec<(String, bool)> {
        let mut out: Vec<(String, bool)> = Vec::new();
        for (key, desc) in &self.rows {
            if let Some(k) = key
                && !out.iter().any(|(o, _)| o == k)
            {
                out.push((k.clone(), *desc));
            }
        }
        out
    }

    /// Sets one level (tests).
    #[cfg(test)]
    pub fn set(&mut self, level: usize, key: Option<&str>, descending: bool) {
        if let Some(row) = self.rows.get_mut(level) {
            *row = (key.map(str::to_string), descending);
        }
    }

    /// Returns false when the window should close. It sorts the views of `doc`, which is the
    /// active plan (the window closes when another one becomes active).
    pub fn show(&mut self, ctx: &Context, doc: &Document, st: &mut UiState) -> bool {
        let choices = crate::fields::sort_choices(&doc.project, st.table);
        let title = |key: &Option<String>| match key {
            Some(k) => choices.iter().find(|(c, _)| c == k).map_or_else(|| k.clone(), |(_, t)| t.clone()),
            None => "(none)".to_string(),
        };
        let mut open = true;
        let mut close = false;
        egui::Window::new("Sort")
            .id(egui::Id::new("sort_dialog"))
            .pivot(egui::Align2::CENTER_TOP)
            .default_pos(ctx.content_rect().center_top() + egui::vec2(0.0, 140.0))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.label(
                    RichText::new("Tasks are sorted within their summary task; ties keep the outline order.")
                        .small()
                        .color(theme::pal().TEXT_WEAK),
                );
                ui.add_space(4.0);
                egui::Grid::new("sort_grid").num_columns(3).spacing([10.0, 8.0]).show(ui, |ui| {
                    for (n, (key, desc)) in self.rows.iter_mut().enumerate() {
                        ui.label(if n == 0 { "Sort by" } else { "Then by" });
                        egui::ComboBox::from_id_salt(("sort_key", n)).width(200.0).selected_text(title(key)).show_ui(ui, |ui| {
                            if ui.selectable_label(key.is_none(), "(none)").clicked() {
                                *key = None;
                            }
                            for (k, t) in &choices {
                                if ui.selectable_label(key.as_ref() == Some(k), t).clicked() {
                                    *key = Some(k.clone());
                                }
                            }
                        });
                        ui.add_enabled_ui(key.is_some(), |ui| {
                            ui.horizontal(|ui| {
                                ui.radio_value(desc, false, "Ascending");
                                ui.radio_value(desc, true, "Descending");
                            });
                        });
                        ui.end_row();
                    }
                });
                ui.separator();
                ui.horizontal(|ui| {
                    if ui.button("Sort").clicked() {
                        st.sort = self.keys();
                        close = true;
                    }
                    if ui.button("Reset").on_hover_text("Back to the outline order").clicked() {
                        st.sort.clear();
                        close = true;
                    }
                    if ui.button("Cancel").clicked() {
                        close = true;
                    }
                });
            });
        open && !close
    }
}

/// Which filter asks first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FilterAsk {
    Resource,
    Dates,
}

/// Using Resource… (which resource) or Date Range… (between which days).
pub struct FilterWindow {
    pub ask: FilterAsk,
    pub resource: Option<ResourceId>,
    pub from: String,
    pub to: String,
    error: Option<String>,
}

impl FilterWindow {
    /// Starts from the filter now when it is the same kind, else from the first resource or the
    /// next two weeks from the status date (or today).
    pub fn new(ask: FilterAsk, p: &Project, st: &UiState) -> Self {
        let start = p.status_date.unwrap_or_else(plancraft_core::dates::today);
        let mut w = Self {
            ask,
            resource: p.resources.first().map(|r| r.id),
            from: start.to_string(),
            to: plancraft_core::dates::add_days(start, 13).unwrap_or(start).to_string(),
            error: None,
        };
        match st.filter {
            TaskFilter::UsingResource(r) if p.resource(r).is_some() => w.resource = Some(r),
            TaskFilter::DateRange(from, to) => (w.from, w.to) = (from.to_string(), to.to_string()),
            _ => {}
        }
        w
    }

    /// The filter asked for, or why it cannot be made.
    pub fn filter(&self, p: &Project) -> Result<TaskFilter, String> {
        match self.ask {
            FilterAsk::Resource => match self.resource.filter(|r| p.resource(*r).is_some()) {
                Some(r) => Ok(TaskFilter::UsingResource(r)),
                None => Err("Pick a resource.".into()),
            },
            FilterAsk::Dates => {
                let day = |text: &str, what: &str| {
                    let t = text.trim();
                    Date::parse_from_str(t, "%Y-%m-%d").map_err(|_| format!("{what}: '{t}' is not a date like YYYY-MM-DD"))
                };
                let (from, to) = (day(&self.from, "From")?, day(&self.to, "To")?);
                if from > to {
                    return Err("The first day comes after the last.".into());
                }
                Ok(TaskFilter::DateRange(from, to))
            }
        }
    }

    /// Returns false when the window should close. It filters the views of `doc`, which is the
    /// active plan.
    pub fn show(&mut self, ctx: &Context, doc: &Document, st: &mut UiState) -> bool {
        let p = &doc.project;
        let mut open = true;
        let mut close = false;
        let mut apply = false;
        let title = match self.ask {
            FilterAsk::Resource => "Using Resource",
            FilterAsk::Dates => "Date Range",
        };
        egui::Window::new(title)
            .id(egui::Id::new("filter_ask_dialog"))
            .pivot(egui::Align2::CENTER_TOP)
            .default_pos(ctx.content_rect().center_top() + egui::vec2(0.0, 140.0))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                let mut in_fields = false;
                match self.ask {
                    FilterAsk::Resource => {
                        ui.label("Show the tasks assigned to:");
                        let name = self.resource.and_then(|r| p.resource(r)).map_or("(none)", |r| r.name.as_str());
                        egui::ComboBox::from_id_salt("filter_resource").width(220.0).selected_text(name).show_ui(ui, |ui| {
                            for r in &p.resources {
                                ui.selectable_value(&mut self.resource, Some(r.id), &r.name);
                            }
                        });
                        if p.resources.is_empty() {
                            ui.colored_label(theme::pal().WARN, "This plan has no resources yet.");
                        }
                    }
                    FilterAsk::Dates => {
                        ui.label("Show the tasks that run at any time between these days:");
                        egui::Grid::new("filter_dates").num_columns(2).spacing([10.0, 8.0]).show(ui, |ui| {
                            ui.label("From:");
                            in_fields |= ui.add(egui::TextEdit::singleline(&mut self.from).desired_width(120.0)).lost_focus();
                            ui.end_row();
                            ui.label("To:");
                            in_fields |= ui.add(egui::TextEdit::singleline(&mut self.to).desired_width(120.0)).lost_focus();
                            ui.end_row();
                        });
                    }
                }
                if let Some(e) = &self.error {
                    ui.colored_label(theme::pal().ERROR, e);
                }
                ui.separator();
                ui.horizontal(|ui| {
                    apply |= ui.button("Apply").clicked();
                    close |= ui.button("Cancel").clicked();
                });
                // Enter in a date field (which ends its editing), not in the grid behind.
                apply |= in_fields && ui.input(|i| i.key_pressed(Key::Enter));
            });
        if apply {
            match self.filter(p) {
                Ok(f) => {
                    st.filter = f;
                    close = true;
                }
                Err(e) => self.error = Some(e),
            }
        }
        open && !close
    }
}
