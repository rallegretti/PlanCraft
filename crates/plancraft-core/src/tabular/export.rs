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
    write(&build(p, table), None)
}

/// The headings of one of the plan's tables, in the order they are written (the task list's
/// custom fields included).
pub fn table_columns(p: &Project, table: CsvTable) -> Vec<String> {
    match table {
        CsvTable::Tasks => HEADINGS.split(',').map(str::to_string).chain(p.custom_fields.iter().map(|f| f.name.clone())).collect(),
        CsvTable::Resources => RESOURCE_HEADINGS.split(',').map(str::to_string).collect(),
        CsvTable::Assignments => ASSIGNMENT_HEADINGS.split(',').map(str::to_string).collect(),
        CsvTable::Time => TIME_HEADINGS.split(',').map(str::to_string).collect(),
    }
}

/// The most column names one export takes.
const MAX_COLUMN_NAMES: usize = 1_000;

/// One of the plan's tables as CSV text with only the `columns` named, written in the table's
/// own order. A name is a heading of [`table_columns`]: as written, else in any case and without
/// spaces at its ends. A name two columns share (a custom field named like a fixed column) is
/// refused, as it cannot tell which one is meant.
pub fn table_to_csv_columns(p: &Project, table: CsvTable, columns: &[String]) -> Result<String, String> {
    if columns.len() > MAX_COLUMN_NAMES {
        return Err(format!("name at most {MAX_COLUMN_NAMES} columns"));
    }
    let all = table_columns(p, table);
    let mut keep = vec![false; all.len()];
    for c in columns {
        let at = |same: &dyn Fn(&str) -> bool| -> Vec<usize> { all.iter().enumerate().filter(|(_, h)| same(h)).map(|(i, _)| i).collect() };
        let mut found = at(&|h| h == c);
        if found.is_empty() {
            found = at(&|h| h.trim().eq_ignore_ascii_case(c.trim()));
        }
        match found.as_slice() {
            [i] => {
                if let Some(k) = keep.get_mut(*i) {
                    *k = true;
                }
            }
            [] => return Err(format!("the {} table has no column '{}'; its columns are: {}", table.key(), c.trim(), all.join(", "))),
            _ => {
                return Err(format!(
                    "{} columns of the {} table are named '{}' (a custom field named like another column): rename the custom field to \
                     choose one of them",
                    found.len(),
                    table.key(),
                    c.trim()
                ));
            }
        }
    }
    table_to_csv_kept(p, table, &keep)
}

/// One of the plan's tables as CSV text with the columns `keep` marks, one mark per heading of
/// [`table_columns`] (a list made for another set of columns is refused).
pub fn table_to_csv_kept(p: &Project, table: CsvTable, keep: &[bool]) -> Result<String, String> {
    if keep.len() != table_columns(p, table).len() {
        return Err("the table's columns changed (a custom field was added or removed): choose them again".into());
    }
    if !keep.contains(&true) {
        return Err("choose at least one column".into());
    }
    Ok(write(&build(p, table), Some(keep)))
}

/// A cell: text, written quoted (and kept from running as a formula), or a plain value (a
/// number, a date, true or false), written as it is.
enum Cell {
    Text(String),
    Plain(String),
}

fn text(v: impl Into<String>) -> Cell {
    Cell::Text(v.into())
}

fn plain(v: impl ToString) -> Cell {
    Cell::Plain(v.to_string())
}

/// A table: its heading row and its rows, cell by cell.
struct Table {
    headings: Vec<Cell>,
    rows: Vec<Vec<Cell>>,
}

fn build(p: &Project, table: CsvTable) -> Table {
    match table {
        CsvTable::Tasks => tasks_table(p),
        CsvTable::Resources => resources_table(p),
        CsvTable::Assignments => assignments_table(p),
        CsvTable::Time => time_table(p),
    }
}

/// The table as CSV: a byte order mark (so Excel reads accented names correctly), the heading
/// row and a row per line, with only the columns `keep` marks (all without it).
fn write(t: &Table, keep: Option<&[bool]>) -> String {
    let line = |cells: &[Cell]| -> String {
        let kept = cells.iter().enumerate().filter(|(i, _)| keep.is_none_or(|k| k.get(*i).copied().unwrap_or(false)));
        let mut out: Vec<String> = Vec::with_capacity(cells.len());
        for (_, c) in kept {
            out.push(match c {
                Cell::Text(v) => quote(v),
                Cell::Plain(v) => v.clone(),
            });
        }
        out.join(",")
    };
    let mut s = format!("\u{feff}{}\n", line(&t.headings));
    for row in &t.rows {
        s.push_str(&line(row));
        s.push('\n');
    }
    s
}

/// Plain headings, from a constant's list.
fn headings(list: &str) -> Vec<Cell> {
    list.split(',').map(plain).collect()
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
    table_to_csv(p, CsvTable::Tasks)
}

fn tasks_table(p: &Project) -> Table {
    let mut heads = headings(HEADINGS);
    heads.extend(p.custom_fields.iter().map(|f| text(&f.name)));
    let rows = p
        .tasks
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let mut row = vec![
                plain(i.saturating_add(1)),
                plain(t.id.0),
                text(p.wbs(i)),
                plain(t.outline_level),
                text(&t.name),
                plain(t.is_milestone()),
                text(p.format_task_duration(t)),
                plain(iso(t.sched.start)),
                plain(iso(t.sched.finish)),
                plain(format!("{:.0}", t.sched.percent_complete)),
                plain(format!("{:.2}", t.sched.work as f64 / 60.0)),
                plain(format!("{:.2}", t.sched.cost)),
                text(p.format_predecessors(t.id)),
                text(p.format_resource_names(t.id)),
                plain(t.sched.critical),
            ];
            row.extend(p.custom_fields.iter().map(|f| text(t.custom.get(&f.id).map(|v| p.custom_value_text(f.id, v)).unwrap_or_default())));
            row
        })
        .collect();
    Table { headings: heads, rows }
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

/// The resource sheet, with each resource's work, actual work and cost over its assignments.
fn resources_table(p: &Project) -> Table {
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
    let rows = p
        .resources
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let (work, actual, cost) = totals.get(&r.id).copied().unwrap_or_default();
            let kind = match r.kind {
                ResourceType::Work => "Work",
                ResourceType::Material => "Material",
                ResourceType::Cost => "Cost",
            };
            let calendar = r.calendar.and_then(|c| p.calendar(c)).map(|c| c.name.as_str()).unwrap_or("");
            let is_work = r.kind == ResourceType::Work;
            vec![
                plain(i.saturating_add(1)),
                plain(r.id.0),
                text(&r.name),
                plain(kind),
                text(r.category.label()),
                text(&r.initials),
                text(&r.group),
                text(&r.email),
                text(&r.material_label),
                plain(if is_work { number(r.max_units * 100.0) } else { String::new() }),
                plain(if r.kind == ResourceType::Cost { String::new() } else { number(r.standard_rate) }),
                plain(if is_work { number(r.overtime_rate) } else { String::new() }),
                plain(number(r.cost_per_use)),
                text(calendar),
                plain(hours(work)),
                plain(hours(actual)),
                plain(number(cost)),
                plain(over.contains(&r.id)),
                text(&r.notes),
            ]
        })
        .collect();
    Table { headings: headings(RESOURCE_HEADINGS), rows }
}

const ASSIGNMENT_HEADINGS: &str =
    "Task ID,Task Unique ID,Task,Resource Unique ID,Resource,Units,Work (h),Actual Work (h),Cost,Contour,Start,Finish";

/// Who works on what: a row per assignment, in task order, with the task's dates.
fn assignments_table(p: &Project) -> Table {
    let logged = logged(p);
    let task_rows = task_rows(p);
    let resources = resources_by_id(p);
    let mut list: Vec<_> = p.assignments.iter().collect();
    list.sort_by_key(|a| task_rows.get(&a.task).map_or(usize::MAX, |r| r.0));
    let rows = list
        .into_iter()
        .map(|a| {
            let task = task_rows.get(&a.task).map(|r| r.1);
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
            vec![
                plain(task_rows.get(&a.task).map(|r| r.0.to_string()).unwrap_or_default()),
                plain(a.task.0),
                text(task.map(|t| t.name.as_str()).unwrap_or("")),
                plain(a.resource.0),
                text(r.map(|r| r.name.as_str()).unwrap_or("")),
                plain(units),
                plain(hours(a.work)),
                plain(hours(actual)),
                plain(r.map(|r| number(a.cost_with(r))).unwrap_or_default()),
                plain(a.contour.label()),
                plain(iso(task.and_then(|t| t.sched.start))),
                plain(iso(task.and_then(|t| t.sched.finish))),
            ]
        })
        .collect();
    Table { headings: headings(ASSIGNMENT_HEADINGS), rows }
}

const TIME_HEADINGS: &str = "Date,Task ID,Task Unique ID,Task,Resource Unique ID,Resource,Hours,Note";

/// The timesheet: a row per logged entry, by date.
fn time_table(p: &Project) -> Table {
    let task_rows = task_rows(p);
    let resources = resources_by_id(p);
    let mut list: Vec<_> = p.time_entries.iter().collect();
    list.sort_by_key(|e| (e.date, task_rows.get(&e.task).map_or(usize::MAX, |r| r.0)));
    let rows = list
        .into_iter()
        .map(|e| {
            vec![
                plain(e.date.format("%Y-%m-%d")),
                plain(task_rows.get(&e.task).map(|r| r.0.to_string()).unwrap_or_default()),
                plain(e.task.0),
                text(task_rows.get(&e.task).map(|r| r.1.name.as_str()).unwrap_or("")),
                plain(e.resource.0),
                text(resources.get(&e.resource).map(|r| r.name.as_str()).unwrap_or("")),
                plain(hours(e.minutes)),
                text(&e.note),
            ]
        })
        .collect();
    Table { headings: headings(TIME_HEADINGS), rows }
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
