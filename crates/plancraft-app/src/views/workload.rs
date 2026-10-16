//! Workload: utilization of every resource plus a resource graph of the
//! selected resource's load against its capacity.

use chrono::Duration;
use egui::{Align2, FontId, Rect, Sense, Stroke, Ui, pos2, vec2};
use plancraft_core::prelude::*;
use plancraft_core::workload::workload;
use plancraft_core::workspace::Document;

use crate::format;
use crate::state::UiState;
use crate::theme;
use crate::views::resource_usage::{period_label, period_toolbar, step};

pub fn show(ui: &mut Ui, rect: Rect, doc: &mut Document, st: &mut UiState) {
    let mut toolbar =
        ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_size(rect.min + vec2(8.0, 4.0), vec2(rect.width() - 16.0, 24.0))));
    let from = period_toolbar(&mut toolbar, st, &doc.project);
    let p = &doc.project;
    let nper = 16usize.max(((rect.width() - 120.0) / 56.0) as usize).min(60);
    let mut to = from;
    for _ in 0..nper {
        to = step(st.granularity, to, 1);
    }
    let report = workload(p, from, to - Duration::days(1), st.granularity);

    let area = Rect::from_min_max(pos2(rect.left() + 8.0, rect.top() + 34.0), pos2(rect.right() - 8.0, rect.bottom() - 6.0));
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(area));
    let ui = &mut child;

    ui.heading("Resource Workload");
    ui.label(
        egui::RichText::new(format!(
            "Planned work against available capacity from {} to {}. Capacity follows each resource's calendar, time off and availability.",
            format::day(from),
            format::day(to - Duration::days(1))
        ))
        .color(theme::pal().TEXT_WEAK),
    );
    ui.add_space(6.0);

    // ---- Summary table ----
    let table_h = (report.resources.len() as f32 * 24.0 + 30.0).min(area.height() * 0.45);
    egui::ScrollArea::vertical().id_salt("workload_table").max_height(table_h).show(ui, |ui| {
        egui::Grid::new("workload_grid").striped(true).min_col_width(60.0).spacing(vec2(14.0, 5.0)).show(ui, |ui| {
            for h in [
                "Resource",
                "Category",
                "Max. Units",
                "Total Work",
                "Points",
                "Capacity (view)",
                "Load (view)",
                "Utilization",
                "Overallocated",
            ] {
                ui.label(egui::RichText::new(h).strong().color(theme::pal().TEXT_WEAK));
            }
            ui.end_row();
            for (r, rl) in p.resources.iter().zip(&report.resources) {
                let selected = st.sel.resources.contains(&r.id);
                if ui.selectable_label(selected, &r.name).clicked() {
                    st.sel.select_resource(r.id);
                }
                ui.label(r.category.label());
                ui.label(if r.kind == ResourceType::Work { format::units(r.max_units) } else { "—".into() });
                ui.label(format_work(rl.total_work));
                ui.label(if rl.complexity_points > 0.0 { format!("{:.1}", rl.complexity_points) } else { "—".into() });
                ui.label(format::hours(rl.window_capacity));
                ui.label(format::hours(rl.window_load));
                let u = rl.utilization();
                ui.horizontal(|ui| {
                    let (bar_rect, _) = ui.allocate_exact_size(vec2(120.0, 14.0), Sense::hover());
                    ui.painter().rect_filled(bar_rect, 3.0, theme::pal().TRACK);
                    let w = (bar_rect.width() * (u.min(1.5) / 1.5) as f32).max(0.0);
                    let color = if u > 1.0 {
                        theme::pal().ERROR
                    } else if u > 0.85 {
                        theme::pal().WARN
                    } else {
                        theme::pal().ACCENT_MID
                    };
                    ui.painter().rect_filled(Rect::from_min_size(bar_rect.min, vec2(w, bar_rect.height())), 3.0, color);
                    let x100 = bar_rect.left() + bar_rect.width() / 1.5;
                    ui.painter().vline(x100, bar_rect.y_range(), Stroke::new(1.0, theme::pal().TEXT_WEAK));
                    ui.label(format!("{:.0}%", u * 100.0));
                });
                if rl.overallocated {
                    ui.label(egui::RichText::new(format!("⚠ {} day(s)", rl.overallocated_days.len())).color(theme::pal().ERROR));
                } else {
                    ui.label(egui::RichText::new("✔ OK").color(theme::pal().OK));
                }
                ui.end_row();
            }
            let unassigned: f64 = report.unassigned.iter().sum();
            if unassigned > 0.0 {
                ui.label(egui::RichText::new("Unassigned (estimates)").italics());
                ui.label("—");
                ui.label("—");
                ui.label(format::hours(unassigned));
                ui.end_row();
            }
        });
    });
    ui.add_space(10.0);

    // ---- Resource graph ----
    let sel = st.sel.single_resource().and_then(|id| p.resources.iter().position(|r| r.id == id)).or(if p.resources.is_empty() {
        None
    } else {
        Some(0)
    });
    let Some(rl) = sel.and_then(|ri| report.resources.get(ri)) else {
        ui.label("Add resources to see their workload.");
        return;
    };
    ui.label(egui::RichText::new(format!("Resource Graph — {}", rl.name)).strong().size(15.0));
    let graph = ui.available_rect_before_wrap();
    let graph = Rect::from_min_max(graph.min + vec2(50.0, 10.0), graph.max - vec2(10.0, 28.0));
    if graph.height() < 60.0 {
        return;
    }
    let painter = ui.painter_at(graph.expand2(vec2(50.0, 30.0)));
    let max_v = rl.periods.iter().map(|pl| pl.load.max(pl.capacity)).fold(60.0, f64::max) * 1.1;
    let y_of = |v: f64| graph.bottom() - (v / max_v) as f32 * graph.height();
    // Axis & gridlines.
    for k in 0..=4 {
        let v = max_v * k as f64 / 4.0;
        let y = y_of(v);
        painter.hline(graph.x_range(), y, Stroke::new(1.0, theme::pal().GRID_LINE));
        painter.text(
            pos2(graph.left() - 6.0, y),
            Align2::RIGHT_CENTER,
            format::hours(v),
            FontId::proportional(10.5),
            theme::pal().TEXT_WEAK,
        );
    }
    let n = rl.periods.len().max(1);
    let slot = graph.width() / n as f32;
    let mut cap_pts = Vec::new();
    for (i, pl) in rl.periods.iter().enumerate() {
        let x0 = graph.left() + i as f32 * slot + slot * 0.15;
        let x1 = x0 + slot * 0.7;
        let within = pl.load.min(pl.capacity);
        if within > 0.0 {
            painter.rect_filled(Rect::from_min_max(pos2(x0, y_of(within)), pos2(x1, graph.bottom())), 1.0, theme::pal().BAR_TASK);
        }
        if pl.load > pl.capacity + 0.5 {
            painter.rect_filled(Rect::from_min_max(pos2(x0, y_of(pl.load)), pos2(x1, y_of(pl.capacity))), 1.0, theme::pal().BAR_CRITICAL);
        }
        let xc = graph.left() + (i as f32 + 0.5) * slot;
        cap_pts.push(pos2(graph.left() + i as f32 * slot, y_of(pl.capacity)));
        cap_pts.push(pos2(graph.left() + (i + 1) as f32 * slot, y_of(pl.capacity)));
        if slot > 34.0 || i % ((40.0 / slot).ceil() as usize).max(1) == 0 {
            painter.text(
                pos2(xc, graph.bottom() + 10.0),
                Align2::CENTER_CENTER,
                period_label(st.granularity, pl.start),
                FontId::proportional(10.0),
                theme::pal().TEXT_WEAK,
            );
        }
    }
    painter.add(egui::Shape::line(cap_pts, Stroke::new(2.0, theme::pal().ACCENT_DARK)));
    // Legend.
    let ly = graph.bottom() + 24.0;
    let mut lx = graph.left();
    for (c, l) in
        [(theme::pal().BAR_TASK, "Allocated"), (theme::pal().BAR_CRITICAL, "Overallocated"), (theme::pal().ACCENT_DARK, "Capacity")]
    {
        painter.rect_filled(Rect::from_min_size(pos2(lx, ly - 5.0), vec2(10.0, 10.0)), 2.0, c);
        painter.text(pos2(lx + 14.0, ly), Align2::LEFT_CENTER, l, FontId::proportional(11.0), theme::pal().TEXT);
        lx += 110.0;
    }
}
