//! Custom Fields dialog: define, rename, change the pick-list of and delete custom task fields.
//! Every change is its own undoable edit.

use std::collections::HashMap;

use egui::{Context, RichText};
use plancraft_core::custom::CustomFieldKind;
use plancraft_core::prelude::*;
use plancraft_core::workspace::Document;

use crate::edit::EditExt;
use crate::theme;

/// Draft of one field's editable parts: name and pick-list (one entry per line).
#[derive(Clone)]
struct Draft {
    name: String,
    choices: String,
}

/// One change to apply to the plan (its own undoable edit).
type Action = Box<dyn FnOnce(&mut Project) -> Result<(), String>>;

#[derive(Default)]
pub struct CustomFields {
    drafts: HashMap<CustomFieldId, Draft>,
    new_name: String,
    new_kind: CustomFieldKind,
    new_choices: String,
    error: Option<String>,
}

/// Pick-list entries typed one per line (or separated by commas).
fn choice_list(text: &str) -> Vec<String> {
    text.split(['\n', ',']).map(str::trim).filter(|s| !s.is_empty()).map(str::to_string).collect()
}

impl CustomFields {
    pub fn show(&mut self, ctx: &Context, doc: &mut Document) -> bool {
        let mut open = true;
        let mut close = false;
        egui::Window::new("Custom Fields")
            .id(egui::Id::new("custom_fields_dialog"))
            .pivot(egui::Align2::CENTER_TOP)
            .default_pos(ctx.content_rect().center_top() + egui::vec2(0.0, 100.0))
            .default_width(560.0)
            .open(&mut open)
            .show(ctx, |ui| {
                ui.label(
                    RichText::new("Fields you add here can be shown as columns (right-click a column header, then Insert Column), sorted and grouped by.")
                        .small()
                        .color(theme::pal().TEXT_WEAK),
                );
                ui.add_space(6.0);
                let fields = doc.project.custom_fields.clone();
                if fields.is_empty() {
                    ui.label(RichText::new("This plan has no custom fields yet.").italics());
                }
                let mut action: Option<(String, Action)> = None;
                egui::ScrollArea::vertical().max_height(300.0).show(ui, |ui| {
                    egui::Grid::new("custom_fields_grid").num_columns(4).spacing([10.0, 6.0]).striped(true).show(ui, |ui| {
                        for h in ["Name", "Type", "Pick-list (one per line)", ""] {
                            ui.label(RichText::new(h).strong());
                        }
                        ui.end_row();
                        for f in &fields {
                            let draft = self
                                .drafts
                                .entry(f.id)
                                .or_insert_with(|| Draft { name: f.name.clone(), choices: f.choices.join("\n") });
                            ui.add_sized([150.0, 22.0], egui::TextEdit::singleline(&mut draft.name));
                            ui.label(f.kind.label());
                            if f.kind == CustomFieldKind::Text {
                                ui.add_sized([200.0, 44.0], egui::TextEdit::multiline(&mut draft.choices));
                            } else {
                                ui.label(RichText::new("–").color(theme::pal().TEXT_WEAK));
                            }
                            ui.horizontal(|ui| {
                                let changed = draft.name.trim() != f.name || choice_list(&draft.choices) != f.choices;
                                if ui.add_enabled(changed, egui::Button::new("Apply")).clicked() {
                                    let (id, name, choices) = (f.id, draft.name.clone(), choice_list(&draft.choices));
                                    action = Some((
                                        format!("Change custom field '{}'", f.name),
                                        Box::new(move |p: &mut Project| {
                                            p.rename_custom_field(id, &name).map_err(|e| e.to_string())?;
                                            p.set_custom_field_choices(id, &choices).map_err(|e| e.to_string())
                                        }),
                                    ));
                                }
                                if ui.button("Delete").on_hover_text("Delete the field and its value on every task (undoable)").clicked() {
                                    let id = f.id;
                                    action = Some((
                                        format!("Delete custom field '{}'", f.name),
                                        Box::new(move |p: &mut Project| p.delete_custom_field(id).map_err(|e| e.to_string())),
                                    ));
                                }
                            });
                            ui.end_row();
                        }
                    });
                });
                ui.separator();
                ui.label(RichText::new("New field").strong());
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut self.new_name).hint_text("Name, e.g. Status").desired_width(150.0));
                    egui::ComboBox::from_id_salt("new_field_kind").width(100.0).selected_text(self.new_kind.label()).show_ui(ui, |ui| {
                        for k in CustomFieldKind::ALL {
                            ui.selectable_value(&mut self.new_kind, k, k.label());
                        }
                    });
                });
                if self.new_kind == CustomFieldKind::Text {
                    ui.add(
                        egui::TextEdit::multiline(&mut self.new_choices)
                            .hint_text("Optional pick-list, one value per line (e.g. To do, In progress, Done)")
                            .desired_rows(2)
                            .desired_width(f32::INFINITY),
                    );
                }
                ui.horizontal(|ui| {
                    if ui.add_enabled(!self.new_name.trim().is_empty(), egui::Button::new("Add Field")).clicked() {
                        let (name, kind) = (self.new_name.clone(), self.new_kind);
                        let choices = if kind == CustomFieldKind::Text { choice_list(&self.new_choices) } else { Vec::new() };
                        action = Some((
                            format!("Add custom field '{}'", name.trim()),
                            Box::new(move |p: &mut Project| p.add_custom_field(&name, kind, &choices).map(|_| ()).map_err(|e| e.to_string())),
                        ));
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Close").clicked() {
                            close = true;
                        }
                    });
                });
                if let Some(e) = &self.error {
                    ui.colored_label(theme::pal().ERROR, e);
                }
                if let Some((label, f)) = action {
                    let adding = label.starts_with("Add ");
                    match doc.edit_msg(&label, f) {
                        Ok(()) => {
                            self.error = None;
                            // Drafts follow the plan again (names and lists as saved).
                            self.drafts.clear();
                            if adding {
                                self.new_name.clear();
                                self.new_choices.clear();
                            }
                        }
                        Err(e) => self.error = Some(e),
                    }
                }
            });
        open && !close
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pick_lists_split_on_lines_and_commas() {
        assert_eq!(choice_list("To do\nIn progress, Done\n\n  "), ["To do", "In progress", "Done"]);
        assert!(choice_list("").is_empty());
    }
}
