//! The Start Page: shown when no project is open, or from File ▸ Start Page.
//!
//! A centered, card-based launcher under the regular title band: start actions
//! on the left, recent projects on the right, a summary of the open project
//! when there is one, and work recovered after a crash.

use std::path::Path;

use egui::{Align, Align2, Color32, FontId, Layout, Rect, RichText, Sense, Stroke, Ui, pos2, vec2};

use crate::commands::Command;
use crate::format;
use crate::logo;
use crate::recovery::RecoveredItem;
use crate::settings::AppSettings;
use crate::theme;

pub struct StartInfo<'a> {
    pub has_doc: bool,
    pub doc_title: String,
    pub doc_path: Option<String>,
    pub stats: Option<plancraft_core::report::ProjectStats>,
    pub settings: &'a AppSettings,
    /// Recovery copies left by an earlier session that did not exit cleanly.
    pub recovered: &'a [RecoveredItem],
}

const MAX_WIDTH: f32 = 1000.0;
const CARD_RADIUS: u8 = 8;

fn card_frame(hovered: bool) -> egui::Frame {
    egui::Frame::new()
        .fill(theme::pal().BG)
        .stroke(Stroke::new(1.0, if hovered { theme::pal().ACCENT_MID } else { theme::pal().GRID_LINE }))
        .corner_radius(CARD_RADIUS)
        .inner_margin(egui::Margin::same(14))
}

fn section_title(ui: &mut Ui, text: &str) {
    ui.label(RichText::new(text).size(15.0).strong().color(theme::pal().TEXT));
    ui.add_space(6.0);
}

/// An action card: tinted icon badge, title and description. Returns true when clicked.
fn action(ui: &mut Ui, id: &str, icon: &str, title: &str, subtitle: &str) -> bool {
    let width = ui.available_width();
    let hovered = ui.ctx().data(|d| d.get_temp::<bool>(egui::Id::new(("start_action_hover", id)))).unwrap_or(false);
    let resp = card_frame(hovered)
        .show(ui, |ui| {
            ui.set_width(width - 30.0);
            ui.horizontal(|ui| {
                let (badge, _) = ui.allocate_exact_size(vec2(38.0, 38.0), Sense::hover());
                ui.painter().rect_filled(badge, 9.0, if hovered { theme::pal().ACCENT_LIGHT } else { theme::pal().ACCENT_PALE });
                ui.painter().text(badge.center(), Align2::CENTER_CENTER, icon, FontId::proportional(18.0), theme::pal().ACCENT);
                ui.add_space(6.0);
                ui.vertical(|ui| {
                    ui.add_space(1.0);
                    ui.label(RichText::new(title).size(14.0).color(theme::pal().TEXT));
                    ui.label(RichText::new(subtitle).size(11.5).color(theme::pal().TEXT_WEAK));
                });
            });
        })
        .response
        .interact(Sense::click())
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    ui.ctx().data_mut(|d| d.insert_temp(egui::Id::new(("start_action_hover", id)), resp.hovered()));
    ui.add_space(8.0);
    resp.clicked()
}

/// Shows paths under the home directory as `~/…`.
fn home_relative(path: &Path) -> String {
    let full = path.display().to_string();
    match std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) {
        Some(home) => {
            let home = home.to_string_lossy().to_string();
            match full.strip_prefix(&home) {
                Some(rest) if !home.is_empty() => format!("~{rest}"),
                _ => full,
            }
        }
        None => full,
    }
}

fn modified_label(path: &Path) -> Option<String> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    Some(time_label(modified.into(), false))
}

/// "Today 14:05", "Yesterday" (with the time when `with_time`), or a date.
fn time_label(dt: chrono::DateTime<chrono::Local>, with_time: bool) -> String {
    let today = chrono::Local::now().date_naive();
    let d = dt.date_naive();
    let time = if with_time { dt.format(" %H:%M").to_string() } else { String::new() };
    if d == today {
        format!("Today {}", dt.format("%H:%M"))
    } else if (today - d).num_days() == 1 {
        format!("Yesterday{time}")
    } else {
        format!("{}{time}", dt.format("%b %-d, %Y"))
    }
}

/// Work recovered after PlanCraft closed without saving: restore or discard each copy.
fn recovered_work(ui: &mut Ui, items: &[RecoveredItem], out: &mut Vec<Command>) {
    let width = ui.available_width();
    let resp = egui::Frame::new()
        .fill(theme::pal().BG)
        .stroke(Stroke::new(1.0, theme::pal().GRID_LINE))
        .corner_radius(CARD_RADIUS)
        .inner_margin(egui::Margin { left: 18, right: 14, top: 12, bottom: 8 })
        .show(ui, |ui| {
            ui.set_width(width - 34.0);
            ui.label(
                RichText::new("PlanCraft closed before these changes were saved. Restore a copy to keep working on it, or discard it.")
                    .size(11.5)
                    .color(theme::pal().TEXT_WEAK),
            );
            ui.add_space(4.0);
            for (i, item) in items.iter().enumerate() {
                let top = ui.cursor().top();
                if i > 0 {
                    ui.painter().hline(ui.max_rect().x_range(), top, Stroke::new(1.0, theme::pal().GRID_LINE));
                }
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    let (badge, _) = ui.allocate_exact_size(vec2(34.0, 34.0), Sense::hover());
                    ui.painter().rect_filled(badge, 8.0, theme::pal().ACCENT_PALE);
                    ui.painter().text(badge.center(), Align2::CENTER_CENTER, "⟳", FontId::proportional(17.0), theme::pal().ACCENT);
                    ui.add_space(4.0);
                    ui.vertical(|ui| {
                        ui.label(RichText::new(&item.name).size(13.5).color(theme::pal().TEXT));
                        let origin = item.original_path.as_deref().map(home_relative).unwrap_or_else(|| "Never saved to a file".into());
                        let when = item.saved_at.map(|t| format!("  ·  Copied {}", time_label(t, true))).unwrap_or_default();
                        ui.label(RichText::new(format!("{origin}{when}")).size(11.0).color(theme::pal().TEXT_WEAK))
                            .on_hover_text(item.original_path.as_ref().map(|p| p.display().to_string()).unwrap_or_default());
                    });
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.button("Discard").on_hover_text("Delete this recovery copy").clicked() {
                            out.push(Command::DiscardRecovered(item.stem.clone()));
                        }
                        let restore = egui::Button::new(RichText::new("Restore").color(Color32::WHITE)).fill(theme::pal().ACCENT);
                        let hint = match &item.original_path {
                            Some(_) => "Open this copy as an unsaved document; Save writes it back to the original file",
                            None => "Open this copy as an unsaved document",
                        };
                        if ui.add(restore).on_hover_text(hint).clicked() {
                            out.push(Command::RestoreRecovered(item.stem.clone()));
                        }
                    });
                });
                ui.add_space(6.0);
            }
        })
        .response;
    // A warm edge marks work that still needs a decision.
    let r = resp.rect;
    ui.painter().rect_filled(
        Rect::from_min_max(r.min, pos2(r.left() + 4.0, r.bottom())),
        egui::CornerRadius { nw: CARD_RADIUS, sw: CARD_RADIUS, ne: 0, se: 0 },
        theme::pal().WARN,
    );
}

fn recent_list(ui: &mut Ui, settings: &AppSettings, out: &mut Vec<Command>) {
    let width = ui.available_width();
    card_frame(false).inner_margin(egui::Margin::symmetric(6, 6)).show(ui, |ui| {
        ui.set_width(width - 14.0);
        if settings.recent.is_empty() {
            ui.add_space(18.0);
            ui.vertical_centered(|ui| {
                ui.label(RichText::new("No recent projects yet").color(theme::pal().TEXT_WEAK));
                ui.label(RichText::new("Projects you open or save appear here.").size(11.5).color(theme::pal().TEXT_WEAK));
            });
            ui.add_space(18.0);
            return;
        }
        for (i, path) in settings.recent.iter().enumerate() {
            let exists = path.exists();
            let name = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
            let folder = path.parent().map(home_relative).unwrap_or_default();
            let (rect, resp) =
                ui.allocate_exact_size(vec2(ui.available_width(), 46.0), if exists { Sense::click() } else { Sense::hover() });
            let p = ui.painter();
            if exists && resp.hovered() {
                p.rect_filled(rect, 6.0, theme::pal().ACCENT_PALE);
            }
            if i > 0 {
                p.hline((rect.left() + 10.0)..=(rect.right() - 10.0), rect.top(), Stroke::new(1.0, theme::pal().GRID_LINE));
            }
            let icon_color = if exists { theme::pal().ACCENT_MID } else { theme::pal().GRID_LINE_STRONG };
            p.text(pos2(rect.left() + 20.0, rect.center().y), Align2::CENTER_CENTER, "📄", FontId::proportional(16.0), icon_color);
            let text_color = if exists { theme::pal().TEXT } else { theme::pal().TEXT_WEAK };
            p.text(pos2(rect.left() + 40.0, rect.top() + 15.0), Align2::LEFT_CENTER, &name, FontId::proportional(13.5), text_color);
            let date = if exists { modified_label(path).unwrap_or_default() } else { "File not found".into() };
            let date_w = p.layout_no_wrap(date.clone(), FontId::proportional(11.5), theme::pal().TEXT_WEAK).size().x;
            p.text(
                pos2(rect.right() - 12.0, rect.center().y),
                Align2::RIGHT_CENTER,
                &date,
                FontId::proportional(11.5),
                theme::pal().TEXT_WEAK,
            );
            let folder_clip = Rect::from_min_max(pos2(rect.left() + 40.0, rect.top()), pos2(rect.right() - date_w - 24.0, rect.bottom()));
            p.with_clip_rect(folder_clip).text(
                pos2(rect.left() + 40.0, rect.top() + 32.0),
                Align2::LEFT_CENTER,
                &folder,
                FontId::proportional(11.0),
                theme::pal().TEXT_WEAK,
            );
            if resp.on_hover_text(path.display().to_string()).clicked() {
                out.push(Command::OpenPath(path.clone()));
            }
        }
    });
}

fn stat(ui: &mut Ui, label: &str, value: &str) {
    ui.vertical(|ui| {
        ui.label(RichText::new(label).size(11.0).color(theme::pal().TEXT_WEAK));
        ui.label(RichText::new(value).size(13.5).color(theme::pal().TEXT));
    });
    ui.add_space(14.0);
}

fn current_project(ui: &mut Ui, info: &StartInfo, out: &mut Vec<Command>) {
    let width = ui.available_width();
    let resp = egui::Frame::new()
        .fill(theme::pal().BG)
        .stroke(Stroke::new(1.0, theme::pal().GRID_LINE))
        .corner_radius(CARD_RADIUS)
        .inner_margin(egui::Margin { left: 18, right: 14, top: 14, bottom: 14 })
        .show(ui, |ui| {
            ui.set_width(width - 34.0);
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.label(RichText::new(&info.doc_title).size(16.0).strong());
                    ui.label(
                        RichText::new(info.doc_path.clone().unwrap_or_else(|| "Not saved yet".into()))
                            .size(11.5)
                            .color(theme::pal().TEXT_WEAK),
                    );
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.button("Close Project").clicked() {
                        out.push(Command::Close);
                    }
                    if ui.button("Project Information…").clicked() {
                        out.push(Command::ProjectInfo);
                    }
                });
            });
            if let Some(s) = &info.stats {
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    stat(ui, "Start", &format::date(s.start));
                    stat(ui, "Finish", &format::date(s.finish));
                    stat(ui, "Tasks", &s.tasks.to_string());
                    stat(ui, "Milestones", &s.milestones.to_string());
                    stat(ui, "Resources", &s.resources.to_string());
                    ui.vertical(|ui| {
                        ui.label(RichText::new("Complete").size(11.0).color(theme::pal().TEXT_WEAK));
                        ui.horizontal(|ui| {
                            let (bar, _) = ui.allocate_exact_size(vec2(120.0, 8.0), Sense::hover());
                            ui.painter().rect_filled(bar, 4.0, theme::pal().TRACK);
                            let w = bar.width() * (s.percent_complete / 100.0).clamp(0.0, 1.0);
                            ui.painter().rect_filled(Rect::from_min_size(bar.min, vec2(w, bar.height())), 4.0, theme::pal().ACCENT_MID);
                            ui.label(RichText::new(format::percent(s.percent_complete)).size(13.5));
                        });
                    });
                });
            }
        })
        .response;
    // An accent edge marks the open project.
    let r = resp.rect;
    ui.painter().rect_filled(
        Rect::from_min_max(r.min, pos2(r.left() + 4.0, r.bottom())),
        egui::CornerRadius { nw: CARD_RADIUS, sw: CARD_RADIUS, ne: 0, se: 0 },
        theme::pal().ACCENT,
    );
}

fn start_actions(ui: &mut Ui, width: f32, out: &mut Vec<Command>) {
    ui.set_width(width);
    section_title(ui, "Start");
    if action(ui, "blank", "➕", "Blank project", "An empty plan starting today") {
        out.push(Command::NewProject);
    }
    if action(ui, "sample", "📊", "Sample project", "A product launch with milestones and a team") {
        out.push(Command::NewDemo);
    }
    if action(ui, "open", "📂", "Open…", "Browse for a .plancraft file") {
        out.push(Command::Open);
    }
    if action(ui, "mcp", "⚡", "Connect an AI agent", "Let Claude Code and other MCP clients plan with you") {
        out.push(Command::McpPanel);
    }
}

/// Draws the Start Page. Returns true when the user asks to go back to the open project.
pub fn show(ui: &mut Ui, info: &StartInfo, out: &mut Vec<Command>) -> bool {
    let full = ui.max_rect();
    ui.painter().rect_filled(full, 0.0, theme::pal().PANEL_BG);
    let mut back = false;
    let width = (full.width() - 64.0).clamp(320.0, MAX_WIDTH);
    let column = Rect::from_min_size(pos2(full.center().x - width / 2.0, full.top()), vec2(width, full.height()));
    let mut col_ui = ui.new_child(egui::UiBuilder::new().max_rect(column));
    egui::ScrollArea::vertical().id_salt("start_page").auto_shrink([false, false]).show(&mut col_ui, |ui| {
        ui.set_width(width);
        ui.add_space(36.0);

        // Header: mark, name and tagline; "back" when a project is open.
        ui.horizontal(|ui| {
            let (r, _) = ui.allocate_exact_size(vec2(52.0, 52.0), Sense::hover());
            logo::paint(ui.painter(), r);
            ui.add_space(10.0);
            ui.vertical(|ui| {
                ui.add_space(2.0);
                ui.label(RichText::new("PlanCraft").size(26.0).color(theme::pal().ACCENT_DARK));
                ui.label(RichText::new("Plan the work, then work the plan.").size(13.0).color(theme::pal().TEXT_WEAK));
            });
            if info.has_doc {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let label = RichText::new(format!("Back to {}  ➡", info.doc_title)).color(Color32::WHITE);
                    if ui.add(egui::Button::new(label).fill(theme::pal().ACCENT).corner_radius(6.0).min_size(vec2(0.0, 30.0))).clicked() {
                        back = true;
                    }
                });
            }
        });
        ui.add_space(26.0);

        if !info.recovered.is_empty() {
            section_title(ui, "Recovered work");
            recovered_work(ui, info.recovered, out);
            ui.add_space(22.0);
        }

        if info.has_doc {
            current_project(ui, info, out);
            ui.add_space(22.0);
        }

        let two_columns = width >= 720.0;
        let left_w = if two_columns { 320.0 } else { width };
        if two_columns {
            ui.horizontal_top(|ui| {
                ui.vertical(|ui| start_actions(ui, left_w, out));
                ui.add_space(28.0);
                ui.vertical(|ui| {
                    section_title(ui, "Recent projects");
                    recent_list(ui, info.settings, out);
                });
            });
        } else {
            ui.vertical(|ui| start_actions(ui, left_w, out));
            ui.add_space(18.0);
            section_title(ui, "Recent projects");
            recent_list(ui, info.settings, out);
        }

        ui.add_space(30.0);
        ui.separator();
        ui.add_space(6.0);
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("Tips:").size(11.5).strong().color(theme::pal().TEXT_WEAK));
            ui.label(
                RichText::new(
                    "Insert adds a task · Alt+Shift+Right indents · drag a bar onto another to link · Ctrl+wheel zooms the timescale",
                )
                .size(11.5)
                .color(theme::pal().TEXT_WEAK),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(RichText::new(format!("v{}", env!("CARGO_PKG_VERSION"))).size(11.5).color(theme::pal().TEXT_WEAK));
            });
        });
        ui.add_space(24.0);
    });
    back
}
