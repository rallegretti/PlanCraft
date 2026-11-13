//! Custom fields and saved table layouts.
//!
//! A custom field is a task field the user defines: text (optionally limited to a pick-list),
//! number, cost, date, duration or a yes/no flag, in the spirit of Microsoft Project's Text,
//! Number, Cost, Date, Duration and Flag fields. Definitions live in [`Project::custom_fields`];
//! each task keeps its values in [`Task::custom`], keyed by field id.
//!
//! A table layout ([`Project::tables`]) records which columns a task table shows, so a plan opens
//! with the columns its author chose. Column keys are opaque to the core except `custom:<id>`,
//! which refers to a custom field and is dropped when that field is deleted.

use std::collections::HashSet;

use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};

use crate::duration::Minutes;
use crate::ids::{CustomFieldId, TaskId};
use crate::limits::{self, MAX_MINUTES, MAX_NAME_CHARS};
use crate::model::Project;
use crate::ops::{PlanError, PlanResult};

/// Custom fields per project.
pub const MAX_CUSTOM_FIELDS: usize = 200;
/// Entries in a text field's pick-list.
pub const MAX_CHOICES: usize = 500;
/// Largest number stored in a number or cost field (either sign).
pub const MAX_CUSTOM_NUMBER: f64 = 1e15;
/// Saved table layouts per project, and columns per table.
pub const MAX_TABLES: usize = 100;
pub const MAX_TABLE_COLUMNS: usize = 200;
/// Characters in a column key.
pub const MAX_COLUMN_KEY_CHARS: usize = 64;

/// Prefix of the column key that shows a custom field (`custom:<id>`).
pub const CUSTOM_COLUMN_PREFIX: &str = "custom:";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum CustomFieldKind {
    #[default]
    Text,
    Number,
    Cost,
    Date,
    Duration,
    Flag,
}

impl CustomFieldKind {
    pub const ALL: [CustomFieldKind; 6] = [
        CustomFieldKind::Text,
        CustomFieldKind::Number,
        CustomFieldKind::Cost,
        CustomFieldKind::Date,
        CustomFieldKind::Duration,
        CustomFieldKind::Flag,
    ];

    pub fn label(self) -> &'static str {
        match self {
            CustomFieldKind::Text => "Text",
            CustomFieldKind::Number => "Number",
            CustomFieldKind::Cost => "Cost",
            CustomFieldKind::Date => "Date",
            CustomFieldKind::Duration => "Duration",
            CustomFieldKind::Flag => "Flag",
        }
    }

    /// Parses a kind name ("text", "Number", …), ignoring case.
    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.label().eq_ignore_ascii_case(text.trim()))
    }
}

/// Definition of a custom field.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CustomFieldDef {
    pub id: CustomFieldId,
    pub name: String,
    pub kind: CustomFieldKind,
    /// Allowed values of a text field (empty: any text).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub choices: Vec<String>,
}

/// A task's value of one custom field.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CustomValue {
    Text(String),
    /// Number and cost fields.
    Number(f64),
    Date(NaiveDateTime),
    Duration(Minutes),
    Flag(bool),
}

impl CustomValue {
    /// Whether this value belongs in a field of `kind`.
    pub fn fits(&self, kind: CustomFieldKind) -> bool {
        matches!(
            (self, kind),
            (CustomValue::Text(_), CustomFieldKind::Text)
                | (CustomValue::Number(_), CustomFieldKind::Number | CustomFieldKind::Cost)
                | (CustomValue::Date(_), CustomFieldKind::Date)
                | (CustomValue::Duration(_), CustomFieldKind::Duration)
                | (CustomValue::Flag(_), CustomFieldKind::Flag)
        )
    }
}

/// The columns a task table shows (by column key), saved with the plan.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TableLayout {
    /// Table name, e.g. "Entry".
    pub name: String,
    pub columns: Vec<String>,
}

/// Whether two custom field names are the same, ignoring case (the same rule a file is checked
/// with when it is opened, so a field accepted here is never refused on load).
fn same_name(a: &str, b: &str) -> bool {
    a.to_lowercase() == b.to_lowercase()
}

/// The column key that shows custom field `id`.
pub fn custom_column_key(id: CustomFieldId) -> String {
    format!("{CUSTOM_COLUMN_PREFIX}{}", id.0)
}

/// The custom field a column key refers to, if it is a `custom:<id>` key.
pub fn custom_column_field(key: &str) -> Option<CustomFieldId> {
    key.strip_prefix(CUSTOM_COLUMN_PREFIX).and_then(|n| n.parse::<u32>().ok()).map(CustomFieldId)
}

fn invalid<T>(msg: impl Into<String>) -> PlanResult<T> {
    Err(PlanError::Invalid(msg.into()))
}

fn clean_name(name: &str, what: &str) -> PlanResult<String> {
    let name = name.trim();
    if name.is_empty() {
        return invalid(format!("{what} cannot be empty"));
    }
    if name.chars().count() > MAX_NAME_CHARS {
        return invalid(format!("{what} is longer than {MAX_NAME_CHARS} characters"));
    }
    Ok(name.to_string())
}

/// Trims, drops blanks and duplicates (ignoring case) and checks the pick-list limits.
fn clean_choices(choices: &[String]) -> PlanResult<Vec<String>> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for c in choices {
        let c = c.trim();
        if c.is_empty() || !seen.insert(c.to_lowercase()) {
            continue;
        }
        if c.chars().count() > MAX_NAME_CHARS {
            return invalid(format!("a choice is longer than {MAX_NAME_CHARS} characters"));
        }
        out.push(c.to_string());
    }
    if out.len() > MAX_CHOICES {
        return invalid(format!("a pick-list can have at most {MAX_CHOICES} entries"));
    }
    Ok(out)
}

impl Project {
    pub fn custom_field(&self, id: CustomFieldId) -> Option<&CustomFieldDef> {
        self.custom_fields.iter().find(|f| f.id == id)
    }

    /// Finds a custom field by name (ignoring case) or by id written as a number.
    pub fn find_custom_field(&self, key: &str) -> Option<&CustomFieldDef> {
        let key = key.trim();
        self.custom_fields
            .iter()
            .find(|f| same_name(&f.name, key))
            .or_else(|| key.parse::<u32>().ok().and_then(|n| self.custom_field(CustomFieldId(n))))
    }

    fn require_custom_field(&self, id: CustomFieldId) -> PlanResult<&CustomFieldDef> {
        self.custom_field(id).ok_or_else(|| PlanError::Invalid(format!("custom field {} not found", id.0)))
    }

    fn check_unique_field_name(&self, name: &str, except: Option<CustomFieldId>) -> PlanResult<()> {
        if self.custom_fields.iter().any(|f| Some(f.id) != except && same_name(&f.name, name)) {
            return invalid(format!("a custom field named '{name}' already exists"));
        }
        Ok(())
    }

    /// Defines a new custom field. `choices` (a pick-list) is allowed for text fields only.
    pub fn add_custom_field(&mut self, name: &str, kind: CustomFieldKind, choices: &[String]) -> PlanResult<CustomFieldId> {
        let name = clean_name(name, "a custom field name")?;
        self.check_unique_field_name(&name, None)?;
        if self.custom_fields.len() >= MAX_CUSTOM_FIELDS {
            return invalid(format!("a project can have at most {MAX_CUSTOM_FIELDS} custom fields"));
        }
        let choices = clean_choices(choices)?;
        if !choices.is_empty() && kind != CustomFieldKind::Text {
            return invalid("only text fields can have a pick-list");
        }
        let id = self.ids.next_custom_field();
        self.custom_fields.push(CustomFieldDef { id, name, kind, choices });
        Ok(id)
    }

    pub fn rename_custom_field(&mut self, id: CustomFieldId, name: &str) -> PlanResult<()> {
        let name = clean_name(name, "a custom field name")?;
        self.require_custom_field(id)?;
        self.check_unique_field_name(&name, Some(id))?;
        if let Some(f) = self.custom_fields.iter_mut().find(|f| f.id == id) {
            f.name = name;
        }
        Ok(())
    }

    /// Replaces a text field's pick-list (empty: any text). Values already set are kept.
    pub fn set_custom_field_choices(&mut self, id: CustomFieldId, choices: &[String]) -> PlanResult<()> {
        let choices = clean_choices(choices)?;
        if !choices.is_empty() && self.require_custom_field(id)?.kind != CustomFieldKind::Text {
            return invalid("only text fields can have a pick-list");
        }
        if let Some(f) = self.custom_fields.iter_mut().find(|f| f.id == id) {
            f.choices = choices;
        }
        Ok(())
    }

    /// Deletes a custom field, its values on every task and its columns in saved tables.
    pub fn delete_custom_field(&mut self, id: CustomFieldId) -> PlanResult<()> {
        self.require_custom_field(id)?;
        self.custom_fields.retain(|f| f.id != id);
        for t in &mut self.tasks {
            t.custom.remove(&id);
        }
        let key = custom_column_key(id);
        for table in &mut self.tables {
            table.columns.retain(|c| *c != key);
        }
        Ok(())
    }

    /// Sets (or with `None`, clears) a task's value of a custom field.
    pub fn set_custom_value(&mut self, task: TaskId, field: CustomFieldId, value: Option<CustomValue>) -> PlanResult<()> {
        let def = self.require_custom_field(field)?.clone();
        let idx = self.task_index(task).ok_or(PlanError::TaskNotFound(task))?;
        let value = match value {
            None => None,
            Some(v) => Some(self.check_custom_value(&def, v)?),
        };
        let t = self.tasks.get_mut(idx).ok_or(PlanError::TaskNotFound(task))?;
        match value {
            Some(v) => {
                t.custom.insert(field, v);
            }
            None => {
                t.custom.remove(&field);
            }
        }
        Ok(())
    }

    /// Validates a value for a field, normalizing a pick-list entry to its listed spelling.
    fn check_custom_value(&self, def: &CustomFieldDef, v: CustomValue) -> PlanResult<CustomValue> {
        if !v.fits(def.kind) {
            return invalid(format!("'{}' is a {} field", def.name, def.kind.label().to_lowercase()));
        }
        Ok(match v {
            CustomValue::Text(s) => {
                let s = s.trim().to_string();
                if s.chars().count() > MAX_NAME_CHARS {
                    return invalid(format!("text is longer than {MAX_NAME_CHARS} characters"));
                }
                if def.choices.is_empty() {
                    CustomValue::Text(s)
                } else {
                    match def.choices.iter().find(|c| c.eq_ignore_ascii_case(&s)) {
                        Some(c) => CustomValue::Text(c.clone()),
                        None => return invalid(format!("'{s}' is not one of the choices of '{}': {}", def.name, def.choices.join(", "))),
                    }
                }
            }
            CustomValue::Number(n) => {
                if !n.is_finite() || n.abs() > MAX_CUSTOM_NUMBER {
                    return invalid("the number is out of range");
                }
                CustomValue::Number(n)
            }
            CustomValue::Date(d) => {
                if limits::clamp_datetime(d) != d {
                    return invalid("dates must be between 1900 and 2300");
                }
                CustomValue::Date(d)
            }
            CustomValue::Duration(m) => {
                if !(0..=MAX_MINUTES).contains(&m) {
                    return invalid("the duration is out of range");
                }
                CustomValue::Duration(m)
            }
            CustomValue::Flag(b) => CustomValue::Flag(b),
        })
    }

    /// Parses text typed for a field (empty clears it): numbers may carry the currency symbol and
    /// thousands separators, dates use the project's date input, durations its duration input,
    /// and flags accept yes/no, true/false, 1/0.
    pub fn parse_custom_value(&self, field: CustomFieldId, text: &str) -> PlanResult<Option<CustomValue>> {
        let def = self.require_custom_field(field)?;
        let t = text.trim();
        if t.is_empty() {
            return Ok(None);
        }
        let bad = || PlanError::Invalid(format!("'{t}' is not a valid {} for '{}'", def.kind.label().to_lowercase(), def.name));
        let v = match def.kind {
            CustomFieldKind::Text => CustomValue::Text(t.to_string()),
            CustomFieldKind::Number | CustomFieldKind::Cost => {
                let digits: String = t.trim_start_matches(self.settings.currency.as_str()).chars().filter(|c| *c != ',').collect();
                CustomValue::Number(digits.trim().parse::<f64>().map_err(|_| bad())?)
            }
            CustomFieldKind::Date => CustomValue::Date(self.parse_datetime(t, false).ok_or_else(bad)?),
            CustomFieldKind::Duration => CustomValue::Duration(self.parse_duration(t).ok_or_else(bad)?.0),
            CustomFieldKind::Flag => CustomValue::Flag(match t.to_lowercase().as_str() {
                "yes" | "y" | "true" | "1" | "on" => true,
                "no" | "n" | "false" | "0" | "off" => false,
                _ => return Err(bad()),
            }),
        };
        Ok(Some(v))
    }

    /// A value as plain text (ISO dates, project duration units), for exports and agents.
    pub fn custom_value_text(&self, field: CustomFieldId, value: &CustomValue) -> String {
        let cost = self.custom_field(field).is_some_and(|f| f.kind == CustomFieldKind::Cost);
        match value {
            CustomValue::Text(s) => s.clone(),
            CustomValue::Number(n) if cost => format!("{}{n:.2}", self.settings.currency),
            CustomValue::Number(n) => format!("{n}"),
            CustomValue::Date(d) => d.format("%Y-%m-%d %H:%M").to_string(),
            CustomValue::Duration(m) => self.format_duration(*m, false),
            CustomValue::Flag(b) => if *b { "Yes" } else { "No" }.to_string(),
        }
    }

    /// The saved column list of a table, if the plan has one.
    pub fn table_layout(&self, name: &str) -> Option<&TableLayout> {
        self.tables.iter().find(|t| t.name.eq_ignore_ascii_case(name))
    }

    /// Saves the columns a table shows (an empty list removes the saved layout, so the table
    /// falls back to its default columns). Keys must be short; `custom:<id>` keys must name an
    /// existing custom field.
    pub fn set_table_columns(&mut self, name: &str, columns: &[String]) -> PlanResult<()> {
        let name = clean_name(name, "a table name")?;
        if columns.len() > MAX_TABLE_COLUMNS {
            return invalid(format!("a table can have at most {MAX_TABLE_COLUMNS} columns"));
        }
        for c in columns {
            if c.is_empty() || c.chars().count() > MAX_COLUMN_KEY_CHARS {
                return invalid("a column key is empty or too long");
            }
            if let Some(id) = custom_column_field(c)
                && self.custom_field(id).is_none()
            {
                return invalid(format!("custom field {} not found", id.0));
            }
        }
        self.tables.retain(|t| !t.name.eq_ignore_ascii_case(&name));
        if !columns.is_empty() {
            if self.tables.len() >= MAX_TABLES {
                return invalid(format!("a project can save at most {MAX_TABLES} tables"));
            }
            self.tables.push(TableLayout { name, columns: columns.to_vec() });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn project() -> Project {
        Project::new("Custom", NaiveDate::from_ymd_opt(2026, 10, 19).unwrap())
    }

    #[test]
    fn fields_values_and_pick_lists() {
        let mut p = project();
        let t = p.add_task("Task", None);
        let status = p
            .add_custom_field("Status", CustomFieldKind::Text, &["To do".into(), "Doing".into(), " done ".into(), "DOING".into()])
            .unwrap();
        assert_eq!(p.custom_field(status).unwrap().choices, ["To do", "Doing", "done"]);
        assert!(p.add_custom_field("status", CustomFieldKind::Number, &[]).is_err(), "names are unique, ignoring case");
        assert!(p.add_custom_field("Budget", CustomFieldKind::Cost, &["a".into()]).is_err(), "pick-lists are for text only");
        let budget = p.add_custom_field("Budget", CustomFieldKind::Cost, &[]).unwrap();
        let due = p.add_custom_field("Review date", CustomFieldKind::Date, &[]).unwrap();
        let est = p.add_custom_field("Review time", CustomFieldKind::Duration, &[]).unwrap();
        let flag = p.add_custom_field("Approved", CustomFieldKind::Flag, &[]).unwrap();

        let v = p.parse_custom_value(status, "doing").unwrap();
        p.set_custom_value(t, status, v).unwrap();
        assert_eq!(p.task(t).unwrap().custom.get(&status), Some(&CustomValue::Text("Doing".into())), "normalized to the listed spelling");
        let v = p.parse_custom_value(status, "Blocked").unwrap();
        assert!(p.set_custom_value(t, status, v).is_err(), "not in the pick-list");

        let v = p.parse_custom_value(budget, "$1,250.50").unwrap();
        p.set_custom_value(t, budget, v).unwrap();
        assert_eq!(p.custom_value_text(budget, p.task(t).unwrap().custom.get(&budget).unwrap()), "$1250.50");
        assert!(p.set_custom_value(t, budget, Some(CustomValue::Number(f64::NAN))).is_err());
        assert!(p.set_custom_value(t, budget, Some(CustomValue::Flag(true))).is_err(), "wrong kind");

        let v = p.parse_custom_value(due, "2026-11-02").unwrap();
        p.set_custom_value(t, due, v).unwrap();
        let v = p.parse_custom_value(est, "3d").unwrap();
        p.set_custom_value(t, est, v).unwrap();
        assert_eq!(p.task(t).unwrap().custom.get(&est), Some(&CustomValue::Duration(3 * 480)));
        let v = p.parse_custom_value(flag, "yes").unwrap();
        p.set_custom_value(t, flag, v).unwrap();
        assert!(p.parse_custom_value(flag, "maybe").is_err());
        assert_eq!(p.parse_custom_value(flag, "  ").unwrap(), None, "empty text clears");

        // Values and field definitions survive a save and load.
        let json = crate::io::to_json(&p).unwrap();
        let back: Project = serde_json::from_str(&json).unwrap();
        assert_eq!(back.custom_fields, p.custom_fields);
        assert_eq!(back.task(t).unwrap().custom, p.task(t).unwrap().custom);

        // Deleting a field removes its values and its columns.
        p.set_table_columns("Entry", &["name".into(), custom_column_key(status)]).unwrap();
        p.delete_custom_field(status).unwrap();
        assert!(!p.task(t).unwrap().custom.contains_key(&status));
        assert_eq!(p.table_layout("entry").unwrap().columns, ["name"]);
        assert!(p.set_table_columns("Entry", &[custom_column_key(status)]).is_err(), "unknown custom field");
        p.set_table_columns("Entry", &[]).unwrap();
        assert!(p.table_layout("Entry").is_none());
    }
}
