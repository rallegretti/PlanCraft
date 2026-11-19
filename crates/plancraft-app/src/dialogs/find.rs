//! Find and Replace (Ctrl+F, Ctrl+H) and Go To (F5) for the task table.

use egui::{Context, Key, RichText};
use plancraft_core::prelude::*;
use plancraft_core::workspace::Document;

use crate::columns::{CellCtx, TaskCol};
use crate::edit::EditExt;
use crate::fields::{self, Field};
use crate::state::{UiState, ViewKind};
use crate::theme;
use crate::views;
use crate::widgets::grid::Editor;

/// Whether `text` contains `find` (ignoring case unless `match_case`).
pub fn contains(text: &str, find: &str, match_case: bool) -> bool {
    !find.is_empty() && replace_all(text, find, "", match_case).1 > 0
}

/// `text` with every `find` replaced by `with`, and how many were replaced. Without `match_case`,
/// letters are compared one by one ignoring case, so the rest of the text keeps its own case.
pub fn replace_all(text: &str, find: &str, with: &str, match_case: bool) -> (String, usize) {
    let needle: Vec<char> = find.chars().collect();
    if needle.is_empty() {
        return (text.to_string(), 0);
    }
    let same = |a: char, b: char| if match_case { a == b } else { a == b || a.to_lowercase().eq(b.to_lowercase()) };
    let hay: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut count = 0;
    let mut i = 0;
    while let Some(&c) = hay.get(i) {
        let here = hay.get(i..i.saturating_add(needle.len())).is_some_and(|w| w.iter().zip(&needle).all(|(a, b)| same(*a, *b)));
        if here {
            out.push_str(with);
            count += 1;
            i += needle.len();
        } else {
            out.push(c);
            i += 1;
        }
    }
    (out, count)
}

/// The columns Find looks in: the current table's, or one of them.
fn columns(p: &Project, st: &UiState, only: Option<&str>) -> Vec<(usize, Field)> {
    fields::table_fields(p, st.table).into_iter().enumerate().filter(|(_, f)| only.is_none_or(|k| f.key() == k)).collect()
}

/// A cell's text as shown (for finding) and as edited (for replacing, `None` when the cell can't
/// be edited).
fn cell_texts(p: &Project, idx: usize, field: Field) -> (String, Option<String>) {
    let ctx = CellCtx { overallocated: &|_| false, others_on: None };
    let cell = field.cell(p, idx, &ctx);
    let editable = !cell.read_only && matches!(field.def(p).editor, Editor::Text);
    let edit = editable.then(|| cell.edit_text.clone().unwrap_or_else(|| cell.text.clone()));
    (cell.text, edit)
}

pub struct FindReplace {
    pub find: String,
    pub replace: String,
    /// Field key to look in (`None`: every column of the current table).
    pub field: Option<String>,
    pub match_case: bool,
    /// Show the Replace row.
    pub replacing: bool,
    message: Option<(String, bool)>,
    focus: bool,
}

impl FindReplace {
    pub fn new(replacing: bool) -> Self {
        Self {
            find: String::new(),
            replace: String::new(),
            field: Some(Field::Builtin(TaskCol::Name).key()),
            match_case: false,
            replacing,
            message: None,
            focus: true,
        }
    }

    /// Opened again with Ctrl+F or Ctrl+H: keeps the text, shows or hides Replace.
    pub fn reopen(&mut self, replacing: bool) {
        self.replacing = replacing;
        self.focus = true;
    }

    /// Returns false when the window should close.
    pub fn show(&mut self, ctx: &Context, doc: &mut Document, st: &mut UiState) -> bool {
        let mut open = true;
        let mut close = false;
        let mut action = None;
        let mut typing = false;
        let shown = fields::table_fields(&doc.project, st.table);
        if self.field.as_ref().is_some_and(|k| !shown.iter().any(|f| f.key() == *k)) {
            self.field = None;
        }
        egui::Window::new(if self.replacing { "Replace" } else { "Find" })
            .id(egui::Id::new("find_dialog"))
            .pivot(egui::Align2::RIGHT_TOP)
            .default_pos(ctx.content_rect().right_top() + egui::vec2(-40.0, 140.0))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                egui::Grid::new("find_grid").num_columns(2).spacing([10.0, 8.0]).show(ui, |ui| {
                    ui.label("Find what:");
                    let r = ui.add(egui::TextEdit::singleline(&mut self.find).desired_width(240.0));
                    if std::mem::take(&mut self.focus) {
                        r.request_focus();
                    }
                    if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                        action = Some(Action::Next);
                    }
                    typing |= r.has_focus() || r.lost_focus();
                    ui.end_row();
                    if self.replacing {
                        ui.label("Replace with:");
                        let r = ui.add(egui::TextEdit::singleline(&mut self.replace).desired_width(240.0));
                        typing |= r.has_focus() || r.lost_focus();
                        ui.end_row();
                    }
                    ui.label("Look in:");
                    let p = &doc.project;
                    let title = self.field.as_ref().and_then(|k| Field::from_key(p, k)).map_or("All columns".to_string(), |f| f.title(p));
                    egui::ComboBox::from_id_salt("find_field").width(240.0).selected_text(title).show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.field, None, "All columns");
                        for f in &shown {
                            ui.selectable_value(&mut self.field, Some(f.key()), f.title(p));
                        }
                    });
                    ui.end_row();
                    ui.label("");
                    ui.checkbox(&mut self.match_case, "Match case");
                    ui.end_row();
                });
                if let Some((m, bad)) = &self.message {
                    let color = if *bad { theme::pal().WARN } else { theme::pal().TEXT_WEAK };
                    ui.label(RichText::new(m).color(color));
                }
                ui.separator();
                ui.horizontal(|ui| {
                    let has = !self.find.is_empty();
                    if ui.add_enabled(has, egui::Button::new("Find Next")).clicked() {
                        action = Some(Action::Next);
                    }
                    if self.replacing {
                        if ui.add_enabled(has, egui::Button::new("Replace")).clicked() {
                            action = Some(Action::Replace);
                        }
                        if ui.add_enabled(has, egui::Button::new("Replace All")).clicked() {
                            action = Some(Action::ReplaceAll);
                        }
                    } else if ui.button("Replace…").clicked() {
                        self.replacing = true;
                    }
                    if ui.button("Close").clicked() {
                        close = true;
                    }
                });
            });
        // Escape in one of its fields closes the window (in the grid, Escape belongs to the grid).
        if typing && ctx.input(|i| i.key_pressed(Key::Escape)) {
            close = true;
        }
        match action {
            Some(Action::Next) => self.find_next(doc, st),
            Some(Action::Replace) => self.replace_one(doc, st),
            Some(Action::ReplaceAll) => self.replace_everywhere(doc, st),
            None => {}
        }
        open && !close
    }

    /// The next task after the selected one (wrapping around) with a matching cell, and the column.
    fn next_match(&self, p: &Project, st: &mut UiState) -> Option<(TaskId, usize)> {
        let order = views::search_order(p, st);
        let cols = columns(p, st, self.field.as_deref());
        let current = st.sel.single_task().or_else(|| st.sel.tasks.iter().next().copied()).and_then(|id| p.task_index(id));
        let start = current.and_then(|c| order.iter().position(|&i| i == c)).map_or(0, |k| k.saturating_add(1));
        let len = order.len();
        (0..len).filter_map(|k| order.get((start + k) % len.max(1))).find_map(|&idx| {
            let col = cols.iter().find(|(_, f)| contains(&cell_texts(p, idx, *f).0, &self.find, self.match_case)).map(|(c, _)| *c)?;
            p.tasks.get(idx).map(|t| (t.id, col))
        })
    }

    fn find_next(&mut self, doc: &Document, st: &mut UiState) {
        match self.next_match(&doc.project, st) {
            Some((id, col)) => {
                views::reveal_task(&doc.project, st, id, Some(col));
                self.message = None;
            }
            None => self.message = Some((format!("'{}' was not found.", self.find), true)),
        }
    }

    /// Replaces in the selected task (if it matches), then finds the next match.
    fn replace_one(&mut self, doc: &mut Document, st: &mut UiState) {
        let p = &doc.project;
        let target = st.sel.single_task().and_then(|id| p.task_index(id).map(|i| (id, i)));
        if let Some((id, idx)) = target {
            let changes: Vec<(Field, String)> = columns(p, st, self.field.as_deref())
                .into_iter()
                .filter_map(|(_, f)| {
                    let (_, edit) = cell_texts(p, idx, f);
                    let (new, n) = replace_all(&edit?, &self.find, &self.replace, self.match_case);
                    (n > 0).then_some((f, new))
                })
                .collect();
            if !changes.is_empty() {
                let r = doc.edit_msg("Replace", |p| changes.iter().try_for_each(|(f, text)| f.commit(p, id, text)));
                if let Err(e) = r {
                    self.message = Some((e, true));
                    return;
                }
            }
        }
        self.find_next(doc, st);
        if self.message.is_some() {
            self.message = Some(("No more matches.".into(), false));
        }
    }

    fn replace_everywhere(&mut self, doc: &mut Document, st: &mut UiState) {
        let p = &doc.project;
        let order = views::search_order(p, st);
        let cols = columns(p, st, self.field.as_deref());
        let mut changes: Vec<(TaskId, Field, String)> = Vec::new();
        let mut count = 0;
        for &idx in &order {
            let Some(id) = p.tasks.get(idx).map(|t| t.id) else { continue };
            for (_, f) in &cols {
                let Some(edit) = cell_texts(p, idx, *f).1 else { continue };
                let (new, n) = replace_all(&edit, &self.find, &self.replace, self.match_case);
                if n > 0 {
                    count += n;
                    changes.push((id, *f, new));
                }
            }
        }
        if changes.is_empty() {
            self.message = Some((format!("'{}' was not found in a cell that can be edited.", self.find), true));
            return;
        }
        let r = doc.edit("Replace All", |p| {
            changes.iter().filter(|(id, f, text)| f.commit(p, *id, text).is_err()).map(|(id, _, _)| *id).collect::<Vec<_>>()
        });
        self.message = Some(match r {
            Ok(failed) if failed.is_empty() => (format!("Replaced {count} {}.", if count == 1 { "match" } else { "matches" }), false),
            Ok(failed) => (
                format!(
                    "Replaced the matches in {} {}; {} {} not valid for {} column and stayed as they were.",
                    changes.len() - failed.len(),
                    if changes.len() - failed.len() == 1 { "cell" } else { "cells" },
                    failed.len(),
                    if failed.len() == 1 { "value was" } else { "values were" },
                    if failed.len() == 1 { "its" } else { "their" },
                ),
                true,
            ),
            Err(e) => (e.to_string(), true),
        });
    }
}

#[derive(Clone, Copy)]
enum Action {
    Next,
    Replace,
    ReplaceAll,
}

/// Go To: a task by its ID (row number), or a date on the chart.
pub struct GoTo {
    pub id: String,
    pub date: String,
    error: Option<String>,
    focus: bool,
}

impl GoTo {
    pub fn new() -> Self {
        Self { id: String::new(), date: String::new(), error: None, focus: true }
    }

    /// Returns false when the window should close.
    pub fn show(&mut self, ctx: &Context, doc: &mut Document, st: &mut UiState) -> bool {
        let mut open = true;
        let mut close = false;
        let mut go = false;
        let mut in_fields = false;
        egui::Window::new("Go To")
            .id(egui::Id::new("goto_dialog"))
            .pivot(egui::Align2::CENTER_TOP)
            .default_pos(ctx.content_rect().center_top() + egui::vec2(0.0, 140.0))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                egui::Grid::new("goto_grid").num_columns(2).spacing([10.0, 8.0]).show(ui, |ui| {
                    ui.label("ID:");
                    let r = ui.add(egui::TextEdit::singleline(&mut self.id).desired_width(120.0).hint_text("row number"));
                    if std::mem::take(&mut self.focus) {
                        r.request_focus();
                    }
                    in_fields |= r.lost_focus();
                    ui.end_row();
                    ui.label("Date:");
                    let r = ui.add(egui::TextEdit::singleline(&mut self.date).desired_width(120.0).hint_text("2026-11-30"));
                    in_fields |= r.lost_focus();
                    ui.end_row();
                });
                if let Some(e) = &self.error {
                    ui.colored_label(theme::pal().ERROR, e);
                }
                ui.separator();
                ui.horizontal(|ui| {
                    if ui.button("Go").clicked() {
                        go = true;
                    }
                    if ui.button("Cancel").clicked() {
                        close = true;
                    }
                });
                // Enter in one of its fields (which ends their editing), not in the grid behind.
                if in_fields && ui.input(|i| i.key_pressed(Key::Enter)) {
                    go = true;
                }
            });
        if go {
            match self.go(doc, st) {
                Ok(()) => close = true,
                Err(e) => self.error = Some(e),
            }
        }
        open && !close
    }

    fn go(&self, doc: &Document, st: &mut UiState) -> Result<(), String> {
        let p = &doc.project;
        let (id, date) = (self.id.trim(), self.date.trim());
        if id.is_empty() && date.is_empty() {
            return Err("Type a task ID or a date.".into());
        }
        if !matches!(st.view, ViewKind::Gantt | ViewKind::TrackingGantt | ViewKind::MultipleBaselines) {
            st.view = ViewKind::Gantt;
        }
        if !id.is_empty() {
            let n: usize = id.trim_start_matches('#').parse().map_err(|_| format!("'{id}' is not a task ID (a row number)."))?;
            let task = n.checked_sub(1).and_then(|i| p.tasks.get(i)).ok_or_else(|| format!("There is no task {n}."))?;
            if !views::reveal_task(p, st, task.id, None) {
                return Err(format!("Task {n} is hidden by the filter or search."));
            }
        }
        if !date.is_empty() {
            let at = p.parse_datetime(date, false).ok_or_else(|| format!("'{date}' is not a date."))?;
            st.gantt.scroll_to(at);
        }
        Ok(())
    }
}

/// Fill Down: the value of the top selected task's cell in the cursor's column copied into the
/// cells of the other selected tasks (in the order the table shows them), in one undo step.
pub fn fill_down(doc: &mut Document, st: &mut UiState) -> Result<String, String> {
    if !matches!(st.view, ViewKind::Gantt | ViewKind::TrackingGantt | ViewKind::MultipleBaselines) {
        return Err("Fill Down works in the task table of the Gantt Chart views.".into());
    }
    let p = &doc.project;
    let Some((_, col)) = st.task_grid.cursor.filter(|_| !st.task_grid.whole_row) else {
        return Err("Click a cell in the column to fill, then select the rows below it (Shift+click or Shift+Down).".into());
    };
    let field = fields::table_fields(p, st.table).get(col).copied().ok_or("Click a cell in the column to fill.")?;
    let title = field.title(p);
    if matches!(field.def(p).editor, Editor::None) {
        return Err(format!("{title} is calculated, so it can't be filled."));
    }
    let rows = views::visible_rows(p, st);
    let selected: Vec<usize> =
        rows.iter().filter_map(views::Row::task).filter(|&i| p.tasks.get(i).is_some_and(|t| st.sel.tasks.contains(&t.id))).collect();
    let [first, rest @ ..] = selected.as_slice() else { return Err("Select the rows to fill.".into()) };
    if rest.is_empty() {
        return Err("Select the rows below too (Shift+click or Shift+Down), then fill down.".into());
    }
    let ctx = CellCtx { overallocated: &|_| false, others_on: None };
    let source = field.cell(p, *first, &ctx);
    let text = source.edit_text.unwrap_or(source.text);
    // Cells that are calculated for some tasks (a summary's duration) are left as they are.
    let targets: Vec<TaskId> =
        rest.iter().filter(|&&i| !field.cell(p, i, &ctx).read_only).filter_map(|&i| p.tasks.get(i)).map(|t| t.id).collect();
    let skipped = rest.len() - targets.len();
    doc.edit_msg(&format!("Fill Down {title}"), |p| targets.iter().try_for_each(|id| field.commit(p, *id, &text)))?;
    Ok(if skipped == 0 {
        format!("Filled {title} into {} {}.", targets.len(), if targets.len() == 1 { "task" } else { "tasks" })
    } else {
        format!("Filled {title} into {} tasks; {skipped} calculated there (summaries) were left as they are.", targets.len())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replacing_ignores_case_letter_by_letter() {
        assert_eq!(replace_all("Design Review and design", "design", "Build", false), ("Build Review and Build".into(), 2));
        assert_eq!(replace_all("Design Review and design", "design", "Build", true), ("Design Review and Build".into(), 1));
        // Letters whose lower case is longer (İ) or not ASCII keep the rest of the text intact.
        assert_eq!(replace_all("İstanbul ÉTÉ été", "été", "x", false), ("İstanbul x x".into(), 2));
        assert_eq!(replace_all("aaa", "aa", "b", false), ("ba".into(), 1));
        assert_eq!(replace_all("abc", "", "x", false), ("abc".into(), 0));
        assert!(contains("Kick-off", "KICK", false) && !contains("Kick-off", "KICK", true));
    }
}
