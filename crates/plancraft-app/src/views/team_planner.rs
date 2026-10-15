//! Team Planner: one swimlane per resource showing assigned tasks on a
//! timeline. Drag a task to another resource to reassign it.

use egui::{Align2, Color32, CursorIcon, FontId, Rect, Sense, Stroke, StrokeKind, Ui, pos2, vec2};
use plancraft_core::prelude::*;
use plancraft_core::workspace::{ChangeSource, Document};

use crate::commands::Command;
use crate::format;
use crate::state::{ToastKind, UiState};
use crate::theme;
use crate::widgets::grid::scrollbar;
use crate::widgets::timescale;

const LANE_H: f32 = 22.0;
const NAME_W: f32 = 200.0;

struct Lane {
    resource: Option<ResourceId>,
    name: String,
    over: bool,
    /// (task index, sub-lane)
    bars: Vec<(usize, usize)>,
    sublanes: usize,
}

fn pack(p: &Project, mut idxs: Vec<usize>) -> (Vec<(usize, usize)>, usize) {
    idxs.sort_by_key(|&i| p.tasks[i].sched.start);
    let mut lane_end: Vec<chrono::NaiveDateTime> = Vec::new();
    let mut out = Vec::new();
    for i in idxs {
        let t = &p.tasks[i];
        let (Some(s), Some(f)) = (t.sched.start, t.sched.finish) else {
            continue;
        };
        let lane = lane_end.iter().position(|e| *e <= s).unwrap_or_else(|| {
            lane_end.push(s);
            lane_end.len() - 1
        });
        lane_end[lane] = f.max(s + chrono::Duration::minutes(1));
        out.push((i, lane));
    }
    let n = lane_end.len().max(1);
    (out, n)
}

pub fn show(ui: &mut Ui, rect: Rect, doc: &mut Document, st: &mut UiState, out: &mut Vec<Command>) {
    let p = &doc.project;
    let over: std::collections::HashSet<ResourceId> = plancraft_core::workload::overallocations(p)
        .into_iter()
        .map(|(r, _)| r)
        .collect();
    let mut lanes: Vec<Lane> = Vec::new();
    for r in p.resources.iter().filter(|r| r.kind == ResourceType::Work) {
        let idxs: Vec<usize> = p
            .assignments_for_resource(r.id)
            .filter_map(|a| p.task_index(a.task))
            .collect();
        let (bars, sublanes) = pack(p, idxs);
        lanes.push(Lane {
            resource: Some(r.id),
            name: r.name.clone(),
            over: over.contains(&r.id),
            bars,
            sublanes,
        });
    }
    let unassigned: Vec<usize> = (0..p.tasks.len())
        .filter(|&i| {
            let t = &p.tasks[i];
            !t.sched.is_summary
                && !t.is_milestone()
                && !p.assignments_for_task(t.id).any(|a| {
                    p.resource(a.resource)
                        .is_some_and(|r| r.kind == ResourceType::Work)
                })
        })
        .collect();
    let (bars, sublanes) = pack(p, unassigned);
    lanes.push(Lane {
        resource: None,
        name: "Unassigned Tasks".into(),
        over: false,
        bars,
        sublanes,
    });

    let header = Rect::from_min_size(rect.min, vec2(rect.width() - 12.0, theme::HEADER_HEIGHT));
    let body = Rect::from_min_max(
        pos2(rect.left(), header.bottom()),
        pos2(rect.right() - 12.0, rect.bottom()),
    );
    let chart_left = rect.left() + NAME_W;
    let chart = Rect::from_min_max(pos2(chart_left, body.top()), body.max);
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, theme::BG);

    // Header: name column + timescale.
    painter.rect_filled(
        Rect::from_min_size(header.min, vec2(NAME_W, header.height())),
        0.0,
        theme::HEADER_BG,
    );
    painter.text(
        pos2(header.left() + 8.0, header.center().y),
        Align2::LEFT_CENTER,
        "Resource Name",
        FontId::proportional(12.5),
        theme::TEXT,
    );
    let ts_rect = Rect::from_min_max(pos2(chart_left, header.top()), header.max);
    {
        let mut g = st.gantt.clone();
        g.left = st.gantt.left;
        timescale::paint_header(&painter.with_clip_rect(ts_rect), ts_rect, &g);
    }

    // Scroll.
    let total_h: f32 = lanes.iter().map(|l| l.sublanes as f32 * LANE_H + 8.0).sum();
    if ui.rect_contains_pointer(body) {
        let (d, shift, cmd) = ui.input(|i| {
            (
                i.smooth_scroll_delta,
                i.modifiers.shift,
                i.modifiers.command,
            )
        });
        if cmd {
            let z = ui.input(|i| i.zoom_delta());
            if (z - 1.0).abs() > 1e-4 {
                let x = ui
                    .input(|i| i.pointer.hover_pos())
                    .map(|p| p.x)
                    .unwrap_or(chart.center().x);
                st.gantt.zoom_by(z, x, chart_left);
            }
        } else if shift {
            st.gantt.left -=
                chrono::Duration::minutes(((d.y + d.x) / st.gantt.px_per_day * 1440.0) as i64);
        } else {
            st.misc_grid.scroll_y -= d.y;
            st.gantt.left -= chrono::Duration::minutes((d.x / st.gantt.px_per_day * 1440.0) as i64);
        }
    }
    st.misc_grid.scroll_y = st
        .misc_grid
        .scroll_y
        .clamp(0.0, (total_h - body.height() + 20.0).max(0.0));

    let bp = painter.with_clip_rect(body);
    let cp = painter.with_clip_rect(chart);
    // Today line.
    let now = chrono::Local::now().naive_local();
    let xt = st.gantt.x_of(chart_left, now);
    if chart.x_range().contains(xt) {
        cp.vline(xt, chart.y_range(), Stroke::new(1.5, theme::TODAY));
    }

    let pointer = ui.input(|i| i.pointer.hover_pos());
    let drag_id = egui::Id::new("team_planner_drag");
    let dragging: Option<(TaskId, Option<ResourceId>)> = ui.ctx().data(|d| d.get_temp(drag_id));
    let mut hover_lane: Option<Option<ResourceId>> = None;
    let mut hovered_bar: Option<(usize, Option<ResourceId>)> = None;
    let mut y = body.top() - st.misc_grid.scroll_y;
    for lane in &lanes {
        let h = lane.sublanes as f32 * LANE_H + 8.0;
        let lane_rect = Rect::from_min_size(pos2(body.left(), y), vec2(body.width(), h));
        if pointer.is_some_and(|pp| lane_rect.contains(pp)) {
            hover_lane = Some(lane.resource);
        }
        let selected = lane.resource.is_some_and(|r| st.sel.resources.contains(&r));
        let name_rect = Rect::from_min_size(lane_rect.min, vec2(NAME_W, h));
        bp.rect_filled(
            name_rect,
            0.0,
            if selected {
                theme::ACCENT_LIGHT
            } else if lane.resource.is_none() {
                theme::PANEL_BG
            } else {
                theme::HEADER_BG
            },
        );
        let name_color = if lane.over { theme::ERROR } else { theme::TEXT };
        let icon = if lane.resource.is_none() {
            "📋"
        } else if lane.over {
            "⚠"
        } else {
            "👤"
        };
        bp.text(
            pos2(name_rect.left() + 10.0, name_rect.top() + 13.0),
            Align2::LEFT_CENTER,
            icon,
            FontId::proportional(12.0),
            name_color,
        );
        bp.text(
            pos2(name_rect.left() + 28.0, name_rect.top() + 13.0),
            Align2::LEFT_CENTER,
            &lane.name,
            FontId::proportional(12.5),
            name_color,
        );
        if let Some(rid) = lane.resource
            && ui
                .interact(
                    name_rect.intersect(body),
                    egui::Id::new(("tp_name", rid)),
                    Sense::click(),
                )
                .clicked()
        {
            st.sel.select_resource(rid);
        }
        bp.hline(
            lane_rect.x_range(),
            lane_rect.bottom() - 0.5,
            Stroke::new(1.0, theme::GRID_LINE_STRONG),
        );
        bp.vline(
            name_rect.right(),
            lane_rect.y_range(),
            Stroke::new(1.0, theme::GRID_LINE_STRONG),
        );
        for &(i, sub) in &lane.bars {
            let t = &p.tasks[i];
            let (Some(s), Some(f)) = (t.sched.start, t.sched.finish) else {
                continue;
            };
            let x0 = st.gantt.x_of(chart_left, s);
            let x1 = st.gantt.x_of(chart_left, f).max(x0 + 3.0);
            let r = Rect::from_min_max(
                pos2(x0, y + 4.0 + sub as f32 * LANE_H),
                pos2(x1, y + 4.0 + sub as f32 * LANE_H + LANE_H - 4.0),
            );
            let complete = t.is_complete();
            let fill = if complete {
                Color32::from_rgb(0x8F, 0xA3, 0xC8)
            } else if t.sched.critical && st.gantt.show_critical {
                theme::BAR_CRITICAL
            } else {
                theme::BAR_TASK
            };
            cp.rect_filled(r, 3.0, fill);
            if t.sched.percent_complete > 0.0 && !complete {
                let px = r.left() + r.width() * t.sched.percent_complete / 100.0;
                cp.rect_filled(
                    Rect::from_min_max(r.min, pos2(px, r.bottom())),
                    3.0,
                    theme::BAR_TASK_EDGE,
                );
            }
            if st.sel.tasks.contains(&t.id) {
                cp.rect_stroke(
                    r,
                    3.0,
                    Stroke::new(2.0, theme::ACCENT_DARK),
                    StrokeKind::Outside,
                );
            }
            cp.with_clip_rect(r.intersect(chart)).text(
                pos2(r.left() + 5.0, r.center().y),
                Align2::LEFT_CENTER,
                &t.name,
                FontId::proportional(11.5),
                Color32::WHITE,
            );
            if pointer.is_some_and(|pp| r.contains(pp) && chart.contains(pp)) {
                hovered_bar = Some((i, lane.resource));
            }
        }
        y += h;
    }

    // Interaction: click selects, double-click opens, drag reassigns.
    let resp = ui.interact(chart, egui::Id::new("tp_chart"), Sense::click_and_drag());
    if let Some((i, lane_res)) = hovered_bar {
        ui.ctx().set_cursor_icon(CursorIcon::Grab);
        let t = &p.tasks[i];
        if dragging.is_none() {
            resp.clone().on_hover_text(format!(
                "{}\n{} – {}\n{}",
                t.name,
                format::date(t.sched.start),
                format::date(t.sched.finish),
                p.format_resource_names(t.id)
            ));
        }
        if resp.clicked() {
            st.sel.select_task(t.id);
        }
        if resp.double_clicked() {
            st.sel.select_task(t.id);
            out.push(Command::TaskInfo);
        }
        if resp.drag_started() {
            ui.ctx()
                .data_mut(|d| d.insert_temp(drag_id, (t.id, lane_res)));
        }
    }
    if let Some((tid, from_res)) = dragging {
        ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
        if let Some(pp) = pointer {
            let name = p.task(tid).map(|t| t.name.clone()).unwrap_or_default();
            painter.text(
                pp + vec2(12.0, 0.0),
                Align2::LEFT_CENTER,
                format!("↪ {name}"),
                FontId::proportional(12.0),
                theme::ACCENT,
            );
        }
        if resp.drag_stopped() || !ui.input(|i| i.pointer.any_down()) {
            ui.ctx()
                .data_mut(|d| d.remove::<(TaskId, Option<ResourceId>)>(drag_id));
            if let Some(to_res) = hover_lane
                && to_res != from_res
            {
                let r = doc.try_edit("Reassign Task", ChangeSource::User, |p| {
                    if let Some(fr) = from_res
                        && let Some(a) = p
                            .assignments
                            .iter()
                            .find(|a| a.task == tid && a.resource == fr)
                            .map(|a| a.id)
                    {
                        p.unassign(a)?;
                    }
                    if let Some(tr) = to_res {
                        p.assign(tid, tr, None, None)?;
                    }
                    Ok::<(), PlanError>(())
                });
                if let Err(e) = r {
                    st.toast(ToastKind::Error, e.to_string());
                }
            }
        }
    }

    let vs = Rect::from_min_max(
        pos2(rect.right() - 12.0, body.top()),
        pos2(rect.right(), body.bottom()),
    );
    st.misc_grid.scroll_y = scrollbar(
        ui,
        egui::Id::new("tp_vs"),
        vs,
        true,
        st.misc_grid.scroll_y,
        total_h + 20.0,
        body.height(),
    );
    painter.hline(
        header.x_range(),
        header.bottom() - 0.5,
        Stroke::new(1.0, theme::GRID_LINE_STRONG),
    );
}
