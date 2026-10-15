//! The File "backstage": full-window page for New, Open, Save, Recent and Info.

use egui::{Align2, Color32, FontId, Rect, RichText, Sense, Ui, pos2, vec2};

use crate::commands::Command;
use crate::format;
use crate::logo;
use crate::settings::AppSettings;
use crate::theme;

#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum Page {
    #[default]
    Home,
    Info,
}

pub struct BackstageInfo<'a> {
    pub has_doc: bool,
    pub doc_title: String,
    pub doc_path: Option<String>,
    pub stats: Option<plancraft_core::report::ProjectStats>,
    pub settings: &'a AppSettings,
    pub can_close: bool,
}

fn nav_item(ui: &mut Ui, label: &str, active: bool, enabled: bool) -> bool {
    let (r, resp) = ui.allocate_exact_size(
        vec2(ui.available_width(), 36.0),
        if enabled {
            Sense::click()
        } else {
            Sense::hover()
        },
    );
    if active {
        ui.painter().rect_filled(r, 0.0, theme::ACCENT_MID);
    } else if enabled && resp.hovered() {
        ui.painter()
            .rect_filled(r, 0.0, Color32::from_white_alpha(28));
    }
    ui.painter().text(
        pos2(r.left() + 22.0, r.center().y),
        Align2::LEFT_CENTER,
        label,
        FontId::proportional(14.0),
        if enabled {
            Color32::WHITE
        } else {
            Color32::from_white_alpha(100)
        },
    );
    resp.clicked()
}

fn tile(ui: &mut Ui, title: &str, subtitle: &str, draw: impl FnOnce(&egui::Painter, Rect)) -> bool {
    let (r, resp) = ui.allocate_exact_size(vec2(170.0, 150.0), Sense::click());
    let p = ui.painter();
    p.rect_filled(
        r,
        6.0,
        if resp.hovered() {
            theme::ACCENT_PALE
        } else {
            theme::BG
        },
    );
    p.rect_stroke(
        r,
        6.0,
        egui::Stroke::new(
            1.0,
            if resp.hovered() {
                theme::ACCENT_MID
            } else {
                theme::GRID_LINE_STRONG
            },
        ),
        egui::StrokeKind::Inside,
    );
    let art = Rect::from_min_size(r.min + vec2(14.0, 12.0), vec2(r.width() - 28.0, 84.0));
    draw(p, art);
    p.text(
        pos2(r.left() + 14.0, r.bottom() - 38.0),
        Align2::LEFT_CENTER,
        title,
        FontId::proportional(14.0),
        theme::TEXT,
    );
    p.text(
        pos2(r.left() + 14.0, r.bottom() - 18.0),
        Align2::LEFT_CENTER,
        subtitle,
        FontId::proportional(11.0),
        theme::TEXT_WEAK,
    );
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
        .clicked()
}

pub fn show(ui: &mut Ui, page: &mut Page, info: &BackstageInfo, out: &mut Vec<Command>) -> bool {
    let mut close = false;
    let rect = ui.max_rect();
    let nav = Rect::from_min_size(rect.min, vec2(200.0, rect.height()));
    ui.painter().rect_filled(rect, 0.0, theme::PANEL_BG);
    ui.painter().rect_filled(nav, 0.0, theme::ACCENT);
    let mut nav_ui = ui.new_child(egui::UiBuilder::new().max_rect(nav));
    nav_ui.spacing_mut().item_spacing.y = 0.0;
    nav_ui.add_space(14.0);
    if info.can_close && nav_item(&mut nav_ui, "⬅  Back", false, true) {
        close = true;
    }
    nav_ui.add_space(14.0);
    if nav_item(&mut nav_ui, "Home", *page == Page::Home, true) {
        *page = Page::Home;
    }
    if nav_item(&mut nav_ui, "Info", *page == Page::Info, info.has_doc) {
        *page = Page::Info;
    }
    if nav_item(&mut nav_ui, "New", false, true) {
        out.push(Command::NewProject);
        close = true;
    }
    if nav_item(&mut nav_ui, "Open…", false, true) {
        out.push(Command::Open);
    }
    nav_ui.add_space(10.0);
    if nav_item(&mut nav_ui, "Save", false, info.has_doc) {
        out.push(Command::Save);
    }
    if nav_item(&mut nav_ui, "Save As…", false, info.has_doc) {
        out.push(Command::SaveAs);
    }
    if nav_item(&mut nav_ui, "Export CSV…", false, info.has_doc) {
        out.push(Command::ExportCsv);
    }
    if nav_item(&mut nav_ui, "Close", false, info.has_doc) {
        out.push(Command::Close);
    }
    nav_ui.add_space(10.0);
    if nav_item(&mut nav_ui, "AI Agents (MCP)", false, true) {
        out.push(Command::McpPanel);
    }
    if nav_item(&mut nav_ui, "About", false, true) {
        out.push(Command::About);
    }
    if nav_item(&mut nav_ui, "Exit", false, true) {
        out.push(Command::Quit);
    }

    let content = Rect::from_min_max(
        pos2(nav.right() + 36.0, rect.top() + 24.0),
        rect.max - vec2(24.0, 24.0),
    );
    let mut ui2 = ui.new_child(egui::UiBuilder::new().max_rect(content));
    let ui = &mut ui2;
    match page {
        Page::Home => {
            ui.horizontal(|ui| {
                logo::paint_with_name(ui, 40.0, theme::ACCENT_DARK);
            });
            ui.add_space(18.0);
            ui.label(RichText::new("New").size(18.0));
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if tile(ui, "Blank Project", "Start from scratch", |p, r| {
                    p.rect_filled(r, 4.0, theme::BG);
                    for i in 0..4 {
                        p.hline(
                            (r.left() + 8.0)..=(r.right() - 8.0),
                            r.top() + 16.0 + i as f32 * 16.0,
                            egui::Stroke::new(1.0, theme::GRID_LINE),
                        );
                    }
                }) {
                    out.push(Command::NewProject);
                    close = true;
                }
                ui.add_space(12.0);
                if tile(
                    ui,
                    "Product Launch",
                    "Sample with milestones & team",
                    |p, r| {
                        p.rect_filled(r, 4.0, theme::BG);
                        let bars = [(0.05, 0.35), (0.25, 0.6), (0.45, 0.85), (0.62, 0.95)];
                        for (i, (a, b)) in bars.iter().enumerate() {
                            let y = r.top() + 12.0 + i as f32 * 17.0;
                            p.rect_filled(
                                Rect::from_min_max(
                                    pos2(r.left() + r.width() * a, y),
                                    pos2(r.left() + r.width() * b, y + 8.0),
                                ),
                                2.0,
                                theme::BAR_TASK,
                            );
                        }
                        let c = pos2(r.right() - 10.0, r.bottom() - 12.0);
                        p.add(egui::Shape::convex_polygon(
                            vec![
                                c + vec2(0.0, -6.0),
                                c + vec2(6.0, 0.0),
                                c + vec2(0.0, 6.0),
                                c + vec2(-6.0, 0.0),
                            ],
                            theme::GOLD,
                            egui::Stroke::NONE,
                        ));
                    },
                ) {
                    out.push(Command::NewDemo);
                    close = true;
                }
                ui.add_space(12.0);
                if tile(ui, "Open…", "Browse for a .plancraft file", |p, r| {
                    p.rect_filled(r, 4.0, theme::ACCENT_PALE);
                    p.text(
                        r.center(),
                        Align2::CENTER_CENTER,
                        "📂",
                        FontId::proportional(38.0),
                        theme::ACCENT,
                    );
                }) {
                    out.push(Command::Open);
                }
            });
            ui.add_space(22.0);
            ui.label(RichText::new("Recent").size(18.0));
            ui.add_space(4.0);
            if info.settings.recent.is_empty() {
                ui.label(
                    RichText::new("Projects you open will appear here.").color(theme::TEXT_WEAK),
                );
            }
            for path in &info.settings.recent {
                let name = path
                    .file_stem()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_default();
                let exists = path.exists();
                let resp = ui.add_enabled(
                    exists,
                    egui::Button::new(
                        RichText::new(format!(
                            "📄  {name}    {}",
                            path.parent()
                                .map(|p| p.display().to_string())
                                .unwrap_or_default()
                        ))
                        .size(13.0),
                    )
                    .frame(false),
                );
                if resp.clicked() {
                    out.push(Command::OpenPath(path.clone()));
                    close = true;
                }
            }
        }
        Page::Info => {
            ui.label(RichText::new("Info").size(24.0));
            ui.add_space(8.0);
            ui.label(RichText::new(&info.doc_title).size(16.0).strong());
            ui.label(
                RichText::new(
                    info.doc_path
                        .clone()
                        .unwrap_or_else(|| "Not saved yet".into()),
                )
                .color(theme::TEXT_WEAK),
            );
            ui.add_space(12.0);
            if ui.button("🗄  Project Information…").clicked() {
                out.push(Command::ProjectInfo);
                close = true;
            }
            if let Some(s) = &info.stats {
                ui.add_space(12.0);
                egui::Grid::new("bs_info")
                    .num_columns(2)
                    .spacing([24.0, 6.0])
                    .show(ui, |ui| {
                        ui.label("Start");
                        ui.label(format::date(s.start));
                        ui.end_row();
                        ui.label("Finish");
                        ui.label(format::date(s.finish));
                        ui.end_row();
                        ui.label("Tasks");
                        ui.label(format!("{} ({} milestones)", s.tasks, s.milestones));
                        ui.end_row();
                        ui.label("Resources");
                        ui.label(s.resources.to_string());
                        ui.end_row();
                        ui.label("Complete");
                        ui.label(format::percent(s.percent_complete));
                        ui.end_row();
                    });
            }
        }
    }
    close
}
