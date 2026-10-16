//! The "AI Agents (MCP)" panel: start/stop the live MCP server and show how
//! to connect Claude Code or other MCP clients.

use egui::{Context, RichText};

use crate::mcp_host::McpHost;
use crate::settings::AppSettings;
use crate::theme;

fn code_line(ui: &mut egui::Ui, text: &str) {
    ui.horizontal(|ui| {
        egui::Frame::new().fill(theme::pal().HEADER_BG).corner_radius(4.0).inner_margin(egui::Margin::symmetric(8, 4)).show(ui, |ui| {
            ui.label(RichText::new(text).monospace());
        });
        if ui.small_button("📋").on_hover_text("Copy").clicked() {
            ui.ctx().copy_text(text.to_string());
        }
    });
}

pub fn show(
    ctx: &Context,
    open: &mut bool,
    host: &mut McpHost,
    settings: &mut AppSettings,
    ws: &plancraft_core::workspace::SharedWorkspace,
) {
    egui::Window::new("AI Agents — MCP Server").id(egui::Id::new("mcp_panel"))
            .pivot(egui::Align2::CENTER_TOP)
            .default_pos(ctx.content_rect().center_top() + egui::vec2(0.0, 120.0)).open(open).default_width(600.0).show(ctx, |ui| {
        ui.label("PlanCraft includes a Model Context Protocol (MCP) server. AI agents such as Claude Code can read and edit every part of your projects: tasks, milestones, dependencies, resources, calendars, time tracking, workload and leveling.");
        ui.add_space(8.0);
        ui.label(RichText::new("Live connection to this window").strong().size(15.0));
        ui.label("Agents edit the documents open here. Changes appear immediately and can be undone like your own edits (marked ⚡ in the undo history).");
        ui.horizontal(|ui| {
            ui.label("Port:");
            let mut port = settings.mcp_port.to_string();
            if ui.add_enabled(!host.running(), egui::TextEdit::singleline(&mut port).desired_width(60.0)).changed()
                && let Ok(p) = port.parse()
            {
                settings.mcp_port = p;
            }
            if host.running() {
                if ui.button("■ Stop server").clicked() {
                    host.stop();
                }
                ui.label(RichText::new("⚡ running").color(theme::pal().OK));
            } else if ui.add(egui::Button::new(RichText::new("▶ Start server").color(egui::Color32::WHITE)).fill(theme::pal().ACCENT)).clicked() {
                host.start(ws.clone(), ctx.clone(), settings.mcp_port);
            }
            let mut auto = settings.mcp_autostart;
            if ui.checkbox(&mut auto, "Start automatically").changed() {
                settings.mcp_autostart = auto;
                settings.save();
            }
        });
        if let Some(e) = &host.error {
            ui.colored_label(theme::pal().ERROR, e);
        }
        if let Some(url) = host.url() {
            let url = url.as_str();
            ui.label("Add it to Claude Code:");
            code_line(ui, &format!("claude mcp add --transport http plancraft {url}"));
            ui.label(RichText::new(format!("Changes made by agents this session: {}", host.changes())).small().color(theme::pal().TEXT_WEAK));
        }
        ui.add_space(10.0);
        ui.separator();
        ui.label(RichText::new("Headless (stdio) mode").strong().size(15.0));
        ui.label("Agents can also run PlanCraft without the window, working directly on files:");
        let exe = std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_else(|_| "plancraft".into());
        code_line(ui, &format!("claude mcp add plancraft -- {exe} mcp path/to/project.plancraft"));
        ui.label(RichText::new("Only connect agents you trust: they can change and save your project files.").small().color(theme::pal().TEXT_WEAK));
    });
}
