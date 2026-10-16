//! The title band and the two-row command bar.
//!
//! Layout:
//!
//! ```text
//! ┌──────────┬───────────────────────────────────────────────────────────┐
//! │  view    │ File ▾  Task  Resource  Project  Report  View  Format      │  tabs
//! │ selector ├───────────────────────────────────────────────────────────┤
//! │    ▾     │ [+ Task] [♦ Milestone] │ ⬅ ➡ │ [🔗 Link] … single toolbar  │  toolbar
//! └──────────┴───────────────────────────────────────────────────────────┘
//! ```
//!
//! The view selector spans both rows. Tabs are plain tabs, and the toolbar is
//! one row of compact inline buttons separated by thin dividers.

use std::cell::RefCell;

use egui::{Align, Align2, Color32, CornerRadius, FontId, Layout, Rect, Sense, Shape, Stroke, StrokeKind, Ui, pos2, vec2};
use plancraft_core::model::ScheduleMode;

use crate::columns::TaskTable;
use crate::commands::Command;
use crate::logo;
use crate::settings::AppSettings;
use crate::state::{MenuTab, TaskFilter, UiState, ViewKind};
use crate::theme;

pub struct MenuInfo {
    pub has_doc: bool,
    pub title: String,
    pub can_undo: bool,
    pub can_redo: bool,
    pub undo_label: Option<String>,
    pub task_selected: bool,
    pub resource_selected: bool,
    pub mcp_running: bool,
    pub mcp_url: Option<String>,
}

const TAB_ROW_H: f32 = 28.0;
const TOOL_ROW_H: f32 = 34.0;
const SELECTOR_W: f32 = 104.0;
const BUTTON_H: f32 = 26.0;

// ---------------------------------------------------------------------------
// Title band
// ---------------------------------------------------------------------------

/// The deep-blue band at the top: logo, quick access buttons, document title, MCP status.
pub fn title_bar(ui: &mut Ui, info: &MenuInfo, out: &mut Vec<Command>) {
    let rect = ui.available_rect_before_wrap();
    let rect = Rect::from_min_size(rect.min, vec2(rect.width(), 34.0));
    ui.painter().rect_filled(rect, 0.0, theme::pal().TITLE_BAR);
    let mut child =
        ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink2(vec2(8.0, 0.0))).layout(Layout::left_to_right(Align::Center)));
    let ui2 = &mut child;
    logo::paint_on_dark(ui2.painter(), Rect::from_center_size(pos2(ui2.cursor().left() + 11.0, rect.center().y), vec2(22.0, 22.0)));
    ui2.add_space(28.0);
    ui2.label(egui::RichText::new("PlanCraft").color(Color32::WHITE).size(14.0).strong());
    ui2.add_space(12.0);
    let qa = |ui: &mut Ui, icon: &str, tip: &str, enabled: bool| -> bool {
        let (r, resp) = ui.allocate_exact_size(vec2(26.0, 24.0), if enabled { Sense::click() } else { Sense::hover() });
        if enabled && resp.hovered() {
            ui.painter().rect_filled(r, 3.0, Color32::from_white_alpha(30));
        }
        let color = if enabled { Color32::WHITE } else { Color32::from_white_alpha(90) };
        ui.painter().text(r.center(), Align2::CENTER_CENTER, icon, FontId::proportional(14.0), color);
        resp.on_hover_text(tip).clicked()
    };
    if qa(ui2, "💾", "Save (Ctrl+S)", info.has_doc) {
        out.push(Command::Save);
    }
    let undo_tip = match &info.undo_label {
        Some(l) => format!("Undo {l} (Ctrl+Z)"),
        None => "Undo (Ctrl+Z)".into(),
    };
    if qa(ui2, "⟲", &undo_tip, info.can_undo) {
        out.push(Command::Undo);
    }
    if qa(ui2, "⟳", "Redo (Ctrl+Y)", info.can_redo) {
        out.push(Command::Redo);
    }
    let title = if info.has_doc { format!("{} - PlanCraft", info.title) } else { "PlanCraft".into() };
    ui2.painter().text(rect.center(), Align2::CENTER_CENTER, title, FontId::proportional(13.0), Color32::WHITE);
    ui2.with_layout(Layout::right_to_left(Align::Center), |ui| {
        let (txt, color) = if info.mcp_running {
            ("⚡ MCP live", Color32::from_rgb(0x8E, 0xF0, 0xB0))
        } else {
            ("○ MCP off", Color32::from_white_alpha(150))
        };
        let resp = ui.add(egui::Label::new(egui::RichText::new(txt).color(color).size(12.0)).sense(Sense::click()));
        let tip = match &info.mcp_url {
            Some(u) => format!("AI agents can control this window via MCP at {u}\nClick to manage."),
            None => "Start the built-in MCP server so AI agents (e.g. Claude Code) can work with your projects.".into(),
        };
        if resp.on_hover_text(tip).clicked() {
            out.push(Command::McpPanel);
        }
    });
    ui.allocate_rect(rect, Sense::hover());
}

// ---------------------------------------------------------------------------
// Toolbar widgets
// ---------------------------------------------------------------------------

/// Inline toolbar button: icon, optional label, tooltip.
fn button(ui: &mut Ui, icon: &str, label: Option<&str>, tip: &str, enabled: bool, active: bool) -> egui::Response {
    let font = FontId::proportional(12.5);
    let label_w = label.map(|l| ui.painter().layout_no_wrap(l.to_string(), font.clone(), theme::pal().TEXT).size().x + 6.0).unwrap_or(0.0);
    let w = 26.0 + label_w + if label.is_some() { 4.0 } else { 0.0 };
    let (rect, resp) = ui.allocate_exact_size(vec2(w, BUTTON_H), if enabled { Sense::click() } else { Sense::hover() });
    let p = ui.painter();
    if active {
        p.rect_filled(rect, 4.0, theme::pal().ACCENT_LIGHT);
        p.rect_stroke(rect, 4.0, Stroke::new(1.0, theme::pal().ACCENT_MID), StrokeKind::Inside);
    } else if enabled && resp.hovered() {
        p.rect_filled(rect, 4.0, theme::pal().ACCENT_PALE);
        p.rect_stroke(rect, 4.0, Stroke::new(1.0, theme::pal().ACCENT_LIGHT), StrokeKind::Inside);
    }
    let icon_color = if enabled { theme::pal().ACCENT } else { theme::pal().GRID_LINE_STRONG };
    p.text(pos2(rect.left() + 13.0, rect.center().y), Align2::CENTER_CENTER, icon, FontId::proportional(14.0), icon_color);
    if let Some(l) = label {
        p.text(
            pos2(rect.left() + 26.0, rect.center().y),
            Align2::LEFT_CENTER,
            l,
            font,
            if enabled { theme::pal().TEXT } else { theme::pal().TEXT_WEAK },
        );
    }
    let resp = if tip.is_empty() { resp } else { resp.on_hover_text(tip) };
    if enabled { resp.on_hover_cursor(egui::CursorIcon::PointingHand) } else { resp }
}

/// Thin vertical divider between groups of related buttons.
fn sep(ui: &mut Ui) {
    let (rect, _) = ui.allocate_exact_size(vec2(11.0, BUTTON_H), Sense::hover());
    ui.painter().vline(rect.center().x, (rect.top() + 4.0)..=(rect.bottom() - 4.0), Stroke::new(1.0, theme::pal().GRID_LINE_STRONG));
}

/// A mini pie button for "x% complete".
fn pie(ui: &mut Ui, pct: u8, enabled: bool) -> egui::Response {
    let (r, resp) = ui.allocate_exact_size(vec2(22.0, BUTTON_H), if enabled { Sense::click() } else { Sense::hover() });
    if enabled && resp.hovered() {
        ui.painter().rect_filled(r, 4.0, theme::pal().ACCENT_PALE);
    }
    let color = if enabled { theme::pal().ACCENT } else { theme::pal().GRID_LINE_STRONG };
    let c = r.center();
    ui.painter().circle_stroke(c, 6.5, Stroke::new(1.0, color));
    if pct >= 100 {
        ui.painter().circle_filled(c, 6.5, color);
    } else if pct > 0 {
        let n = 16;
        let mut pts = vec![c];
        for i in 0..=n {
            let a = -std::f32::consts::FRAC_PI_2 + (pct as f32 / 100.0) * std::f32::consts::TAU * i as f32 / n as f32;
            pts.push(c + vec2(a.cos(), a.sin()) * 6.5);
        }
        ui.painter().add(Shape::convex_polygon(pts, color, Stroke::NONE));
    }
    resp.on_hover_text(format!("Mark {pct}% complete"))
}

fn combo_label(ui: &mut Ui, text: &str) {
    ui.label(egui::RichText::new(text).size(12.5).color(theme::pal().TEXT_WEAK));
}

// ---------------------------------------------------------------------------
// View selector (spans both rows)
// ---------------------------------------------------------------------------

fn view_selector(ui: &mut Ui, rect: Rect, st: &UiState, enabled: bool, push: &dyn Fn(Command)) {
    let resp = ui.interact(rect, egui::Id::new("view_selector"), if enabled { Sense::click() } else { Sense::hover() });
    let p = ui.painter();
    p.rect_filled(rect, 0.0, if resp.hovered() && enabled { theme::pal().ACCENT_PALE } else { theme::pal().BG });
    p.vline(rect.right() - 0.5, rect.y_range(), Stroke::new(1.0, theme::pal().GRID_LINE_STRONG));
    let color = if enabled { theme::pal().ACCENT } else { theme::pal().GRID_LINE_STRONG };
    p.text(pos2(rect.center().x, rect.top() + 18.0), Align2::CENTER_CENTER, st.view.icon(), FontId::proportional(20.0), color);
    let galley = p.layout(st.view.label().to_string(), FontId::proportional(12.0), theme::pal().TEXT, rect.width() - 12.0);
    let gh = galley.size().y;
    p.galley(pos2(rect.center().x - galley.size().x / 2.0, rect.top() + 32.0), galley, theme::pal().TEXT);
    // Dropdown chevron.
    let cy = rect.top() + 32.0 + gh + 6.0;
    let c = pos2(rect.center().x, cy.min(rect.bottom() - 5.0));
    p.add(Shape::convex_polygon(vec![c + vec2(-4.0, -2.0), c + vec2(4.0, -2.0), c + vec2(0.0, 2.5)], theme::pal().TEXT_WEAK, Stroke::NONE));
    let resp = resp.on_hover_text("Change view");
    egui::Popup::menu(&resp).show(|ui| {
        ui.set_min_width(190.0);
        ui.label(egui::RichText::new("Task views").small().color(theme::pal().TEXT_WEAK));
        for v in ViewKind::ALL {
            if v == ViewKind::ResourceSheet {
                ui.separator();
                ui.label(egui::RichText::new("Resource views").small().color(theme::pal().TEXT_WEAK));
            }
            if ui.selectable_label(st.view == v, format!("{}  {}", v.icon(), v.label())).clicked() {
                push(Command::SetView(v));
            }
        }
    });
}

// ---------------------------------------------------------------------------
// Tabs
// ---------------------------------------------------------------------------

fn file_menu(ui: &mut Ui, info: &MenuInfo, settings: &AppSettings, push: &dyn Fn(Command)) {
    let doc = info.has_doc;
    let item = |ui: &mut Ui, label: &str, shortcut: &str, enabled: bool, cmd: Command| {
        let resp = ui.add_enabled(enabled, egui::Button::new(label).shortcut_text(shortcut));
        if resp.clicked() {
            push(cmd);
            ui.close();
        }
    };
    ui.set_min_width(220.0);
    item(ui, "New Project", "Ctrl+N", true, Command::NewProject);
    item(ui, "New from Sample", "", true, Command::NewDemo);
    item(ui, "Open…", "Ctrl+O", true, Command::Open);
    ui.menu_button("Open Recent", |ui| {
        if settings.recent.is_empty() {
            ui.label(egui::RichText::new("No recent projects").color(theme::pal().TEXT_WEAK));
        }
        for path in &settings.recent {
            let name = path.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
            let resp = ui.add_enabled(path.exists(), egui::Button::new(name)).on_hover_text(path.display().to_string());
            if resp.clicked() {
                push(Command::OpenPath(path.clone()));
                ui.close();
            }
        }
    });
    ui.separator();
    item(ui, "Save", "Ctrl+S", doc, Command::Save);
    item(ui, "Save As…", "Ctrl+Shift+S", doc, Command::SaveAs);
    item(ui, "Export CSV…", "", doc, Command::ExportCsv);
    ui.separator();
    item(ui, "Project Information…", "", doc, Command::ProjectInfo);
    item(ui, "Start Page", "", true, Command::StartPage);
    item(ui, "Close Project", "", doc, Command::Close);
    ui.separator();
    item(ui, "AI Agents (MCP)…", "", true, Command::McpPanel);
    item(ui, "About PlanCraft", "", true, Command::About);
    item(ui, "Exit", "", true, Command::Quit);
}

fn tab_strip(ui: &mut Ui, rect: Rect, st: &mut UiState, info: &MenuInfo, settings: &AppSettings, push: &dyn Fn(Command)) {
    let p = ui.painter().clone();
    p.rect_filled(rect, 0.0, theme::pal().PANEL_BG);
    p.hline(rect.x_range(), rect.bottom() - 0.5, Stroke::new(1.0, theme::pal().GRID_LINE_STRONG));
    let font = FontId::proportional(13.0);
    let mut x = rect.left() + 6.0;
    let tab_rect = |x: f32, w: f32| Rect::from_min_max(pos2(x, rect.top() + 4.0), pos2(x + w, rect.bottom()));

    // File menu, drawn as a tab with a chevron.
    let w = p.layout_no_wrap("File".into(), font.clone(), theme::pal().TEXT).size().x + 30.0;
    let r = tab_rect(x, w);
    let resp = ui.interact(r, egui::Id::new("tab_file"), Sense::click());
    let open = egui::Popup::is_id_open(ui.ctx(), egui::Popup::default_response_id(&resp));
    if resp.hovered() || open {
        p.rect_filled(r.shrink2(vec2(0.0, 1.0)), CornerRadius { nw: 5, ne: 5, sw: 0, se: 0 }, theme::pal().ACCENT_PALE);
    }
    p.text(pos2(r.left() + 10.0, r.center().y), Align2::LEFT_CENTER, "File", font.clone(), theme::pal().TEXT);
    let c = pos2(r.right() - 11.0, r.center().y);
    p.add(Shape::convex_polygon(vec![c + vec2(-3.5, -1.5), c + vec2(3.5, -1.5), c + vec2(0.0, 2.5)], theme::pal().TEXT_WEAK, Stroke::NONE));
    egui::Popup::menu(&resp).show(|ui| file_menu(ui, info, settings, push));
    x += w + 2.0;

    for tab in MenuTab::ALL {
        let w = p.layout_no_wrap(tab.label().into(), font.clone(), theme::pal().TEXT).size().x + 26.0;
        let r = tab_rect(x, w);
        let resp = ui.interact(r, egui::Id::new(("tab", tab.label())), Sense::click());
        let active = st.menu_tab == tab;
        if active {
            // Active tab: white, outlined, open at the bottom so it joins the toolbar row.
            let rr = CornerRadius { nw: 5, ne: 5, sw: 0, se: 0 };
            p.rect_filled(r, rr, theme::pal().BG);
            p.add(Shape::line(
                vec![pos2(r.left() + 0.5, r.bottom()), pos2(r.left() + 0.5, r.top() + 4.0), pos2(r.left() + 4.0, r.top() + 0.5)],
                Stroke::new(1.0, theme::pal().GRID_LINE_STRONG),
            ));
            p.hline((r.left() + 4.0)..=(r.right() - 4.0), r.top() + 0.5, Stroke::new(1.0, theme::pal().GRID_LINE_STRONG));
            p.add(Shape::line(
                vec![pos2(r.right() - 4.0, r.top() + 0.5), pos2(r.right() - 0.5, r.top() + 4.0), pos2(r.right() - 0.5, r.bottom())],
                Stroke::new(1.0, theme::pal().GRID_LINE_STRONG),
            ));
            p.hline((r.left() + 4.0)..=(r.right() - 4.0), r.top() + 1.5, Stroke::new(2.0, theme::pal().ACCENT));
            p.hline((r.left() + 1.0)..=(r.right() - 1.0), rect.bottom() - 0.5, Stroke::new(1.0, theme::pal().BG));
        } else if resp.hovered() {
            p.rect_filled(r.shrink2(vec2(0.0, 1.0)), CornerRadius { nw: 5, ne: 5, sw: 0, se: 0 }, theme::pal().TAB_HOVER);
        }
        let color = if active { theme::pal().ACCENT_DARK } else { theme::pal().TEXT };
        p.text(r.center() + vec2(0.0, 1.0), Align2::CENTER_CENTER, tab.label(), font.clone(), color);
        if resp.clicked() {
            st.menu_tab = tab;
        }
        x += w + 2.0;
    }

    // Right side: help/about.
    let r = Rect::from_center_size(pos2(rect.right() - 16.0, rect.center().y + 1.0), vec2(22.0, 20.0));
    let resp = ui.interact(r, egui::Id::new("tab_help"), Sense::click());
    if resp.hovered() {
        p.rect_filled(r, 4.0, theme::pal().ACCENT_PALE);
    }
    p.text(r.center(), Align2::CENTER_CENTER, "?", FontId::proportional(13.0), theme::pal().ACCENT);
    if resp.on_hover_text("About PlanCraft").clicked() {
        push(Command::About);
    }
}

// ---------------------------------------------------------------------------
// Toolbar contents per tab
// ---------------------------------------------------------------------------

fn toolbar(ui: &mut Ui, st: &mut UiState, info: &MenuInfo, settings: &AppSettings, push: &dyn Fn(Command)) {
    let doc = info.has_doc;
    let sel = info.task_selected && doc;
    let res = info.resource_selected && doc;
    let b = |ui: &mut Ui, icon: &str, label: Option<&str>, tip: &str, enabled: bool, cmd: Command| {
        if button(ui, icon, label, tip, enabled, false).clicked() {
            push(cmd);
        }
    };
    match st.menu_tab {
        MenuTab::Task => {
            b(ui, "➕", Some("Task"), "Insert a task above the selection (Insert)", doc, Command::InsertTask);
            b(ui, "♦", Some("Milestone"), "Insert a milestone", doc, Command::InsertMilestone);
            b(ui, "☰", Some("Summary"), "Group the selected tasks under a new summary task", doc, Command::InsertSummary);
            b(ui, "🗑", None, "Delete the selected tasks (Delete)", sel, Command::DeleteSelection);
            sep(ui);
            b(ui, "⬅", None, "Outdent (Alt+Shift+Left)", sel, Command::Outdent);
            b(ui, "➡", None, "Indent (Alt+Shift+Right)", sel, Command::Indent);
            b(ui, "⬆", None, "Move up (Alt+Shift+Up)", sel, Command::MoveUp);
            b(ui, "⬇", None, "Move down (Alt+Shift+Down)", sel, Command::MoveDown);
            sep(ui);
            b(ui, "🔗", Some("Link"), "Link the selected tasks finish-to-start (Ctrl+F2)", sel, Command::LinkSelected);
            b(ui, "⛓", Some("Unlink"), "Remove links between the selected tasks (Ctrl+Shift+F2)", sel, Command::UnlinkSelected);
            sep(ui);
            for pct in [0u8, 25, 50, 75, 100] {
                if pie(ui, pct, sel).clicked() {
                    push(Command::SetPercent(pct));
                }
            }
            b(ui, "✔", Some("On Track"), "Set progress to where the schedule says it should be today", sel, Command::MarkOnTrack);
            sep(ui);
            b(ui, "⟳", None, "Auto schedule the selected tasks", sel, Command::SetMode(ScheduleMode::Auto));
            b(ui, "📌", None, "Manually schedule the selected tasks", sel, Command::SetMode(ScheduleMode::Manual));
            b(ui, "⏸", None, "Inactivate / activate", sel, Command::ToggleActive);
            sep(ui);
            b(ui, "ℹ", Some("Information"), "Task information (Shift+F2)", sel, Command::TaskInfo);
            b(ui, "📝", None, "Task notes", sel, Command::TaskNotes);
            b(ui, "📅", None, "Show on the timeline", sel, Command::ToggleTimeline);
            b(ui, "🎯", None, "Scroll the chart to the task", sel, Command::ScrollToTask);
            sep(ui);
            b(ui, "✂", None, "Cut (Ctrl+X)", sel, Command::Cut);
            b(ui, "📋", None, "Copy (Ctrl+C)", sel, Command::Copy);
            b(ui, "📥", None, "Paste (Ctrl+V)", doc && !st.clipboard.is_empty(), Command::Paste);
        }
        MenuTab::Resource => {
            b(ui, "👥", Some("Assign Resources"), "Assign resources to the selected tasks", sel, Command::AssignResources);
            sep(ui);
            b(ui, "➕", Some("Add Resource"), "Add a resource", doc, Command::InsertResource);
            b(ui, "ℹ", Some("Information"), "Resource information, time off and availability", res, Command::ResourceInfo);
            b(ui, "🗑", None, "Delete the selected resources", res, Command::DeleteResources);
            sep(ui);
            b(ui, "⚖", Some("Level All"), "Delay tasks to resolve resource overallocations", doc, Command::LevelAll);
            b(ui, "↺", Some("Clear Leveling"), "Remove all leveling delays", doc, Command::ClearLeveling);
            sep(ui);
            b(ui, "⏱", Some("Timesheet"), "Log hours per resource and task", doc, Command::SetView(ViewKind::Timesheet));
            b(ui, "⟳", Some("Update Progress"), "Set % complete from tracked time", doc, Command::UpdateProgressFromTime);
        }
        MenuTab::Project => {
            b(ui, "🗄", Some("Project Information"), "Name, dates, calendar and scheduling options", doc, Command::ProjectInfo);
            b(ui, "📆", Some("Working Time"), "Calendars, work weeks and holidays", doc, Command::ChangeWorkingTime);
            sep(ui);
            b(ui, "🔗", Some("Links Between Projects"), "Depend on tasks in other project files", doc, Command::ExternalLinks);
            b(ui, "⟳", None, "Refresh links to other projects", doc, Command::RefreshExternal);
            sep(ui);
            b(ui, "📏", Some("Set Baseline"), "Save the current schedule as the baseline", doc, Command::SetBaseline);
            b(ui, "✖", None, "Clear the baseline", doc, Command::ClearBaseline);
            sep(ui);
            if button(ui, "⚡", Some("AI Agents"), "Built-in MCP server for AI agents", true, info.mcp_running).clicked() {
                push(Command::McpPanel);
            }
        }
        MenuTab::Report => {
            b(ui, "📊", Some("Statistics"), "Project statistics, variance and earned value", doc, Command::Statistics);
            b(ui, "♦", Some("Milestones"), "Milestone status report", doc, Command::SetView(ViewKind::Milestones));
            b(ui, "📶", Some("Workload"), "Resource utilization and resource graph", doc, Command::SetView(ViewKind::Workload));
            b(ui, "📈", Some("Tracking Gantt"), "Current schedule against the baseline", doc, Command::SetView(ViewKind::TrackingGantt));
            sep(ui);
            b(ui, "🗐", Some("Export CSV"), "Export the task list as CSV", doc, Command::ExportCsv);
        }
        MenuTab::View => {
            combo_label(ui, "Filter");
            egui::ComboBox::from_id_salt("tb_filter").width(130.0).selected_text(st.filter.label()).show_ui(ui, |ui| {
                for f in TaskFilter::ALL {
                    if ui.selectable_label(st.filter == f, f.label()).clicked() {
                        push(Command::Filter(f));
                    }
                }
            });
            combo_label(ui, "Table");
            egui::ComboBox::from_id_salt("tb_table").width(100.0).selected_text(st.table.label()).show_ui(ui, |ui| {
                for t in TaskTable::ALL {
                    if ui.selectable_label(st.table == t, t.label()).clicked() {
                        push(Command::Table(t));
                    }
                }
            });
            ui.add(egui::TextEdit::singleline(&mut st.search).desired_width(130.0).hint_text("🔍 Search tasks"));
            sep(ui);
            b(ui, "⊟", None, "Collapse all summaries", doc, Command::CollapseAll);
            b(ui, "⊞", None, "Expand all summaries", doc, Command::ExpandAll);
            sep(ui);
            b(ui, "🔍", None, "Zoom in (Ctrl++)", doc, Command::ZoomIn);
            b(ui, "🔎", None, "Zoom out (Ctrl+-)", doc, Command::ZoomOut);
            b(ui, "⛶", Some("Entire Project"), "Fit the whole project in the chart", doc, Command::ZoomEntire);
            b(ui, "🎯", Some("Selected"), "Fit the selected tasks", sel, Command::ZoomSelected);
            sep(ui);
            if button(ui, "📅", Some("Timeline"), "Show the timeline strip", doc, st.timeline_visible).clicked() {
                st.timeline_visible = !st.timeline_visible;
            }
            if button(ui, "🕘", Some("History"), "Undo history", doc, st.show_history).clicked() {
                push(Command::History);
            }
            sep(ui);
            combo_label(ui, "Theme");
            egui::ComboBox::from_id_salt("tb_theme").width(80.0).selected_text(settings.theme.label()).show_ui(ui, |ui| {
                for t in crate::theme::ThemeChoice::ALL {
                    if ui.selectable_label(settings.theme == t, t.label()).clicked() {
                        push(Command::SetTheme(t));
                    }
                }
            });
        }
        MenuTab::Format => {
            let g = &mut st.gantt;
            let toggle = |ui: &mut Ui, icon: &str, label: &str, tip: &str, v: &mut bool| {
                if button(ui, icon, Some(label), tip, doc, *v).clicked() {
                    *v = !*v;
                }
            };
            toggle(ui, "⚠", "Critical Path", "Highlight critical tasks", &mut g.show_critical);
            toggle(ui, "⌛", "Slack", "Show total slack after each task", &mut g.show_slack);
            toggle(ui, "📏", "Baseline", "Show baseline bars", &mut g.show_baseline);
            sep(ui);
            toggle(ui, "▣", "Progress", "Show progress inside bars", &mut g.show_progress);
            toggle(ui, "🔗", "Links", "Show dependency arrows", &mut g.show_links);
            toggle(ui, "👤", "Resource Names", "Show resource names beside bars", &mut g.show_resource_names);
            toggle(ui, "☰", "Project Summary", "Show the project summary bar", &mut g.show_project_summary);
        }
    }
}

// ---------------------------------------------------------------------------
// The whole command bar
// ---------------------------------------------------------------------------

/// Draws the view selector, tab row and toolbar row. Commands are appended to `out`.
pub fn command_bar(ui: &mut Ui, st: &mut UiState, info: &MenuInfo, settings: &AppSettings, out: &mut Vec<Command>) {
    let avail = ui.available_rect_before_wrap();
    let rect = Rect::from_min_size(avail.min, vec2(avail.width(), TAB_ROW_H + TOOL_ROW_H));
    ui.allocate_rect(rect, Sense::hover());
    let cmds = RefCell::new(Vec::new());
    let push = |c: Command| cmds.borrow_mut().push(c);

    let selector = Rect::from_min_size(rect.min, vec2(SELECTOR_W, rect.height()));
    let tabs = Rect::from_min_max(pos2(selector.right(), rect.top()), pos2(rect.right(), rect.top() + TAB_ROW_H));
    let tools = Rect::from_min_max(pos2(selector.right(), tabs.bottom()), rect.max);

    tab_strip(ui, tabs, st, info, settings, &push);

    ui.painter().rect_filled(tools, 0.0, theme::pal().BG);
    let mut child =
        ui.new_child(egui::UiBuilder::new().max_rect(tools.shrink2(vec2(6.0, 0.0))).layout(Layout::left_to_right(Align::Center)));
    egui::ScrollArea::horizontal()
        .id_salt("toolbar_scroll")
        .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
        .show(&mut child, |ui| {
            ui.set_height(TOOL_ROW_H);
            ui.spacing_mut().item_spacing.x = 2.0;
            ui.with_layout(Layout::left_to_right(Align::Center), |ui| toolbar(ui, st, info, settings, &push));
        });

    view_selector(ui, selector, st, info.has_doc, &push);
    ui.painter().hline(rect.x_range(), rect.bottom() - 0.5, Stroke::new(1.0, theme::pal().GRID_LINE_STRONG));
    out.extend(cmds.into_inner());
}
