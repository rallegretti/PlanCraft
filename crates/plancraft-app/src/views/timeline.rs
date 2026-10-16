//! The Timeline strip above the main view: a project-at-a-glance bar with
//! highlighted tasks and milestones, and a draggable window showing the part of
//! the Gantt chart currently in view.

use chrono::{Duration, NaiveDateTime};
use egui::{Align2, Color32, CursorIcon, FontId, Rect, Sense, Shape, Stroke, StrokeKind, Ui, pos2, vec2};
use plancraft_core::prelude::*;

use crate::state::GanttState;
use crate::theme;

pub const HEIGHT: f32 = 78.0;

pub fn show(ui: &mut Ui, rect: Rect, p: &Project, g: &mut GanttState, chart_width: f32) {
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, theme::pal().PANEL_BG);
    painter.hline(rect.x_range(), rect.bottom() - 0.5, Stroke::new(1.0, theme::pal().GRID_LINE_STRONG));
    painter.text(
        pos2(rect.left() + 8.0, rect.top() + 9.0),
        Align2::LEFT_CENTER,
        "TIMELINE",
        FontId::proportional(10.0),
        theme::pal().TEXT_WEAK,
    );

    let (Some(start), Some(finish)) = (p.sched.start, p.sched.finish) else {
        painter.text(
            rect.center(),
            Align2::CENTER_CENTER,
            "Add tasks to see the timeline",
            FontId::proportional(12.0),
            theme::pal().TEXT_WEAK,
        );
        return;
    };
    let span = (finish - start).num_minutes().max(60) as f32;
    let bar = Rect::from_min_max(pos2(rect.left() + 90.0, rect.top() + 30.0), pos2(rect.right() - 90.0, rect.top() + 52.0));
    let x_of = |t: NaiveDateTime| bar.left() + (t - start).num_minutes() as f32 / span * bar.width();
    let t_of = |x: f32| start + Duration::minutes(((x - bar.left()) / bar.width() * span) as i64);

    // Start/finish labels.
    painter.text(
        pos2(rect.left() + 10.0, bar.center().y - 7.0),
        Align2::LEFT_CENTER,
        "Start",
        FontId::proportional(10.0),
        theme::pal().TEXT_WEAK,
    );
    painter.text(
        pos2(rect.left() + 10.0, bar.center().y + 7.0),
        Align2::LEFT_CENTER,
        start.format("%a %-m/%-d/%y").to_string(),
        FontId::proportional(11.5),
        theme::pal().TEXT,
    );
    painter.text(
        pos2(rect.right() - 10.0, bar.center().y - 7.0),
        Align2::RIGHT_CENTER,
        "Finish",
        FontId::proportional(10.0),
        theme::pal().TEXT_WEAK,
    );
    painter.text(
        pos2(rect.right() - 10.0, bar.center().y + 7.0),
        Align2::RIGHT_CENTER,
        finish.format("%a %-m/%-d/%y").to_string(),
        FontId::proportional(11.5),
        theme::pal().TEXT,
    );

    painter.rect_filled(bar, 3.0, theme::pal().TIMELINE_BAND);
    painter.rect_stroke(bar, 3.0, Stroke::new(1.0, theme::pal().ACCENT_LIGHT), StrokeKind::Inside);

    // Month ticks.
    let mut m = crate::widgets::timescale::Unit::Month.floor(start);
    let unit =
        if (finish - start).num_days() > 400 { crate::widgets::timescale::Unit::Quarter } else { crate::widgets::timescale::Unit::Month };
    let mut guard = 0;
    while m <= finish && guard < 200 {
        guard += 1;
        let x = x_of(m);
        if x > bar.left() + 2.0 {
            painter.vline(x, bar.top()..=bar.bottom(), Stroke::new(1.0, theme::pal().TIMELINE_TICK));
        }
        let next = unit.next(m);
        let label_x = x_of(m).max(bar.left()) + 4.0;
        if x_of(next) - label_x > 30.0 {
            painter.text(
                pos2(label_x, bar.top() - 7.0),
                Align2::LEFT_CENTER,
                unit.label(m, x_of(next) - label_x),
                FontId::proportional(10.5),
                theme::pal().TEXT_WEAK,
            );
        }
        m = next;
    }

    // Tasks marked for the timeline as bars inside the band, milestones as diamonds below.
    let font = FontId::proportional(11.0);
    let mut ms_slots: Vec<f32> = Vec::new();
    for t in p.tasks.iter().filter(|t| t.show_on_timeline) {
        let (Some(s), Some(f)) = (t.sched.start, t.sched.finish) else {
            continue;
        };
        if t.is_milestone() {
            let x = x_of(f);
            let c = pos2(x, bar.bottom() + 5.0);
            painter.add(Shape::convex_polygon(
                vec![c + vec2(0.0, -5.0), c + vec2(5.0, 0.0), c + vec2(0.0, 5.0), c + vec2(-5.0, 0.0)],
                theme::pal().GOLD,
                Stroke::new(1.0, theme::pal().ACCENT_DARK),
            ));
            // Alternate label rows to reduce overlap.
            let row = ms_slots.iter().filter(|&&px| (px - x).abs() < 110.0).count() % 2;
            ms_slots.push(x);
            let y = bar.bottom() + 15.0 + row as f32 * 11.0;
            painter.text(
                pos2(x, y.min(rect.bottom() - 6.0)),
                Align2::CENTER_CENTER,
                format!("{} {}", t.name, f.format("%-m/%-d")),
                font.clone(),
                theme::pal().TEXT,
            );
        } else {
            let r = Rect::from_min_max(pos2(x_of(s), bar.top() + 2.0), pos2(x_of(f), bar.bottom() - 2.0));
            painter.rect_filled(r, 2.0, theme::pal().BAR_TASK);
            let c = painter.with_clip_rect(r);
            c.text(pos2(r.left() + 4.0, r.center().y), Align2::LEFT_CENTER, &t.name, font.clone(), Color32::WHITE);
        }
    }

    // Today marker.
    let now = chrono::Local::now().naive_local();
    if now >= start && now <= finish {
        let x = x_of(now);
        painter.vline(x, (bar.top() - 4.0)..=(bar.bottom() + 2.0), Stroke::new(2.0, theme::pal().TODAY));
        painter.text(pos2(x, bar.top() - 9.0), Align2::CENTER_CENTER, "Today", FontId::proportional(9.5), theme::pal().TODAY);
    }

    // Visible-range window (drag to pan the Gantt chart).
    let view_start = g.left;
    let view_end = g.time_at(0.0, chart_width);
    let wx0 = x_of(view_start).clamp(bar.left(), bar.right());
    let wx1 = x_of(view_end).clamp(bar.left(), bar.right());
    if wx1 > wx0 + 1.0 {
        let w = Rect::from_min_max(pos2(wx0, bar.top() - 2.0), pos2(wx1, bar.bottom() + 2.0));
        painter.rect_filled(w, 2.0, theme::pal().TIMELINE_WINDOW);
        painter.rect_stroke(w, 2.0, Stroke::new(1.5, theme::pal().ACCENT), StrokeKind::Outside);
    }
    let resp = ui.interact(bar.expand(4.0), ui.id().with("timeline_bar"), Sense::click_and_drag());
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::Grab);
    }
    if (resp.dragged() || resp.clicked())
        && let Some(pp) = resp.interact_pointer_pos()
    {
        let half = (view_end - view_start) / 2;
        g.left = t_of(pp.x) - half;
    }
}
