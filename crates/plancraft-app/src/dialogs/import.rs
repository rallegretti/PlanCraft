//! Import Spreadsheet: shows the first rows of a CSV or workbook file, lets the user say what
//! each column holds (guessed from its heading), and imports it as a new, unsaved plan.

use std::path::PathBuf;

use egui::{Context, RichText};
use plancraft_core::bidi;
use plancraft_core::tabular::{self, DateOrder, Field, Grid, ImportOptions, Sheets};
use plancraft_core::workspace::Workspace;

use crate::theme;

/// Rows shown in the preview.
const PREVIEW_ROWS: usize = 8;
/// Columns shown in the preview (a column beyond them keeps its guessed field).
const PREVIEW_COLUMNS: usize = 200;
/// Characters shown of a cell in the preview.
const PREVIEW_CHARS: usize = 40;

pub struct ImportSheet {
    pub path: PathBuf,
    sheets: Sheets,
    sheet: usize,
    options: ImportOptions,
    /// The user picked the date order: it is no longer guessed again when columns change.
    date_order_set: bool,
    /// What the preview shows of the current worksheet, worked out once (cells shortened and in
    /// display order), not every frame.
    preview: Preview,
    error: Option<String>,
}

#[derive(Default)]
struct Preview {
    headers: Vec<String>,
    cells: Vec<Vec<String>>,
    more_rows: usize,
}

impl Preview {
    fn of(grid: &Grid) -> Preview {
        let columns = grid.headers.len().max(grid.rows.iter().take(PREVIEW_ROWS).map(Vec::len).max().unwrap_or(0)).min(PREVIEW_COLUMNS);
        let shown = |text: &str| bidi::display(&short(text)).into_owned();
        Preview {
            headers: (0..columns).map(|c| shown(grid.headers.get(c).map_or("", String::as_str))).collect(),
            cells: grid
                .rows
                .iter()
                .take(PREVIEW_ROWS)
                .map(|r| (0..columns).map(|c| shown(r.get(c).map_or("", String::as_str))).collect())
                .collect(),
            more_rows: grid.rows.len().saturating_sub(PREVIEW_ROWS),
        }
    }
}

/// What the window did this frame.
pub enum Outcome {
    Open,
    Cancelled,
    /// Imported as the document with this handle.
    Imported(u64),
}

impl ImportSheet {
    pub fn new(path: PathBuf, sheets: Sheets) -> Self {
        let name = path.file_stem().map_or_else(|| "Imported plan".to_string(), |s| s.to_string_lossy().into_owned());
        let options = sheets.sheets.first().map(|(_, g)| ImportOptions::guess(g, name.clone())).unwrap_or_default();
        let preview = sheets.sheets.first().map(|(_, g)| Preview::of(g)).unwrap_or_default();
        Self { path, sheets, sheet: 0, options, date_order_set: false, preview, error: None }
    }

    /// Switches to another worksheet: its own guesses and preview (the plan's name stays).
    fn pick_sheet(&mut self, sheet: usize) {
        self.sheet = sheet;
        let name = std::mem::take(&mut self.options.name);
        self.options = self.grid().map(|g| ImportOptions::guess(g, name.clone())).unwrap_or_default();
        self.options.name = name;
        self.date_order_set = false;
        self.preview = self.grid().map(Preview::of).unwrap_or_default();
    }

    fn grid(&self) -> Option<&Grid> {
        self.sheets.sheets.get(self.sheet).map(|(_, g)| g)
    }

    /// The text the window shows (sheet names, the preview), for loading fonts. Other sheets'
    /// previews are loaded when they are picked.
    pub fn texts(&self) -> impl Iterator<Item = &str> {
        self.sheets
            .sheets
            .iter()
            .map(|(name, _)| name.as_str())
            .chain(self.preview.headers.iter().map(String::as_str))
            .chain(self.preview.cells.iter().flatten().map(String::as_str))
    }

    /// Picks the field for a column; a field can belong to one column only.
    fn choose(&mut self, column: usize, field: Option<Field>) {
        if let Some(f) = field {
            for c in self.options.columns.iter_mut().filter(|c| **c == Some(f)) {
                *c = None;
            }
        }
        if self.options.columns.len() <= column {
            self.options.columns.resize(column.saturating_add(1), None);
        }
        if let Some(c) = self.options.columns.get_mut(column) {
            *c = field;
        }
        // A column newly read as Start or Finish may tell the date order.
        if !self.date_order_set
            && let Some(grid) = self.sheets.sheets.get(self.sheet).map(|(_, g)| g)
        {
            self.options.date_order = tabular::guess_date_order(grid, &self.options.columns);
        }
    }

    /// Builds the plan and adds it to the workspace.
    fn import(&mut self, ws: &mut Workspace) -> Result<u64, String> {
        let grid = self.grid().ok_or("the worksheet is empty")?;
        let (project, mut notes) = tabular::build(grid, &self.options)?;
        let mut all = self.sheets.notes.clone();
        all.append(&mut notes);
        let from = std::fs::canonicalize(&self.path).unwrap_or_else(|_| self.path.clone());
        Ok(ws.add_imported(project, all, from))
    }

    pub fn show(&mut self, ctx: &Context, ws: &mut Workspace) -> Outcome {
        let mut open = true;
        let mut outcome = Outcome::Open;
        let file = self.path.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
        egui::Window::new("Import Spreadsheet")
            .id(egui::Id::new("import_spreadsheet"))
            .pivot(egui::Align2::CENTER_TOP)
            .default_pos(ctx.content_rect().center_top() + egui::vec2(0.0, 100.0))
            .default_width((ctx.content_rect().width() - 80.0).clamp(560.0, 1100.0))
            .open(&mut open)
            .collapsible(false)
            .show(ctx, |ui| {
                ui.label(RichText::new(bidi::display(&file)).strong());
                if self.sheets.sheets.len() > 1 {
                    let mut sheet = self.sheet;
                    let current = self.sheets.sheets.get(sheet).map(|(n, _)| bidi::display(n)).unwrap_or_default();
                    egui::ComboBox::from_label("Worksheet").selected_text(current).show_ui(ui, |ui| {
                        for (i, (name, _)) in self.sheets.sheets.iter().enumerate() {
                            ui.selectable_value(&mut sheet, i, bidi::display(name));
                        }
                    });
                    if sheet != self.sheet {
                        self.pick_sheet(sheet);
                    }
                }
                egui::Grid::new("import_settings").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                    ui.label("Plan name");
                    ui.add(egui::TextEdit::singleline(&mut self.options.name).desired_width(320.0));
                    ui.end_row();
                    ui.label("Dates like 03/04/2026");
                    ui.horizontal(|ui| {
                        let month = ui.radio_value(&mut self.options.date_order, DateOrder::MonthFirst, "Month first (March 4)");
                        let day = ui.radio_value(&mut self.options.date_order, DateOrder::DayFirst, "Day first (3 April)");
                        if month.clicked() || day.clicked() {
                            self.date_order_set = true;
                        }
                    });
                    ui.end_row();
                });
                ui.add_space(4.0);
                ui.label(
                    RichText::new("Choose what each column holds. PlanCraft guessed from the headings; columns set to “—” are left out.")
                        .small()
                        .color(theme::pal().TEXT_WEAK),
                );
                self.preview(ui);
                for note in &self.sheets.notes {
                    ui.label(RichText::new(format!("Note: {note}")).small().color(theme::pal().TEXT_WEAK));
                }
                let has_name = self.options.columns.contains(&Some(Field::Name));
                if !has_name {
                    ui.colored_label(theme::pal().ERROR, "Choose the column that holds the task names.");
                }
                if let Some(e) = &self.error {
                    ui.colored_label(theme::pal().ERROR, e);
                }
                ui.separator();
                let row = egui::vec2(ui.available_width(), 24.0);
                ui.allocate_ui_with_layout(row, egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Cancel").clicked() {
                        outcome = Outcome::Cancelled;
                    }
                    let import = egui::Button::new(RichText::new("  Import  ").color(egui::Color32::WHITE)).fill(theme::pal().ACCENT);
                    if ui.add_enabled(has_name, import).clicked() {
                        match self.import(ws) {
                            Ok(handle) => outcome = Outcome::Imported(handle),
                            Err(e) => self.error = Some(e),
                        }
                    }
                });
            });
        if !open {
            outcome = Outcome::Cancelled;
        }
        outcome
    }

    /// The column choosers above the first rows of the sheet.
    fn preview(&mut self, ui: &mut egui::Ui) {
        let mut chosen: Vec<(usize, Option<Field>)> = Vec::new();
        // A solid bar that is always shown: a wide sheet has more columns to the right.
        ui.spacing_mut().scroll = egui::style::ScrollStyle::solid();
        let preview = &self.preview;
        let columns = &self.options.columns;
        egui::ScrollArea::horizontal().max_height(300.0).scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible).show(
            ui,
            |ui| {
                egui::Grid::new("import_preview").striped(true).spacing([10.0, 4.0]).show(ui, |ui| {
                    for (c, heading) in preview.headers.iter().enumerate() {
                        let field = columns.get(c).copied().flatten();
                        let mut pick = field;
                        egui::ComboBox::from_id_salt(("import_column", c))
                            .width(130.0)
                            .selected_text(pick.map_or("—", Field::label))
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut pick, None, "— (leave out)");
                                for f in Field::ALL {
                                    ui.selectable_value(&mut pick, Some(f), f.label());
                                }
                            })
                            .response
                            .on_hover_text(format!("Column “{heading}”"));
                        if pick != field {
                            chosen.push((c, pick));
                        }
                    }
                    ui.end_row();
                    for heading in &preview.headers {
                        ui.label(RichText::new(heading).strong());
                    }
                    ui.end_row();
                    for row in &preview.cells {
                        for cell in row {
                            ui.label(cell);
                        }
                        ui.end_row();
                    }
                });
            },
        );
        if preview.more_rows > 0 {
            ui.label(RichText::new(format!("…and {} more rows", preview.more_rows)).small().color(theme::pal().TEXT_WEAK));
        }
        for (c, f) in chosen {
            self.choose(c, f);
        }
    }
}

/// A cell shortened for the preview, on one line (shortened first: a cell can be long).
fn short(text: &str) -> String {
    let (head, cut) = match text.char_indices().nth(PREVIEW_CHARS) {
        Some((end, _)) => (text.get(..end).unwrap_or(""), true),
        None => (text, false),
    };
    let one_line = head.replace(['\r', '\n'], " ");
    if cut { format!("{one_line}…") } else { one_line }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dialog(text: &str) -> ImportSheet {
        let sheets = tabular::read_bytes(text.as_bytes(), "csv").unwrap();
        ImportSheet::new(PathBuf::from("/tmp/Plan.csv"), sheets)
    }

    #[test]
    fn opens_with_guessed_columns_and_the_file_name() {
        let d = dialog("Task,Duration,Owner\nA,2d,Ana\n");
        assert_eq!(d.options.name, "Plan");
        assert_eq!(d.options.columns, [Some(Field::Name), Some(Field::Duration), Some(Field::Resources)]);
    }

    #[test]
    fn a_field_moves_to_the_column_that_chose_it() {
        let mut d = dialog("Task,Title,Duration\nA,B,2d\n");
        d.choose(1, Some(Field::Name));
        assert_eq!(d.options.columns, [None, Some(Field::Name), Some(Field::Duration)]);
        d.choose(5, Some(Field::Notes));
        assert_eq!(d.options.columns.get(5), Some(&Some(Field::Notes)));
    }

    #[test]
    fn importing_adds_an_unsaved_plan_with_notes() {
        let mut d = dialog("Task,Duration,Extra\nA,2d,x\nB,bad,y\n");
        let mut ws = Workspace::default();
        let handle = d.import(&mut ws).unwrap();
        let doc = ws.active_doc().unwrap();
        assert_eq!(doc.handle, handle);
        assert!(doc.path.is_none() && doc.imported && doc.dirty);
        assert_eq!(doc.project.tasks.len(), 2);
        assert!(doc.load_notes.iter().any(|n| n.contains("Extra")), "{:?}", doc.load_notes);
    }

    /// Regression (review): mapping a column the headings didn't give away as Start kept the
    /// month-first guess, so 25/10/2026 could not be read.
    #[test]
    fn choosing_a_date_column_guesses_the_date_order_again() {
        let mut d = dialog("Task,Kick-off\nA,25/10/2026\n");
        assert_eq!(d.options.date_order, DateOrder::MonthFirst);
        d.choose(1, Some(Field::Start));
        assert_eq!(d.options.date_order, DateOrder::DayFirst);
        // Once the user picks it, it stays.
        d.date_order_set = true;
        d.options.date_order = DateOrder::MonthFirst;
        d.choose(1, Some(Field::Start));
        assert_eq!(d.options.date_order, DateOrder::MonthFirst);
    }

    #[test]
    fn previews_are_short_and_on_one_line() {
        assert_eq!(short("a\nb"), "a b");
        assert_eq!(short(&"x".repeat(100)).chars().count(), PREVIEW_CHARS + 1);
        assert_eq!(short(&"é".repeat(41)).chars().count(), PREVIEW_CHARS + 1);
    }
}
