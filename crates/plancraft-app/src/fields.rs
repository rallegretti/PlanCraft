//! Table fields: the built-in task columns and the project's custom fields behind one type, with
//! stable keys (saved in the plan's table layouts), display, editing and sort keys.

use std::cmp::Ordering;

use chrono::NaiveDateTime;
use plancraft_core::custom::{self, CustomFieldKind, CustomValue};
use plancraft_core::prelude::*;

use crate::columns::{self, CellCtx, TaskCol, TaskTable};
use crate::format;
use crate::widgets::grid::{Cell, ColumnDef};

/// The pick-list entry that clears a custom value.
pub const NO_VALUE: &str = "(none)";

/// A task table column.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Field {
    Builtin(TaskCol),
    Custom(CustomFieldId),
}

/// Built-in columns and their keys (as saved in table layouts). Keys never change.
const BUILTIN: [(TaskCol, &str); 51] = [
    (TaskCol::Indicators, "indicators"),
    (TaskCol::Mode, "mode"),
    (TaskCol::Wbs, "wbs"),
    (TaskCol::Name, "name"),
    (TaskCol::Duration, "duration"),
    (TaskCol::Start, "start"),
    (TaskCol::Finish, "finish"),
    (TaskCol::Predecessors, "predecessors"),
    (TaskCol::Successors, "successors"),
    (TaskCol::Resources, "resources"),
    (TaskCol::PercentComplete, "percent_complete"),
    (TaskCol::Work, "work"),
    (TaskCol::ActualWork, "actual_work"),
    (TaskCol::RemainingWork, "remaining_work"),
    (TaskCol::Cost, "cost"),
    (TaskCol::FixedCost, "fixed_cost"),
    (TaskCol::BaselineStart, "baseline_start"),
    (TaskCol::BaselineFinish, "baseline_finish"),
    (TaskCol::BaselineWork, "baseline_work"),
    (TaskCol::BaselineCost, "baseline_cost"),
    (TaskCol::StartVariance, "start_variance"),
    (TaskCol::FinishVariance, "finish_variance"),
    (TaskCol::WorkVariance, "work_variance"),
    (TaskCol::CostVariance, "cost_variance"),
    (TaskCol::ActualStart, "actual_start"),
    (TaskCol::ActualFinish, "actual_finish"),
    (TaskCol::ActualDuration, "actual_duration"),
    (TaskCol::RemainingDuration, "remaining_duration"),
    (TaskCol::Stop, "stop"),
    (TaskCol::Resume, "resume"),
    (TaskCol::EarlyStart, "early_start"),
    (TaskCol::EarlyFinish, "early_finish"),
    (TaskCol::LateStart, "late_start"),
    (TaskCol::LateFinish, "late_finish"),
    (TaskCol::FreeSlack, "free_slack"),
    (TaskCol::TotalSlack, "total_slack"),
    (TaskCol::ConstraintType, "constraint_type"),
    (TaskCol::ConstraintDate, "constraint_date"),
    (TaskCol::Deadline, "deadline"),
    (TaskCol::Priority, "priority"),
    (TaskCol::TaskType, "task_type"),
    (TaskCol::EstimateMethod, "estimate_method"),
    (TaskCol::Points, "points"),
    (TaskCol::Optimistic, "optimistic"),
    (TaskCol::MostLikely, "most_likely"),
    (TaskCol::Pessimistic, "pessimistic"),
    (TaskCol::ExpectedWork, "expected_work"),
    (TaskCol::Deliverable, "deliverable"),
    (TaskCol::Milestones, "milestones"),
    (TaskCol::Notes, "notes"),
    (TaskCol::UniqueId, "unique_id"),
];

/// A value to sort or group by. Empty values sort after everything else.
#[derive(Clone, Debug, PartialEq)]
pub enum SortKey {
    Num(f64),
    Time(NaiveDateTime),
    Text(String),
    Missing,
}

impl SortKey {
    fn rank(&self) -> u8 {
        match self {
            SortKey::Num(_) => 0,
            SortKey::Time(_) => 1,
            SortKey::Text(_) => 2,
            SortKey::Missing => 3,
        }
    }

    /// Total order: numbers, then dates, then text (case-insensitive), then empty values.
    pub fn cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (SortKey::Num(a), SortKey::Num(b)) => a.total_cmp(b),
            (SortKey::Time(a), SortKey::Time(b)) => a.cmp(b),
            (SortKey::Text(a), SortKey::Text(b)) => a.cmp(b),
            _ => self.rank().cmp(&other.rank()),
        }
    }
}

impl Field {
    pub fn key(&self) -> String {
        match self {
            Field::Builtin(c) => BUILTIN.iter().find(|(b, _)| b == c).map_or("", |(_, k)| k).to_string(),
            Field::Custom(id) => custom::custom_column_key(*id),
        }
    }

    /// The field a key names in this project (`None` for unknown keys or deleted custom fields).
    pub fn from_key(p: &Project, key: &str) -> Option<Field> {
        if let Some(id) = custom::custom_column_field(key) {
            return p.custom_field(id).map(|_| Field::Custom(id));
        }
        BUILTIN.iter().find(|(_, k)| *k == key).map(|(c, _)| Field::Builtin(*c))
    }

    pub fn title(&self, p: &Project) -> String {
        match self {
            Field::Builtin(c) => c.def(p).title,
            Field::Custom(id) => p.custom_field(*id).map_or_else(|| "?".into(), |f| f.name.clone()),
        }
    }

    pub fn def(&self, p: &Project) -> ColumnDef {
        match self {
            Field::Builtin(c) => c.def(p),
            Field::Custom(id) => {
                let Some(f) = p.custom_field(*id) else { return ColumnDef::new("?", 80.0).read_only() };
                let def = ColumnDef::new(f.name.clone(), 110.0);
                match f.kind {
                    CustomFieldKind::Text if !f.choices.is_empty() => {
                        let mut options: Vec<&str> = f.choices.iter().map(String::as_str).collect();
                        options.push(NO_VALUE);
                        def.choice(&options)
                    }
                    CustomFieldKind::Text => def,
                    CustomFieldKind::Number | CustomFieldKind::Cost | CustomFieldKind::Duration => def.right(),
                    CustomFieldKind::Date => def,
                    CustomFieldKind::Flag => def.center().choice(&["Yes", "No"]),
                }
            }
        }
    }

    pub fn cell(&self, p: &Project, idx: usize, ctx: &CellCtx) -> Cell {
        match self {
            Field::Builtin(c) => columns::cell(*c, p, idx, ctx),
            Field::Custom(id) => {
                let Some(t) = p.tasks.get(idx) else { return Cell::default() };
                let mut cell = match t.custom.get(id) {
                    None => Cell::default(),
                    Some(v) => {
                        let text = custom_display(p, *id, v);
                        let edit_text = match v {
                            CustomValue::Date(d) => Some(format::iso_date(Some(*d))),
                            CustomValue::Number(n) => Some(format!("{n}")),
                            _ => None,
                        };
                        Cell { text, edit_text, ..Default::default() }
                    }
                };
                if t.sched.is_summary {
                    cell.bold = true;
                }
                if !t.active {
                    cell.color = Some(crate::theme::pal().TEXT_WEAK);
                }
                cell
            }
        }
    }

    /// Applies an edited value. Runs inside an undoable edit.
    pub fn commit(&self, p: &mut Project, id: TaskId, text: &str) -> Result<(), String> {
        match self {
            Field::Builtin(c) => columns::commit(*c, p, id, text),
            Field::Custom(field) => {
                let listed = p.custom_field(*field).is_some_and(|f| f.choices.iter().any(|c| c == NO_VALUE));
                let text = if text == NO_VALUE && !listed { "" } else { text };
                let value = p.parse_custom_value(*field, text).map_err(|e| e.to_string())?;
                p.set_custom_value(id, *field, value).map_err(|e| e.to_string())
            }
        }
    }

    /// The value tasks are sorted and grouped by.
    pub fn sort_key(&self, p: &Project, idx: usize) -> SortKey {
        let Some(t) = p.tasks.get(idx) else { return SortKey::Missing };
        let s = &t.sched;
        let num = |v: f64| SortKey::Num(v);
        let mins = |m: Minutes| SortKey::Num(m as f64);
        let time = |d: Option<NaiveDateTime>| d.map_or(SortKey::Missing, SortKey::Time);
        let Field::Builtin(col) = self else {
            let Field::Custom(id) = self else { return SortKey::Missing };
            return match t.custom.get(id) {
                None => SortKey::Missing,
                Some(CustomValue::Text(v)) => SortKey::Text(v.to_lowercase()),
                Some(CustomValue::Number(n)) => num(*n),
                Some(CustomValue::Date(d)) => SortKey::Time(*d),
                Some(CustomValue::Duration(m)) => mins(*m),
                Some(CustomValue::Flag(b)) => num(if *b { 0.0 } else { 1.0 }),
            };
        };
        use TaskCol::*;
        match col {
            Wbs => num(idx as f64),
            UniqueId => num(f64::from(t.id.0)),
            Duration => mins(s.duration),
            Start | EarlyStart => time(s.start),
            Finish | EarlyFinish => time(s.finish),
            LateStart => time(s.late_start),
            LateFinish => time(s.late_finish),
            ActualStart => time(t.actual_start),
            ActualFinish => time(t.actual_finish),
            ActualDuration => mins(t.actual_duration()),
            RemainingDuration => mins(t.remaining_duration()),
            Stop => time(s.stop),
            Resume => time(s.resume),
            BaselineStart => time(t.baseline.map(|b| b.start)),
            BaselineFinish => time(t.baseline.map(|b| b.finish)),
            ConstraintDate => time(t.constraint.date),
            Deadline => time(t.deadline),
            PercentComplete => num(f64::from(s.percent_complete)),
            Work => mins(s.work),
            ActualWork => mins(s.actual_work),
            RemainingWork => mins((s.work - s.actual_work).max(0)),
            Cost => num(s.cost),
            FixedCost => num(t.fixed_cost),
            BaselineWork => t.baseline.map_or(SortKey::Missing, |b| mins(b.work)),
            BaselineCost => t.baseline.map_or(SortKey::Missing, |b| num(b.cost)),
            WorkVariance => t.baseline.map_or(SortKey::Missing, |b| mins(s.work - b.work)),
            CostVariance => t.baseline.map_or(SortKey::Missing, |b| num(s.cost - b.cost)),
            FreeSlack => mins(s.free_slack),
            TotalSlack => mins(s.total_slack),
            Priority => num(f64::from(t.priority)),
            ExpectedWork => t.estimate.map_or(SortKey::Missing, |e| mins(e.expected_work(&p.settings))),
            _ => {
                let none = |_: TaskId| false;
                let text = columns::cell(*col, p, idx, &CellCtx { overallocated: &none }).text;
                if text.trim().is_empty() { SortKey::Missing } else { SortKey::Text(text.to_lowercase()) }
            }
        }
    }

    /// The label tasks are grouped under (empty values share the "No value" group).
    pub fn group_label(&self, p: &Project, idx: usize) -> String {
        let text = match self {
            Field::Custom(id) => p.tasks.get(idx).and_then(|t| t.custom.get(id)).map(|v| custom_display(p, *id, v)).unwrap_or_default(),
            Field::Builtin(col) => {
                let none = |_: TaskId| false;
                columns::cell(*col, p, idx, &CellCtx { overallocated: &none }).text
            }
        };
        if text.trim().is_empty() { "No value".to_string() } else { text }
    }
}

/// A custom value as shown in the table (dates and money in the app's formats).
pub fn custom_display(p: &Project, id: CustomFieldId, v: &CustomValue) -> String {
    let cost = p.custom_field(id).is_some_and(|f| f.kind == CustomFieldKind::Cost);
    match v {
        CustomValue::Number(n) if cost => format::money(p, *n),
        CustomValue::Date(d) => format::date(Some(*d)),
        other => p.custom_value_text(id, other),
    }
}

/// The columns a task table shows: the plan's saved layout, or the table's defaults.
pub fn table_fields(p: &Project, table: TaskTable) -> Vec<Field> {
    match p.table_layout(table.label()) {
        Some(layout) => layout.columns.iter().filter_map(|k| Field::from_key(p, k)).collect(),
        None => table.columns().into_iter().map(Field::Builtin).collect(),
    }
}

/// Every field a column can show: the built-in columns, then the custom fields.
pub fn all_fields(p: &Project) -> Vec<Field> {
    BUILTIN.iter().map(|(c, _)| Field::Builtin(*c)).chain(p.custom_fields.iter().map(|f| Field::Custom(f.id))).collect()
}

/// Saves a changed column list for `table` (keys of `fields`).
pub fn save_columns(p: &mut Project, table: TaskTable, fields: &[Field]) -> Result<(), String> {
    let keys: Vec<String> = fields.iter().map(Field::key).collect();
    p.set_table_columns(table.label(), &keys).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_round_trip_and_are_unique() {
        let p = plancraft_core::sample::demo_project(chrono::NaiveDate::from_ymd_opt(2026, 10, 19).unwrap());
        let fields = all_fields(&p);
        let keys: std::collections::HashSet<String> = fields.iter().map(Field::key).collect();
        assert_eq!(keys.len(), fields.len(), "every field has its own key");
        for f in &fields {
            assert_eq!(Field::from_key(&p, &f.key()), Some(*f));
        }
        assert!(Field::from_key(&p, "custom:999").is_none());
        assert!(Field::from_key(&p, "nonsense").is_none());
    }

    #[test]
    fn every_default_table_column_has_a_key() {
        for table in TaskTable::ALL {
            for col in table.columns() {
                assert!(!Field::Builtin(col).key().is_empty(), "{col:?} in {table:?} has no key");
            }
        }
    }

    #[test]
    fn sort_keys_order_numbers_dates_text_then_blanks() {
        let mut keys = [SortKey::Missing, SortKey::Text("b".into()), SortKey::Num(2.0), SortKey::Text("a".into()), SortKey::Num(-1.0)];
        keys.sort_by(SortKey::cmp);
        assert_eq!(keys, [SortKey::Num(-1.0), SortKey::Num(2.0), SortKey::Text("a".into()), SortKey::Text("b".into()), SortKey::Missing]);
    }
}
