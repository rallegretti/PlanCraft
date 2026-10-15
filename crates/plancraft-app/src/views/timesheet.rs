//! Timesheet: weekly entry of hours worked per resource and task.
//!
//! Each cell holds the total tracked time for (task, resource, day); editing a
//! cell replaces the time entries for that day.

use chrono::{Datelike, Duration, NaiveDate};
use egui::{Rect, Ui, pos2, vec2};
use plancraft_core::prelude::*;
use plancraft_core::workspace::{ChangeSource, Document};

use crate::format;
use crate::state::{ToastKind, UiState};
use crate::theme;
use crate::widgets::grid::{self, Cell, ColumnDef, GridConfig, GridEvent};

#[derive(Clone)]
struct Line {
    task: TaskId,
    resource: ResourceId,
    label: String,
    resource_name: String,
}

/// Replaces the tracked time of (task, resource) on `date` with `minutes`.
fn set_day(p: &mut Project, task: TaskId, resource: ResourceId, date: NaiveDate, minutes: Minutes) -> Result<(), PlanError> {
    p.time_entries.retain(|e| !(e.task == task && e.resource == resource && e.date == date));
    if minutes > 0 {
        p.log_time(task, resource, date, minutes, "")?;
    }
    Ok(())
}

pub fn show(ui: &mut Ui, rect: Rect, doc: &mut Document, st: &mut UiState) {
    let week = st.timesheet_week - Duration::days(st.timesheet_week.weekday().num_days_from_monday() as i64);
    st.timesheet_week = week;
    let mut bar =
        ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_size(rect.min + vec2(8.0, 4.0), vec2(rect.width() - 16.0, 26.0))));
    bar.horizontal(|ui| {
        ui.label(egui::RichText::new("Timesheet").strong().size(15.0));
        ui.separator();
        if ui.button("⏴").on_hover_text("Previous week").clicked() {
            st.timesheet_week = week - Duration::days(7);
        }
        ui.label(format!("Week of {}", week.format("%b %-d, %Y")));
        if ui.button("⏵").on_hover_text("Next week").clicked() {
            st.timesheet_week = week + Duration::days(7);
        }
        if ui.button("This Week").clicked() {
            let today = chrono::Local::now().date_naive();
            st.timesheet_week = today - Duration::days(today.weekday().num_days_from_monday() as i64);
        }
        ui.separator();
        ui.label("Resource:");
        let name =
            st.timesheet_resource.and_then(|r| doc.project.resource(r)).map(|r| r.name.clone()).unwrap_or_else(|| "All resources".into());
        egui::ComboBox::from_id_salt("ts_res").selected_text(name).width(170.0).show_ui(ui, |ui| {
            if ui.selectable_label(st.timesheet_resource.is_none(), "All resources").clicked() {
                st.timesheet_resource = None;
            }
            for r in doc.project.resources.iter().filter(|r| r.kind == ResourceType::Work) {
                if ui.selectable_label(st.timesheet_resource == Some(r.id), &r.name).clicked() {
                    st.timesheet_resource = Some(r.id);
                }
            }
        });
        ui.separator();
        if ui.button("⏱ Update task progress from time").on_hover_text("Set each task's % complete from tracked vs planned work").clicked()
        {
            doc.edit("Update Progress from Time", |p| p.update_progress_from_time());
            st.toast(ToastKind::Success, "Task progress updated from tracked time");
        }
    });
    let rect = Rect::from_min_max(pos2(rect.left(), rect.top() + 34.0), rect.max);

    let p = &doc.project;
    let days: Vec<NaiveDate> = (0..7).map(|i| week + Duration::days(i)).collect();
    let mut lines: Vec<Line> = Vec::new();
    for a in &p.assignments {
        if st.timesheet_resource.is_some_and(|r| r != a.resource) {
            continue;
        }
        let (Some(t), Some(r)) = (p.task(a.task), p.resource(a.resource)) else {
            continue;
        };
        if r.kind != ResourceType::Work {
            continue;
        }
        // Show assignments active this week, or with time logged this week.
        let active = t.sched.start.zip(t.sched.finish).is_some_and(|(s, f)| s.date() <= days[6] && f.date() >= days[0]);
        let logged = p.time_entries.iter().any(|e| e.task == a.task && e.resource == a.resource && days.contains(&e.date));
        if active || logged {
            lines.push(Line { task: t.id, resource: r.id, label: t.name.clone(), resource_name: r.name.clone() });
        }
    }
    lines.sort_by(|a, b| a.resource_name.cmp(&b.resource_name).then(p.task_index(a.task).cmp(&p.task_index(b.task))));

    let mut defs = vec![ColumnDef::new("Resource", 150.0).read_only(), ColumnDef::new("Task", 230.0).read_only()];
    for d in &days {
        defs.push(ColumnDef::new(d.format("%a %-m/%-d").to_string(), 74.0).right());
    }
    defs.push(ColumnDef::new("Week Total", 84.0).right().read_only());
    defs.push(ColumnDef::new("Actual / Planned", 140.0).right().read_only());

    let tracked = |task: TaskId, res: ResourceId, d: NaiveDate| -> Minutes {
        p.time_entries.iter().filter(|e| e.task == task && e.resource == res && e.date == d).map(|e| e.minutes).sum()
    };
    let cfg = GridConfig {
        id: egui::Id::new("timesheet"),
        columns: &defs,
        row_count: lines.len() + 1,
        blank_rows: 0,
        row_numbers: false,
        show_vscroll: true,
        header_height: 30.0,
        row_height: theme::ROW_HEIGHT,
    };
    let output = {
        let lines = &lines;
        let mut cell = |r: usize, c: usize| -> Cell {
            if r == lines.len() {
                // Totals row.
                return match c {
                    1 => Cell { text: "Total".into(), bold: true, ..Default::default() },
                    2..=8 => {
                        let d = days[c - 2];
                        let total: Minutes = lines.iter().map(|l| tracked(l.task, l.resource, d)).sum();
                        Cell {
                            text: if total > 0 { format::hours(total as f64) } else { String::new() },
                            bold: true,
                            bg: Some(theme::HEADER_BG),
                            ..Default::default()
                        }
                    }
                    9 => {
                        let total: Minutes =
                            lines.iter().flat_map(|l| days.iter().map(move |d| (l, d))).map(|(l, d)| tracked(l.task, l.resource, *d)).sum();
                        Cell { text: format::hours(total as f64), bold: true, bg: Some(theme::HEADER_BG), ..Default::default() }
                    }
                    _ => Cell::default(),
                };
            }
            let l = &lines[r];
            match c {
                0 => Cell::text(l.resource_name.clone()),
                1 => Cell::text(l.label.clone()),
                2..=8 => {
                    let d = days[c - 2];
                    let m = tracked(l.task, l.resource, d);
                    let workday =
                        p.resource(l.resource).map(|r| plancraft_core::workload::resource_calendar(p, r).is_working_day(d)).unwrap_or(true);
                    Cell {
                        text: if m > 0 { format::hours(m as f64) } else { String::new() },
                        edit_text: Some(if m > 0 { format!("{}", m as f64 / 60.0) } else { String::new() }),
                        bg: if workday { None } else { Some(theme::NONWORKING) },
                        ..Default::default()
                    }
                }
                9 => Cell::text(format::hours(days.iter().map(|d| tracked(l.task, l.resource, *d)).sum::<Minutes>() as f64)),
                10 => {
                    let a = p.assignments.iter().find(|a| a.task == l.task && a.resource == l.resource);
                    let actual: Minutes =
                        p.time_entries.iter().filter(|e| e.task == l.task && e.resource == l.resource).map(|e| e.minutes).sum::<Minutes>()
                            + a.map(|a| a.actual_work).unwrap_or(0);
                    let planned = a.map(|a| a.work).unwrap_or(0);
                    let over = planned > 0 && actual > planned;
                    Cell {
                        text: format!("{} / {}", format_work(actual), format_work(planned)),
                        color: Some(if over { theme::ERROR } else { theme::TEXT }),
                        ..Default::default()
                    }
                }
                _ => Cell::default(),
            }
        };
        grid::show(ui, rect, &mut st.misc_grid, &cfg, &|_| false, &mut cell)
    };
    for ev in output.events {
        if let GridEvent::Commit { row, col, text } = ev {
            if row >= lines.len() || !(2..=8).contains(&col) {
                continue;
            }
            let l = lines[row].clone();
            let d = days[col - 2];
            let minutes = if text.trim().is_empty() { Some(0) } else { parse_work(&text, &doc.project.settings.time) };
            match minutes {
                Some(m) => {
                    if let Err(e) = doc.try_edit("Log Time", ChangeSource::User, |p| set_day(p, l.task, l.resource, d, m)) {
                        st.toast(ToastKind::Error, e.to_string());
                    }
                }
                None => st.toast(ToastKind::Error, format!("'{text}' is not a valid number of hours")),
            }
        }
    }
    if lines.is_empty() {
        ui.painter().text(
            pos2(rect.center().x, rect.top() + 90.0),
            egui::Align2::CENTER_CENTER,
            "No assignments are scheduled this week. Assign resources to tasks to track their time here.",
            egui::FontId::proportional(13.0),
            theme::TEXT_WEAK,
        );
    }
}
