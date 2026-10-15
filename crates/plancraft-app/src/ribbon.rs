//! The title bar, ribbon tabs and command groups.

use egui::{Align, Align2, Color32, FontId, Layout, Rect, Sense, Stroke, Ui, pos2, vec2};
use plancraft_core::model::ScheduleMode;

use crate::columns::TaskTable;
use crate::commands::Command;
use crate::logo;
use crate::state::{RibbonTab, TaskFilter, UiState, ViewKind};
use crate::theme;

pub struct RibbonInfo {
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

const LARGE_W: f32 = 58.0;
const CONTENT_H: f32 = 70.0;

fn large_button(ui: &mut Ui, icon: &str, label: &str, enabled: bool, active: bool) -> egui::Response {
    let galley = ui.painter().layout(label.to_string(), FontId::proportional(11.5), theme::TEXT, LARGE_W + 14.0);
    let w = LARGE_W.max(galley.size().x + 10.0);
    let (rect, resp) = ui.allocate_exact_size(vec2(w, CONTENT_H), if enabled { Sense::click() } else { Sense::hover() });
    let p = ui.painter();
    if active {
        p.rect_filled(rect, 3.0, theme::ACCENT_LIGHT);
    } else if enabled && resp.hovered() {
        p.rect_filled(rect, 3.0, theme::ACCENT_PALE);
        p.rect_stroke(rect, 3.0, Stroke::new(1.0, theme::ACCENT_LIGHT), egui::StrokeKind::Inside);
    }
    let color = if enabled { theme::ACCENT } else { theme::GRID_LINE_STRONG };
    p.text(pos2(rect.center().x, rect.top() + 20.0), Align2::CENTER_CENTER, icon, FontId::proportional(24.0), color);
    let text_color = if enabled { theme::TEXT } else { theme::TEXT_WEAK };
    let galley = ui.painter().layout(label.to_string(), FontId::proportional(11.5), text_color, w);
    let gp = pos2(rect.center().x - galley.size().x / 2.0, rect.top() + 38.0);
    ui.painter().galley(gp, galley, text_color);
    resp.on_hover_cursor(if enabled { egui::CursorIcon::PointingHand } else { egui::CursorIcon::Default })
}

fn small_button(ui: &mut Ui, icon: &str, label: &str, enabled: bool, active: bool) -> egui::Response {
    let font = FontId::proportional(12.0);
    let galley = ui.painter().layout_no_wrap(label.to_string(), font.clone(), theme::TEXT);
    let w = 26.0 + galley.size().x + 6.0;
    let (rect, resp) = ui.allocate_exact_size(vec2(w, 22.0), if enabled { Sense::click() } else { Sense::hover() });
    let p = ui.painter();
    if active {
        p.rect_filled(rect, 3.0, theme::ACCENT_LIGHT);
    } else if enabled && resp.hovered() {
        p.rect_filled(rect, 3.0, theme::ACCENT_PALE);
    }
    let color = if enabled { theme::ACCENT } else { theme::GRID_LINE_STRONG };
    p.text(pos2(rect.left() + 12.0, rect.center().y), Align2::CENTER_CENTER, icon, FontId::proportional(14.0), color);
    p.text(
        pos2(rect.left() + 24.0, rect.center().y),
        Align2::LEFT_CENTER,
        label,
        font,
        if enabled { theme::TEXT } else { theme::TEXT_WEAK },
    );
    resp
}

fn icon_button(ui: &mut Ui, icon: &str, tip: &str, enabled: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(24.0, 22.0), if enabled { Sense::click() } else { Sense::hover() });
    if enabled && resp.hovered() {
        ui.painter().rect_filled(rect, 3.0, theme::ACCENT_PALE);
    }
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        icon,
        FontId::proportional(13.0),
        if enabled { theme::ACCENT } else { theme::GRID_LINE_STRONG },
    );
    resp.on_hover_text(tip)
}

fn group(ui: &mut Ui, label: &str, add: impl FnOnce(&mut Ui)) {
    let r = ui.vertical(|ui| {
        ui.spacing_mut().item_spacing = vec2(2.0, 1.0);
        ui.allocate_ui_with_layout(vec2(0.0, CONTENT_H), Layout::left_to_right(Align::Min), |ui| {
            ui.set_min_height(CONTENT_H);
            add(ui);
        });
        ui.add_space(1.0);
        ui.label(egui::RichText::new(label).size(11.0).color(theme::TEXT_WEAK));
    });
    // Center the group label under its content.
    let _ = r;
    let rect = ui.max_rect();
    let x = ui.cursor().left() + 4.0;
    ui.painter().vline(x, (rect.top() + 4.0)..=(rect.bottom() - 4.0), Stroke::new(1.0, theme::GRID_LINE));
    ui.add_space(9.0);
}

fn stack(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing = vec2(0.0, 1.0);
        add(ui);
    });
}

/// The deep-blue band at the top: logo, quick access toolbar, document title, MCP status.
pub fn title_bar(ui: &mut Ui, info: &RibbonInfo, out: &mut Vec<Command>) {
    let rect = ui.available_rect_before_wrap();
    let rect = Rect::from_min_size(rect.min, vec2(rect.width(), 34.0));
    ui.painter().rect_filled(rect, 0.0, theme::ACCENT);
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
        ui.painter().text(
            r.center(),
            Align2::CENTER_CENTER,
            icon,
            FontId::proportional(14.0),
            if enabled { Color32::WHITE } else { Color32::from_white_alpha(90) },
        );
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

/// Tab strip with the File button.
pub fn tabs(ui: &mut Ui, st: &mut UiState, info: &RibbonInfo, out: &mut Vec<Command>) {
    let rect = ui.available_rect_before_wrap();
    let rect = Rect::from_min_size(rect.min, vec2(rect.width(), 28.0));
    ui.painter().rect_filled(rect, 0.0, theme::BG);
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink2(vec2(6.0, 0.0))).layout(Layout::left_to_right(Align::Max)));
    let ui2 = &mut child;
    // File tab (accent-filled, opens backstage).
    let (fr, fresp) = ui2.allocate_exact_size(vec2(52.0, 26.0), Sense::click());
    ui2.painter().rect_filled(
        fr,
        egui::CornerRadius { nw: 3, ne: 3, sw: 0, se: 0 },
        if fresp.hovered() { theme::ACCENT_MID } else { theme::ACCENT },
    );
    ui2.painter().text(fr.center(), Align2::CENTER_CENTER, "File", FontId::proportional(13.0), Color32::WHITE);
    if fresp.clicked() {
        st.backstage = true;
    }
    ui2.add_space(4.0);
    for tab in RibbonTab::ALL {
        let label = if tab == RibbonTab::Format { "Format" } else { tab.label() };
        let galley = ui2.painter().layout_no_wrap(label.into(), FontId::proportional(13.0), theme::TEXT);
        let (r, resp) = ui2.allocate_exact_size(vec2(galley.size().x + 22.0, 26.0), Sense::click());
        let active = st.ribbon_tab == tab && !st.ribbon_collapsed;
        if resp.hovered() && !active {
            ui2.painter().rect_filled(r, 2.0, theme::ACCENT_PALE);
        }
        let color = if active { theme::ACCENT } else { theme::TEXT };
        ui2.painter().text(r.center(), Align2::CENTER_CENTER, label, FontId::proportional(13.0), color);
        if active {
            ui2.painter().hline((r.left() + 6.0)..=(r.right() - 6.0), r.bottom() - 2.0, Stroke::new(3.0, theme::ACCENT));
        }
        if resp.clicked() {
            if st.ribbon_tab == tab {
                st.ribbon_collapsed = !st.ribbon_collapsed;
            } else {
                st.ribbon_tab = tab;
                st.ribbon_collapsed = false;
            }
        }
        if resp.double_clicked() {
            st.ribbon_collapsed = !st.ribbon_collapsed;
        }
    }
    ui2.with_layout(Layout::right_to_left(Align::Center), |ui| {
        if icon_button(ui, if st.ribbon_collapsed { "⏷" } else { "⏶" }, "Collapse / expand the ribbon", true).clicked() {
            st.ribbon_collapsed = !st.ribbon_collapsed;
        }
        if icon_button(ui, "?", "About PlanCraft", true).clicked() {
            out.push(Command::About);
        }
        let _ = info;
    });
    ui.allocate_rect(rect, Sense::hover());
    ui.painter().hline(rect.x_range(), rect.bottom() - 0.5, Stroke::new(1.0, theme::GRID_LINE));
}

fn view_menu(ui: &mut Ui, st: &UiState, push: &dyn Fn(Command)) {
    let resp = large_button(ui, st.view.icon(), &format!("{} ⏷", st.view.label()), true, false);
    egui::Popup::menu(&resp).show(|ui| {
        ui.label(egui::RichText::new("Task Views").small().color(theme::TEXT_WEAK));
        for v in ViewKind::ALL {
            if v == ViewKind::ResourceSheet {
                ui.separator();
                ui.label(egui::RichText::new("Resource Views").small().color(theme::TEXT_WEAK));
            }
            if ui.selectable_label(st.view == v, format!("{}  {}", v.icon(), v.label())).clicked() {
                push(Command::SetView(v));
            }
        }
    });
}

/// The ribbon body for the active tab.
pub fn body(ui: &mut Ui, st: &mut UiState, info: &RibbonInfo, out: &mut Vec<Command>) {
    let doc = info.has_doc;
    let sel = info.task_selected && doc;
    let cmds = std::cell::RefCell::new(Vec::new());
    let push = |c: Command| cmds.borrow_mut().push(c);
    ui.horizontal(|ui| {
        ui.set_height(CONTENT_H + 20.0);
        ui.add_space(6.0);
        match st.ribbon_tab {
            RibbonTab::Task => {
                group(ui, "View", |ui| view_menu(ui, st, &push));
                group(ui, "Clipboard", |ui| {
                    if large_button(ui, "📥", "Paste", sel || !st.clipboard.is_empty(), false).clicked() {
                        push(Command::Paste);
                    }
                    stack(ui, |ui| {
                        if small_button(ui, "✂", "Cut", sel, false).clicked() {
                            push(Command::Cut);
                        }
                        if small_button(ui, "📋", "Copy", sel, false).clicked() {
                            push(Command::Copy);
                        }
                    });
                });
                group(ui, "Schedule", |ui| {
                    stack(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 1.0;
                            for (pct, tip) in [
                                (0u8, "0% Complete"),
                                (25, "25% Complete"),
                                (50, "50% Complete"),
                                (75, "75% Complete"),
                                (100, "100% Complete"),
                            ] {
                                let label = format!("{pct}");
                                let (r, resp) = ui.allocate_exact_size(vec2(26.0, 22.0), if sel { Sense::click() } else { Sense::hover() });
                                if sel && resp.hovered() {
                                    ui.painter().rect_filled(r, 3.0, theme::ACCENT_PALE);
                                }
                                // Pie glyph showing the completion.
                                let c = pos2(r.center().x, r.center().y - 2.0);
                                ui.painter().circle_stroke(
                                    c,
                                    6.0,
                                    Stroke::new(1.0, if sel { theme::ACCENT } else { theme::GRID_LINE_STRONG }),
                                );
                                if pct > 0 {
                                    let n = 16;
                                    let mut pts = vec![c];
                                    for i in 0..=n {
                                        let a = -std::f32::consts::FRAC_PI_2
                                            + (pct as f32 / 100.0) * std::f32::consts::TAU * i as f32 / n as f32;
                                        pts.push(c + vec2(a.cos(), a.sin()) * 6.0);
                                    }
                                    ui.painter().add(egui::Shape::convex_polygon(
                                        pts,
                                        if sel { theme::ACCENT } else { theme::GRID_LINE_STRONG },
                                        Stroke::NONE,
                                    ));
                                }
                                ui.painter().text(
                                    pos2(r.center().x, r.bottom() - 2.0),
                                    Align2::CENTER_BOTTOM,
                                    &label,
                                    FontId::proportional(7.5),
                                    theme::TEXT_WEAK,
                                );
                                if resp.on_hover_text(tip).clicked() {
                                    push(Command::SetPercent(pct));
                                }
                            }
                        });
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 1.0;
                            if icon_button(ui, "⬅", "Outdent Task (Alt+Shift+Left)", sel).clicked() {
                                push(Command::Outdent);
                            }
                            if icon_button(ui, "➡", "Indent Task (Alt+Shift+Right)", sel).clicked() {
                                push(Command::Indent);
                            }
                            if icon_button(ui, "🔗", "Link the Selected Tasks (Ctrl+F2)", sel).clicked() {
                                push(Command::LinkSelected);
                            }
                            if icon_button(ui, "⛓", "Unlink Tasks (Ctrl+Shift+F2)", sel).clicked() {
                                push(Command::UnlinkSelected);
                            }
                        });
                    });
                    stack(ui, |ui| {
                        if small_button(ui, "✔", "Mark on Track", sel, false).clicked() {
                            push(Command::MarkOnTrack);
                        }
                        if small_button(ui, "⏸", "Inactivate", sel, false).clicked() {
                            push(Command::ToggleActive);
                        }
                        if small_button(ui, "⏱", "Update from Time", doc, false).clicked() {
                            push(Command::UpdateProgressFromTime);
                        }
                    });
                });
                group(ui, "Tasks", |ui| {
                    stack(ui, |ui| {
                        if small_button(ui, "📌", "Manually Schedule", sel, false).clicked() {
                            push(Command::SetMode(ScheduleMode::Manual));
                        }
                        if small_button(ui, "⟳", "Auto Schedule", sel, false).clicked() {
                            push(Command::SetMode(ScheduleMode::Auto));
                        }
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 1.0;
                            if icon_button(ui, "⬆", "Move Up", sel).clicked() {
                                push(Command::MoveUp);
                            }
                            if icon_button(ui, "⬇", "Move Down", sel).clicked() {
                                push(Command::MoveDown);
                            }
                        });
                    });
                });
                group(ui, "Insert", |ui| {
                    if large_button(ui, "➕", "Task", doc, false).clicked() {
                        push(Command::InsertTask);
                    }
                    stack(ui, |ui| {
                        if small_button(ui, "☰", "Summary", doc, false).clicked() {
                            push(Command::InsertSummary);
                        }
                        if small_button(ui, "♦", "Milestone", doc, false).clicked() {
                            push(Command::InsertMilestone);
                        }
                        if small_button(ui, "🗑", "Delete", sel, false).clicked() {
                            push(Command::DeleteSelection);
                        }
                    });
                });
                group(ui, "Properties", |ui| {
                    if large_button(ui, "ℹ", "Information", sel, false).clicked() {
                        push(Command::TaskInfo);
                    }
                    stack(ui, |ui| {
                        if small_button(ui, "📝", "Notes", sel, false).clicked() {
                            push(Command::TaskNotes);
                        }
                        if small_button(ui, "📅", "Add to Timeline", sel, false).clicked() {
                            push(Command::ToggleTimeline);
                        }
                        if small_button(ui, "🎯", "Scroll to Task", sel, false).clicked() {
                            push(Command::ScrollToTask);
                        }
                    });
                });
            }
            RibbonTab::Resource => {
                group(ui, "View", |ui| {
                    if large_button(ui, "👤", "Team\nPlanner", doc, st.view == ViewKind::TeamPlanner).clicked() {
                        push(Command::SetView(ViewKind::TeamPlanner));
                    }
                    stack(ui, |ui| {
                        if small_button(ui, "👥", "Resource Sheet", doc, st.view == ViewKind::ResourceSheet).clicked() {
                            push(Command::SetView(ViewKind::ResourceSheet));
                        }
                        if small_button(ui, "📋", "Resource Usage", doc, st.view == ViewKind::ResourceUsage).clicked() {
                            push(Command::SetView(ViewKind::ResourceUsage));
                        }
                        if small_button(ui, "📶", "Workload", doc, st.view == ViewKind::Workload).clicked() {
                            push(Command::SetView(ViewKind::Workload));
                        }
                    });
                });
                group(ui, "Assignments", |ui| {
                    if large_button(ui, "👥", "Assign\nResources", sel, false).clicked() {
                        push(Command::AssignResources);
                    }
                    if large_button(ui, "⏱", "Timesheet", doc, st.view == ViewKind::Timesheet).clicked() {
                        push(Command::SetView(ViewKind::Timesheet));
                    }
                });
                group(ui, "Insert", |ui| {
                    if large_button(ui, "➕", "Add\nResource", doc, false).clicked() {
                        push(Command::InsertResource);
                    }
                    stack(ui, |ui| {
                        if small_button(ui, "🗑", "Delete Resource", info.resource_selected, false).clicked() {
                            push(Command::DeleteResources);
                        }
                    });
                });
                group(ui, "Properties", |ui| {
                    if large_button(ui, "ℹ", "Information", info.resource_selected, false).clicked() {
                        push(Command::ResourceInfo);
                    }
                });
                group(ui, "Level", |ui| {
                    if large_button(ui, "⚖", "Level\nAll", doc, false).clicked() {
                        push(Command::LevelAll);
                    }
                    stack(ui, |ui| {
                        if small_button(ui, "↺", "Clear Leveling", doc, false).clicked() {
                            push(Command::ClearLeveling);
                        }
                    });
                });
            }
            RibbonTab::Report => {
                group(ui, "Reports", |ui| {
                    if large_button(ui, "📊", "Project\nStatistics", doc, false).clicked() {
                        push(Command::Statistics);
                    }
                    if large_button(ui, "♦", "Milestone\nReport", doc, st.view == ViewKind::Milestones).clicked() {
                        push(Command::SetView(ViewKind::Milestones));
                    }
                    if large_button(ui, "📶", "Workload", doc, st.view == ViewKind::Workload).clicked() {
                        push(Command::SetView(ViewKind::Workload));
                    }
                    if large_button(ui, "📈", "Tracking\nGantt", doc, st.view == ViewKind::TrackingGantt).clicked() {
                        push(Command::SetView(ViewKind::TrackingGantt));
                    }
                });
                group(ui, "Export", |ui| {
                    if large_button(ui, "🗐", "Export\nCSV", doc, false).clicked() {
                        push(Command::ExportCsv);
                    }
                });
            }
            RibbonTab::Project => {
                group(ui, "Properties", |ui| {
                    if large_button(ui, "🗄", "Project\nInformation", doc, false).clicked() {
                        push(Command::ProjectInfo);
                    }
                    if large_button(ui, "📆", "Change\nWorking Time", doc, false).clicked() {
                        push(Command::ChangeWorkingTime);
                    }
                });
                group(ui, "Links", |ui| {
                    if large_button(ui, "🔗", "Links Between\nProjects", doc, false).clicked() {
                        push(Command::ExternalLinks);
                    }
                    stack(ui, |ui| {
                        if small_button(ui, "⟳", "Refresh Links", doc, false).clicked() {
                            push(Command::RefreshExternal);
                        }
                    });
                });
                group(ui, "Schedule", |ui| {
                    if large_button(ui, "📏", "Set\nBaseline", doc, false).clicked() {
                        push(Command::SetBaseline);
                    }
                    stack(ui, |ui| {
                        if small_button(ui, "✖", "Clear Baseline", doc, false).clicked() {
                            push(Command::ClearBaseline);
                        }
                        if small_button(ui, "⏱", "Update Progress", doc, false).clicked() {
                            push(Command::UpdateProgressFromTime);
                        }
                    });
                });
                group(ui, "AI Agents", |ui| {
                    if large_button(ui, "⚡", "MCP\nServer", true, info.mcp_running).clicked() {
                        push(Command::McpPanel);
                    }
                });
                group(ui, "Options", |ui| {
                    if large_button(ui, "⚙", "Options", doc, false).clicked() {
                        push(Command::Options);
                    }
                });
            }
            RibbonTab::View => {
                group(ui, "Task Views", |ui| {
                    if large_button(ui, "📊", "Gantt\nChart", doc, st.view == ViewKind::Gantt).clicked() {
                        push(Command::SetView(ViewKind::Gantt));
                    }
                    stack(ui, |ui| {
                        for v in [ViewKind::TrackingGantt, ViewKind::TaskUsage, ViewKind::Calendar] {
                            if small_button(ui, v.icon(), v.label(), doc, st.view == v).clicked() {
                                push(Command::SetView(v));
                            }
                        }
                    });
                    stack(ui, |ui| {
                        if small_button(ui, ViewKind::Milestones.icon(), "Milestones", doc, st.view == ViewKind::Milestones).clicked() {
                            push(Command::SetView(ViewKind::Milestones));
                        }
                    });
                });
                group(ui, "Resource Views", |ui| {
                    if large_button(ui, "👤", "Team\nPlanner", doc, st.view == ViewKind::TeamPlanner).clicked() {
                        push(Command::SetView(ViewKind::TeamPlanner));
                    }
                    stack(ui, |ui| {
                        for v in [ViewKind::ResourceSheet, ViewKind::ResourceUsage, ViewKind::Workload] {
                            if small_button(ui, v.icon(), v.label(), doc, st.view == v).clicked() {
                                push(Command::SetView(v));
                            }
                        }
                    });
                    stack(ui, |ui| {
                        if small_button(ui, "⏱", "Timesheet", doc, st.view == ViewKind::Timesheet).clicked() {
                            push(Command::SetView(ViewKind::Timesheet));
                        }
                    });
                });
                group(ui, "Data", |ui| {
                    stack(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label("Filter:");
                            egui::ComboBox::from_id_salt("ribbon_filter").width(130.0).selected_text(st.filter.label()).show_ui(ui, |ui| {
                                for f in TaskFilter::ALL {
                                    if ui.selectable_label(st.filter == f, f.label()).clicked() {
                                        push(Command::Filter(f));
                                    }
                                }
                            });
                        });
                        ui.horizontal(|ui| {
                            ui.label("Table:");
                            egui::ComboBox::from_id_salt("ribbon_table").width(130.0).selected_text(st.table.label()).show_ui(ui, |ui| {
                                for t in TaskTable::ALL {
                                    if ui.selectable_label(st.table == t, t.label()).clicked() {
                                        push(Command::Table(t));
                                    }
                                }
                            });
                        });
                        ui.horizontal(|ui| {
                            ui.label("Search:");
                            ui.add(egui::TextEdit::singleline(&mut st.search).desired_width(124.0).hint_text("task name…"));
                        });
                    });
                    stack(ui, |ui| {
                        if small_button(ui, "⊟", "Collapse All", doc, false).clicked() {
                            push(Command::CollapseAll);
                        }
                        if small_button(ui, "⊞", "Expand All", doc, false).clicked() {
                            push(Command::ExpandAll);
                        }
                    });
                });
                group(ui, "Zoom", |ui| {
                    stack(ui, |ui| {
                        if small_button(ui, "🔍", "Zoom In", doc, false).clicked() {
                            push(Command::ZoomIn);
                        }
                        if small_button(ui, "🔎", "Zoom Out", doc, false).clicked() {
                            push(Command::ZoomOut);
                        }
                    });
                    if large_button(ui, "⛶", "Entire\nProject", doc, false).clicked() {
                        push(Command::ZoomEntire);
                    }
                    if large_button(ui, "🎯", "Selected\nTasks", sel, false).clicked() {
                        push(Command::ZoomSelected);
                    }
                });
                group(ui, "Split View", |ui| {
                    stack(ui, |ui| {
                        ui.checkbox(&mut st.timeline_visible, "Timeline");
                        if small_button(ui, "🕘", "Undo History", doc, st.show_history).clicked() {
                            push(Command::History);
                        }
                    });
                });
            }
            RibbonTab::Format => {
                group(ui, "Bar Styles", |ui| {
                    stack(ui, |ui| {
                        ui.checkbox(&mut st.gantt.show_critical, "Critical Tasks");
                        ui.checkbox(&mut st.gantt.show_slack, "Slack");
                        ui.checkbox(&mut st.gantt.show_baseline, "Baseline");
                    });
                    stack(ui, |ui| {
                        ui.checkbox(&mut st.gantt.show_progress, "Progress");
                        ui.checkbox(&mut st.gantt.show_links, "Links");
                        ui.checkbox(&mut st.gantt.show_resource_names, "Resource Names");
                    });
                });
                group(ui, "Show/Hide", |ui| {
                    stack(ui, |ui| {
                        ui.checkbox(&mut st.gantt.show_project_summary, "Project Summary Bar");
                        ui.checkbox(&mut st.timeline_visible, "Timeline");
                    });
                });
                group(ui, "Columns", |ui| {
                    stack(ui, |ui| {
                        ui.label("Table:");
                        egui::ComboBox::from_id_salt("fmt_table").width(120.0).selected_text(st.table.label()).show_ui(ui, |ui| {
                            for t in TaskTable::ALL {
                                if ui.selectable_label(st.table == t, t.label()).clicked() {
                                    push(Command::Table(t));
                                }
                            }
                        });
                    });
                });
            }
        }
    });
    out.extend(cmds.into_inner());
}
