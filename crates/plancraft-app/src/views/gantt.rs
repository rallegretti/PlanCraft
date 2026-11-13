//! Gantt Chart / Tracking Gantt view: task table on the left, chart on the right.

use std::collections::HashSet;

use egui::{CursorIcon, Rect, Sense, Stroke, Ui, pos2, vec2};
use plancraft_core::prelude::*;
use plancraft_core::schedule::task_calendar;
use plancraft_core::workspace::{ChangeSource, Document};

use crate::columns::{CellCtx, TaskCol};
use crate::commands::Command;
use crate::edit::EditExt;
use crate::fields::{self, Field};
use crate::state::{ToastKind, UiState};
use crate::theme;
use crate::views::gantt_chart::{self, ChartAction, ChartInput};
use crate::views::{GroupRow, Row};
use crate::widgets::grid::{self, GridConfig, GridEvent};

pub const BLANK_ROWS: usize = 20;

/// Blank rows below the tasks (for typing new tasks), only in the plain outline.
pub fn blank_rows(st: &UiState) -> usize {
    if st.filter == crate::state::TaskFilter::All && st.search.is_empty() && st.group.is_none() { BLANK_ROWS } else { 0 }
}

/// The cell of a group header row: the value and count under the name column, the span and
/// totals under date, work and cost columns.
fn group_cell(field: Field, p: &Project, g: &GroupRow) -> grid::Cell {
    let tasks: Vec<&Task> = g.tasks.iter().filter_map(|&i| p.tasks.get(i)).collect();
    let start = tasks.iter().filter_map(|t| t.sched.start).min();
    let finish = tasks.iter().filter_map(|t| t.sched.finish).max();
    let text = match field {
        Field::Builtin(TaskCol::Name) => format!("{}  ({})", g.label, g.tasks.len()),
        Field::Builtin(TaskCol::Start) => crate::format::date(start),
        Field::Builtin(TaskCol::Finish) => crate::format::date(finish),
        Field::Builtin(TaskCol::Work) => format_work(tasks.iter().fold(0, |a: Minutes, t| a.saturating_add(t.sched.work))),
        Field::Builtin(TaskCol::Cost) => crate::format::money(p, tasks.iter().map(|t| t.sched.cost).sum()),
        _ => String::new(),
    };
    grid::Cell {
        text,
        bold: true,
        bg: Some(theme::pal().HEADER_BG),
        expander: (field == Field::Builtin(TaskCol::Name)).then_some(!g.collapsed),
        ..Default::default()
    }
}

/// Shows the task table (shared by Gantt-type views). Returns the grid output.
pub fn task_table(
    ui: &mut Ui,
    rect: Rect,
    doc: &mut Document,
    st: &mut UiState,
    rows: &[Row],
    show_vscroll: bool,
    out: &mut Vec<Command>,
) -> grid::GridOutput {
    let fields: Vec<Field> = fields::table_fields(&doc.project, st.table);
    let sort = st.sort.clone();
    let defs: Vec<grid::ColumnDef> = fields
        .iter()
        .map(|f| {
            let desc = sort.as_ref().filter(|(k, _)| *k == f.key()).map(|(_, d)| *d);
            f.def(&doc.project).sorted(desc)
        })
        .collect();
    // Column widths follow the field, so adding or moving columns keeps everyone's width.
    st.task_grid.widths =
        fields.iter().zip(&defs).map(|(f, d)| st.column_widths.get(&(st.table, f.key())).copied().unwrap_or(d.width)).collect();
    let over_res: HashSet<ResourceId> = plancraft_core::workload::overallocations(&doc.project).into_iter().map(|(r, _)| r).collect();
    let over_tasks = super::overallocated_tasks(&doc.project, &over_res);
    let blank = blank_rows(st);
    // The cursor and an open editor follow their task when the rows re-sort or regroup (after
    // an edit, or an agent's change), so typing never lands on another task.
    if let Some((was, id)) = st.cursor_task
        && let Some((row, col)) = st.task_grid.cursor
        && row == was
        && super::task_at(&doc.project, rows, row).is_none_or(|t| t.id != id)
        && let Some(now) = doc.project.task_index(id).and_then(|i| super::row_of_task(rows, i))
    {
        st.task_grid.cursor = Some((now, col));
        if let Some(e) = st.task_grid.editing.as_mut()
            && e.row == was
        {
            e.row = now;
        }
        st.task_grid.scroll_to = Some(now);
    }
    let output = {
        let p = &doc.project;
        // Rows show the task's row number (what predecessors refer to), also when sorted or grouped.
        let row_label = |r: usize| rows.get(r).and_then(Row::task).map_or_else(String::new, |i| (i + 1).to_string());
        let cfg = GridConfig {
            id: egui::Id::new(("task_grid", st.table.label())),
            columns: &defs,
            row_count: rows.len(),
            blank_rows: blank,
            row_numbers: true,
            row_label: Some(&row_label),
            show_vscroll,
            header_height: theme::HEADER_HEIGHT,
            row_height: theme::ROW_HEIGHT,
        };
        let sel = &st.sel;
        let collapsed = &st.collapsed;
        let is_over = |id: TaskId| over_tasks.contains(&id);
        let ctx = CellCtx { overallocated: &is_over };
        let is_selected = |r: usize| match rows.get(r) {
            Some(Row::Group(g)) => {
                !g.tasks.is_empty() && g.tasks.iter().all(|&i| p.tasks.get(i).is_some_and(|t| sel.tasks.contains(&t.id)))
            }
            _ => super::task_at(p, rows, r).is_some_and(|t| sel.tasks.contains(&t.id)),
        };
        let grouped = st.group.is_some();
        let mut cell = |r: usize, c: usize| {
            let (Some(row), Some(&field)) = (rows.get(r), fields.get(c)) else { return grid::Cell::default() };
            let idx = match row {
                Row::Group(g) => return group_cell(field, p, g),
                Row::Task(idx) => *idx,
            };
            let mut cell = field.cell(p, idx, &ctx);
            if field == Field::Builtin(TaskCol::Name) {
                if grouped {
                    // Grouped rows are not an outline: no indent or summary toggles.
                    cell.indent = 14.0;
                    cell.expander = None;
                } else if cell.expander.is_some() {
                    cell.expander = Some(!p.tasks.get(idx).is_some_and(|t| collapsed.contains(&t.id)));
                } else {
                    cell.indent += 14.0;
                }
            }
            cell
        };
        grid::show(ui, rect, &mut st.task_grid, &cfg, &is_selected, &mut cell)
    };
    for (f, w) in fields.iter().zip(&st.task_grid.widths) {
        st.column_widths.insert((st.table, f.key()), *w);
    }

    for ev in output.events.clone() {
        match ev {
            GridEvent::Select { row, shift, ctrl } => super::select_row(&mut st.sel, &doc.project, rows, row, shift, ctrl),
            GridEvent::CursorRow { row, shift } => {
                // Arrowing onto a group header selects nothing (a click selects the group), so a
                // key pressed next can't act on the whole group by surprise.
                if matches!(rows.get(row), Some(Row::Group(_))) {
                    if !shift {
                        st.sel.tasks.clear();
                    }
                } else {
                    super::select_row(&mut st.sel, &doc.project, rows, row, shift, false);
                }
            }
            GridEvent::Activate { row, .. } => {
                if super::task_at(&doc.project, rows, row).is_some() {
                    super::select_row(&mut st.sel, &doc.project, rows, row, false, false);
                    out.push(Command::TaskInfo);
                }
            }
            GridEvent::ToggleExpand(row) => match rows.get(row) {
                Some(Row::Group(g)) => {
                    if !st.collapsed_groups.remove(&g.label) {
                        st.collapsed_groups.insert(g.label.clone());
                    }
                }
                _ => {
                    let Some(id) = super::task_at(&doc.project, rows, row).map(|t| t.id) else { continue };
                    if !st.collapsed.remove(&id) {
                        st.collapsed.insert(id);
                    }
                }
            },
            GridEvent::Commit { row, col, text } => {
                let (Some(&field), Some(def)) = (fields.get(col), defs.get(col)) else { continue };
                if row < rows.len() {
                    let Some(id) = super::task_at(&doc.project, rows, row).map(|t| t.id) else { continue };
                    let label = format!("Edit {}", def.title);
                    if let Err(e) = doc.edit_msg(&label, |p| field.commit(p, id, &text)) {
                        st.toast(ToastKind::Error, e);
                    }
                } else if !text.is_empty() {
                    // Typing into a blank row creates a task.
                    let r = doc.edit_msg("New Task", |p| {
                        let is_name = field == Field::Builtin(TaskCol::Name);
                        let name = if is_name { text.clone() } else { "New Task".to_string() };
                        let id = p.add_task(name, None);
                        if !is_name {
                            field.commit(p, id, &text)?;
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
            GridEvent::HeaderClick { col } => {
                // Ascending, then descending, then back to the outline order.
                let Some(key) = fields.get(col).map(Field::key) else { continue };
                let next = match &st.sort {
                    Some((k, false)) if *k == key => Some((key, true)),
                    Some((k, true)) if *k == key => None,
                    _ => Some((key, false)),
                };
                out.push(Command::SortBy(next));
            }
            GridEvent::HeaderMenu { col, pos } => {
                ui.ctx().data_mut(|d| d.insert_temp(egui::Id::new("column_menu"), (pos, col)));
            }
        }
    }
    st.cursor_task = st.task_grid.cursor.and_then(|(r, _)| super::task_at(&doc.project, rows, r).map(|t| (r, t.id)));
    output
}

fn st_context_menu(ui: &Ui, pos: egui::Pos2) {
    ui.ctx().data_mut(|d| d.insert_temp(egui::Id::new("task_ctx_menu"), pos));
}

/// Keeps a popup (an [`egui::Area`] at a stored position) open until a choice, Escape or a
/// click elsewhere. Returns whether it should close.
fn popup_area(ctx: &egui::Context, id: egui::Id, pos: egui::Pos2, add: impl FnOnce(&mut Ui) -> bool) -> bool {
    let mut chosen = false;
    let resp = egui::Area::new(id.with("area")).order(egui::Order::Foreground).fixed_pos(pos).show(ctx, |ui| {
        egui::Frame::menu(ui.style()).show(ui, |ui| {
            ui.set_min_width(200.0);
            chosen = add(ui);
        });
    });
    let clicked_outside = ctx.input(|i| i.pointer.any_pressed()) && !resp.response.contains_pointer() && resp.response.rect.width() > 0.0;
    let esc = ctx.input(|i| i.key_pressed(egui::Key::Escape));
    // The first frame after opening sees the opening click; ignore it.
    let fresh = ctx.data_mut(|d| {
        let f = d.get_temp::<bool>(id.with("fresh")).unwrap_or(true);
        d.insert_temp(id.with("fresh"), false);
        f
    });
    let close = chosen || esc || (clicked_outside && !fresh);
    if close {
        ctx.data_mut(|d| d.remove::<bool>(id.with("fresh")));
    }
    close
}

/// Shows the floating context menu requested by the task table, if any.
pub fn context_menu_popup(ctx: &egui::Context, out: &mut Vec<Command>) {
    let id = egui::Id::new("task_ctx_menu");
    let Some(pos) = ctx.data(|d| d.get_temp::<egui::Pos2>(id)) else {
        return;
    };
    let close = popup_area(ctx, id, pos, |ui| {
        let mut picked = false;
        crate::commands::task_context_menu(ui, &mut |c| {
            out.push(c);
            picked = true;
        });
        picked
    });
    if close {
        ctx.data_mut(|d| d.remove::<egui::Pos2>(id));
    }
}

/// Shows the column menu requested by a right-click on a task table header, if any: sort, group,
/// insert, hide and move columns, and custom fields.
pub fn column_menu_popup(ctx: &egui::Context, p: &Project, st: &UiState, out: &mut Vec<Command>) {
    let id = egui::Id::new("column_menu");
    let Some((pos, col)) = ctx.data(|d| d.get_temp::<(egui::Pos2, usize)>(id)) else {
        return;
    };
    let shown = fields::table_fields(p, st.table);
    let Some(field) = shown.get(col).copied() else {
        ctx.data_mut(|d| d.remove::<(egui::Pos2, usize)>(id));
        return;
    };
    let key = field.key();
    let close = popup_area(ctx, id, pos, |ui| {
        let mut cmd = None;
        ui.label(egui::RichText::new(field.title(p)).strong());
        ui.separator();
        if ui.button("Sort Ascending").clicked() {
            cmd = Some(Command::SortBy(Some((key.clone(), false))));
        }
        if ui.button("Sort Descending").clicked() {
            cmd = Some(Command::SortBy(Some((key.clone(), true))));
        }
        if st.sort.is_some() && ui.button("Clear Sort").clicked() {
            cmd = Some(Command::SortBy(None));
        }
        ui.separator();
        if ui.button("Group by This Field").clicked() {
            cmd = Some(Command::GroupBy(Some(key.clone())));
        }
        if st.group.is_some() && ui.button("Clear Group").clicked() {
            cmd = Some(Command::GroupBy(None));
        }
        ui.separator();
        ui.menu_button("Insert Column", |ui| {
            egui::ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
                for f in fields::all_fields(p).into_iter().filter(|f| !shown.contains(f)) {
                    if ui.button(f.title(p)).clicked() {
                        cmd = Some(Command::InsertColumn { at: col, key: f.key() });
                    }
                }
            });
        });
        if ui.add_enabled(shown.len() > 1, egui::Button::new("Hide Column")).clicked() {
            cmd = Some(Command::HideColumn(col));
        }
        if ui.add_enabled(col > 0, egui::Button::new("Move Left")).clicked() {
            cmd = Some(Command::MoveColumn { at: col, right: false });
        }
        if ui.add_enabled(col + 1 < shown.len(), egui::Button::new("Move Right")).clicked() {
            cmd = Some(Command::MoveColumn { at: col, right: true });
        }
        if p.table_layout(st.table.label()).is_some() && ui.button("Reset Columns").clicked() {
            cmd = Some(Command::ResetColumns);
        }
        ui.separator();
        if ui.button("Custom Fields…").clicked() {
            cmd = Some(Command::CustomFields);
        }
        let picked = cmd.is_some();
        out.extend(cmd);
        picked
    });
    if close {
        ctx.data_mut(|d| d.remove::<(egui::Pos2, usize)>(id));
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
    ui.painter().rect_filled(Rect::from_min_max(pos2(x - 2.0, rect.top()), pos2(x + 2.0, rect.bottom())), 0.0, theme::pal().HEADER_BG);
    ui.painter().vline(x + 2.0, rect.y_range(), Stroke::new(1.0, theme::pal().GRID_LINE_STRONG));
}

pub fn show(ui: &mut Ui, rect: Rect, doc: &mut Document, st: &mut UiState, tracking: bool, out: &mut Vec<Command>) {
    let rows = super::visible_rows(&doc.project, st);
    let table_w = st.table_width.min(rect.width() - 160.0).max(120.0);
    let table_rect = Rect::from_min_size(rect.min, vec2(table_w, rect.height()));
    let chart_rect = Rect::from_min_max(pos2(rect.left() + table_w + 4.0, rect.top()), rect.max);

    task_table(ui, table_rect, doc, st, &rows, false, out);
    splitter(ui, rect.left() + table_w + 2.0, rect, &mut st.table_width);

    let blank = blank_rows(st);
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
        gantt_chart::show(ui, chart_rect, &mut st.gantt, &mut st.task_grid.scroll_y, &input)
    };
    apply_chart_actions(doc, st, &rows, actions, out);
}

pub fn apply_chart_actions(doc: &mut Document, st: &mut UiState, rows: &[Row], actions: Vec<ChartAction>, out: &mut Vec<Command>) {
    for a in actions {
        match a {
            ChartAction::Select { task, shift, ctrl } => {
                if let Some(r) = doc.project.task_index(task).and_then(|i| super::row_of_task(rows, i)) {
                    super::select_row(&mut st.sel, &doc.project, rows, r, shift, ctrl);
                    st.task_grid.cursor = Some((r, st.task_grid.cursor.map(|c| c.1).unwrap_or(2)));
                    st.cursor_task = Some((r, task));
                } else {
                    st.sel.select_task(task);
                }
            }
            ChartAction::Open(task) => {
                st.sel.select_task(task);
                out.push(Command::TaskInfo);
            }
            ChartAction::MoveTo { task, start } => {
                let r = doc.edit_msg("Move Task", |p| {
                    let t = p.task_mut(task).ok_or("task not found")?;
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
                let r = doc.edit_msg("Change Duration", |p| {
                    let idx = p.task_index(task).ok_or("task not found")?;
                    let start = p.task(task).and_then(|t| t.sched.start).ok_or("task has no start")?;
                    let cal = task_calendar(p, idx);
                    let mut mins = cal.between(start, finish).max(0);
                    let day = p.settings.time.minutes_per_day.max(1);
                    if mins >= day / 2 {
                        mins = ((mins + day / 2) / day * day).max(day);
                    } else {
                        mins = (mins + 30) / 60 * 60;
                    }
                    let manual_finish = cal.add(start, mins);
                    if let Some(t) = p.task_mut(task)
                        && t.mode == ScheduleMode::Manual
                    {
                        t.manual_finish = Some(manual_finish);
                    }
                    p.set_duration(task, mins, false).map_err(|e| e.to_string())
                });
                if let Err(e) = r {
                    st.toast(ToastKind::Warning, e);
                }
            }
            ChartAction::SetProgress { task, pct } => {
                if let Err(e) = doc.try_edit("Update Progress", ChangeSource::User, |p| p.set_percent_complete(task, pct)) {
                    st.toast(ToastKind::Warning, e.to_string());
                }
            }
            ChartAction::Link { from, to } => {
                let r = doc.try_edit("Link Tasks", ChangeSource::User, |p| p.link(from.into(), to, LinkType::FS, Lag::None));
                if let Err(e) = r {
                    st.toast(ToastKind::Warning, e.to_string());
                }
            }
            ChartAction::Command(c) => out.push(c),
        }
    }
}
