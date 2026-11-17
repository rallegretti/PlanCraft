//! Timesheet: weekly entry of hours worked per resource and task.
//!
//! Each cell holds the total tracked time for (task, resource, day); editing a
//! cell replaces the time entries for that day.

use egui::{Rect, Ui, pos2, vec2};
use plancraft_core::dates::prelude::*;
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
fn set_day(p: &mut Project, task: TaskId, resource: ResourceId, date: Date, minutes: Minutes) -> Result<(), PlanError> {
    p.time_entries.retain(|e| !(e.task == task && e.resource == resource && e.date == date));
    if minutes > 0 {
        p.log_time(task, resource, date, minutes, "")?;
    }
    Ok(())
}

/// The seven days of the week starting on `monday`.
fn week_days(monday: Date) -> [Date; 7] {
    std::array::from_fn(|i| plancraft_core::dates::add_days(monday, i64::try_from(i).unwrap_or(0)).unwrap_or(monday))
}

/// The timesheet's lines for a week: work assignments active that week or with time logged in
/// it (of `resource` only, when given), by resource name and then task order.
fn week_lines(p: &Project, days: &[Date; 7], resource: Option<ResourceId>) -> Vec<Line> {
    let (first, last) = (days[0], days[6]);
    let mut lines: Vec<Line> = Vec::new();
    for a in &p.assignments {
        if resource.is_some_and(|r| r != a.resource) {
            continue;
        }
        let (Some(t), Some(r)) = (p.task(a.task), p.resource(a.resource)) else {
            continue;
        };
        if r.kind != ResourceType::Work {
            continue;
        }
        // Show assignments active this week, or with time logged this week.
        let active = t.sched.start.zip(t.sched.finish).is_some_and(|(s, f)| s.date() <= last && f.date() >= first);
        let logged = p.time_entries.iter().any(|e| e.task == a.task && e.resource == a.resource && days.contains(&e.date));
        if active || logged {
            lines.push(Line { task: t.id, resource: r.id, label: t.name.clone(), resource_name: r.name.clone() });
        }
    }
    lines.sort_by(|a, b| a.resource_name.cmp(&b.resource_name).then(p.task_index(a.task).cmp(&p.task_index(b.task))));
    lines
}

pub fn show(ui: &mut Ui, rect: Rect, doc: &mut Document, st: &mut UiState) {
    let back = i64::from(st.timesheet_week.weekday().num_days_from_monday());
    let week = plancraft_core::dates::add_days(st.timesheet_week, -back).unwrap_or(st.timesheet_week);
    st.timesheet_week = week;
    let mut bar =
        ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_size(rect.min + vec2(8.0, 4.0), vec2(rect.width() - 16.0, 26.0))));
    bar.horizontal(|ui| {
        ui.label(egui::RichText::new("Timesheet").strong().size(15.0));
        ui.separator();
        if ui.button("⏴").on_hover_text("Previous week").clicked() {
            st.timesheet_week = plancraft_core::dates::add_days(week, -7).unwrap_or(week);
        }
        ui.label(format!("Week of {}", week.format("%b %-d, %Y")));
        if ui.button("⏵").on_hover_text("Next week").clicked() {
            st.timesheet_week = plancraft_core::dates::add_days(week, 7).unwrap_or(week);
        }
        if ui.button("This Week").clicked() {
            let today = plancraft_core::dates::today();
            st.timesheet_week = today - plancraft_core::dates::days(today.weekday().num_days_from_monday() as i64);
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
            match doc.edit("Update Progress from Time", |p| p.update_progress_from_time()) {
                Ok(_) => st.toast(ToastKind::Success, "Task progress updated from tracked time"),
                Err(e) => st.toast(ToastKind::Warning, e.to_string()),
            }
        }
    });
    let rect = Rect::from_min_max(pos2(rect.left(), rect.top() + 34.0), rect.max);

    let p = &doc.project;
    let days = week_days(week);
    let lines = week_lines(p, &days, st.timesheet_resource);

    let mut defs = vec![ColumnDef::new("Resource", 150.0).read_only(), ColumnDef::new("Task", 230.0).read_only()];
    for d in &days {
        defs.push(ColumnDef::new(d.format("%a %-m/%-d").to_string(), 74.0).right());
    }
    defs.push(ColumnDef::new("Week Total", 84.0).right().read_only());
    defs.push(ColumnDef::new("Actual / Planned", 140.0).right().read_only());

    let tracked = |task: TaskId, res: ResourceId, d: Date| -> Minutes {
        p.time_entries.iter().filter(|e| e.task == task && e.resource == res && e.date == d).map(|e| e.minutes).sum()
    };
    let cfg = GridConfig {
        id: egui::Id::new("timesheet"),
        columns: &defs,
        row_count: lines.len() + 1,
        blank_rows: 0,
        row_numbers: false,
        row_label: None,
        show_vscroll: true,
        header_height: 30.0,
        row_height: theme::ROW_HEIGHT,
        row_key: None,
    };
    let output = {
        let lines = &lines;
        let mut cell = |r: usize, c: usize| -> Cell {
            if r == lines.len() {
                // Totals row: sums, so no cell of it can be edited.
                let sum = match c {
                    1 => Cell { text: "Total".into(), bold: true, ..Default::default() },
                    2..=8 => {
                        let Some(&d) = days.get(c - 2) else { return Cell::default() };
                        let total: Minutes = lines.iter().map(|l| tracked(l.task, l.resource, d)).sum();
                        Cell {
                            text: if total > 0 { format::hours(total as f64) } else { String::new() },
                            bold: true,
                            bg: Some(theme::pal().HEADER_BG),
                            ..Default::default()
                        }
                    }
                    9 => {
                        let total: Minutes =
                            lines.iter().flat_map(|l| days.iter().map(move |d| (l, d))).map(|(l, d)| tracked(l.task, l.resource, *d)).sum();
                        Cell { text: format::hours(total as f64), bold: true, bg: Some(theme::pal().HEADER_BG), ..Default::default() }
                    }
                    _ => Cell::default(),
                };
                return Cell { read_only: true, ..sum };
            }
            let Some(l) = lines.get(r) else { return Cell::default() };
            match c {
                0 => Cell::text(l.resource_name.clone()),
                1 => Cell::text(l.label.clone()),
                2..=8 => {
                    let Some(&d) = days.get(c - 2) else { return Cell::default() };
                    let m = tracked(l.task, l.resource, d);
                    let workday =
                        p.resource(l.resource).map(|r| plancraft_core::workload::resource_calendar(p, r).is_working_day(d)).unwrap_or(true);
                    Cell {
                        text: if m > 0 { format::hours(m as f64) } else { String::new() },
                        edit_text: Some(if m > 0 { format!("{}", m as f64 / 60.0) } else { String::new() }),
                        bg: if workday { None } else { Some(theme::pal().NONWORKING) },
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
                        color: Some(if over { theme::pal().ERROR } else { theme::pal().TEXT }),
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
            let (Some(l), Some(&d)) = (lines.get(row).cloned(), col.checked_sub(2).and_then(|k| days.get(k))) else {
                continue;
            };
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
            theme::pal().TEXT_WEAK,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::Harness;

    /// Regression: the Totals row's day cells opened an editor, and what was typed there was
    /// accepted and then silently dropped.
    #[test]
    fn the_totals_row_cannot_be_edited() {
        let monday = plancraft_core::dates::ymd(2026, 10, 19).unwrap();
        let p = plancraft_core::sample::demo_project(monday);
        let lines = week_lines(&p, &week_days(monday), None).len();
        assert!(lines > 0, "the demo plan has work this week");
        let entries = p.time_entries.len();
        let st = UiState { timesheet_week: monday, ..UiState::default() };
        let mut h = Harness::builder().with_size(egui::vec2(1400.0, 700.0)).build_ui_state(
            |ui, state: &mut (Document, UiState)| {
                let (doc, st) = state;
                show(ui, ui.max_rect(), doc, st);
            },
            (Document::new(1, p, None), st),
        );
        h.run_steps(2);
        // Column 3 is Tuesday: a line's cell edits, the totals row's doesn't.
        for (row, editable) in [(0, true), (lines, false)] {
            h.state_mut().1.misc_grid.cursor = Some((row, 3));
            h.ctx.memory_mut(|m| m.request_focus(egui::Id::new("timesheet").with("body")));
            h.run_steps(2);
            h.key_press(egui::Key::F2);
            h.run_steps(2);
            assert_eq!(h.state().1.misc_grid.editing.is_some(), editable, "F2 on row {row}");
            h.key_press(egui::Key::Escape);
            h.run_steps(2);
            h.event(egui::Event::Text("5".into()));
            h.run_steps(2);
            assert_eq!(h.state().1.misc_grid.editing.is_some(), editable, "typing on row {row}");
            h.key_press(egui::Key::Escape);
            h.run_steps(2);
        }
        assert_eq!(h.state().0.project.time_entries.len(), entries, "nothing was logged");
    }
}
