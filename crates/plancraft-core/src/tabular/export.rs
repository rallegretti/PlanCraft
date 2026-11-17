//! A plan's tables as CSV, for spreadsheets: tasks, resources, assignments and logged time.
//! Reading the task list back with this module's import gives the same tasks, outline,
//! durations, links, resources and progress; the other tables are for reading elsewhere.

use std::collections::{HashMap, HashSet};

use crate::dates::prelude::*;
use crate::ids::{ResourceId, TaskId};
use crate::model::{Project, Resource, ResourceType, Task};

/// Which of a plan's tables to export.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CsvTable {
    Tasks,
    Resources,
    Assignments,
    /// Logged time (timesheet entries).
    Time,
}

impl CsvTable {
    pub const ALL: [CsvTable; 4] = [CsvTable::Tasks, CsvTable::Resources, CsvTable::Assignments, CsvTable::Time];

    /// The menu label.
    pub fn label(self) -> &'static str {
        match self {
            CsvTable::Tasks => "Tasks",
            CsvTable::Resources => "Resources",
            CsvTable::Assignments => "Assignments",
            CsvTable::Time => "Logged Time",
        }
    }

    /// The name agents use (`export_csv`'s `table`).
    pub fn key(self) -> &'static str {
        match self {
            CsvTable::Tasks => "tasks",
            CsvTable::Resources => "resources",
            CsvTable::Assignments => "assignments",
            CsvTable::Time => "time",
        }
    }

    pub fn from_key(key: &str) -> Option<CsvTable> {
        CsvTable::ALL.into_iter().find(|t| t.key().eq_ignore_ascii_case(key.trim()))
    }

    /// Added to the plan's name to suggest a file name (`Plan resources.csv`); tasks add nothing.
    pub fn file_suffix(self) -> &'static str {
        match self {
            CsvTable::Tasks => "",
            CsvTable::Resources => " resources",
            CsvTable::Assignments => " assignments",
            CsvTable::Time => " time",
        }
    }

    /// How many rows the table has in this plan.
    pub fn rows(self, p: &Project) -> usize {
        match self {
            CsvTable::Tasks => p.tasks.len(),
            CsvTable::Resources => p.resources.len(),
            CsvTable::Assignments => p.assignments.len(),
            CsvTable::Time => p.time_entries.len(),
        }
    }
}

/// One of the plan's tables as CSV text (see [`to_csv`]).
pub fn table_to_csv(p: &Project, table: CsvTable) -> String {
    match table {
        CsvTable::Tasks => to_csv(p),
        CsvTable::Resources => resources_csv(p),
        CsvTable::Assignments => assignments_csv(p),
        CsvTable::Time => time_csv(p),
    }
}

/// The fixed columns, before the plan's custom fields.
const HEADINGS: &str =
    "ID,Unique ID,WBS,Outline Level,Name,Milestone,Duration,Start,Finish,% Complete,Work (h),Cost,Predecessors,Resources,Critical";

/// A cell, quoted. A cell a spreadsheet would run as a formula (starting with `=`, `+`, `-` or
/// `@`, or a tab or line break, also after leading spaces, and the full-width forms) gets an
/// apostrophe in front, so it stays text.
fn quote(v: &str) -> String {
    let risky = v.starts_with(['\t', '\r', '\n']) || v.trim_start().starts_with(['=', '+', '-', '@', '＝', '＋', '－', '＠']);
    let guarded = if risky { format!("'{v}") } else { v.to_string() };
    format!("\"{}\"", guarded.replace('"', "\"\""))
}

fn iso(dt: Option<DateTime>) -> String {
    dt.map(|d| d.format("%Y-%m-%d %H:%M").to_string()).unwrap_or_default()
}

/// The task list as CSV text: a byte order mark (so Excel reads accented names correctly), a
/// heading row, and a row per task.
pub fn to_csv(p: &Project) -> String {
    let mut s = format!("\u{feff}{HEADINGS}");
    for f in &p.custom_fields {
        s.push(',');
        s.push_str(&quote(&f.name));
    }
    s.push('\n');
    for (i, t) in p.tasks.iter().enumerate() {
        s += &format!(
            "{},{},{},{},{},{},{},{},{},{:.0},{:.2},{:.2},{},{},{}",
            i.saturating_add(1),
            t.id.0,
            quote(&p.wbs(i)),
            t.outline_level,
            quote(&t.name),
            t.is_milestone(),
            quote(&p.format_task_duration(t)),
            iso(t.sched.start),
            iso(t.sched.finish),
            t.sched.percent_complete,
            t.sched.work as f64 / 60.0,
            t.sched.cost,
            quote(&p.format_predecessors(t.id)),
            quote(&p.format_resource_names(t.id)),
            t.sched.critical
        );
        for f in &p.custom_fields {
            let v = t.custom.get(&f.id).map(|v| p.custom_value_text(f.id, v)).unwrap_or_default();
            s.push(',');
            s.push_str(&quote(&v));
        }
        s.push('\n');
    }
    s
}

/// Hours with two decimals, from working minutes.
fn hours(minutes: i64) -> String {
    format!("{:.2}", minutes as f64 / 60.0)
}

/// A number with up to two decimals, or empty when it isn't finite.
fn number(v: f64) -> String {
    if v.is_finite() { format!("{v:.2}") } else { String::new() }
}

/// Minutes logged on the timesheet per (task, resource).
fn logged(p: &Project) -> HashMap<(TaskId, ResourceId), i64> {
    let mut m: HashMap<(TaskId, ResourceId), i64> = HashMap::new();
    for e in &p.time_entries {
        let slot = m.entry((e.task, e.resource)).or_default();
        *slot = slot.saturating_add(e.minutes);
    }
    m
}

/// Each task with its row number (the ID column of the task export), by id: one lookup per row
/// instead of a search, so a plan at the size limits exports in linear time.
fn task_rows(p: &Project) -> HashMap<TaskId, (usize, &Task)> {
    p.tasks.iter().enumerate().map(|(i, t)| (t.id, (i.saturating_add(1), t))).collect()
}

/// The resources by id.
fn resources_by_id(p: &Project) -> HashMap<ResourceId, &Resource> {
    p.resources.iter().map(|r| (r.id, r)).collect()
}

const RESOURCE_HEADINGS: &str = "ID,Unique ID,Name,Type,Category,Initials,Group,Email,Material Label,Max Units (%),Std. Rate,Ovt. Rate,Cost/Use,Calendar,Work (h),Actual Work (h),Cost,Overallocated,Notes";

/// The resource sheet as CSV, with each resource's work, actual work and cost over its
/// assignments.
fn resources_csv(p: &Project) -> String {
    let logged = logged(p);
    let over: HashSet<ResourceId> = crate::workload::overallocations(p).into_iter().map(|(r, _)| r).collect();
    let resources = resources_by_id(p);
    // (work, actual work, cost) per resource.
    let mut totals: HashMap<ResourceId, (i64, i64, f64)> = HashMap::new();
    for a in &p.assignments {
        let Some(r) = resources.get(&a.resource) else {
            continue;
        };
        let e = totals.entry(a.resource).or_default();
        e.0 = e.0.saturating_add(a.work);
        e.1 = e.1.saturating_add(a.actual_work).saturating_add(logged.get(&(a.task, a.resource)).copied().unwrap_or(0));
        e.2 += a.cost_with(r);
    }
    let mut s = format!("\u{feff}{RESOURCE_HEADINGS}\n");
    for (i, r) in p.resources.iter().enumerate() {
        let (work, actual, cost) = totals.get(&r.id).copied().unwrap_or_default();
        let kind = match r.kind {
            ResourceType::Work => "Work",
            ResourceType::Material => "Material",
            ResourceType::Cost => "Cost",
        };
        let calendar = r.calendar.and_then(|c| p.calendar(c)).map(|c| c.name.as_str()).unwrap_or("");
        let is_work = r.kind == ResourceType::Work;
        s += &format!(
            "{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{}\n",
            i.saturating_add(1),
            r.id.0,
            quote(&r.name),
            kind,
            quote(r.category.label()),
            quote(&r.initials),
            quote(&r.group),
            quote(&r.email),
            quote(&r.material_label),
            if is_work { number(r.max_units * 100.0) } else { String::new() },
            if r.kind == ResourceType::Cost { String::new() } else { number(r.standard_rate) },
            if is_work { number(r.overtime_rate) } else { String::new() },
            number(r.cost_per_use),
            quote(calendar),
            hours(work),
            hours(actual),
            number(cost),
            over.contains(&r.id),
            quote(&r.notes),
        );
    }
    s
}

const ASSIGNMENT_HEADINGS: &str =
    "Task ID,Task Unique ID,Task,Resource Unique ID,Resource,Units,Work (h),Actual Work (h),Cost,Contour,Start,Finish";

/// Who works on what: a row per assignment, in task order, with the task's dates.
fn assignments_csv(p: &Project) -> String {
    let logged = logged(p);
    let rows = task_rows(p);
    let resources = resources_by_id(p);
    let mut list: Vec<_> = p.assignments.iter().collect();
    list.sort_by_key(|a| rows.get(&a.task).map_or(usize::MAX, |r| r.0));
    let mut s = format!("\u{feff}{ASSIGNMENT_HEADINGS}\n");
    for a in list {
        let task = rows.get(&a.task).map(|r| r.1);
        let r = resources.get(&a.resource).copied();
        let units = match r.map(|r| r.kind) {
            Some(ResourceType::Work) | None => {
                if a.units.is_finite() {
                    format!("{:.0}%", a.units * 100.0)
                } else {
                    String::new()
                }
            }
            Some(ResourceType::Material) => number(a.units),
            Some(ResourceType::Cost) => String::new(),
        };
        let actual = a.actual_work.saturating_add(logged.get(&(a.task, a.resource)).copied().unwrap_or(0));
        s += &format!(
            "{},{},{},{},{},{},{},{},{},{},{},{}\n",
            rows.get(&a.task).map(|r| r.0.to_string()).unwrap_or_default(),
            a.task.0,
            quote(task.map(|t| t.name.as_str()).unwrap_or("")),
            a.resource.0,
            quote(r.map(|r| r.name.as_str()).unwrap_or("")),
            units,
            hours(a.work),
            hours(actual),
            r.map(|r| number(a.cost_with(r))).unwrap_or_default(),
            a.contour.label(),
            iso(task.and_then(|t| t.sched.start)),
            iso(task.and_then(|t| t.sched.finish)),
        );
    }
    s
}

const TIME_HEADINGS: &str = "Date,Task ID,Task Unique ID,Task,Resource Unique ID,Resource,Hours,Note";

/// The timesheet as CSV: a row per logged entry, by date.
fn time_csv(p: &Project) -> String {
    let rows = task_rows(p);
    let resources = resources_by_id(p);
    let mut list: Vec<_> = p.time_entries.iter().collect();
    list.sort_by_key(|e| (e.date, rows.get(&e.task).map_or(usize::MAX, |r| r.0)));
    let mut s = format!("\u{feff}{TIME_HEADINGS}\n");
    for e in list {
        s += &format!(
            "{},{},{},{},{},{},{},{}\n",
            e.date.format("%Y-%m-%d"),
            rows.get(&e.task).map(|r| r.0.to_string()).unwrap_or_default(),
            e.task.0,
            quote(rows.get(&e.task).map(|r| r.1.name.as_str()).unwrap_or("")),
            e.resource.0,
            quote(resources.get(&e.resource).map(|r| r.name.as_str()).unwrap_or("")),
            hours(e.minutes),
            quote(&e.note),
        );
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formulas_stay_text() {
        assert_eq!(quote("=1+1"), "\"'=1+1\"");
        assert_eq!(quote("  @SUM(A1)"), "\"'  @SUM(A1)\"");
        assert_eq!(quote("Say \"hi\""), "\"Say \"\"hi\"\"\"");
        assert_eq!(quote("Plain"), "\"Plain\"");
    }
}
