//! Task Usage: tasks with their assignments and time-phased work.

use std::collections::BTreeMap;

use chrono::{Duration, NaiveDate};
use egui::{Align2, FontId, Rect, Stroke, Ui, pos2, vec2};
use plancraft_core::prelude::*;
use plancraft_core::workload::assignment_daily_work;
use plancraft_core::workspace::Document;

use crate::commands::Command;
use crate::format;
use crate::state::UiState;
use crate::theme;
use crate::views::resource_usage::{PERIOD_W, period_label, period_toolbar, step};
use crate::widgets::grid::{self, Cell, ColumnDef, GridConfig, GridEvent, scrollbar};

#[derive(Clone, Copy)]
enum Row {
    Task(usize),
    Assignment(usize, AssignmentId),
}

pub fn show(ui: &mut Ui, rect: Rect, doc: &mut Document, st: &mut UiState, out: &mut Vec<Command>) {
    let mut toolbar =
        ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_size(rect.min + vec2(8.0, 4.0), vec2(rect.width() - 16.0, 24.0))));
    let from = period_toolbar(&mut toolbar, st, &doc.project);
    let rect = Rect::from_min_max(pos2(rect.left(), rect.top() + 32.0), rect.max);
    let p = &doc.project;

    let left_w = 380.0f32.min(rect.width() * 0.45);
    let right = Rect::from_min_max(pos2(rect.left() + left_w + 4.0, rect.top()), rect.max);
    let nper = ((right.width() - 12.0) / PERIOD_W).ceil().max(1.0) as usize;
    let mut periods: Vec<NaiveDate> = Vec::with_capacity(nper);
    let mut d = from;
    for _ in 0..nper {
        periods.push(d);
        d = step(st.granularity, d, 1);
    }
    let end = d;
    let bucket = |day: NaiveDate| -> Option<usize> {
        if day < from || day >= end {
            return None;
        }
        let b = st.granularity.bucket_start(day);
        periods.binary_search(&b).ok()
    };

    let task_rows = super::visible_rows(p, st);
    let mut rows = Vec::new();
    for &i in &task_rows {
        rows.push(Row::Task(i));
        for a in p.assignments_for_task(p.tasks[i].id) {
            rows.push(Row::Assignment(i, a.id));
        }
    }
    // Time-phased values per assignment, summed into tasks (summaries roll up leaves).
    let mut per_assignment: BTreeMap<AssignmentId, Vec<f64>> = BTreeMap::new();
    for a in &p.assignments {
        let mut v = vec![0.0; nper];
        for (day, w) in assignment_daily_work(p, a) {
            if let Some(b) = bucket(day) {
                v[b] += w;
            }
        }
        per_assignment.insert(a.id, v);
    }
    let task_values = |i: usize| -> Vec<f64> {
        let mut v = vec![0.0; nper];
        let range = if p.has_children(i) { p.descendants_range(i) } else { i..i + 1 };
        for j in range {
            for a in p.assignments_for_task(p.tasks[j].id) {
                if let Some(av) = per_assignment.get(&a.id) {
                    for k in 0..nper {
                        v[k] += av[k];
                    }
                }
            }
        }
        v
    };

    let defs = vec![ColumnDef::new("Task Name", 260.0).read_only(), ColumnDef::new("Work", 80.0).right().read_only()];
    let cfg = GridConfig {
        id: egui::Id::new("task_usage_grid"),
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
        let is_sel = |r: usize| matches!(rows[r], Row::Task(i) if sel.tasks.contains(&p.tasks[i].id));
        let mut cell = |r: usize, c: usize| match rows[r] {
            Row::Task(i) => {
                let t = &p.tasks[i];
                if c == 0 {
                    Cell {
                        text: t.name.clone(),
                        bold: t.sched.is_summary,
                        indent: (t.outline_level - 1) as f32 * 14.0,
                        ..Default::default()
                    }
                } else {
                    Cell { text: format_work(t.sched.work), bold: t.sched.is_summary, ..Default::default() }
                }
            }
            Row::Assignment(i, aid) => {
                let t = &p.tasks[i];
                let a = p.assignment(aid);
                let name = a.and_then(|a| p.resource(a.resource)).map(|r| r.name.clone()).unwrap_or_default();
                if c == 0 {
                    Cell {
                        text: name,
                        indent: (t.outline_level - 1) as f32 * 14.0 + 22.0,
                        color: Some(theme::pal().ACCENT_DARK),
                        italic: false,
                        ..Default::default()
                    }
                } else {
                    Cell::text(a.map(|a| format_work(a.work)).unwrap_or_default())
                }
            }
        };
        grid::show(ui, left, &mut st.usage_grid, &cfg, &is_sel, &mut cell)
    };
    for ev in output.events {
        match ev {
            GridEvent::Select { row, .. } | GridEvent::CursorRow { row, .. } => {
                let i = match rows[row] {
                    Row::Task(i) | Row::Assignment(i, _) => i,
                };
                st.sel.select_task(p.tasks[i].id);
            }
            GridEvent::Activate { row, .. } => {
                let i = match rows[row] {
                    Row::Task(i) | Row::Assignment(i, _) => i,
                };
                st.sel.select_task(p.tasks[i].id);
                out.push(Command::TaskInfo);
            }
            _ => {}
        }
    }

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
    for (k, pd) in periods.iter().enumerate() {
        let x = right.left() + k as f32 * PERIOD_W;
        let cr = Rect::from_min_size(pos2(x, header.top() + 20.0), vec2(PERIOD_W, 20.0));
        painter.vline(cr.right(), header.y_range(), Stroke::new(1.0, theme::pal().GRID_LINE_STRONG));
        painter.with_clip_rect(cr).text(
            cr.center(),
            Align2::CENTER_CENTER,
            period_label(st.granularity, *pd),
            FontId::proportional(11.0),
            theme::pal().TEXT,
        );
        bp.vline(cr.right(), body.y_range(), Stroke::new(1.0, theme::pal().GRID_LINE));
    }
    painter.hline(header.x_range(), header.bottom() - 0.5, Stroke::new(1.0, theme::pal().GRID_LINE_STRONG));
    let rh = theme::ROW_HEIGHT;
    for (r, row) in rows.iter().enumerate() {
        let y = body.top() + r as f32 * rh - st.usage_grid.scroll_y;
        if y + rh < body.top() || y > body.bottom() {
            continue;
        }
        let (vals, is_task) = match row {
            Row::Task(i) => (task_values(*i), true),
            Row::Assignment(_, aid) => (per_assignment.get(aid).cloned().unwrap_or_default(), false),
        };
        if is_task {
            bp.rect_filled(Rect::from_min_max(pos2(body.left(), y), pos2(body.right(), y + rh)), 0.0, theme::pal().ACCENT_PALE);
        }
        for (k, v) in vals.iter().enumerate() {
            if *v > 0.5 {
                let x = right.left() + k as f32 * PERIOD_W;
                bp.text(
                    pos2(x + PERIOD_W - 5.0, y + rh / 2.0),
                    Align2::RIGHT_CENTER,
                    format::hours(*v),
                    FontId::proportional(12.0),
                    theme::pal().TEXT,
                );
            }
        }
        bp.hline(body.x_range(), y + rh - 0.5, Stroke::new(1.0, theme::pal().GRID_LINE));
    }
    let vs = Rect::from_min_max(pos2(right.right() - 12.0, body.top()), pos2(right.right(), body.bottom()));
    st.usage_grid.scroll_y =
        scrollbar(ui, egui::Id::new("tu_vs"), vs, true, st.usage_grid.scroll_y, rows.len() as f32 * rh + rh, body.height());
    if ui.rect_contains_pointer(body) {
        let dlt = ui.input(|i| i.smooth_scroll_delta.y);
        st.usage_grid.scroll_y = (st.usage_grid.scroll_y - dlt).clamp(0.0, (rows.len() as f32 * rh - body.height() + rh).max(0.0));
    }
    let _ = Duration::days(0);
}
