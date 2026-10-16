//! The Gantt chart canvas: timescale, bars, links, and bar dragging.

use std::collections::HashMap;

use chrono::{Duration, NaiveDateTime};
use egui::{Align2, Color32, CursorIcon, FontId, Painter, Pos2, Rect, Sense, Shape, Stroke, StrokeKind, Ui, pos2, vec2};
use plancraft_core::prelude::*;
use plancraft_core::schedule::{project_calendar, task_calendar};

use crate::commands::Command;
use crate::format;
use crate::state::{BarDrag, GanttState};
use crate::theme;
use crate::widgets::grid::scrollbar;
use crate::widgets::timescale;

#[derive(Clone, Debug)]
pub enum ChartAction {
    Select { task: TaskId, shift: bool, ctrl: bool },
    Open(TaskId),
    MoveTo { task: TaskId, start: NaiveDateTime },
    SetFinish { task: TaskId, finish: NaiveDateTime },
    SetProgress { task: TaskId, pct: u8 },
    Link { from: TaskId, to: TaskId },
    Command(Command),
}

pub struct ChartInput<'a> {
    pub project: &'a Project,
    pub rows: &'a [usize],
    pub selected: &'a dyn Fn(TaskId) -> bool,
    pub tracking: bool,
    pub row_height: f32,
    pub header_height: f32,
    pub blank_rows: usize,
}

#[derive(Clone, Copy)]
struct BarGeom {
    y: f32,
    x0: f32,
    x1: f32,
}

const BAR_H: f32 = 10.0;

fn diamond(painter: &Painter, c: Pos2, r: f32, fill: Color32, stroke: Stroke) {
    painter.add(Shape::convex_polygon(vec![c + vec2(0.0, -r), c + vec2(r, 0.0), c + vec2(0.0, r), c + vec2(-r, 0.0)], fill, stroke));
}

fn arrow_head(painter: &Painter, tip: Pos2, dir: egui::Vec2, color: Color32) {
    let d = dir.normalized();
    let n = vec2(-d.y, d.x);
    let base = tip - d * 5.0;
    painter.add(Shape::convex_polygon(vec![tip, base + n * 3.0, base - n * 3.0], color, Stroke::NONE));
}

fn polyline(painter: &Painter, pts: &[Pos2], stroke: Stroke) {
    painter.add(Shape::line(pts.to_vec(), stroke));
}

/// Shows the chart. `scroll_y` is shared with the task table.
pub fn show(ui: &mut Ui, rect: Rect, g: &mut GanttState, scroll_y: &mut f32, input: &ChartInput) -> Vec<ChartAction> {
    let mut actions = Vec::new();
    let p = input.project;
    let rh = input.row_height;
    let header = Rect::from_min_size(rect.min, vec2(rect.width() - 12.0, input.header_height));
    let body = Rect::from_min_max(pos2(rect.left(), header.bottom()), pos2(rect.right() - 12.0, rect.bottom() - 12.0));
    let chart_left = body.left();

    // Initial positioning per project.
    if g.initialized_for != Some(p.id) {
        g.initialized_for = Some(p.id);
        g.scroll_to(p.sched.start.unwrap_or(p.start));
    }

    let resp = ui.interact(body, ui.id().with("gantt_body"), Sense::click_and_drag());

    // Wheel: vertical scroll, Ctrl = zoom, Shift = horizontal.
    if ui.rect_contains_pointer(body) {
        let (scroll, zoom_mod, shift, pointer) =
            ui.input(|i| (i.smooth_scroll_delta, i.modifiers.command, i.modifiers.shift, i.pointer.hover_pos()));
        if zoom_mod {
            let z = ui.input(|i| i.zoom_delta());
            if (z - 1.0).abs() > 1e-4 {
                g.zoom_by(z, pointer.map(|p| p.x).unwrap_or(body.center().x), chart_left);
            }
        } else if shift {
            let dx = scroll.y + scroll.x;
            g.left -= Duration::minutes((dx / g.px_per_day * 1440.0) as i64);
        } else {
            *scroll_y -= scroll.y;
            if scroll.x.abs() > 0.0 {
                g.left -= Duration::minutes((scroll.x / g.px_per_day * 1440.0) as i64);
            }
        }
    }
    let content_h = (input.rows.len() + input.blank_rows) as f32 * rh;
    *scroll_y = scroll_y.clamp(0.0, (content_h - body.height() + rh).max(0.0));

    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, theme::pal().BG);

    // ---- Timescale & background ----
    let ticks = timescale::paint_header(&painter.with_clip_rect(header), header, g);
    let bp = painter.with_clip_rect(body);
    let proj_cal = project_calendar(p);
    if g.px_per_day >= 4.0 {
        // Non-working day shading.
        let mut d = g.left.date();
        let end = g.time_at(chart_left, body.right()).date();
        while d <= end {
            if !proj_cal.is_working_day(d) {
                let t0 = d.and_hms_opt(0, 0, 0).unwrap();
                let x0 = g.x_of(chart_left, t0);
                let x1 = g.x_of(chart_left, t0 + Duration::days(1));
                bp.rect_filled(Rect::from_x_y_ranges(x0..=x1, body.y_range()), 0.0, theme::pal().NONWORKING);
            }
            d += Duration::days(1);
        }
    }
    let (_, bottom_unit) = timescale::tiers(g.px_per_day);
    for (x, _) in &ticks {
        if bottom_unit != timescale::Unit::Day || g.px_per_day >= 14.0 {
            bp.vline(x.round() + 0.5, body.y_range(), Stroke::new(1.0, theme::pal().CHART_GRID));
        }
    }
    // Row separators (light) for readability.
    let first = (*scroll_y / rh).floor().max(0.0) as usize;
    let last = ((*scroll_y + body.height()) / rh).ceil() as usize + 1;
    for r in first..last.min(input.rows.len()) {
        let y = body.top() + r as f32 * rh - *scroll_y;
        if (input.selected)(p.tasks[input.rows[r]].id) {
            bp.rect_filled(Rect::from_min_size(pos2(body.left(), y), vec2(body.width(), rh)), 0.0, theme::pal().SELECTION_TINT);
        }
    }

    // Today & status date & project start.
    let now = chrono::Local::now().naive_local();
    let xt = g.x_of(chart_left, now);
    if body.x_range().contains(xt) {
        dashed_vline(&bp, xt, body.top(), body.bottom(), theme::pal().TODAY);
    }
    if let Some(sd) = p.status_date {
        let xs = g.x_of(chart_left, proj_cal.day_finish(sd));
        if body.x_range().contains(xs) {
            dashed_vline(&bp, xs, body.top(), body.bottom(), theme::pal().STATUS_DATE);
        }
    }
    let xps = g.x_of(chart_left, p.start);
    if body.x_range().contains(xps) {
        bp.vline(xps, body.y_range(), Stroke::new(1.0, theme::pal().PROJECT_START));
    }

    // ---- Geometry for all rows (links need off-screen ones too) ----
    let mut geom: HashMap<TaskId, BarGeom> = HashMap::with_capacity(input.rows.len());
    for (r, &idx) in input.rows.iter().enumerate() {
        let t = &p.tasks[idx];
        let (Some(s), Some(f)) = (t.sched.start, t.sched.finish) else {
            continue;
        };
        let y = body.top() + r as f32 * rh - *scroll_y + rh / 2.0;
        geom.insert(t.id, BarGeom { y, x0: g.x_of(chart_left, s), x1: g.x_of(chart_left, f) });
    }

    // ---- Links ----
    if g.show_links {
        let link_stroke = Stroke::new(1.0, theme::pal().LINK);
        for &idx in input.rows {
            let t = &p.tasks[idx];
            let Some(sg) = geom.get(&t.id).copied() else {
                continue;
            };
            for d in &t.predecessors {
                let Some(pid) = d.predecessor.local() else {
                    continue;
                };
                let Some(pg) = geom.get(&pid).copied() else {
                    continue;
                };
                let top_vis = body.top() - rh;
                let bot_vis = body.bottom() + rh;
                if (pg.y < top_vis && sg.y < top_vis) || (pg.y > bot_vis && sg.y > bot_vis) {
                    continue;
                }
                let critical = g.show_critical && t.sched.critical && p.task(pid).is_some_and(|pt| pt.sched.critical);
                let stroke = if critical { Stroke::new(1.2, theme::pal().BAR_CRITICAL_EDGE) } else { link_stroke };
                let color = stroke.color;
                let down = sg.y > pg.y;
                let half = BAR_H / 2.0 + 1.0;
                let succ_ms = t.is_milestone();
                match d.kind {
                    LinkType::FS => {
                        let sx = if succ_ms { sg.x0 } else { sg.x0 + 3.0 };
                        if sx >= pg.x1 + 6.0 {
                            let ty = if down { sg.y - half } else { sg.y + half };
                            polyline(&bp, &[pos2(pg.x1, pg.y), pos2(sx, pg.y), pos2(sx, ty)], stroke);
                            arrow_head(&bp, pos2(sx, ty), vec2(0.0, if down { 1.0 } else { -1.0 }), color);
                        } else {
                            let mid_y = if down { pg.y + rh / 2.0 } else { pg.y - rh / 2.0 };
                            let xa = pg.x1 + 6.0;
                            let xb = sg.x0 - 8.0;
                            polyline(
                                &bp,
                                &[
                                    pos2(pg.x1, pg.y),
                                    pos2(xa, pg.y),
                                    pos2(xa, mid_y),
                                    pos2(xb, mid_y),
                                    pos2(xb, sg.y),
                                    pos2(sg.x0 - 1.0, sg.y),
                                ],
                                stroke,
                            );
                            arrow_head(&bp, pos2(sg.x0 - 1.0, sg.y), vec2(1.0, 0.0), color);
                        }
                    }
                    LinkType::SS => {
                        let xa = pg.x0.min(sg.x0) - 8.0;
                        polyline(&bp, &[pos2(pg.x0, pg.y), pos2(xa, pg.y), pos2(xa, sg.y), pos2(sg.x0 - 1.0, sg.y)], stroke);
                        arrow_head(&bp, pos2(sg.x0 - 1.0, sg.y), vec2(1.0, 0.0), color);
                    }
                    LinkType::FF => {
                        let xa = pg.x1.max(sg.x1) + 8.0;
                        polyline(&bp, &[pos2(pg.x1, pg.y), pos2(xa, pg.y), pos2(xa, sg.y), pos2(sg.x1 + 1.0, sg.y)], stroke);
                        arrow_head(&bp, pos2(sg.x1 + 1.0, sg.y), vec2(-1.0, 0.0), color);
                    }
                    LinkType::SF => {
                        let xa = pg.x0 - 8.0;
                        let xb = sg.x1 + 8.0;
                        let mid_y = if down { pg.y + rh / 2.0 } else { pg.y - rh / 2.0 };
                        polyline(
                            &bp,
                            &[pos2(pg.x0, pg.y), pos2(xa, pg.y), pos2(xa, mid_y), pos2(xb, mid_y), pos2(xb, sg.y), pos2(sg.x1 + 1.0, sg.y)],
                            stroke,
                        );
                        arrow_head(&bp, pos2(sg.x1 + 1.0, sg.y), vec2(-1.0, 0.0), color);
                    }
                }
            }
        }
    }

    // Milestone membership: dashed connectors from member tasks to their milestone.
    if g.show_links {
        let stroke = Stroke::new(1.0, theme::pal().MEMBER_LINK);
        for &idx in input.rows {
            let t = &p.tasks[idx];
            if !t.is_milestone() || t.members.is_empty() {
                continue;
            }
            let Some(mg) = geom.get(&t.id).copied() else { continue };
            for m in &t.members {
                let Some(gm) = m.local().and_then(|id| geom.get(&id)).copied() else { continue };
                if (gm.y < body.top() && mg.y < body.top()) || (gm.y > body.bottom() && mg.y > body.bottom()) {
                    continue;
                }
                let xm = mg.x0.max(gm.x1 + 4.0);
                let pts = [pos2(gm.x1, gm.y), pos2(xm, gm.y), pos2(xm, mg.y + if mg.y > gm.y { -7.0 } else { 7.0 })];
                bp.extend(Shape::dashed_line(&pts, stroke, 3.0, 3.0));
            }
        }
    }

    // ---- Bars ----
    let label_font = FontId::proportional(11.5);
    let mut hovered: Option<(TaskId, &'static str)> = None;
    let pointer = ui.input(|i| i.pointer.hover_pos());
    for r in first..last.min(input.rows.len()) {
        let idx = input.rows[r];
        let t = &p.tasks[idx];
        let Some(bg) = geom.get(&t.id).copied() else {
            continue;
        };
        let y = bg.y;
        let (x0, x1) = (bg.x0, bg.x1.max(bg.x0));
        let critical = (g.show_critical || input.tracking) && t.sched.critical;
        let summary = t.sched.is_summary;
        let bar_rect = Rect::from_min_max(pos2(x0, y - BAR_H / 2.0), pos2(x1.max(x0 + 1.0), y + BAR_H / 2.0));

        // Baseline (below the bar).
        if (g.show_baseline || input.tracking)
            && let Some(b) = t.baseline
        {
            let bx0 = g.x_of(chart_left, b.start);
            let bx1 = g.x_of(chart_left, b.finish);
            if t.is_milestone() {
                diamond(&bp, pos2(bx0, y + 6.0), 4.0, theme::pal().BAR_BASELINE, Stroke::NONE);
            } else {
                bp.rect_filled(Rect::from_min_max(pos2(bx0, y + 3.0), pos2(bx1.max(bx0 + 1.0), y + 8.0)), 1.0, theme::pal().BAR_BASELINE);
            }
        }
        let y_shift = if (g.show_baseline || input.tracking) && t.baseline.is_some() { -3.0 } else { 0.0 };
        let bar_rect = bar_rect.translate(vec2(0.0, y_shift));
        let yc = y + y_shift;

        if t.is_milestone() {
            let fill = if critical {
                theme::pal().BAR_CRITICAL
            } else if t.is_complete() {
                theme::pal().ACCENT_DARK
            } else {
                theme::pal().MILESTONE
            };
            diamond(&bp, pos2(x0, yc), 6.5, fill, Stroke::new(1.0, theme::pal().BG));
            let label = t.sched.finish.map(|f| f.format("%-m/%-d").to_string()).unwrap_or_default().to_string();
            bp.text(pos2(x0 + 10.0, yc), Align2::LEFT_CENTER, label, label_font.clone(), theme::pal().TEXT);
            if t.deadline.is_some_and(|dl| t.sched.finish.is_some_and(|f| f > dl)) {
                bp.text(pos2(x0 - 10.0, yc), Align2::RIGHT_CENTER, "⚠", label_font.clone(), theme::pal().ERROR);
            }
        } else if summary {
            let fill = if critical { theme::pal().BAR_CRITICAL_EDGE } else { theme::pal().BAR_SUMMARY };
            let r = Rect::from_min_max(pos2(x0, yc - 4.0), pos2(x1, yc + 1.0));
            bp.rect_filled(r, 0.0, fill);
            for x in [x0, x1] {
                bp.add(Shape::convex_polygon(
                    vec![pos2(x - 4.0, yc + 1.0), pos2(x + 4.0, yc + 1.0), pos2(x, yc + 6.0)],
                    fill,
                    Stroke::NONE,
                ));
            }
            if g.show_progress && t.sched.percent_complete > 0.0 {
                let px = x0 + (x1 - x0) * t.sched.percent_complete / 100.0;
                bp.rect_filled(Rect::from_min_max(pos2(x0, yc - 2.5), pos2(px, yc - 0.5)), 0.0, theme::pal().GOLD);
            }
        } else {
            let manual = t.mode == ScheduleMode::Manual;
            let (fill, edge) = if !t.active {
                (theme::pal().BAR_INACTIVE, theme::pal().BAR_BASELINE)
            } else if critical {
                (theme::pal().BAR_CRITICAL, theme::pal().BAR_CRITICAL_EDGE)
            } else if manual {
                (theme::pal().BAR_MANUAL, theme::pal().BAR_TASK_EDGE)
            } else if let Some([r, gg, b]) = t.bar_color {
                (Color32::from_rgb(r, gg, b), Color32::from_rgb(r / 2, gg / 2, b / 2))
            } else {
                (theme::pal().BAR_TASK, theme::pal().BAR_TASK_EDGE)
            };
            bp.rect_filled(bar_rect, 2.0, fill);
            bp.rect_stroke(bar_rect, 2.0, Stroke::new(1.0, edge), StrokeKind::Inside);
            if manual {
                // Bracket ends mark manually scheduled tasks.
                for (x, dir) in [(bar_rect.left(), 1.0), (bar_rect.right(), -1.0)] {
                    bp.line_segment([pos2(x, bar_rect.top() - 2.0), pos2(x, bar_rect.bottom() + 2.0)], Stroke::new(2.0, edge));
                    bp.line_segment([pos2(x, bar_rect.top() - 2.0), pos2(x + 3.0 * dir, bar_rect.top() - 2.0)], Stroke::new(2.0, edge));
                }
            }
            if g.show_progress && t.sched.percent_complete > 0.0 {
                let px = x0 + (x1 - x0) * t.sched.percent_complete / 100.0;
                bp.rect_filled(Rect::from_min_max(pos2(x0, yc - 1.5), pos2(px, yc + 1.5)), 0.0, theme::pal().BAR_PROGRESS);
            }
            if g.show_slack && t.sched.total_slack > 0 {
                let cal = task_calendar(p, idx);
                if let Some(f) = t.sched.finish {
                    let lf = cal.add(f, t.sched.total_slack);
                    let sx = g.x_of(chart_left, lf);
                    bp.hline(x1..=sx, yc + BAR_H / 2.0 + 2.0, Stroke::new(1.5, theme::pal().BAR_SLACK));
                }
            }
            if t.deadline.is_some_and(|dl| t.sched.finish.is_some_and(|f| f > dl)) {
                bp.text(pos2(x1 + 3.0, yc - 8.0), Align2::LEFT_CENTER, "⚠", FontId::proportional(10.0), theme::pal().ERROR);
            }
        }
        // Deadline marker.
        if let Some(dl) = t.deadline {
            let xd = g.x_of(chart_left, dl);
            bp.add(Shape::convex_polygon(
                vec![pos2(xd, yc - 6.0), pos2(xd + 5.0, yc - 6.0), pos2(xd, yc - 1.0)],
                theme::pal().DEADLINE,
                Stroke::NONE,
            ));
            bp.vline(xd, (yc - 6.0)..=(yc + 6.0), Stroke::new(1.0, theme::pal().DEADLINE));
        }
        // Resource names to the right.
        if g.show_resource_names && !summary && !t.is_milestone() {
            let names = p.format_resource_names(t.id);
            if !names.is_empty() {
                bp.text(pos2(x1 + 6.0, yc), Align2::LEFT_CENTER, names, label_font.clone(), theme::pal().TEXT);
            }
        }

        // Hit testing.
        if let Some(pp) = pointer
            && body.contains(pp)
        {
            let hit =
                if t.is_milestone() { Rect::from_center_size(pos2(x0, yc), vec2(14.0, 14.0)) } else { bar_rect.expand2(vec2(2.0, 3.0)) };
            if hit.contains(pp) {
                let zone = if summary || t.is_milestone() {
                    "bar"
                } else if pp.x >= hit.right() - 5.0 {
                    "end"
                } else if pp.x <= hit.left() + 4.0 && x1 - x0 > 14.0 {
                    "progress"
                } else {
                    "bar"
                };
                hovered = Some((t.id, zone));
            }
        }
    }

    // Project summary bar across the top.
    if g.show_project_summary
        && let (Some(s), Some(f)) = (p.sched.start, p.sched.finish)
    {
        let x0 = g.x_of(chart_left, s);
        let x1 = g.x_of(chart_left, f);
        bp.rect_filled(Rect::from_min_max(pos2(x0, body.top() + 2.0), pos2(x1, body.top() + 5.0)), 0.0, theme::pal().ACCENT_DARK);
    }

    // ---- Interaction ----
    match hovered {
        Some((_, "end")) => ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal),
        Some((_, "progress")) => ui.ctx().set_cursor_icon(CursorIcon::ResizeEast),
        Some(_) => ui.ctx().set_cursor_icon(CursorIcon::Grab),
        None => {}
    }
    if resp.drag_started()
        && let Some((tid, zone)) = hovered
    {
        let t = p.task(tid).unwrap();
        let grab_x = resp.interact_pointer_pos().map(|pp| pp.x).unwrap_or(0.0);
        let start = t.sched.start.unwrap_or(p.start);
        g.drag = Some(match zone {
            "end" => BarDrag::Resize { task: tid },
            "progress" => BarDrag::Progress { task: tid },
            _ if t.sched.is_summary => BarDrag::Link { from: tid },
            _ => BarDrag::Move { task: tid, grab_dx: grab_x - g.x_of(chart_left, start), origin_start: start },
        });
    }
    let snap = |t: NaiveDateTime| -> NaiveDateTime {
        if g.px_per_day >= 72.0 {
            let m = (t - t.date().and_hms_opt(0, 0, 0).unwrap()).num_minutes();
            t.date().and_hms_opt(0, 0, 0).unwrap() + Duration::minutes((m + 30) / 60 * 60)
        } else {
            (t + Duration::hours(12)).date().and_hms_opt(0, 0, 0).unwrap()
        }
    };
    if let (Some(drag), Some(pp)) = (g.drag.clone(), ui.input(|i| i.pointer.interact_pos())) {
        let row_at = |y: f32| -> Option<usize> {
            let r = ((y - body.top() + *scroll_y) / rh).floor();
            (r >= 0.0 && (r as usize) < input.rows.len()).then_some(r as usize)
        };
        match drag {
            BarDrag::Move { task, grab_dx, origin_start } => {
                let src = geom.get(&task).copied();
                // Dragging vertically switches to linking, like Microsoft Project.
                if let Some(sg) = src
                    && (pp.y - sg.y).abs() > rh * 0.9
                {
                    g.drag = Some(BarDrag::Link { from: task });
                } else {
                    let t = p.task(task).unwrap();
                    let new_start = snap(g.time_at(chart_left, pp.x - grab_dx));
                    let dur_px = geom.get(&task).map(|bgm| bgm.x1 - bgm.x0).unwrap_or(0.0);
                    let x = g.x_of(chart_left, new_start);
                    if let Some(sg) = src {
                        let ghost =
                            Rect::from_min_max(pos2(x, sg.y - BAR_H / 2.0 - 1.0), pos2(x + dur_px.max(2.0), sg.y + BAR_H / 2.0 + 1.0));
                        bp.rect_stroke(ghost, 2.0, Stroke::new(1.5, theme::pal().ACCENT), StrokeKind::Outside);
                    }
                    tooltip_at(ui, pp, &format!("{}\nStart: {}", t.name, format::date_long(Some(new_start))));
                    if resp.drag_stopped() {
                        g.drag = None;
                        if new_start != origin_start {
                            actions.push(ChartAction::MoveTo { task, start: new_start });
                        }
                    }
                }
            }
            BarDrag::Resize { task } => {
                let t = p.task(task).unwrap();
                let new_finish = snap(g.time_at(chart_left, pp.x));
                if let Some(sg) = geom.get(&task) {
                    let x = g.x_of(chart_left, new_finish).max(sg.x0 + 2.0);
                    bp.rect_stroke(
                        Rect::from_min_max(pos2(sg.x0, sg.y - BAR_H / 2.0 - 1.0), pos2(x, sg.y + BAR_H / 2.0 + 1.0)),
                        2.0,
                        Stroke::new(1.5, theme::pal().ACCENT),
                        StrokeKind::Outside,
                    );
                }
                tooltip_at(ui, pp, &format!("{}\nFinish: {}", t.name, format::date_long(Some(new_finish))));
                if resp.drag_stopped() {
                    g.drag = None;
                    actions.push(ChartAction::SetFinish { task, finish: new_finish });
                }
            }
            BarDrag::Progress { task } => {
                if let Some(sg) = geom.get(&task) {
                    let pct = (((pp.x - sg.x0) / (sg.x1 - sg.x0).max(1.0)) * 100.0).clamp(0.0, 100.0).round() as u8;
                    let px = sg.x0 + (sg.x1 - sg.x0) * pct as f32 / 100.0;
                    bp.rect_filled(Rect::from_min_max(pos2(sg.x0, sg.y - 2.0), pos2(px, sg.y + 2.0)), 0.0, theme::pal().GOLD);
                    tooltip_at(ui, pp, &format!("{pct}% complete"));
                    if resp.drag_stopped() {
                        g.drag = None;
                        actions.push(ChartAction::SetProgress { task, pct });
                    }
                }
            }
            BarDrag::Link { from } => {
                if let Some(sg) = geom.get(&from) {
                    bp.line_segment([pos2(sg.x1, sg.y), pp], Stroke::new(1.5, theme::pal().ACCENT));
                    let target = row_at(pp.y).map(|r| p.tasks[input.rows[r]].id).filter(|id| *id != from);
                    let msg = match target {
                        Some(to) => format!(
                            "Link: {} – {}",
                            p.task(from).map(|t| t.name.as_str()).unwrap_or(""),
                            p.task(to).map(|t| t.name.as_str()).unwrap_or("")
                        ),
                        None => "Drop on another task to link".into(),
                    };
                    tooltip_at(ui, pp, &msg);
                    if resp.drag_stopped() {
                        g.drag = None;
                        if let Some(to) = target {
                            actions.push(ChartAction::Link { from, to });
                        }
                    }
                }
            }
        }
        if resp.drag_stopped() {
            g.drag = None;
        }
    } else if let Some((tid, _)) = hovered {
        // Tooltip with task details.
        if let Some(t) = p.task(tid)
            && let Some(pp) = pointer
        {
            let mut text = format!(
                "{}\nStart:  {}\nFinish: {}\nDuration: {}",
                t.name,
                format::date_long(t.sched.start),
                format::date_long(t.sched.finish),
                p.format_duration(t.sched.duration, t.estimated)
            );
            if t.sched.percent_complete > 0.0 {
                text += &format!("\nComplete: {}", format::percent(t.sched.percent_complete));
            }
            let res = p.format_resource_names(t.id);
            if !res.is_empty() {
                text += &format!("\nResources: {res}");
            }
            if t.sched.critical {
                text += "\nOn the critical path";
            }
            if t.is_milestone() && !t.deliverable.is_empty() {
                text += &format!("\nDeliverable: {}", t.deliverable);
            }
            tooltip_at(ui, pp + vec2(12.0, 12.0), &text);
        }
    }

    if resp.clicked() {
        if let Some((tid, _)) = hovered {
            let (shift, ctrl) = ui.input(|i| (i.modifiers.shift, i.modifiers.command));
            actions.push(ChartAction::Select { task: tid, shift, ctrl });
        } else if let Some(pp) = resp.interact_pointer_pos() {
            let r = ((pp.y - body.top() + *scroll_y) / rh).floor();
            if r >= 0.0 && (r as usize) < input.rows.len() {
                actions.push(ChartAction::Select { task: p.tasks[input.rows[r as usize]].id, shift: false, ctrl: false });
            }
        }
    }
    if resp.double_clicked()
        && let Some((tid, _)) = hovered
    {
        actions.push(ChartAction::Open(tid));
    }
    if resp.secondary_clicked()
        && let Some((tid, _)) = hovered
    {
        actions.push(ChartAction::Select { task: tid, shift: false, ctrl: false });
    }
    resp.context_menu(|ui| {
        crate::commands::task_context_menu(ui, &mut |c| actions.push(ChartAction::Command(c)));
    });

    // ---- Scrollbars ----
    let vs = Rect::from_min_max(pos2(rect.right() - 12.0, body.top()), pos2(rect.right(), body.bottom()));
    *scroll_y = scrollbar(ui, ui.id().with("gantt_vs"), vs, true, *scroll_y, content_h + rh, body.height());
    painter.rect_filled(
        Rect::from_min_max(pos2(rect.right() - 12.0, rect.top()), pos2(rect.right(), header.bottom())),
        0.0,
        theme::pal().HEADER_BG,
    );
    let hs = Rect::from_min_max(pos2(body.left(), body.bottom()), pos2(body.right(), rect.bottom()));
    // Horizontal range: project span with margins.
    let span_start = p.sched.start.unwrap_or(p.start) - Duration::days(30);
    let span_end = p.sched.finish.unwrap_or(p.start).max(now) + Duration::days(60);
    let total_px = ((span_end - span_start).num_minutes() as f32 / 1440.0 * g.px_per_day).max(body.width() + 1.0);
    let cur = ((g.left - span_start).num_minutes() as f32 / 1440.0 * g.px_per_day).clamp(0.0, total_px - body.width());
    let new = scrollbar(ui, ui.id().with("gantt_hs"), hs, false, cur, total_px, body.width());
    if (new - cur).abs() > 0.5 {
        g.left = span_start + Duration::minutes((new / g.px_per_day * 1440.0) as i64);
    }
    painter.rect_filled(Rect::from_min_max(pos2(rect.right() - 12.0, body.bottom()), rect.max), 0.0, theme::pal().PANEL_BG);
    painter.vline(rect.left() + 0.5, rect.y_range(), Stroke::new(1.0, theme::pal().GRID_LINE_STRONG));

    actions
}

fn dashed_vline(p: &Painter, x: f32, y0: f32, y1: f32, color: Color32) {
    let mut y = y0;
    while y < y1 {
        p.line_segment([pos2(x, y), pos2(x, (y + 5.0).min(y1))], Stroke::new(1.5, color));
        y += 9.0;
    }
}

fn tooltip_at(ui: &Ui, pos: Pos2, text: &str) {
    egui::Area::new(ui.id().with("gantt_tip")).order(egui::Order::Tooltip).fixed_pos(pos + vec2(14.0, 14.0)).interactable(false).show(
        ui.ctx(),
        |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.label(egui::RichText::new(text).size(12.0));
            });
        },
    );
}
