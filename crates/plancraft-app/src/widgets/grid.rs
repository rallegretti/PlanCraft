//! A virtualized, spreadsheet-like table with in-place editing.
//!
//! Used by the task sheet next to the Gantt chart, the resource sheet and the
//! usage views.  Rows are painted on demand, so thousands of rows stay fast.
//! Vertical scrolling is exposed through [`GridState::scroll_y`] so a chart
//! next to the grid can stay in sync.

use egui::{Align, Align2, Color32, CursorIcon, EventFilter, FontId, Id, Key, Pos2, Rect, Sense, Stroke, StrokeKind, Ui, pos2, vec2};

use crate::theme;

#[derive(Clone, Debug, PartialEq)]
pub enum Editor {
    /// Read-only (double click raises [`GridEvent::Activate`]).
    None,
    Text,
    Choice(Vec<String>),
}

#[derive(Clone, Debug)]
pub struct ColumnDef {
    pub title: String,
    pub width: f32,
    pub align: Align,
    pub editor: Editor,
}

impl ColumnDef {
    pub fn new(title: impl Into<String>, width: f32) -> Self {
        Self { title: title.into(), width, align: Align::Min, editor: Editor::Text }
    }
    pub fn right(mut self) -> Self {
        self.align = Align::Max;
        self
    }
    pub fn center(mut self) -> Self {
        self.align = Align::Center;
        self
    }
    pub fn read_only(mut self) -> Self {
        self.editor = Editor::None;
        self
    }
    pub fn choice(mut self, options: &[&str]) -> Self {
        self.editor = Editor::Choice(options.iter().map(|s| s.to_string()).collect());
        self
    }
}

#[derive(Clone, Debug, Default)]
pub struct Cell {
    pub text: String,
    pub color: Option<Color32>,
    pub bold: bool,
    pub italic: bool,
    pub strike: bool,
    pub indent: f32,
    /// `Some(expanded)` draws an expand/collapse toggle before the text.
    pub expander: Option<bool>,
    /// Icon glyphs with color and tooltip.
    pub icons: Vec<(String, Color32, String)>,
    pub bg: Option<Color32>,
    pub tooltip: Option<String>,
    /// Value placed in the editor when editing starts (defaults to `text`).
    pub edit_text: Option<String>,
}

impl Cell {
    pub fn text(t: impl Into<String>) -> Self {
        Self { text: t.into(), ..Default::default() }
    }
    pub fn weak(t: impl Into<String>) -> Self {
        Self { text: t.into(), color: Some(theme::TEXT_WEAK), ..Default::default() }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Edit {
    pub row: usize,
    pub col: usize,
    pub text: String,
    started: bool,
}

#[derive(Clone, Debug, Default)]
pub struct GridState {
    pub scroll_y: f32,
    pub scroll_x: f32,
    pub cursor: Option<(usize, usize)>,
    pub editing: Option<Edit>,
    pub widths: Vec<f32>,
    /// Row that should be scrolled into view on the next frame.
    pub scroll_to: Option<usize>,
}

impl GridState {
    pub fn begin_edit(&mut self, row: usize, col: usize, text: String) {
        self.cursor = Some((row, col));
        self.editing = Some(Edit { row, col, text, started: false });
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum GridEvent {
    /// Row (and optionally column) clicked.
    Select {
        row: usize,
        shift: bool,
        ctrl: bool,
    },
    /// Double click on a read-only cell or the row header.
    Activate {
        row: usize,
        col: Option<usize>,
    },
    Commit {
        row: usize,
        col: usize,
        text: String,
    },
    ToggleExpand(usize),
    ContextMenu {
        row: usize,
        pos: Pos2,
    },
    /// Cursor moved by keyboard to `row`.
    CursorRow {
        row: usize,
        shift: bool,
    },
    DeleteKey,
    InsertKey,
    /// Click on empty space below the last row.
    ClickEmpty {
        row: usize,
    },
}

pub struct GridConfig<'a> {
    pub id: Id,
    pub columns: &'a [ColumnDef],
    pub row_count: usize,
    /// Extra empty rows painted (and editable) below the data, like a spreadsheet.
    pub blank_rows: usize,
    pub row_numbers: bool,
    pub show_vscroll: bool,
    pub header_height: f32,
    pub row_height: f32,
}

pub struct GridOutput {
    pub events: Vec<GridEvent>,
    pub body_rect: Rect,
    pub hovered_row: Option<usize>,
    pub content_height: f32,
}

const ROW_NUM_W: f32 = 34.0;
const HSCROLL_H: f32 = 12.0;
const VSCROLL_W: f32 = 12.0;

/// Paints a scrollbar. Returns the new offset.
pub fn scrollbar(ui: &mut Ui, id: Id, track: Rect, vertical: bool, offset: f32, content: f32, viewport: f32) -> f32 {
    let painter = ui.painter_at(track);
    painter.rect_filled(track, 0.0, theme::PANEL_BG);
    if content <= viewport + 0.5 {
        return 0.0;
    }
    let len = if vertical { track.height() } else { track.width() };
    let thumb_len = (len * viewport / content).max(24.0).min(len);
    let max_off = content - viewport;
    let t = (offset / max_off).clamp(0.0, 1.0);
    let start = t * (len - thumb_len);
    let thumb = if vertical {
        Rect::from_min_size(pos2(track.left() + 2.0, track.top() + start), vec2(track.width() - 4.0, thumb_len))
    } else {
        Rect::from_min_size(pos2(track.left() + start, track.top() + 2.0), vec2(thumb_len, track.height() - 4.0))
    };
    let resp = ui.interact(track, id, Sense::click_and_drag());
    let mut new = offset;
    if resp.dragged() {
        let d = if vertical { resp.drag_delta().y } else { resp.drag_delta().x };
        new += d * max_off / (len - thumb_len).max(1.0);
    } else if resp.clicked()
        && let Some(p) = resp.interact_pointer_pos()
    {
        let pos = if vertical { p.y - track.top() } else { p.x - track.left() };
        new = ((pos - thumb_len / 2.0) / (len - thumb_len).max(1.0)) * max_off;
    }
    let color = if resp.hovered() || resp.dragged() { theme::GRID_LINE_STRONG } else { Color32::from_rgb(0xD0, 0xD4, 0xDB) };
    painter.rect_filled(thumb, 4.0, color);
    new.clamp(0.0, max_off)
}

/// Shows the grid in `rect`.
pub fn show(
    ui: &mut Ui,
    rect: Rect,
    state: &mut GridState,
    cfg: &GridConfig,
    is_selected: &dyn Fn(usize) -> bool,
    cell: &mut dyn FnMut(usize, usize) -> Cell,
) -> GridOutput {
    let mut events = Vec::new();
    let ncols = cfg.columns.len();
    if state.widths.len() != ncols {
        state.widths = cfg.columns.iter().map(|c| c.width).collect();
    }
    let rn_w = if cfg.row_numbers { ROW_NUM_W } else { 0.0 };
    let total_cols_w: f32 = state.widths.iter().sum();
    let vs_w = if cfg.show_vscroll { VSCROLL_W } else { 0.0 };
    let header = Rect::from_min_size(rect.min, vec2(rect.width() - vs_w, cfg.header_height));
    let body = Rect::from_min_max(pos2(rect.left(), header.bottom()), pos2(rect.right() - vs_w, rect.bottom() - HSCROLL_H));
    let cols_view_w = (body.width() - rn_w).max(0.0);
    let total_rows = cfg.row_count + cfg.blank_rows;
    let content_h = total_rows as f32 * cfg.row_height;

    // Scrolling.
    let body_resp = ui.interact(body, cfg.id.with("body"), Sense::click_and_drag());
    if ui.rect_contains_pointer(body) {
        let (scroll, shift) = ui.input(|i| (i.smooth_scroll_delta, i.modifiers.shift));
        if shift {
            state.scroll_x -= scroll.y + scroll.x;
        } else {
            state.scroll_y -= scroll.y;
            state.scroll_x -= scroll.x;
        }
    }
    if let Some(r) = state.scroll_to.take() {
        let top = r as f32 * cfg.row_height;
        if top < state.scroll_y {
            state.scroll_y = top;
        } else if top + cfg.row_height > state.scroll_y + body.height() {
            state.scroll_y = top + cfg.row_height - body.height();
        }
    }
    state.scroll_y = state.scroll_y.clamp(0.0, (content_h - body.height() + cfg.row_height).max(0.0));
    state.scroll_x = state.scroll_x.clamp(0.0, (total_cols_w - cols_view_w).max(0.0));

    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, theme::BG);

    // Column x positions.
    let mut col_x = Vec::with_capacity(ncols + 1);
    let mut x = body.left() + rn_w - state.scroll_x;
    for w in &state.widths {
        col_x.push(x);
        x += *w;
    }
    col_x.push(x);

    // ---- Header ----
    painter.rect_filled(header, 0.0, theme::HEADER_BG);
    let hdr_painter = painter.with_clip_rect(Rect::from_min_max(pos2(header.left() + rn_w, header.top()), header.max));
    let font = FontId::proportional(12.5);
    for c in 0..ncols {
        let r = Rect::from_x_y_ranges(col_x[c]..=col_x[c + 1], header.y_range());
        hdr_painter.vline(r.right() - 0.5, header.y_range(), Stroke::new(1.0, theme::GRID_LINE));
        let text_pos = match cfg.columns[c].align {
            Align::Max => pos2(r.right() - 6.0, r.center().y),
            Align::Center => r.center(),
            Align::Min => pos2(r.left() + 6.0, r.center().y),
        };
        let anchor = match cfg.columns[c].align {
            Align::Max => Align2::RIGHT_CENTER,
            Align::Center => Align2::CENTER_CENTER,
            Align::Min => Align2::LEFT_CENTER,
        };
        let hp = hdr_painter.with_clip_rect(r.shrink2(vec2(2.0, 0.0)).intersect(header));
        hp.text(text_pos, anchor, &cfg.columns[c].title, font.clone(), theme::TEXT);
        // Resize handle.
        let handle = Rect::from_center_size(pos2(r.right(), header.center().y), vec2(6.0, header.height()));
        if handle.left() > header.left() + rn_w && handle.right() < header.right() + 3.0 {
            let hr = ui.interact(handle, cfg.id.with(("resize", c)), Sense::drag());
            if hr.hovered() || hr.dragged() {
                ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
            }
            if hr.dragged() {
                state.widths[c] = (state.widths[c] + hr.drag_delta().x).max(24.0);
            }
        }
    }
    if cfg.row_numbers {
        let r = Rect::from_min_size(header.min, vec2(rn_w, header.height()));
        painter.rect_filled(r, 0.0, theme::HEADER_BG);
        painter.vline(r.right() - 0.5, header.y_range(), Stroke::new(1.0, theme::GRID_LINE_STRONG));
    }
    painter.hline(header.x_range(), header.bottom() - 0.5, Stroke::new(1.0, theme::GRID_LINE_STRONG));

    // ---- Body ----
    let body_painter = painter.with_clip_rect(body);
    let first = (state.scroll_y / cfg.row_height).floor().max(0.0) as usize;
    let visible = (body.height() / cfg.row_height).ceil() as usize + 1;
    let last = (first + visible).min(total_rows);
    let pointer = ui.input(|i| i.pointer.hover_pos());
    let mut hovered_row = None;
    let cell_font = FontId::proportional(13.0);
    let mut edit_rect: Option<(Rect, usize, usize)> = None;

    for row in first..last {
        let y = body.top() + row as f32 * cfg.row_height - state.scroll_y;
        let row_rect = Rect::from_min_size(pos2(body.left(), y), vec2(body.width(), cfg.row_height));
        let data_row = row < cfg.row_count;
        if pointer.is_some_and(|p| row_rect.contains(p) && body.contains(p)) {
            hovered_row = Some(row);
        }
        let selected = data_row && is_selected(row);
        if selected {
            body_painter.rect_filled(row_rect, 0.0, theme::SELECTION);
        }
        for c in 0..ncols {
            let r = Rect::from_x_y_ranges(col_x[c]..=col_x[c + 1], row_rect.y_range());
            if r.right() < body.left() + rn_w || r.left() > body.right() {
                continue;
            }
            let clip = r.intersect(Rect::from_min_max(pos2(body.left() + rn_w, body.top()), body.max));
            let cp = body_painter.with_clip_rect(clip);
            let cursor_here = state.cursor == Some((row, c));
            let content = if data_row { cell(row, c) } else { Cell::default() };
            if let Some(bg) = content.bg {
                cp.rect_filled(r, 0.0, bg);
            }
            if cursor_here {
                cp.rect_filled(r, 0.0, theme::SELECTION_CELL);
            }
            cp.vline(r.right() - 0.5, r.y_range(), Stroke::new(1.0, theme::GRID_LINE));
            if state.editing.as_ref().is_some_and(|e| e.row == row && e.col == c) {
                edit_rect = Some((r, row, c));
                continue;
            }
            let mut tx = r.left() + 5.0 + content.indent;
            if let Some(expanded) = content.expander {
                let tri_c = pos2(tx + 5.0, r.center().y);
                let pts = if expanded {
                    vec![tri_c + vec2(-4.0, -2.0), tri_c + vec2(4.0, -2.0), tri_c + vec2(0.0, 3.0)]
                } else {
                    vec![tri_c + vec2(-2.0, -4.0), tri_c + vec2(3.0, 0.0), tri_c + vec2(-2.0, 4.0)]
                };
                cp.add(egui::Shape::convex_polygon(pts, theme::TEXT_WEAK, Stroke::NONE));
                let tr = Rect::from_center_size(tri_c, vec2(14.0, 14.0));
                if ui.interact(tr.intersect(clip), cfg.id.with(("exp", row)), Sense::click()).clicked() {
                    events.push(GridEvent::ToggleExpand(row));
                }
                tx += 14.0;
            }
            for (glyph, color, tip) in &content.icons {
                let ir = Rect::from_min_size(pos2(tx, r.top()), vec2(16.0, r.height()));
                cp.text(ir.center(), Align2::CENTER_CENTER, glyph, FontId::proportional(12.0), *color);
                if !tip.is_empty() && pointer.is_some_and(|p| ir.intersect(clip).contains(p)) {
                    egui::Tooltip::always_open(ui.ctx().clone(), ui.layer_id(), cfg.id.with(("tip", row, c)), egui::PopupAnchor::Pointer)
                        .show(|ui| {
                            ui.label(tip);
                        });
                }
                tx += 16.0;
            }
            if !content.text.is_empty() {
                let color = content.color.unwrap_or(theme::TEXT);
                let galley = cp.layout_no_wrap(content.text.clone(), cell_font.clone(), color);
                let gx = match cfg.columns[c].align {
                    Align::Max => r.right() - 5.0 - galley.size().x,
                    Align::Center => r.center().x - galley.size().x / 2.0,
                    Align::Min => tx,
                };
                let gpos = pos2(gx, r.center().y - galley.size().y / 2.0);
                let width = galley.size().x;
                if content.bold {
                    // Faux bold: the bundled UI font has no bold face.
                    cp.galley(gpos + vec2(0.55, 0.0), galley.clone(), color);
                }
                cp.galley(gpos, galley, color);
                if content.italic || content.strike {
                    cp.hline(gx..=gx + width, r.center().y, Stroke::new(1.0, color));
                }
            }
            if cursor_here {
                cp.rect_stroke(r.shrink(0.5), 0.0, Stroke::new(1.5, theme::ACCENT_MID), StrokeKind::Inside);
            }
            if let Some(t) = &content.tooltip
                && pointer.is_some_and(|p| clip.contains(p))
                && !t.is_empty()
            {
                egui::Tooltip::always_open(ui.ctx().clone(), ui.layer_id(), cfg.id.with(("ctip", row, c)), egui::PopupAnchor::Pointer)
                    .show(|ui| {
                        ui.label(t);
                    });
            }
        }
        body_painter.hline(row_rect.x_range(), row_rect.bottom() - 0.5, Stroke::new(1.0, theme::GRID_LINE));
        if cfg.row_numbers {
            let rr = Rect::from_min_size(row_rect.min, vec2(rn_w, cfg.row_height));
            body_painter.rect_filled(rr, 0.0, if selected { theme::ACCENT_LIGHT } else { theme::HEADER_BG });
            body_painter.vline(rr.right() - 0.5, rr.y_range(), Stroke::new(1.0, theme::GRID_LINE_STRONG));
            body_painter.hline(rr.x_range(), rr.bottom() - 0.5, Stroke::new(1.0, theme::GRID_LINE));
            if data_row {
                body_painter.text(
                    pos2(rr.right() - 6.0, rr.center().y),
                    Align2::RIGHT_CENTER,
                    (row + 1).to_string(),
                    FontId::proportional(11.5),
                    theme::TEXT_WEAK,
                );
            }
        }
    }

    // ---- Mouse ----
    let (shift, ctrl) = ui.input(|i| (i.modifiers.shift, i.modifiers.command));
    let hit = |p: Pos2| -> Option<(usize, Option<usize>)> {
        if !body.contains(p) {
            return None;
        }
        let row = ((p.y - body.top() + state.scroll_y) / cfg.row_height).floor() as usize;
        let col = (0..ncols).find(|&c| p.x >= col_x[c] && p.x < col_x[c + 1] && p.x >= body.left() + rn_w);
        Some((row, col))
    };
    if body_resp.clicked() || body_resp.double_clicked() || body_resp.secondary_clicked() {
        body_resp.request_focus();
    }
    if let Some(p) = body_resp.interact_pointer_pos()
        && let Some((row, col)) = hit(p)
    {
        if body_resp.clicked() {
            if row >= cfg.row_count {
                state.cursor = col.map(|c| (row, c));
                events.push(GridEvent::ClickEmpty { row });
            } else {
                if let Some(c) = col {
                    state.cursor = Some((row, c));
                }
                // Clicking another cell commits the current edit (handled below via focus loss).
                events.push(GridEvent::Select { row, shift, ctrl });
            }
        }
        if body_resp.double_clicked() {
            match col {
                Some(c) if cfg.columns[c].editor != Editor::None && row < total_rows => {
                    let text = if row < cfg.row_count {
                        let ce = cell(row, c);
                        ce.edit_text.unwrap_or(ce.text)
                    } else {
                        String::new()
                    };
                    state.begin_edit(row, c, text);
                }
                _ if row < cfg.row_count => events.push(GridEvent::Activate { row, col }),
                _ => {}
            }
        }
        if body_resp.secondary_clicked() && row < cfg.row_count {
            if !is_selected(row) {
                events.push(GridEvent::Select { row, shift: false, ctrl: false });
            }
            if let Some(c) = col {
                state.cursor = Some((row, c));
            }
            events.push(GridEvent::ContextMenu { row, pos: p });
        }
    }

    // ---- Editing ----
    let mut finished: Option<(bool, Option<(isize, isize)>)> = None; // (commit, move)
    if let Some((r, row, c)) = edit_rect {
        let editor = cfg.columns[c].editor.clone();
        let clip = Rect::from_min_max(pos2(body.left() + rn_w, body.top()), body.max);
        let edit = state.editing.as_mut().expect("editing");
        match editor {
            Editor::Choice(options) => {
                body_painter.rect_filled(r, 0.0, theme::BG);
                body_painter.rect_stroke(r.shrink(0.5), 0.0, Stroke::new(1.5, theme::ACCENT), StrokeKind::Inside);
                body_painter.text(pos2(r.left() + 5.0, r.center().y), Align2::LEFT_CENTER, &edit.text, cell_font.clone(), theme::TEXT);
                let area = egui::Area::new(cfg.id.with("choice"))
                    .order(egui::Order::Foreground)
                    .fixed_pos(pos2(r.left(), r.bottom()))
                    .show(ui.ctx(), |ui| {
                        egui::Frame::popup(ui.style()).show(ui, |ui| {
                            ui.set_min_width(r.width().max(120.0));
                            let mut picked = None;
                            for o in &options {
                                if ui.selectable_label(*o == edit.text, o).clicked() {
                                    picked = Some(o.clone());
                                }
                            }
                            picked
                        })
                    });
                if let Some(p) = area.inner.inner {
                    edit.text = p;
                    finished = Some((true, None));
                } else if ui.input(|i| i.key_pressed(Key::Escape)) {
                    finished = Some((false, None));
                } else if edit.started
                    && ui.input(|i| i.pointer.any_pressed())
                    && !area.response.contains_pointer()
                    && !ui.rect_contains_pointer(r)
                {
                    finished = Some((false, None));
                }
                edit.started = true;
            }
            _ => {
                let id = cfg.id.with(("edit", row, c));
                let mut child = ui.new_child(egui::UiBuilder::new().max_rect(r).layout(egui::Layout::left_to_right(Align::Center)));
                child.set_clip_rect(clip);
                body_painter.rect_filled(r, 0.0, theme::BG);
                let te = egui::TextEdit::singleline(&mut edit.text)
                    .id(id)
                    .frame(egui::Frame::NONE)
                    .margin(egui::Margin::symmetric(4, 2))
                    .font(cell_font.clone())
                    .desired_width(r.width() - 4.0)
                    .horizontal_align(cfg.columns[c].align);
                let resp = child.add(te);
                body_painter.rect_stroke(r.shrink(0.5), 0.0, Stroke::new(1.5, theme::ACCENT), StrokeKind::Inside);
                if !edit.started {
                    resp.request_focus();
                    edit.started = true;
                } else {
                    let (enter, esc, tab, up, down) = ui.input(|i| {
                        (
                            i.key_pressed(Key::Enter),
                            i.key_pressed(Key::Escape),
                            i.key_pressed(Key::Tab),
                            i.key_pressed(Key::ArrowUp),
                            i.key_pressed(Key::ArrowDown),
                        )
                    });
                    if esc {
                        finished = Some((false, None));
                    } else if enter {
                        finished = Some((true, Some((1, 0))));
                    } else if tab {
                        finished = Some((true, Some((0, if shift { -1 } else { 1 }))));
                    } else if up && resp.has_focus() {
                        finished = Some((true, Some((-1, 0))));
                    } else if down && resp.has_focus() {
                        finished = Some((true, Some((1, 0))));
                    } else if resp.lost_focus() {
                        finished = Some((true, None));
                    }
                }
            }
        }
    } else if let Some(e) = &state.editing
        && (e.row < first || e.row >= last)
    {
        // Edited row scrolled away: commit.
        finished = Some((true, None));
    }
    if let Some((commit, mv)) = finished {
        if let Some(e) = state.editing.take() {
            if commit {
                events.push(GridEvent::Commit { row: e.row, col: e.col, text: e.text.trim().to_string() });
            }
            if let Some((dr, dc)) = mv {
                let nr = (e.row as isize + dr).clamp(0, total_rows.saturating_sub(1) as isize) as usize;
                let nc = (e.col as isize + dc).clamp(0, ncols as isize - 1) as usize;
                state.cursor = Some((nr, nc));
                state.scroll_to = Some(nr);
                if dr != 0 && nr < cfg.row_count {
                    events.push(GridEvent::CursorRow { row: nr, shift: false });
                }
            }
            body_resp.request_focus();
        }
    }

    // ---- Keyboard (grid focused, not editing) ----
    if body_resp.has_focus() && state.editing.is_none() {
        ui.memory_mut(|m| {
            m.set_focus_lock_filter(body_resp.id, EventFilter { tab: true, horizontal_arrows: true, vertical_arrows: true, escape: false })
        });
        let (mut dr, mut dc) = (0isize, 0isize);
        let mut typed: Option<String> = None;
        let mut start_edit = false;
        let mut del = false;
        let mut ins = false;
        ui.input(|i| {
            for ev in &i.events {
                match ev {
                    egui::Event::Key { key, pressed: true, modifiers, .. } if !modifiers.alt && !modifiers.command => match key {
                        Key::ArrowUp => dr -= 1,
                        Key::ArrowDown | Key::Enter => dr += 1,
                        Key::ArrowLeft => dc -= 1,
                        Key::ArrowRight | Key::Tab => dc += 1,
                        Key::F2 => start_edit = true,
                        Key::Delete => del = true,
                        Key::Insert => ins = true,
                        Key::PageDown => dr += (body.height() / cfg.row_height) as isize,
                        Key::PageUp => dr -= (body.height() / cfg.row_height) as isize,
                        _ => {}
                    },
                    egui::Event::Text(t) if !i.modifiers.command && !i.modifiers.alt => typed = Some(typed.take().unwrap_or_default() + t),
                    _ => {}
                }
            }
        });
        let (row, col) = state.cursor.unwrap_or((0, 0));
        if dr != 0 || dc != 0 {
            let nr = (row as isize + dr).clamp(0, total_rows.saturating_sub(1) as isize) as usize;
            let nc = (col as isize + dc).clamp(0, ncols as isize - 1) as usize;
            state.cursor = Some((nr, nc));
            state.scroll_to = Some(nr);
            if nr != row && nr < cfg.row_count {
                events.push(GridEvent::CursorRow { row: nr, shift });
            }
        }
        if del {
            events.push(GridEvent::DeleteKey);
        }
        if ins {
            events.push(GridEvent::InsertKey);
        }
        if let Some((r, c)) = state.cursor
            && r < total_rows
            && cfg.columns[c].editor != Editor::None
        {
            if let Some(t) = typed.filter(|t| !t.trim().is_empty()) {
                if matches!(cfg.columns[c].editor, Editor::Text) {
                    state.begin_edit(r, c, t);
                    if let Some(e) = state.editing.as_mut() {
                        // Place cursor at end of the typed text.
                        let id = cfg.id.with(("edit", r, c));
                        let len = e.text.chars().count();
                        let mut st = egui::text_edit::TextEditState::load(ui.ctx(), id).unwrap_or_default();
                        st.cursor.set_char_range(Some(egui::text::CCursorRange::one(egui::text::CCursor::new(len))));
                        st.store(ui.ctx(), id);
                    }
                }
            } else if start_edit {
                let text = if r < cfg.row_count {
                    let ce = cell(r, c);
                    ce.edit_text.unwrap_or(ce.text)
                } else {
                    String::new()
                };
                state.begin_edit(r, c, text);
            }
        }
    }

    // ---- Scrollbars ----
    let hs = Rect::from_min_max(pos2(body.left() + rn_w, body.bottom()), pos2(body.right(), rect.bottom()));
    state.scroll_x = scrollbar(ui, cfg.id.with("hscroll"), hs, false, state.scroll_x, total_cols_w, cols_view_w);
    if cfg.row_numbers {
        painter.rect_filled(
            Rect::from_min_max(pos2(body.left(), body.bottom()), pos2(body.left() + rn_w, rect.bottom())),
            0.0,
            theme::PANEL_BG,
        );
    }
    if cfg.show_vscroll {
        let vs = Rect::from_min_max(pos2(rect.right() - VSCROLL_W, body.top()), pos2(rect.right(), body.bottom()));
        state.scroll_y = scrollbar(ui, cfg.id.with("vscroll"), vs, true, state.scroll_y, content_h + cfg.row_height, body.height());
        painter.rect_filled(
            Rect::from_min_max(pos2(rect.right() - VSCROLL_W, rect.top()), pos2(rect.right(), header.bottom())),
            0.0,
            theme::HEADER_BG,
        );
    }

    GridOutput { events, body_rect: body, hovered_row, content_height: content_h }
}
