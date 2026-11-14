//! Task Board: tasks as cards in status columns (see `plancraft_core::board`).
//!
//! - Click a card to select it (Ctrl adds to the selection); double-click opens Task Information.
//! - Drag cards to another column to change their Board Status.
//! - Right-click a column header to rename, move or delete the column, or set its % complete.
//! - "+ Add Column" adds a column on the right.
//! - The wheel scrolls the cards; Shift+wheel scrolls across the columns.

use std::sync::Arc;

use egui::text::{LayoutJob, TextWrapping};
use egui::{Align2, Color32, CursorIcon, FontId, Pos2, Rect, RichText, Sense, Stroke, StrokeKind, TextFormat, Ui, pos2, vec2};
use plancraft_core::ids::BoardColumnId;
use plancraft_core::prelude::*;
use plancraft_core::workspace::Document;

use crate::commands::Command;
use crate::edit::EditExt;
use crate::format;
use crate::state::{BoardDrag, ColumnEdit, ToastKind, UiState};
use crate::theme;

const COL_W: f32 = 250.0;
const GAP: f32 = 12.0;
const HEAD_H: f32 = 38.0;
const CARD_H: f32 = 62.0;
const CARD_GAP: f32 = 8.0;
const PAD: f32 = 8.0;
const BAR_W: f32 = 12.0;

/// One line of text cut to `width` with an ellipsis.
fn line(ui: &Ui, text: &str, size: f32, color: Color32, width: f32) -> Arc<egui::Galley> {
    let mut job = LayoutJob::default();
    job.append(&plancraft_core::bidi::display(text), 0.0, TextFormat { font_id: FontId::proportional(size), color, ..Default::default() });
    job.wrap = TextWrapping::truncate_at_width(width.max(1.0));
    ui.fonts_mut(|f| f.layout_job(job))
}

/// A column as shown: its id, name, Set % Complete and the rows of its cards (filtered).
struct Col {
    id: BoardColumnId,
    name: String,
    percent: Option<u8>,
    rows: Vec<usize>,
}

#[cfg(test)]
/// The centre of card `n` of column `k` on screen, with the board scrolled to `scroll`.
pub fn card_centre(body: Rect, scroll: egui::Vec2, k: usize, n: usize) -> Pos2 {
    pos2(
        body.left() + GAP + k as f32 * (COL_W + GAP) - scroll.x + COL_W / 2.0,
        body.top() + GAP / 2.0 + HEAD_H + PAD + n as f32 * (CARD_H + CARD_GAP) - scroll.y + CARD_H / 2.0,
    )
}

#[cfg(test)]
/// The centre of column `k`'s header on screen.
pub fn header_centre(body: Rect, scroll: egui::Vec2, k: usize) -> Pos2 {
    pos2(body.left() + GAP + k as f32 * (COL_W + GAP) - scroll.x + COL_W / 2.0, body.top() + GAP / 2.0 + HEAD_H / 2.0)
}

/// What the pointer is over.
#[derive(Clone, Copy, PartialEq)]
enum Hit {
    Card(usize),
    Header(usize),
    Column(usize),
    AddColumn,
}

pub fn show(ui: &mut Ui, rect: Rect, doc: &mut Document, st: &mut UiState, out: &mut Vec<Command>) {
    let cols: Vec<Col> = {
        let p = &doc.project;
        p.board()
            .into_iter()
            .map(|(c, rows)| Col {
                id: c.id,
                name: c.name.clone(),
                percent: c.percent_complete,
                rows: rows.into_iter().filter(|&i| p.tasks.get(i).is_some_and(|t| super::task_matches(p, st, t))).collect(),
            })
            .collect()
    };
    let body = Rect::from_min_max(rect.min, pos2(rect.right() - BAR_W, rect.bottom() - BAR_W));
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, theme::pal().BG);
    let resp = ui.allocate_rect(body, Sense::click_and_drag());
    st.board.body = body;

    // Scrolling: the wheel moves the cards, Shift+wheel moves across the columns.
    let content_w = GAP + (cols.len() as f32 + 0.6) * (COL_W + GAP);
    let most = cols.iter().map(|c| c.rows.len()).max().unwrap_or(0) as f32;
    let content_h = HEAD_H + PAD + most * (CARD_H + CARD_GAP) + PAD;
    if ui.rect_contains_pointer(body) {
        let (delta, shift) = ui.input(|i| (i.smooth_scroll_delta, i.modifiers.shift));
        if shift {
            st.board.scroll.x -= delta.y + delta.x;
        } else {
            st.board.scroll.x -= delta.x;
            st.board.scroll.y -= delta.y;
        }
    }
    let max_x = (content_w - body.width()).max(0.0);
    let max_y = (content_h - body.height()).max(0.0);
    st.board.scroll.x = st.board.scroll.x.clamp(0.0, max_x);
    st.board.scroll.y = st.board.scroll.y.clamp(0.0, max_y);
    // Bring the selected card into view (after Insert Task or Scroll to Task).
    if std::mem::take(&mut st.board.reveal_pending)
        && let Some(i) = st.sel.single_task().and_then(|id| doc.project.task_index(id))
        && let Some((k, n)) = cols.iter().enumerate().find_map(|(k, c)| c.rows.iter().position(|&r| r == i).map(|n| (k, n)))
    {
        let x = GAP + k as f32 * (COL_W + GAP);
        let y = HEAD_H + PAD + n as f32 * (CARD_H + CARD_GAP);
        if x < st.board.scroll.x || x + COL_W > st.board.scroll.x + body.width() {
            st.board.scroll.x = (x - GAP).clamp(0.0, max_x);
        }
        if y < st.board.scroll.y || y + CARD_H + HEAD_H > st.board.scroll.y + body.height() {
            st.board.scroll.y = (y - HEAD_H - PAD).clamp(0.0, max_y);
        }
    }
    let (sx, sy) = (st.board.scroll.x, st.board.scroll.y);

    let col_x = |k: usize| body.left() + GAP + k as f32 * (COL_W + GAP) - sx;
    let col_rect = |k: usize| Rect::from_min_max(pos2(col_x(k), body.top() + GAP / 2.0), pos2(col_x(k) + COL_W, body.bottom()));
    let card_rect = |k: usize, n: usize| {
        let x = col_x(k) + PAD;
        let y = body.top() + GAP / 2.0 + HEAD_H + PAD + n as f32 * (CARD_H + CARD_GAP) - sy;
        Rect::from_min_size(pos2(x, y), vec2(COL_W - 2.0 * PAD, CARD_H))
    };
    let add_rect = Rect::from_min_size(pos2(col_x(cols.len()), body.top() + GAP / 2.0), vec2(COL_W * 0.6, HEAD_H));
    let hit_at = |pos: Pos2| -> Option<Hit> {
        if !body.contains(pos) {
            return None;
        }
        if add_rect.contains(pos) {
            return Some(Hit::AddColumn);
        }
        let k = cols.iter().enumerate().position(|(k, _)| col_rect(k).x_range().contains(pos.x))?;
        if pos.y < body.top() + GAP / 2.0 + HEAD_H {
            return Some(Hit::Header(k));
        }
        let rows = &cols.get(k)?.rows;
        let top = body.top() + GAP / 2.0 + HEAD_H + PAD - sy;
        let n = ((pos.y - top) / (CARD_H + CARD_GAP)).floor();
        let card = (n >= 0.0).then_some(n as usize).filter(|&n| card_rect(k, n).contains(pos)).and_then(|n| rows.get(n).copied());
        card.map(Hit::Card).or(Some(Hit::Column(k)))
    };
    let pointer = ui.input(|i| i.pointer.hover_pos());
    // Where a drag ends (a touch release leaves no hover position).
    let latest = ui.input(|i| i.pointer.latest_pos());
    let hovered = pointer.and_then(hit_at);
    let dragging = st.board.drag.clone();
    let drop_col = match (&dragging, latest.and_then(hit_at)) {
        (Some(_), Some(Hit::Card(i))) => cols.iter().position(|c| c.rows.contains(&i)),
        (Some(_), Some(Hit::Header(k) | Hit::Column(k))) => Some(k),
        _ => None,
    };

    // ---- Columns and cards ----
    let p = &doc.project;
    let card_painter = painter.with_clip_rect(Rect::from_min_max(pos2(body.left(), body.top() + GAP / 2.0 + HEAD_H), body.max));
    let status = plancraft_core::report::status_moment(p);
    for (k, col) in cols.iter().enumerate() {
        let r = col_rect(k);
        if r.right() < body.left() || r.left() > body.right() {
            continue;
        }
        let fill = if drop_col == Some(k) { theme::pal().ACCENT_PALE } else { theme::pal().HEADER_BG };
        painter.with_clip_rect(body).rect_filled(r, 6.0, fill);
        // Header: name, card count and the % complete the column sets.
        let head = Rect::from_min_size(r.min, vec2(COL_W, HEAD_H));
        let hp = painter.with_clip_rect(body);
        let title = line(ui, &col.name, 14.0, theme::pal().TEXT, COL_W - 90.0);
        hp.galley(pos2(head.left() + 10.0, head.center().y - title.size().y / 2.0), title, theme::pal().TEXT);
        let mut tail = format!("{}", col.rows.len());
        if let Some(pct) = col.percent {
            tail = format!("{pct}% · {tail}");
        }
        hp.text(pos2(head.right() - 10.0, head.center().y), Align2::RIGHT_CENTER, tail, FontId::proportional(12.0), theme::pal().TEXT_WEAK);
        hp.hline(head.left() + 6.0..=head.right() - 6.0, head.bottom(), Stroke::new(1.0, theme::pal().GRID_LINE_STRONG));

        for (n, &i) in col.rows.iter().enumerate() {
            let cr = card_rect(k, n);
            if cr.bottom() < body.top() || cr.top() > body.bottom() {
                continue;
            }
            let Some(t) = p.tasks.get(i) else { continue };
            let selected = st.sel.tasks.contains(&t.id);
            let lifted = dragging.as_ref().is_some_and(|d| d.tasks.contains(&t.id));
            card(ui, &card_painter, cr, p, t, selected, lifted, report_late(t, status, p));
        }
    }
    // "+ Add Column" on the right.
    let add_hover = hovered == Some(Hit::AddColumn) && dragging.is_none();
    let ap = painter.with_clip_rect(body);
    ap.rect_stroke(
        add_rect,
        6.0,
        Stroke::new(1.0, if add_hover { theme::pal().ACCENT_MID } else { theme::pal().GRID_LINE_STRONG }),
        StrokeKind::Inside,
    );
    ap.text(add_rect.center(), Align2::CENTER_CENTER, "+ Add Column", FontId::proportional(13.0), theme::pal().ACCENT_MID);
    if cols.iter().all(|c| c.rows.is_empty()) {
        painter.text(
            body.center(),
            Align2::CENTER_CENTER,
            "No tasks on the board. New tasks start in the first column.",
            FontId::proportional(14.0),
            theme::pal().TEXT_WEAK,
        );
    }

    // ---- Interaction ----
    match (&dragging, hovered) {
        (Some(_), _) => ui.ctx().set_cursor_icon(CursorIcon::Grabbing),
        (None, Some(Hit::Card(_))) => ui.ctx().set_cursor_icon(CursorIcon::Grab),
        (None, Some(Hit::AddColumn)) => ui.ctx().set_cursor_icon(CursorIcon::PointingHand),
        _ => {}
    }
    let press = ui.input(|i| i.pointer.press_origin());
    if resp.drag_started()
        && let Some(Hit::Card(i)) = press.and_then(hit_at)
        && let Some(t) = p.tasks.get(i)
    {
        // Dragging a selected card moves the selected cards shown on the board; tasks selected
        // elsewhere (summaries, filtered out, hidden from the board) stay where they are.
        let tasks: Vec<TaskId> = if st.sel.tasks.contains(&t.id) {
            let shown: std::collections::HashSet<usize> = cols.iter().flat_map(|c| c.rows.iter().copied()).collect();
            st.sel.tasks.iter().copied().filter(|id| p.task_index(*id).is_some_and(|i| shown.contains(&i))).collect()
        } else {
            vec![t.id]
        };
        st.board.drag = Some(BoardDrag { tasks });
    }
    if let (Some(drag), Some(pp)) = (&dragging, pointer) {
        // The lifted card follows the pointer.
        if let Some(t) = drag.tasks.first().and_then(|id| p.task(*id)) {
            let ghost = Rect::from_center_size(pp, vec2(COL_W - 2.0 * PAD, CARD_H));
            let gp = ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Tooltip, ui.id().with("board_ghost")));
            gp.rect_filled(ghost, 6.0, theme::pal().BG);
            gp.rect_stroke(ghost, 6.0, Stroke::new(1.5, theme::pal().ACCENT), StrokeKind::Inside);
            let more = if drag.tasks.len() > 1 { format!("  (+{})", drag.tasks.len() - 1) } else { String::new() };
            let g = line(ui, &format!("{}{more}", t.name), 13.0, theme::pal().TEXT, ghost.width() - 16.0);
            gp.galley(pos2(ghost.left() + 8.0, ghost.center().y - g.size().y / 2.0), g, theme::pal().TEXT);
        }
    }
    if resp.drag_stopped()
        && let Some(drag) = st.board.drag.take()
        && let Some(k) = drop_col
        && let Some(col) = cols.get(k)
    {
        let moving: Vec<TaskId> =
            drag.tasks.into_iter().filter(|id| p.task_index(*id).and_then(|i| p.board_column_of(i)) != Some(col.id)).collect();
        if !moving.is_empty() {
            let (target, name) = (col.id, col.name.clone());
            if let Err(e) = doc.edit_msg(&format!("Move to {name}"), |p| p.set_board_status(&moving, target).map_err(|e| e.to_string())) {
                st.toast(ToastKind::Warning, e);
            }
        }
    }
    if !ui.input(|i| i.pointer.any_down()) || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        st.board.drag = None;
    }
    let p = &doc.project;
    if resp.clicked() {
        let ctrl = ui.input(|i| i.modifiers.command);
        match resp.interact_pointer_pos().and_then(hit_at) {
            Some(Hit::Card(i)) => {
                if let Some(id) = p.tasks.get(i).map(|t| t.id) {
                    if ctrl {
                        if !st.sel.tasks.remove(&id) {
                            st.sel.tasks.insert(id);
                            st.sel.anchor = Some(id);
                        }
                    } else {
                        st.sel.select_task(id);
                    }
                }
            }
            Some(Hit::AddColumn) => st.board.edit = Some(ColumnEdit::new_column()),
            Some(Hit::Column(_)) => st.sel.tasks.clear(),
            _ => {}
        }
    }
    if resp.double_clicked()
        && let Some(Hit::Card(i)) = resp.interact_pointer_pos().and_then(hit_at)
        && let Some(id) = p.tasks.get(i).map(|t| t.id)
    {
        st.sel.select_task(id);
        out.push(Command::TaskInfo);
    }
    if resp.secondary_clicked() {
        st.board.menu = resp.interact_pointer_pos().and_then(hit_at).map(|h| match h {
            Hit::Header(k) => cols.get(k).map(|c| c.id),
            _ => None,
        });
        match resp.interact_pointer_pos().and_then(hit_at) {
            Some(Hit::Card(i)) => {
                if let Some(id) = p.tasks.get(i).map(|t| t.id)
                    && !st.sel.tasks.contains(&id)
                {
                    st.sel.select_task(id);
                }
            }
            // An empty spot: the menu acts on nothing hidden.
            Some(Hit::Column(_)) => st.sel.tasks.clear(),
            _ => {}
        }
    }
    let menu = st.board.menu;
    let mut col_cmd: Option<ColumnCmd> = None;
    resp.context_menu(|ui| match menu {
        Some(Some(id)) => {
            let k = cols.iter().position(|c| c.id == id).unwrap_or(0);
            let mut item = |ui: &mut Ui, label: &str, enabled: bool, cmd: ColumnCmd| {
                if ui.add_enabled(enabled, egui::Button::new(label)).clicked() {
                    col_cmd = Some(cmd);
                    ui.close();
                }
            };
            item(ui, "📝  Rename / Set % Complete…", true, ColumnCmd::Edit(id));
            item(ui, "⬅  Move Left", k > 0, ColumnCmd::Move(id, k.saturating_sub(1)));
            item(ui, "➡  Move Right", k + 1 < cols.len(), ColumnCmd::Move(id, k + 1));
            ui.separator();
            item(ui, "➕  Add Column…", true, ColumnCmd::Add);
            item(ui, "🗑  Delete Column", cols.len() > 1, ColumnCmd::Delete(id));
        }
        _ => crate::commands::task_context_menu(ui, &mut |c| out.push(c)),
    });
    match col_cmd {
        Some(ColumnCmd::Edit(id)) => {
            if let Some(c) = doc.project.board_column(id) {
                st.board.edit = Some(ColumnEdit::existing(c));
            }
        }
        Some(ColumnCmd::Add) => st.board.edit = Some(ColumnEdit::new_column()),
        Some(ColumnCmd::Move(id, to)) => {
            if let Err(e) = doc.edit_msg("Move Column", |p| p.move_board_column(id, to).map_err(|e| e.to_string())) {
                st.toast(ToastKind::Warning, e);
            }
        }
        Some(ColumnCmd::Delete(id)) => match doc.edit_msg("Delete Column", |p| p.delete_board_column(id).map_err(|e| e.to_string())) {
            Ok(n) if n > 0 => st.toast(ToastKind::Info, format!("Column deleted; its {n} task(s) moved to the first column")),
            Ok(_) => {}
            Err(e) => st.toast(ToastKind::Warning, e),
        },
        None => {}
    }

    // Keys, once the board has been clicked: Delete, Insert, and Enter for Task Information.
    if resp.clicked() || resp.secondary_clicked() || resp.drag_started() {
        resp.request_focus();
    }
    if resp.has_focus() && !ui.ctx().egui_wants_keyboard_input() {
        let (del, ins, enter) =
            ui.input(|i| (i.key_pressed(egui::Key::Delete), i.key_pressed(egui::Key::Insert), i.key_pressed(egui::Key::Enter)));
        if del && !st.sel.tasks.is_empty() {
            out.push(Command::DeleteSelection);
        }
        if ins {
            out.push(Command::InsertTask);
        }
        if enter && st.sel.single_task().is_some() {
            out.push(Command::TaskInfo);
        }
    }

    // ---- Scrollbars ----
    let vs = Rect::from_min_max(pos2(rect.right() - BAR_W, rect.top()), pos2(rect.right(), body.bottom()));
    st.board.scroll.y = crate::widgets::grid::scrollbar(ui, ui.id().with("board_vs"), vs, true, sy, content_h, body.height());
    let hs = Rect::from_min_max(pos2(rect.left(), body.bottom()), pos2(body.right(), rect.bottom()));
    st.board.scroll.x = crate::widgets::grid::scrollbar(ui, ui.id().with("board_hs"), hs, false, sx, content_w, body.width());

    column_window(ui.ctx(), doc, st);
}

#[derive(Clone, Copy)]
enum ColumnCmd {
    Edit(BoardColumnId),
    Add,
    Move(BoardColumnId, usize),
    Delete(BoardColumnId),
}

fn report_late(t: &Task, status: chrono::NaiveDateTime, p: &Project) -> bool {
    plancraft_core::report::is_late(t, status, p)
}

/// One card: name, resources, finish date and progress; a coloured edge for critical tasks.
#[allow(clippy::too_many_arguments)]
fn card(ui: &Ui, painter: &egui::Painter, r: Rect, p: &Project, t: &Task, selected: bool, lifted: bool, late: bool) {
    let pal = theme::pal();
    let fill = if selected { pal.SELECTION_TINT } else { pal.BG };
    painter.rect_filled(r, 6.0, fill);
    let edge = if selected { Stroke::new(1.5, pal.ACCENT) } else { Stroke::new(1.0, pal.GRID_LINE_STRONG) };
    painter.rect_stroke(r, 6.0, edge, StrokeKind::Inside);
    if lifted {
        painter.rect_filled(r, 6.0, pal.ACCENT_PALE.gamma_multiply(0.6));
    }
    let stripe = if t.sched.critical {
        pal.BAR_CRITICAL
    } else if t.is_milestone() {
        pal.MILESTONE
    } else {
        pal.ACCENT_MID
    };
    painter.rect_filled(
        Rect::from_min_max(r.min, pos2(r.left() + 4.0, r.bottom())),
        egui::CornerRadius { nw: 6, sw: 6, ne: 0, se: 0 },
        stripe,
    );
    let x = r.left() + 12.0;
    let done = t.is_complete();
    let mark = if done {
        "✔ "
    } else if t.is_milestone() {
        "♦ "
    } else {
        ""
    };
    let name = line(ui, &format!("{mark}{}", t.name), 13.5, if done { pal.TEXT_WEAK } else { pal.TEXT }, r.width() - 20.0);
    painter.galley(pos2(x, r.top() + 8.0), name, pal.TEXT);
    let who = p.format_resource_names(t.id);
    let who = line(ui, if who.is_empty() { "Unassigned" } else { &who }, 11.5, pal.TEXT_WEAK, r.width() - 122.0);
    painter.galley(pos2(x, r.top() + 29.0), who, pal.TEXT_WEAK);
    let due = format!("Due {}", format::date(t.sched.finish));
    painter.text(
        pos2(r.right() - 8.0, r.top() + 36.0),
        Align2::RIGHT_CENTER,
        due,
        FontId::proportional(11.5),
        if late { pal.ERROR } else { pal.TEXT_WEAK },
    );
    // Progress along the bottom.
    let track = Rect::from_min_max(pos2(x, r.bottom() - 9.0), pos2(r.right() - 10.0, r.bottom() - 6.0));
    painter.rect_filled(track, 1.5, pal.TRACK);
    let pct = t.sched.percent_complete.clamp(0.0, 100.0) / 100.0;
    if pct > 0.0 {
        let filled = Rect::from_min_max(track.min, pos2(track.left() + track.width() * pct, track.bottom()));
        painter.rect_filled(filled, 1.5, if done { pal.OK } else { pal.BAR_PROGRESS });
    }
}

/// Rename a column and set its % complete, or name a new one.
fn column_window(ctx: &egui::Context, doc: &mut Document, st: &mut UiState) {
    let Some(edit) = st.board.edit.as_mut() else { return };
    let mut open = true;
    let mut close = false;
    let mut apply = false;
    let title = if edit.id.is_some() { "Board Column" } else { "Add Board Column" };
    egui::Window::new(title)
        .id(egui::Id::new("board_column"))
        .pivot(Align2::CENTER_TOP)
        .default_pos(ctx.content_rect().center_top() + vec2(0.0, 160.0))
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .show(ctx, |ui| {
            egui::Grid::new("bc").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
                ui.label("Name:");
                ui.add(egui::TextEdit::singleline(&mut edit.name).desired_width(180.0));
                ui.end_row();
                ui.label("Set % Complete:");
                ui.add(egui::TextEdit::singleline(&mut edit.percent).desired_width(50.0).hint_text("none"));
                ui.end_row();
            });
            ui.label(
                RichText::new("Tasks moved into this column take this % complete. Leave it empty to keep their progress.")
                    .small()
                    .color(theme::pal().TEXT_WEAK),
            );
            if let Some(e) = &edit.error {
                ui.colored_label(theme::pal().ERROR, e);
            }
            ui.separator();
            ui.horizontal(|ui| {
                if ui.add(egui::Button::new(RichText::new("  OK  ").color(Color32::WHITE)).fill(theme::pal().ACCENT)).clicked() {
                    apply = true;
                }
                if ui.button("Cancel").clicked() {
                    close = true;
                }
            });
        });
    if apply {
        let pct = edit.percent.trim().trim_end_matches('%').trim();
        let percent = if pct.is_empty() {
            Ok(None)
        } else {
            pct.parse::<u8>()
                .ok()
                .filter(|v| *v <= 100)
                .map(Some)
                .ok_or_else(|| "% complete must be a whole number from 0 to 100".to_string())
        };
        let (id, name) = (edit.id, edit.name.clone());
        let r = percent.and_then(|percent| {
            doc.edit_msg(if id.is_some() { "Change Column" } else { "Add Column" }, |p| {
                match id {
                    Some(id) => p.rename_board_column(id, &name).and_then(|()| p.set_board_column_percent(id, percent)),
                    None => p.add_board_column(&name, None, percent).map(|_| ()),
                }
                .map_err(|e| e.to_string())
            })
        });
        match r {
            Ok(()) => close = true,
            Err(e) => edit.error = Some(e),
        }
    }
    if !open || close {
        st.board.edit = None;
    }
}
