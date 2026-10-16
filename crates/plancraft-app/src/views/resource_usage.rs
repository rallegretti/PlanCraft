//! Resource Usage: resources and their assignments with time-phased work,
//! highlighting overallocation.

use chrono::{Datelike, Duration, NaiveDate};
use egui::{Align2, Color32, FontId, Rect, Sense, Stroke, Ui, pos2, vec2};
use plancraft_core::prelude::*;
use plancraft_core::workload::{AssignmentLoad, Granularity, ResourceLoad, workload};
use plancraft_core::workspace::Document;

use crate::commands::Command;
use crate::format;
use crate::state::UiState;
use crate::theme;
use crate::widgets::grid::{self, Cell, ColumnDef, GridConfig, GridEvent, scrollbar};

pub const PERIOD_W: f32 = 64.0;

#[derive(Clone, Copy)]
enum Row<'a> {
    Resource(&'a Resource, &'a ResourceLoad),
    Assignment(&'a Resource, &'a AssignmentLoad),
}

pub fn period_label(g: Granularity, d: NaiveDate) -> String {
    match g {
        Granularity::Day => d.format("%a %-m/%-d").to_string(),
        Granularity::Week => format!("Wk {}", d.format("%-m/%-d")),
        Granularity::Month => d.format("%b %Y").to_string(),
    }
}

/// Toolbar shared by the time-phased views. Returns the first period to show.
pub fn period_toolbar(ui: &mut Ui, st: &mut UiState, p: &Project) -> NaiveDate {
    let default_from = st.granularity.bucket_start(p.sched.start.unwrap_or(p.start).date());
    let mut from = st.usage_from.unwrap_or(default_from);
    ui.horizontal(|ui| {
        ui.label("Timescale:");
        for (g, l) in [(Granularity::Day, "Days"), (Granularity::Week, "Weeks"), (Granularity::Month, "Months")] {
            if ui.selectable_label(st.granularity == g, l).clicked() {
                st.granularity = g;
                from = g.bucket_start(from);
            }
        }
        ui.separator();
        if ui.button("⏴").on_hover_text("Earlier").clicked() {
            from = step(st.granularity, from, -4);
        }
        if ui.button("Project Start").clicked() {
            from = default_from;
        }
        if ui.button("Today").clicked() {
            from = st.granularity.bucket_start(chrono::Local::now().date_naive());
        }
        if ui.button("⏵").on_hover_text("Later").clicked() {
            from = step(st.granularity, from, 4);
        }
        ui.label(egui::RichText::new(format!("from {}", format::day(from))).color(theme::pal().TEXT_WEAK));
    });
    from = st.granularity.bucket_start(from);
    st.usage_from = Some(from);
    from
}

pub fn step(g: Granularity, d: NaiveDate, n: i32) -> NaiveDate {
    match g {
        Granularity::Day => d + Duration::days(n as i64),
        Granularity::Week => d + Duration::days(7 * n as i64),
        Granularity::Month => {
            let total = d.year() * 12 + d.month0() as i32 + n;
            NaiveDate::from_ymd_opt(total.div_euclid(12), total.rem_euclid(12) as u32 + 1, 1).unwrap_or(d)
        }
    }
}

pub fn show(ui: &mut Ui, rect: Rect, doc: &mut Document, st: &mut UiState, out: &mut Vec<Command>) {
    let mut toolbar =
        ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_size(rect.min + vec2(8.0, 4.0), vec2(rect.width() - 16.0, 24.0))));
    let from = period_toolbar(&mut toolbar, st, &doc.project);
    let rect = Rect::from_min_max(pos2(rect.left(), rect.top() + 32.0), rect.max);

    let p = &doc.project;
    let left_w = 330.0f32.min(rect.width() * 0.45);
    let right = Rect::from_min_max(pos2(rect.left() + left_w + 4.0, rect.top()), rect.max);
    let nper = ((right.width() - 12.0) / PERIOD_W).ceil().max(1.0) as usize;
    let mut to = from;
    for _ in 0..nper {
        to = step(st.granularity, to, 1);
    }
    let report = workload(p, from, to - Duration::days(1), st.granularity);

    // Rows.
    let mut rows: Vec<Row> = Vec::new();
    // The report lists resources in project order.
    for (r, rl) in p.resources.iter().zip(&report.resources) {
        rows.push(Row::Resource(r, rl));
        if !st.collapsed_resources.contains(&r.id) {
            rows.extend(rl.assignments.iter().map(|a| Row::Assignment(r, a)));
        }
    }

    // Left grid.
    let defs = vec![ColumnDef::new("Resource Name", 220.0).read_only(), ColumnDef::new("Work", 80.0).right().read_only()];
    let cfg = GridConfig {
        id: egui::Id::new("usage_grid"),
        columns: &defs,
        row_count: rows.len(),
        blank_rows: 0,
        row_numbers: false,
        show_vscroll: false,
        header_height: theme::HEADER_HEIGHT,
        row_height: theme::ROW_HEIGHT,
    };
    let left = Rect::from_min_size(rect.min, vec2(left_w, rect.height()));
    let output = {
        let sel = &st.sel;
        let collapsed = &st.collapsed_resources;
        let is_sel = |r: usize| matches!(rows.get(r), Some(Row::Resource(res, _)) if sel.resources.contains(&res.id));
        let mut cell = |r: usize, c: usize| match rows.get(r).copied() {
            None => Cell::default(),
            Some(Row::Resource(res, rl)) => {
                let over = rl.overallocated;
                let mut cl = if c == 0 {
                    Cell {
                        text: res.name.clone(),
                        bold: true,
                        expander: if rl.assignments.is_empty() { None } else { Some(!collapsed.contains(&res.id)) },
                        indent: if rl.assignments.is_empty() { 14.0 } else { 0.0 },
                        icons: if over {
                            vec![(
                                "⚠".into(),
                                theme::pal().ERROR,
                                format!("Overallocated on {} day(s) in view", rl.overallocated_days.len()),
                            )]
                        } else {
                            vec![]
                        },
                        ..Default::default()
                    }
                } else {
                    Cell { text: format_work(rl.total_work), bold: true, ..Default::default() }
                };
                if over {
                    cl.color = Some(theme::pal().ERROR);
                }
                cl
            }
            Some(Row::Assignment(_, a)) => {
                if c == 0 {
                    Cell { text: a.task_name.clone(), indent: 30.0, ..Default::default() }
                } else {
                    Cell::text(p.assignment(a.assignment).map(|x| format_work(x.work)).unwrap_or_default())
                }
            }
        };
        grid::show(ui, left, &mut st.usage_grid, &cfg, &is_sel, &mut cell)
    };
    for ev in output.events {
        match ev {
            GridEvent::ToggleExpand(r) | GridEvent::Activate { row: r, .. } => {
                if let Some(Row::Resource(res, _)) = rows.get(r) {
                    let id = res.id;
                    if !st.collapsed_resources.remove(&id) {
                        st.collapsed_resources.insert(id);
                    }
                }
            }
            GridEvent::Select { row, .. } | GridEvent::CursorRow { row, .. } => match rows.get(row) {
                Some(Row::Resource(res, _)) => st.sel.select_resource(res.id),
                Some(Row::Assignment(res, a)) => {
                    st.sel.select_resource(res.id);
                    st.sel.select_task(a.task);
                }
                None => {}
            },
            _ => {}
        }
    }
    let _ = out;

    // Right: time-phased grid.
    let painter = ui.painter_at(right);
    painter.rect_filled(right, 0.0, theme::pal().BG);
    let header = Rect::from_min_size(right.min, vec2(right.width() - 12.0, theme::HEADER_HEIGHT));
    painter.rect_filled(header, 0.0, theme::pal().HEADER_BG);
    let body = Rect::from_min_max(pos2(right.left(), header.bottom()), pos2(right.right() - 12.0, right.bottom() - 12.0));
    let bp = painter.with_clip_rect(body);
    painter.text(
        pos2(header.left() + 6.0, header.top() + 10.0),
        Align2::LEFT_CENTER,
        "Details: Work",
        FontId::proportional(11.0),
        theme::pal().TEXT_WEAK,
    );
    for (i, d) in report.periods.iter().enumerate() {
        let x = right.left() + i as f32 * PERIOD_W;
        let cr = Rect::from_min_size(pos2(x, header.top() + 20.0), vec2(PERIOD_W, 20.0));
        painter.vline(cr.right(), header.y_range(), Stroke::new(1.0, theme::pal().GRID_LINE_STRONG));
        painter.with_clip_rect(cr).text(
            cr.center(),
            Align2::CENTER_CENTER,
            period_label(st.granularity, *d),
            FontId::proportional(11.0),
            theme::pal().TEXT,
        );
        bp.vline(cr.right(), body.y_range(), Stroke::new(1.0, theme::pal().GRID_LINE));
    }
    painter.hline(header.x_range(), header.bottom() - 0.5, Stroke::new(1.0, theme::pal().GRID_LINE_STRONG));
    let rh = theme::ROW_HEIGHT;
    let font = FontId::proportional(12.0);
    for (r, row) in rows.iter().enumerate() {
        let y = body.top() + r as f32 * rh - st.usage_grid.scroll_y;
        if y + rh < body.top() || y > body.bottom() {
            continue;
        }
        for i in 0..report.periods.len() {
            let x = right.left() + i as f32 * PERIOD_W;
            let cr = Rect::from_min_size(pos2(x, y), vec2(PERIOD_W, rh));
            let (val, util, bold) = match row {
                Row::Resource(_, rl) => {
                    let pl = rl.periods.get(i);
                    (pl.map_or(0.0, |pl| pl.load), pl.map(|pl| pl.utilization()), true)
                }
                Row::Assignment(_, a) => (a.load.get(i).copied().unwrap_or(0.0), None, false),
            };
            if let Some(u) = util {
                bp.rect_filled(cr.shrink(0.5), 0.0, theme::utilization_color(u));
            }
            if val > 0.5 {
                let over = util.is_some_and(|u| u > 1.0001);
                let color = if over { theme::pal().ERROR } else { theme::pal().TEXT };
                let t = format::hours(val);
                bp.text(pos2(cr.right() - 5.0, cr.center().y), Align2::RIGHT_CENTER, &t, font.clone(), color);
                if bold {
                    bp.text(pos2(cr.right() - 4.5, cr.center().y), Align2::RIGHT_CENTER, &t, font.clone(), color);
                }
            }
        }
        bp.hline(body.x_range(), y + rh - 0.5, Stroke::new(1.0, theme::pal().GRID_LINE));
    }
    // Shared vertical scrollbar.
    let vs = Rect::from_min_max(pos2(right.right() - 12.0, body.top()), pos2(right.right(), body.bottom()));
    st.usage_grid.scroll_y =
        scrollbar(ui, egui::Id::new("usage_vs"), vs, true, st.usage_grid.scroll_y, rows.len() as f32 * rh + rh, body.height());
    if ui.rect_contains_pointer(body) {
        let d = ui.input(|i| i.smooth_scroll_delta);
        st.usage_grid.scroll_y = (st.usage_grid.scroll_y - d.y).clamp(0.0, (rows.len() as f32 * rh - body.height() + rh).max(0.0));
        if d.x.abs() > 20.0 {
            st.usage_from = Some(step(st.granularity, from, if d.x < 0.0 { 1 } else { -1 }));
        }
    }
    // Legend.
    let legend = Rect::from_min_max(pos2(right.left(), right.bottom() - 12.0), right.max);
    painter.rect_filled(legend, 0.0, theme::pal().PANEL_BG);
    let resp = ui.interact(body, egui::Id::new("usage_body"), Sense::hover());
    let _ = resp;
    let _ = Color32::WHITE;
}
