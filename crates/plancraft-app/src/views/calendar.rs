//! Calendar view: a month grid with task bars.

use chrono::{Datelike, Duration, NaiveDate};
use egui::{Align2, Color32, FontId, Rect, Sense, Stroke, Ui, pos2, vec2};
use plancraft_core::prelude::*;
use plancraft_core::schedule::project_calendar;
use plancraft_core::workspace::Document;

use crate::commands::Command;
use crate::state::UiState;
use crate::theme;

pub fn show(ui: &mut Ui, rect: Rect, doc: &mut Document, st: &mut UiState, out: &mut Vec<Command>) {
    let p = &doc.project;
    let month = st.calendar_month.with_day(1).unwrap_or(st.calendar_month);
    let mut bar =
        ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_size(rect.min + vec2(8.0, 4.0), vec2(rect.width() - 16.0, 26.0))));
    bar.horizontal(|ui| {
        if ui.button("⏴").clicked() {
            st.calendar_month = (month - Duration::days(1)).with_day(1).unwrap();
        }
        ui.label(egui::RichText::new(month.format("%B %Y").to_string()).size(16.0).strong());
        if ui.button("⏵").clicked() {
            st.calendar_month = (month + Duration::days(32)).with_day(1).unwrap();
        }
        if ui.button("Today").clicked() {
            st.calendar_month = chrono::Local::now().date_naive().with_day(1).unwrap();
        }
        if ui.button("Project Start").clicked() {
            st.calendar_month = p.sched.start.unwrap_or(p.start).date().with_day(1).unwrap();
        }
    });
    let area = Rect::from_min_max(pos2(rect.left() + 8.0, rect.top() + 36.0), rect.max - vec2(8.0, 8.0));
    let painter = ui.painter_at(area);
    let header_h = 22.0;
    let first = month - Duration::days(month.weekday().num_days_from_monday() as i64);
    let weeks = 6;
    let cell_w = area.width() / 7.0;
    let cell_h = (area.height() - header_h) / weeks as f32;
    for (i, name) in ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"].iter().enumerate() {
        let r = Rect::from_min_size(pos2(area.left() + i as f32 * cell_w, area.top()), vec2(cell_w, header_h));
        painter.rect_filled(r, 0.0, theme::pal().HEADER_BG);
        painter.text(r.center(), Align2::CENTER_CENTER, *name, FontId::proportional(12.0), theme::pal().TEXT);
    }
    let cal = project_calendar(p);
    let today = chrono::Local::now().date_naive();
    let pointer = ui.input(|i| i.pointer.hover_pos());
    let mut clicked_task: Option<(TaskId, bool)> = None;
    for w in 0..weeks {
        let week_start = first + Duration::days(7 * w as i64);
        let wy = area.top() + header_h + w as f32 * cell_h;
        for dcol in 0..7 {
            let day = week_start + Duration::days(dcol);
            let r = Rect::from_min_size(pos2(area.left() + dcol as f32 * cell_w, wy), vec2(cell_w, cell_h));
            let fill = if !cal.is_working_day(day) { theme::pal().NONWORKING } else { theme::pal().BG };
            painter.rect_filled(r, 0.0, fill);
            painter.rect_stroke(r, 0.0, Stroke::new(1.0, theme::pal().GRID_LINE), egui::StrokeKind::Inside);
            let color = if day.month() == month.month() { theme::pal().TEXT } else { theme::pal().TEXT_WEAK };
            let label = if day.day() == 1 { day.format("%b %-d").to_string() } else { day.day().to_string() };
            if day == today {
                painter.rect_filled(Rect::from_min_size(r.right_top() + vec2(-34.0, 3.0), vec2(30.0, 16.0)), 8.0, theme::pal().ACCENT);
                painter.text(r.right_top() + vec2(-19.0, 11.0), Align2::CENTER_CENTER, &label, FontId::proportional(11.5), Color32::WHITE);
            } else {
                painter.text(r.right_top() + vec2(-6.0, 11.0), Align2::RIGHT_CENTER, &label, FontId::proportional(11.5), color);
            }
        }
        // Task bars for this week, stacked.
        let week_end = week_start + Duration::days(7);
        let mut lane = 0usize;
        let max_lanes = ((cell_h - 24.0) / 18.0).floor().max(0.0) as usize;
        let mut hidden = 0;
        for t in p.tasks.iter().filter(|t| !t.sched.is_summary) {
            let (Some(s), Some(f)) = (t.sched.start, t.sched.finish) else {
                continue;
            };
            let (sd, fd) = (s.date(), if t.is_milestone() { s.date() } else { (f - Duration::minutes(1)).date() });
            if fd < week_start || sd >= week_end {
                continue;
            }
            if lane >= max_lanes {
                hidden += 1;
                continue;
            }
            let c0 = (sd.max(week_start) - week_start).num_days() as f32;
            let c1 = (fd.min(week_end - Duration::days(1)) - week_start).num_days() as f32 + 1.0;
            let y = wy + 22.0 + lane as f32 * 18.0;
            let r = Rect::from_min_max(pos2(area.left() + c0 * cell_w + 3.0, y), pos2(area.left() + c1 * cell_w - 3.0, y + 15.0));
            let selected = st.sel.tasks.contains(&t.id);
            let fill = if t.is_milestone() {
                theme::pal().GOLD
            } else if t.is_complete() {
                theme::pal().BAR_COMPLETE
            } else {
                theme::pal().BAR_TASK
            };
            painter.rect_filled(r, 3.0, fill);
            if selected {
                painter.rect_stroke(r, 3.0, Stroke::new(2.0, theme::pal().ACCENT_DARK), egui::StrokeKind::Outside);
            }
            // Gold milestone chips need dark text in both themes.
            let text_color = if t.is_milestone() { theme::LIGHT.TEXT } else { Color32::WHITE };
            let label = if t.is_milestone() { format!("♦ {}", t.name) } else { t.name.clone() };
            painter.with_clip_rect(r).text(
                pos2(r.left() + 4.0, r.center().y),
                Align2::LEFT_CENTER,
                label,
                FontId::proportional(11.0),
                text_color,
            );
            if pointer.is_some_and(|pp| r.contains(pp)) {
                let resp = ui.interact(r, egui::Id::new(("cal_task", t.id, w)), Sense::click());
                if resp.clicked() {
                    clicked_task = Some((t.id, false));
                }
                if resp.double_clicked() {
                    clicked_task = Some((t.id, true));
                }
                resp.on_hover_text(format!("{}\n{} – {}", t.name, crate::format::date(t.sched.start), crate::format::date(t.sched.finish)));
            }
            lane += 1;
        }
        if hidden > 0 {
            painter.text(
                pos2(area.left() + 6.0, wy + cell_h - 9.0),
                Align2::LEFT_CENTER,
                format!("+{hidden} more"),
                FontId::proportional(10.5),
                theme::pal().TEXT_WEAK,
            );
        }
    }
    if let Some((id, open)) = clicked_task {
        st.sel.select_task(id);
        if open {
            out.push(Command::TaskInfo);
        }
    }
    let _ = NaiveDate::MIN;
}
