//! File › Export CSV: which table, and which of its columns, before the file is chosen.

use egui::{Context, RichText};
use plancraft_core::tabular::{self, CsvTable};
use plancraft_core::workspace::Document;

use crate::settings::AppSettings;
use crate::theme;

/// What the window asks for.
pub enum Outcome {
    Open,
    Closed,
    /// Write this table with these columns (headings): the file is asked for next.
    Export(CsvTable, Vec<String>),
}

pub struct CsvExport {
    pub table: CsvTable,
    /// The table's headings and whether each is written.
    pub columns: Vec<(String, bool)>,
    /// Which plan revision and table the columns were listed for (custom fields come and go).
    listed: Option<(u64, CsvTable)>,
}

impl CsvExport {
    pub fn new(table: CsvTable) -> Self {
        Self { table, columns: Vec::new(), listed: None }
    }

    /// Lists the table's columns again when the table or the plan changed, keeping what was
    /// ticked (a column not seen before: as the settings say, else ticked).
    fn list(&mut self, doc: &Document, settings: &AppSettings) {
        if self.listed == Some((doc.revision, self.table)) {
            return;
        }
        let same_table = self.listed.is_some_and(|(_, t)| t == self.table);
        let left_out = settings.csv_left_out.get(self.table.key());
        let before = std::mem::take(&mut self.columns);
        self.columns = tabular::table_columns(&doc.project, self.table)
            .into_iter()
            .map(|h| {
                let kept = before.iter().find(|(b, _)| *b == h).filter(|_| same_table).map(|(_, on)| *on);
                let on = kept.unwrap_or_else(|| !left_out.is_some_and(|l| l.contains(&h)));
                (h, on)
            })
            .collect();
        self.listed = Some((doc.revision, self.table));
    }

    /// The headings ticked.
    pub fn chosen(&self) -> Vec<String> {
        self.columns.iter().filter(|(_, on)| *on).map(|(h, _)| h.clone()).collect()
    }

    /// The headings left out.
    pub fn left_out(&self) -> Vec<String> {
        self.columns.iter().filter(|(_, on)| !*on).map(|(h, _)| h.clone()).collect()
    }

    pub fn show(&mut self, ctx: &Context, doc: &Document, settings: &AppSettings) -> Outcome {
        self.list(doc, settings);
        let mut open = true;
        let mut outcome = Outcome::Open;
        egui::Window::new("Export CSV")
            .id(egui::Id::new("csv_export_dialog"))
            .pivot(egui::Align2::CENTER_TOP)
            .default_pos(ctx.content_rect().center_top() + egui::vec2(0.0, 110.0))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label("Table:");
                    egui::ComboBox::from_id_salt("csv_table").width(160.0).selected_text(self.table.label()).show_ui(ui, |ui| {
                        for t in CsvTable::ALL {
                            ui.selectable_value(&mut self.table, t, t.label());
                        }
                    });
                    ui.label(RichText::new(format!("{} rows", self.table.rows(&doc.project))).color(theme::pal().TEXT_WEAK));
                });
                // Another table picked: its own columns from the next frame on.
                if self.listed.is_some_and(|(_, t)| t != self.table) {
                    self.list(doc, settings);
                }
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.label("Columns:");
                    if ui.small_button("All").clicked() {
                        self.columns.iter_mut().for_each(|(_, on)| *on = true);
                    }
                    if ui.small_button("None").clicked() {
                        self.columns.iter_mut().for_each(|(_, on)| *on = false);
                    }
                });
                // Three columns of boxes; a plan with many custom fields scrolls.
                egui::ScrollArea::vertical().max_height(300.0).show(ui, |ui| {
                    egui::Grid::new("csv_columns").num_columns(3).spacing([16.0, 4.0]).show(ui, |ui| {
                        for (n, (heading, on)) in self.columns.iter_mut().enumerate() {
                            ui.checkbox(on, heading.as_str());
                            if n % 3 == 2 {
                                ui.end_row();
                            }
                        }
                    });
                });
                if self.table == CsvTable::Tasks {
                    ui.label(
                        RichText::new("Import Excel or CSV reads it back as a plan when it has at least the Name column.")
                            .small()
                            .color(theme::pal().TEXT_WEAK),
                    );
                }
                ui.separator();
                let chosen = self.chosen();
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(!chosen.is_empty(), egui::Button::new("Export…"))
                        .on_disabled_hover_text("Tick a column first")
                        .clicked()
                    {
                        outcome = Outcome::Export(self.table, chosen.clone());
                    }
                    if ui.button("Cancel").clicked() {
                        outcome = Outcome::Closed;
                    }
                });
            });
        if !open {
            return Outcome::Closed;
        }
        outcome
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::indexing_slicing)]
    use super::*;
    use plancraft_core::workspace::Workspace;

    fn workspace() -> (Workspace, u64) {
        let mut ws = Workspace::default();
        let h = ws.add(plancraft_core::sample::demo_project(plancraft_core::dates::ymd(2026, 10, 19).unwrap()), None);
        (ws, h)
    }

    #[test]
    fn columns_start_from_what_was_left_out_last_time() {
        let (mut ws, h) = workspace();
        let mut settings = AppSettings::default();
        settings.csv_left_out.insert("tasks".into(), vec!["WBS".into(), "Critical".into()]);
        let mut w = CsvExport::new(CsvTable::Tasks);
        let doc = |ws: &Workspace| ws.docs.iter().find(|d| d.handle == h).unwrap().clone();
        w.list(&doc(&ws), &settings);
        assert_eq!(w.left_out(), ["WBS", "Critical"]);
        assert!(w.chosen().contains(&"Name".to_string()));
        // Unticked here, then the plan changes (a custom field added): kept, and the new one is in.
        w.columns.iter_mut().find(|(c, _)| c == "Cost").unwrap().1 = false;
        {
            let d = ws.docs.iter_mut().find(|d| d.handle == h).unwrap();
            d.edit("Add field", |p| p.add_custom_field("Owner", plancraft_core::custom::CustomFieldKind::Text, &[]).map(drop))
                .unwrap()
                .unwrap();
        }
        w.list(&doc(&ws), &settings);
        assert_eq!(w.left_out(), ["WBS", "Cost", "Critical"]);
        assert!(w.chosen().contains(&"Owner".to_string()));
        // Another table: its own columns, as the settings say.
        w.table = CsvTable::Time;
        w.list(&doc(&ws), &settings);
        assert!(w.left_out().is_empty());
        assert_eq!(w.chosen().first().map(String::as_str), Some("Date"));
    }
}
