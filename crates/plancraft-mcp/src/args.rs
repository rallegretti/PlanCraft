//! Typed access to tool arguments and resolution of entity references.
//!
//! Agents refer to tasks, resources and calendars either by unique id
//! (numbers, preferred) or by name (strings). Dates, durations, work, lags and
//! units are accepted in a forgiving set of textual formats.

use std::collections::HashMap;

use chrono::{NaiveDate, NaiveDateTime};
use plancraft_core::calendar::TimeRange;
use plancraft_core::duration::{Lag, Minutes, parse_work};
use plancraft_core::ids::*;
use plancraft_core::model::*;
use serde_json::{Map, Value};
use uuid::Uuid;

/// A failure reported back to the agent as a tool result with `isError: true`.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolError(pub String);

impl std::fmt::Display for ToolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ToolError {}

impl From<String> for ToolError {
    fn from(s: String) -> Self {
        ToolError(s)
    }
}
impl From<&str> for ToolError {
    fn from(s: &str) -> Self {
        ToolError(s.to_string())
    }
}
impl From<plancraft_core::ops::PlanError> for ToolError {
    fn from(e: plancraft_core::ops::PlanError) -> Self {
        ToolError(e.to_string())
    }
}
impl From<plancraft_core::io::FileError> for ToolError {
    fn from(e: plancraft_core::io::FileError) -> Self {
        ToolError(e.to_string())
    }
}

pub type ToolResult<T = Value> = Result<T, ToolError>;

/// Shorthand for an error result.
pub fn err<T>(msg: impl Into<String>) -> ToolResult<T> {
    Err(ToolError(msg.into()))
}

/// Typed view over a tool's `arguments` object.
#[derive(Clone, Copy, Debug)]
pub struct Args<'a> {
    map: &'a Map<String, Value>,
}

static EMPTY: std::sync::LazyLock<Map<String, Value>> = std::sync::LazyLock::new(Map::new);

impl<'a> Args<'a> {
    pub fn new(map: &'a Map<String, Value>) -> Self {
        Self { map }
    }

    pub fn empty() -> Args<'static> {
        Args { map: &EMPTY }
    }

    /// Wraps a JSON value that must be an object.
    pub fn from_value(v: &'a Value, what: &str) -> ToolResult<Self> {
        match v.as_object() {
            Some(map) => Ok(Self { map }),
            None => err(format!("{what} must be an object")),
        }
    }

    pub fn map(&self) -> &'a Map<String, Value> {
        self.map
    }

    /// The raw value, treating JSON `null` as absent.
    pub fn get(&self, key: &str) -> Option<&'a Value> {
        self.map.get(key).filter(|v| !v.is_null())
    }

    /// Whether the key is present (possibly with an explicit `null`).
    pub fn present(&self, key: &str) -> bool {
        self.map.contains_key(key)
    }

    /// Whether the key is present with value `null` or `""` (meaning "clear").
    pub fn is_clear(&self, key: &str) -> bool {
        match self.map.get(key) {
            Some(Value::Null) => true,
            Some(Value::String(s)) => s.trim().is_empty(),
            _ => false,
        }
    }

    pub fn str(&self, key: &str) -> ToolResult<Option<&'a str>> {
        match self.get(key) {
            None => Ok(None),
            Some(Value::String(s)) => Ok(Some(s.as_str())),
            Some(other) => err(format!("'{key}' must be a string, got {other}")),
        }
    }

    pub fn req_str(&self, key: &str) -> ToolResult<&'a str> {
        self.str(key)?
            .ok_or_else(|| ToolError(format!("missing required argument '{key}'")))
    }

    /// A string argument; numbers and booleans are converted to text.
    pub fn text(&self, key: &str) -> ToolResult<Option<String>> {
        match self.get(key) {
            None => Ok(None),
            Some(Value::String(s)) => Ok(Some(s.clone())),
            Some(Value::Number(n)) => Ok(Some(n.to_string())),
            Some(Value::Bool(b)) => Ok(Some(b.to_string())),
            Some(other) => err(format!("'{key}' must be a string, got {other}")),
        }
    }

    pub fn bool(&self, key: &str) -> ToolResult<Option<bool>> {
        match self.get(key) {
            None => Ok(None),
            Some(Value::Bool(b)) => Ok(Some(*b)),
            Some(Value::String(s)) => match s.trim().to_ascii_lowercase().as_str() {
                "true" | "yes" | "1" => Ok(Some(true)),
                "false" | "no" | "0" => Ok(Some(false)),
                _ => err(format!("'{key}' must be a boolean")),
            },
            Some(other) => err(format!("'{key}' must be a boolean, got {other}")),
        }
    }

    pub fn f64(&self, key: &str) -> ToolResult<Option<f64>> {
        match self.get(key) {
            None => Ok(None),
            Some(Value::Number(n)) => Ok(n.as_f64()),
            Some(Value::String(s)) => s
                .trim()
                .parse::<f64>()
                .map(Some)
                .map_err(|_| ToolError(format!("'{key}' must be a number"))),
            Some(other) => err(format!("'{key}' must be a number, got {other}")),
        }
    }

    pub fn u64(&self, key: &str) -> ToolResult<Option<u64>> {
        match self.f64(key)? {
            None => Ok(None),
            Some(v) if v >= 0.0 && v.fract() == 0.0 => Ok(Some(v as u64)),
            Some(_) => err(format!("'{key}' must be a non-negative integer")),
        }
    }

    pub fn array(&self, key: &str) -> ToolResult<Option<&'a Vec<Value>>> {
        match self.get(key) {
            None => Ok(None),
            Some(Value::Array(a)) => Ok(Some(a)),
            Some(other) => err(format!("'{key}' must be an array, got {other}")),
        }
    }

    /// An array argument that also accepts a single value.
    pub fn list(&self, key: &str) -> Vec<&'a Value> {
        match self.get(key) {
            None => Vec::new(),
            Some(Value::Array(a)) => a.iter().collect(),
            Some(v) => vec![v],
        }
    }

    pub fn object(&self, key: &str) -> ToolResult<Option<Args<'a>>> {
        match self.get(key) {
            None => Ok(None),
            Some(Value::Object(m)) => Ok(Some(Args { map: m })),
            Some(other) => err(format!("'{key}' must be an object, got {other}")),
        }
    }
}

// ---------------------------------------------------------------------------
// Formatting
// ---------------------------------------------------------------------------

pub const DATETIME_FMT: &str = "%Y-%m-%d %H:%M";
pub const DATE_FMT: &str = "%Y-%m-%d";

pub fn fmt_dt(dt: Option<NaiveDateTime>) -> Value {
    match dt {
        Some(d) => Value::String(d.format(DATETIME_FMT).to_string()),
        None => Value::Null,
    }
}

pub fn fmt_date(d: Option<NaiveDate>) -> Value {
    match d {
        Some(d) => Value::String(d.format(DATE_FMT).to_string()),
        None => Value::Null,
    }
}

/// Rounds to two decimals for compact output.
pub fn round2(v: f64) -> f64 {
    if !v.is_finite() {
        return 0.0;
    }
    (v * 100.0).round() / 100.0
}

/// Hours from minutes, rounded to two decimals.
pub fn hours(m: impl Into<f64>) -> f64 {
    round2(m.into() / 60.0)
}

/// Units as a percent string, e.g. `"50%"`.
pub fn pct_text(units: f64) -> String {
    let p = units * 100.0;
    if (p - p.round()).abs() < 1e-6 {
        format!("{}%", p.round() as i64)
    } else {
        format!("{p:.1}%")
    }
}

// ---------------------------------------------------------------------------
// Parsing values with project context
// ---------------------------------------------------------------------------

/// Parses a date/date-time. `finish` selects the end of the working day for bare dates.
pub fn parse_dt(p: &Project, v: &Value, finish: bool, key: &str) -> ToolResult<NaiveDateTime> {
    let s = match v {
        Value::String(s) => s.as_str(),
        other => {
            return err(format!(
                "'{key}' must be a date string like \"2026-10-19\" or \"2026-10-19 13:00\", got {other}"
            ));
        }
    };
    p.parse_datetime(s, finish).ok_or_else(|| {
        ToolError(format!(
            "'{key}': cannot parse date '{s}'; use \"YYYY-MM-DD\" or \"YYYY-MM-DD HH:MM\""
        ))
    })
}

/// Parses a calendar date (no time).
pub fn parse_date(v: &Value, key: &str) -> ToolResult<NaiveDate> {
    let s = match v {
        Value::String(s) => s.trim(),
        other => {
            return err(format!(
                "'{key}' must be a date string like \"2026-10-19\", got {other}"
            ));
        }
    };
    let s = s.split([' ', 'T']).next().unwrap_or(s);
    for f in ["%Y-%m-%d", "%m/%d/%Y", "%d %b %Y", "%b %d %Y", "%b %d, %Y"] {
        if let Ok(d) = NaiveDate::parse_from_str(s, f) {
            return Ok(d);
        }
    }
    err(format!(
        "'{key}': cannot parse date '{s}'; use \"YYYY-MM-DD\""
    ))
}

/// Parses a duration: text like `"3d"`, `"4h"`, `"2w"`, `"1.5 days"`, `"3d?"`, or a number of days.
///
/// Returns minutes and whether the value is marked as an estimate (`?`).
pub fn parse_duration_value(p: &Project, v: &Value, key: &str) -> ToolResult<(Minutes, bool)> {
    match v {
        Value::Number(n) => {
            let d = n.as_f64().unwrap_or(0.0);
            if d < 0.0 {
                return err(format!("'{key}' must not be negative"));
            }
            Ok((
                (d * p.settings.time.minutes_per_day as f64).round() as Minutes,
                false,
            ))
        }
        Value::String(s) => {
            let (m, est) = p.parse_duration(s).ok_or_else(|| ToolError(format!("'{key}': cannot parse duration '{s}'; use e.g. \"3d\", \"4h\", \"2w\", \"1.5 days\" or \"3d?\"")))?;
            if m < 0 {
                return err(format!("'{key}' must not be negative"));
            }
            Ok((m, est))
        }
        other => err(format!(
            "'{key}' must be a duration string like \"3d\" or a number of days, got {other}"
        )),
    }
}

/// Parses work: text like `"16h"`, `"2d"`, or a number of hours.
pub fn parse_work_value(p: &Project, v: &Value, key: &str) -> ToolResult<Minutes> {
    match v {
        Value::Number(n) => {
            let h = n.as_f64().unwrap_or(0.0);
            if h < 0.0 {
                return err(format!("'{key}' must not be negative"));
            }
            Ok((h * 60.0).round() as Minutes)
        }
        Value::String(s) => {
            let m = parse_work(s, &p.settings.time).ok_or_else(|| ToolError(format!("'{key}': cannot parse work '{s}'; use e.g. \"16h\", \"2d\" or a number of hours")))?;
            if m < 0 {
                return err(format!("'{key}' must not be negative"));
            }
            Ok(m)
        }
        other => err(format!(
            "'{key}' must be work like \"16h\" or a number of hours, got {other}"
        )),
    }
}

/// Parses a lag: `"2d"`, `"-1d"`, `"50%"`, `"3ed"` (elapsed days) or a number of working days.
pub fn parse_lag_value(p: &Project, v: Option<&Value>, key: &str) -> ToolResult<Lag> {
    match v {
        None | Some(Value::Null) => Ok(Lag::None),
        Some(Value::Number(n)) => {
            let d = n.as_f64().unwrap_or(0.0);
            Ok(if d == 0.0 {
                Lag::None
            } else {
                Lag::Working((d * p.settings.time.minutes_per_day as f64).round() as Minutes)
            })
        }
        Some(Value::String(s)) => Lag::parse(s, &p.settings.time).ok_or_else(|| {
            ToolError(format!(
                "'{key}': cannot parse lag '{s}'; use e.g. \"2d\", \"-1d\", \"50%\" or \"3ed\""
            ))
        }),
        Some(other) => err(format!(
            "'{key}' must be a lag string like \"2d\", got {other}"
        )),
    }
}

/// Parses units: `"50%"`, `"1.5"`, or a number (values above 10 are read as percentages).
pub fn parse_units_value(v: &Value, key: &str) -> ToolResult<f64> {
    let units = match v {
        Value::Number(n) => {
            let x = n.as_f64().unwrap_or(0.0);
            if x > 10.0 { x / 100.0 } else { x }
        }
        Value::String(s) => {
            let t = s.trim();
            if let Some(pct) = t.strip_suffix('%') {
                pct.trim()
                    .parse::<f64>()
                    .map_err(|_| ToolError(format!("'{key}': cannot parse '{s}'")))?
                    / 100.0
            } else {
                let x = t.parse::<f64>().map_err(|_| {
                    ToolError(format!(
                        "'{key}': cannot parse '{s}'; use e.g. \"50%\" or 0.5"
                    ))
                })?;
                if x > 10.0 { x / 100.0 } else { x }
            }
        }
        other => err(format!(
            "'{key}' must be a percentage like \"50%\" or a fraction like 0.5, got {other}"
        ))?,
    };
    if units < 0.0 || !units.is_finite() {
        return err(format!("'{key}' must not be negative"));
    }
    Ok(units)
}

/// Parses `"HH:MM-HH:MM"` ranges.
pub fn parse_time_ranges(v: Option<&Value>, key: &str) -> ToolResult<Vec<TimeRange>> {
    let Some(v) = v else { return Ok(Vec::new()) };
    let items: Vec<&Value> = match v {
        Value::Null => return Ok(Vec::new()),
        Value::Array(a) => a.iter().collect(),
        Value::String(s) if s.contains(',') => {
            return s
                .split(',')
                .filter(|x| !x.trim().is_empty())
                .map(|x| parse_one_range(x, key))
                .collect();
        }
        other => vec![other],
    };
    let mut out = Vec::new();
    for it in items {
        match it {
            Value::String(s) => out.push(parse_one_range(s, key)?),
            other => {
                return err(format!(
                    "'{key}' items must be strings like \"08:00-12:00\", got {other}"
                ));
            }
        }
    }
    plancraft_core::calendar::normalize_ranges(&mut out);
    Ok(out)
}

fn parse_one_range(s: &str, key: &str) -> ToolResult<TimeRange> {
    TimeRange::parse(s).ok_or_else(|| {
        ToolError(format!(
            "'{key}': invalid time range '{s}', expected \"HH:MM-HH:MM\" (e.g. \"08:00-12:00\")"
        ))
    })
}

/// Parses `"HH:MM"` to minutes after midnight.
pub fn parse_hhmm(s: &str, key: &str) -> ToolResult<u32> {
    let t = s.trim();
    let (h, m) = t.split_once(':').unwrap_or((t, "0"));
    match (h.trim().parse::<u32>(), m.trim().parse::<u32>()) {
        (Ok(h), Ok(m)) if h < 24 && m < 60 => Ok(h * 60 + m),
        _ => err(format!("'{key}': invalid time '{s}', expected \"HH:MM\"")),
    }
}

pub fn parse_link_type(v: Option<&Value>) -> ToolResult<LinkType> {
    match v {
        None | Some(Value::Null) => Ok(LinkType::FS),
        Some(Value::String(s)) => {
            let norm = s.trim().to_ascii_lowercase().replace(['-', '_', ' '], "");
            let code = match norm.as_str() {
                "finishtostart" => "FS",
                "starttostart" => "SS",
                "finishtofinish" => "FF",
                "starttofinish" => "SF",
                _ => s.trim(),
            };
            LinkType::parse(code)
                .ok_or_else(|| ToolError(format!("invalid link type '{s}'; use FS, SS, FF or SF")))
        }
        Some(other) => err(format!(
            "link type must be one of FS, SS, FF, SF, got {other}"
        )),
    }
}

/// Parses an enum value from snake_case text (also accepting spaces and dashes).
pub fn parse_enum<T: serde::de::DeserializeOwned>(
    v: &Value,
    key: &str,
    allowed: &[&str],
) -> ToolResult<T> {
    let s = v
        .as_str()
        .ok_or_else(|| ToolError(format!("'{key}' must be one of: {}", allowed.join(", "))))?;
    let norm = s.trim().to_ascii_lowercase().replace([' ', '-'], "_");
    serde_json::from_value(Value::String(norm)).map_err(|_| {
        ToolError(format!(
            "invalid {key} '{s}'; use one of: {}",
            allowed.join(", ")
        ))
    })
}

pub fn parse_constraint_kind(v: &Value) -> ToolResult<ConstraintKind> {
    let s = v
        .as_str()
        .ok_or_else(|| ToolError("'constraint_type' must be a string".into()))?;
    let norm = s.trim().to_ascii_lowercase().replace([' ', '-', '_'], "");
    let k = match norm.as_str() {
        "asap" | "assoonaspossible" => ConstraintKind::Asap,
        "alap" | "aslateaspossible" => ConstraintKind::Alap,
        "snet" | "startnoearlierthan" => ConstraintKind::Snet,
        "snlt" | "startnolaterthan" => ConstraintKind::Snlt,
        "fnet" | "finishnoearlierthan" => ConstraintKind::Fnet,
        "fnlt" | "finishnolaterthan" => ConstraintKind::Fnlt,
        "mso" | "muststarton" => ConstraintKind::Mso,
        "mfo" | "mustfinishon" => ConstraintKind::Mfo,
        _ => {
            return err(format!(
                "invalid constraint_type '{s}'; use asap, alap, snet, snlt, fnet, fnlt, mso or mfo"
            ));
        }
    };
    Ok(k)
}

pub fn constraint_code(k: ConstraintKind) -> &'static str {
    match k {
        ConstraintKind::Asap => "asap",
        ConstraintKind::Alap => "alap",
        ConstraintKind::Snet => "snet",
        ConstraintKind::Snlt => "snlt",
        ConstraintKind::Fnet => "fnet",
        ConstraintKind::Fnlt => "fnlt",
        ConstraintKind::Mso => "mso",
        ConstraintKind::Mfo => "mfo",
    }
}

/// Serializes a unit-like enum to its snake_case string.
pub fn enum_str<T: serde::Serialize>(v: &T) -> String {
    match serde_json::to_value(v) {
        Ok(Value::String(s)) => s,
        _ => String::new(),
    }
}

// ---------------------------------------------------------------------------
// Entity references
// ---------------------------------------------------------------------------

fn as_id(v: &Value) -> Option<u32> {
    match v {
        Value::Number(n) => n.as_u64().and_then(|x| u32::try_from(x).ok()),
        Value::String(s) => s.trim().parse::<u32>().ok(),
        _ => None,
    }
}

/// Batch-local keys (`create_tasks`) mapped to the tasks they created.
pub type TaskKeys = HashMap<String, TaskId>;

/// Resolves a task reference: a unique id (number or numeric string), `"#N"` for row
/// number N, an exact task name, or a batch key from `create_tasks`.
pub fn resolve_task_with(p: &Project, v: &Value, keys: Option<&TaskKeys>) -> ToolResult<TaskId> {
    match v {
        Value::Number(_) => {
            let id = as_id(v).ok_or_else(|| ToolError(format!("invalid task id {v}")))?;
            p.task(TaskId(id)).map(|t| t.id).ok_or_else(|| {
                ToolError(format!(
                    "no task with id {id} (use list_tasks to see task ids)"
                ))
            })
        }
        Value::String(s) => {
            let t = s.trim();
            if let Some(k) = keys
                && let Some(id) = k.get(t)
            {
                return Ok(*id);
            }
            if let Some(row) = t.strip_prefix('#') {
                let row: usize = row
                    .trim()
                    .parse()
                    .map_err(|_| ToolError(format!("invalid row reference '{t}'")))?;
                return p
                    .tasks
                    .get(row.wrapping_sub(1))
                    .map(|t| t.id)
                    .ok_or_else(|| ToolError(format!("there is no task in row {row}")));
            }
            if let Ok(id) = t.parse::<u32>() {
                if let Some(task) = p.task(TaskId(id)) {
                    return Ok(task.id);
                }
                return err(format!("no task with id {id}"));
            }
            let exact: Vec<&Task> = p.tasks.iter().filter(|x| x.name == t).collect();
            let matches = if exact.is_empty() {
                p.tasks
                    .iter()
                    .filter(|x| x.name.trim().eq_ignore_ascii_case(t))
                    .collect()
            } else {
                exact
            };
            match matches.len() {
                1 => Ok(matches[0].id),
                0 => err(format!(
                    "no task named '{t}' (use list_tasks to find task ids)"
                )),
                _ => err(format!(
                    "task name '{t}' is ambiguous; matching ids: {}. Use the numeric id instead",
                    matches
                        .iter()
                        .map(|x| x.id.0.to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                )),
            }
        }
        Value::Object(m) => {
            if let Some(id) = m.get("id") {
                resolve_task_with(p, id, keys)
            } else if let Some(row) = m.get("row").and_then(as_id) {
                p.tasks
                    .get((row as usize).wrapping_sub(1))
                    .map(|t| t.id)
                    .ok_or_else(|| ToolError(format!("there is no task in row {row}")))
            } else if let Some(name) = m.get("name") {
                resolve_task_with(p, name, keys)
            } else {
                err("task reference object needs 'id', 'row' or 'name'")
            }
        }
        other => err(format!(
            "invalid task reference {other}; use the task's numeric id or exact name"
        )),
    }
}

pub fn resolve_task(p: &Project, v: &Value) -> ToolResult<TaskId> {
    resolve_task_with(p, v, None)
}

/// Resolves a required task argument.
pub fn task_arg(p: &Project, a: &Args, key: &str) -> ToolResult<TaskId> {
    let v = a
        .get(key)
        .ok_or_else(|| ToolError(format!("missing required argument '{key}'")))?;
    resolve_task(p, v).map_err(|e| ToolError(format!("'{key}': {e}")))
}

/// Resolves an array (or single) task argument.
pub fn task_list_arg(p: &Project, a: &Args, key: &str) -> ToolResult<Vec<TaskId>> {
    let mut out = Vec::new();
    for v in a.list(key) {
        let id = resolve_task(p, v).map_err(|e| ToolError(format!("'{key}': {e}")))?;
        if !out.contains(&id) {
            out.push(id);
        }
    }
    Ok(out)
}

/// Resolves a resource by id or name/initials.
pub fn resolve_resource(p: &Project, v: &Value) -> ToolResult<ResourceId> {
    match v {
        Value::Number(_) => {
            let id = as_id(v).ok_or_else(|| ToolError(format!("invalid resource id {v}")))?;
            p.resource(ResourceId(id))
                .map(|r| r.id)
                .ok_or_else(|| ToolError(format!("no resource with id {id} (use list_resources)")))
        }
        Value::String(s) => {
            let t = s.trim();
            if let Ok(id) = t.parse::<u32>()
                && let Some(r) = p.resource(ResourceId(id))
            {
                return Ok(r.id);
            }
            if let Some(r) = p.resources.iter().find(|r| r.name == t) {
                return Ok(r.id);
            }
            p.resource_by_name(t).map(|r| r.id).ok_or_else(|| {
                ToolError(format!(
                    "no resource named '{t}' (use list_resources or create_resource)"
                ))
            })
        }
        Value::Object(m) => match m.get("id").or_else(|| m.get("name")) {
            Some(x) => resolve_resource(p, x),
            None => err("resource reference object needs 'id' or 'name'"),
        },
        other => err(format!(
            "invalid resource reference {other}; use the resource's numeric id or name"
        )),
    }
}

pub fn resource_arg(p: &Project, a: &Args, key: &str) -> ToolResult<ResourceId> {
    let v = a
        .get(key)
        .ok_or_else(|| ToolError(format!("missing required argument '{key}'")))?;
    resolve_resource(p, v).map_err(|e| ToolError(format!("'{key}': {e}")))
}

/// Resolves a calendar by id or name.
pub fn resolve_calendar(p: &Project, v: &Value) -> ToolResult<CalendarId> {
    if let Some(id) = as_id(v)
        && let Some(c) = p.calendar(CalendarId(id))
    {
        return Ok(c.id);
    }
    if let Value::String(s) = v {
        let t = s.trim();
        if let Some(c) = p.calendars.iter().find(|c| c.name.eq_ignore_ascii_case(t)) {
            return Ok(c.id);
        }
    }
    err(format!(
        "no calendar {v}; available: {}",
        p.calendars
            .iter()
            .map(|c| format!("{} ({})", c.id.0, c.name))
            .collect::<Vec<_>>()
            .join(", ")
    ))
}

pub fn calendar_arg(p: &Project, a: &Args, key: &str) -> ToolResult<CalendarId> {
    let v = a
        .get(key)
        .ok_or_else(|| ToolError(format!("missing required argument '{key}'")))?;
    resolve_calendar(p, v).map_err(|e| ToolError(format!("'{key}': {e}")))
}

/// Resolves an external project by name, uuid, or 1-based position in the list.
pub fn resolve_external(p: &Project, v: &Value) -> ToolResult<Uuid> {
    let s = match v {
        Value::String(s) => s.trim().to_string(),
        Value::Number(n) => n.to_string(),
        other => return err(format!("invalid external project reference {other}")),
    };
    if let Some(e) = p
        .external_projects
        .iter()
        .find(|e| e.id.to_string() == s || e.name.eq_ignore_ascii_case(&s))
    {
        return Ok(e.id);
    }
    if let Some(e) = p.external_projects.iter().find(|e| {
        let path = std::path::Path::new(&e.path);
        path.file_name().is_some_and(|f| f.to_string_lossy() == s)
            || path.file_stem().is_some_and(|f| f.to_string_lossy() == s)
            || e.path == s
    }) {
        return Ok(e.id);
    }
    if let Ok(n) = s.parse::<usize>()
        && let Some(e) = p.external_projects.get(n.wrapping_sub(1))
    {
        return Ok(e.id);
    }
    err(format!(
        "no external project '{s}'; registered: [{}]. Use add_external_project first",
        p.external_projects
            .iter()
            .map(|e| e.name.clone())
            .collect::<Vec<_>>()
            .join(", ")
    ))
}

/// Resolves a predecessor/member reference that may be local or external.
///
/// External references are objects `{"external_project": <name|uuid>, "task": <id>}`.
pub fn resolve_task_ref(p: &Project, v: &Value, keys: Option<&TaskKeys>) -> ToolResult<TaskRef> {
    if let Value::Object(m) = v
        && let Some(ext) = m.get("external_project").or_else(|| m.get("project"))
    {
        let project = resolve_external(p, ext)?;
        let task = m
            .get("task")
            .or_else(|| m.get("id"))
            .and_then(as_id)
            .ok_or_else(|| ToolError("external reference needs a numeric 'task' id".into()))?;
        return Ok(TaskRef::External {
            project,
            task: TaskId(task),
        });
    }
    if let Value::Object(m) = v
        && let Some(t) = m.get("task")
    {
        return resolve_task_with(p, t, keys).map(TaskRef::Local);
    }
    resolve_task_with(p, v, keys).map(TaskRef::Local)
}

/// Parses a predecessor list.
///
/// * A string uses the Microsoft Project "Predecessors" column syntax with ROW numbers
///   (e.g. `"3,5SS+2d,Other\12"`).
/// * An array holds unique ids / names, or objects `{task, type, lag}` (or
///   `{external_project, task, type, lag}`).
pub fn parse_predecessor_list(
    p: &Project,
    v: &Value,
    keys: Option<&TaskKeys>,
) -> ToolResult<Vec<Dependency>> {
    match v {
        Value::Null => Ok(Vec::new()),
        Value::String(s) => Ok(p.parse_predecessors(s)?),
        Value::Array(items) => {
            let mut out = Vec::new();
            for it in items {
                let (pred, kind, lag) = match it {
                    Value::Object(m) => {
                        let pred = resolve_task_ref(p, it, keys)?;
                        let kind = parse_link_type(m.get("type").or_else(|| m.get("kind")))?;
                        let lag = parse_lag_value(p, m.get("lag"), "lag")?;
                        (pred, kind, lag)
                    }
                    other => (
                        TaskRef::Local(resolve_task_with(p, other, keys)?),
                        LinkType::FS,
                        Lag::None,
                    ),
                };
                out.push(Dependency {
                    predecessor: pred,
                    kind,
                    lag,
                });
            }
            Ok(out)
        }
        Value::Number(_) => Ok(vec![Dependency::fs(resolve_task_with(p, v, keys)?)]),
        other => err(format!("invalid predecessors {other}")),
    }
}

/// Parses an estimate object: `{points}`, `{optimistic, most_likely, pessimistic}` or `{work}`.
pub fn parse_estimate(p: &Project, v: &Value) -> ToolResult<Option<Estimate>> {
    let m = match v {
        Value::Null => return Ok(None),
        Value::Object(m) => m,
        Value::Number(n) => {
            return Ok(Some(Estimate::Complexity {
                points: n.as_f64().unwrap_or(0.0),
            }));
        }
        other => return err(format!("'estimate' must be an object, got {other}")),
    };
    let a = Args::new(m);
    let method = a
        .str("method")?
        .map(|s| s.to_ascii_lowercase().replace([' ', '-'], "_"));
    let method = match method.as_deref() {
        Some(x) => x.to_string(),
        None if a.present("points") => "complexity".into(),
        None if a.present("most_likely") || a.present("optimistic") || a.present("pessimistic") => {
            "three_point".into()
        }
        None if a.present("work") || a.present("minutes") => "work".into(),
        None => {
            return err(
                "'estimate' needs 'points', 'optimistic'/'most_likely'/'pessimistic', or 'work'",
            );
        }
    };
    match method.as_str() {
        "complexity" | "points" | "story_points" => {
            let points = a
                .f64("points")?
                .ok_or_else(|| ToolError("complexity estimate needs 'points'".into()))?;
            if points < 0.0 {
                return err("'points' must not be negative");
            }
            Ok(Some(Estimate::Complexity { points }))
        }
        "three_point" | "pert" => {
            let get = |k: &str| -> ToolResult<Minutes> {
                let v = a
                    .get(k)
                    .ok_or_else(|| ToolError(format!("three-point estimate needs '{k}'")))?;
                parse_work_value(p, v, k)
            };
            let (o, m, pe) = (get("optimistic")?, get("most_likely")?, get("pessimistic")?);
            if !(o <= m && m <= pe) {
                return err(
                    "three-point estimate requires optimistic <= most_likely <= pessimistic",
                );
            }
            Ok(Some(Estimate::ThreePoint {
                optimistic: o,
                most_likely: m,
                pessimistic: pe,
            }))
        }
        "work" => {
            let minutes = match a.get("work") {
                Some(w) => parse_work_value(p, w, "work")?,
                None => a
                    .f64("minutes")?
                    .map(|m| m.round() as Minutes)
                    .ok_or_else(|| ToolError("work estimate needs 'work'".into()))?,
            };
            Ok(Some(Estimate::Work { minutes }))
        }
        other => err(format!(
            "unknown estimate method '{other}'; use complexity, three_point or work"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn p() -> Project {
        let mut p = Project::new("T", NaiveDate::from_ymd_opt(2026, 10, 19).unwrap());
        p.add_task("Design", None);
        p.add_task("Build", None);
        p
    }

    #[test]
    fn parses_values() {
        let p = p();
        assert_eq!(
            parse_duration_value(&p, &json!("3d?"), "d").unwrap(),
            (1440, true)
        );
        assert_eq!(
            parse_duration_value(&p, &json!(2), "d").unwrap(),
            (960, false)
        );
        assert_eq!(
            parse_duration_value(&p, &json!("1.5 days"), "d").unwrap(),
            (720, false)
        );
        assert_eq!(parse_work_value(&p, &json!(16), "w").unwrap(), 960);
        assert_eq!(parse_work_value(&p, &json!("2d"), "w").unwrap(), 960);
        assert_eq!(parse_units_value(&json!("50%"), "u").unwrap(), 0.5);
        assert_eq!(parse_units_value(&json!(150), "u").unwrap(), 1.5);
        assert_eq!(parse_units_value(&json!(2), "u").unwrap(), 2.0);
        assert_eq!(
            parse_lag_value(&p, Some(&json!("-1d")), "l").unwrap(),
            Lag::Working(-480)
        );
        assert_eq!(
            parse_link_type(Some(&json!("start-to-start"))).unwrap(),
            LinkType::SS
        );
    }

    #[test]
    fn resolves_tasks() {
        let p = p();
        assert_eq!(resolve_task(&p, &json!(2)).unwrap(), TaskId(2));
        assert_eq!(resolve_task(&p, &json!("build")).unwrap(), TaskId(2));
        assert_eq!(resolve_task(&p, &json!("#1")).unwrap(), TaskId(1));
        assert!(resolve_task(&p, &json!(9)).is_err());
    }
}
