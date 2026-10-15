//! Gantt Chart / Tracking Gantt view: task table on the left, chart on the right.

use std::collections::HashSet;

use egui::{CursorIcon, Rect, Sense, Stroke, Ui, pos2, vec2};
use plancraft_core::prelude::*;
use plancraft_core::schedule::task_calendar;
use plancraft_core::workspace::{ChangeSource, Document};

use crate::columns::{self, CellCtx, TaskCol};
use crate::commands::Command;
use crate::state::{ToastKind, UiState};
use crate::theme;
use crate::views::gantt_chart::{self, ChartAction, ChartInput};
use crate::widgets::grid::{self, GridConfig, GridEvent};

pub const BLANK_ROWS: usize = 20;

/// Shows the task table (shared by Gantt-type views). Returns the grid output.
pub fn task_table(
    ui: &mut Ui,
    rect: Rect,
    doc: &mut Document,
    st: &mut UiState,
    rows: &[usize],
    show_vscroll: bool,
    out: &mut Vec<Command>,
) -> grid::GridOutput {
    let cols: Vec<TaskCol> = st.table.columns();
    let defs: Vec<grid::ColumnDef> = cols.iter().map(|c| c.def(&doc.project)).collect();
    let over_res: HashSet<ResourceId> = plancraft_core::workload::overallocations(&doc.project)
        .into_iter()
        .map(|(r, _)| r)
        .collect();
    let over_tasks = super::overallocated_tasks(&doc.project, &over_res);
    let blank = if st.filter == crate::state::TaskFilter::All && st.search.is_empty() {
        BLANK_ROWS
    } else {
        0
    };
    let cfg = GridConfig {
        id: egui::Id::new(("task_grid", st.table.label())),
        columns: &defs,
        row_count: rows.len(),
        blank_rows: blank,
        row_numbers: true,
        show_vscroll,
        header_height: theme::HEADER_HEIGHT,
        row_height: theme::ROW_HEIGHT,
    };
    let output = {
        let p = &doc.project;
        let sel = &st.sel;
        let collapsed = &st.collapsed;
        let is_over = |id: TaskId| over_tasks.contains(&id);
        let ctx = CellCtx {
            overallocated: &is_over,
        };
        let is_selected = |r: usize| sel.tasks.contains(&p.tasks[rows[r]].id);
        let mut cell = |r: usize, c: usize| {
            let idx = rows[r];
            let mut cell = columns::cell(cols[c], p, idx, &ctx);
            if cols[c] == TaskCol::Name && cell.expander.is_some() {
                cell.expander = Some(!collapsed.contains(&p.tasks[idx].id));
            } else if cols[c] == TaskCol::Name {
                cell.indent += 14.0;
            }
            cell
        };
        grid::show(ui, rect, &mut st.task_grid, &cfg, &is_selected, &mut cell)
    };

    for ev in output.events.clone() {
        match ev {
            GridEvent::Select { row, shift, ctrl } => {
                super::select_row(&mut st.sel, &doc.project, rows, row, shift, ctrl)
            }
            GridEvent::CursorRow { row, shift } => {
                super::select_row(&mut st.sel, &doc.project, rows, row, shift, false)
            }
            GridEvent::Activate { row, .. } => {
                super::select_row(&mut st.sel, &doc.project, rows, row, false, false);
                out.push(Command::TaskInfo);
            }
            GridEvent::ToggleExpand(row) => {
                let id = doc.project.tasks[rows[row]].id;
                if !st.collapsed.remove(&id) {
                    st.collapsed.insert(id);
                }
            }
            GridEvent::Commit { row, col, text } => {
                let column = cols[col];
                if row < rows.len() {
                    let id = doc.project.tasks[rows[row]].id;
                    let label = format!("Edit {}", defs[col].title);
                    if let Err(e) = doc.try_edit(&label, ChangeSource::User, |p| {
                        columns::commit(column, p, id, &text)
                    }) {
                        st.toast(ToastKind::Error, e);
                    }
                } else if !text.is_empty() {
                    // Typing into a blank row creates a task.
                    let r = doc.try_edit("New Task", ChangeSource::User, |p| {
                        let name = if column == TaskCol::Name {
                            text.clone()
                        } else {
                            "New Task".to_string()
                        };
                        let id = p.add_task(name, None);
                        if column != TaskCol::Name {
                            columns::commit(column, p, id, &text)?;
                        }
                        Ok::<TaskId, String>(id)
                    });
                    match r {
                        Ok(id) => st.sel.select_task(id),
                        Err(e) => st.toast(ToastKind::Error, e),
                    }
                }
            }
            GridEvent::ContextMenu { pos, .. } => st_context_menu(ui, pos),
            GridEvent::DeleteKey => out.push(Command::DeleteSelection),
            GridEvent::InsertKey => out.push(Command::InsertTask),
            GridEvent::ClickEmpty { .. } => {
                st.sel.tasks.clear();
            }
        }
    }
    output
}

fn st_context_menu(ui: &Ui, pos: egui::Pos2) {
    ui.ctx()
        .data_mut(|d| d.insert_temp(egui::Id::new("task_ctx_menu"), pos));
}

/// Shows the floating context menu requested by the task table, if any.
pub fn context_menu_popup(ctx: &egui::Context, out: &mut Vec<Command>) {
    let id = egui::Id::new("task_ctx_menu");
    let Some(pos) = ctx.data(|d| d.get_temp::<egui::Pos2>(id)) else {
        return;
    };
    let mut close = false;
    let resp = egui::Area::new(id.with("area"))
        .order(egui::Order::Foreground)
        .fixed_pos(pos)
        .show(ctx, |ui| {
            egui::Frame::menu(ui.style()).show(ui, |ui| {
                ui.set_min_width(200.0);
                crate::commands::task_context_menu(ui, &mut |c| {
                    out.push(c);
                    close = true;
                });
            });
        });
    let clicked_outside = ctx.input(|i| i.pointer.any_pressed())
        && !resp.response.contains_pointer()
        && resp.response.rect.width() > 0.0;
    let esc = ctx.input(|i| i.key_pressed(egui::Key::Escape));
    // The first frame after opening sees the opening click; ignore it.
    let fresh = ctx.data_mut(|d| {
        let f = d.get_temp::<bool>(id.with("fresh")).unwrap_or(true);
        d.insert_temp(id.with("fresh"), false);
        f
    });
    if close || esc || (clicked_outside && !fresh) {
        ctx.data_mut(|d| {
            d.remove::<egui::Pos2>(id);
            d.remove::<bool>(id.with("fresh"));
        });
    }
}

/// Splitter between the table and the chart.
pub fn splitter(ui: &mut Ui, x: f32, rect: Rect, width: &mut f32) {
    let r = Rect::from_min_max(pos2(x - 3.0, rect.top()), pos2(x + 3.0, rect.bottom()));
    let resp = ui.interact(r, ui.id().with("splitter"), Sense::drag());
    if resp.hovered() || resp.dragged() {
        ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
    }
    if resp.dragged() {
        *width = (*width + resp.drag_delta().x).clamp(120.0, rect.width() - 160.0);
    }
    ui.painter().rect_filled(
        Rect::from_min_max(pos2(x - 2.0, rect.top()), pos2(x + 2.0, rect.bottom())),
        0.0,
        theme::HEADER_BG,
    );
    ui.painter().vline(
        x + 2.0,
        rect.y_range(),
        Stroke::new(1.0, theme::GRID_LINE_STRONG),
    );
}

pub fn show(
    ui: &mut Ui,
    rect: Rect,
    doc: &mut Document,
    st: &mut UiState,
    tracking: bool,
    out: &mut Vec<Command>,
) {
    let rows = super::visible_rows(&doc.project, st);
    let table_w = st.table_width.min(rect.width() - 160.0).max(120.0);
    let table_rect = Rect::from_min_size(rect.min, vec2(table_w, rect.height()));
    let chart_rect = Rect::from_min_max(pos2(rect.left() + table_w + 4.0, rect.top()), rect.max);

    task_table(ui, table_rect, doc, st, &rows, false, out);
    splitter(ui, rect.left() + table_w + 2.0, rect, &mut st.table_width);

    let blank = if st.filter == crate::state::TaskFilter::All && st.search.is_empty() {
        BLANK_ROWS
    } else {
        0
    };
    let actions = {
        let sel = &st.sel;
        let is_sel = |id: TaskId| sel.tasks.contains(&id);
        let input = ChartInput {
            project: &doc.project,
            rows: &rows,
            selected: &is_sel,
            tracking,
            row_height: theme::ROW_HEIGHT,
            header_height: theme::HEADER_HEIGHT,
            blank_rows: blank,
        };
        gantt_chart::show(
            ui,
            chart_rect,
            &mut st.gantt,
            &mut st.task_grid.scroll_y,
            &input,
        )
    };
    apply_chart_actions(doc, st, &rows, actions, out);
}

pub fn apply_chart_actions(
    doc: &mut Document,
    st: &mut UiState,
    rows: &[usize],
    actions: Vec<ChartAction>,
    out: &mut Vec<Command>,
) {
    for a in actions {
        match a {
            ChartAction::Select { task, shift, ctrl } => {
                if let Some(r) = rows.iter().position(|&i| doc.project.tasks[i].id == task) {
                    super::select_row(&mut st.sel, &doc.project, rows, r, shift, ctrl);
                    st.task_grid.cursor = Some((r, st.task_grid.cursor.map(|c| c.1).unwrap_or(2)));
                } else {
                    st.sel.select_task(task);
                }
            }
            ChartAction::Open(task) => {
                st.sel.select_task(task);
                out.push(Command::TaskInfo);
            }
            ChartAction::MoveTo { task, start } => {
                let r = doc.try_edit("Move Task", ChangeSource::User, |p| {
                    let idx = p.task_index(task).ok_or("task not found")?;
                    let t = &mut p.tasks[idx];
                    if t.mode == ScheduleMode::Manual {
                        let dur = t.sched.finish.zip(t.sched.start).map(|(f, s)| f - s).unwrap_or_default();
                        t.manual_start = Some(start);
                        t.manual_finish = Some(start + dur);
                    } else if t.actual_start.is_some() {
                        return Err("tasks that have started cannot be moved; change the actual start instead".to_string());
                    } else {
                        t.constraint = Constraint { kind: ConstraintKind::Snet, date: Some(start) };
                    }
                    Ok(())
                });
                if let Err(e) = r {
                    st.toast(ToastKind::Warning, e);
                }
            }
            ChartAction::SetFinish { task, finish } => {
                let r = doc.try_edit("Change Duration", ChangeSource::User, |p| {
                    let idx = p.task_index(task).ok_or("task not found")?;
                    let start = p.tasks[idx].sched.start.ok_or("task has no start")?;
                    let cal = task_calendar(p, idx);
                    let mut mins = cal.between(start, finish).max(0);
                    let day = p.settings.time.minutes_per_day.max(1);
                    if mins >= day / 2 {
                        mins = ((mins + day / 2) / day * day).max(day);
                    } else {
                        mins = (mins + 30) / 60 * 60;
                    }
                    if p.tasks[idx].mode == ScheduleMode::Manual {
                        p.tasks[idx].manual_finish = Some(cal.add(start, mins));
                    }
                    p.set_duration(task, mins, false).map_err(|e| e.to_string())
                });
                if let Err(e) = r {
                    st.toast(ToastKind::Warning, e);
                }
            }
            ChartAction::SetProgress { task, pct } => {
                let _ = doc.try_edit("Update Progress", ChangeSource::User, |p| {
                    p.set_percent_complete(task, pct)
                });
            }
            ChartAction::Link { from, to } => {
                let r = doc.try_edit("Link Tasks", ChangeSource::User, |p| {
                    p.link(from.into(), to, LinkType::FS, Lag::None)
                });
                if let Err(e) = r {
                    st.toast(ToastKind::Warning, e.to_string());
                }
            }
            ChartAction::Command(c) => out.push(c),
        }
    }
}
