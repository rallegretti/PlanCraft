//! Milestones: each checkpoint with its deliverable, member tasks and status.

use egui::{Color32, Rect, RichText, Sense, Stroke, Ui, vec2};
use plancraft_core::prelude::*;
use plancraft_core::report;
use plancraft_core::workspace::{ChangeSource, Document};

use crate::commands::Command;
use crate::format;
use crate::state::{ToastKind, UiState, ViewKind};
use crate::theme;

fn chip(ui: &mut Ui, text: &str, fg: Color32, bg: Color32) {
    egui::Frame::new().fill(bg).corner_radius(10.0).inner_margin(egui::Margin::symmetric(8, 2)).show(ui, |ui| {
        ui.label(RichText::new(text).color(fg).size(11.5).strong());
    });
}

fn progress_bar(ui: &mut Ui, frac: f32, width: f32, color: Color32) {
    let (r, _) = ui.allocate_exact_size(vec2(width, 8.0), Sense::hover());
    ui.painter().rect_filled(r, 4.0, Color32::from_rgb(0xE6, 0xEA, 0xF1));
    ui.painter().rect_filled(Rect::from_min_size(r.min, vec2(r.width() * frac.clamp(0.0, 1.0), r.height())), 4.0, color);
}

pub fn show(ui: &mut Ui, rect: Rect, doc: &mut Document, st: &mut UiState, out: &mut Vec<Command>) {
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink(12.0)));
    let ui = &mut child;
    let at = report::status_moment(&doc.project);
    let statuses = report::milestone_status(&doc.project, at);
    ui.horizontal(|ui| {
        ui.heading("Milestones");
        ui.add_space(12.0);
        if ui.button("♦ New Milestone").clicked() {
            out.push(Command::InsertMilestone);
        }
        ui.label(RichText::new(format!("{} milestones · status as of {}", statuses.len(), format::date(Some(at)))).color(theme::TEXT_WEAK));
    });
    ui.add_space(6.0);
    if statuses.is_empty() {
        ui.label(
            RichText::new(
                "No milestones yet. A milestone is a checkpoint at which a set of tasks should be complete, producing a deliverable.",
            )
            .color(theme::TEXT_WEAK),
        );
        return;
    }
    let mut edits: Vec<(TaskId, TaskRef, bool)> = Vec::new();
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        for ms in &statuses {
            let selected = st.sel.tasks.contains(&ms.id);
            let frame = egui::Frame::new()
                .fill(if selected { theme::ACCENT_PALE } else { theme::BG })
                .stroke(Stroke::new(1.0, if selected { theme::ACCENT_MID } else { theme::GRID_LINE_STRONG }))
                .corner_radius(6.0)
                .inner_margin(12.0);
            let resp = frame
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("♦").color(theme::GOLD).size(20.0));
                        ui.label(RichText::new(&ms.name).size(16.0).strong());
                        ui.add_space(8.0);
                        if ms.complete {
                            chip(ui, "Complete", Color32::WHITE, theme::OK);
                        } else if ms.late {
                            chip(ui, "Late", Color32::WHITE, theme::ERROR);
                        } else if ms.baseline.zip(ms.date).is_some_and(|(b, d)| d > b) {
                            chip(ui, "Slipping", Color32::WHITE, theme::WARN);
                        } else {
                            chip(ui, "On track", theme::ACCENT_DARK, theme::ACCENT_LIGHT);
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button("Open in Gantt").clicked() {
                                st.sel.select_task(ms.id);
                                out.push(Command::SetView(ViewKind::Gantt));
                                out.push(Command::ScrollToTask);
                            }
                            if ui.button("Information…").clicked() {
                                st.sel.select_task(ms.id);
                                out.push(Command::TaskInfo);
                            }
                        });
                    });
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new("Date:").color(theme::TEXT_WEAK));
                        ui.label(format::date_long(ms.date));
                        if let Some(dl) = ms.deadline {
                            ui.label(RichText::new("  Deadline:").color(theme::TEXT_WEAK));
                            ui.label(RichText::new(format::date(Some(dl))).color(if ms.date.is_some_and(|d| d > dl) {
                                theme::ERROR
                            } else {
                                theme::TEXT
                            }));
                        }
                        if let Some(b) = ms.baseline {
                            ui.label(RichText::new("  Baseline:").color(theme::TEXT_WEAK));
                            ui.label(format::date(Some(b)));
                        }
                    });
                    if !ms.deliverable.is_empty() {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("Deliverable:").color(theme::TEXT_WEAK));
                            ui.label(RichText::new(&ms.deliverable).italics());
                        });
                    }
                    ui.horizontal(|ui| {
                        progress_bar(ui, ms.progress / 100.0, 260.0, if ms.complete { theme::OK } else { theme::ACCENT_MID });
                        ui.label(format!("{} · {}/{} tasks complete", format::percent(ms.progress), ms.members_complete, ms.members));
                    });
                    ui.add_space(4.0);
                    let p = &doc.project;
                    let Some(m) = p.task(ms.id) else { return };
                    egui::CollapsingHeader::new(
                        RichText::new(format!("Tasks that make up this milestone ({})", m.members.len())).size(12.5),
                    )
                    .id_salt(("ms_members", ms.id))
                    .default_open(m.members.len() <= 8)
                    .show(ui, |ui| {
                        for r in &m.members {
                            ui.horizontal(|ui| {
                                let (name, pct, finish) = match r {
                                    TaskRef::Local(id) => match p.task(*id) {
                                        Some(t) => (t.name.clone(), t.sched.percent_complete, t.sched.finish),
                                        None => continue_label(),
                                    },
                                    TaskRef::External { .. } => {
                                        let snap = match r {
                                            TaskRef::External { project, task } => p.external(*project).and_then(|e| e.task(*task)),
                                            _ => None,
                                        };
                                        (
                                            p.task_ref_name(r),
                                            snap.map(|s| s.percent_complete as f32).unwrap_or(0.0),
                                            snap.and_then(|s| s.finish),
                                        )
                                    }
                                };
                                ui.label(if pct >= 100.0 {
                                    RichText::new("✔").color(theme::OK)
                                } else {
                                    RichText::new("○").color(theme::TEXT_WEAK)
                                });
                                ui.label(&name);
                                progress_bar(ui, pct / 100.0, 80.0, theme::ACCENT_MID);
                                ui.label(
                                    RichText::new(format!("{} · {}", format::percent(pct), format::date(finish))).color(theme::TEXT_WEAK),
                                );
                                if ui.small_button("✖").on_hover_text("Remove from milestone").clicked() {
                                    edits.push((ms.id, *r, false));
                                }
                            });
                        }
                        ui.horizontal(|ui| {
                            egui::ComboBox::from_id_salt(("ms_add", ms.id)).selected_text("Add task…").width(260.0).show_ui(ui, |ui| {
                                for t in p
                                    .tasks
                                    .iter()
                                    .filter(|t| t.id != ms.id && !t.is_milestone() && !m.members.contains(&TaskRef::Local(t.id)))
                                {
                                    if ui.selectable_label(false, format!("{}  {}", p.row_number(t.id).unwrap_or(0), t.name)).clicked() {
                                        edits.push((ms.id, TaskRef::Local(t.id), true));
                                    }
                                }
                            });
                        });
                    });
                })
                .response;
            if resp.interact(Sense::click()).clicked() {
                st.sel.select_task(ms.id);
            }
            ui.add_space(8.0);
        }
    });
    for (ms, r, add) in edits {
        let res = doc.try_edit(if add { "Add Milestone Task" } else { "Remove Milestone Task" }, ChangeSource::User, |p| {
            if add { p.add_milestone_members(ms, &[r]) } else { p.remove_milestone_members(ms, &[r]) }
        });
        if let Err(e) = res {
            st.toast(ToastKind::Error, e.to_string());
        }
    }
}

fn continue_label() -> (String, f32, Option<chrono::NaiveDateTime>) {
    ("<deleted task>".into(), 0.0, None)
}
